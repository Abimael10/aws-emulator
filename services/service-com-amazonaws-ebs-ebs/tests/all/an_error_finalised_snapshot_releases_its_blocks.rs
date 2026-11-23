// SPDX-License-Identifier: CC0-1.0

//! What this store keeps for a snapshot whose status is `error`.
//!
//! RULE: `error` is a TERMINAL status of the modelled `Status` enum
//! (`smithy_query(query="shape",shape="com.amazonaws.ebs#Status")`) — such a snapshot is
//! resolved by no read ("Seals and completes the snapshot after all of the required blocks
//! of data have been written to it" is what makes a snapshot readable,
//! `smithy_query(query="operation",operation="CompleteSnapshot")`), takes no further block
//! ("The target snapshot must be in the pending state",
//! `smithy_query(query="doc",shape="com.amazonaws.ebs#PutSnapshotBlockRequest",member="SnapshotId")`)
//! and can be moved on by no completion — so nothing can ever reach its content again and
//! the store releases every block it held, whichever fact finalised it: its `Timeout`
//! elapsing while it was still pending, or a `CompleteSnapshot` whose `ChangedBlocksCount`
//! disagrees with the number of blocks written into it. The released bytes stop counting
//! against the store-wide block budget `PutSnapshotBlock` and `StartSnapshot` are gated on
//! (`ComAmazonawsEbsEbsState::stored_block_count`), so content nothing can read never keeps
//! a later, valid write out.
//!
//! The retention policy belongs to the STATUS and not to one route into it, so each row of
//! the table below is driven to `error` through BOTH routes and must leave the same store
//! behind. Rows differ along how many blocks the snapshot holds, at which indexes (adjacent,
//! sparse, one index rewritten), along the count its completion declares against them, and
//! along whether the blocks were written into the snapshot or inherited from a parent — a
//! parent that is a stored snapshot of its own, whose blocks the release must leave alone.
//!
//! The control is the other half of the same rule: a completion that AGREES with the writes
//! made finalises the snapshot to `completed`, which is not terminal-unreadable, and keeps
//! every byte.

use service_com_amazonaws_ebs_ebs::state::service_state;
use smithy_server_runtime::MultiServiceRouter;

use crate::read_path_requires_a_completed_snapshot::assert_not_found;
use crate::snapshot_block_loop::{
    block_bytes, checksum, complete_snapshot, elapse, list_blocks, listed_blocks, member,
    put_block, router_with_engine, start_snapshot, write_snapshot, BLOCK_SIZE,
};

/// The `Timeout`, in minutes, the snapshots taken down the cancellation route are started
/// with: the modelled minimum of `StartSnapshotRequest$Timeout`
/// (`smithy_query(query="shape",shape="com.amazonaws.ebs#StartSnapshotRequest")`, member
/// `Timeout` `range` 10..4320).
const MINIMUM_TIMEOUT_MINUTES: u32 = 10;

/// One row of the table: what the snapshot inherits from a parent, what is written into it,
/// and a `ChangedBlocksCount` that disagrees with those writes.
struct Row {
    case: &'static str,
    volume_size: i64,
    /// `(index, byte seed)` blocks written into a sealed PARENT the snapshot is then started
    /// from, or empty for a snapshot with no parent. Indexes are distinct.
    inherited: &'static [(i32, u8)],
    /// `(index, byte seed)` writes this snapshot makes, in order. An index may repeat: "If
    /// the specified block contains data, the existing data is overwritten"
    /// (`smithy_query(query="operation",operation="PutSnapshotBlock")`).
    writes: &'static [(i32, u8)],
    /// A count that is NOT the number of distinct indexes `writes` wrote, so the completion
    /// declaring it finalises the snapshot to `error`.
    mismatched_count: i32,
}

