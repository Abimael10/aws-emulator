// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of the AWS Supply Chain **data-integration flow**, driven over
//! the wire (restJson1, signing name `scn`) through the same `MultiServiceRouter` the
//! emulator's front door uses — so every assertion here is about what a real client
//! observes, not about a handler's internals.
//!
//! RULE under test: `CreateDataIntegrationFlow` persists one flow per
//! (`instanceId`, `name`) pair of an existing instance, and `GetDataIntegrationFlow`
//! answers with exactly the definition the flow carries — its pipeline (`sources`,
//! `transformation`, `target`, each dataset end with the identity of the dataset it moves),
//! its timestamps, who manages it and what triggers it; a pair that is already taken is a
//! conflict, a request
//! whose declared source/target/transformation type carries none of its configuration is
//! a validation failure that persists nothing, an `instanceId` naming no instance is an
//! authorization failure, and a name the instance holds no flow under is a not-found.
//!
//! `UpdateDataIntegrationFlow` is how a stored flow changes: it applies each configuration
//! its request carries, leaves the rest as it was, and answers with the flow a later read
//! answers with.
//!
//! `DeleteDataIntegrationFlow` is the other side of that pair: it TAKES the entry out of
//! the store, so a name it has removed is a name the instance holds no flow under — every
//! later read of it misses and the name is free to be created again.
//!
//! `UpdateDataIntegrationFlow` is the third: it writes the ends of an EXISTING flow's
//! pipeline that the request carries (`sources`, `transformation`, `target`, each replaced
//! whole), leaves every end the request omits as it was, moves `lastModifiedTime` while
//! `createdTime` stays the create's, and is refused for a pair the store holds no flow for
//! or a declared type whose configuration the request omits.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::ops::types::{
    DATA_INTEGRATION_FLOW_FILE_TYPE_VALUES, DATA_INTEGRATION_FLOW_LOAD_TYPE_VALUES,
    DATA_INTEGRATION_FLOW_SOURCE_TYPE_VALUES, DATA_INTEGRATION_FLOW_TARGET_TYPE_VALUES,
    DATA_INTEGRATION_FLOW_TRANSFORMATION_TYPE_VALUES,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::state::{
    is_modelled_uuid, MAX_FLOWS_PER_INSTANCE,
};
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

    /// `UpdateDataIntegrationFlow` for one (instance, name) pair, with `request` as body —
    /// a PATCH, as the model's `http` trait binds this operation.
    async fn update_flow(&self, instance_id: &str, name: &str, request: Value) -> (u16, Value) {
        self.send("PATCH", &flow_path(instance_id, name), Some(request))
            .await
    }

    /// `CreateDataLakeNamespace` for `name`, asserted to succeed.
    async fn given_namespace(&self, case: &str, instance_id: &str, name: &str) {
        let (status, body) = self
            .send(
                "PUT",
                &format!("/api/datalake/instance/{instance_id}/namespaces/{name}"),
                Some(json!({})),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateDataLakeNamespace expected 200, got {status} with {body}"
        );
    }

    /// `CreateDataLakeDataset` for one triple, returning the ARN the create reported — the
    /// identifier a flow end names that dataset by.
    async fn given_dataset(
        &self,
        case: &str,
        instance_id: &str,
        namespace: &str,
        name: &str,
    ) -> String {
        let (status, body) = self
            .send(
                "PUT",
                &format!(
                    "/api/datalake/instance/{instance_id}/namespaces/{namespace}/datasets/{name}"
                ),
                Some(json!({
                    "schema": {
                        "name": "SimSchema",
                        "fields": [{ "name": "id", "type": "STRING", "isRequired": true }],
                    }
                })),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateDataLakeDataset expected 200, got {status} with {body}"
        );
        body.get("dataset")
            .and_then(|dataset| dataset.get("arn"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case}: the create carried no dataset arn: {body}"))
            .to_string()
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

/// The namespace AWS Supply Chain keeps its OWN data-lake datasets in: the one namespace the
/// model singles out (`DataIntegrationFlowTarget$datasetTarget` — "for AWS Supply Chain
/// dataset under asc namespace…"), which is what tells a dataset of the service from a
/// dataset a customer defined.
const SERVICE_NAMESPACE: &str = "asc";

/// The `datasetType` a flow read reports for one dataset end, written out HERE rather than
/// read from the crate so a drift in the service's projection fails this suite instead of
/// travelling with it.
///
/// Its SOURCE is the recorded flow read, which describes each dataset end by the dataset's
/// id and type and reports CUSTOM for a dataset of a customer namespace: the type says
/// whether the dataset is one the service keeps in its own namespace or one a customer
/// defined.
fn expected_dataset_type(identifier: &str) -> &'static str {
    let segments: Vec<&str> = identifier.split('/').collect();
    let namespace = match segments.as_slice() {
        [.., "namespaces", namespace, "datasets", _] => *namespace,
        _ => panic!("the identifier {identifier:?} is not a dataset ARN"),
    };
    if namespace == SERVICE_NAMESPACE {
        "ASC"
    } else {
        "CUSTOM"
    }
}

/// One dataset end as the flow read reports it: the configuration the request carried, plus
/// the dataset's type — and no `datasetId`, which only an end whose dataset the data lake
/// actually holds has (the rows these helpers serve name datasets nothing created, so the
/// read has no dataset to take an id from).
fn expected_dataset_end(configuration: &Value) -> Value {
    let identifier = configuration
        .get("datasetIdentifier")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a dataset end carries a datasetIdentifier: {configuration}"));
    let mut projected = configuration.clone();
    projected["datasetType"] = json!(expected_dataset_type(identifier));
    projected
}

/// The `sources` a flow read reports for the `sources` a flow write carried: each S3 source
/// as it was written, each dataset source with the dataset's identity beside its
/// configuration.
fn expected_sources(requested: &Value) -> Value {
    let sources = requested
        .as_array()
        .unwrap_or_else(|| panic!("`sources` is a list: {requested}"));
    Value::Array(
        sources
            .iter()
            .map(|source| {
                let mut projected = source.clone();
                if let Some(configuration) = source.get("datasetSource") {
                    projected["datasetSource"] = expected_dataset_end(configuration);
                }
                projected
            })
            .collect(),
    )
}

/// The `target` a flow read reports for the `target` a flow write carried.
fn expected_target(requested: &Value) -> Value {
    let mut projected = requested.clone();
    if let Some(configuration) = requested.get("datasetTarget") {
        projected["datasetTarget"] = expected_dataset_end(configuration);
    }
    projected
}

/// How every flow a caller creates reads back as managed: the recorded `managedBy` of a
/// customer-created flow.
const EXPECTED_MANAGED_BY: &str = "CUSTOMER";

/// How every flow a caller creates reads back as triggered: the recorded `triggerOptions` of
/// a flow with no schedule — it runs when the data at its source changes.
fn expected_trigger_options() -> Value {
    json!({ "triggerType": "SOURCE_CHANGE" })
}

/// Every member a LISTING entry carries, written out here rather than read from the crate so
/// a drift in the listing's projection fails this suite instead of travelling with it.
///
/// The first seven are the modelled `DataIntegrationFlow` members; `managedBy` is reported
/// beside them by the recorded responses, which carry no `triggerOptions` key on a listing
/// entry — that one belongs to the single-flow read.
const EXPECTED_LISTING_ENTRY_MEMBERS: [&str; 8] = [
    "instanceId",
    "name",
    "sources",
    "transformation",
    "target",
    "createdTime",
    "lastModifiedTime",
    "managedBy",
];

/// The entry the listing must report for the flow the single-flow read answered with `read`:
/// that same projection of that same stored flow, minus the trigger descriptor, which a
/// listing entry does not carry.
fn expected_listing_entry(case: &str, read: &Value) -> Value {
    let mut entry = flow_of(case, read);
    let members = entry
        .as_object_mut()
        .unwrap_or_else(|| panic!("case {case:?}: a flow read answers an object, got {read}"));
    assert!(
        members.remove("triggerOptions").is_some(),
        "case {case:?}: the single-flow read reports how the flow is triggered, got {read}"
    );
    entry
}

/// One row of the update table: the create that establishes the flow, and the update
/// applied to it.
struct UpdateRow {
    case: &'static str,
    base: Value,
    update: Value,
}

/// The configuration the flow is expected to END UP with: each end the update carries,
/// falling back to the one the create stored for an end the update says nothing about — each
/// as the READ reports it, so a dataset end carries the dataset identity the projection adds
/// beside the configuration that was written ([`expected_sources`], [`expected_target`]).
///
/// This is the rule under test spelled out once, independently of the handler, and read by
/// every row.
fn expected_configuration(base: &Value, update: &Value) -> Value {
    let mut expected = json!({});
    for member in ["sources", "transformation", "target"] {
        let value = update
            .get(member)
            .or_else(|| base.get(member))
            .unwrap_or_else(|| panic!("the base create carries every end of the pipeline: {base}"));
        expected[member] = match member {
            "sources" => expected_sources(value),
            "target" => expected_target(value),
            _ => value.clone(),
        };
    }
    expected
}

/// The `sources`, `transformation` and `target` of a flow payload, as
/// [`expected_configuration`] spells them, so the two are compared member for member.
fn configuration_of(case: &str, flow: &Value) -> Value {
    let mut actual = json!({});
    for member in ["sources", "transformation", "target"] {
        let value = flow
            .get(member)
            .unwrap_or_else(|| panic!("case {case}: the flow carries no `{member}`: {flow}"));
        actual[member] = value.clone();
    }
    actual
}

/// A timestamp member of a flow payload, in the epoch seconds restJson1 carries it as.
fn timestamp_of(case: &str, flow: &Value, member: &str) -> f64 {
    flow.get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case}: `{member}` must be a timestamp: {flow}"))
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
        let expected_sources = expected_sources(
            row.request
                .get("sources")
                .unwrap_or_else(|| panic!("case {case}: the request carries sources")),
        );
        assert_eq!(
            read.get("sources"),
            Some(&expected_sources),
            "case {case}: the stored sources must be the requested ones, got {read}"
        );
        assert_eq!(
            read.get("transformation"),
            row.request.get("transformation"),
            "case {case}: the stored transformation must be the requested one, got {read}"
        );
        let expected_target = expected_target(
            row.request
                .get("target")
                .unwrap_or_else(|| panic!("case {case}: the request carries a target")),
        );
        assert_eq!(
            read.get("target"),
            Some(&expected_target),
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

/// One row of the projection table: the flow's name, the request that defines it, and
/// whether each end of its pipeline names a dataset.
struct ProjectionRow {
    case: &'static str,
    name: &'static str,
    request: Value,
    /// Whether `sources[0]` and `target` describe a dataset end, which is what carries the
    /// dataset's identity.
    source_names_dataset: bool,
    target_names_dataset: bool,
}

/// RULE: a flow read reports the whole definition the flow carries — not only its pipeline
/// configuration and timestamps, but who manages it, what triggers it, and for each end that
/// reads or writes a data-lake dataset the identity of that dataset: the id the dataset owns
/// and whether the lake or the customer defines it.
///
/// The rows differ along what the rule generalises over: which ends name datasets (both, the
/// source only, the target only, neither), the namespace the dataset lives in (the service's
/// own `asc`, the pre-defined `default`, one the caller created), whether the lake holds the
/// dataset the end names, and whether the end carries the optional `options` member.
#[tokio::test]
async fn a_flow_reports_how_it_is_managed_triggered_and_which_datasets_it_moves() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("projection").await;
    emulator
        .given_namespace("projection", &instance_id, "supply_custom")
        .await;

    // The datasets the lake HOLDS, each by the identifier a flow end names it by. A row
    // below also names one the lake does not hold, which is the end that has no dataset to
    // take an id from.
    let mut held: BTreeSet<String> = BTreeSet::new();
    for (namespace, name) in [
        ("default", "product"),
        ("asc", "inbound_order"),
        ("supply_custom", "lines"),
    ] {
        held.insert(
            emulator
                .given_dataset("projection", &instance_id, namespace, name)
                .await,
        );
    }

    let rows = vec![
        ProjectionRow {
            case: "a dataset of a pre-defined customer namespace read into one of the \
                   service's own",            name: "dataset-to-dataset",
            request: json!({
                "sources": [dataset_source(
                    "product",
                    &dataset_identifier(&instance_id, "default", "product"),
                    None,
                )],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "DATASET",
                    "datasetTarget": {
                        "datasetIdentifier":
                            dataset_identifier(&instance_id, "asc", "inbound_order"),
                    },
                },
            }),
            source_names_dataset: true,
            target_names_dataset: true,
        },
        ProjectionRow {
            case: "a dataset of a namespace the caller created, with every option, written \
                   to S3",
            name: "custom-namespace-to-s3",
            request: json!({
                "sources": [dataset_source(
                    "lines",
                    &dataset_identifier(&instance_id, "supply_custom", "lines"),
                    Some(json!({ "loadType": "INCREMENTAL", "dedupeRecords": false })),
                )],
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": { "query": "SELECT * FROM lines" },
                },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "lines/" },
                },
            }),
            source_names_dataset: true,
            target_names_dataset: false,
        },
        ProjectionRow {
            case: "S3 read into a dataset of the service's own namespace",
            name: "s3-to-dataset",
            request: json!({
                "sources": [s3_source("orders", "supply-landing", "orders/", Some("CSV"))],
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": { "query": "SELECT * FROM orders" },
                },
                "target": {
                    "targetType": "DATASET",
                    "datasetTarget": {
                        "datasetIdentifier":
                            dataset_identifier(&instance_id, "asc", "shipment"),
                        "options": { "loadType": "REPLACE" },
                    },
                },
            }),
            source_names_dataset: false,
            target_names_dataset: true,
        },
        ProjectionRow {
            case: "no dataset at either end",
            name: "s3-to-s3",
            request: json!({
                "sources": [s3_source("raw", "supply-landing", "raw/", None)],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "raw/" },
                },
            }),
            source_names_dataset: false,
            target_names_dataset: false,
        },
    ];

    // identifier -> the id the read reported for that dataset, collected across the rows so
    // the invariants a minted id has (one per dataset, the same one every time) are asserted
    // over all of them.
    let mut reported_ids: BTreeMap<String, String> = BTreeMap::new();

    for row in rows {
        let case = row.case;
        let (status, body) = emulator
            .create_flow(&instance_id, row.name, row.request.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateDataIntegrationFlow expected 200, got {status} with {body}"
        );

        let (status, body) = emulator.get_flow(&instance_id, row.name).await;
        assert_eq!(
            status, 200,
            "case {case}: GetDataIntegrationFlow expected 200, got {status} with {body}"
        );
        let read = flow_of(case, &body);

        assert_eq!(
            read.get("managedBy").and_then(Value::as_str),
            Some(EXPECTED_MANAGED_BY),
            "case {case}: a flow a caller created is the caller's, got {read}"
        );
        assert_eq!(
            read.get("triggerOptions"),
            Some(&expected_trigger_options()),
            "case {case}: a flow with no schedule runs when its source changes, got {read}"
        );

        // Each dataset end carries the identity of the dataset it moves: the id that
        // dataset owns, and the kind of dataset it is. An S3 end has no dataset and so
        // carries neither member, and an end naming a dataset the lake does not hold has no
        // id to report.
        let source = read
            .get("sources")
            .and_then(|sources| sources.get(0))
            .cloned()
            .unwrap_or_else(|| panic!("case {case}: the flow reports no source: {read}"));
        let target = read
            .get("target")
            .cloned()
            .unwrap_or_else(|| panic!("case {case}: the flow reports no target: {read}"));
        for (end_case, end, member, names_dataset) in [
            (
                "source",
                &source,
                "datasetSource",
                row.source_names_dataset,
            ),
            (
                "target",
                &target,
                "datasetTarget",
                row.target_names_dataset,
            ),
        ] {
            match (names_dataset, end.get(member)) {
                (true, Some(configuration)) => {
                    let identifier = string_of(case, configuration, "datasetIdentifier");
                    assert_eq!(
                        configuration.get("datasetType").and_then(Value::as_str),
                        Some(expected_dataset_type(&identifier)),
                        "case {case}, {end_case}: the end must report the kind of dataset it \
                         moves, got {configuration}"
                    );
                    let reported = configuration
                        .get("datasetId")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    match reported {
                        Some(dataset_id) => {
                            assert!(
                                held.contains(&identifier),
                                "case {case}, {end_case}: an end naming a dataset the lake does \
                                 not hold must report no id, got {configuration}"
                            );
                            assert!(
                                is_modelled_uuid(&dataset_id),
                                "case {case}, {end_case}: the dataset's id must be of the \
                                 modelled UUID shape, got {dataset_id:?}"
                            );
                            // One dataset keeps ONE id, however many ends and flows name it.
                            if let Some(seen) = reported_ids.get(&identifier) {
                                assert_eq!(
                                    seen, &dataset_id,
                                    "case {case}, {end_case}: one dataset must keep one id, got \
                                     {dataset_id:?}"
                                );
                            } else {
                                reported_ids.insert(identifier.clone(), dataset_id);
                            }
                        }
                        None => assert!(
                            !held.contains(&identifier),
                            "case {case}, {end_case}: an end naming a dataset the lake holds \
                             must report that dataset's id, got {configuration}"
                        ),
                    }
                }
                (true, None) => panic!(
                    "case {case}, {end_case}: the flow reports no `{member}`, got {end}"
                ),
                (false, configuration) => assert!(
                    configuration.is_none(),
                    "case {case}, {end_case}: an end that names no dataset reports no \
                     `{member}`, got {end}"
                ),
            }
        }

        // The projection is what the stored record says, so reading again says the same.
        let (status, body) = emulator.get_flow(&instance_id, row.name).await;
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

    // Every dataset the lake holds and a row named was reported with an id, and no two of
    // them share one.
    assert_eq!(
        reported_ids.len(),
        held.len(),
        "every held dataset a row named must have been reported with its id, got \
         {reported_ids:?}"
    );
    let distinct: BTreeSet<&String> = reported_ids.values().collect();
    assert_eq!(
        distinct.len(),
        reported_ids.len(),
        "two datasets must not share one id, got {reported_ids:?}"
    );
}

