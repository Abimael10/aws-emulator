// SPDX-License-Identifier: CC0-1.0

//! The service EVENT LOG, end to end over HTTP: what every mutation of a service appends
//! to it, and what `ListServiceEvents` reports from it.
//!
//! The rule under test: a service's event log holds one entry per change made to that
//! service or to a resource inside it, typed by what changed and timestamped when it
//! happened, and `ListServiceEvents` reports that log in recording order, keeping only the
//! entries whose type the request's `eventTypes` names and whose timestamp falls in the
//! `startTime`/`endTime` window, cut into pages of `maxResults` with a `nextToken` that
//! names where the next page resumes. A READ never changes the log, a `serviceArn` naming
//! no stored service is refused `ResourceNotFoundException`, and a `nextToken` this service
//! did not issue is refused `ValidationException`.
//!
//! Every suite drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.
//!
//! The suites are TABLES over the class each rule generalises over — one row per mutation
//! kind, per filter set, per page size, per window — rather than one recorded request.

use std::collections::BTreeSet;
use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    ACTOR_TYPE_USER, MAX_RESULTS_MAX, MAX_RESULTS_MIN, SERVICE_EVENT_ASSERTION_CREATED,
    SERVICE_EVENT_ASSERTION_DELETED, SERVICE_EVENT_ASSERTION_UPDATED, SERVICE_EVENT_CREATED,
    SERVICE_EVENT_FUNCTION_CREATED, SERVICE_EVENT_FUNCTION_DELETED,
    SERVICE_EVENT_FUNCTION_RESOURCES_ADDED, SERVICE_EVENT_FUNCTION_UPDATED,
    SERVICE_EVENT_INPUT_SOURCES_UPDATED, SERVICE_EVENT_POLICY_ASSOCIATED,
    SERVICE_EVENT_POLICY_DISASSOCIATED, SERVICE_EVENT_SYSTEM_ASSOCIATED,
    SERVICE_EVENT_SYSTEM_DISASSOCIATED,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive, from the crate's `OPERATIONS.md`.
const CREATE_SERVICE: &str = "/v2/create-service";
const UPDATE_SERVICE: &str = "/v2/update-service";
const DELETE_SERVICE: &str = "/v2/delete-service";
const GET_SERVICE: &str = "/v2/get-service";
const CREATE_POLICY: &str = "/v2/create-policy";
const CREATE_SYSTEM: &str = "/v2/create-system";
const CREATE_SERVICE_FUNCTION: &str = "/v2/create-service-function";
const UPDATE_SERVICE_FUNCTION: &str = "/v2/update-function";
const DELETE_SERVICE_FUNCTION: &str = "/v2/delete-function";
const CREATE_SERVICE_FUNCTION_RESOURCES: &str = "/v2/create-service-function-resources";
const CREATE_ASSERTION: &str = "/v2/create-assertion";
const UPDATE_ASSERTION: &str = "/v2/update-assertion";
const DELETE_ASSERTION: &str = "/v2/delete-assertion";
const CREATE_INPUT_SOURCE: &str = "/v2/create-input-source";
const DELETE_INPUT_SOURCE: &str = "/v2/delete-input-source";
const LIST_SERVICE_EVENTS: &str = "/v2/list-service-events";
const LIST_ASSERTIONS: &str = "/v2/list-assertions";

/// A SigV4 `Authorization` header whose credential scope names this service's signing
/// name (`resiliencehub`) and the Region ARNs are minted in. Signature verification is off
/// for these tests, so the value only has to parse.
const SIGNED: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTONLY00000000/20260101/us-east-1/resiliencehub/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// A `ServiceEventType` value this crate's implemented mutations never cause, so no stored
/// event carries it.
///
/// SOURCE: `smithy_query(query="shape", shape="ServiceEventType")` publishes
/// `SERVICE_ACHIEVABILITY_UPDATED` among its values; nothing in this slice assesses a
/// service, so the log never holds one.
const UNCAUSED_EVENT_TYPE: &str = "SERVICE_ACHIEVABILITY_UPDATED";

/// An emulator holding only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("a JSON body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", SIGNED)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The modelled error code of a refusal, from the restJson1 `__type` discriminator.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

async fn list_events(router: &MultiServiceRouter, query: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("{LIST_SERVICE_EVENTS}?{query}"),
        None,
    )
    .await
}

/// Every event of `service_arn`'s log, read in one page.
async fn whole_log(router: &MultiServiceRouter, service_arn: &str) -> Vec<Value> {
    let (status, body) = list_events(
        router,
        &format!("serviceArn={service_arn}&maxResults={MAX_RESULTS_MAX}"),
    )
    .await;
    assert_eq!(status, 200, "reading the whole log: got {body:?}");
    assert!(
        body.get("nextToken").is_none(),
        "reading the whole log: a page holding every event carries no token, got {body:?}"
    );
    events_of(&body)
}

