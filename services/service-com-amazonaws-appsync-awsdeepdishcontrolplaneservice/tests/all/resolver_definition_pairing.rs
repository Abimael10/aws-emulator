// SPDX-License-Identifier: CC0-1.0

//! The cross-member rule a resolver's DEFINITION is judged by, on BOTH calls that write
//! one, with every refusal paired against the repair of the same request.
//!
//! The rule is stated over the members that carry a definition rather than over one
//! request's values:
//!
//! * a resolver of kind `PIPELINE` is defined by the functions its `pipelineConfig`
//!   lists ("The pipeline configuration for a resolver of kind `PIPELINE`"), so it must
//!   carry one, whatever that pipeline holds;
//! * a resolver of kind `UNIT` — the kind a request that names none asks for, "A UNIT
//!   resolver is the default resolver type" (`Resolver$kind`) — has no pipeline, so it
//!   must carry none, and it "run[s] a GraphQL query against a single data source", so it
//!   must name that data source.
//!
//! A definition that breaks the rule is refused 400 `BadRequestException` carrying
//! AppSync's own sentence for the member that is wrong, and the refusal WRITES NOTHING:
//! the field a create named carries no resolver afterwards (404 `NotFoundException` with
//! AppSync's fixed no-resolver text), and the resolver an update addressed is left
//! exactly as it was.
//!
//! Each row below carries the broken definition AND the repair of that same definition,
//! so a handler that merely refused everything would fail it: the repaired request is
//! served 200 and reports the member that repaired it, read back through `GetResolver`.
//! The rows differ along what the rule generalises over — which kind the request names
//! (`UNIT`, `PIPELINE`, and a request that names none), which member of the pair is
//! missing or forbidden, how the repair is made (dropping the forbidden member, naming
//! the kind the member belongs to, supplying the missing one) and the size of the
//! pipeline supplied (none, empty, one function).
//!
//! `CreateResolver` and `UpdateResolver` are the two calls that write a resolver's
//! definition, so every row is asserted against both. The harness (`router`, `call`, the
//! API with its data source and function, and the create/read helpers) is the resolver
//! suite's own, reused rather than copied, so every assertion goes through the router the
//! production emulator mounts.

use serde_json::{json, Value};
use service_com_amazonaws_appsync_awsdeepdishcontrolplaneservice::state;
use smithy_server_runtime::MultiServiceRouter;

use super::error_contract::{call, router, Answer};
use super::resolvers::{
    api_with_dependencies, create_resolver_body, get_resolver, DATA_SOURCE, TYPE,
};

/// AppSync's sentence for a definition carrying a pipeline the kind it names has none of.
///
/// Written out here rather than read off the service's own constants, so a change of
/// wording is caught rather than followed. The same holds for the three below.
const PIPELINE_NOT_ALLOWED: &str = "PipelineConfig should not be specified.";

/// AppSync's sentence for a `PIPELINE` resolver that carries no pipeline.
const PIPELINE_REQUIRED: &str = "PipelineConfig can't be null.";

/// AppSync's sentence for a `UNIT` resolver that names no data source.
const DATA_SOURCE_MISSING: &str = "Data source name not specified.";

/// AppSync's fixed text for a field that carries no resolver.
const NO_RESOLVER: &str = "No resolver found.";

/// The mapping templates every row carries, so no row's refusal can come from a template
/// it left out.
const REQUEST_TEMPLATE: &str = "{\"version\": \"2018-05-29\"}";

/// See [`REQUEST_TEMPLATE`].
const RESPONSE_TEMPLATE: &str = "$util.toJson($ctx.result)";

/// One row of the pairing table: a definition AppSync could not run, the sentence it is
/// refused with, and the SAME definition repaired, with what the repaired resolver
/// reports.
#[derive(Debug)]
struct Pairing {
    /// What the row exercises, named in every assertion of it.
    label: &'static str,
    /// The definition members of the broken request, minus the field it attaches to.
    broken: Value,
    /// The refusal text the broken definition is owed.
    message: &'static str,
    /// The definition members of the repaired request, minus the field.
    repaired: Value,
    /// The `kind` the repaired definition is stored and reported as.
    kind: &'static str,
    /// The `dataSourceName` the repaired resolver reports, or `None` where it names none.
    data_source: Option<&'static str>,
    /// The `pipelineConfig.functions` the repaired resolver reports, or `None` where it
    /// carries no pipeline at all.
    functions: Option<Vec<String>>,
}

