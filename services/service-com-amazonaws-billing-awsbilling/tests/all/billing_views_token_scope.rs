// SPDX-License-Identifier: CC0-1.0

//! Which `nextToken` `ListBillingViews` takes back, end to end over the
//! awsJson1_0 wire.
//!
//! Every assertion here is about ONE rule: a `nextToken` is a position among the
//! views ONE request selected, so this read takes it back only from a request
//! that selects the same views. Every member that narrows the walk — `arns`,
//! `billingViewTypes`, `names`, `ownerAccountId`, `sourceAccountId` and
//! `activeTimeRange` — travels in the token and is compared with the resuming
//! request; a token minted under another selection is refused with the modeled
//! `ValidationException` instead of being applied as this walk's cut point,
//! which would silently drop every view of the new selection sorting at or below
//! that cursor — views the walk the token came from never reported — and answer
//! that truncated, token-less page with a 200. `maxResults` is no part of a
//! selection: a page size is how much of one walk a caller takes, so it may
//! change between pages.
//!
//! The rows span what the rule generalises over: which filter member the
//! resuming request carries (each of the six), whether it selects all of the
//! walked set, a strict part of it, one view or none at all, how many values a
//! list filter carries and in which ORDER (a set filter tests membership, so the
//! same values in another order are the same selection), and the page size. The
//! second test is the invariant behind the rule: for every ordered pair of
//! selections, a token of one is refused by the other, and every selection still
//! reports each view it selects exactly once when it is paged with its OWN
//! tokens. The third is the sharpest case — a request naming exactly one view by
//! ARN, which this read resolves and `GetBillingView` answers 200 for, must
//! never be answered an empty page because a foreign cursor sorts above it. The
//! fourth holds the whole scope at once against the bound the wire puts on it:
//! a token minted for a request carrying every filter at the sizes the model
//! admits is still a `PageToken` the next request may send back.

use std::collections::BTreeSet;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsJson1_0 harness — a router over ONE shared engine, plus `call`,
// `error_code` and the clock reading — already exists for the billing-view
// lifecycle suite, so this module reuses it instead of building a second one.
use crate::billing_views::{
    call, emulator_with_engine, error_code, now_secs, ACCOUNT_ID, PRIMARY_ARN,
};

/// A 12-digit account that is not the caller's: no view this emulator holds is
/// owned by it or draws on it, so a request narrowed to it selects nothing.
const OTHER_ACCOUNT_ID: &str = "123456789012";

/// The `message` this read refuses a token it cannot read back at all with,
/// which a token minted for ANOTHER selection must not be answered with: that
/// token IS one this service minted, and what makes it unusable is the sequence
/// it is a cut point of.
const UNUSABLE_TOKEN_REFUSAL: &str = "nextToken is not a token this service issued";

/// The `ValidationExceptionReason` a refusal about a member carries.
const REASON_FIELD: &str = "fieldValidationFailed";

/// What each member that narrows this read's sequence is called in the refusal
/// of a token minted under another selection, in the order the token carries
/// their values.
const SCOPE_ARNS: &str = "set of billing view ARNs";
const SCOPE_TYPES: &str = "set of billing view types";
const SCOPE_NAMES: &str = "name search";
const SCOPE_OWNER: &str = "owner account";
const SCOPE_SOURCE: &str = "source account";
const SCOPE_ACTIVE_TIME_RANGE: &str = "active time range";

/// The names the three `CUSTOM` views this suite creates carry: two share a
/// prefix, so a `names` search selects a strict part of the set, and the third
/// does not.
const PREFIXED_NAMES: [&str; 2] = ["scope-alpha", "scope-beta"];
const OTHER_NAME: &str = "other-gamma";

/// The prefix [`PREFIXED_NAMES`] share, and one no view's name starts with.
const MATCHING_PREFIX: &str = "scope";
const ABSENT_PREFIX: &str = "nothing-starts-with-this";

/// The modeled `SearchOption`, which declares exactly one value
/// (`smithy_query(query="shape", shape="SearchOption")`).
const STARTS_WITH: &str = "STARTS_WITH";

/// Every value the modeled `BillingViewType` declares
/// (`smithy_query(query="shape", shape="BillingViewType")`), so the widest
/// `billingViewTypes` filter a request can carry is the whole enum.
const EVERY_VIEW_TYPE: [&str; 5] = [
    "PRIMARY",
    "BILLING_GROUP",
    "CUSTOM",
    "BILLING_TRANSFER",
    "BILLING_TRANSFER_SHOWBACK",
];