/// The `events` member of a listing, as a list.
fn events_of(body: &Value) -> Vec<Value> {
    body["events"]
        .as_array()
        .unwrap_or_else(|| panic!("no events list in {body:?}"))
        .clone()
}

/// The `eventType` of each event, in the order reported.
fn types_of(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["eventType"].as_str().unwrap_or("<none>").to_string())
        .collect()
}

/// The `eventId` of each event, in the order reported.
fn ids_of(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| event["eventId"].as_str().unwrap_or("<none>").to_string())
        .collect()
}

/// The instant an event reports, in milliseconds since the epoch.
///
/// restJson1 renders a `Timestamp` as epoch seconds, so the member is a number; a string
/// rendering is read too, so the helper states the instant rather than the encoding.
fn millis_of(event: &Value) -> i64 {
    let at = &event["timestamp"];
    let seconds = at
        .as_f64()
        .or_else(|| at.as_str().and_then(|text| text.parse::<f64>().ok()))
        .unwrap_or_else(|| panic!("no numeric timestamp in {event:?}"));
    (seconds * 1000.0).round() as i64
}

/// `millis` as the ISO-8601 instant a `@httpQuery` timestamp is carried as.
///
/// SOURCE: Smithy binds a `@httpQuery` timestamp with the `date-time` format by default,
/// so the window bounds go on the query string in that form.
fn as_query_instant(millis: i64) -> String {
    ::aws_smithy_types::DateTime::from_millis(millis)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("a representable instant")
}

/// Whether `value` has the modelled `Uuid` form: 36 characters, 8-4-4-4-12 lower-case hex.
///
/// SOURCE: `smithy_query(query="shape", shape="ServiceEvent")` types `eventId` as `Uuid`
/// with `length {min: 36, max: 36}` and the 8-4-4-4-12 pattern.
fn is_uuid(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && [8, 4, 4, 4, 12]
            .iter()
            .zip(&groups)
            .all(|(width, group)| group.len() == *width)
        && groups
            .iter()
            .all(|group| group.chars().all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()))
}

async fn create_service(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", CREATE_SERVICE, Some(body)).await
}

/// Create a service named `name` carrying no references, and answer its ARN.
async fn plain_service(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = create_service(
        router,
        json!({
            "name": name,
            "regions": ["us-east-1"],
            "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
        }),
    )
    .await;
    assert_eq!(status, 200, "creating service {name}: got {body:?}");
    body["service"]["serviceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no service.serviceArn in {body:?}"))
        .to_string()
}

/// Create a policy named `name` and answer its ARN.
async fn create_policy(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        CREATE_POLICY,
        Some(json!({"name": name, "availabilitySlo": {"target": 99.9}})),
    )
    .await;
    assert_eq!(status, 200, "creating policy {name}: got {body:?}");
    body["policy"]["policyArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no policy.policyArn in {body:?}"))
        .to_string()
}

/// Create a system named `name` and answer its ARN.
async fn create_system(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        CREATE_SYSTEM,
        Some(json!({"name": name})),
    )
    .await;
    assert_eq!(status, 200, "creating system {name}: got {body:?}");
    body["system"]["systemArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no system.systemArn in {body:?}"))
        .to_string()
}

/// One step of the mutation table: a call, and the event types it must append to the log.
struct Step {
    case: &'static str,
    appended: Vec<&'static str>,
}

/// What a seeded service's log holds: its ARN, the types in recording order, and the ids of
/// the resources the steps created (so a metadata assertion can be backed by them).
struct Seeded {
    service_arn: String,
    expected_types: Vec<String>,
    service_function_id: String,
    assertion_id: String,
    associated_resources: Vec<String>,
}

/// The resource identifiers one step associates with a service function.
///
/// Two are well-formed ARNs and one is a bare physical identifier, which is the other form
/// a discovered resource is addressed by — the metadata member is typed `Arn`, so only the
/// ARNs have a representation in it.
fn associable_resources() -> Vec<String> {
    vec![
        "arn:aws:sqs:us-east-1:000000000000:orders".to_string(),
        "arn:aws:dynamodb:us-east-1:000000000000:table/orders".to_string(),
        "orders-bucket".to_string(),
    ]
}