/// RULE: `UpdateDataIntegrationFlow` writes the ends of an EXISTING flow's pipeline that
/// the request carries — `sources`, `transformation` and `target`, each replaced whole —
/// leaves every end the request omits as it was, answers with the stored record, and moves
/// `lastModifiedTime` while `createdTime` stays the create's.
#[tokio::test]
async fn an_update_writes_the_ends_it_carries_and_leaves_the_rest() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("update").await;

    let sql = json!({
        "transformationType": "SQL",
        "sqlTransformation": { "query": "SELECT * FROM orders" },
    });
    let s3_target_value = json!({
        "targetType": "S3",
        "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
    });
    let dataset_target_value = json!({
        "targetType": "DATASET",
        "datasetTarget": {
            "datasetIdentifier": dataset_identifier(&instance_id, "default", "product"),
        },
    });
    let base = json!({
        "sources": [s3_source("orders", "supply-landing", "orders/", None)],
        "transformation": sql.clone(),
        "target": s3_target_value.clone(),
    });

    // A flow of the same instance that no row addresses: whatever an update writes, it
    // writes it to the ONE pair it is addressed at.
    let bystander = "bystander";
    let (status, body) = emulator
        .create_flow(&instance_id, bystander, base.clone())
        .await;
    assert_eq!(
        status, 200,
        "case bystander: the untouched flow must be created, got {status} with {body}"
    );
    let (_, body) = emulator.get_flow(&instance_id, bystander).await;
    let bystander_before = flow_of("bystander", &body);

    // Rows spanning what the rule generalises over: which ends the request carries (one,
    // another, all three, none), the source count (1, 2 and the modelled maximum of 40),
    // and both modelled values of every `…Type` enum in either direction.
    let rows = vec![
        UpdateRow {
            case: "the transformation alone, SQL replaced by another query",
            base: base.clone(),
            update: json!({
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": { "query": "SELECT id, total FROM orders" },
                },
            }),
        },
        UpdateRow {
            case: "the transformation alone, SQL replaced by NONE",
            base: base.clone(),
            update: json!({ "transformation": { "transformationType": "NONE" } }),
        },
        UpdateRow {
            case: "the transformation alone, NONE replaced by SQL",
            base: json!({
                "sources": [s3_source("orders", "supply-landing", "orders/", None)],
                "transformation": { "transformationType": "NONE" },
                "target": s3_target_value.clone(),
            }),
            update: json!({ "transformation": sql.clone() }),
        },
        UpdateRow {
            case: "the sources alone, one S3 source replaced by two of both types",
            base: base.clone(),
            update: json!({
                "sources": [
                    s3_source("raw", "supply-landing", "raw/", None),
                    dataset_source(
                        "enriched",
                        &dataset_identifier(&instance_id, "default", "enriched"),
                        None,
                    ),
                ],
            }),
        },
        UpdateRow {
            case: "the sources alone, replaced by the modelled maximum of 40",
            base: base.clone(),
            update: json!({
                "sources": (0..40)
                    .map(|index| s3_source(
                        &format!("source_{index}"),
                        "supply-landing",
                        &format!("part-{index}/"),
                        None,
                    ))
                    .collect::<Vec<Value>>(),
            }),
        },
        UpdateRow {
            case: "the target alone, S3 replaced by DATASET",
            base: base.clone(),
            update: json!({ "target": dataset_target_value.clone() }),
        },
        UpdateRow {
            case: "the target alone, DATASET replaced by S3",
            base: json!({
                "sources": [dataset_source(
                    "product",
                    &dataset_identifier(&instance_id, "default", "product"),
                    None,
                )],
                "transformation": { "transformationType": "NONE" },
                "target": dataset_target_value.clone(),
            }),
            update: json!({ "target": s3_target_value.clone() }),
        },
        UpdateRow {
            case: "every end at once",
            base: base.clone(),
            update: json!({
                "sources": [dataset_source(
                    "product",
                    &dataset_identifier(&instance_id, "default", "product"),
                    // The last entry of the model's own load-type catalog
                    // (`DATA_INTEGRATION_FLOW_LOAD_TYPE_VALUES`), so the row does not ride
                    // on its first value.
                    Some(json!({
                        "loadType": DATA_INTEGRATION_FLOW_LOAD_TYPE_VALUES
                            .last()
                            .expect("the model declares at least one load type"),
                    })),
                )],
                "transformation": { "transformationType": "NONE" },
                "target": dataset_target_value.clone(),
            }),
        },
        UpdateRow {
            case: "no end at all, which asks for no change",
            base: base.clone(),
            update: json!({}),
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let case = row.case;
        let name = format!("update-{index}");

        let (status, body) = emulator
            .create_flow(&instance_id, &name, row.base.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the flow to update must be created, got {status} with {body}"
        );
        let (status, body) = emulator.get_flow(&instance_id, &name).await;
        assert_eq!(
            status, 200,
            "case {case}: the created flow must be readable, got {status} with {body}"
        );
        let before = flow_of(case, &body);
        let created_time = timestamp_of(case, &before, "createdTime");

        let (status, body) = emulator
            .update_flow(&instance_id, &name, row.update.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {case}: UpdateDataIntegrationFlow expected 200, got {status} with {body}"
        );
        let updated = flow_of(case, &body);

        assert_eq!(
            string_of(case, &updated, "instanceId"),
            instance_id,
            "case {case}: the updated flow must report its own instance, got {updated}"
        );
        assert_eq!(
            string_of(case, &updated, "name"),
            name,
            "case {case}: the updated flow must report its own name, got {updated}"
        );
        assert_eq!(
            configuration_of(case, &updated),
            expected_configuration(&row.base, &row.update),
            "case {case}: the update must write the ends it carries and leave the rest, got \
             {updated}"
        );
        // What an update never touches: who manages the flow and what triggers it, which
        // belong to the flow rather than to its pipeline.
        for member in ["managedBy", "triggerOptions"] {
            assert_eq!(
                updated.get(member),
                before.get(member),
                "case {case}: an update must leave `{member}` as it was, got {updated}"
            );
        }
        assert_eq!(
            timestamp_of(case, &updated, "createdTime"),
            created_time,
            "case {case}: createdTime belongs to the create and must not move, got {updated}"
        );
        assert!(
            timestamp_of(case, &updated, "lastModifiedTime") >= created_time,
            "case {case}: lastModifiedTime must be the time of the write, got {updated}"
        );

        // The response is the stored record, so the read that follows answers with exactly
        // it — the update persisted, rather than being reported and dropped.
        let (status, body) = emulator.get_flow(&instance_id, &name).await;
        assert_eq!(
            status, 200,
            "case {case}: the updated flow must be readable, got {status} with {body}"
        );
        assert_eq!(
            flow_of(case, &body),
            updated,
            "case {case}: the read must answer with what the update stored, got {body}"
        );
    }

    let (status, body) = emulator.get_flow(&instance_id, bystander).await;
    assert_eq!(
        status, 200,
        "case bystander: the untouched flow must still be readable, got {status} with {body}"
    );
    assert_eq!(
        flow_of("bystander", &body),
        bystander_before,
        "case bystander: an update must change only the pair it addresses, got {body}"
    );
}

