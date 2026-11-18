// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose resolver DEFINITION asks for the batched invocation of its
//! Lambda data source — end to end over the real `MultiServiceRouter`, with AppSync, Lambda,
//! IAM and CloudFormation mounted on one engine.
//!
//! RULE — where a resolver's request half names the `BatchInvoke` operation of an `AWS_LAMBDA`
//! data source, the sibling resolutions of that resolver in one GraphQL request are merged
//! into ONE invocation of its function whose event is the bare LIST of the payloads those
//! request halves rendered, as many of them as the resolver's maximum batching size
//! (`Resolver$maxBatchSize`) and the resolutions past it in a further invocation; the returned
//! list supplies each sibling its own element, in the order the siblings were merged. An
//! element that reports an error of its own item — `errorMessage`, with `errorType` naming its
//! class — is the one sibling that did not resolve: it is reported as exactly one error of
//! that sibling's own path, carrying the element's own class and sentence, its value is the
//! element's `data`, and every other sibling of the invocation resolves from its own element's
//! `data` as though nothing had failed.
//!
//! Three services have to agree for the rule to hold — Lambda runs the function, IAM holds the
//! data source's service role, and AppSync publishes the endpoint, renders the payloads, merges
//! them and applies the answer — so this is the suite that can pin it.
//!
//! The rows span what the rule generalises over: the FORM the definition is written in (VTL
//! mapping templates and APPSYNC_JS code), how many siblings one invocation merges (one, two,
//! three, and one past the batching size), and whether an element reported an error of its own
//! item (none, and exactly one of two).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here signs for.
const REGION: &str = "us-east-1";

/// The long-term key every emulator publishes, which every setup call is signed with.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The path a GraphQL API's `GRAPHQL` endpoint serves.
const GRAPHQL_PATH: &str = "/graphql";

/// Where `GetFunctionConfiguration` is bound (`GET /2015-03-31/functions/<name>/configuration`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `State` a function reports once it can be invoked, and the status a stack settles at.
const FUNCTION_ACTIVE: &str = "Active";
const STACK_CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The runtime and the handler entry point of the package below.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is what the handler computed and not how fast
/// an interpreter got through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long a case waits for a transition the clock owes, and how often it looks again. Both
/// are generous: what is under test is the transition, never how fast this host reaches it.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The data source every resolver here resolves against.
const DATA_SOURCE: &str = "handler";

/// "The maximum batching size for a resolver" (`Resolver$maxBatchSize`) every resolver of this
/// suite configures: three siblings fit in one invocation, and a fourth makes a second one.
const MAX_BATCH_SIZE: i32 = 3;

/// The `errorType` and the sentence the handler reports for the item a row asks it to fail.
const ITEM_ERROR_TYPE: &str = "ItemNotFound";

/// The schema every API of this suite is created with: one field per definition form, each
/// answering the item its own element carried, and the parent list whose own child field is
/// resolved for every element of it.
const SCHEMA: &str = "type Item {\n  id: String\n  batchSize: Int\n}\n\
                      \n\
                      type Post {\n  id: String\n  detail: Item\n}\n\
                      \n\
                      type Query {\n  item(id: String, fail: Boolean): Item\n  \
                      jsItem(id: String, fail: Boolean): Item\n  posts: [Post]\n}\n";

/// The handler every resolver of this suite invokes.
///
/// It answers from the EVENT alone: a batched invocation is a LIST of the payloads the request
/// halves rendered, and each element of the answer is the per-item envelope AppSync reads
/// (`data`, plus `errorMessage`/`errorType` for an item that failed). The `batchSize` each
/// item's data carries is the LENGTH of the event list the handler was given, so it is what
/// proves how many resolutions one invocation merged. An event that is not a list is one
/// resolution on its own, which this handler answers for too so an unmerged invocation is
/// visible rather than silent.
const BATCH_HANDLER: &str = r#"
def _answered(item, size):
    if item.get("fail"):
        return {
            "data": None,
            "errorMessage": "item " + str(item.get("id")) + " could not be read",
            "errorType": "ItemNotFound",
        }
    return {"data": {"id": item.get("id"), "batchSize": size}}


def handler(event, context):
    if isinstance(event, list):
        return [_answered(item, len(event)) for item in event]
    return {"data": {"id": (event or {}).get("id"), "batchSize": 0}}
"#;

/// The request half of the VTL definition: it names the batched operation and renders the one
/// item its own resolution is about. Source for the members: the request object of a Lambda
/// data source (`version`, `operation`, `payload`) in the AppSync resolver mapping-template
/// reference.
const BATCH_REQUEST_TEMPLATE: &str = r#"{"version": "2018-05-29", "operation": "BatchInvoke", "payload": {"id": $util.toJson($ctx.args.id), "fail": $util.toJson($ctx.args.fail)}}"#;

