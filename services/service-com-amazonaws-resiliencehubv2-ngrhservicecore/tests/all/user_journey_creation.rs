// SPDX-License-Identifier: CC0-1.0

//! `CreateUserJourney`, end to end over HTTP: `CreateSystem` -> `CreateUserJourney` ->
//! `GetUserJourney` / `ListUserJourneys`.
//!
//! The rule under test: a create of a child resource STORES it inside the parent the
//! request names and answers the projection every later read answers from — so the journey
//! reads back unchanged through `GetUserJourney` and appears in that parent's listing alone
//! — while every call the operation cannot serve is refused with the error its model
//! declares and leaves the store exactly as it was: a `systemArn` naming no stored system
//! is a 404, an `@idempotencyToken` presented for creation parameters it was not first
//! accepted with is a 409, a create past the per-parent ceiling is a 402, and a member
//! outside its modelled constraints is a 400.
//!
//! Every case drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.
//!
//! The suites are TABLES: rows differ along which optional members the create carries, how
//! long each value is, which member a reused token disagrees on, which parent is addressed
//! and how many journeys the parent already holds.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    not_found_message, refusal_resource_id, two_requests_one_token_message,
    MAX_USER_JOURNEYS_PER_SYSTEM, SYSTEM_RESOURCE, SYSTEM_RESOURCE_TYPE,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
///
/// SOURCE: `smithy_query(query="operation", operation="CreateUserJourney")` — `http
/// {method: POST, uri: /v2/create-user-journey}`.
const CREATE_SYSTEM: &str = "/v2/create-system";
const DELETE_SYSTEM: &str = "/v2/delete-system";
const CREATE_POLICY: &str = "/v2/create-policy";
const CREATE_USER_JOURNEY: &str = "/v2/create-user-journey";
const GET_USER_JOURNEY: &str = "/v2/get-user-journey";
const LIST_USER_JOURNEYS: &str = "/v2/list-user-journeys";

/// The Region these suites sign for.
const REGION: &str = "us-east-1";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// `EntityLabel`'s published `@length`, which `CreateUserJourney$name` targets.
///
/// SOURCE: `smithy_query(query="shape", shape="EntityLabel")` — `length {min: 2, max: 60}`.
const ENTITY_LABEL_MIN: usize = 2;
const ENTITY_LABEL_MAX: usize = 60;

/// `EntityDescription`'s published `@length` ceiling, which `CreateUserJourney$description`
/// targets.
///
/// SOURCE: `smithy_query(query="shape", shape="EntityDescription")` — `length {max: 500}`.
const ENTITY_DESCRIPTION_MAX: usize = 500;

/// Every member `UserJourney` declares, each of which a create must report.
///
/// SOURCE: `smithy_query(query="shape", shape="UserJourney")`.
const JOURNEY_MEMBERS: [&str; 6] = [
    "userJourneyId",
    "name",
    "description",
    "policyArn",
    "createdAt",
    "updatedAt",
];

/// The members a create ECHOES from the request, as opposed to the ones the service derives
/// (the id and the two timestamps).
///
/// SOURCE: `smithy_query(query="operation", operation="CreateUserJourney")` compared with
/// `shape="UserJourney"`.
const ECHOED_MEMBERS: [&str; 3] = ["name", "description", "policyArn"];

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

/// Create a system named `name` and answer its ARN.
async fn system_named(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(router, "POST", CREATE_SYSTEM, Some(json!({"name": name}))).await;
    assert_eq!(status, 200, "creating system {name:?} got {status} {body:?}");
    body["system"]["systemArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no system.systemArn in {body:?}"))
        .to_string()
}

/// Create a resilience policy named `name` and answer its ARN.
async fn policy_named(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(router, "POST", CREATE_POLICY, Some(json!({"name": name}))).await;
    assert_eq!(status, 200, "creating policy {name:?} got {status} {body:?}");
    body["policy"]["policyArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no policy.policyArn in {body:?}"))
        .to_string()
}

/// `CreateUserJourney` with `body` exactly as given.
async fn create_journey(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", CREATE_USER_JOURNEY, Some(body)).await
}

