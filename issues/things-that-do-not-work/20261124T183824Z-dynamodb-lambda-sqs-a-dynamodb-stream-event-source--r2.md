Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: dynamodb__lambda__sqs.ddb-stream-esm-filter-criteria-insert-only-not-invoked
Issue-Key: dynamodb__lambda__sqs.ddb-stream-esm-filter-criteria-insert-only-not-invoked|_collect#19|Timeout|resp = sqs.receive_message(
Issue-Commit: 8b958dd9ac2b7b6aba3fca63da2d876136547e6d

# dynamodb__lambda__sqs: a DynamoDB-stream event source mapping with FilterCriteria eventName=[INSERT] reaches no verdict — neither the admitted INSERT nor the suppressed MODIFY is observable inside the replay budget

> _Note: this issue was identified against the code base at commit `8b958dd9ac2b7b6aba3fca63da2d876136547e6d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
One net-new root cause.

Cause 1 of 1: a Lambda event source mapping whose EventSourceArn is a DynamoDB stream and which carries a FilterCriteria never reaches the state this check needs, so neither half of the filtering contract (the matching record IS handed to the function, the non-matching record is NOT) can be graded.

AWS: Smithy (services/service-com-amazonaws-lambda-awsgirapiservice) models CreateEventSourceMapping input member FilterCriteria -> com.amazonaws.lambda#FilterCriteria { Filters: FilterList of com.amazonaws.lambda#Filter { Pattern: @length(min 0, max 4096) @pattern "^[\\s\\S]*$" } }; DynamoDB's CreateTable models StreamSpecification with StreamViewType NEW_AND_OLD_IMAGES and DescribeTable reports Table.LatestStreamArn once the table is ACTIVE. Beyond the model this is recorded behavior only: the recording of this scenario completes well inside the budget — the table becomes ACTIVE and publishes a LatestStreamArn, CreateEventSourceMapping(StartingPosition=TRIM_HORIZON, BatchSize=10, MaximumBatchingWindowInSeconds=1, FilterCriteria=[{"eventName":["INSERT"]}]) converges to State 'Enabled', the shard is read from the horizon, the INSERT record for the key written first is delivered to the function (which forwards eventName/Keys.pk to a sink queue), and the MODIFY record written over the same key is read past and never handed to the function.

Emulator: the replay reports only `Error: Check timed out after 60.0s`. The check never returned, so no assertion in it was evaluated and no status, error code or response body can be quoted for any call. Its own internal deadlines are all larger than the harness budget — DescribeTable polled to TableStatus ACTIVE with LatestStreamArn at timeout_s=120.0/interval_s=3.0, GetEventSourceMapping polled to State 'Enabled' at timeout_s=180.0/interval_s=5.0, the control INSERT awaited at timeout_s=180.0, and a further 60.0 s leakage window — so the run is killed before any of them expires. Hypothesis: one of the first three waits never settles: either DescribeTable never reports a LatestStreamArn for a table created with StreamSpecification NEW_AND_OLD_IMAGES, or the stream mapping never converges to 'Enabled', or the TRIM_HORIZON poller never delivers the INSERT record to the function. The rest of this heading's SQS-source and plain-stream traffic is covered by other issues, so the filtered-stream path is what is unobserved here.

Why: a client that relies on FilterCriteria over a DynamoDB stream to suppress record types cannot confirm either half of the contract — that the admitted eventName is delivered with its Keys intact, or that the rejected eventName is read past rather than redelivered forever. A mapping that delivers nothing at all and one that filters exactly right are indistinguishable, and a caller that blocks on its first delivery before proceeding hangs.

Fix: the DynamoDB-stream event source mapping poller in the Lambda service crate — its StartingPosition=TRIM_HORIZON shard read, its FilterCriteria evaluation and hand-off of the matching record, and the mapping's convergence to State 'Enabled' — together with DynamoDB's publication of Table.LatestStreamArn for a stream-enabled table. Whichever of those three waits blocks is the component that owns the cause.

Traces: dynamodb__lambda__sqs.ddb-stream-esm-filter-criteria-insert-only-not-invoked, the only failure under this heading that is not already owned. Related but NOT covering it: open 20261120T001238Z-dynamodb-lambda-logs-the-dynamodb-stream-processing-failure-lifecycle-never-reaches.md (StartingPosition LATEST arming, no FilterCriteria), 20261114T143402Z-dynamodb-lambda-sqs-dynamodb-stream-event-source-mapping-over-reports-to.md (OnFailure destination over-reporting), 20261109T172142Z-dynamodb-lambda-sqs-dynamodb-stream-records-never-reach-net-java-or.md (.NET/Java/Ruby runtimes), 20261122T004908Z-dynamodb-lambda-sqs-the-sqs-queue-policy-deny.md (SQS source, not a stream), and 20261118T000000Z-lambda-sqs-the-filter-criteria-check-reaches-no-verdict.md (SQS-source FilterCriteria, whose filtering is pinned correct).

## Evidence
Check dynamodb__lambda__sqs.ddb-stream-esm-filter-criteria-insert-only-not-invoked (verdict: mismatch).

Expected (AWS): DescribeTable -> TableStatus 'ACTIVE' with a Table.LatestStreamArn; GetEventSourceMapping -> State 'Enabled'; the sink queue receives exactly one message whose body is {"eventName":"INSERT","pk":"row-1"}; no message with eventName 'MODIFY' ever arrives, although the MODIFY record was written to the stream BEFORE the mapping started reading from TRIM_HORIZON and therefore had to be read past.

Actual (emulator): `Error: Check timed out after 60.0s`. No assertion ran. The check's own waits are 120.0 s (table ACTIVE + LatestStreamArn), 180.0 s (mapping State='Enabled'), 180.0 s (control INSERT at the sink) and a 60.0 s leakage window, all of which exceed the 60 s harness cap, so the run was cut before any of them could fail with a diagnosis.

## Steps to Reproduce
Against the emulator's dynamodb/lambda/sqs endpoints (any region/account):
1. CreateQueue a sink queue; read its QueueArn with GetQueueAttributes.
2. CreateTable with KeySchema pk(HASH), BillingMode PAY_PER_REQUEST and StreamSpecification {StreamEnabled: true, StreamViewType: NEW_AND_OLD_IMAGES}. Poll DescribeTable until TableStatus == 'ACTIVE' and Table.LatestStreamArn is present (AWS settles well inside 120 s).
3. Create an execution role trusted by lambda.amazonaws.com granting dynamodb:DescribeStream/GetRecords/GetShardIterator/ListStreams on the stream ARN and sqs:SendMessage on the sink; CreateFunction (python3.12) whose handler forwards each record's eventName and dynamodb.Keys.pk to the sink queue.
4. PutItem pk=row-1 twice (an INSERT then a MODIFY over the same key) BEFORE creating the mapping.
5. CreateEventSourceMapping(EventSourceArn=<LatestStreamArn>, FunctionName=<fn>, StartingPosition='TRIM_HORIZON', BatchSize=10, MaximumBatchingWindowInSeconds=1, FilterCriteria={'Filters':[{'Pattern':'{"eventName":["INSERT"]}'}]}). Poll GetEventSourceMapping until State == 'Enabled'.
6. ReceiveMessage on the sink: exactly one message with eventName 'INSERT' and pk 'row-1' must arrive, and no message with eventName 'MODIFY' must ever arrive.
7. Replay dynamodb__lambda__sqs.ddb-stream-esm-filter-criteria-insert-only-not-invoked: the check is killed at 60 s with no verdict. Note which of steps 2, 5 or 6 does not settle.
