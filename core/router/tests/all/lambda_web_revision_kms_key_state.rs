// SPDX-License-Identifier: CC0-1.0

//! The KMS key a Lambda Web revision would be encrypted under has to be a key KMS would
//! still encrypt with.
//!
//! The RULE pinned here: **a create that takes a customer managed KMS key for the resource it
//! is about to build resolves that key in KMS before it accepts the request, and refuses the
//! request — with the modelled client validation error, carrying the sentence KMS states about
//! the key — when KMS reports the key in a state that admits no cryptographic use; a refused
//! create stores nothing, so the revision list is exactly what it was, and the identical call
//! succeeds once the key is usable again.** Lambda Web takes that key on both of its create
//! paths: `CreateWebFunctionRevision.kmsKeyArn` and `CreateWebFunction.revisionConfig.kmsKeyArn`
//! ("the Amazon Resource Name (ARN) of the AWS Key Management Service (AWS KMS) key used to
//! encrypt the revision's code and environment variables",
//! `smithy_query(query="shape",shape="RevisionConfig")`), and both model `ValidationException`
//! at HTTP 400 (`smithy_query(query="operation",operation="CreateWebFunctionRevision")`).
//!
//! THREE crates have to agree for that to hold, which is why the suite lives here: Lambda Web
//! decides the refusal, KMS is the service that holds the key and its state, and Amazon S3
//! holds the deployment package the accepted revision's build reads. A crate-local test can
//! only ever see one side of it — and the owning crate still roots its integration tests at a
//! hand-declared `tests/all.rs`, a file a fix may not edit, while the router's `tests/all/`
//! picks a new suite up with no declaration at all.
//!
//! The rows differ along what the rule generalises over — the key's STATE at the instant of the
//! call: a key KMS reports as usable, one whose owner turned it off (the recorded case), one
//! whose deletion is scheduled, and the same key once it is usable again. Two further rows span
//! the other two forms the member takes: an identifier KMS resolves to no key at all, and no key
//! named at all. Every expected message is computed from the key the row created and the state
//! KMS reported it in, never from a value this suite wrote down.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for, and therefore the Region of every key and
/// function it creates.
const REGION: &str = "us-east-1";

/// The URI prefix Lambda Web binds every operation under
/// (`smithy_query(query="digest")`: `/2025-03-07/...`).
const PREFIX: &str = "/2025-03-07";

/// The bucket and key the deployment package every accepted revision is built from is staged
/// at.
const BUCKET: &str = "lambda-web-kms-key-state";
const PACKAGE_KEY: &str = "app.zip";

/// A .zip deployment package with no entries: the 22 bytes of an end-of-central-directory
/// record, which is the whole of an empty archive and is what a zip reader opens it from. The
/// package's CONTENT is not what any row turns on — only that the build can read an archive, so
/// an accepted revision settles `Active`.
const EMPTY_ARCHIVE: &[u8] = b"PK\x05\x06\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0";

/// A valid `executionRoleArn` (model pattern
/// `^arn:(aws[a-z-]*):iam::[0-9]{12}:role/[a-zA-Z0-9+=,.@_/-]+$`).
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-web-exec";

/// The runtime every `runtimeConfig` below names (model pattern
/// `^[a-z][a-z0-9]*(\.[a-z0-9]+)*$`).
const RUNTIME: &str = "nodejs24.x";

/// The states of the Lambda Web resources these rows read: `RevisionState$Active` ("ready to
/// serve traffic") and `FunctionState$Active`.
const ACTIVE: &str = "Active";

/// `KeyState` values of the KMS model (`smithy_query(query="shape",shape="KeyState")`), spelled
/// out here rather than imported, so a wrong constant in an implementation cannot make this
/// suite pass.
const KEY_STATE_ENABLED: &str = "Enabled";
const KEY_STATE_DISABLED: &str = "Disabled";
const KEY_STATE_PENDING_DELETION: &str = "PendingDeletion";

