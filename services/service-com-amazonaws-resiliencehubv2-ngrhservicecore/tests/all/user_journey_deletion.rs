// SPDX-License-Identifier: CC0-1.0

//! `DeleteUserJourney`, end to end over HTTP: `CreateSystem` -> `CreateUserJourney` ->
//! `DeleteUserJourney` -> `GetUserJourney` / `ListUserJourneys`.
//!
//! The rule under test: a delete of a child resource REMOVES it from the parent that holds
//! it and echoes the identifier it removed, so every later reader of that child reports it
//! as not found — `GetUserJourney` answers `ResourceNotFoundException` (HTTP 404) with
//! `message` `User journey not found: <userJourneyId>`, `resourceId` that same id and
//! `resourceType` `UserJourney`, the parent's listing no longer names it, a repeated delete
//! is refused the same way rather than reported as a second success, and the
//! `@idempotencyToken` that created it replays nothing. Everything the call did not address
//! — the parent's other journeys, the other systems' journeys and the parent itself — is
//! left exactly as it was.
//!
//! The suites are TABLES: rows differ along what the deleted journey carried (the bare
//! required member, a `clientToken`, every optional member), how many journeys its parent
//! held and which of them is deleted (the only one, the first, the last, one in the middle),
//! and which identifier a refused delete names.
//!
//! Every case drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    not_found_message, refusal_resource_id, SYSTEM_RESOURCE, SYSTEM_RESOURCE_TYPE,
    USER_JOURNEY_RESOURCE_TYPE,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
///
/// SOURCE: `smithy_query(query="operation", operation="DeleteUserJourney")` — `http
/// {method: POST, uri: /v2/delete-user-journey}`, and the same query for each sibling.
const CREATE_SYSTEM: &str = "/v2/create-system";
const CREATE_USER_JOURNEY: &str = "/v2/create-user-journey";
const DELETE_USER_JOURNEY: &str = "/v2/delete-user-journey";
const GET_USER_JOURNEY: &str = "/v2/get-user-journey";
const LIST_USER_JOURNEYS: &str = "/v2/list-user-journeys";

/// The Region these suites sign for.
const REGION: &str = "us-east-1";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// `ListUserJourneys$maxResults`' published maximum, used so one listing reports every
/// journey a system holds.
///
/// SOURCE: `smithy_query(query="operation", operation="ListUserJourneys")` — `range {min: 1,
/// max: 100}`.
const MAX_RESULTS_MAX: usize = 100;

/// An emulator holding only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header whose credential scope names this service's signing name
/// (`resiliencehub`) and Region. Signature verification is off for these tests, so the
/// value only has to parse.
fn signed() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY00000000/20260101/{REGION}\
         /resiliencehub/aws4_request, SignedHeaders=host;x-amz-date, \
         Signature=0123456789abcdef"
    )
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
        .header("authorization", signed())
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

/// Create a system named `name` and answer its ARN.
async fn system_named(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(router, "POST", CREATE_SYSTEM, Some(json!({"name": name}))).await;
    assert_eq!(status, 200, "creating system {name:?} got {status} {body:?}");
    body["system"]["systemArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no system.systemArn in {body:?}"))
        .to_string()
}

/// `CreateUserJourney` in `system_arn` from `extras`, which must succeed; answers the
/// created journey's `userJourneyId`.
async fn journey_in(router: &MultiServiceRouter, system_arn: &str, extras: &Value) -> String {
    let mut body = extras.clone();
    body["systemArn"] = json!(system_arn);
    let (status, answered) = call(router, "POST", CREATE_USER_JOURNEY, Some(body)).await;
    assert_eq!(
        status, 200,
        "creating a journey from {extras:?} got {status} {answered:?}"
    );
    answered["userJourney"]["userJourneyId"]
        .as_str()
        .unwrap_or_else(|| panic!("no userJourney.userJourneyId in {answered:?}"))
        .to_string()
}

async fn delete_journey(
    router: &MultiServiceRouter,
    system_arn: &str,
    user_journey_id: &str,
) -> (u16, Value) {
    call(
        router,
        "POST",
        DELETE_USER_JOURNEY,
        Some(json!({"systemArn": system_arn, "userJourneyId": user_journey_id})),
    )
    .await
}

async fn get_journey(
    router: &MultiServiceRouter,
    system_arn: &str,
    user_journey_id: &str,
) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("{GET_USER_JOURNEY}?systemArn={system_arn}&userJourneyId={user_journey_id}"),
        None,
    )
    .await
}