/// The pairing table, over the definition surface the rule reads: `kind`,
/// `pipelineConfig` and `dataSourceName`.
///
/// Source for the member semantics: `smithy_query(query="shape",
/// shape="Resolver,ResolverKind,PipelineConfig")` and `smithy_query(query="operation",
/// operation="CreateResolver,UpdateResolver")`.
fn pairings(function_id: &str) -> Vec<Pairing> {
    vec![
        Pairing {
            label: "a UNIT resolver carrying a pipeline over no function",
            broken: json!({
                "kind": state::UNIT_KIND,
                "dataSourceName": DATA_SOURCE,
                "pipelineConfig": {"functions": []},
                "requestMappingTemplate": REQUEST_TEMPLATE,
                "responseMappingTemplate": RESPONSE_TEMPLATE,
            }),
            message: PIPELINE_NOT_ALLOWED,
            // Repaired by dropping the member the kind has no use for.
            repaired: json!({
                "kind": state::UNIT_KIND,
                "dataSourceName": DATA_SOURCE,
                "requestMappingTemplate": REQUEST_TEMPLATE,
                "responseMappingTemplate": RESPONSE_TEMPLATE,
            }),
            kind: state::UNIT_KIND,
            data_source: Some(DATA_SOURCE),
            functions: None,
        },
        Pairing {
            label: "a resolver naming no kind that carries a pipeline over one function",
            broken: json!({
                "dataSourceName": DATA_SOURCE,
                "pipelineConfig": {"functions": [function_id]},
                "requestMappingTemplate": REQUEST_TEMPLATE,
            }),
            message: PIPELINE_NOT_ALLOWED,
            // Repaired the other way round: by naming the kind the pipeline belongs to,
            // which has no data source of its own.
            repaired: json!({
                "kind": state::PIPELINE_KIND,
                "pipelineConfig": {"functions": [function_id]},
                "requestMappingTemplate": REQUEST_TEMPLATE,
            }),
            kind: state::PIPELINE_KIND,
            data_source: None,
            functions: Some(vec![function_id.to_string()]),
        },
        Pairing {
            label: "a PIPELINE resolver carrying no pipeline",
            broken: json!({
                "kind": state::PIPELINE_KIND,
                "requestMappingTemplate": REQUEST_TEMPLATE,
                "responseMappingTemplate": RESPONSE_TEMPLATE,
            }),
            message: PIPELINE_REQUIRED,
            repaired: json!({
                "kind": state::PIPELINE_KIND,
                "pipelineConfig": {"functions": [function_id]},
                "requestMappingTemplate": REQUEST_TEMPLATE,
                "responseMappingTemplate": RESPONSE_TEMPLATE,
            }),
            kind: state::PIPELINE_KIND,
            data_source: None,
            functions: Some(vec![function_id.to_string()]),
        },
        Pairing {
            label: "a PIPELINE resolver repaired with a pipeline over no function",
            broken: json!({
                "kind": state::PIPELINE_KIND,
                "responseMappingTemplate": RESPONSE_TEMPLATE,
            }),
            message: PIPELINE_REQUIRED,
            // The member has to be there; what it lists is the caller's business, so an
            // empty pipeline repairs the definition as a populated one does.
            repaired: json!({
                "kind": state::PIPELINE_KIND,
                "pipelineConfig": {"functions": []},
                "responseMappingTemplate": RESPONSE_TEMPLATE,
            }),
            kind: state::PIPELINE_KIND,
            data_source: None,
            functions: Some(Vec::new()),
        },
        Pairing {
            label: "a UNIT resolver naming no data source",
            broken: json!({
                "kind": state::UNIT_KIND,
                "requestMappingTemplate": REQUEST_TEMPLATE,
                "responseMappingTemplate": RESPONSE_TEMPLATE,
            }),
            message: DATA_SOURCE_MISSING,
            repaired: json!({
                "kind": state::UNIT_KIND,
                "dataSourceName": DATA_SOURCE,
                "requestMappingTemplate": REQUEST_TEMPLATE,
                "responseMappingTemplate": RESPONSE_TEMPLATE,
            }),
            kind: state::UNIT_KIND,
            data_source: Some(DATA_SOURCE),
            functions: None,
        },
        Pairing {
            label: "a resolver naming neither a kind nor a data source",
            broken: json!({"requestMappingTemplate": REQUEST_TEMPLATE}),
            message: DATA_SOURCE_MISSING,
            // A request that names no kind asks for a UNIT resolver, so the repair is the
            // data source that kind resolves against.
            repaired: json!({
                "dataSourceName": DATA_SOURCE,
                "requestMappingTemplate": REQUEST_TEMPLATE,
            }),
            kind: state::UNIT_KIND,
            data_source: Some(DATA_SOURCE),
            functions: None,
        },
    ]
}

/// One row's definition members as a request body attaching it to `field`.
fn on_field(members: &Value, field: &str) -> Value {
    let mut body = members.clone();
    body.as_object_mut()
        .expect("every row's definition members are a JSON object")
        .insert("fieldName".to_string(), json!(field));
    body
}

/// Send one `UpdateResolver` restating the resolver on `type_name`/`field` as `body`.
async fn update_resolver(
    router: &MultiServiceRouter,
    api_id: &str,
    type_name: &str,
    field: &str,
    body: Value,
) -> Answer {
    call(
        router,
        "POST",
        &format!("/v1/apis/{api_id}/types/{type_name}/resolvers/{field}"),
        Some(body),
    )
    .await
}