/// The refusal both creates model for a request they will not accept: `ValidationException`,
/// HTTP 400.
const REFUSED: (u16, &str) = (400, "ValidationException");

/// The status both creates answer an accepted request with (`http {code: 202}` on each).
const ACCEPTED: u16 = 202;

/// An ARN shaped like a key of this account and Region that KMS holds no key under — it names
/// no key any row creates. It satisfies `KmsKeyArn`'s own pattern
/// (`^arn:(aws[a-z-]*):kms:[a-z0-9-]+:[0-9]{12}:key/[a-z0-9-]+$`), so what refuses it is KMS's
/// answer rather than the member's constraint.
const UNHELD_KEY: &str = "arn:aws:kms:us-east-1:000000000000:key/00000000-0000-4000-8000-000000000000";

/// How often a wait looks again, and how long it keeps looking. The span a created revision
/// spends `Pending` is Lambda Web's own, so nothing here sleeps for it: the wait is on the
/// CONDITION, with a deadline generous enough that a loaded machine delays a row rather than
/// failing it.
const LOOK_AGAIN: Duration = Duration::from_millis(25);
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// One answered request: the status, the error shape it named and its parsed body.
struct Answered {
    status: u16,
    error_type: String,
    body: Value,
}

impl Answered {
    /// The error shape this answer names: the `x-amzn-errortype` header the REST protocols
    /// carry, falling back to the body's `__type`, reduced to the shape's own name.
    fn error(&self) -> String {
        let named = if self.error_type.is_empty() {
            self.body
                .get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
        } else {
            self.error_type.as_str()
        };
        named
            .split(['#', ':'])
            .next_back()
            .unwrap_or_default()
            .to_string()
    }

    /// A member of the answer as text, or the empty string when it is absent.
    fn text(&self, member: &str) -> String {
        self.body
            .get(member)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// Lambda Web, KMS and Amazon S3 over ONE engine, as `core/router/src/lib.rs` mounts them: the
/// key a create names is KMS's own resource and the package its build reads is Amazon S3's.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: every call carries a credential scope so the
    // router can name the target service, and the signature itself is not under test.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambdaweb_lambdaweb::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request and where each handler reads its Region from.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answered {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response =
        Box::pin(router.dispatch(builder.body(body).expect("a well-formed request"))).await;
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let status = response.status().as_u16();
    let body = response.into_body();
    let body = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    Answered {
        status,
        error_type,
        body,
    }
}

/// One Lambda Web call over its restJson1 surface (`sigv4Name: lambda`).
async fn web(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answered {
    let (headers, payload): (Vec<(&str, &str)>, Bytes) = match &body {
        Some(value) => (
            vec![("content-type", "application/json")],
            Bytes::from(value.to_string()),
        ),
        None => (Vec::new(), Bytes::new()),
    };
    let authorization = authorization("lambda");
    let mut all = vec![("authorization", authorization.as_str())];
    all.extend(headers);
    dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        &format!("{PREFIX}{path}"),
        &all,
        payload,
    )
    .await
}

/// One KMS call over its awsJson1_1 surface (`X-Amz-Target: TrentService.<Operation>`).
async fn kms(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answered {
    let target = format!("TrentService.{operation}");
    let authorization = authorization("kms");
    dispatch(
        router,
        "POST",
        &format!("kms.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", target.as_str()),
            ("authorization", authorization.as_str()),
        ],
        Bytes::from(body.to_string()),
    )
    .await
}

/// A customer managed key, made through KMS's own wire so every row names a key that service
/// really holds. Returns the ARN KMS minted for it.
async fn created_key(router: &MultiServiceRouter, case: &str) -> String {
    let created = kms(router, "CreateKey", &json!({})).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected kms:CreateKey to answer 200, got {} with {:?}",
        created.status, created.body
    );
    created.body["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// The `KeyState` KMS reports for `key_arn`, asserted to be `expected` — so a row's setup is
/// pinned on KMS's own answer rather than assumed from the call that changed it.
async fn key_state_is(
    router: &MultiServiceRouter,
    case: &str,
    key_arn: &str,
    expected: &str,
) {
    let described = kms(router, "DescribeKey", &json!({"KeyId": key_arn})).await;
    assert_eq!(
        described.status, 200,
        "case {case:?}: expected kms:DescribeKey of {key_arn} to answer 200, got {} with {:?}",
        described.status, described.body
    );
    let reported = described.body["KeyMetadata"]["KeyState"]
        .as_str()
        .unwrap_or_default();
    assert_eq!(
        reported, expected,
        "case {case:?}: expected KMS to report {key_arn} in KeyState {expected:?}, got {reported:?}"
    );
}

/// What a row does to its key before the create is made.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outage {
    /// Nothing: the key stays as KMS created it.
    None,
    /// Its owner turns it off (`DisableKey`).
    Disabled,
    /// Its deletion is scheduled (`ScheduleKeyDeletion`).
    DeletionScheduled,
    /// Turned off, then turned back on (`DisableKey` then `EnableKey`) — the recovery the
    /// rule's last clause is about.
    DisabledThenEnabled,
}

/// Put `key_arn` into the state `outage` names, and leave KMS reporting it.
async fn apply(router: &MultiServiceRouter, case: &str, key_arn: &str, outage: Outage) {
    let calls: &[&str] = match outage {
        Outage::None => &[],
        Outage::Disabled => &["DisableKey"],
        Outage::DeletionScheduled => &["ScheduleKeyDeletion"],
        Outage::DisabledThenEnabled => &["DisableKey", "EnableKey"],
    };
    for operation in calls {
        let answered = kms(router, operation, &json!({"KeyId": key_arn})).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected kms:{operation} of {key_arn} to answer 200, got {} with {:?}",
            answered.status, answered.body
        );
    }
}

