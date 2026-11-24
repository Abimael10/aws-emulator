// SPDX-License-Identifier: CC0-1.0

//! End-to-end behaviour of `ListDataIntegrationEvents`, driven over the wire (restJson1,
//! signing name `scn`) through the same `MultiServiceRouter` the emulator's front door
//! uses — so every assertion here is about what a real client observes, not about a
//! handler's internals.
//!
//! RULE under test: the listing reports exactly the data-integration events the store
//! holds for the addressed instance — every stored member of each one — in one stable
//! order, keeping only the ones whose `eventType` the request named, cut into pages of at
//! most `maxResults` with an opaque `nextToken` that resumes where the page stopped; an
//! `instanceId` naming no instance of this account is an authorization failure, a
//! `nextToken` the service did not issue is a validation failure, and the listing changes
//! nothing a later caller sees.
//!
//! The events the cases list are seeded into the shared store through the same
//! `state::service_state` accessor the handlers read it through: `SendDataIntegrationEvent`
//! — the operation that writes an event — is still a scaffold with its own issue, so the
//! store is the only producer available to a test of the READ. The listing therefore
//! answers from persisted records, which is what every assertion below pins.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::ops::types::{
    DataIntegrationEventDatasetLoadExecutionDetails, DataIntegrationEventDatasetTargetDetails,
    DATA_INTEGRATION_EVENT_DATASET_LOAD_STATUS_VALUES,
    DATA_INTEGRATION_EVENT_DATASET_OPERATION_TYPE_VALUES, DATA_INTEGRATION_EVENT_TYPE_VALUES,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::state::{
    data_integration_event_key, service_state, DataIntegrationEventRecord,
    MAX_DATA_INTEGRATION_EVENTS_PER_INSTANCE,
};
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region these cases sign for.
const REGION: &str = "us-east-1";

/// The published bounds of `DataIntegrationEventMaxResults` (`@range(min: 1, max: 20)`).
const MAX_RESULTS_MIN: i64 = 1;
const MAX_RESULTS_MAX: i64 = 20;

/// The `@default` the model gives `maxResults`, applied when a request omits it.
const MAX_RESULTS_DEFAULT: usize = 10;

