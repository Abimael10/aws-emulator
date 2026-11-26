// SPDX-License-Identifier: CC0-1.0

//! An AppSync GraphQL API whose parent LIST field is ended by `runtime.earlyReturn` on a `NONE`
//! data source and whose child field is resolved by a BATCHED `AWS_LAMBDA` resolver — end to end
//! over the real `MultiServiceRouter`, with AppSync, Lambda, IAM and CloudFormation mounted on one
//! engine.
//!
//! RULE — an APPSYNC_JS request handler that calls `runtime.earlyReturn(value)` ENDS the step it
//! belongs to with `value` as the field's result: the data source the step names is not called and
//! the step's response handler does not run. Where the field so ended is a LIST, the child field
//! selected on every element of it is resolved once per element, and where that child's own
//! request half names the `BatchInvoke` operation of an `AWS_LAMBDA` data source those sibling
//! resolutions are merged, in the order of the list, into invocations of at most the resolver's
//! maximum batching size (`Resolver$maxBatchSize`, an integer whose range is 0 to 2000, per
//! `smithy_query(query="shape",shape="Resolver,MaxBatchSize")`); each invocation's event is the
//! bare JSON LIST of the payloads those resolutions rendered, and the element the function answers
//! at a position supplies the sibling whose payload sat at that same position. So the whole
//! request is answered by ONE HTTP 200 carrying `data` and no `errors` member at all, with as many
//! parents as the request asked for and every child resolved from its own element.
//!
//! Three services have to agree for the rule to hold — Lambda runs the function, IAM holds the
//! data source's service role, and AppSync ends the parent step, merges the child's resolutions
//! and applies the answer — so this is the suite that can pin it. The sibling suites
//! [`crate::appsync_lambda_batch_invoke_resolvers`] and
//! [`crate::appsync_batch_invoke_batching_sizes`] span the definition FORMS and the batching SIZE
//! over parents the request half rendered as a payload; what these rows add is the parent list the
//! request handler ENDED its step with, taken from the request's own arguments, and the invocation
//! IDENTITY that says how many invocations the siblings really travelled in.
//!
//! The rows span what the rule generalises over: HOW MANY parents the ended step produced (one,
//! two, three, exactly the batching size, and one group and a remainder past it), WHICH ids they
//! carry and in WHAT order (unsorted, so an answer ordered by anything but the argument is seen).

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

/// The `AWS_LAMBDA` data source the child field resolves against, and the `NONE` one the parent
/// list resolves against — "there is no data source", so nothing but the parent's own code can
/// decide the list.
const DATA_SOURCE: &str = "handler";
const LOCAL_DATA_SOURCE: &str = "local";

/// "The maximum batching size for a resolver" (`Resolver$maxBatchSize`) the child resolver
/// configures: four sibling resolutions fit in one invocation, and a fifth makes a second one.
const MAX_BATCH_SIZE: i32 = 4;

/// How long a poll loop waits for a state another service owes, and how often it looks again.
/// Generous: what is under test is what the endpoint answers, never how fast the host reaches a
/// transition.
const BUDGET: Duration = Duration::from_secs(120);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The schema every API of this suite is created with: the parent list, taken from the ids the
/// request asks for, and the child field resolved for every element of it.
const SCHEMA: &str = "type Detail {\n  forId: String\n  batchLen: Int\n  invocation: String\n}\n\
                      \n\
                      type Post {\n  id: String\n  detail: Detail\n}\n\
                      \n\
                      type Query {\n  posts(ids: [ID!]!): [Post]\n}\n";

/// The handler the child field's data source invokes.
///
/// It answers from the EVENT alone, one per-item envelope per element (`data`, plus
/// `errorMessage`/`errorType` for an item that failed): `forId` is the item it was given,
/// `batchLen` is the LENGTH of the event list, so it says how many resolutions the invocation it
/// travelled in merged, and `invocation` is the id of the invocation ITSELF, so siblings that
/// shared one invocation can be told from siblings that did not. An event that is not a list is
/// one resolution that was never merged, which it reports as an error of that item rather than
/// answering, so an unbatched invocation is visible rather than silent.
const BATCH_HANDLER: &str = r#"
def handler(event, context):
    if not isinstance(event, list):
        return {
            "data": None,
            "errorMessage": "the event was not a list of merged payloads",
            "errorType": "UnbatchedEvent",
        }
    return [
        {
            "data": {
                "forId": item.get("forId"),
                "batchLen": len(event),
                "invocation": context.aws_request_id,
            },
            "errorMessage": None,
            "errorType": None,
        }
        for item in event
    ]
