// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose batched Lambda resolvers differ only in the maximum batching size
//! they configure — end to end over the real `MultiServiceRouter`, with AppSync, Lambda, IAM and
//! CloudFormation mounted on one engine.
//!
//! RULE — "the maximum batching size for a resolver" (`Resolver$maxBatchSize`, an integer whose
//! range is 0 to 2000, per `smithy_query(query="shape",shape="Resolver,MaxBatchSize")`) is HOW
//! MANY of that resolver's sibling resolutions in one GraphQL request one invocation of its
//! `AWS_LAMBDA` data source carries: the siblings are merged in request order into invocations of
//! at most that many, each invocation's event being the bare LIST of the payloads those
//! resolutions rendered, and each element of the returned list supplying the sibling whose payload
//! sat at the same position. So the same request over the same siblings is one invocation under a
//! size that covers them all and several under a smaller one, and an element reporting an error of
//! its own item (`errorMessage`, with `errorType` naming its class) nulls that one sibling and
//! reports exactly one error of its path, wherever among the invocations its own resolution
//! travelled.
//!
//! Three services have to agree for the rule to hold — Lambda runs the function, IAM holds the
//! data source's service role, and AppSync merges the resolutions and applies the answer — so this
//! is the suite that can pin it. The sibling suites
//! [`crate::appsync_lambda_batch_invoke_resolvers`] and
//! [`crate::appsync_lambda_batch_invoke_item_errors`] span the definition FORMS and the position
//! of a failing item under ONE batching size; what these rows add is the SIZE itself.
//!
//! The rows span what the rule generalises over: the batching size, over its published range —
//! one resolution per invocation, two, three, and the largest size a resolver may configure — and,
//! for the per-item failure, which invocation the failing sibling travelled in (one of its own,
//! the first of two, and the second of two).

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::appsync_lambda_data_source_and_authorizer::{
    allowing, appsync_ok, build_router, create_function, create_role, post_published_uri,
};

/// The path a GraphQL API's `GRAPHQL` endpoint serves.
const GRAPHQL_PATH: &str = "/graphql";

/// The `runtime` a resolver defined by `code` names. Source: `AppSyncRuntime`'s own members
/// (`smithy_query(query="shape",shape="AppSyncRuntime,RuntimeName")`).
const APPSYNC_JS: (&str, &str) = ("APPSYNC_JS", "1.0.0");

/// The IAM action an invocation of a function performs, which the data source's service role has
/// to allow for any field of it to resolve at all.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The data source every resolver here resolves against.
const DATA_SOURCE: &str = "handler";

/// The `errorType` and the sentence the handler reports for the item a row asks it to fail.
const ITEM_ERROR_TYPE: &str = "ItemNotFound";

/// How long a poll loop waits for a state another service owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches a
/// transition.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The batching sizes the suite's resolvers configure, and the field each is attached to: the
/// smallest size that batches at all, two, three, and the largest a resolver may configure
/// (`MaxBatchSize`'s own range maximum).
const SIZES: [(&str, i32); 4] = [
    ("batchOne", 1),
    ("batchTwo", 2),
    ("batchThree", 3),
    ("batchMax", 2000),
];

/// The ids one row's siblings are resolved for, in the order the document selects them.
const SIBLING_IDS: [&str; 4] = ["a", "b", "c", "d"];

/// The schema every API of this suite is created with: one field per batching size, each
/// answering the item its own element of the invocation carried.
const SCHEMA: &str = "type Item {\n  id: String\n  batchSize: Int\n}\n\
                      \n\
                      type Query {\n  \
                      batchOne(id: String, fail: Boolean): Item\n  \
                      batchTwo(id: String, fail: Boolean): Item\n  \
                      batchThree(id: String, fail: Boolean): Item\n  \
                      batchMax(id: String, fail: Boolean): Item\n}\n";

