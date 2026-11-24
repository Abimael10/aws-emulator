// SPDX-License-Identifier: CC0-1.0

//! Which `nextToken` `ListEnterpriseSupportLinkedAccountCharges` takes back, end
//! to end over the awsJson1_0 wire.
//!
//! Every assertion here is about ONE rule: a `nextToken` is a position among the
//! bills ONE request selected, so this read takes it back only from a request
//! that selects the same bills. The spend store is keyed billing month FIRST, so
//! the `billingMonth` a request names is what SELECTS the walk: it travels in
//! the token and is compared with the resuming request, and a token minted while
//! paging another month is refused with the modeled `ValidationException`
//! instead of being applied as a cut point of this month's walk — where it would
//! drop every bill of the month sorting at or below it (an earlier month: all of
//! them) or be passed by every key and restart the walk (a later month).
//! `maxResults` is no part of a selection, and neither is `accountId`: a page
//! size is how much of one walk a caller takes, and the account filter narrows
//! one and the same walk, so a cursor stays a cut point of it.
//!
//! The rows span what the rule generalises over: which month the resuming
//! request names (the walked month again, the month just before it, a month
//! further back, a later month, a month holding bills but none of this
//! profile's), the account filter and the page size. The second test is the
//! invariant behind the rule: for every ORDERED PAIR of months the profile is
//! billed in, a token of one month is refused by the other, and that other month
//! still reports every bill it selects exactly once when it is paged with its
//! OWN tokens.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state::{
    SupportEligibleSpend, SupportServiceSpend, SPEND_ITEM_TYPE_USAGE, SUPPORT_PLAN_BUSINESS,
    SUPPORT_PLAN_ENTERPRISE,
};
use service_com_amazonaws_billing_awsbilling::ComAmazonawsBillingAwsbillingState;
use smithy_server_runtime::MultiServiceRouter;

// The awsJson1_0 harness — a router over ONE shared engine, the call it makes
// and the error reader — already exists for the billing-view lifecycle suite, so
// this module reuses it instead of building a second one. The default state
// makes the caller the payer account of the billing family, which is the account
// this read answers for.
use crate::billing_views::{call, error_code, standalone_emulator_with_engine, ACCOUNT_ID};

/// Further 12-digit accounts of the caller's consolidated billing family, so
/// each month's walk holds more than one bill. They sort above the caller's own
/// account id, so an ascending walk of a month visits the caller first.
const MEMBER_ACCOUNT_ID: &str = "111111111111";
const OTHER_MEMBER_ACCOUNT_ID: &str = "222222222222";

/// The billing month whose walk mints the token every row below replays. Far
/// enough in the past that the clock cannot move it out of the months this read
/// answers for.
const WALKED_MONTH: &str = "2024-03";

/// The month immediately BEFORE the walked one — a 29-day February, so the pair
/// also differs in the length of its billing period — and a month further back
/// still. Their keys sort below every key of the walked month, so a cursor of
/// that month honoured here would drop their whole breakdown.
const ADJACENT_EARLIER_MONTH: &str = "2024-02";
const EARLIER_MONTH: &str = "2024-01";

/// A month AFTER the walked one, still in the past: its keys all sort above a
/// cursor of the walked month, so one honoured here would be ignored and restart
/// the walk at the month's first account.
const LATER_MONTH: &str = "2024-05";

/// A past month the family IS billed in, under the Business Support plan only,
/// so it holds bills but none this Enterprise Support report selects.
const MONTH_WITHOUT_ENTERPRISE_BILLS: &str = "2024-04";

/// The `message` this read refuses a token it cannot read back at all with,
/// which a token it DID mint for another month must not be answered with: what
/// makes that token unusable is the sequence it is a cut point of.
const UNUSABLE_TOKEN_REFUSAL: &str = "nextToken is not a token this service issued";

/// The `ValidationExceptionReason` a refusal about a request member carries
/// (`fieldValidationFailed`, `state::VALIDATION_REASON_FIELD`).
const REASON_FIELD: &str = "fieldValidationFailed";

/// One account-month of Support-eligible spend under the Support plan `plan`.
fn spend_in(billing_month: &str, account_id: &str, plan: &str) -> SupportEligibleSpend {
    SupportEligibleSpend {
        account_id: account_id.to_string(),
        billing_month: billing_month.to_string(),
        support_plan_name: plan.to_string(),
        currency_code: "USD".to_string(),
        by_service: vec![SupportServiceSpend {
            contributing_service: "AmazonEC2".to_string(),
            item_type: SPEND_ITEM_TYPE_USAGE.to_string(),
            description: None,
            charge_amount_minor: 100_000,
        }],
        discount: None,
        reserved_instance_spend_minor: 0,
        savings_plan_spend_minor: 0,
        is_estimated_bill: false,
    }
}