/// How long a poll loop waits for the condition it is about before giving up, and how long
/// it waits between looks.
///
/// Generous on purpose: the loop ends as soon as the condition holds, so the deadline is
/// only there to end a case that can never satisfy it and never a budget the host has to
/// beat.
const POLL_DEADLINE: Duration = Duration::from_secs(30);
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// The emulator under test: one engine, one service, driven through the router.
struct Emulator {
    router: MultiServiceRouter,
    /// The SAME engine the service was built from, so a case can seed the event store
    /// and read it back the way a handler does.
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
    async fn given_instance(&self, name: &str) -> String {
        let (status, body) = self
            .send("POST", "/api/instance", Some(json!({ "instanceName": name })))
            .await;
        assert_eq!(
            status, 200,
            "CreateInstance expected 200, got {status} with {body}"
        );
        body.get("instance")
            .and_then(|instance| instance.get("instanceId"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("CreateInstance carried no instanceId: {body}"))
            .to_string()
    }

    /// Persist one data-integration event for `instance_id`, as the send that received it
    /// would have.
    fn given_event(&self, instance_id: &str, event: &SeededEvent) {
        let record = DataIntegrationEventRecord {
            instance_id: instance_id.to_string(),
            event_id: event.event_id.clone(),
            event_type: event.event_type.clone(),
            event_group_id: event.event_group_id.clone(),
            event_timestamp: SystemTime::UNIX_EPOCH + Duration::from_secs(event.timestamp_secs),
            dataset_target_details: event.dataset_target_details.clone(),
        };
        service_state(&*self.engine)
            .expect("the supplychain store is reachable on this engine")
            .lock()
            .events
            .insert(
                data_integration_event_key(instance_id, &record.event_id),
                record,
            );
    }

    /// How many events the store holds for `instance_id`.
    fn stored_events(&self, instance_id: &str) -> usize {
        service_state(&*self.engine)
            .expect("the supplychain store is reachable on this engine")
            .lock()
            .data_integration_event_count(instance_id)
    }

    /// Fill `instance_id`'s event store up to `target` records, returning how many it
    /// then holds.
    fn given_events_up_to(&self, instance_id: &str, target: usize) -> usize {
        let mut index = 0;
        while self.stored_events(instance_id) < target {
            index += 1;
            self.given_event(instance_id, &SeededEvent::new(index, "scn.data.shipment"));
        }
        self.stored_events(instance_id)
    }

    /// Whether the store reports `instance_id`'s events at the per-instance cap — the
    /// predicate the one site that adds an event checks before inserting.
    fn events_at_capacity(&self, instance_id: &str) -> bool {
        service_state(&*self.engine)
            .expect("the supplychain store is reachable on this engine")
            .lock()
            .data_integration_events_at_capacity(instance_id)
    }

    /// `ListDataIntegrationEvents` for `instance_id`, with `query` appended verbatim.
    async fn list_events(&self, instance_id: &str, query: &str) -> (u16, Value) {
        let path =
            format!("/api-data/data-integration/instance/{instance_id}/data-integration-events");
        let path = if query.is_empty() {
            path
        } else {
            format!("{path}?{query}")
        };
        self.send("GET", &path, None).await
    }
}

/// One event a case seeds into the store before listing it back.
#[derive(Debug, Clone)]
struct SeededEvent {
    event_id: String,
    event_type: String,
    event_group_id: String,
    timestamp_secs: u64,
    dataset_target_details: Option<DataIntegrationEventDatasetTargetDetails>,
}

impl SeededEvent {
    fn new(index: u64, event_type: &str) -> Self {
        Self {
            event_id: event_id(index),
            event_type: event_type.to_string(),
            event_group_id: format!("group-{index}"),
            // Distinct, and deliberately DESCENDING as the id ascends, so a case that
            // asserts the listing's order cannot pass by accident on timestamp order.
            timestamp_secs: 1_900_000_000 - index,
            dataset_target_details: None,
        }
    }

