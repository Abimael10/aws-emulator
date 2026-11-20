// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose requests are decided by a Lambda AUTHORIZER and whose fields
//! are resolved by APPSYNC_JS code over a `NONE` data source — end to end over the real
//! `MultiServiceRouter`, with AppSync, Lambda, IAM and CloudFormation mounted on one engine.
//!
//! RULE — the value an APPSYNC_JS request handler ends its step with through
//! `runtime.earlyReturn(value)`, and the value a response handler returns, IS the field's value
//! whatever that value was computed from: a member of what the API's Lambda authorizer published
//! about the caller (`resolverContext`, which a resolver reads as
//! `ctx.identity.resolverContext`), something derived from it, or a whole object the request
//! selects only part of. Such a field is answered HTTP 200 with that value under its own
//! response key and with no `errors` member, so a resolver that ran is never reported as a field
//! that resolved to nothing. And "the default is 0 seconds, which disables caching"
//! (`LambdaAuthorizerConfig$authorizerResultTtlInSeconds`, which "the Lambda function can
//! override … by returning a `ttlOverride` key in its response"), so a decision whose window is
//! zero is reused for no later request: every request takes a FRESH decision and carries that
//! decision's own published context, while a positive window is one decision shared by every
//! request inside it.
//!
//! Two services have to agree for the rule to hold — Lambda runs the authorizer and AppSync
//! binds what it published onto the resolver's context, runs the code and applies what the code
//! ended with — so this is the suite that can pin it.
//!
//! The rows span what the rule generalises over: WHAT the handler ends its step with (a member
//! of the published context, a value derived from it, a whole object partially selected, and the
//! same value taken through the response handler instead), the FORM of the token presented (a
//! short one, a long one, one carrying punctuation), and the reuse WINDOW the authorizer stated
//! for its decision (zero against a positive configured default, and the configured default
//! itself).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_lambda_data_source_and_authorizer::post_published_uri;

/// The Region every request here signs for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which every setup call is signed with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The path a GraphQL API's `GRAPHQL` endpoint serves.
const GRAPHQL_PATH: &str = "/graphql";

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `State` a function reports once it can be invoked, and the status a stack settles at.
const FUNCTION_ACTIVE: &str = "Active";
const STACK_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The runtime, handler entry point and timeout the authorizer is created on. The timeout is
/// well past what a run needs on any host: what is under test is what the function returned,
/// never how fast an interpreter got through it.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";
const FUNCTION_TIMEOUT: u64 = 120;

/// The `runtime` a resolver defined by `code` names. Source: `AppSyncRuntime`'s own members
/// (`smithy_query(query="shape",shape="AppSyncRuntime,RuntimeName")`).
const APPSYNC_JS: (&str, &str) = ("APPSYNC_JS", "1.0.0");

/// The data source every resolver here resolves against: `NONE` is "there is no data source",
/// so nothing but the resolver's own code can decide the field.
const LOCAL_DATA_SOURCE: &str = "local";

/// The window the API CONFIGURES for its authorizer's decisions, which the function overrides
/// for itself. Positive, so a zero override has something to override.
const CONFIGURED_TTL: u64 = 300;

/// How long a poll loop waits for a state another service owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches a
/// transition.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// What every response handler of an early-returning resolver answers: a value no row expects,
/// so a row that read it back is one whose response handler RAN when the early return had
/// already ended its step.
const RESPONSE_MARKER: &str = "the response handler ran";

/// The schema every API of this suite is created with: one field per row below.
const SCHEMA: &str = "\
type Published {\n  token: String\n  tier: String\n}\n\
\n\
type Query {\n  \
  whoami: String\n  \
  tier: String\n  \
  shouted: String\n  \
  published: Published\n  \
  viaResponse: String\n  \
  freshness: String\n}\n";

/// The authorizer every API of this suite is gated by.
///
/// It authorizes every call, publishes what it read off the token AND the id of its OWN
/// invocation — so a row can tell one invocation from four — and states the zero window that
/// "disables caching" for every token but the one asking for the configured default.
const AUTHORIZER_HANDLER: &str = r#"
def handler(event, context):
    token = event.get("authorizationToken") or ""
    answered = {
        "isAuthorized": True,
        "resolverContext": {
            "token": token,
            "tier": "gold" if token.endswith("-gold") else "standard",
            "invocation": context.aws_request_id,
        },
    }
    if not token.endswith("-cached"):
        answered["ttlOverride"] = 0
    return answered