/// RULE: an update addresses a flow that already exists, so a pair the store holds none
/// for is refused — a name the instance holds no flow under (including one a delete has
/// taken) with `ResourceNotFoundException` (404) carrying the not-found sentence, and an
/// `instanceId` this account does not hold with the gateway's authorization failure (403)
/// — and neither creates anything.
#[tokio::test]
async fn an_update_of_a_flow_that_does_not_exist_is_refused() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("update refusal").await;
    let request = json!({ "transformation": { "transformationType": "NONE" } });
    let stored = "stored-flow";

    let (status, body) = emulator
        .create_flow(
            &instance_id,
            stored,
            json!({
                "sources": [s3_source("orders", "supply-landing", "orders/", None)],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
                },
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case stored: the flow to address must be created, got {status} with {body}"
    );

    // Names this instance holds no flow under, differing in how close they are to the one
    // it does hold.
    for (case, name) in [
        ("a name never created", "never-created"),
        ("one character off a stored name", "stored-flows"),
        ("the stored name in another case", "Stored-Flow"),
        ("a single character", "q"),
    ] {
        let (status, body) = emulator
            .update_flow(&instance_id, name, request.clone())
            .await;
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
            expected_not_found_sentence(name, &instance_id),
            "case {case}: the refusal must name the flow and its instance, got {body}"
        );

        let (status, body) = emulator.get_flow(&instance_id, name).await;
        assert_eq!(
            status, 404,
            "case {case}: a refused update must create nothing, got {status} with {body}"
        );
    }

    // An `instanceId` of the modelled shape that names no instance of this account is the
    // gateway's authorization failure, whatever name the update addresses; one outside the
    // modelled UUID shape is refused before any handler runs.
    for (case, instance, expected) in [
        (
            "the all-zero UUID, which no instance holds",
            "00000000-0000-0000-0000-000000000000".to_string(),
            403,
        ),
        (
            "a UUID that no instance holds, addressing a name one does",
            "00000000-0000-4000-8000-000000000000".to_string(),
            403,
        ),
        (
            "an instanceId outside the modelled UUID shape",
            "not-a-uuid".to_string(),
            400,
        ),
    ] {
        let (status, body) = emulator
            .update_flow(&instance, stored, request.clone())
            .await;
        assert_eq!(
            status, expected,
            "case {case}: expected {expected}, got {status} with {body}"
        );
        if expected == 403 {
            assert!(
                error_type(&body).contains("AccessDeniedException"),
                "case {case}: expected an AccessDeniedException envelope, got {body}"
            );
            assert_eq!(
                error_message(&body),
                expected_authorization_failure(&instance),
                "case {case}: the refusal must carry the gateway's authorization sentence, got \
                 {body}"
            );
        }
    }

    // A flow a delete has taken is a flow the instance holds no name for, so the update
    // that follows the delete is the same not-found and resurrects nothing.
    let (status, body) = emulator.delete_flow(&instance_id, stored).await;
    assert_eq!(
        status, 200,
        "case deleted: the delete must be served, got {status} with {body}"
    );
    let (status, body) = emulator
        .update_flow(&instance_id, stored, request.clone())
        .await;
    assert_eq!(
        status, 404,
        "case deleted: expected 404, got {status} with {body}"
    );
    assert_eq!(
        error_message(&body),
        expected_not_found_sentence(stored, &instance_id),
        "case deleted: the refusal must name the flow and its instance, got {body}"
    );
    let (status, body) = emulator.get_flow(&instance_id, stored).await;
    assert_eq!(
        status, 404,
        "case deleted: a refused update must bring nothing back, got {status} with {body}"
    );
}

