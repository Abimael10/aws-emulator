// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose batched Lambda resolver answers ONE element per sibling, where
//! one of those elements reports an error of its own item — end to end over the real
//! `MultiServiceRouter`, with AppSync, Lambda, IAM and CloudFormation mounted on one engine.
//!
//! RULE — each element of a batched (`BatchInvoke`) Lambda answer is the ANSWER of the one
//! sibling it belongs to: an element reporting an error of its own item (`errorMessage`, with
//! `errorType` naming its class) is the one sibling that did not resolve, reported as exactly one
//! GraphQL error of that sibling's own path carrying the element's own class and sentence, while
//! every OTHER sibling of the same invocation resolves from its own element as though nothing had
//! failed — through the step's own definition, which is handed the element as its `ctx.result`,
//! or, where the step carries no definition, from the `data` the per-item envelope carries. The
//! answer is HTTP 200 either way.
//!
//! Three services have to agree for the rule to hold — Lambda runs the function, IAM holds the
//! data source's service role, and AppSync publishes the endpoint, merges the resolutions and
//! applies the answer — so this is the suite that can pin it.
//!
//! The rows span what the rule generalises over: the FORM the batched definition is written in
//! (APPSYNC_JS code, VTL mapping templates, and no definition at all — the Direct Lambda
//! resolver), how many siblings one invocation merges, and WHICH of them failed (the first, one
//! in the middle, the last, none of them, and every one of them).

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

/// How long one invocation is given, as the function's own `Timeout`: well past what a run needs
/// on any host, because what is under test is what the handler computed and not how fast an
/// interpreter got through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// How long a case waits for a transition the clock owes, and how often it looks again. Both are
/// generous: what is under test is the transition, never how fast this host reaches it.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The data sources every resolver here resolves against: the function, and the one that is "no
/// data source" at all, which the parent list resolves against.
const DATA_SOURCE: &str = "handler";
const LOCAL_DATA_SOURCE: &str = "local";

/// "The maximum batching size for a resolver" (`Resolver$maxBatchSize`) every batched resolver
/// here configures: past every row's sibling count, so each row is ONE invocation.
const MAX_BATCH_SIZE: i32 = 10;

/// The class the handler reports for the item a row asks it to fail.
const ITEM_ERROR_TYPE: &str = "ItemNotFound";

/// The marker in an id that asks the handler to fail that item.
const FAILING_MARKER: &str = "fail";

/// The schema every API of this suite is created with: a list field taking the ids its parents
/// are made of, and one batched child field per definition form.
const SCHEMA: &str = "type Detail {\n  forId: String\n  batchLen: Int\n}\n\
                      \n\
                      type Post {\n  id: String\n  detail: Detail\n  \
                      vtlDetail: Detail\n  directDetail: Detail\n}\n\
                      \n\
                      type Query {\n  posts(ids: [ID!]!): [Post]\n}\n";

/// The handler every resolver of this suite invokes.
///
/// It answers from the EVENT alone: a batched invocation is a LIST of one event per merged
/// resolution, and each element of the answer is the per-item envelope AppSync documents —
/// `data`, beside the `errorMessage`/`errorType` an item that failed is reported under, which an
/// item that did not fail carries as nulls. The id of the item an event is about is the one the
/// payload a definition rendered names, or — for a Direct Lambda resolver, whose event is the
/// field's whole context document — the `id` of the parent the field is being resolved for. The
/// `batchLen` each item's data carries is the LENGTH of the event list, so it is what proves how
/// many resolutions one invocation merged.
const BATCH_HANDLER: &str = r#"
def _for_id(item):
    item = item or {}
    if "forId" in item:
        return item["forId"]
    return ((item.get("source") or {}).get("id"))


def _answered(item, size):
    for_id = _for_id(item)
    if "fail" in str(for_id):
        return {
            "data": None,
            "errorMessage": "item " + str(for_id) + " could not be read",
            "errorType": "ItemNotFound",
        }
    return {
        "data": {"forId": for_id, "batchLen": size},
        "errorMessage": None,
        "errorType": None,
    }