/// The bounds the modeled `PageToken` puts on the `nextToken` this read mints
/// and takes back: `length 1..4095`, `pattern ^[-a-zA-Z0-9+=/_]+$`
/// (`smithy_query(query="shape", shape="PageToken")`).
const PAGE_TOKEN_MAX_LENGTH: usize = 4095;
const PAGE_TOKEN_CHARACTERS: &str =
    "-abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789+=/_";

/// The largest `arns` filter the model admits (`BillingViewArnList`,
/// `len 0..10`), which is also how many views the fourth test creates.
const MAX_ARNS: usize = 10;

/// A name one character short of the modeled `BillingViewName` maximum of 128,
/// so a `names` search for it is near the `SearchValue` maximum of 128 too.
fn long_name_prefix() -> String {
    "p".repeat(127)
}

/// Create one `CUSTOM` billing view drawing on the account's `PRIMARY` view and
/// answer the ARN the service minted for it.
async fn create_view(router: &MultiServiceRouter, name: &str) -> String {
    let (status, created) = call(
        router,
        "CreateBillingView",
        &json!({ "name": name, "sourceViews": [PRIMARY_ARN], "clientToken": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup {name:?}: expected CreateBillingView to answer 200, got {status} {created}"
    );
    created["arn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup {name:?}: a create must answer an arn, got {created}"))
        .to_string()
}

/// The world every test here reads over: the account's `PRIMARY` view and three
/// `CUSTOM` views drawing on it, so each selection below has a walk of its own
/// and every walk of more than one view mints tokens.
///
/// The minted `CUSTOM` ARNs sort BELOW `billingview/primary`, so a cut point of
/// the unfiltered walk sits inside the ARN order: a selection holding only a view
/// below it would be answered empty if that cursor were honoured, and one holding
/// only the `PRIMARY` view would be answered its whole walk — two different wrong
/// answers from one foreign cursor.
async fn seed(router: &MultiServiceRouter) -> Vec<String> {
    let mut arns = Vec::new();
    for name in PREFIXED_NAMES.into_iter().chain([OTHER_NAME]) {
        arns.push(create_view(router, name).await);
    }
    arns.sort();
    arns
}

/// Send one `ListBillingViews` call.
async fn list(router: &MultiServiceRouter, request: &Value) -> (u16, Value) {
    call(router, "ListBillingViews", request).await
}

/// The ARNs one page of `request` reports, asserting the call succeeded.
async fn walk(router: &MultiServiceRouter, case: &str, request: &Value) -> Vec<String> {
    let (status, body) = list(router, request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for {request}, got {status} {body}"
    );
    reported_arns(&body)
}

/// The ARNs an answer reports, in the order it reports them.
fn reported_arns(body: &Value) -> Vec<String> {
    body["billingViews"]
        .as_array()
        .map(|listed| {
            listed
                .iter()
                .map(|view| view["arn"].as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The token a first page of ONE view of `request` carries.
async fn first_token(router: &MultiServiceRouter, case: &str, request: &Value) -> String {
    let mut paged = request.clone();
    paged["maxResults"] = json!(1);
    let (status, body) = list(router, &paged).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 for a first page of one view, got {status} {body}"
    );
    body["nextToken"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: a first page of one of several views must carry a nextToken, \
                 got {body}"
            )
        })
        .to_string()
}

/// Assert that `body` is the refusal this read answers a token minted under
/// another selection with: 400 `ValidationException` naming `nextToken` and the
/// member that put the caller on another walk, in the wording this service's
/// other filtered paginated reads refuse such a token with.
fn assert_other_selection_refusal(case: &str, status: u16, body: &Value, names: &str) {
    let why = format!("the pagination token was issued for another {names}");
    assert_eq!(
        (status, error_code(body).as_str()),
        (400, "ValidationException"),
        "case {case:?}: expected 400 ValidationException, got {status} {body}"
    );
    assert_eq!(
        body["message"].as_str(),
        Some(format!("nextToken: {why}").as_str()),
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
        (Some("nextToken"), Some(why.as_str())),
        "case {case:?}: expected the fieldList to name nextToken and why, got {body}"
    );
}

/// One sequence a request selects: the request itself, and the six filter
/// members a `nextToken` of its walk carries — each normalised the way the
/// filter reads it, so a set filter is a SET (the same values in another order,
/// or one of them twice, are the same selection) and an absent or empty filter
/// is the one that narrows nothing.
struct Selection {
    case: &'static str,
    request: Value,
    arns: BTreeSet<String>,
    types: BTreeSet<String>,
    names: BTreeSet<String>,
    owner: Option<String>,
    source: Option<String>,
    active_time_range: Option<(i64, i64)>,
}

impl Selection {
    /// The selection a request narrowing nothing makes; each row below then
    /// restates the members it does narrow by.
    fn everything(case: &'static str, request: Value) -> Self {
        Self {
            case,
            request,
            arns: BTreeSet::new(),
            types: BTreeSet::new(),
            names: BTreeSet::new(),
            owner: None,
            source: None,
            active_time_range: None,
        }
    }

    /// The page size this selection's request asks for, which bounds what a
    /// resumed page reports without being part of the selection at all. From the
    /// `maxResults` member doc: "Default is 100."
    fn page_size(&self) -> usize {
        self.request["maxResults"]
            .as_u64()
            .and_then(|size| usize::try_from(size).ok())
            .unwrap_or(100)
    }
}

/// The member whose value differs first between two selections — the one the
/// refusal names, in the order a token carries them.
fn differing_member(minted: &Selection, replayed: &Selection) -> Option<&'static str> {
    if minted.arns != replayed.arns {
        Some(SCOPE_ARNS)
    } else if minted.types != replayed.types {
        Some(SCOPE_TYPES)
    } else if minted.names != replayed.names {
        Some(SCOPE_NAMES)
    } else if minted.owner != replayed.owner {
        Some(SCOPE_OWNER)
    } else if minted.source != replayed.source {
        Some(SCOPE_SOURCE)
    } else if minted.active_time_range != replayed.active_time_range {
        Some(SCOPE_ACTIVE_TIME_RANGE)
    } else {
        None
    }
}

/// A `names` search as the one value its scope field carries.
fn name_search(prefix: &str) -> BTreeSet<String> {
    BTreeSet::from([format!("{STARTS_WITH}:{prefix}")])
}

/// The `names` request member searching for `prefix`.
fn names_filter(prefix: &str) -> Value {
    json!([{ "searchOption": STARTS_WITH, "searchValue": prefix }])
}

/// The `activeTimeRange` request member over `range`.
fn range_filter(range: (i64, i64)) -> Value {
    json!({ "activeAfterInclusive": range.0, "activeBeforeInclusive": range.1 })
}

/// The selections the tests cross with each other: they differ along every
/// member that narrows the walk, along how much of the walked set each selects
/// (all of it, a strict part, one view, none), along how many values a list
/// filter carries and in which order, and along the page size, which selects
/// nothing.
///
/// `activeTimeRange` "must be within one calendar month" (the member's own doc),
/// so the two periods below are an hour wide each: one ending after every view
/// was created, which selects them all, and one long past, which selects only
/// the account's `PRIMARY` view — the one the member documents as "always
/// listed".
fn selections(views: &[String]) -> Vec<Selection> {
    let now = now_secs();
    let covering_range = (now, now + 3_600);
    let past_range = (0, 3_600);
    let prefixed: Vec<String> = views[..PREFIXED_NAMES.len()].to_vec();
    let prefixed_set: BTreeSet<String> = prefixed.iter().cloned().collect();
    vec![
        Selection::everything("every view", json!({})),
        // The same selection spelled three other ways: a page size is not part
        // of a selection, and a list filter carrying no value narrows nothing,
        // so all four share one scope and one another's tokens.
        Selection::everything("every view, a page size of two", json!({ "maxResults": 2 })),
        Selection::everything("every view, an empty arns list", json!({ "arns": [] })),
        Selection::everything(
            "every view, an empty billingViewTypes list",
            json!({ "billingViewTypes": [] }),
        ),
        Selection {
            arns: prefixed_set.clone(),
            ..Selection::everything("two views named by ARN", json!({ "arns": prefixed }))
        },
        Selection {
            arns: prefixed_set,
            ..Selection::everything(
                "the same two ARNs in the other order",
                json!({ "arns": [views[1].clone(), views[0].clone()] }),
            )
        },
        Selection {
            arns: BTreeSet::from([views[0].clone()]),
            ..Selection::everything(
                "one view named by ARN",
                json!({ "arns": [views[0].clone()] }),
            )
        },
        Selection {
            types: BTreeSet::from(["CUSTOM".to_string()]),
            ..Selection::everything("the CUSTOM views", json!({ "billingViewTypes": ["CUSTOM"] }))
        },
        Selection {
            types: BTreeSet::from(["PRIMARY".to_string()]),
            ..Selection::everything("the PRIMARY view", json!({ "billingViewTypes": ["PRIMARY"] }))
        },
        Selection {
            types: BTreeSet::from(["CUSTOM".to_string(), "PRIMARY".to_string()]),
            ..Selection::everything(
                "both view types",
                json!({ "billingViewTypes": ["PRIMARY", "CUSTOM"] }),
            )
        },
        Selection {
            names: name_search(MATCHING_PREFIX),
            ..Selection::everything(
                "the views whose name starts with the prefix",
                json!({ "names": names_filter(MATCHING_PREFIX) }),
            )
        },
        Selection {
            names: name_search(ABSENT_PREFIX),
            ..Selection::everything(
                "a name prefix no view carries",
                json!({ "names": names_filter(ABSENT_PREFIX) }),
            )
        },
        Selection {
            owner: Some(ACCOUNT_ID.to_string()),
            ..Selection::everything(
                "the caller's own views",
                json!({ "ownerAccountId": ACCOUNT_ID }),
            )
        },
        Selection {
            owner: Some(OTHER_ACCOUNT_ID.to_string()),
            ..Selection::everything(
                "the views of another owner",
                json!({ "ownerAccountId": OTHER_ACCOUNT_ID }),
            )
        },
        Selection {
            source: Some(ACCOUNT_ID.to_string()),
            ..Selection::everything(
                "the views drawing on the caller's account",
                json!({ "sourceAccountId": ACCOUNT_ID }),
            )
        },
        Selection {
            source: Some(OTHER_ACCOUNT_ID.to_string()),
            ..Selection::everything(
                "the views drawing on another account",
                json!({ "sourceAccountId": OTHER_ACCOUNT_ID }),
            )
        },
        Selection {
            active_time_range: Some(covering_range),
            ..Selection::everything(
                "the views active over a period covering them all",
                json!({ "activeTimeRange": range_filter(covering_range) }),
            )
        },
        Selection {
            active_time_range: Some(past_range),
            ..Selection::everything(
                "the views active over a period long past",
                json!({ "activeTimeRange": range_filter(past_range) }),
            )
        },
        Selection {
            types: BTreeSet::from(["CUSTOM".to_string()]),
            names: name_search(MATCHING_PREFIX),
            ..Selection::everything(
                "the CUSTOM views carrying the prefix",
                json!({
                    "billingViewTypes": ["CUSTOM"],
                    "names": names_filter(MATCHING_PREFIX),
                }),
            )
        },
    ]
}

#[tokio::test]
async fn a_token_resumes_only_a_request_that_selects_the_same_views() {
    let (router, _engine) = emulator_with_engine();
    let views = seed(&router).await;
    let selections = selections(&views);

    // The walked sequence: every view the account holds, ascending by ARN.
    let walked = &selections[0];
    let every = walk(&router, walked.case, &walked.request).await;
    assert_eq!(
        every.len(),
        views.len() + 1,
        "setup: expected the created views and the PRIMARY one, got {every:?}"
    );
    let token = first_token(&router, walked.case, &walked.request).await;

    // What each selection reports before any foreign token is replayed against
    // it, so the refusals can be shown to have changed none of it.
    let mut before: Vec<Vec<String>> = Vec::new();
    for selection in &selections {
        before.push(walk(&router, selection.case, &selection.request).await);
    }

    for replayed in &selections {
        let case = format!("{} resuming a token of {}", replayed.case, walked.case);
        let mut request = replayed.request.clone();
        request["nextToken"] = json!(token.clone());
        let (status, body) = list(&router, &request).await;
        match differing_member(walked, replayed) {
            None => {
                // The same selection, whatever page size it asks for: the walk
                // resumes and reports the views after the cursor, up to that
                // size.
                let expected: Vec<String> = every[1..]
                    .iter()
                    .take(replayed.page_size())
                    .cloned()
                    .collect();
                assert_eq!(
                    (status, reported_arns(&body)),
                    (200, expected.clone()),
                    "case {case:?}: expected {expected:?}, got {status} {body}"
                );
            }
            Some(names) => assert_other_selection_refusal(&case, status, &body, names),
        }
    }

    // What the refusals kept: every selection still reports exactly the views it
    // selects, none of them dropped by a cursor of another selection, and the
    // token still resumes the walk it was minted for.
    for (selection, expected) in selections.iter().zip(&before) {
        let case = format!("{} after the refusals", selection.case);
        assert_eq!(
            &walk(&router, &case, &selection.request).await,
            expected,
            "case {case:?}: expected the selection's own whole walk"
        );
    }
    let mut resumed = walked.request.clone();
    resumed["nextToken"] = json!(token);
    assert_eq!(
        walk(&router, "the walked selection after the refusals", &resumed).await,
        every[1..].to_vec(),
        "case \"the walked selection after the refusals\": expected the views after the cursor"
    );
}

/// Walk `request` to the end one view a page, following only the tokens the read
/// itself issues, and report every ARN it reported in order.
///
/// The loop is bounded by the number of views the suite seeds, so a walk that
/// fails to advance fails the test instead of running forever.
async fn paged_walk(
    router: &MultiServiceRouter,
    case: &str,
    request: &Value,
    bound: usize,
) -> Vec<String> {
    let mut reported: Vec<String> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..(bound + 2) {
        let mut page = request.clone();
        page["maxResults"] = json!(1);
        if let Some(token) = &token {
            page["nextToken"] = json!(token);
        }
        let (status, body) = list(router, &page).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 for a page of {request}, got {status} {body}"
        );
        reported.extend(reported_arns(&body));
        token = body["nextToken"].as_str().map(ToString::to_string);
        if token.is_none() {
            return reported;
        }
    }
    panic!("case {case:?}: the paged walk of {request} must end, got {reported:?}")
}