/// RULE: the same discriminator rule the create applies holds for the configuration an
/// update would leave the flow with, so an update that declares a source, target or
/// transformation type and carries none of its configuration is refused
/// `ValidationException` (400) and the stored flow is left exactly as it was; the type that
/// is configured by nothing (`NONE`) is served.
#[tokio::test]
async fn an_update_declaring_a_type_without_its_configuration_is_refused() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("update configuration").await;
    let base = json!({
        "sources": [s3_source("orders", "supply-landing", "orders/", None)],
        "transformation": {
            "transformationType": "SQL",
            "sqlTransformation": { "query": "SELECT * FROM orders" },
        },
        "target": {
            "targetType": "S3",
            "s3Target": { "bucketName": "supply-exports", "prefix": "orders/" },
        },
    });

    // One row per (declared type, missing configuration member) pair, over every value of
    // the three modelled enums, plus the served `NONE` row.
    let rows: Vec<ConfigurationRow> = vec![
        ConfigurationRow {
            case: "S3 source without s3Source",
            request: json!({ "sources": [{ "sourceType": "S3", "sourceName": "orders" }] }),
            missing: Some(Missing {
                member: "s3Source",
                discriminator: "sourceType",
                value: "S3",
            }),
        },
        ConfigurationRow {
            case: "DATASET source without datasetSource",
            request: json!({ "sources": [{ "sourceType": "DATASET", "sourceName": "product" }] }),
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
            }),
            missing: Some(Missing {
                member: "datasetSource",
                discriminator: "sourceType",
                value: "DATASET",
            }),
        },
        ConfigurationRow {
            case: "S3 target without s3Target",
            request: json!({ "target": { "targetType": "S3" } }),
            missing: Some(Missing {
                member: "s3Target",
                discriminator: "targetType",
                value: "S3",
            }),
        },
        ConfigurationRow {
            case: "DATASET target without datasetTarget",
            request: json!({ "target": { "targetType": "DATASET" } }),
            missing: Some(Missing {
                member: "datasetTarget",
                discriminator: "targetType",
                value: "DATASET",
            }),
        },
        ConfigurationRow {
            case: "SQL transformation without sqlTransformation",
            request: json!({ "transformation": { "transformationType": "SQL" } }),
            missing: Some(Missing {
                member: "sqlTransformation",
                discriminator: "transformationType",
                value: "SQL",
            }),
        },
        ConfigurationRow {
            case: "NONE transformation, which is configured by nothing",
            request: json!({ "transformation": { "transformationType": "NONE" } }),
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
        let name = format!("update-configuration-{index}");
        let (status, body) = emulator
            .create_flow(&instance_id, &name, base.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the flow to update must be created, got {status} with {body}"
        );
        let (_, body) = emulator.get_flow(&instance_id, &name).await;
        let before = flow_of(case, &body);

        let (status, body) = emulator
            .update_flow(&instance_id, &name, row.request.clone())
            .await;
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
                    status, 200,
                    "case {case}: a refused update must leave the flow, got {status} with {body}"
                );
                assert_eq!(
                    flow_of(case, &body),
                    before,
                    "case {case}: a refused update must change nothing, got {body}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case}: expected 200, got {status} with {body}"
                );
                assert_eq!(
                    configuration_of(case, &flow_of(case, &body)),
                    expected_configuration(&base, &row.request),
                    "case {case}: the served update must write the end it carries, got {body}"
                );
            }
        }
    }
}

