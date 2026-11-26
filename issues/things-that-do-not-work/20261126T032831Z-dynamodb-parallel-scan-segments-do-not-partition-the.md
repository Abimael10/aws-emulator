Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-scan-total-segments-4096-pins-number-partition-keys
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-scan-total-segments-4096-pins-number-partition-keys dynamodb-trajwf-parallel-scan-segment-contract
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|Scan|2xx->2xx|$.Count
Issue-Key: dynamodb|Scan|2xx->2xx|$.ScannedCount
Issue-Related: 20261120T225636Z-dynamodb-scan-partition-ordering-and-parallel-scan-segment.md
Issue-Commit: 857f59baf94ca876ddeb888a415f04d0868c32ab

# DynamoDB parallel Scan segments do not partition the table: one segment is empty, another serves every item

> _Note: this issue was identified against the code base at commit `857f59baf94ca876ddeb888a415f04d0868c32ab`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Scan

## Description
A segmented (parallel) Scan answers 200 but the item set it serves is not the requested segment's share of the table. Across the corpus the same defect shows up in both directions: segments that should own items serve an empty page, and a segment that should own exactly one item serves the whole table. The segments of one parallel scan therefore neither cover the table nor are disjoint.

THIS BUNDLE CONTAINS 3 CAUSES.

Cause 1 of 3 - with TotalSegments at its upper practical range the item pinned to a segment is not in that segment.
AWS: Smithy: Scan.TotalSegments -> ScanTotalSegments @range 1..1000000; Scan.Segment -> ScanSegment @range 0..999999; output Items -> ItemList with Count/ScannedCount (Integer, default 0). AWS documents that for a parallel scan each worker scans its own Segment and the segments together cover the table exactly once; the model states no ordering, so the specific placement is recorded behavior only. The recording scans the segment that owns a specific Number partition key and gets 200 with Count 1 / ScannedCount 1 and that one item.
Emulator: 200 with Count 0, ScannedCount 0 and no Items - the segment that the key belongs to is served empty, so the item has been placed in some other segment (or in none).
Why: a client that fans a scan out over TotalSegments workers silently loses rows: the worker that AWS would have handed the item processes nothing, and no other worker is expected to see it. That is data loss, not a cosmetic difference.
Fix: the Segment/TotalSegments assignment function in the Scan handler - item -> segment must be a stable total partition of the table, computed from the partition key, for any TotalSegments in the modeled range.
Traces: dynamodb-scan-total-segments-4096-pins-number-partition-keys#task#6 (cluster also covers #task#8 and #task#9).

Cause 2 of 3 - a segmented Scan returns an empty page where AWS returns that segment's items.
AWS: recorded behavior only (the model pins no placement): the recorded segmented Scan over a seeded table answers 200 with Count 2 / ScannedCount 2 and the two items belonging to the scanned segment.
Emulator: 200 with Count 0 / ScannedCount 0 and no Items member at all.
Why: same client consequence as Cause 1 - that segment's worker has nothing to do, so the rows it owned are dropped from the parallel scan's overall result.
Fix: as Cause 1 - segment assignment in the Scan handler; this is the empty-page half of the broken partition.
Traces: dynamodb-trajwf-parallel-scan-segment-contract#task#6.

Cause 3 of 3 - a segmented Scan ignores Segment/TotalSegments and returns the whole table.
AWS: recorded behavior only: the recorded segmented Scan answers 200 with Count 1 / ScannedCount 1 - only the single item belonging to that segment.
Emulator: 200 with Count 3 / ScannedCount 3, i.e. every item in the table, with the other two items appearing as extra Items entries - Segment/TotalSegments is not applied to the scanned set for this request.
Why: combined with Causes 1 and 2 a parallel scan both drops rows and processes rows more than once; any fan-out that assumes disjoint segments double-processes records and the aggregate Count is wrong.
Fix: as Cause 1 - whenever TotalSegments is present the Scan handler must restrict the scanned set to the requested Segment before applying Limit and filters.
Traces: dynamodb-trajwf-parallel-scan-segment-contract#task#7.

Related open issue: 20261120T225636Z-dynamodb-scan-partition-ordering-and-parallel-scan-segment.md describes the same family of Scan placement defects, but the replay report binds none of these three clusters to it (owned_by is empty for all three: its Issue-Trace-Ids name bare fixture ids with no step, so they do not join a step key), so these causes are not gated by it and are filed here with their own replay keys.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout. All three clusters are verdict=mismatch with empty stub_path and empty owned_by; status is 200 -> 200 in every case, so the divergence is entirely in the served item set.

Cause 1 - fixture dynamodb-scan-total-segments-4096-pins-number-partition-keys, step task#6 (affected_count 3; also task#8, task#9):
  expected status 200, actual 200
  $.Count: expected 1, got 0
  $.ScannedCount: expected 1, got 0
  $.Items[0]: missing (AWS returned the item whose partition key belongs to the scanned segment; the emulator returned no Items)

Cause 2 - fixture dynamodb-trajwf-parallel-scan-segment-contract, step task#6 (affected_count 1):
  $.Count: expected 2, got 0
  $.ScannedCount: expected 2, got 0
  $.Items[0] and $.Items[1]: missing (both items of that segment absent)

Cause 3 - fixture dynamodb-trajwf-parallel-scan-segment-contract, step task#7 (affected_count 1):
  $.Count: expected 1, got 3
  $.ScannedCount: expected 1, got 3
  $.Items[0]: a different item than AWS returned
  $.Items[1], $.Items[2]: extra - the table's other two items are returned as well

Model citations: Scan.TotalSegments @range 1..1000000, Scan.Segment @range 0..999999, output Items -> ItemList, Count/ScannedCount Integer default 0 (dynamodb-20120810 Smithy model). No ordering or placement is modeled, so the per-segment membership requirement is the recorded behavior.

## Steps to Reproduce
1. Boot the emulator built from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: replay dynamodb-scan-total-segments-4096-pins-number-partition-keys step task#6 (then task#8, task#9) - seed the Number partition key and Scan with TotalSegments set high at the Segment that owns that key; assert Count=1, ScannedCount=1 and the item in Items.
3. Cause 2: replay dynamodb-trajwf-parallel-scan-segment-contract step task#6 - the segmented Scan that must return that segment's two items; assert Count=2 / ScannedCount=2.
4. Cause 3: replay dynamodb-trajwf-parallel-scan-segment-contract step task#7 - the segmented Scan that must return exactly one item; assert Count=1 / ScannedCount=1 and that the other segments' items are absent.
5. Cross-check the invariant directly: for a seeded table, scan every Segment of one TotalSegments fan-out and confirm the union is the whole table and no item appears twice.
6. All three causes must pass before closing.