    /// The same event, carrying the dataset target details only an event targeting a
    /// data-lake dataset has.
    fn with_dataset_target(mut self, instance_id: &str, namespace: &str, dataset: &str) -> Self {
        self.dataset_target_details = Some(DataIntegrationEventDatasetTargetDetails {
            dataset_identifier: Some(format!(
                "arn:aws:scn:{REGION}:{}:instance/{instance_id}/namespaces/{namespace}/datasets/{dataset}",
                engine::emulator_account_id()
            )),
            // The first entry of each modelled enum's value list, so the case names a
            // value the model admits rather than a string of its own.
            operation_type: DATA_INTEGRATION_EVENT_DATASET_OPERATION_TYPE_VALUES[0].to_string(),
            dataset_load_execution: DataIntegrationEventDatasetLoadExecutionDetails {
                status: DATA_INTEGRATION_EVENT_DATASET_LOAD_STATUS_VALUES[0].to_string(),
                message: Some("loaded".to_string()),
            },
        });
        self
    }
}

/// A distinct `eventId` of the modelled `UUID` form
/// (`^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`) for `index`, built
/// so that ascending `index` is ascending `eventId`.
fn event_id(index: u64) -> String {
    format!("{index:08x}-0000-4000-8000-{index:012x}")
}

/// Look again until the deletion of `instance_id` has SETTLED — until `GetInstance`
/// answers 404 — or until [`POLL_DEADLINE`].
///
/// `DeleteInstance` ACCEPTS a deletion: the instance moves to `Deleting` and stays
/// addressable, with the events addressed by it, while the deletion settles; the removal
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

/// The `events` member of a success body, as a list.
fn events_of(case: &str, body: &Value) -> Vec<Value> {    body.get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: response carries no `events` list: {body}"))
}

/// One string member of a listed event.
fn string_member(case: &str, event: &Value, member: &str) -> String {
    event
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: event carries no string `{member}`: {event}"))
        .to_string()
}

/// One numeric member of a listed event — restJson1 renders a `Timestamp` as epoch
/// seconds, a JSON number.
fn number_member(case: &str, event: &Value, member: &str) -> f64 {
    event
        .get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: event carries no numeric `{member}`: {event}"))
}

/// The `eventId` of every entry of a success body's `events`, in the order answered.
fn listed_ids(case: &str, body: &Value) -> Vec<String> {
    events_of(case, body)
        .iter()
        .map(|event| string_member(case, event, "eventId"))
        .collect()
}

/// The `nextToken` a success body carries, if any.
fn next_token(body: &Value) -> Option<String> {
    body.get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// The error code of a refusal envelope (restJson1 renders it in `__type`).
fn error_code(case: &str, body: &Value) -> String {
    body.get("__type")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: refusal carries no `__type`: {body}"))
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .to_string()
}

/// The `message` of a refusal envelope.
fn error_message(case: &str, body: &Value) -> String {
    body.get("message")
        .or_else(|| body.get("Message"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: refusal carries no message: {body}"))
        .to_string()
}

/// How the gateway refuses a call whose `instanceId` names no instance of this account.
fn expected_authorization_failure(instance_id: &str) -> String {
    format!(
        "Authorization Failed (Public). The instanceId {instance_id} does not exist, or does not \
         belong to the account {}",
        engine::emulator_account_id()
    )
}

/// Walk every page of one listing and return the ids answered, in order, together with
/// how many pages it took.
async fn walk_pages(
    emulator: &Emulator,
    case: &str,
    instance_id: &str,
    filter: Option<&str>,
    max_results: Option<i64>,
) -> (Vec<String>, usize) {
    let limit = match max_results {
        Some(size) => usize::try_from(size).expect("a page size a case asks for is positive"),
        None => MAX_RESULTS_DEFAULT,
    };
    let mut ids = Vec::new();
    let mut pages = 0;
    let mut token: Option<String> = None;
    // A listing of a bounded store terminates in a bounded number of pages; the cap is
    // here so a token that failed to advance fails the case instead of looping forever.
    while pages < 64 {
        let mut query: Vec<String> = Vec::new();
        if let Some(event_type) = filter {
            query.push(format!("eventType={event_type}"));
        }
        if let Some(size) = max_results {
            query.push(format!("maxResults={size}"));
        }
        if let Some(value) = &token {
            query.push(format!("nextToken={value}"));
        }
        let (status, body) = emulator.list_events(instance_id, &query.join("&")).await;
        assert_eq!(
            status, 200,
            "case {case:?}: page {pages} expected 200, got {status} with {body}"
        );
        let page = listed_ids(case, &body);
        assert!(
            page.len() <= limit,
            "case {case:?}: page {pages} holds {} events, more than maxResults {limit}",
            page.len()
        );
        pages += 1;
        token = next_token(&body);
        if token.is_some() {
            assert!(
                !page.is_empty(),
                "case {case:?}: page {pages} is empty yet carries a nextToken, so a client \
                 paging in a loop could never advance"
            );
        }
        ids.extend(page);
        if token.is_none() {
            return (ids, pages);
        }
    }
    panic!("case {case:?}: the listing never ran out of pages after {pages} pages");
}

/// RULE: an `eventType` outside the values the model's `DataIntegrationEventType` enum
/// admits is a bad request — never a successful, empty page.
#[tokio::test]
async fn an_event_type_outside_the_modelled_enum_is_refused() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("enum-host").await;
    emulator.given_event(&instance, &SeededEvent::new(1, "scn.data.shipment"));

    // Rows differ along HOW the value misses the enum: a plausible type the model does
    // not declare, a declared value in the wrong case, and a bare suffix of one.
    let rows: Vec<(&str, &str)> = vec![
        ("a type the model does not declare", "scn.data.bogus"),
        ("a declared value upper-cased", "SCN.DATA.DATASET"),
        ("a bare suffix of a declared value", "dataset"),
    ];

    for (case, event_type) in rows {
        let (status, body) = emulator
            .list_events(&instance, &format!("eventType={event_type}"))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: eventType {event_type:?} expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(case, &body),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {body}"
        );
    }
}

/// RULE: the listing reports every event the store holds for the addressed instance, with
/// every stored member projected onto the modelled `DataIntegrationEvent` shape, in one
/// stable order — and nothing belonging to another instance.
#[tokio::test]
async fn every_stored_event_of_the_addressed_instance_is_reported_exactly_as_persisted() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("event-host").await;
    let other_instance = emulator.given_instance("other-event-host").await;

    // Rows differ along the event type, the group id, the timestamp and whether the event
    // carries `datasetTargetDetails` — the member the model makes optional.
    let seeded = vec![
        SeededEvent::new(1, "scn.data.inboundorder"),
        SeededEvent::new(2, "scn.data.dataset").with_dataset_target(
            &instance,
            "default",
            "my_dataset",
        ),
        SeededEvent::new(3, "scn.data.shipmentstop"),
    ];
    for event in &seeded {
        emulator.given_event(&instance, event);
    }
    // One event of the OTHER instance, which this listing must never report.
    emulator.given_event(
        &other_instance,
        &SeededEvent::new(4, "scn.data.inboundorder"),
    );

    let case = "every stored event";
    let (status, body) = emulator
        .list_events(&instance, &format!("maxResults={MAX_RESULTS_MAX}"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {body}"
    );
    assert_eq!(
        next_token(&body),
        None,
        "case {case:?}: one page holds every event, so it must carry no nextToken: {body}"
    );

    let listed = events_of(case, &body);
    assert_eq!(
        listed.len(),
        seeded.len(),
        "case {case:?}: expected {} events, got {}: {body}",
        seeded.len(),
        listed.len()
    );

    for (expected, actual) in seeded.iter().zip(listed.iter()) {
        let case = format!("{case}: {}", expected.event_id);
        assert_eq!(
            string_member(&case, actual, "instanceId"),
            instance,
            "case {case:?}: the event must name the instance it was stored under: {actual}"
        );
        assert_eq!(
            string_member(&case, actual, "eventId"),
            expected.event_id,
            "case {case:?}: wrong eventId: {actual}"
        );
        assert_eq!(
            string_member(&case, actual, "eventType"),
            expected.event_type,
            "case {case:?}: wrong eventType: {actual}"
        );
        assert_eq!(
            string_member(&case, actual, "eventGroupId"),
            expected.event_group_id,
            "case {case:?}: wrong eventGroupId: {actual}"
        );
        assert_eq!(
            number_member(&case, actual, "eventTimestamp"),
            expected.timestamp_secs as f64,
            "case {case:?}: wrong eventTimestamp: {actual}"
        );
        match &expected.dataset_target_details {
            Some(target) => {
                let details = actual.get("datasetTargetDetails").unwrap_or_else(|| {
                    panic!("case {case:?}: event carries no datasetTargetDetails: {actual}")
                });
                assert_eq!(
                    Some(string_member(&case, details, "datasetIdentifier")),
                    target.dataset_identifier,
                    "case {case:?}: wrong datasetIdentifier: {details}"
                );
                assert_eq!(
                    string_member(&case, details, "operationType"),
                    target.operation_type,
                    "case {case:?}: wrong operationType: {details}"
                );
                let execution = details.get("datasetLoadExecution").unwrap_or_else(|| {
                    panic!("case {case:?}: no datasetLoadExecution: {details}")
                });
                assert_eq!(
                    string_member(&case, execution, "status"),
                    target.dataset_load_execution.status,
                    "case {case:?}: wrong load status: {execution}"
                );
                assert_eq!(
                    Some(string_member(&case, execution, "message")),
                    target.dataset_load_execution.message,
                    "case {case:?}: wrong load message: {execution}"
                );
            }
            None => assert!(
                actual.get("datasetTargetDetails").is_none(),
                "case {case:?}: an event stored without dataset target details must carry \
                 none: {actual}"
            ),
        }
    }

    // The order is the store's key order (`eventId`), not the order the timestamps run
    // in: the seeded timestamps descend as the ids ascend.
    let ids = listed_ids(case, &body);
    let mut sorted = ids.clone();
    sorted.sort();
    assert_eq!(
        ids, sorted,
        "case {case:?}: expected the events in ascending eventId order, got {ids:?}"
    );
}

/// RULE: `eventType` filters the listing — every event of the named type and no other —
/// for every value the model's `DataIntegrationEventType` enum admits.
#[tokio::test]
async fn the_event_type_filter_keeps_exactly_the_events_of_that_type() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("filter-host").await;

    // The catalog comes from the `DataIntegrationEventType` enum of this service's model
    // (`smithy_query(query="shape", shape="DataIntegrationEventType")`), re-exported by
    // the crate as `DATA_INTEGRATION_EVENT_TYPE_VALUES`. Every entry is seeded twice, so
    // a filter answering "the first match" or "everything" fails here.
    let mut by_type: Vec<(&str, Vec<String>)> = Vec::new();
    let mut index = 0;
    for event_type in DATA_INTEGRATION_EVENT_TYPE_VALUES {
        let mut ids = Vec::new();
        for _ in 0..2 {
            index += 1;
            let event = SeededEvent::new(index, event_type);
            emulator.given_event(&instance, &event);
            ids.push(event.event_id);
        }
        by_type.push((event_type, ids));
    }

    for (event_type, expected) in &by_type {
        let case = format!("filter on {event_type}");
        let (status, body) = emulator
            .list_events(
                &instance,
                &format!("eventType={event_type}&maxResults={MAX_RESULTS_MAX}"),
            )
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200, got {status} with {body}"
        );
        assert_eq!(
            &listed_ids(&case, &body),
            expected,
            "case {case:?}: expected exactly the events of that type, got {body}"
        );
    }

