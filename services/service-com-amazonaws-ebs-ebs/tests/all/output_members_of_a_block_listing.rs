// SPDX-License-Identifier: CC0-1.0

//! Which members an answer of a block listing reports, over BOTH listings of this service.
//!
//! RULE: every output member is reported from what the model says it describes, so the page
//! a listing happens to report decides only the members that are OF that page.
//!
//! * `ExpiryTime` is "The time when the `BlockToken` expires"
//!   (`smithy_query(query="doc",shape="ListSnapshotBlocksResponse",member="ExpiryTime")`),
//!   so it is of the tokens the answer handed out. Every block an answer reports carries a
//!   token and an answer that reports no block hands out none, so a page that reports
//!   nothing has no expiry instant to report and omits the member.
//! * `VolumeSize` ("The size of the volume in GB") and `BlockSize` ("The size of the blocks
//!   in the snapshot, in bytes") describe the snapshot the listing is of, not the page, and
//!   are the same on every page of one listing. An answer that resolved a snapshot reports
//!   both — a page that reports no block included, which `MaxResults` ("Even if additional
//!   blocks can be retrieved from the snapshot, the request can return less blocks than
//!   MaxResults or an empty array of blocks") makes a page of any listing.
//!
//! Each listing is asserted over a TABLE, whose rows differ along WHY the page reports
//! nothing (a snapshot that holds no block, a start past every index it holds, a comparison
//! that finds no difference), along the declared volume size, along how the block indexes
//! are spread, along whether the request bounds the page with `MaxResults`, and along pages
//! that DO report blocks — so the rule is asserted in both directions rather than on one
//! shape of answer.

use crate::changed_blocks_between_snapshots::{compared, sealed_snapshot};
use crate::snapshot_block_loop::{list_blocks, router, write_snapshot, Reply, BLOCK_SIZE};

/// RULE, asserted on one answer: `array` reports exactly `expected`, the snapshot geometry
/// is reported whatever the page reported, and `ExpiryTime` is reported exactly when the
/// answer handed out at least one block token.
///
/// `volume_size` is the size the snapshot the listing is of was declared with, which is what
/// every answer about it must report.
fn assert_page_members(
    case: &str,
    reply: &Reply,
    array: &str,
    expected: &[i32],
    volume_size: i64,
) {
    let answer = reply.json();
    let reported = answer.get(array).and_then(serde_json::Value::as_array);
    assert!(
        reported.is_some(),
        "case {case:?}: every answer must report {array}, got {answer}"
    );
    let indexes: Vec<i32> = reported
        .map(|blocks| {
            blocks
                .iter()
                .map(|block| {
                    block
                        .get("BlockIndex")
                        .and_then(serde_json::Value::as_i64)
                        .and_then(|index| i32::try_from(index).ok())
                        .unwrap_or(-1)
                })
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        indexes,
        expected.to_vec(),
        "case {case:?}: expected the indexes {expected:?} in {array}, got {answer}"
    );
    assert_eq!(
        answer.get("BlockSize").and_then(serde_json::Value::as_i64),
        Some(i64::from(BLOCK_SIZE)),
        "case {case:?}: every answer must report the block size of the snapshot it is of, \
         got {answer}"
    );
    assert_eq!(
        answer.get("VolumeSize").and_then(serde_json::Value::as_i64),
        Some(volume_size),
        "case {case:?}: every answer must report the volume size of the snapshot it is of, \
         got {answer}"
    );
    if expected.is_empty() {
        assert!(
            answer.get("ExpiryTime").is_none(),
            "case {case:?}: an answer that handed out no block token must omit ExpiryTime, \
             got {answer}"
        );
    } else {
        assert!(
            answer.get("ExpiryTime").is_some(),
            "case {case:?}: an answer that handed out block tokens must report when they \
             expire, got {answer}"
        );
    }
}

/// One row of the `ListSnapshotBlocks` table: the case, which fixture snapshot to list, the
/// query the request carries, and the block indexes the page must report.
type SnapshotBlocksRow<'a> = (&'a str, usize, &'a str, Vec<i32>);

/// The rule over `ListSnapshotBlocks`: the geometry of the snapshot on every answer, the
/// token expiry only where the page handed tokens out.
#[tokio::test]
async fn a_snapshot_listing_reports_the_geometry_whatever_the_page_reported() {
    let router = router();
    let case = "the listed snapshots";

    // Four fixtures, differing along the declared volume size, how many blocks they hold
    // and how those indexes are spread — including one that holds no block at all. Each is
    // `(snapshot id, declared volume size)`.
    let fixtures: Vec<(String, i64)> = vec![
        (
            write_snapshot(&router, case, 8, &[(0, 41), (3, 42), (9, 43), (10, 44)]).await,
            8,
        ),
        (write_snapshot(&router, case, 1, &[(0, 7)]).await, 1),
        (write_snapshot(&router, case, 64, &[(4096, 9)]).await, 64),
        (write_snapshot(&router, case, 2, &[]).await, 2),
    ];

    let rows: Vec<SnapshotBlocksRow<'_>> = vec![
        ("every block of the snapshot", 0, "", vec![0, 3, 9, 10]),
        (
            "a page bounded by the smallest MaxResults the model admits",
            0,
            "maxResults=100",
            vec![0, 3, 9, 10],
        ),
        (
            "starting on the last written index",
            0,
            "startingBlockIndex=10",
            vec![10],
        ),
        (
            "starting past every written index",
            0,
            "startingBlockIndex=11",
            Vec::new(),
        ),
        (
            "the one block of a one-block snapshot",
            1,
            "startingBlockIndex=0",
            vec![0],
        ),
        (
            "starting past the one written index",
            1,
            "startingBlockIndex=1",
            Vec::new(),
        ),
        ("a sparse high index", 2, "", vec![4096]),
        (
            "starting exactly on a sparse high index",
            2,
            "startingBlockIndex=4096",
            vec![4096],
        ),
        (
            "starting past a sparse high index",
            2,
            "startingBlockIndex=4097",
            Vec::new(),
        ),
        ("a snapshot that holds no block", 3, "", Vec::new()),
        (
            "a snapshot that holds no block, started past index 0",
            3,
            "startingBlockIndex=5",
            Vec::new(),
        ),
    ];

    for (case, fixture, query, expected) in rows {
        let (snapshot_id, volume_size) = &fixtures[fixture];
        let listed = list_blocks(&router, snapshot_id, query).await;
        assert_eq!(
            listed.status,
            200,
            "case {case:?}: ListSnapshotBlocks must answer 200, got {} with body {}",
            listed.status,
            String::from_utf8_lossy(&listed.body)
        );
        assert_page_members(case, &listed, "Blocks", &expected, *volume_size);
    }
}

