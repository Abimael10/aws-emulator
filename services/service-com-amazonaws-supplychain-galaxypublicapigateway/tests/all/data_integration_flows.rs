// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of the AWS Supply Chain **data-integration flow**, driven over
//! the wire (restJson1, signing name `scn`) through the same `MultiServiceRouter` the
//! emulator's front door uses — so every assertion here is about what a real client
//! observes, not about a handler's internals.
//!
//! RULE under test: `CreateDataIntegrationFlow` persists one flow per
//! (`instanceId`, `name`) pair of an existing instance, and `GetDataIntegrationFlow`
//! answers with exactly the configuration (`sources`, `transformation`, `target`) and the
//! timestamps the create stored; a pair that is already taken is a conflict, a request
//! whose declared source/target/transformation type carries none of its configuration is
//! a validation failure that persists nothing, an `instanceId` naming no instance is an
//! authorization failure, and a name the instance holds no flow under is a not-found.
//!
//! `DeleteDataIntegrationFlow` is the other side of that pair: it TAKES the entry out of
//! the store, so a name it has removed is a name the instance holds no flow under — every
//! later read of it misses and the name is free to be created again.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::ops::types::{
    DATA_INTEGRATION_FLOW_FILE_TYPE_VALUES, DATA_INTEGRATION_FLOW_LOAD_TYPE_VALUES,
    DATA_INTEGRATION_FLOW_SOURCE_TYPE_VALUES, DATA_INTEGRATION_FLOW_TARGET_TYPE_VALUES,
    DATA_INTEGRATION_FLOW_TRANSFORMATION_TYPE_VALUES,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::state::MAX_FLOWS_PER_INSTANCE;
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these tests sign for.
const REGION: &str = "us-east-1";

/// How a flow call refuses a name the addressed instance holds no flow under: the fixed
/// text with the flow name and the instance id from the request's two `@httpLabel`
/// members. Spelled out here, independently of the handler, so a change to the service's
/// wording fails this suite instead of travelling with it.
fn expected_not_found_sentence(name: &str, instance_id: &str) -> String {
    format!("Flow {name} does not exist in instance {instance_id}")
}

/// How long a poll loop waits for the condition it is about before giving up, and how long
/// it waits between looks.
///
/// Generous on purpose: the loop ends as soon as the condition holds, so the deadline is
/// only there to end a case that can never satisfy it and never a budget the host has to
/// beat.
const POLL_DEADLINE: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// How a create refuses a name the addressed instance already holds a flow under; only the
/// flow name comes from the request.
fn expected_conflict_sentence(name: &str) -> String {
    format!("Flow with flow name: {name} already exists")
}

/// How a flow write refuses a declared source/target/transformation type whose configuring
/// member the request omitted.
fn expected_missing_configuration_sentence(
    member: &str,
    discriminator: &str,
    value: &str,
) -> String {
    format!("{member} is required when {discriminator} is {value}")
}

