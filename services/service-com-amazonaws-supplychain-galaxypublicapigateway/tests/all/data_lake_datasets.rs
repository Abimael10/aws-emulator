// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of the AWS Supply Chain **data-lake dataset**, driven over the
//! wire (restJson1, signing name `scn`) through the same `MultiServiceRouter` the
//! emulator's front door uses — so every assertion here is about what a real client
//! observes, not about a handler's internals.
//!
//! RULE under test: `CreateDataLakeDataset` persists one dataset per
//! (`instanceId`, `namespace`, `name`) triple of an existing instance and one of its
//! namespaces, and every member of the dataset it answers with (`arn`, `schema`,
//! `description`, `partitionSpec`, the timestamps) is read back unchanged by
//! `GetDataLakeDataset`; a triple that is already taken is a conflict, an instance or
//! namespace that does not exist is a not-found, and a create carrying no `schema` is a
//! validation failure that persists nothing.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::ops::create_data_lake_dataset::SCHEMA_REQUIRED_MESSAGE;
use service_com_amazonaws_supplychain_galaxypublicapigateway::ops::types::{
    DATA_LAKE_DATASET_PARTITION_TRANSFORM_TYPE_VALUES, DATA_LAKE_DATASET_SCHEMA_FIELD_TYPE_VALUES,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::state::{
    MAX_DATASETS_PER_INSTANCE, PREDEFINED_NAMESPACES,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these tests sign for unless a case names another one.
const REGION: &str = "us-east-1";

/// How long a poll loop waits for the condition it is about before giving up, and how long
/// it waits between looks.
///
/// Generous on purpose: the loop ends as soon as the condition holds, so the deadline is
/// only there to end a case that can never satisfy it and never a budget the host has to
/// beat.
const POLL_DEADLINE: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The length a dataset description is bounded by, written out HERE rather than read from
/// the crate so a drift in the enforced bound fails this suite instead of travelling with
/// it. SOURCE: the `@length` 1..500 on `DataLakeDatasetDescription`, the shape the
/// `description` member of `UpdateDataLakeDatasetRequest` targets
/// (`smithy_query(query="operation", operation="UpdateDataLakeDataset")`).
const DESCRIPTION_MIN_LENGTH: usize = 1;
const DESCRIPTION_MAX_LENGTH: usize = 500;

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

    /// Send one request signed for `region` and return its HTTP status, the
    /// `x-amzn-errortype` response header — the header restJson1 names the error shape in,
    /// and the one this service's CORS configuration exposes — and its JSON body.
    async fn dispatch_in(
        &self,
        region: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Option<String>, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        // A SigV4 `Authorization` header whose credential scope names this service
        // (`scn`), which is how the router picks the service that claims the request.
        // Signature verification is off for the test router, so the signature itself is
        // never read.
        let credential = format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/scn/aws4_request, \
             SignedHeaders=host, Signature=00"
        );
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://scn.{region}.amazonaws.com{path}"))
            .header("host", format!("scn.{region}.amazonaws.com"))
            .header("authorization", credential)
            .header("content-type", "application/json")
            .body(body_bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_type = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
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
        (status, error_type, json)
    }

    /// Send one request signed for `region`, keeping only its status and body.
    async fn send_in(
        &self,
        region: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> (u16, Value) {
        let (status, _error_type, json) = self.dispatch_in(region, method, path, body).await;
        (status, json)
    }

    /// Send one request signed for [`REGION`].
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        self.send_in(REGION, method, path, body).await
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

    /// `CreateDataLakeDataset` for one triple, with `request` as its body.
    async fn create_dataset(
        &self,
        region: &str,
        instance_id: &str,
        namespace: &str,
        name: &str,
        request: Value,
    ) -> (u16, Value) {
        self.send_in(
            region,
            "PUT",
            &dataset_path(instance_id, namespace, name),
            Some(request),
        )
        .await
    }

    /// `GetDataLakeDataset` for one triple.
    async fn get_dataset(&self, instance_id: &str, namespace: &str, name: &str) -> (u16, Value) {
        self.send("GET", &dataset_path(instance_id, namespace, name), None)
            .await
    }

    /// `GetDataLakeDataset` for one triple, keeping the `x-amzn-errortype` header too.
    async fn read_dataset(
        &self,
        instance_id: &str,
        namespace: &str,
        name: &str,
    ) -> (u16, Option<String>, Value) {
        self.dispatch_in(REGION, "GET", &dataset_path(instance_id, namespace, name), None)
            .await
    }

    /// `UpdateDataLakeDataset` for one triple — PATCH on the dataset URI — with `request`
    /// as its body.
    async fn update_dataset(
        &self,
        instance_id: &str,
        namespace: &str,
        name: &str,
        request: Value,
    ) -> (u16, Option<String>, Value) {
        self.dispatch_in(
            REGION,
            "PATCH",
            &dataset_path(instance_id, namespace, name),
            Some(request),
        )
        .await
    }

    /// `DeleteDataLakeDataset` for one triple.
    async fn delete_dataset(&self, instance_id: &str, namespace: &str, name: &str) -> (u16, Value) {
        self.send("DELETE", &dataset_path(instance_id, namespace, name), None)
            .await
    }

    /// `CreateDataLakeNamespace` for `name`.
    async fn create_namespace(&self, instance_id: &str, name: &str) -> (u16, Value) {
        self.send("PUT", &namespace_path(instance_id, name), Some(json!({})))
            .await
    }

    /// `GetDataLakeNamespace` for `name`, keeping the `x-amzn-errortype` header too.
    async fn read_namespace(&self, instance_id: &str, name: &str) -> (u16, Option<String>, Value) {
        self.dispatch_in(REGION, "GET", &namespace_path(instance_id, name), None)
            .await
    }

    /// `DeleteDataLakeNamespace` for `name`.
    async fn delete_namespace(&self, instance_id: &str, name: &str) -> (u16, Value) {
        self.send("DELETE", &namespace_path(instance_id, name), None)
            .await
    }

    /// `ListDataLakeDatasets` for one namespace.
    async fn list_datasets(&self, instance_id: &str, namespace: &str) -> (u16, Value) {
        self.send(
            "GET",
            &format!("/api/datalake/instance/{instance_id}/namespaces/{namespace}/datasets"),
            None,
        )
        .await
    }
}

/// The URI every dataset operation is addressed at, as the model's `http` trait spells
/// it: `/api/datalake/instance/{instanceId}/namespaces/{namespace}/datasets/{name}`.
fn dataset_path(instance_id: &str, namespace: &str, name: &str) -> String {
    format!("/api/datalake/instance/{instance_id}/namespaces/{namespace}/datasets/{name}")
}

/// The URI every namespace operation is addressed at, as the model's `http` trait spells
/// it: `/api/datalake/instance/{instanceId}/namespaces/{name}`.
fn namespace_path(instance_id: &str, name: &str) -> String {
    format!("/api/datalake/instance/{instance_id}/namespaces/{name}")
}

/// The sentence a dataset operation refuses a triple the data lake does not hold with,
/// written out HERE rather than read from the crate so a drift in the handlers' wording
/// fails this suite instead of travelling with it. Its SOURCE is the recorded wire
/// response of a read of a dataset that was deleted: the fixed text is "Dataset " and
/// " not exist in namespace ", and the only variable parts are the two `@httpLabel`
/// members of the request — never the `instanceId`.
fn dataset_not_found_sentence(name: &str, namespace: &str) -> String {
    format!("Dataset {name} not exist in namespace {namespace}")
}

/// The sentence a namespace operation refuses a name the data lake does not hold with;
/// its SOURCE is the recorded wire response of a read of a namespace that was deleted,
/// whose fixed text is "Namespace " and " not exist" with the `name` `@httpLabel` as its
/// only variable part.
fn namespace_not_found_sentence(name: &str) -> String {
    format!("Namespace {name} not exist")
}

/// Assert that `response` is the refusal a client must see: the modelled HTTP status, the
/// error shape named BOTH in the `x-amzn-errortype` header a restJson1 client resolves it
/// from and in the body envelope, and the service's own sentence.
fn assert_refusal(
    case: &str,
    expected_status: u16,
    shape: &str,
    expected_message: &str,
    response: &(u16, Option<String>, Value),
) {
    let (status, error_type, body) = response;
    assert_eq!(
        *status, expected_status,
        "case {case}: expected HTTP {expected_status}, got {status} with {body}"
    );
    let error_type = error_type.as_deref().unwrap_or_else(|| {
        panic!("case {case}: the refusal carries no x-amzn-errortype header: {body}")
    });
    assert!(
        error_type.contains(shape),
        "case {case}: expected an x-amzn-errortype naming {shape}, got {error_type:?}"
    );
    assert!(
        body.get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains(shape),
        "case {case}: expected a {shape} envelope, got {body}"
    );
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some(expected_message),
        "case {case}: expected the service's sentence {expected_message:?}, got {body}"
    );
}

/// Look again until the deletion of `instance_id` has SETTLED — until `GetInstance`
/// answers 404 — or until [`POLL_DEADLINE`].
///
/// `DeleteInstance` ACCEPTS a deletion: the instance moves to `Deleting` and stays
/// addressable, with the datasets addressed by it, while the deletion settles; the removal
/// is the clock's work, materialised by whichever CALL next observes the store — which is
/// why each look is a request.
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

/// The `dataset` member of a response body.
fn dataset_of(case: &str, body: &Value) -> Value {
    body.get("dataset")
        .cloned()
        .unwrap_or_else(|| panic!("case {case}: response carries no `dataset` member: {body}"))
}

/// A string member of a `dataset` payload.
fn string_of(case: &str, dataset: &Value, member: &str) -> String {
    dataset
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case}: dataset carries no `{member}`: {dataset}"))
        .to_string()
}

