// SPDX-License-Identifier: CC0-1.0

//! Which `nextToken` `ListBusinessSupportAccountCharges` takes back, end to end
//! over the awsJson1_0 wire.
//!
//! Every assertion here is about ONE rule: a `nextToken` is a position among the
//! bills ONE request selected, so this read takes it back only from a request
//! that selects the same bills. The members that select them — the
//! `billingMonth` whose bills are walked and the `accountId` the breakdown is
//! narrowed to — travel in the token and are compared with the resuming request;
//! a token minted under another selection is refused with the modeled
//! `ValidationException` instead of being applied as this walk's cut point,
//! which would silently drop every bill of it sorting at or below that cut point
//! — bills the walk the token came from never reported — and answer that
//! truncated page with a 200 whose `accountCount` and `totalSupportCharge` still
//! report the month's whole set. `maxResults` is no part of a selection: a page
//! size is how much of one walk a caller takes, so it may change between pages.
//!
//! The rows span what the rule generalises over: which month the resuming
//! request asks about (the walked one, an earlier one whose bills sort below the
//! cut point, a later one, a month that holds no bill at all) and which account
//! it narrows the breakdown to (none, the account the first page reported, an
//! account still to come, an account with no bill in the month), beside the page
//! size. The second test is the invariant behind the rule: for every ordered
//! pair of selections, a token of one is refused by the other, and that other
//! still reports every bill it selects exactly once when it is paged with its
//! OWN tokens.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_billing_awsbilling::state::{
    caller_account_id, SUPPORT_PLAN_BASIC, SUPPORT_PLAN_BUSINESS, SUPPORT_PLAN_DEVELOPER,
};
use smithy_server_runtime::MultiServiceRouter;

// The awsJson1_0 harness — a router over ONE shared engine, the bill writer, the
// one call this suite makes and the response reader — already exists for this
// operation's pricing suite, so this module reuses it instead of building a
// second one.
use crate::business_support_account_charges::{
    call, emulator, error_code, persist, reported_accounts, units, Seed, EARLIER_MONTH,
    MEMBER_ACCOUNT_ID, OTHER_MEMBER_ACCOUNT_ID, PAST_MONTH,
};

/// A past billing month the seeded family holds no bill in at all, EARLIER than
/// every seeded one: a request naming it selects nothing, and a cut point of a
/// seeded month's walk names no bill of it.
const EMPTY_EARLIER_MONTH: &str = "2024-01";

/// A past billing month the seeded family holds no bill in, LATER than every
/// seeded one: a cut point of a seeded month's walk sorts below every key such a
/// month could hold, so honouring it would silently restart the walk.
const EMPTY_LATER_MONTH: &str = "2024-04";

/// A 12-digit account of the consolidated billing family that no seeded bill
/// names, so narrowing a month to it selects nothing.
const UNBILLED_ACCOUNT_ID: &str = "444444444444";

/// What the two members that select this read's sequence are called in the
/// refusal of a token minted under another selection, as the handler words them.
const SCOPE_BILLING_MONTH: &str = "billing month";
const SCOPE_ACCOUNT_FILTER: &str = "account filter";

/// The `message` this read refuses a token it cannot read back at all with,
/// which a token minted for ANOTHER selection must not be answered with: that
/// token IS one this service minted, and what makes it unusable is the sequence
/// it is a cut point of.
const UNUSABLE_TOKEN_REFUSAL: &str = "nextToken is not a token this service issued";

/// The `ValidationExceptionReason` a refusal about a member carries.
const REASON_FIELD: &str = "fieldValidationFailed";