/// How the gateway refuses a call whose `instanceId` names no instance of this account.
fn expected_authorization_failure(instance_id: &str) -> String {
    format!(
        "Authorization Failed (Public). The instanceId {instance_id} does not exist, or does not \
         belong to the account {}",
        engine::emulator_account_id()
    )
}

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Keep the test independent of how this process was launched: no authenticator,
        // so a request routes on its credential scope alone.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one request signed for [`REGION`] and return its HTTP status and JSON body.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        // A SigV4 `Authorization` header whose credential scope names this service
        // (`scn`), which is how the router picks the service that claims the request.
        // Signature verification is off for the test router, so the signature itself is
        // never read.
        let credential = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/scn/aws4_request, \
             SignedHeaders=host, Signature=00"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://scn.{REGION}.amazonaws.com{path}"))
            .header("host", format!("scn.{REGION}.amazonaws.com"))
            .header("authorization", credential)
            .header("content-type", "application/json")
            .body(body_bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let bytes = response.into_body();
        let json = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                panic!(
                    "{method} {path}: body is not JSON ({e}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, json)
    }

    /// A fresh instance, returning its `instanceId`.
    async fn create_instance(&self, case: &str) -> String {
        let (status, body) = self.send("POST", "/api/instance", Some(json!({}))).await;
        assert_eq!(
            status, 200,
            "case {case}: CreateInstance expected 200, got {status} with {body}"
        );
        body.get("instance")
            .and_then(|instance| instance.get("instanceId"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case}: CreateInstance returned no instanceId: {body}"))
            .to_string()
    }

    /// `CreateDataIntegrationFlow` for one (instance, name) pair, with `request` as body.
    async fn create_flow(&self, instance_id: &str, name: &str, request: Value) -> (u16, Value) {
        self.send("PUT", &flow_path(instance_id, name), Some(request))
            .await
    }

    /// `GetDataIntegrationFlow` for one (instance, name) pair.
    async fn get_flow(&self, instance_id: &str, name: &str) -> (u16, Value) {
        self.send("GET", &flow_path(instance_id, name), None).await
    }

    /// `DeleteDataIntegrationFlow` for one (instance, name) pair.
    async fn delete_flow(&self, instance_id: &str, name: &str) -> (u16, Value) {
        self.send("DELETE", &flow_path(instance_id, name), None)
            .await
    }
}

/// The URI every flow operation is addressed at, as the model's `http` trait spells it:
/// `/api/data-integration/instance/{instanceId}/data-integration-flows/{name}`.
fn flow_path(instance_id: &str, name: &str) -> String {
    format!("/api/data-integration/instance/{instance_id}/data-integration-flows/{name}")
}

/// The `flow` member of a response body.
fn flow_of(case: &str, body: &Value) -> Value {
    body.get("flow")
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: response carries no `flow` member: {body}"))
}

/// Look again until the deletion of `instance_id` has SETTLED — until `GetInstance`
/// answers 404 — or until [`POLL_DEADLINE`].
///
/// `DeleteInstance` ACCEPTS a deletion: the instance moves to `Deleting` and stays
/// addressable, with the flows addressed by it, while the deletion settles; the removal is
/// the clock's work, materialised by whichever CALL next observes the store — which is why
/// each look is a request.
async fn wait_until_deleted(emulator: &Emulator, instance_id: &str) {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let (status, body) = emulator
            .send("GET", &format!("/api/instance/{instance_id}"), None)
            .await;
        if status == 404 {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the deletion of {instance_id} never settled; GetInstance still answers {status} \
             with {body}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// A string member of a payload.
fn string_of(case: &str, payload: &Value, member: &str) -> String {
    payload
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: payload carries no `{member}`: {payload}"))
        .to_string()
}

/// The `__type` of an error envelope.
fn error_type(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The `message` of an error envelope.
fn error_message(body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// An S3 source reading `prefix` of `bucket`, optionally with a file type.
fn s3_source(source_name: &str, bucket: &str, prefix: &str, file_type: Option<&str>) -> Value {
    let mut configuration = json!({ "bucketName": bucket, "prefix": prefix });
    if let Some(file_type) = file_type {
        configuration["options"] = json!({ "fileType": file_type });
    }
    json!({
        "sourceType": "S3",
        "sourceName": source_name,
        "s3Source": configuration,
    })
}

/// A dataset source reading `dataset_identifier`, optionally with load/dedupe options.
fn dataset_source(source_name: &str, dataset_identifier: &str, options: Option<Value>) -> Value {
    let mut configuration = json!({ "datasetIdentifier": dataset_identifier });
    if let Some(options) = options {
        configuration["options"] = options;
    }
    json!({
        "sourceType": "DATASET",
        "sourceName": source_name,
        "datasetSource": configuration,
    })
}

/// The dataset ARN a flow addresses a data-lake dataset by, in `AscResourceArn`'s
/// modelled form.
fn dataset_identifier(instance_id: &str, namespace: &str, name: &str) -> String {
    format!(
        "arn:aws:scn:{REGION}:{}:instance/{instance_id}/namespaces/{namespace}/datasets/{name}",
        engine::emulator_account_id()
    )
}

/// The configuration a flow request declared a type for but did not carry: the member
/// that is missing, the `…Type` member that discriminates it, and the declared value.
struct Missing {
    member: &'static str,
    discriminator: &'static str,
    value: &'static str,
}

/// One row of the declared-type table: the request, and the configuration it omits —
/// `None` for a row every declared type of which carries its configuration.
struct ConfigurationRow {
    case: &'static str,
    request: Value,
    missing: Option<Missing>,
}

/// One row of the create/read-back table: the flow's name and request body.
struct Row {
    case: &'static str,
    name: String,
    request: Value,
}

/// The rows spanning the class the rule generalises over: every modelled source type and
/// target type, both modelled transformation types, one and many sources (the modelled
/// `@length` 1..40 of `sources`), the optional `options` and `tags` members present and
/// absent, and names at both ends of the modelled `^[A-Za-z0-9-]+$` / 1..256 shape.
///
/// The enum values the rows draw on come from the model's own enums, re-exported as
/// `DATA_INTEGRATION_FLOW_SOURCE_TYPE_VALUES`, `DATA_INTEGRATION_FLOW_TARGET_TYPE_VALUES`,
/// `DATA_INTEGRATION_FLOW_TRANSFORMATION_TYPE_VALUES`,
/// `DATA_INTEGRATION_FLOW_FILE_TYPE_VALUES` and `DATA_INTEGRATION_FLOW_LOAD_TYPE_VALUES`.
fn create_rows(instance_id: &str) -> Vec<Row> {
    // The last entry of each catalog, so no row rides on the first value alone.
    let last_file_type = *DATA_INTEGRATION_FLOW_FILE_TYPE_VALUES
        .last()
        .expect("the model declares at least one file type");
    let last_load_type = *DATA_INTEGRATION_FLOW_LOAD_TYPE_VALUES
        .last()
        .expect("the model declares at least one load type");

    vec![
        Row {
            case: "one S3 source, SQL transformation, dataset target, nothing optional",
            name: "inbound-orders".to_string(),
            request: json!({
                "sources": [s3_source("orders", "supply-landing", "orders/", None)],
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": { "query": "SELECT * FROM orders" },
                },
                "target": {
                    "targetType": "DATASET",
                    "datasetTarget": {
                        "datasetIdentifier": dataset_identifier(instance_id, "asc", "inbound_order"),
                    },
                },
            }),
        },
        Row {
            case: "dataset source with every option, NONE transformation, S3 target, tagged",
            name: "dataset-to-s3".to_string(),
            request: json!({
                "sources": [dataset_source(
                    "product",
                    &dataset_identifier(instance_id, "default", "product"),
                    Some(json!({
                        "loadType": last_load_type,
                        "dedupeRecords": true,
                        "dedupeStrategy": {
                            "type": "FIELD_PRIORITY",
                            "fieldPriority": {
                                "fields": [{ "name": "updated_at", "sortOrder": "DESC" }],
                            },
                        },
                    })),
                )],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "S3",
                    "s3Target": {
                        "bucketName": "supply-exports",
                        "prefix": "product/",
                        "options": { "fileType": last_file_type },
                    },
                },
                "tags": { "team": "supply", "env": "" },
            }),
        },
        Row {
            case: "one source of every modelled source type joined by SQL into S3",
            name: "joined-feed".to_string(),
            request: json!({
                "sources": [
                    s3_source("raw", "supply-landing", "raw/", Some(last_file_type)),
                    dataset_source(
                        "enriched",
                        &dataset_identifier(instance_id, "default", "enriched"),
                        None,
                    ),
                ],
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": {
                        "query": "SELECT r.id, e.label FROM raw r JOIN enriched e ON r.id = e.id",
                    },
                },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "joined/" },
                },
            }),
        },
        Row {
            case: "single-character name, the modelled minimum",
            name: "a".to_string(),
            request: json!({
                "sources": [s3_source("single", "supply-landing", "a/", None)],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "a/" },
                },
            }),
        },
        Row {
            case: "name at the modelled 256-character maximum",
            // 256 characters of `^[A-Za-z0-9-]+$`, the shape's upper length bound.
            name: format!("f{}", "-A9".repeat(85)),
            request: json!({
                "sources": [s3_source("longname", "supply-landing", "long/", None)],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "long/" },
                },
            }),
        },
        Row {
            case: "sources at the modelled maximum of 40",
            name: "forty-sources".to_string(),
            request: json!({
                "sources": (0..40)
                    .map(|index| s3_source(
                        &format!("source_{index}"),
                        "supply-landing",
                        &format!("part-{index}/"),
                        None,
                    ))
                    .collect::<Vec<Value>>(),
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": { "query": "SELECT * FROM source_0" },
                },
                "target": {
                    "targetType": "DATASET",
                    "datasetTarget": {
                        "datasetIdentifier": dataset_identifier(instance_id, "default", "wide"),
                        "options": { "loadType": last_load_type },
                    },
                },
            }),
        },
    ]
}

