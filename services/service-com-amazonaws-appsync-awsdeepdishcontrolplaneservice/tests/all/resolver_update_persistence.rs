// SPDX-License-Identifier: CC0-1.0

//! What an accepted `UpdateResolver` leaves behind, end to end over HTTP: the stored
//! resolver every later reader answers with.
//!
//! The rule asserted here, over a CLASS of definitions rather than over the one request
//! that exposed the defect: a resolver's stored record is exactly what its LAST ACCEPTED
//! write stated — an update replaces the members it carries and drops the ones it leaves
//! out — and every operation that reports that resolver projects that one record, so
//! `UpdateResolver`'s own answer, `GetResolver`, `ListResolvers` and
//! `ListResolversByFunction` cannot disagree about it. A read is a read: none of them
//! changes what the next caller sees.
//!
//! The rows differ along what the rule generalises over: the member a write sets (the two
//! mapping templates, `code` with its `runtime`, `syncConfig`, `cachingConfig`,
//! `maxBatchSize`, `metricsConfig`), the `ConflictHandlerType` crossed with the
//! `ConflictDetectionType` a sync configuration names, the `ResolverKind` the definition
//! is of, and whether the write carries a member at all. Every row is written twice — once
//! on a resolver of its own and once in sequence on ONE resolver — so a lost write shows
//! up either as a creation-time value surviving an update or as an earlier row's value
//! surviving a later one.
//!
//! Every assertion goes through the router the production emulator mounts, so these
//! exercise routing, restJson1 (de)serialization, the boundary validator, the handlers and
//! the shared engine together. The harness (`call`, `router`, `Answer`), the
//! API-with-dependencies seed (whose data source is configured for versioned items, which
//! is what a resolver carrying a `syncConfig` resolves against) and the resolver reader are
//! the sibling suites' — reused rather than copied.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use crate::error_contract::{call, router, Answer};
use crate::functions::{ACCOUNT, REGION, RUNTIME};
use crate::resolvers::{
    api_with_dependencies, create_resolver_body, get_resolver, DATA_SOURCE, TYPE,
};

/// The definition every resolver below is CREATED with, which no row of the table
/// restates: a member still reporting one of these values after an accepted update is a
/// lost write rather than a coincidence.
const CREATED_REQUEST_TEMPLATE: &str = "## created\n{\"version\": \"2018-05-29\"}";
const CREATED_RESPONSE_TEMPLATE: &str = "## created\n$util.toJson($ctx.result)";

/// The ARN of a Lambda function of the calling account, which is what the LAMBDA conflict
/// handler names ("the Amazon Resource Name (ARN) for the Lambda function to use as the
/// Conflict Handler", `LambdaConflictHandlerConfig$lambdaConflictHandlerArn`).
fn conflict_handler_arn() -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:resolve_conflicts")
}

/// A sync configuration naming `handler` and `detection`, supplying the Lambda conflict
/// handler where the handler is the one configured by a member of its own.
fn sync_config(handler: &str, detection: &str) -> Value {
    let mut config = json!({"conflictHandler": handler, "conflictDetection": detection});
    if handler == state::LAMBDA_CONFLICT_HANDLER {
        config["lambdaConflictHandlerConfig"] =
            json!({"lambdaConflictHandlerArn": conflict_handler_arn()});
    }
    config
}