"#;

/// The trust policy the authorizer's EXECUTION role carries, which `CreateFunction` checks: the
/// role has to be assumable by the Lambda service principal. Written compactly and free of the
/// characters an `application/x-www-form-urlencoded` body treats specially (`&`, `=`, `+`, `%`,
/// space), so it needs no encoding.
const LAMBDA_TRUST_POLICY: &str = "{\"Version\":\"2012-10-17\",\"Statement\":\
                                   [{\"Effect\":\"Allow\",\"Principal\":\
                                   {\"Service\":\"lambda.amazonaws.com\"},\
                                   \"Action\":\"sts:AssumeRole\"}]}";

/// The whole emulator over ONE engine, exactly as `core/router/src/lib.rs` assembles it for the
/// binary: AppSync publishes the endpoint and runs the code, Lambda runs the authorizer, IAM
/// holds its execution role and CloudFormation archives the handler into a deployment package.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Request authentication is off in this target: what a graded call pins is what the
    // endpoint ANSWERS, never signature verification.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential is the ambient key — the key a service reads
/// the caller's identity from. The signature is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One AppSync control-plane call (restJson1), as its status and parsed body.
async fn appsync(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map_or_else(Vec::new, |body| {
        serde_json::to_vec(&body).expect("the test body serializes")
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://appsync.{REGION}.amazonaws.com{path}"))
        .header("host", format!("appsync.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("appsync"))
        .body(Bytes::from(payload))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The same, asserting the call was served.
async fn appsync_ok(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Value {
    let (status, answered) = appsync(router, method, path, body).await;
    assert_eq!(
        status, 200,
        "setup: {method} {path} expected 200, got {status} {answered}"
    );
    answered
}

/// One Lambda call (restJson1), as its parsed body.
async fn lambda(router: &MultiServiceRouter, method: &str, path: &str) -> Value {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://lambda.{REGION}.amazonaws.com{path}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    }
}

/// One IAM call (awsQuery), answering the response body.
async fn iam(router: &MultiServiceRouter, body: String) -> String {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("iam.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body.clone()))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: iam {body} expected 200, got {status} {text}"
    );
    text
}

/// One CloudFormation call (awsQuery), answering the response body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (name, value) in params {
        body.push('&');
        body.push_str(&crate::query_wire::form_encode(name));
        body.push('=');
        body.push_str(&crate::query_wire::form_encode(value));
    }
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("the test request builds");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "setup: cloudformation {action} expected 200, got {status} {text}"
    );
    text
}

/// Deploy the function `name` from `source` and answer the ARN Lambda reports for it, once that
/// service reports it `Active`.
///
/// The handler is declared as a stack's INLINE source, which CloudFormation archives into the
/// deployment package a function is created from. Both the stack's completion and the function's
/// own provisioning window are owed by the clock, so this looks until each holds rather than
/// sleeping a span on either service's behalf.
async fn create_function(router: &MultiServiceRouter, name: &str, source: &str) -> String {
    let execution_role = iam(
        router,
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}-exec\
             &AssumeRolePolicyDocument={LAMBDA_TRUST_POLICY}"
        ),
    )
    .await;
    let execution_role = crate::query_wire::opt_element(&execution_role, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole reports no <Arn>: {execution_role}"));
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Authorizer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": name,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": execution_role,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": source},
                },
            },
        },
    })
    .to_string();
    let stack = format!("{name}-stack");
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", &stack), ("TemplateBody", &template)],
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    let described = loop {
        let described = cloudformation(router, "DescribeStacks", &[("StackName", &stack)]).await;
        if crate::query_wire::opt_element(&described, "StackStatus").as_deref()
            == Some(STACK_CREATE_COMPLETE)
            || Instant::now() >= deadline
        {
            break described;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        crate::query_wire::opt_element(&described, "StackStatus").as_deref(),
        Some(STACK_CREATE_COMPLETE),
        "setup: the stack of {name} must reach {STACK_CREATE_COMPLETE}, last saw {described}"
    );
    let configuration = loop {
        let reported = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
        )
        .await;
        if reported["State"].as_str() == Some(FUNCTION_ACTIVE) || Instant::now() >= deadline {
            break reported;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        configuration["State"].as_str(),
        Some(FUNCTION_ACTIVE),
        "setup: Lambda must report {name} {FUNCTION_ACTIVE}, got {configuration}"
    );
    configuration["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: GetFunctionConfiguration reports no FunctionArn"))
        .to_string()
}

