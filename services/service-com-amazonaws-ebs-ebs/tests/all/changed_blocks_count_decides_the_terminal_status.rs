// SPDX-License-Identifier: CC0-1.0

//! What `CompleteSnapshot`'s `ChangedBlocksCount` decides about the snapshot it completes.
//!
//! Every test drives real HTTP through the same `MultiServiceRouter` the production router
//! uses, over the harness `snapshot_block_loop` owns.
//!
//! RULE 1: `ChangedBlocksCount` is the producer's own accounting of its writes ("The number
//! of blocks that were written to the snapshot",
//! `smithy_query(query="doc",shape="com.amazonaws.ebs#CompleteSnapshotRequest",member="ChangedBlocksCount")`),
//! constrained only by the model's `range` min 0
//! (`smithy_query(query="operation",operation="CompleteSnapshot")`). It is never refused
//! synchronously — every modelled count is accepted with the operation's `@http(code: 202)`
//! answer carrying the status the completion was applied to — and it is what the completion
//! CHECKS: the snapshot finalises to `completed` only when the declared count is the number
//! of distinct block indexes that were written into it, and otherwise to `error` (both
//! members of the modelled `Status` enum,
//! `smithy_query(query="shape",shape="com.amazonaws.ebs#Status")`), where no read resolves
//! it.
//!
//! RULE 2: once a completion has been accepted, no further block is written ("You cannot
//! write new blocks to a snapshot after it has been completed", `CompleteSnapshot`), and the
//! refusal is the fixed sentence for that class — it interpolates neither the snapshot id
//! nor the block the write carried — whichever status the completion finalised the snapshot
//! to, and whatever the snapshot already holds.
//!
//! Each rule is asserted over a TABLE: rows differ along the declared count against the
//! writes made (equal, zero, one short, one over, far over, and the largest count the header
//! carries), along how those writes were spread (none, adjacent indexes, sparse indexes, one
//! index rewritten), and along whether the snapshot inherited blocks from a parent — which
//! were written to that parent and so count towards none of its own.

use crate::snapshot_block_loop::{
    block_bytes, checksum, complete_snapshot, list_blocks, listed_blocks, member, put_block,
    router, start_snapshot, write_snapshot, Reply, BLOCK_SIZE,
};

/// The fixed sentence the service answers a write to an already-completed snapshot with,
/// mirroring `crate::state::SNAPSHOT_ALREADY_COMPLETED_MESSAGE`.
const ALREADY_COMPLETED: &str = "Snapshot is already completed";

/// One row of the table: what the snapshot inherits, what is written into it, the count its
/// completion declares, and the status that completion must finalise it to.
struct Row {
    case: &'static str,
    volume_size: i64,
    /// `(index, byte seed)` blocks written into a PARENT the snapshot is then started from,
    /// or empty for a snapshot with no parent.
    inherited: &'static [(i32, u8)],
    /// `(index, byte seed)` writes this snapshot makes, in order. An index may repeat: "If
    /// the specified block contains data, the existing data is overwritten".
    writes: &'static [(i32, u8)],
    declared_count: i32,
    /// One of the modelled `Status` values the completion finalises the record to.
    finalised: &'static str,
}