/// The definitions an update can state: what the row exercises, and the body it submits
/// beside the field it addresses.
///
/// The value sets are the model's: `ConflictHandlerType` (OPTIMISTIC_CONCURRENCY | LAMBDA
/// | AUTOMERGE | NONE) crossed with `ConflictDetectionType` (VERSION | NONE), the two
/// `ResolverLevelMetricsConfig` values, the published `maxBatchSize` bounds (`@range`
/// 0..2000) and the cache-entry TTL window ("Valid values are 1–3,600 seconds",
/// `CachingConfig$ttl`). Source: `smithy_query(query="shape",
/// shape="SyncConfig,ConflictHandlerType,ConflictDetectionType,CachingConfig,MaxBatchSize,
/// ResolverLevelMetricsConfig")`.
fn rows() -> Vec<(String, Value)> {
    let mut rows: Vec<(String, Value)> = vec![
        (
            "a new request mapping template alone".to_string(),
            json!({
                "dataSourceName": DATA_SOURCE,
                "requestMappingTemplate": "{\"updated\": true}",
            }),
        ),
        (
            "both mapping templates replaced".to_string(),
            json!({
                "dataSourceName": DATA_SOURCE,
                "requestMappingTemplate": "{\"version\": \"2017-02-28\", \"operation\": \"GetItem\"}",
                "responseMappingTemplate": "$util.toJson($ctx.result.items)",
            }),
        ),
        (
            "APPSYNC_JS code in place of the templates".to_string(),
            json!({
                "dataSourceName": DATA_SOURCE,
                "code": "export function request(ctx) {\n  return { payload: ctx.args };\n}\n\
                         export function response(ctx) {\n  return ctx.result;\n}\n",
                "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
            }),
        ),
        (
            "other APPSYNC_JS code, replacing the first".to_string(),
            json!({
                "dataSourceName": DATA_SOURCE,
                "code": "export function request(ctx) {\n  return {};\n}\n\
                         export function response(ctx) {\n  return ctx.result;\n}\n",
                "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
            }),
        ),
        (
            "a cached resolver at the minimum TTL".to_string(),
            json!({
                "dataSourceName": DATA_SOURCE,
                "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                "cachingConfig": {
                    "ttl": state::CACHE_TTL_MIN_SECONDS,
                    "cachingKeys": ["$context.arguments.id"],
                },
                "maxBatchSize": 0,
                "metricsConfig": "ENABLED",
            }),
        ),
        (
            "a cached resolver at the maximum TTL and batch size".to_string(),
            json!({
                "dataSourceName": DATA_SOURCE,
                "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                "cachingConfig": {"ttl": state::CACHE_TTL_MAX_SECONDS},
                "maxBatchSize": 2_000,
                "metricsConfig": "DISABLED",
            }),
        ),
        (
            "a definition carrying nothing but its data source".to_string(),
            json!({"dataSourceName": DATA_SOURCE}),
        ),
    ];
    // One row per conflict-resolution strategy crossed with each detection strategy: the
    // configuration is stored whole, so a reader reports the handler AND the detection the
    // last accepted write named.
    for handler in [
        "OPTIMISTIC_CONCURRENCY",
        "AUTOMERGE",
        "NONE",
        state::LAMBDA_CONFLICT_HANDLER,
    ] {
        for detection in ["VERSION", "NONE"] {
            rows.push((
                format!("{handler} conflict resolution with {detection} detection"),
                json!({
                    "dataSourceName": DATA_SOURCE,
                    "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                    "syncConfig": sync_config(handler, detection),
                }),
            ));
        }
    }
    rows
}

/// The `CreateResolver` body every resolver below is attached with: a UNIT resolver whose
/// definition no row restates.
fn created_body(field: &str) -> Value {
    json!({
        "fieldName": field,
        "dataSourceName": DATA_SOURCE,
        "requestMappingTemplate": CREATED_REQUEST_TEMPLATE,
        "responseMappingTemplate": CREATED_RESPONSE_TEMPLATE,
        "syncConfig": sync_config(state::LAMBDA_CONFLICT_HANDLER, "VERSION"),
        "metricsConfig": "ENABLED",
        "maxBatchSize": 7,
    })
}

/// Send one `UpdateResolver` restating the resolver on `field` as `body` describes.
async fn update_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    field: &str,
    body: &Value,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers/{field}"),
        Some(body.clone()),
    )
    .await
}

/// The `ListResolvers` entry for `field`, or a panic naming the case: the listing is
/// another reader of the same stored record.
async fn listed_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    field: &str,
    case: &str,
) -> Value {
    let listing = call(
        router,
        "GET",
        &format!("/v1/apis/{api_id}/types/{TYPE}/resolvers"),
        None,
    )
    .await;
    assert_eq!(
        listing.status, 200,
        "case {case:?}: expected ListResolvers 200, got {listing:?}"
    );
    entry_for(&listing, field).unwrap_or_else(|| {
        panic!("case {case:?}: expected the listing of {TYPE} to report {field}, got {listing:?}")
    })
}

/// The `ListResolversByFunction` entry for `field`, or a panic naming the case.
async fn listed_by_function(
    router: &MultiServiceRouter,
    api_id: &str,
    function_id: &str,
    field: &str,
    case: &str,
) -> Value {
    let listing = call(
        router,
        "GET",
        &format!("/v1/apis/{api_id}/functions/{function_id}/resolvers"),
        None,
    )
    .await;
    assert_eq!(
        listing.status, 200,
        "case {case:?}: expected ListResolversByFunction 200, got {listing:?}"
    );
    entry_for(&listing, field).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the function's resolver listing to report {field}, \
             got {listing:?}"
        )
    })
}

