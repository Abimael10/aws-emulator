// SPDX-License-Identifier: CC0-1.0

//! `ListSystems`, end to end over HTTP: `CreateSystem` -> `ListSystems` -> `GetSystem`.
//!
//! The rule under test: a listing REPORTS the stored systems — every `SystemSummary` it
//! emits is projected from the same stored record `GetSystem` answers from, its two counts
//! are counted from the state that holds them when the call runs, the pages walk the
//! creation order exactly once at the requested `maxResults` with a position token only
//! while systems remain, the `ouId` filter is an equality test against the organizational
//! unit the stored system sits in, and the call changes nothing a later caller sees.
//!
//! Every case drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.
//!
//! The suites are TABLES: rows differ along page size, system count, which optional members
//! the stored system carries, how many user journeys and services the system holds, the
//! filtered organizational unit and the shape of a forged token.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
const CREATE_SYSTEM: &str = "/v2/create-system";
const GET_SYSTEM: &str = "/v2/get-system";
const DELETE_SYSTEM: &str = "/v2/delete-system";
const LIST_SYSTEMS: &str = "/v2/list-systems";
const CREATE_SERVICE: &str = "/v2/create-service";
const CREATE_USER_JOURNEY: &str = "/v2/create-user-journey";

/// The Region these suites sign for.
const REGION: &str = "us-east-1";

/// `MaxResults`' modelled range, which `ListSystems$maxResults` targets.
///
/// SOURCE: `smithy_query(query="operation", operation="ListSystems")` — `range {min: 1,
/// max: 100}`, `default 100`.
const MAX_RESULTS_MIN: usize = 1;
const MAX_RESULTS_MAX: usize = 100;

/// Well-formed `OuId` values, none of which any stored system sits in: this emulator joins
/// no account to an organizational unit, so every system reports no `ouId` at all.
///
/// SOURCE: `smithy_query(query="operation", operation="ListSystems")` — `ouId` targets
/// `OuId`, `length {min: 16, max: 68}`, pattern `^ou-[a-z0-9]{4,32}-[a-z0-9]{8,32}$`. The
/// rows are that pattern at its shortest, at a middling length, and at its longest.
const WELL_FORMED_OU_IDS: [&str; 3] = [
    "ou-ab12-3456cdef",
    "ou-root1234-examplebranch01",
    "ou-abcdefghijklmnopqrstuvwxyz012345-abcdefghijklmnopqrstuvwxyz012345",
];

/// The members `SystemSummary` shares with the full `System` body, each of which must read
/// the same stored value through either reader.
///
/// SOURCE: `smithy_query(query="shape", shape="SystemSummary")` compared with
/// `shape="System"`.
const SUMMARY_MEMBERS: [&str; 7] = [
    "systemId",
    "name",
    "systemArn",
    "organizationId",
    "ouId",
    "createdAt",
    "updatedAt",
];

/// The `System` members `SystemSummary` does NOT declare, so a summary must not report
/// them.
///
/// SOURCE: `smithy_query(query="shape", shape="SystemSummary")` compared with
/// `shape="System"`.
const MEMBERS_ABSENT_FROM_A_SUMMARY: [&str; 4] =
    ["description", "sharingEnabled", "kmsKeyId", "tags"];

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

async fn list(router: &MultiServiceRouter, query: &str) -> (u16, Value) {
    let uri = if query.is_empty() {
        LIST_SYSTEMS.to_string()
    } else {
        format!("{LIST_SYSTEMS}?{query}")
    };
    call(router, "GET", &uri, None).await
}

async fn get_system(router: &MultiServiceRouter, system_arn: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("{GET_SYSTEM}?systemArn={system_arn}"),
        None,
    )
    .await
}

/// Create a system from `body` and answer its minted ARN.
async fn create_system(router: &MultiServiceRouter, body: Value) -> String {
    let (status, answered) = call(router, "POST", CREATE_SYSTEM, Some(body.clone())).await;
    assert_eq!(
        status, 200,
        "creating {body:?}: expected HTTP 200, got {status} {answered:?}"
    );
    answered["system"]["systemArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no system.systemArn in {answered:?}"))
        .to_string()
}

/// Create the simplest system under `name` and answer its ARN.
async fn create_named(router: &MultiServiceRouter, name: &str) -> String {
    create_system(router, json!({"name": name})).await
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

/// The `systemSummaries` of a successful listing.
fn summaries(case: &str, status: u16, body: &Value) -> Vec<Value> {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {body:?}"
    );
    body["systemSummaries"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the @required systemSummaries is absent from {body:?}")
        })
        .clone()
}

