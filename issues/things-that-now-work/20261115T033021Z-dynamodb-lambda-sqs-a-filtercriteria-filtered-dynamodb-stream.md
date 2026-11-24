Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__lambda__sqs.ddb-stream-esm-filter-criteria-insert-only-not-invoked
Issue-Key: dynamodb__lambda__sqs.ddb-stream-esm-filter-criteria-insert-only-not-invoked|_collect#19|Timeout|resp = sqs.receive_message(
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# dynamodb__lambda__sqs: a FilterCriteria-filtered DynamoDB-stream event source mapping produces no invocation at all — the INSERT positive control never arrives and the check hangs to the budget

> _Resolution: the cause as REPORTED — "nothing is ever delivered", no invocation for the
> matching record — is NOT present at this commit. Driven through the recorded wiring, the
> mapping reads both records off the shard from `TRIM_HORIZON`, its one pattern admits the
> `INSERT` and rejects the `MODIFY`, the function is invoked exactly once with the admitted
> record, the handler's own `SendMessage` succeeds, and the message is received off the sink
> queue by a consumer long-polling it — after which the queue is empty and the mapping reports
> `Enabled` with an `OK` processing result. That whole chain is now pinned by
> `services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/dynamodb_stream_filtered_mapping_long_polling_consumer.rs`,
> over four wirings: the recorded insert-only shape, one with a record written behind the
> withheld change, one whose two ORed patterns admit a change and a removal with the rejected
> record at the head of the shard, and one whose clause names a value nested inside the changed
> item's key.
>
> What remains is the recorded check's own WALL CLOCK, which no AWS rule fixes: its collect step
> keeps long-polling the sink queue after the one message it was waiting for has arrived, and
> the harness's 60 s cap falls while it is inside `receive_message` — the `_collect#19|Timeout`
> key this report carries. The delivery itself lands about 7 s after the table is created, so
> the replay of this check will keep timing out until a later change makes the whole check fit
> the cap; the key is NOT evidence that a record is undelivered._

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Cause 1 of 1: a DynamoDB-stream event source mapping created with FilterCriteria never delivers the MATCHING records either, so the scenario's positive control (the INSERT record reaching the handler) never materialises and the check runs out its budget without completing.

AWS:      Smithy (lambda, restJson1, POST /2015-03-31/event-source-mappings, 202): CreateEventSourceMapping takes EventSourceArn, FunctionName, StartingPosition (EventSourcePosition enum, here TRIM_HORIZON), BatchSize (1..10000), MaximumBatchingWindowInSeconds (0..300) and FilterCriteria (Filters[].Pattern), and echoes FilterCriteria plus State/StateTransitionReason on the EventSourceMappingConfiguration output. For a DynamoDB stream the poller reads the shard from the StartingPosition and evaluates each record against the patterns: records that MATCH are batched and the function IS invoked with them; records that do not match are dropped. With records written before the mapping exists and TRIM_HORIZON, AWS replays the shard from its start, so a filter of {"eventName":["INSERT"]} over an INSERT followed by a MODIFY on the same key invokes the function exactly once, with the INSERT record (Keys.pk present), and never with the MODIFY.

Emulator: after CreateTable(StreamEnabled, NEW_AND_OLD_IMAGES) goes ACTIVE with a LatestStreamArn, two PutItems on the same key, CreateEventSourceMapping with the INSERT filter, and the mapping reporting State=Enabled, nothing is ever delivered: the output SQS queue the handler forwards to stays empty, so the wait for the INSERT control record never settles and the check is cut off — `Check timed out after 60.0s`. The mapping is accepted and reports Enabled, which makes the absence of delivery a silent failure rather than a refusal.

Why:      a client that relies on a filtered stream mapping gets a handler that is wired, reports Enabled, and is never invoked. Insert-only pipelines (projection builders, outbox publishers, audit fan-out) silently process nothing; because the ESM advertises Enabled and LastProcessingResult never reports a problem, there is no signal to retry or alarm on, and the records are left unconsumed in the shard.

Fix:      the lambda event-source-mapping poller for DynamoDB streams — once the mapping reaches Enabled it must read the stream shard from the configured StartingPosition, evaluate FilterCriteria per record, and invoke the function with the batches of matching records (dropping only the non-matching ones).

Related open issue that does NOT carry this failure's shape: 20261107T173512Z-dynamodb-lambda-sqs-an-event-source-mapping-s.md names the same check but for the OPPOSITE symptom — the filter being ignored so a MODIFY record leaked through. Here no record of any eventName is delivered, so the filter can no longer be observed at all; the delivery gap must be closed before the filter behaviour it describes can be re-verified.

Traces:   dynamodb__lambda__sqs.ddb-stream-esm-filter-criteria-insert-only-not-invoked.

## Evidence
Expected: with FilterCriteria {"Filters": [{"Pattern": "{\"eventName\":[\"INSERT\"]}"}]} on a DynamoDB-stream ESM reading from TRIM_HORIZON, the function is invoked with the INSERT record (so the handler forwards {"eventName": "INSERT", "pk": "row-1"} to the output queue) and never with the MODIFY record.

Actual (replay): `Check timed out after 60.0s`. The only blocking waits in the check are (a) the table becoming ACTIVE with a LatestStreamArn, (b) the ESM reaching State=Enabled, and (c) the wait for a queue message whose body has eventName == "INSERT". No message of any eventName is ever received, so the positive control — `assert len(ctl) >= 1, "control: the INSERT record must be delivered to the function"` — is never satisfied and the negative assertion on MODIFY leakage is never reached.

The diverging requirement is that a matching DynamoDB stream record must produce an invocation. The table/function/queue names and the key value "row-1" are per-run values from the request; the 60 s figure is the harness budget, not a requirement.

## Steps to Reproduce
1. sqs CreateQueue (the handler's output sink).
2. dynamodb CreateTable: KeySchema [{pk, HASH}], AttributeDefinitions [{pk, S}], BillingMode PAY_PER_REQUEST, StreamSpecification {StreamEnabled: true, StreamViewType: NEW_AND_OLD_IMAGES}. Poll DescribeTable until TableStatus ACTIVE and LatestStreamArn is present.
3. Create a python3.12 Lambda whose handler forwards each record's eventName and dynamodb.Keys.pk to the queue, with an execution role allowing dynamodb:DescribeStream/GetRecords/GetShardIterator/ListStreams on the stream ARN and sqs:SendMessage on the queue.
4. BEFORE creating the mapping: PutItem {pk: row-1, v: first} then PutItem {pk: row-1, v: second} (an INSERT followed by a MODIFY on the same key).
5. lambda CreateEventSourceMapping(EventSourceArn=<LatestStreamArn>, FunctionName=<fn>, StartingPosition=TRIM_HORIZON, BatchSize=10, MaximumBatchingWindowInSeconds=1, FilterCriteria={"Filters":[{"Pattern":"{\"eventName\":[\"INSERT\"]}"}]}). Poll GetEventSourceMapping until State == "Enabled".
6. Long-poll ReceiveMessage on the queue. Observe that no message ever arrives — not the INSERT that must be delivered, nor any other — while the mapping continues to report State Enabled.