/// The `userJourneyId` of every journey `system_arn` holds, in the order listed.
async fn listed_ids(router: &MultiServiceRouter, system_arn: &str) -> Vec<String> {
    let (status, body) = call(
        router,
        "GET",
        &format!("{LIST_USER_JOURNEYS}?systemArn={system_arn}&maxResults={MAX_RESULTS_MAX}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "listing the journeys of {system_arn} got {status} {body:?}"
    );
    body["userJourneySummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("the @required userJourneySummaries is absent from {body:?}"))
        .iter()
        .map(|summary| {
            summary["userJourneyId"]
                .as_str()
                .unwrap_or_else(|| panic!("no userJourneyId in {summary:?}"))
                .to_string()
        })
        .collect()
}

/// Assert that `body` is the refusal AWS answers a read of a journey that is not there with:
/// the fixed `User journey not found: ` text around the id the request supplied, that same id
/// as `resourceId`, and the model's own shape name for the resource as `resourceType`.
fn assert_journey_not_found(case: &str, status: u16, body: &Value, user_journey_id: &str) {
    assert_eq!(
        status, 404,
        "case {case:?}: expected HTTP 404 for a journey that is not there, got {status} \
         {body:?}"
    );
    let code = error_code(body);
    assert_eq!(
        code, "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException, got {code:?} in {body:?}"
    );
    let expected = not_found_message("User journey", user_journey_id);
    assert_eq!(
        body["message"].as_str(),
        Some(expected.as_str()),
        "case {case:?}: expected the refusal {expected:?}, got {:?}",
        body["message"]
    );
    assert_eq!(
        body["resourceId"].as_str(),
        Some(user_journey_id),
        "case {case:?}: expected resourceId {user_journey_id:?}, got {:?}",
        body["resourceId"]
    );
    assert_eq!(
        body["resourceType"].as_str(),
        Some(USER_JOURNEY_RESOURCE_TYPE),
        "case {case:?}: expected resourceType {USER_JOURNEY_RESOURCE_TYPE:?}, got {:?}",
        body["resourceType"]
    );
}

// ---------------------------------------------------------------------------
// What a delete removes
// ---------------------------------------------------------------------------

/// RULE: a delete removes the journey from the system that holds it, echoes the identifier
/// it removed, and afterwards every reader of that journey reports it as not found while the
/// journeys the call did not address are still there.
///
/// The rows differ along what the deleted journey carried and where it sat among its
/// system's journeys: the only journey of a system, the first of several, one in the middle,
/// the last, one created with a `clientToken`, and one carrying every optional member.
#[tokio::test]
async fn a_deleted_journey_is_gone_from_every_reader() {
    let rows: Vec<(&str, Value, usize, usize)> = vec![
        ("the only journey of its system", json!({"name": "Only"}), 1, 0),
        ("the first of three", json!({"name": "First"}), 3, 0),
        ("one in the middle of three", json!({"name": "Middle"}), 3, 1),
        ("the last of three", json!({"name": "Last"}), 3, 2),
        (
            "a journey created with a clientToken",
            json!({"name": "Tokened", "clientToken": "journey-delete-token"}),
            1,
            0,
        ),
        (
            "a journey carrying every optional member",
            json!({"name": "Fully Described", "description": "every member"}),
            2,
            1,
        ),
    ];

    for (case, deleted_body, held, position) in rows {
        let router = emulator();
        let system_arn = system_named(&router, "Delete-Host").await;
        // A second system holding a journey of its own, so a delete can be shown not to
        // reach past the system it names.
        let bystander_system = system_named(&router, "Delete-Bystander").await;
        let bystander_journey =
            journey_in(&router, &bystander_system, &json!({"name": "Bystander"})).await;

        // The system's journeys, with the row's own journey at `position`.
        let mut ids = Vec::new();
        for index in 0..held {
            let id = if index == position {
                journey_in(&router, &system_arn, &deleted_body).await
            } else {
                journey_in(&router, &system_arn, &json!({"name": format!("Kept{index}")})).await
            };
            ids.push(id);
        }
        let target = ids[position].clone();
        let survivors: Vec<String> = ids
            .iter()
            .filter(|id| **id != target)
            .cloned()
            .collect();

        // The delete answers the @required identifier of the journey it removed.
        let (status, body) = delete_journey(&router, &system_arn, &target).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DeleteUserJourney, got {status} {body:?}"
        );
        assert_eq!(
            body["userJourneyId"].as_str(),
            Some(target.as_str()),
            "case {case:?}: expected the deleted id {target:?} echoed, got {body:?}"
        );

        // The read of the deleted journey is the modelled refusal.
        let (status, body) = get_journey(&router, &system_arn, &target).await;
        assert_journey_not_found(case, status, &body, &target);

        // A second delete of the same id is refused the same way: there is nothing left to
        // remove, so it is not a second success.
        let (status, body) = delete_journey(&router, &system_arn, &target).await;
        assert_journey_not_found(case, status, &body, &target);

        // The listing of the parent names exactly the journeys that are left, in creation
        // order, and the other system's journey is untouched.
        let listed = listed_ids(&router, &system_arn).await;
        assert_eq!(
            listed, survivors,
            "case {case:?}: expected the surviving journeys {survivors:?}, got {listed:?}"
        );
        for survivor in &survivors {
            let (status, body) = get_journey(&router, &system_arn, survivor).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the surviving journey {survivor:?} must still read back, got \
                 {status} {body:?}"
            );
        }
        let listed = listed_ids(&router, &bystander_system).await;
        assert_eq!(
            listed,
            vec![bystander_journey.clone()],
            "case {case:?}: the other system must still hold its own journey, got {listed:?}"
        );
    }
}