/// The `name` of every summary on a page, in the order the page listed them.
fn summary_names(case: &str, status: u16, body: &Value) -> Vec<String> {
    summaries(case, status, body)
        .iter()
        .map(|summary| {
            summary["name"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no name in {summary:?}"))
                .to_string()
        })
        .collect()
}

/// The one summary of `system_arn` on a default-sized page.
async fn summary_of(router: &MultiServiceRouter, case: &str, system_arn: &str) -> Value {
    let (status, body) = list(router, "").await;
    summaries(case, status, &body)
        .into_iter()
        .find(|summary| summary["systemArn"].as_str() == Some(system_arn))
        .unwrap_or_else(|| panic!("case {case:?}: {system_arn} is absent from {body:?}"))
}

// ---------------------------------------------------------------------------
// What a listing reports
// ---------------------------------------------------------------------------

/// RULE: an empty store lists nothing, and says so with an empty list and no token.
#[tokio::test]
async fn an_empty_store_lists_no_systems() {
    let router = emulator();
    let (status, body) = list(&router, "").await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {body:?}");
    assert_eq!(
        body["systemSummaries"],
        json!([]),
        "an empty store must answer an empty list, got {body:?}"
    );
    assert!(
        body["nextToken"].is_null(),
        "an empty page has no nextToken, got {body:?}"
    );
}

/// RULE: every `SystemSummary` is projected from the stored record `GetSystem` answers
/// from, so each member the summary shares with the full `System` body reads the same
/// value, and a member the summary shape does not declare is not reported.
///
/// The rows differ along which optional members the stored system carries.
#[tokio::test]
async fn every_summary_is_the_stored_system_projected() {
    let router = emulator();
    let rows = vec![
        ("only the required name", json!({"name": "Ab"})),
        (
            "described",
            json!({"name": "described-system", "description": "The checkout path"}),
        ),
        (
            "sharing on, customer-managed key, tagged",
            json!({
                "name": "Tier0_Payments",
                "description": "Tier-0 payments",
                "sharingEnabled": true,
                "kmsKeyId": "arn:aws:kms:us-east-1:000000000000:key/11111111-2222-3333-4444-555555555555",
                "tags": {"tier": "0"},
            }),
        ),
        (
            "sharing explicitly off",
            json!({"name": "sharing-off", "sharingEnabled": false}),
        ),
    ];

    let mut arns = Vec::new();
    for (case, body) in &rows {
        arns.push((case, create_system(&router, body.clone()).await));
    }

    let (status, body) = list(&router, "").await;
    let page = summaries("every stored system", status, &body);
    assert_eq!(
        page.len(),
        rows.len(),
        "case every stored system: expected {} summaries, got {page:?}",
        rows.len()
    );

    for (case, arn) in &arns {
        let summary = page
            .iter()
            .find(|summary| summary["systemArn"].as_str() == Some(arn.as_str()))
            .unwrap_or_else(|| panic!("case {case:?}: {arn} is absent from {page:?}"));
        let (get_status, read) = get_system(&router, arn).await;
        assert_eq!(
            get_status, 200,
            "case {case:?}: reading {arn} back got {get_status} {read:?}"
        );
        let system = &read["system"];
        for member in SUMMARY_MEMBERS {
            assert_eq!(
                summary.get(member),
                system.get(member),
                "case {case:?}: summary member {member} must equal the stored system's, \
                 got {:?} against {:?}",
                summary.get(member),
                system.get(member)
            );
        }
        for member in MEMBERS_ABSENT_FROM_A_SUMMARY {
            assert!(
                summary.get(member).is_none(),
                "case {case:?}: {member} is not a SystemSummary member, got {summary:?}"
            );
        }
    }
}

/// RULE: `userJourneysCount` is the number of user journeys in the system, counted from
/// the journeys the system holds, so a listing reports the count in force when it ran.
///
/// The rows are the counts zero, one and two journeys.
#[tokio::test]
async fn the_summary_counts_the_user_journeys_of_the_system() {
    let router = emulator();
    let arn = create_named(&router, "counted-journeys").await;

    for expected in [0usize, 1, 2] {
        if expected > 0 {
            let body = json!({
                "systemArn": arn,
                "name": format!("Journey {expected}"),
            });
            let (status, created) = call(&router, "POST", CREATE_USER_JOURNEY, Some(body)).await;
            assert_eq!(
                status, 200,
                "case {expected} journeys: creating a journey got {status} {created:?}"
            );
        }
        let case = format!("{expected} journeys");
        let summary = summary_of(&router, &case, &arn).await;
        assert_eq!(
            summary["userJourneysCount"].as_i64(),
            Some(expected as i64),
            "case {case:?}: expected userJourneysCount {expected}, got {summary:?}"
        );
    }
}

/// RULE: `servicesCount` is the number of services in the system, counted from the
/// services that reference it, so a listing reports the count in force when it ran rather
/// than one frozen at create time.
///
/// The rows are the counts zero, one and two referencing services.
#[tokio::test]
async fn the_summary_counts_the_services_in_the_system() {
    let router = emulator();
    let arn = create_named(&router, "counted-services").await;
    let other = create_named(&router, "unreferenced-system").await;

    for expected in [0usize, 1, 2] {
        if expected > 0 {
            let body = json!({
                "name": format!("counted-svc-{expected}"),
                "regions": [REGION],
                "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
                "associatedSystems": [{"systemArn": arn}],
            });
            let (status, created) = call(&router, "POST", CREATE_SERVICE, Some(body)).await;
            assert_eq!(
                status, 200,
                "case {expected} services: creating a referencing service got {status} \
                 {created:?}"
            );
        }
        let case = format!("{expected} services");
        let summary = summary_of(&router, &case, &arn).await;
        assert_eq!(
            summary["servicesCount"].as_i64(),
            Some(expected as i64),
            "case {case:?}: expected servicesCount {expected}, got {summary:?}"
        );
        let untouched = summary_of(&router, &case, &other).await;
        assert_eq!(
            untouched["servicesCount"].as_i64(),
            Some(0),
            "case {case:?}: a system no service references counts none, got {untouched:?}"
        );
    }
}

/// RULE: a listing REPORTS — it changes nothing a later caller sees, so repeating it, and
/// reading the system back after it, answers exactly what it did before.
#[tokio::test]
async fn listing_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    let arn = create_named(&router, "untouched-by-reads").await;
    let (_, before) = get_system(&router, &arn).await;

    let (first_status, first) = list(&router, "").await;
    assert_eq!(first_status, 200, "expected HTTP 200, got {first:?}");
    for round in 1..=3 {
        let (status, again) = list(&router, "").await;
        assert_eq!(
            (status, &again),
            (first_status, &first),
            "round {round}: a repeated listing must answer the same page, got {again:?}"
        );
    }

    let (status, after) = get_system(&router, &arn).await;
    assert_eq!(status, 200, "reading {arn} back got {status} {after:?}");
    assert_eq!(
        after, before,
        "listing must not change the stored system: {before:?} became {after:?}"
    );
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// RULE: `ListSystems` is `@paginated` — successive pages of `maxResults` summaries,
/// resumed by the previous page's `nextToken`, enumerate every stored system exactly once
/// in creation order, and only the last page omits the token.
///
/// The rows exhaust every page size from 1 to one past the number of stored systems, plus
/// the published `maxResults` maximum.
#[tokio::test]
async fn pagination_walks_every_system_exactly_once() {
    let router = emulator();
    let created: Vec<String> = ["sys-1", "sys-2", "sys-3", "sys-4", "sys-5"]
        .iter()
        .map(|name| name.to_string())
        .collect();
    for name in &created {
        create_named(&router, name).await;
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
            let (status, body) = list(&router, &query).await;
            let names = summary_names(&case, status, &body);
            let remaining = created.len() - seen.len();
            assert_eq!(
                names.len(),
                page_size.min(remaining),
                "case {case:?}: page {pages} must be full while systems remain, got {names:?}"
            );
            for name in &names {
                assert!(
                    !seen.contains(name),
                    "case {case:?}: {name} appeared on two pages, already seen {seen:?}"
                );
            }
            seen.extend(names);
            pages += 1;
            assert!(
                pages <= created.len() + 1,
                "case {case:?}: pagination did not terminate after {pages} pages"
            );
            match body["nextToken"].as_str() {
                Some(cursor) => {
                    assert!(
                        seen.len() < created.len(),
                        "case {case:?}: a token was issued after the last system, got {body:?}"
                    );
                    token = Some(cursor.to_string());
                }
                None => break,
            }
        }
        assert_eq!(
            seen, created,
            "case {case:?}: the pages must enumerate every system in creation order"
        );
    }
}

