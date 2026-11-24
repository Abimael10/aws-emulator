// SPDX-License-Identifier: CC0-1.0

//! The customer managed KMS key a HealthImaging data store is created under has to be a
//! key AWS KMS holds.
//!
//! The RULE pinned here: **`CreateDatastore$kmsKeyArn` names "the Key Management Service
//! (KMS) key for accessing encrypted data"
//! (`smithy_query(query="operation",operation="CreateDatastore")`), so the identifier is
//! resolved against the service that OWNS keys before a data store is minted: a
//! well-formed identifier KMS answers it holds no key for is refused `ValidationException`
//! (HTTP 400) and creates nothing — no data store in `ListDatastores` and no `clientToken`
//! consumed — while an identifier KMS resolves, and one KMS says nothing about, is
//! created.**
//!
//! Only a cross-service suite can pin it: the keys and the aliases are KMS's resources and
//! the data store is AWS HealthImaging's, so the rule is observable only where both are
//! mounted on one engine, as `core/router/src/lib.rs` mounts them. The medical-imaging
//! crate's own suites reach one of the two.
//!
//! The rows vary what the rule generalises over — the FORM of the identifier the member's
//! pattern admits (a key ARN and an alias ARN), whether KMS resolves it, which endpoint the
//! identifier's ARN names (the Region the data store is created in, and another Region,
//! where that key is not), whose account it names, and the member being absent altogether
//! — rather than which name the data store carries. No row writes a key id: every
//! identifier asserted on is either one KMS itself minted or one deliberately made up, and
//! the made-up ones are built per row, because one run's generated id is no part of the
//! requirement.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every data store below is created in.
const REGION: &str = "us-east-1";

/// ANOTHER Region, for the rows whose ARN names an endpoint that holds no such key.
const OTHER_REGION: &str = "us-west-2";

/// The account id every emulated ARN carries.
const ACCOUNT: &str = engine::EMULATOR_ACCOUNT_ID;

/// An account that is not this emulator's, for the row whose key is not this emulator's to
/// hold. Twelve digits, as the `kmsKeyArn` pattern's account segment requires.
const OTHER_ACCOUNT: &str = "210987654321";

/// How long one poll of a clock-owed transition waits before asking again.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a poll loop waits for a clock-owed transition before giving up. Far past every
/// span this emulator gives one, so a slow machine does not fail a case the clock would
/// have settled.
const POLL_BUDGET: Duration = Duration::from_secs(30);

/// The status a data store settles into, and the one a fresh create answers with
/// (`DatastoreStatus`).
const ACTIVE: &str = "ACTIVE";
const CREATING: &str = "CREATING";

/// The error code and the sentence AWS HealthImaging refuses a `kmsKeyArn` naming a key it
/// cannot locate with — the service's own fixed text, stated here rather than imported from
/// the crate under test, so a wrong constant in the implementation cannot make this suite
/// pass.
const VALIDATION_EXCEPTION: &str = "ValidationException";
const UNLOCATABLE_KEY: &str = "The provided KMS key for datastore cannot be located.";

