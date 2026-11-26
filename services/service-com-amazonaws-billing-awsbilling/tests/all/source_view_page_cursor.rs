// SPDX-License-Identifier: CC0-1.0

//! What a `ListSourceViewsForBillingView` page resumes at, end to end over the
//! awsJson1_0 wire.
//!
//! Every assertion here is about the RULE every paginated read of this service
//! follows: a page resumes strictly AFTER the entries the previous pages
//! reported, identified by the VALUES the store holds and bound to the sequence
//! the token was minted over, so an entry associated or disassociated between
//! two pages cannot make the walk skip or repeat another one, and a token minted
//! while walking one billing view is not a cut point in another's list.
//!
//! The rows differ along what the rule generalises over: how many entries the
//! view holds (from one to the modelled maximum of 10,
//! `BillingViewSourceViewsList` len 1..10), the page size the caller asks for,
//! and WHICH entries the walk's own list loses between two pages — the first,
//! the one the page ended on, a later one, or several at once.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The awsJson1_0 harness — a router over ONE shared engine, plus `call` and
// `error_code` — already exists for the billing-view lifecycle suite, so this
// module reuses it instead of building a second one.
use crate::billing_views::{call, emulator_with_engine, error_code, PRIMARY_ARN};
// The `BillingViewArn` formatter and the create helper the disassociation suite
// already owns (the create plants the views a row names as sources, since a
// create resolves each of them against the store), and the `sourceViews`
// extractor the suite that owns this operation's table owns: this module adds a
// second copy of none of them.
use crate::source_view_disassociation::{create_view, source_arn};
use crate::source_views::source_views;

/// One page of the walk: its entries and the token that continues it.
async fn page_of(
    router: &MultiServiceRouter,
    case: &str,
    arn: &str,
    max_results: usize,
    next_token: Option<&str>,
) -> (Vec<String>, Option<String>) {
    let mut request = json!({ "arn": arn, "maxResults": max_results });
    if let Some(token) = next_token {
        request["nextToken"] = json!(token);
    }
    let (status, page) = call(router, "ListSourceViewsForBillingView", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected 200 from ListSourceViewsForBillingView, got {status} {page:?}"
    );
    let entries = source_views(&page);
    assert!(
        entries.len() <= max_results,
        "case {case:?}: a page holds at most maxResults entries, got {entries:?}"
    );
    let token = page
        .get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    (entries, token)
}