/// `CreateUserJourney` in `system_arn` from `extras`, which must succeed; answers the
/// `UserJourney` the create projected.
async fn journey_in(router: &MultiServiceRouter, system_arn: &str, extras: &Value) -> Value {
    let mut body = extras.clone();
    body["systemArn"] = json!(system_arn);
    let (status, answered) = create_journey(router, body).await;
    assert_eq!(
        status, 200,
        "creating a journey from {extras:?} got {status} {answered:?}"
    );
    answered["userJourney"].clone()
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

/// `ListUserJourneys$maxResults`' published maximum, used so one listing reports every
/// journey a system can hold.
///
/// SOURCE: `smithy_query(query="operation", operation="ListUserJourneys")` — `range {min: 1,
/// max: 100}`.
const MAX_RESULTS_MAX: usize = 100;

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

/// A name of exactly `length` characters that satisfies `EntityLabel`'s `@pattern`
/// (`^[A-Za-z0-9][A-Za-z0-9 _\-]{1,59}$`): an alphanumeric first character and the rest
/// drawn from the same admitted set.
fn name_of_length(length: usize) -> String {
    "J".repeat(length)
}

// ---------------------------------------------------------------------------
// What a create stores
// ---------------------------------------------------------------------------

/// RULE: a create stores the child inside the parent the request names and answers the
/// projection every later read answers from — so each echoed member carries exactly what
/// the request supplied, the id is freshly minted per create, a journey that nothing has
/// changed yet reports the same `createdAt` and `updatedAt`, `GetUserJourney` answers the
/// create's own body, and the journey is listed by that parent and by no other.
///
/// The rows differ along which optional members the create carries and how long each value
/// is: the bare required name, a name at each published `EntityLabel` bound, the space
/// `EntityLabel` admits, an empty and a ceiling-length description, and a `policyArn`.
#[tokio::test]
async fn a_create_stores_the_journey_in_the_system_it_names() {
    let router = emulator();
    let system_arn = system_named(&router, "Created-System").await;
    let bystander = system_named(&router, "Bystander-System").await;
    let policy_arn = policy_named(&router, "Journey-Policy").await;

    let rows: Vec<(String, Value)> = vec![
        ("only the required name".to_string(), json!({"name": "Ab"})),
        (
            format!("a name at EntityLabel's minimum of {ENTITY_LABEL_MIN}"),
            json!({"name": name_of_length(ENTITY_LABEL_MIN)}),
        ),
        (
            format!("a name at EntityLabel's maximum of {ENTITY_LABEL_MAX}"),
            json!({"name": name_of_length(ENTITY_LABEL_MAX)}),
        ),
        (
            "a name carrying the space EntityLabel admits".to_string(),
            json!({"name": "Checkout journey"}),
        ),
        (
            "an empty description".to_string(),
            json!({"name": "Undescribed", "description": ""}),
        ),
        (
            format!("a description at EntityDescription's ceiling of {ENTITY_DESCRIPTION_MAX}"),
            json!({
                "name": "Longest Described",
                "description": "d".repeat(ENTITY_DESCRIPTION_MAX),
            }),
        ),
        (
            "a journey held to a policy".to_string(),
            json!({"name": "Policy Held", "policyArn": policy_arn.clone()}),
        ),
        (
            "every optional member at once".to_string(),
            json!({
                "name": "Fully Specified",
                "description": "described and policied",
                "policyArn": policy_arn,
            }),
        ),
    ];

    let mut minted: Vec<String> = Vec::new();
    for (case, extras) in &rows {
        let created = journey_in(&router, &system_arn, extras).await;

        for member in ECHOED_MEMBERS {
            assert_eq!(
                created.get(member),
                extras.get(member),
                "case {case:?}: expected {member} to echo {:?}, got {:?}",
                extras.get(member),
                created.get(member)
            );
        }
        let id = created["userJourneyId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no userJourneyId in {created:?}"))
            .to_string();
        assert!(
            !minted.contains(&id),
            "case {case:?}: every create mints a fresh id, but {id} repeated among {minted:?}"
        );
        assert_eq!(
            created["createdAt"], created["updatedAt"],
            "case {case:?}: a journey nothing has changed was last updated when it was \
             created, got {created:?}"
        );

        // The read answers exactly what the create projected, member for member.
        let (status, read) = get_journey(&router, &system_arn, &id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading {id} back got {status} {read:?}"
        );
        assert_eq!(
            read["userJourney"], created,
            "case {case:?}: GetUserJourney must answer what the create stored, got {:?} \
             against {created:?}",
            read["userJourney"]
        );
        for member in JOURNEY_MEMBERS {
            assert_eq!(
                read["userJourney"].get(member),
                created.get(member),
                "case {case:?}: member {member} must read back as created, got {:?} against {:?}",
                read["userJourney"].get(member),
                created.get(member)
            );
        }

        minted.push(id);
    }

    assert_eq!(
        listed_ids(&router, &system_arn).await,
        minted,
        "the addressed system must list every journey it was given, in creation order"
    );
    assert_eq!(
        listed_ids(&router, &bystander).await,
        Vec::<String>::new(),
        "a create stores the journey in the system it names and in no other"
    );
}

