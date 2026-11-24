// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of `SendDataIntegrationEvent`, driven over the real wire path (a
//! restJson1 request through the multi-service router into the mounted service), so every
//! assertion is about what a client observes.
//!
//! RULES under test:
//!
//! * a send RECORDS the event: the `eventId` it answers names a stored event that the
//!   event reads then report, with every member the request carried — so a send a client
//!   was told succeeded is one a later read can find;
//! * the gateway authorizes the call against the addressed INSTANCE before it looks at
//!   anything the request carries, so an `instanceId` naming no instance of this account is
//!   `AccessDeniedException` (HTTP 403) and records nothing;
//! * an event whose `datasetTarget` names a dataset the instance's data lake does not hold
//!   has nothing to load into: it is refused `ResourceNotFoundException` (HTTP 404) and
//!   records nothing, while the identical send is accepted once that dataset exists;
//! * the operation is `@idempotent` with an `@idempotencyToken`, so a replayed
//!   `clientToken` answers with the event the first call recorded instead of recording a
//!   second one.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these cases sign for, and the account every ARN this emulator mints carries.
const REGION: &str = "us-east-1";
const ACCOUNT: &str = "000000000000";

/// The one event type that names its own target dataset ("scn.data.dataset - Send data
/// directly to any specified dataset", the `eventType` member's documentation).
const DATASET_EVENT_TYPE: &str = "scn.data.dataset";

/// The status an accepted event's dataset load reports, as the modelled
/// `DataIntegrationEventDatasetLoadStatus` spells it.
const LOAD_SUCCEEDED: &str = "SUCCEEDED";

/// The sentence a send is refused with when its `datasetTarget` names no dataset the
/// instance holds — written out HERE rather than read from the crate, because the wording
/// is the requirement.
fn target_not_valid(dataset_identifier: &str) -> String {
    format!("Target dataset resource arn: {dataset_identifier} is not valid")
}

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    /// Send one request and return its HTTP status and JSON body.
    async fn send(&self, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
        let body_bytes = match &body {
            Some(value) => Bytes::from(serde_json::to_vec(value).expect("serialize request body")),
            None => Bytes::new(),
        };
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

    /// A live instance of this account, returning its `instanceId`.
    async fn given_instance(&self) -> String {
        let (status, body) = self.send("POST", "/api/instance", Some(json!({}))).await;
        assert_eq!(
            status, 200,
            "CreateInstance expected 200, got {status} with {body}"
        );
        body["instance"]["instanceId"]
            .as_str()
            .unwrap_or_else(|| panic!("CreateInstance carried no instanceId: {body}"))
            .to_string()
    }

    /// A dataset of `instance_id`'s `namespace`, created through `CreateDataLakeDataset`.
    async fn given_dataset(&self, instance_id: &str, namespace: &str, name: &str) {
        let (status, body) = self
            .send(
                "PUT",
                &format!(
                    "/api/datalake/instance/{instance_id}/namespaces/{namespace}/datasets/{name}"
                ),
                Some(json!({
                    "schema": {
                        "name": "Shipment",
                        "fields": [{ "name": "id", "type": "STRING", "isRequired": true }],
                    },
                })),
            )
            .await;
        assert_eq!(
            status, 200,
            "CreateDataLakeDataset expected 200, got {status} with {body}"
        );
    }

    /// `SendDataIntegrationEvent` for one instance, with `request` as its body.
    async fn send_event(&self, instance_id: &str, request: Value) -> (u16, Value) {
        self.send(
            "POST",
            &format!(
                "/api-data/data-integration/instance/{instance_id}/data-integration-events"
            ),
            Some(request),
        )
        .await
    }

    /// `GetDataIntegrationEvent` for one (instance, event) pair.
    async fn get_event(&self, instance_id: &str, event_id: &str) -> (u16, Value) {
        self.send(
            "GET",
            &format!(
                "/api-data/data-integration/instance/{instance_id}/data-integration-events/\
                 {event_id}"
            ),
            None,
        )
        .await
    }

    /// The events `ListDataIntegrationEvents` reports for one instance, asserting the 200.
    async fn listed_events(&self, case: &str, instance_id: &str) -> Vec<Value> {
        let (status, body) = self
            .send(
                "GET",
                &format!(
                    "/api-data/data-integration/instance/{instance_id}/data-integration-events\
                     ?maxResults=20"
                ),
                None,
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: ListDataIntegrationEvents expected 200, got {status} with {body}"
        );
        body.get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: the page carries no `events` list: {body}"))
    }
}