/// One row: the key's state at the instant of the call, and what the create must answer.
struct Row {
    case: &'static str,
    outage: Outage,
    /// The `KeyState` KMS must report the key in once the row's outage is applied.
    state: &'static str,
    /// The tail of the sentence the create is refused with — the key's ARN followed by this —
    /// or `None` for a key the create must accept.
    refusal: Option<&'static str>,
}

/// The rows every test below drives: the class of key state, spanning what the rule
/// generalises over.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a key KMS reports as usable",
            outage: Outage::None,
            state: KEY_STATE_ENABLED,
            refusal: None,
        },
        Row {
            // The recorded case.
            case: "a key its owner turned off",
            outage: Outage::Disabled,
            state: KEY_STATE_DISABLED,
            refusal: Some("is disabled."),
        },
        Row {
            case: "a key whose deletion is scheduled",
            outage: Outage::DeletionScheduled,
            state: KEY_STATE_PENDING_DELETION,
            refusal: Some("is pending deletion."),
        },
        Row {
            case: "a key turned off and then turned back on",
            outage: Outage::DisabledThenEnabled,
            state: KEY_STATE_ENABLED,
            refusal: None,
        },
    ]
}

/// The `buildConfig` every create below carries: the staged package, and a runtime.
fn build_config() -> Value {
    json!({
        "codeConfig": { "s3Object": { "bucket": BUCKET, "key": PACKAGE_KEY } },
        "runtimeConfig": { "runtime": RUNTIME },
    })
}