/// RULE: a created flow is persisted under its (instance, name) pair and
/// `GetDataIntegrationFlow` answers with exactly the configuration the create stored —
/// every member projected from the stored record, the optional ones absent where the
/// request named none — and reading it again changes nothing.
#[tokio::test]
async fn a_flow_round_trips_through_create_and_get() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("round trip").await;

    for row in create_rows(&instance_id) {
        let case = row.case;
        let (status, body) = emulator
            .create_flow(&instance_id, &row.name, row.request.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateDataIntegrationFlow expected 200, got {status} with {body}"
        );
        assert_eq!(
            string_of(case, &body, "instanceId"),
            instance_id,
            "case {case}: the create must report the instance it was served for, got {body}"
        );
        assert_eq!(
            string_of(case, &body, "name"),
            row.name,
            "case {case}: the create must report the name it created, got {body}"
        );

        let (status, body) = emulator.get_flow(&instance_id, &row.name).await;
        assert_eq!(
            status, 200,
            "case {case}: GetDataIntegrationFlow expected 200, got {status} with {body}"
        );
        let read = flow_of(case, &body);

        assert_eq!(
            string_of(case, &read, "instanceId"),
            instance_id,
            "case {case}: the flow must report the instance it belongs to, got {read}"
        );
        assert_eq!(
            string_of(case, &read, "name"),
            row.name,
            "case {case}: the flow must report its own name, got {read}"
        );
        assert_eq!(
            read.get("sources"),
            row.request.get("sources"),
            "case {case}: the stored sources must be the requested ones, got {read}"
        );
        assert_eq!(
            read.get("transformation"),
            row.request.get("transformation"),
            "case {case}: the stored transformation must be the requested one, got {read}"
        );
        assert_eq!(
            read.get("target"),
            row.request.get("target"),
            "case {case}: the stored target must be the requested one, got {read}"
        );
        assert_eq!(
            read.get("createdTime"),
            read.get("lastModifiedTime"),
            "case {case}: an untouched flow was last modified when it was created, got {read}"
        );
        assert!(
            read.get("createdTime").and_then(Value::as_f64).is_some(),
            "case {case}: createdTime must be a timestamp, got {read}"
        );
        assert!(
            read.get("tags").is_none(),
            "case {case}: `tags` is not a member of the DataIntegrationFlow shape, got {read}"
        );

        let (status, body) = emulator.get_flow(&instance_id, &row.name).await;
        assert_eq!(
            status, 200,
            "case {case}: a second read expected 200, got {status} with {body}"
        );
        assert_eq!(
            flow_of(case, &body),
            read,
            "case {case}: reading a flow must not change it, got {body}"
        );
    }
}

