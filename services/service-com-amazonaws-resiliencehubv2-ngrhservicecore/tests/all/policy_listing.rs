// SPDX-License-Identifier: CC0-1.0

//! `ListPolicies`, end to end over HTTP: `CreatePolicy` -> `ListPolicies` -> `GetPolicy`.
//!
//! The rule under test: a listing REPORTS the stored policies — every `PolicySummary` it
//! emits is projected from the same stored record `GetPolicy` answers from, the pages walk
//! the creation order exactly once at the requested `maxResults` with a position token
//! only while policies remain, the `accountId` filter is an equality test against the
//! account the policy's ARN carries, and the call changes nothing a later caller sees.
//!
//! Every case drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.
//!
//! The suites are TABLES: rows differ along page size, policy count, which optional target
//! blocks the stored policy carries, the filtered account and the shape of a forged token —
//! the recorded case is one row.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
const CREATE_POLICY: &str = "/v2/create-policy";
const GET_POLICY: &str = "/v2/get-policy";
const DELETE_POLICY: &str = "/v2/delete-policy";
const LIST_POLICIES: &str = "/v2/list-policies";
const CREATE_SERVICE: &str = "/v2/create-service";

/// The Region these suites sign for.
const REGION: &str = "us-east-1";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// An `AccountId` that is well-formed (`len 12..12`, `^[0-9]{12}$`) and is NOT the account
/// the emulator mints ARNs under, so no stored policy is owned by it.
///
/// SOURCE: `smithy_query(query="operation", operation="ListPolicies")` —
/// `accountId` targets `AccountId`.
const OTHER_ACCOUNT: &str = "123456789012";

/// `MaxResults`' modelled range, which `ListPolicies$maxResults` targets.
///
/// SOURCE: `smithy_query(query="operation", operation="ListPolicies")` — `range {min: 1,
/// max: 100}`, `default 100`.
const MAX_RESULTS_MIN: usize = 1;
const MAX_RESULTS_MAX: usize = 100;

/// The members `PolicySummary` shares with the full `Policy` body, each of which must read
/// the same stored value through either reader.
///
/// SOURCE: `smithy_query(query="shape", shape="PolicySummary")` — every member it declares.
const SUMMARY_MEMBERS: [&str; 11] = [
    "policyArn",
    "name",
    "availabilitySlo",
    "multiAz",
    "multiRegion",
    "dataRecovery",
    "sharingEnabled",
    "organizationId",
    "associatedServiceCount",
    "createdAt",
    "updatedAt",
];

/// The `Policy` members `PolicySummary` does NOT declare, so a summary must not report
/// them.
///
/// SOURCE: `smithy_query(query="shape", shape="PolicySummary")` compared with
/// `shape="Policy"`.
const MEMBERS_ABSENT_FROM_A_SUMMARY: [&str; 3] = ["description", "kmsKeyId", "tags"];

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
        LIST_POLICIES.to_string()
    } else {
        format!("{LIST_POLICIES}?{query}")
    };
    call(router, "GET", &uri, None).await
}

async fn get_policy(router: &MultiServiceRouter, policy_arn: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("{GET_POLICY}?policyArn={policy_arn}"),
        None,
    )
    .await
}

/// Create a policy from `body` and answer its minted ARN.
async fn create_policy(router: &MultiServiceRouter, body: Value) -> String {
    let (status, answered) = call(router, "POST", CREATE_POLICY, Some(body.clone())).await;
    assert_eq!(
        status, 200,
        "creating {body:?}: expected HTTP 200, got {status} {answered:?}"
    );
    answered["policy"]["policyArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no policy.policyArn in {answered:?}"))
        .to_string()
}

/// Create the simplest policy under `name` and answer its ARN.
async fn create_named(router: &MultiServiceRouter, name: &str) -> String {
    create_policy(router, json!({"name": name})).await
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

/// The `policySummaries` of a successful listing.
fn summaries(case: &str, status: u16, body: &Value) -> Vec<Value> {
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200, got {status} {body:?}"
    );
    body["policySummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: the @required policySummaries is absent from {body:?}"))
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

// ---------------------------------------------------------------------------
// What a listing reports
// ---------------------------------------------------------------------------