/// Its response half: the value of the field is the item's own result.
const BATCH_RESPONSE_TEMPLATE: &str = "$util.toJson($ctx.result)";

/// The same definition written in APPSYNC_JS `code`, which is the other form a definition takes.
const BATCH_CODE: &str = "export function request(ctx) {\n  \
                          return {operation: 'BatchInvoke', \
                          payload: {id: ctx.args.id, fail: ctx.args.fail}};\n}\n\
                          export function response(ctx) {\n  return ctx.result;\n}\n";

/// The data source the parent list resolves against: `NONE` is "there is no data source", so
/// the parents are the request half's own payload and no invocation stands between the parents
/// and the child field whose resolutions are merged.
const LOCAL_DATA_SOURCE: &str = "local";

/// The ids the parent list resolves to, in order: the middle one is the item the handler fails,
/// so the error belongs to a sibling in the MIDDLE of the invocation rather than at its edge.
const PARENT_IDS: [&str; 3] = ["p-one", "fail-me", "p-three"];

/// The parent list's own definition, which carries no data source call of its own.
fn parents_code() -> String {
    let parents: Vec<String> = PARENT_IDS
        .iter()
        .map(|id| format!("{{id: '{id}'}}"))
        .collect();
    format!(
        "export function request(ctx) {{ return {{payload: [{}]}}; }}\n\
         export function response(ctx) {{ return ctx.result; }}\n",
        parents.join(", ")
    )
}

/// The child field's definition: its payload is the parent it is resolved FOR, so one
/// invocation carries one payload per sibling parent. An id the handler fails is named by the
/// payload itself, which is how a row asks for a per-item failure without a field argument.
const CHILD_CODE: &str = "export function request(ctx) {\n  \
                          return {operation: 'BatchInvoke', \
                          payload: {id: ctx.source.id, \
                          fail: ctx.source.id.indexOf('fail') === 0}};\n}\n\
                          export function response(ctx) {\n  return ctx.result;\n}\n";

/// The trust policy a function's EXECUTION role carries, which `CreateFunction` checks: the
/// role has to be assumable by the Lambda service principal. Written compactly and free of the
/// characters an `application/x-www-form-urlencoded` body treats specially.
const LAMBDA_TRUST_POLICY: &str = "{\"Version\":\"2012-10-17\",\"Statement\":\
                                   [{\"Effect\":\"Allow\",\"Principal\":\
                                   {\"Service\":\"lambda.amazonaws.com\"},\
                                   \"Action\":\"sts:AssumeRole\"}]}";

/// The whole emulator over ONE engine, exactly as `core/router/src/lib.rs` assembles it.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Request authentication is off in this target: what a graded call pins is what the
    // endpoint ANSWERS, never signature verification.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential is the ambient key. The signature is never
/// checked here.
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

