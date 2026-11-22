// SPDX-License-Identifier: CC0-1.0

//! `CreateResolver` is served from this service's state, over the CLASS of requests the
//! operation accepts and the class it refuses.
//!
//! The rule: every operation this service's model binds is answered from the service's
//! own persisted state — a write records the resource and answers the record it stored,
//! so an independent reader answers the same body, and a request the model or AppSync's
//! documented constraints rule out takes that class's modelled error; no request is
//! answered by a not-implemented refusal, and a refused one leaves the resource it would
//! have written absent.
//!
//! This suite pins that rule on `CreateResolver` specifically:
//!
//! 1. every definition form AppSync can run is a 200 whose `resolver` is the stored
//!    record — `GetResolver` answers that same body member for member, and the
//!    `ListResolvers` page of the type reports the field exactly once;
//! 2. a definition AppSync could not run, and an `apiId` that names no API, take the
//!    modelled error of their class (400 `BadRequestException`, 404 `NotFoundException`)
//!    and leave the field without a resolver;
//! 3. in neither case is the answer a 501 or an error typed `NotImplemented`, the way a
//!    scaffolded operation answers every request.
//!
//! The rows span the definition surface rather than one recorded request: a request that
//! names no `kind` and both `ResolverKind` values, a resolver defined by VTL mapping
//! templates and one defined by `APPSYNC_JS` `code`, both published `maxBatchSize`
//! bounds, both `ResolverLevelMetricsConfig` values, a cache at each published TTL bound
//! and one past the maximum, and a `syncConfig`.
//!
//! Every assertion goes through the router the production emulator mounts, so these
//! exercise routing, restJson1 (de)serialization, the boundary validator, the handler and
//! the shared engine together. The harness (`call`, `router`, `Answer`), the API-with-
//! dependencies seed and the resolver read are the sibling suites' — reused, not copied.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::error_contract::{call, router, Answer};
use super::functions::{ACCOUNT, REGION, RUNTIME};
use super::resolvers::{
    api_with_dependencies, assert_no_such_type, create_resolver_body, get_resolver, DATA_SOURCE,
    TYPE,
};

/// The error type a scaffolded operation answers every request with. No request below
/// may take it: the operation is implemented, so every answer is a modelled one.
const NOT_IMPLEMENTED: &str = "NotImplemented";

/// The published bounds of `maxBatchSize` (`MaxBatchSize`, `@range` 0..2000). Source:
/// `smithy_query(query="shape", shape="MaxBatchSize")`.
const MAX_BATCH_SIZES: &[i64] = &[0, 2_000];

/// `ResolverLevelMetricsConfig`'s whole value set. Source:
/// `smithy_query(query="shape", shape="ResolverLevelMetricsConfig")`.
const METRICS_CONFIGS: &[&str] = &["ENABLED", "DISABLED"];

/// What the service must answer a row with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    /// A definition AppSync can run: the create is answered from the record it stored.
    Served,
    /// A request AppSync rules out, answered by the modelled error of that class, with
    /// no resolver left on the field.
    Refused {
        status: u16,
        error_type: &'static str,
    },
}

/// One row of the table: a `CreateResolver` body, minus the field it attaches to, and
/// the answer the rule requires for it.
struct Row {
    /// What the row exercises, named in every assertion of it.
    label: &'static str,
    /// The body for this row, given the id of the seeded function a pipeline invokes.
    body: fn(&str) -> Value,
    outcome: Outcome,
}

/// AppSync's 400 for a request it rules out.
const BAD_REQUEST: Outcome = Outcome::Refused {
    status: 400,
    error_type: "BadRequestException",
};