/// The ARN a dataset of this triple must carry, built independently of the handler from
/// `AscResourceArn`'s modelled form (`arn:aws:scn:<region>:<account>:instance/<id>` plus
/// free `[-_./A-Za-z0-9]*`) and the emulator's canonical account id.
fn expected_arn(region: &str, instance_id: &str, namespace: &str, name: &str) -> String {
    format!(
        "arn:aws:scn:{region}:{}:instance/{instance_id}/namespaces/{namespace}/datasets/{name}",
        engine::emulator_account_id()
    )
}

/// A one-field schema, the smallest the model allows (`fields` is `@length` 1..500).
fn minimal_schema() -> Value {
    json!({
        "name": "MinimalSchema",
        "fields": [{ "name": "id", "type": "STRING", "isRequired": true }],
    })
}

/// A schema carrying one field of EVERY modelled `DataLakeDatasetSchemaFieldType`
/// (source: that enum, re-exported as `DATA_LAKE_DATASET_SCHEMA_FIELD_TYPE_VALUES`), so
/// no field type is special-cased.
fn schema_over_every_field_type() -> Value {
    let fields: Vec<Value> = DATA_LAKE_DATASET_SCHEMA_FIELD_TYPE_VALUES
        .iter()
        .enumerate()
        .map(|(index, field_type)| {
            json!({
                "name": format!("field_{}", field_type.to_lowercase()),
                "type": field_type,
                "isRequired": index % 2 == 0,
            })
        })
        .collect();
    json!({
        "name": "EveryFieldType",
        "fields": fields,
        "primaryKeys": [{ "name": format!("field_{}", DATA_LAKE_DATASET_SCHEMA_FIELD_TYPE_VALUES[0].to_lowercase()) }],
    })
}

/// A partition spec carrying one field of every modelled
/// `DataLakeDatasetPartitionTransformType` (source: that enum, re-exported as
/// `DATA_LAKE_DATASET_PARTITION_TRANSFORM_TYPE_VALUES`); the list's modelled maximum is
/// 10 fields, so every transform fits.
fn partition_spec_over_every_transform() -> Value {
    let fields: Vec<Value> = DATA_LAKE_DATASET_PARTITION_TRANSFORM_TYPE_VALUES
        .iter()
        .map(|transform| {
            json!({
                "name": format!("part_{}", transform.to_lowercase()),
                "transform": { "type": transform },
            })
        })
        .collect();
    json!({ "fields": fields })
}

/// One row of the create/read-back table: the dataset's namespace, name and request body.
struct Row {
    case: &'static str,
    namespace: &'static str,
    name: String,
    request: Value,
}

