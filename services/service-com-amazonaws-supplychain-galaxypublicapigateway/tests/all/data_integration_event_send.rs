// SPDX-License-Identifier: CC0-1.0

//! `SendDataIntegrationEvent`'s dataset target, driven over the wire (restJson1, signing
//! name `scn`) through the same `MultiServiceRouter` the emulator's front door uses — so
//! every assertion here is about what a real client observes, not about a handler's
//! internals.
//!
//! THE RULE UNDER TEST: a `datasetTarget` is a reference to a dataset the data lake must
//! already hold, so the send resolves it against persisted state BEFORE it records
//! anything — an identifier that resolves to no such dataset is refused
//! `ResourceNotFoundException` (404) with the service's "Target dataset resource arn:
//! <arn> is not valid", carrying the ARN the request sent and nothing else, and no event
//! is recorded for it; once the identifier resolves, the send records exactly one event
//! and every read answers from that record. An event of any OTHER type names no target of
//! its own, because the service keeps one dataset per entity type for it, so its event
//! reports the load into THAT dataset. An `instanceId` naming no instance of this
//! account is refused by the gateway's authorization step before the target is looked up
//! at all, and an `eventId` an instance holds no event under is a not-found.
//!
//! The tables span what the rule generalises over: the namespace the target names
//! (pre-defined, created, never created), the dataset name, every value of the
//! `operationType` enum, a dataset the instance held and deleted, the payload form (one
//! record as a JSON object, several as a JSON array), whether the send stamped its own
//! `eventTimestamp`, and which instance an `eventId` belongs to.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::ops::types::{
    DATA_INTEGRATION_EVENT_DATASET_OPERATION_TYPE_VALUES, DATA_INTEGRATION_EVENT_TYPE_VALUES,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::state::{
    is_modelled_uuid, service_state, DATASET_LOAD_STATUS_SUCCEEDED,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these cases sign for, and so the region every ARN they mint carries.
const REGION: &str = "us-east-1";

/// The `eventType` whose payload goes to a dataset the request names (source: the
/// `eventType` member's documentation, "scn.data.dataset - Send data directly to any
/// specified dataset").
const DATASET_EVENT_TYPE: &str = "scn.data.dataset";

/// The sentence a send whose `datasetTarget` resolves to no dataset is refused with,
/// written out HERE rather than read from the crate: the wording is the requirement, so a
/// drift in the function the handler answers from fails this suite instead of travelling
/// with it. Its SOURCE is the recorded wire response of such a send; the only variable
/// part is the ARN the request carried.
fn target_not_valid_sentence(dataset_identifier: &str) -> String {
    format!("Target dataset resource arn: {dataset_identifier} is not valid")
}

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
    /// The SAME engine the service was built from, so a case can read the store back and
    /// assert what a refused send left behind as well as what it reported.
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Keep the test independent of how this process was launched: no authenticator,
        // so a request routes on its credential scope alone.
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
    }

    /// How many event records the store holds for `instance_id`.
    fn stored_events(&self, instance_id: &str) -> usize {
        service_state(&*self.engine)
            .expect("the supplychain store is reachable on this engine")
            .lock()
            .data_integration_event_count(instance_id)
    }

    /// Send one request and return its HTTP status and JSON body.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
        // A SigV4 `Authorization` header whose credential scope names this service
        // (`scn`), which is how the router picks the service that claims the request.
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
        let parsed = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or_else(|error| {
                panic!(
                    "{method} {path}: body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&bytes)
                )
            })
        };
        (status, parsed)
    }

    /// A live instance of this account, created through `CreateInstance`.
    async fn given_instance(&self, case: &str) -> String {
        let (status, body) = self.send("POST", "/api/instance", Some(json!({}))).await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreateInstance expected 200, got {status} with {body}"
        );
        body.get("instance")
            .and_then(|instance| instance.get("instanceId"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: CreateInstance carried no instanceId: {body}"))
            .to_string()
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
            "case {case:?}: CreateDataLakeNamespace expected 200, got {status} with {body}"
        );
    }

    /// `CreateDataLakeDataset` for one triple, returning the ARN the create reported —
    /// the identifier a `datasetTarget` names that dataset by.
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
            "case {case:?}: CreateDataLakeDataset expected 200, got {status} with {body}"
        );
        body.get("dataset")
            .and_then(|dataset| dataset.get("arn"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: the create carried no dataset arn: {body}"))
            .to_string()
    }

    /// `DeleteDataLakeDataset` for one triple.
    async fn delete_dataset(&self, instance_id: &str, namespace: &str, name: &str) -> (u16, Value) {
        self.send(
            "DELETE",
            &format!("/api/datalake/instance/{instance_id}/namespaces/{namespace}/datasets/{name}"),
            None,
        )
        .await
    }

    /// `SendDataIntegrationEvent` with the request body `request`.
    async fn send_event(&self, instance_id: &str, request: Value) -> (u16, Value) {
        self.send("POST", &events_path(instance_id), Some(request))
            .await
    }

    /// `GetDataIntegrationEvent` for one `eventId`.
    async fn get_event(&self, instance_id: &str, event_id: &str) -> (u16, Value) {
        self.send(
            "GET",
            &format!("{}/{event_id}", events_path(instance_id)),
            None,
        )
        .await
    }

    /// `ListDataIntegrationEvents` for one instance.
    async fn list_events(&self, instance_id: &str) -> (u16, Value) {
        self.send("GET", &events_path(instance_id), None).await
    }
}