/// The entry a resolver listing reports for `field`, where it reports one.
fn entry_for(listing: &Answer, field: &str) -> Option<Value> {
    listing.body["resolvers"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_default()
        .iter()
        .find(|resolver| resolver["fieldName"] == json!(field))
        .cloned()
}

/// The members a write STATES, as the `Resolver` the service answers reports them: the
/// ones the body carried, and `null` for every settable member it left out — an update
/// states the resolver, so a member it omits is a member the resolver no longer has.
///
/// `maxBatchSize` is modelled with a default of 0, so a request naming none asks for that
/// default rather than for no member at all.
fn expected_members(body: &Value) -> Vec<(&'static str, Value)> {
    let member = |name: &str| body.get(name).cloned().unwrap_or(Value::Null);
    vec![
        ("dataSourceName", member("dataSourceName")),
        ("requestMappingTemplate", member("requestMappingTemplate")),
        ("responseMappingTemplate", member("responseMappingTemplate")),
        ("code", member("code")),
        ("runtime", member("runtime")),
        ("syncConfig", member("syncConfig")),
        ("cachingConfig", member("cachingConfig")),
        ("pipelineConfig", member("pipelineConfig")),
        ("metricsConfig", member("metricsConfig")),
        (
            "maxBatchSize",
            body.get("maxBatchSize").cloned().unwrap_or(json!(0)),
        ),
    ]
}

/// Assert `resolver`, as `source` answered it, states exactly what `body` wrote.
fn assert_states(case: &str, source: &str, resolver: &Value, body: &Value) {
    for (name, expected) in expected_members(body) {
        assert_eq!(
            resolver[name], expected,
            "case {case:?}: expected {source} to answer {name} as {expected}, got {}",
            resolver[name]
        );
    }
    // The identity of the record survives every write: the type and field it is attached
    // to, which is also what its ARN names.
    assert_eq!(
        resolver["typeName"], json!(TYPE),
        "case {case:?}: expected {source} to answer the type it is attached to, got {resolver}"
    );
}

/// Attach the resolver an update addresses and assert the creation-time definition is what
/// a read of it answers, so each row starts from a resolver whose stored definition is
/// known and differs from the one it submits.
async fn settled_resolver(router: &MultiServiceRouter, api_id: &str, field: &str, case: &str) {
    let body = created_body(field);
    let created = create_resolver_body(router, api_id, TYPE, body.clone()).await;
    assert_eq!(
        created.status, 200,
        "case {case:?}: expected CreateResolver 200, got {created:?}"
    );
    assert_states(case, "CreateResolver", &created.body["resolver"], &body);
    let read = get_resolver(router, api_id, TYPE, field).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected the attached resolver to be served, got {read:?}"
    );
    assert_states(case, "GetResolver before the update", &read.body["resolver"], &body);
}

/// Update the resolver on `field` as `body` states, and assert every reader of that field
/// answers the record the update stored.
async fn assert_update_is_persisted(
    router: &MultiServiceRouter,
    api_id: &str,
    field: &str,
    case: &str,
    body: &Value,
) {
    let updated = update_resolver(router, api_id, field, body).await;
    assert_eq!(
        updated.status, 200,
        "case {case:?}: expected UpdateResolver 200, got {updated:?}"
    );
    assert_states(case, "UpdateResolver", &updated.body["resolver"], body);

    // The answer above is the STORED record, so an independent read answers the same
    // resolver — member for member, not only on the members the row set.
    let read = get_resolver(router, api_id, TYPE, field).await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected GetResolver 200 after the update, got {read:?}"
    );
    assert_states(case, "GetResolver", &read.body["resolver"], body);
    assert_eq!(
        read.body["resolver"], updated.body["resolver"],
        "case {case:?}: expected GetResolver to answer the record UpdateResolver stored"
    );

    // And so does the listing of the type the field belongs to.
    let listed = listed_resolver(router, api_id, field, case).await;
    assert_eq!(
        listed, read.body["resolver"],
        "case {case:?}: expected ListResolvers to report the record GetResolver answers"
    );

    // Reading it twice leaves the next caller seeing the same record: a read is a read.
    let again = get_resolver(router, api_id, TYPE, field).await;
    assert_eq!(
        again.body["resolver"], read.body["resolver"],
        "case {case:?}: expected a second GetResolver to answer the same record"
    );
}

