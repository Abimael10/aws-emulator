Issue-Source: trace-sweep-divergence
Issue-Trace: dynamodb-scan-total-segments-4096-pins-number-partition-keys
Issue-Step: task#6
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: dynamodb-scan-total-segments-4096-pins-number-partition-keys dynamodb-trajwf-parallel-scan-segment-contract
Issue-Shape-Id: com.amazonaws.dynamodb#DynamoDB_20120810
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*]
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*].id.S
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*].status.S
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*].timestamp.S
Issue-Key: dynamodb|Scan|2xx->2xx|$.Items[*].workflowId.S
Issue-Related: 20261111T221143Z-dynamodb-condition-key-and-filter-expression-defects-are.md
Issue-Commit: 71f59c7071ad8d7d30fc1c90098cde1ce626b621

# dynamodb: parallel Scan ignores Segment/TotalSegments partitioning, so a segment returns the whole table or nothing (3 causes)

> _Note: this issue was identified against the code base at commit `71f59c7071ad8d7d30fc1c90098cde1ce626b621`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
Scan

## Description
Three net-new wire divergences, all on dynamodb Scan, all 200/200 body mismatches in which the item set returned for a parallel-Scan segment is wrong. They share one root fault: the emulator does not partition the table's items across the requested TotalSegments, so the share it returns for a given Segment is unrelated to the share AWS returns. Every cause below must be closed before this issue is submitted.

Cause 1 of 3: a requested segment that owns an item returns no items at all (TotalSegments at the upper end of the range).
AWS: Smithy: Scan.Segment range min 0 max 999999, Scan.TotalSegments range min 1 max 1000000; Scan.Segment documentation — "For a parallel Scan request, Segment identifies an individual segment to be scanned by an application worker... Segment IDs are zero-based... The value for Segment must be greater than or equal to 0, and less than the value provided for TotalSegments." The model does not state the partitioning function, so for the placement itself this is recorded behavior only: with a fixed TotalSegments (4096 in the recording) AWS deterministically pins each partition key to exactly one segment, and the recorded 200 response for the segment that owns the numeric partition key carries Count 1, ScannedCount 1 and that one item.
Emulator: the same request answers 200 with Count 0, ScannedCount 0 and no Items — the item that belongs to the requested segment is reported by no segment it was asked for.
Why: a client doing a parallel scan (every worker a distinct Segment, same TotalSegments) silently loses rows. The scan completes with no error and no LastEvaluatedKey, so the loss is indistinguishable from an empty table and corrupts any full-table sweep, migration or re-index built on parallel Scan.
Fix: the Scan handler's parallel-segment selection — the predicate that decides, for a given Segment/TotalSegments pair, which stored items a scan visits (its segment/hash assignment over the item's partition key), not the filtering or projection that runs after it.
Traces: dynamodb-scan-total-segments-4096-pins-number-partition-keys#task#6 (cluster also covers that fixture's task#8 and task#9).

Cause 2 of 3: a segment that owns several items returns none of them.
AWS: recorded behavior only for the placement (see Cause 1 for the modeled Segment/TotalSegments contract). The recording shows a small table scanned with a low TotalSegments: the requested segment answers 200 with Count 2, ScannedCount 2 and the two items that AWS assigned to it.
Emulator: answers 200 with Count 0, ScannedCount 0 and no Items for that same segment.
Why: same client harm as Cause 1 and the complement of Cause 3 — the union of all segments is not the table, so a parallel sweep under-reads without any signal.
Fix: same component as Cause 1 — the Scan handler's segment assignment, which must place every item in exactly one of the TotalSegments buckets so the segments partition the table.
Traces: dynamodb-trajwf-parallel-scan-segment-contract#task#6.