/// The URI `SendDataIntegrationEvent` and `ListDataIntegrationEvents` bind.
fn events_path(instance_id: &str) -> String {
    format!("/api-data/data-integration/instance/{instance_id}/data-integration-events")
}

/// The ARN form `DataIntegrationDatasetArn` fixes
/// (`^arn:aws:scn:([a-z0-9-]+):([0-9]+):instance/<uuid>/namespaces/[^/]+/datasets/[^/]+$`),
/// built for a triple the data lake need not hold — which is how a case names a target
/// that resolves to nothing.
fn dataset_arn(instance_id: &str, namespace: &str, name: &str) -> String {
    format!(
        "arn:aws:scn:{REGION}:{}:instance/{instance_id}/namespaces/{namespace}/datasets/{name}",
        engine::emulator_account_id()
    )
}

/// The `message` member of a refusal, as a client reads it.
fn message_of(case: &str, body: &Value) -> String {
    body.get("message")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no `message`: {body}"))
        .to_string()
}

/// The wire error code of a refusal (`__type` on restJson1).
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the refusal carries no `__type`: {body}"))
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `event` member of an event read, which the model declares `@required`.
fn event_of(case: &str, body: &Value) -> Value {
    body.get("event")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: the response carries no `event`: {body}"))
}

/// The `eventId` a send answers with, checked against the modelled `UUID` shape.
fn event_id_of(case: &str, body: &Value) -> String {
    let event_id = body
        .get("eventId")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the send carried no `eventId`: {body}"))
        .to_string();
    assert!(
        is_modelled_uuid(&event_id),
        "case {case:?}: the eventId must be a modelled UUID, got {event_id:?}"
    );
    event_id
}

/// One record, in the `JsonObject` form the `data` member documents for a single record.
fn one_record() -> Value {
    json!({ "id": "sim-1" })
}