/// The rows spanning the class the rule generalises over: both pre-defined namespaces,
/// names at the ends of the modelled `^[a-z0-9_]+$` / 1..75 shape, every modelled field
/// type and partition transform, and the optional members (`description`,
/// `partitionSpec`, `tags`) both present and absent.
fn create_rows() -> Vec<Row> {
    vec![
        Row {
            case: "default namespace, nothing optional",
            namespace: "default",
            name: "orders".to_string(),
            request: json!({ "schema": minimal_schema() }),
        },
        Row {
            case: "default namespace, every optional member",
            namespace: "default",
            name: "shipment_lines_2024".to_string(),
            request: json!({
                "schema": schema_over_every_field_type(),
                "description": "every modelled field type, partitioned and tagged",
                "partitionSpec": partition_spec_over_every_transform(),
                "tags": { "team": "supply", "env": "" },
            }),
        },
        Row {
            case: "asc namespace with an explicit schema",
            namespace: "asc",
            name: "inbound_order".to_string(),
            request: json!({
                "schema": minimal_schema(),
                "description": "a dataset in the pre-defined asc namespace",
            }),
        },
        Row {
            case: "single-character name",
            namespace: "default",
            name: "a".to_string(),
            request: json!({ "schema": minimal_schema() }),
        },
        Row {
            case: "name at the modelled 75-character maximum",
            namespace: "default",
            // 75 characters of `^[a-z0-9_]+$`, the shape's upper length bound.
            name: format!("d{}", "_9".repeat(37)),
            request: json!({
                "schema": minimal_schema(),
                "partitionSpec": json!({
                    "fields": [{ "name": "id", "transform": { "type": "IDENTITY" } }],
                }),
            }),
        },
    ]
}

/// RULE: a created dataset is persisted under its (instance, namespace, name) triple and
/// `GetDataLakeDataset` answers with exactly the dataset the create returned — every
/// member projected from the stored record, the optional ones absent when the request
/// named none — and reading it again changes nothing.
#[tokio::test]
async fn a_dataset_round_trips_through_create_and_get() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("round trip").await;

    for row in create_rows() {
        let case = row.case;
        let (status, body) = emulator
            .create_dataset(REGION, &instance_id, row.namespace, &row.name, row.request.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateDataLakeDataset expected 200, got {status} with {body}"
        );
        let created = dataset_of(case, &body);

        assert_eq!(
            string_of(case, &created, "instanceId"),
            instance_id,
            "case {case}: the dataset must report the instance it was created under, got {created}"
        );
        assert_eq!(
            string_of(case, &created, "namespace"),
            row.namespace,
            "case {case}: the dataset must report the namespace it was created in, got {created}"
        );
        assert_eq!(
            string_of(case, &created, "name"),
            row.name,
            "case {case}: the dataset must report the name it was created with, got {created}"
        );
        assert_eq!(
            string_of(case, &created, "arn"),
            expected_arn(REGION, &instance_id, row.namespace, &row.name),
            "case {case}: the dataset ARN must name this triple, got {created}"
        );
        assert_eq!(
            created.get("schema"),
            row.request.get("schema"),
            "case {case}: the stored schema must be the requested one, got {created}"
        );
        assert_eq!(
            created.get("description"),
            row.request.get("description"),
            "case {case}: the stored description must be the requested one, got {created}"
        );
        assert_eq!(
            created.get("partitionSpec"),
            row.request.get("partitionSpec"),
            "case {case}: the stored partitionSpec must be the requested one, got {created}"
        );
        assert_eq!(
            created.get("createdTime"),
            created.get("lastModifiedTime"),
            "case {case}: an untouched dataset was last modified when it was created, got {created}"
        );
        assert!(
            created.get("createdTime").and_then(Value::as_f64).is_some(),
            "case {case}: createdTime must be a timestamp, got {created}"
        );
        assert!(
            created.get("tags").is_none(),
            "case {case}: `tags` is not a member of the DataLakeDataset shape, got {created}"
        );

        let (status, body) = emulator
            .get_dataset(&instance_id, row.namespace, &row.name)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: GetDataLakeDataset expected 200, got {status} with {body}"
        );
        let read = dataset_of(case, &body);
        assert_eq!(
            read, created,
            "case {case}: the dataset read back must equal the one created, got {read}"
        );

        let (status, body) = emulator
            .get_dataset(&instance_id, row.namespace, &row.name)
            .await;
        let read_again = dataset_of(case, &body);
        assert_eq!(status, 200, "case {case}: a second read expected 200, got {status}");
        assert_eq!(
            read_again, created,
            "case {case}: reading a dataset must not change it, got {read_again}"
        );
    }
}

/// RULE: a dataset is addressed by the whole (instance, namespace, name) triple, so one
/// name may be held once per namespace of each instance and reading another triple of
/// the same name is a not-found.
#[tokio::test]
async fn datasets_are_scoped_to_their_instance_and_namespace() {
    let emulator = Emulator::new();
    let first = emulator.create_instance("first instance").await;
    let second = emulator.create_instance("second instance").await;
    let name = "shared_name";

    // One dataset of this name per namespace of each instance: four distinct triples.
    let mut arns = Vec::new();
    for instance_id in [&first, &second] {
        for namespace in PREDEFINED_NAMESPACES {
            let case = format!("{instance_id}/{namespace}");
            let (status, body) = emulator
                .create_dataset(
                    REGION,
                    instance_id,
                    namespace,
                    name,
                    json!({ "schema": minimal_schema(), "description": case.clone() }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {case}: each triple is its own dataset, got {status} with {body}"
            );
            arns.push(string_of(&case, &dataset_of(&case, &body), "arn"));
        }
    }
    let distinct: std::collections::BTreeSet<&String> = arns.iter().collect();
    assert_eq!(
        distinct.len(),
        arns.len(),
        "each triple must carry its own ARN, got {arns:?}"
    );

    // Each stored dataset answers with the description its own create carried, so no
    // triple is reading another's record.
    for instance_id in [&first, &second] {
        for namespace in PREDEFINED_NAMESPACES {
            let case = format!("read {instance_id}/{namespace}");
            let (status, body) = emulator.get_dataset(instance_id, namespace, name).await;
            assert_eq!(
                status, 200,
                "case {case}: expected 200, got {status} with {body}"
            );
            let dataset = dataset_of(&case, &body);
            assert_eq!(
                string_of(&case, &dataset, "description"),
                format!("{instance_id}/{namespace}"),
                "case {case}: each triple keeps its own record, got {dataset}"
            );
        }
    }

    // A name no triple of this instance holds is still a not-found.
    let (status, body) = emulator.get_dataset(&first, "default", "never_created").await;
    assert_eq!(
        status, 404,
        "case absent name: expected 404, got {status} with {body}"
    );
}

/// RULE: a (instance, namespace, name) triple names one dataset, so a create for a
/// triple that already holds one is refused `ConflictException` (409) and the stored
/// dataset is left exactly as it was.
#[tokio::test]
async fn a_second_create_of_one_triple_is_a_conflict() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("conflict").await;

    // Rows differing in what the SECOND create changes: nothing, the schema, the
    // description, the partition spec.
    let rows: Vec<(&str, &str, Value)> = vec![
        ("identical request", "repeat_identical", json!({ "schema": minimal_schema() })),
        (
            "a different schema",
            "repeat_other_schema",
            json!({ "schema": schema_over_every_field_type() }),
        ),
        (
            "a different description",
            "repeat_other_description",
            json!({ "schema": minimal_schema(), "description": "second attempt" }),
        ),
        (
            "an added partition spec",
            "repeat_added_partition",
            json!({
                "schema": minimal_schema(),
                "partitionSpec": partition_spec_over_every_transform(),
            }),
        ),
    ];

    for (case, name, second_request) in rows {
        let (status, body) = emulator
            .create_dataset(
                REGION,
                &instance_id,
                "default",
                name,
                json!({ "schema": minimal_schema(), "description": "first" }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: the first create expected 200, got {status} with {body}"
        );
        let first = dataset_of(case, &body);

        let (status, body) = emulator
            .create_dataset(REGION, &instance_id, "default", name, second_request)
            .await;
        assert_eq!(
            status, 409,
            "case {case}: a second create of one triple must be refused, got {status} with {body}"
        );
        assert!(
            body.get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("ConflictException"),
            "case {case}: expected a ConflictException envelope, got {body}"
        );
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            message,
            format!("Dataset {name} already exist in namespace default"),
            "case {case}: the refusal must carry the service's conflict sentence for this \
             dataset and namespace, got {message:?}"
        );

        let (status, body) = emulator.get_dataset(&instance_id, "default", name).await;
        assert_eq!(
            status, 200,
            "case {case}: the stored dataset survives the refusal, got {status} with {body}"
        );
        assert_eq!(
            dataset_of(case, &body),
            first,
            "case {case}: a refused create must change nothing, got {body}"
        );
    }
}