#[tokio::test]
async fn a_selection_still_reports_every_view_it_holds_after_another_selection_s_token() {
    let (router, _engine) = emulator_with_engine();
    let views = seed(&router).await;
    let selections = selections(&views);
    let bound = views.len() + 1;

    // Every ordered pair of selections that differ: a token of one is refused by
    // the other, naming the member that disagrees.
    for minted in &selections {
        // Only a selection whose walk holds more than one view issues a token.
        if walk(&router, minted.case, &minted.request).await.len() < 2 {
            continue;
        }
        let token = first_token(&router, minted.case, &minted.request).await;
        for replayed in &selections {
            let Some(names) = differing_member(minted, replayed) else {
                continue;
            };
            let case = format!("{} resuming a token of {}", replayed.case, minted.case);
            let mut request = replayed.request.clone();
            request["nextToken"] = json!(token.clone());
            let (status, body) = list(&router, &request).await;
            assert_other_selection_refusal(&case, status, &body, names);
        }
    }

    // The invariant the refusals protect: paging any selection with its OWN
    // tokens reports every view it selects, exactly once and in order — nothing
    // a foreign cut point sorted above is missing.
    for selection in &selections {
        let case = format!("the paged walk of {}", selection.case);
        // The whole walk in one page: the modeled `maxResults` maximum, which
        // holds every view this suite seeds, so a row that asks for a smaller
        // page is compared against its own whole selection.
        let mut unpaged = selection.request.clone();
        unpaged["maxResults"] = json!(100);
        let whole = walk(&router, &case, &unpaged).await;
        let paged = paged_walk(&router, &case, &selection.request, bound).await;
        assert_eq!(
            paged, whole,
            "case {case:?}: expected the paged walk to report every view of {} exactly once, \
             got {paged:?} against {whole:?}",
            selection.request
        );
    }
}