"#;

/// The id the parent list would carry had the request handler NOT ended its step, and the one it
/// would carry had the response handler run over the step's result: neither may ever be answered,
/// because ending the step is what decides the field.
const UNENDED_ID: &str = "the-request-handler-did-not-end-its-step";
const RESPONDED_ID: &str = "the-response-handler-ran";

/// The parent list's own definition: its request handler ENDS the step with one object per id the
/// request asked for, and both halves go on to name a value of their own — so a run that did not
/// honour the ending would be answered with ids this suite can see rather than with the request's.
fn parents_code() -> String {
    format!(
        "import {{ runtime }} from '@aws-appsync/utils';\n\
         export function request(ctx) {{\n  \
         runtime.earlyReturn(ctx.args.ids.map((id) => ({{id: id}})));\n  \
         return {{payload: [{{id: '{UNENDED_ID}'}}]}};\n}}\n\
         export function response(ctx) {{ return [{{id: '{RESPONDED_ID}'}}]; }}\n"
    )
}

/// The child field's definition: its request half names the batched operation and renders the one
/// parent it is resolved FOR, so one invocation carries one payload per sibling parent, and its
/// response half takes the field's value from the per-item envelope its own element carried.
const CHILD_CODE: &str = "export function request(ctx) {\n  \
                          return {operation: 'BatchInvoke', payload: {forId: ctx.source.id}};\n}\n\
                          export function response(ctx) {\n  return ctx.result.data;\n}\n";

/// The document every row POSTs: the parent list over the ids the variables carry, and the child
/// field of every parent of it.
const DOCUMENT: &str = "query Batched($ids: [ID!]!) { posts(ids: $ids) \
                        { id detail { forId batchLen invocation } } }";

/// One created GraphQL API: where it is served, and a live API key.
struct Api {
    /// The URL the API publishes for its `GRAPHQL` endpoint, which is what a client POSTs to.
    graphql_uri: String,
    api_key: String,
}

/// Create one `API_KEY` API with [`SCHEMA`], the `NONE` data source the parent list resolves
/// against, the `AWS_LAMBDA` data source that invokes `function_arn` under `role_arn`, the parent
/// resolver that ends its own step and the batched child resolver; answer the URL the API
/// published for its GraphQL endpoint and a live API key.
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
        Some(json!({"name": LOCAL_DATA_SOURCE, "type": "NONE"})),
    )
    .await;
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
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": "posts",
            "kind": "UNIT",
            "dataSourceName": LOCAL_DATA_SOURCE,
            "code": parents_code(),
            "runtime": {"name": APPSYNC_JS.0, "runtimeVersion": APPSYNC_JS.1},
        })),
    )
    .await;
    let child = appsync_ok(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Post/resolvers"),
        Some(json!({
            "fieldName": "detail",
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "maxBatchSize": MAX_BATCH_SIZE,
            "code": CHILD_CODE,
            "runtime": {"name": APPSYNC_JS.0, "runtimeVersion": APPSYNC_JS.1},
        })),
    )
    .await;
    assert_eq!(
        child["resolver"]["maxBatchSize"],
        json!(MAX_BATCH_SIZE),
        "setup: the created resolver must carry the batching size it was created with, got {child}"
    );
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

/// POST [`DOCUMENT`] over `ids` to the URL the API PUBLISHED, as a client does, carrying the API's
/// own key: the host, the port and the path all come from `uris.GRAPHQL` rather than from anything
/// this suite knows.
async fn post_published(case: &str, api: &Api, ids: &[&str]) -> (u16, Value) {
    post_published_uri(
        case,
        &api.graphql_uri,
        &json!({"query": DOCUMENT, "variables": {"ids": ids}}),
        &[("x-api-key", api.api_key.clone())],
    )
    .await
}