def handler(event, context):
    if isinstance(event, list):
        return [_answered(item, len(event)) for item in event]
    return [_answered(event, 1)]
"#;

/// The parent list's definition: it ends its own request half with the parents the document's
/// `ids` argument names, so no data source stands between the document and the batched children.
const PARENTS_CODE: &str = "export function request(ctx) {\n  \
                            runtime.earlyReturn(ctx.args.ids.map((id) => ({id})));\n}\n\
                            export function response(ctx) {\n  return ctx.result;\n}\n";

/// The APPSYNC_JS form of a batched child: its payload is the parent it is resolved FOR, so one
/// invocation carries one payload per sibling parent, and its response half renders the field
/// from the element the invocation answered for it — the `data` the per-item envelope carries.
const CHILD_CODE: &str = "export function request(ctx) {\n  \
                          return {operation: 'BatchInvoke', \
                          payload: {forId: ctx.source.id}};\n}\n\
                          export function response(ctx) {\n  return ctx.result.data;\n}\n";

/// The same definition written as VTL MAPPING TEMPLATES, which is the other form a definition
/// takes. Source for the request object's members: the request object of a Lambda data source
/// (`version`, `operation`, `payload`) in the AppSync resolver mapping-template reference.
const CHILD_REQUEST_TEMPLATE: &str = r#"{"version": "2018-05-29", "operation": "BatchInvoke", "payload": {"forId": $util.toJson($ctx.source.id)}}"#;
const CHILD_RESPONSE_TEMPLATE: &str = "$util.toJson($ctx.result.data)";

/// The trust policy a function's EXECUTION role carries, which `CreateFunction` checks: the role
/// has to be assumable by the Lambda service principal. Written compactly and free of the
/// characters an `application/x-www-form-urlencoded` body treats specially.
const LAMBDA_TRUST_POLICY: &str = "{\"Version\":\"2012-10-17\",\"Statement\":\
                                   [{\"Effect\":\"Allow\",\"Principal\":\
                                   {\"Service\":\"lambda.amazonaws.com\"},\
                                   \"Action\":\"sts:AssumeRole\"}]}";

/// The whole emulator over ONE engine, exactly as `core/router/src/lib.rs` assembles it.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // Request authentication is off in this target: what a graded call pins is what the endpoint
    // ANSWERS, never signature verification.
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

/// Create the role `name`, trusted by AppSync and allowed to invoke any function, and answer its
/// ARN: the invocation is made under the data source's own service role, so the role has to allow
/// it for any field to resolve at all.
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

/// One created GraphQL API: where it is served, and a live API key.
struct Api {
    /// The URL the API publishes for its `GRAPHQL` endpoint, which is what a client POSTs to.
    graphql_uri: String,
    api_key: String,
}