    // No filter: every seeded event, whatever its type.
    let case = "no filter";
    let (ids, _) = walk_pages(&emulator, case, &instance, None, Some(MAX_RESULTS_MAX)).await;
    let mut all: Vec<String> = by_type
        .iter()
        .flat_map(|(_, ids)| ids.iter().cloned())
        .collect();
    all.sort();
    assert_eq!(
        ids, all,
        "case {case:?}: an unfiltered listing must report every stored event"
    );
}

/// RULE: a listing is cut into pages of at most `maxResults`, and the pages a client
/// walks with the tokens it is handed concatenate to the whole order exactly once —
/// whatever the page size, filtered or not.
#[tokio::test]
async fn pagination_walks_the_whole_order_exactly_once_at_every_page_size() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("paging-host").await;

    // 24 events: more than the largest page (20) and more than the default (10), so every
    // row below needs at least two pages. Every third one is of a second type, so the
    // filtered rows page over a non-contiguous subset of the order.
    const SEEDED: u64 = 24;
    let filtered_type = "scn.data.forecast";
    let other_type = "scn.data.inventorylevel";
    let mut all_ids = Vec::new();
    let mut filtered_ids = Vec::new();
    for index in 1..=SEEDED {
        let event_type = if index % 3 == 0 {
            filtered_type
        } else {
            other_type
        };
        let event = SeededEvent::new(index, event_type);
        emulator.given_event(&instance, &event);
        if event_type == filtered_type {
            filtered_ids.push(event.event_id.clone());
        }
        all_ids.push(event.event_id);
    }

    // Rows differ along the page size: both published bounds of `maxResults`, sizes in
    // between (one of which divides the total exactly), and the modelled default, which
    // a row omits `maxResults` to get.
    let rows: Vec<(&str, Option<i64>)> = vec![
        ("one event per page", Some(MAX_RESULTS_MIN)),
        ("two events per page", Some(2)),
        ("eight events per page", Some(8)),
        ("seven events per page", Some(7)),
        ("the largest page", Some(MAX_RESULTS_MAX)),
        ("the modelled default page", None),
    ];

    for (case, max_results) in rows {
        let (ids, pages) = walk_pages(&emulator, case, &instance, None, max_results).await;
        assert_eq!(
            ids, all_ids,
            "case {case:?}: the pages must concatenate to the whole order exactly once"
        );
        let size = match max_results {
            Some(size) => usize::try_from(size).expect("a page size a row asks for is positive"),
            None => MAX_RESULTS_DEFAULT,
        };
        let expected_pages = all_ids.len().div_ceil(size);
        assert_eq!(
            pages, expected_pages,
            "case {case:?}: {} events in pages of {size} expected {expected_pages} pages",
            all_ids.len()
        );

        let filtered_case = format!("{case}, filtered");
        let (ids, _) = walk_pages(
            &emulator,
            &filtered_case,
            &instance,
            Some(filtered_type),
            max_results,
        )
        .await;
        assert_eq!(
            ids, filtered_ids,
            "case {filtered_case:?}: the filtered pages must concatenate to exactly the events \
             of that type"
        );
    }
}