/// The code of one resolver whose request handler ends its own step with `ended`, and whose
/// response handler answers [`RESPONSE_MARKER`] — so a value a client reads is either what the
/// request handler ended with or proof the response handler ran after it.
fn ending_code(ended: &str) -> String {
    format!(
        "import {{ runtime }} from '@aws-appsync/utils';\n\
         export function request(ctx) {{ runtime.earlyReturn({ended}); }}\n\
         export function response(ctx) {{ return '{RESPONSE_MARKER}'; }}\n"
    )
}

/// Create one `AWS_LAMBDA` API gated by `function_arn`, install [`SCHEMA`], add the `NONE` data
/// source and attach one APPSYNC_JS resolver per field of the schema, and answer the URL the
/// API published for its GraphQL endpoint — which is what a client POSTs to.
async fn create_api(router: &MultiServiceRouter, name: &str, function_arn: &str) -> String {
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({
            "name": name,
            "authenticationType": "AWS_LAMBDA",
            "lambdaAuthorizerConfig": {
                "authorizerUri": function_arn,
                "authorizerResultTtlInSeconds": CONFIGURED_TTL,
            },
        })),
    )
    .await;
    let api = &created["graphqlApi"];
    let api_id = api["apiId"].as_str().unwrap_or_default().to_string();
    let graphql_uri = api["uris"]["GRAPHQL"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        graphql_uri.ends_with(GRAPHQL_PATH) && graphql_uri.contains("127.0.0.1:"),
        "setup: uris.GRAPHQL must be a URL this emulator serves, got {graphql_uri:?}"
    );
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/schemacreation"),
        Some(json!({"definition": ::aws_smithy_types::base64::encode(SCHEMA)})),
    )
    .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let settled = appsync_ok(
            router,
            "GET",
            &format!("/v1/apis/{api_id}/schemacreation"),
            None,
        )
        .await;
        if settled["status"].as_str() == Some("SUCCESS") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "setup: the schema creation must be performed, last read {settled}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": LOCAL_DATA_SOURCE, "type": "NONE"})),
    )
    .await;
    // One resolver per field: the first four end their own step with something read off what the
    // authorizer published, and the fifth takes the same value through the RESPONSE handler
    // instead — the local data source hands a request handler's own `payload` back as
    // `ctx.result`.
    let resolvers: &[(&str, String)] = &[
        ("whoami", ending_code("ctx.identity.resolverContext.token")),
        ("tier", ending_code("ctx.identity.resolverContext.tier")),
        (
            "shouted",
            ending_code("ctx.identity.resolverContext.token.toUpperCase()"),
        ),
        (
            "published",
            ending_code(
                "{token: ctx.identity.resolverContext.token, \
                 tier: ctx.identity.resolverContext.tier}",
            ),
        ),
        (
            "viaResponse",
            "export function request(ctx) {\n  \
               return {payload: ctx.identity.resolverContext.token};\n}\n\
             export function response(ctx) { return 'seen:' + ctx.result; }\n"
                .to_string(),
        ),
        (
            "freshness",
            ending_code("ctx.identity.resolverContext.invocation"),
        ),
    ];
    for (field, code) in resolvers {
        appsync_ok(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers"),
            Some(json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": LOCAL_DATA_SOURCE,
                "code": code,
                "runtime": {"name": APPSYNC_JS.0, "runtimeVersion": APPSYNC_JS.1},
            })),
        )
        .await;
    }
    graphql_uri
}

/// POST one GraphQL document to the URL the API PUBLISHED, presenting `token` the way AppSync
/// reads an authorizer's input: the API's own `authorization` field.
async fn post_as(case: &str, graphql_uri: &str, document: &str, token: &str) -> (u16, Value) {
    post_published_uri(
        case,
        graphql_uri,
        &json!({"query": document}),
        &[("authorization", token.to_string())],
    )
    .await
}

