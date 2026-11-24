// SPDX-License-Identifier: CC0-1.0

//! Which `nextToken` `GetCreditAllocationHistory` takes back, end to end over
//! the awsJson1_0 wire.
//!
//! Every assertion here is about ONE rule: a `nextToken` is a position among the
//! entries ONE request selected, so this read takes it back only from a request
//! that selects the same entries. The members that select them — the `accountId`
//! whose history is reported, the billing months the `startDate`/`endDate`
//! window covers, and the `creditId` the answer is narrowed to — travel in the
//! token and are compared with the resuming request; a token minted under
//! another selection is refused with the modeled `ValidationException` instead
//! of being applied as this walk's cut point, which would silently drop every
//! entry of it sorting at or above that cut point — entries the walk the token
//! came from never reported. `maxResults` is no part of a selection: a page size
//! is how much of one walk a caller takes, so it may change between pages.
//!
//! The rows span what the rule generalises over: how the window differs (widened
//! at either end, narrowed at either end, shifted, disjoint from the walked one,
//! the same months spelled by other instants inside them), the account (the
//! walked one, another one), the credit filter (none, the credit the first page
//! reported, a credit still to come, a credit no entry names) and the page size.
//! The second test is the invariant behind the rule: for every ordered pair of
//! selections, a token of one is refused by the other, and that other still
//! reports every entry it selects exactly once when it is paged with its OWN
//! tokens.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsJson1_0 harness — a router over ONE shared engine, the seeded
// allocations, the one call this suite makes and the response reader — already
// exists for this operation's derivation suite, so this module reuses it instead
// of building a second one.
use crate::credit_allocation_history::{
    at, call, emulator, error_code, persist, reported, seeds, EntryId, ACCOUNT_ID,
    OTHER_ACCOUNT_ID,
};

/// What the three members that select this read's sequence are called in the
/// refusal of a token minted under another selection, as the handler words them.
const SCOPE_BILLING_MONTH_RANGE: &str = "billing month range";
const SCOPE_ACCOUNT: &str = "account";
const SCOPE_CREDIT_FILTER: &str = "credit filter";

/// The `message` this read refuses a token it cannot read back at all with,
/// which a token minted for ANOTHER selection must not be answered with: that
/// token IS one this service minted, and what makes it unusable is the sequence
/// it is a cut point of.
const UNUSABLE_TOKEN_REFUSAL: &str = "nextToken is not a token this service issued";

/// The `ValidationExceptionReason` a refusal about a member carries.
const REASON_FIELD: &str = "fieldValidationFailed";

/// One sequence a request selects: the request itself, and the three things a
/// `nextToken` of its walk carries — the billing months the walk covers, the
/// account whose history it reports, and the credit it is narrowed to.
struct Selection {
    case: &'static str,
    request: Value,
    months: (&'static str, &'static str),
    account: &'static str,
    credit: Option<i64>,
}

/// Send one `GetCreditAllocationHistory` call.
async fn history(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "GetCreditAllocationHistory", request).await
}

/// The entries `request` reports in one page, asserting the call succeeded.
async fn walk(router: &MultiServiceRouter, case: &str, request: &Value) -> Vec<EntryId> {
    let (status, body) = history(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for {request}, got {status} {body}"
    );
    reported(&body)
}

/// The token a first page of ONE entry of `request` carries.
async fn first_token(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let mut paged = request.clone();
    paged["maxResults"] = json!(1);
    let (status, body) = history(router, &paged).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for a first page of one entry, got {status} {body}"
    );
    body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: a first page of one of several entries must carry a nextToken, \
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
    /// The request selects the same entries, so the walk resumes and reports
    /// exactly the entries of it that follow the cursor.
    Resumes,
    /// The request selects other entries, so the token is refused as one issued
    /// for another `&'static str` — the member that names them.
    RefusedFor(&'static str),
}

/// The window the first test walks: the two earliest seeded billing months of
/// the caller's own account, which hold three allocations — enough to page over,
/// and with newer months above them for a widened window to select.
fn walked_window() -> Value {
    json!({
        "accountId": ACCOUNT_ID,
        "startDate": at(2025, 1, 1, 0),
        "endDate": at(2025, 2, 15, 0),
    })
}

