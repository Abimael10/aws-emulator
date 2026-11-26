// SPDX-License-Identifier: CC0-1.0

//! The ONE pagination rule both listings of this service implement, driven end-to-end
//! through each of them over the same wire the sibling suites use (the shared harness in
//! [`super::snapshot_block_loop`]).
//!
//! `ListSnapshotBlocks` (over the blocks of one snapshot) and `ListChangedBlocks` (over the
//! changed indexes of two) page identically, so the rule is stated once in
//! `crate::state` and asserted here through BOTH operations:
//!
//! 1. THE PAGE SIZE IS `MaxResults`, OR THE SERVICE'S DEFAULT: a request that names
//!    `MaxResults` gets at most that many entries, and one that names none gets the
//!    emulator's default page — the model bounds `MaxResults` at `range 100..10000` and
//!    documents no default, so the default is the smallest page a caller may ask for. A page
//!    carries a `NextToken` exactly while a further entry remains.
//! 2. A `NextToken` IS CHECKED AGAINST THE LISTING IT RESUMES: a token is accepted exactly
//!    when it is one this service issued for that listing — it names the snapshot the
//!    listing is of AND an index that listing reports — and resumes the page there,
//!    overriding any `StartingBlockIndex` beside it ("If you specify NextToken, then
//!    StartingBlockIndex is ignored"). Anything else is refused with the modelled
//!    `ValidationException` carrying the `INVALID_PAGE_TOKEN` reason and the one sentence
//!    naming the snapshot the listing is of. Because each listing reports a different set,
//!    the same token can be issued for one and not for the other, and each listing judges it
//!    against its own set.
//!
//! The rows differ along which listing is asked, whether and how `MaxResults` is named,
//! which page of the listing is fetched, and how a token fails to be one this service
//! issued (not hex at all, hex that is not a page token, a token of another snapshot, an
//! index the listing does not report) — so no row is the specification.

use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_ebs_ebs::state::{encode_page_token, query_safe_token_text};

use super::changed_blocks_between_snapshots::sealed_snapshot;
use super::snapshot_block_loop::{list_blocks, member, router, send, Reply};

/// The HTTP status the modelled `ValidationException` carries (`@httpError 400`).
const VALIDATION_STATUS: u16 = 400;

/// The error type the modelled `ValidationException` is dispatched on, as the
/// `x-amzn-errortype` header carries it.
const VALIDATION_ERROR_TYPE: &str = "ValidationException";

/// The reason the modelled `ValidationException` carries for a `NextToken` this service did
/// not issue for the listing it was handed to (`ValidationExceptionReason`).
const INVALID_PAGE_TOKEN_REASON: &str = "INVALID_PAGE_TOKEN";

/// The smallest `MaxResults` the model admits, which is also the page an unasked-for page
/// carries: the modelled `range` is 100..10000.
const SMALLEST_MAX_RESULTS: i32 = 100;

/// The largest `MaxResults` the model admits (`range 100..10000`).
const LARGEST_MAX_RESULTS: i32 = 10_000;

/// The sentence a token this service did not issue is refused with: fixed text around the
/// one variable part, the snapshot the listing is of. Written out here so both listings are
/// pinned to the same words.
fn invalid_page_token_message(snapshot_id: &str) -> String {
    format!("NextToken is not a page token this service issued for snapshot {snapshot_id}")
}

/// Which listing a row asks. Both page by the one rule; they differ in what they list and
/// therefore in which indexes a token of theirs may name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listing {
    /// `ListSnapshotBlocks`: the blocks one snapshot holds.
    SnapshotBlocks,
    /// `ListChangedBlocks`: the indexes two snapshots of one lineage differ at.
    ChangedBlocks,
}

impl Listing {
    /// The output member this listing reports its entries in.
    fn entries_member(self) -> &'static str {
        match self {
            Self::SnapshotBlocks => "Blocks",
            Self::ChangedBlocks => "ChangedBlocks",
        }
    }
}

/// One page of `listing` over `snapshot_id`, comparing it against `base` where the listing
/// is a comparison, with `query` appended to the URI.
async fn page(
    router: &MultiServiceRouter,
    listing: Listing,
    base: &str,
    snapshot_id: &str,
    query: &str,
) -> Reply {
    match listing {
        Listing::SnapshotBlocks => list_blocks(router, snapshot_id, query).await,
        Listing::ChangedBlocks => {
            let mut full = format!("firstSnapshotId={base}");
            if !query.is_empty() {
                full.push('&');
                full.push_str(query);
            }
            let uri = format!("/snapshots/{snapshot_id}/changedblocks?{full}");
            send(router, "GET", &uri, &[], Vec::new()).await
        }
    }
}

