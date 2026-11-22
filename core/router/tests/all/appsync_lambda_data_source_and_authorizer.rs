// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose fields are resolved by AWS LAMBDA, and whose requests are
//! decided by a Lambda AUTHORIZER — end to end over the real `MultiServiceRouter`, with
//! AppSync, Lambda, IAM, STS and CloudWatch mounted on one engine.
//!
//! RULE 1 — a resolver bound to a data source whose type is `AWS_LAMBDA` RESOLVES BY INVOKING
//! that data source's function: the event is what the resolver's definition rendered as the
//! request's `payload`, or — for a resolver carrying no definition at all, the Direct Lambda
//! resolver AppSync documents — the field's whole resolver context document (`arguments`,
//! `identity`, `source`, `request`, `prev`, `stash` and `info` with its `fieldName`,
//! `parentTypeName` and `selectionSetList`), and the value the function RETURNED is the
//! field's value. A handler that RAISED leaves the field null and reports exactly one error of
//! that field's own path, whose `errorType` is the class the runtime named for the raised
//! exception and whose message is the exception's own text. Where the resolver configures a
//! maximum batching size, the sibling resolutions that address it are merged into ONE
//! invocation whose event is the LIST of their context documents, and the returned list
//! supplies each sibling's own value element by element — an element reporting an error of its
//! own item nulling only that sibling. The invocation is made under the data source's own
//! service role ("the system assumes this role when accessing the data source",
//! `DataSource$serviceRoleArn`), so a role whose policies do not allow `lambda:InvokeFunction`
//! resolves no field of that data source and the authorization failure is what the one reported
//! error states.
//!
//! RULE 2 — an API whose authorization mode is `AWS_LAMBDA` INVOKES its authorizer with the
//! token the request presented, before any field of the request resolves, and what the function
//! returned decides the request: `isAuthorized: false` reaches no resolver at all and is
//! answered this mode's `UnauthorizedException`; `deniedFields` nulls exactly the fields it
//! names and leaves every other field resolved, so an empty list resolves every field and
//! reports no error; `resolverContext` is what a resolver reads as
//! `$context.identity.resolverContext`; and `ttlOverride` is how long THAT decision may be
//! reused, so a positive window is exactly one invocation of the authorizer inside it and a
//! zero window is one invocation per request. An `identityValidationExpression` the presented
//! token does not match refuses the request with no invocation made for it.
//!
//! Three services have to agree for either rule to hold — Lambda runs the functions, IAM holds
//! the service role and its policy, and AppSync publishes the endpoint, builds the event and
//! applies the answer — so this is the suite that can pin them.
//!
//! The rows span what each rule generalises over: the shape the handler returns (a scalar, an
//! object, a projected object, a raised exception), the form of the invocation (one event, a
//! batched list of two, a per-item failure inside one), the step the invocation is made from (a
//! UNIT resolver and each function of a pipeline), what the service role's policy allows, and
//! what the authorizer answered (authorized, refused, with fields denied, cached and
//! uncached, and a token the validation expression rejects).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// The Region every request here signs for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which every setup call is signed with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The path a GraphQL API's `GRAPHQL` endpoint serves.
const GRAPHQL_PATH: &str = "/graphql";

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked"), and the status a stack settles at.
const FUNCTION_ACTIVE: &str = "Active";
const STACK_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The runtime every function here is created on.
const RUNTIME: &str = "python3.12";

/// The handler entry point of every package below.
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is what the handler returned and not how fast
/// an interpreter got through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// The data source every resolver here resolves against.
const DATA_SOURCE: &str = "handler";

/// The `errorType` and the sentence this mode refuses an unauthorized call with.
const UNAUTHORIZED_ERROR_TYPE: &str = "UnauthorizedException";
const UNAUTHORIZED_MESSAGE: &str = "You are not authorized to make this call.";

/// The `errorType` AppSync reports for a field whose data source could not be invoked, and the
/// session name it assumes a data source's service role under — which is the principal such a
/// refusal blames.
const INVOCATION_ERROR_TYPE: &str = "Lambda:IllegalArgument";
const ASSUMED_SESSION_NAME: &str = "APPSYNC_ASSUME_ROLE";

/// The `errorType` AppSync reports for a field the caller may not access, and the fixed part of
/// the sentence it states.
const DENIED_ERROR_TYPE: &str = "Unauthorized";

/// How long a poll loop waits for a state another service owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches a
/// transition.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How long a case waits for one answer from the API's own socket.
const ANSWER_DEADLINE: Duration = Duration::from_secs(60);

/// The schema every API of this suite is created with: one field per row of either rule.
const SCHEMA: &str = "\
type Echo {\n  said: String\n}\n\
\n\
type Seen {\n  fieldName: String\n  parentTypeName: String\n  selected: String\n  \
  argument: String\n  resolverContext: String\n  previous: String\n}\n\
\n\
type Item {\n  id: String\n  batchSize: Int\n}\n\
\n\
type Group {\n  id: String\n  member: Item\n}\n\
\n\
type Query {\n  \
  echo(text: String): Echo\n  \
  plain(text: String): String\n  \
  seen(text: String): Seen\n  \
  broken: Echo\n  \
  eventKeys: String\n  \
  item(id: String, fail: Boolean): Item\n  \
  other(id: String): Item\n  \
  groups: [Group]\n  \
  piped: Seen\n  \
  open: String\n  \
  secret: String\n  \
  vtlContext: String\n  \
  jsContext: String\n}\n";

/// The handler every resolver of this suite invokes.
///
/// It answers from the EVENT alone: a list event is the batched invocation, and each element of
/// its answer is that item's own; a single event is one field's resolver context document, and
/// which field it belongs to is read off `info.fieldName`. So one package serves every row,
/// and what a row asserts is what this handler computed from the context AppSync handed it.
const RESOLVER_HANDLER: &str = r#"
def _seen(event):
    info = event.get("info") or {}
    identity = event.get("identity") or {}
    return {
        "fieldName": info.get("fieldName"),
        "parentTypeName": info.get("parentTypeName"),
        "selected": ",".join(info.get("selectionSetList") or []),
        "argument": (event.get("arguments") or {}).get("text"),
        "resolverContext": (identity.get("resolverContext") or {}).get("nonce"),
        "previous": None,
    }


