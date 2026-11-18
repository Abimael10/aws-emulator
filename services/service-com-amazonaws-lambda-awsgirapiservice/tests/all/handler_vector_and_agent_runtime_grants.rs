// SPDX-License-Identifier: CC0-1.0

//! ONE handler reaching TWO services through the AWS SDK its runtime supplies, and what the
//! grant in force on its execution role decides: AWS Lambda, the vector store, the Bedrock
//! Agent runtime plane, IAM and the token service on one engine, and a `python3.12` function
//! whose code builds a client for each at module scope and lists through both.
//!
//! RULE: a managed `python3.x` runtime puts an AWS SDK on the handler's import path carrying
//! the service data of EVERY service the runtime's call seam binds, so a client the handler
//! builds for one is CONSTRUCTED and its call dispatched to that service — and the only thing
//! that decides the outcome is the identity-based policy in force on the function's execution
//! role: the operation's own answer where it allows the action, and that service's modelled
//! 403 `AccessDeniedException` where it does not. A client-side failure of the SDK itself
//! (`botocore.exceptions.UnknownServiceError`, raised before anything is signed) is neither
//! outcome: a role that merely lacks a grant would then be indistinguishable from a runtime
//! whose SDK has no model for the service, and a handler whose role DOES carry the grant could
//! never make the call at all.
//!
//! The rows differ along what the rule generalises over: WHICH grant is in force when the
//! handler calls (neither action, one of them, the other, both, and both named by a
//! service-wide wildcard rather than by action), and therefore which of the two calls of the
//! SAME invocation is served and which is refused. The two calls themselves differ in the
//! service they reach, the IAM action prefix they are decided against (`s3vectors:` and
//! `bedrock:`), and the resource the decision names — an account-level listing that names no
//! resource at all, and one that names every session of the account — so each row asserts the
//! refusal sentence of both shapes.
//!
//! Each served listing is asserted against the state the OWNING service holds, seeded through
//! that service's own API and read back through it afterwards: the handler reports the buckets
//! the vector store holds and the sessions the agent runtime plane holds, and the two counts
//! differ, so a listing answered out of the wrong service's store is visible.
//!
//! The suite RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{await_function_active, call, router_on};
// The role, grant and refusal helpers a handler's own calls are decided by, owned by the
// suite that first needed them rather than written again here.
use crate::handler_bedrock_agent_sdk::{
    allow, denial, execution_role, logs_only, role_policy, session_principal, REGION,
};
// The one .zip writer this crate's suites package a handler with, exported by the suite that
// first needed it rather than written again here.
use crate::node_invoke_run::package;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The role the function runs as, and the function's own name.
const ROLE_NAME: &str = "lambda-vector-and-agent-lister";
const FUNCTION: &str = "vector-and-agent-lister";

/// The access-key id standing for the account's own published credential, which is what this
/// suite's own setup and read-back calls present.
const ACCOUNT_KEY: &str = "AKIDEMULATOR";

/// The two IAM actions the handler's two calls are decided against, as a policy statement
/// spells them: the service's IAM prefix and the operation's own name.
const LIST_BUCKETS_ACTION: &str = "s3vectors:ListVectorBuckets";
const LIST_SESSIONS_ACTION: &str = "bedrock:ListSessions";

/// The same two grants written as every action of each service, which is the other form a
/// policy states them in.
const VECTORS_WILDCARD: &str = "s3vectors:*";
const BEDROCK_WILDCARD: &str = "bedrock:*";

/// The error code and HTTP status AWS states an authorization failure with, which both
/// services' models declare for these two listings (`AccessDeniedException`, HTTP 403).
const ACCESS_DENIED: &str = "AccessDeniedException";
const ACCESS_DENIED_STATUS: i64 = 403;

/// The exception class the SDK raises for a token it carries no service data for, which
/// neither of these two tokens may ever be raised as.
const UNKNOWN_SERVICE: &str = "UnknownServiceError";

/// The vector buckets this suite seeds through the vector store's own `CreateVectorBucket`,
/// which are the buckets the handler's listing has to report. `VectorBucketName` is
/// `@length(min: 3, max: 63)` with pattern `^[a-z0-9][a-z0-9-]{1,61}[a-z0-9]$`.
const BUCKETS: [&str; 2] = ["handler-embeddings-a", "handler-embeddings-b"];