/// RULE: an empty store lists nothing, and says so with an empty list and no token.
#[tokio::test]
async fn an_empty_store_lists_no_policies() {
    let router = emulator();
    let (status, body) = list(&router, "").await;
    assert_eq!(status, 200, "expected HTTP 200, got {status} {body:?}");
    assert_eq!(
        body["policySummaries"],
        json!([]),
        "an empty store must answer an empty list, got {body:?}"
    );
    assert!(
        body["nextToken"].is_null(),
        "an empty page has no nextToken, got {body:?}"
    );
}

/// RULE: every `PolicySummary` is projected from the stored record `GetPolicy` answers
/// from, so each member the summary shares with the full `Policy` body reads the same
/// value, and a member the summary shape does not declare is not reported.
///
/// The rows differ along which optional target blocks the stored policy carries and
/// whether cross-account sharing is on.
#[tokio::test]
async fn every_summary_is_the_stored_policy_projected() {
    let router = emulator();
    let rows = vec![
        ("only the required name", json!({"name": "Ab"})),
        (
            "availability SLO only",
            json!({"name": "slo-only", "availabilitySlo": {"target": 99.9}}),
        ),
        (
            "data recovery only",
            json!({"name": "backups_only", "dataRecovery": {"timeBetweenBackupsInMinutes": 15}}),
        ),
        (
            "multi-AZ and multi-Region targets, sharing on, tagged and described",
            json!({
                "name": "Tier0-Payments",
                "description": "Targets for the tier-0 payments path",
                "multiAz": {
                    "rtoInMinutes": 15,
                    "rpoInMinutes": 5,
                    "disasterRecoveryApproach": "HOT_STANDBY",
                },
                "multiRegion": {
                    "rtoInMinutes": 240,
                    "rpoInMinutes": 60,
                    "disasterRecoveryApproach": "ACTIVE_ACTIVE",
                },
                "sharingEnabled": true,
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
        arns.push((case, create_policy(&router, body.clone()).await));
    }

    let (status, body) = list(&router, "").await;
    let page = summaries("every stored policy", status, &body);
    assert_eq!(
        page.len(),
        rows.len(),
        "case every stored policy: expected {} summaries, got {page:?}",
        rows.len()
    );

    for (case, arn) in &arns {
        let summary = page
            .iter()
            .find(|summary| summary["policyArn"].as_str() == Some(arn.as_str()))
            .unwrap_or_else(|| panic!("case {case:?}: {arn} is absent from {page:?}"));
        let (get_status, read) = get_policy(&router, arn).await;
        assert_eq!(
            get_status, 200,
            "case {case:?}: reading {arn} back got {get_status} {read:?}"
        );
        let policy = &read["policy"];
        for member in SUMMARY_MEMBERS {
            assert_eq!(
                summary.get(member),
                policy.get(member),
                "case {case:?}: summary member {member} must equal the stored policy's, \
                 got {:?} against {:?}",
                summary.get(member),
                policy.get(member)
            );
        }
        for member in MEMBERS_ABSENT_FROM_A_SUMMARY {
            assert!(
                summary.get(member).is_none(),
                "case {case:?}: {member} is not a PolicySummary member, got {summary:?}"
            );
        }
    }
}

/// RULE: `associatedServiceCount` is counted from the services that reference the policy,
/// so a listing reports the count in force when it ran, not one frozen at create time.
///
/// The rows are the counts zero, one and two services.
#[tokio::test]
async fn the_summary_counts_the_services_that_reference_the_policy() {
    let router = emulator();
    let arn = create_named(&router, "counted-policy").await;

    for expected in [0usize, 1, 2] {
        if expected > 0 {
            let body = json!({
                "name": format!("counted-svc-{expected}"),
                "regions": [REGION],
                "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
                "policyArn": arn,
            });
            let (status, created) = call(&router, "POST", CREATE_SERVICE, Some(body)).await;
            assert_eq!(
                status, 200,
                "case {expected} services: creating a referencing service got {status} \
                 {created:?}"
            );
        }
        let (status, body) = list(&router, "").await;
        let page = summaries(&format!("{expected} services"), status, &body);
        let counted = page[0]["associatedServiceCount"].as_i64();
        assert_eq!(
            counted,
            Some(expected as i64),
            "case {expected} services: expected associatedServiceCount {expected}, got \
             {counted:?} in {page:?}"
        );
    }
}

/// RULE: a listing REPORTS — it changes nothing a later caller sees, so repeating it, and
/// reading the policy back after it, answers exactly what it did before.
#[tokio::test]
async fn listing_changes_nothing_a_later_caller_sees() {
    let router = emulator();
    let arn = create_named(&router, "untouched-by-reads").await;
    let (_, before) = get_policy(&router, &arn).await;

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

    let (status, after) = get_policy(&router, &arn).await;
    assert_eq!(status, 200, "reading {arn} back got {status} {after:?}");
    assert_eq!(
        after, before,
        "listing must not change the stored policy: {before:?} became {after:?}"
    );
}

// ---------------------------------------------------------------------------
// Pagination
// ---------------------------------------------------------------------------

/// RULE: `ListPolicies` is `@paginated` — successive pages of `maxResults` summaries,
/// resumed by the previous page's `nextToken`, enumerate every stored policy exactly once
/// in creation order, and only the last page omits the token.
///
/// The rows exhaust every page size from 1 to one past the number of stored policies, plus
/// the published `maxResults` maximum.
#[tokio::test]
async fn pagination_walks_every_policy_exactly_once() {
    let router = emulator();
    let created: Vec<String> = ["pol-1", "pol-2", "pol-3", "pol-4", "pol-5"]
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
                "case {case:?}: page {pages} must be full while policies remain, got \
                 {names:?}"
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
                        "case {case:?}: a token was issued after the last policy, got \
                         {body:?}"
                    );
                    token = Some(cursor.to_string());
                }
                None => break,
            }
        }
        assert_eq!(
            seen, created,
            "case {case:?}: the pages must enumerate every policy in creation order"
        );
    }
}