/// Deploy the function `name` from `source` and answer the ARN Lambda reports for it, once that
/// service reports it `Active`.
///
/// The handler is declared as a stack's INLINE source, which CloudFormation archives into the
/// deployment package a function is created from. Both the stack's completion and the
/// function's own provisioning window are owed by the clock, so this looks until each holds
/// rather than sleeping a span on either service's behalf.
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
            "Batched": {
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

/// Create the role `name`, trusted by AppSync, allowed to invoke any function, and answer its
/// ARN: the invocation is made under the data source's own service role, so the role has to
/// allow it for any field to resolve at all.
async fn create_role(router: &MultiServiceRouter, name: &str) -> String {
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
    let role_arn = crate::query_wire::opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("setup: CreateRole reports no <Arn>: {created}"));
    let document = "{\"Version\":\"2012-10-17\",\"Statement\":[{\"Effect\":\"Allow\",\
                    \"Action\":\"lambda:InvokeFunction\",\"Resource\":\"*\"}]}";
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

/// One created GraphQL API: what it is addressed by, where it is served, and a live API key.
struct Api {
    /// The URL the API publishes for its `GRAPHQL` endpoint, which is what a client POSTs to.
    graphql_uri: String,
    api_key: String,
}

/// Create one API with [`SCHEMA`], the `AWS_LAMBDA` data source that invokes `function_arn`
/// under `role_arn`, and one batched resolver per definition form.
async fn create_api(
    router: &MultiServiceRouter,
    name: &str,
    function_arn: &str,
    role_arn: &str,
) -> Api {
    let created = appsync_ok(
        router,
        "POST",
        "/v1/apis",
        Some(json!({"name": name, "authenticationType": "API_KEY"})),
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
        Some(json!({
            "name": DATA_SOURCE,
            "type": "AWS_LAMBDA",
            "serviceRoleArn": role_arn,
            "lambdaConfig": {"lambdaFunctionArn": function_arn},
        })),
    )
    .await;
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": LOCAL_DATA_SOURCE, "type": "NONE"})),
    )
    .await;
    // The two definition FORMS of one rule: mapping templates, and APPSYNC_JS code.
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": "item",
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "maxBatchSize": MAX_BATCH_SIZE,
            "requestMappingTemplate": BATCH_REQUEST_TEMPLATE,
            "responseMappingTemplate": BATCH_RESPONSE_TEMPLATE,
        })),
    )
    .await;
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": "jsItem",
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "maxBatchSize": MAX_BATCH_SIZE,
            "code": BATCH_CODE,
            "runtime": {"name": "APPSYNC_JS", "runtimeVersion": "1.0.0"},
        })),
    )
    .await;
    // The parent list, and the child field of every element of it: the child's resolutions
    // across those SIBLING PARENTS are what one invocation merges.
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": "posts",
            "kind": "UNIT",
            "dataSourceName": LOCAL_DATA_SOURCE,
            "code": parents_code(),
            "runtime": {"name": "APPSYNC_JS", "runtimeVersion": "1.0.0"},
        })),
    )
    .await;
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Post/resolvers"),
        Some(json!({
            "fieldName": "detail",
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "maxBatchSize": MAX_BATCH_SIZE,
            "code": CHILD_CODE,
            "runtime": {"name": "APPSYNC_JS", "runtimeVersion": "1.0.0"},
        })),
    )
    .await;
    let key = appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/apikeys"),
        Some(json!({"description": "the suite's key"})),
    )
    .await;
    Api {
        graphql_uri,
        api_key: key["apiKey"]["id"].as_str().unwrap_or_default().to_string(),
    }
}

/// POST one GraphQL document to the URL the API PUBLISHED, as a client does, carrying the
/// API's own key: the host, the port and the path all come from `uris.GRAPHQL` rather than from
/// anything this suite knows.
///
/// The socket client itself is this target's one reading of how a client reaches a published
/// GraphQL endpoint ([`crate::appsync_lambda_data_source_and_authorizer::post_published_uri`]).
async fn post_published(case: &str, api: &Api, body: &Value) -> (u16, Value) {
    crate::appsync_lambda_data_source_and_authorizer::post_published_uri(
        case,
        &api.graphql_uri,
        body,
        &[("x-api-key", api.api_key.clone())],
    )
    .await
}