/// The handler every resolver of this suite invokes.
///
/// It answers from the EVENT alone, one per-item envelope per element: the `batchSize` an item's
/// data carries is the LENGTH of the event list the handler was given, so it is what says how many
/// resolutions the invocation that item travelled in merged. An event that is not a list is one
/// resolution on its own, answered with a length of zero so an unmerged invocation is visible
/// rather than silent.
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

/// The definition every batched resolver here is written in: its request half names the batched
/// operation and renders the one item its own resolution is about, and its response half takes the
/// field's value from the per-item envelope its own element carried.
const BATCH_CODE: &str = "export function request(ctx) {\n  \
                          return {operation: 'BatchInvoke', \
                          payload: {id: ctx.args.id, fail: ctx.args.fail}};\n}\n\
                          export function response(ctx) {\n  return ctx.result.data;\n}\n";

/// Create one `API_KEY` API with [`SCHEMA`], the `AWS_LAMBDA` data source that invokes
/// `function_arn` under `role_arn`, and one batched resolver per [`SIZES`] entry; answer the URL
/// the API published for its GraphQL endpoint and a live API key.
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
    // One resolver per batching size, identical in every other respect: the size is the only
    // thing a row varies.
    for (field, size) in SIZES {
        appsync_ok(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers"),
            Some(json!({
                "fieldName": field,
                "kind": "UNIT",
                "dataSourceName": DATA_SOURCE,
                "maxBatchSize": size,
                "code": BATCH_CODE,
                "runtime": {"name": APPSYNC_JS.0, "runtimeVersion": APPSYNC_JS.1},
            })),
        )
        .await;
    }
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

/// One created GraphQL API: where it is served, and a live API key.
struct Api {
    /// The URL the API publishes for its `GRAPHQL` endpoint, which is what a client POSTs to.
    graphql_uri: String,
    api_key: String,
}

/// POST one GraphQL document to the URL the API PUBLISHED, as a client does, carrying the API's
/// own key: the host, the port and the path all come from `uris.GRAPHQL` rather than from anything
/// this suite knows.
async fn post_published(case: &str, api: &Api, document: &str) -> (u16, Value) {
    post_published_uri(
        case,
        &api.graphql_uri,
        &json!({"query": document}),
        &[("x-api-key", api.api_key.clone())],
    )
    .await
}

/// The document one row POSTs: the `field` resolved for every id of [`SIBLING_IDS`], each under
/// its own response key, with `failing` the position of the one item the handler is asked to fail
/// (none where it is `None`).
fn document(field: &str, failing: Option<usize>) -> String {
    let selections: Vec<String> = SIBLING_IDS
        .iter()
        .enumerate()
        .map(|(at, id)| {
            let fail = failing == Some(at);
            format!("s{at}: {field}(id: \"{id}\", fail: {fail}) {{ id batchSize }}")
        })
        .collect();
    format!("query {{ {} }}", selections.join(" "))
}

/// How many resolutions the invocation that the sibling at `at` travelled in merged, where the
/// resolver merges at most `size` of them: a full invocation for every group of `size` siblings in
/// request order, and whatever is left over in the last one.
fn merged_with(at: usize, size: usize) -> i64 {
    let invocation = at / size;
    let merged = (SIBLING_IDS.len() - invocation * size).min(size);
    i64::try_from(merged).unwrap_or_default()
}