#[tokio::test]
async fn a_request_naming_one_view_is_never_answered_an_empty_page_from_a_foreign_cursor() {
    let (router, _engine) = emulator_with_engine();
    let views = seed(&router).await;

    // The cursor of an unfiltered first page: the lowest ARN the account holds,
    // which is one of the created CUSTOM views.
    let every = walk(&router, "every view", &json!({})).await;
    let token = first_token(&router, "every view", &json!({})).await;
    assert_eq!(
        every.first().map(String::as_str),
        views.first().map(String::as_str),
        "setup: expected the walk to open at the lowest created ARN, got {every:?}"
    );

    // Each row names exactly one view this store resolves, at, below and above
    // the cursor: honouring the cursor would answer the first two an empty,
    // token-less page for a view the request names and the store holds.
    for arn in every.iter() {
        let case = format!("the view {arn} named by ARN");
        let mut request = json!({ "arns": [arn] });
        request["nextToken"] = json!(token.clone());
        let (status, body) = list(&router, &request).await;
        assert_other_selection_refusal(&case, status, &body, SCOPE_ARNS);

        // Without the foreign token the same request reports that one view, and
        // the view resolves through the single-view read as well: the refusal is
        // what keeps the two reads agreeing.
        assert_eq!(
            walk(&router, &case, &json!({ "arns": [arn] })).await,
            vec![arn.clone()],
            "case {case:?}: expected the one view the request names"
        );
        let (status, read) = call(&router, "GetBillingView", &json!({ "arn": arn })).await;
        assert_eq!(
            (status, read["billingView"]["arn"].as_str()),
            (200, Some(arn.as_str())),
            "case {case:?}: expected GetBillingView to answer the same view, got {status} {read}"
        );
    }

    // The widening direction: a token minted under a filter, replayed with the
    // filter dropped, must not answer a complete-looking list that is missing
    // every view at or below the cursor.
    let narrow = json!({ "billingViewTypes": ["CUSTOM"] });
    let narrow_token = first_token(&router, "the CUSTOM views", &narrow).await;
    let mut widened = json!({ "nextToken": narrow_token });
    let (status, body) = list(&router, &widened).await;
    assert_other_selection_refusal(
        "the unfiltered walk resuming a CUSTOM-only token",
        status,
        &body,
        SCOPE_TYPES,
    );
    widened = json!({});
    assert_eq!(
        walk(&router, "the unfiltered walk after the refusal", &widened).await,
        every,
        "case \"the unfiltered walk after the refusal\": expected every view the account holds"
    );
}