/// Rows differ along how much the snapshot holds, how those blocks are spread, where they
/// came from, and how far the declared count misses.
const ROWS: &[Row] = &[
    Row {
        case: "one block written, a count one over",
        volume_size: 1,
        inherited: &[],
        writes: &[(0, 11)],
        mismatched_count: 2,
    },
    Row {
        case: "two adjacent blocks, a count of zero",
        volume_size: 1,
        inherited: &[],
        writes: &[(0, 12), (1, 13)],
        mismatched_count: 0,
    },
    Row {
        case: "two sparse blocks, a count one short",
        volume_size: 1,
        inherited: &[],
        writes: &[(0, 14), (3, 15)],
        mismatched_count: 1,
    },
    Row {
        case: "one index rewritten, a count of the writes rather than the indexes",
        volume_size: 1,
        inherited: &[],
        writes: &[(2, 16), (2, 17)],
        mismatched_count: 2,
    },
    Row {
        case: "nothing written at all, a positive count",
        volume_size: 1,
        inherited: &[],
        writes: &[],
        mismatched_count: 3,
    },
    Row {
        case: "an inherited block and one write, a count of both",
        volume_size: 2,
        inherited: &[(0, 18)],
        writes: &[(5, 19)],
        mismatched_count: 2,
    },
    Row {
        case: "only inherited blocks, a count of them",
        volume_size: 2,
        inherited: &[(0, 20), (1, 21)],
        writes: &[],
        mismatched_count: 2,
    },
];

impl Row {
    /// Every block index the snapshot holds once its writes are in, ascending: the inherited
    /// ones and the written ones, each index once.
    fn held_indexes(&self) -> Vec<i32> {
        let mut indexes: Vec<i32> = self
            .inherited
            .iter()
            .chain(self.writes.iter())
            .map(|(index, _)| *index)
            .collect();
        indexes.sort_unstable();
        indexes.dedup();
        indexes
    }

    /// The number of DISTINCT indexes this snapshot's own producer wrote: the count a
    /// completion must declare for the snapshot to complete.
    fn matching_count(&self) -> i32 {
        let mut indexes: Vec<i32> = self.writes.iter().map(|(index, _)| *index).collect();
        indexes.sort_unstable();
        indexes.dedup();
        i32::try_from(indexes.len()).expect("a handful of written indexes")
    }

    /// The blocks the PARENT holds, where the row has one: they belong to another stored
    /// snapshot, so they stay resident whatever happens to the child.
    fn parent_block_count(&self) -> usize {
        self.inherited.len()
    }
}

/// A snapshot under test, with the parent it was started from where the row has one.
struct Started {
    snapshot_id: String,
    parent_snapshot_id: Option<String>,
}

/// Start the snapshot of `row` — from a sealed parent holding its inherited blocks, where it
/// has any — and write its writes. Nothing is completed here: the finalisation is the
/// caller's.
async fn pending_snapshot(
    router: &MultiServiceRouter,
    row: &Row,
    timeout_minutes: Option<u32>,
) -> Started {
    let mut body = serde_json::json!({ "VolumeSize": row.volume_size });
    let parent_snapshot_id = if row.inherited.is_empty() {
        None
    } else {
        // Sealed by `write_snapshot` with the count of the blocks written into it, which is
        // what lets it be named as a `ParentSnapshotId` at all.
        let parent = write_snapshot(router, row.case, row.volume_size, row.inherited).await;
        body["ParentSnapshotId"] = serde_json::json!(parent);
        Some(parent)
    };
    if let Some(minutes) = timeout_minutes {
        body["Timeout"] = serde_json::json!(minutes);
    }

    let started = start_snapshot(router, &body).await;
    assert_eq!(
        started.status, 201,
        "case {:?}: StartSnapshot must answer 201, got {} with body {}",
        row.case,
        started.status,
        String::from_utf8_lossy(&started.body)
    );
    let snapshot_id = member(&started, "SnapshotId");
    assert!(
        snapshot_id.starts_with("snap-"),
        "case {:?}: StartSnapshot must mint a snapshot id, got {snapshot_id:?}",
        row.case
    );

    for (block_index, seed) in row.writes {
        let data = block_bytes(*seed);
        let data_checksum = checksum(&data);
        let written = put_block(
            router,
            &snapshot_id,
            *block_index,
            data,
            BLOCK_SIZE,
            &data_checksum,
        )
        .await;
        assert_eq!(
            written.status, 201,
            "case {:?}: PutSnapshotBlock at index {block_index} must answer 201, got {} with \
             body {}",
            row.case,
            written.status,
            String::from_utf8_lossy(&written.body)
        );
    }

    Started {
        snapshot_id,
        parent_snapshot_id,
    }
}