/// Rows differ along the declared count against the writes made, along how the writes are
/// spread, and along whether the snapshot inherited any block.
const ROWS: &[Row] = &[
    Row {
        case: "a count that matches two writes",
        volume_size: 1,
        inherited: &[],
        writes: &[(0, 81), (1, 82)],
        declared_count: 2,
        finalised: "completed",
    },
    Row {
        case: "zero against a snapshot nothing was written to",
        volume_size: 1,
        inherited: &[],
        writes: &[],
        declared_count: 0,
        finalised: "completed",
    },
    Row {
        case: "a count of one against one index rewritten three times",
        volume_size: 1,
        inherited: &[],
        writes: &[(0, 83), (0, 84), (0, 85)],
        declared_count: 1,
        finalised: "completed",
    },
    Row {
        case: "a count of three against one index rewritten three times",
        volume_size: 1,
        inherited: &[],
        writes: &[(0, 83), (0, 84), (0, 85)],
        declared_count: 3,
        finalised: "error",
    },
    Row {
        case: "zero against two writes",
        volume_size: 1,
        inherited: &[],
        writes: &[(0, 86), (1, 87)],
        declared_count: 0,
        finalised: "error",
    },
    Row {
        case: "a count one short of two sparse writes",
        volume_size: 1,
        inherited: &[],
        writes: &[(0, 88), (3, 89)],
        declared_count: 1,
        finalised: "error",
    },
    Row {
        case: "a count one over a single write",
        volume_size: 1,
        inherited: &[],
        writes: &[(0, 90)],
        declared_count: 2,
        finalised: "error",
    },
    Row {
        case: "a count far over a single write",
        volume_size: 1,
        inherited: &[],
        writes: &[(0, 91)],
        declared_count: 5,
        finalised: "error",
    },
    Row {
        case: "the largest count the header carries against a single write",
        volume_size: 1,
        inherited: &[],
        writes: &[(2, 92)],
        declared_count: i32::MAX,
        finalised: "error",
    },
    Row {
        case: "a count against a snapshot nothing was written to",
        volume_size: 1,
        inherited: &[],
        writes: &[],
        declared_count: 4,
        finalised: "error",
    },
    Row {
        case: "zero against a snapshot that only inherited blocks",
        volume_size: 2,
        inherited: &[(0, 93), (1, 94)],
        writes: &[],
        declared_count: 0,
        finalised: "completed",
    },
    Row {
        case: "a count of the blocks a snapshot only inherited",
        volume_size: 2,
        inherited: &[(0, 95), (1, 96)],
        writes: &[],
        declared_count: 2,
        finalised: "error",
    },
    Row {
        case: "a count of the one write a snapshot with an inherited block made",
        volume_size: 2,
        inherited: &[(0, 97)],
        writes: &[(5, 98)],
        declared_count: 1,
        finalised: "completed",
    },
    Row {
        case: "a count of the one write that replaced an inherited block",
        volume_size: 2,
        inherited: &[(0, 99)],
        writes: &[(0, 100)],
        declared_count: 1,
        finalised: "completed",
    },
    Row {
        case: "a count of both the inherited and the written block",
        volume_size: 2,
        inherited: &[(0, 101)],
        writes: &[(5, 102)],
        declared_count: 2,
        finalised: "error",
    },
];

impl Row {
    /// Every block index this snapshot holds once its writes are in, ascending: what a
    /// listing of it must report where the read path resolves it at all.
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
}

/// Start the snapshot of `row` — from a sealed parent holding its inherited blocks, where it
/// has any — write its writes, and answer its id. Nothing is completed here: the completion
/// under test is the caller's.
async fn pending_snapshot(router: &smithy_server_runtime::MultiServiceRouter, row: &Row) -> String {
    let mut body = serde_json::json!({ "VolumeSize": row.volume_size });
    if !row.inherited.is_empty() {
        // The parent is sealed by `write_snapshot` with the count of the blocks written into
        // it, which is what lets it be used as a `ParentSnapshotId` at all.
        let parent = write_snapshot(router, row.case, row.volume_size, row.inherited).await;
        body["ParentSnapshotId"] = serde_json::json!(parent);
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
    snapshot_id
}

/// Assert `reply` is the 202 that accepts a completion: the operation's modelled success
/// status, carrying the status the record was in when the request arrived, and no refusal.
fn assert_accepted(case: &str, reply: &Reply) {
    assert_eq!(
        reply.status, 202,
        "case {case:?}: CompleteSnapshot must accept any modelled ChangedBlocksCount with 202, \
         got {} with body {}",
        reply.status,
        String::from_utf8_lossy(&reply.body)
    );
    assert!(
        reply.error_code().is_empty(),
        "case {case:?}: an accepted completion carries no error code, got {:?}",
        reply.error_code()
    );
    assert_eq!(
        member(reply, "Status"),
        "pending",
        "case {case:?}: the 202 must acknowledge the status the completion was applied to, got {}",
        reply.json()
    );
}

/// RULE 1: the declared count is never a synchronous refusal, and it is what decides the
/// status the completion finalises the record to — which a later call reads back, and which
/// the read path then either serves or resolves nowhere.
#[tokio::test]
async fn the_declared_count_decides_the_status_the_completion_finalises_to() {
    for row in ROWS {
        // A fresh router per row: no row can pass on another's state.
        let router = router();
        let snapshot_id = pending_snapshot(&router, row).await;

        let accepted = complete_snapshot(&router, &snapshot_id, row.declared_count).await;
        assert_accepted(row.case, &accepted);

        // The finalised status, read back off the record through a repeat of the call: the
        // completion was applied by the accepted one, so nothing is left outstanding.
        let read_back = complete_snapshot(&router, &snapshot_id, row.declared_count).await;
        assert_eq!(
            read_back.status, 202,
            "case {:?}: reading the status back must answer 202, got {} with body {}",
            row.case,
            read_back.status,
            String::from_utf8_lossy(&read_back.body)
        );
        assert_eq!(
            member(&read_back, "Status"),
            row.finalised,
            "case {:?}: expected the record to finalise to {:?}, got {}",
            row.case,
            row.finalised,
            read_back.json()
        );

        // The read path follows that status: a snapshot that completed serves exactly the
        // blocks it holds, and one that finalised to `error` resolves nowhere.
        let listing = list_blocks(&router, &snapshot_id, "").await;
        if row.finalised == "completed" {
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
                "case {:?}: the listing must hold exactly the indexes the snapshot holds, got \
                 {listed:?}",
                row.case
            );
        } else {
            assert_eq!(
                listing.status, 404,
                "case {:?}: a snapshot that finalised to error must resolve nowhere, got {} with \
                 body {}",
                row.case,
                listing.status,
                String::from_utf8_lossy(&listing.body)
            );
            assert!(
                listing.error_code().ends_with("ResourceNotFoundException"),
                "case {:?}: expected the ResourceNotFoundException error code, got {:?}",
                row.case,
                listing.error_code()
            );
        }
    }
}

