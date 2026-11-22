// SPDX-License-Identifier: CC0-1.0

//! `ListUserJourneys`, end to end over HTTP: `CreateSystem` -> `CreateUserJourney` ->
//! `ListUserJourneys` -> `GetUserJourney`.
//!
//! The rule under test: a listing of a child collection REPORTS what the parent's record
//! holds — every `UserJourneySummary` it emits is projected from the same stored record
//! `GetUserJourney` answers from, the pages walk the creation order exactly once at the
//! requested `maxResults` with a position token only while journeys remain, the collection
//! is scoped to the one parent the request addresses, a parent that is not stored is a 404
//! rather than an empty page, and the call changes nothing a later caller sees.
//!
//! Every case drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.
//!
//! The suites are TABLES: rows differ along page size, journey count, which optional
//! members the stored journey carries, repeated names, which system is addressed and the
//! shape of a forged token — the recorded case is one row.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    SYSTEM_RESOURCE, SYSTEM_RESOURCE_TYPE,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
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

/// `MaxResults`' modelled range, which `ListUserJourneys$maxResults` targets.
///
/// SOURCE: `smithy_query(query="operation", operation="ListUserJourneys")` — `range {min:
/// 1, max: 100}`, `default 100`.
const MAX_RESULTS_MIN: usize = 1;
const MAX_RESULTS_MAX: usize = 100;

/// Every member `UserJourneySummary` declares, each of which must read the same stored
/// value through the summary and through the full `UserJourney` body.
///
/// SOURCE: `smithy_query(query="shape", shape="UserJourneySummary")`.
const SUMMARY_MEMBERS: [&str; 4] = ["userJourneyId", "name", "createdAt", "updatedAt"];

/// The `UserJourney` members `UserJourneySummary` does NOT declare, so a summary must not
/// report them.
///
/// SOURCE: `smithy_query(query="shape", shape="UserJourneySummary")` compared with
/// `shape="UserJourney"`.
const MEMBERS_ABSENT_FROM_A_SUMMARY: [&str; 2] = ["description", "policyArn"];

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

/// `ListUserJourneys` for `system_arn`, with whatever extra query `query` carries.
async fn list(router: &MultiServiceRouter, system_arn: &str, query: &str) -> (u16, Value) {
    let mut uri = format!("{LIST_USER_JOURNEYS}?systemArn={system_arn}");
    if !query.is_empty() {
        uri.push('&');
        uri.push_str(query);
    }
    call(router, "GET", &uri, None).await
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

/// Create a journey in `system_arn` from `extras` and answer the `UserJourney` the create
/// projected.
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

/// Create a journey named `name` in `system_arn` and answer its minted id.
async fn journey_named(router: &MultiServiceRouter, system_arn: &str, name: &str) -> String {
    let created = journey_in(router, system_arn, &json!({"name": name})).await;
    created["userJourneyId"]
        .as_str()
        .unwrap_or_else(|| panic!("no userJourneyId in {created:?}"))
        .to_string()
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

/// The `userJourneySummaries` of a successful listing.
fn summaries(case: &str, status: u16, body: &Value) -> Vec<Value> {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {body:?}"
    );
    body["userJourneySummaries"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the @required userJourneySummaries is absent from {body:?}")
        })
        .clone()
}

/// The `userJourneyId` of every summary on a page, in the order the page listed them.
fn summary_ids(case: &str, status: u16, body: &Value) -> Vec<String> {
    summaries(case, status, body)
        .iter()
        .map(|summary| {
            summary["userJourneyId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no userJourneyId in {summary:?}"))
                .to_string()
        })
        .collect()
}

// ---------------------------------------------------------------------------
// What a listing reports
// ---------------------------------------------------------------------------

/// RULE: a stored parent holding no children lists nothing, and says so with an empty list
/// and no token — the empty page is reserved for a parent that exists.
#[tokio::test]
async fn a_system_holding_no_journeys_lists_an_empty_page() {
    let router = emulator();
    let system_arn = system_named(&router, "Empty-System").await;
    let (status, body) = list(&router, &system_arn, "").await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {body:?}");
    assert_eq!(
        body["userJourneySummaries"],
        json!([]),
        "a system with no journeys must answer an empty list, got {body:?}"
    );
    assert!(
        body["nextToken"].is_null(),
        "an empty page has no nextToken, got {body:?}"
    );
}