/// The world every test here reads over: two past months of the caller's
/// consolidated billing family, each holding more than one priced bill, so each
/// mints tokens of its own.
///
/// The spend store is keyed month FIRST, so the earlier month's keys all sort
/// below the later month's: a cut point of the later month's walk names no bill
/// of the earlier one (honouring it would drop the whole month), and a cut point
/// of the earlier month's walk sorts below every bill of the later one
/// (honouring it would restart that walk).
fn seed(engine: &Arc<engine::InMemoryEngine>) {
    persist(
        engine,
        &[
            Seed::one_service(
                caller_account_id(),
                PAST_MONTH,
                SUPPORT_PLAN_BUSINESS,
                units(20_000),
            ),
            Seed::one_service(
                MEMBER_ACCOUNT_ID,
                PAST_MONTH,
                SUPPORT_PLAN_DEVELOPER,
                units(1_000),
            ),
            Seed::one_service(
                OTHER_MEMBER_ACCOUNT_ID,
                PAST_MONTH,
                SUPPORT_PLAN_BASIC,
                units(500),
            ),
            Seed::one_service(
                caller_account_id(),
                EARLIER_MONTH,
                SUPPORT_PLAN_BUSINESS,
                units(5_000),
            ),
            Seed::one_service(
                OTHER_MEMBER_ACCOUNT_ID,
                EARLIER_MONTH,
                SUPPORT_PLAN_BUSINESS,
                units(1_000),
            ),
        ],
    );
}

/// How many bills the world above holds, the bound every paged walk is read
/// under.
const SEEDED_BILLS: usize = 5;

/// Send one `ListBusinessSupportAccountCharges` call.
async fn charges(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "ListBusinessSupportAccountCharges", request).await
}

/// The accounts `request` reports in one page, asserting the call succeeded.
async fn walk(router: &MultiServiceRouter, case: &str, request: &Value) -> Vec<String> {
    let (status, body) = charges(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for {request}, got {status} {body}"
    );
    reported_accounts(&body)
}

/// The token a first page of ONE bill of `request` carries.
async fn first_token(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let mut paged = request.clone();
    paged["maxResults"] = json!(1);
    let (status, body) = charges(router, &paged).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for a first page of one bill, got {status} {body}"
    );
    body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: a first page of one of several bills must carry a nextToken, \
                 got {body}"
            )
        })
        .to_string()
}

/// Assert that `body` is the refusal this read answers a token minted under
/// another selection with: 400 `ValidationException` naming `nextToken` and the
/// member that put the caller on another walk, in the wording this service's
/// other scoped paginated reads refuse such a token with.
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
        "case {case:?}: expected the refusal naming the {names} the token was issued for, \
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