/// The error code a refusal envelope carries.
fn error_code(body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The sentence a refusal envelope carries.
fn error_message(body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
        .to_string()
}

/// The ARN of one dataset, as this service mints it.
fn dataset_arn(instance_id: &str, namespace: &str, name: &str) -> String {
    format!(
        "arn:aws:scn:{REGION}:{ACCOUNT}:instance/{instance_id}/namespaces/{namespace}/datasets/\
         {name}"
    )
}

/// A `clientToken` of the modelled `ClientToken` length (33..126).
fn client_token(seed: &str) -> String {
    format!("{seed}{}", "0".repeat(33_usize.saturating_sub(seed.len())))
}

/// RULE: a send records the event, and the `eventId` it answers names a stored event the
/// reads report with every member the request carried.
#[tokio::test]
async fn a_sent_event_is_recorded_and_read_back() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance().await;

    // Rows span the shapes a send takes: several event types, a request that stamps its own
    // `eventTimestamp` and one that leaves the service to stamp it.
    let rows = [
        ("a shipment event, timestamped by the caller", "scn.data.shipment", Some(1_700_000_000)),
        ("a forecast event, timestamped by the caller", "scn.data.forecast", Some(1_700_000_500)),
        ("a supply plan event, stamped by the service", "scn.data.supplyplan", None),
    ];

    let mut sent: Vec<(String, String)> = Vec::new();
    for (case, event_type, timestamp) in rows {
        let mut request = json!({
            "eventType": event_type,
            "eventGroupId": format!("group-{event_type}"),
            "data": "{\"id\": \"one\"}",
        });
        if let Some(stamp) = timestamp {
            request["eventTimestamp"] = json!(stamp);
        }

        let (status, body) = emulator.send_event(&instance_id, request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: SendDataIntegrationEvent expected 200, got {status} with {body}"
        );
        let event_id = body
            .get("eventId")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: the send carried no eventId: {body}"))
            .to_string();
        assert_eq!(
            event_id.len(),
            36,
            "case {case:?}: the eventId must be of the modelled UUID shape, got {event_id:?}"
        );
        sent.push((event_id.clone(), event_type.to_string()));

        // The stored event is the one the listing reports, with the members the request
        // carried.
        let listed = emulator.listed_events(case, &instance_id).await;
        let event = listed
            .iter()
            .find(|event| event.get("eventId").and_then(Value::as_str) == Some(&event_id))
            .unwrap_or_else(|| panic!("case {case:?}: the sent event is not listed: {listed:?}"));

        // ...and the event READ answers the same record the listing does: one stored event,
        // reported identically by both readers.
        let (status, body) = emulator.get_event(&instance_id, &event_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetDataIntegrationEvent expected 200, got {status} with {body}"
        );
        assert_eq!(
            body.get("event"),
            Some(event),
            "case {case:?}: the read and the listing must report one record, got {body}"
        );
        assert_eq!(
            event.get("eventType").and_then(Value::as_str),
            Some(event_type),
            "case {case:?}: the stored eventType must be the one sent, got {event}"
        );
        assert_eq!(
            event.get("eventGroupId").and_then(Value::as_str),
            Some(format!("group-{event_type}").as_str()),
            "case {case:?}: the stored eventGroupId must be the one sent, got {event}"
        );
        match timestamp {
            // A request that stamped the event is answered with ITS stamp.
            Some(stamp) => assert_eq!(
                event.get("eventTimestamp").and_then(Value::as_i64),
                Some(stamp),
                "case {case:?}: the caller's eventTimestamp must be persisted, got {event}"
            ),
            // One that did not is stamped by the service, which is still a timestamp.
            None => assert!(
                event
                    .get("eventTimestamp")
                    .and_then(Value::as_f64)
                    .is_some_and(|stamp| stamp > 0.0),
                "case {case:?}: an unstamped event must be stamped by the service, got {event}"
            ),
        }
        // An event of an entity type names no target of its own, so the load it drove is
        // reported against the dataset the service keeps for that type: no
        // `datasetIdentifier`, because no client addresses that dataset, the APPEND
        // operation the load performs (a value of the modelled
        // `DataIntegrationEventDatasetOperationType`) and the load's terminal status.
        let details = event
            .get("datasetTargetDetails")
            .unwrap_or_else(|| panic!("case {case:?}: the event reports no load: {event}"));
        assert!(
            details.get("datasetIdentifier").is_none(),
            "case {case:?}: a load into a dataset no client addressed names none, got {details}"
        );
        assert_eq!(
            details.get("operationType").and_then(Value::as_str),
            Some("APPEND"),
            "case {case:?}: an entity event's load appends its records, got {details}"
        );
        assert_eq!(
            details
                .get("datasetLoadExecution")
                .and_then(|execution| execution.get("status"))
                .and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: an accepted event's load must report its terminal status, got \
             {details}"
        );
    }

    // Every send recorded its OWN event: the ids differ and the listing holds them all.
    let listed = emulator.listed_events("every send", &instance_id).await;
    assert_eq!(
        listed.len(),
        sent.len(),
        "case \"every send\": each send must record one event, got {listed:?}"
    );

    // An `eventId` the instance holds no event under is the read's own not-found, and an
    // instance that holds no such event is not told about another instance's.
    for (case, event_id) in [
        ("an eventId nothing recorded", "00000000-0000-4000-8000-000000000000".to_string()),
        ("the lowest eventId the shape admits", "00000000-0000-0000-0000-000000000000".to_string()),
    ] {
        let (status, body) = emulator.get_event(&instance_id, &event_id).await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected 404 ResourceNotFoundException, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            "Event not found.",
            "case {case:?}: the refusal must carry the fixed not-found sentence, got {body}"
        );
    }

    // An event of one instance is not readable under another instance's id.
    let other = emulator.given_instance().await;
    let (first_event, _) = sent.first().cloned().expect("a sent event");
    let (status, body) = emulator.get_event(&other, &first_event).await;
    assert_eq!(
        (status, error_code(&body).as_str()),
        (404, "ResourceNotFoundException"),
        "case \"another instance\": an event is addressed by its own instance, got {status} \
         with {body}"
    );
}