def handler(event, context):
    if isinstance(event, list):
        answers = []
        for item in event:
            arguments = item.get("arguments") or {}
            parent = item.get("source") or {}
            identifier = arguments.get("id") or parent.get("id")
            if arguments.get("fail"):
                answers.append({
                    "errorMessage": "item " + str(identifier) + " could not be read",
                    "errorType": "ItemNotFound",
                })
            else:
                answers.append({"id": identifier, "batchSize": len(event)})
        return answers
    field = (event.get("info") or {}).get("fieldName")
    arguments = event.get("arguments") or {}
    if field == "broken":
        raise ValueError("the handler could not read this field")
    if field == "eventKeys":
        return ",".join(sorted(event.keys()))
    if field == "groups":
        return [{"id": "g1"}, {"id": "g2"}, {"id": "g3"}]
    if field == "plain":
        return "plain:" + str(arguments.get("text"))
    if field == "open" or field == "secret":
        return field + "-value"
    if field == "piped":
        previous = (event.get("prev") or {}).get("result")
        if previous is None:
            return {"step": "first"}
        answered = _seen(event)
        answered["previous"] = str(previous.get("step"))
        return answered
    if field == "seen":
        return _seen(event)
    return {"said": arguments.get("text")}
"#;

/// The authorizer every API of RULE 2 is gated by: it decides from the token it was handed and
/// publishes the id of its OWN invocation, so a row can tell one invocation from two.
const AUTHORIZER_HANDLER: &str = r#"
def handler(event, context):
    token = event.get("authorizationToken") or ""
    answered = {
        "isAuthorized": token.startswith("allow"),
        "resolverContext": {"nonce": context.aws_request_id},
        "deniedFields": [],
        "ttlOverride": 0,
    }
    if token == "allow-denied":
        answered["deniedFields"] = ["Query.secret"]
    if token == "allow-cached":
        answered["ttlOverride"] = 300
    return answered
"#;

/// The expression an API of RULE 2 validates a token against before its authorizer is invoked.
const IDENTITY_VALIDATION_EXPRESSION: &str = "^(allow|deny)-[a-z]+$";

/// The members a Direct Lambda resolver's event carries, in the order the handler sorts them:
/// the resolver context document's own, and no others.
const DIRECT_EVENT_MEMBERS: &str = "arguments,identity,info,prev,request,source,stash";

/// A second data source every API of RULE 2 also holds: `NONE` is "there is no data source",
/// so a resolver bound to it resolves against its own definition alone — which is how a case
/// reads what the authorizer published without a function in the way.
const LOCAL_DATA_SOURCE: &str = "local";

/// The template pair and the code that read what the authorizer published, each through its own
/// runtime: the one member a resolver reads it under is `$ctx.identity.resolverContext`.
const CONTEXT_REQUEST_TEMPLATE: &str =
    r#"{"version": "2018-05-29", "payload": $util.toJson($ctx.identity.resolverContext.nonce)}"#;
const CONTEXT_RESPONSE_TEMPLATE: &str = "$util.toJson($ctx.result)";
const CONTEXT_CODE: &str = "export function request(ctx) {\n  \
                            return {payload: ctx.identity.resolverContext.nonce};\n}\n\
                            export function response(ctx) {\n  return ctx.result;\n}\n";

/// The trust policy a function's EXECUTION role carries, which `CreateFunction` checks: the
/// role has to be assumable by the Lambda service principal. Written compactly and free of the
/// characters an `application/x-www-form-urlencoded` body treats specially (`&`, `=`, `+`, `%`,
/// space), so it needs no encoding.
const LAMBDA_TRUST_POLICY: &str = "{\"Version\":\"2012-10-17\",\"Statement\":\
                                   [{\"Effect\":\"Allow\",\"Principal\":\
                                   {\"Service\":\"lambda.amazonaws.com\"},\
                                   \"Action\":\"sts:AssumeRole\"}]}";

