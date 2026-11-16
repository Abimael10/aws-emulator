// SPDX-License-Identifier: CC0-1.0

//! How this service words a request its own modelled constraints refuse — driven end to
//! end over the real wire path (a restJson1 request through the multi-service router into
//! the mounted service), so every assertion is about what a client observes.
//!
//! RULE under test: a value outside a modelled constraint, and a required member the
//! request left out, are refused `ValidationException` (HTTP 400) whose message states the
//! offending MEMBER PATH and the CONSTRAINT that member violated — the bound taken from
//! the model — and NOTHING of the value the caller sent. An absent member is the one
//! exception, reported as the `null` it was, which is a fact of the request's shape rather
//! than a value. The clauses are carried in AWS's envelope, which counts them.
//!
//! The rows below span the constraint classes this service's own model declares — a
//! `@range` bound, a `@length` bound on a string and on a payload, a `@pattern`, an
//! enumerated value set, and an absent `@required` member — across more than one operation,
//! because the wording belongs to the service's validation layer rather than to any
//! handler. That is also why a row may address an operation whose handler is still a
//! scaffold: the refusal happens at the boundary, before any handler runs.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_supplychain_galaxypublicapigateway::ops::types::DATA_INTEGRATION_EVENT_TYPE_VALUES;
use service_com_amazonaws_supplychain_galaxypublicapigateway::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header whose credential scope names this service (`scn`), which
/// is how the router picks the service that claims the request. Signature verification is
/// switched off for the test router, so the signature itself is never read.
const SCN_CREDENTIAL: &str = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/scn/aws4_request, SignedHeaders=host, Signature=00";

/// The published bounds of the members these rows violate, as the model declares them
/// (`smithy_query(query="operation", operation="ListInstances,SendDataIntegrationEvent")`):
/// `ListInstances$maxResults` targets `InstanceMaxResults` with `@range(min: 0, max: 20)`,
/// `SendDataIntegrationEvent$data` targets `DataIntegrationEventData` with
/// `@length(min: 1, max: 1048576)`, and `$eventGroupId` targets
/// `DataIntegrationEventGroupId` with `@length(min: 1, max: 255)`.
const MAX_RESULTS_MIN: i64 = 0;
const MAX_RESULTS_MAX: i64 = 20;
const DATA_MAX_LENGTH: usize = 1_048_576;
const EVENT_GROUP_ID_MIN_LENGTH: usize = 1;

/// The smallest `maxResults` `ListInstances` serves a page for.
///
/// RECORDED behaviour, not modelled: the model's `@range` lower bound on
/// `InstanceMaxResults` is [`MAX_RESULTS_MIN`], but the service refuses a listing asking
/// for fewer than this — its published minimum page size — with the same constraint
/// wording as a bound the model does express.
const PUBLISHED_MIN_MAX_RESULTS: i64 = 1;

/// The `@pattern` of the modelled `UUID` shape every `instanceId` targets.
const UUID_PATTERN: &str = "^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$";

/// The value set of `DataIntegrationEventType`, in the order the MODEL declares its
/// members
/// (`smithy_query(query="shape", shape="DataIntegrationEventType", model_path="services/service-com-amazonaws-supplychain-galaxypublicapigateway/smithy-model/com-amazonaws-supplychain-galaxypublicapigateway.json")`)
/// — which is the order a refusal lists the admitted values in, and not the sorted order
/// the crate's own `DATA_INTEGRATION_EVENT_TYPE_VALUES` holds them in.
const EVENT_TYPE_VALUES_IN_MODEL_ORDER: [&str; 16] = [
    "scn.data.forecast",
    "scn.data.inventorylevel",
    "scn.data.inboundorder",
    "scn.data.inboundorderline",
    "scn.data.inboundorderlineschedule",
    "scn.data.outboundorderline",
    "scn.data.outboundshipment",
    "scn.data.processheader",
    "scn.data.processoperation",
    "scn.data.processproduct",
    "scn.data.reservation",
    "scn.data.shipment",
    "scn.data.shipmentstop",
    "scn.data.shipmentstoporder",
    "scn.data.supplyplan",
    "scn.data.dataset",
];

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
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://scn.us-east-1.amazonaws.com{path}"))
            .header("host", "scn.us-east-1.amazonaws.com")
            .header("authorization", SCN_CREDENTIAL)
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

/// AWS's envelope over ONE clause.
fn one_error(clause: &str) -> String {
    format!("1 validation error detected: {clause}")
}

/// The clause a violation of `requirement` at `path` states: the member and the
/// requirement, and no value.
fn clause(path: &str, requirement: &str) -> String {
    format!("Value at '{path}' failed to satisfy constraint: {requirement}")
}

/// The URI `SendDataIntegrationEvent` is addressed at, as the model's `http` trait spells
/// it.
fn send_event_path(instance_id: &str) -> String {
    format!("/api-data/data-integration/instance/{instance_id}/data-integration-events")
}