/// RULE 2: whichever status the completion finalised the snapshot to, a block written
/// afterwards is refused with the modelled `ValidationException` carrying the FIXED sentence
/// for an already-completed snapshot — the same bytes for every row, interpolating neither
/// the snapshot id nor the index the write named — and the refusal changes nothing about
/// what the snapshot already holds.
#[tokio::test]
async fn a_write_after_a_completion_is_refused_with_the_fixed_sentence() {
    for row in ROWS {
        let router = router();
        let snapshot_id = pending_snapshot(&router, row).await;
        let accepted = complete_snapshot(&router, &snapshot_id, row.declared_count).await;
        assert_accepted(row.case, &accepted);

        // The indexes a late write names: one the snapshot already holds where it holds
        // any, and two it does not.
        let mut late_indexes: Vec<i32> = vec![1, 7];
        if let Some(held) = row.held_indexes().first() {
            late_indexes.push(*held);
        }

        for block_index in late_indexes {
            let case = format!("{}, a late write at index {block_index}", row.case);
            let data = block_bytes(201);
            let refused = put_block(
                &router,
                &snapshot_id,
                block_index,
                data.clone(),
                BLOCK_SIZE,
                &checksum(&data),
            )
            .await;
            assert_eq!(
                refused.status, 400,
                "case {case:?}: a write after a completion must be refused 400, got {} with body \
                 {}",
                refused.status,
                String::from_utf8_lossy(&refused.body)
            );
            assert!(
                refused.error_code().ends_with("ValidationException"),
                "case {case:?}: expected the ValidationException error code, got {:?}",
                refused.error_code()
            );
            assert_eq!(
                member(&refused, "Reason"),
                "INVALID_SNAPSHOT_ID",
                "case {case:?}: the refusal must name the snapshot as the rejected input, got {}",
                refused.json()
            );
            let message = member(&refused, "Message");
            assert_eq!(
                message, ALREADY_COMPLETED,
                "case {case:?}: the refusal's sentence is fixed text, got {message:?}"
            );
            assert!(
                !message.contains("snap-") && !message.contains(&block_index.to_string()),
                "case {case:?}: fixed text interpolates no request value, got {message:?}"
            );
        }

        // The refusals left the snapshot exactly as the completion finalised it: a
        // completed one still serves every block written before it, under the indexes it
        // held.
        if row.finalised == "completed" {
            let listing = list_blocks(&router, &snapshot_id, "").await;
            let listed: Vec<i32> = listed_blocks(&listing)
                .iter()
                .map(|(index, _)| *index)
                .collect();
            assert_eq!(
                listed,
                row.held_indexes(),
                "case {:?}: a refused late write must change nothing about what is held, got \
                 {listed:?}",
                row.case
            );
        }
    }
}