#[tokio::test]
async fn a_token_of_the_widest_filtered_request_is_still_a_page_token_the_wire_carries() {
    let (router, _engine) = emulator_with_engine();

    // The modeled maximum of `arns` entries, each a view this store resolves,
    // every one named at the `BillingViewName` maximum so the `names` search is
    // near the `SearchValue` maximum too.
    let prefix = long_name_prefix();
    let mut arns: Vec<String> = Vec::new();
    for index in 0..MAX_ARNS {
        arns.push(create_view(&router, &format!("{prefix}{index}")).await);
    }
    arns.sort();

    let now = now_secs();
    let widest = json!({
        "arns": arns,
        "billingViewTypes": EVERY_VIEW_TYPE,
        "names": names_filter(&prefix),
        "ownerAccountId": ACCOUNT_ID,
        "sourceAccountId": ACCOUNT_ID,
        "activeTimeRange": range_filter((now, now + 3_600)),
    });
    let case = "every filter at the sizes the model admits";
    let every = walk(&router, case, &widest).await;
    assert_eq!(
        every.len(),
        MAX_ARNS,
        "setup: expected the filters to select every created view, got {every:?}"
    );

    // The whole selection travels in the token, so the token a request of this
    // size mints is the largest this read can hand back — and it is still a
    // value the next request may send, inside the modeled `PageToken` length
    // and pattern.
    let token = first_token(&router, case, &widest).await;
    assert!(
        !token.is_empty() && token.len() <= PAGE_TOKEN_MAX_LENGTH,
        "case {case:?}: expected a token within the modeled PageToken length, got {} characters",
        token.len()
    );
    assert!(
        token.chars().all(|digit| PAGE_TOKEN_CHARACTERS.contains(digit)),
        "case {case:?}: expected a token inside the modeled PageToken pattern, got {token:?}"
    );

    // And the wire takes it back: the walk resumes after the cursor, while the
    // same request with one ARN dropped selects other views and is refused.
    let mut resumed = widest.clone();
    resumed["nextToken"] = json!(token.clone());
    assert_eq!(
        walk(&router, case, &resumed).await,
        every[1..].to_vec(),
        "case {case:?}: expected the views after the cursor"
    );
    let mut narrowed = widest.clone();
    narrowed["arns"] = json!(arns[1..].to_vec());
    narrowed["nextToken"] = json!(token);
    let (status, body) = list(&router, &narrowed).await;
    assert_other_selection_refusal(
        "the widest request with one ARN dropped",
        status,
        &body,
        SCOPE_ARNS,
    );
}
