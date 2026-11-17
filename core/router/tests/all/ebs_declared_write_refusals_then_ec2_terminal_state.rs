// SPDX-License-Identifier: CC0-1.0

//! The EBS direct-API write-then-wait-then-read loop a client drives THROUGH a write whose
//! DECLARED values the service does not accept, and the terminal state the completion it then
//! takes converges to as Amazon EC2 reports it.
//!
//! RULE: a `PutSnapshotBlock` whose declared values the service does not accept — a
//! `DataLength` other than the one supported block size ("Currently, the only supported size
//! is 524288 bytes",
//! `smithy_query(query="doc",shape="com.amazonaws.ebs#PutSnapshotBlockRequest",member="DataLength")`),
//! or a `BlockIndex` at or past the extent of the snapshot's own volume ("a logical index in
//! units of 512 KiB blocks", same query, member `BlockIndex`, so a volume of N GiB addresses
//! `N * 2048` indexes) — is refused synchronously and stores NOTHING, so the blocks a snapshot
//! holds are exactly the ones its ACCEPTED writes built. The EBS direct APIs model no read of
//! a snapshot's state, so the lifecycle that follows is observed through
//! `ec2:DescribeSnapshots`: the completion is accepted with its modelled 202 whatever count it
//! declares, and within a BOUNDED wait the snapshot reaches a terminal state — `completed`
//! with `Progress` `100%` when the declared `ChangedBlocksCount` is the number of distinct
//! indexes the snapshot's own accepted writes covered, and `error` otherwise (both members of
//! EC2's `SnapshotState`) — after which `ebs:ListSnapshotBlocks` serves exactly those indexes
//! for a snapshot that completed and resolves nowhere (`ResourceNotFoundException`, 404) for
//! one in `error`.
//!
//! This suite lives beside the router because the rule spans two services: the refusals and
//! the listing are the EBS crate's, the state the write loop converges to is only readable
//! through EC2, and the EBS crate's own tests see no EC2 at all. Both services are mounted on
//! ONE engine, as `core/router/src/lib.rs` mounts them, through the wire helpers the sibling
//! suite `ebs_refused_block_write_then_ec2_snapshot_completion` exports.
//!
//! The rows differ along what the rule generalises over: WHICH class the refused write fails
//! (an unsupported declared length — zero, one byte, a round value, half a block, one over a
//! block, two blocks — and an index at the extent, one past it, and far past it), the declared
//! volume size that moves that extent, how many refused writes one snapshot takes and where
//! they sit relative to the accepted ones (before, between, after, and at an index later
//! written correctly), how many indexes are accepted and in which order (including one index
//! written twice, which covers one index), and the count the completion declares against them
//! (equal, one over, far over, and zero).

use std::future::Future;
use std::time::{Duration, Instant};

use smithy_server_runtime::MultiServiceRouter;

use crate::ebs_refused_block_write_then_ec2_snapshot_completion::{
    block_bytes, checksum, complete_snapshot, describe, ebs, emulator, get_block, list_blocks,
    listed_blocks, member, payload_for, start_snapshot, Reply, BLOCK_SIZE, CHECKSUM_ALGORITHM,
    EBS_NOT_FOUND_CODE, PROGRESS_COMPLETED, STATE_COMPLETED, STATE_PENDING,
};

/// A snapshot whose content is not the content its producer claimed: the `error` member of
/// EC2's `SnapshotState`, which is the same word as the EBS direct APIs' own `Status` value.
const STATE_ERROR: &str = "error";

/// Block indexes one GiB of volume addresses: one GiB divided by the `524288`-byte block
/// `BlockIndex` counts in.
const BLOCKS_PER_GIB: i32 = (1024 * 1024 * 1024) / BLOCK_SIZE;

/// How long a poll for the terminal state looks before it gives up. Generous: the wait under
/// test is that the state converges AT ALL, not how quickly it does.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll waits between two looks.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// The `ValidationException` `Reason` the service rejects an unsupported declared length with.
const REASON_INVALID_PARAMETER_VALUE: &str = "INVALID_PARAMETER_VALUE";

