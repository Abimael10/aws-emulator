// SPDX-License-Identifier: CC0-1.0

//! `UpdateUserJourney`, end to end over HTTP, against the journeys a system holds.
//!
//! The rule under test: an update of a child resource addressed by its parent's ARN and its
//! own id replaces exactly those editable members the request carries, leaves every member
//! it omits at the value the store already held, stamps the record as updated without
//! re-minting its identity or its creation instant, and is refused
//! `ResourceNotFoundException` — writing nothing — when either the parent or the child it
//! addresses is not stored. For a user journey the editable members are `name`,
//! `description` and `policyArn`; `userJourneyId` and `createdAt` are not members of the
//! request, so no update can move them.
//!
//! Every suite drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.
//!
//! The suites are TABLES: each row is one input of the class the rule covers — which
//! editable members the request carries, each member's value at its modelled bounds, a
//! value replaced versus a value first supplied, and which of the addressing members names
//! nothing stored — not one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    SYSTEM_RESOURCE, SYSTEM_RESOURCE_TYPE,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
const CREATE_SYSTEM: &str = "/v2/create-system";
const CREATE_POLICY: &str = "/v2/create-policy";
const CREATE_USER_JOURNEY: &str = "/v2/create-user-journey";
const GET_USER_JOURNEY: &str = "/v2/get-user-journey";
const UPDATE_USER_JOURNEY: &str = "/v2/update-user-journey";

/// The Region these suites sign for.
const REGION: &str = "us-east-1";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// `EntityLabel`'s modelled bounds, which `UpdateUserJourney$name` targets.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdateUserJourney")` — `name`
/// targets `EntityLabel`, `length {min: 2, max: 60}`, pattern
/// `^[A-Za-z0-9][A-Za-z0-9 _\-]{1,59}$`.
const ENTITY_LABEL_MIN_LENGTH: usize = 2;
const ENTITY_LABEL_MAX_LENGTH: usize = 60;

/// `EntityDescription`'s modelled ceiling, which `UpdateUserJourney$description` targets.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdateUserJourney")` —
/// `description` targets `EntityDescription`, `length {max: 500}`.
const ENTITY_DESCRIPTION_MAX_LENGTH: usize = 500;

/// The members of `UserJourney` an update can replace.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdateUserJourney")` — the request
/// members other than the `systemArn` / `userJourneyId` pair that addresses the journey.
const EDITABLE_MEMBERS: [&str; 3] = ["name", "description", "policyArn"];

/// A SigV4 `Authorization` header whose credential scope names this service's signing name
/// (`resiliencehub`) and the Region ARNs are minted in. Signature verification is off for
/// these tests, so the value only has to parse.
const SIGNED: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTONLY00000000/20260101/us-east-1/resiliencehub/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

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

/// Create a system named `name` and answer its ARN.
async fn system_named(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(router, "POST", CREATE_SYSTEM, Some(json!({"name": name}))).await;
    assert_eq!(status, 200, "creating system {name:?} got {body:?}");
    body["system"]["systemArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no system.systemArn in {body:?}"))
        .to_string()
}

/// Create a resilience policy named `name` and answer its ARN — a real policy, so a journey
/// held to it names a policy this emulator actually stores.
async fn policy_named(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(router, "POST", CREATE_POLICY, Some(json!({"name": name}))).await;
    assert_eq!(status, 200, "creating policy {name:?} got {body:?}");
    body["policy"]["policyArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no policy.policyArn in {body:?}"))
        .to_string()
}

/// Create a journey in `system_arn` from `extras` (its `name` and whatever optional members
/// the case supplies) and answer the `UserJourney` the create projected.
async fn journey_in(router: &MultiServiceRouter, system_arn: &str, extras: &Value) -> Value {
    let mut body = extras.clone();
    body["systemArn"] = json!(system_arn);
    let (status, answered) = call(router, "POST", CREATE_USER_JOURNEY, Some(body)).await;
    assert_eq!(
        status, 200,
        "creating a journey from {extras:?} got {status} {answered:?}"
    );
    answered["userJourney"].clone()
}

async fn update_journey(
    router: &MultiServiceRouter,
    system_arn: &str,
    user_journey_id: &str,
    extras: &Value,
) -> (u16, Value) {
    let mut body = extras.clone();
    body["systemArn"] = json!(system_arn);
    body["userJourneyId"] = json!(user_journey_id);
    call(router, "POST", UPDATE_USER_JOURNEY, Some(body)).await
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

/// The `userJourneyId` a projected journey carries.
fn journey_id_of(case: &str, journey: &Value) -> String {
    journey["userJourneyId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no userJourneyId in {journey:?}"))
        .to_string()
}