/// RULE: an (instance, name) pair names one flow, so a create for a pair that already
/// holds one is refused `ConflictException` (409) carrying the service's conflict
/// sentence, and the stored flow is left exactly as it was.
#[tokio::test]
async fn a_second_create_of_one_pair_is_a_conflict() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("conflict").await;

    let first_request = |query: &str| {
        json!({
            "sources": [s3_source("orders", "supply-landing", "orders/", None)],
            "transformation": {
                "transformationType": "SQL",
                "sqlTransformation": { "query": query },
            },
            "target": {
                "targetType": "S3",
                "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
            },
        })
    };

    // Rows differing in what the SECOND create changes: nothing, the sources, the
    // transformation, the target, the tags — and in the shape of the name it reuses.
    let rows: Vec<(&str, String, Value)> = vec![
        (
            "identical request",
            "repeat-identical".to_string(),
            first_request("SELECT * FROM orders"),
        ),
        (
            "different sources",
            "repeat-other-sources".to_string(),
            json!({
                "sources": [dataset_source(
                    "orders",
                    &dataset_identifier(&instance_id, "default", "orders"),
                    None,
                )],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
                },
            }),
        ),
        (
            "a different transformation query",
            "repeat-other-query".to_string(),
            first_request("SELECT id FROM orders"),
        ),
        (
            "a different target",
            "repeat-other-target".to_string(),
            json!({
                "sources": [s3_source("orders", "supply-landing", "orders/", None)],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "DATASET",
                    "datasetTarget": {
                        "datasetIdentifier": dataset_identifier(&instance_id, "default", "orders"),
                    },
                },
            }),
        ),
        (
            "single-character name",
            "z".to_string(),
            first_request("SELECT * FROM orders"),
        ),
    ];

    for (case, name, second_request) in rows {
        let (status, body) = emulator
            .create_flow(&instance_id, &name, first_request("SELECT * FROM orders"))
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the first create expected 200, got {status} with {body}"
        );
        let (status, body) = emulator.get_flow(&instance_id, &name).await;
        assert_eq!(
            status, 200,
            "case {case}: the first flow must be readable, got {status} with {body}"
        );
        let first = flow_of(case, &body);

        let (status, body) = emulator
            .create_flow(&instance_id, &name, second_request)
            .await;
        assert_eq!(
            status, 409,
            "case {case}: a second create of one pair must be refused, got {status} with {body}"
        );
        assert!(
            error_type(&body).contains("ConflictException"),
            "case {case}: expected a ConflictException envelope, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_conflict_sentence(&name),
            "case {case}: the refusal must carry the service's conflict sentence for this flow, \
             got {body}"
        );

        let (status, body) = emulator.get_flow(&instance_id, &name).await;
        assert_eq!(
            status, 200,
            "case {case}: the stored flow survives the refusal, got {status} with {body}"
        );
        assert_eq!(
            flow_of(case, &body),
            first,
            "case {case}: a refused create must change nothing, got {body}"
        );
    }
}