/// The `ValidationException` `Reason` the service rejects an out-of-extent index with.
const REASON_INVALID_BLOCK: &str = "INVALID_BLOCK";

/// The template the service refuses an unsupported `DataLength` with: the rejected length and
/// the one length it accepts, and no other quantity. Written out here so the wording is
/// pinned by this suite rather than by the code under test.
fn invalid_length_message(data_length: i32) -> String {
    format!("Block data length {data_length} is not valid, expected {BLOCK_SIZE}")
}

/// The template the service refuses an out-of-extent `BlockIndex` with: the rejected index and
/// nothing else — not the volume, not the extent derived from it.
fn beyond_extent_message(block_index: i32) -> String {
    format!("Block index {block_index} is beyond the volume size extent")
}

/// One write whose DECLARED values the service does not accept.
struct RefusedWrite {
    /// The index the request names: inside the volume's extent when the declared LENGTH is
    /// what fails, at or past the extent when the index itself is.
    index: i32,
    /// The length the request declares for its payload. The payload is always a full,
    /// correctly checksummed block, so this is the declared value and not the bytes.
    declared_length: i32,
}

impl RefusedWrite {
    /// The `(Reason, Message)` the service must refuse this write with, decided by which of
    /// the two declared values is the one it does not accept.
    fn refusal(&self, extent: i32) -> (&'static str, String) {
        if self.index >= extent {
            (REASON_INVALID_BLOCK, beyond_extent_message(self.index))
        } else {
            (
                REASON_INVALID_PARAMETER_VALUE,
                invalid_length_message(self.declared_length),
            )
        }
    }
}

/// One row: the snapshot's declared size, the writes its declared values get refused for, the
/// indexes whose writes are accepted in the order the client makes them, the count the
/// completion declares, and the terminal state that count must drive the snapshot to.
struct Row {
    case: &'static str,
    volume_size: i64,
    refused: &'static [RefusedWrite],
    written: &'static [i32],
    declared_count: usize,
    /// One of EC2's `SnapshotState` values: `completed` or `error`.
    finalised: &'static str,
}