/// RULE: every `UserJourneySummary` is projected from the stored record `GetUserJourney`
/// answers from, so each member the summary declares reads the same value, and a member the
/// summary shape does not declare is not reported.
///
/// The rows differ along which optional members the stored journey carries.
#[tokio::test]
async fn every_summary_is_the_stored_journey_projected() {
    let router = emulator();
    let system_arn = system_named(&router, "Projected-System").await;
    let policy_arn = policy_named(&router, "Projection-Policy").await;

    let rows = vec![
        ("only the required name", json!({"name": "Ab"})),
        (
            "a described journey",
            json!({"name": "Checkout journey", "description": "the paid checkout path"}),
        ),
        (
            "a journey held to a policy",
            json!({"name": "Policied journey", "policyArn": policy_arn}),
        ),
        (
            "every optional member at once",
            json!({
                "name": "Fully specified",
                "description": "described and policied",
                "policyArn": policy_arn,
            }),
        ),
    ];

    let mut created = Vec::new();
    for (case, extras) in &rows {
        created.push((case, journey_in(&router, &system_arn, extras).await));
    }

    let (status, body) = list(&router, &system_arn, "").await;
    let page = summaries("every stored journey", status, &body);
    assert_eq!(
        page.len(),
        rows.len(),
        "case every stored journey: expected {} summaries, got {page:?}",
        rows.len()
    );

    for (case, journey) in &created {
        let id = journey["userJourneyId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no userJourneyId in {journey:?}"));
        let summary = page
            .iter()
            .find(|summary| summary["userJourneyId"].as_str() == Some(id))
            .unwrap_or_else(|| panic!("case {case:?}: {id} is absent from {page:?}"));
        let (get_status, read) = get_journey(&router, &system_arn, id).await;
        assert_eq!(
            get_status, 200,
            "case {case:?}: reading {id} back got {get_status} {read:?}"
        );
        let stored = &read["userJourney"];
        for member in SUMMARY_MEMBERS {
            assert_eq!(
                summary.get(member),
                stored.get(member),
                "case {case:?}: summary member {member} must equal the stored journey's, \
                 got {:?} against {:?}",
                summary.get(member),
                stored.get(member)
            );
        }
        for member in MEMBERS_ABSENT_FROM_A_SUMMARY {
            assert!(
                summary.get(member).is_none(),
                "case {case:?}: {member} is not a UserJourneySummary member, got {summary:?}"
            );
        }
    }
}

/// RULE: a child is identified by its own minted id within its parent, so journeys created
/// under the SAME name are distinct entries and the listing reports one summary per stored
/// journey.
///
/// The rows are the repeat counts one, two and three journeys sharing a name.
#[tokio::test]
async fn repeated_names_are_listed_as_distinct_journeys() {
    let router = emulator();
    let system_arn = system_named(&router, "Repeated-Names").await;

    let shared_name = "Checkout journey";
    let mut ids: Vec<String> = Vec::new();
    for repeat in 1..=3usize {
        let case = format!("{repeat} journeys named {shared_name:?}");
        ids.push(journey_named(&router, &system_arn, shared_name).await);

        let (status, body) = list(&router, &system_arn, "").await;
        let page = summaries(&case, status, &body);
        assert_eq!(
            page.len(),
            repeat,
            "case {case:?}: expected {repeat} summaries, got {page:?}"
        );
        let listed = summary_ids(&case, status, &body);
        assert_eq!(
            listed, ids,
            "case {case:?}: every same-named journey keeps its own id, in creation order"
        );
        for summary in &page {
            assert_eq!(
                summary["name"].as_str(),
                Some(shared_name),
                "case {case:?}: every summary carries the shared name, got {summary:?}"
            );
        }
    }

    // Each of those ids resolves to its own stored journey.
    for (position, id) in ids.iter().enumerate() {
        let case = format!("journey {position}");
        let (status, body) = get_journey(&router, &system_arn, id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the journey to read back, got {status} {body:?}"
        );
        assert_eq!(
            body["userJourney"]["userJourneyId"].as_str(),
            Some(id.as_str()),
            "case {case:?}: expected the journey {id}, got {:?}",
            body["userJourney"]
        );
    }
}