/// RULE: a replayed `clientToken` answers with the event the first call recorded, and
/// records no second event — while a different token records its own.
#[tokio::test]
async fn a_replayed_client_token_records_one_event() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance().await;
    let request = |token: &str| {
        json!({
            "eventType": "scn.data.shipment",
            "eventGroupId": "group-one",
            "data": "{\"id\": \"one\"}",
            "clientToken": token,
        })
    };

    let token = client_token("replayed");
    let (status, body) = emulator.send_event(&instance_id, request(&token)).await;
    assert_eq!(
        status, 200,
        "case \"the first send\": expected 200, got {status} with {body}"
    );
    let first = body["eventId"].as_str().expect("an eventId").to_string();

    for attempt in 1..=2 {
        let case = format!("replay {attempt}");
        let (status, body) = emulator.send_event(&instance_id, request(&token)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a replay expected 200, got {status} with {body}"
        );
        assert_eq!(
            body["eventId"].as_str(),
            Some(first.as_str()),
            "case {case:?}: a replayed token must answer with the first event, got {body}"
        );
        assert_eq!(
            emulator.listed_events(&case, &instance_id).await.len(),
            1,
            "case {case:?}: a replay must record no second event"
        );
    }

    let (status, body) = emulator
        .send_event(&instance_id, request(&client_token("another")))
        .await;
    assert_eq!(
        status, 200,
        "case \"another token\": expected 200, got {status} with {body}"
    );
    assert_ne!(
        body["eventId"].as_str(),
        Some(first.as_str()),
        "case \"another token\": a different token must record its own event, got {body}"
    );
    assert_eq!(
        emulator
            .listed_events("another token", &instance_id)
            .await
            .len(),
        2,
        "case \"another token\": the store must hold both events"
    );

    // The token is remembered per INSTANCE: an event belongs to the instance that was sent
    // it, and the token is a value the request chooses, so the SAME token presented for
    // another instance names no event of this one — it records that instance's own first
    // event, and leaves this instance's events alone.
    let other = emulator.given_instance().await;
    let (status, body) = emulator.send_event(&other, request(&token)).await;
    assert_eq!(
        status, 200,
        "case \"the same token, another instance\": expected 200, got {status} with {body}"
    );
    assert_ne!(
        body["eventId"].as_str(),
        Some(first.as_str()),
        "case \"the same token, another instance\": a token of one instance must not answer \
         with another instance's event, got {body}"
    );
    assert_eq!(
        emulator
            .listed_events("the same token, another instance", &other)
            .await
            .len(),
        1,
        "case \"the same token, another instance\": the send must record that instance's own \
         event"
    );
    assert_eq!(
        emulator
            .listed_events("the same token, another instance", &instance_id)
            .await
            .len(),
        2,
        "case \"the same token, another instance\": the first instance's events are untouched"
    );
}