/// RULE: a flow's declared source, target and transformation type is the discriminator for
/// the configuration member that configures it, so a request that declares a type and
/// carries none of its configuration is refused `ValidationException` (400) and persists
/// nothing; the one type that is configured by nothing (`NONE`) is served.
#[tokio::test]
async fn a_declared_type_without_its_configuration_is_refused() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("configuration").await;

    let sql = json!({
        "transformationType": "SQL",
        "sqlTransformation": { "query": "SELECT * FROM orders" },
    });
    let s3_target = json!({
        "targetType": "S3",
        "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
    });
    let ok_sources = json!([s3_source("orders", "supply-landing", "orders/", None)]);

    // Rows: one per (declared type, missing configuration member) pair the rule covers,
    // over every value of the three modelled enums, plus the served `NONE` row.
    let rows: Vec<ConfigurationRow> = vec![
        ConfigurationRow {
            case: "S3 source without s3Source",
            request: json!({
                "sources": [{ "sourceType": "S3", "sourceName": "orders" }],
                "transformation": sql.clone(),
                "target": s3_target.clone(),
            }),
            missing: Some(Missing {
                member: "s3Source",
                discriminator: "sourceType",
                value: "S3",
            }),
        },
        ConfigurationRow {
            case: "DATASET source without datasetSource",
            request: json!({
                "sources": [{ "sourceType": "DATASET", "sourceName": "orders" }],
                "transformation": sql.clone(),
                "target": s3_target.clone(),
            }),
            missing: Some(Missing {
                member: "datasetSource",
                discriminator: "sourceType",
                value: "DATASET",
            }),
        },
        ConfigurationRow {
            case: "the second of two sources missing its configuration",
            request: json!({
                "sources": [
                    s3_source("orders", "supply-landing", "orders/", None),
                    { "sourceType": "DATASET", "sourceName": "product" },
                ],
                "transformation": sql.clone(),
                "target": s3_target.clone(),
            }),
            missing: Some(Missing {
                member: "datasetSource",
                discriminator: "sourceType",
                value: "DATASET",
            }),
        },
        ConfigurationRow {
            case: "S3 target without s3Target",
            request: json!({
                "sources": ok_sources.clone(),
                "transformation": sql.clone(),
                "target": { "targetType": "S3" },
            }),
            missing: Some(Missing {
                member: "s3Target",
                discriminator: "targetType",
                value: "S3",
            }),
        },
        ConfigurationRow {
            case: "DATASET target without datasetTarget",
            request: json!({
                "sources": ok_sources.clone(),
                "transformation": sql.clone(),
                "target": { "targetType": "DATASET" },
            }),
            missing: Some(Missing {
                member: "datasetTarget",
                discriminator: "targetType",
                value: "DATASET",
            }),
        },
        ConfigurationRow {
            case: "SQL transformation without sqlTransformation",
            request: json!({
                "sources": ok_sources.clone(),
                "transformation": { "transformationType": "SQL" },
                "target": s3_target.clone(),
            }),
            missing: Some(Missing {
                member: "sqlTransformation",
                discriminator: "transformationType",
                value: "SQL",
            }),
        },
        ConfigurationRow {
            case: "NONE transformation, which is configured by nothing",
            request: json!({
                "sources": ok_sources.clone(),
                "transformation": { "transformationType": "NONE" },
                "target": s3_target.clone(),
            }),
            missing: None,
        },
    ];

    // Every value of each modelled enum is exercised by the rows above, so a value added
    // to the model later shows up here as an uncovered one.
    let covered: BTreeSet<&str> = rows
        .iter()
        .filter_map(|row| row.missing.as_ref().map(|missing| missing.value))
        .chain(["NONE"])
        .collect();
    for value in DATA_INTEGRATION_FLOW_SOURCE_TYPE_VALUES
        .iter()
        .chain(DATA_INTEGRATION_FLOW_TARGET_TYPE_VALUES)
        .chain(DATA_INTEGRATION_FLOW_TRANSFORMATION_TYPE_VALUES)
    {
        assert!(
            covered.contains(value),
            "case enum coverage: no row declares the {value} type, covered {covered:?}"
        );
    }

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let name = format!("configuration-{index}");
        let (status, body) = emulator.create_flow(&instance_id, &name, row.request).await;
        match row.missing {
            Some(missing) => {
                assert_eq!(
                    status, 400,
                    "case {case}: a declared type with no configuration must be refused, got \
                     {status} with {body}"
                );
                assert!(
                    error_type(&body).contains("ValidationException"),
                    "case {case}: expected a ValidationException envelope, got {body}"
                );
                assert_eq!(
                    error_message(&body),
                    expected_missing_configuration_sentence(
                        missing.member,
                        missing.discriminator,
                        missing.value
                    ),
                    "case {case}: the refusal must name the missing configuration, got {body}"
                );

                let (status, body) = emulator.get_flow(&instance_id, &name).await;
                assert_eq!(
                    status, 404,
                    "case {case}: a refused create must persist nothing, got {status} with {body}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case}: expected 200, got {status} with {body}"
                );
                let (status, body) = emulator.get_flow(&instance_id, &name).await;
                assert_eq!(
                    status, 200,
                    "case {case}: the created flow must be readable, got {status} with {body}"
                );
            }
        }
    }
}

/// RULE: a flow exists only under an instance this account holds, so every flow call
/// naming another `instanceId` is refused by the gateway's authorization step (403)
/// before the flow is looked up — and an `instanceId` outside the modelled `UUID` shape is
/// refused `ValidationException` (400) before any handler runs.
#[tokio::test]
async fn a_flow_call_for_an_unknown_instance_is_an_authorization_failure() {
    let emulator = Emulator::new();
    let live = emulator.create_instance("authorization").await;
    let request = json!({
        "sources": [s3_source("orders", "supply-landing", "orders/", None)],
        "transformation": { "transformationType": "NONE" },
        "target": {
            "targetType": "S3",
            "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
        },
    });

    // Rows: (case, instanceId, expected status).
    let rows: Vec<(&str, String, u16)> = vec![
        (
            "the all-zero UUID, which no instance holds",
            "00000000-0000-0000-0000-000000000000".to_string(),
            403,
        ),
        (
            "a well-formed UUID one character off a live one",
            {
                let mut other = live.clone();
                let last = other.pop().expect("an instance id is never empty");
                let swapped = if last == 'a' { 'b' } else { 'a' };
                other.push(swapped);
                other
            },
            403,
        ),
        ("an instanceId outside the modelled UUID shape", "not-a-uuid".to_string(), 400),
        ("the live instance", live.clone(), 200),
    ];

    for (case, instance_id, expected) in rows {
        let (status, body) = emulator
            .create_flow(&instance_id, "probe", request.clone())
            .await;
        assert_eq!(
            status, expected,
            "case {case}: CreateDataIntegrationFlow expected {expected}, got {status} with {body}"
        );
        if expected == 403 {
            assert!(
                error_type(&body).contains("AccessDeniedException"),
                "case {case}: expected an AccessDeniedException envelope, got {body}"
            );
            assert_eq!(
                error_message(&body),
                expected_authorization_failure(&instance_id),
                "case {case}: the refusal must carry the gateway's authorization sentence, got \
                 {body}"
            );
        }

        // The read is refused the same way, and is served only where the create was.
        let (status, body) = emulator.get_flow(&instance_id, "probe").await;
        assert_eq!(
            status, expected,
            "case {case}: GetDataIntegrationFlow expected {expected}, got {status} with {body}"
        );
    }
}

