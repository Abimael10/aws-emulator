Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: firehose__kinesis__s3.firehose-kinesis-source-missing-role-permission-negative
Issue-Key: firehose__kinesis__s3.firehose-kinesis-source-missing-role-permission-negative|_create_delivery_stream_expect_denied#10|AssertionError|raise AssertionError(
Issue-Commit: ac0d009460b15386fb7f532991bd4fb2bd9b5be8

# firehose__kinesis__s3: CreateDeliveryStream accepts a KinesisStreamAsSource delivery stream whose source role holds no kinesis read permissions, instead of refusing it with InvalidArgumentException

> _Note: this issue was identified against the code base at commit `ac0d009460b15386fb7f532991bd4fb2bd9b5be8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeliveryStream

## Description
Cause 1 of 1: CreateDeliveryStream performs no synchronous authorization check of the KinesisStreamSourceConfiguration's RoleARN against the named KinesisStreamARN, so a delivery stream whose source role cannot read the Kinesis stream at all is created successfully.

AWS: Smithy: CreateDeliveryStream takes DeliveryStreamType (enum, here KinesisStreamAsSource) and KinesisStreamSourceConfiguration { KinesisStreamARN, RoleARN } and models InvalidArgumentException (HTTP 400, "The specified input parameter has a value that is not valid."). Recorded behavior: Firehose validates the source role's Kinesis read permissions synchronously at CreateDeliveryStream time, so a role whose policy grants only S3 writes and no kinesis:DescribeStream / GetRecords / GetShardIterator is refused immediately with HTTP 400, error code 'InvalidArgumentException' and the message frame "Role <roleArn> is not authorized to perform: kinesis:DescribeStream on resource <streamArn>." — fixed text "Role ", " is not authorized to perform: kinesis:DescribeStream on resource " and the terminal period, with the role ARN and the stream ARN taken verbatim from the request (the recording's generated role and stream names only fill that template). The stream is never created, so it can never deliver. A sibling delivery stream over the SAME Kinesis stream whose role does carry the read actions is accepted, becomes ACTIVE and delivers a put record to its S3 bucket. The distinct, retryable refusal for a role whose trust policy has not propagated yet is the same code with the different message "Firehose is unable to assume role <roleArn>. Please check the role provided."

Emulator: the call is ACCEPTED. The check's helper, which retries only the propagation message and returns any other InvalidArgumentException, instead reports: "AssertionError: expected CreateDeliveryStream(<name>) to be rejected for missing kinesis read permissions, but it succeeded". No 400, no error code and no message exist to compare — the under-permissioned delivery stream is created like a fully permissioned one.

Why: a client that relies on AWS refusing an under-permissioned Firehose source role gets a stream that CreateDeliveryStream reported as valid, and only discovers the gap (if at all) as permanent silent non-delivery later. Least-privilege tests and IaC that asserts this rejection pass against the emulator and fail in AWS, and the modeled InvalidArgumentException branch of this validation is unreachable.

Fix: the CreateDeliveryStream handler's validation of KinesisStreamSourceConfiguration in the firehose service crate — before the stream is persisted, resolve the configured RoleARN in IAM state and authorize kinesis:DescribeStream (and the rest of the source read set) against the configured KinesisStreamARN, refusing with the 400 InvalidArgumentException and the message frame above when the role's identity policy does not allow it, while leaving the correctly-permissioned sibling accepted.

Note on existing issues: the CFN-deployed variant of this scenario is owned by open issues 20261120T082455Z-firehose-kinesis-s3-the-cfn-deployed-kinesis-firehose.md and 20261121T221736Z-firehose-kinesis-s3-the-cfn-deployed-kinesis-firehose-cf9fce.md, whose cause is the inert Kinesis-source ingestion loop (no delivery, no Kinesis.AccessDenied log record). Neither covers the direct-API divergence filed here: the synchronous CreateDeliveryStream refusal, which happens before any ingestion.

Traces: firehose__kinesis__s3.firehose-kinesis-source-missing-role-permission-negative.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout.

[mismatch] firehose__kinesis__s3.firehose-kinesis-source-missing-role-permission-negative
  File "check_firehose__kinesis__s3_firehose_kinesis_source_missing_role_permission_negative.py", line 46, in run
    denial = _create_delivery_stream_expect_denied(
  File "check_firehose__kinesis__s3_firehose_kinesis_source_missing_role_permission_negative.py", line 277, in _create_delivery_stream_expect_denied
    raise AssertionError(
  AssertionError: expected CreateDeliveryStream(sim2real-fh-bad-<id>) to be rejected for missing kinesis read permissions, but it succeeded

Expected (AWS): CreateDeliveryStream(DeliveryStreamType='KinesisStreamAsSource', KinesisStreamSourceConfiguration={KinesisStreamARN: <stream>, RoleARN: <role with only S3 writes>}, S3DestinationConfiguration={...}) is refused with HTTP 400, Error.Code 'InvalidArgumentException' and Error.Message "Role <roleArn> is not authorized to perform: kinesis:DescribeStream on resource <streamArn>." and no delivery stream is created.
Actual (emulator): the call returns success and the delivery stream exists (the check tears it down and fails loudly). No error code or message is produced.

The generated bucket, stream, role and delivery-stream names are per-run values that fill the message template; the requirement is the 400, the code 'InvalidArgumentException' and the message frame (fixed text plus the request's role and stream ARNs).

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. CreateStream(ShardCount=1), wait ACTIVE, read its StreamARN. Create two S3 buckets.
3. Create two IAM roles trusted by firehose.amazonaws.com: role A with an inline policy granting only the S3 write set on its bucket (NO kinesis actions), role B granting kinesis:DescribeStream/DescribeStreamSummary/GetRecords/GetShardIterator/ListShards on the stream plus the S3 write set on its bucket.
4. CreateDeliveryStream(DeliveryStreamType='KinesisStreamAsSource', KinesisStreamSourceConfiguration={KinesisStreamARN: <stream arn>, RoleARN: <role A arn>}, S3DestinationConfiguration={RoleARN: <role A arn>, BucketARN: <bucket A arn>, BufferingHints: {SizeInMBs:1, IntervalInSeconds:60}}). On AWS this is refused with 400 InvalidArgumentException and the message "Role <role A arn> is not authorized to perform: kinesis:DescribeStream on resource <stream arn>." Observe that it succeeds instead and the stream appears in DescribeDeliveryStream.
5. CreateDeliveryStream with role B over the same Kinesis stream and bucket B: it must be accepted and reach ACTIVE (positive control).
6. PutRecord to the Kinesis stream: bucket B receives the record; bucket A must hold nothing because stream A was never created.
7. Re-replay the check in trace_ids to confirm a fix.
