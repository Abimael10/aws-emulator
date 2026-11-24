// SPDX-License-Identifier: CC0-1.0

//! Which `nextToken` `ListBusinessSupportSubscriptionHistory` takes back, end to
//! end over the awsJson1_0 wire.
//!
//! Every assertion here is about ONE rule: a `nextToken` is a position among the
//! contracts ONE request selected, so this read takes it back only from a
//! request that selects the same contracts. The members that select them — the
//! period a contract has to overlap (the `billingMonth`, with each end of it
//! replaced by `startDate`/`endDate`) and the account the history is narrowed to
//! — travel in the token and are compared with the resuming request; a token
//! minted under another selection is refused with the modeled
//! `ValidationException` instead of being applied as a cut point of this walk,
//! which would drop every contract of it sorting at or below that cut point —
//! contracts the walk the token came from never reported. `maxResults` is no
//! part of a selection: a page size is how much of one walk a caller takes, so
//! it may change between pages.
//!
//! The rows span what the rule generalises over: which period the resuming
//! request asks about (another month, a month with no contracts at all, a wider
//! window, the walked month spelled by its own two bounds, no month at all), the
//! account filter (none, the account the first page reported, the account still
//! to come) and the page size. The second test is the invariant behind the rule:
//! for every ordered pair of periods, a token of one period is refused by the
//! other and that other period still reports every contract it selects exactly
//! once when it is paged with its OWN tokens.

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state::{caller_account_id, SUPPORT_PLAN_BUSINESS};
use smithy_server_runtime::MultiServiceRouter;

// The awsJson1_0 harness — a router over ONE shared engine, the one call this
// suite makes, the bill writer and the response reader — already exists for this
// operation's derivation suite, so this module reuses it instead of building a
// second one.
use crate::business_support_subscription_history::{
    emulator, error_code, history, instant, persist, reported, FEBRUARY, JANUARY, MARCH,
    MEMBER_ACCOUNT_ID, OTHER_MEMBER_ACCOUNT_ID,
};

/// The `message` this read refuses a token it cannot read back at all with,
/// which a token minted for ANOTHER selection must not be answered with: that
/// token IS one this service minted, and what makes it unusable is the sequence
/// it is a cut point of.
const UNUSABLE_TOKEN_REFUSAL: &str = "nextToken is not a token this service issued";

/// The `ValidationExceptionReason` a refusal about a member carries.
const REASON_FIELD: &str = "fieldValidationFailed";

/// The world every test here reads over: three accounts of the caller's
/// consolidated billing family, billed so that two periods select DIFFERENT
/// contracts whose keys interleave.
///
/// The caller's own account is billed in January only, one member in March only,
/// and the third in January and March with February missing — so that account
/// has TWO contracts (a run of months ends at a month with no bill). The keys a
/// page resumes after are account first, so the March walk's first cut point
/// sorts ABOVE the caller's January contract: a token of the March walk honoured
/// in January would drop a contract the March walk never reported.
fn seed(engine: &std::sync::Arc<engine::InMemoryEngine>) {
    persist(
        engine,
        &[
            (caller_account_id(), JANUARY, SUPPORT_PLAN_BUSINESS),
            (MEMBER_ACCOUNT_ID, MARCH, SUPPORT_PLAN_BUSINESS),
            (OTHER_MEMBER_ACCOUNT_ID, JANUARY, SUPPORT_PLAN_BUSINESS),
            (OTHER_MEMBER_ACCOUNT_ID, MARCH, SUPPORT_PLAN_BUSINESS),
        ],
    );
}

/// The accounts a response reports a contract for, in the order it reports them.
fn accounts(body: &Value) -> Vec<String> {
    reported(body)
        .into_iter()
        .map(|(account, _, _, _)| account)
        .collect()
}

/// The contracts `request` reports in one page, asserting the call succeeded.
async fn walk(router: &MultiServiceRouter, case: &str, request: &Value) -> Vec<String> {
    let (status, body) = history(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for {request}, got {status} {body}"
    );
    accounts(&body)
}

/// The token a first page of one contract of `request` carries.
async fn first_token(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let mut paged = request.clone();
    paged["maxResults"] = json!(1);
    let (status, body) = history(router, &paged).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for a first page of one contract, got {status} {body}"
    );
    body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a first page of one of several contracts must carry a nextToken, got {body}")
        })
        .to_string()
}

