Issue-Source: trace-sweep-divergence
Issue-Trace: ebs-list-changed-blocks-pagination-self-diff-and-not-found
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ebs-list-changed-blocks-pagination-self-diff-and-not-found ebs-snapshot-lifecycle-and-block-listing
Issue-Shape-Id: com.amazonaws.ebs#Ebs
Issue-Key: ebs|ListChangedBlocks|2xx->2xx|$.BlockSize
Issue-Key: ebs|ListChangedBlocks|2xx->2xx|$.VolumeSize
Issue-Key: ebs|ListSnapshotBlocks|2xx->2xx|$.ExpiryTime
Issue-Commit: 419b562cbe2018b58dd876132b3a1c26cb67862a

# ebs block listings gate ExpiryTime, VolumeSize and BlockSize on page emptiness (2 causes)

> _Note: this issue was identified against the code base at commit `419b562cbe2018b58dd876132b3a1c26cb67862a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListChangedBlocks

## Description
Both EBS direct-API block listings compute one boolean `hands_out_block_tokens = !page.is_empty()` and use it to decide which of the three snapshot-describing output members (ExpiryTime, VolumeSize, BlockSize) they serialize. The recorded service does not gate those members that way, and the emulator gets it wrong in BOTH directions: ListChangedBlocks emits VolumeSize/BlockSize on an answer the service leaves bare, and ListSnapshotBlocks withholds ExpiryTime on an answer the service fills. Two causes, one shared fault — the presence rule for these optional members.

Cause 1 of 2 — ListChangedBlocks on a comparison that reports no changed block still serializes VolumeSize and BlockSize (fixture ebs-list-changed-blocks-pagination-self-diff-and-not-found, step task#8).
AWS: recorded behavior only. Smithy (services/service-com-amazonaws-ebs-ebs/smithy-model/com-amazonaws-ebs-ebs.json) models ListChangedBlocksResponse$ExpiryTime (TimeStamp), $VolumeSize (VolumeSize, @range min 1) and $BlockSize (BlockSize) as OPTIONAL members and documents no presence rule for them ('The size of the volume in GB', 'The size of the blocks in the snapshot, in bytes', 'The time when the BlockToken expires'). The recording shows that when the comparison yields an empty ChangedBlocks array — the self-diff case, firstSnapshotId equal to the snapshot named in the URI, where nothing differs — the service answers HTTP 200 with the empty ChangedBlocks array and NONE of ExpiryTime, VolumeSize or BlockSize present. Hypothesis: all three describe the block tokens the answer hands out, so an answer that hands out none reports none of them.
Emulator: answers HTTP 200 (status matches) but the body carries BlockSize and VolumeSize where the recording has neither — replay reports '$.BlockSize: type_mismatch (expected None, got 524288)' and '$.VolumeSize: type_mismatch (expected None, got 1)'. ExpiryTime is correctly omitted, so only two of the three members are conditioned wrongly.
Why: a client that treats the presence of VolumeSize/BlockSize as 'this comparison returned block data' — or that maps the response into an optional-typed struct and branches on it — sees a populated geometry on a diff that found nothing, which on AWS is the signal that the two snapshots are identical. Values differ per snapshot (any volume size, any block size), so the defect is the presence of the members, not the numbers.
Fix: the output construction in the ListChangedBlocks handler — the member-gating decision, which must put VolumeSize and BlockSize under the same condition as ExpiryTime rather than filling them unconditionally.
Traces: ebs-list-changed-blocks-pagination-self-diff-and-not-found#task#8.

Cause 2 of 2 — ListSnapshotBlocks omits ExpiryTime on an answer whose Blocks page is empty (fixture ebs-snapshot-lifecycle-and-block-listing, step task#4).
AWS: recorded behavior only. Smithy models ListSnapshotBlocksResponse$ExpiryTime as an optional TimeStamp ('The time when the BlockToken expires') with no documented presence rule. The recording shows the service answering HTTP 200 WITH ExpiryTime set (a timestamp; the recorded epoch value 1791769681.65 is that run's clock and is not itself a requirement) while the emulator's own Blocks list matched the recording — i.e. the service reports the snapshot's token-expiry instant on an answer that lists no block.
Emulator: answers HTTP 200 with ExpiryTime absent — replay reports '$.ExpiryTime: missing (expected 1791769681.65, got None)'. The handler suppresses the member whenever the page reports no block, on the premise that the member is 'of the tokens this answer hands out'; the recording refutes that premise for this operation.
Why: ExpiryTime is how a client learns the deadline for the snapshot's block tokens. A client that pages a listing and reads ExpiryTime off any page (including a short or empty intermediate page, which 'the request can return less blocks than MaxResults or an empty array of blocks' explicitly permits) gets no deadline at all and either treats the tokens as non-expiring or fails on an absent required-by-the-client field.
Fix: the output construction in the ListSnapshotBlocks handler — ExpiryTime must be reported on every answer that resolved the snapshot, not only on one whose page carries blocks.
Traces: ebs-snapshot-lifecycle-and-block-listing#task#4.

Neither cause is covered by an existing open ebs issue: 20261120T090942Z-ebs-listchangedblocks-wrong-behavior.md is about a ChangedBlock emitted with no SecondBlockToken, 20261119T102154Z-ebs-listsnapshotblocks-maintainability.md is about duplicated page-size/page-token code, and 20261119T102154Z-ebs-getsnapshotblock-wrong-behavior.md is about tokens resolving past their ExpiryTime. None of them changes which output members these two listings serialize.

Since this was filed, CAUSE 1 has been closed by issues/things-that-now-work/20261109T074035Z-ebs-listchangedblocks-empty-snapshot-comparisons-answer-200-instead.md — ListChangedBlocks no longer serializes VolumeSize/BlockSize on a comparison that reports no changed block. CAUSE 2 is still open: services/service-com-amazonaws-ebs-ebs/src/ops/list_snapshot_blocks.rs:283 still computes `hands_out_block_tokens = !blocks.is_empty()` and :291 still gates `expiry_time` on it, so a fixer picking this up has only that cause to close.

## Evidence
Replay of trace set simparity-wire-aws-v1 (61 steps, 59 passed, 2 mismatched):

Cause 1 — service=ebs, operation=ListChangedBlocks, fixture ebs-list-changed-blocks-pagination-self-diff-and-not-found, step task#8, verdict=mismatch, expected_status=200, actual_status=200.
  detail: $.BlockSize: type_mismatch (expected None, got 524288)
          $.VolumeSize: type_mismatch (expected None, got 1)
  Expected (recording): 200, body with the empty ChangedBlocks array and no ExpiryTime / VolumeSize / BlockSize.
  Actual (emulator):    200, body carrying VolumeSize and BlockSize.
  Code: services/service-com-amazonaws-ebs-ebs/src/ops/list_changed_blocks.rs:333 computes `hands_out_block_tokens = !page.is_empty()`, gates `expiry_time` on it at :342, then sets `volume_size: Some(second.volume_size)` and `block_size: Some(second.block_size)` unconditionally at :354-355.

Cause 2 — service=ebs, operation=ListSnapshotBlocks, fixture ebs-snapshot-lifecycle-and-block-listing, step task#4, verdict=mismatch, expected_status=200, actual_status=200.
  detail: $.ExpiryTime: missing (expected 1791769681.65, got None)
  Expected (recording): 200 with ExpiryTime present (a timestamp).
  Actual (emulator):    200 with ExpiryTime absent; Blocks matched.
  Code: services/service-com-amazonaws-ebs-ebs/src/ops/list_snapshot_blocks.rs:283 computes `hands_out_block_tokens = !blocks.is_empty()` and :291-292 serializes `expiry_time` only when it is true.

Smithy evidence (model services/service-com-amazonaws-ebs-ebs/smithy-model/com-amazonaws-ebs-ebs.json): ListChangedBlocks output = {ChangedBlocks, ExpiryTime: TimeStamp, VolumeSize: long @range min 1, BlockSize: integer, NextToken} and ListSnapshotBlocks output = {Blocks, ExpiryTime: TimeStamp, VolumeSize, BlockSize, NextToken}; all of ExpiryTime/VolumeSize/BlockSize are optional on both, and the model states no rule for when they are present — so the recorded responses are the contract.

## Steps to Reproduce
Cause 1 (ListChangedBlocks, fixture ebs-list-changed-blocks-pagination-self-diff-and-not-found, step task#8):
1. POST /snapshots {"VolumeSize": 1} to mint a snapshot; PUT at least one 524288-byte block (x-amz-Data-Length and the matching x-amz-Checksum); POST /snapshots/completion/{snap} with the matching x-amz-ChangedBlocksCount so it seals `completed`.
2. GET /snapshots/{snap}/changedblocks?firstSnapshotId={snap} — the snapshot compared against itself, so nothing differs.
3. Observe the 200 body: ChangedBlocks is empty, but VolumeSize and BlockSize are present. The recorded service returns the empty array with no VolumeSize, no BlockSize and no ExpiryTime.

Cause 2 (ListSnapshotBlocks, fixture ebs-snapshot-lifecycle-and-block-listing, step task#4):
1. Mint and complete a snapshot as above.
2. GET /snapshots/{snap}/blocks so that the page reports no block (e.g. a startingBlockIndex past the highest index the snapshot holds).
3. Observe the 200 body: Blocks is empty and ExpiryTime is absent. The recorded service reports ExpiryTime (the snapshot's block-token expiry instant) on that same answer.

Both are reproduced together by replaying trace set simparity-wire-aws-v1 against the emulator; the two steps are the only mismatches in the corpus.
