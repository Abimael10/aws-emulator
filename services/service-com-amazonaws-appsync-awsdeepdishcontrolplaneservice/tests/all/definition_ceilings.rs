// SPDX-License-Identifier: CC0-1.0

//! The two ceilings a resolver or a function definition is held to, end to end over HTTP.
//!
//! Neither bound is one the request boundary can state for the service, and both are
//! carried by `BadRequestException` (400), which every operation that writes a resolver or
//! a function models:
//!
//! 1. a `PIPELINE` resolver "invoke[s] a series of `Function` objects in a serial manner"
//!    (`Resolver$kind`) and `PipelineConfig$functions` targets a list with no modelled
//!    bound (`smithy_query(query="shape", shape="PipelineConfig")`), so the ceiling is the
//!    service's: a pipeline of at most 10 functions is stored and read back, one function
//!    more is refused naming the ceiling, a refused write leaves the stored pipeline
//!    exactly as it was, and a same-size write still succeeds. Asserted on both operations
//!    that write a resolver, over pipeline sizes spanning the class;
//! 2. `code` targets `Code`, `@length` 1..32768 (`smithy_query(query="shape",
//!    shape="Code")`), and a definition past that ceiling is refused with AppSync's own
//!    sentence naming the limit — not with the framework's constraint report, which would
//!    echo the whole submitted module back. Asserted on every operation that carries the
//!    member, at the ceiling and past it, and the message is checked to carry none of the
//!    submitted code.
//!
//! Every assertion goes through the router the production emulator mounts. The harness
//! (`call`, `router`, `Answer`, `create_api`) is the error-contract suite's.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, create_api, router, Answer};
use crate::functions::{FUNCTION_VERSION, RUNTIME};

/// How many functions one `PIPELINE` resolver is composed of at most, which the service's
/// own refusal names.
const FUNCTIONS_PER_PIPELINE: usize = 10;

/// `Code`'s modelled maximum length, which AppSync's ceiling refusal names.
const CODE_MAX_LEN: usize = 32_768;

/// The data source every function and resolver below resolves against: `NONE` needs no
/// configuration of its own.
const DATA_SOURCE: &str = "steps";

/// A module of exactly `length` bytes that the APPSYNC_JS runtime can read: the
/// request/response pair a definition is made of, padded out with one trailing comment.
fn code_of(length: usize) -> String {
    let head = "export function request(ctx) { return {}; }\n\
                export function response(ctx) { return ctx.result; }\n// ";
    let mut code = String::from(head);
    code.push_str(&"a".repeat(length.saturating_sub(head.len())));
    code
}

/// AppSync's sentence for a definition past the `code` ceiling.
fn code_ceiling_message() -> String {
    format!("Code must be {CODE_MAX_LEN} bytes or less")
}

/// Create one GraphQL API with the data source every case resolves against, and return
/// its `apiId`.
async fn api_with_data_source(router: &MultiServiceRouter, name: &str) -> String {
    let api_id = create_api(router, name).await;
    let created = call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/datasources"),
        Some(json!({"name": DATA_SOURCE, "type": "NONE"})),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case create the data source for {name:?}: expected 200, got {created:?}"
    );
    api_id
}