/// AWS HealthImaging and AWS KMS over ONE engine.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_medicalimaging_ahigatewayservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and `region`.
/// Signatures are not verified: this router runs with request authentication off.
fn authorization(signing_name: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{region}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// What one call answered: its status and its parsed body (`Value::Null` for an empty or
/// unparseable one), plus the wire error code a refusal carried.
#[derive(Debug)]
struct Answer {
    status: u16,
    body: Value,
    error_code: String,
}

impl Answer {
    /// A body member at `path`, as text — empty where the member is absent, so an assertion
    /// reports what arrived rather than panicking on it.
    fn string(&self, path: &[&str]) -> String {
        let mut cursor = &self.body;
        for key in path {
            cursor = &cursor[*key];
        }
        cursor.as_str().unwrap_or_default().to_string()
    }
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed test request"))
        .await;
    let status = response.status().as_u16();
    let header_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let body: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    let raw_code = if header_code.is_empty() {
        body["__type"].as_str().unwrap_or_default().to_string()
    } else {
        header_code
    };
    let error_code = raw_code
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string();
    Answer {
        status,
        body,
        error_code,
    }
}

/// One AWS HealthImaging call (`restJson1`), signed for `region`.
async fn imaging(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Answer {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    dispatch(
        router,
        method,
        &format!("medical-imaging.{region}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("medical-imaging", region)),
        ],
        Bytes::from(payload),
    )
    .await
}

/// One AWS KMS call (`awsJson1_1`, `X-Amz-Target: TrentService.<Operation>`) against
/// `region`'s endpoint, which must answer 200.
async fn kms_ok(router: &MultiServiceRouter, region: &str, operation: &str, body: &Value) -> Answer {
    let target = format!("TrentService.{operation}");
    let answer = dispatch(
        router,
        "POST",
        &format!("kms.{region}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &target),
            ("authorization", &authorization("kms", region)),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "setup: expected kms:{operation} to answer 200, got {answer:?}"
    );
    answer
}

/// A `CreateDatastore` call carrying `kms_key_arn` only where the row names one.
async fn create_datastore(
    router: &MultiServiceRouter,
    datastore_name: &str,
    client_token: &str,
    kms_key_arn: Option<&str>,
) -> Answer {
    let mut body = json!({ "datastoreName": datastore_name, "clientToken": client_token });
    if let Some(key) = kms_key_arn {
        body["kmsKeyArn"] = json!(key);
    }
    imaging(router, REGION, "POST", "/datastore", Some(body)).await
}

/// Every data store name `ListDatastores` reports, following the pages it hands out.
async fn listed_datastore_names(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut next: Option<String> = None;
    loop {
        let path = match &next {
            Some(token) => format!("/datastore?nextToken={token}"),
            None => "/datastore".to_string(),
        };
        let answer = imaging(router, REGION, "GET", &path, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected ListDatastores to answer 200, got {answer:?}"
        );
        for summary in answer.body["datastoreSummaries"]
            .as_array()
            .cloned()
            .unwrap_or_default()
        {
            names.push(
                summary["datastoreName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            );
        }
        next = answer.body["nextToken"].as_str().map(str::to_string);
        if next.is_none() {
            return names;
        }
    }
}

/// The data store's status and the `kmsKeyArn` it reports, read back through
/// `GetDatastore`, polled until it leaves `CREATING` or the budget runs out — the
/// transition is the clock's, so the condition is waited on rather than a span.
async fn settled_datastore(
    router: &MultiServiceRouter,
    case: &str,
    datastore_id: &str,
) -> (String, String) {
    let path = format!("/datastore/{datastore_id}");
    let deadline = tokio::time::Instant::now() + POLL_BUDGET;
    loop {
        let answer = imaging(router, REGION, "GET", &path, None).await;
        assert_eq!(
            answer.status, 200,
            "case {case:?}: expected GetDatastore to answer 200, got {answer:?}"
        );
        let status = answer.string(&["datastoreProperties", "datastoreStatus"]);
        if status != CREATING || tokio::time::Instant::now() >= deadline {
            return (status, answer.string(&["datastoreProperties", "kmsKeyArn"]));
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A `kmsKeyArn` naming a key, in the one form the member's pattern admits for one
/// (`arn:aws…:kms:<region>:<account>:key/<uuid>`).
fn key_arn(region: &str, account: &str, key_id: &str) -> String {
    format!("arn:aws:kms:{region}:{account}:key/{key_id}")
}

/// A `kmsKeyArn` naming a key through an ALIAS, the member's other admitted form
/// (`arn:aws…:kms:<region>:<account>:alias/<name>`), from the `alias/<name>` path itself.
fn alias_arn(region: &str, account: &str, alias_path: &str) -> String {
    format!("arn:aws:kms:{region}:{account}:{alias_path}")
}

/// One row of the table.
#[derive(Debug)]
struct Case {
    case: &'static str,
    /// The `kmsKeyArn` the create carries, `None` for the member being absent.
    kms_key_arn: Option<String>,
    /// Whether the create is expected to be REFUSED for a key that cannot be located.
    refused: bool,
}

/// RULE: a `kmsKeyArn` AWS KMS answers it holds no key for is refused with 400
/// `ValidationException` and the service's unlocatable-key sentence, and creates no data
/// store; one KMS resolves, and one KMS says nothing about, creates a data store that
/// reports the key it was created under and settles into `ACTIVE`.
#[tokio::test]
async fn a_datastores_kms_key_is_resolved_against_kms() {
    let router = router();

    // Two keys KMS itself minted, one at each endpoint, so a row that names a key names a
    // key that service really holds rather than an ARN this suite wrote. The alias is made
    // through KMS's own wire too, so the alias row names an association only KMS holds.
    let held_key = kms_ok(&router, REGION, "CreateKey", &json!({})).await;
    let held_key_arn = held_key.string(&["KeyMetadata", "Arn"]);
    let held_key_id = held_key.string(&["KeyMetadata", "KeyId"]);
    let held_alias = "alias/imaging-datastore";
    kms_ok(
        &router,
        REGION,
        "CreateAlias",
        &json!({ "AliasName": held_alias, "TargetKeyId": held_key_id }),
    )
    .await;
    let elsewhere = kms_ok(&router, OTHER_REGION, "CreateKey", &json!({})).await;
    let elsewhere_key_id = elsewhere.string(&["KeyMetadata", "KeyId"]);

    // A key id of the right shape that no `CreateKey` of this run issued: KMS mints version
    // 4 UUIDs, whose variant nibble is never 0, so this one is held nowhere.
    let unheld_key_id = "00000000-0000-0000-0000-000000000000";

    let rows = [
        Case {
            case: "no kmsKeyArn at all",
            kms_key_arn: None,
            refused: false,
        },
        Case {
            case: "a key ARN KMS resolves",
            kms_key_arn: Some(held_key_arn.clone()),
            refused: false,
        },
        Case {
            case: "an alias ARN KMS resolves",
            kms_key_arn: Some(alias_arn(REGION, ACCOUNT, held_alias)),
            refused: false,
        },
        Case {
            case: "a key ARN of another account, which this emulator holds no keys of",
            kms_key_arn: Some(key_arn(REGION, OTHER_ACCOUNT, &held_key_id)),
            refused: false,
        },
        Case {
            case: "a well-formed key ARN KMS holds no key for",
            kms_key_arn: Some(key_arn(REGION, ACCOUNT, unheld_key_id)),
            refused: true,
        },
        Case {
            case: "an alias ARN KMS holds no alias for",
            kms_key_arn: Some(alias_arn(REGION, ACCOUNT, "alias/never-created")),
            refused: true,
        },
        Case {
            case: "a key ARN naming an endpoint that holds no such key",
            kms_key_arn: Some(key_arn(REGION, ACCOUNT, &elsewhere_key_id)),
            refused: true,
        },
        Case {
            case: "an alias ARN naming an endpoint that holds no such alias",
            kms_key_arn: Some(alias_arn(OTHER_REGION, ACCOUNT, held_alias)),
            refused: true,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let datastore_name = format!("kms-key-row-{index}");
        let client_token = format!("kms-key-token-{index}");
        let created = create_datastore(
            &router,
            &datastore_name,
            &client_token,
            row.kms_key_arn.as_deref(),
        )
        .await;

        if row.refused {
            assert_eq!(
                (created.status, created.error_code.as_str()),
                (400, VALIDATION_EXCEPTION),
                "case {case:?}: expected 400 {VALIDATION_EXCEPTION}, got {created:?}"
            );
            assert_eq!(
                created.string(&["message"]),
                UNLOCATABLE_KEY,
                "case {case:?}: expected the unlocatable-key sentence, got {created:?}"
            );
            let listed = listed_datastore_names(&router, case).await;
            assert!(
                !listed.contains(&datastore_name),
                "case {case:?}: expected no data store named {datastore_name:?} to exist, \
                 got {listed:?}"
            );
            // The refused call consumed no `clientToken` either: the same token, with the
            // member absent, still mints a data store.
            let retried = create_datastore(&router, &datastore_name, &client_token, None).await;
            assert_eq!(
                retried.status, 200,
                "case {case:?}: expected the refused clientToken to be free to reuse, \
                 got {retried:?}"
            );
            let (status, reported_key) =
                settled_datastore(&router, case, &retried.string(&["datastoreId"])).await;
            assert_eq!(
                (status.as_str(), reported_key.as_str()),
                (ACTIVE, ""),
                "case {case:?}: expected the retry to settle ACTIVE under no key, got \
                 {status:?} / {reported_key:?}"
            );
            continue;
        }

        assert_eq!(
            created.status, 200,
            "case {case:?}: expected 200, got {created:?}"
        );
        assert_eq!(
            created.string(&["datastoreStatus"]),
            CREATING,
            "case {case:?}: expected a fresh data store to answer {CREATING}, got {created:?}"
        );
        let datastore_id = created.string(&["datastoreId"]);
        let (status, reported_key) = settled_datastore(&router, case, &datastore_id).await;
        assert_eq!(
            status, ACTIVE,
            "case {case:?}: expected the data store to settle {ACTIVE}, got {status:?}"
        );
        assert_eq!(
            reported_key,
            row.kms_key_arn.clone().unwrap_or_default(),
            "case {case:?}: expected the data store to report the key it was created under, \
             got {reported_key:?}"
        );
        let listed = listed_datastore_names(&router, case).await;
        assert!(
            listed.contains(&datastore_name),
            "case {case:?}: expected the data store {datastore_name:?} to be listed, got \
             {listed:?}"
        );
    }
}

/// RULE, the other side of the same resolution: the key is resolved by the CREATE, so a
/// later READ does not resolve it again — a data store goes on reporting its key, and
/// answering reads, after that key is scheduled for deletion, because the key was there
/// when the create was made.
#[tokio::test]
async fn a_created_datastore_outlives_its_keys_scheduled_deletion() {
    let router = router();

    let held = kms_ok(&router, REGION, "CreateKey", &json!({})).await;
    let key_arn = held.string(&["KeyMetadata", "Arn"]);
    let created =
        create_datastore(&router, "kms-key-outlives", "kms-key-outlives", Some(&key_arn)).await;
    assert_eq!(
        created.status, 200,
        "setup: expected the create under a held key to answer 200, got {created:?}"
    );
    let datastore_id = created.string(&["datastoreId"]);
    let (status, reported_key) =
        settled_datastore(&router, "before the deletion", &datastore_id).await;
    assert_eq!(
        (status.as_str(), reported_key.as_str()),
        (ACTIVE, key_arn.as_str()),
        "expected the data store to settle {ACTIVE} under {key_arn}, got {status:?} / \
         {reported_key:?}"
    );

    kms_ok(
        &router,
        REGION,
        "ScheduleKeyDeletion",
        &json!({ "KeyId": key_arn }),
    )
    .await;

    let (status, reported_key) =
        settled_datastore(&router, "after the deletion", &datastore_id).await;
    assert_eq!(
        (status.as_str(), reported_key.as_str()),
        (ACTIVE, key_arn.as_str()),
        "expected the data store to keep reporting {key_arn} and stay {ACTIVE}, got \
         {status:?} / {reported_key:?}"
    );
}