/// How many sessions this suite seeds through the agent runtime plane's own `CreateSession`.
/// A different count from the buckets above, so a listing answered out of the other service's
/// store cannot pass.
const SESSIONS: usize = 3;

/// The page the handler asks the session listing for, inside the model's own bound
/// (`ListSessions$maxResults`, `@range(min: 1, max: 1000)`) and above the count seeded above,
/// so one page carries every session.
const SESSION_PAGE: u64 = 50;

/// Lambda, the vector store, the Bedrock Agent runtime plane, IAM and the token service over
/// ONE shared engine, as `core/router/src/lib.rs` mounts them: the handler's SDK calls are
/// dispatched back into this same router, so what it lists is what these services hold.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_s3vectors_s3vectors::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_bedrockagentruntime_amazonbedrockagentruntimeservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. Nothing here is signed: this suite's own calls present the account's own published
/// credential, which is the position an account's root credential is in.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={ACCOUNT_KEY}/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One `restJson1` call made as a client makes it, answered as the status and the document it
/// carried. `signing_name` is the credential scope that routes it and `host` the authority it
/// is addressed at.
async fn rest_json(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    method: &str,
    path: &str,
    body: &Value,
) -> (u16, Value) {
    let payload = if body.is_null() {
        bytes::Bytes::new()
    } else {
        bytes::Bytes::from(body.to_string())
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization(signing_name))
        .body(payload)
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let read = response.body();
    let parsed: Value = if read.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(read).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One vector-store call that must be served. Every operation of that service binds
/// `POST /<OperationName>` with its input as the JSON body.
async fn vectors(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let host = format!("s3vectors.{REGION}.amazonaws.com");
    let (status, parsed) =
        rest_json(router, "s3vectors", &host, "POST", &format!("/{operation}"), body).await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from the vector store's {operation}, got {status} ({parsed})"
    );
    parsed
}

/// One session opened through the agent runtime plane's own `CreateSession`, which the model
/// binds as `PUT /sessions/` answering 201.
async fn create_session(router: &MultiServiceRouter) -> Value {
    let host = format!("bedrock-agent-runtime.{REGION}.amazonaws.com");
    let (status, parsed) =
        rest_json(router, "bedrock", &host, "PUT", "/sessions/", &json!({})).await;
    assert_eq!(
        status, 201,
        "setup: expected HTTP 201 from CreateSession, got {status} ({parsed})"
    );
    parsed
}

