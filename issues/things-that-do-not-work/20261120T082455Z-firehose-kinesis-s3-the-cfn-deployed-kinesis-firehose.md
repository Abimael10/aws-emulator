Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative
Issue-Key: firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative|run#19|Timeout|found = _find_marker_in_bucket(ctx, bucket_name, "bad/", bad_marker)
Issue-Commit: abb164e6ce3e65cba242bbd0cd4b0598950b9faf

# firehose__kinesis__s3: the CFN-deployed Kinesis→Firehose→S3 missing-kinesis-permission scenario reaches no verdict — the whole 60 s budget is consumed with neither the permissioned sibling's delivery nor the denied stream's Kinesis.AccessDenied log

> _Note: this issue was identified against the code base at commit `abb164e6ce3e65cba242bbd0cd4b0598950b9faf`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateDeliveryStream

## Description
Cause 1 of 1: the Kinesis-sourced Firehose delivery path deployed by CloudFormation makes no observable progress, so the check that separates "this role cannot read Kinesis" from "nothing delivers at all" cannot reach a verdict: it is cut off at the suite's 60 s budget instead of asserting.

AWS: recorded behavior only for the delivery/logging timing; the modeled surface is Firehose CreateDeliveryStream with a KinesisStreamSourceConfiguration (RoleARN + KinesisStreamARN), an ExtendedS3DestinationConfiguration, and CloudWatchLoggingOptions. On AWS a stream whose source role holds kinesis:GetRecords/GetShardIterator pulls the records put to the Kinesis stream and writes them under its S3 prefix within the buffering window; a sibling stream on the SAME Kinesis stream whose role is missing only kinesis:GetRecords delivers nothing and Firehose writes a delivery-error record to the stream's CloudWatchLoggingOptions log stream whose `errorCode` is exactly `Kinesis.AccessDenied`. Granting the missing permission in place on the same role restores delivery for the same stream with nothing recreated.

Emulator: the check never reaches any of its three assertions. The replay reports only `Check timed out after 60.0s` — the first wait in the check is a poll for the permissioned sibling's marker object under the `good/` prefix (its own budget is 240 s), so the emulator produced neither the positive-control delivery nor, after it, a delivery-error record with an errorCode in the CloudWatch log stream, within the window the suite allows. Nothing after that point in the check — the `Kinesis.AccessDenied` code, the absence of the denied stream's marker under `bad/`, and the restore-after-grant beat — is exercised at all.

Hypothesis: the Kinesis-source ingestion loop for a delivery stream is inert (no records are pulled from the source stream, so neither the good nor the bad prefix is ever written and no read is ever attempted to deny), which is the same mechanism open issue 20261029T175617Z-firehose-kinesis-s3-4-net-new-root-causes.md records for its sibling checks. That issue does NOT carry this check's replay key, so this check is not covered by it; 20261104T055008Z-firehose-logs-s3-cloudwatchloggingoptions-never-emits-a-delivery.md records the missing delivery-error event from the S3 side.

Why: a client that models an under-permissioned Firehose source role against the emulator sees the same silence it would see for a correctly-permissioned one — no delivered objects and no CloudWatch delivery-error record — so it cannot distinguish a permission defect from a working pipeline, and cannot verify that granting kinesis:GetRecords repaired it.

Fix: the Firehose delivery engine's Kinesis-source ingestion and its CloudWatchLoggingOptions error path — pull records from the configured KinesisStreamARN under the source RoleARN, authorize that read against the role's policy, deliver to the ExtendedS3Destination prefix on success, and emit a delivery-error record with errorCode `Kinesis.AccessDenied` on a denied read.

Traces: firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout.

[mismatch] firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative
  Error: Check timed out after 60.0s

Expected (AWS): within the check's own poll windows, (a) the permissioned sibling stream's record appears as an object under the bucket's `good/` prefix, (b) the under-permissioned stream's CloudWatch log stream carries a delivery-error record whose errorCode is the fixed string `Kinesis.AccessDenied`, (c) no object carrying the denied stream's marker appears under `bad/`, and (d) after kinesis:GetRecords is granted in place to the same role, the previously withheld record is delivered.
Actual (emulator): the check reaches none of those assertions — its first wait (the positive-control delivery under `good/`) is still unsatisfied when the suite's 60 s budget expires, so no verdict is produced.

The marker strings, bucket/stream/role names and log-stream names are per-run values generated by the check and are not part of the requirement; the requirement is the delivery itself and the fixed errorCode `Kinesis.AccessDenied`.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. CreateStack a template with: one Kinesis stream; one S3 bucket; one CloudWatch log group with two log streams; two IAM roles with the same S3 write policy, one holding kinesis:GetRecords/GetShardIterator/DescribeStream on the stream and one holding none of them; and two Firehose delivery streams, each with a KinesisStreamSourceConfiguration naming the same Kinesis stream and its own role, an ExtendedS3DestinationConfiguration under prefix `good/` and `bad/` respectively, and CloudWatchLoggingOptions pointing at its log stream.
3. kinesis:PutRecord two records to the source stream.
4. List the bucket under `good/`: on AWS the permissioned stream's record lands there. Observe that no object appears.
5. GetLogEvents on the under-permissioned stream's log stream: on AWS a delivery-error record with errorCode `Kinesis.AccessDenied` appears. Observe that no delivery-error record is written.
6. Re-replay the check in trace_ids to confirm a fix.