/// The `datasetTargetDetails.datasetLoadExecution.status` of one event.
fn load_status(case: &str, event: &Value) -> String {
    event
        .get("datasetTargetDetails")
        .and_then(|details| details.get("datasetLoadExecution"))
        .and_then(|execution| execution.get("status"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: the event carries no load status: {event}"))
        .to_string()
}

/// RULE: a `datasetTarget` whose `datasetIdentifier` resolves to no dataset of the data
/// lake is refused `ResourceNotFoundException` (404) with the service's sentence carrying
/// that ARN verbatim, and the send records nothing — so the event is absent from both the
/// listing and the store.
#[tokio::test]
async fn a_target_naming_no_dataset_is_refused_and_records_nothing() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("missing target").await;
    emulator
        .given_namespace("missing target", &instance, "orders")
        .await;

    // A dataset of THIS instance that is created and then deleted: its ARN is a target
    // the store no longer resolves.
    let deleted_arn = emulator
        .given_dataset("deleted dataset", &instance, "default", "gone")
        .await;
    let (status, body) = emulator.delete_dataset(&instance, "default", "gone").await;
    assert_eq!(
        status, 200,
        "the dataset must be deleted first, got {status} with {body}"
    );

    // Rows differing along the namespace the target names (one the instance comes with,
    // one a caller created, one nobody created), the dataset name, and the
    // `operationType` — every value of the modelled enum.
    let mut rows: Vec<(String, String, &str)> = Vec::new();
    for (namespace_case, namespace, name) in [
        ("pre-defined namespace", "default", "never_created"),
        ("the other pre-defined namespace", "asc", "inbound_order"),
        ("a namespace this caller created", "orders", "lines_2026"),
        ("a namespace nobody created", "shipments", "l"),
    ] {
        for operation_type in DATA_INTEGRATION_EVENT_DATASET_OPERATION_TYPE_VALUES.iter().copied()
        {
            rows.push((
                format!("{namespace_case} / {operation_type}"),
                dataset_arn(&instance, namespace, name),
                operation_type,
            ));
        }
    }
    rows.push((
        "a dataset this instance deleted".to_string(),
        deleted_arn,
        "APPEND",
    ));

    for (case, arn, operation_type) in rows {
        let group = format!("group-{case}");
        let (status, body) = emulator
            .send_event(
                &instance,
                json!({
                    "eventType": DATASET_EVENT_TYPE,
                    "eventGroupId": group,
                    "data": one_record().to_string(),
                    "datasetTarget": {
                        "datasetIdentifier": arn,
                        "operationType": operation_type,
                    },
                }),
            )
            .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} with {body}"
        );
        assert_eq!(
            error_code(&case, &body),
            "ResourceNotFoundException",
            "case {case:?}: expected a ResourceNotFoundException envelope, got {body}"
        );
        let expected = target_not_valid_sentence(&arn);
        let message = message_of(&case, &body);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );

        // The refusal recorded nothing: no event of this instance names the group.
        let (status, body) = emulator.list_events(&instance).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListDataIntegrationEvents expected 200, got {status} with {body}"
        );
        assert!(
            !body.to_string().contains(group.as_str()),
            "case {case:?}: the refused send must record no event, got {body}"
        );
        assert_eq!(
            emulator.stored_events(&instance),
            0,
            "case {case:?}: the refused send must store no event"
        );
    }
}

/// One accepted send: the dataset its target names, the load operation, the payload form
/// and the `eventTimestamp` the send stamped (or none, so the service assigns one).
struct AcceptedSend {
    case: &'static str,
    namespace: &'static str,
    name: &'static str,
    operation_type: &'static str,
    data: Value,
    stamp: Option<f64>,
}