/// The table: the definition forms a resolver can take, and the forms AppSync refuses.
///
/// Source for the value sets and the pairing rules: `smithy_query(query="shape",
/// shape="Resolver,ResolverKind,PipelineConfig,CachingConfig,SyncConfig,
/// ResolverLevelMetricsConfig,MaxBatchSize,AppSyncRuntime")`.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "a UNIT resolver whose request names no kind",
            body: |_| {
                json!({
                    "dataSourceName": DATA_SOURCE,
                    "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                })
            },
            outcome: Outcome::Served,
        },
        Row {
            label: "a UNIT resolver at the published maximum batch size",
            body: |_| {
                json!({
                    "kind": state::UNIT_KIND,
                    "dataSourceName": DATA_SOURCE,
                    "requestMappingTemplate": "{\"version\": \"2018-05-29\", \"payload\": {}}",
                    "responseMappingTemplate": "$util.toJson($ctx.result)",
                    "maxBatchSize": MAX_BATCH_SIZES[1],
                    "metricsConfig": METRICS_CONFIGS[0],
                })
            },
            outcome: Outcome::Served,
        },
        Row {
            label: "a UNIT resolver defined by APPSYNC_JS code",
            body: |_| {
                json!({
                    "kind": state::UNIT_KIND,
                    "dataSourceName": DATA_SOURCE,
                    "code": "export function request(ctx) { return {}; }",
                    "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
                    "maxBatchSize": MAX_BATCH_SIZES[0],
                    "metricsConfig": METRICS_CONFIGS[1],
                })
            },
            outcome: Outcome::Served,
        },
        Row {
            label: "a PIPELINE resolver over one function",
            body: |function_id| {
                json!({
                    "kind": state::PIPELINE_KIND,
                    "pipelineConfig": {"functions": [function_id]},
                    "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                    "responseMappingTemplate": "$util.toJson($ctx.result)",
                })
            },
            outcome: Outcome::Served,
        },
        Row {
            label: "a versioned UNIT resolver cached at the minimum TTL",
            body: |_| {
                json!({
                    "dataSourceName": DATA_SOURCE,
                    "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                    "cachingConfig": {
                        "ttl": state::CACHE_TTL_MIN_SECONDS,
                        "cachingKeys": ["$context.arguments.id"],
                    },
                    "syncConfig": {
                        "conflictDetection": "VERSION",
                        "conflictHandler": "OPTIMISTIC_CONCURRENCY",
                    },
                })
            },
            outcome: Outcome::Served,
        },
        Row {
            label: "a UNIT resolver cached at the maximum TTL",
            body: |_| {
                json!({
                    "dataSourceName": DATA_SOURCE,
                    "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                    "cachingConfig": {"ttl": state::CACHE_TTL_MAX_SECONDS},
                })
            },
            outcome: Outcome::Served,
        },
        Row {
            label: "a UNIT resolver carrying a pipeline",
            body: |function_id| {
                json!({
                    "kind": state::UNIT_KIND,
                    "dataSourceName": DATA_SOURCE,
                    "pipelineConfig": {"functions": [function_id]},
                    "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                })
            },
            outcome: BAD_REQUEST,
        },
        Row {
            label: "a PIPELINE resolver carrying no pipeline",
            body: |_| {
                json!({
                    "kind": state::PIPELINE_KIND,
                    "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                })
            },
            outcome: BAD_REQUEST,
        },
        Row {
            label: "code without the runtime it needs",
            body: |_| {
                json!({
                    "dataSourceName": DATA_SOURCE,
                    "code": "export function request(ctx) { return {}; }",
                })
            },
            outcome: BAD_REQUEST,
        },
        Row {
            label: "a cache one second past the published maximum TTL",
            body: |_| {
                json!({
                    "dataSourceName": DATA_SOURCE,
                    "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                    "cachingConfig": {"ttl": state::CACHE_TTL_MAX_SECONDS + 1},
                })
            },
            outcome: BAD_REQUEST,
        },
    ]
}

/// `row`'s body, attaching the resolver to `field`.
fn body_on(row: &Row, field: &str, function_id: &str) -> Value {
    let mut body = (row.body)(function_id);
    body.as_object_mut()
        .expect("the row's body is an object")
        .insert("fieldName".to_string(), json!(field));
    body
}

/// Assert `answer` is not the refusal a scaffolded operation answers with.
fn assert_not_scaffolded(case: &str, answer: &Answer) {
    assert_ne!(
        answer.status, 501,
        "case {case}: expected an implemented answer, got the not-implemented status \
         {answer:?}"
    );
    assert_ne!(
        answer.error_type, NOT_IMPLEMENTED,
        "case {case}: expected a modelled answer, got the not-implemented error \
         {answer:?}"
    );
}

/// The field names the `ListResolvers` page of `type_name` reports, in page order.
async fn listed_fields(
    router: &MultiServiceRouter,
    api_id: &str,
    type_name: &str,
) -> Vec<String> {
    let answer = call(
        router,
        "GET",
        &format!("/v1/apis/{api_id}/types/{type_name}/resolvers"),
        None,
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "case the listing of {type_name}: expected ListResolvers 200, got {answer:?}"
    );
    answer.body["resolvers"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case the listing of {type_name}: expected a resolvers list, got {answer:?}")
        })
        .iter()
        .map(|resolver| {
            resolver["fieldName"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!(
                        "case the listing of {type_name}: expected every entry to carry a \
                         fieldName, got {answer:?}"
                    )
                })
                .to_string()
        })
        .collect()
}