/// Create the bucket and stage the deployment package every accepted revision is built from.
async fn stage_package(router: &MultiServiceRouter) {
    let host = format!("s3.{REGION}.amazonaws.com");
    let authorization = authorization("s3");
    // No `CreateBucketConfiguration`: `us-east-1` is the one Region a create must not state a
    // location constraint for.
    let created = dispatch(
        router,
        "PUT",
        &host,
        &format!("/{BUCKET}"),
        &[("authorization", authorization.as_str())],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "staging: expected Amazon S3 to create {BUCKET}, got {} with {:?}",
        created.status, created.body
    );
    let staged = dispatch(
        router,
        "PUT",
        &host,
        &format!("/{BUCKET}/{PACKAGE_KEY}"),
        &[
            ("authorization", authorization.as_str()),
            ("content-type", "application/zip"),
        ],
        Bytes::from_static(EMPTY_ARCHIVE),
    )
    .await;
    assert_eq!(
        staged.status, 200,
        "staging: expected Amazon S3 to hold s3://{BUCKET}/{PACKAGE_KEY}, got {} with {:?}",
        staged.status, staged.body
    );
}

/// `POST /web-functions/{name}/revisions`, with `kmsKeyArn` when the row names a key.
async fn create_revision(
    router: &MultiServiceRouter,
    function: &str,
    kms_key_arn: Option<&str>,
) -> Answered {
    let mut body = json!({
        "buildConfig": build_config(),
        "serviceConfig": { "executionRoleArn": ROLE },
    });
    if let Some(key) = kms_key_arn {
        body["kmsKeyArn"] = json!(key);
    }
    web(
        router,
        "POST",
        &format!("/web-functions/{function}/revisions"),
        Some(body),
    )
    .await
}

/// `PUT /web-functions` with an initial revision, carrying `kmsKeyArn` when the row names a
/// key.
async fn create_function(
    router: &MultiServiceRouter,
    name: &str,
    kms_key_arn: Option<&str>,
) -> Answered {
    let mut revision_config = json!({
        "buildConfig": build_config(),
        "serviceConfig": { "executionRoleArn": ROLE },
    });
    if let Some(key) = kms_key_arn {
        revision_config["kmsKeyArn"] = json!(key);
    }
    web(
        router,
        "PUT",
        "/web-functions",
        Some(json!({ "functionName": name, "revisionConfig": revision_config })),
    )
    .await
}

