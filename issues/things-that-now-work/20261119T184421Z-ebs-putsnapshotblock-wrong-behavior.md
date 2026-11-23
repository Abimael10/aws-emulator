Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 6a4ecefd511132c775de04942ca2f205592ed51b

# A block write never re-anchors the snapshot's Timeout, so the second documented cancellation trigger is never implemented (and in-flight blocks are discarded)

_Filed: 2026-11-19T18:44:21Z_
_Operation: PutSnapshotBlock | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `6a4ecefd511132c775de04942ca2f205592ed51b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`StartSnapshotRequest$Timeout` documents TWO triggers for the automatic cancellation of a pending snapshot: the snapshot has no blocks written to it, and the snapshot "is not completed after writing the last block of data". The crate implements only ONE deadline, anchored at the instant `StartSnapshot` ran: `start_snapshot.rs:414` stores `expiry_time: expiry_from_timeout(now, timeout_minutes)` and no other code in the crate ever assigns `expiry_time` (a grep for `expiry_time` in `src/` finds only that write, the schedule reads in `state.rs`, and the two listings that report it). `PutSnapshotBlock` — the operation that writes the "last block of data" the second trigger is anchored to — persists the block (`put_snapshot_block.rs:369-384`) and never touches the snapshot's deadline.

Two consequences:

1. The second documented trigger does not exist. A producer that keeps writing blocks is cancelled on the FIRST clock, not `Timeout` minutes after its last write, so the deadline the service enforces is indistinguishable from trigger 1 ("no blocks are written"): writing blocks has no effect on when the snapshot is cancelled.
2. The cancellation that fires mid-upload DISCARDS the blocks already written: `apply_due_transitions` (`state.rs:392-399`) flips the status to `Error` and calls `snapshot.blocks.clear()`. Because `PutSnapshotBlock` itself calls `apply_due_transitions` before resolving the snapshot (`put_snapshot_block.rs:262`), the very write that should have re-anchored the deadline is instead the call that destroys the stored blocks and is then refused with `INVALID_SNAPSHOT_ID` (`put_snapshot_block.rs:284-297`), and the later `CompleteSnapshot` is refused "was cancelled and can no longer be completed" (`complete_snapshot.rs:265-275`).

No new constant or window is proposed: `Snapshot::timeout_minutes` is already stored on the record alongside `expiry_time`, which is what the second trigger would be recomputed from on each accepted write.

## Evidence

Smithy, `StartSnapshotRequest$Timeout` (smithy_query query="doc"): "The amount of time (in minutes) after which the snapshot is automatically cancelled if:" — "No blocks are written to the snapshot." / "The snapshot is not completed after writing the last block of data." / "If no value is specified, the timeout defaults to 60 minutes."

The only anchor the crate computes is the START instant:
- services/service-com-amazonaws-ebs-ebs/src/ops/start_snapshot.rs:401,414 — "let timeout_minutes = input.timeout.unwrap_or(DEFAULT_SNAPSHOT_TIMEOUT_MINUTES);" … "expiry_time: expiry_from_timeout(now, timeout_minutes),"
- services/service-com-amazonaws-ebs-ebs/src/state.rs:740-743 — "pub fn expiry_from_timeout(start_time: SystemTime, timeout_minutes: i32) -> SystemTime {\n    let minutes = u64::try_from(timeout_minutes).unwrap_or(0);\n    start_time + Duration::from_secs(minutes.saturating_mul(60))\n}"
- The crate's own paraphrase drops the last-write anchor rather than justifying its absence — services/service-com-amazonaws-ebs-ebs/src/state.rs:374-377: "/// One is owed, at an instant stored when the call that started it ran, so it was\n    /// owed whether or not anybody called again: a `Timeout`.\n    /// `StartSnapshotRequest$Timeout` documents that a snapshot is \"automatically\n    /// cancelled\" when it is not completed within that many minutes"

The write path neither re-anchors nor protects what it stored:
- services/service-com-amazonaws-ebs-ebs/src/ops/put_snapshot_block.rs:259-262 — "let state = crate::state::service_state(&*self.engine).ok_or_else(no_state)?;\n        let now = SystemTime::now();\n        let mut store = state.lock();\n        store.apply_due_transitions(now);"
- services/service-com-amazonaws-ebs-ebs/src/ops/put_snapshot_block.rs:384 — "snapshot.blocks.insert(input.block_index, entry);" — the handler's only mutation of the record; `expiry_time` is never read or written in this file.
- services/service-com-amazonaws-ebs-ebs/src/state.rs:392-399 — "for snapshot in self.snapshots.values_mut() {\n            if snapshot.status != SnapshotStatus::Pending {\n                continue;\n            }\n            if now >= snapshot.expiry_time {\n                snapshot.status = SnapshotStatus::Error;\n                snapshot.blocks.clear();" — the blocks written before the start-anchored deadline are dropped.
- services/service-com-amazonaws-ebs-ebs/src/ops/put_snapshot_block.rs:284-297 — the next write is then refused: "let write_refusal = (!snapshot.accepts_writes()).then(|| { … format!(\"snapshot {} is {}; blocks can only be written to a pending snapshot\" …"

Test coverage: no test exercises the cancellation at all. tests/all/read_path_requires_a_completed_snapshot.rs:19-23 states why — "it also covers a snapshot its `Timeout` cancelled — the one other status the read path cannot see — which no request can reach inside a test: the documented minimum `Timeout` is 10 minutes". The three tests that pass `Timeout` (snapshot_block_loop.rs:727,752,1105; sealed_status_and_fixed_refusal_text.rs:379) only send the value; nothing pins a start-anchored deadline, so nothing in the crate justifies the divergence.

## Steps to Reproduce

1. StartSnapshot with `{"VolumeSize": 1, "Timeout": 10}` (10 minutes is the modelled minimum) and note the returned SnapshotId and StartTime.
2. At about minute 5, PutSnapshotBlock a 524288-byte block at BlockIndex 0 (it is accepted, 201).
3. Wait past minute 10 — i.e. more than `Timeout` after StartTime but only ~5 minutes after the last block was written, which is the window the second documented trigger allows.
4. PutSnapshotBlock a second block at BlockIndex 1. Observe HTTP 400 ValidationException `INVALID_SNAPSHOT_ID` ("snapshot … is error; blocks can only be written to a pending snapshot"): `apply_due_transitions` (state.rs:392-399) cancelled the snapshot on the start-anchored deadline and cleared the block written in step 2, because nothing re-anchored `expiry_time` when that block was persisted.
5. CompleteSnapshot on the same snapshot: observe HTTP 400 "snapshot … was cancelled and can no longer be completed" (complete_snapshot.rs:265-275) — the upload that was still making progress is unrecoverable and the block bytes are gone.