/// RULE: a name an instance holds no flow under is refused `ResourceNotFoundException`
/// (404) carrying the service's not-found sentence, and a flow is addressed by its whole
/// (instance, name) pair — so one name may be held once per instance and each instance
/// reads back its own flow.
#[tokio::test]
async fn a_flow_is_scoped_to_its_instance_and_name() {
    let emulator = Emulator::new();
    let first = emulator.create_instance("first instance").await;
    let second = emulator.create_instance("second instance").await;
    let shared = "shared-name";

    for (case, instance_id) in [("first", &first), ("second", &second)] {
        let (status, body) = emulator
            .create_flow(
                instance_id,
                shared,
                json!({
                    "sources": [s3_source(case, "supply-landing", &format!("{case}/"), None)],
                    "transformation": { "transformationType": "NONE" },
                    "target": {
                        "targetType": "S3",
                        "s3Target": { "bucketName": "supply-exports", "prefix": format!("{case}/") },
                    },
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: one name per instance is its own flow, got {status} with {body}"
        );
    }

    // Each instance reads back the flow IT created, told apart by its source name.
    for (case, instance_id) in [("first", &first), ("second", &second)] {
        let (status, body) = emulator.get_flow(instance_id, shared).await;
        assert_eq!(
            status, 200,
            "case {case}: expected 200, got {status} with {body}"
        );
        let flow = flow_of(case, &body);
        let source_name = flow
            .get("sources")
            .and_then(Value::as_array)
            .and_then(|sources| sources.first())
            .map(|source| string_of(case, source, "sourceName"))
            .unwrap_or_else(|| panic!("case {case}: the flow carries no source: {flow}"));
        assert_eq!(
            source_name, case,
            "case {case}: each instance must read back its own flow, got {flow}"
        );
        assert_eq!(
            string_of(case, &flow, "instanceId"),
            instance_id.as_str(),
            "case {case}: the flow must report its own instance, got {flow}"
        );
    }

    // Names the instance holds no flow under, differing in how close they are to one it
    // does hold.
    for (case, name) in [
        ("a name never created", "never-created"),
        ("one character off a stored name", "shared-names"),
        ("the stored name in another case", "Shared-Name"),
        ("a single character", "q"),
    ] {
        let (status, body) = emulator.get_flow(&first, name).await;
        assert_eq!(
            status, 404,
            "case {case}: expected 404, got {status} with {body}"
        );
        assert!(
            error_type(&body).contains("ResourceNotFoundException"),
            "case {case}: expected a ResourceNotFoundException envelope, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_not_found_sentence(name, &first),
            "case {case}: the refusal must name the flow and its instance, got {body}"
        );
    }
}

/// RULE: a flow is addressed by its instance, so it does not outlive the instance whose
/// deletion removed it — after which every call for it is the gateway's authorization
/// failure, as for any instance this account does not hold.
#[tokio::test]
async fn a_flow_does_not_outlive_its_instance() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("cascade").await;
    let request = json!({
        "sources": [s3_source("orders", "supply-landing", "orders/", None)],
        "transformation": { "transformationType": "NONE" },
        "target": {
            "targetType": "S3",
            "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
        },
    });

    for name in ["cascade-one", "cascade-two"] {
        let (status, body) = emulator
            .create_flow(&instance_id, name, request.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {name}: the create expected 200, got {status} with {body}"
        );
    }

    let (status, body) = emulator
        .send("DELETE", &format!("/api/instance/{instance_id}"), None)
        .await;
    assert_eq!(
        status, 200,
        "case delete: DeleteInstance expected 200, got {status} with {body}"
    );
    // A flow goes when the accepted deletion SETTLES: while the instance is `Deleting` its
    // flows are still readable.
    for name in ["cascade-one", "cascade-two"] {
        let (status, body) = emulator.get_flow(&instance_id, name).await;
        assert_eq!(
            status, 200,
            "case {name}: a Deleting instance's flow is still readable, got {status} with \
             {body}"
        );
    }
    wait_until_deleted(&emulator, &instance_id).await;

    for name in ["cascade-one", "cascade-two"] {
        let (status, body) = emulator.get_flow(&instance_id, name).await;
        assert_eq!(
            status, 403,
            "case {name}: a flow of a deleted instance is unreachable, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_authorization_failure(&instance_id),
            "case {name}: the refusal must carry the gateway's authorization sentence, got {body}"
        );
    }

    // The name is free again for a new instance: nothing of the deleted instance's flows
    // is left behind to conflict with it.
    let fresh = emulator.create_instance("fresh instance").await;
    let (status, body) = emulator
        .create_flow(&fresh, "cascade-one", request.clone())
        .await;
    assert_eq!(
        status, 200,
        "case reuse: a new instance may hold the same flow name, got {status} with {body}"
    );
}

/// RULE: the store holds its flows for the life of the process, so the number one instance
/// may hold is capped at the one site that adds a flow: a create past the cap is refused
/// with the modelled `ServiceQuotaExceededException` (402) and stores nothing, while the
/// flows already stored stay readable and another instance is unaffected by its neighbour's
/// cap.
#[tokio::test]
async fn a_create_past_the_per_instance_cap_is_refused() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("cap").await;
    let other = emulator.create_instance("other instance").await;
    let request = json!({
        "sources": [s3_source("orders", "supply-landing", "orders/", None)],
        "transformation": { "transformationType": "NONE" },
        "target": {
            "targetType": "S3",
            "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
        },
    });

    for index in 0..MAX_FLOWS_PER_INSTANCE {
        let (status, body) = emulator
            .create_flow(&instance_id, &format!("capped-{index}"), request.clone())
            .await;
        assert_eq!(
            status, 200,
            "case fill {index}: a create below the cap expected 200, got {status} with {body}"
        );
    }

    // Rows: a create past the cap carrying a fresh name is the quota refusal; one carrying
    // a name the instance already holds is still the conflict, because that create would
    // add nothing to the store at all.
    for (case, name, expected, kind) in [
        (
            "a fresh name",
            "capped-overflow",
            402,
            "ServiceQuotaExceededException",
        ),
        ("a name already held", "capped-0", 409, "ConflictException"),
    ] {
        let (status, body) = emulator.create_flow(&instance_id, name, request.clone()).await;
        assert_eq!(
            status, expected,
            "case {case}: a create at the cap expected {expected}, got {status} with {body}"
        );
        assert!(
            error_type(&body).contains(kind),
            "case {case}: expected a {kind} envelope, got {body}"
        );
    }

    let (status, body) = emulator.get_flow(&instance_id, "capped-overflow").await;
    assert_eq!(
        status, 404,
        "case overflow: a refused create must persist nothing, got {status} with {body}"
    );
    let (status, body) = emulator.get_flow(&instance_id, "capped-0").await;
    assert_eq!(
        status, 200,
        "case stored: the flows already stored survive the refusal, got {status} with {body}"
    );

    // The cap counts one instance's flows, so a neighbour at its cap does not refuse this
    // instance's first create.
    let (status, body) = emulator
        .create_flow(&other, "capped-0", request.clone())
        .await;
    assert_eq!(
        status, 200,
        "case neighbour: the cap is per instance, got {status} with {body}"
    );
}