/// One row of the `ListChangedBlocks` table: the case, the first side, the second side, the
/// query the request carries, the volume size both sides were declared with, and the changed
/// indexes the page must report.
type ChangedBlocksRow<'a> = (&'a str, &'a str, &'a str, &'a str, i64, Vec<i32>);

/// The same rule over `ListChangedBlocks`: a comparison that reports no changed block still
/// reports the geometry of the snapshots it compared.
#[tokio::test]
async fn a_comparison_reports_the_geometry_whatever_the_page_reported() {
    let router = router();
    let case = "the compared lineages";

    // One lineage over a 2 GiB volume: the parent holds two blocks, the child inherits both,
    // overwrites one and adds one (so indexes 5 and 9 differ).
    let parent = sealed_snapshot(&router, case, 2, None, &[(0, 1), (5, 2)]).await;
    let child = sealed_snapshot(&router, case, 2, Some(&parent), &[(5, 3), (9, 4)]).await;
    // A second lineage over a 64 GiB volume, with its one changed block at a high index.
    let wide_parent = sealed_snapshot(&router, case, 64, None, &[(4096, 5)]).await;
    let wide_child = sealed_snapshot(&router, case, 64, Some(&wide_parent), &[(9000, 6)]).await;

    let rows: Vec<ChangedBlocksRow<'_>> = vec![
        (
            "every changed block of the lineage",
            parent.as_str(),
            child.as_str(),
            "",
            2,
            vec![5, 9],
        ),
        (
            "starting past every changed index",
            parent.as_str(),
            child.as_str(),
            "startingBlockIndex=10",
            2,
            Vec::new(),
        ),
        (
            "a snapshot compared against itself",
            child.as_str(),
            child.as_str(),
            "",
            2,
            Vec::new(),
        ),
        (
            "the one changed block of a larger volume",
            wide_parent.as_str(),
            wide_child.as_str(),
            "maxResults=100",
            64,
            vec![9000],
        ),
        (
            "starting past the one changed index of a larger volume",
            wide_parent.as_str(),
            wide_child.as_str(),
            "startingBlockIndex=9001",
            64,
            Vec::new(),
        ),
    ];

    for (case, first, second, query, volume_size, expected) in rows {
        let reply = compared(&router, case, first, second, query).await;
        assert_page_members(case, &reply, "ChangedBlocks", &expected, volume_size);
    }
}