#[tokio::test]
async fn a_token_resumes_only_a_request_that_selects_the_same_allocations() {
    let (router, engine) = emulator();
    persist(&engine, &seeds());

    let walked = walk(&router, "the walked window", &walked_window()).await;
    assert!(
        walked.len() >= 3,
        "setup: the walked window must hold entries to page over, got {walked:?}"
    );
    let token = first_token(&router, "the walked window", &walked_window()).await;

    // What the widened window reports with NO token: the entries above the cut
    // point of the walked window, which a token honoured across the two windows
    // would drop.
    let widened = json!({
        "accountId": ACCOUNT_ID,
        "startDate": at(2025, 1, 1, 0),
        "endDate": at(2026, 6, 30, 23),
    });
    let whole = walk(&router, "the widened window", &widened).await;
    assert!(
        whole.len() > walked.len(),
        "setup: the widened window must select entries the walked one does not, got {whole:?}"
    );

    let rows: Vec<(&str, Value, Owed)> = vec![
        ("the walked selection again", walked_window(), Owed::Resumes),
        (
            "the walked selection with a larger page size",
            json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 1, 1, 0),
                "endDate": at(2025, 2, 15, 0),
                "maxResults": 1000,
            }),
            Owed::Resumes,
        ),
        (
            // The walk seeks whole billing months, so two windows whose bounds
            // fall in the same two months select the very same entries.
            "the walked months spelled by other instants inside them",
            json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 1, 31, 23),
                "endDate": at(2025, 2, 2, 0),
            }),
            Owed::Resumes,
        ),
        (
            "a window widened at its end",
            widened.clone(),
            Owed::RefusedFor(SCOPE_BILLING_MONTH_RANGE),
        ),
        (
            "a window widened at its start",
            json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2024, 12, 1, 0),
                "endDate": at(2025, 2, 15, 0),
            }),
            Owed::RefusedFor(SCOPE_BILLING_MONTH_RANGE),
        ),
        (
            "a window narrowed to the month the first page reported",
            json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 2, 1, 0),
                "endDate": at(2025, 2, 15, 0),
            }),
            Owed::RefusedFor(SCOPE_BILLING_MONTH_RANGE),
        ),
        (
            "a window narrowed to the month still to come",
            json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 1, 1, 0),
                "endDate": at(2025, 1, 31, 23),
            }),
            Owed::RefusedFor(SCOPE_BILLING_MONTH_RANGE),
        ),
        (
            "a window disjoint from the walked one",
            json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2026, 1, 1, 0),
                "endDate": at(2026, 6, 30, 23),
            }),
            Owed::RefusedFor(SCOPE_BILLING_MONTH_RANGE),
        ),
        (
            "another account's history over the walked months",
            json!({
                "accountId": OTHER_ACCOUNT_ID,
                "startDate": at(2025, 1, 1, 0),
                "endDate": at(2025, 2, 15, 0),
            }),
            Owed::RefusedFor(SCOPE_ACCOUNT),
        ),
        (
            "the walked months narrowed to the credit the first page reported",
            json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 1, 1, 0),
                "endDate": at(2025, 2, 15, 0),
                "creditId": 42,
            }),
            Owed::RefusedFor(SCOPE_CREDIT_FILTER),
        ),
        (
            "the walked months narrowed to a credit still to come",
            json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 1, 1, 0),
                "endDate": at(2025, 2, 15, 0),
                "creditId": 7,
            }),
            Owed::RefusedFor(SCOPE_CREDIT_FILTER),
        ),
        (
            "the walked months narrowed to a credit no entry names",
            json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 1, 1, 0),
                "endDate": at(2025, 2, 15, 0),
                "creditId": 1,
            }),
            Owed::RefusedFor(SCOPE_CREDIT_FILTER),
        ),
    ];

    for (case, mut request, owed) in rows {
        request["nextToken"] = json!(token.clone());
        let (status, body) = history(&router, &request).await;
        match owed {
            Owed::Resumes => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} {body}"
                );
                assert_eq!(
                    reported(&body),
                    walked[1..].to_vec(),
                    "case {case:?}: expected the entries after the cursor, got {body}"
                );
            }
            Owed::RefusedFor(names) => assert_other_selection_refusal(case, status, &body, names),
        }
    }

    // What the refusals kept: the widened window still reports every entry it
    // selects — including the newer months a cut point of the walked window
    // sorts below, which it would have dropped — and the token still resumes
    // the walk it was minted for.
    let after = walk(&router, "the widened window after the refusals", &widened).await;
    assert_eq!(
        after, whole,
        "case \"the widened window after the refusals\": expected every entry it selects, \
         got {after:?}"
    );
    let mut resumed = walked_window();
    resumed["nextToken"] = json!(token);
    let tail = walk(&router, "the walked window after the refusals", &resumed).await;
    assert_eq!(
        tail,
        walked[1..].to_vec(),
        "case \"the walked window after the refusals\": expected the entries after the cursor, \
         got {tail:?}"
    );
}