/// RULE: `maxResults` outside the bounds the model publishes is a validation failure, and
/// each published bound itself is served.
#[tokio::test]
async fn max_results_is_served_at_its_bounds_and_refused_outside_them() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("bounds-host").await;
    const SEEDED: u64 = 3;
    for index in 1..=SEEDED {
        emulator.given_event(&instance, &SeededEvent::new(index, "scn.data.shipment"));
    }

    // Rows: both published bounds of `DataIntegrationEventMaxResults`, and one past each.
    let rows: Vec<(&str, i64, u16)> = vec![
        ("one below the minimum", MAX_RESULTS_MIN - 1, 400),
        ("the minimum", MAX_RESULTS_MIN, 200),
        ("the maximum", MAX_RESULTS_MAX, 200),
        ("one past the maximum", MAX_RESULTS_MAX + 1, 400),
    ];

    for (case, max_results, expected_status) in rows {
        let (status, body) = emulator
            .list_events(&instance, &format!("maxResults={max_results}"))
            .await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: maxResults {max_results} expected HTTP {expected_status}, got \
             {status} with {body}"
        );
        if expected_status == 400 {
            assert_eq!(
                error_code(case, &body),
                "ValidationException",
                "case {case:?}: expected the modelled ValidationException, got {body}"
            );
        } else {
            let listed = listed_ids(case, &body);
            let expected = usize::try_from(max_results)
                .expect("a served bound is positive")
                .min(usize::try_from(SEEDED).expect("the seeded count fits"));
            assert_eq!(
                listed.len(),
                expected,
                "case {case:?}: expected {expected} events on the page, got {listed:?}"
            );
        }
    }
}