/// Assert that `body` is the refusal this read answers a token minted under
/// another selection with: 400 `ValidationException` naming `nextToken` and the
/// member that put the caller on another walk, in the wording this operation's
/// sibling reads of a billing period refuse such a token with.
fn assert_other_selection_refusal(case: &str, status: u16, body: &Value, names: &str) {
    let constraint = format!("must have been issued for the {names} the request names");
    assert_eq!(
        (status, error_code(body).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {status} {body}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(format!("InvalidArgument: nextToken (DetailedCode=nextToken {constraint})").as_str()),
        "case {case:?}: expected the refusal naming the {names} the token was issued for, got {body}"
    );
    assert_ne!(
        body["message"].as_str(),
        Some(UNUSABLE_TOKEN_REFUSAL),
        "case {case:?}: a token this service DID mint must not be refused as one it did not, \
         got {body}"
    );
    assert_eq!(
        body["reason"].as_str(),
        Some(REASON_FIELD),
        "case {case:?}: expected the reason for a member the request got wrong, got {body}"
    );
    assert_eq!(
        (
            body["fieldList"][0]["name"].as_str(),
            body["fieldList"][0]["message"].as_str(),
        ),
        (Some("nextToken"), Some(format!("Member {constraint}").as_str())),
        "case {case:?}: expected the fieldList to name nextToken and the constraint, got {body}"
    );
}

/// What this read owes a request that offers it a `nextToken` of the March walk.
enum Owed {
    /// The request selects the same contracts, so the walk resumes and reports
    /// exactly the contracts of it that follow the cursor.
    Resumes(&'static str),
    /// The request selects other contracts, so the token is refused as one
    /// issued for another `&'static str` — the member that names them.
    RefusedFor(&'static str),
}

#[tokio::test]
async fn a_token_resumes_only_a_request_that_selects_the_same_contracts() {
    let (router, engine) = emulator();
    seed(&engine);

    // The walked sequence: the contracts overlapping March, ascending by account
    // and then by contract start.
    let march = json!({ "billingMonth": MARCH });
    let walked = walk(&router, "the March walk", &march).await;
    assert_eq!(
        walked,
        vec![MEMBER_ACCOUNT_ID.to_string(), OTHER_MEMBER_ACCOUNT_ID.to_string()],
        "setup: expected March to report the two accounts billed in it, got {walked:?}"
    );
    let token = first_token(&router, "the March walk", &march).await;

    let rows: Vec<(&str, Value, Owed)> = vec![
        (
            "the walked period again",
            march.clone(),
            Owed::Resumes(OTHER_MEMBER_ACCOUNT_ID),
        ),
        (
            "the walked period with a larger page size",
            json!({ "billingMonth": MARCH, "maxResults": 100 }),
            Owed::Resumes(OTHER_MEMBER_ACCOUNT_ID),
        ),
        (
            "the walked period spelled by its own two bounds",
            json!({
                "billingMonth": MARCH,
                "startDate": instant("2024-03-01T00:00:00Z"),
                "endDate": instant("2024-04-01T00:00:00Z"),
            }),
            Owed::Resumes(OTHER_MEMBER_ACCOUNT_ID),
        ),
        (
            "an earlier month, whose contracts sort around the cut point",
            json!({ "billingMonth": JANUARY }),
            Owed::RefusedFor("billing period"),
        ),
        (
            "a month that selects no contract at all",
            json!({ "billingMonth": FEBRUARY }),
            Owed::RefusedFor("billing period"),
        ),
        (
            "a window widened at its start",
            json!({ "billingMonth": MARCH, "startDate": instant("2024-01-01T00:00:00Z") }),
            Owed::RefusedFor("billing period"),
        ),
        (
            "a window narrowed at its end",
            json!({ "billingMonth": MARCH, "endDate": instant("2024-03-15T00:00:00Z") }),
            Owed::RefusedFor("billing period"),
        ),
        (
            "no month at all, which asks about the month the clock is in",
            json!({}),
            Owed::RefusedFor("billing period"),
        ),
        (
            "the walked period narrowed to the account the first page reported",
            json!({ "billingMonth": MARCH, "accountId": MEMBER_ACCOUNT_ID }),
            Owed::RefusedFor("account filter"),
        ),
        (
            "the walked period narrowed to the account still to come",
            json!({ "billingMonth": MARCH, "accountId": OTHER_MEMBER_ACCOUNT_ID }),
            Owed::RefusedFor("account filter"),
        ),
    ];

    for (case, mut request, owed) in rows {
        request["nextToken"] = json!(token.clone());
        let (status, body) = history(&router, &request).await;
        match owed {
            Owed::Resumes(expected) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} {body}"
                );
                assert_eq!(
                    accounts(&body),
                    vec![expected.to_string()],
                    "case {case:?}: expected the contracts after the cursor, got {body}"
                );
            }
            Owed::RefusedFor(names) => {
                assert_other_selection_refusal(case, status, &body, names);
            }
        }
    }

    // What the refusals kept: January still reports BOTH its contracts,
    // including the caller's — the one a cut point of the March walk sorts above
    // and would have dropped — and the March token still resumes its own walk.
    let january = walk(&router, "January after the refusals", &json!({ "billingMonth": JANUARY })).await;
    assert_eq!(
        january,
        vec![caller_account_id().to_string(), OTHER_MEMBER_ACCOUNT_ID.to_string()],
        "case \"January after the refusals\": expected both January contracts, got {january:?}"
    );
    let mut resumed = march.clone();
    resumed["nextToken"] = json!(token);
    let after = walk(&router, "the March walk after the refusals", &resumed).await;
    assert_eq!(
        after,
        vec![OTHER_MEMBER_ACCOUNT_ID.to_string()],
        "case \"the March walk after the refusals\": expected the contracts after the cursor, \
         got {after:?}"
    );
}