/// RULE: the `clientToken` of a deleted journey replays nothing — the token named a journey
/// that is gone, so repeating the create that first used it creates a new journey rather
/// than answering the deleted one.
#[tokio::test]
async fn the_token_that_created_a_deleted_journey_replays_nothing() {
    let router = emulator();
    let system_arn = system_named(&router, "Token-Replay-Host").await;
    let create = json!({"name": "Replayed", "clientToken": "replayed-journey-token"});

    let first = journey_in(&router, &system_arn, &create).await;
    let (status, body) = delete_journey(&router, &system_arn, &first).await;
    assert_eq!(status, 200, "deleting {first} got {status} {body:?}");

    let second = journey_in(&router, &system_arn, &create).await;
    assert_ne!(
        second, first,
        "the token of a deleted journey must not replay it, got {second} again"
    );
    let (status, body) = get_journey(&router, &system_arn, &first).await;
    assert_journey_not_found("a replayed token", status, &body, &first);
    let (status, body) = get_journey(&router, &system_arn, &second).await;
    assert_eq!(
        status, 200,
        "the journey the repeated create made must read back, got {status} {body:?}"
    );
}

// ---------------------------------------------------------------------------
// What a delete refuses
// ---------------------------------------------------------------------------

/// RULE: a delete that names no stored journey removes nothing and is refused with the error
/// the model declares for it: a `userJourneyId` the addressed system does not hold is the
/// journey's own 404, and a `systemArn` naming no stored system is the system's.
///
/// The rows differ along WHY the delete resolves nothing: an id nothing ever minted, an id
/// at each end of `UserJourneyId`'s published `@length`, the id of a journey that belongs to
/// another system, and — the whole parent missing — the ARN of a system nothing created and
/// of one that was deleted.
#[tokio::test]
async fn a_delete_naming_nothing_stored_is_refused_and_removes_nothing() {
    let router = emulator();
    let system_arn = system_named(&router, "Refusal-Host").await;
    let live = journey_in(&router, &system_arn, &json!({"name": "Survivor"})).await;

    let other_system = system_named(&router, "Refusal-Other").await;
    let other_journey = journey_in(&router, &other_system, &json!({"name": "Foreign"})).await;

    // `UserJourneyId` is `@length 1..255` with pattern `^\S{1,255}$`
    // (`smithy_query(query="operation", operation="DeleteUserJourney")`), so a value at each
    // bound reaches the handler rather than being refused at the boundary.
    let shortest = "x".to_string();
    let longest = "x".repeat(255);

    for (case, user_journey_id) in [
        ("an id nothing ever minted", "no-such-journey".to_string()),
        ("an id at UserJourneyId's minimum length", shortest),
        ("an id at UserJourneyId's maximum length", longest),
        ("a journey of another system", other_journey.clone()),
    ] {
        let (status, body) = delete_journey(&router, &system_arn, &user_journey_id).await;
        assert_journey_not_found(case, status, &body, &user_journey_id);
    }

    // A parent that is not there is reported as the missing SYSTEM: the caller addressed a
    // system, not a journey of it.
    let never_created =
        format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SYSTEM_RESOURCE}/never-created");
    let doomed = system_named(&router, "Refusal-Doomed").await;
    let doomed_journey = journey_in(&router, &doomed, &json!({"name": "Doomed"})).await;
    let (status, body) = call(
        &router,
        "POST",
        "/v2/delete-system",
        Some(json!({"systemArn": doomed})),
    )
    .await;
    assert_eq!(status, 200, "deleting {doomed} got {status} {body:?}");

    for (case, system_arn, user_journey_id) in [
        (
            "a systemArn naming no stored system",
            never_created.as_str(),
            live.as_str(),
        ),
        (
            "the systemArn of a deleted system",
            doomed.as_str(),
            doomed_journey.as_str(),
        ),
    ] {
        let (status, body) = delete_journey(&router, system_arn, user_journey_id).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404 for a missing system, got {status} {body:?}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {code:?} in {body:?}"
        );
        let identifier = refusal_resource_id(SYSTEM_RESOURCE_TYPE, system_arn);
        let expected = not_found_message(SYSTEM_RESOURCE_TYPE, identifier);
        assert_eq!(
            body["message"].as_str(),
            Some(expected.as_str()),
            "case {case:?}: expected the refusal {expected:?}, got {:?}",
            body["message"]
        );
        assert_eq!(
            body["resourceType"].as_str(),
            Some(SYSTEM_RESOURCE_TYPE),
            "case {case:?}: expected resourceType {SYSTEM_RESOURCE_TYPE:?}, got {body:?}"
        );
    }

    // Every refusal above left the live journeys where they were.
    let listed = listed_ids(&router, &system_arn).await;
    assert_eq!(
        listed,
        vec![live.clone()],
        "a refused delete must remove nothing, got {listed:?}"
    );
    let listed = listed_ids(&router, &other_system).await;
    assert_eq!(
        listed,
        vec![other_journey],
        "a refused delete must not reach another system's journeys, got {listed:?}"
    );
}