/// Every definition form the table names is written and answered from the stored record,
/// and every form AppSync rules out takes its modelled error and attaches nothing —
/// neither by a not-implemented refusal.
#[tokio::test]
async fn every_create_resolver_request_is_answered_from_state() {
    let router = router();
    let (api_id, function_id) =
        api_with_dependencies(&router, "Api_create_resolver_from_state").await;

    let table = rows();
    let mut served: Vec<String> = Vec::new();
    for (index, row) in table.iter().enumerate() {
        let field = format!("served_{index}");
        let case = format!("{} on {field}", row.label);
        let created =
            create_resolver_body(&router, &api_id, TYPE, body_on(row, &field, &function_id)).await;
        assert_not_scaffolded(&case, &created);

        match row.outcome {
            Outcome::Served => {
                assert_eq!(
                    created.status, 200,
                    "case {case}: expected CreateResolver 200, got {created:?}"
                );
                let resolver = created.body["resolver"].clone();
                assert_eq!(
                    resolver["typeName"], json!(TYPE),
                    "case {case}: expected the type it is attached to, got {resolver:?}"
                );
                assert_eq!(
                    resolver["fieldName"], json!(field),
                    "case {case}: expected the field it is attached to, got {resolver:?}"
                );
                // The record is named under the API's own type/field path, in the region
                // the caller addressed.
                let expected_arn = format!(
                    "arn:aws:{}:{REGION}:{ACCOUNT}:{}",
                    state::ARN_SERVICE,
                    state::resolver_resource(&api_id, TYPE, &field)
                );
                assert_eq!(
                    resolver["resolverArn"], json!(expected_arn),
                    "case {case}: expected the ARN {expected_arn}, got {resolver:?}"
                );

                // An independent reader answers the record the create stored, so the
                // answer came from state rather than from the request alone.
                let read = get_resolver(&router, &api_id, TYPE, &field).await;
                assert_not_scaffolded(&format!("the read of {case}"), &read);
                assert_eq!(
                    read.status, 200,
                    "case {case}: expected GetResolver 200, got {read:?}"
                );
                assert_eq!(
                    read.body["resolver"], resolver,
                    "case {case}: expected the read to answer the stored record {resolver:?}, \
                     got {:?}",
                    read.body["resolver"]
                );
                served.push(field);
            }
            Outcome::Refused { status, error_type } => {
                assert_eq!(
                    (created.status, created.error_type.as_str()),
                    (status, error_type),
                    "case {case}: expected {status} {error_type}, got {created:?}"
                );
                // A refused create writes nothing, so the field still carries no
                // resolver.
                let read = get_resolver(&router, &api_id, TYPE, &field).await;
                assert_eq!(
                    (read.status, read.error_type.as_str()),
                    (404, "NotFoundException"),
                    "case {case}: expected the field to carry no resolver, got {read:?}"
                );
            }
        }
    }

    // The listing of the type reports exactly the resolvers the served rows attached,
    // each once: the create wrote to the collection every reader of the API pages over.
    let mut listed = listed_fields(&router, &api_id, TYPE).await;
    listed.sort();
    let mut expected = served.clone();
    expected.sort();
    assert_eq!(
        listed, expected,
        "case the listing of {TYPE}: expected exactly the attached fields {expected:?}, got \
         {listed:?}"
    );
}

/// An `apiId` no API of this account can be named by takes the modelled error of its
/// class — not a not-implemented refusal — and attaches nothing.
#[tokio::test]
async fn an_api_id_that_names_no_api_takes_its_modelled_error() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_create_resolver_api_id").await;

    // An id longer than the characters an AppSync API id has names no API at all, so the
    // request is malformed; one of a length an id can have simply names nothing.
    let long_id = "x".repeat(state::API_ID_LEN + 1);
    let unknown_id = "z".repeat(state::API_ID_LEN);
    let rows: &[(&str, &str, u16, &str, String)] = &[
        (
            "an apiId longer than an API id",
            &long_id,
            400,
            "BadRequestException",
            state::invalid_api_id_message(&long_id),
        ),
        (
            "an apiId of an id's length naming no API",
            &unknown_id,
            404,
            "NotFoundException",
            state::API_NOT_FOUND_MESSAGE.to_string(),
        ),
    ];

    for (case, candidate, status, error_type, message) in rows {
        let body = json!({
            "fieldName": "unreachable_field",
            "dataSourceName": DATA_SOURCE,
            "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
        });
        let answer = create_resolver_body(&router, candidate, TYPE, body).await;
        assert_not_scaffolded(case, &answer);
        assert_eq!(
            (answer.status, answer.error_type.as_str(), answer.message.as_str()),
            (*status, *error_type, message.as_str()),
            "case {case}: expected {status} {error_type} {message:?}, got {answer:?}"
        );
    }

    // Nothing was written under the API that does exist either: no resolver is attached to
    // its type, which the listing of a type the API holds nothing under refuses as a
    // missing type (`ListResolvers`' `typeName` names a type OF the API).
    assert_no_such_type(&router, &api_id, TYPE).await;
}