/// Walk `request` to the end one contract a page, following only the tokens the
/// read itself issues, and report every contract it reported in order.
///
/// The loop is bounded by the number of contracts the store can hold for the
/// seeded family, so a walk that fails to advance fails the test instead of
/// running forever.
async fn paged_walk(router: &MultiServiceRouter, case: &str, request: &Value) -> Vec<String> {
    let mut reported_accounts: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..8 {
        let mut page = request.clone();
        page["maxResults"] = json!(1);
        if let Some(token) = &token {
            page["nextToken"] = json!(token);
        }
        let (status, body) = history(router, &page).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for a page of {request}, got {status} {body}"
        );
        reported_accounts.extend(accounts(&body));
        token = body["nextToken"].as_str().map(ToString::to_string);
        if token.is_none() {
            return reported_accounts;
        }
    }
    panic!("case {case:?}: the paged walk of {request} must end, got {reported_accounts:?}")
}

#[tokio::test]
async fn a_period_still_reports_every_contract_it_selects_after_another_period_s_token() {
    let (router, engine) = emulator();
    seed(&engine);

    // Each period the seeded bills give a walk of its own: the two months that
    // hold contracts, the month that holds none, and the window a `startDate`
    // widens to every one of them.
    let periods: Vec<(&str, Value)> = vec![
        ("January", json!({ "billingMonth": JANUARY })),
        ("February", json!({ "billingMonth": FEBRUARY })),
        ("March", json!({ "billingMonth": MARCH })),
        (
            "January through March",
            json!({ "billingMonth": MARCH, "startDate": instant("2024-01-01T00:00:00Z") }),
        ),
    ];

    for (minted_in, minting) in &periods {
        // Only a period whose walk holds more than one contract issues a token.
        if walk(&router, minted_in, minting).await.len() < 2 {
            continue;
        }
        let token = first_token(&router, minted_in, minting).await;

        for (replayed_in, replaying) in &periods {
            if replayed_in == minted_in {
                continue;
            }
            let case = format!("{replayed_in} resuming a {minted_in} token");
            let mut request = replaying.clone();
            request["nextToken"] = json!(token.clone());
            let (status, body) = history(&router, &request).await;
            assert_other_selection_refusal(&case, status, &body, "billing period");

            // The invariant the refusal protects: paging the other period with
            // its OWN tokens reports every contract it selects, exactly once
            // and in order — nothing the foreign cut point sorted above is
            // missing.
            let whole = walk(&router, &case, replaying).await;
            let paged = paged_walk(&router, &case, replaying).await;
            assert_eq!(
                paged, whole,
                "case {case:?}: expected the paged walk to report every contract of \
                 {replaying} exactly once, got {paged:?} against {whole:?}"
            );
        }
    }
}