/// RULE: an event whose `datasetTarget` names a dataset the instance's data lake does not
/// hold has nothing to load into — it is refused 404 and records nothing — while the
/// identical send is accepted once that dataset exists, and reports the load it drove.
#[tokio::test]
async fn an_event_targeting_a_dataset_that_does_not_exist_is_refused() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance().await;

    // Rows differ along WHY the target names no dataset: a dataset never created under a
    // namespace the instance has, one under a namespace it does not have, and an ARN of
    // another instance.
    let other_instance = "00000000-0000-4000-8000-000000000000";
    let rows = [
        (
            "a dataset that was never created",
            dataset_arn(&instance_id, "default", "never_created"),
        ),
        (
            "a dataset under a namespace that was never created",
            dataset_arn(&instance_id, "never_created", "shipments"),
        ),
        (
            "a dataset of another instance",
            dataset_arn(other_instance, "default", "shipments"),
        ),
    ];

    for (case, identifier) in &rows {
        let (status, body) = emulator
            .send_event(
                &instance_id,
                json!({
                    "eventType": DATASET_EVENT_TYPE,
                    "eventGroupId": format!("group-{case}"),
                    "data": "{\"id\": \"one\"}",
                    "datasetTarget": {
                        "datasetIdentifier": identifier,
                        "operationType": "APPEND",
                    },
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: expected 404 ResourceNotFoundException, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            target_not_valid(identifier),
            "case {case:?}: the refusal must name the target ARN the request carried, got {body}"
        );
    }

    assert!(
        emulator
            .listed_events("the refused sends", &instance_id)
            .await
            .is_empty(),
        "case \"the refused sends\": a refused send must record nothing"
    );

    // The identical send is accepted once the dataset exists, and the event reports the
    // load it drove into that dataset.
    emulator
        .given_dataset(&instance_id, "default", "never_created")
        .await;
    let identifier = dataset_arn(&instance_id, "default", "never_created");
    let (status, body) = emulator
        .send_event(
            &instance_id,
            json!({
                "eventType": DATASET_EVENT_TYPE,
                "eventGroupId": "group-accepted",
                "data": "{\"id\": \"one\"}",
                "datasetTarget": {
                    "datasetIdentifier": identifier,
                    "operationType": "APPEND",
                },
            }),
        )
        .await;
    assert_eq!(
        status, 200,
        "case \"the dataset now exists\": expected 200, got {status} with {body}"
    );
    let event_id = body["eventId"].as_str().expect("an eventId").to_string();
    let listed = emulator
        .listed_events("the dataset now exists", &instance_id)
        .await;
    let event = listed
        .iter()
        .find(|event| event.get("eventId").and_then(Value::as_str) == Some(&event_id))
        .unwrap_or_else(|| panic!("the accepted event is not listed: {listed:?}"));
    assert_eq!(
        event["datasetTargetDetails"]["datasetIdentifier"].as_str(),
        Some(identifier.as_str()),
        "case \"the dataset now exists\": the stored target must be the one sent, got {event}"
    );
    assert_eq!(
        event["datasetTargetDetails"]["operationType"].as_str(),
        Some("APPEND"),
        "case \"the dataset now exists\": the stored operation type must be the one sent, got \
         {event}"
    );
    assert_eq!(
        event["datasetTargetDetails"]["datasetLoadExecution"]["status"].as_str(),
        Some(LOAD_SUCCEEDED),
        "case \"the dataset now exists\": the load of an accepted event must be reported, got \
         {event}"
    );
}

/// RULE: an `instanceId` naming no instance of this account is refused by the authorization
/// step — before anything the request carries is looked at — and records nothing.
#[tokio::test]
async fn a_send_to_an_unknown_instance_is_an_authorization_failure() {
    let emulator = Emulator::new();
    let live = emulator.given_instance().await;

    // Rows differ along the id: ids of the modelled shape that no instance holds.
    for (case, instance_id) in [
        ("the lowest id the shape admits", "00000000-0000-0000-0000-000000000000"),
        ("an id nothing created", "d0e75531-eb2d-4a3b-a1bf-8dd8e148e377"),
    ] {
        let (status, body) = emulator
            .send_event(
                instance_id,
                json!({
                    "eventType": "scn.data.shipment",
                    "eventGroupId": "group-one",
                    "data": "{\"id\": \"one\"}",
                }),
            )
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (403, "AccessDeniedException"),
            "case {case:?}: expected 403 AccessDeniedException, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            format!(
                "Authorization Failed (Public). The instanceId {instance_id} does not exist, or \
                 does not belong to the account {ACCOUNT}"
            ),
            "case {case:?}: the refusal must carry the gateway's authorization sentence, got \
             {body}"
        );
    }

    assert!(
        emulator.listed_events("the live instance", &live).await.is_empty(),
        "case \"the live instance\": a refused send must record nothing anywhere"
    );
}