/// The block indexes a page of either listing reported, in the order it reported them.
fn reported_indexes(listing: Listing, reply: &Reply) -> Vec<i32> {
    reply
        .json()
        .get(listing.entries_member())
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| {
                    let index = entry
                        .get("BlockIndex")
                        .and_then(serde_json::Value::as_i64)
                        .unwrap_or(-1);
                    i32::try_from(index).unwrap_or(-1)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The `NextToken` a page carries, or `None` when it carries none.
fn next_token(reply: &Reply) -> Option<String> {
    reply
        .json()
        .get("NextToken")
        .and_then(serde_json::Value::as_str)
        .map(str::to_string)
}

/// Assert a page answered 200 and reported exactly `expected`, and answer its `NextToken`.
fn assert_page(case: &str, listing: Listing, reply: &Reply, expected: &[i32]) -> Option<String> {
    assert_eq!(
        reply.status,
        200,
        "case {case:?}: expected HTTP 200, got {} with body {}",
        reply.status,
        String::from_utf8_lossy(&reply.body)
    );
    let reported = reported_indexes(listing, reply);
    assert_eq!(
        reported,
        expected.to_vec(),
        "case {case:?}: expected the page to report {expected:?}, got {reported:?}"
    );
    next_token(reply)
}

/// Assert a listing refused the `pageToken` it was handed: the modelled validation, the
/// `INVALID_PAGE_TOKEN` reason, the one sentence naming `snapshot_id`, and no entries at
/// all.
fn assert_page_token_refused(case: &str, listing: Listing, reply: &Reply, snapshot_id: &str) {
    assert_eq!(
        reply.status,
        VALIDATION_STATUS,
        "case {case:?}: expected HTTP {VALIDATION_STATUS}, got {} with body {}",
        reply.status,
        String::from_utf8_lossy(&reply.body)
    );
    assert!(
        reply.header("x-amzn-errortype").contains(VALIDATION_ERROR_TYPE),
        "case {case:?}: expected the x-amzn-errortype header to name {VALIDATION_ERROR_TYPE}, \
         got {:?}",
        reply.header("x-amzn-errortype")
    );
    assert_eq!(
        member(reply, "Reason"),
        INVALID_PAGE_TOKEN_REASON,
        "case {case:?}: expected the {INVALID_PAGE_TOKEN_REASON} reason, got {}",
        reply.json()
    );
    assert_eq!(
        member(reply, "Message"),
        invalid_page_token_message(snapshot_id),
        "case {case:?}: expected the shared page-token refusal wording, got {}",
        reply.json()
    );
    assert!(
        reply.json().get(listing.entries_member()).is_none(),
        "case {case:?}: a refused listing must report no entries, got {}",
        reply.json()
    );
}

/// One row of the page-size table: the case, which listing is asked, the query it carries,
/// the indexes the page must report, and whether a further entry must remain.
type PageSizeRow = (&'static str, Listing, String, Vec<i32>, bool);

/// RULE 1: the page size is the `MaxResults` the request named, or this service's default
/// when it named none — through BOTH listings, over one lineage large enough to paginate at
/// that default.
#[tokio::test]
async fn both_listings_page_at_max_results_or_at_the_shared_default() {
    let router = router();
    let case = "a lineage larger than the default page";

    // The default page is the smallest `MaxResults` the model admits, so one entry more
    // than that is the smallest listing that has to paginate without being asked to.
    let base = sealed_snapshot(&router, case, 1, None, &[(0, 1)]).await;
    let written: Vec<(i32, u8)> = (1..=SMALLEST_MAX_RESULTS + 1)
        .map(|index| (index, u8::try_from(index % 251).expect("below 251")))
        .collect();
    let child = sealed_snapshot(&router, case, 1, Some(&base), &written).await;

    // The child holds the inherited block at 0 and everything it wrote; the comparison
    // against the base reports only what it wrote, since index 0 holds the same bytes on
    // both sides.
    let held: Vec<i32> = (0..=SMALLEST_MAX_RESULTS + 1).collect();
    let changed: Vec<i32> = (1..=SMALLEST_MAX_RESULTS + 1).collect();
    let page_size = usize::try_from(SMALLEST_MAX_RESULTS).expect("a small page size");

    let rows: Vec<PageSizeRow> = vec![
        (
            "ListSnapshotBlocks naming no MaxResults",
            Listing::SnapshotBlocks,
            String::new(),
            held[..page_size].to_vec(),
            true,
        ),
        (
            "ListChangedBlocks naming no MaxResults",
            Listing::ChangedBlocks,
            String::new(),
            changed[..page_size].to_vec(),
            true,
        ),
        (
            "ListSnapshotBlocks naming the smallest MaxResults the model admits",
            Listing::SnapshotBlocks,
            format!("maxResults={SMALLEST_MAX_RESULTS}"),
            held[..page_size].to_vec(),
            true,
        ),
        (
            "ListChangedBlocks naming the smallest MaxResults the model admits",
            Listing::ChangedBlocks,
            format!("maxResults={SMALLEST_MAX_RESULTS}"),
            changed[..page_size].to_vec(),
            true,
        ),
        (
            "ListSnapshotBlocks naming the largest MaxResults the model admits",
            Listing::SnapshotBlocks,
            format!("maxResults={LARGEST_MAX_RESULTS}"),
            held.clone(),
            false,
        ),
        (
            "ListChangedBlocks naming the largest MaxResults the model admits",
            Listing::ChangedBlocks,
            format!("maxResults={LARGEST_MAX_RESULTS}"),
            changed.clone(),
            false,
        ),
        (
            "ListSnapshotBlocks starting past the default page",
            Listing::SnapshotBlocks,
            format!("startingBlockIndex={SMALLEST_MAX_RESULTS}"),
            held[page_size..].to_vec(),
            false,
        ),
        (
            "ListChangedBlocks starting past the default page",
            Listing::ChangedBlocks,
            format!("startingBlockIndex={SMALLEST_MAX_RESULTS}"),
            changed[page_size - 1..].to_vec(),
            false,
        ),
    ];

    for (case, listing, query, expected, more_remains) in rows {
        let reply = page(&router, listing, &base, &child, &query).await;
        let token = assert_page(case, listing, &reply, &expected);
        assert_eq!(
            token.is_some(),
            more_remains,
            "case {case:?}: a NextToken must be carried exactly while a further entry \
             remains (expected more: {more_remains}), got {}",
            reply.json()
        );
    }

    // The default page of each listing, resumed through the token it carried: the rest of
    // the listing, bounded by that same default, and no token once nothing remains.
    for (listing, entries) in [
        (Listing::SnapshotBlocks, held),
        (Listing::ChangedBlocks, changed),
    ] {
        let case = "the default page resumed through its own NextToken";
        let first = page(&router, listing, &base, &child, "").await;
        let token = assert_page(case, listing, &first, &entries[..page_size])
            .unwrap_or_else(|| panic!("case {case:?}: {listing:?} must carry a NextToken"));
        let resumed = page(&router, listing, &base, &child, &format!("pageToken={token}")).await;
        let tail = assert_page(case, listing, &resumed, &entries[page_size..]);
        assert!(
            tail.is_none(),
            "case {case:?}: {listing:?} must omit NextToken once nothing remains, got {}",
            resumed.json()
        );
    }
}

/// One row of the page-token table: the case, which listing is asked, the query it carries,
/// and the indexes the page must report — or `None` where the token is not one this service
/// issued for that listing, so the listing must refuse it.
type PageTokenRow = (&'static str, Listing, String, Option<Vec<i32>>);

/// RULE 2: a `NextToken` is accepted exactly when it is one this service issued for the
/// listing it is handed to, and refused in the one shared wording otherwise — asserted
/// through BOTH listings, including the index one of them reports and the other does not.
#[tokio::test]
async fn both_listings_check_a_page_token_against_what_they_report() {
    let router = router();
    let case = "a small lineage";

    let base = sealed_snapshot(&router, case, 1, None, &[(0, 1)]).await;
    // The child inherits the base's block at 0 and writes two of its own, so it HOLDS
    // {0, 1, 3} while the comparison against the base reports {1, 3}: index 0 is an index
    // one listing reports and the other does not.
    let child = sealed_snapshot(&router, case, 1, Some(&base), &[(1, 2), (3, 4)]).await;

    let rows: Vec<PageTokenRow> = vec![
        (
            "a token for an index both listings report, through the block listing",
            Listing::SnapshotBlocks,
            format!("pageToken={}", encode_page_token(&child, 1)),
            Some(vec![1, 3]),
        ),
        (
            "a token for an index both listings report, through the comparison",
            Listing::ChangedBlocks,
            format!("pageToken={}", encode_page_token(&child, 1)),
            Some(vec![1, 3]),
        ),
        (
            "a token whose index only the block listing reports, through the block listing",
            Listing::SnapshotBlocks,
            format!("pageToken={}", encode_page_token(&child, 0)),
            Some(vec![0, 1, 3]),
        ),
        (
            "a token whose index only the block listing reports, through the comparison",
            Listing::ChangedBlocks,
            format!("pageToken={}", encode_page_token(&child, 0)),
            None,
        ),
        (
            "a token beside a StartingBlockIndex it overrides, through the block listing",
            Listing::SnapshotBlocks,
            format!(
                "startingBlockIndex=0&pageToken={}",
                encode_page_token(&child, 3)
            ),
            Some(vec![3]),
        ),
        (
            "a token beside a StartingBlockIndex it overrides, through the comparison",
            Listing::ChangedBlocks,
            format!(
                "startingBlockIndex=0&pageToken={}",
                encode_page_token(&child, 3)
            ),
            Some(vec![3]),
        ),
        (
            "a token for an index neither listing reports, through the block listing",
            Listing::SnapshotBlocks,
            format!("pageToken={}", encode_page_token(&child, 2)),
            None,
        ),
        (
            "a token for an index neither listing reports, through the comparison",
            Listing::ChangedBlocks,
            format!("pageToken={}", encode_page_token(&child, 2)),
            None,
        ),
        (
            "a token minted for another snapshot of the lineage, through the block listing",
            Listing::SnapshotBlocks,
            format!("pageToken={}", encode_page_token(&base, 0)),
            None,
        ),
        (
            "a token minted for another snapshot of the lineage, through the comparison",
            Listing::ChangedBlocks,
            format!("pageToken={}", encode_page_token(&base, 0)),
            None,
        ),
        (
            "text no page token is spelled in, through the block listing",
            Listing::SnapshotBlocks,
            "pageToken=notapagetoken".to_string(),
            None,
        ),
        (
            "text no page token is spelled in, through the comparison",
            Listing::ChangedBlocks,
            "pageToken=notapagetoken".to_string(),
            None,
        ),
        (
            "a token spelled like one but carrying no index, through the block listing",
            Listing::SnapshotBlocks,
            format!("pageToken={}", query_safe_token_text(b"nocolon")),
            None,
        ),
        (
            "a token spelled like one but carrying no index, through the comparison",
            Listing::ChangedBlocks,
            format!("pageToken={}", query_safe_token_text(b"nocolon")),
            None,
        ),
    ];

    for (case, listing, query, expected) in rows {
        let reply = page(&router, listing, &base, &child, &query).await;
        match expected {
            Some(indexes) => {
                assert_page(case, listing, &reply, &indexes);
            }
            None => assert_page_token_refused(case, listing, &reply, &child),
        }
    }
}

/// A listing of a snapshot that is not stored never reaches the shared page-token check:
/// the snapshot is resolved first, so a token-carrying request for an unknown snapshot is
/// the not-found refusal rather than a page-token validation.
#[tokio::test]
async fn an_unresolvable_snapshot_is_refused_before_the_page_token_is_judged() {
    let router = router();
    // A well-formed id of the 17 hex digit form this service mints that nothing is stored
    // under.
    let unknown = "snap-0123456789abcdef0";

    for listing in [Listing::SnapshotBlocks, Listing::ChangedBlocks] {
        let case = "a page token for a snapshot that is not stored";
        let reply = page(
            &router,
            listing,
            unknown,
            unknown,
            &format!("pageToken={}", encode_page_token(unknown, 0)),
        )
        .await;
        assert_eq!(
            reply.status,
            404,
            "case {case:?}: expected {listing:?} to answer HTTP 404, got {} with body {}",
            reply.status,
            String::from_utf8_lossy(&reply.body)
        );
        assert_eq!(
            member(&reply, "Reason"),
            "SNAPSHOT_NOT_FOUND",
            "case {case:?}: expected {listing:?} to answer the snapshot not-found reason, got {}",
            reply.json()
        );
    }
}