/// The value a resolver ends its step with is the field's value whatever it was computed from —
/// including from what the authorizer published about the caller.
#[tokio::test]
async fn an_early_returned_value_read_off_the_authorizers_context_is_the_fields_value() {
    let router = build_router();
    let authorizer = create_function(&router, "early-return-authorizer", AUTHORIZER_HANDLER).await;
    let graphql_uri = create_api(&router, "early-return-published-context", &authorizer).await;

    /// One row: the token the request presents, and the tier the authorizer publishes for it.
    struct Row {
        case: &'static str,
        token: String,
        tier: &'static str,
    }
    // The rows differ along the FORM of the token the request presents, which is what the
    // published context is computed from: a short one, one the authorizer reads a second member
    // off, a long one, and one carrying the punctuation a bearer token is written with.
    let rows: Vec<Row> = vec![
        Row {
            case: "a short token",
            token: "allow-plain".to_string(),
            tier: "standard",
        },
        Row {
            case: "a token the authorizer publishes a second member for",
            token: "allow-gold".to_string(),
            tier: "gold",
        },
        Row {
            case: "a long token",
            token: format!("allow-{}", "a".repeat(64)),
            tier: "standard",
        },
        Row {
            case: "a token carrying punctuation",
            token: "eyJhbGciOiJIUzI1NiJ9.allow_42-9.sig".to_string(),
            tier: "standard",
        },
    ];

    let document = "query { whoami tier shouted published { token } viaResponse }";
    for row in &rows {
        let case = row.case;
        let token = row.token.as_str();
        let (status, answered) = post_as(case, &graphql_uri, document, token).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an authorized document is answered 200, got {status} {answered}"
        );
        // The field carries what the resolver ended its step with, and nothing of it failed:
        // a field that resolved reports no error of its own.
        assert!(
            answered.get("errors").is_none(),
            "case {case:?}: expected no errors member, got {answered}"
        );
        let expected = json!({
            "whoami": token,
            "tier": row.tier,
            "shouted": token.to_uppercase(),
            // Only the member the request selected, which is what a GraphQL result carries.
            "published": {"token": token},
            "viaResponse": format!("seen:{token}"),
        });
        assert_eq!(
            answered["data"], expected,
            "case {case:?}: expected the data {expected}, got {answered}"
        );
    }
}

/// A decision's reuse window decides how many times the authorizer runs, which is observable
/// because each decision's own published context reaches the field that reads it.
#[tokio::test]
async fn a_zero_reuse_window_takes_a_fresh_decision_for_every_request() {
    let router = build_router();
    let authorizer = create_function(&router, "ttl-override-authorizer", AUTHORIZER_HANDLER).await;
    let graphql_uri = create_api(&router, "ttl-override-windows", &authorizer).await;

    /// One row: the token presented on every request of the leg, and how many authorizer
    /// invocations the leg's requests must come from.
    struct Row {
        case: &'static str,
        token: &'static str,
        invocations: usize,
    }
    /// How many requests each leg makes, all presenting the same token.
    const REQUESTS: usize = 4;
    // The windows differ along what decides them: the zero one the function states for itself,
    // which overrides the API's positive configured default, and that default itself where the
    // function states nothing.
    let rows: Vec<Row> = vec![
        Row {
            case: "a ttlOverride of zero against a positive configured window",
            token: "allow-open",
            invocations: REQUESTS,
        },
        Row {
            case: "the configured window, which the function leaves in force",
            token: "allow-cached",
            invocations: 1,
        },
    ];

    for row in &rows {
        let case = row.case;
        let mut seen = std::collections::BTreeSet::new();
        for attempt in 0..REQUESTS {
            let (status, answered) =
                post_as(case, &graphql_uri, "query { freshness whoami }", row.token).await;
            assert_eq!(
                status, 200,
                "case {case:?} request {attempt}: expected HTTP 200, got {status} {answered}"
            );
            assert!(
                answered.get("errors").is_none(),
                "case {case:?} request {attempt}: expected no errors member, got {answered}"
            );
            assert_eq!(
                answered["data"]["whoami"].as_str(),
                Some(row.token),
                "case {case:?} request {attempt}: expected the field to carry the token \
                 {:?}, got {answered}",
                row.token
            );
            let published = answered["data"]["freshness"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?} request {attempt}: expected the field to carry the \
                         invocation the decision was taken by, got {answered}"
                    )
                })
                .to_string();
            seen.insert(published);
        }
        assert_eq!(
            seen.len(),
            row.invocations,
            "case {case:?}: expected {REQUESTS} requests to come from {} authorizer \
             invocation(s), got the published invocations {seen:?}",
            row.invocations
        );
    }
}
