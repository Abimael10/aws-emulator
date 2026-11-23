Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kinesis__lambda__s3.kinesis-esm-s3-on-failure-destination-writes-the-full-invocation-record
Issue-Key: kinesis__lambda__s3.kinesis-esm-s3-on-failure-destination-writes-the-full-invocation-record|_poll_until#13|TimeoutError|raise TimeoutError(f"{description} not observed within {timeout_s:.0f}s; last saw {last!r}")
Issue-Commit: 68c71f6585a425304635c9d8c1cc1d9408beb2ea

# kinesis__lambda__s3: a Kinesis ESM with an S3 OnFailure destination never writes the invocation record object, so the exhausted batch is lost silently

> _Note: this issue was identified against the code base at commit `68c71f6585a425304635c9d8c1cc1d9408beb2ea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateEventSourceMapping

## Description
Cause 1 of 1: a Lambda event source mapping over a Kinesis stream whose DestinationConfig.OnFailure.Destination names an S3 bucket ARN produces NO object in that bucket after the mapped function exhausts its retries. Failing check: kinesis__lambda__s3.kinesis-esm-s3-on-failure-destination-writes-the-full-invocation-record.

AWS: Smithy (com.amazonaws.lambda) models CreateEventSourceMapping.DestinationConfig -> DestinationConfig.OnFailure -> OnFailure.Destination (DestinationArn, @length min 0 max 350, pattern '^$|kafka://...|arn:(aws[a-zA-Z0-9-]*):...'), documented as: "To retain records of failed invocations from Kinesis, DynamoDB, self-managed Kafka or Amazon MSK, you can configure an Amazon SNS topic, Amazon SQS queue, or Amazon S3 bucket as the destination." For a Kinesis source whose batch exhausts MaximumRetryAttempts, AWS writes exactly one S3 object per exhausted batch, keyed aws/lambda/<ESM-UUID>/<shardId>/<YYYY>/<MM>/<DD>/<YYYY-MM-DDTHH.MM.SS>-<uuid> — every variable part comes from the mapping and the failing batch (the mapping's UUID, the source shard id, the failure time) and the fixed parts are the literal 'aws/lambda/' prefix and the '/'-delimited date path. Its body carries the same pointer the SQS/SNS destinations get — KinesisBatchInfo with shardId, startSequenceNumber, endSequenceNumber, batchSize, streamArn, approximateArrivalOfFirstRecord, approximateArrivalOfLastRecord — plus requestContext (condition 'RetryAttemptsExhausted' when retries are exhausted, approximateInvokeCount, functionArn, requestId), responseContext (functionError 'Unhandled' for a raised exception), and, uniquely for an S3 destination, a 'payload' member holding the ENTIRE original invocation event (Records[].kinesis.data base64-encoded, sequenceNumber equal to the failed record's).

Emulator: the mapping is accepted (CreateEventSourceMapping returns a UUID; no refusal is recorded), the source record is put successfully and the handler is configured to raise, but ListObjectsV2 on the destination bucket under prefix aws/lambda/<ESM-UUID>/ returns an empty Contents for the whole polling window — the check fails with TimeoutError: "an OnFailure object under the ESM's S3 prefix not observed within 49s; last saw []". Nothing is ever written, so the key layout, the KinesisBatchInfo pointer, requestContext.condition/approximateInvokeCount/functionArn, responseContext.functionError and the S3-only full 'payload' member are all unobservable.

Why: a client that configures an S3 OnFailure destination to retain records of poison Kinesis batches gets an empty bucket. The failed batch is dropped with no durable record, so operational replay/triage tooling that reads these objects (and any alarm keyed on their existence) sees a healthy stream while records are being discarded. The silent-loss shape is worse than an error: CreateEventSourceMapping accepted the destination, so the client has no signal the retention never happens.

Fix: the Lambda event-source-mapping poller/failure-destination delivery path — the component that, on retry exhaustion for a stream (Kinesis/DynamoDB) source, builds the OnFailure invocation record and dispatches it to the configured destination, plus its S3 branch (key construction under aws/lambda/<UUID>/<shardId>/<date>/ and the S3-only embedded 'payload' member). Note the existing open issue 20261026T005239Z-dynamodb-kinesis-lambda-a-dynamodb-streams-event-source.md covers the DynamoDB-Streams DDBStreamBatchInfo OnFailure record and 20261029T180026Z-dynamodb-kinesis-lambda-event-source-mappings-are-created-for-non-existent.md covers ESMs created for non-existent/unpermitted sources; neither covers the Kinesis-source S3 OnFailure destination exercised here, which the suite did not mark owned.

Traces: kinesis__lambda__s3.kinesis-esm-s3-on-failure-destination-writes-the-full-invocation-record.

## Evidence
Expected (AWS): after the single-record batch exhausts MaximumRetryAttempts=0, exactly one object exists under the destination bucket at key aws/lambda/<ESM-UUID>/<shardId>/YYYY/MM/DD/YYYY-MM-DDTHH.MM.SS-<uuid>; its JSON body contains KinesisBatchInfo = {shardId: <the PutRecord ShardId>, startSequenceNumber: <the PutRecord SequenceNumber>, endSequenceNumber: <the same>, batchSize: 1, streamArn: <the stream's ARN>} plus approximateArrivalOfFirstRecord/approximateArrivalOfLastRecord; requestContext.condition = "RetryAttemptsExhausted", requestContext.approximateInvokeCount = 1, requestContext.functionArn naming the function; responseContext.functionError = "Unhandled"; and a 'payload' member whose Records[0].kinesis.sequenceNumber equals the put record's sequence number and whose Records[0].kinesis.data base64-decodes to the original record body.

Actual (emulator): s3.list_objects_v2(Bucket=<dest>, Prefix="aws/lambda/<ESM-UUID>/") returns no Contents for the entire poll. Replay output:
  File "check_kinesis__lambda__s3_kinesis_esm_s3_on_failure_destination_writes_the_full_invocation_record.py", line 68, in run
    objs = _poll_until(
  File "...", line 149, in _poll_until
    raise TimeoutError(f"{description} not observed within {timeout_s:.0f}s; last saw {last!r}")
  TimeoutError: an OnFailure object under the ESM's S3 prefix not observed within 49s; last saw []

No AWS error code or non-2xx status is involved: CreateEventSourceMapping, PutRecord and ListObjectsV2 all return 200. The divergence is the absent side effect.

## Steps to Reproduce
1. Kinesis: CreateStream with ShardCount=1; wait for StreamStatus ACTIVE.
2. S3: CreateBucket for the destination.
3. IAM: create a Lambda execution role allowing kinesis:GetRecords/GetShardIterator/DescribeStream/DescribeStreamSummary/ListShards on the stream ARN and s3:PutObject/s3:ListBucket on the bucket and bucket/*.
4. Lambda: CreateFunction (python3.12) whose handler unconditionally raises RuntimeError("deliberate-failure"); wait for State=Active.
5. Kinesis: PutRecord(StreamName=<stream>, PartitionKey="k", Data=<json payload>); record the returned SequenceNumber and ShardId.
6. Lambda: CreateEventSourceMapping(FunctionName=<fn>, EventSourceArn=<stream ARN>, Enabled=true, StartingPosition=TRIM_HORIZON, BatchSize=1, MaximumBatchingWindowInSeconds=0, MaximumRetryAttempts=0, DestinationConfig={OnFailure:{Destination:"arn:aws:s3:::<bucket>"}}); record the UUID.
7. S3: poll ListObjectsV2(Bucket=<bucket>, Prefix="aws/lambda/<UUID>/").
   Observed: Contents stays empty until the budget expires. Expected: one object, keyed and bodied as above.