/// RULE: a dataset exists only under an instance the store holds and a namespace that
/// instance has, so a create or read naming either of them wrongly is
/// `ResourceNotFoundException` (404) — and an `instanceId` outside the modelled `UUID`
/// shape is refused `ValidationException` (400) before any handler runs.
#[tokio::test]
async fn an_unknown_instance_or_namespace_is_not_found() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("not found").await;

    // Rows: (case, instanceId, namespace, expected status).
    let rows: Vec<(&str, String, &str, u16)> = vec![
        (
            "well-formed instance id no instance holds",
            "00000000-0000-0000-0000-000000000000".to_string(),
            "default",
            404,
        ),
        (
            "a namespace that was never created",
            instance_id.clone(),
            "custom_namespace",
            404,
        ),
        (
            "a namespace one character off a pre-defined one",
            instance_id.clone(),
            "defaults",
            404,
        ),
        (
            "an instance id outside the modelled UUID shape",
            "not-a-uuid".to_string(),
            "default",
            400,
        ),
        ("a live instance and a pre-defined namespace", instance_id.clone(), "default", 200),
    ];

    for (case, instance, namespace, expected) in rows {
        let (status, body) = emulator
            .create_dataset(
                REGION,
                &instance,
                namespace,
                "probe",
                json!({ "schema": minimal_schema() }),
            )
            .await;
        assert_eq!(
            status, expected,
            "case {case}: CreateDataLakeDataset expected {expected}, got {status} with {body}"
        );
        if expected != 200 {
            let kind = if expected == 404 {
                "ResourceNotFoundException"
            } else {
                "ValidationException"
            };
            assert!(
                body.get("__type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains(kind),
                "case {case}: expected a {kind} envelope, got {body}"
            );
        }

        // The same triple reads back exactly as the create left it: present only where
        // the create was served.
        let (status, body) = emulator.get_dataset(&instance, namespace, "probe").await;
        assert_eq!(
            status, expected,
            "case {case}: GetDataLakeDataset expected {expected}, got {status} with {body}"
        );
    }
}

/// RULE: a dataset's schema is part of the stored resource and the pre-defined schemas of
/// the `asc` namespace are not part of this service's model, so a create naming no
/// `schema` is refused `ValidationException` (400) and persists nothing.
#[tokio::test]
async fn a_create_without_a_schema_is_refused_and_persists_nothing() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("no schema").await;

    // Rows: every namespace an instance comes with, and a body that names other members
    // but no schema.
    for namespace in PREDEFINED_NAMESPACES {
        for (case, request) in [
            ("empty body", json!({})),
            (
                "description and tags but no schema",
                json!({ "description": "no schema here", "tags": { "team": "supply" } }),
            ),
            (
                "a partition spec but no schema",
                json!({ "partitionSpec": partition_spec_over_every_transform() }),
            ),
        ] {
            let case = format!("{namespace}: {case}");
            let (status, body) = emulator
                .create_dataset(REGION, &instance_id, namespace, "schemaless", request)
                .await;
            assert_eq!(
                status, 400,
                "case {case}: a schemaless create must be refused, got {status} with {body}"
            );
            assert!(
                body.get("__type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .contains("ValidationException"),
                "case {case}: expected a ValidationException envelope, got {body}"
            );
            assert_eq!(
                body.get("message").and_then(Value::as_str),
                Some(SCHEMA_REQUIRED_MESSAGE),
                "case {case}: expected the schema refusal sentence, got {body}"
            );

            let (status, body) = emulator
                .get_dataset(&instance_id, namespace, "schemaless")
                .await;
            assert_eq!(
                status, 404,
                "case {case}: a refused create must persist nothing, got {status} with {body}"
            );
        }
    }
}

