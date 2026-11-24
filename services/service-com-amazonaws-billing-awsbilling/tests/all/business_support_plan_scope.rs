// SPDX-License-Identifier: CC0-1.0

//! Which bills the Business Support charge report covers, end to end over the
//! awsJson1_0 wire.
//!
//! Every assertion here is about ONE rule: a Support charge report covers only
//! the account-months whose bill records a Support plan the report's own
//! `supportPlanName` member can name, and its month-wide
//! `totalSupportCharge`, `totalSupportEligibleSpend` and `accountCount` are
//! summed over exactly those bills. A bill recording any other plan — the
//! Enterprise Support plan an Enterprise Support profile's accounts are billed
//! under, or any plan outside the documented value list — is that plan's charge
//! to report: it is no entry of this report, and neither its spend nor its
//! head-count reaches this report's totals.
//!
//! The emulator runs no billing pipeline, so nothing accrues spend and the
//! store is empty on a fresh emulator: like the rest of this crate's charge
//! suites, the tables below put the records in state directly (the same shared
//! engine the router hands every handler) and then assert what the API reports
//! for them. The rows differ along what the rule generalises over: the plan a
//! bill records (each documented plan, the Enterprise plan, a plan outside the
//! list and a name differing from a documented one only in case), which
//! accounts the covered and uncovered bills belong to, the `accountId` filter,
//! the page size and the page boundary.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state::{
    caller_account_id, SupportEligibleSpend, SupportServiceSpend, SPEND_ITEM_TYPE_USAGE,
    SUPPORT_PLAN_BASIC, SUPPORT_PLAN_BUSINESS, SUPPORT_PLAN_DEVELOPER, SUPPORT_PLAN_ENTERPRISE,
    SUPPORT_PLAN_NAMES,
};
use service_com_amazonaws_billing_awsbilling::{
    ComAmazonawsBillingAwsbillingState, EmulatedService,
};
use smithy_server_runtime::MultiServiceRouter;

/// Further 12-digit accounts of the caller's consolidated billing family, so a
/// month can hold bills on several plans at once. They interleave with the
/// caller's own account in ascending order, so an uncovered bill sits between
/// two covered ones and the walk has to step over it.
const SECOND_ACCOUNT_ID: &str = "111111111111";
const THIRD_ACCOUNT_ID: &str = "222222222222";
const FOURTH_ACCOUNT_ID: &str = "333333333333";
const FIFTH_ACCOUNT_ID: &str = "444444444444";

/// A billing month far enough in the past that the clock cannot move it.
const PAST_MONTH: &str = "2024-03";

/// One minor unit of currency (a cent), the scale the stored amounts use.
const CENTS: i64 = 100;

/// A Support plan name outside the documented value list: AWS bills accounts
/// under Support plans this operation's `supportPlanName` does not name, so the
/// rule cannot be about the Enterprise plan alone.
const PLAN_OUTSIDE_THE_LIST: &str = "AWSSupportEnterpriseOnRamp";

/// A name differing from a documented value only in case — not one of the
/// documented values, which are the exact strings
/// `BusinessSupportAccountCharge$supportPlanName` lists.
const PLAN_DIFFERING_IN_CASE: &str = "awssupportbusiness";