/// Rule: each definition an update states is the one every reader answers, with each row
/// against a resolver of its own, so no row's outcome depends on another's.
#[tokio::test]
async fn an_accepted_update_is_what_every_reader_of_the_resolver_answers() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_resolver_update_persistence").await;

    for (index, (case, body)) in rows().iter().enumerate() {
        let field = format!("updated_{index}");
        settled_resolver(&router, &api_id, &field, case).await;
        let mut body = body.clone();
        body.as_object_mut()
            .expect("the update body is an object")
            .insert("fieldName".to_string(), json!(field));
        assert_update_is_persisted(&router, &api_id, &field, case, &body).await;
    }
}

/// Rule, over ONE resolver written again and again: the write that stated the resolver
/// LAST is the one every reader answers, so no update is lost behind an earlier one and
/// none leaves an earlier row's members behind.
#[tokio::test]
async fn successive_updates_each_supersede_the_stored_resolver() {
    let router = router();
    let (api_id, _) = api_with_dependencies(&router, "Api_resolver_update_sequence").await;
    let field = "restated";
    settled_resolver(&router, &api_id, field, "the resolver successive updates restate").await;

    for (case, body) in rows() {
        let case = format!("{case} restated in sequence");
        let mut body = body;
        body.as_object_mut()
            .expect("the update body is an object")
            .insert("fieldName".to_string(), json!(field));
        assert_update_is_persisted(&router, &api_id, field, &case, &body).await;
    }
}

/// Rule, over the listing scoped to a FUNCTION: a pipeline resolver's update is what the
/// listing of the function it invokes reports, member for member — the listing projects
/// the stored record, not a copy made when the resolver was attached.
#[tokio::test]
async fn the_function_scoped_listing_reports_the_updated_resolver() {
    let router = router();
    let (api_id, function_id) =
        api_with_dependencies(&router, "Api_resolver_update_by_function").await;
    let field = "piped";

    /// The pipeline definition a row states, given the function it invokes.
    fn pipeline_body(field: &str, function_id: &str, definition: Value) -> Value {
        let mut body = json!({
            "fieldName": field,
            "kind": state::PIPELINE_KIND,
            "pipelineConfig": {"functions": [function_id]},
        });
        let object = body.as_object_mut().expect("the body is an object");
        for (name, value) in definition
            .as_object()
            .expect("the definition is an object")
            .clone()
        {
            object.insert(name, value);
        }
        body
    }

    let created_definition = json!({"requestMappingTemplate": CREATED_REQUEST_TEMPLATE});
    let created_body = pipeline_body(field, &function_id, created_definition);
    let created = create_resolver_body(&router, &api_id, TYPE, created_body.clone()).await;
    assert_eq!(
        created.status, 200,
        "case the pipeline resolver the updates restate: expected CreateResolver 200, \
         got {created:?}"
    );
    let listed = listed_by_function(
        &router,
        &api_id,
        &function_id,
        field,
        "the pipeline resolver as created",
    )
    .await;
    assert_states(
        "the pipeline resolver as created",
        "ListResolversByFunction",
        &listed,
        &created_body,
    );

    // The rows differ along the definition the update states: a new template, APPSYNC_JS
    // code in place of the templates, both templates, and the template restored.
    let updates: &[(&str, Value)] = &[
        (
            "a new request mapping template",
            json!({"requestMappingTemplate": "{\"updated\": true}"}),
        ),
        (
            "APPSYNC_JS code in place of the templates",
            json!({
                "code": "export function request(ctx) {\n  return { payload: ctx.args };\n}\n",
                "runtime": {"name": RUNTIME.0, "runtimeVersion": RUNTIME.1},
            }),
        ),
        (
            "both mapping templates",
            json!({
                "requestMappingTemplate": "{\"version\": \"2018-05-29\"}",
                "responseMappingTemplate": "$util.toJson($ctx.result)",
            }),
        ),
        (
            "the creation-time request mapping template again",
            json!({"requestMappingTemplate": CREATED_REQUEST_TEMPLATE}),
        ),
    ];
    for (case, definition) in updates {
        let body = pipeline_body(field, &function_id, definition.clone());
        let updated = update_resolver(&router, &api_id, field, &body).await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: expected UpdateResolver 200, got {updated:?}"
        );
        assert_states(case, "UpdateResolver", &updated.body["resolver"], &body);

        let read = get_resolver(&router, &api_id, TYPE, field).await;
        assert_eq!(
            read.body["resolver"], updated.body["resolver"],
            "case {case:?}: expected GetResolver to answer the record UpdateResolver stored"
        );
        let listed = listed_by_function(&router, &api_id, &function_id, field, case).await;
        assert_eq!(
            listed, read.body["resolver"],
            "case {case:?}: expected ListResolversByFunction to report the stored record"
        );
    }
}