/// What this read owes a request that offers it a `nextToken` of the walked
/// selection.
enum Owed {
    /// The request selects the same bills, so the walk resumes and reports
    /// exactly the bills of it that follow the cursor.
    Resumes,
    /// The request selects other bills, so the token is refused as one issued
    /// for another `&'static str` — the member that names them.
    RefusedFor(&'static str),
}

#[tokio::test]
async fn a_token_resumes_only_a_request_that_selects_the_same_bills() {
    let (router, engine) = emulator();
    seed(&engine);

    // The walked sequence: every priced bill of the later month, ascending by
    // account.
    let walked_request = json!({ "billingMonth": PAST_MONTH });
    let walked = walk(&router, "the walked month", &walked_request).await;
    assert_eq!(
        walked.len(),
        3,
        "setup: expected the later month to hold three priced bills, got {walked:?}"
    );
    let token = first_token(&router, "the walked month", &walked_request).await;
    let reported_first = walked[0].clone();
    let still_to_come = walked[1].clone();

    let rows: Vec<(&str, Value, Owed)> = vec![
        ("the walked selection again", walked_request.clone(), Owed::Resumes),
        (
            "the walked selection with a larger page size",
            json!({ "billingMonth": PAST_MONTH, "maxResults": 100 }),
            Owed::Resumes,
        ),
        (
            "an earlier month, whose bills all sort below the cut point",
            json!({ "billingMonth": EARLIER_MONTH }),
            Owed::RefusedFor(SCOPE_BILLING_MONTH),
        ),
        (
            "an earlier month that holds no bill at all",
            json!({ "billingMonth": EMPTY_EARLIER_MONTH }),
            Owed::RefusedFor(SCOPE_BILLING_MONTH),
        ),
        (
            "a later month, every key of which sorts above the cut point",
            json!({ "billingMonth": EMPTY_LATER_MONTH }),
            Owed::RefusedFor(SCOPE_BILLING_MONTH),
        ),
        (
            "the walked month narrowed to the account the first page reported",
            json!({ "billingMonth": PAST_MONTH, "accountId": reported_first }),
            Owed::RefusedFor(SCOPE_ACCOUNT_FILTER),
        ),
        (
            "the walked month narrowed to an account still to come",
            json!({ "billingMonth": PAST_MONTH, "accountId": still_to_come }),
            Owed::RefusedFor(SCOPE_ACCOUNT_FILTER),
        ),
        (
            "the walked month narrowed to an account with no bill in it",
            json!({ "billingMonth": PAST_MONTH, "accountId": UNBILLED_ACCOUNT_ID }),
            Owed::RefusedFor(SCOPE_ACCOUNT_FILTER),
        ),
    ];

    for (case, mut request, owed) in rows {
        request["nextToken"] = json!(token.clone());
        let (status, body) = charges(&router, &request).await;
        match owed {
            Owed::Resumes => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} {body}"
                );
                assert_eq!(
                    reported_accounts(&body),
                    walked[1..].to_vec(),
                    "case {case:?}: expected the bills after the cursor, got {body}"
                );
            }
            Owed::RefusedFor(names) => assert_other_selection_refusal(case, status, &body, names),
        }
    }

    // What the refusals kept. The earlier month still reports both its bills —
    // the ones a cut point of the later month's walk sorts above and would have
    // dropped.
    let earlier = walk(
        &router,
        "the earlier month after the refusals",
        &json!({ "billingMonth": EARLIER_MONTH }),
    )
    .await;
    assert_eq!(
        earlier.len(),
        2,
        "case \"the earlier month after the refusals\": expected both its bills, got {earlier:?}"
    );

    // A month narrowed to the account its first page reported still reports that
    // account's charge, and reports it beside the month's own account count: a
    // body that counts the month's priced accounts and then lists none of them
    // is what honouring a foreign cut point would answer.
    let (status, filtered) = charges(
        &router,
        &json!({ "billingMonth": PAST_MONTH, "accountId": reported_first }),
    )
    .await;
    assert_eq!(
        (status, reported_accounts(&filtered)),
        (200, vec![reported_first.clone()]),
        "case \"the narrowed month after the refusals\": expected that account's charge, \
         got {filtered}"
    );
    assert_eq!(
        filtered["accountCount"].as_i64(),
        Some(3),
        "case \"the narrowed month after the refusals\": expected the month's own account count, \
         got {filtered}"
    );

    // And the token still resumes the walk it was minted for.
    let mut resumed = walked_request.clone();
    resumed["nextToken"] = json!(token);
    let tail = walk(&router, "the walked month after the refusals", &resumed).await;
    assert_eq!(
        tail,
        walked[1..].to_vec(),
        "case \"the walked month after the refusals\": expected the bills after the cursor, \
         got {tail:?}"
    );
}

/// One sequence a request selects: the request itself, and the two things a
/// `nextToken` of its walk carries — the billing month whose bills it walks and
/// the account it is narrowed to.
struct Selection {
    case: &'static str,
    request: Value,
    month: &'static str,
    account: Option<&'static str>,
}

/// The member whose value differs first between two selections — the one the
/// refusal names, in the order a token carries them.
fn differing_member(minted: &Selection, replayed: &Selection) -> Option<&'static str> {
    if minted.month != replayed.month {
        Some(SCOPE_BILLING_MONTH)
    } else if minted.account != replayed.account {
        Some(SCOPE_ACCOUNT_FILTER)
    } else {
        None
    }
}