/// `(the blocks the snapshot `snapshot_id` names holds, the blocks this store holds across
/// every snapshot)`.
///
/// The second is the value the storage guards are measured against, so it is read from the
/// store the handlers read it from rather than inferred: it is what decides whether a later
/// write to an UNRELATED snapshot is admitted at all.
fn resident_blocks(engine: &engine::InMemoryEngine, case: &str, snapshot_id: &str) -> (usize, usize) {
    let state = service_state(engine)
        .unwrap_or_else(|| panic!("case {case:?}: this service's store must be reachable"));
    let store = state.lock();
    let snapshot = store
        .snapshots
        .get(snapshot_id)
        .unwrap_or_else(|| panic!("case {case:?}: {snapshot_id} must still be stored"));
    (snapshot.blocks.len(), store.stored_block_count())
}

/// Assert the snapshot `started` names holds every block the row put into it, and that the
/// store counts exactly those plus the parent's.
fn assert_blocks_resident(engine: &engine::InMemoryEngine, row: &Row, started: &Started) {
    let (held, stored) = resident_blocks(engine, row.case, &started.snapshot_id);
    let expected_held = row.held_indexes().len();
    assert_eq!(
        held, expected_held,
        "case {:?}: expected the snapshot to hold {expected_held} block(s), got {held}",
        row.case
    );
    let expected_stored = expected_held + row.parent_block_count();
    assert_eq!(
        stored, expected_stored,
        "case {:?}: expected the store to hold {expected_stored} block(s), got {stored}",
        row.case
    );
}

/// Assert the snapshot `started` names has released every block it held, and that the only
/// blocks the store still counts are the parent's — so the budget those bytes were holding
/// is free for a later write to another snapshot.
fn assert_blocks_released(engine: &engine::InMemoryEngine, row: &Row, started: &Started) {
    let (held, stored) = resident_blocks(engine, row.case, &started.snapshot_id);
    assert_eq!(
        held, 0,
        "case {:?}: a snapshot finalised to error must hold no block, got {held}",
        row.case
    );
    let expected_stored = row.parent_block_count();
    assert_eq!(
        stored, expected_stored,
        "case {:?}: expected the store to count only the {expected_stored} block(s) of the \
         parent, got {stored}",
        row.case
    );
}

/// Assert the parent, where the row has one, still serves exactly the blocks written into it:
/// the release touches the snapshot that failed, not the lineage it came from.
async fn assert_parent_intact(router: &MultiServiceRouter, row: &Row, started: &Started) {
    let Some(parent_id) = started.parent_snapshot_id.as_deref() else {
        return;
    };
    let listing = list_blocks(router, parent_id, "").await;
    assert_eq!(
        listing.status, 200,
        "case {:?}: the parent must still be listable, got {} with body {}",
        row.case,
        listing.status,
        String::from_utf8_lossy(&listing.body)
    );
    let listed: Vec<i32> = listed_blocks(&listing)
        .iter()
        .map(|(index, _)| *index)
        .collect();
    let mut expected: Vec<i32> = row.inherited.iter().map(|(index, _)| *index).collect();
    expected.sort_unstable();
    assert_eq!(
        listed, expected,
        "case {:?}: the parent must still hold exactly its own indexes, got {listed:?}",
        row.case
    );
}

