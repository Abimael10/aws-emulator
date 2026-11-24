// SPDX-License-Identifier: CC0-1.0

//! WHO a running Python handler's own SDK resolves as its credentials, and what it SIGNS
//! with, end to end over the wire: Lambda, IAM and the token service on ONE engine, and a
//! `python3.12` function that reads its credentials off the SDK's default chain, asks the
//! token service who it is, and mints an IAM database authentication token for a database
//! login.
//!
//! The rule pinned here: an SDK's DEFAULT credential chain inside a managed runtime resolves
//! the credentials the runtime exported for the invocation — the TEMPORARY credentials of a
//! session of the function's execution role, session token included — and every artefact the
//! SDK signs in the client out of that chain carries the SAME access key id, and the session
//! token beside it. So the identity the token service reports for the chain's credential and
//! the credential scope of a token the client presigns name one session; an execution
//! environment that holds no credential resolves to NO credential rather than an empty one.
//!
//! The rows differ along what the rule generalises over: which execution role the function
//! runs as (two roles, one of them shared by two functions, and one the account's IAM holds no
//! record of), which function the session is named for, and which endpoint, port and database
//! user the token is minted for.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::handler_bedrock_agent_sdk::execution_role;
use crate::handler_s3_object_access::{role_arn, session_arn};
use crate::invoke_request_id::one_invocation_stream_at_a_time;
use crate::synchronous_invoke_run::package;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The emulator's own endpoint, as the `Host` of every call this suite makes to Lambda.
const LAMBDA_HOST: &str = "lambda.us-east-1.amazonaws.com";

/// How long this suite leaves between two readings of a status the clock owes. Every wait
/// below looks again for the CONDITION rather than sleeping for a span.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a status the clock owes is waited for before the wait itself is the failure.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// The query field a presigned SigV4 request carries a TEMPORARY credential's session token
/// in, as every AWS SDK's own presigner writes it.
const SESSION_TOKEN_FIELD: &str = "X-Amz-Security-Token";

/// Lambda, IAM and the token service over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: the handler's own calls are dispatched back into this same router, so the
/// session it signs with is the session this emulator minted for the role IAM holds.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. Nothing in it is verified by a router built with authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda `restJson1` call, as its status, headers and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{LAMBDA_HOST}{uri}"))
        .header("host", LAMBDA_HOST)
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_vec()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// The inline identity policy each held role below carries: the grant a database login under
/// IAM database authentication needs.
fn database_connect_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": "rds-db:connect", "Resource": "*" }],
    })
    .to_string()
}

/// The handler every row runs: it resolves its credentials off the SDK's DEFAULT chain
/// (`boto3.Session()` and the lower half's own session, which name no credentials of their
/// own), asks the token service who that credential speaks for, and mints a database
/// authentication token through the RDS client — reporting the key the chain resolved, whether
/// a session token stands behind it, the identity and the token.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.session


def lambda_handler(event, context):
    session = boto3.Session()
    held = session.get_credentials()
    lower = botocore.session.get_session().get_credentials()
    if held is None or lower is None:
        return {"resolved": False}
    frozen = held.get_frozen_credentials()
    token = session.client("rds").generate_db_auth_token(
        DBHostname=event["host"], Port=event["port"], DBUsername=event["user"]
    )
    return {
        "resolved": True,
        "arn": session.client("sts").get_caller_identity()["Arn"],
        "access_key_id": frozen.access_key,
        "has_session_token": bool(frozen.token),
        "lower_access_key_id": lower.get_frozen_credentials().access_key,
        "lower_has_session_token": bool(lower.get_frozen_credentials().token),
        "token": token,
    }
"#;

/// Create one function and wait for the state an invocation needs, reading the CONDITION back
/// through the function's own configuration rather than sleeping for a span.
async fn create_function(router: &MultiServiceRouter, case: &str, name: &str, role: &str) {
    let (status, _headers, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &serde_json::to_vec(&json!({
            "FunctionName": name,
            "Role": role,
            "Runtime": "python3.12",
            "Handler": "lambda_function.lambda_handler",
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        }))
        .expect("the create body serializes"),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: expected HTTP 201 from CreateFunction, got {status} ({created})"
    );
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let (status, _headers, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &[],
        )
        .await;
        if status == 200 && configuration["State"] == json!("Active") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: the function never became Active: {status} ({configuration})"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// `Invoke` the function synchronously with `event`, answering the payload the run produced.
async fn invoke(router: &MultiServiceRouter, case: &str, name: &str, event: &Value) -> Value {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{name}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} ({payload})"
    );
    assert_eq!(
        headers.get("x-amz-function-error"),
        None,
        "case {case:?}: the handler must run to completion, got {payload}"
    );
    payload
}

/// One row: the function, the role it runs as, and the database login it mints a token for.
struct Row {
    case: &'static str,
    function: &'static str,
    role: &'static str,
    /// Whether this account's IAM holds a record of that role, which is what decides whether
    /// a session of it can be minted at all.
    role_is_held: bool,
    host: &'static str,
    port: u32,
    user: &'static str,
}

/// The two roles the held rows run as: one shared by two functions — so the session names
/// differ while the role does not — and one of its own.
const ROLE: &str = "db-auth-token-exec";
const OTHER_ROLE: &str = "db-auth-token-other-exec";

/// The role no record is held for, which is what a function whose execution role was
/// provisioned outside this emulator looks like here.
const UNHELD_ROLE: &str = "db-auth-token-unheld-exec";

