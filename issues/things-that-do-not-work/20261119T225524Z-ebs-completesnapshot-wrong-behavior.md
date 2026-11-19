Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 25f334ef49f0fe78ce039b48068cddfc6b5b1616

# A completion that finalises a snapshot to `error` never releases its blocks, unlike the sibling cancellation path

_Filed: 2026-11-19T22:55:24Z_
_Operation: CompleteSnapshot | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `25f334ef49f0fe78ce039b48068cddfc6b5b1616`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

`CompleteSnapshot` with a `ChangedBlocksCount` that disagrees with the number of blocks written finalises the record to `SnapshotStatus::Error` (`Snapshot::apply_completion`). From that moment the snapshot is permanently unreachable: `is_readable()` is true only for `Completed`, so `ListSnapshotBlocks`, `GetSnapshotBlock` and `ListChangedBlocks` all resolve it to the 404 not-found refusal; `accepts_writes()` is true only for `Pending`, so `PutSnapshotBlock` refuses every further write; and a repeat of `CompleteSnapshot` takes the `SnapshotStatus::Error if snapshot.completion_accepted()` arm, which applies no second completion. Nothing in the crate can ever read those bytes again.

The crate's OTHER route to the same terminal `Error` state — the `Timeout` cancellation in `ComAmazonawsEbsEbsState::apply_due_transitions` — treats that same fact as the trigger for releasing the bytes (`snapshot.blocks.clear()`), and its doc states the rule explicitly as the store's retention policy. The completion path skips it: `apply_completion` sets `status = SnapshotStatus::Error` and leaves `self.blocks` untouched, so the entries (512 KiB of `BlockEntry::data` each, plus their block tokens, which live on the entries) stay resident forever.

That retained, unreachable state is not inert: `stored_block_count()` sums `snapshot.blocks.len()` across EVERY stored snapshot whatever its status, and that sum is the gate `PutSnapshotBlock` and `StartSnapshot`'s parent-copy check read. Blocks that belong to snapshots nothing can ever list, fetch, compare or write to therefore consume the store-wide budget and make later, perfectly valid writes to UNRELATED snapshots answer the modelled `ServiceQuotaExceededException` (HTTP 402). The two paths to one status disagree about the same rule, and nothing in the crate reconciles them.

## Evidence

The two routes to `Error` are the SAME condition — services/service-com-amazonaws-ebs-ebs/src/state.rs:93-96:
"    /// The snapshot cannot be used: its `Timeout` elapsed while it was still pending, or
    /// the completion it accepted declared a different number of blocks from the number
    /// written into it, so its content was never the content its producer claimed.
    Error,"

The path that RELEASES — state.rs:383-386 (doc) and state.rs:396-399 (code):
"    /// A cancelled snapshot can never be completed, nor its blocks listed or fetched
    /// again, so this is also where their bytes are released (and with them their block
    /// tokens, which live on the entries): it is this store's retention policy, not just
    /// a status flip."
"            if now >= snapshot.expiry_time {
                snapshot.status = SnapshotStatus::Error;
                snapshot.blocks.clear();
            }"

The path that RETAINS — state.rs:267-278 (`Snapshot::apply_completion`, called from the handler):
"        self.declared_changed_blocks_count = Some(declared_changed_blocks_count);
        if i64::from(declared_changed_blocks_count) == self.written_block_count() {
            self.status = SnapshotStatus::Completed;
            ...
        } else {
            self.status = SnapshotStatus::Error;
        }"
(no `self.blocks.clear()`), reached from services/service-com-amazonaws-ebs-ebs/src/ops/complete_snapshot.rs:254-256:
"        match snapshot.status {
            SnapshotStatus::Pending => {
                snapshot.apply_completion(input.changed_blocks_count, now);"

That the retained blocks are unreachable — state.rs:290-292:
"    pub fn is_readable(&self) -> bool {
        self.status == SnapshotStatus::Completed
    }"
and complete_snapshot.rs:264 ("SnapshotStatus::Error if snapshot.completion_accepted() => {}") plus put_snapshot_block.rs:284-297, which refuses every later write.

That the retained blocks still consume the store-wide budget — state.rs:477-482:
"    pub fn stored_block_count(&self) -> usize {
        self.snapshots
            .values()
            .map(|snapshot| snapshot.blocks.len())
            .sum()
    }"
read by put_snapshot_block.rs:349-353:
"            if store.stored_block_count() >= MAX_STORED_BLOCKS_TOTAL {
                return Err(quota_exceeded(format!(
                    \"this emulator already holds its maximum of {MAX_STORED_BLOCKS_TOTAL} snapshot blocks\"
                )));"

Test coverage: tests/all/changed_blocks_count_decides_the_terminal_status.rs:326-341 asserts only that a snapshot finalised to `error` "must resolve nowhere" (404 `ResourceNotFoundException`); no test in the crate pins whether its bytes are kept or released, so nothing justifies the divergence from the cancellation path.

## Steps to Reproduce

1. POST /snapshots with {"VolumeSize": 1} to mint snapshot A.
2. PUT /snapshots/A/blocks/0 with 524288 bytes, x-amz-Data-Length: 524288, x-amz-Checksum-Algorithm: SHA256 and the matching x-amz-Checksum — 201.
3. POST /snapshots/completion/A with x-amz-ChangedBlocksCount: 5 (it disagrees with the one block written) — 202; a repeat of the call now reads Status "error".
4. GET /snapshots/A/blocks — 404 ResourceNotFoundException, and PUT /snapshots/A/blocks/0 — 400 ValidationException "Snapshot is already completed": the block is unreachable and unreplaceable forever.
5. Observe that the block is nevertheless still stored: `ComAmazonawsEbsEbsState::stored_block_count()` still counts it (unlike the Timeout path, which clears `snapshot.blocks`).
6. Repeat steps 1-3 until the retained blocks of error-finalised snapshots reach MAX_STORED_BLOCKS_TOTAL, then PUT a block to a fresh pending snapshot B: it is refused HTTP 402 ServiceQuotaExceededException "this emulator already holds its maximum of 512 snapshot blocks", although every block holding the budget belongs to a snapshot nothing can ever read.
7. For contrast, start a snapshot with Timeout: 10, write a block, let the timeout elapse and call any operation: `apply_due_transitions` flips the status to error AND clears the blocks, freeing the same budget.