/// The sessions the agent runtime plane itself holds, read through its own `ListSessions`,
/// which the model binds as `POST /sessions/` with `maxResults` as a query parameter.
async fn held_sessions(router: &MultiServiceRouter) -> Value {
    let host = format!("bedrock-agent-runtime.{REGION}.amazonaws.com");
    let (status, parsed) = rest_json(
        router,
        "bedrock",
        &host,
        "POST",
        &format!("/sessions/?maxResults={SESSION_PAGE}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected HTTP 200 from ListSessions, got {status} ({parsed})"
    );
    parsed
}

/// The handler every row runs: it holds a client for each service at MODULE scope — where a
/// real function builds one, and where a runtime whose SDK carried no model for the service
/// would fail before any handler code ran — and reports, per service, what its listing
/// answered or what refusal it was told about.
const HANDLER_SOURCE: &str = r#"
import boto3
import botocore.exceptions

VECTORS = boto3.client("s3vectors")
AGENT_RUNTIME = boto3.client("bedrock-agent-runtime")


def _reported(listing):
    """What one listing answered, or what the SDK told the handler instead."""
    try:
        names = listing()
    except botocore.exceptions.ClientError as error:
        return {
            "ok": False,
            "raised": "ClientError",
            "error_code": error.response["Error"]["Code"],
            "message": error.response["Error"]["Message"],
            "status": error.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    except botocore.exceptions.BotoCoreError as error:
        return {"ok": False, "raised": type(error).__name__, "message": str(error)}
    return {"ok": True, "names": names, "count": len(names)}


def _buckets():
    answered = VECTORS.list_vector_buckets()
    return [bucket["vectorBucketName"] for bucket in answered.get("vectorBuckets") or []]


def _sessions(page):
    answered = AGENT_RUNTIME.list_sessions(maxResults=page)
    return [summary["sessionId"] for summary in answered.get("sessionSummaries") or []]


def lambda_handler(event, context):
    page = event["sessionPage"]
    return {
        "s3vectors": _reported(_buckets),
        "bedrock": _reported(lambda: _sessions(page)),
    }
"#;

/// One row: the grant in force on the execution role when the handler calls, and whether each
/// of its two listings is served under it.
struct Row {
    case: &'static str,
    /// The actions the role's inline identity policy allows beside the logging ones every
    /// execution role carries.
    granted: &'static [&'static str],
    /// Whether the vector store's listing is served under that grant.
    buckets_served: bool,
    /// Whether the agent runtime plane's listing is served under it.
    sessions_served: bool,
}

/// The rows, spanning every combination of the two grants plus the wildcard form of both.
const ROWS: &[Row] = &[
    Row {
        case: "a role granted neither listing action",
        granted: &[],
        buckets_served: false,
        sessions_served: false,
    },
    Row {
        case: "a role granted the vector store's listing action only",
        granted: &[LIST_BUCKETS_ACTION],
        buckets_served: true,
        sessions_served: false,
    },
    Row {
        case: "a role granted the agent runtime plane's listing action only",
        granted: &[LIST_SESSIONS_ACTION],
        buckets_served: false,
        sessions_served: true,
    },
    Row {
        case: "a role granted both listing actions by name",
        granted: &[LIST_BUCKETS_ACTION, LIST_SESSIONS_ACTION],
        buckets_served: true,
        sessions_served: true,
    },
    Row {
        case: "a role granted both services' actions by wildcard",
        granted: &[VECTORS_WILDCARD, BEDROCK_WILDCARD],
        buckets_served: true,
        sessions_served: true,
    },
];

/// The policy document one row puts in force: the logging grant every execution role carries,
/// beside the actions the row names.
fn policy_for(row: &Row) -> String {
    if row.granted.is_empty() {
        return logs_only();
    }
    let mut actions: Vec<&str> = vec![
        "logs:CreateLogGroup",
        "logs:CreateLogStream",
        "logs:PutLogEvents",
    ];
    actions.extend_from_slice(row.granted);
    allow(&actions)
}

/// The resource the session listing's decision names: every session of this account in the
/// Region the invocation's client calls in, as an identity-based policy for
/// `bedrock:ListSessions` names one.
fn every_session_arn() -> String {
    format!(
        "arn:aws:bedrock:{REGION}:{}:session/*",
        engine::emulator_account_id()
    )
}

/// The sentence an authorization refusal of an ACCOUNT-LEVEL action is worded in: such an
/// action names no resource, so its refusal states no resource clause.
fn denial_without_resource(principal: &str, action: &str) -> String {
    format!(
        "User: {principal} is not authorized to perform: {action} because no identity-based \
         policy allows the {action} action"
    )
}

/// The names one listing of the handler's reported, sorted, so a listing that states no order
/// of its own is compared as the set it is.
fn reported_names(reported: &Value) -> Vec<String> {
    let mut names: Vec<String> = reported["names"]
        .as_array()
        .map(|carried| {
            carried
                .iter()
                .filter_map(|name| name.as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// Assert one of the two listings came back as the row says it must: served with the names the
/// owning service holds, or refused with that service's own 403 and its own sentence.
fn assert_listing(
    case: &str,
    label: &str,
    reported: &Value,
    served: bool,
    expected_names: &[String],
    expected_message: &str,
) {
    // Whatever the grant, the call REACHED a service: the SDK raised no client-side failure of
    // its own, which is what a runtime whose SDK carries no model for the service looks like
    // from inside a handler.
    assert_ne!(
        reported["raised"].as_str(),
        Some(UNKNOWN_SERVICE),
        "case {case:?}: expected the {label} client to be built and its call dispatched, got \
         {reported:?}"
    );
    if served {
        assert_eq!(
            reported["ok"],
            json!(true),
            "case {case:?}: expected the {label} listing to be served, got {reported:?}"
        );
        assert_eq!(
            reported_names(reported),
            expected_names,
            "case {case:?}: expected the {label} listing to report {expected_names:?}, got \
             {reported:?}"
        );
        return;
    }
    assert_eq!(
        reported["ok"],
        json!(false),
        "case {case:?}: expected the {label} listing to be refused, got {reported:?}"
    );
    assert_eq!(
        reported["raised"],
        json!("ClientError"),
        "case {case:?}: expected the refusal to reach the handler as the SDK's service error, \
         got {reported:?}"
    );
    assert_eq!(
        reported["error_code"],
        json!(ACCESS_DENIED),
        "case {case:?}: expected the {label} refusal {ACCESS_DENIED:?}, got {reported:?}"
    );
    assert_eq!(
        reported["status"],
        json!(ACCESS_DENIED_STATUS),
        "case {case:?}: expected HTTP {ACCESS_DENIED_STATUS} for the {label} refusal, got \
         {reported:?}"
    );
    assert_eq!(
        reported["message"],
        json!(expected_message),
        "case {case:?}: expected the {label} refusal to be IAM's own sentence, got {reported:?}"
    );
}

/// RULE: the SDK a managed `python3.x` runtime supplies carries the service data of every
/// service the runtime's call seam binds, so a client a handler builds for one is constructed
/// and its call dispatched — and what decides the outcome is the grant in force on the
/// function's execution role, never the SDK's own knowledge of the service name.
#[tokio::test]
async fn a_handler_lists_two_services_through_its_runtimes_sdk_under_each_grant() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let router = emulator();

    // The state each listing has to report, written through the OWNING service's own API so a
    // served listing is answered out of that service's store.
    for bucket in BUCKETS {
        vectors(
            &router,
            "CreateVectorBucket",
            &json!({ "vectorBucketName": bucket }),
        )
        .await;
    }
    let mut opened: Vec<String> = Vec::new();
    for _ in 0..SESSIONS {
        let created = create_session(&router).await;
        let id = created["sessionId"].as_str().unwrap_or_default().to_string();
        assert!(
            !id.is_empty(),
            "setup: expected CreateSession to mint a session id, got {created}"
        );
        opened.push(id);
    }
    let mut expected_buckets: Vec<String> = BUCKETS.iter().map(|name| name.to_string()).collect();
    expected_buckets.sort();
    let mut expected_sessions = opened.clone();
    expected_sessions.sort();

    // The execution role, written with IAM's own operations, and the function that runs as it.
    let role = execution_role(&router, ROLE_NAME, &logs_only()).await;
    let created = call(
        &router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": role,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&[("lambda_function.py", HANDLER_SOURCE)])),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: expected CreateFunction to answer 201, got {} {}",
        created.status,
        created.text()
    );
    await_function_active(&router, "setup", FUNCTION).await;

    // The identity a function's own calls are made as, which is the principal a refusal blames.
    let principal = session_principal(ROLE_NAME, FUNCTION);
    let buckets_denial = denial_without_resource(&principal, LIST_BUCKETS_ACTION);
    let sessions_denial = denial(&principal, LIST_SESSIONS_ACTION, &every_session_arn());

    for row in ROWS {
        let case = row.case;
        role_policy(&router, case, ROLE_NAME, &policy_for(row)).await;
        let answered = call(
            &router,
            "POST",
            &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
            &json!({ "sessionPage": SESSION_PAGE }),
        )
        .await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200 from Invoke, got {} {}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.header("x-amz-function-error"),
            None,
            "case {case:?}: the handler must run to completion, got {}",
            answered.text()
        );
        let payload = answered.json();
        assert_listing(
            case,
            "vector store",
            &payload["s3vectors"],
            row.buckets_served,
            &expected_buckets,
            &buckets_denial,
        );
        assert_listing(
            case,
            "agent runtime plane",
            &payload["bedrock"],
            row.sessions_served,
            &expected_sessions,
            &sessions_denial,
        );
    }

    // The other half of each served listing: what each service itself holds, read back where a
    // client reads it. The two counts differ, so neither listing above can have been answered
    // out of the other service's store.
    let held_buckets = vectors(&router, "ListVectorBuckets", &json!({})).await;
    let mut names: Vec<String> = held_buckets["vectorBuckets"]
        .as_array()
        .map(|carried| {
            carried
                .iter()
                .filter_map(|bucket| bucket["vectorBucketName"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    assert_eq!(
        names, expected_buckets,
        "expected the vector store to hold the buckets the handler listed, got {held_buckets}"
    );
    let held = held_sessions(&router).await;
    let mut ids: Vec<String> = held["sessionSummaries"]
        .as_array()
        .map(|carried| {
            carried
                .iter()
                .filter_map(|summary| summary["sessionId"].as_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    ids.sort();
    assert_eq!(
        ids, expected_sessions,
        "expected the agent runtime plane to hold the sessions the handler listed, got {held}"
    );
    assert_ne!(
        expected_buckets.len(),
        expected_sessions.len(),
        "the two listings must differ in count, got {expected_buckets:?} and {expected_sessions:?}"
    );
}