/// The `errors` a result reports, as the `(errorType, message, path)` each entry states.
fn reported_errors(answered: &Value) -> Vec<(String, String, String)> {
    answered["errors"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["errorType"].as_str().unwrap_or_default().to_string(),
                        entry["message"].as_str().unwrap_or_default().to_string(),
                        entry["path"][0].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

#[tokio::test]
async fn a_batch_invoke_resolver_merges_its_sibling_resolutions_into_one_invocation() {
    let router = build_router();
    let function = create_function(&router, "batch-invoke-handler", BATCH_HANDLER).await;
    let role = create_role(&router, "appsync-batch-invoke").await;
    let api = create_api(&router, "batch-invoke", &function, &role).await;

    /// One row: the document, the data it must be answered with, and the errors it must report
    /// as `(errorType, message, path)`.
    struct Row {
        case: &'static str,
        query: String,
        data: Value,
        errors: Vec<(String, String, String)>,
    }

    // One set of rows per definition FORM, because the rule is about what the request half
    // NAMED and not about the runtime that named it.
    for (form, field) in [
        ("a definition written in mapping templates", "item"),
        ("a definition written in APPSYNC_JS code", "jsItem"),
    ] {
        // The rows differ along HOW MANY siblings one invocation merges, and along whether one
        // of them failed on its own: the `batchSize` each answer carries is the length of the
        // event list the handler was given, so it is what proves the merge.
        let rows: Vec<Row> = vec![
            Row {
                case: "one resolution, which travels as a batch of one",
                query: format!("query {{ only: {field}(id: \"a\") {{ id batchSize }} }}"),
                data: json!({"only": {"id": "a", "batchSize": 1}}),
                errors: Vec::new(),
            },
            Row {
                case: "two sibling resolutions merged into one invocation",
                query: format!(
                    "query {{ first: {field}(id: \"a\") {{ id batchSize }} \
                     second: {field}(id: \"b\") {{ id batchSize }} }}"
                ),
                data: json!({
                    "first": {"id": "a", "batchSize": 2},
                    "second": {"id": "b", "batchSize": 2},
                }),
                errors: Vec::new(),
            },
            Row {
                case: "three sibling resolutions, which is the resolver's whole batching size",
                query: format!(
                    "query {{ first: {field}(id: \"a\") {{ batchSize }} \
                     second: {field}(id: \"b\") {{ batchSize }} \
                     third: {field}(id: \"c\") {{ batchSize }} }}"
                ),
                data: json!({
                    "first": {"batchSize": 3},
                    "second": {"batchSize": 3},
                    "third": {"batchSize": 3},
                }),
                errors: Vec::new(),
            },
            Row {
                case: "four siblings, one past the batching size, which is two invocations",
                query: format!(
                    "query {{ first: {field}(id: \"a\") {{ batchSize }} \
                     second: {field}(id: \"b\") {{ batchSize }} \
                     third: {field}(id: \"c\") {{ batchSize }} \
                     fourth: {field}(id: \"d\") {{ batchSize }} }}"
                ),
                data: json!({
                    "first": {"batchSize": 3},
                    "second": {"batchSize": 3},
                    "third": {"batchSize": 3},
                    "fourth": {"batchSize": 1},
                }),
                errors: Vec::new(),
            },
            Row {
                case: "an element reporting an error of its own item, which nulls only that \
                       sibling",
                query: format!(
                    "query {{ kept: {field}(id: \"a\") {{ id batchSize }} \
                     failed: {field}(id: \"b\", fail: true) {{ id batchSize }} }}"
                ),
                data: json!({"kept": {"id": "a", "batchSize": 2}, "failed": null}),
                errors: vec![(
                    ITEM_ERROR_TYPE.to_string(),
                    "item b could not be read".to_string(),
                    "failed".to_string(),
                )],
            },
        ];
        for row in rows {
            let case = format!("{form}: {}", row.case);
            let (status, answered) =
                post_published(&case, &api, &json!({"query": row.query})).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} {answered}"
            );
            assert_eq!(
                answered["data"], row.data,
                "case {case:?}: expected the data {}, got {answered}",
                row.data
            );
            let reported = reported_errors(&answered);
            assert_eq!(
                reported, row.errors,
                "case {case:?}: expected the errors {:?}, got {reported:?}",
                row.errors
            );
        }
    }
}

/// The same rule across SIBLING PARENTS: the resolutions of ONE batched child field over the
/// elements its parent resolved to are merged into one invocation, each element of the answer
/// resolves the parent it belongs to, and an element reporting an error of its own item nulls
/// only that parent's child while every other parent's child resolves.
#[tokio::test]
async fn a_batch_invoke_resolver_merges_one_field_across_its_sibling_parents() {
    let router = build_router();
    let function = create_function(&router, "batch-parents-handler", BATCH_HANDLER).await;
    let role = create_role(&router, "appsync-batch-parents").await;
    let api = create_api(&router, "batch-parents", &function, &role).await;

    let case = "one batched child field across the parents its list resolved to";
    let (status, answered) = post_published(
        case,
        &api,
        &json!({"query": "query { posts { id detail { id batchSize } } }"}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {answered}"
    );
    // The `batchSize` each child's answer carries is the length of the event list the handler
    // was given, so the same size in every child that resolved is the ONE invocation their
    // resolutions shared — including the parent whose own item failed, which the handler
    // answered inside that same list.
    let merged = i64::try_from(PARENT_IDS.len()).unwrap_or_default();
    let expected: Vec<Value> = PARENT_IDS
        .iter()
        .map(|id| match id.starts_with("fail") {
            true => json!({"id": id, "detail": null}),
            false => json!({"id": id, "detail": {"id": id, "batchSize": merged}}),
        })
        .collect();
    assert_eq!(
        answered["data"],
        json!({"posts": expected}),
        "case {case:?}: expected every parent's child to resolve from its own element of one \
         invocation, got {answered}"
    );
    let failing = PARENT_IDS
        .iter()
        .position(|id| id.starts_with("fail"))
        .unwrap_or_default();
    let reported = reported_errors(&answered);
    assert_eq!(
        reported,
        vec![(
            ITEM_ERROR_TYPE.to_string(),
            format!("item {} could not be read", PARENT_IDS[failing]),
            "posts".to_string(),
        )],
        "case {case:?}: expected exactly one error, for the one item that failed, got \
         {reported:?}"
    );
    // That error belongs to the failing PARENT's own child and to no other, which is where the
    // result reports a field that did not resolve.
    assert_eq!(
        answered["errors"][0]["path"],
        json!(["posts", failing, "detail"]),
        "case {case:?}: expected the error on the failing parent's own path, got {answered}"
    );
}