/// How many resolutions the invocation the sibling at `at` travelled in merged, out of `total`
/// siblings merged `size` at a time: a full invocation per group of `size` in request order, and
/// whatever is left over in the last one.
fn merged_with(at: usize, total: usize, size: usize) -> i64 {
    let invocation = at / size;
    let merged = (total - invocation * size).min(size);
    i64::try_from(merged).unwrap_or_default()
}

/// A parent list the request handler ended its step with, and the child field of every element of
/// it: the request is answered by one 200 whose `data` carries a parent per id in the argument's
/// own order, each with its child resolved from the element at its own position of the ONE
/// invocation their resolutions were merged into — and with no error reported at all.
#[tokio::test]
async fn an_ended_parent_step_has_its_children_merged_into_one_invocation_per_batching_size() {
    let router = build_router();
    let function = create_function(&router, "early-return-batch-handler", BATCH_HANDLER).await;
    let role = create_role(&router, "appsync-early-return-batch", &allowing(INVOKE_ACTION)).await;
    let api = create_api(&router, "early-return-batch", &function, &role).await;

    // The rows differ along HOW MANY parents the ended step produced and WHICH ids they carry: one
    // on its own, two, three, exactly the batching size, and one full group plus a remainder past
    // it. No row's ids are in sorted order, so an answer ordered by anything but the argument
    // itself would be seen.
    let rows: [&[&str]; 5] = [
        &["solo"],
        &["second", "first"],
        &["p-3", "p-1", "p-2"],
        &["d", "a", "c", "b"],
        &["six", "one", "five", "two", "four", "three"],
    ];
    let size = usize::try_from(MAX_BATCH_SIZE).unwrap_or(1);

    for ids in rows {
        let case = format!("a parent list of {} ids, ended by its own step", ids.len());
        let case = case.as_str();
        let (status, answered) = post_published(case, &api, ids).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} {answered}"
        );
        assert!(
            answered.get("errors").is_none(),
            "case {case:?}: expected a result carrying no errors member, got {answered}"
        );
        let posts = answered["data"]["posts"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: expected a list of parents, got {answered}"));
        assert_eq!(
            posts.len(),
            ids.len(),
            "case {case:?}: expected {} parents, got {answered}",
            ids.len()
        );
        // Every parent is the id the argument carried at its own position — never a value the
        // definition would have produced had the ending not been honoured — and every child
        // carries the element answered for that same position.
        for (at, id) in ids.iter().enumerate() {
            let post = &posts[at];
            assert_eq!(
                post["id"],
                json!(id),
                "case {case:?}: expected the parent at {at} to be {id:?} (never {UNENDED_ID:?} or \
                 {RESPONDED_ID:?}), got {post}"
            );
            assert_eq!(
                post["detail"]["forId"],
                json!(id),
                "case {case:?}: expected the child of parent {at} to answer the payload its own \
                 position rendered, got {post}"
            );
            assert_eq!(
                post["detail"]["batchLen"],
                json!(merged_with(at, ids.len(), size)),
                "case {case:?}: expected the child of parent {at} to have travelled in an \
                 invocation of {} merged payloads, got {post}",
                merged_with(at, ids.len(), size)
            );
        }
        // The invocation each child's element came from: siblings of one group share one, and two
        // groups never do — so the children of a request no larger than the batching size came
        // from exactly ONE invocation.
        let invocations: Vec<String> = posts
            .iter()
            .map(|post| {
                post["detail"]["invocation"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        for (at, invocation) in invocations.iter().enumerate() {
            assert!(
                !invocation.is_empty(),
                "case {case:?}: expected the child of parent {at} to name the invocation it \
                 travelled in, got {answered}"
            );
            assert_eq!(
                invocation == &invocations[0],
                at / size == 0,
                "case {case:?}: expected the child of parent {at} to share the first child's \
                 invocation only inside the first group of {size}, got {invocations:?}"
            );
        }
        let mut distinct = invocations.clone();
        distinct.sort();
        distinct.dedup();
        let expected = ids.len().div_ceil(size);
        assert_eq!(
            distinct.len(),
            expected,
            "case {case:?}: expected {expected} invocation(s) over {} siblings merged {size} at a \
             time, got {invocations:?}",
            ids.len()
        );
    }
}
