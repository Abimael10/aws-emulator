Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis__lambda.kinesis-esm-tumbling-window-carries-state-and-closes-each-window-once
Issue-Key: kinesis__lambda.kinesis-esm-tumbling-window-carries-state-and-closes-each-window-once|run#15|AssertionError|assert _ts(window["start"]) <= r["ts"] < _ts(window["end"]), (
Issue-Commit: 37301ce23688d3043715086de5be7f765b8fadfc

# kinesis__lambda: a Kinesis ESM tumbling window assigns records to a window that does not contain their approximate arrival time

> _Note: this issue was identified against the code base at commit `37301ce23688d3043715086de5be7f765b8fadfc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
One net-new root cause in the Kinesis event-source-mapping tumbling-window accumulator.

Cause 1 of 1: a record is delivered inside a window whose `start` is later than the record's own `approximateArrivalTimestamp`, so the record is attributed to the wrong window.
AWS: Smithy: CreateEventSourceMapping.TumblingWindowInSeconds, @range min 0 max 900, documented as '(Kinesis and DynamoDB Streams only) The duration in seconds of a processing window for DynamoDB and Kinesis Streams event sources. A value of 0 seconds indicates no tumbling window.' The window semantics are not in the model, so: recorded behavior only (https://docs.aws.amazon.com/lambda/latest/dg/services-kinesis-windows.html) — each invocation's event carries a per-shard `window` object of span exactly TumblingWindowInSeconds, and every record is delivered exactly once, in the window that CONTAINS its approximate arrival time. The invariant a consumer relies on is `window.start <= record.kinesis.approximateArrivalTimestamp < window.end` for every record of every invocation of that window; windows are contiguous and non-overlapping, so a record whose arrival falls in [T, T+W) must be delivered under the window starting at T, never under the next one.
Emulator: the window bookkeeping now exists (the event carries window/state/shardId/isFinalInvokeForWindow, the span is exactly TumblingWindowInSeconds, state threads across invocations and the window closes with one record-less final invocation — so this is no longer the inert-window gap open issue 20261115T090642Z-kinesis-lambda-2-net-new-causes-a-kinesis.md describes, and that issue does not carry this check), but the record-to-window assignment is wrong: a record whose approximateArrivalTimestamp falls just BEFORE a window boundary is delivered under the window that starts at that boundary. In the replay a record with approximateArrivalTimestamp 1794978529.833 (2026-11-18T05:08:49.833Z) was delivered in the window {'start': '2026-11-18T05:08:50Z', 'end': '2026-11-18T05:09:00Z'}, i.e. 0.17 s before its window's start, when the window containing it is the preceding 10-second window. Hypothesis: the accumulator buckets records by the time the poller read/batched them (or by the time the window was opened) rather than by each record's own approximateArrivalTimestamp. No HTTP refusal is involved — CreateEventSourceMapping echoes TumblingWindowInSeconds correctly and every call succeeds.
Why: a tumbling-window consumer exists to compute per-window aggregates. When records cross into the neighbouring window, every aggregate is wrong at the boundaries — a count/sum attributed to the wrong window, and a window already closed by its final invocation can never receive the record it should have held. The handler gets no error, so the aggregate is silently off, which is worse than a refusal.
Fix: the Kinesis poller of the Lambda event-source-mapping data plane — the tumbling-window accumulator's record-to-window bucketing step, which must place each record in the window containing its approximateArrivalTimestamp (and derive the window bounds from that timestamp), not the invoke-event builder's window serialization and not CreateEventSourceMapping's validation, both of which already behave.
Traces: kinesis__lambda.kinesis-esm-tumbling-window-carries-state-and-closes-each-window-once.

## Evidence
Check kinesis__lambda.kinesis-esm-tumbling-window-carries-state-and-closes-each-window-once, [mismatch]:
Setup: a Kinesis stream and a function whose handler echoes event['window'], event['state'], event['shardId'], event['isFinalInvokeForWindow'], event['isWindowTerminatedEarly'] and, per record, kinesis.sequenceNumber and kinesis.approximateArrivalTimestamp; CreateEventSourceMapping with StartingPosition=TRIM_HORIZON, BatchSize=2 and TumblingWindowInSeconds=10 (echoed back correctly); six records put to the stream.
Expected (AWS): for every delivered record, window.start <= record approximateArrivalTimestamp < window.end.
Actual: AssertionError: record arrived at 1794978529.8331819, outside its window {'start': '2026-11-18T05:08:50Z', 'end': '2026-11-18T05:09:00Z'} — the record's arrival precedes its window's start, so it belongs in the preceding 10-second window.
The earlier assertions in the same check pass: the mapping echoes TumblingWindowInSeconds, no invocation lacks a `window`, the window span equals TumblingWindowInSeconds, state threads from each invocation to the next, each window closes with exactly one record-less isFinalInvokeForWindow invocation, and no window terminates early. (The epoch timestamp, the ISO window bounds and the record count are this run's values, not requirements; the requirement is the containment invariant.)

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite; the check id in trace_ids fails.
2. Create a Kinesis stream plus a second stream (or any side channel) and a python function whose handler writes out event.get('window'), event.get('state'), event.get('shardId'), event.get('isFinalInvokeForWindow') and, per record, record['kinesis']['sequenceNumber'] and record['kinesis']['approximateArrivalTimestamp'], returning {'state': <accumulator>}.
3. CreateEventSourceMapping with EventSourceArn the first stream, StartingPosition=TRIM_HORIZON, BatchSize=2 and TumblingWindowInSeconds=10.
4. PutRecord a handful of records so that at least one arrives within a fraction of a second before a 10-second window boundary.
5. Read the emitted invocations and compare each record's approximateArrivalTimestamp with the window it was delivered under: on AWS every record satisfies window.start <= ts < window.end; on the emulator a record arriving just before a boundary is delivered under the window that starts at that boundary.
6. Re-replay exactly the check id in trace_ids to confirm a fix.