/// RULE: only a `nextToken` this service issued for THIS query is a page boundary;
/// anything else is a validation failure rather than a silently truncated or restarted
/// page.
#[tokio::test]
async fn a_next_token_this_service_did_not_issue_is_refused() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("token-host").await;
    let other_instance = emulator.given_instance("other-token-host").await;
    for index in 1..=3 {
        emulator.given_event(&instance, &SeededEvent::new(index, "scn.data.reservation"));
        emulator.given_event(
            &other_instance,
            &SeededEvent::new(index, "scn.data.reservation"),
        );
    }

    // The token this service hands out for a query is the control every forged row below
    // is a variation on: it names the instance, the filter in effect and the event to
    // resume at.
    let issued = |instance_id: &str, filter: &str, event: &str| {
        hex::encode(format!(
            "scn:1:data-integration-event:{instance_id}:{filter}:{event}"
        ))
    };

    // Rows differ along HOW the value fails to be a token this service issued for this
    // query: not hex at all, hex of a foreign payload, hex of a plausible store key with
    // no stamp, the stamp over something that is not an `eventId`, a token issued for
    // another instance, and a token issued under a different `eventType` filter.
    let rows: Vec<(&str, String)> = vec![
        ("not hex", "not-a-token".to_string()),
        (
            "hex of a foreign payload",
            hex::encode("someone-elses-cursor"),
        ),
        ("hex of a bare store key", hex::encode(event_id(2))),
        (
            "the stamp over a non-uuid key",
            issued(&instance, "", "not-a-uuid"),
        ),
        (
            "a token issued for another instance",
            issued(&other_instance, "", &event_id(2)),
        ),
        (
            "a token issued under a different filter",
            issued(&instance, "scn.data.reservation", &event_id(2)),
        ),
    ];

    for (case, token) in rows {
        let (status, body) = emulator
            .list_events(&instance, &format!("nextToken={token}"))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} with {body}"
        );
        assert_eq!(
            error_code(case, &body),
            "ValidationException",
            "case {case:?}: expected the modelled ValidationException, got {body}"
        );
    }

    // The control: the token the service itself hands out resumes the listing.
    let case = "the token the service issued";
    let (status, first) = emulator.list_events(&instance, "maxResults=1").await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {first}"
    );
    let token = next_token(&first)
        .unwrap_or_else(|| panic!("case {case:?}: a truncated page must carry a nextToken"));
    let (status, second) = emulator
        .list_events(&instance, &format!("maxResults=1&nextToken={token}"))
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200, got {status} with {second}"
    );
    assert_eq!(
        listed_ids(case, &second),
        vec![event_id(2)],
        "case {case:?}: the token must resume at the event the first page stopped at"
    );

    // ...and that same token is refused once the query it was cut from changes.
    let case = "the issued token against a changed filter";
    let (status, body) = emulator
        .list_events(
            &instance,
            &format!("maxResults=1&eventType=scn.data.reservation&nextToken={token}"),
        )
        .await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body}"
    );
    assert_eq!(
        error_code(case, &body),
        "ValidationException",
        "case {case:?}: expected the modelled ValidationException, got {body}"
    );
}

