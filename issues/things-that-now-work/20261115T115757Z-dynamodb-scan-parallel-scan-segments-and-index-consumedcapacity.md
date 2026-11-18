Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-trajwf-parallel-scan-segment-contract
Issue-Step: task#6
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-trajwf-parallel-scan-segment-contract dynamodb-scan-on-index-consumed-capacity-attributes-the-index
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*]
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*].id.S
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*].status.S
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*].timestamp.S
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*].workflowId.S
Issue-Key: dynamodb|Scan|2xx->2xx|$.ConsumedCapacity.CapacityUnits
Issue-Related: 20261111T221143Z-dynamodb-condition-key-and-filter-expression-defects-are.md
Issue-Related: 20261111T221143Z-dynamodb-index-scan-consumedcapacity-is-under-reported-and.md
Issue-Commit: 015bd153266295538afd2721c5b6fd3d37297a70

# dynamodb Scan: parallel-scan segments and index ConsumedCapacity diverge (4 causes)

> _Note: this issue was identified against the code base at commit `015bd153266295538afd2721c5b6fd3d37297a70`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

> _Resolution: causes 3 and 4 are CLOSED — a scan of a secondary index is charged a whole read unit per 4 KB of each entry it evaluated, on that index's own arm, and the two recorded index-scan steps pass. Of causes 1 and 2, what is CLOSED is the walk the segments are cut from: a scan walks the key space in the order its partitions are laid out, so a segment is a contiguous RUN of the scan's own walk, the segments in segment order are the unsegmented scan, and a segment pages along its own run. What is DECLINED is WHICH items each segment answers (`$.Items[*]` and the `Count`/`ScannedCount` that follow from it, the two `dynamodb-trajwf-parallel-scan-segment-contract` segment steps): a partition's place in the key space follows the service's INTERNAL hash function, which is published nowhere and cannot be derived from the model, the request or the stored state, so reproducing one recording's per-segment item sets would mean choosing a digest variant until that fixture's keys landed as recorded. The emulator's placement is its own, stated as such where it is computed; what callers can rely on — one segment per partition, stable under writes, disjoint, covering, and contiguous in the walk — is implemented and pinned by `tests/all/scan_key_space_walk.rs` and `tests/all/scan_segment_partitions.rs`. A later sweep that re-files those two steps should read this paragraph first._

## Operation
Scan

## Description
Four net-new Scan response defects on 200 replies: a parallel Scan does not partition the table across its segments (one segment answers nothing, another answers the whole table), and an index Scan still reports half the capacity the operation consumed. 4 causes.

Cause 1 of 4: a parallel Scan segment that must return its share of the table returns nothing.
AWS: Smithy: Scan.Segment (ScanSegment, range 0..999999) 'identifies an individual segment to be scanned by an application worker... Segment IDs are zero-based... The value for Segment must be greater than or equal to 0, and less than the value provided for TotalSegments', and Scan.TotalSegments (range 1..1000000) is the number of segments the table is divided into. The contract is that each stored item belongs to exactly ONE segment, so the segments of one TotalSegments fan-out partition the table: every worker gets a non-trivial, disjoint share and the union of all segments is the full scan, with Count/ScannedCount per segment reflecting only that share. Recorded behavior: for this table of three items the recorded segment returns the two items that hash into it, with Count = ScannedCount = 2.
Emulator: answered 200 with Count 0, ScannedCount 0 and no Items at all for that segment.
Why: a client that fans a parallel Scan out over TotalSegments workers and concatenates the per-segment results gets a table view that is wrong per worker: the worker owning this segment sees an empty table and skips all of its share, so sharded exports, migrations and parallel aggregations silently process a subset (or, on the sibling segment, re-process items that are not theirs). Any work distribution derived from per-segment Count is also wrong.
Fix: the Scan handler's segment selection — assign each item to exactly one segment by a stable hash of its partition key modulo TotalSegments before applying Limit/filters, and count only the items of the requested Segment into Count/ScannedCount. Hypothesis: the current implementation routes every item to a single segment rather than distributing them.
Traces: dynamodb-trajwf-parallel-scan-segment-contract#task#6.