/// Drive one mutation of every kind this crate implements against one service, asserting
/// after each that the log grew by exactly the events that mutation causes, in order.
///
/// This is the TABLE over the mutation class: one row per kind of change (the service
/// itself, its input sources, its service functions and their resources, its assertions,
/// the policy and system it references), each asserted against the log the previous rows
/// left behind.
async fn seed_log(router: &MultiServiceRouter) -> Seeded {
    let mut expected: Vec<String> = Vec::new();
    let assert_step = |step: Step, reported: Vec<String>, expected: &mut Vec<String>| {
        for appended in &step.appended {
            expected.push((*appended).to_string());
        }
        assert_eq!(
            &reported, expected,
            "case {:?}: the log must hold exactly the events the mutations so far caused, \
             in recording order",
            step.case
        );
    };

    let service_arn = plain_service(router, "event-log-service").await;
    assert_step(
        Step {
            case: "CreateService",
            appended: vec![SERVICE_EVENT_CREATED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    // An input source: the service's resources are discovered through these, so creating
    // one changes what the service draws on.
    let (status, body) = call(
        router,
        "POST",
        CREATE_INPUT_SOURCE,
        Some(json!({
            "serviceArn": service_arn,
            "resourceConfiguration": {"resourceTags": [{"key": "Env", "values": ["prod"]}]},
        })),
    )
    .await;
    assert_eq!(status, 200, "creating an input source: got {body:?}");
    let input_source_id = body["inputSourceId"]
        .as_str()
        .unwrap_or_else(|| panic!("no inputSourceId in {body:?}"))
        .to_string();
    assert_step(
        Step {
            case: "CreateInputSource",
            appended: vec![SERVICE_EVENT_INPUT_SOURCES_UPDATED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let (status, body) = call(
        router,
        "POST",
        CREATE_SERVICE_FUNCTION,
        Some(json!({
            "serviceArn": service_arn,
            "name": "checkout",
            "criticality": "PRIMARY",
        })),
    )
    .await;
    assert_eq!(status, 200, "creating a service function: got {body:?}");
    let service_function_id = body["serviceFunction"]["serviceFunctionId"]
        .as_str()
        .unwrap_or_else(|| panic!("no serviceFunction.serviceFunctionId in {body:?}"))
        .to_string();
    assert_step(
        Step {
            case: "CreateServiceFunction",
            appended: vec![SERVICE_EVENT_FUNCTION_CREATED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let (status, body) = call(
        router,
        "POST",
        UPDATE_SERVICE_FUNCTION,
        Some(json!({
            "serviceArn": service_arn,
            "serviceFunctionId": service_function_id,
            "description": "the checkout path",
        })),
    )
    .await;
    assert_eq!(status, 200, "updating a service function: got {body:?}");
    assert_step(
        Step {
            case: "UpdateServiceFunction",
            appended: vec![SERVICE_EVENT_FUNCTION_UPDATED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let associated_resources = associable_resources();
    let (status, body) = call(
        router,
        "POST",
        CREATE_SERVICE_FUNCTION_RESOURCES,
        Some(json!({
            "serviceArn": service_arn,
            "serviceFunctionId": service_function_id,
            "resources": associated_resources,
        })),
    )
    .await;
    assert_eq!(status, 200, "associating resources: got {body:?}");
    assert_step(
        Step {
            case: "CreateServiceFunctionResources",
            appended: vec![SERVICE_EVENT_FUNCTION_RESOURCES_ADDED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let (status, body) = call(
        router,
        "POST",
        CREATE_ASSERTION,
        Some(json!({"serviceArn": service_arn, "text": "checkout survives one AZ loss"})),
    )
    .await;
    assert_eq!(status, 200, "creating an assertion: got {body:?}");
    let assertion_id = body["assertion"]["assertionId"]
        .as_str()
        .unwrap_or_else(|| panic!("no assertion.assertionId in {body:?}"))
        .to_string();
    assert_step(
        Step {
            case: "CreateAssertion",
            appended: vec![SERVICE_EVENT_ASSERTION_CREATED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let (status, body) = call(
        router,
        "POST",
        UPDATE_ASSERTION,
        Some(json!({
            "serviceArn": service_arn,
            "assertionId": assertion_id,
            "text": "checkout survives one Region loss",
        })),
    )
    .await;
    assert_eq!(status, 200, "updating an assertion: got {body:?}");
    assert_step(
        Step {
            case: "UpdateAssertion",
            appended: vec![SERVICE_EVENT_ASSERTION_UPDATED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    // A policy the service did not reference before is an association; swapping it for
    // another is a disassociation of the first and an association of the second.
    let first_policy = create_policy(router, "event-log-policy-one").await;
    let second_policy = create_policy(router, "event-log-policy-two").await;
    let (status, body) = call(
        router,
        "POST",
        UPDATE_SERVICE,
        Some(json!({"serviceArn": service_arn, "policyArn": first_policy})),
    )
    .await;
    assert_eq!(status, 200, "attaching a policy: got {body:?}");
    assert_step(
        Step {
            case: "UpdateService attaching a policy",
            appended: vec![SERVICE_EVENT_POLICY_ASSOCIATED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let (status, body) = call(
        router,
        "POST",
        UPDATE_SERVICE,
        Some(json!({"serviceArn": service_arn, "policyArn": second_policy})),
    )
    .await;
    assert_eq!(status, 200, "swapping the policy: got {body:?}");
    assert_step(
        Step {
            case: "UpdateService swapping the policy",
            appended: vec![
                SERVICE_EVENT_POLICY_DISASSOCIATED,
                SERVICE_EVENT_POLICY_ASSOCIATED,
            ],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    // Restating the same policy changes nothing, so it appends nothing.
    let (status, body) = call(
        router,
        "POST",
        UPDATE_SERVICE,
        Some(json!({"serviceArn": service_arn, "policyArn": second_policy})),
    )
    .await;
    assert_eq!(status, 200, "restating the policy: got {body:?}");
    assert_step(
        Step {
            case: "UpdateService restating the same policy",
            appended: vec![],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let system_arn = create_system(router, "event-log-system").await;
    let (status, body) = call(
        router,
        "POST",
        UPDATE_SERVICE,
        Some(json!({
            "serviceArn": service_arn,
            "associatedSystems": [{"systemArn": system_arn}],
        })),
    )
    .await;
    assert_eq!(status, 200, "associating a system: got {body:?}");
    assert_step(
        Step {
            case: "UpdateService associating a system",
            appended: vec![SERVICE_EVENT_SYSTEM_ASSOCIATED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let (status, body) = call(
        router,
        "POST",
        UPDATE_SERVICE,
        Some(json!({"serviceArn": service_arn, "associatedSystems": []})),
    )
    .await;
    assert_eq!(status, 200, "disassociating the system: got {body:?}");
    assert_step(
        Step {
            case: "UpdateService disassociating the system",
            appended: vec![SERVICE_EVENT_SYSTEM_DISASSOCIATED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let (status, body) = call(
        router,
        "POST",
        DELETE_ASSERTION,
        Some(json!({"serviceArn": service_arn, "assertionId": assertion_id})),
    )
    .await;
    assert_eq!(status, 200, "deleting the assertion: got {body:?}");
    assert_step(
        Step {
            case: "DeleteAssertion",
            appended: vec![SERVICE_EVENT_ASSERTION_DELETED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let (status, body) = call(
        router,
        "POST",
        DELETE_SERVICE_FUNCTION,
        Some(json!({
            "serviceArn": service_arn,
            "serviceFunctionId": service_function_id,
        })),
    )
    .await;
    assert_eq!(status, 200, "deleting the service function: got {body:?}");
    assert_step(
        Step {
            case: "DeleteServiceFunction",
            appended: vec![SERVICE_EVENT_FUNCTION_DELETED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    let (status, body) = call(
        router,
        "POST",
        DELETE_INPUT_SOURCE,
        Some(json!({
            "serviceArn": service_arn,
            "inputSourceId": input_source_id,
        })),
    )
    .await;
    assert_eq!(status, 200, "deleting the input source: got {body:?}");
    assert_step(
        Step {
            case: "DeleteInputSource",
            appended: vec![SERVICE_EVENT_INPUT_SOURCES_UPDATED],
        },
        types_of(&whole_log(router, &service_arn).await),
        &mut expected,
    );

    Seeded {
        service_arn,
        expected_types: expected,
        service_function_id,
        assertion_id,
        associated_resources,
    }
}

/// RULE: every mutation of a service appends one entry per change it made, typed by what it
/// changed, and every member of each entry is read back from what was stored.
#[tokio::test]
async fn every_mutation_of_a_service_is_recorded_in_its_event_log() {
    let router = emulator();
    let seeded = seed_log(&router).await;
    let events = whole_log(&router, &seeded.service_arn).await;

    assert_eq!(
        types_of(&events),
        seeded.expected_types,
        "case the whole log: expected one entry per change made, in recording order, got \
         {events:?}"
    );

    let ids: BTreeSet<String> = ids_of(&events).into_iter().collect();
    assert_eq!(
        ids.len(),
        events.len(),
        "case the whole log: every event carries its own eventId, got {events:?}"
    );

    for event in &events {
        let case = event["eventType"].as_str().unwrap_or("<none>").to_string();
        let event_id = event["eventId"].as_str().unwrap_or_default();
        assert!(
            is_uuid(event_id),
            "case {case}: eventId must have the modelled Uuid form, got {event_id:?}"
        );
        assert_eq!(
            event["serviceArn"].as_str(),
            Some(seeded.service_arn.as_str()),
            "case {case}: an event names the service whose log it is, got {event:?}"
        );
        assert_eq!(
            event["actor"]["type"].as_str(),
            Some(ACTOR_TYPE_USER),
            "case {case}: a change a request made carries a USER actor, got {event:?}"
        );
        assert!(
            event["actor"]["principalId"]
                .as_str()
                .is_some_and(|id| !id.is_empty()),
            "case {case}: the actor names the principal that caused the change, got {event:?}"
        );
        assert!(
            event["eventDetails"]["title"]
                .as_str()
                .is_some_and(|title| !title.is_empty()),
            "case {case}: an event carries its title, got {event:?}"
        );
        assert!(
            event["eventDetails"]["description"]
                .as_str()
                .is_some_and(|text| !text.is_empty()),
            "case {case}: an event carries its description, got {event:?}"
        );
    }

    // The metadata of an event is BACKED by the resource the change was made to: the
    // function events name the function that was created and deleted, the assertion events
    // the assertion, and the resources event only the identifiers that are ARNs, which is
    // the one form the modelled `ArnList` member can carry.
    let function_created = events
        .iter()
        .find(|event| event["eventType"] == json!(SERVICE_EVENT_FUNCTION_CREATED))
        .unwrap_or_else(|| panic!("no {SERVICE_EVENT_FUNCTION_CREATED} event in {events:?}"));
    assert_eq!(
        function_created["eventDetails"]["eventMetadata"]["serviceFunctionCreated"],
        json!({
            "serviceFunctionId": seeded.service_function_id,
            "serviceFunctionName": "checkout",
        }),
        "case {SERVICE_EVENT_FUNCTION_CREATED}: the metadata names the function that was \
         created, got {function_created:?}"
    );

    let assertion_deleted = events
        .iter()
        .find(|event| event["eventType"] == json!(SERVICE_EVENT_ASSERTION_DELETED))
        .unwrap_or_else(|| panic!("no {SERVICE_EVENT_ASSERTION_DELETED} event in {events:?}"));
    assert_eq!(
        assertion_deleted["eventDetails"]["eventMetadata"]["assertionDeleted"]["assertionId"],
        json!(seeded.assertion_id),
        "case {SERVICE_EVENT_ASSERTION_DELETED}: the metadata names the assertion that was \
         deleted, got {assertion_deleted:?}"
    );

    let resources_added = events
        .iter()
        .find(|event| event["eventType"] == json!(SERVICE_EVENT_FUNCTION_RESOURCES_ADDED))
        .unwrap_or_else(|| {
            panic!("no {SERVICE_EVENT_FUNCTION_RESOURCES_ADDED} event in {events:?}")
        });
    let expected_arns: Vec<&String> = seeded
        .associated_resources
        .iter()
        .filter(|identifier| identifier.starts_with("arn:"))
        .collect();
    assert_eq!(
        resources_added["eventDetails"]["eventMetadata"]["serviceFunctionResourcesAdded"]
            ["resourcesAdded"],
        json!(expected_arns),
        "case {SERVICE_EVENT_FUNCTION_RESOURCES_ADDED}: the metadata reports the ARNs this \
         call associated, got {resources_added:?}"
    );
}

/// RULE: `eventTypes` keeps the entries whose type the request names and nothing else; a
/// request naming none reports every type.
#[tokio::test]
async fn the_event_type_filter_keeps_exactly_the_types_it_names() {
    let router = emulator();
    let seeded = seed_log(&router).await;
    let whole = whole_log(&router, &seeded.service_arn).await;

    // Each row is a filter set, differing in how many types it names, whether a named type
    // has entries, and whether one type is named twice.
    let rows: Vec<(&str, Vec<&str>)> = vec![
        ("no filter", vec![]),
        ("one type with one entry", vec![SERVICE_EVENT_CREATED]),
        (
            "one type with several entries",
            vec![SERVICE_EVENT_INPUT_SOURCES_UPDATED],
        ),
        (
            "two types",
            vec![
                SERVICE_EVENT_ASSERTION_CREATED,
                SERVICE_EVENT_ASSERTION_DELETED,
            ],
        ),
        (
            "a type with entries and one without",
            vec![SERVICE_EVENT_FUNCTION_CREATED, UNCAUSED_EVENT_TYPE],
        ),
        ("a type with no entries", vec![UNCAUSED_EVENT_TYPE]),
        (
            "the same type named twice",
            vec![SERVICE_EVENT_POLICY_ASSOCIATED, SERVICE_EVENT_POLICY_ASSOCIATED],
        ),
        (
            "every type the log holds",
            seeded
                .expected_types
                .iter()
                .map(String::as_str)
                .collect::<BTreeSet<&str>>()
                .into_iter()
                .collect(),
        ),
    ];

    for (case, wanted) in rows {
        let mut query = format!(
            "serviceArn={}&maxResults={MAX_RESULTS_MAX}",
            seeded.service_arn
        );
        for kind in &wanted {
            query.push_str(&format!("&eventTypes={kind}"));
        }
        let (status, body) = list_events(&router, &query).await;
        assert_eq!(status, 200, "case {case:?}: got {body:?}");
        let reported = events_of(&body);
        let expected: Vec<Value> = whole
            .iter()
            .filter(|event| {
                wanted.is_empty()
                    || wanted
                        .iter()
                        .any(|kind| event["eventType"] == json!(*kind))
            })
            .cloned()
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: the listing must report exactly the log's entries of the named \
             types, in log order"
        );
    }
}

/// RULE: pages partition the log — walking `nextToken` at any page size reports every
/// entry the same filter reports unpaged, exactly once, in the same order, and the last
/// page carries no token.
#[tokio::test]
async fn the_pages_partition_the_log_at_every_page_size() {
    let router = emulator();
    let seeded = seed_log(&router).await;
    let whole = whole_log(&router, &seeded.service_arn).await;
    assert!(
        whole.len() > 3,
        "the seeded log must be long enough to page, got {} entries",
        whole.len()
    );

    // Each row is a page size and the filter the walk carries: the smallest page, one that
    // divides the log, one that does not, one larger than the log, and the modelled bounds.
    let page_sizes = [
        MAX_RESULTS_MIN,
        2,
        3,
        i32::try_from(whole.len()).unwrap_or(MAX_RESULTS_MAX),
        MAX_RESULTS_MAX,
    ];
    let filters: [(&str, &[&str]); 2] = [
        ("unfiltered", &[]),
        (
            "filtered to two types",
            &[
                SERVICE_EVENT_INPUT_SOURCES_UPDATED,
                SERVICE_EVENT_POLICY_ASSOCIATED,
            ],
        ),
    ];

    for (filter_case, wanted) in filters {
        let expected: Vec<Value> = whole
            .iter()
            .filter(|event| {
                wanted.is_empty()
                    || wanted
                        .iter()
                        .any(|kind| event["eventType"] == json!(*kind))
            })
            .cloned()
            .collect();
        for page_size in page_sizes {
            let case = format!("{filter_case} at maxResults={page_size}");
            let mut walked: Vec<Value> = Vec::new();
            let mut token: Option<String> = None;
            // One more round than pages are needed, so a token that never clears is caught
            // instead of looping forever.
            for _ in 0..=expected.len() + 1 {
                let mut query = format!(
                    "serviceArn={}&maxResults={page_size}",
                    seeded.service_arn
                );
                for kind in wanted {
                    query.push_str(&format!("&eventTypes={kind}"));
                }
                if let Some(carried) = token.as_deref() {
                    query.push_str(&format!("&nextToken={carried}"));
                }
                let (status, body) = list_events(&router, &query).await;
                assert_eq!(status, 200, "case {case:?}: got {body:?}");
                let page = events_of(&body);
                assert!(
                    page.len() <= page_size as usize,
                    "case {case:?}: a page holds at most maxResults entries, got {page:?}"
                );
                walked.extend(page);
                token = body["nextToken"].as_str().map(str::to_string);
                if token.is_none() {
                    break;
                }
            }
            assert!(
                token.is_none(),
                "case {case:?}: the last page must carry no nextToken, got {token:?}"
            );
            assert_eq!(
                walked, expected,
                "case {case:?}: the pages together must be exactly the unpaged listing, each \
                 entry once, in log order"
            );
        }
    }
}

/// RULE: `startTime` and `endTime` keep the entries whose instant falls in the window,
/// both bounds included; a window the log falls outside of reports nothing.
#[tokio::test]
async fn the_time_window_keeps_the_entries_inside_it() {
    let router = emulator();
    let seeded = seed_log(&router).await;
    let whole = whole_log(&router, &seeded.service_arn).await;
    let instants: Vec<i64> = whole.iter().map(millis_of).collect();
    let first = *instants.iter().min().expect("a seeded log");
    let last = *instants.iter().max().expect("a seeded log");
    let hour: i64 = 60 * 60 * 1000;

    // Each row is a window: unbounded on each side, the whole span, a single instant, and
    // windows wholly before and wholly after the log.
    let rows: Vec<(&str, Option<i64>, Option<i64>)> = vec![
        ("no bound", None, None),
        ("from the first instant", Some(first), None),
        ("until the last instant", None, Some(last)),
        ("the whole span", Some(first), Some(last)),
        ("the first instant alone", Some(first), Some(first)),
        ("the last instant alone", Some(last), Some(last)),
        ("an hour before the log", Some(first - 2 * hour), Some(first - hour)),
        ("an hour after the log", Some(last + hour), Some(last + 2 * hour)),
        ("a window that closes before it opens", Some(last), Some(first - hour)),
    ];

    for (case, from, until) in rows {
        let mut query = format!(
            "serviceArn={}&maxResults={MAX_RESULTS_MAX}",
            seeded.service_arn
        );
        if let Some(from) = from {
            query.push_str(&format!("&startTime={}", as_query_instant(from)));
        }
        if let Some(until) = until {
            query.push_str(&format!("&endTime={}", as_query_instant(until)));
        }
        let (status, body) = list_events(&router, &query).await;
        assert_eq!(status, 200, "case {case:?}: got {body:?}");
        let reported = events_of(&body);
        let expected: Vec<Value> = whole
            .iter()
            .filter(|event| {
                let at = millis_of(event);
                from.is_none_or(|from| at >= from) && until.is_none_or(|until| at <= until)
            })
            .cloned()
            .collect();
        assert_eq!(
            reported, expected,
            "case {case:?}: the listing must report exactly the entries inside the window"
        );
    }

    // A window that opens after the log closes holds nothing — stated on its own, so the
    // row above cannot pass by reporting everything.
    let mut query = format!("serviceArn={}", seeded.service_arn);
    query.push_str(&format!("&startTime={}", as_query_instant(last + hour)));
    let (status, body) = list_events(&router, &query).await;
    assert_eq!(status, 200, "case a window after the log: got {body:?}");
    assert!(
        events_of(&body).is_empty(),
        "case a window after the log: nothing falls in it, got {body:?}"
    );
}

/// RULE: reading the log, or the service, leaves the log exactly as it was — a report is
/// not a change.
#[tokio::test]
async fn reading_the_log_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    let seeded = seed_log(&router).await;
    let before = whole_log(&router, &seeded.service_arn).await;

    for (case, method, uri) in [
        (
            "ListServiceEvents",
            "GET",
            format!(
                "{LIST_SERVICE_EVENTS}?serviceArn={}&maxResults={MAX_RESULTS_MIN}",
                seeded.service_arn
            ),
        ),
        (
            "GetService",
            "GET",
            format!("{GET_SERVICE}?serviceArn={}", seeded.service_arn),
        ),
        (
            "ListAssertions",
            "GET",
            format!("{LIST_ASSERTIONS}?serviceArn={}", seeded.service_arn),
        ),
    ] {
        let (status, body) = call(&router, method, &uri, None).await;
        assert_eq!(status, 200, "case {case:?}: got {body:?}");
        let after = whole_log(&router, &seeded.service_arn).await;
        assert_eq!(
            after, before,
            "case {case:?}: a read must leave the log as it was"
        );
    }
}

/// RULE: the log is the log OF one service, so a `serviceArn` naming no stored service is
/// refused `ResourceNotFoundException` — including the ARN of a service that was deleted,
/// whose log went with it.
#[tokio::test]
async fn an_arn_naming_no_service_is_refused_rather_than_answered_with_an_empty_log() {
    let router = emulator();
    let seeded = seed_log(&router).await;
    let other = plain_service(&router, "event-log-other-service").await;

    let (status, body) = call(
        &router,
        "POST",
        DELETE_SERVICE,
        Some(json!({"serviceArn": seeded.service_arn})),
    )
    .await;
    assert_eq!(status, 200, "deleting the service: got {body:?}");

    // Each row is an ARN that names no stored service: the deleted one, and a well-formed
    // ARN of this service's own form that was never minted.
    let never_minted = format!(
        "arn:aws:resiliencehub:us-east-1:{}:service/00000000-0000-4000-8000-000000000000",
        ::engine::emulator_account_id()
    );
    for (case, arn) in [
        ("the ARN of a deleted service", seeded.service_arn.clone()),
        ("an ARN this service never minted", never_minted),
    ] {
        let (status, body) =
            list_events(&router, &format!("serviceArn={arn}&maxResults=10")).await;
        assert_eq!(status, 404, "case {case:?}: got {body:?}");
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: got {body:?}"
        );
    }

    // The other service's log is untouched by the delete: it holds its own create.
    let events = whole_log(&router, &other).await;
    assert_eq!(
        types_of(&events),
        vec![SERVICE_EVENT_CREATED.to_string()],
        "case a service beside the deleted one: its own log is untouched, got {events:?}"
    );
}

/// RULE: a `nextToken` this service did not issue, and a `maxResults` outside the modelled
/// range, are bad requests — refused `ValidationException` rather than answered with a
/// page.
#[tokio::test]
async fn a_token_this_service_did_not_issue_is_refused() {
    let router = emulator();
    let seeded = seed_log(&router).await;

    // Each row is a token of a different shape: not hex at all, hex of something that is
    // not a position, and hex of a position past every one this store handed out.
    for (case, token) in [
        ("a token that is not this service's encoding", "not-a-token".to_string()),
        ("hex of a value that is no position", ::hex::encode("elsewhere")),
        ("hex of a position never handed out", ::hex::encode("999999")),
        ("hex of the zeroth position", ::hex::encode("0")),
    ] {
        let (status, body) = list_events(
            &router,
            &format!("serviceArn={}&nextToken={token}", seeded.service_arn),
        )
        .await;
        assert_eq!(status, 400, "case {case:?}: got {body:?}");
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case {case:?}: got {body:?}"
        );
    }

    // `maxResults` is modelled `range 1..100`: each bound is served and each value one past
    // it is refused.
    for (case, max_results, expected) in [
        ("the lowest page size", MAX_RESULTS_MIN, 200),
        ("the highest page size", MAX_RESULTS_MAX, 200),
        ("one below the lowest", MAX_RESULTS_MIN - 1, 400),
        ("one above the highest", MAX_RESULTS_MAX + 1, 400),
    ] {
        let (status, body) = list_events(
            &router,
            &format!(
                "serviceArn={}&maxResults={max_results}",
                seeded.service_arn
            ),
        )
        .await;
        assert_eq!(status, expected, "case {case:?}: got {body:?}");
        if expected == 400 {
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?}: got {body:?}"
            );
        }
    }
}

/// RULE: a service created WITH references records them beside its creation, and a service
/// has only its OWN log.
#[tokio::test]
async fn a_create_records_the_references_it_establishes_and_logs_stay_separate() {
    let router = emulator();
    let policy_arn = create_policy(&router, "created-with-policy").await;
    let system_arn = create_system(&router, "created-with-system").await;

    let (status, body) = create_service(
        &router,
        json!({
            "name": "service-with-references",
            "regions": ["us-east-1"],
            "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
            "policyArn": policy_arn,
            "associatedSystems": [{"systemArn": system_arn}],
        }),
    )
    .await;
    assert_eq!(status, 200, "creating a service with references: got {body:?}");
    let with_references = body["service"]["serviceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no service.serviceArn in {body:?}"))
        .to_string();

    let events = whole_log(&router, &with_references).await;
    assert_eq!(
        types_of(&events),
        vec![
            SERVICE_EVENT_CREATED.to_string(),
            SERVICE_EVENT_POLICY_ASSOCIATED.to_string(),
            SERVICE_EVENT_SYSTEM_ASSOCIATED.to_string(),
        ],
        "case a create carrying references: expected the create and the two references it \
         established, got {events:?}"
    );
    let policy_event = &events[1];
    assert_eq!(
        policy_event["eventDetails"]["eventMetadata"]["servicePolicyAssociated"],
        json!({
            "policyName": "created-with-policy",
            "policyArn": policy_arn,
            "policyOwnerAccountId": ::engine::emulator_account_id(),
            "policySource": "SELF",
        }),
        "case the policy association: the metadata is read from the policy the ARN names, \
         got {policy_event:?}"
    );
    let system_event = &events[2];
    assert_eq!(
        system_event["eventDetails"]["eventMetadata"]["serviceSystemAssociated"],
        json!({"systemName": "created-with-system", "systemArn": system_arn}),
        "case the system association: the metadata is read from the system the ARN names, \
         got {system_event:?}"
    );

    // A second service's mutations go in ITS log, never in the first's.
    let plain = plain_service(&router, "service-without-references").await;
    let (status, body) = call(
        &router,
        "POST",
        CREATE_ASSERTION,
        Some(json!({"serviceArn": plain, "text": "an assertion of the other service"})),
    )
    .await;
    assert_eq!(status, 200, "creating an assertion elsewhere: got {body:?}");
    assert_eq!(
        types_of(&whole_log(&router, &plain).await),
        vec![
            SERVICE_EVENT_CREATED.to_string(),
            SERVICE_EVENT_ASSERTION_CREATED.to_string(),
        ],
        "case the second service: its log holds its own changes"
    );
    assert_eq!(
        types_of(&whole_log(&router, &with_references).await),
        vec![
            SERVICE_EVENT_CREATED.to_string(),
            SERVICE_EVENT_POLICY_ASSOCIATED.to_string(),
            SERVICE_EVENT_SYSTEM_ASSOCIATED.to_string(),
        ],
        "case the first service: another service's changes are no part of its log"
    );
}