/// RULE: once the target resolves, the send records the event and answers with its
/// `eventId`, and the reads report exactly what it persisted — which is also what makes
/// the same send acceptable after the dataset is created.
#[tokio::test]
async fn a_send_against_a_dataset_the_instance_holds_is_recorded_and_read_back() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("recorded send").await;
    emulator
        .given_namespace("recorded send", &instance, "orders")
        .await;

    // Rows differing along the namespace, the dataset name, the `operationType`, the
    // payload form (one record as a JSON object, several as a JSON array) and whether the
    // send stamped its own `eventTimestamp`.
    let rows = vec![
        AcceptedSend {
            case: "one record, created namespace, APPEND, no stamp",
            namespace: "orders",
            name: "lines",
            operation_type: "APPEND",
            data: one_record(),
            stamp: None,
        },
        AcceptedSend {
            case: "several records, pre-defined namespace, UPSERT, stamped",
            namespace: "default",
            name: "l",
            operation_type: "UPSERT",
            data: json!([{ "id": "sim-1" }, { "id": "sim-2" }]),
            stamp: Some(1_767_225_600.0),
        },
        AcceptedSend {
            case: "one record, the other pre-defined namespace, DELETE, stamped",
            namespace: "asc",
            name: "inbound_order",
            operation_type: "DELETE",
            data: one_record(),
            stamp: Some(1_700_000_000.0),
        },
    ];

    for AcceptedSend {
        case,
        namespace,
        name,
        operation_type,
        data,
        stamp,
    } in rows
    {
        // The identical send is refused while the dataset does not exist, which is the
        // other half of the rule: the target is resolved against stored state, not parsed.
        let arn = dataset_arn(&instance, namespace, name);
        let mut request = json!({
            "eventType": DATASET_EVENT_TYPE,
            "eventGroupId": format!("group-{name}"),
            "data": data.to_string(),
            "datasetTarget": {
                "datasetIdentifier": arn,
                "operationType": operation_type,
            },
        });
        if let Some(stamp) = stamp {
            request["eventTimestamp"] = json!(stamp);
        }
        let (status, body) = emulator.send_event(&instance, request.clone()).await;
        assert_eq!(
            status, 404,
            "case {case:?}: the send must be refused before the dataset exists, got {status} \
             with {body}"
        );

        let created = emulator
            .given_dataset(case, &instance, namespace, name)
            .await;
        assert_eq!(
            created, arn,
            "case {case:?}: the dataset's own ARN is the identifier a target names it by"
        );

        let (status, body) = emulator.send_event(&instance, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: SendDataIntegrationEvent expected 200, got {status} with {body}"
        );
        let event_id = event_id_of(case, &body);

        // The read answers from the stored record: every member the send supplied, and
        // the target it resolved.
        let (status, body) = emulator.get_event(&instance, &event_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDataIntegrationEvent expected 200, got {status} with {body}"
        );
        let event = event_of(case, &body);
        assert_eq!(
            (
                event.get("instanceId").and_then(Value::as_str),
                event.get("eventId").and_then(Value::as_str),
                event.get("eventType").and_then(Value::as_str),
                event.get("eventGroupId").and_then(Value::as_str),
            ),
            (
                Some(instance.as_str()),
                Some(event_id.as_str()),
                Some(DATASET_EVENT_TYPE),
                Some(format!("group-{name}").as_str()),
            ),
            "case {case:?}: the event must report what the send carried, got {event}"
        );
        let target = event
            .get("datasetTargetDetails")
            .unwrap_or_else(|| panic!("case {case:?}: the event carries no target: {event}"));
        assert_eq!(
            (
                target.get("datasetIdentifier").and_then(Value::as_str),
                target.get("operationType").and_then(Value::as_str),
            ),
            (Some(arn.as_str()), Some(operation_type)),
            "case {case:?}: the target must be the one the send named, got {target}"
        );
        assert_eq!(
            load_status(case, &event),
            DATASET_LOAD_STATUS_SUCCEEDED,
            "case {case:?}: the load of an accepted event reports its status, got {event}"
        );
        if let Some(stamp) = stamp {
            assert_eq!(
                event.get("eventTimestamp").and_then(Value::as_f64),
                Some(stamp),
                "case {case:?}: a stamped send keeps its eventTimestamp, got {event}"
            );
        } else {
            assert!(
                event.get("eventTimestamp").and_then(Value::as_f64).is_some(),
                "case {case:?}: an unstamped send is assigned a timestamp, got {event}"
            );
        }

        // A second read reports the same record: recording an event changes nothing a
        // later caller sees.
        let (_, again) = emulator.get_event(&instance, &event_id).await;
        assert_eq!(
            event_of(case, &again),
            event,
            "case {case:?}: a repeated read must report the same event, got {again}"
        );

        // And the listing reports the same event the read does.
        let (status, body) = emulator.list_events(&instance).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListDataIntegrationEvents expected 200, got {status} with {body}"
        );
        let listed = body
            .get("events")
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("case {case:?}: the listing carries no events: {body}"));
        assert!(
            listed
                .iter()
                .any(|event| event.get("eventId").and_then(Value::as_str)
                    == Some(event_id.as_str())),
            "case {case:?}: the recorded event must be listed, got {body}"
        );
    }

    assert_eq!(
        emulator.stored_events(&instance),
        3,
        "each accepted send records exactly one event"
    );
}