/// Create `count` VTL functions under `api_id` and return the ids the service minted,
/// which a pipeline names.
async fn mint_functions(router: &MultiServiceRouter, api_id: &str, count: usize) -> Vec<String> {
    let mut ids = Vec::with_capacity(count);
    for index in 0..count {
        let created = call(
            router,
            "POST",
            &format!("/v1/apis/{api_id}/functions"),
            Some(json!({
                "name": format!("step_{index}"),
                "dataSourceName": DATA_SOURCE,
                "functionVersion": FUNCTION_VERSION,
                "requestMappingTemplate": "{}",
                "responseMappingTemplate": "$util.toJson($ctx.result)",
                "maxBatchSize": 0,
            })),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case mint function {index}: expected 200, got {created:?}"
        );
        ids.push(
            created.body["functionConfiguration"]["functionId"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        );
    }
    ids
}

/// A `CreateResolver`/`UpdateResolver` body for a `PIPELINE` resolver over `functions`.
fn pipeline_body(field: &str, functions: &[String]) -> Value {
    json!({
        "fieldName": field,
        "kind": "PIPELINE",
        "pipelineConfig": {"functions": functions},
        "maxBatchSize": 0,
    })
}

/// The functions a resolver reply reports in its pipeline.
fn reported_functions(answer: &Answer) -> Vec<String> {
    answer.body["resolver"]["pipelineConfig"]["functions"]
        .as_array()
        .map(|functions| {
            functions
                .iter()
                .map(|function| function.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Rule 1: the pipeline ceiling bounds both writes, and a refused write changes nothing.
#[tokio::test]
async fn a_pipeline_resolver_holds_at_most_the_published_number_of_functions() {
    let router = router();
    let api_id = api_with_data_source(&router, "pipeline-function-ceiling").await;
    let ids = mint_functions(&router, &api_id, FUNCTIONS_PER_PIPELINE + 4).await;
    let expected = format!("A resolver can only be composed of {FUNCTIONS_PER_PIPELINE} functions.");

    // Pipelines inside the ceiling are created and read back, each on a field of its own.
    for size in [1_usize, 2, FUNCTIONS_PER_PIPELINE - 1, FUNCTIONS_PER_PIPELINE] {
        let field = format!("widget_{size}");
        let created = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers"),
            Some(pipeline_body(&field, &ids[..size])),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case a pipeline of {size} functions: expected CreateResolver 200, got {created:?}"
        );
        assert_eq!(
            reported_functions(&created),
            ids[..size].to_vec(),
            "case a pipeline of {size} functions: expected the submitted pipeline back"
        );
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/types/Query/resolvers/{field}"),
            None,
        )
        .await;
        assert_eq!(
            reported_functions(&read),
            ids[..size].to_vec(),
            "case a pipeline of {size} functions: expected the stored pipeline read back"
        );
    }

    // Past the ceiling, the create is refused and attaches no resolver.
    for size in [FUNCTIONS_PER_PIPELINE + 1, FUNCTIONS_PER_PIPELINE + 4] {
        let field = format!("over_{size}");
        let created = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers"),
            Some(pipeline_body(&field, &ids[..size])),
        )
        .await;
        assert_eq!(
            (
                created.status,
                created.error_type.as_str(),
                created.message.as_str()
            ),
            (400, "BadRequestException", expected.as_str()),
            "case a created pipeline of {size} functions: expected the ceiling refusal, got \
             {created:?}"
        );
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/types/Query/resolvers/{field}"),
            None,
        )
        .await;
        assert_eq!(
            read.status, 404,
            "case a created pipeline of {size} functions: expected no resolver to be attached, \
             got {read:?}"
        );
    }

    // And the update: the resolver at the ceiling cannot grow past it, and keeps the
    // pipeline it had.
    let at_ceiling: Vec<String> = ids[..FUNCTIONS_PER_PIPELINE].to_vec();
    let field = format!("widget_{FUNCTIONS_PER_PIPELINE}");
    for size in [FUNCTIONS_PER_PIPELINE + 1, FUNCTIONS_PER_PIPELINE + 4] {
        let updated = call(
            &router,
            "POST",
            &format!("/v1/apis/{api_id}/types/Query/resolvers/{field}"),
            Some(pipeline_body(&field, &ids[..size])),
        )
        .await;
        assert_eq!(
            (
                updated.status,
                updated.error_type.as_str(),
                updated.message.as_str()
            ),
            (400, "BadRequestException", expected.as_str()),
            "case an update to {size} functions: expected the ceiling refusal, got {updated:?}"
        );
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/types/Query/resolvers/{field}"),
            None,
        )
        .await;
        assert_eq!(
            reported_functions(&read),
            at_ceiling,
            "case an update to {size} functions: expected the stored pipeline to survive"
        );
    }

    // A same-size update — the ceiling's own functions, reordered — still succeeds.
    let mut reordered = at_ceiling.clone();
    reordered.reverse();
    let updated = call(
        &router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers/{field}"),
        Some(pipeline_body(&field, &reordered)),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case a reordering update at the ceiling: expected 200, got {updated:?}"
    );
    assert_eq!(
        reported_functions(&updated),
        reordered,
        "case a reordering update at the ceiling: expected the new order stored"
    );
}