/// The `errors` a result reports, as the `(path, errorType, message)` each entry states.
fn reported_errors(answered: &Value) -> Vec<(String, String, String)> {
    answered["errors"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    (
                        entry["path"][0].as_str().unwrap_or_default().to_string(),
                        entry["errorType"].as_str().unwrap_or_default().to_string(),
                        entry["message"].as_str().unwrap_or_default().to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The batching size is how many sibling resolutions one invocation carries: the same four
/// siblings travel as one invocation under a size that covers them all and as several under a
/// smaller one, and each sibling resolves from the element at its own position.
#[tokio::test]
async fn the_batching_size_decides_how_many_siblings_one_invocation_carries() {
    let router = build_router();
    let function = create_function(&router, "batch-sizes-handler", BATCH_HANDLER).await;
    let role = create_role(&router, "appsync-batch-sizes", &allowing(INVOKE_ACTION)).await;
    let api = create_api(&router, "batch-sizes", &function, &role).await;

    for (field, size) in SIZES {
        let merged = usize::try_from(size).unwrap_or(1);
        let case = format!(
            "a resolver merging at most {size} resolutions per invocation, over {} siblings",
            SIBLING_IDS.len()
        );
        let case = case.as_str();
        let (status, answered) = post_published(case, &api, &document(field, None)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} {answered}"
        );
        let expected: Value = Value::Object(
            SIBLING_IDS
                .iter()
                .enumerate()
                .map(|(at, id)| {
                    (
                        format!("s{at}"),
                        json!({"id": id, "batchSize": merged_with(at, merged)}),
                    )
                })
                .collect(),
        );
        assert_eq!(
            answered["data"], expected,
            "case {case:?}: expected the data {expected}, got {answered}"
        );
        let reported = reported_errors(&answered);
        assert!(
            reported.is_empty(),
            "case {case:?}: expected every sibling to resolve with no error reported, got \
             {reported:?} in {answered}"
        );
    }
}

/// An element reporting an error of its own item nulls that one sibling and reports exactly one
/// error of its path, wherever among the invocations the siblings were split into its own
/// resolution travelled.
#[tokio::test]
async fn a_failing_item_isolates_its_sibling_whichever_invocation_it_travelled_in() {
    let router = build_router();
    let function = create_function(&router, "batch-size-errors-handler", BATCH_HANDLER).await;
    let role = create_role(&router, "appsync-batch-size-errors", &allowing(INVOKE_ACTION)).await;
    let api = create_api(&router, "batch-size-errors", &function, &role).await;

    /// One row: the field whose resolver carries the size, that size, and which sibling's item the
    /// handler fails.
    struct Row {
        case: &'static str,
        field: &'static str,
        size: usize,
        failing: usize,
    }
    // The rows differ along WHICH invocation the failing sibling travelled in: one of its own, the
    // first of two, and the second of two.
    let rows: [Row; 3] = [
        Row {
            case: "a failing item alone in its own invocation",
            field: "batchOne",
            size: 1,
            failing: 2,
        },
        Row {
            case: "a failing item in the first of two invocations",
            field: "batchTwo",
            size: 2,
            failing: 1,
        },
        Row {
            case: "a failing item in the second of two invocations",
            field: "batchTwo",
            size: 2,
            failing: 3,
        },
    ];

    for row in &rows {
        let case = row.case;
        let (status, answered) =
            post_published(case, &api, &document(row.field, Some(row.failing))).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} {answered}"
        );
        // Every sibling but the failing one resolves from its own element, with the length of the
        // event list its own invocation carried; the failing one resolves to nothing.
        let expected: Value = Value::Object(
            SIBLING_IDS
                .iter()
                .enumerate()
                .map(|(at, id)| {
                    let value = match at == row.failing {
                        true => Value::Null,
                        false => json!({"id": id, "batchSize": merged_with(at, row.size)}),
                    };
                    (format!("s{at}"), value)
                })
                .collect(),
        );
        assert_eq!(
            answered["data"], expected,
            "case {case:?}: expected the data {expected}, got {answered}"
        );
        let reported = reported_errors(&answered);
        let expected_errors = vec![(
            format!("s{}", row.failing),
            ITEM_ERROR_TYPE.to_string(),
            format!("item {} could not be read", SIBLING_IDS[row.failing]),
        )];
        assert_eq!(
            reported, expected_errors,
            "case {case:?}: expected exactly the errors {expected_errors:?}, got {reported:?} in \
             {answered}"
        );
    }
}