/// Remove `sources` from the view `arn`'s association set.
async fn disassociate(router: &MultiServiceRouter, case: &str, arn: &str, sources: &[String]) {
    let (status, body) = call(
        router,
        "DisassociateSourceViews",
        &json!({ "arn": arn, "sourceViews": sources }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected DisassociateSourceViews to answer 200, got {status} {body:?}"
    );
}

/// One row of the shrink-mid-walk table.
struct ShrinkRow {
    case: &'static str,
    /// How many source views the view is created with.
    entries: usize,
    /// The page size every page of the walk asks for.
    page_size: usize,
    /// The positions in the created list that are disassociated once the first
    /// page has been read.
    removed: &'static [usize],
}

/// The rows: which entries the list loses after the first page, over several
/// list sizes and page sizes.
fn shrink_rows() -> Vec<ShrinkRow> {
    vec![
        ShrinkRow {
            case: "the entry before the page boundary goes",
            entries: 4,
            page_size: 2,
            removed: &[0],
        },
        ShrinkRow {
            case: "the entry the page ended on goes",
            entries: 4,
            page_size: 2,
            removed: &[1],
        },
        ShrinkRow {
            case: "the whole first page goes",
            entries: 5,
            page_size: 2,
            removed: &[0, 1],
        },
        ShrinkRow {
            case: "an entry the walk has not reached goes",
            entries: 5,
            page_size: 2,
            removed: &[4],
        },
        ShrinkRow {
            case: "entries on both sides of the boundary go",
            entries: 6,
            page_size: 2,
            removed: &[0, 2, 5],
        },
        ShrinkRow {
            case: "one entry per page over the modelled maximum",
            entries: 10,
            page_size: 1,
            removed: &[0],
        },
        ShrinkRow {
            // A removal may not empty the association (`source_view_minimum.rs`),
            // so the smallest list a walk can lose an entry from holds two.
            case: "the entry a one-entry page reported, leaving the modelled minimum",
            entries: 2,
            page_size: 1,
            removed: &[0],
        },
    ]
}

#[tokio::test]
async fn a_walk_reports_every_entry_that_stays_associated_exactly_once() {
    let (router, engine) = emulator_with_engine();

    for (index, row) in shrink_rows().iter().enumerate() {
        let case = row.case;
        let stored: Vec<String> = (0..row.entries)
            .map(|entry| source_arn(&format!("shrink-{index}-{entry}")))
            .collect();
        let arn = create_view(
            &router,
            &engine,
            case,
            &format!("shrink-view-{index}"),
            &stored,
        )
        .await;

        // The first page, then the list loses the row's entries, then the walk
        // runs to its end with the token the first page handed back.
        let (first, mut token) = page_of(&router, case, &arn, row.page_size, None).await;
        let mut collected = first;

        let removed: Vec<String> = row.removed.iter().map(|at| stored[*at].clone()).collect();
        disassociate(&router, case, &arn, &removed).await;

        let mut pages = 1;
        while let Some(resume) = token.clone() {
            let (entries, next) = page_of(&router, case, &arn, row.page_size, Some(&resume)).await;
            collected.extend(entries);
            token = next;
            pages += 1;
            assert!(
                pages <= row.entries + 2,
                "case {case:?}: the walk must terminate, got {pages} pages of {collected:?}"
            );
        }

        // No entry is reported twice, however the list moved under the walk.
        let mut seen = collected.clone();
        seen.sort();
        let before = seen.len();
        seen.dedup();
        assert_eq!(
            seen.len(),
            before,
            "case {case:?}: an entry was reported more than once, got {collected:?}"
        );

        // Every entry that was associated for the whole walk is reported, so no
        // entry is skipped; an entry the walk lost may or may not have been
        // reported before it went, but it is never invented afterwards.
        for (at, entry) in stored.iter().enumerate() {
            let reported = collected.iter().filter(|seen| *seen == entry).count();
            if row.removed.contains(&at) {
                assert!(
                    reported <= 1,
                    "case {case:?}: the disassociated {entry} was reported {reported} times in {collected:?}"
                );
            } else {
                assert_eq!(
                    reported, 1,
                    "case {case:?}: expected {entry} reported exactly once, got {reported} in {collected:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn an_entry_associated_between_two_pages_is_reported_without_repeating_another() {
    let (router, engine) = emulator_with_engine();
    let case = "a source view associated mid-walk";

    // `AssociateSourceViews` requires every source to name a view this account
    // holds, so the two views that join the walk are created first.
    let joined =
        create_view(&router, &engine, case, "cursor-joined", &[PRIMARY_ARN.to_string()]).await;
    let late =
        create_view(&router, &engine, case, "cursor-late", &[PRIMARY_ARN.to_string()]).await;
    let target = create_view(
        &router,
        &engine,
        case,
        "cursor-target",
        &[PRIMARY_ARN.to_string(), joined.clone()],
    )
    .await;

    let (first, token) = page_of(&router, case, &target, 1, None).await;
    assert_eq!(
        first,
        vec![PRIMARY_ARN.to_string()],
        "case {case:?}: expected the first stored entry, got {first:?}"
    );
    let token = token.unwrap_or_else(|| panic!("case {case:?}: two entries need a second page"));

    let (status, associated) = call(
        &router,
        "AssociateSourceViews",
        &json!({ "arn": target, "sourceViews": [late.clone()] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected AssociateSourceViews to answer 200, got {status} {associated:?}"
    );

    // The walk continues after the entry it reported, so the entry that joined
    // the list behind the cursor is reported too and the one already handed back
    // is not repeated.
    let mut collected = first;
    let mut token = Some(token);
    let mut pages = 1;
    while let Some(resume) = token.clone() {
        let (entries, next) = page_of(&router, case, &target, 1, Some(&resume)).await;
        collected.extend(entries);
        token = next;
        pages += 1;
        assert!(
            pages <= 5,
            "case {case:?}: the walk must terminate, got {pages} pages of {collected:?}"
        );
    }
    assert_eq!(
        collected,
        vec![PRIMARY_ARN.to_string(), joined, late],
        "case {case:?}: expected every association exactly once in stored order, got {collected:?}"
    );
}

/// One row of the cross-view table: a walk of `minted_entries` entries whose
/// token is offered to a view holding `other_entries`.
struct CrossViewRow {
    case: &'static str,
    minted_entries: usize,
    other_entries: usize,
    page_size: usize,
}

#[tokio::test]
async fn a_token_minted_while_walking_one_view_is_refused_for_another() {
    let (router, engine) = emulator_with_engine();

    let rows = [
        CrossViewRow {
            case: "a shorter walk's token offered to a longer list",
            minted_entries: 2,
            other_entries: 3,
            page_size: 1,
        },
        CrossViewRow {
            case: "a longer walk's token offered to a shorter list",
            minted_entries: 6,
            other_entries: 2,
            page_size: 2,
        },
        CrossViewRow {
            case: "a token offered to a view holding the modelled maximum",
            minted_entries: 3,
            other_entries: 10,
            page_size: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let minted_sources: Vec<String> = (0..row.minted_entries)
            .map(|entry| source_arn(&format!("walked-{index}-{entry}")))
            .collect();
        let other_sources: Vec<String> = (0..row.other_entries)
            .map(|entry| source_arn(&format!("other-{index}-{entry}")))
            .collect();
        let walked = create_view(
            &router,
            &engine,
            case,
            &format!("walked-{index}"),
            &minted_sources,
        )
        .await;
        let other = create_view(
            &router,
            &engine,
            case,
            &format!("other-{index}"),
            &other_sources,
        )
        .await;

        let (_, token) = page_of(&router, case, &walked, row.page_size, None).await;
        let token = token.unwrap_or_else(|| {
            panic!("case {case:?}: a walk of {} entries pages", row.minted_entries)
        });

        let (status, refused) = call(
            &router,
            "ListSourceViewsForBillingView",
            &json!({ "arn": other, "nextToken": token, "maxResults": row.page_size }),
        )
        .await;
        assert_eq!(
            (status, error_code(&refused).as_str()),
            (400, "ValidationException"),
            "case {case:?}: expected 400 ValidationException for a foreign token, got {status} {refused:?}"
        );

        // The refusal left the other view's own walk intact: it still reports
        // every entry it holds, starting at the first one.
        let (first, _) = page_of(&router, case, &other, row.other_entries, None).await;
        assert_eq!(
            first, other_sources,
            "case {case:?}: expected the other view's whole list, got {first:?}"
        );

        // And the token still continues the walk it was minted for.
        let (resumed, _) = page_of(
            &router,
            case,
            &walked,
            row.minted_entries,
            Some(token.as_str()),
        )
        .await;
        assert_eq!(
            resumed,
            minted_sources[row.page_size..].to_vec(),
            "case {case:?}: expected the walk to resume after its first page, got {resumed:?}"
        );
    }
}