const ROWS: &[Row] = &[
    Row {
        case: "a proxy endpoint on the MySQL port",
        function: "db-auth-token-first",
        role: ROLE,
        role_is_held: true,
        host: "orders.proxy-abcdef012345.us-east-1.rds.amazonaws.com",
        port: 3306,
        user: "app_user",
    },
    Row {
        // The same role, another function: the session a run's credentials speak for is named
        // for the FUNCTION, so this row's identity is a different session of the same role.
        case: "another function of the same execution role, on the PostgreSQL port",
        function: "db-auth-token-second",
        role: ROLE,
        role_is_held: true,
        host: "reports.cluster-abcdef012345.us-east-1.rds.amazonaws.com",
        port: 5432,
        user: "reporting",
    },
    Row {
        // Another role, and a user name a query string has to escape.
        case: "a function of another execution role, and a user name a query escapes",
        function: "db-auth-token-third",
        role: OTHER_ROLE,
        role_is_held: true,
        host: "127.0.0.1",
        port: 15_432,
        user: "iam user+1",
    },
    Row {
        // Nothing is known about the role, so no session is minted and the run is given no
        // credential: the default chain resolves none rather than an empty one.
        case: "a function whose execution role this account's IAM holds no record of",
        function: "db-auth-token-unheld",
        role: UNHELD_ROLE,
        role_is_held: false,
        host: "orders.proxy-abcdef012345.us-east-1.rds.amazonaws.com",
        port: 3306,
        user: "app_user",
    },
];

/// The fields the query string of a token carries, by name.
fn fields(token: &str) -> Vec<(String, String)> {
    let query = token.split_once("/?").map(|(_, query)| query).unwrap_or("");
    form_urlencoded::parse(query.as_bytes())
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect()
}

/// The one value of the field `name`.
fn field(token: &str, name: &str) -> Option<String> {
    fields(token)
        .into_iter()
        .find(|(held, _)| held == name)
        .map(|(_, value)| value)
}

/// RULE: the SDK's default credential chain inside a run resolves the temporary credentials of
/// the session of the function's execution role, and what the client presigns out of that
/// chain carries the same access key id and the session token beside it.
#[tokio::test]
async fn a_handler_signs_a_database_token_with_its_own_execution_role_session() {
    let _stream = one_invocation_stream_at_a_time().await;
    let router = emulator();
    execution_role(&router, ROLE, &database_connect_policy()).await;
    execution_role(&router, OTHER_ROLE, &database_connect_policy()).await;
    for row in ROWS {
        create_function(&router, row.case, row.function, &role_arn(row.role)).await;
    }

    for row in ROWS {
        let case = row.case;
        let payload = invoke(
            &router,
            case,
            row.function,
            &json!({ "host": row.host, "port": row.port, "user": row.user }),
        )
        .await;
        if !row.role_is_held {
            // No session, so no credential: the chain resolves nothing at all.
            assert_eq!(
                payload,
                json!({ "resolved": false }),
                "case {case:?}: expected the default chain to resolve no credential, got \
                 {payload}"
            );
            continue;
        }
        assert_eq!(
            payload["resolved"],
            json!(true),
            "case {case:?}: expected the default chain to resolve the run's credential, got \
             {payload}"
        );

        // The credential is TEMPORARY, and both halves of the SDK resolve the one the runtime
        // exported.
        let access_key_id = payload["access_key_id"].as_str().unwrap_or_default();
        assert!(
            !access_key_id.is_empty(),
            "case {case:?}: expected the chain to resolve an access key id, got {payload}"
        );
        assert_eq!(
            payload["has_session_token"],
            json!(true),
            "case {case:?}: a role session carries a session token, got {payload}"
        );
        assert_eq!(
            payload["lower_access_key_id"].as_str().unwrap_or_default(),
            access_key_id,
            "case {case:?}: expected both halves of the SDK to resolve one credential, got \
             {payload}"
        );
        assert_eq!(
            payload["lower_has_session_token"],
            json!(true),
            "case {case:?}: a role session carries a session token, got {payload}"
        );

        // And it is the session of THIS function's execution role, named for the function.
        let wanted = session_arn(row.role, row.function);
        assert_eq!(
            payload["arn"].as_str().unwrap_or_default(),
            wanted,
            "case {case:?}: expected the chain's credential to speak for {wanted}, got {payload}"
        );

        // The token addresses the login the handler named, as the connect action.
        let token = payload["token"].as_str().unwrap_or_default().to_string();
        let addressed = format!("{}:{}/?Action=connect", row.host, row.port);
        assert!(
            token.starts_with(&addressed),
            "case {case:?}: expected the token to address {addressed:?}, got {token:?}"
        );
        assert_eq!(
            field(&token, "DBUser").as_deref(),
            Some(row.user),
            "case {case:?}: expected the token to name the database user, got {token:?}"
        );

        // And it is signed BY THAT SESSION: the credential scope names the very key the
        // environment exposed, and the session token travels with it.
        let credential = field(&token, "X-Amz-Credential").unwrap_or_default();
        assert_eq!(
            credential.split('/').next(),
            Some(access_key_id),
            "case {case:?}: expected the token's credential scope to name the key the chain \
             resolved, got {credential:?}"
        );
        assert!(
            token.contains(&format!("X-Amz-Credential={access_key_id}%2F")),
            "case {case:?}: expected the token to carry the resolved key in its scope, got \
             {token:?}"
        );
        let carried = field(&token, SESSION_TOKEN_FIELD).unwrap_or_default();
        assert!(
            !carried.is_empty(),
            "case {case:?}: expected the presigned token to carry {SESSION_TOKEN_FIELD}, got \
             {token:?}"
        );
        let signature = field(&token, "X-Amz-Signature").unwrap_or_default();
        assert!(
            signature.len() == 64 && signature.chars().all(|c| c.is_ascii_hexdigit()),
            "case {case:?}: expected a SigV4 signature, got {signature:?}"
        );
    }
}