/// Rule 2: a `code` at the modelled ceiling is stored, and one past it is refused with
/// AppSync's own sentence naming the limit, on every operation that carries the member.
#[tokio::test]
async fn a_definition_past_the_code_ceiling_is_refused_with_the_services_own_sentence() {
    let router = router();
    let api_id = api_with_data_source(&router, "code-size-ceiling").await;
    let runtime = json!({"name": RUNTIME.0, "runtimeVersion": RUNTIME.1});
    let expected = code_ceiling_message();

    // A definition AT the ceiling is accepted, so the refusal below is about the size and
    // not about the definition.
    let at_ceiling = code_of(CODE_MAX_LEN);
    assert_eq!(
        at_ceiling.len(),
        CODE_MAX_LEN,
        "case the ceiling itself: expected a {CODE_MAX_LEN}-byte module, got {}",
        at_ceiling.len()
    );
    let created = call(
        &router,
        "POST",
        &format!("/v1/apis/{api_id}/functions"),
        Some(json!({
            "name": "at_the_ceiling",
            "dataSourceName": DATA_SOURCE,
            "runtime": runtime,
            "code": at_ceiling,
            "maxBatchSize": 0,
        })),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "case the ceiling itself: expected CreateFunction 200, got {}",
        created.status
    );
    let function_id = created.body["functionConfiguration"]["functionId"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // A resolver at the ceiling too, which the update rows below address.
    let resolver = call(
        &router,
        "POST",
        &format!("/v1/apis/{api_id}/types/Query/resolvers"),
        Some(json!({
            "fieldName": "coded",
            "kind": "UNIT",
            "dataSourceName": DATA_SOURCE,
            "runtime": runtime,
            "code": at_ceiling,
            "maxBatchSize": 0,
        })),
    )
    .await;
    assert_eq!(
        resolver.status, 200,
        "case the ceiling itself (CreateResolver): expected 200, got {}",
        resolver.status
    );

    // Past the ceiling, on every operation that carries `code`: one byte over, and well
    // over.
    for length in [CODE_MAX_LEN + 1, CODE_MAX_LEN + 1_024] {
        let code = code_of(length);
        let rows: Vec<(&str, String, Value)> = vec![
            (
                "CreateFunction",
                format!("/v1/apis/{api_id}/functions"),
                json!({
                    "name": "over_the_ceiling",
                    "dataSourceName": DATA_SOURCE,
                    "runtime": runtime,
                    "code": code,
                    "maxBatchSize": 0,
                }),
            ),
            (
                "UpdateFunction",
                format!("/v1/apis/{api_id}/functions/{function_id}"),
                json!({
                    "name": "at_the_ceiling",
                    "dataSourceName": DATA_SOURCE,
                    "runtime": runtime,
                    "code": code,
                    "maxBatchSize": 0,
                }),
            ),
            (
                "CreateResolver",
                format!("/v1/apis/{api_id}/types/Query/resolvers"),
                json!({
                    "fieldName": "over_the_ceiling",
                    "kind": "UNIT",
                    "dataSourceName": DATA_SOURCE,
                    "runtime": runtime,
                    "code": code,
                    "maxBatchSize": 0,
                }),
            ),
            (
                "UpdateResolver",
                format!("/v1/apis/{api_id}/types/Query/resolvers/coded"),
                json!({
                    "kind": "UNIT",
                    "dataSourceName": DATA_SOURCE,
                    "runtime": runtime,
                    "code": code,
                    "maxBatchSize": 0,
                }),
            ),
        ];

        for (operation, path, body) in rows {
            let answer = call(&router, "POST", &path, Some(body)).await;
            assert_eq!(
                (
                    answer.status,
                    answer.error_type.as_str(),
                    answer.message.as_str()
                ),
                (400, "BadRequestException", expected.as_str()),
                "case {operation} with a {length}-byte definition: expected the ceiling \
                 sentence, got {} {} {:?}",
                answer.status,
                answer.error_type,
                answer.message
            );
            // The sentence carries the limit and nothing of the submitted module.
            assert!(
                !answer.message.contains("aaaa"),
                "case {operation} with a {length}-byte definition: expected no submitted code in \
                 the message, got a message of {} characters",
                answer.message.len()
            );
        }

        // Neither refused create left a resource behind, and neither refused update
        // changed the stored definition.
        let listed = call(&router, "GET", &format!("/v1/apis/{api_id}/functions"), None).await;
        let names: Vec<&str> = listed.body["functions"]
            .as_array()
            .map(|functions| {
                functions
                    .iter()
                    .filter_map(|function| function["name"].as_str())
                    .collect()
            })
            .unwrap_or_default();
        assert_eq!(
            names,
            vec!["at_the_ceiling"],
            "case a {length}-byte definition: expected only the accepted function to exist"
        );
        let read = call(
            &router,
            "GET",
            &format!("/v1/apis/{api_id}/functions/{function_id}"),
            None,
        )
        .await;
        assert_eq!(
            read.body["functionConfiguration"]["code"]
                .as_str()
                .map(str::len),
            Some(CODE_MAX_LEN),
            "case a {length}-byte definition: expected the stored definition to survive"
        );
    }
}
