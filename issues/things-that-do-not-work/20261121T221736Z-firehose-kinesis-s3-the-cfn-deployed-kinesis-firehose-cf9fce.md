Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative
Issue-Key: firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative|run#19|Timeout|time.sleep(10)
Issue-Commit: 344bb9336e0011430569d037e3a195e35382835e

# firehose__kinesis__s3: the CFN-deployed Kinesis->Firehose->S3 missing-kinesis-permission scenario still reaches no verdict — neither the permissioned sibling's delivery nor the denied stream's Kinesis.AccessDenied log appears

> _Note: this issue was identified against the code base at commit `344bb9336e0011430569d037e3a195e35382835e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeliveryStream

## Description
Cause 1 of 1: a CloudFormation-deployed Firehose delivery stream with a KinesisStreamSourceConfiguration makes no observable progress — nothing is pulled from the source Kinesis stream, so no object is written under the destination prefix and no delivery-error record is written to the stream's CloudWatchLoggingOptions log stream. The check that separates "this source role cannot read Kinesis" from "nothing delivers at all" therefore never reaches any of its assertions and is cut off at the suite's 60 s budget.

AWS: recorded behavior only for the delivery and logging timing; the modeled surface is Firehose CreateDeliveryStream with KinesisStreamSourceConfiguration (RoleARN + KinesisStreamARN), an ExtendedS3DestinationConfiguration and CloudWatchLoggingOptions. On AWS a stream whose source role holds kinesis:GetRecords/GetShardIterator pulls records put to the source stream and writes them under its S3 prefix within the buffering window; a sibling stream reading the SAME Kinesis stream whose role is missing only kinesis:GetRecords delivers nothing and Firehose writes a delivery-error record to its CloudWatch log stream whose `errorCode` is exactly the fixed string `Kinesis.AccessDenied`. Granting the missing permission in place on the same role restores delivery with nothing recreated.

Emulator: the replay reports only `Check timed out after 60.0s`. The check's first wait is the positive control — a poll for the permissioned sibling's marker object under the `good/` prefix — so the emulator produced neither that delivery nor, after it, any delivery-error record carrying an errorCode. Everything downstream (the `Kinesis.AccessDenied` code, the absence of the denied stream's marker under `bad/`, and the restore-after-grant leg) is never exercised. Hypothesis: the Kinesis-source ingestion loop for a delivery stream is inert, so no read is ever attempted and therefore none is ever denied — the same mechanism open issue 20261029T175617Z-firehose-kinesis-s3-4-net-new-root-causes.md records for its sibling checks. A prior issue, 20261120T082455Z-firehose-kinesis-s3-the-cfn-deployed-kinesis-firehose.md, named this same check but its recorded replay key (`...|run#19|Timeout|found = _find_marker_in_bucket(ctx, bucket_name, "bad/", bad_marker)`) no longer matches where this replay stops, so the suite does not report this failure as owned by it and it is re-filed here against the current stopping point (the `good/` positive control).

Why: a client that models an under-permissioned Firehose source role against the emulator sees exactly the silence a correctly-permissioned one produces — no delivered objects and no CloudWatch delivery-error record — so it cannot tell a permission defect from a working pipeline, and cannot verify that granting kinesis:GetRecords repaired it.

Fix: the Firehose delivery engine's Kinesis-source ingestion path and its CloudWatchLoggingOptions error path — pull records from the configured KinesisStreamARN under the source RoleARN, authorize that read against the role's policy, deliver to the ExtendedS3Destination prefix on success, and emit a delivery-error record with errorCode `Kinesis.AccessDenied` on a denied read.

Traces: firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout.

[mismatch] firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative
  Error: Check timed out after 60.0s

Expected (AWS): within the check's own poll windows, (a) the permissioned sibling stream's record appears as an object under the bucket's `good/` prefix, (b) the under-permissioned stream's CloudWatch log stream carries a delivery-error record whose errorCode is the fixed string `Kinesis.AccessDenied`, (c) no object carrying the denied stream's marker appears under `bad/`, and (d) after kinesis:GetRecords is granted in place to the same role, the withheld record is delivered.
Actual (emulator): none of those assertions is reached — the first wait (the `good/` positive-control delivery) is still unsatisfied when the suite's budget expires, so no verdict is produced and no object or log record is observable at all.

The marker strings and the generated bucket, stream, role and log-stream names are per-run values and are not part of the requirement; the requirement is the delivery itself and the fixed errorCode `Kinesis.AccessDenied`.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. CreateStack a template with: one Kinesis stream; one S3 bucket; one CloudWatch log group with two log streams; two IAM roles with the same S3 write policy, one holding kinesis:GetRecords/GetShardIterator/DescribeStream on the stream and one holding none of them; and two Firehose delivery streams, each with a KinesisStreamSourceConfiguration naming that same Kinesis stream and its own role, an ExtendedS3DestinationConfiguration under prefix `good/` and `bad/` respectively, and CloudWatchLoggingOptions pointing at its log stream.
3. kinesis:PutRecord two records to the source stream.
4. List the bucket under `good/`: on AWS the permissioned stream's record lands there. Observe that no object ever appears.
5. GetLogEvents on the under-permissioned stream's log stream: on AWS a delivery-error record with errorCode `Kinesis.AccessDenied` appears. Observe that no delivery-error record is written.
6. Grant kinesis:GetRecords to the denied role in place: on AWS the withheld record is then delivered under `bad/`.
7. Re-replay the check in trace_ids to confirm a fix.
