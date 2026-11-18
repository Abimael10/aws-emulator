// SPDX-License-Identifier: CC0-1.0

//! Parameter Store's reserved `/aws/reference/secretsmanager` front door, pinned over the
//! LIFECYCLE of the secret behind it: Secrets Manager holds the secret, KMS holds the key its
//! value is sealed under, CloudFormation is one of the clients that creates and removes such a
//! secret, and Systems Manager serves the read.
//!
//! RULE under test: a `GetParameter` whose name is under that reserved hierarchy resolves the
//! secret the level below it names and is served for exactly as long as Secrets Manager would
//! serve that secret's value; the moment that service holds no readable secret under the name —
//! it never did, a delete scheduled it for removal, a force delete removed it outright, or the
//! client that created it (a CloudFormation stack, say) was torn down — the same read is refused
//! HTTP 400 `ParameterNotFound`, and restoring the secret makes the identical read resolvable
//! again. So the answer tracks the SOURCE's state in both directions, and a refusal never stands
//! in for a reference the emulator simply does not serve.
//!
//! Only a cross-service suite can pin it: the secret, the key its value opens under and the
//! stack that owns it each belong to a different service, and the front door's answer is the
//! composition of all of them.
//!
//! The rows differ along what the rule generalises over: the form of the secret's name (flat,
//! hierarchical, punctuated) and of its value (a plain token, a JSON document, a multi-line
//! string), and the way the secret stops being readable (scheduled for deletion with a recovery
//! window, force-deleted without one, and — the recorded ordering — removed with the
//! CloudFormation stack that declared it). Every expected value is computed from what the row
//! states, never read back out of the answer being checked.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The long-term key every emulator publishes, which is the account's own credential: no
/// document bounds it, so each read below is decided by the state of the secret alone.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The reserved hierarchy, as a client writes it.
const REFERENCE_ROOT: &str = "/aws/reference/secretsmanager";

/// The `ParameterType` a reference reports, from the modelled enum
/// (`smithy_query(query="shape", shape="ParameterType")`).
const REFERENCE_TYPE: &str = "SecureString";

/// The error code a reference nothing resolves is refused with, and the status that refusal
/// carries (`smithy_query(query="operation", operation="GetParameter")`: `ParameterNotFound`,
/// HTTP 400).
const NOT_FOUND_CODE: &str = "ParameterNotFound";
/// See [`NOT_FOUND_CODE`].
const NOT_FOUND_STATUS: u16 = 400;

/// How long one `DescribeStacks` poll waits before the next, and how many are made: a stack's
/// status transition is owed on its own clock, so the suite waits for the STATUS rather than for
/// a span.
const POLL_INTERVAL: Duration = Duration::from_millis(100);
/// See [`POLL_INTERVAL`].
const POLLS: usize = 300;

/// The four services this lifecycle needs, over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These cases exercise operation behaviour, not request signing: the `Authorization` header
    // carries only the credential scope, which is where each service reads the Region and the
    // caller's access key id.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_ssm_amazonssm::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name` and whose
/// credential is the account's own key — the key a service reads the caller's identity from. The
/// signature is never checked: request authentication is off on this router.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0000"
    )
}

/// One `awsJson1_1` call to a service of this emulator, reported as `(status, the parsed body)`.
async fn json_call(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed awsJson1_1 request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Systems Manager call.
async fn ssm(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    json_call(router, "ssm", "AmazonSSM", operation, body).await
}

/// One Secrets Manager call.
async fn secrets(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    json_call(router, "secretsmanager", "secretsmanager", operation, body).await
}

/// One CloudFormation call over its awsQuery wire, reported as `(status, the XML text)`.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(key));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", &host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// The wire error code of an error response, as the short shape name a client reads.
fn error_code(body: &Value) -> String {
    let raw = body
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    raw.split('#').next_back().unwrap_or(raw).to_string()
}

/// A required string member of a body.
fn text(parsed: &Value, member: &str, case: &str) -> String {
    parsed
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: no {member} in {parsed}"))
        .to_string()
}

/// The reference path a read asks for `name` by.
fn reference_path(name: &str) -> String {
    format!("{REFERENCE_ROOT}/{name}")
}

/// The `ClientRequestToken` a create names its first version by, of the modelled length
/// (`smithy_query(query="shape", shape="SecretVersionIdType")` reads `{"min": 32, "max": 64}`),
/// built from the secret's own name so a failing case names the version it meant. A client that
/// is not an AWS SDK supplies this itself.
fn request_token(seed: &str) -> String {
    let readable: String = seed
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character
            } else {
                '-'
            }
        })
        .collect();
    format!("{readable:0<36}")
}