/// RULE: `DeleteDataIntegrationFlow` TAKES the addressed (instance, name) entry out of the
/// store, so a name it has removed is a name the instance holds no flow under: the delete
/// answers 200 naming what it removed, every later read of that pair is refused
/// `ResourceNotFoundException` (404) naming the flow and its instance, a repeat delete is
/// that same not-found, and the name is free for a new flow.
#[tokio::test]
async fn a_deleted_flow_is_no_longer_readable() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("delete").await;

    // Rows differ along what the rule generalises over: the shape of the name the delete
    // addresses — both ends of the modelled `@length` 1..256 of `DataIntegrationFlowName`
    // and a name of digits and hyphens, each inside its `^[A-Za-z0-9-]+$` — and the
    // configuration the removed flow carried.
    let longest_name = "z".repeat(256);
    let rows: Vec<(&str, String, Value)> = vec![
        (
            "a single-character name, S3 to S3 untransformed",
            "q".to_string(),
            json!({
                "sources": [s3_source("orders", "supply-landing", "orders/", None)],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
                },
            }),
        ),
        (
            "the longest name the shape admits, a dataset source",
            longest_name.clone(),
            json!({
                "sources": [dataset_source(
                    "inbound",
                    &dataset_identifier(&instance_id, "default", "orders"),
                    None,
                )],
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": { "query": "SELECT * FROM inbound" },
                },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "longest/" },
                },
            }),
        ),
        (
            "a name of digits and hyphens, two sources into a dataset",
            "2026-01-31-0".to_string(),
            json!({
                "sources": [
                    s3_source("first", "supply-landing", "first/", None),
                    s3_source("second", "supply-landing", "second/", None),
                ],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "DATASET",
                    "datasetTarget": {
                        "datasetIdentifier": dataset_identifier(
                            &instance_id,
                            "default",
                            "orders",
                        ),
                    },
                },
            }),
        ),
    ];

    for (case, name, request) in rows {
        let (status, body) = emulator
            .create_flow(&instance_id, &name, request.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the create expected 200, got {status} with {body}"
        );
        let (status, body) = emulator.get_flow(&instance_id, &name).await;
        assert_eq!(
            status, 200,
            "case {case}: the created flow must be readable, got {status} with {body}"
        );

        // The delete reports the pair it removed, from the two labels that addressed it.
        let (status, body) = emulator.delete_flow(&instance_id, &name).await;
        assert_eq!(
            status, 200,
            "case {case}: the delete expected 200, got {status} with {body}"
        );
        assert_eq!(
            string_of(case, &body, "instanceId"),
            instance_id,
            "case {case}: the delete must name the instance it removed from, got {body}"
        );
        assert_eq!(
            string_of(case, &body, "name"),
            name,
            "case {case}: the delete must name the flow it removed, got {body}"
        );

        // The read misses, twice over: the entry is gone, not merely hidden from the
        // first look.
        for look in ["the first read after the delete", "the second read"] {
            let (status, body) = emulator.get_flow(&instance_id, &name).await;
            assert_eq!(
                status, 404,
                "case {case}, {look}: a deleted flow must not be readable, got {status} \
                 with {body}"
            );
            assert!(
                error_type(&body).contains("ResourceNotFoundException"),
                "case {case}, {look}: expected a ResourceNotFoundException envelope, got \
                 {body}"
            );
            assert_eq!(
                error_message(&body),
                expected_not_found_sentence(&name, &instance_id),
                "case {case}, {look}: the refusal must name the flow and its instance, got \
                 {body}"
            );
        }

        // A repeat delete has nothing left to take, so it is the same not-found.
        let (status, body) = emulator.delete_flow(&instance_id, &name).await;
        assert_eq!(
            status, 404,
            "case {case}: a repeat delete expected 404, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_not_found_sentence(&name, &instance_id),
            "case {case}: the repeat delete must carry the not-found sentence, got {body}"
        );

        // The name is free again: nothing of the removed flow is left to conflict with a
        // new one, and the new one is what the read answers with.
        let (status, body) = emulator
            .create_flow(&instance_id, &name, request.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the name must be free after the delete, got {status} with {body}"
        );
        let (status, body) = emulator.get_flow(&instance_id, &name).await;
        assert_eq!(
            status, 200,
            "case {case}: the re-created flow must be readable, got {status} with {body}"
        );
        assert_eq!(
            string_of(case, &flow_of(case, &body), "name"),
            name,
            "case {case}: the read must answer with the re-created flow, got {body}"
        );
    }
}