/// RULE: an event type that names no target of its own is loaded into the dataset the
/// service keeps for that type, so the send records it WITH that load — the APPEND operation
/// the load performs and the status it reached — and WITHOUT a `datasetIdentifier`, because
/// no client addresses that dataset. Every modelled `eventType` beside `scn.data.dataset`,
/// whose event names its target itself.
#[tokio::test]
async fn an_event_of_an_entity_type_reports_the_load_into_its_own_dataset() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("event types").await;

    // Every modelled event type beside the one that names its own target, so no type is left
    // to ride on another's row.
    let entity_types: Vec<&str> = DATA_INTEGRATION_EVENT_TYPE_VALUES
        .iter()
        .copied()
        .filter(|event_type| *event_type != DATASET_EVENT_TYPE)
        .collect();
    assert!(
        entity_types.len() > 1,
        "the modelled enum carries more than one non-dataset event type"
    );

    let mut recorded = 0;
    for event_type in entity_types {
        let (status, body) = emulator
            .send_event(
                &instance,
                json!({
                    "eventType": event_type,
                    "eventGroupId": format!("group-{event_type}"),
                    "data": one_record().to_string(),
                }),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {event_type:?}: expected 200, got {status} with {body}"
        );
        let event_id = event_id_of(event_type, &body);
        recorded += 1;

        let (status, body) = emulator.get_event(&instance, &event_id).await;
        assert_eq!(
            status, 200,
            "case {event_type:?}: GetDataIntegrationEvent expected 200, got {status} with {body}"
        );
        let event = event_of(event_type, &body);
        assert_eq!(
            event.get("eventType").and_then(Value::as_str),
            Some(event_type),
            "case {event_type:?}: the event must report the type it was sent as, got {event}"
        );
        let details = event.get("datasetTargetDetails").unwrap_or_else(|| {
            panic!("case {event_type:?}: the event reports no dataset load: {event}")
        });
        assert!(
            details.get("datasetIdentifier").is_none(),
            "case {event_type:?}: a load into a dataset no client addressed names none, got \
             {details}"
        );
        // `APPEND` of the modelled `DataIntegrationEventDatasetOperationType`: the load adds
        // the event's records to that dataset.
        assert_eq!(
            details.get("operationType").and_then(Value::as_str),
            Some(DATA_INTEGRATION_EVENT_DATASET_OPERATION_TYPE_VALUES[0]),
            "case {event_type:?}: an entity event's load appends, got {details}"
        );
        assert_eq!(
            load_status(event_type, &event),
            DATASET_LOAD_STATUS_SUCCEEDED,
            "case {event_type:?}: an accepted event's load must reach its terminal status, got \
             {event}"
        );
    }

    assert_eq!(
        emulator.stored_events(&instance),
        recorded,
        "every accepted send recorded exactly one event"
    );
}

/// RULE: a `GetDataIntegrationEvent` for an `eventId` the addressed instance holds no
/// event under is refused `ResourceNotFoundException` (404) — an `eventId` of another
/// instance is no event of this one.
#[tokio::test]
async fn reading_an_event_the_instance_does_not_hold_is_not_found() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("absent event").await;
    let other = emulator.given_instance("other instance").await;
    let arn = emulator
        .given_dataset("other instance", &other, "default", "lines")
        .await;
    let (status, body) = emulator
        .send_event(
            &other,
            json!({
                "eventType": DATASET_EVENT_TYPE,
                "eventGroupId": "group-other",
                "data": one_record().to_string(),
                "datasetTarget": { "datasetIdentifier": arn, "operationType": "APPEND" },
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "the other instance's send must succeed, got {status} with {body}"
    );
    let other_event = event_id_of("other instance", &body);

    // Rows: an id nobody minted, and one another instance's send minted.
    for (case, event_id) in [
        (
            "an eventId nobody minted",
            "11111111-2222-3333-4444-555555555555",
        ),
        ("an eventId of another instance", other_event.as_str()),
    ] {
        let (status, body) = emulator.get_event(&instance, event_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected 404, got {status} with {body}"
        );
        assert_eq!(
            error_code(case, &body),
            "ResourceNotFoundException",
            "case {case:?}: expected a ResourceNotFoundException envelope, got {body}"
        );
    }
}

/// RULE: a send whose `instanceId` names no instance of this account is refused by the
/// gateway's authorization step — before the target is looked up at all — so it answers
/// `AccessDeniedException` (403) and records nothing.
#[tokio::test]
async fn a_send_naming_an_unknown_instance_is_refused_by_authorization() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("authorization").await;
    let arn = emulator
        .given_dataset("authorization", &instance, "default", "lines")
        .await;

    // A well-formed `UUID` (the modelled shape) that names no instance of this account.
    let unknown = "11111111-2222-3333-4444-555555555555";
    let (status, body) = emulator
        .send_event(
            unknown,
            json!({
                "eventType": DATASET_EVENT_TYPE,
                "eventGroupId": "group-unknown",
                "data": one_record().to_string(),
                "datasetTarget": { "datasetIdentifier": arn, "operationType": "APPEND" },
            }),
        )
        .await;
    assert_eq!(
        status, 403,
        "case \"unknown instance\": expected 403, got {status} with {body}"
    );
    assert_eq!(
        error_code("unknown instance", &body),
        "AccessDeniedException",
        "case \"unknown instance\": expected an AccessDeniedException envelope, got {body}"
    );
    let message = message_of("unknown instance", &body);
    assert!(
        message.contains(unknown),
        "case \"unknown instance\": the gateway sentence must name the instanceId, got {message:?}"
    );
    assert_eq!(
        emulator.stored_events(unknown),
        0,
        "case \"unknown instance\": the refused send must record nothing"
    );
    assert_eq!(
        emulator.stored_events(&instance),
        0,
        "case \"unknown instance\": the refused send must record nothing for the real instance"
    );
}