/// The accounts every billed month below holds a bill for, in the ascending
/// order the walk reports them in.
fn billed_accounts() -> Vec<String> {
    vec![
        ACCOUNT_ID.to_string(),
        MEMBER_ACCOUNT_ID.to_string(),
        OTHER_MEMBER_ACCOUNT_ID.to_string(),
    ]
}

/// The months this profile is billed in, each selecting a walk of its own.
fn enterprise_months() -> Vec<&'static str> {
    vec![
        EARLIER_MONTH,
        ADJACENT_EARLIER_MONTH,
        WALKED_MONTH,
        LATER_MONTH,
    ]
}

/// The world every test here reads over: the caller's consolidated billing
/// family, billed under the Enterprise Support plan in each of
/// [`enterprise_months`] and under the Business plan in
/// [`MONTH_WITHOUT_ENTERPRISE_BILLS`].
///
/// The emulator runs no billing pipeline, so the spend store is empty on a fresh
/// emulator and the Support suites seed it this way.
fn seed(engine: &Arc<engine::InMemoryEngine>) {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let mut state = state.lock();
    for account_id in billed_accounts() {
        state
            .consolidated_billing_family
            .insert(account_id, "Linked Account".to_string());
    }
    for billing_month in enterprise_months() {
        for account_id in billed_accounts() {
            assert!(
                state.record_support_eligible_spend(spend_in(
                    billing_month,
                    &account_id,
                    SUPPORT_PLAN_ENTERPRISE,
                )),
                "setup: the seeded {billing_month} spend of {account_id:?} must fit in the store"
            );
        }
    }
    for account_id in billed_accounts() {
        assert!(
            state.record_support_eligible_spend(spend_in(
                MONTH_WITHOUT_ENTERPRISE_BILLS,
                &account_id,
                SUPPORT_PLAN_BUSINESS,
            )),
            "setup: the seeded {MONTH_WITHOUT_ENTERPRISE_BILLS} spend of {account_id:?} must fit \
             in the store"
        );
    }
}

/// Ask for one page of linked-account charges.
async fn charges(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(
        router,
        "ListEnterpriseSupportLinkedAccountCharges",
        request,
    )
    .await
}