/// The member whose value differs first between two selections — the one the
/// refusal names, in the order a token carries them.
fn differing_member(minted: &Selection, replayed: &Selection) -> Option<&'static str> {
    if minted.months != replayed.months {
        Some(SCOPE_BILLING_MONTH_RANGE)
    } else if minted.account != replayed.account {
        Some(SCOPE_ACCOUNT)
    } else if minted.credit != replayed.credit {
        Some(SCOPE_CREDIT_FILTER)
    } else {
        None
    }
}

/// The selections the second test crosses with each other: they differ along the
/// billing months walked, the account whose history is read and the credit the
/// answer is narrowed to — including one selection that holds no entry at all.
fn selections() -> Vec<Selection> {
    vec![
        Selection {
            case: "the two earliest months",
            request: json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 1, 1, 0),
                "endDate": at(2025, 2, 15, 0),
            }),
            months: ("2025-01", "2025-02"),
            account: ACCOUNT_ID,
            credit: None,
        },
        Selection {
            case: "every seeded month",
            request: json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 1, 1, 0),
                "endDate": at(2026, 6, 30, 23),
            }),
            months: ("2025-01", "2026-06"),
            account: ACCOUNT_ID,
            credit: None,
        },
        Selection {
            case: "every seeded month, narrowed to one credit",
            request: json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 1, 1, 0),
                "endDate": at(2026, 6, 30, 23),
                "creditId": 7,
            }),
            months: ("2025-01", "2026-06"),
            account: ACCOUNT_ID,
            credit: Some(7),
        },
        Selection {
            case: "another account's history over every seeded month",
            request: json!({
                "accountId": OTHER_ACCOUNT_ID,
                "startDate": at(2025, 1, 1, 0),
                "endDate": at(2026, 6, 30, 23),
            }),
            months: ("2025-01", "2026-06"),
            account: OTHER_ACCOUNT_ID,
            credit: None,
        },
        Selection {
            case: "months that hold no allocation",
            request: json!({
                "accountId": ACCOUNT_ID,
                "startDate": at(2025, 7, 1, 0),
                "endDate": at(2025, 8, 15, 0),
            }),
            months: ("2025-07", "2025-08"),
            account: ACCOUNT_ID,
            credit: None,
        },
    ]
}

/// Walk `request` to the end one entry a page, following only the tokens the
/// read itself issues, and report every entry it reported in order.
///
/// The loop is bounded by the number of allocations the suite seeds, so a walk
/// that fails to advance fails the test instead of running forever.
async fn paged_walk(router: &MultiServiceRouter, case: &str, request: &Value) -> Vec<EntryId> {
    let mut walked: Vec<EntryId> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..(seeds().len() + 2) {
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
        walked.extend(reported(&body));
        token = body["nextToken"].as_str().map(ToString::to_string);
        if token.is_none() {
            return walked;
        }
    }
    panic!("case {case:?}: the paged walk of {request} must end, got {walked:?}")
}

#[tokio::test]
async fn a_selection_still_reports_every_entry_it_holds_after_another_selection_s_token() {
    let (router, engine) = emulator();
    persist(&engine, &seeds());
    let selections = selections();

    for minted in &selections {
        // Only a selection whose walk holds more than one entry issues a token.
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
            let (status, body) = history(&router, &request).await;
            assert_other_selection_refusal(&case, status, &body, names);

            // The invariant the refusal protects: paging the other selection
            // with its OWN tokens reports every entry it holds, exactly once
            // and in order — nothing the foreign cut point sorted above is
            // missing.
            let whole = walk(&router, &case, &replayed.request).await;
            let paged = paged_walk(&router, &case, &replayed.request).await;
            assert_eq!(
                paged, whole,
                "case {case:?}: expected the paged walk to report every entry of \
                 {} exactly once, got {paged:?} against {whole:?}",
                replayed.request
            );
        }
    }
}
