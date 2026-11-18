Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: eventbridgev2__iam__sqs.jsonata-transformer-reads-octet-stream-data-as-base64
Issue-Key: eventbridgev2__iam__sqs.jsonata-transformer-reads-octet-stream-data-as-base64|_warm_up#7|TimeoutError|raise TimeoutError("the run budget ended during warm-up; not graded")
Issue-Commit: c2a45cb39008eec8053ec65a5a3a807cace171f5

# eventbridgev2__iam__sqs: events published with PutRawEvents never reach a JSONATA-transformer subscriber, so the octet-stream Base64 contract cannot be measured

> _Note: this issue was identified against the code base at commit `c2a45cb39008eec8053ec65a5a3a807cace171f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutRawEvents

## Description
One net-new root cause in the eventbridgev2__iam__sqs heading. The open issue 20261113T094431Z-eventbridgev2-iam-sqs-9-net-new-causes-putevents.md covers the PutEvents-published paths (its Cause 3 lists the transformer subscriber fed by PutEvents) but carries no replay key for this check and never names PutRawEvents; this check's publish path is PutRawEvents with a binary Data blob and SystemMetadata.ContentType, which the emulator must ingest and hand to the subscriber's transformer.

Cause 1 of 1: a PutRawEvents entry is never delivered to a subscriber of the bus it was published to, so the JSONATA view of raw event data is unobservable.
AWS: Smithy — PutRawEvents(EventBusArn required, Entries 1..100 required) returns 200 with FailedEntryCount and one result entry per input entry; its modelled errors are InvalidStateException(409), ResourceNotFoundException(404), SchemaRegistryUnavailableException(400), AccessDeniedException(403), InternalException(500), InvalidInputException(400), ThrottlingException(429). PutRawEventsRequestEntry.Data is a required blob, "The event data as a base64-encoded blob. Supports binary formats", and SystemMetadata is required (it carries ContentType, here application/octet-stream). Subscriber.Transformer with Type JSONATA and JsonataConfiguration.Expression ("JSONata expression to transform the event. Must be wrapped in {% %} delimiters") is evaluated per delivered event and the result is what the SQS target receives. Recorded behavior: for an application/octet-stream entry, $events.Data is the Base64 TEXT of the raw bytes and is never parsed, so the expression {% $type($events.Data) & ':' & $events.Data %} delivers the single string "string:" + base64(Data) — "string:" is fixed text produced by $type, the remainder is the request's own bytes Base64-encoded — even when those bytes decode to JSON.
Emulator: PutRawEvents is accepted, but no event is ever delivered to the subscriber's SQS target. The check's warm-up loop republishes a marker entry every 5 s and polls the target queue; nothing arrives and the run budget is consumed — "TimeoutError: the run budget ended during warm-up; not graded" raised at _warm_up, before the graded PutRawEvents is even issued.
Why: a producer that publishes binary or non-JSON payloads through PutRawEvents gets an accepted response (FailedEntryCount 0) and silent loss: subscribers of the bus receive nothing and there is no error or dead-letter record to explain it. The transformer's documented treatment of raw data as Base64 text is also unobservable, so a consumer cannot tell a dropped event from a mis-transformed one.
Fix: the eventbridgev2 PutRawEvents ingestion path and the bus-to-subscriber dispatcher — a raw entry must enter the bus's event stream like a PutEvents entry, expose its Data to the subscriber's JSONATA transformer as Base64 text per SystemMetadata.ContentType, and be delivered to the subscriber's SQS target.
Traces: eventbridgev2__iam__sqs.jsonata-transformer-reads-octet-stream-data-as-base64.

## Evidence
Check eventbridgev2__iam__sqs.jsonata-transformer-reads-octet-stream-data-as-base64, verdict [error]:
  File "check_eventbridgev2__iam__sqs_jsonata_transformer_reads_octet_stream_data_as_base64.py", line 246, in run
    _warm_up(ctx, lambda m: ebv2.put_raw_events(EventBusArn=bus_arn, Entries=[{"Data": m.encode(), "SystemMetadata": {"ContentType": "application/octet-stream"}}]), sqs, [text_url])
  File "...", line 213, in _warm_up
    raise TimeoutError("the run budget ended during warm-up; not graded")
  TimeoutError: the run budget ended during warm-up; not graded

Setup before that point: CreateEventBus (ACTIVE), an SQS queue, a delivery role granting sqs:SendMessage on that queue, and CreateSubscriber with InvokeConfiguration {RoleArn, TargetArn, SqsParameters:{}} and Transformer {Type: JSONATA, JsonataConfiguration: {Expression: "{% $type($events.Data) & ':' & $events.Data %}"}}.
Expected: every PutRawEvents entry is delivered, so the warm-up marker arrives on the queue and the graded entry Data=b'{"marker": "<marker>"}' with ContentType application/octet-stream is delivered as exactly one body equal to "string:" + base64(Data).
Actual: no body ever arrives on the target queue. Bus/queue/role ARNs and the marker are per-run values.

## Steps to Reproduce
1. Build and boot the emulator from this checkout and replay the behavioral suite.
2. CreateEventBus and wait for State ACTIVE; create an SQS queue and an IAM role the bus may assume with sqs:SendMessage on that queue.
3. CreateSubscriber(EventBusArn=<bus>, InvokeConfiguration={RoleArn, TargetArn: <queue arn>, SqsParameters: {}}, Transformer={Type: "JSONATA", JsonataConfiguration: {Expression: "{% $type($events.Data) & ':' & $events.Data %}"}}).
4. Call PutRawEvents(EventBusArn=<bus>, Entries=[{Data: b'{"marker": "m1"}', SystemMetadata: {ContentType: "application/octet-stream"}}]) and check FailedEntryCount == 0.
5. Poll the target queue: AWS delivers one body "string:" + base64(b'{"marker": "m1"}'); the emulator delivers nothing, for this entry or for any repeated publish.
6. Re-replay eventbridgev2__iam__sqs.jsonata-transformer-reads-octet-stream-data-as-base64 to confirm the fix.
