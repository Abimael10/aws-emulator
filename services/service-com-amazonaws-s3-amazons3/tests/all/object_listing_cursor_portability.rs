// SPDX-License-Identifier: CC0-1.0

//! A `ListObjectsV2` continuation token issued over ONE bucket's listing, handed back
//! alongside the listing's own SELECTION parameters on ANOTHER bucket.
//!
//! RULE under test: the token names only the position the next page starts at — it is
//! "obfuscated and is not a real key" (`ListObjectsV2Output$NextContinuationToken`) and the
//! resource the listing is taken over is the `Bucket` the request addresses — so the page is
//! computed over the ADDRESSED bucket's keys at or after that position, under that request's
//! own selection: `Prefix` keeps the keys that "begin with the indicated prefix", `Delimiter`
//! rolls the rest of them up into `CommonPrefixes` where "all of the keys that roll up into a
//! common prefix count as a single return", and `MaxKeys` bounds the entries and leaves the
//! rest for a token of this bucket's own.
//!
//! [`super::listing_echoed_members`] owns the base rule (a token resumes whichever bucket is
//! addressed, and only a value this service minted names a page at all); this suite is the
//! INTERACTION with the three parameters that decide which entries a page carries, so a
//! foreign cursor is seen to be a position in the key order rather than a page the service
//! kept on the side. The rows span the selections one listing can state: none, a prefix that
//! selects some of the keys past the cursor, a prefix that selects one, a prefix that selects
//! none, a delimiter alone, a prefix and a delimiter together, and a bounded page.
//!
//! Every call goes over the wire through the harnesses the other object suites own, so what
//! is pinned is the wire document.

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::router;
use super::continuation_token_alphabet::handed_back_in_query;
use super::object_and_tagging_roundtrip::{bucket, text};
use super::object_storage::{list_objects_v2, listing_of, store_keys};

/// The keys the bucket that ISSUES the cursor holds. A page of one key stops before the
/// second, so the position the token names is `notes/may.txt`.
const ISSUER_KEYS: &[&str] = &["notes/april.txt", "notes/may.txt", "plain.txt"];

/// The keys the ADDRESSED bucket holds: two that sort before the issued position, and four
/// at or after it under two prefixes, so a selection has something to keep and something to
/// drop on either side of the cursor.
const ADDRESSED_KEYS: &[&str] = &[
    "aaa.txt",
    "notes/april.txt",
    "notes/may.txt",
    "notes/zulu.txt",
    "photos/june.jpg",
    "plain.txt",
];

/// One selection stated beside the foreign cursor, and the page it must answer.
struct Selected {
    case: &'static str,
    /// The query the request carries beside the `continuation-token`.
    query: &'static str,
    /// The `Contents` keys the page must report, in listing order.
    contents: &'static [&'static str],
    /// The `CommonPrefixes` it must report, in listing order.
    prefixes: &'static [&'static str],
}

/// RULE: a foreign cursor fixes WHERE the page starts, and the request's own selection
/// decides which of the addressed bucket's entries from there on it carries.
#[tokio::test]
async fn a_foreign_cursor_is_a_position_the_requests_own_selection_is_applied_from() {
    let router = router();
    let issuer = "cursor-selection-issuer";
    let addressed = "cursor-selection-addressed";
    bucket(&router, issuer).await;
    store_keys(&router, issuer, ISSUER_KEYS).await;
    bucket(&router, addressed).await;
    store_keys(&router, addressed, ADDRESSED_KEYS).await;

    let token = issued_cursor(&router, issuer).await;

    let rows: &[Selected] = &[
        // The CONTROL row, which the suite that owns the base rule also covers: with no
        // selection stated the page is every entry of the addressed bucket from the position
        // on, so each row below differs from this one only by what its selection drops.
        Selected {
            case: "no selection at all",
            query: "",
            contents: &[
                "notes/may.txt",
                "notes/zulu.txt",
                "photos/june.jpg",
                "plain.txt",
            ],
            prefixes: &[],
        },
        Selected {
            case: "a prefix selecting keys on both sides of the position",
            query: "&prefix=notes/",
            contents: &["notes/may.txt", "notes/zulu.txt"],
            prefixes: &[],
        },
        Selected {
            case: "a prefix selecting one key past the position",
            query: "&prefix=photos/",
            contents: &["photos/june.jpg"],
            prefixes: &[],
        },
        Selected {
            case: "a prefix selecting only keys before the position",
            query: "&prefix=aaa",
            contents: &[],
            prefixes: &[],
        },
        Selected {
            case: "a delimiter alone, rolling the keys past the position up",
            query: "&delimiter=/",
            contents: &["plain.txt"],
            prefixes: &["notes/", "photos/"],
        },
        Selected {
            case: "a prefix and a delimiter together",
            query: "&prefix=notes/&delimiter=/",
            contents: &["notes/may.txt", "notes/zulu.txt"],
            prefixes: &[],
        },
    ];

    for row in rows {
        let answer = list_objects_v2(
            &router,
            addressed,
            &format!(
                "{}&continuation-token={}",
                row.query,
                handed_back_in_query(&token)
            ),
        )
        .await;
        assert_eq!(
            answer.status,
            200,
            "case {:?}: the cursor resumes the addressed bucket, got {} with body {:?}",
            row.case,
            answer.status,
            text(&answer)
        );
        let page = listing_of(&answer);
        assert_eq!(
            page.keys(),
            row.contents
                .iter()
                .map(|key| (*key).to_string())
                .collect::<Vec<String>>(),
            "case {:?}: expected the entries {:?}, got {:?}",
            row.case,
            row.contents,
            page.keys()
        );
        assert_eq!(
            page.common_prefixes,
            row.prefixes
                .iter()
                .map(|prefix| (*prefix).to_string())
                .collect::<Vec<String>>(),
            "case {:?}: expected the rolled-up prefixes {:?}, got {:?}",
            row.case,
            row.prefixes,
            page.common_prefixes
        );
        // "All of the keys that roll up into a common prefix count as a single return", and
        // every entry from the position on fitted on this page.
        assert_eq!(
            (
                page.field("KeyCount"),
                page.field("IsTruncated"),
                page.field("NextContinuationToken")
            ),
            (
                (row.contents.len() + row.prefixes.len()).to_string(),
                "false".to_string(),
                String::new()
            ),
            "case {:?}: expected an untruncated page counting {} entries and handing back no \
             token, got {:?}",
            row.case,
            row.contents.len() + row.prefixes.len(),
            text(&answer)
        );
    }
}