/// RULE: a dataset's ARN carries the region the call that created it was signed for, and
/// the canonical emulator account — the stored value, so a read reports the same ARN.
#[tokio::test]
async fn a_dataset_arn_names_the_region_the_create_was_signed_for() {
    let emulator = Emulator::new();

    for region in ["us-east-1", "eu-west-1", "ap-southeast-2", "us-gov-west-1"] {
        let instance_id = emulator.create_instance(region).await;
        let (status, body) = emulator
            .create_dataset(
                region,
                &instance_id,
                "default",
                "regional",
                json!({ "schema": minimal_schema() }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {region}: CreateDataLakeDataset expected 200, got {status} with {body}"
        );
        let created = dataset_of(region, &body);
        assert_eq!(
            string_of(region, &created, "arn"),
            expected_arn(region, &instance_id, "default", "regional"),
            "case {region}: the ARN must name the region the create was signed for, got {created}"
        );

        // Read through the default region: the ARN is stored, not rebuilt per request.
        let (status, body) = emulator.get_dataset(&instance_id, "default", "regional").await;
        assert_eq!(
            status, 200,
            "case {region}: GetDataLakeDataset expected 200, got {status} with {body}"
        );
        assert_eq!(
            dataset_of(region, &body),
            created,
            "case {region}: a read reports the stored ARN, got {body}"
        );
    }
}

/// RULE: a dataset is addressed by its instance, so deleting the instance takes its
/// datasets with it and leaves every other instance's datasets reachable.
#[tokio::test]
async fn deleting_an_instance_drops_its_datasets() {
    let emulator = Emulator::new();
    let doomed = emulator.create_instance("doomed").await;
    let survivor = emulator.create_instance("survivor").await;

    for instance_id in [&doomed, &survivor] {
        for namespace in PREDEFINED_NAMESPACES {
            let (status, body) = emulator
                .create_dataset(
                    REGION,
                    instance_id,
                    namespace,
                    "kept",
                    json!({ "schema": minimal_schema() }),
                )
                .await;
            assert_eq!(
                status, 200,
                "case {instance_id}/{namespace}: expected 200, got {status} with {body}"
            );
        }
    }

    let (status, body) = emulator
        .send("DELETE", &format!("/api/instance/{doomed}"), None)
        .await;
    assert_eq!(
        status, 200,
        "case delete: DeleteInstance expected 200, got {status} with {body}"
    );
    // A dataset goes when the accepted deletion SETTLES: while the instance is `Deleting`
    // its datasets are still addressable.
    for namespace in PREDEFINED_NAMESPACES {
        let (status, body) = emulator.get_dataset(&doomed, namespace, "kept").await;
        assert_eq!(
            status, 200,
            "case {namespace}: a Deleting instance's dataset is still readable, got {status} \
             with {body}"
        );
    }
    wait_until_deleted(&emulator, &doomed).await;

    for namespace in PREDEFINED_NAMESPACES {
        let (status, body) = emulator.get_dataset(&doomed, namespace, "kept").await;
        assert_eq!(
            status, 404,
            "case {namespace}: a deleted instance's dataset is gone, got {status} with {body}"
        );
        let (status, body) = emulator.get_dataset(&survivor, namespace, "kept").await;
        assert_eq!(
            status, 200,
            "case {namespace}: another instance's dataset survives, got {status} with {body}"
        );
    }
}

/// RULE: the number of datasets one instance holds is capped, and a create past the cap
/// is refused with the modelled `ServiceQuotaExceededException` (402) while every stored
/// dataset stays readable.
#[tokio::test]
async fn a_create_past_the_dataset_cap_is_a_quota_refusal() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("quota").await;

    for index in 0..MAX_DATASETS_PER_INSTANCE {
        let name = format!("dataset_{index}");
        let (status, body) = emulator
            .create_dataset(
                REGION,
                &instance_id,
                "default",
                &name,
                json!({ "schema": minimal_schema() }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {name}: a create below the cap expected 200, got {status} with {body}"
        );
    }

    let (status, body) = emulator
        .create_dataset(
            REGION,
            &instance_id,
            "asc",
            "one_too_many",
            json!({ "schema": minimal_schema() }),
        )
        .await;
    assert_eq!(
        status, 402,
        "case past the cap: expected the modelled quota refusal, got {status} with {body}"
    );
    assert!(
        body.get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .contains("ServiceQuotaExceededException"),
        "case past the cap: expected a ServiceQuotaExceededException envelope, got {body}"
    );

    // The cap is per instance, so another instance can still create.
    let other = emulator.create_instance("other instance").await;
    let (status, body) = emulator
        .create_dataset(
            REGION,
            &other,
            "default",
            "first_here",
            json!({ "schema": minimal_schema() }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case other instance: the cap is per instance, got {status} with {body}"
    );

    let (status, body) = emulator.get_dataset(&instance_id, "default", "dataset_0").await;
    assert_eq!(
        status, 200,
        "case first stored dataset: still readable at the cap, got {status} with {body}"
    );
}

/// One row of the update table: which namespace holds the dataset, what description its
/// create carried, the body the update sends and the description the dataset must carry
/// afterwards.
struct UpdateRow {
    case: &'static str,
    namespace: &'static str,
    name: &'static str,
    created_description: Option<String>,
    request: Value,
    expected_description: Option<String>,
}

/// The rows spanning what the update rule generalises over: whether the dataset was
/// created with a description, whether the update names one, whether the value it names
/// differs from the stored one, both published bounds of the `DataLakeDatasetDescription`
/// shape (`@length` 1..500, source: the `description` member of
/// `UpdateDataLakeDatasetRequest`), and which namespace holds the dataset.
fn update_rows(custom_namespace: &'static str) -> Vec<UpdateRow> {
    vec![
        UpdateRow {
            case: "sets a description on a dataset created without one",
            namespace: "default",
            name: "described_later",
            created_description: None,
            request: json!({ "description": "named after the fact" }),
            expected_description: Some("named after the fact".to_string()),
        },
        UpdateRow {
            case: "replaces the stored description",
            namespace: "asc",
            name: "inbound_order",
            created_description: Some("as created".to_string()),
            request: json!({ "description": "as updated" }),
            expected_description: Some("as updated".to_string()),
        },
        UpdateRow {
            case: "repeats the stored description",
            namespace: custom_namespace,
            name: "unchanged_value",
            created_description: Some("steady".to_string()),
            request: json!({ "description": "steady" }),
            expected_description: Some("steady".to_string()),
        },
        UpdateRow {
            case: "names no description at all",
            namespace: custom_namespace,
            name: "untouched",
            created_description: Some("kept".to_string()),
            request: json!({}),
            expected_description: Some("kept".to_string()),
        },
        UpdateRow {
            case: "description at the modelled 1-character minimum",
            namespace: "default",
            name: "shortest_description",
            created_description: Some("as created".to_string()),
            request: json!({ "description": "x" }),
            expected_description: Some("x".to_string()),
        },
        UpdateRow {
            case: "description at the modelled 500-character maximum",
            namespace: "default",
            name: "longest_description",
            created_description: None,
            request: json!({ "description": "d".repeat(500) }),
            expected_description: Some("d".repeat(500)),
        },
    ]
}

/// RULE: an update of a data-lake resource rewrites only the member the request names and
/// answers with the record it leaves in the store, so a named `description` replaces the
/// stored one, an omitted one leaves it alone, every other member keeps the value the
/// create persisted, and `GetDataLakeDataset` reports exactly what the update answered.
#[tokio::test]
async fn a_dataset_description_round_trips_through_update_and_get() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("update").await;
    let custom_namespace = "orders_2024";
    let (status, body) = emulator
        .create_namespace(&instance_id, custom_namespace)
        .await;
    assert_eq!(
        status, 200,
        "the custom namespace must be created first, got {status} with {body}"
    );

    for row in update_rows(custom_namespace) {
        let case = row.case;
        let mut create_request = json!({ "schema": minimal_schema() });
        if let Some(description) = &row.created_description {
            create_request["description"] = json!(description);
        }
        let (status, body) = emulator
            .create_dataset(REGION, &instance_id, row.namespace, row.name, create_request)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateDataLakeDataset expected 200, got {status} with {body}"
        );
        let created = dataset_of(case, &body);

        let (status, _error_type, body) = emulator
            .update_dataset(&instance_id, row.namespace, row.name, row.request.clone())
            .await;
        assert_eq!(
            status, 200,
            "case {case}: UpdateDataLakeDataset expected 200, got {status} with {body}"
        );
        let updated = dataset_of(case, &body);

        assert_eq!(
            updated.get("description").and_then(Value::as_str),
            row.expected_description.as_deref(),
            "case {case}: expected the description {:?}, got {updated}",
            row.expected_description
        );
        for member in ["instanceId", "namespace", "name", "arn", "schema", "createdTime"] {
            assert_eq!(
                updated.get(member),
                created.get(member),
                "case {case}: the update must leave `{member}` as the create persisted it, got \
                 {updated}"
            );
        }
        let created_time = created
            .get("createdTime")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| panic!("case {case}: createdTime must be a timestamp: {created}"));
        let last_modified = updated
            .get("lastModifiedTime")
            .and_then(Value::as_f64)
            .unwrap_or_else(|| {
                panic!("case {case}: lastModifiedTime must be a timestamp: {updated}")
            });
        assert!(
            last_modified >= created_time,
            "case {case}: lastModifiedTime {last_modified} must not precede createdTime \
             {created_time}"
        );
        // A write that moves nothing is not a modification, so the stamp the create set
        // stands.
        if row.expected_description == row.created_description {
            assert_eq!(
                updated.get("lastModifiedTime"),
                created.get("lastModifiedTime"),
                "case {case}: an update that changes nothing must not move lastModifiedTime, got \
                 {updated}"
            );
        }

        let (status, body) = emulator
            .get_dataset(&instance_id, row.namespace, row.name)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: GetDataLakeDataset expected 200, got {status} with {body}"
        );
        assert_eq!(
            dataset_of(case, &body),
            updated,
            "case {case}: the dataset read back must equal the one the update answered, got {body}"
        );
    }
}