/// One router over ONE shared engine — what `core/router` builds in production,
/// so every call in a test observes the same state.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// Send one awsJson1_0 call and return its HTTP status and decoded body.
async fn call(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AWSBilling.{operation}"))
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/billing/aws4_request, \
             SignedHeaders=host, Signature=0",
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// A whole number of currency units, in the minor units the store holds.
fn units(whole: i64) -> i64 {
    whole * CENTS
}

/// One account-month of Support-eligible spend to persist: the account, the
/// plan its bill records and its usage spend in whole currency units.
struct Seed {
    account_id: &'static str,
    support_plan_name: &'static str,
    usage: i64,
}

impl Seed {
    fn record(&self) -> SupportEligibleSpend {
        SupportEligibleSpend {
            account_id: self.account_id.to_string(),
            billing_month: PAST_MONTH.to_string(),
            support_plan_name: self.support_plan_name.to_string(),
            currency_code: "USD".to_string(),
            by_service: vec![SupportServiceSpend {
                contributing_service: "Amazon Elastic Compute Cloud".to_string(),
                item_type: SPEND_ITEM_TYPE_USAGE.to_string(),
                description: None,
                charge_amount_minor: units(self.usage),
            }],
            discount: None,
            reserved_instance_spend_minor: 0,
            savings_plan_spend_minor: 0,
            is_estimated_bill: false,
        }
    }
}

/// Persist every seeded record and put every account it names in the
/// consolidated billing family, so the payer account's answer covers their
/// bills.
fn persist(engine: &Arc<engine::InMemoryEngine>, seeds: &[Seed]) {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let mut state = state.lock();
    for seed in seeds {
        state
            .consolidated_billing_family
            .insert(seed.account_id.to_string(), "Linked Account".to_string());
        assert!(
            state.record_support_eligible_spend(seed.record()),
            "setup: the seeded spend of {:?} must fit in the store",
            (seed.account_id, seed.support_plan_name)
        );
    }
}

/// How many spend records are resident, so a read can be shown not to write.
fn resident(engine: &Arc<engine::InMemoryEngine>) -> usize {
    let state = engine.state::<ComAmazonawsBillingAwsbillingState>();
    let state = state.lock();
    state.support_eligible_spend.len()
}

/// Ask for one billing month's Business Support charges, with the extra request
/// members `extra` carries merged in.
async fn charges(router: &MultiServiceRouter, extra: Value) -> (u16, Value) {
    let mut body = json!({ "billingMonth": PAST_MONTH });
    if let Some(members) = extra.as_object() {
        for (name, value) in members {
            body[name] = value.clone();
        }
    }
    call(router, "ListBusinessSupportAccountCharges", &body).await
}

/// The accounts a response reports, in the order it reports them.
fn reported_accounts(body: &Value) -> Vec<String> {
    body["accountCharges"]
        .as_array()
        .map(|charges| {
            charges
                .iter()
                .map(|charge| charge["accountId"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The Support plans a response reports, one per entry.
fn reported_plans(body: &Value) -> Vec<String> {
    body["accountCharges"]
        .as_array()
        .map(|charges| {
            charges
                .iter()
                .map(|charge| {
                    charge["supportPlanName"]
                        .as_str()
                        .unwrap_or_default()
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Assert the month-wide totals of `body`, which the model documents for all
/// accounts in the billing month and which are therefore summed over exactly
/// the bills this report covers.
fn assert_totals(case: &str, body: &Value, charge: &str, spend: &str, account_count: i64) {
    assert_eq!(
        body["totalSupportCharge"].as_str(),
        Some(charge),
        "case {case:?}: expected totalSupportCharge {charge}, got {body}"
    );
    assert_eq!(
        body["totalSupportEligibleSpend"].as_str(),
        Some(spend),
        "case {case:?}: expected totalSupportEligibleSpend {spend}, got {body}"
    );
    assert_eq!(
        body["accountCount"].as_i64(),
        Some(account_count),
        "case {case:?}: expected accountCount {account_count}, got {body}"
    );
}

/// The month every test below seeds: a bill on each documented plan, and
/// between them bills on plans this report cannot name.
///
/// Business Support charges 10% of 2,000 = 200; Developer Support 3% of 1,000 =
/// 30, over its 29 minimum; Basic Support nothing. So the report's own totals
/// are 230.00 of charge over 8,000.00 of spend across three accounts, whatever
/// the uncovered bills beside them hold.
fn mixed_month() -> Vec<Seed> {
    vec![
        Seed {
            account_id: caller_account_id(),
            support_plan_name: SUPPORT_PLAN_BUSINESS,
            usage: 2_000,
        },
        Seed {
            account_id: SECOND_ACCOUNT_ID,
            support_plan_name: SUPPORT_PLAN_ENTERPRISE,
            usage: 70_000,
        },
        Seed {
            account_id: THIRD_ACCOUNT_ID,
            support_plan_name: SUPPORT_PLAN_DEVELOPER,
            usage: 1_000,
        },
        Seed {
            account_id: FOURTH_ACCOUNT_ID,
            support_plan_name: PLAN_OUTSIDE_THE_LIST,
            usage: 9_000,
        },
        Seed {
            account_id: FIFTH_ACCOUNT_ID,
            support_plan_name: SUPPORT_PLAN_BASIC,
            usage: 5_000,
        },
    ]
}

/// The accounts of [`mixed_month`] whose bill records a plan this report can
/// name, in the ascending account order the report walks.
fn covered_accounts() -> Vec<String> {
    vec![
        caller_account_id().to_string(),
        THIRD_ACCOUNT_ID.to_string(),
        FIFTH_ACCOUNT_ID.to_string(),
    ]
}

#[tokio::test]
async fn reports_only_the_bills_recording_a_plan_the_report_can_name() {
    let (router, engine) = emulator();
    let seeds = mixed_month();
    persist(&engine, &seeds);

    let (status, body) = charges(&router, json!({})).await;
    assert_eq!(
        status, 200,
        "case \"mixed month\": expected HTTP 200, got {status} {body}"
    );
    assert_eq!(
        reported_accounts(&body),
        covered_accounts(),
        "case \"mixed month\": expected only the accounts billed under a plan the report \
         can name, got {body}"
    );
    for plan in reported_plans(&body) {
        assert!(
            SUPPORT_PLAN_NAMES.contains(&plan.as_str()),
            "case \"mixed month\": expected every reported supportPlanName to be one of \
             {SUPPORT_PLAN_NAMES:?}, got {plan:?}"
        );
    }
    assert_totals("mixed month", &body, "230.00", "8000.00", 3);
    // A reporting operation: the uncovered bills are left where they are, not
    // dropped from the store.
    assert_eq!(
        resident(&engine),
        seeds.len(),
        "case \"mixed month\": the read must write nothing, got {} records",
        resident(&engine)
    );
}

#[tokio::test]
async fn leaves_a_bill_on_another_plan_out_of_the_entries_and_the_totals() {
    // Each row is a plan the report's `supportPlanName` cannot name, held by a
    // second account of the same family in the same month beside the caller's
    // Business Support bill. Whatever the row's plan and spend, the report is
    // the caller's bill alone.
    let rows: Vec<(&str, &str, i64)> = vec![
        ("the Enterprise Support plan", SUPPORT_PLAN_ENTERPRISE, 70_000),
        ("a plan outside the documented list", PLAN_OUTSIDE_THE_LIST, 9_000),
        (
            "a documented name differing only in case",
            PLAN_DIFFERING_IN_CASE,
            4_000,
        ),
    ];

    for (case, plan, usage) in rows {
        let (router, engine) = emulator();
        persist(
            &engine,
            &[
                Seed {
                    account_id: caller_account_id(),
                    support_plan_name: SUPPORT_PLAN_BUSINESS,
                    usage: 2_000,
                },
                Seed {
                    account_id: SECOND_ACCOUNT_ID,
                    support_plan_name: plan,
                    usage,
                },
            ],
        );

        let (status, body) = charges(&router, json!({})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} {body}"
        );
        assert_eq!(
            reported_accounts(&body),
            vec![caller_account_id().to_string()],
            "case {case:?}: expected only the account billed under a plan the report can \
             name, got {body}"
        );
        assert_totals(case, &body, "200.00", "2000.00", 1);

        // The `accountId` filter narrows the entries and not the month's
        // totals, so asking about the uncovered account reports no entry
        // beside the same month-wide totals.
        let (status, filtered) = charges(&router, json!({ "accountId": SECOND_ACCOUNT_ID })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for the filtered read, got {status} {filtered}"
        );
        assert!(
            reported_accounts(&filtered).is_empty(),
            "case {case:?}: expected no entry for the account billed under another plan, \
             got {filtered}"
        );
        assert_totals(case, &filtered, "200.00", "2000.00", 1);
    }
}

#[tokio::test]
async fn a_month_whose_every_bill_records_another_plan_reports_nothing() {
    let (router, engine) = emulator();
    persist(
        &engine,
        &[
            Seed {
                account_id: caller_account_id(),
                support_plan_name: SUPPORT_PLAN_ENTERPRISE,
                usage: 70_000,
            },
            Seed {
                account_id: SECOND_ACCOUNT_ID,
                support_plan_name: PLAN_OUTSIDE_THE_LIST,
                usage: 9_000,
            },
        ],
    );

    let (status, body) = charges(&router, json!({})).await;
    assert_eq!(
        status, 200,
        "case \"no covered bill\": expected HTTP 200, got {status} {body}"
    );
    assert!(
        reported_accounts(&body).is_empty(),
        "case \"no covered bill\": expected no entry at all, got {body}"
    );
    assert_totals("no covered bill", &body, "0.00", "0.00", 0);
    assert!(
        body.get("nextToken").is_none_or(Value::is_null),
        "case \"no covered bill\": expected no nextToken, got {body}"
    );

    // The spend of an account inside the Enterprise Support profile is the
    // Enterprise report's to report, and it still is.
    let (status, summary) = call(
        &router,
        "GetEnterpriseSupportChargeSummary",
        &json!({ "billingMonth": PAST_MONTH }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"no covered bill\": expected HTTP 200 for the Enterprise summary, \
         got {status} {summary}"
    );
    assert_eq!(
        summary["totalSupportEligibleSpend"].as_str(),
        Some("70000.00"),
        "case \"no covered bill\": expected the Enterprise profile's spend reported by its \
         own read, got {summary}"
    );
}

#[tokio::test]
async fn pages_over_only_the_bills_the_report_covers() {
    // Each row is a page size the covered bills are walked at: one entry per
    // page, two, and more than the walk holds.
    for page_size in [1_i64, 2, 10] {
        let case = format!("maxResults {page_size}");
        let (router, engine) = emulator();
        persist(&engine, &mixed_month());

        let mut walked: Vec<String> = Vec::new();
        let mut token: Option<String> = None;
        // One iteration per covered bill is enough to drain the walk; the
        // bound keeps a repeating cursor from looping forever.
        for _ in 0..=covered_accounts().len() {
            let extra = match token.as_deref() {
                Some(next) => json!({ "maxResults": page_size, "nextToken": next }),
                None => json!({ "maxResults": page_size }),
            };
            let (status, body) = charges(&router, extra).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} {body}"
            );
            let page = reported_accounts(&body);
            assert!(
                !page.is_empty(),
                "case {case:?}: a page of a non-empty walk must carry an entry, got {body}"
            );
            assert!(
                i64::try_from(page.len()).is_ok_and(|len| len <= page_size),
                "case {case:?}: expected at most {page_size} entries, got {body}"
            );
            // The totals are the month's, so they do not move with the page.
            assert_totals(&case, &body, "230.00", "8000.00", 3);
            walked.extend(page);
            token = body
                .get("nextToken")
                .and_then(Value::as_str)
                .map(ToString::to_string);
            if token.is_none() {
                break;
            }
        }

        assert!(
            token.is_none(),
            "case {case:?}: the walk must end, got a further token {token:?}"
        );
        assert_eq!(
            walked,
            covered_accounts(),
            "case {case:?}: expected the covered bills once each, in ascending account \
             order, got {walked:?}"
        );
    }
}