Cause 2 of 4: the sibling segment of the same fan-out returns the WHOLE table instead of its own share.
AWS: as cause 1 — this segment owns only the items that hash into it. Recorded behavior: one item, Count = ScannedCount = 1, and the item returned is the one belonging to that segment (a different partition-key value from the items of cause 1's segment).
Emulator: answered 200 with Count 3 and ScannedCount 3, returning every item in the table, so Items[0] is a different item than the recorded one and two extra Items are appended.
Why: as cause 1, from the other side: this worker reads items another segment owns, so a fan-out over all segments double-processes those items while cause 1's share is never read. De-duplication by key is the only recovery, which defeats the point of parallel Scan.
Fix: as cause 1, same code path in the Scan handler.
Traces: dynamodb-trajwf-parallel-scan-segment-contract#task#7.

Cause 3 of 4: Scan on a global secondary index under-reports ConsumedCapacity by half.
AWS: Smithy: ConsumedCapacity.CapacityUnits is 'The total number of capacity units consumed by the operation' and ConsumedCapacity.GlobalSecondaryIndexes is a SecondaryIndexesCapacityMap holding 'The amount of throughput consumed on each global index affected by the operation'. Recorded behavior: both the total and the per-index entry carry the same figure, sized from the bytes the index scan actually read, which for this scan is twice the value the emulator reports; the per-call eventually-consistent floor is not the answer for a scan that read more than one 4 KB unit.
Emulator: reported the same, halved figure for both $.ConsumedCapacity.CapacityUnits and $.ConsumedCapacity.GlobalSecondaryIndexes.<indexName>.CapacityUnits.
Why: a client that budgets, meters, autoscales or alarms on ConsumedCapacity sees a cost far below what the scan consumed, so capacity planning, throttling simulation and cost attribution to the GSI are all wrong.
Fix: the Scan handler's capacity accounting for index reads — size the read units from the total bytes of the index entries scanned (rounded up per 4 KB, halved only for eventually-consistent reads) instead of a fixed per-call amount, and propagate the same figure into the GlobalSecondaryIndexes map.
Traces: dynamodb-scan-on-index-consumed-capacity-attributes-the-index#task#5.

Cause 4 of 4: the same halved accounting on a local secondary index.
AWS: as cause 3, with the figure also appearing under $.ConsumedCapacity.LocalSecondaryIndexes.<indexName>.CapacityUnits.
Emulator: half the recorded figure for both the total and the LSI entry.
Why: as cause 3, mis-attributing LSI read cost.
Fix: as cause 3, on the LSI path.
Traces: dynamodb-scan-on-index-consumed-capacity-attributes-the-index#task#6.

Related but not covering: open issue 20261111T221143Z-dynamodb-index-scan-consumedcapacity-is-under-reported-and.md describes an earlier, larger shortfall on the same two index-scan steps, and 20261111T170613Z-dynamodb-consumedcapacity-numbers-diverge-on-index-reads-and.md covers other ConsumedCapacity causes; neither carries these clusters' current replay keys (the mismatch now also spans $.ConsumedCapacity.CapacityUnits), and no open issue mentions parallel-scan segmentation.

## Evidence
Cause 1: Scan 200, dynamodb-trajwf-parallel-scan-segment-contract#task#6 — $.Count expected 2, got 0; $.ScannedCount expected 2, got 0; $.Items[0] and $.Items[1] expected present (the two items of that segment), got None.
Cause 2: Scan 200, dynamodb-trajwf-parallel-scan-segment-contract#task#7 — $.Count expected 1, got 3; $.ScannedCount expected 1, got 3; $.Items[0] is the wrong item (every attribute differs: the recorded segment's item vs. the first item of the table) and $.Items[1], $.Items[2] are extra.
Cause 3: Scan 200, dynamodb-scan-on-index-consumed-capacity-attributes-the-index#task#5 — $.ConsumedCapacity.CapacityUnits expected 2.0, got 1.0; $.ConsumedCapacity.GlobalSecondaryIndexes.gsi1.CapacityUnits expected 2.0, got 1.0.
Cause 4: Scan 200, dynamodb-scan-on-index-consumed-capacity-attributes-the-index#task#6 — $.ConsumedCapacity.CapacityUnits expected 2.0, got 1.0; $.ConsumedCapacity.LocalSecondaryIndexes.lsi1.CapacityUnits expected 2.0, got 1.0.
All four steps answered HTTP 200 with the expected status; only the response body diverges.

## Steps to Reproduce
1. Replay dynamodb-trajwf-parallel-scan-segment-contract#task#6 and #task#7: with the same TotalSegments, each Segment must return only the items assigned to it — the two segments' Items must be disjoint, their union must be the full table, and Count/ScannedCount must match each segment's own share (2 and 1 respectively), not 0 and 3.
2. Replay dynamodb-scan-on-index-consumed-capacity-attributes-the-index#task#5 (GSI) and #task#6 (LSI): ConsumedCapacity.CapacityUnits and the matching per-index entry must both be sized from the index bytes the scan read, not from a per-call floor.