/// The accounts a response reports a charge for, in the order it reports them.
fn accounts(body: &Value) -> Vec<String> {
    body["linkedAccount"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry["accountId"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The accounts `request` reports in one page, asserting the call succeeded.
async fn walk(router: &MultiServiceRouter, case: &str, request: &Value) -> Vec<String> {
    let (status, body) = charges(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for {request}, got {status} {body}"
    );
    accounts(&body)
}

/// The token a first page of ONE charge of `billing_month` carries.
async fn first_token(router: &MultiServiceRouter, case: &str, billing_month: &str) -> String {
    let (status, body) = charges(
        router,
        &json!({ "billingMonth": billing_month, "maxResults": 1 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for a first page of one charge, got {status} {body}"
    );
    body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: a first page of one of several charges must carry a nextToken, \
                 got {body}"
            )
        })
        .to_string()
}

/// Page `billing_month` with its OWN tokens, `page_size` charges at a time, and
/// report every account it reported in order.
async fn whole_walk(
    router: &MultiServiceRouter,
    case: &str,
    billing_month: &str,
    page_size: usize,
) -> Vec<String> {
    let mut reported: Vec<String> = Vec::new();
    let mut next_token: Option<String> = None;
    // The seeded months hold three bills each, so a walk of pages of at least
    // one charge ends well inside this many pages; the bound is what keeps a
    // read that mints a token for an empty page from looping forever.
    for _ in 0..=billed_accounts().len() + 1 {
        let mut request = json!({ "billingMonth": billing_month, "maxResults": page_size });
        if let Some(token) = next_token.as_deref() {
            request["nextToken"] = json!(token);
        }
        let (status, body) = charges(router, &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for {request}, got {status} {body}"
        );
        reported.extend(accounts(&body));
        next_token = body["nextToken"].as_str().map(str::to_string);
        if next_token.is_none() {
            return reported;
        }
    }
    panic!("case {case:?}: paging {billing_month} with its own tokens never ended, got {reported:?}")
}

/// Assert that `body` is the refusal this read answers a token minted while
/// paging another billing month with: 400 `ValidationException` naming
/// `nextToken` and the member that put the caller on another walk, in the
/// wording this crate's sibling reads of a billing period refuse such a token
/// with.
fn assert_other_month_refusal(case: &str, status: u16, body: &Value) {
    let constraint = "must have been issued for the billing month the request names";
    assert_eq!(
        (status, error_code(body).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {status} {body}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(format!("InvalidArgument: nextToken (DetailedCode=nextToken {constraint})").as_str()),
        "case {case:?}: expected the refusal naming the billing month the token was issued for, \
         got {body}"
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
        (
            Some("nextToken"),
            Some(format!("Member {constraint}").as_str())
        ),
        "case {case:?}: expected the fieldList to name nextToken and the constraint, got {body}"
    );
}

/// What this read owes a request that offers it a `nextToken` of the
/// [`WALKED_MONTH`] walk.
enum Owed {
    /// The request selects the same bills, so the walk resumes and reports
    /// exactly the accounts of it that follow the cursor.
    Resumes(Vec<String>),
    /// The request selects other bills, so the token is refused as one issued
    /// for another billing month.
    RefusedForTheBillingMonth,
}

#[tokio::test]
async fn a_charge_token_resumes_only_a_request_that_selects_the_same_bills() {
    let (router, engine) = standalone_emulator_with_engine();
    seed(&engine);

    let walked = walk(
        &router,
        "the walked month",
        &json!({ "billingMonth": WALKED_MONTH }),
    )
    .await;
    assert_eq!(
        walked,
        billed_accounts(),
        "setup: expected {WALKED_MONTH} to report every billed account of the family, \
         got {walked:?}"
    );
    let token = first_token(&router, "the walked month", WALKED_MONTH).await;
    // The cursor is the first account of the walk, so what follows it is the
    // rest of the walked month's accounts.
    let after_the_cursor: Vec<String> = walked[1..].to_vec();

    let rows: Vec<(&str, Value, Owed)> = vec![
        (
            "the walked month again",
            json!({ "billingMonth": WALKED_MONTH }),
            Owed::Resumes(after_the_cursor.clone()),
        ),
        (
            "the walked month with a larger page size",
            json!({ "billingMonth": WALKED_MONTH, "maxResults": 100 }),
            Owed::Resumes(after_the_cursor.clone()),
        ),
        (
            "the walked month narrowed to one account still to come",
            json!({ "billingMonth": WALKED_MONTH, "accountId": OTHER_MEMBER_ACCOUNT_ID }),
            Owed::Resumes(vec![OTHER_MEMBER_ACCOUNT_ID.to_string()]),
        ),
        (
            "the walked month narrowed to the account the cursor is at",
            json!({ "billingMonth": WALKED_MONTH, "accountId": ACCOUNT_ID }),
            Owed::Resumes(Vec::new()),
        ),
        (
            "the month just before the walked one, whose keys all sort below the cursor",
            json!({ "billingMonth": ADJACENT_EARLIER_MONTH }),
            Owed::RefusedForTheBillingMonth,
        ),
        (
            "a month further back still",
            json!({ "billingMonth": EARLIER_MONTH }),
            Owed::RefusedForTheBillingMonth,
        ),
        (
            "a later month, whose keys all sort above the cursor",
            json!({ "billingMonth": LATER_MONTH }),
            Owed::RefusedForTheBillingMonth,
        ),
        (
            "a month billed under another Support plan, which selects no bill of this profile",
            json!({ "billingMonth": MONTH_WITHOUT_ENTERPRISE_BILLS }),
            Owed::RefusedForTheBillingMonth,
        ),
        (
            "a later month with the page size the token was minted under",
            json!({ "billingMonth": LATER_MONTH, "maxResults": 1 }),
            Owed::RefusedForTheBillingMonth,
        ),
    ];

    for (case, mut request, owed) in rows {
        request["nextToken"] = json!(token.clone());
        let (status, body) = charges(&router, &request).await;
        match owed {
            Owed::Resumes(expected) => {
                assert_eq!(
                    (status, accounts(&body)),
                    (200, expected.clone()),
                    "case {case:?}: expected the accounts of the walk following the cursor \
                     {expected:?}, got {status} {body}"
                );
                // Every resuming row above takes the whole tail of its walk in
                // one page, so the walk is over and there is nothing left to
                // cut: a page that reported the last entry — or none at all —
                // carries no cursor for a caller to come back with.
                assert!(
                    body["nextToken"].is_null(),
                    "case {case:?}: a page that reported the rest of the walk must carry no \
                     nextToken, got {body}"
                );
            }
            Owed::RefusedForTheBillingMonth => {
                assert_other_month_refusal(case, status, &body);
            }
        }
    }
}

#[tokio::test]
async fn every_month_reports_its_own_bills_whatever_month_a_token_came_from() {
    let (router, engine) = standalone_emulator_with_engine();
    seed(&engine);

    for minted_for in enterprise_months() {
        let token = first_token(&router, minted_for, minted_for).await;
        for replayed_with in enterprise_months() {
            if replayed_with == minted_for {
                continue;
            }
            let case = format!("a {minted_for} token replayed with {replayed_with}");
            let (status, body) = charges(
                &router,
                &json!({ "billingMonth": replayed_with, "nextToken": token.clone() }),
            )
            .await;
            assert_other_month_refusal(&case, status, &body);

            // The refusal is what keeps the replayed month's breakdown from
            // being dropped: paged with its OWN tokens, that month still
            // reports every bill it selects, once and in order.
            let its_own = whole_walk(&router, &case, replayed_with, 1).await;
            assert_eq!(
                its_own,
                billed_accounts(),
                "case {case:?}: expected {replayed_with} to still report every billed account \
                 of the family, got {its_own:?}"
            );
        }
    }
}
