// SPDX-License-Identifier: CC0-1.0

//! What admits Amazon Cognito's invocation of a user-pool trigger: the FUNCTION's own
//! resource-based policy.
//!
//! RULE: Amazon Cognito invokes the function a pool's `LambdaConfig` names as its OWN service
//! principal rather than as the caller of the operation that fired the trigger, so the only
//! document that can admit the invocation is that function's resource-based policy — the one
//! `AddPermission` writes — allowing `lambda:InvokeFunction` to `cognito-idp.amazonaws.com`,
//! optionally narrowed by the `aws:SourceArn`/`aws:SourceAccount` of the POOL the invocation
//! is made for. A function whose policy admits this service nothing is NOT invoked: the
//! operation that fired the trigger answers its modelled `UnexpectedLambdaException` naming
//! the trigger and the class the refusal came back as, and the trigger's own effect does not
//! happen.
//!
//! The rows span what that rule generalises over — every part of the statement the decision
//! turns on, on both sides of it: no policy at all, the grant unscoped, the grant narrowed to
//! the pool's own ARN, narrowed to another pool's ARN, narrowed to the account that owns the
//! pool, narrowed to another account, a grant of every action of the service, a grant of
//! ANOTHER action, and the grant made to another service's principal. Every row drives the
//! same `SignUp` against a pool whose `PreSignUp` trigger names that row's own function, so
//! the policy is the only thing that can decide it, and every row asserts BOTH halves: what
//! the operation answered, and whether the pool then holds the account the trigger gates.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The local name of Cognito's Smithy shape, which is the `X-Amz-Target` prefix.
const COGNITO_TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// Where Lambda's `CreateFunction` is bound, and where `AddPermission` writes a function's
/// resource-based policy.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN, satisfying Lambda's `Role` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The service principal Amazon Cognito invokes a user-pool trigger as, which is what a
/// grant on the function's policy names ("for Amazon Web Services services, the principal is
/// a domain-style identifier that the service defines", Lambda's `AddPermission$Principal`).
pub(crate) const COGNITO_PRINCIPAL: &str = "cognito-idp.amazonaws.com";

/// The action invoking a function performs, as `AddPermission$Action` spells one
/// (`lambda:<Operation>`): Lambda's `Invoke` "requires permission for the
/// `lambda:InvokeFunction` action".
pub(crate) const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The `StatementId` the grant below is written under.
pub(crate) const STATEMENT_ID: &str = "cognito-idp-trigger-invoke";

/// Another action of the same service, which is not the one invoking a function performs.
const READ_ACTION: &str = "lambda:GetFunction";

/// Another service's principal, which is not the one this service invokes a trigger as.
const OTHER_PRINCIPAL: &str = "events.amazonaws.com";

/// The error class an invocation no policy in force on the function allows is refused with,
/// which is what the operation that fired the trigger reports.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// An account that is not the one this emulator serves, for the row whose grant is narrowed
/// to a different `aws:SourceAccount`.
const OTHER_ACCOUNT: &str = "210987654321";

/// A pool ARN of this account that is not the pool the trigger fires for, for the row whose
/// grant is narrowed to a different `aws:SourceArn`. It is never created: `aws:SourceArn` is
/// a value a statement is written against, not a resource the grant resolves.
const OTHER_POOL_ARN: &str = "arn:aws:cognito-idp:us-east-1:000000000000:userpool/us-east-1_other";