/// The selections the second test crosses with each other: they differ along the
/// billing month walked and the account the breakdown is narrowed to —
/// including months that hold no bill at all and an account no bill names.
fn selections() -> Vec<Selection> {
    vec![
        Selection {
            case: "the later month",
            request: json!({ "billingMonth": PAST_MONTH }),
            month: PAST_MONTH,
            account: None,
        },
        Selection {
            case: "the earlier month",
            request: json!({ "billingMonth": EARLIER_MONTH }),
            month: EARLIER_MONTH,
            account: None,
        },
        Selection {
            case: "an earlier month that holds no bill",
            request: json!({ "billingMonth": EMPTY_EARLIER_MONTH }),
            month: EMPTY_EARLIER_MONTH,
            account: None,
        },
        Selection {
            case: "a later month that holds no bill",
            request: json!({ "billingMonth": EMPTY_LATER_MONTH }),
            month: EMPTY_LATER_MONTH,
            account: None,
        },
        Selection {
            case: "the later month narrowed to one billed account",
            request: json!({ "billingMonth": PAST_MONTH, "accountId": MEMBER_ACCOUNT_ID }),
            month: PAST_MONTH,
            account: Some(MEMBER_ACCOUNT_ID),
        },
        Selection {
            case: "the later month narrowed to another billed account",
            request: json!({ "billingMonth": PAST_MONTH, "accountId": OTHER_MEMBER_ACCOUNT_ID }),
            month: PAST_MONTH,
            account: Some(OTHER_MEMBER_ACCOUNT_ID),
        },
        Selection {
            case: "the later month narrowed to an account with no bill in it",
            request: json!({ "billingMonth": PAST_MONTH, "accountId": UNBILLED_ACCOUNT_ID }),
            month: PAST_MONTH,
            account: Some(UNBILLED_ACCOUNT_ID),
        },
        Selection {
            case: "the earlier month narrowed to one billed account",
            request: json!({ "billingMonth": EARLIER_MONTH, "accountId": OTHER_MEMBER_ACCOUNT_ID }),
            month: EARLIER_MONTH,
            account: Some(OTHER_MEMBER_ACCOUNT_ID),
        },
    ]
}

/// Walk `request` to the end one bill a page, following only the tokens the read
/// itself issues, and report every account it reported in order.
///
/// The loop is bounded by the number of bills the suite seeds, so a walk that
/// fails to advance fails the test instead of running forever.
async fn paged_walk(router: &MultiServiceRouter, case: &str, request: &Value) -> Vec<String> {
    let mut reported: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..(SEEDED_BILLS + 2) {
        let mut page = request.clone();
        page["maxResults"] = json!(1);
        if let Some(token) = &token {
            page["nextToken"] = json!(token);
        }
        let (status, body) = charges(router, &page).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for a page of {request}, got {status} {body}"
        );
        reported.extend(reported_accounts(&body));
        token = body["nextToken"].as_str().map(ToString::to_string);
        if token.is_none() {
            return reported;
        }
    }
    panic!("case {case:?}: the paged walk of {request} must end, got {reported:?}")
}

#[tokio::test]
async fn a_selection_still_reports_every_bill_it_holds_after_another_selection_s_token() {
    let (router, engine) = emulator();
    seed(&engine);
    let selections = selections();

    for minted in &selections {
        // Only a selection whose walk holds more than one bill issues a token.
        if walk(&router, minted.case, &minted.request).await.len() < 2 {
            continue;
        }
        let token = first_token(&router, minted.case, &minted.request).await;

        for replayed in &selections {
            let Some(names) = differing_member(minted, replayed) else {
                continue;
            };
            let case = format!("{} resuming a {} token", replayed.case, minted.case);
            let mut request = replayed.request.clone();
            request["nextToken"] = json!(token.clone());
            let (status, body) = charges(&router, &request).await;
            assert_other_selection_refusal(&case, status, &body, names);

            // The invariant the refusal protects: paging the other selection
            // with its OWN tokens reports every bill it holds, exactly once and
            // in order — nothing the foreign cut point sorted above is missing.
            let whole = walk(&router, &case, &replayed.request).await;
            let paged = paged_walk(&router, &case, &replayed.request).await;
            assert_eq!(
                paged, whole,
                "case {case:?}: expected the paged walk to report every bill of {} exactly once, \
                 got {paged:?} against {whole:?}",
                replayed.request
            );
        }
    }
}