/// RULE, bounded: a page taken from a foreign cursor is truncated when the ADDRESSED bucket
/// has more entries past the cursor than the page carries, and the token it hands back
/// continues that bucket — so the cursor is a position and the bound is the request's.
#[tokio::test]
async fn a_bounded_page_from_a_foreign_cursor_hands_back_a_cursor_of_the_addressed_bucket() {
    let router = router();
    let issuer = "cursor-bounded-issuer";
    let addressed = "cursor-bounded-addressed";
    bucket(&router, issuer).await;
    store_keys(&router, issuer, ISSUER_KEYS).await;
    bucket(&router, addressed).await;
    store_keys(&router, addressed, ADDRESSED_KEYS).await;

    let token = issued_cursor(&router, issuer).await;

    let bounded = list_objects_v2(
        &router,
        addressed,
        &format!(
            "&max-keys=2&continuation-token={}",
            handed_back_in_query(&token)
        ),
    )
    .await;
    assert_eq!(
        bounded.status,
        200,
        "a bounded resume is served, got {} with body {:?}",
        bounded.status,
        text(&bounded)
    );
    let page = listing_of(&bounded);
    assert_eq!(
        page.keys(),
        vec!["notes/may.txt".to_string(), "notes/zulu.txt".to_string()],
        "expected the first two entries past the position, got {:?}",
        page.keys()
    );
    assert_eq!(
        page.field("IsTruncated"),
        "true",
        "a page that left entries behind is truncated, got {:?}",
        text(&bounded)
    );
    // "Sets the maximum number of keys returned in the response": the number the REQUEST
    // asked for, whatever the cursor it resumed from.
    assert_eq!(
        page.field("MaxKeys"),
        "2",
        "expected the page size the request asked for, got {:?}",
        page.field("MaxKeys")
    );

    let next = page.field("NextContinuationToken");
    assert!(
        !next.is_empty(),
        "a truncated page must hand back a token, got {:?}",
        text(&bounded)
    );
    assert_ne!(
        next, token,
        "the token a truncated page hands back names ITS last position, not the one it \
         resumed from"
    );
    let rest = list_objects_v2(
        &router,
        addressed,
        &format!("&continuation-token={}", handed_back_in_query(&next)),
    )
    .await;
    assert_eq!(
        rest.status,
        200,
        "the page's own token continues the addressed bucket, got {} with body {:?}",
        rest.status,
        text(&rest)
    );
    assert_eq!(
        listing_of(&rest).keys(),
        vec!["photos/june.jpg".to_string(), "plain.txt".to_string()],
        "expected the entries the bounded page left behind, got {:?}",
        listing_of(&rest).keys()
    );
}

/// The `NextContinuationToken` a one-entry page of `bucket_name` hands back: the position
/// every case above hands to the other bucket's listing.
async fn issued_cursor(router: &MultiServiceRouter, bucket_name: &str) -> String {
    let first = list_objects_v2(router, bucket_name, "&max-keys=1").await;
    assert_eq!(
        first.status,
        200,
        "case {bucket_name:?}: a bounded listing is served, got {} with body {:?}",
        first.status,
        text(&first)
    );
    let token = listing_of(&first).field("NextContinuationToken");
    assert!(
        !token.is_empty(),
        "case {bucket_name:?}: a truncated page must hand back a token, got {:?}",
        text(&first)
    );
    token
}