/// How long this suite waits for a created function to become `Active`, and how often it
/// looks again. Generous: the wait is for the condition, and a loaded host takes far longer
/// than an idle one.
const ACTIVE_BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// Cognito and Lambda over ONE shared engine, as `core/router/src/lib.rs` mounts them, so
/// Cognito's in-process callback reaches this very Lambda.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked here.
fn authorization(service: &str, region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{region}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda `restJson1` call, as its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = match body {
        Some(body) => Bytes::from(body.to_string()),
        None => Bytes::new(),
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{region}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda", region))
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Cognito call: `awsJson1_1`, addressed by `X-Amz-Target`.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cognito-idp.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("{COGNITO_TARGET_PREFIX}.{operation}"),
        )
        .header("authorization", authorization("cognito-idp", REGION))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Cognito call that must answer 200, with its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = cognito(router, operation, body).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The error code of an error body (`__type`), without its namespace, and its message.
fn refusal(body: &Value) -> (String, String) {
    (
        body.get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string(),
        body.get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
    )
}

/// A .zip deployment package holding `source` as `index.py`, the module a `Handler` of
/// `index.handler` names.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("index.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// The handler every row's function runs: a pre-sign-up function that answers the event it
/// was handed, which is what lets the registration through.
const PASSING_HANDLER: &str = r#"
def handler(event, context):
    return event
"#;

/// Create the `python3.12` function `name` and answer the ARN Lambda minted for it. The
/// function carries NO resource-based policy: each row writes its own.
async fn create_function(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = lambda(
        router,
        REGION,
        "POST",
        FUNCTIONS_URI,
        Some(&json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Timeout": 60,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(PASSING_HANDLER)),
            },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreateFunction({name:?}) must answer 201, got {status} with {created}"
    );
    created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateFunction must mint an ARN, got {created}"))
        .to_string()
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs: the
/// transition is the CLOCK's, so this looks again until it holds rather than sleeping a
/// fixed span.
async fn await_active(router: &MultiServiceRouter, name: &str) {
    let deadline = Instant::now() + ACTIVE_BUDGET;
    loop {
        let (_, read) = lambda(
            router,
            REGION,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            None,
        )
        .await;
        if read["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the function {name:?} never became Active within {ACTIVE_BUDGET:?}, got {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Write one `AddPermission` statement on `name`'s resource-based policy, as `body` states
/// it.
async fn add_permission(router: &MultiServiceRouter, region: &str, name: &str, body: &Value) {
    let (status, written) = lambda(
        router,
        region,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/policy"),
        Some(body),
    )
    .await;
    assert_eq!(
        status, 201,
        "AddPermission({name:?}) must answer 201, got {status} with {written}"
    );
}

/// Let Amazon Cognito invoke the trigger function `name`: the grant AWS asks for on every
/// function a pool's `LambdaConfig` names, written through Lambda's own `AddPermission`.
///
/// `pub(crate)` because EVERY suite of this crate that has a pool's trigger invoke a real
/// function needs that grant in force — Cognito invokes as its own service principal, so
/// without it the function admits the invocation nothing (this suite's own rule) — and one
/// spelling of it means no two suites can write two different grants.
pub(crate) async fn grant_cognito_trigger_invoke(
    router: &MultiServiceRouter,
    region: &str,
    name: &str,
) {
    add_permission(
        router,
        region,
        name,
        &json!({
            "StatementId": STATEMENT_ID,
            "Action": INVOKE_ACTION,
            "Principal": COGNITO_PRINCIPAL,
        }),
    )
    .await;
}

/// Create a pool whose `PreSignUp` trigger names `function_arn`, with an app client of it,
/// and answer the pool's id, its ARN and the client id.
async fn pool_with_trigger(
    router: &MultiServiceRouter,
    name: &str,
    function_arn: &str,
) -> (String, String, String) {
    let created = ok(
        router,
        "CreateUserPool",
        &json!({
            "PoolName": name,
            "LambdaConfig": { "PreSignUp": function_arn },
        }),
    )
    .await;
    let pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool must mint an id, got {created}"))
        .to_string();
    let pool_arn = created["UserPool"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool must report the pool's ARN, got {created}"))
        .to_string();
    let client = ok(
        router,
        "CreateUserPoolClient",
        &json!({ "UserPoolId": pool_id, "ClientName": format!("{name}-app") }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPoolClient must mint a client id, got {client}"))
        .to_string();
    (pool_id, pool_arn, client_id)
}

/// How one row's statement is written on its function's policy, or that the function carries
/// no policy at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// No statement at all: the function carries no resource-based policy.
    None,
    /// The invoking action for this service's principal, narrowed to no source.
    Unscoped,
    /// The same, narrowed to the POOL the trigger fires for.
    ThisPool,
    /// The same, narrowed to another pool of the account.
    AnotherPool,
    /// The same, narrowed to the account that owns the pool.
    ThisAccount,
    /// The same, narrowed to another account.
    AnotherAccount,
    /// Every action of the service, for this service's principal.
    EveryAction,
    /// Another action of the service, for this service's principal.
    AnotherAction,
    /// The invoking action, for another service's principal.
    AnotherPrincipal,
}

impl Grant {
    /// The `AddPermission` body this grant is written as for the pool `pool_arn`, or `None`
    /// for the row that writes no statement.
    fn stated(self, pool_arn: &str) -> Option<Value> {
        let mut body = json!({
            "StatementId": STATEMENT_ID,
            "Action": INVOKE_ACTION,
            "Principal": COGNITO_PRINCIPAL,
        });
        match self {
            Self::None => return None,
            Self::Unscoped => {}
            Self::ThisPool => body["SourceArn"] = json!(pool_arn),
            Self::AnotherPool => body["SourceArn"] = json!(OTHER_POOL_ARN),
            Self::ThisAccount => body["SourceAccount"] = json!(engine::emulator_account_id()),
            Self::AnotherAccount => body["SourceAccount"] = json!(OTHER_ACCOUNT),
            Self::EveryAction => body["Action"] = json!("lambda:*"),
            Self::AnotherAction => body["Action"] = json!(READ_ACTION),
            Self::AnotherPrincipal => body["Principal"] = json!(OTHER_PRINCIPAL),
        }
        Some(body)
    }
}

/// One row: the grant its function carries, and whether that grant admits the invocation.
struct Row {
    case: &'static str,
    grant: Grant,
    admits: bool,
}

/// The rows, one function and one pool each.
const ROWS: &[Row] = &[
    Row {
        case: "a function carrying no resource-based policy at all",
        grant: Grant::None,
        admits: false,
    },
    Row {
        case: "the invoking action for this service, narrowed to no source",
        grant: Grant::Unscoped,
        admits: true,
    },
    Row {
        case: "the invoking action narrowed to the pool the trigger fires for",
        grant: Grant::ThisPool,
        admits: true,
    },
    Row {
        case: "the invoking action narrowed to another pool",
        grant: Grant::AnotherPool,
        admits: false,
    },
    Row {
        case: "the invoking action narrowed to the account that owns the pool",
        grant: Grant::ThisAccount,
        admits: true,
    },
    Row {
        case: "the invoking action narrowed to another account",
        grant: Grant::AnotherAccount,
        admits: false,
    },
    Row {
        case: "every action of the service for this service's principal",
        grant: Grant::EveryAction,
        admits: true,
    },
    Row {
        case: "another action of the service for this service's principal",
        grant: Grant::AnotherAction,
        admits: false,
    },
    Row {
        case: "the invoking action for another service's principal",
        grant: Grant::AnotherPrincipal,
        admits: false,
    },
];

/// The sentence a trigger Amazon Cognito could not invoke carries: the trigger's name and
/// the CLASS the refusal came back as — written out here rather than built from the service's
/// own helper, so the wording has something to be wrong about.
fn not_permitted_message() -> String {
    format!("PreSignUp invocation failed due to error {ACCESS_DENIED}.")
}

/// Every row: the function's own policy decides whether the pool's trigger is invoked, and
/// what the registration the trigger gates leaves behind follows that decision.
#[tokio::test]
async fn a_trigger_is_invoked_only_where_the_functions_policy_admits_this_service() {
    let router = router();

    // Every function is created first and waited for afterwards: the `Active` transition is
    // the clock's, so the waits overlap instead of adding up.
    let mut functions = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("trigger-grant-{index}");
        let arn = create_function(&router, &name).await;
        functions.push((name, arn, row));
    }
    for (name, _, _) in &functions {
        await_active(&router, name).await;
    }

    for (index, (name, function_arn, row)) in functions.iter().enumerate() {
        let case = row.case;
        let (pool_id, pool_arn, client_id) =
            pool_with_trigger(&router, &format!("trigger-grant-pool-{index}"), function_arn).await;
        if let Some(statement) = row.grant.stated(&pool_arn) {
            add_permission(&router, REGION, name, &statement).await;
        }

        let username = format!("registrant{index}");
        let (status, answered) = cognito(
            &router,
            "SignUp",
            &json!({
                "ClientId": client_id,
                "Username": username,
                "Password": GOOD_PASSWORD,
            }),
        )
        .await;

        if row.admits {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the registration to be served, got {status} with \
                 {answered}"
            );
        } else {
            let (code, message) = refusal(&answered);
            assert_eq!(
                (status, code.as_str()),
                (400, "UnexpectedLambdaException"),
                "case {case:?}: expected the trigger to fail as uninvokable, got {status} \
                 {code:?} with {answered}"
            );
            assert_eq!(
                message,
                not_permitted_message(),
                "case {case:?}: expected the refusal to name the trigger and the class the \
                 invocation was refused with, got {answered}"
            );
        }

        // The other half: the registration this trigger GATES happened only where the
        // invocation was admitted, so a refused row registered nobody.
        let (status, read) = cognito(
            &router,
            "AdminGetUser",
            &json!({ "UserPoolId": pool_id, "Username": username }),
        )
        .await;
        let held = status == 200;
        assert_eq!(
            held, row.admits,
            "case {case:?}: expected the pool to hold the account={}, got {status} with {read}",
            row.admits
        );
    }
}