/// RULE: `maxResults` is modelled `range 1..100`, so each published bound is accepted and a
/// value one past either bound is refused as a 400 `ValidationException`.
#[tokio::test]
async fn max_results_is_held_to_its_published_range() {
    let router = emulator();
    create_named(&router, "bounded-policy").await;

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
        "the first page must be the first two policies, got {first_page:?}"
    );
    let token = first_page["nextToken"]
        .as_str()
        .unwrap_or_else(|| panic!("no nextToken in {first_page:?}"))
        .to_string();

    let (status, deleted) = call(
        &router,
        "POST",
        DELETE_POLICY,
        Some(json!({"policyArn": arns[1]})),
    )
    .await;
    assert_eq!(
        status, 200,
        "deleting the policy the first page ended on got {status} {deleted:?}"
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
// The accountId filter
// ---------------------------------------------------------------------------

/// RULE: `accountId` selects the policies OWNED by that account — the account segment of
/// the policy's ARN — so the emulator's own account lists them all, another well-formed
/// account lists none, and an absent filter constrains nothing.
///
/// The rows are: no filter, the owning account, and an account no stored policy carries.
#[tokio::test]
async fn the_account_filter_selects_by_the_owning_account() {
    let router = emulator();
    let names = ["acct-1", "acct-2", "acct-3"];
    for name in names {
        let arn = create_named(&router, name).await;
        assert!(
            arn.contains(&format!(":{ACCOUNT}:")),
            "a minted ARN carries the emulator account, got {arn:?}"
        );
    }

    for (case, query, expected) in [
        ("no filter", String::new(), names.len()),
        (
            "the owning account",
            format!("accountId={ACCOUNT}"),
            names.len(),
        ),
        (
            "an account owning no stored policy",
            format!("accountId={OTHER_ACCOUNT}"),
            0,
        ),
    ] {
        let (status, body) = list(&router, &query).await;
        let page = summaries(case, status, &body);
        assert_eq!(
            page.len(),
            expected,
            "case {case:?}: expected {expected} summaries, got {page:?}"
        );
        assert!(
            body["nextToken"].is_null(),
            "case {case:?}: every match fits one default-sized page, so no nextToken is \
             due, got {body:?}"
        );
    }
}

/// RULE: a listing reports what is stored NOW, so a deleted policy is gone from the next
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
        "both policies must be listed, got {body:?}"
    );

    let (status, deleted) = call(
        &router,
        "POST",
        DELETE_POLICY,
        Some(json!({"policyArn": first})),
    )
    .await;
    assert_eq!(status, 200, "deleting {first} got {status} {deleted:?}");

    let (status, body) = list(&router, "").await;
    assert_eq!(
        summary_names("after the delete", status, &body),
        vec!["churn-2"],
        "a deleted policy must be gone from the next page, got {body:?}"
    );

    create_named(&router, "churn-3").await;
    let (status, body) = list(&router, "").await;
    assert_eq!(
        summary_names("after a later create", status, &body),
        vec!["churn-2", "churn-3"],
        "a newly created policy must appear on the next page, got {body:?}"
    );
}