Cause 3 of 3: a segment returns every item in the table instead of its own share.
AWS: recorded behavior only for the placement (see Cause 1). The recording shows the requested segment answering 200 with Count 1, ScannedCount 1 and the single item AWS assigned to it, while the table holds three items.
Emulator: answers 200 with Count 3, ScannedCount 3 and all three items — the first item differs from the expected one and two further items are extra, i.e. the response is the unsegmented scan result.
Why: a parallel-scan client processes the same rows once per worker. Every row is duplicated TotalSegments-fold, so counters, exports and any at-most-once downstream write are wrong, and ScannedCount-based capacity/progress estimates are inflated.
Fix: same component as Cause 1 — the Scan handler must apply the Segment/TotalSegments restriction to the candidate item set; returning the full table when TotalSegments is present is the defect. Hypothesis: all items are currently assigned to a single bucket (so one segment sees everything and the others see nothing), which would explain Causes 1-3 together; the fix is one consistent partitioning over the item's partition key.
Traces: dynamodb-trajwf-parallel-scan-segment-contract#task#7.

Not covered by any open issue: the replay reports these three clusters as unowned. The nearest open issues are "DynamoDB data-plane results diverge: transaction cancellations, batch and PartiQL errors, scan order and consumed capacity" (20261117T221515Z-dynamodb-data-plane-results-diverge-transaction-cancellations-batch.md) and "dynamodb: 3 net-new causes — Scan's Limit=0 refusal uses Query's wording..." (20261112T161524Z-dynamodb-3-net-new-causes-scan-s-limit.md); neither addresses Segment/TotalSegments partitioning. No status or error code diverges here — all three are 200 responses with wrong item sets.

## Evidence
All three steps replayed 200 vs expected 200; only the body diverges.

Cause 1 — dynamodb-scan-total-segments-4096-pins-number-partition-keys#task#6 (Scan, expected 200, actual 200):
  $.Count: expected 1, got 0
  $.ScannedCount: expected 1, got 0
  $.Items[0]: missing (expected the one item AWS pins to the requested segment, recorded as {'pk': {'N': '300300'}})
  Same cluster: task#8, task#9 of that fixture.

Cause 2 — dynamodb-trajwf-parallel-scan-segment-contract#task#6 (Scan, expected 200, actual 200):
  $.Count: expected 2, got 0
  $.ScannedCount: expected 2, got 0
  $.Items[0], $.Items[1]: missing (expected the two items AWS assigns to that segment)

Cause 3 — dynamodb-trajwf-parallel-scan-segment-contract#task#7 (Scan, expected 200, actual 200):
  $.Count: expected 1, got 3
  $.ScannedCount: expected 1, got 3
  $.Items[0]: wrong item (every member differs: the emulator returned the table's first item, not the segment's item)
  $.Items[1], $.Items[2]: extra (expected None) — the other two table items, i.e. the whole table was returned for one segment

Taken together: one segment of a parallel scan is served the entire table (Cause 3) while the other segments are served nothing (Causes 1 and 2), so the segments do not partition the table. The recorded item values, table names and the numeric key 300300 are that run's data filling the template; the requirement is that for any TotalSegments N the N per-segment result sets are disjoint and their union is the full scan result, with each item's segment fixed by its partition key.

## Steps to Reproduce
1. Replay the wire corpus against the emulator (replay_wire_corpus); trace set simparity-wire-aws-v1.
2. Cause 1: fixture dynamodb-scan-total-segments-4096-pins-number-partition-keys, step task#6 — create the table, put the probe items, then Scan with TotalSegments=4096 and the Segment that AWS pins the numeric partition key to. Expected Count/ScannedCount 1 with that item; the emulator returns Count/ScannedCount 0 and no Items. Steps task#8 and task#9 of the same fixture fail identically.
3. Cause 2: fixture dynamodb-trajwf-parallel-scan-segment-contract, step task#6 — Scan the 3-item workflow table with the small TotalSegments and the segment that owns two items. Expected Count/ScannedCount 2 with both items; the emulator returns Count/ScannedCount 0 and no Items.
4. Cause 3: fixture dynamodb-trajwf-parallel-scan-segment-contract, step task#7 — Scan the same table with the same TotalSegments and the sibling Segment, which owns one item. Expected Count/ScannedCount 1 with that item; the emulator returns Count/ScannedCount 3 and all three items.
5. General check after a fix: for any table and any TotalSegments N, issue the N Scans with Segment 0..N-1 and confirm the result sets are pairwise disjoint and their union equals an unsegmented Scan of the same table, and that repeating a segment returns the same item set.