/// The rows, differing along the refusal class, the volume size, where the refused writes sit,
/// which indexes are accepted and in which order, and the declared count.
const ROWS: &[Row] = &[
    Row {
        case: "a declared length of 1000 at index 0, then the write at index 1",
        volume_size: 1,
        refused: &[RefusedWrite {
            index: 0,
            declared_length: 1000,
        }],
        written: &[1],
        declared_count: 1,
        finalised: STATE_COMPLETED,
    },
    Row {
        case: "a half-block declared length at the index that is then written correctly",
        volume_size: 1,
        refused: &[RefusedWrite {
            index: 0,
            declared_length: BLOCK_SIZE / 2,
        }],
        written: &[0],
        declared_count: 1,
        finalised: STATE_COMPLETED,
    },
    Row {
        case: "a declared length of zero between two accepted writes",
        volume_size: 2,
        refused: &[RefusedWrite {
            index: 1,
            declared_length: 0,
        }],
        written: &[0, 2],
        declared_count: 2,
        finalised: STATE_COMPLETED,
    },
    Row {
        case: "an index far past a 1 GiB volume's extent, then the write at index 0",
        volume_size: 1,
        refused: &[RefusedWrite {
            index: 99_999,
            declared_length: BLOCK_SIZE,
        }],
        written: &[0],
        declared_count: 1,
        finalised: STATE_COMPLETED,
    },
    Row {
        case: "the extent itself and the index past it, against the last index addressed",
        volume_size: 2,
        refused: &[
            RefusedWrite {
                index: 2 * BLOCKS_PER_GIB,
                declared_length: BLOCK_SIZE,
            },
            RefusedWrite {
                index: 2 * BLOCKS_PER_GIB + 1,
                declared_length: BLOCK_SIZE,
            },
        ],
        written: &[2 * BLOCKS_PER_GIB - 1, 0],
        declared_count: 2,
        finalised: STATE_COMPLETED,
    },
    Row {
        case: "both refusal classes against one snapshot",
        volume_size: 1,
        refused: &[
            RefusedWrite {
                index: 0,
                declared_length: 1,
            },
            RefusedWrite {
                index: BLOCKS_PER_GIB,
                declared_length: BLOCK_SIZE,
            },
        ],
        written: &[3, 4],
        declared_count: 2,
        finalised: STATE_COMPLETED,
    },
    Row {
        case: "one index written twice after a two-block declared length",
        volume_size: 1,
        refused: &[RefusedWrite {
            index: 1,
            declared_length: BLOCK_SIZE * 2,
        }],
        written: &[1, 1],
        declared_count: 1,
        finalised: STATE_COMPLETED,
    },
    Row {
        case: "a snapshot whose only write was refused, completed declaring none",
        volume_size: 1,
        refused: &[RefusedWrite {
            index: 0,
            declared_length: 1000,
        }],
        written: &[],
        declared_count: 0,
        finalised: STATE_COMPLETED,
    },
    Row {
        case: "a count one over the accepted write, after a length refusal",
        volume_size: 1,
        refused: &[RefusedWrite {
            index: 5,
            declared_length: BLOCK_SIZE + 1,
        }],
        written: &[5],
        declared_count: 2,
        finalised: STATE_ERROR,
    },
    Row {
        case: "a count far over the accepted write, after an extent refusal",
        volume_size: 1,
        refused: &[RefusedWrite {
            index: BLOCKS_PER_GIB,
            declared_length: BLOCK_SIZE,
        }],
        written: &[0],
        declared_count: 5,
        finalised: STATE_ERROR,
    },
    Row {
        case: "a count of zero against one accepted write",
        volume_size: 1,
        refused: &[],
        written: &[0],
        declared_count: 0,
        finalised: STATE_ERROR,
    },
    Row {
        case: "a count of the refused write as well as the accepted one",
        volume_size: 2,
        refused: &[RefusedWrite {
            index: 7,
            declared_length: 1000,
        }],
        written: &[7, 8],
        declared_count: 3,
        finalised: STATE_ERROR,
    },
];

impl Row {
    /// The block indexes this snapshot addresses: `volume_size * 2048`.
    fn extent(&self) -> i32 {
        i32::try_from(self.volume_size).expect("a row's volume size fits an i32") * BLOCKS_PER_GIB
    }

    /// The indexes the accepted writes covered, ascending and each counted once: what a
    /// listing of the completed snapshot must report, and what the declared count is checked
    /// against.
    fn accepted_indexes(&self) -> Vec<i32> {
        let mut indexes = self.written.to_vec();
        indexes.sort_unstable();
        indexes.dedup();
        indexes
    }
}