/// RULE: an update is addressed by the whole triple, so a triple the data lake holds no
/// dataset under — including the triple of one a delete already removed — is refused
/// `ResourceNotFoundException` (404) with the service's own sentence and writes nothing.
#[tokio::test]
async fn an_update_of_an_absent_dataset_is_a_not_found_that_writes_nothing() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("absent update").await;
    let other = emulator.create_instance("other instance").await;

    // A dataset that exists and is then removed, so one row addresses a triple that was
    // live a moment earlier.
    let (status, body) = emulator
        .create_dataset(REGION, &instance_id, "default", "deleted", json!({ "schema": minimal_schema() }))
        .await;
    assert_eq!(
        status, 200,
        "the dataset to delete must be created first, got {status} with {body}"
    );
    let (status, body) = emulator
        .delete_dataset(&instance_id, "default", "deleted")
        .await;
    assert_eq!(
        status, 200,
        "DeleteDataLakeDataset expected 200, got {status} with {body}"
    );

    // Rows differing along which part of the triple names nothing: the dataset name, the
    // namespace, the instance — plus the triple of a dataset that was deleted.
    let rows: Vec<(&str, &str, &str, &str)> = vec![
        ("a name no dataset was created under", instance_id.as_str(), "default", "never_created"),
        ("the triple of a deleted dataset", instance_id.as_str(), "default", "deleted"),
        ("a namespace nobody created", instance_id.as_str(), "shipments", "lines"),
        ("another instance's triple", other.as_str(), "default", "deleted"),
        (
            "a well-formed instance id no instance holds",
            "00000000-0000-0000-0000-000000000000",
            "asc",
            "inbound_order",
        ),
    ];

    for (case, instance, namespace, name) in rows {
        let response = emulator
            .update_dataset(instance, namespace, name, json!({ "description": "no such dataset" }))
            .await;
        assert_refusal(
            case,
            404,
            "ResourceNotFoundException",
            &dataset_not_found_sentence(name, namespace),
            &response,
        );
        let message = response.2.get("message").and_then(Value::as_str).unwrap_or_default();
        assert!(
            !message.contains(instance),
            "case {case}: the sentence must not carry the instanceId, got {message:?}"
        );

        // A refused update writes nothing, so the triple it addressed still names no
        // dataset.
        let (status, body) = emulator.get_dataset(instance, namespace, name).await;
        assert_eq!(
            status, 404,
            "case {case}: a refused update must create nothing, got {status} with {body}"
        );
    }
}

