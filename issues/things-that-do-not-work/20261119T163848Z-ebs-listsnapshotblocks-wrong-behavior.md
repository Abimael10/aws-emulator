Issue-Severity: MEDIUM
Issue-Gap-Class: wrong-behavior
Issue-Commit: 23a6305fb4194ac652f562ce813f73dafa581013

# An empty ListSnapshotBlocks page still reports ExpiryTime, VolumeSize and BlockSize, unlike its sibling listing

_Filed: 2026-11-19T16:38:48Z_
_Operation: ListSnapshotBlocks | Class: wrong-behavior_

> _Note: this issue was identified against the code base at commit `23a6305fb4194ac652f562ce813f73dafa581013`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Description

The two listings of this crate apply OPPOSITE rules to the same three output members (`ExpiryTime`, `VolumeSize`, `BlockSize`) under the same condition — a page that reports no block and therefore hands out no `BlockToken`.

`ListChangedBlocks` implements the rule explicitly: it computes `hands_out_block_tokens = !page.is_empty()` and gates all three members on it, on the grounds that `ExpiryTime` is "The time when the `BlockToken` expires" and an answer that hands out no token has no such instant, with `VolumeSize`/`BlockSize` described there as members that "describe the blocks a page reported". `ListSnapshotBlocks` fills all three unconditionally, so a page that reports zero blocks (and hands out zero tokens) still carries the expiry of tokens nobody was given plus the geometry of blocks it did not report.

The empty page is reachable through exactly the input the sibling suite exercises for `ListChangedBlocks`: `startingBlockIndex` past every stored index (the model documents the member as "The block index from which the list should start", `@range` min 0). One of the two handlers must be wrong about the same three members; nothing in the crate reconciles them — there is no comment in `list_snapshot_blocks.rs` explaining why the rule stated in `list_changed_blocks.rs` does not apply to it, and no test pins the members on an empty `ListSnapshotBlocks` page.

## Evidence

The rule, applied by the sibling — services/service-com-amazonaws-ebs-ebs/src/ops/list_changed_blocks.rs:330-350:
"        // Whether this page hands out any block token at all. A changed index is one at
        // least one side holds a block at, so a page carrying changed blocks always
        // carries tokens, and a page carrying none carries none.
        let hands_out_block_tokens = !page.is_empty();

        Ok(ListChangedBlocksOutput {
            changed_blocks: Some(page),
            // \"The time when the `BlockToken` expires\": the tokens this answer hands out
            // are of BOTH snapshots, so the one instant it reports is the earliest at
            // which any of them stops resolving. An answer that hands out no token has no
            // such instant to report, so the member is OMITTED rather than filled with the
            // expiry of tokens nobody was given — as are the geometry members below, which
            // describe the blocks a page reported.
            expiry_time: hands_out_block_tokens.then(|| {...}),
            volume_size: hands_out_block_tokens.then_some(second.volume_size),
            block_size: hands_out_block_tokens.then_some(second.block_size),"

The same three members, unconditional — services/service-com-amazonaws-ebs-ebs/src/ops/list_snapshot_blocks.rs:281-287:
"        Ok(ListSnapshotBlocksOutput {
            blocks: Some(blocks),
            expiry_time: Some(::aws_smithy_types::DateTime::from(snapshot.expiry_time)),
            volume_size: Some(snapshot.volume_size),
            block_size: Some(snapshot.block_size),
            next_token,
        })"
where `blocks` is whatever `snapshot.blocks.range(starting_block_index..).take(page_size)` yields — the empty vector when `starting_block_index` is past every stored index (list_snapshot_blocks.rs:265-273).

The member whose meaning the sibling reasons from, as this operation's own modelled contract states it — services/service-com-amazonaws-ebs-ebs/src/ops/list_snapshot_blocks.rs:76:
"//! * `ExpiryTime` (TimeStamp) — The time when the BlockToken expires."

Test coverage: the sibling rule is pinned in both directions, including the exact analogous input — services/service-com-amazonaws-ebs-ebs/tests/all/changed_blocks_between_snapshots.rs:888-890 ("RULE 6: the token members belong to the tokens — a page that reports no changed block hands out no block token, so it omits `ExpiryTime`, `VolumeSize` and `BlockSize`, while a page that reports changed blocks carries all three.") with the row at changed_blocks_between_snapshots.rs:930-937 ("a listing starting past every changed index", "startingBlockIndex=10"). The `ListSnapshotBlocks` counterpart row exists but asserts only the reported indexes — services/service-com-amazonaws-ebs-ebs/tests/all/snapshot_block_loop.rs:606 ("(\"start past every written index\", 11, &[])") checked at snapshot_block_loop.rs:616-630, and the only assertion on `ExpiryTime` (snapshot_block_loop.rs:534-538) runs on non-empty listings, so no existing test pins the members on an empty page.

## Steps to Reproduce

1. POST /snapshots with {"VolumeSize": 1} to mint a snapshot.
2. PUT /snapshots/{id}/blocks/0 with 524288 bytes, x-amz-Data-Length: 524288, x-amz-Checksum: <base64 sha256>, x-amz-Checksum-Algorithm: SHA256.
3. POST /snapshots/completion/{id} with x-amz-ChangedBlocksCount: 1 so the snapshot is sealed `completed`.
4. GET /snapshots/{id}/blocks?startingBlockIndex=1 — HTTP 200 with "Blocks": [] (no block, so no BlockToken is handed out) and yet ExpiryTime, VolumeSize and BlockSize all present in the body.
5. For the sibling's behaviour on the same shape of answer, build a lineage and GET /snapshots/{child}/changedblocks?firstSnapshotId={parent}&startingBlockIndex=<past every changed index> — HTTP 200 with "ChangedBlocks": [] and ExpiryTime, VolumeSize and BlockSize all OMITTED.