/// RULE: `maxResults` is modelled `range 1..100`, so each published bound is accepted and a
/// value one past either bound is refused as a 400 `ValidationException`.
#[tokio::test]
async fn max_results_is_held_to_its_published_range() {
    let router = emulator();
    create_named(&router, "bounded-system").await;

    for accepted in [MAX_RESULTS_MIN, MAX_RESULTS_MAX] {
        let (status, body) = list(&router, &format!("maxResults={accepted}")).await;
        assert_eq!(
            status, 200,
            "case maxResults={accepted}: expected HTTP 200, got {status} {body:?}"
        );
    }
    for refused in [MAX_RESULTS_MIN - 1, MAX_RESULTS_MAX + 1] {
        let (status, body) = list(&router, &format!("maxResults={refused}")).await;
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

/// RULE: a `nextToken` names a POSITION in the creation order, so a page resumes the same
/// scan even when the record the previous page ended on has since been deleted; and a
/// token this service never issued is a 400 `ValidationException`, not an empty page.
#[tokio::test]
async fn a_page_token_survives_a_delete_and_rejects_a_forged_token() {
    let router = emulator();
    let mut arns = Vec::new();
    for name in ["tok-1", "tok-2", "tok-3", "tok-4"] {
        arns.push(create_named(&router, name).await);
    }

    let (status, first_page) = list(&router, "maxResults=2").await;
    assert_eq!(
        summary_names("the first page", status, &first_page),
        vec!["tok-1", "tok-2"],
        "the first page must be the first two systems, got {first_page:?}"
    );
    let token = first_page["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("no nextToken in {first_page:?}"))
        .to_string();

    let (status, deleted) = call(
        &router,
        "POST",
        DELETE_SYSTEM,
        Some(json!({"systemArn": arns[1]})),
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting the system the first page ended on got {status} {deleted:?}"
    );

    let (status, resumed) = list(&router, &format!("maxResults=2&nextToken={token}")).await;
    assert_eq!(
        summary_names("resuming after a delete", status, &resumed),
        vec!["tok-3", "tok-4"],
        "a token must resume the same scan after a delete, got {resumed:?}"
    );

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
        let (status, body) = list(&router, &format!("nextToken={forged}")).await;
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
// The ouId filter
// ---------------------------------------------------------------------------

/// RULE: `ouId` selects the systems sitting in that organizational unit — an equality test
/// against the stored system's own `ouId` — so a filter no stored system matches lists
/// none, and an absent filter constrains nothing.
///
/// The rows are: no filter, then each well-formed `OuId` in the published pattern's catalog
/// (shortest, middling, longest), none of which any stored system sits in because this
/// emulator joins no account to an organizational unit.
#[tokio::test]
async fn the_ou_filter_selects_by_the_organizational_unit() {
    let router = emulator();
    let names = ["ou-1", "ou-2", "ou-3"];
    for name in names {
        let arn = create_named(&router, name).await;
        let summary = summary_of(&router, name, &arn).await;
        assert!(
            summary.get("ouId").is_none_or(Value::is_null),
            "case {name:?}: a created system sits in no organizational unit, got {summary:?}"
        );
    }

    let (status, body) = list(&router, "").await;
    let page = summaries("no filter", status, &body);
    assert_eq!(
        page.len(),
        names.len(),
        "case no filter: expected {} summaries, got {page:?}",
        names.len()
    );

    for ou_id in WELL_FORMED_OU_IDS {
        let case = format!("ouId={ou_id}");
        let (status, body) = list(&router, &format!("ouId={ou_id}")).await;
        let page = summaries(&case, status, &body);
        assert!(
            page.is_empty(),
            "case {case:?}: no stored system sits in that unit, got {page:?}"
        );
        assert!(
            body["nextToken"].is_null(),
            "case {case:?}: an empty page has no nextToken, got {body:?}"
        );
    }
}

/// RULE: a listing reports what is stored NOW, so a deleted system is gone from the next
/// page and a newly created one appears on it.
#[tokio::test]
async fn a_listing_follows_creates_and_deletes() {
    let router = emulator();
    let first = create_named(&router, "churn-1").await;
    create_named(&router, "churn-2").await;

    let (status, body) = list(&router, "").await;
    assert_eq!(
        summary_names("both stored", status, &body),
        vec!["churn-1", "churn-2"],
        "both systems must be listed, got {body:?}"
    );

    let (status, deleted) = call(
        &router,
        "POST",
        DELETE_SYSTEM,
        Some(json!({"systemArn": first})),
    )
    .await;
    assert_eq!(status, 200, "deleting {first} got {status} {deleted:?}");

    let (status, body) = list(&router, "").await;
    assert_eq!(
        summary_names("after the delete", status, &body),
        vec!["churn-2"],
        "a deleted system must be gone from the next page, got {body:?}"
    );

    create_named(&router, "churn-3").await;
    let (status, body) = list(&router, "").await;
    assert_eq!(
        summary_names("after a later create", status, &body),
        vec!["churn-2", "churn-3"],
        "a newly created system must appear on the next page, got {body:?}"
    );
}