/// A timestamp member as the epoch-seconds number restJson1 carries it as.
fn instant_of(case: &str, journey: &Value, member: &str) -> f64 {
    journey[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: no numeric {member} in {journey:?}"))
}

/// RULE: an update replaces exactly the editable members the request carries and leaves
/// every other member of the stored journey — the ones it omits, its id and its creation
/// instant — where they were, and the next read answers what the update stored.
///
/// The rows span the class: each editable member alone, each at its modelled bound, a value
/// first supplied versus one replaced, an empty description, every editable member at once,
/// and an update that carries none of them.
#[tokio::test]
async fn an_update_replaces_only_the_members_the_request_carries() {
    let router = emulator();
    let system_arn = system_named(&router, "Journeyed-System").await;
    let first_policy = policy_named(&router, "First-Policy").await;
    let second_policy = policy_named(&router, "Second-Policy").await;
    let shortest_name = "A".repeat(ENTITY_LABEL_MIN_LENGTH);
    let longest_name = "N".repeat(ENTITY_LABEL_MAX_LENGTH);
    let longest_description = "d".repeat(ENTITY_DESCRIPTION_MAX_LENGTH);

    let rows: Vec<(&str, Value, Value, Value)> = vec![
        (
            "a new name on a journey carrying nothing else",
            json!({"name": "Checkout journey"}),
            json!({"name": "Checkout journey v2"}),
            json!({"name": "Checkout journey v2"}),
        ),
        (
            "a name at EntityLabel's floor",
            json!({"name": "Floor journey"}),
            json!({"name": shortest_name}),
            json!({"name": shortest_name}),
        ),
        (
            "a name at EntityLabel's ceiling",
            json!({"name": "Ceiling journey"}),
            json!({"name": longest_name}),
            json!({"name": longest_name}),
        ),
        (
            "a description on a journey that had none",
            json!({"name": "Described later"}),
            json!({"description": "now described"}),
            json!({"name": "Described later", "description": "now described"}),
        ),
        (
            "a description at EntityDescription's ceiling over a shorter one",
            json!({"name": "Described", "description": "short"}),
            json!({"description": longest_description}),
            json!({"name": "Described", "description": longest_description}),
        ),
        (
            "an empty description over a filled one",
            json!({"name": "Emptied", "description": "filled in"}),
            json!({"description": ""}),
            json!({"name": "Emptied", "description": ""}),
        ),
        (
            "a policyArn on a journey held to no policy",
            json!({"name": "Policy added"}),
            json!({"policyArn": first_policy}),
            json!({"name": "Policy added", "policyArn": first_policy}),
        ),
        (
            "a policyArn over the one the create stored",
            json!({"name": "Policy moved", "policyArn": first_policy}),
            json!({"policyArn": second_policy}),
            json!({"name": "Policy moved", "policyArn": second_policy}),
        ),
        (
            "every editable member at once",
            json!({"name": "All edited", "description": "before", "policyArn": first_policy}),
            json!({"name": "All edited v2", "description": "after", "policyArn": second_policy}),
            json!({"name": "All edited v2", "description": "after", "policyArn": second_policy}),
        ),
        (
            "an update carrying no editable member at all",
            json!({"name": "Untouched", "description": "kept", "policyArn": first_policy}),
            json!({}),
            json!({"name": "Untouched", "description": "kept", "policyArn": first_policy}),
        ),
    ];

    for (case, create_extras, update_extras, expected) in rows {
        let created = journey_in(&router, &system_arn, &create_extras).await;
        let journey_id = journey_id_of(case, &created);

        let (status, answered) =
            update_journey(&router, &system_arn, &journey_id, &update_extras).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} {answered:?}"
        );
        let updated = answered["userJourney"].clone();

        for member in EDITABLE_MEMBERS {
            assert_eq!(
                updated[member], expected[member],
                "case {case:?}: expected {member} to be {:?}, got {:?}",
                expected[member], updated[member]
            );
        }
        assert_eq!(
            journey_id_of(case, &updated),
            journey_id,
            "case {case:?}: an update must not re-mint the journey's id, got {updated:?}"
        );
        assert_eq!(
            updated["createdAt"], created["createdAt"],
            "case {case:?}: expected createdAt to stay {:?}, got {:?}",
            created["createdAt"], updated["createdAt"]
        );
        let created_at = instant_of(case, &updated, "createdAt");
        let updated_at = instant_of(case, &updated, "updatedAt");
        assert!(
            updated_at >= created_at,
            "case {case:?}: expected updatedAt at or after createdAt {created_at}, got \
             {updated_at}"
        );

        // The read answers the same projection the update did: the store holds the edit.
        let (read_status, read_body) = get_journey(&router, &system_arn, &journey_id).await;
        assert_eq!(
            read_status, 200,
            "case {case:?}: expected the journey to read back, got {read_status} {read_body:?}"
        );
        assert_eq!(
            read_body["userJourney"], updated,
            "case {case:?}: GetUserJourney must answer what the update stored, got {:?}",
            read_body["userJourney"]
        );
    }
}