/// The `Reason` member of a modelled EBS refusal body, or the empty string when it carries
/// none.
fn refusal_reason(reply: &Reply) -> String {
    reply
        .json()
        .get("Reason")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `ebs:PutSnapshotBlock` of a full, correctly checksummed block at `index`, DECLARING
/// `declared_length` as its length — so what the request declares about itself, and not the
/// bytes it carries, is what the service judges.
async fn put_block_declaring(
    router: &MultiServiceRouter,
    snapshot_id: &str,
    index: i32,
    payload: Vec<u8>,
    declared_length: i32,
) -> Reply {
    let supplied = checksum(&payload);
    ebs(
        router,
        "PUT",
        &format!("/snapshots/{snapshot_id}/blocks/{index}"),
        &[
            ("content-type", "application/octet-stream".to_string()),
            ("x-amz-Data-Length", declared_length.to_string()),
            ("x-amz-Checksum", supplied),
            ("x-amz-Checksum-Algorithm", CHECKSUM_ALGORITHM.to_string()),
        ],
        payload,
    )
    .await
}

/// Look again until `probe` answers something, or [`POLL_DEADLINE`] has passed: the way this
/// suite waits for the CONDITION the write loop converges on rather than for a span.
async fn poll_until<T, P, F>(case: &str, waiting_for: &str, mut probe: P) -> T
where
    P: FnMut() -> F,
    F: Future<Output = Option<T>>,
{
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        if let Some(settled) = probe().await {
            return settled;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: waited for {waiting_for} and it never happened"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Poll `ec2:DescribeSnapshots` until the snapshot reports a terminal state, answering
/// `(that state, the answer that reported it)`.
async fn terminal_state(
    router: &MultiServiceRouter,
    case: &str,
    snapshot_id: &str,
) -> (String, String) {
    poll_until(case, "the snapshot to reach a terminal state", || async {
        let (status, body) = describe(router, snapshot_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ec2:DescribeSnapshots must report the completed snapshot, got \
             {status} with {body}"
        );
        let state = member(&body, "status");
        (state == STATE_COMPLETED || state == STATE_ERROR).then_some((state, body))
    })
    .await
}

/// RULE: the refused writes store nothing, the completion is accepted, the snapshot converges
/// within a bounded wait to the state its declared count decides, and the read path then
/// follows that state.
#[tokio::test]
async fn a_refused_declared_write_leaves_the_snapshot_its_accepted_writes_and_its_count_built() {
    for row in ROWS {
        let case = row.case;
        let router = emulator();
        let snapshot_id = start_snapshot(&router, case, row.volume_size).await;

        // The started snapshot is readable on the EC2 side at once, before anything is
        // written: the lifecycle the direct API begins is observable from its first instant.
        let (status, body) = describe(&router, &snapshot_id).await;
        assert_eq!(
            status, 200,
            "case {case:?}: ec2:DescribeSnapshots must report the snapshot ebs:StartSnapshot \
             minted, got {status} with {body}"
        );
        assert_eq!(
            member(&body, "status"),
            STATE_PENDING,
            "case {case:?}: expected a started snapshot to be {STATE_PENDING}, got {body}"
        );

        // A write the service does not accept the declared values of is refused with the
        // modelled ValidationException for the class that failed.
        for refused in row.refused {
            let (reason, message) = refused.refusal(row.extent());
            let reply = put_block_declaring(
                &router,
                &snapshot_id,
                refused.index,
                payload_for(refused.index),
                refused.declared_length,
            )
            .await;
            assert_eq!(
                reply.status,
                400,
                "case {case:?}: a declared value the service does not accept must be refused \
                 400 at index {}, got {} with {}",
                refused.index,
                reply.status,
                reply.text()
            );
            assert!(
                reply.error_code().ends_with("ValidationException"),
                "case {case:?}: expected the ValidationException error code at index {}, got \
                 {:?}",
                refused.index,
                reply.error_code()
            );
            assert_eq!(
                refusal_reason(&reply),
                reason,
                "case {case:?}: expected Reason {reason} at index {}, got {}",
                refused.index,
                reply.text()
            );
            assert_eq!(
                reply.message(),
                message,
                "case {case:?}: the refusal must be worded as this service words it at index \
                 {}, got {}",
                refused.index,
                reply.text()
            );
        }

        // The writes whose declared length is the one supported size, at an index the volume
        // addresses, are accepted and report the checksum of the bytes they stored.
        for index in row.written {
            let payload = payload_for(*index);
            let expected = checksum(&payload);
            let reply =
                put_block_declaring(&router, &snapshot_id, *index, payload, BLOCK_SIZE).await;
            assert_eq!(
                reply.status,
                201,
                "case {case:?}: the accepted write at index {index} must answer 201, got {} \
                 with {}",
                reply.status,
                reply.text()
            );
            assert_eq!(
                reply.header("x-amz-Checksum"),
                expected,
                "case {case:?}: the write at index {index} must report the checksum of the \
                 stored bytes, got {:?}",
                reply.header("x-amz-Checksum")
            );
        }

        // The completion is accepted whatever count it declares: the count is never a
        // synchronous refusal.
        let sealed = complete_snapshot(&router, &snapshot_id, row.declared_count).await;
        assert_eq!(
            sealed.status,
            202,
            "case {case:?}: ebs:CompleteSnapshot declaring {} must be accepted with 202, got \
             {} with {}",
            row.declared_count,
            sealed.status,
            sealed.text()
        );

        // And within a bounded wait the snapshot reaches the terminal state the declared
        // count decides, which is the only read of that state a client has.
        let (state, body) = terminal_state(&router, case, &snapshot_id).await;
        assert_eq!(
            state, row.finalised,
            "case {case:?}: a count of {} against the accepted indexes {:?} must converge to \
             {:?}, got {state:?} in {body}",
            row.declared_count,
            row.accepted_indexes(),
            row.finalised
        );
        let progress = member(&body, "progress");
        if row.finalised == STATE_COMPLETED {
            assert_eq!(
                progress, PROGRESS_COMPLETED,
                "case {case:?}: expected progress {PROGRESS_COMPLETED} on a completed \
                 snapshot, got {progress:?}"
            );
        } else {
            assert_ne!(
                progress, PROGRESS_COMPLETED,
                "case {case:?}: a snapshot that did not complete accounts for none of its \
                 data, got progress {progress:?}"
            );
        }

        // The read path follows that state: a snapshot that completed serves exactly the
        // indexes its ACCEPTED writes covered — the refused ones left no residue — and one in
        // error resolves nowhere.
        let listing = list_blocks(&router, &snapshot_id).await;
        if row.finalised == STATE_COMPLETED {
            assert_eq!(
                listing.status,
                200,
                "case {case:?}: the completed snapshot must be listable, got {} with {}",
                listing.status,
                listing.text()
            );
            let listed = listed_blocks(&listing);
            let reported: Vec<i32> = listed.keys().copied().collect();
            assert_eq!(
                reported,
                row.accepted_indexes(),
                "case {case:?}: expected the accepted writes {:?} and nothing else, got \
                 {reported:?}",
                row.accepted_indexes()
            );
            for refused in row.refused {
                assert!(
                    row.written.contains(&refused.index) || !listed.contains_key(&refused.index),
                    "case {case:?}: the refused write at index {} must have stored nothing, \
                     got {reported:?}",
                    refused.index
                );
            }

            // And every index it reports holds the bytes its accepted write stored.
            for (index, token) in &listed {
                let fetched = get_block(&router, &snapshot_id, *index, token).await;
                assert_eq!(
                    fetched.status,
                    200,
                    "case {case:?}: the block at index {index} must be readable back, got {} \
                     with {}",
                    fetched.status,
                    fetched.text()
                );
                assert_eq!(
                    fetched.body,
                    payload_for(*index),
                    "case {case:?}: the block at index {index} must hold the bytes its \
                     accepted write stored, got {} byte(s) whose checksum is {:?}",
                    fetched.body.len(),
                    checksum(&fetched.body)
                );
            }
        } else {
            assert_eq!(
                listing.status,
                404,
                "case {case:?}: a snapshot that converged to {STATE_ERROR} must resolve \
                 nowhere, got {} with {}",
                listing.status,
                listing.text()
            );
            assert!(
                listing.error_code().ends_with(EBS_NOT_FOUND_CODE),
                "case {case:?}: expected the {EBS_NOT_FOUND_CODE} class, got {:?}",
                listing.error_code()
            );
        }

        // A block whose payload IS a full correctly declared block is still written to a
        // pending snapshot of the same shape, so the refusals above are about the values the
        // refused requests declared and not about the operation.
        let control = emulator();
        let control_id = start_snapshot(&control, case, row.volume_size).await;
        let payload = block_bytes(7);
        let accepted =
            put_block_declaring(&control, &control_id, row.extent() - 1, payload, BLOCK_SIZE).await;
        assert_eq!(
            accepted.status,
            201,
            "case {case:?}: the last index the volume addresses, at the supported length, must \
             answer 201, got {} with {}",
            accepted.status,
            accepted.text()
        );
    }
}