// --------------------------------------------------------------------------------------
// `ListDataIntegrationFlows`
//
// RULE under test: the listing reports exactly the data-integration flows the store holds
// for the addressed instance — each one as `GetDataIntegrationFlow` reports it — in one
// stable name order, cut into pages of at most `maxResults` with an opaque `nextToken` that
// resumes where the page stopped; an `instanceId` naming no instance of this account is an
// authorization failure, a `nextToken` the service did not issue for this query is a
// validation failure, and the listing changes nothing a later caller sees.
// --------------------------------------------------------------------------------------

/// The published bounds of `DataIntegrationFlowMaxResults` (`@range(min: 0, max: 20)`).
const FLOW_MAX_RESULTS_MIN: i64 = 0;
const FLOW_MAX_RESULTS_MAX: i64 = 20;

/// The `@default` the model gives `maxResults`, applied when a request omits it.
const FLOW_MAX_RESULTS_DEFAULT: usize = 10;

/// The URI the flow listing is addressed at, as the model's `http` trait spells it:
/// `/api/data-integration/instance/{instanceId}/data-integration-flows`.
fn flow_listing_path(instance_id: &str) -> String {
    format!("/api/data-integration/instance/{instance_id}/data-integration-flows")
}

/// `ListDataIntegrationFlows` for one instance, with `query` appended verbatim.
async fn list_flows(emulator: &Emulator, instance_id: &str, query: &str) -> (u16, Value) {
    let path = flow_listing_path(instance_id);
    let path = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };
    emulator.send("GET", &path, None).await
}

/// The `flows` member of a success body, as a list.
fn flows_of(case: &str, body: &Value) -> Vec<Value> {
    body.get("flows")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: response carries no `flows` list: {body}"))
}

/// The `name` of every entry of a success body's `flows`, in the order answered.
fn listed_flow_names(case: &str, body: &Value) -> Vec<String> {
    flows_of(case, body)
        .iter()
        .map(|flow| string_of(case, flow, "name"))
        .collect()
}

