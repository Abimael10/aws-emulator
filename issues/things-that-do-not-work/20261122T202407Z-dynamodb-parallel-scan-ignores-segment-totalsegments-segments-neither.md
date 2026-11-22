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
Issue-Commit: 02c6380a70f664dba771786c55045481408bc4cc

# DynamoDB parallel Scan ignores Segment/TotalSegments: segments neither partition nor cover the table (3 causes)

> _Note: this issue was identified against the code base at commit `02c6380a70f664dba771786c55045481408bc4cc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Scan

## Description
THIS BUNDLE CONTAINS 3 CAUSES. All three are Scan requests that carry Segment + TotalSegments; all three answer HTTP 200 in both AWS and the emulator, and all three diverge in the item SET the segment serves. Taken together the emulator's segments do not partition the table: one segment is served empty while another is served the whole table, so a parallel fan-out both loses and double-processes items.

Cause 1 of 3 — with TotalSegments at 4096, the segment that owns a Number partition key is served empty. Fixture dynamodb-scan-total-segments-4096-pins-number-partition-keys, step task#6 (same cause at task#8 and task#9).
AWS: Smithy: Scan.TotalSegments -> ScanTotalSegments @range 1..1000000, Scan.Segment -> ScanSegment @range 0..999999; ScanOutput.Count/ScannedCount are Integer (default 0) and Items is ItemList. Smithy doc for ScanInput$Segment: "For a parallel Scan request, Segment identifies an individual segment to be scanned by an application worker ... The value for Segment must be greater than or equal to 0, and less than the value provided for TotalSegments." The segment set is therefore a partition of the table: every item belongs to exactly one Segment and the union of all Segments is the whole table. Which segment a given partition key lands in is recorded behavior only (the model does not state the hash); the recording scans the one segment that owns a seeded Number partition key and gets Count 1, ScannedCount 1 and that single item.
Emulator: answers 200 with Count 0, ScannedCount 0 and no Items member — the item is placed in some other segment, so the segment the client computed for that key is empty.
Why: a client that fans a scan out over TotalSegments workers and computes the owning segment for a key never sees that key; at 4096 segments the loss is silent, because every worker legitimately gets an empty page sometimes. This is item loss on a read path, not a cosmetic difference.
Fix: the Segment/TotalSegments assignment function used by the Scan handler's read path — item -> segment must be a stable partition of the table, agreeing with AWS's placement.
Traces: dynamodb-scan-total-segments-4096-pins-number-partition-keys#task#6 (representative), #task#8, #task#9.

Cause 2 of 3 — a segmented Scan serves an empty page where the segment owns two items. Fixture dynamodb-trajwf-parallel-scan-segment-contract, step task#6.
AWS: as Cause 1 (Smithy Segment/TotalSegments ranges and the Segment doc above); the per-segment membership is recorded behavior only. The recording returns Count 2, ScannedCount 2 and the two items belonging to that segment.
Emulator: 200 with Count 0, ScannedCount 0 and both Items entries absent.
Why: the worker assigned to that segment processes nothing, so the parallel scan as a whole skips those items even though the client issued every segment.
Fix: as Cause 1 — segment membership in the Scan handler; this is the empty-page half of the broken partition.
Traces: dynamodb-trajwf-parallel-scan-segment-contract#task#6.

Cause 3 of 3 — a segmented Scan ignores the Segment/TotalSegments pair entirely and returns the whole table. Fixture dynamodb-trajwf-parallel-scan-segment-contract, step task#7.
AWS: as Cause 1; the recording returns Count 1, ScannedCount 1 and only the single item belonging to the requested segment.
Emulator: 200 with Count 3, ScannedCount 3 — every item in the table is returned, i.e. the request's Segment/TotalSegments is not applied to the scanned set at all.
Why: combined with Cause 2, a parallel scan processes some records more than once while dropping others; any fan-out that relies on segments being disjoint duplicates work and can double-apply side effects.
Fix: as Cause 1 — the Scan handler must restrict the scanned set to the requested Segment whenever TotalSegments is present, before Limit/pagination is applied.
Traces: dynamodb-trajwf-parallel-scan-segment-contract#task#7.

Related open issue that does NOT own these replay steps: 20261120T225636Z-dynamodb-scan-partition-ordering-and-parallel-scan-segment.md enumerates the same three behaviors as its Causes 3-5 and names both fixtures in its Issue-Trace-Ids, but the replay report returns owned_by = [] for all three clusters here (its Issue-Key list covers only $.Items[*] member paths, not the $.Count / $.ScannedCount keys these clusters diverge on), so they are filed here with the exact fixture#step pointers a re-replay needs. A fixer who corrects the segment-assignment function should close both issues together; the unfiltered-scan ORDERING causes of that issue (Causes 1-2) are a different defect and are not re-filed here.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout. All three clusters: verdict mismatch, expected_status 200, actual_status 200, stub_path empty, owned_by empty.

Cause 1 — cluster service=dynamodb|Scan|status=200->200|keys=$.Count,$.Items[0],$.ScannedCount, fixture dynamodb-scan-total-segments-4096-pins-number-partition-keys#task#6 (3 affected steps: task#6, task#8, task#9):
  $.Count: expected 1, got 0
  $.ScannedCount: expected 1, got 0
  $.Items[0]: missing — expected the single item whose Number partition key owns the scanned segment, got None

Cause 2 — cluster service=dynamodb|Scan|status=200->200|keys=$.Count,$.Items[0],$.Items[1],$.ScannedCount, fixture dynamodb-trajwf-parallel-scan-segment-contract#task#6 (1 affected step):
  $.Count: expected 2, got 0
  $.ScannedCount: expected 2, got 0
  $.Items[0], $.Items[1]: missing (expected the two items of that segment, got None)

Cause 3 — cluster service=dynamodb|Scan|status=200->200|keys=$.Count,$.Items[0].id.S,$.Items[0].status.S,$.Items[0].timestamp.S,$.Items[0].workflowId.S,$.Items[1],$.Items[2],$.ScannedCount, fixture dynamodb-trajwf-parallel-scan-segment-contract#task#7 (1 affected step):
  $.Count: expected 1, got 3
  $.ScannedCount: expected 1, got 3
  $.Items[0] is a different item than AWS served (every member of the first entry diverges), and $.Items[1] / $.Items[2] are extra — the other two table items are returned as well.

Model citations used: Smithy Scan input members TotalSegments (ScanTotalSegments, @range min 1 max 1000000) and Segment (ScanSegment, @range min 0 max 999999); ScanOutput members Items (ItemList), Count and ScannedCount (Integer, default 0); ScanInput$Segment documentation ("Segment identifies an individual segment to be scanned by an application worker ... must be greater than or equal to 0, and less than the value provided for TotalSegments"). The model states no item -> segment mapping, so the specific placement is recorded behavior; the partition/coverage property is what the model's definition of a parallel scan requires.

## Steps to Reproduce
1. Boot the emulator built from this checkout and replay the wire corpus (replay_wire_corpus).
2. Cause 1: replay dynamodb-scan-total-segments-4096-pins-number-partition-keys steps task#6, task#8 and task#9 — seed a table with Number partition keys, then Scan with TotalSegments 4096 at the Segment that owns the seeded key. Expect Count 1 / ScannedCount 1 and that item in Items; the emulator returns Count 0 / ScannedCount 0 and no Items.
3. Cause 2: replay dynamodb-trajwf-parallel-scan-segment-contract step task#6 — the segmented Scan whose segment owns two items. Expect Count 2 / ScannedCount 2 with both items; the emulator returns an empty page.
4. Cause 3: replay dynamodb-trajwf-parallel-scan-segment-contract step task#7 — the segmented Scan whose segment owns exactly one item. Expect Count 1 / ScannedCount 1 with only that item; the emulator returns all three table items.
5. Cross-check the invariant after fixing: issue every Segment from 0..TotalSegments-1 for the same table and confirm the concatenated result contains each item exactly once (no item missing, no item in two segments).
6. All three steps above must pass before this issue is closed.