/// Assert `answer` is the refusal `row`'s broken definition is owed.
fn assert_refused(case: &str, answer: &Answer, row: &Pairing) {
    assert_eq!(
        (
            answer.status,
            answer.error_type.as_str(),
            answer.message.as_str()
        ),
        (400, "BadRequestException", row.message),
        "case {case}: expected 400 BadRequestException {:?}, got {answer:?}",
        row.message
    );
}

/// Assert `resolver` reports the definition `row`'s repair asked for: the kind it is of
/// and the members that define that kind.
fn assert_reports(case: &str, resolver: &Value, row: &Pairing) {
    assert_eq!(
        resolver["kind"].as_str(),
        Some(row.kind),
        "case {case}: expected kind {:?}, got {resolver:?}",
        row.kind
    );
    match row.data_source {
        Some(name) => assert_eq!(
            resolver["dataSourceName"].as_str(),
            Some(name),
            "case {case}: expected dataSourceName {name:?}, got {resolver:?}"
        ),
        None => assert!(
            resolver["dataSourceName"].is_null(),
            "case {case}: expected no dataSourceName, got {resolver:?}"
        ),
    }
    match &row.functions {
        Some(functions) => assert_eq!(
            resolver["pipelineConfig"]["functions"],
            json!(functions),
            "case {case}: expected the pipeline {functions:?}, got {resolver:?}"
        ),
        None => assert!(
            resolver["pipelineConfig"].is_null(),
            "case {case}: expected no pipelineConfig, got {resolver:?}"
        ),
    }
}

#[tokio::test]
async fn a_create_refuses_a_definition_that_does_not_pair_up_and_serves_its_repair() {
    let router = router();
    let (api_id, function_id) =
        api_with_dependencies(&router, "Api_definition_pairing_create").await;

    for (index, row) in pairings(&function_id).iter().enumerate() {
        let broken_field = format!("broken_{index}");
        let refused =
            create_resolver_body(&router, &api_id, TYPE, on_field(&row.broken, &broken_field))
                .await;
        assert_refused(&format!("create {}", row.label), &refused, row);

        // The refusal wrote nothing: the field it named carries no resolver.
        let absent = get_resolver(&router, &api_id, TYPE, &broken_field).await;
        assert_eq!(
            (
                absent.status,
                absent.error_type.as_str(),
                absent.message.as_str()
            ),
            (404, "NotFoundException", NO_RESOLVER),
            "case create {}: expected the field to carry no resolver, got {absent:?}",
            row.label
        );

        // The repair of the SAME definition is served, and reports the member that
        // repaired it ...
        let repaired_field = format!("repaired_{index}");
        let created = create_resolver_body(
            &router,
            &api_id,
            TYPE,
            on_field(&row.repaired, &repaired_field),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case create {} repaired: expected 200, got {created:?}",
            row.label
        );
        let case = format!("create {} repaired", row.label);
        assert_reports(&case, &created.body["resolver"], row);

        // ... and the stored record an independent read answers is that same body.
        let read = get_resolver(&router, &api_id, TYPE, &repaired_field).await;
        assert_eq!(
            read.body["resolver"], created.body["resolver"],
            "case create {} repaired: expected GetResolver to answer the stored record \
             {:?}, got {:?}",
            row.label, created.body["resolver"], read.body["resolver"]
        );
    }
}

#[tokio::test]
async fn an_update_refuses_the_same_definitions_and_leaves_the_resolver_as_it_was() {
    let router = router();
    let (api_id, function_id) =
        api_with_dependencies(&router, "Api_definition_pairing_update").await;

    for (index, row) in pairings(&function_id).iter().enumerate() {
        // The resolver the update addresses: the row's repaired definition, which a
        // create serves.
        let field = format!("updated_{index}");
        let created =
            create_resolver_body(&router, &api_id, TYPE, on_field(&row.repaired, &field)).await;
        assert_eq!(
            created.status, 200,
            "case update {}: expected the resolver to attach first, got {created:?}",
            row.label
        );
        let stored = created.body["resolver"].clone();

        // An update states the definition in full, so the same rule judges it — and the
        // refusal leaves the resolver already attached exactly as it was.
        let refused = update_resolver(&router, &api_id, TYPE, &field, row.broken.clone()).await;
        assert_refused(&format!("update {}", row.label), &refused, row);
        let read = get_resolver(&router, &api_id, TYPE, &field).await;
        assert_eq!(
            read.body["resolver"], stored,
            "case update {}: expected the stored resolver untouched {stored:?}, got {:?}",
            row.label, read.body["resolver"]
        );

        // The same call carrying a definition that pairs up is served, so the refusal is
        // about the definition rather than about the operation.
        let updated = update_resolver(&router, &api_id, TYPE, &field, row.repaired.clone()).await;
        assert_eq!(
            updated.status, 200,
            "case update {} repaired: expected 200, got {updated:?}",
            row.label
        );
        assert_reports(
            &format!("update {} repaired", row.label),
            &updated.body["resolver"],
            row,
        );
    }
}