/// Create one API with [`SCHEMA`], the `AWS_LAMBDA` data source that invokes `function_arn` under
/// `role_arn`, the local data source the parent list resolves against, and one batched child
/// resolver per definition form.
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
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": "posts",
            "kind": "UNIT",
            "dataSourceName": LOCAL_DATA_SOURCE,
            "code": PARENTS_CODE,
            "runtime": {"name": "APPSYNC_JS", "runtimeVersion": "1.0.0"},
        })),
    )
    .await;
    // One batched child per definition FORM: APPSYNC_JS code, VTL mapping templates, and the
    // Direct Lambda resolver, which carries no definition at all.
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
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Post/resolvers"),
        Some(json!({
            "fieldName": "vtlDetail",
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "maxBatchSize": MAX_BATCH_SIZE,
            "requestMappingTemplate": CHILD_REQUEST_TEMPLATE,
            "responseMappingTemplate": CHILD_RESPONSE_TEMPLATE,
        })),
    )
    .await;
    appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Post/resolvers"),
        Some(json!({
            "fieldName": "directDetail",
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "maxBatchSize": MAX_BATCH_SIZE,
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

/// POST one GraphQL document to the URL the API PUBLISHED, as a client does, carrying the API's
/// own key: the host, the port and the path all come from `uris.GRAPHQL` rather than from
/// anything this suite knows.
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
fn reported_errors(answered: &Value) -> Vec<(String, String, Value)> {
    answered["errors"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["errorType"].as_str().unwrap_or_default().to_string(),
                        entry["message"].as_str().unwrap_or_default().to_string(),
                        entry["path"].clone(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ids one row's parents are made of, and which of those items the handler fails: the rows
/// differ along how many siblings one invocation merges and WHERE among them the failing item
/// sits.
const ROWS: &[(&str, &[&str])] = &[
    ("two siblings, the second item failing", &["p-one", "fail-two"]),
    ("two siblings, the first item failing", &["fail-one", "p-two"]),
    (
        "four siblings, one item in the middle failing",
        &["p-one", "p-two", "fail-three", "p-four"],
    ),
    (
        "four siblings, the last item failing",
        &["p-one", "p-two", "p-three", "fail-four"],
    ),
    (
        "three siblings, no item failing",
        &["p-one", "p-two", "p-three"],
    ),
    (
        "three siblings, every item failing",
        &["fail-one", "fail-two", "fail-three"],
    ),
];

/// The batched child field each definition FORM is written for, and what a failing assertion
/// calls that form.
const FORMS: [(&str, &str); 3] = [
    ("a definition written in APPSYNC_JS code", "detail"),
    ("a definition written in mapping templates", "vtlDetail"),
    ("no definition at all, a Direct Lambda resolver", "directDetail"),
];

/// RULE: a per-element error of a batched Lambda answer isolates the sibling whose element
/// reported it — that sibling alone resolves to nothing and is reported as one error of its own
/// path, carrying the element's own class and sentence, and every other sibling of the invocation
/// resolves from its own element.
#[tokio::test]
async fn a_batch_item_error_isolates_the_sibling_whose_element_reported_it() {
    let router = build_router();
    let function = create_function(&router, "batch-item-errors-handler", BATCH_HANDLER).await;
    let role = create_role(&router, "appsync-batch-item-errors").await;
    let api = create_api(&router, "batch-item-errors", &function, &role).await;

    for (form, field) in FORMS {
        for (row, ids) in ROWS {
            let case = format!("{form}: {row}");
            let case = case.as_str();
            let query = format!(
                "query Batched($ids: [ID!]!) {{ posts(ids: $ids) \
                 {{ id {field} {{ forId batchLen }} }} }}"
            );
            let (status, answered) =
                post_published(case, &api, &json!({"query": query, "variables": {"ids": ids}}))
                    .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} {answered}"
            );
            // Every sibling of one invocation: the item that failed resolves to nothing, and
            // each other resolves from its own element, whose `batchLen` is the length of the
            // one event list all of them travelled in.
            let merged = i64::try_from(ids.len()).unwrap_or_default();
            let expected: Vec<Value> = ids
                .iter()
                .map(|id| match id.contains(FAILING_MARKER) {
                    true => json!({"id": id, field: null}),
                    false => json!({"id": id, field: {"forId": id, "batchLen": merged}}),
                })
                .collect();
            assert_eq!(
                answered["data"],
                json!({"posts": expected}),
                "case {case:?}: expected every sibling but the failing one to resolve from its \
                 own element of one invocation, got {answered}"
            );
            // One error per element that reported one, on that element's own sibling's path,
            // with the class and the sentence the element itself carried.
            let expected_errors: Vec<(String, String, Value)> = ids
                .iter()
                .enumerate()
                .filter(|(_, id)| id.contains(FAILING_MARKER))
                .map(|(at, id)| {
                    (
                        ITEM_ERROR_TYPE.to_string(),
                        format!("item {id} could not be read"),
                        json!(["posts", at, field]),
                    )
                })
                .collect();
            let reported = reported_errors(&answered);
            assert_eq!(
                reported, expected_errors,
                "case {case:?}: expected one error per failing item, on its own sibling's path, \
                 got {reported:?} in {answered}"
            );
        }
    }
}
