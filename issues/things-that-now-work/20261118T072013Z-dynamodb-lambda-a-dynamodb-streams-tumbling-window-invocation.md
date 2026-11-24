Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__lambda.ddb-stream-esm-tumbling-window-state-carries-into-a-final-invoke-per-window
Issue-Key: dynamodb__lambda.ddb-stream-esm-tumbling-window-state-carries-into-a-final-invoke-per-window|run#15|AssertionError|assert _ts(window["start"]) <= r["ts"] < _ts(window["end"]), (
Issue-Commit: 92e7e832444d3251314187cac9454f231606c9e1

# dynamodb__lambda: a DynamoDB Streams tumbling-window invocation is stamped with a window interval that does not contain the records it carries

> _Note: this issue was identified against the code base at commit `92e7e832444d3251314187cac9454f231606c9e1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Cause 1 of 1: with a DynamoDB Streams event source mapping configured with TumblingWindowInSeconds, the emulator hands the handler an invocation whose `window` object ({start, end}) does not contain the ApproximateCreationDateTime of the stream records delivered in that same invocation. The window appears to be derived from the wall-clock moment the poller formed the batch rather than from the fixed, aligned window the record's creation time falls in, so a record created shortly BEFORE the window boundary is delivered inside the window that starts after it.

AWS: Smithy (com.amazonaws.lambda) CreateEventSourceMapping.TumblingWindowInSeconds — "(Kinesis and DynamoDB Streams only) The duration in seconds of a processing window for DynamoDB and Kinesis Streams event sources. A value of 0 seconds indicates no tumbling window." The model says nothing further about the event shape, so for the boundary rule this is recorded behavior only (reference the check cites: https://docs.aws.amazon.com/lambda/latest/dg/with-ddb-windows.html): Lambda partitions the stream into non-overlapping windows of the configured duration and each invocation's event carries the `window` ({start, end}) of the window being aggregated, with `state`, `shardId`, `isFinalInvokeForWindow` and `isWindowTerminatedEarly`. Every record delivered in that invocation belongs to that window, i.e. window.start <= record.dynamodb.ApproximateCreationDateTime < window.end. No status or error code is involved — the delivery is a 200/aggregating invoke; the divergence is in the event payload.

Emulator: the invocation carried a record whose ApproximateCreationDateTime is earlier than the window.start stamped on the same invocation. The replay reports: "AssertionError: record arrived at <epoch-seconds>, outside its window {'start': '<ISO start>Z', 'end': '<ISO end>Z'}" — the recorded run shows the record's creation time roughly two seconds before window.start, with window.end - window.start equal to the configured TumblingWindowInSeconds (15 s). The particular timestamps are per-run values; the requirement is the containment relation, not those instants.

Why: a handler that aggregates per window keys its state and its emitted results on event['window']. When a record is delivered under a window that does not contain it, the aggregate for that window counts records belonging to the previous window and the previous window's final invoke (isFinalInvokeForWindow) reports a total that omits them. Any client doing time-bucketed aggregation over a DynamoDB stream — the whole point of TumblingWindowInSeconds — produces wrong buckets, and the wrongness is invisible because every invocation looks well-formed.

Fix: the Lambda event-source-mapping poller's tumbling-window assignment for stream sources — the window stamped on an invocation must be the fixed window (aligned to TumblingWindowInSeconds) containing the ApproximateCreationDateTime of the records in that batch, and records must be grouped into invocations by that window, rather than the window being taken from the poller's own clock at batch time.

Traces: dynamodb__lambda.ddb-stream-esm-tumbling-window-state-carries-into-a-final-invoke-per-window

## Evidence
Check: dynamodb__lambda.ddb-stream-esm-tumbling-window-state-carries-into-a-final-invoke-per-window [mismatch]

Setup: a DynamoDB table with a stream, an ESM with StartingPosition=TRIM_HORIZON, BatchSize=2, TumblingWindowInSeconds=15; 8 items put; the handler forwards event['window'], event['state'], event['isFinalInvokeForWindow'], event['shardId'] and each record's dynamodb.Keys.id.S + dynamodb.ApproximateCreationDateTime to an SQS sink.

Expected (AWS): for every forwarded invocation and every record in it, window.start <= record.ApproximateCreationDateTime < window.end.

Actual (emulator), at line 118 of the check:
  AssertionError: record arrived at 1794984223, outside its window {'start': '2026-11-18T06:43:45Z', 'end': '2026-11-18T06:44:00Z'}
(window length 15 s matches the configured TumblingWindowInSeconds; the record's creation time precedes window.start by ~2 s.)

## Steps to Reproduce
1. Create a DynamoDB table with StreamSpecification enabled (NEW_AND_OLD_IMAGES).
2. Create a Lambda function whose handler echoes event['window'], event['state'], event['isFinalInvokeForWindow'], event['shardId'] and, for each record, record['dynamodb']['Keys']['id']['S'] and record['dynamodb']['ApproximateCreationDateTime'].
3. CreateEventSourceMapping(EventSourceArn=<table stream arn>, FunctionName=<fn>, StartingPosition='TRIM_HORIZON', BatchSize=2, TumblingWindowInSeconds=15).
4. PutItem 8 items in quick succession.
5. Collect the invocations and assert, for each record, window.start <= ApproximateCreationDateTime < window.end. The emulator delivers records whose creation time is outside the window stamped on their invocation.