/// ROUTE 1: a completion whose declared count disagrees with the writes made finalises the
/// snapshot to `error`, and the store releases every block that snapshot held.
#[tokio::test]
async fn a_completion_that_finalises_to_error_releases_the_blocks() {
    for row in ROWS {
        // A fresh router per row: no row can pass on another's state.
        let (router, engine) = router_with_engine();
        let started = pending_snapshot(&router, row, None).await;
        assert_blocks_resident(&engine, row, &started);

        let accepted = complete_snapshot(&router, &started.snapshot_id, row.mismatched_count).await;
        assert_eq!(
            accepted.status, 202,
            "case {:?}: CompleteSnapshot must accept the count with 202, got {} with body {}",
            row.case,
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );

        // The status the completion finalised the record to, read back off it.
        let read_back = complete_snapshot(&router, &started.snapshot_id, row.mismatched_count).await;
        assert_eq!(
            member(&read_back, "Status"),
            "error",
            "case {:?}: a mismatched count must finalise the record to error, got {}",
            row.case,
            read_back.json()
        );

        // Nothing can reach the content again: the read path resolves the snapshot nowhere.
        let listing = list_blocks(&router, &started.snapshot_id, "").await;
        assert_not_found(row.case, &listing, &started.snapshot_id);

        assert_blocks_released(&engine, row, &started);
        assert_parent_intact(&router, row, &started).await;
    }
}

/// ROUTE 2: the same rows taken to the same status by the `Timeout` instead — the store that
/// is left behind is the same one, because the retention policy is the status's and not the
/// route's.
#[tokio::test]
async fn a_cancelled_snapshot_releases_the_same_blocks() {
    for row in ROWS {
        let (router, engine) = router_with_engine();
        let started = pending_snapshot(&router, row, Some(MINIMUM_TIMEOUT_MINUTES)).await;
        assert_blocks_resident(&engine, row, &started);

        // The whole window passes after the last thing that happened to the snapshot. No
        // test can wait one out — `Timeout` is in minutes, with a modelled minimum of 10 —
        // so the stored instants are rewound instead, and the next request observes the
        // clock that rewind created.
        elapse(
            &engine,
            row.case,
            &started.snapshot_id,
            std::time::Duration::from_secs(u64::from(MINIMUM_TIMEOUT_MINUTES) * 60),
        );

        let listing = list_blocks(&router, &started.snapshot_id, "").await;
        assert_not_found(row.case, &listing, &started.snapshot_id);

        assert_blocks_released(&engine, row, &started);
        assert_parent_intact(&router, row, &started).await;
    }
}

/// THE CONTROL: the release is keyed to the terminal `error` status, so a completion that
/// AGREES with the writes made keeps every byte — the snapshot serves exactly the indexes it
/// holds and the store still counts them.
#[tokio::test]
async fn a_completion_that_completes_keeps_the_blocks() {
    for row in ROWS {
        let (router, engine) = router_with_engine();
        let started = pending_snapshot(&router, row, None).await;

        let accepted = complete_snapshot(&router, &started.snapshot_id, row.matching_count()).await;
        assert_eq!(
            accepted.status, 202,
            "case {:?}: CompleteSnapshot must accept the count with 202, got {} with body {}",
            row.case,
            accepted.status,
            String::from_utf8_lossy(&accepted.body)
        );
        let read_back = complete_snapshot(&router, &started.snapshot_id, row.matching_count()).await;
        assert_eq!(
            member(&read_back, "Status"),
            "completed",
            "case {:?}: a matching count must finalise the record to completed, got {}",
            row.case,
            read_back.json()
        );

        let listing = list_blocks(&router, &started.snapshot_id, "").await;
        assert_eq!(
            listing.status, 200,
            "case {:?}: a completed snapshot must be listable, got {} with body {}",
            row.case,
            listing.status,
            String::from_utf8_lossy(&listing.body)
        );
        let listed: Vec<i32> = listed_blocks(&listing)
            .iter()
            .map(|(index, _)| *index)
            .collect();
        assert_eq!(
            listed,
            row.held_indexes(),
            "case {:?}: a completed snapshot must serve every index it holds, got {listed:?}",
            row.case
        );

        assert_blocks_resident(&engine, row, &started);
        assert_parent_intact(&router, row, &started).await;
    }
}