/// RULE: the collection is scoped to the parent the request addresses — another parent's
/// children are never listed, even when they share a name.
#[tokio::test]
async fn a_listing_reports_only_the_addressed_systems_journeys() {
    let router = emulator();
    let first = system_named(&router, "First-System").await;
    let second = system_named(&router, "Second-System").await;
    let third = system_named(&router, "Third-System").await;

    let first_ids = vec![
        journey_named(&router, &first, "Shared name").await,
        journey_named(&router, &first, "First only").await,
    ];
    let second_ids = vec![journey_named(&router, &second, "Shared name").await];

    for (case, system_arn, expected) in [
        ("the first system", first.as_str(), first_ids.clone()),
        ("the second system", second.as_str(), second_ids.clone()),
        ("a system with no journeys", third.as_str(), Vec::new()),
    ] {
        let (status, body) = list(&router, system_arn, "").await;
        assert_eq!(
            summary_ids(case, status, &body),
            expected,
            "case {case:?}: expected exactly that system's journeys, got {body:?}"
        );
    }
}

/// RULE: a listing REPORTS — it changes nothing a later caller sees, so repeating it, and
/// reading a journey back after it, answers exactly what it did before.
#[tokio::test]
async fn listing_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    let system_arn = system_named(&router, "Untouched-System").await;
    let id = journey_named(&router, &system_arn, "Untouched by reads").await;
    let (_, before) = get_journey(&router, &system_arn, &id).await;

    let (first_status, first) = list(&router, &system_arn, "").await;
    assert_eq!(first_status, 200, "expected HTTP 200, got {first:?}");
    for round in 1..=3 {
        let (status, again) = list(&router, &system_arn, "").await;
        assert_eq!(
            (status, &again),
            (first_status, &first),
            "round {round}: a repeated listing must answer the same page, got {again:?}"
        );
    }

    let (status, after) = get_journey(&router, &system_arn, &id).await;
    assert_eq!(status, 200, "reading {id} back got {status} {after:?}");
    assert_eq!(
        after, before,
        "listing must not change the stored journey: {before:?} became {after:?}"
    );
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// RULE: `ListUserJourneys` is `@paginated` — successive pages of `maxResults` summaries,
/// resumed by the previous page's `nextToken`, enumerate every stored journey of the
/// addressed system exactly once in creation order, and only the last page omits the token.
///
/// The rows exhaust every page size from 1 to one past the number of stored journeys, plus
/// the published `maxResults` maximum.
#[tokio::test]
async fn pagination_walks_every_journey_exactly_once() {
    let router = emulator();
    let system_arn = system_named(&router, "Paged-System").await;
    // A second system holding journeys of its own: the pages must never cross into it.
    let bystander = system_named(&router, "Bystander-System").await;
    for name in ["Bystander 1", "Bystander 2"] {
        journey_named(&router, &bystander, name).await;
    }

    let mut created: Vec<String> = Vec::new();
    for position in 1..=5usize {
        created.push(journey_named(&router, &system_arn, &format!("Journey {position}")).await);
    }

    for page_size in [1usize, 2, 3, 4, 5, 6, MAX_RESULTS_MAX] {
        let case = format!("maxResults={page_size}");
        let mut seen: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        let mut pages = 0usize;
        loop {
            let mut query = format!("maxResults={page_size}");
            if let Some(cursor) = &token {
                query.push_str(&format!("&nextToken={cursor}"));
            }
            let (status, body) = list(&router, &system_arn, &query).await;
            let ids = summary_ids(&case, status, &body);
            let remaining = created.len() - seen.len();
            assert_eq!(
                ids.len(),
                page_size.min(remaining),
                "case {case:?}: page {pages} must be full while journeys remain, got {ids:?}"
            );
            for id in &ids {
                assert!(
                    !seen.contains(id),
                    "case {case:?}: {id} appeared on two pages, already seen {seen:?}"
                );
            }
            seen.extend(ids);
            pages += 1;
            assert!(
                pages <= created.len() + 1,
                "case {case:?}: pagination did not terminate after {pages} pages"
            );
            match body["nextToken"].as_str() {
                Some(cursor) => {
                    assert!(
                        seen.len() < created.len(),
                        "case {case:?}: a token was issued after the last journey, got {body:?}"
                    );
                    token = Some(cursor.to_string());
                }
                None => break,
            }
        }
        assert_eq!(
            seen, created,
            "case {case:?}: the pages must enumerate every journey in creation order"
        );
    }
}