/// RULE: a flow is addressed by its whole (instance, name) pair, so a delete takes exactly
/// that one entry: a sibling flow of the same instance and the same name under another
/// instance are untouched, a name no flow is held under is the flow's not-found, and an
/// `instanceId` naming no instance of this account is refused by the gateway's
/// authorization step before any flow is looked up.
#[tokio::test]
async fn a_delete_takes_only_the_addressed_flow() {
    let emulator = Emulator::new();
    let first = emulator.create_instance("first instance").await;
    let second = emulator.create_instance("second instance").await;
    let request = json!({
        "sources": [s3_source("orders", "supply-landing", "orders/", None)],
        "transformation": { "transformationType": "NONE" },
        "target": {
            "targetType": "S3",
            "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
        },
    });

    // The same two names in both instances, so one delete has a sibling and a namesake to
    // leave alone.
    for instance_id in [&first, &second] {
        for name in ["shared-name", "sibling-name"] {
            let (status, body) = emulator
                .create_flow(instance_id, name, request.clone())
                .await;
            assert_eq!(
                status, 200,
                "case {name}: the create expected 200, got {status} with {body}"
            );
        }
    }

    let (status, body) = emulator.delete_flow(&first, "shared-name").await;
    assert_eq!(
        status, 200,
        "case delete: expected 200, got {status} with {body}"
    );

    // Rows: every (instance, name) pair of the four created, and what the read answers for
    // it now that one of them is gone.
    for (case, instance_id, name, expected) in [
        ("the deleted pair", &first, "shared-name", 404),
        ("its sibling in the same instance", &first, "sibling-name", 200),
        ("the same name in another instance", &second, "shared-name", 200),
        ("the sibling in another instance", &second, "sibling-name", 200),
    ] {
        let (status, body) = emulator.get_flow(instance_id, name).await;
        assert_eq!(
            status, expected,
            "case {case}: expected {expected}, got {status} with {body}"
        );
    }

    // Names this instance holds no flow under, differing in how close they are to one it
    // does hold: each delete is the flow's not-found and takes nothing.
    for (case, name) in [
        ("a name never created", "never-created"),
        ("one character off a stored name", "sibling-names"),
        ("the stored name in another case", "Sibling-Name"),
        ("a single character", "q"),
    ] {
        let (status, body) = emulator.delete_flow(&first, name).await;
        assert_eq!(
            status, 404,
            "case {case}: expected 404, got {status} with {body}"
        );
        assert!(
            error_type(&body).contains("ResourceNotFoundException"),
            "case {case}: expected a ResourceNotFoundException envelope, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_not_found_sentence(name, &first),
            "case {case}: the refusal must name the flow and its instance, got {body}"
        );
    }

    // An `instanceId` of the modelled shape that names no instance of this account is the
    // gateway's authorization failure, whatever name the delete addresses.
    let unknown = "00000000-0000-4000-8000-000000000000";
    for (case, name) in [
        ("a name held elsewhere", "sibling-name"),
        ("a name held nowhere", "never-created"),
    ] {
        let (status, body) = emulator.delete_flow(unknown, name).await;
        assert_eq!(
            status, 403,
            "case {case}: expected 403, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_authorization_failure(unknown),
            "case {case}: the refusal must carry the gateway's authorization sentence, got \
             {body}"
        );
    }

    // Nothing the refusals touched: the three flows that were never deleted are still
    // readable.
    for (case, instance_id, name) in [
        ("sibling", &first, "sibling-name"),
        ("namesake", &second, "shared-name"),
        ("namesake sibling", &second, "sibling-name"),
    ] {
        let (status, body) = emulator.get_flow(instance_id, name).await;
        assert_eq!(
            status, 200,
            "case {case}: a refused delete must take nothing, got {status} with {body}"
        );
    }
}