/// RULE: an update whose addressing pair names nothing stored is refused
/// `ResourceNotFoundException` and writes nothing — a missing parent is reported as the
/// missing system, and an id the addressed system does not hold as the missing journey,
/// another system's journey included.
#[tokio::test]
async fn an_update_naming_nothing_stored_is_refused_and_writes_nothing() {
    let router = emulator();
    let system_arn = system_named(&router, "Addressed-System").await;
    let other_system_arn = system_named(&router, "Other-System").await;
    let stored = journey_in(
        &router,
        &system_arn,
        &json!({"name": "Stored journey", "description": "as created"}),
    )
    .await;
    let journey_id = journey_id_of("setup", &stored);

    // A well-formed `Arn` (the modelled `length 31..` and pattern are enforced at the
    // boundary) whose resource segment names a system nothing created.
    let missing_system_id = "11111111-1111-4111-8111-111111111111";
    let missing_system_arn = format!(
        "arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SYSTEM_RESOURCE}/{missing_system_id}"
    );

    let rows: [(&str, &str, &str, String, &str); 3] = [
        (
            "a systemArn naming no stored system",
            missing_system_arn.as_str(),
            journey_id.as_str(),
            format!("{SYSTEM_RESOURCE_TYPE} not found: {missing_system_id}"),
            SYSTEM_RESOURCE_TYPE,
        ),
        (
            "the journey of another system",
            other_system_arn.as_str(),
            journey_id.as_str(),
            format!("User journey not found: {journey_id}"),
            "UserJourney",
        ),
        (
            "a userJourneyId nothing minted",
            system_arn.as_str(),
            "no-such-journey",
            "User journey not found: no-such-journey".to_string(),
            "UserJourney",
        ),
    ];

    for (case, addressed_system, addressed_id, expected_message, expected_type) in rows {
        let (status, body) = update_journey(
            &router,
            addressed_system,
            addressed_id,
            &json!({"name": "Renamed by a refused call", "description": "refused"}),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the refusal {expected_message:?}, got {:?}",
            body["message"]
        );
        assert_eq!(
            body["resourceType"].as_str(),
            Some(expected_type),
            "case {case:?}: expected resourceType {expected_type:?}, got {:?}",
            body["resourceType"]
        );
    }

    // None of those refusals touched the stored journey.
    let (status, body) = get_journey(&router, &system_arn, &journey_id).await;
    assert_eq!(status, 200, "re-reading the stored journey got {body:?}");
    assert_eq!(
        body["userJourney"], stored,
        "a refused update must leave the journey as it was, got {:?}",
        body["userJourney"]
    );
}

/// RULE: an update reaches the ONE journey its addressing pair names — the siblings it
/// shares a system with, and the same-named journeys of another system, are left as they
/// were.
#[tokio::test]
async fn an_update_touches_only_the_addressed_journey() {
    let router = emulator();
    let system_arn = system_named(&router, "Sibling-System").await;
    let other_system_arn = system_named(&router, "Bystander-System").await;

    let mut siblings = Vec::new();
    for position in 0..3 {
        siblings.push(
            journey_in(
                &router,
                &system_arn,
                &json!({"name": format!("Sibling {position}")}),
            )
            .await,
        );
    }
    let bystander = journey_in(
        &router,
        &other_system_arn,
        &json!({"name": "Sibling 1", "description": "in another system"}),
    )
    .await;

    let edited_id = journey_id_of("the middle sibling", &siblings[1]);
    let (status, answered) = update_journey(
        &router,
        &system_arn,
        &edited_id,
        &json!({"name": "Sibling 1 edited", "description": "edited"}),
    )
    .await;
    assert_eq!(status, 200, "updating the middle sibling got {answered:?}");
    assert_eq!(
        answered["userJourney"]["name"].as_str(),
        Some("Sibling 1 edited"),
        "the addressed journey must carry the new name, got {:?}",
        answered["userJourney"]
    );

    for (position, sibling) in siblings.iter().enumerate() {
        if position == 1 {
            continue;
        }
        let case = format!("sibling {position}");
        let sibling_id = journey_id_of(&case, sibling);
        let (status, body) = get_journey(&router, &system_arn, &sibling_id).await;
        assert_eq!(status, 200, "case {case:?}: re-reading got {body:?}");
        assert_eq!(
            body["userJourney"], *sibling,
            "case {case:?}: an update of another journey must change nothing here, got {:?}",
            body["userJourney"]
        );
    }

    let bystander_id = journey_id_of("the bystander system's journey", &bystander);
    let (status, body) = get_journey(&router, &other_system_arn, &bystander_id).await;
    assert_eq!(status, 200, "re-reading the bystander journey got {body:?}");
    assert_eq!(
        body["userJourney"], bystander,
        "an update in one system must not reach another system's journey, got {:?}",
        body["userJourney"]
    );
}