/// RULE: an update addresses ONE record by its whole (`instanceId`, `namespace`, `name`)
/// triple, so it rewrites the description of that dataset alone — a sibling in the same
/// namespace, the same dataset name under another namespace and the same triple in another
/// instance keep every member their create persisted — and every reader of the data lake
/// (`GetDataLakeDataset` and `ListDataLakeDatasets` alike) reports what the update left.
#[tokio::test]
async fn an_update_writes_only_the_dataset_its_triple_addresses() {
    let emulator = Emulator::new();
    let first = emulator.create_instance("first instance").await;
    let second = emulator.create_instance("second instance").await;
    let custom_namespace = "zone_one";
    let (status, body) = emulator
        .create_namespace(&first, custom_namespace)
        .await;
    assert_eq!(
        status, 200,
        "the custom namespace must be created first, got {status} with {body}"
    );

    // The triples the addressing rule generalises over: which part of the triple a row
    // shares with the one being updated — the namespace (a sibling name), the name (another
    // namespace of the same instance), the whole namespace-and-name pair (another
    // instance).
    let triples: Vec<(&str, String, &str, &str)> = vec![
        (
            "a dataset of the default namespace",
            first.clone(),
            "default",
            "inventory_level",
        ),
        (
            "a sibling of the same namespace",
            first.clone(),
            "default",
            "shipment_lines",
        ),
        (
            "the same dataset name under another namespace",
            first.clone(),
            custom_namespace,
            "inventory_level",
        ),
        (
            "the same triple in another instance",
            second.clone(),
            "default",
            "inventory_level",
        ),
    ];

    for (case, instance, namespace, name) in &triples {
        let (status, body) = emulator
            .create_dataset(
                REGION,
                instance,
                namespace,
                name,
                json!({
                    "schema": minimal_schema(),
                    "description": format!("as created: {case}"),
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateDataLakeDataset expected 200, got {status} with {body}"
        );
    }

    // Every triple is updated in turn, so no row is privileged: each update must move the
    // record it addresses and leave the other three byte for byte.
    for (index, (case, instance, namespace, name)) in triples.iter().enumerate() {
        let mut before: Vec<Value> = Vec::with_capacity(triples.len());
        for (other_case, other_instance, other_namespace, other_name) in &triples {
            let (status, body) = emulator
                .get_dataset(other_instance, other_namespace, other_name)
                .await;
            assert_eq!(
                status, 200,
                "case {other_case}: GetDataLakeDataset expected 200, got {status} with {body}"
            );
            before.push(dataset_of(other_case, &body));
        }

        let description = format!("rewritten while addressing {case}");
        let (status, _error_type, body) = emulator
            .update_dataset(
                instance,
                namespace,
                name,
                json!({ "description": description.clone() }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: UpdateDataLakeDataset expected 200, got {status} with {body}"
        );
        let updated = dataset_of(case, &body);
        assert_eq!(
            updated.get("description").and_then(Value::as_str),
            Some(description.as_str()),
            "case {case}: expected the description {description:?}, got {updated}"
        );

        for (position, (other_case, other_instance, other_namespace, other_name)) in
            triples.iter().enumerate()
        {
            let (status, body) = emulator
                .get_dataset(other_instance, other_namespace, other_name)
                .await;
            assert_eq!(
                status, 200,
                "case {other_case}: GetDataLakeDataset expected 200, got {status} with {body}"
            );
            let read = dataset_of(other_case, &body);
            if position == index {
                assert_eq!(
                    read, updated,
                    "case {case}: the dataset read back must equal the one the update \
                     answered, got {read}"
                );
            } else {
                assert_eq!(
                    read, before[position],
                    "case {case}: the update must leave {other_case} exactly as it was, got \
                     {read}"
                );
            }
        }
    }

    // The listing reads the same records, so it reports every updated description too.
    for (namespace, names) in [
        ("default", vec!["inventory_level", "shipment_lines"]),
        (custom_namespace, vec!["inventory_level"]),
    ] {
        let (status, body) = emulator.list_datasets(&first, namespace).await;
        assert_eq!(
            status, 200,
            "case {namespace}: ListDataLakeDatasets expected 200, got {status} with {body}"
        );
        let listed = body
            .get("datasets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("case {namespace}: the listing carries no datasets: {body}"));
        assert_eq!(
            listed.len(),
            names.len(),
            "case {namespace}: expected {} dataset(s), got {listed:?}",
            names.len()
        );
        for name in names {
            let (status, read) = emulator.get_dataset(&first, namespace, name).await;
            assert_eq!(
                status, 200,
                "case {namespace}/{name}: GetDataLakeDataset expected 200, got {status} with \
                 {read}"
            );
            let read = dataset_of(name, &read);
            assert!(
                listed.contains(&read),
                "case {namespace}/{name}: the listing must report the record the update left, \
                 got {listed:?} without {read}"
            );
        }
    }
}

/// RULE: a description outside the bound its shape publishes is a `ValidationException`
/// (HTTP 400) naming the member and the requirement, and a refused update writes nothing —
/// while the bound's own endpoints are accepted, so the refusal is the bound and not the
/// member. The bound is `@length` 1..500 on `DataLakeDatasetDescription` (source: the
/// `description` member of `UpdateDataLakeDatasetRequest`,
/// `smithy_query(query="operation", operation="UpdateDataLakeDataset")`).
#[tokio::test]
async fn an_update_outside_the_description_bound_is_refused_and_writes_nothing() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("bounded description").await;
    let (status, body) = emulator
        .create_dataset(
            REGION,
            &instance_id,
            "default",
            "bounded",
            json!({ "schema": minimal_schema(), "description": "as created" }),
        )
        .await;
    assert_eq!(
        status, 200,
        "the dataset to update must be created first, got {status} with {body}"
    );
    let (status, body) = emulator
        .get_dataset(&instance_id, "default", "bounded")
        .await;
    assert_eq!(
        status, 200,
        "the created dataset must be readable, got {status} with {body}"
    );
    let stored = dataset_of("as created", &body);

    // Rows: each published bound, one character outside it.
    let rows: Vec<(&str, String, String)> = vec![
        (
            "one character below the published minimum",
            String::new(),
            format!("Member must have length greater than or equal to {DESCRIPTION_MIN_LENGTH}"),
        ),
        (
            "one character past the published maximum",
            "d".repeat(DESCRIPTION_MAX_LENGTH + 1),
            format!("Member must have length less than or equal to {DESCRIPTION_MAX_LENGTH}"),
        ),
    ];

    for (case, description, requirement) in rows {
        let (status, _error_type, body) = emulator
            .update_dataset(
                &instance_id,
                "default",
                "bounded",
                json!({ "description": description.clone() }),
            )
            .await;
        assert_eq!(
            status, 400,
            "case {case}: expected HTTP 400, got {status} with {body}"
        );
        assert!(
            body.get("__type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .contains("ValidationException"),
            "case {case}: expected a ValidationException envelope, got {body}"
        );
        let expected = format!(
            "1 validation error detected: Value at 'description' failed to satisfy constraint: \
             {requirement}"
        );
        let message = body.get("message").and_then(Value::as_str);
        assert_eq!(
            message,
            Some(expected.as_str()),
            "case {case}: expected the member-and-requirement sentence {expected:?}, got \
             {message:?}"
        );

        // Nothing of the refused request reached the store, so the dataset still holds
        // every value it held — the description included.
        let (status, body) = emulator
            .get_dataset(&instance_id, "default", "bounded")
            .await;
        assert_eq!(
            status, 200,
            "case {case}: GetDataLakeDataset expected 200, got {status} with {body}"
        );
        assert_eq!(
            dataset_of(case, &body),
            stored,
            "case {case}: a refused update must write nothing, got {body}"
        );
    }

    for (case, description) in [
        ("the published minimum length", "x".repeat(DESCRIPTION_MIN_LENGTH)),
        ("the published maximum length", "d".repeat(DESCRIPTION_MAX_LENGTH)),
    ] {
        let (status, _error_type, body) = emulator
            .update_dataset(
                &instance_id,
                "default",
                "bounded",
                json!({ "description": description.clone() }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case}: a description at the bound expected 200, got {status} with {body}"
        );
        let updated = dataset_of(case, &body);
        assert_eq!(
            updated.get("description").and_then(Value::as_str),
            Some(description.as_str()),
            "case {case}: expected the description the update named, got {updated}"
        );
    }
}

/// RULE: a delete makes the resource it names non-existent, so every read of it afterwards
/// is the modelled `ResourceNotFoundException` (404) carrying the error shape in
/// `x-amzn-errortype` and the service's name-bearing sentence — never a 200 projection of
/// the record the delete removed — and no listing of the data lake reports it any more.
#[tokio::test]
async fn deleted_data_lake_resources_stop_being_readable() {
    let emulator = Emulator::new();
    let instance_id = emulator.create_instance("teardown").await;

    // Rows spanning the names the rule generalises over: both published length bounds of
    // `DataLakeNamespaceName` (1..50) and `DataLakeDatasetName` (1..75), digits and
    // underscores, and a dataset whose create carried a description.
    let rows: Vec<(&str, String, String, Value)> = vec![
        (
            "single-character names",
            "n".to_string(),
            "d".to_string(),
            json!({ "schema": minimal_schema() }),
        ),
        (
            "digits and underscores",
            "zone_42_raw".to_string(),
            "shipment_lines_2024".to_string(),
            json!({ "schema": minimal_schema(), "description": "deleted later" }),
        ),
        (
            "names at the modelled length maxima",
            // 50 characters of `^[a-z0-9_]+$`: `DataLakeNamespaceName`'s upper bound.
            format!("z{}0", "_9".repeat(24)),
            // 75 characters of `^[a-z0-9_]+$`: `DataLakeDatasetName`'s upper bound.
            format!("d{}", "_9".repeat(37)),
            json!({ "schema": schema_over_every_field_type() }),
        ),
    ];

    for (case, namespace, name, create_request) in rows {
        let (status, body) = emulator.create_namespace(&instance_id, &namespace).await;
        assert_eq!(
            status, 200,
            "case {case}: CreateDataLakeNamespace expected 200, got {status} with {body}"
        );
        let (status, body) = emulator
            .create_dataset(REGION, &instance_id, &namespace, &name, create_request)
            .await;
        assert_eq!(
            status, 200,
            "case {case}: CreateDataLakeDataset expected 200, got {status} with {body}"
        );

        // Both resources are readable while they exist, so the 404s below are the delete's
        // work and not a dataset that was never there.
        let (status, _error_type, body) = emulator.read_dataset(&instance_id, &namespace, &name).await;
        assert_eq!(
            status, 200,
            "case {case}: the live dataset must be readable, got {status} with {body}"
        );
        let (status, _error_type, body) = emulator.read_namespace(&instance_id, &namespace).await;
        assert_eq!(
            status, 200,
            "case {case}: the live namespace must be readable, got {status} with {body}"
        );

        let (status, body) = emulator.delete_dataset(&instance_id, &namespace, &name).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteDataLakeDataset expected 200, got {status} with {body}"
        );

        let response = emulator.read_dataset(&instance_id, &namespace, &name).await;
        assert_refusal(
            &format!("{case}: read of the deleted dataset"),
            404,
            "ResourceNotFoundException",
            &dataset_not_found_sentence(&name, &namespace),
            &response,
        );

        // The namespace that held it stops reporting it as well, so no reader of this data
        // lake still has it.
        let (status, body) = emulator.list_datasets(&instance_id, &namespace).await;
        assert_eq!(
            status, 200,
            "case {case}: ListDataLakeDatasets expected 200, got {status} with {body}"
        );
        let listed = body
            .get("datasets")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        assert!(
            listed.is_empty(),
            "case {case}: the namespace must report no dataset after the delete, got {listed:?}"
        );

        let (status, body) = emulator.delete_namespace(&instance_id, &namespace).await;
        assert_eq!(
            status, 200,
            "case {case}: DeleteDataLakeNamespace expected 200, got {status} with {body}"
        );

        let response = emulator.read_namespace(&instance_id, &namespace).await;
        assert_refusal(
            &format!("{case}: read of the deleted namespace"),
            404,
            "ResourceNotFoundException",
            &namespace_not_found_sentence(&namespace),
            &response,
        );
        let message = response.2.get("message").and_then(Value::as_str).unwrap_or_default();
        assert!(
            !message.contains(instance_id.as_str()),
            "case {case}: the namespace sentence must not carry the instanceId, got {message:?}"
        );

        // A repeat of either delete finds nothing, which is what makes a "wait until gone"
        // poll loop terminate.
        let (status, body) = emulator.delete_namespace(&instance_id, &namespace).await;
        assert_eq!(
            status, 404,
            "case {case}: a repeat namespace delete expected 404, got {status} with {body}"
        );
        let (status, body) = emulator.delete_dataset(&instance_id, &namespace, &name).await;
        assert_eq!(
            status, 404,
            "case {case}: a repeat dataset delete expected 404, got {status} with {body}"
        );

        // The name is free again: a fresh namespace of the same name starts empty, so the
        // delete removed the record rather than hiding it.
        let (status, body) = emulator.create_namespace(&instance_id, &namespace).await;
        assert_eq!(
            status, 200,
            "case {case}: the namespace name is free again, got {status} with {body}"
        );
        let (status, _error_type, body) = emulator.read_dataset(&instance_id, &namespace, &name).await;
        assert_eq!(
            status, 404,
            "case {case}: a re-created namespace carries none of the removed datasets, got \
             {status} with {body}"
        );
    }
}