/// Create a secret through Secrets Manager's own `CreateSecret`, answering its ARN.
async fn given_secret(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    value: &str,
) -> String {
    let (status, created) = secrets(
        router,
        "CreateSecret",
        &json!({
            "Name": name,
            "SecretString": value,
            "ClientRequestToken": request_token(name),
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: setup CreateSecret({name:?}) expected 200, got {status} {created}"
    );
    text(&created, "ARN", case)
}

/// Read the reference for `name`, asking for the secret's bytes, and assert it is served as a
/// parameter of the SECRET's identity holding `value`.
async fn reference_is_served(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    arn: &str,
    value: &str,
) {
    let path = reference_path(name);
    let (status, body) = ssm(
        router,
        "GetParameter",
        &json!({ "Name": path, "WithDecryption": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the live secret's reference {path:?} to be served, got {status} \
         {body}"
    );
    let parameter = &body["Parameter"];
    assert_eq!(
        parameter["Value"].as_str(),
        Some(value),
        "case {case:?}: expected the reference to carry the secret's value {value:?}, got \
         {parameter}"
    );
    assert_eq!(
        parameter["Type"].as_str(),
        Some(REFERENCE_TYPE),
        "case {case:?}: expected the reference to be reported as {REFERENCE_TYPE}, got \
         {parameter}"
    );
    assert_eq!(
        parameter["ARN"].as_str(),
        Some(arn),
        "case {case:?}: expected the reference to carry the secret's ARN {arn:?}, got {parameter}"
    );
    assert_eq!(
        parameter["Name"].as_str(),
        Some(path.as_str()),
        "case {case:?}: expected the reference to be named as asked, got {parameter}"
    );
}

/// Read the reference for `name` and assert it is refused as a reference nothing resolves,
/// whichever way the secret stopped being readable. The refusal is reported with the status and
/// code the model declares, and its message names the reference that could not be resolved.
async fn reference_is_not_found(router: &MultiServiceRouter, case: &str, name: &str) {
    let path = reference_path(name);
    for with_decryption in [true, false] {
        let (status, body) = ssm(
            router,
            "GetParameter",
            &json!({ "Name": path, "WithDecryption": with_decryption }),
        )
        .await;
        let code = error_code(&body);
        assert_eq!(
            (status, code.as_str()),
            (NOT_FOUND_STATUS, NOT_FOUND_CODE),
            "case {case:?} (WithDecryption={with_decryption}): expected \
             {NOT_FOUND_STATUS} {NOT_FOUND_CODE} for the reference {path:?} of a secret nothing \
             holds, got {status} {body}"
        );
    }
}

/// How a row's secret stops being readable through Secrets Manager.
enum Removal {
    /// A `DeleteSecret` stating a recovery window: the secret is only SCHEDULED for deletion,
    /// and while it is "its details, including the encrypted secret value, is not accessible"
    /// (`DeleteSecret`), so the reference resolves to nothing from that instant.
    Scheduled { recovery_window_in_days: i64 },
    /// A `DeleteSecret` asking for no recovery window at all: the secret is gone outright.
    Forced,
}

/// One row of the table: a secret, and the way it stops being readable.
struct Row {
    case: &'static str,
    /// The secret's name, which is also the leaf of its reference path.
    name: &'static str,
    /// The `SecretString` the create stores.
    value: &'static str,
    removal: Removal,
}

const ROWS: &[Row] = &[
    Row {
        case: "a flat name holding a plain token, scheduled for deletion",
        name: "emu-lifecycle-token",
        value: "a-live-token",
        // The shortest window the model admits ("The minimum value is 7 days",
        // `DeleteSecretRequest$RecoveryWindowInDays`).
        removal: Removal::Scheduled {
            recovery_window_in_days: 7,
        },
    },
    Row {
        case: "a hierarchical name holding a JSON document, force-deleted",
        name: "emu/lifecycle/db-credentials",
        value: r#"{"username":"admin","password":"first-one"}"#,
        removal: Removal::Forced,
    },
    Row {
        case: "a punctuated name holding a multi-line value, scheduled for the longest window",
        name: "emu.lifecycle_1",
        value: "line-one\nline-two\nline-three",
        // "The maximum value is 30 days" (`DeleteSecretRequest$RecoveryWindowInDays`).
        removal: Removal::Scheduled {
            recovery_window_in_days: 30,
        },
    },
];

/// RULE: the reference is served while the secret is readable and refused `ParameterNotFound`
/// once it is not, whichever way the secret stopped being readable.
#[tokio::test]
async fn the_reference_is_served_exactly_while_the_secret_is_readable() {
    let router = build_router();

    for row in ROWS {
        let case = row.case;

        // Before anything created it, the reference resolves to nothing: the refusal is what a
        // name the source holds nothing for answers with.
        reference_is_not_found(&router, case, row.name).await;

        let arn = given_secret(&router, case, row.name, row.value).await;
        reference_is_served(&router, case, row.name, &arn, row.value).await;

        let request = match row.removal {
            Removal::Scheduled {
                recovery_window_in_days,
            } => json!({
                "SecretId": row.name,
                "RecoveryWindowInDays": recovery_window_in_days,
            }),
            Removal::Forced => json!({
                "SecretId": row.name,
                "ForceDeleteWithoutRecovery": true,
            }),
        };
        let (status, deleted) = secrets(&router, "DeleteSecret", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the delete of {:?} to be served, got {status} {deleted}",
            row.name
        );

        // The source would no longer serve the value, so neither does the front door — and the
        // two answers agree, which is what keeps a reference from reporting a secret that is on
        // its way out.
        let (source_status, source_body) =
            secrets(&router, "GetSecretValue", &json!({ "SecretId": row.name })).await;
        assert_ne!(
            source_status, 200,
            "case {case:?}: expected Secrets Manager itself to serve no value once the secret is \
             gone, got {source_status} {source_body}"
        );
        reference_is_not_found(&router, case, row.name).await;
    }
}

/// RULE, from the other side: the answer tracks the SOURCE, so a scheduled deletion the client
/// cancels makes the identical reference read resolvable again — the refusal is never a latch.
#[tokio::test]
async fn restoring_a_scheduled_secret_makes_its_reference_resolvable_again() {
    let router = build_router();
    let case = "a secret scheduled for deletion and then restored";
    let name = "emu-lifecycle-restored";
    let value = "a-restored-value";

    let arn = given_secret(&router, case, name, value).await;
    reference_is_served(&router, case, name, &arn, value).await;

    let (status, body) = secrets(
        &router,
        "DeleteSecret",
        &json!({ "SecretId": name, "RecoveryWindowInDays": 7 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the scheduled delete to be served, got {status} {body}"
    );
    reference_is_not_found(&router, case, name).await;

    let (status, body) = secrets(&router, "RestoreSecret", &json!({ "SecretId": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the restore to be served, got {status} {body}"
    );
    reference_is_served(&router, case, name, &arn, value).await;
}

/// The logical id the stack declares its secret under.
const SECRET_LOGICAL_ID: &str = "AppSecret";

/// How many characters the stack's `GenerateSecretString` asks for, which is the length the
/// generated value must have.
const GENERATED_LENGTH: usize = 24;

/// Poll `stack` until it first reports `wanted`, answering the last status read and the
/// `DescribeStacks` body it came from. The transition is owed on the stack's own clock, so
/// nothing is waited out past the first observation.
async fn wait_for_stack_status(
    router: &MultiServiceRouter,
    stack: &str,
    wanted: &str,
) -> (String, String) {
    for _ in 0..POLLS {
        let (status, body) =
            cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
        assert_eq!(
            status, 200,
            "DescribeStacks {stack:?}: expected HTTP 200, got {status} with body {body}"
        );
        let reported = crate::query_wire::opt_element(&body, "StackStatus").unwrap_or_default();
        if reported == wanted {
            return (reported, body);
        }
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    let (_, body) = cloudformation(router, "DescribeStacks", &[("StackName", stack)]).await;
    (
        crate::query_wire::opt_element(&body, "StackStatus").unwrap_or_default(),
        body,
    )
}

/// RULE, as a client meets it end to end: the secret a stack declares is readable through the
/// reference for as long as the stack holds it, and deleting the stack — which deletes the
/// secret in the service that owns it — turns the identical read into `ParameterNotFound`.
#[tokio::test]
async fn a_stacks_secret_is_resolvable_through_the_reference_until_the_stack_is_deleted() {
    let router = build_router();
    let case = "a stack-declared secret read through the reference, then torn down";
    let stack_name = "referencedsecretstack";
    let secret_name = format!("{stack_name}-secret");

    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            SECRET_LOGICAL_ID: {
                "Type": "AWS::SecretsManager::Secret",
                "Properties": {
                    // The recorded template names the secret after the stack it belongs to.
                    "Name": { "Fn::Sub": "${AWS::StackName}-secret" },
                    "GenerateSecretString": {
                        "PasswordLength": GENERATED_LENGTH,
                        "ExcludePunctuation": true
                    }
                }
            }
        },
        "Outputs": { "SecretRef": { "Value": { "Ref": SECRET_LOGICAL_ID } } }
    })
    .to_string();

    let (status, body) = cloudformation(
        &router,
        "CreateStack",
        &[
            ("StackName", stack_name),
            ("TemplateBody", template.as_str()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack, got {status} with body {body}"
    );
    let stack_id = crate::query_wire::opt_element(&body, "StackId")
        .unwrap_or_else(|| panic!("case {case:?}: no StackId in {body}"));

    let (reported, body) = wait_for_stack_status(&router, stack_name, "CREATE_COMPLETE").await;
    assert_eq!(
        reported, "CREATE_COMPLETE",
        "case {case:?}: expected the stack to complete, got {body}"
    );
    let published = crate::query_wire::outputs_of(&body)
        .get("SecretRef")
        .unwrap_or_else(|| panic!("case {case:?}: no SecretRef output in {body}"))
        .clone();

    // The reference the stack's own name builds resolves to the secret the stack created: the
    // value is the one the owning service generated for the declaration, which the reference
    // reports under the secret's published ARN.
    let path = reference_path(&secret_name);
    let (status, read) = ssm(
        &router,
        "GetParameter",
        &json!({ "Name": path, "WithDecryption": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the stack's secret to resolve through {path:?}, got {status} \
         {read}"
    );
    let parameter = &read["Parameter"];
    assert_eq!(
        parameter["ARN"].as_str(),
        Some(published.as_str()),
        "case {case:?}: expected the reference to carry the ARN the stack published, got \
         {parameter}"
    );
    assert_eq!(
        parameter["Type"].as_str(),
        Some(REFERENCE_TYPE),
        "case {case:?}: expected the reference to be reported as {REFERENCE_TYPE}, got \
         {parameter}"
    );
    let served = parameter["Value"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no Value in {parameter}"))
        .to_string();
    assert_eq!(
        served.chars().count(),
        GENERATED_LENGTH,
        "case {case:?}: expected the declared password length, got {served:?}"
    );
    // The front door reports what the SOURCE holds: the same bytes Secrets Manager answers for
    // the secret the stack published.
    let (status, source) = secrets(
        &router,
        "GetSecretValue",
        &json!({ "SecretId": published }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the source read to be served, got {status} {source}"
    );
    assert_eq!(
        source["SecretString"].as_str(),
        Some(served.as_str()),
        "case {case:?}: expected the reference to report the source's own bytes {served:?}, got \
         {source}"
    );

    let (status, body) = cloudformation(&router, "DeleteStack", &[("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {body}"
    );
    // A deleted stack is addressed by its id: CloudFormation answers nothing for the NAME of a
    // stack it no longer holds.
    let (reported, body) = wait_for_stack_status(&router, &stack_id, "DELETE_COMPLETE").await;
    assert_eq!(
        reported, "DELETE_COMPLETE",
        "case {case:?}: expected the stack's deletion to complete, got {body}"
    );

    reference_is_not_found(&router, case, &secret_name).await;
}