/// The `nextToken` a listing body carries, if any.
fn listing_next_token(body: &Value) -> Option<String> {
    body.get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// A distinct flow name of the modelled `DataIntegrationFlowName` shape (`^[A-Za-z0-9-]+$`),
/// built so that ascending `index` is ascending name.
fn listing_flow_name(index: usize) -> String {
    format!("flow-{index:03}")
}

/// The smallest create body the model admits: one S3 source, no transformation, an S3 target.
fn minimal_flow_request(slot: &str) -> Value {
    json!({
        "sources": [s3_source("orders", "supply-landing", &format!("{slot}/"), None)],
        "transformation": { "transformationType": "NONE" },
        "target": {
            "targetType": "S3",
            "s3Target": { "bucketName": "supply-exports", "prefix": format!("{slot}/") },
        },
    })
}

/// Walk every page of one listing and return the flow ENTRIES answered, in order, together
/// with how many pages it took. [`names_of`] is the name order of what it walked.
async fn walk_flow_pages(
    emulator: &Emulator,
    case: &str,
    instance_id: &str,
    max_results: Option<i64>,
) -> (Vec<Value>, usize) {
    let limit = match max_results {
        Some(size) => usize::try_from(size).expect("a page size a case asks for is not negative"),
        None => FLOW_MAX_RESULTS_DEFAULT,
    };
    let mut entries: Vec<Value> = Vec::new();
    let mut pages = 0;
    let mut token: Option<String> = None;
    // A listing of a bounded store terminates in a bounded number of pages; the cap is here
    // so a token that failed to advance fails the case instead of looping forever.
    while pages < 64 {
        let mut query: Vec<String> = Vec::new();
        if let Some(size) = max_results {
            query.push(format!("maxResults={size}"));
        }
        if let Some(value) = &token {
            query.push(format!("nextToken={value}"));
        }
        let (status, body) = list_flows(emulator, instance_id, &query.join("&")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: page {pages} expected 200, got {status} with {body}"
        );
        let page = flows_of(case, &body);
        assert!(
            page.len() <= limit,
            "case {case:?}: page {pages} holds {} flows, more than maxResults {limit}",
            page.len()
        );
        pages += 1;
        token = listing_next_token(&body);
        if token.is_some() {
            assert!(
                !page.is_empty(),
                "case {case:?}: page {pages} is empty yet carries a nextToken, so a client paging \
                 in a loop could never advance"
            );
        }
        entries.extend(page);
        if token.is_none() {
            return (entries, pages);
        }
    }
    panic!("case {case:?}: the listing never ran out of pages after {pages} pages");
}

/// The `name` of every entry of a walked listing, in the order answered.
fn names_of(case: &str, entries: &[Value]) -> Vec<String> {
    entries
        .iter()
        .map(|flow| string_of(case, flow, "name"))
        .collect()
}

/// RULE: the listing reports exactly the flows the store holds for the addressed instance,
/// in name order, each one exactly as `GetDataIntegrationFlow` reports it — and nothing of
/// another instance.
#[tokio::test]
async fn flow_listing_reports_exactly_the_stored_flows_of_the_addressed_instance() {
    let case = "the stored flows of one instance";
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance(case).await;
    let other_instance = emulator.create_instance(case).await;
    let empty_instance = emulator.create_instance(case).await;
    emulator
        .given_namespace(case, &instance_id, "logistics")
        .await;
    let dataset = emulator
        .given_dataset(case, &instance_id, "logistics", "orders")
        .await;

    // Rows differ along the name (the modelled one-character minimum, mixed case and
    // digits, a name sorting last, the 256-character maximum) and along the pipeline the
    // flow carries (an S3 end, a dataset end with every option, a SQL transformation and
    // tags) — deliberately created OUT of name order.
    let rows: Vec<(&str, String, Value)> = vec![
        (
            "single-character name, the modelled minimum",
            "a".to_string(),
            minimal_flow_request("a"),
        ),
        (
            "mixed case and digits, SQL into S3",
            "Zed-9".to_string(),
            json!({
                "sources": [s3_source("raw", "supply-landing", "raw/", Some("CSV"))],
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": { "query": "SELECT * FROM raw" },
                },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "zed/" },
                },
            }),
        ),
        (
            "a dataset source and a dataset target, tagged",
            "dataset-pipeline".to_string(),
            json!({
                "sources": [dataset_source(
                    "orders",
                    &dataset,
                    Some(json!({ "loadType": "INCREMENTAL", "dedupeRecords": false })),
                )],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "DATASET",
                    "datasetTarget": { "datasetIdentifier": dataset.clone() },
                },
                "tags": { "team": "supply" },
            }),
        ),
        (
            "name at the modelled 256-character maximum",
            // 256 characters of `^[A-Za-z0-9-]+$`, the shape's upper length bound.
            format!("f{}", "-A9".repeat(85)),
            minimal_flow_request("long"),
        ),
    ];

    let mut expected_names: Vec<String> = Vec::new();
    for (row_case, name, request) in &rows {
        let (status, body) = emulator
            .create_flow(&instance_id, name, request.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {row_case:?}: the flow to list must be created, got {status} with {body}"
        );
        // The same name under another instance, so a listing that leaked across the
        // instance boundary would report it twice.
        let (status, body) = emulator
            .create_flow(&other_instance, name, minimal_flow_request("other"))
            .await;
        assert_eq!(
            status, 200,
            "case {row_case:?}: the other instance's flow must be created, got {status} with \
             {body}"
        );
        expected_names.push(name.clone());
    }
    // One flow only the other instance holds: the addressed instance's listing must not
    // report it.
    let (status, body) = emulator
        .create_flow(&other_instance, "only-elsewhere", minimal_flow_request("x"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the foreign-only flow must be created, got {status} with {body}"
    );
    expected_names.sort();

    let (status, body) = list_flows(
        &emulator,
        &instance_id,
        &format!("maxResults={FLOW_MAX_RESULTS_MAX}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {body}"
    );
    assert_eq!(
        listed_flow_names(case, &body),
        expected_names,
        "case {case:?}: expected the stored flows in name order, got {body}"
    );
    assert!(
        listing_next_token(&body).is_none(),
        "case {case:?}: one page holds every flow, so it must carry no nextToken: {body}"
    );

    // Every listed flow is the record the single-flow read reports, member for member, bar
    // the trigger descriptor a listing entry does not carry: the listing projects persisted
    // state, not a summary of its own.
    for listed in flows_of(case, &body) {
        let name = string_of(case, &listed, "name");
        let (status, read) = emulator.get_flow(&instance_id, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDataIntegrationFlow {name} expected 200, got {status} with {read}"
        );
        assert_eq!(
            listed,
            expected_listing_entry(case, &read),
            "case {case:?}: the listed {name} must equal what GetDataIntegrationFlow reports, \
             bar the trigger descriptor"
        );
    }

    // An instance holding no flow is a successful empty page, never a refusal.
    let case = "an instance holding no flow";
    let (status, body) = list_flows(&emulator, &empty_instance, "").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {body}"
    );
    assert_eq!(
        listed_flow_names(case, &body),
        Vec::<String>::new(),
        "case {case:?}: an instance holding no flow is an empty page, got {body}"
    );
    assert!(
        listing_next_token(&body).is_none(),
        "case {case:?}: an empty page must carry no nextToken: {body}"
    );
}

/// RULE: a listing is cut into pages of at most `maxResults`, and the pages a client walks
/// with the tokens it is handed concatenate to the whole name order exactly once — whatever
/// the page size.
#[tokio::test]
async fn flow_pagination_walks_the_whole_order_exactly_once_at_every_page_size() {
    let case = "paging over one instance's flows";
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance(case).await;

    // 24 flows: more than the largest page (20) and more than the default (10), so every
    // row below needs at least two pages.
    const SEEDED: usize = 24;
    let mut all_names = Vec::new();
    for index in 1..=SEEDED {
        let name = listing_flow_name(index);
        let (status, body) = emulator
            .create_flow(&instance_id, &name, minimal_flow_request(&name))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: creating {name} expected 200, got {status} with {body}"
        );
        all_names.push(name);
    }
    all_names.sort();

    // Rows differ along the page size: the smallest page that can advance, sizes in between
    // (one of which divides the total exactly), the largest page the model publishes, and
    // the modelled default, which a row omits `maxResults` to get.
    let rows: Vec<(&str, Option<i64>)> = vec![
        ("one flow per page", Some(1)),
        ("two flows per page", Some(2)),
        ("eight flows per page", Some(8)),
        ("seven flows per page", Some(7)),
        ("the largest page", Some(FLOW_MAX_RESULTS_MAX)),
        ("the modelled default page", None),
    ];

    for (row_case, max_results) in rows {
        let (entries, pages) =
            walk_flow_pages(&emulator, row_case, &instance_id, max_results).await;
        assert_eq!(
            names_of(row_case, &entries),
            all_names,
            "case {row_case:?}: the pages must concatenate to the whole name order exactly once"
        );
        let size = match max_results {
            Some(size) => usize::try_from(size).expect("a page size a row asks for is positive"),
            None => FLOW_MAX_RESULTS_DEFAULT,
        };
        let expected_pages = all_names.len().div_ceil(size);
        assert_eq!(
            pages, expected_pages,
            "case {row_case:?}: {} flows in pages of {size} expected {expected_pages} pages",
            all_names.len()
        );
    }
}