/// RULE: an event is addressed by its instance and by nothing else, so the store keeps
/// one instance's events only while that instance lives, and holds at most
/// [`MAX_DATA_INTEGRATION_EVENTS_PER_INSTANCE`] of them per live instance.
#[tokio::test]
async fn an_instances_events_are_bounded_and_reclaimed_with_the_instance() {
    let emulator = Emulator::new();
    let doomed = emulator.given_instance("doomed-host").await;
    let survivor = emulator.given_instance("surviving-host").await;
    for index in 1..=3 {
        emulator.given_event(&doomed, &SeededEvent::new(index, "scn.data.shipment"));
        emulator.given_event(&survivor, &SeededEvent::new(index, "scn.data.shipment"));
    }

    let case = "reclaimed with the instance";
    let (status, body) = emulator
        .send("DELETE", &format!("/api/instance/{doomed}"), None)
        .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteInstance expected 200, got {status} with {body}"
    );
    // The events go when the accepted deletion SETTLES: while the instance is `Deleting`
    // they are still held.
    assert_eq!(
        emulator.stored_events(&doomed),
        3,
        "case {case:?}: the events of a Deleting instance must still be held"
    );
    wait_until_deleted(&emulator, &doomed).await;
    assert_eq!(
        emulator.stored_events(&doomed),
        0,
        "case {case:?}: the deleted instance's events must not outlive the only path that \
         addresses them"
    );
    assert_eq!(
        emulator.stored_events(&survivor),
        3,
        "case {case:?}: another instance's events must survive that deletion"
    );

    // The per-instance bound the growth site checks: false right up to the cap, true at
    // it, so a send past it has something to refuse on.
    let case = "the per-instance bound";
    let counted = emulator.given_events_up_to(&survivor, MAX_DATA_INTEGRATION_EVENTS_PER_INSTANCE);
    assert_eq!(
        counted,
        MAX_DATA_INTEGRATION_EVENTS_PER_INSTANCE,
        "case {case:?}: the store must hold exactly the cap after filling it"
    );
    assert!(
        emulator.events_at_capacity(&survivor),
        "case {case:?}: an instance holding {MAX_DATA_INTEGRATION_EVENTS_PER_INSTANCE} events is \
         at capacity"
    );
    assert!(
        !emulator.events_at_capacity(&doomed),
        "case {case:?}: an instance holding no event is not at capacity"
    );
}