/// The whole emulator over ONE engine, exactly as `core/router/src/lib.rs` assembles it for the
/// binary: AppSync publishes the endpoint and builds the event, Lambda runs the functions, IAM
/// holds the service role and its policy, and CloudFormation archives each handler's inline
/// source into the deployment package it is created from.
///
/// Shared with the sibling suites that drive an AppSync data plane backed by Lambda, so one
/// assembly of the emulator serves them all.
pub(crate) fn build_router() -> MultiServiceRouter {
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

/// The same, asserting the call was served. Shared with the sibling suites that set an AppSync
/// API up over this target's router.
pub(crate) async fn appsync_ok(
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

/// One Lambda call (restJson1), as its status and parsed body.
async fn lambda(
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
        .uri(format!("https://lambda.{REGION}.amazonaws.com{path}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
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

/// The text of the first `<name>…</name>` element of an XML body.
fn xml_element(body: &str, name: &str) -> Option<String> {
    crate::query_wire::opt_element(body, name)
}

/// Deploy the function `name` from `source` and answer the ARN Lambda reports for it, once that
/// service reports it `Active`.
///
/// The handler is declared as a stack's INLINE source, which CloudFormation archives into the
/// deployment package a function is created from — the way a function whose handler is written
/// out in a suite comes to exist at all. Both the stack's completion and the function's own
/// provisioning window are owed by the clock, so this looks until each holds rather than
/// sleeping a span on either service's behalf.
///
/// Shared with the sibling suites that gate an AppSync API on a function of their own, so there
/// is one reading of how a handler written out in a suite comes to be invocable.
pub(crate) async fn create_function(
    router: &MultiServiceRouter,
    name: &str,
    source: &str,
) -> String {
    // The role the function RUNS as: trusted by Lambda, which `CreateFunction` checks. What it
    // may do is not what this suite is about — the role under test is the DATA SOURCE's.
    let execution_role = iam(
        router,
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}-exec\
             &AssumeRolePolicyDocument={LAMBDA_TRUST_POLICY}"
        ),
    )
    .await;
    let execution_role = xml_element(&execution_role, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole reports no <Arn>: {execution_role}"));
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Probe": {
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
        let described =
            cloudformation(router, "DescribeStacks", &[("StackName", &stack)]).await;
        if xml_element(&described, "StackStatus").as_deref() == Some(STACK_CREATE_COMPLETE)
            || Instant::now() >= deadline
        {
            break described;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    };
    assert_eq!(
        xml_element(&described, "StackStatus").as_deref(),
        Some(STACK_CREATE_COMPLETE),
        "setup: the stack of {name} must reach {STACK_CREATE_COMPLETE}, last saw {described}"
    );
    let configuration = loop {
        let (_, reported) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            None,
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

/// Create the role `name`, trusted by AppSync, carrying `document` as its one inline policy,
/// and answer its ARN.
///
/// The documents are written compactly and free of the characters an
/// `application/x-www-form-urlencoded` body treats specially (`&`, `=`, `+`, `%`, space), so
/// they need no encoding.
async fn create_role(router: &MultiServiceRouter, name: &str, document: &str) -> String {
    let trust = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                 \"Principal\":{\"Service\":\"appsync.amazonaws.com\"},\
                 \"Action\":\"sts:AssumeRole\"}]}";
    let created = iam(
        router,
        format!(
            "Action=CreateRole&Version=2010-05-08&RoleName={name}\
             &AssumeRolePolicyDocument={trust}"
        ),
    )
    .await;
    let role_arn = xml_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole reports no <Arn>: {created}"));
    iam(
        router,
        format!(
            "Action=PutRolePolicy&Version=2010-05-08&RoleName={name}\
             &PolicyName=data-source-access&PolicyDocument={document}"
        ),
    )
    .await;
    role_arn
}

/// A policy allowing `action` on every resource.
fn allowing(action: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":\
         [{{\"Effect\":\"Allow\",\"Action\":\"{action}\",\"Resource\":\"*\"}}]}}"
    )
}

/// One created GraphQL API: what it is addressed by, and where it is served.
struct Api {
    api_id: String,
    /// The URL the API publishes for its `GRAPHQL` endpoint, which is what a client POSTs to.
    graphql_uri: String,
}

/// Create one API with `authentication_type` (and `lambda_authorizer_config` where the mode
/// needs one), install [`SCHEMA`], and attach the `AWS_LAMBDA` data source that invokes
/// `function_arn` under `role_arn`.
async fn create_api(
    router: &MultiServiceRouter,
    name: &str,
    authentication_type: &str,
    lambda_authorizer_config: Option<Value>,
    function_arn: &str,
    role_arn: &str,
) -> Api {
    let mut body = json!({"name": name, "authenticationType": authentication_type});
    if let Some(configured) = lambda_authorizer_config {
        body["lambdaAuthorizerConfig"] = configured;
    }
    let created = appsync_ok(router, "POST", "/v1/apis", Some(body)).await;
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
        Some(json!({
            "definition": ::aws_smithy_types::base64::encode(SCHEMA),
        })),
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
        Some(json!({
            "name": DATA_SOURCE,
            "type": "AWS_LAMBDA",
            "serviceRoleArn": role_arn,
            "lambdaConfig": {"lambdaFunctionArn": function_arn},
        })),
    )
    .await;
    Api {
        api_id,
        graphql_uri,
    }
}

/// Attach a Direct Lambda resolver — no mapping templates and no code — to `field` of
/// `type_name`, with `max_batch_size` as its maximum batching size.
async fn create_direct_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    type_name: &str,
    field: &str,
    max_batch_size: i32,
) {
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
        Some(json!({
            "fieldName": field,
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "maxBatchSize": max_batch_size,
        })),
    )
    .await;
}

/// Mint an API key for `api_id`, which is what a call on an `API_KEY` API presents.
async fn create_api_key(router: &MultiServiceRouter, api_id: &str) -> String {
    let key = appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/apikeys"),
        Some(json!({"description": "the suite's key"})),
    )
    .await;
    key["apiKey"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// POST one GraphQL document to the URL the API PUBLISHED, as a client does: the host, the port
/// and the path all come from `uris.GRAPHQL` rather than from anything this suite knows.
async fn post_published(
    case: &str,
    api: &Api,
    body: &Value,
    fields: &[(&str, String)],
) -> (u16, Value) {
    post_published_uri(case, &api.graphql_uri, body, fields).await
}

/// The same, over a published URL itself rather than over one of this suite's own APIs.
///
/// The ONE socket client of this target's AppSync data-plane suites, so there is one reading of
/// how a client reaches a published GraphQL endpoint rather than one per suite
/// ([`crate::appsync_lambda_batch_invoke_resolvers`] drives its cases through this too).
pub(crate) async fn post_published_uri(
    case: &str,
    graphql_uri: &str,
    body: &Value,
    fields: &[(&str, String)],
) -> (u16, Value) {
    let served = post_published_served(case, graphql_uri, body, fields).await;
    (served.status, served.document)
}

/// What a call on a published endpoint was ANSWERED: the status, the response HEAD — which is
/// where the endpoint writes what a client correlates the answer by — and the document served.
pub(crate) struct ServedResponse {
    pub(crate) status: u16,
    pub(crate) head: String,
    pub(crate) document: Value,
}

/// [`post_published_uri`] answering the whole exchange rather than its status and document, for
/// a suite that grades a RESPONSE FIELD as well: the socket client stays this one, so there is
/// still one reading of how a client reaches a published GraphQL endpoint.
pub(crate) async fn post_published_served(
    case: &str,
    graphql_uri: &str,
    body: &Value,
    fields: &[(&str, String)],
) -> ServedResponse {
    let (authority, path) = published_parts(case, graphql_uri);
    let port: u16 = authority
        .rsplit_once(':')
        .and_then(|(_, port)| port.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: {authority:?} must name a port"));
    let mut stream = tokio::time::timeout(ANSWER_DEADLINE, TcpStream::connect(("127.0.0.1", port)))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: {authority:?} must accept a connection"))
        .unwrap_or_else(|error| {
            panic!("case {case:?}: {authority:?} must accept a connection, got {error}")
        });
    let payload = body.to_string();
    let mut request = format!(
        "POST {path} HTTP/1.1\r\nhost: {authority}\r\ncontent-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n",
        payload.len()
    );
    for (field, value) in fields {
        request.push_str(&format!("{field}: {value}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(&payload);
    stream
        .write_all(request.as_bytes())
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request must be written: {error}"));
    let mut answered = Vec::new();
    tokio::time::timeout(ANSWER_DEADLINE, stream.read_to_end(&mut answered))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the endpoint must answer"))
        .unwrap_or_else(|error| panic!("case {case:?}: the endpoint must answer: {error}"));
    let text = String::from_utf8_lossy(&answered).to_string();
    let (head, payload) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("case {case:?}: expected an HTTP response, got {text:?}"));
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a status line, got {head:?}"));
    let document = serde_json::from_str(payload).unwrap_or(Value::Null);
    ServedResponse {
        status,
        head: head.to_string(),
        document,
    }
}

/// The authority and the path of a published URL — what a client dials and what it asks for.
///
/// Shared with the sibling suites that grade a published address, so there is one reading of how
/// such a URL is decomposed.
pub(crate) fn published_parts(case: &str, uri: &str) -> (String, String) {
    let rest = uri
        .split_once("://")
        .map(|(_, rest)| rest)
        .unwrap_or_else(|| panic!("case {case:?}: {uri:?} must be an absolute URL"));
    match rest.split_once('/') {
        Some((authority, path)) => (authority.to_string(), format!("/{path}")),
        None => (rest.to_string(), "/".to_string()),
    }
}

/// The `errors` a result reports, as the pairs `(errorType, message)` each entry states.
fn reported_errors(answered: &Value) -> Vec<(String, String)> {
    answered["errors"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["errorType"].as_str().unwrap_or_default().to_string(),
                        entry["message"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// RULE 1, the happy path and the raised one: every Direct Lambda resolver resolves its field
/// from what the function returned for the context document it was invoked with, and a handler
/// that raised leaves the field null with exactly one error naming the exception's class.
#[tokio::test]
async fn a_direct_lambda_resolver_resolves_its_field_from_what_the_function_returned() {
    let router = build_router();
    let function = create_function(&router, "resolver-handler", RESOLVER_HANDLER).await;
    let role = create_role(&router, "appsync-invoker", &allowing("lambda:InvokeFunction")).await;
    let api = create_api(
        &router,
        "direct-lambda",
        "API_KEY",
        None,
        &function,
        &role,
    )
    .await;
    for field in ["echo", "plain", "seen", "broken", "eventKeys"] {
        create_direct_resolver(&router, &api.api_id, "Query", field, 0).await;
    }
    let api_key = create_api_key(&router, &api.api_id).await;

    /// One row: the document a case POSTs, the data it must be answered with, and the one
    /// error it must report, where it reports one.
    struct Row {
        case: &'static str,
        query: &'static str,
        data: Value,
        error: Option<(&'static str, &'static str)>,
    }
    // The rows differ along the SHAPE the handler returns for the field and along what of the
    // context document the field's answer is computed from.
    let rows: Vec<Row> = vec![
        Row {
            case: "a handler returning an object, projected by the selection",
            query: "query { echo(text: \"spoken\") { said } }",
            data: json!({"echo": {"said": "spoken"}}),
            error: None,
        },
        Row {
            case: "a handler returning a scalar for a field with no sub-selection",
            query: "query { plain(text: \"value\") }",
            data: json!({"plain": "plain:value"}),
            error: None,
        },
        Row {
            case: "the whole context document: info, the arguments and the selection set",
            query: "query { seen(text: \"carried\") { fieldName parentTypeName selected \
                    argument previous } }",
            data: json!({"seen": {
                "fieldName": "seen",
                "parentTypeName": "Query",
                "selected": "fieldName,parentTypeName,selected,argument,previous",
                "argument": "carried",
                "previous": null,
            }}),
            error: None,
        },
        Row {
            case: "a handler that raised, reported as the exception's own class and text",
            query: "query { broken { said } }",
            data: json!({"broken": null}),
            error: Some(("ValueError", "the handler could not read this field")),
        },
        Row {
            case: "the members the event carries, which are the context document's own",
            query: "query { eventKeys }",
            data: json!({"eventKeys": DIRECT_EVENT_MEMBERS}),
            error: None,
        },
        Row {
            case: "two fields of one request, each its own invocation",
            query: "query { echo(text: \"one\") { said } plain(text: \"two\") }",
            data: json!({"echo": {"said": "one"}, "plain": "plain:two"}),
            error: None,
        },
    ];
    for row in rows {
        let (status, answered) = post_published(
            row.case,
            &api,
            &json!({"query": row.query}),
            &[("x-api-key", api_key.clone())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status} {answered}",
            row.case
        );
        assert_eq!(
            answered["data"], row.data,
            "case {:?}: expected data {}, got {answered}",
            row.case, row.data
        );
        let errors = reported_errors(&answered);
        match row.error {
            None => assert!(
                answered.get("errors").is_none(),
                "case {:?}: expected no errors key, got {answered}",
                row.case
            ),
            Some((error_type, message)) => {
                assert_eq!(
                    errors.len(),
                    1,
                    "case {:?}: expected exactly one error, got {errors:?}",
                    row.case
                );
                assert_eq!(
                    (errors[0].0.as_str(), errors[0].1.as_str()),
                    (error_type, message),
                    "case {:?}: expected the error {error_type:?}/{message:?}, got {errors:?}",
                    row.case
                );
            }
        }
    }
}

/// RULE 1, batched: the sibling resolutions of a batched resolver are ONE invocation whose
/// event is the list of their context documents, and an element reporting an error of its own
/// item nulls only that sibling.
#[tokio::test]
async fn a_batched_resolver_merges_its_sibling_resolutions_into_one_invocation() {
    let router = build_router();
    let function = create_function(&router, "batched-handler", RESOLVER_HANDLER).await;
    let role = create_role(&router, "appsync-batch", &allowing("lambda:InvokeFunction")).await;
    let api = create_api(&router, "batched-lambda", "API_KEY", None, &function, &role).await;
    // "The maximum batching size for a resolver" (`Resolver$maxBatchSize`): three siblings fit
    // in one invocation of this resolver, and `other` is resolved the same way — the same data
    // source, no definition and the same batching size — so its resolutions merge with them.
    create_direct_resolver(&router, &api.api_id, "Query", "item", 3).await;
    create_direct_resolver(&router, &api.api_id, "Query", "other", 3).await;
    let api_key = create_api_key(&router, &api.api_id).await;

    /// One row: the document, the data it must be answered with, and the errors it must report.
    struct Row {
        case: &'static str,
        query: &'static str,
        data: Value,
        errors: Vec<(&'static str, &'static str)>,
    }
    // The rows differ along HOW MANY siblings one invocation merges, and along whether one of
    // them failed on its own: the `batchSize` each answer carries is the length of the event
    // list the handler was given, so it is what proves the merge.
    let rows: Vec<Row> = vec![
        Row {
            case: "one resolution, which travels as a batch of one",
            query: "query { only: item(id: \"a\") { id batchSize } }",
            data: json!({"only": {"id": "a", "batchSize": 1}}),
            errors: Vec::new(),
        },
        Row {
            case: "two sibling resolutions merged into one invocation",
            query: "query { first: item(id: \"a\") { id batchSize } \
                    second: item(id: \"b\") { id batchSize } }",
            data: json!({
                "first": {"id": "a", "batchSize": 2},
                "second": {"id": "b", "batchSize": 2},
            }),
            errors: Vec::new(),
        },
        Row {
            case: "three sibling resolutions, which is the resolver's whole batching size",
            query: "query { first: item(id: \"a\") { batchSize } \
                    second: item(id: \"b\") { batchSize } third: item(id: \"c\") { batchSize } }",
            data: json!({
                "first": {"batchSize": 3},
                "second": {"batchSize": 3},
                "third": {"batchSize": 3},
            }),
            errors: Vec::new(),
        },
        Row {
            case: "a per-item error, which nulls only the sibling it belongs to",
            query: "query { kept: item(id: \"a\") { id batchSize } \
                    failed: item(id: \"b\", fail: true) { id batchSize } }",
            data: json!({"kept": {"id": "a", "batchSize": 2}, "failed": null}),
            errors: vec![("ItemNotFound", "item b could not be read")],
        },
        Row {
            case: "four siblings, one past the batching size, which is two invocations",
            query: "query { first: item(id: \"a\") { batchSize } \
                    second: item(id: \"b\") { batchSize } third: item(id: \"c\") { batchSize } \
                    fourth: item(id: \"d\") { batchSize } }",
            data: json!({
                "first": {"batchSize": 3},
                "second": {"batchSize": 3},
                "third": {"batchSize": 3},
                "fourth": {"batchSize": 1},
            }),
            errors: Vec::new(),
        },
        Row {
            case: "two sibling FIELDS resolved the same way, merged into one invocation",
            query: "query { item(id: \"a\") { id batchSize } other(id: \"b\") { id batchSize } }",
            data: json!({
                "item": {"id": "a", "batchSize": 2},
                "other": {"id": "b", "batchSize": 2},
            }),
            errors: Vec::new(),
        },
        Row {
            case: "two sibling fields with another selection between them, still one invocation",
            query: "query { item(id: \"a\") { batchSize } __typename \
                    other(id: \"b\") { batchSize } }",
            data: json!({
                "item": {"batchSize": 2},
                "__typename": "Query",
                "other": {"batchSize": 2},
            }),
            errors: Vec::new(),
        },
    ];
    for row in rows {
        let (status, answered) = post_published(
            row.case,
            &api,
            &json!({"query": row.query}),
            &[("x-api-key", api_key.clone())],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status} {answered}",
            row.case
        );
        assert_eq!(
            answered["data"], row.data,
            "case {:?}: expected data {}, got {answered}",
            row.case, row.data
        );
        let reported = reported_errors(&answered);
        let expected: Vec<(String, String)> = row
            .errors
            .iter()
            .map(|(error_type, message)| (error_type.to_string(), message.to_string()))
            .collect();
        assert_eq!(
            reported, expected,
            "case {:?}: expected the errors {expected:?}, got {reported:?}",
            row.case
        );
    }
}

/// RULE 1, batched across SIBLING PARENTS: the resolutions of one batched field across the
/// elements its parent resolved to are merged into one invocation, which is the batching
/// AppSync's own guide is written about — one invocation for the whole list rather than one per
/// element.
#[tokio::test]
async fn a_batched_resolver_merges_the_resolutions_of_one_field_across_sibling_parents() {
    let router = build_router();
    let function = create_function(&router, "sibling-handler", RESOLVER_HANDLER).await;
    let role = create_role(&router, "appsync-siblings", &allowing("lambda:InvokeFunction")).await;
    let api = create_api(&router, "sibling-lambda", "API_KEY", None, &function, &role).await;
    create_direct_resolver(&router, &api.api_id, "Query", "groups", 0).await;
    create_direct_resolver(&router, &api.api_id, "Group", "member", 3).await;
    let api_key = create_api_key(&router, &api.api_id).await;

    let case = "one batched field across the three elements its parent resolved to";
    let (status, answered) = post_published(
        case,
        &api,
        &json!({"query": "query { groups { id member { id batchSize } } }"}),
        &[("x-api-key", api_key)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {answered}"
    );
    // The `batchSize` each element's answer carries is the length of the event list the handler
    // was given, so a 3 in every element is the one invocation the three resolutions shared.
    assert_eq!(
        answered["data"],
        json!({"groups": [
            {"id": "g1", "member": {"id": "g1", "batchSize": 3}},
            {"id": "g2", "member": {"id": "g2", "batchSize": 3}},
            {"id": "g3", "member": {"id": "g3", "batchSize": 3}},
        ]}),
        "case {case:?}: expected the three resolutions to share one invocation, got {answered}"
    );
    assert!(
        answered.get("errors").is_none(),
        "case {case:?}: expected no errors key, got {answered}"
    );
}

/// RULE 1, batched at a pipeline's FUNCTION: "the maximum batching size for a function"
/// (`FunctionConfiguration$maxBatchSize`) is each function's own, so a pipeline that IS one
/// batched function merges its sibling resolutions into one invocation just as a UNIT resolver
/// does.
#[tokio::test]
async fn a_pipeline_of_one_batched_function_merges_its_sibling_resolutions() {
    let router = build_router();
    let function = create_function(&router, "piped-batch-handler", RESOLVER_HANDLER).await;
    let role = create_role(&router, "appsync-piped-batch", &allowing("lambda:InvokeFunction")).await;
    let api = create_api(&router, "piped-batch", "API_KEY", None, &function, &role).await;
    let created = appsync_ok(
        &router,
        "POST",
        &format!("/v1/apis/{}/functions", api.api_id),
        Some(json!({
            "name": "readItems",
            "dataSourceName": DATA_SOURCE,
            "functionVersion": "2018-05-29",
            "maxBatchSize": 3,
        })),
    )
    .await;
    let function_id = created["functionConfiguration"]["functionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    appsync_ok(
        &router,
        "POST",
        &format!("/v1/apis/{}/types/Query/resolvers", api.api_id),
        Some(json!({
            "fieldName": "item",
            "kind": "PIPELINE",
            "pipelineConfig": {"functions": [function_id]},
        })),
    )
    .await;
    let api_key = create_api_key(&router, &api.api_id).await;

    let case = "a pipeline of one batched function over two sibling resolutions";
    let (status, answered) = post_published(
        case,
        &api,
        &json!({
            "query": "query { first: item(id: \"a\") { id batchSize } \
                      second: item(id: \"b\") { id batchSize } }"
        }),
        &[("x-api-key", api_key)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {answered}"
    );
    assert_eq!(
        answered["data"],
        json!({
            "first": {"id": "a", "batchSize": 2},
            "second": {"id": "b", "batchSize": 2},
        }),
        "case {case:?}: expected the two resolutions to share one invocation, got {answered}"
    );
    assert!(
        answered.get("errors").is_none(),
        "case {case:?}: expected no errors key, got {answered}"
    );
}

/// RULE 1, a pipeline: each function of a pipeline of Direct Lambda functions is its own
/// invocation, and each is handed the previous one's result as `prev.result`.
#[tokio::test]
async fn a_pipeline_of_direct_lambda_functions_threads_its_previous_result() {
    let router = build_router();
    let function = create_function(&router, "pipeline-handler", RESOLVER_HANDLER).await;
    let role = create_role(&router, "appsync-pipeline", &allowing("lambda:InvokeFunction")).await;
    let api = create_api(&router, "pipeline-lambda", "API_KEY", None, &function, &role).await;
    // Two functions of the pipeline, each carrying no definition of its own, so each invokes
    // the data source's function over its own context document. "When using VTL and mapping
    // templates, the functionVersion is required" (`CreateFunctionRequest$functionVersion`).
    let mut function_ids = Vec::new();
    for name in ["before", "after"] {
        let created = appsync_ok(
            &router,
            "POST",
            &format!("/v1/apis/{}/functions", api.api_id),
            Some(json!({
                "name": name,
                "dataSourceName": DATA_SOURCE,
                "functionVersion": "2018-05-29",
            })),
        )
        .await;
        function_ids.push(
            created["functionConfiguration"]["functionId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }
    appsync_ok(
        &router,
        "POST",
        &format!("/v1/apis/{}/types/Query/resolvers", api.api_id),
        Some(json!({
            "fieldName": "piped",
            "kind": "PIPELINE",
            "pipelineConfig": {"functions": function_ids},
        })),
    )
    .await;
    let api_key = create_api_key(&router, &api.api_id).await;

    let case = "a pipeline of two Direct Lambda functions";
    let (status, answered) = post_published(
        case,
        &api,
        &json!({"query": "query { piped { fieldName previous } }"}),
        &[("x-api-key", api_key)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {answered}"
    );
    // The field's value is the LAST function's, and its `previous` is what the FIRST function
    // returned — so the pipeline really threaded the result from one invocation to the next.
    assert_eq!(
        answered["data"],
        json!({"piped": {"fieldName": "piped", "previous": "first"}}),
        "case {case:?}: expected the second function to be handed the first's result, got \
         {answered}"
    );
    assert!(
        answered.get("errors").is_none(),
        "case {case:?}: expected no errors key, got {answered}"
    );
}

/// RULE 1, the service role: the invocation is made under the data source's own role, so a role
/// whose policies do not allow `lambda:InvokeFunction` resolves no field of that data source.
#[tokio::test]
async fn a_data_source_resolves_only_where_its_service_role_may_invoke_the_function() {
    /// One row: what the data source's service role is allowed to do, and whether the field
    /// then resolves.
    struct Row {
        case: &'static str,
        action: &'static str,
        resolves: bool,
    }
    // The rows differ along what the role's one policy allows: the invocation itself, the whole
    // Lambda namespace (which covers it), and another service's action (which does not).
    let rows: Vec<Row> = vec![
        Row {
            case: "a role allowed the invocation itself",
            action: "lambda:InvokeFunction",
            resolves: true,
        },
        Row {
            case: "a role allowed every Lambda action",
            action: "lambda:*",
            resolves: true,
        },
        Row {
            case: "a role allowed another service's action only",
            action: "logs:PutLogEvents",
            resolves: false,
        },
        Row {
            case: "a role allowed another Lambda action only",
            action: "lambda:GetFunction",
            resolves: false,
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let router = build_router();
        let function = create_function(&router, "role-gated-handler", RESOLVER_HANDLER).await;
        let role_name = format!("appsync-role-{index}");
        let role = create_role(&router, &role_name, &allowing(row.action)).await;
        let api = create_api(
            &router,
            &format!("role-gated-{index}"),
            "API_KEY",
            None,
            &function,
            &role,
        )
        .await;
        create_direct_resolver(&router, &api.api_id, "Query", "plain", 0).await;
        let api_key = create_api_key(&router, &api.api_id).await;
        let (status, answered) = post_published(
            row.case,
            &api,
            &json!({"query": "query { plain(text: \"value\") }"}),
            &[("x-api-key", api_key)],
        )
        .await;
        assert_eq!(
            status, 200,
            "case {:?}: expected HTTP 200, got {status} {answered}",
            row.case
        );
        if row.resolves {
            assert_eq!(
                answered["data"],
                json!({"plain": "plain:value"}),
                "case {:?}: expected the field to resolve, got {answered}",
                row.case
            );
            assert!(
                answered.get("errors").is_none(),
                "case {:?}: expected no errors key, got {answered}",
                row.case
            );
            continue;
        }
        assert_eq!(
            answered["data"],
            json!({"plain": null}),
            "case {:?}: expected the field to resolve to nothing, got {answered}",
            row.case
        );
        let errors = reported_errors(&answered);
        assert_eq!(
            errors.len(),
            1,
            "case {:?}: expected exactly one error, got {errors:?}",
            row.case
        );
        assert_eq!(
            errors[0].0, INVOCATION_ERROR_TYPE,
            "case {:?}: expected the errorType {INVOCATION_ERROR_TYPE:?}, got {errors:?}",
            row.case
        );
        // AppSync invokes a data source's function under the data source's own service role, so
        // what the refusal blames is the SESSION of that role rather than the role itself.
        assert!(
            errors[0]
                .1
                .contains("is not authorized to perform: lambda:InvokeFunction")
                && errors[0]
                    .1
                    .contains(&format!("assumed-role/{role_name}/{ASSUMED_SESSION_NAME}")),
            "case {:?}: expected the error to name the authorization failure of the service \
             role's own session, got {errors:?}",
            row.case
        );
    }
}

/// RULE 2: the API's Lambda authorizer decides every request before any field of it resolves —
/// the grant, the refusal, the fields it denies, and the token its validation expression
/// rejects before it is invoked at all.
#[tokio::test]
async fn a_lambda_authorizer_decides_every_request_before_any_field_resolves() {
    let router = build_router();
    let resolver = create_function(&router, "gated-handler", RESOLVER_HANDLER).await;
    let authorizer = create_function(&router, "api-authorizer", AUTHORIZER_HANDLER).await;
    let role = create_role(&router, "appsync-gated", &allowing("lambda:InvokeFunction")).await;
    let api = create_api(
        &router,
        "lambda-authorized",
        "AWS_LAMBDA",
        Some(json!({
            "authorizerUri": authorizer,
            "identityValidationExpression": IDENTITY_VALIDATION_EXPRESSION,
        })),
        &resolver,
        &role,
    )
    .await;
    for field in ["open", "secret", "seen"] {
        create_direct_resolver(&router, &api.api_id, "Query", field, 0).await;
    }

    /// One row: the token presented, the status and data the request must be answered with,
    /// and the one error it must report where it reports one.
    struct Row {
        case: &'static str,
        token: &'static str,
        query: &'static str,
        status: u16,
        data: Value,
        error: Option<(&'static str, &'static str)>,
    }
    // The rows differ along WHAT the authorizer answered for the token presented, and along the
    // token the validation expression rejects before any invocation is made for it.
    let rows: Vec<Row> = vec![
        Row {
            case: "an authorized call with no field denied, which resolves every field",
            token: "allow-open",
            query: "query { open secret }",
            status: 200,
            data: json!({"open": "open-value", "secret": "secret-value"}),
            error: None,
        },
        Row {
            case: "an authorized call with one field denied, which resolves the rest",
            token: "allow-denied",
            query: "query { open secret }",
            status: 200,
            data: json!({"open": "open-value", "secret": null}),
            error: Some((DENIED_ERROR_TYPE, "Not Authorized to access secret on type Query")),
        },
        Row {
            case: "a call the authorizer did not authorize, which reaches no resolver",
            token: "deny-me",
            query: "query { open secret }",
            status: 401,
            data: Value::Null,
            error: Some((UNAUTHORIZED_ERROR_TYPE, UNAUTHORIZED_MESSAGE)),
        },
        Row {
            case: "a token the validation expression rejects before the authorizer is invoked",
            token: "nope",
            query: "query { open secret }",
            status: 401,
            data: Value::Null,
            error: Some((UNAUTHORIZED_ERROR_TYPE, UNAUTHORIZED_MESSAGE)),
        },
        Row {
            // The API configures no `AWS_IAM` mode, so nothing of it reads a signature: what
            // such a call presented is this mode's input like any other, and it is decided
            // rather than served unchecked.
            case: "a signed call on an API that set up no mode that reads a signature",
            token: "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/us-east-1/\
                    appsync/aws4_request, SignedHeaders=host, Signature=0000",
            query: "query { open secret }",
            status: 401,
            data: Value::Null,
            error: Some((UNAUTHORIZED_ERROR_TYPE, UNAUTHORIZED_MESSAGE)),
        },
    ];
    for row in rows {
        let (status, answered) = post_published(
            row.case,
            &api,
            &json!({"query": row.query}),
            &[("authorization", row.token.to_string())],
        )
        .await;
        assert_eq!(
            status, row.status,
            "case {:?}: expected HTTP {}, got {status} {answered}",
            row.case, row.status
        );
        assert_eq!(
            answered["data"], row.data,
            "case {:?}: expected data {}, got {answered}",
            row.case, row.data
        );
        let errors = reported_errors(&answered);
        match row.error {
            None => assert!(
                answered.get("errors").is_none(),
                "case {:?}: expected no errors key, got {answered}",
                row.case
            ),
            Some((error_type, message)) => {
                assert_eq!(
                    errors.len(),
                    1,
                    "case {:?}: expected exactly one error, got {errors:?}",
                    row.case
                );
                assert_eq!(
                    (errors[0].0.as_str(), errors[0].1.as_str()),
                    (error_type, message),
                    "case {:?}: expected the error {error_type:?}/{message:?}, got {errors:?}",
                    row.case
                );
            }
        }
    }

    // What the authorizer PUBLISHED about the caller is what a resolver reads as
    // `$context.identity.resolverContext`, whichever runtime reads it: the handler of a Direct
    // Lambda resolver, a VTL mapping template, and APPSYNC_JS code.
    appsync_ok(
        &router,
        "POST",
        &format!("/v1/apis/{}/datasources", api.api_id),
        Some(json!({"name": LOCAL_DATA_SOURCE, "type": "NONE"})),
    )
    .await;
    appsync_ok(
        &router,
        "POST",
        &format!("/v1/apis/{}/types/Query/resolvers", api.api_id),
        Some(json!({
            "fieldName": "vtlContext",
            "kind": "UNIT",
            "dataSourceName": LOCAL_DATA_SOURCE,
            "requestMappingTemplate": CONTEXT_REQUEST_TEMPLATE,
            "responseMappingTemplate": CONTEXT_RESPONSE_TEMPLATE,
        })),
    )
    .await;
    appsync_ok(
        &router,
        "POST",
        &format!("/v1/apis/{}/types/Query/resolvers", api.api_id),
        Some(json!({
            "fieldName": "jsContext",
            "kind": "UNIT",
            "dataSourceName": LOCAL_DATA_SOURCE,
            "code": CONTEXT_CODE,
            "runtime": {"name": "APPSYNC_JS", "runtimeVersion": "1.0.0"},
        })),
    )
    .await;
    let case = "the resolverContext the authorizer published";
    let (status, answered) = post_published(
        case,
        &api,
        &json!({"query": "query { seen { resolverContext } vtlContext jsContext }"}),
        &[("authorization", "allow-open".to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {answered}"
    );
    let published = answered["data"]["seen"]["resolverContext"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert!(
        !published.is_empty(),
        "case {case:?}: expected the resolver to read what the authorizer published, got \
         {answered}"
    );
    // Every runtime reads the SAME published value, so none of the three can read it
    // differently from the others.
    for (reader, field) in [
        ("a VTL mapping template", "vtlContext"),
        ("APPSYNC_JS code", "jsContext"),
    ] {
        assert_eq!(
            answered["data"][field].as_str(),
            Some(published.as_str()),
            "case {case:?}: expected {reader} to read {published:?} as the published value, \
             got {answered}"
        );
    }
}

/// RULE 2, the reuse window: a positive `ttlOverride` is exactly one invocation of the
/// authorizer inside it, and a zero one is an invocation per request.
#[tokio::test]
async fn a_positive_result_ttl_caches_exactly_one_authorizer_invocation() {
    let router = build_router();
    let resolver = create_function(&router, "cached-handler", RESOLVER_HANDLER).await;
    let authorizer = create_function(&router, "cached-authorizer", AUTHORIZER_HANDLER).await;
    let role = create_role(&router, "appsync-cached", &allowing("lambda:InvokeFunction")).await;
    let api = create_api(
        &router,
        "lambda-cached",
        "AWS_LAMBDA",
        Some(json!({"authorizerUri": authorizer})),
        &resolver,
        &role,
    )
    .await;
    create_direct_resolver(&router, &api.api_id, "Query", "seen", 0).await;

    /// One row: the token presented, and whether the decision taken for it may be reused.
    struct Row {
        case: &'static str,
        token: &'static str,
        cached: bool,
    }
    // The rows differ along the window the authorizer stated for its own decision: the positive
    // one it returns for `allow-cached`, and the zero one — "which disables caching" — it
    // returns for every other token.
    let rows: Vec<Row> = vec![
        Row {
            case: "a positive ttlOverride, which is one invocation for both requests",
            token: "allow-cached",
            cached: true,
        },
        Row {
            case: "a ttlOverride of zero, which is one invocation per request",
            token: "allow-open",
            cached: false,
        },
    ];
    for row in rows {
        let mut published = Vec::new();
        for attempt in 0..2 {
            let (status, answered) = post_published(
                row.case,
                &api,
                &json!({"query": "query { seen { resolverContext } }"}),
                &[("authorization", row.token.to_string())],
            )
            .await;
            assert_eq!(
                status, 200,
                "case {:?} request {attempt}: expected HTTP 200, got {status} {answered}",
                row.case
            );
            let read = answered["data"]["seen"]["resolverContext"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case {:?} request {attempt}: expected the resolver to read what the \
                         authorizer published, got {answered}",
                        row.case
                    )
                })
                .to_string();
            published.push(read);
        }
        let same = published[0] == published[1];
        assert_eq!(
            same, row.cached,
            "case {:?}: expected the two requests to share one authorizer invocation to be \
             {}, got the published values {published:?}",
            row.case, row.cached
        );
    }
}