/// RULE: `maxResults` is served at each bound the model publishes and refused outside them.
#[tokio::test]
async fn flow_listing_max_results_is_served_at_its_bounds_and_refused_outside_them() {
    let case = "the bounds of maxResults";
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance(case).await;
    const SEEDED: usize = 3;
    for index in 1..=SEEDED {
        let name = listing_flow_name(index);
        let (status, body) = emulator
            .create_flow(&instance_id, &name, minimal_flow_request(&name))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: creating {name} expected 200, got {status} with {body}"
        );
    }

    // Rows: both published bounds of `DataIntegrationFlowMaxResults`, and one past each.
    let rows: Vec<(&str, i64, u16)> = vec![
        ("one below the minimum", FLOW_MAX_RESULTS_MIN - 1, 400),
        ("the minimum", FLOW_MAX_RESULTS_MIN, 200),
        ("the maximum", FLOW_MAX_RESULTS_MAX, 200),
        ("one past the maximum", FLOW_MAX_RESULTS_MAX + 1, 400),
    ];

    for (row_case, max_results, expected_status) in rows {
        let (status, body) =
            list_flows(&emulator, &instance_id, &format!("maxResults={max_results}")).await;
        assert_eq!(
            status, expected_status,
            "case {row_case:?}: maxResults {max_results} expected HTTP {expected_status}, got \
             {status} with {body}"
        );
        if expected_status == 400 {
            assert!(
                error_type(&body).contains("ValidationException"),
                "case {row_case:?}: expected the modelled ValidationException, got {body}"
            );
            continue;
        }
        let listed = listed_flow_names(row_case, &body);
        let expected = usize::try_from(max_results)
            .expect("a served bound is not negative")
            .min(SEEDED);
        assert_eq!(
            listed.len(),
            expected,
            "case {row_case:?}: expected {expected} flows on the page, got {listed:?}"
        );
        if expected == 0 {
            // A page that holds nothing carries no token: a token a client cannot advance
            // with would make it page forever.
            assert!(
                listing_next_token(&body).is_none(),
                "case {row_case:?}: a page holding nothing must carry no nextToken: {body}"
            );
        }
    }
}

/// RULE: only a `nextToken` this service issued for THIS query is a page boundary; anything
/// else is a validation failure rather than a silently truncated or restarted page.
#[tokio::test]
async fn a_flow_listing_next_token_this_service_did_not_issue_is_refused() {
    let case = "a nextToken this service did not issue";
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance(case).await;
    let other_instance = emulator.create_instance(case).await;
    for index in 1..=3 {
        let name = listing_flow_name(index);
        for host in [&instance_id, &other_instance] {
            let (status, body) = emulator
                .create_flow(host, &name, minimal_flow_request(&name))
                .await;
            assert_eq!(
                status, 200,
                "case {case:?}: creating {name} expected 200, got {status} with {body}"
            );
        }
    }

    // The token this service hands out for a query is the control every forged row below is
    // a variation on: it names the instance and the flow to resume at.
    let issued = |host: &str, name: &str| {
        hex::encode(format!("scn:1:data-integration-flow:{host}:{name}"))
    };

    // Rows differ along HOW the value fails to be a token this service issued for this
    // query: not hex at all, hex of a foreign payload, hex of a bare store key with no
    // stamp, the stamp over something no `DataIntegrationFlowName` can be, the stamp of
    // another listing of this service, and a token issued for another instance.
    let rows: Vec<(&str, String)> = vec![
        ("not hex", "not-a-token".to_string()),
        (
            "hex of a foreign payload",
            hex::encode("someone-elses-cursor"),
        ),
        (
            "hex of a bare store key",
            hex::encode(listing_flow_name(2)),
        ),
        (
            "the stamp over a name the model cannot admit",
            issued(&instance_id, "not a flow name"),
        ),
        (
            "the stamp of another listing of this service",
            hex::encode(format!(
                "scn:1:data-lake-dataset:{instance_id}:default:{}",
                listing_flow_name(2)
            )),
        ),
        (
            "a token issued for another instance",
            issued(&other_instance, &listing_flow_name(2)),
        ),
    ];

    for (row_case, token) in rows {
        let (status, body) =
            list_flows(&emulator, &instance_id, &format!("nextToken={token}")).await;
        assert_eq!(
            status, 400,
            "case {row_case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert!(
            error_type(&body).contains("ValidationException"),
            "case {row_case:?}: expected the modelled ValidationException, got {body}"
        );
    }

    // The control: the token the service itself hands out resumes the listing where the
    // page stopped.
    let case = "the token the service issued";
    let (status, first) = list_flows(&emulator, &instance_id, "maxResults=1").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {first}"
    );
    let token = listing_next_token(&first)
        .unwrap_or_else(|| panic!("case {case:?}: a truncated page must carry a nextToken"));
    let (status, second) = list_flows(
        &emulator,
        &instance_id,
        &format!("maxResults=1&nextToken={token}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {second}"
    );
    assert_eq!(
        listed_flow_names(case, &second),
        vec![listing_flow_name(2)],
        "case {case:?}: the token must resume at the flow the first page stopped at"
    );

    // ...and that same token is refused once the query it was cut from changes.
    let case = "the issued token against another instance";
    let (status, body) = list_flows(
        &emulator,
        &other_instance,
        &format!("maxResults=1&nextToken={token}"),
    )
    .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body}"
    );
    assert!(
        error_type(&body).contains("ValidationException"),
        "case {case:?}: expected the modelled ValidationException, got {body}"
    );
}

/// RULE: a listing is answered only for a live instance of this account — an `instanceId`
/// naming none is the gateway's authorization failure, whatever else the request asks for.
#[tokio::test]
async fn a_flow_listing_for_an_unknown_instance_is_an_authorization_failure() {
    let emulator = Emulator::new();

    // Rows differ along what else the request carries, so the authorization step is shown
    // to run before the page size and the token are read at all: an `instanceId` of the
    // modelled `UUID` shape that names no instance, with no query, with the largest page
    // the model publishes, and with a token. (A page size OUTSIDE the modelled range is
    // refused by the modelled constraint before any handler runs, so it is not a row here.)
    let unknown_instance = "00000000-0000-4000-8000-000000000000";
    let rows: Vec<(&str, String)> = vec![
        ("no query at all", String::new()),
        (
            "the largest modelled page",
            format!("maxResults={FLOW_MAX_RESULTS_MAX}"),
        ),
        (
            "the smallest modelled page",
            format!("maxResults={FLOW_MAX_RESULTS_MIN}"),
        ),
        (
            "a token of another instance's listing",
            format!(
                "nextToken={}",
                hex::encode(format!(
                    "scn:1:data-integration-flow:{unknown_instance}:{}",
                    listing_flow_name(1)
                ))
            ),
        ),
    ];

    for (case, query) in rows {
        let (status, body) = list_flows(&emulator, unknown_instance, &query).await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected HTTP 403, got {status} with {body}"
        );
        assert!(
            error_type(&body).contains("AccessDeniedException"),
            "case {case:?}: expected the modelled AccessDeniedException, got {body}"
        );
        assert_eq!(
            error_message(&body),
            expected_authorization_failure(unknown_instance),
            "case {case:?}: the refusal must carry the gateway's authorization sentence"
        );
    }
}