/// The sentence a send whose `data` payload is not the JSON its member documents is refused
/// with, written out HERE rather than read from the crate: the wording is the requirement.
/// Its SOURCE is the recorded wire response of such a send — the whole sentence is fixed,
/// and no part of the payload (which is `@sensitive`) appears in it.
const MALFORMED_DATA_SENTENCE: &str =
    "Schema validation failed: Invalid JSON: Malformed JSON structure";

/// RULE: the `data` member carries records as JSON — "To send single data record, use
/// JsonObject format; to send multiple data records, use JsonArray format" — so a payload
/// that is not well-formed JSON, or is JSON that is neither an object nor an array, carries
/// no record at all: the send is refused `ValidationException` (400) with the service's
/// sentence and records nothing. A payload of either documented form is accepted.
#[tokio::test]
async fn a_payload_that_is_not_the_documented_json_is_refused() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("payload").await;

    // Rows spanning the forms a payload takes: both documented ones, JSON that is a scalar
    // rather than a record or a list of them, JSON truncated at each end, JSON with a
    // syntax error inside it, and text that is not JSON at all.
    let rows: Vec<(&str, &str, bool)> = vec![
        ("one record, the documented JsonObject form", "{\"id\": \"sim-1\"}", true),
        ("the empty record, still an object", "{}", true),
        (
            "several records, the documented JsonArray form",
            "[{\"id\": \"sim-1\"}, {\"id\": \"sim-2\"}]",
            true,
        ),
        ("the empty list, still an array", "[]", true),
        ("a JSON string, which is no record", "\"sim-1\"", false),
        ("a JSON number, which is no record", "42", false),
        ("a JSON boolean, which is no record", "true", false),
        ("JSON null, which is no record", "null", false),
        ("an object truncated at its end", "{\"id\": \"sim-1\"", false),
        ("an object truncated at its start", "\"id\": \"sim-1\"}", false),
        ("a list truncated at its end", "[{\"id\": \"sim-1\"}", false),
        ("an object with a missing value", "{\"id\": }", false),
        ("an object with an unquoted key", "{id: \"sim-1\"}", false),
        ("two records with no list around them", "{\"a\": 1} {\"b\": 2}", false),
        ("text that is not JSON at all", "id=sim-1", false),
        ("a single brace", "{", false),
    ];

    let mut accepted = 0;
    for (case, data, well_formed) in rows {
        let (status, body) = emulator
            .send_event(
                &instance,
                json!({
                    "eventType": "scn.data.inventorylevel",
                    "eventGroupId": format!("group-{accepted}"),
                    "data": data,
                }),
            )
            .await;
        if well_formed {
            assert_eq!(
                status, 200,
                "case {case:?}: a documented payload form expected 200, got {status} with {body}"
            );
            accepted += 1;
        } else {
            assert_eq!(
                status, 400,
                "case {case:?}: a payload carrying no record must be refused, got {status} with \
                 {body}"
            );
            assert_eq!(
                error_code(case, &body),
                "ValidationException",
                "case {case:?}: expected a ValidationException envelope, got {body}"
            );
            assert_eq!(
                message_of(case, &body),
                MALFORMED_DATA_SENTENCE,
                "case {case:?}: the refusal must carry the service's sentence, got {body}"
            );
        }
        // Only the accepted sends recorded anything, so a refused payload leaves the store
        // exactly as it was.
        assert_eq!(
            emulator.stored_events(&instance),
            accepted,
            "case {case:?}: only an accepted send records an event"
        );
    }
}