/// RULE: `maxResults` is modelled `range 1..100`, so each published bound is accepted and a
/// value one past either bound is refused as a 400 `ValidationException`.
#[tokio::test]
async fn max_results_is_held_to_its_published_range() {
    let router = emulator();
    let system_arn = system_named(&router, "Bounded-System").await;
    journey_named(&router, &system_arn, "Bounded journey").await;

    for accepted in [MAX_RESULTS_MIN, MAX_RESULTS_MAX] {
        let (status, body) = list(&router, &system_arn, &format!("maxResults={accepted}")).await;
        assert_eq!(
            status, 200,
            "case maxResults={accepted}: expected HTTP 200, got {status} {body:?}"
        );
    }
    for refused in [MAX_RESULTS_MIN - 1, MAX_RESULTS_MAX + 1] {
        let (status, body) = list(&router, &system_arn, &format!("maxResults={refused}")).await;
        assert_eq!(
            status, 400,
            "case maxResults={refused}: expected HTTP 400, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case maxResults={refused}: expected ValidationException, got {body:?}"
        );
    }
}

/// RULE: a `nextToken` must be one this service issued; anything else is a 400
/// `ValidationException`, not an empty page.
///
/// The rows span the forgeries: a token that is not a position at all, and well-formed ones
/// naming positions this store never handed out.
#[tokio::test]
async fn a_forged_page_token_is_refused() {
    let router = emulator();
    let system_arn = system_named(&router, "Tokened-System").await;
    for name in ["Token 1", "Token 2"] {
        journey_named(&router, &system_arn, name).await;
    }

    for forged in [
        // Not the hex of a decimal position at all.
        "zzzz",
        "0",
        "not-a-token",
        // Well-formed, but naming a position this store never handed out: hex of "0"
        // (positions start at 1) and hex of "999".
        "30",
        "393939",
    ] {
        let (status, body) = list(&router, &system_arn, &format!("nextToken={forged}")).await;
        assert_eq!(
            status, 400,
            "case nextToken={forged}: expected HTTP 400, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ValidationException",
            "case nextToken={forged}: expected ValidationException, got {body:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// A parent that is not stored
// ---------------------------------------------------------------------------

/// RULE: the children live inside the parent's record, so a `systemArn` naming no stored
/// system is refused `ResourceNotFoundException` — an empty page would claim the system
/// exists and holds nothing — and that holds for a system that was deleted just as much as
/// for one nothing ever created.
#[tokio::test]
async fn a_system_that_is_not_stored_is_refused_rather_than_listed_empty() {
    let router = emulator();

    // A well-formed `Arn` (the modelled `length 31..` and pattern are enforced at the
    // boundary) whose resource segment names a system nothing created.
    let missing_system_id = "11111111-1111-4111-8111-111111111111";
    let never_created =
        format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SYSTEM_RESOURCE}/{missing_system_id}");

    // A system that held journeys and was then deleted: removing it removes them with it.
    let deleted = system_named(&router, "Deleted-System").await;
    for name in ["Gone 1", "Gone 2"] {
        journey_named(&router, &deleted, name).await;
    }
    let (status, body) = list(&router, &deleted, "").await;
    assert_eq!(
        summaries("before the delete", status, &body).len(),
        2,
        "the system must list its two journeys before it is deleted, got {body:?}"
    );
    let (status, body) = call(
        &router,
        "POST",
        DELETE_SYSTEM,
        Some(json!({"systemArn": deleted})),
    )
    .await;
    assert_eq!(status, 200, "deleting {deleted} got {status} {body:?}");

    for (case, system_arn, expected_id) in [
        (
            "a systemArn naming no stored system",
            never_created.as_str(),
            missing_system_id,
        ),
        (
            "the systemArn of a deleted system",
            deleted.as_str(),
            deleted.rsplit('/').next().expect("an ARN resource segment"),
        ),
    ] {
        let (status, body) = list(&router, system_arn, "").await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        let expected_message = format!("{SYSTEM_RESOURCE_TYPE} not found: {expected_id}");
        assert_eq!(
            body["message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the refusal {expected_message:?}, got {:?}",
            body["message"]
        );
        assert_eq!(
            body["resourceType"].as_str(),
            Some(SYSTEM_RESOURCE_TYPE),
            "case {case:?}: expected resourceType {SYSTEM_RESOURCE_TYPE:?}, got {:?}",
            body["resourceType"]
        );
    }
}