/// RULE: the listing is a report of live state — it changes nothing a later caller sees, and
/// it answers from the store as every write left it.
#[tokio::test]
async fn flow_listing_reports_live_state_and_changes_nothing() {
    let case = "listing is a report";
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance(case).await;
    const SEEDED: usize = 5;
    let mut names: Vec<String> = Vec::new();
    for index in 1..=SEEDED {
        let name = listing_flow_name(index);
        let (status, body) = emulator
            .create_flow(&instance_id, &name, minimal_flow_request(&name))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: creating {name} expected 200, got {status} with {body}"
        );
        names.push(name);
    }
    names.sort();

    let (first, _) = walk_flow_pages(&emulator, case, &instance_id, Some(2)).await;
    let (second, _) = walk_flow_pages(&emulator, case, &instance_id, Some(2)).await;
    assert_eq!(
        first, second,
        "case {case:?}: two identical listings must answer identically, entry for entry"
    );
    assert_eq!(
        names_of(case, &first),
        names,
        "case {case:?}: the listing must report every stored flow"
    );

    let case = "a listing at another page size";
    let (whole, pages) =
        walk_flow_pages(&emulator, case, &instance_id, Some(FLOW_MAX_RESULTS_MAX)).await;
    assert_eq!(
        whole, first,
        "case {case:?}: the page size must not change which flows are reported"
    );
    assert_eq!(
        pages, 1,
        "case {case:?}: {SEEDED} flows fit in one page of {FLOW_MAX_RESULTS_MAX}"
    );

    // ...and the single-flow read still reports each of them after the listing.
    for name in &names {
        let (status, body) = emulator.get_flow(&instance_id, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDataIntegrationFlow {name} after listing expected 200, got \
             {status} with {body}"
        );
    }

    // A write between two listings is what the next listing reports: the update's new
    // pipeline, and the deleted flow gone from the order.
    let case = "a listing after an update and a delete";
    let updated = names[1].clone();
    let (status, body) = emulator
        .update_flow(
            &instance_id,
            &updated,
            json!({
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": { "query": "SELECT * FROM orders" },
                },
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the update expected 200, got {status} with {body}"
    );
    let removed = names[0].clone();
    let (status, body) = emulator.delete_flow(&instance_id, &removed).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the delete expected 200, got {status} with {body}"
    );

    let (status, body) = list_flows(
        &emulator,
        &instance_id,
        &format!("maxResults={FLOW_MAX_RESULTS_MAX}"),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {body}"
    );
    let listed = listed_flow_names(case, &body);
    let expected: Vec<String> = names
        .iter()
        .filter(|name| **name != removed)
        .cloned()
        .collect();
    assert_eq!(
        listed, expected,
        "case {case:?}: the listing must report the flows the writes left, got {body}"
    );
    for flow in flows_of(case, &body) {
        let name = string_of(case, &flow, "name");
        let (status, read) = emulator.get_flow(&instance_id, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDataIntegrationFlow {name} expected 200, got {status} with {read}"
        );
        assert_eq!(
            flow,
            expected_listing_entry(case, &read),
            "case {case:?}: the listed {name} must equal what the single-flow read reports, bar \
             the trigger descriptor"
        );
    }
}

/// RULE: a member the model does not declare is reported by exactly the reads whose recorded
/// response carries it — the trigger descriptor is reported by the single-flow read and by no
/// entry of the listing, which reports the same projection of the same stored flow (the
/// modelled members plus `managedBy`) and nothing else, whatever the flow's pipeline, however
/// the stored flow was last written, and at whatever page size the listing is cut.
#[tokio::test]
async fn flow_listing_entries_report_no_trigger_descriptor_whatever_the_flow() {
    let case = "the projection of a listing entry";
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance(case).await;
    emulator
        .given_namespace(case, &instance_id, "logistics")
        .await;
    let dataset = emulator
        .given_dataset(case, &instance_id, "logistics", "orders")
        .await;

    // Rows differ along what the rule generalises over: the source type and how many sources
    // the pipeline has, the transformation, the target type, and whether the stored flow was
    // last written by the create or by an update.
    let rows: Vec<(&str, &str, Value, Option<Value>)> = vec![
        (
            "one S3 source, no transformation, an S3 target",
            "s3-passthrough",
            minimal_flow_request("s3"),
            None,
        ),
        (
            "two S3 sources and a SQL transformation",
            "sql-fan-in",
            json!({
                "sources": [
                    s3_source("raw", "supply-landing", "raw/", Some("CSV")),
                    s3_source("late", "supply-landing", "late/", Some("PARQUET")),
                ],
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": { "query": "SELECT * FROM raw UNION ALL SELECT * FROM late" },
                },
                "target": {
                    "targetType": "S3",
                    "s3Target": { "bucketName": "supply-exports", "prefix": "sql/" },
                },
            }),
            None,
        ),
        (
            "a dataset source into a dataset target",
            "dataset-to-dataset",
            json!({
                "sources": [dataset_source(
                    "orders",
                    &dataset,
                    Some(json!({ "loadType": "INCREMENTAL", "dedupeRecords": true })),
                )],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "DATASET",
                    "datasetTarget": { "datasetIdentifier": dataset.clone() },
                },
            }),
            None,
        ),
        (
            "an S3 source into a dataset target, rewritten by an update",
            "updated-pipeline",
            json!({
                "sources": [s3_source("orders", "supply-landing", "orders/", None)],
                "transformation": { "transformationType": "NONE" },
                "target": {
                    "targetType": "DATASET",
                    "datasetTarget": { "datasetIdentifier": dataset.clone() },
                },
            }),
            Some(json!({
                "transformation": {
                    "transformationType": "SQL",
                    "sqlTransformation": { "query": "SELECT id FROM orders" },
                },
            })),
        ),
    ];

    for (row_case, name, request, update) in &rows {
        let (status, body) = emulator
            .create_flow(&instance_id, name, request.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {row_case:?}: the flow to list must be created, got {status} with {body}"
        );
        if let Some(update) = update {
            let (status, body) = emulator.update_flow(&instance_id, name, update.clone()).await;
            assert_eq!(
                status, 200,
                "case {row_case:?}: the update must be applied, got {status} with {body}"
            );
        }
    }

    // The page size changes nothing about an entry's members: the projection is the same
    // whether a page holds one flow or every flow.
    let page_sizes: Vec<(&str, Option<i64>)> = vec![
        ("one flow per page", Some(1)),
        ("two flows per page", Some(2)),
        ("the largest modelled page", Some(FLOW_MAX_RESULTS_MAX)),
        ("the modelled default page", None),
    ];

    for (size_case, max_results) in page_sizes {
        let (entries, _) =
            walk_flow_pages(&emulator, size_case, &instance_id, max_results).await;
        assert_eq!(
            entries.len(),
            rows.len(),
            "case {size_case:?}: the listing must report every stored flow, got {entries:?}"
        );

        for entry in entries {
            let name = string_of(size_case, &entry, "name");
            let case = format!("{size_case}, the entry for {name}");

            // The member the model declares nowhere and a recorded listing entry does not
            // carry: the trigger descriptor.
            assert_eq!(
                entry.get("triggerOptions"),
                None,
                "case {case:?}: a listing entry carries no trigger descriptor, got {entry}"
            );

            // ...and no other member beyond the ones a listing entry does carry.
            let mut reported: Vec<&str> = entry
                .as_object()
                .unwrap_or_else(|| panic!("case {case:?}: an entry is an object, got {entry}"))
                .keys()
                .map(String::as_str)
                .collect();
            reported.sort_unstable();
            let mut expected: Vec<&str> = EXPECTED_LISTING_ENTRY_MEMBERS.to_vec();
            expected.sort_unstable();
            assert_eq!(
                reported, expected,
                "case {case:?}: expected exactly the members a listing entry carries, got {entry}"
            );
            assert_eq!(
                entry.get("managedBy").and_then(Value::as_str),
                Some(EXPECTED_MANAGED_BY),
                "case {case:?}: a flow a caller created is the caller's, got {entry}"
            );

            // The SINGLE-flow read of the same flow does report the trigger descriptor, and
            // agrees with the entry on everything else: one stored record, two projections.
            let (status, read) = emulator.get_flow(&instance_id, &name).await;
            assert_eq!(
                status, 200,
                "case {case:?}: GetDataIntegrationFlow expected 200, got {status} with {read}"
            );
            assert_eq!(
                flow_of(&case, &read).get("triggerOptions"),
                Some(&expected_trigger_options()),
                "case {case:?}: the single-flow read reports how the flow is triggered, got \
                 {read}"
            );
            assert_eq!(
                entry,
                expected_listing_entry(&case, &read),
                "case {case:?}: the entry must be the read's projection without the trigger \
                 descriptor"
            );
        }
    }
}