/// Every revision id `ListWebFunctionRevisions` reports for `function`, in the order it
/// reported them — which is what tells a revision that was stored from one a refused create
/// never made. The model binds the listing at `POST
/// /web-functions/{functionName}/list-revisions`.
async fn listed_revisions(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
) -> Vec<String> {
    let listed = web(
        router,
        "POST",
        &format!("/web-functions/{function}/list-revisions"),
        Some(json!({})),
    )
    .await;
    assert_eq!(
        listed.status, 200,
        "case {case:?}: expected ListWebFunctionRevisions to answer 200, got {} with {:?}",
        listed.status, listed.body
    );
    listed.body["revisions"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|revision| {
            revision
                .get("revisionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// `GET /web-functions/{name}/revisions/{revisionId}`, looked at again until the revision
/// reports `Active` — the state the clock owes it — or the deadline passes.
async fn settled_revision(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    revision_id: &str,
) -> Answered {
    let started = Instant::now();
    loop {
        let read = web(
            router,
            "GET",
            &format!("/web-functions/{function}/revisions/{revision_id}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: expected GetWebFunctionRevision of {revision_id} to answer 200, got \
             {} with {:?}",
            read.status, read.body
        );
        if read.text("state") == ACTIVE || started.elapsed() > SETTLE_DEADLINE {
            return read;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE, on the operation that adds a revision to a stored function: the create is refused
/// exactly when KMS reports the key in a state that admits no use, with the sentence KMS states
/// about that key — and a refused create adds no revision, while the identical call on a key
/// that is usable again is accepted and reports the key it was given.
#[tokio::test]
async fn a_revision_create_is_refused_exactly_when_its_key_admits_no_use() {
    let router = router();
    stage_package(&router).await;

    let function = "kms-key-state-revisions";
    let created = create_function(&router, function, None).await;
    assert_eq!(
        created.status, ACCEPTED,
        "setup: expected CreateWebFunction to answer {ACCEPTED}, got {} with {:?}",
        created.status, created.body
    );
    // The revisions that existed before any row ran: a refused create must leave exactly these.
    let mut expected_revisions = listed_revisions(&router, "setup", function).await;
    assert_eq!(
        expected_revisions.len(),
        1,
        "setup: expected the create's own revision to be the only one, got {expected_revisions:?}"
    );

    for row in rows() {
        let key = created_key(&router, row.case).await;
        apply(&router, row.case, &key, row.outage).await;
        key_state_is(&router, row.case, &key, row.state).await;

        let answered = create_revision(&router, function, Some(&key)).await;
        match row.refusal {
            Some(tail) => {
                let expected = format!("{key} {tail}");
                assert_eq!(
                    (answered.status, answered.error().as_str()),
                    REFUSED,
                    "case {:?}: expected {REFUSED:?}, got {} {:?} with {:?}",
                    row.case,
                    answered.status,
                    answered.error(),
                    answered.body
                );
                assert_eq!(
                    answered.text("message"),
                    expected,
                    "case {:?}: expected the message {expected:?}, got {:?}",
                    row.case,
                    answered.body
                );
            }
            None => {
                assert_eq!(
                    answered.status, ACCEPTED,
                    "case {:?}: expected {ACCEPTED}, got {} with {:?}",
                    row.case, answered.status, answered.body
                );
                let revision_id = answered.text("revisionId");
                assert!(
                    !revision_id.is_empty() && !expected_revisions.contains(&revision_id),
                    "case {:?}: expected a revision id distinct from {expected_revisions:?}, got \
                     {revision_id:?}",
                    row.case
                );
                assert_eq!(
                    answered.text("kmsKeyArn"),
                    key,
                    "case {:?}: expected the create to report the key it was given, got {:?}",
                    row.case,
                    answered.body
                );
                // Read back through the service's own reader: the stored revision reports the
                // same key and settles `Active`.
                let settled = settled_revision(&router, row.case, function, &revision_id).await;
                assert_eq!(
                    (settled.text("state"), settled.text("kmsKeyArn")),
                    (ACTIVE.to_string(), key.clone()),
                    "case {:?}: expected the revision to settle {ACTIVE} under {key}, got {:?}",
                    row.case,
                    settled.body
                );
                expected_revisions.push(revision_id);
            }
        }

        let mut listed = listed_revisions(&router, row.case, function).await;
        listed.sort();
        let mut expected = expected_revisions.clone();
        expected.sort();
        assert_eq!(
            listed, expected,
            "case {:?}: expected the function to hold exactly {expected:?}, got {listed:?}",
            row.case
        );
    }
}

/// RULE, on the OTHER entry point that takes a revision's key: `CreateWebFunction` applies the
/// same reading to `revisionConfig.kmsKeyArn`, and a refused create stores no function at all.
#[tokio::test]
async fn a_function_create_applies_the_same_reading_to_its_revision_configs_key() {
    let router = router();
    stage_package(&router).await;

    for (index, row) in rows().into_iter().enumerate() {
        let key = created_key(&router, row.case).await;
        apply(&router, row.case, &key, row.outage).await;
        key_state_is(&router, row.case, &key, row.state).await;

        let name = format!("kms-key-state-function-{index}");
        let answered = create_function(&router, &name, Some(&key)).await;
        let read = web(&router, "GET", &format!("/web-functions/{name}"), None).await;
        match row.refusal {
            Some(tail) => {
                let expected = format!("{key} {tail}");
                assert_eq!(
                    (answered.status, answered.error().as_str()),
                    REFUSED,
                    "case {:?}: expected {REFUSED:?}, got {} {:?} with {:?}",
                    row.case,
                    answered.status,
                    answered.error(),
                    answered.body
                );
                assert_eq!(
                    answered.text("message"),
                    expected,
                    "case {:?}: expected the message {expected:?}, got {:?}",
                    row.case,
                    answered.body
                );
                assert_eq!(
                    read.status, 404,
                    "case {:?}: expected the refused create to store no function, got {} with \
                     {:?}",
                    row.case, read.status, read.body
                );
            }
            None => {
                assert_eq!(
                    answered.status, ACCEPTED,
                    "case {:?}: expected {ACCEPTED}, got {} with {:?}",
                    row.case, answered.status, answered.body
                );
                assert_eq!(
                    read.status, 200,
                    "case {:?}: expected GetWebFunction of the created function to answer 200, \
                     got {} with {:?}",
                    row.case, read.status, read.body
                );
                let revision_id = answered.body["revision"]
                    .get("revisionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let settled = settled_revision(&router, row.case, &name, &revision_id).await;
                assert_eq!(
                    (settled.text("state"), settled.text("kmsKeyArn")),
                    (ACTIVE.to_string(), key.clone()),
                    "case {:?}: expected the initial revision to settle {ACTIVE} under {key}, \
                     got {:?}",
                    row.case,
                    settled.body
                );
            }
        }
    }
}

/// RULE, over the other two forms the member takes: an identifier KMS resolves to NO key is
/// refused with the sentence KMS itself answered about it, and a create that names no key at all
/// has nothing to resolve and is accepted.
#[tokio::test]
async fn an_identifier_kms_resolves_to_no_key_is_refused_and_naming_none_is_accepted() {
    let router = router();
    stage_package(&router).await;

    let function = "kms-key-state-identifiers";
    let created = create_function(&router, function, None).await;
    assert_eq!(
        created.status, ACCEPTED,
        "setup: expected CreateWebFunction to answer {ACCEPTED}, got {} with {:?}",
        created.status, created.body
    );
    let before = listed_revisions(&router, "setup", function).await;

    // What KMS itself answers about this identifier, read here so the expected sentence is
    // that service's own rather than one this suite wrote.
    let described = kms(&router, "DescribeKey", &json!({"KeyId": UNHELD_KEY})).await;
    assert_eq!(
        described.status, 400,
        "setup: expected kms:DescribeKey of {UNHELD_KEY} to be refused at 400, got {} with {:?}",
        described.status, described.body
    );
    let kms_sentence = described
        .body
        .get("message")
        .or_else(|| described.body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        !kms_sentence.is_empty(),
        "setup: expected KMS to state a sentence about {UNHELD_KEY}, got {:?}",
        described.body
    );

    let case = "an identifier KMS resolves to no key";
    let answered = create_revision(&router, function, Some(UNHELD_KEY)).await;
    assert_eq!(
        (answered.status, answered.error().as_str()),
        REFUSED,
        "case {case:?}: expected {REFUSED:?}, got {} {:?} with {:?}",
        answered.status,
        answered.error(),
        answered.body
    );
    assert_eq!(
        answered.text("message"),
        kms_sentence,
        "case {case:?}: expected the sentence KMS answered ({kms_sentence:?}), got {:?}",
        answered.body
    );
    let after = listed_revisions(&router, case, function).await;
    assert_eq!(
        after, before,
        "case {case:?}: expected the refused create to add no revision, got {after:?}"
    );

    let case = "a create naming no key at all";
    let answered = create_revision(&router, function, None).await;
    assert_eq!(
        answered.status, ACCEPTED,
        "case {case:?}: expected {ACCEPTED}, got {} with {:?}",
        answered.status, answered.body
    );
    assert_eq!(
        answered.text("kmsKeyArn"),
        String::new(),
        "case {case:?}: expected no kmsKeyArn to be reported, got {:?}",
        answered.body
    );
    let after = listed_revisions(&router, case, function).await;
    assert_eq!(
        after.len(),
        before.len() + 1,
        "case {case:?}: expected exactly one revision more than {before:?}, got {after:?}"
    );
}