/// RULE: the clause names the member and the constraint, never the value — across every
/// constraint class this service's model declares, and across operations.
#[tokio::test]
async fn a_constraint_refusal_names_the_member_and_the_requirement_only() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance().await;
    // A payload one byte past the modelled maximum: the smallest value the bound refuses.
    let oversized = "x".repeat(DATA_MAX_LENGTH + 1);
    let valid_event = json!({
        "eventType": DATA_INTEGRATION_EVENT_TYPE_VALUES[0],
        "eventGroupId": "group-one",
        "data": "{}",
    });

    // Rows: (case, the call, the clause the refusal must carry, the value the caller sent
    // and the message must not quote).
    let rows: Vec<(&str, (u16, Value), String, String)> = vec![
        (
            "a @range maximum, one past the bound",
            emulator
                .send("GET", &format!("/api/instance?maxResults={}", MAX_RESULTS_MAX + 1), None)
                .await,
            clause(
                "maxResults",
                &format!("Member must have value less than or equal to {MAX_RESULTS_MAX}"),
            ),
            (MAX_RESULTS_MAX + 1).to_string(),
        ),
        (
            "a @range maximum, far past the bound",
            emulator
                .send("GET", "/api/instance?maxResults=4096", None)
                .await,
            clause(
                "maxResults",
                &format!("Member must have value less than or equal to {MAX_RESULTS_MAX}"),
            ),
            "4096".to_string(),
        ),
        (
            // The published minimum page size is higher than the modelled lower bound, so
            // a value below it is refused naming the PUBLISHED bound — whether the modelled
            // bound caught it (below zero) or only the published one does (zero itself).
            "a @range minimum, one below the modelled bound",
            emulator
                .send("GET", &format!("/api/instance?maxResults={}", MAX_RESULTS_MIN - 1), None)
                .await,
            clause(
                "maxResults",
                &format!(
                    "Member must have value greater than or equal to {PUBLISHED_MIN_MAX_RESULTS}"
                ),
            ),
            (MAX_RESULTS_MIN - 1).to_string(),
        ),
        (
            "a page size below the published minimum",
            emulator
                .send("GET", &format!("/api/instance?maxResults={MAX_RESULTS_MIN}"), None)
                .await,
            clause(
                "maxResults",
                &format!(
                    "Member must have value greater than or equal to {PUBLISHED_MIN_MAX_RESULTS}"
                ),
            ),
            String::new(),
        ),
        (
            "a @length maximum on a @sensitive payload",
            emulator
                .send(
                    "POST",
                    &send_event_path(&instance_id),
                    Some(json!({
                        "eventType": DATA_INTEGRATION_EVENT_TYPE_VALUES[0],
                        "eventGroupId": "group-one",
                        "data": oversized,
                    })),
                )
                .await,
            clause(
                "data",
                &format!("Member must have length less than or equal to {DATA_MAX_LENGTH}"),
            ),
            oversized[..64].to_string(),
        ),
        (
            "a @length minimum on a string",
            emulator
                .send(
                    "POST",
                    &send_event_path(&instance_id),
                    Some(json!({
                        "eventType": DATA_INTEGRATION_EVENT_TYPE_VALUES[0],
                        "eventGroupId": "",
                        "data": "{}",
                    })),
                )
                .await,
            clause(
                "eventGroupId",
                &format!(
                    "Member must have length greater than or equal to {EVENT_GROUP_ID_MIN_LENGTH}"
                ),
            ),
            String::new(),
        ),
        (
            "a @pattern, on a value of the right length",
            emulator
                .send(
                    "POST",
                    &send_event_path("gggggggg-gggg-gggg-gggg-gggggggggggg"),
                    Some(valid_event.clone()),
                )
                .await,
            clause(
                "instanceId",
                &format!("Member must satisfy regular expression pattern: {UUID_PATTERN}"),
            ),
            "gggggggg-gggg-gggg-gggg-gggggggggggg".to_string(),
        ),
        (
            "a value outside an enumerated set",
            emulator
                .send(
                    "POST",
                    &send_event_path(&instance_id),
                    Some(json!({
                        "eventType": "scn.data.nothing",
                        "eventGroupId": "group-one",
                        "data": "{}",
                    })),
                )
                .await,
            clause(
                "eventType",
                &format!(
                    "Member must satisfy enum value set: [{}]",
                    EVENT_TYPE_VALUES_IN_MODEL_ORDER.join(", ")
                ),
            ),
            "scn.data.nothing".to_string(),
        ),
    ];

    for (case, (status, body), clause, sent) in rows {
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} with {body}"
        );
        let message = error_message(&body);
        assert_eq!(
            message,
            one_error(&clause),
            "case {case:?}: expected the member-and-requirement wording, got {message:?}"
        );
        if !sent.is_empty() {
            assert!(
                !message.contains(&sent),
                "case {case:?}: the message must not quote the value the caller sent, got \
                 {message:?}"
            );
        }
        // A payload is never summarised either: no byte count stands in for it.
        assert!(
            !message.contains("bytes"),
            "case {case:?}: the message must not describe the value at all, got {message:?}"
        );
    }
}

/// RULE: a required member the request left out is reported as the `null` it was, at its
/// own member path — the one case where the clause names anything about the value.
#[tokio::test]
async fn an_absent_required_member_is_reported_as_null() {
    let emulator = Emulator::new();
    let instance_id = emulator.given_instance().await;

    // Rows differ along WHICH required member of one operation the request omits.
    let rows = [
        (
            "no eventType",
            json!({ "eventGroupId": "group-one", "data": "{}" }),
            "eventType",
        ),
        (
            "no eventGroupId",
            json!({ "eventType": DATA_INTEGRATION_EVENT_TYPE_VALUES[0], "data": "{}" }),
            "eventGroupId",
        ),
        (
            "no data",
            json!({
                "eventType": DATA_INTEGRATION_EVENT_TYPE_VALUES[0],
                "eventGroupId": "group-one",
            }),
            "data",
        ),
    ];

    for (case, request, member) in rows {
        let (status, body) = emulator
            .send("POST", &send_event_path(&instance_id), Some(request))
            .await;
        assert_eq!(
            (status, error_code(&body).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException, got {status} with {body}"
        );
        assert_eq!(
            error_message(&body),
            one_error(&format!(
                "Value null at '{member}' failed to satisfy constraint: Member must not be null"
            )),
            "case {case:?}: expected the absent-member wording, got {body}"
        );
    }
}