// ---------------------------------------------------------------------------
// A parent that is not stored
// ---------------------------------------------------------------------------

/// RULE: a child is created INSIDE its parent, so a parent identifier naming nothing stored
/// is refused `ResourceNotFoundException` (HTTP 404) reporting that resource's type and
/// identifier, and the refused call stores nothing.
///
/// The rows differ along why the parent is not there: an identifier nothing ever created,
/// and one whose resource was deleted.
#[tokio::test]
async fn a_create_naming_no_stored_system_is_refused_and_stores_nothing() {
    let router = emulator();

    // A live system whose contents must be untouched by every refusal below.
    let live = system_named(&router, "Live-System").await;
    let survivor = journey_in(&router, &live, &json!({"name": "Survivor"})).await;
    let survivor_id = survivor["userJourneyId"]
        .as_str()
        .expect("a userJourneyId")
        .to_string();

    // A well-formed `Arn` (the modelled `length 31..` and pattern are enforced at the
    // boundary) whose resource segment names a system nothing created.
    let never_created = format!(
        "arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SYSTEM_RESOURCE}\
         /11111111-1111-4111-8111-111111111111"
    );

    // A system that is deleted after it is created: its journeys go with it.
    let deleted = system_named(&router, "Deleted-System").await;
    journey_in(&router, &deleted, &json!({"name": "Doomed"})).await;
    let (status, body) = call(
        &router,
        "POST",
        DELETE_SYSTEM,
        Some(json!({"systemArn": deleted})),
    )
    .await;
    assert_eq!(status, 200, "deleting {deleted} got {status} {body:?}");

    for (case, system_arn) in [
        ("a systemArn naming no stored system", never_created.as_str()),
        ("the systemArn of a deleted system", deleted.as_str()),
    ] {
        let (status, body) =
            create_journey(&router, json!({"systemArn": system_arn, "name": "Orphan"})).await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        let expected_id = refusal_resource_id(SYSTEM_RESOURCE_TYPE, system_arn);
        let expected_message = not_found_message(SYSTEM_RESOURCE_TYPE, expected_id);
        assert_eq!(
            body["message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the refusal {expected_message:?}, got {:?}",
            body["message"]
        );
        assert_eq!(
            body["resourceId"].as_str(),
            Some(expected_id),
            "case {case:?}: expected resourceId {expected_id:?}, got {:?}",
            body["resourceId"]
        );
        assert_eq!(
            body["resourceType"].as_str(),
            Some(SYSTEM_RESOURCE_TYPE),
            "case {case:?}: expected resourceType {SYSTEM_RESOURCE_TYPE:?}, got {:?}",
            body["resourceType"]
        );
    }

    assert_eq!(
        listed_ids(&router, &live).await,
        vec![survivor_id.clone()],
        "a refused create must store nothing, so the live system still holds just its one \
         journey"
    );
    let (status, read) = get_journey(&router, &live, &survivor_id).await;
    assert_eq!(status, 200, "reading {survivor_id} back got {status} {read:?}");
    assert_eq!(
        read["userJourney"], survivor,
        "a refused create must leave the stored journey exactly as it was"
    );
}

// ---------------------------------------------------------------------------
// The idempotency token
// ---------------------------------------------------------------------------

/// RULE: an `@idempotencyToken` makes a RETRY of one create safe — the same token with the
/// same creation parameters answers the resource the first call created rather than a second
/// one — while the same token offered for DIFFERENT parameters is two requests wearing one
/// token, refused `ConflictException` (HTTP 409), storing nothing.
///
/// The rows differ along which member the repeat disagrees on: the name, an added
/// description, a changed description, an added `policyArn`.
#[tokio::test]
async fn a_token_is_a_retry_only_of_the_create_it_was_first_used_for() {
    let router = emulator();
    let system_arn = system_named(&router, "Tokened-System").await;
    let policy_arn = policy_named(&router, "Token-Policy").await;

    let token = "journey-token.1";
    let first = json!({
        "systemArn": system_arn,
        "name": "Checkout journey",
        "description": "the paid checkout path",
        "clientToken": token,
    });
    let (status, answered) = create_journey(&router, first.clone()).await;
    assert_eq!(status, 200, "the first create got {status} {answered:?}");
    let created = answered["userJourney"].clone();
    let created_id = created["userJourneyId"]
        .as_str()
        .expect("a userJourneyId")
        .to_string();

    // The same token with the same parameters is a retry: it answers the first journey.
    for round in 1..=2 {
        let (status, replay) = create_journey(&router, first.clone()).await;
        assert_eq!(
            status, 200,
            "round {round}: a replayed token got {status} {replay:?}"
        );
        assert_eq!(
            replay["userJourney"], created,
            "round {round}: a replayed token must answer the first journey, got {:?}",
            replay["userJourney"]
        );
    }

    let expected_message = two_requests_one_token_message(token);
    let rows: Vec<(&str, Value)> = vec![
        (
            "a different name",
            json!({"name": "Refund journey", "description": "the paid checkout path"}),
        ),
        (
            "a different description",
            json!({"name": "Checkout journey", "description": "another path"}),
        ),
        (
            "no description at all",
            json!({"name": "Checkout journey"}),
        ),
        (
            "an added policyArn",
            json!({
                "name": "Checkout journey",
                "description": "the paid checkout path",
                "policyArn": policy_arn,
            }),
        ),
    ];
    for (case, extras) in rows {
        let mut body = extras.clone();
        body["systemArn"] = json!(system_arn);
        body["clientToken"] = json!(token);
        let (status, refused) = create_journey(&router, body).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409, got {status} {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {refused:?}"
        );
        assert_eq!(
            refused["message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the refusal {expected_message:?}, got {:?}",
            refused["message"]
        );
    }

    // Nothing the refused calls carried was stored, and the first journey is untouched.
    assert_eq!(
        listed_ids(&router, &system_arn).await,
        vec![created_id.clone()],
        "a refused token must store nothing, so the system still holds just the first journey"
    );
    let (status, read) = get_journey(&router, &system_arn, &created_id).await;
    assert_eq!(status, 200, "reading {created_id} back got {status} {read:?}");
    assert_eq!(
        read["userJourney"], created,
        "a refused token must leave the first journey exactly as it was"
    );
}

/// RULE: a child's `@idempotencyToken` is remembered inside the parent it created the child
/// in, so the SAME token used under another parent is a create of its own rather than a
/// replay of the first parent's child.
#[tokio::test]
async fn a_client_token_is_scoped_to_the_system_it_created_in() {
    let router = emulator();
    let first = system_named(&router, "First-System").await;
    let second = system_named(&router, "Second-System").await;

    let token = "shared-token";
    let mut ids: Vec<(String, String)> = Vec::new();
    for (case, system_arn) in [("the first system", &first), ("the second system", &second)] {
        let created = journey_in(
            &router,
            system_arn,
            &json!({"name": "Shared Journey", "clientToken": token}),
        )
        .await;
        ids.push((
            case.to_string(),
            created["userJourneyId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no userJourneyId in {created:?}"))
                .to_string(),
        ));
    }

    assert_ne!(
        ids[0].1, ids[1].1,
        "one token under two systems creates two journeys, got {ids:?}"
    );
    for (case, system_arn, expected) in [
        ("the first system", first.as_str(), ids[0].1.clone()),
        ("the second system", second.as_str(), ids[1].1.clone()),
    ] {
        assert_eq!(
            listed_ids(&router, system_arn).await,
            vec![expected.clone()],
            "case {case:?}: each system holds exactly the journey created in it"
        );
        let (status, body) = get_journey(&router, system_arn, &expected).await;
        assert_eq!(
            status, 200,
            "case {case:?}: reading {expected} back got {status} {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// The per-parent ceiling
// ---------------------------------------------------------------------------

/// RULE: the number of children one parent holds is bounded, so a create that would pass the
/// ceiling is refused `ServiceQuotaExceededException` (HTTP 402) and stores nothing, and the
/// bound is PER parent — another parent at or below it still accepts creates.
#[tokio::test]
async fn the_journey_ceiling_is_enforced_per_system() {
    let router = emulator();
    let filled = system_named(&router, "Filled-System").await;
    let roomy = system_named(&router, "Roomy-System").await;

    let mut stored: Vec<String> = Vec::new();
    for position in 1..=MAX_USER_JOURNEYS_PER_SYSTEM {
        let created = journey_in(
            &router,
            &filled,
            &json!({"name": format!("Journey {position}")}),
        )
        .await;
        stored.push(
            created["userJourneyId"]
                .as_str()
                .unwrap_or_else(|| panic!("journey {position}: no userJourneyId in {created:?}"))
                .to_string(),
        );
    }
    assert_eq!(
        stored.len(),
        MAX_USER_JOURNEYS_PER_SYSTEM,
        "the system must accept every create up to the ceiling"
    );

    // Two rows past the ceiling: the refusal is the same every time, and the second proves
    // the first stored nothing.
    for round in 1..=2 {
        let (status, refused) = create_journey(
            &router,
            json!({"systemArn": filled, "name": format!("Past the ceiling {round}")}),
        )
        .await;
        assert_eq!(
            status, 402,
            "round {round}: expected HTTP 402, got {status} {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "ServiceQuotaExceededException",
            "round {round}: expected ServiceQuotaExceededException, got {refused:?}"
        );
        let message = refused["message"].as_str().unwrap_or_default();
        assert!(
            message.contains(&filled) && message.contains(&MAX_USER_JOURNEYS_PER_SYSTEM.to_string()),
            "round {round}: the refusal must name the system and the ceiling it reached, \
             got {message:?}"
        );
        assert_eq!(
            listed_ids(&router, &filled).await,
            stored,
            "round {round}: a refused create must store nothing"
        );
    }

    // The ceiling is per system: another system still accepts a create.
    let elsewhere = journey_in(&router, &roomy, &json!({"name": "Roomy Journey"})).await;
    assert_eq!(
        listed_ids(&router, &roomy).await,
        vec![elsewhere["userJourneyId"]
            .as_str()
            .expect("a userJourneyId")
            .to_string()],
        "one system at its ceiling must not bound another"
    );
}

// ---------------------------------------------------------------------------
// The modelled constraints
// ---------------------------------------------------------------------------

/// RULE: every member is held to the constraints the model publishes for its shape, so a
/// value at a published bound is accepted and one past it — or one outside the member's
/// pattern — is refused `ValidationException` (HTTP 400) before anything is stored.
///
/// The rows walk each published bound of `EntityLabel`'s and `EntityDescription`'s `@length`
/// and one past it, plus a name the `EntityLabel` pattern does not admit and a token the
/// `ClientToken` pattern does not admit.
#[tokio::test]
async fn every_member_is_held_to_its_published_constraints() {
    let router = emulator();
    let system_arn = system_named(&router, "Constrained-System").await;

    let accepted: Vec<(String, Value)> = vec![
        (
            format!("a name of EntityLabel's minimum length {ENTITY_LABEL_MIN}"),
            json!({"name": name_of_length(ENTITY_LABEL_MIN)}),
        ),
        (
            format!("a name of EntityLabel's maximum length {ENTITY_LABEL_MAX}"),
            json!({"name": name_of_length(ENTITY_LABEL_MAX)}),
        ),
        (
            format!("a description of EntityDescription's maximum length {ENTITY_DESCRIPTION_MAX}"),
            json!({
                "name": "Described To The Limit",
                "description": "d".repeat(ENTITY_DESCRIPTION_MAX),
            }),
        ),
    ];
    let mut stored: Vec<String> = Vec::new();
    for (case, extras) in &accepted {
        let created = journey_in(&router, &system_arn, extras).await;
        stored.push(
            created["userJourneyId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no userJourneyId in {created:?}"))
                .to_string(),
        );
    }

    let refused: Vec<(String, Value)> = vec![
        (
            format!("a name one short of EntityLabel's minimum {ENTITY_LABEL_MIN}"),
            json!({"name": name_of_length(ENTITY_LABEL_MIN - 1)}),
        ),
        (
            format!("a name one past EntityLabel's maximum {ENTITY_LABEL_MAX}"),
            json!({"name": name_of_length(ENTITY_LABEL_MAX + 1)}),
        ),
        (
            format!("a description one past EntityDescription's maximum {ENTITY_DESCRIPTION_MAX}"),
            json!({
                "name": "Over Described",
                "description": "d".repeat(ENTITY_DESCRIPTION_MAX + 1),
            }),
        ),
        (
            "a name EntityLabel's pattern does not admit".to_string(),
            json!({"name": " Leading space"}),
        ),
        (
            "a token ClientToken's pattern does not admit".to_string(),
            json!({"name": "Bad Token", "clientToken": "token with spaces"}),
        ),
        (
            "no name at all, which the model requires".to_string(),
            json!({"description": "nameless"}),
        ),
    ];
    for (case, extras) in &refused {
        let mut body = extras.clone();
        body["systemArn"] = json!(system_arn);
        let (status, answered) = create_journey(&router, body).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400, got {status} {answered:?}"
        );
        assert_eq!(
            error_code(&answered),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {answered:?}"
        );
    }

    assert_eq!(
        listed_ids(&router, &system_arn).await,
        stored,
        "a refused create must store nothing, so only the accepted rows are held"
    );
}