/// RULE: an instance-scoped read whose `instanceId` names no instance of this account is
/// refused by the authorization step, before any event is looked up.
#[tokio::test]
async fn an_instance_this_account_does_not_hold_is_an_authorization_failure() {
    let emulator = Emulator::new();
    let live = emulator.given_instance("live-host").await;
    emulator.given_event(&live, &SeededEvent::new(1, "scn.data.processheader"));

    // Rows differ along WHY the instance is not this account's: one never created, and
    // one this account deleted.
    let deleted = emulator.given_instance("deleted-host").await;
    let (status, body) = emulator
        .send("DELETE", &format!("/api/instance/{deleted}"), None)
        .await;
    assert_eq!(
        status, 200,
        "DeleteInstance expected 200, got {status} with {body}"
    );
    // An instance is this account's until its accepted deletion settles, so the row below
    // about a deleted instance waits for the removal the clock owes.
    wait_until_deleted(&emulator, &deleted).await;

    let rows: Vec<(&str, String)> = vec![
        ("never created", event_id(0xfeed)),
        ("deleted by this account", deleted.clone()),
    ];

    for (case, instance_id) in rows {
        let (status, body) = emulator.list_events(&instance_id, "").await;
        assert_eq!(
            status, 403,
            "case {case:?}: expected HTTP 403, got {status} with {body}"
        );
        assert_eq!(
            error_code(case, &body),
            "AccessDeniedException",
            "case {case:?}: expected the modelled AccessDeniedException, got {body}"
        );
        assert_eq!(
            error_message(case, &body),
            expected_authorization_failure(&instance_id),
            "case {case:?}: the refusal must name the instanceId the request carried"
        );
    }
}

/// RULE: the listing is a report — it changes nothing a later caller sees, so the store
/// holds the same events afterwards and a repeated call answers identically.
#[tokio::test]
async fn listing_events_changes_nothing_a_later_caller_sees() {
    let emulator = Emulator::new();
    let instance = emulator.given_instance("report-host").await;
    for index in 1..=4 {
        emulator.given_event(&instance, &SeededEvent::new(index, "scn.data.supplyplan"));
    }

    let case = "repeated listing";
    let before = emulator.stored_events(&instance);
    assert_eq!(
        before, 4,
        "case {case:?}: the store must hold the four seeded events"
    );
    let (_, first) = emulator.list_events(&instance, "maxResults=2").await;
    let (_, second) = emulator.list_events(&instance, "maxResults=2").await;
    assert_eq!(
        first, second,
        "case {case:?}: two identical listings must answer identically"
    );
    assert_eq!(
        emulator.stored_events(&instance),
        before,
        "case {case:?}: a listing must not add or drop a stored event"
    );

    // And the pages the tokens name are the same on a second walk, so no boundary moved.
    let (walked_once, _) = walk_pages(&emulator, case, &instance, None, Some(2)).await;
    let (walked_twice, _) = walk_pages(&emulator, case, &instance, None, Some(2)).await;
    assert_eq!(
        walked_once, walked_twice,
        "case {case:?}: walking every page twice must answer the same order"
    );
    assert_eq!(
        emulator.stored_events(&instance),
        before,
        "case {case:?}: paging must not add or drop a stored event"
    );
}
