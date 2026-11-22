Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative
Issue-Key: firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative|_deploy_cfn#2|AssertionError|raise AssertionError(f"Stack {name} ended in {stack['StackStatus']}")
Issue-Commit: 99b4003ffaa9702facb506eecc4da0e295bc99f1

# firehose__kinesis__s3: the CFN Kinesis→Firehose→S3 missing-permission stack no longer deploys at all — CreateStack ends ROLLBACK_COMPLETE instead of CREATE_COMPLETE

> _Note: this issue was identified against the code base at commit `99b4003ffaa9702facb506eecc4da0e295bc99f1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateStack

## Description
Cause 1 of 1: a CloudFormation stack that declares a Kinesis→Firehose→S3 pipeline is rolled back during create, so the stack never reaches CREATE_COMPLETE and none of the pipeline resources (bucket, source stream, log group/streams, roles, delivery streams) is available to the caller. The template is plain, fully-modelled CloudFormation: one AWS::Kinesis::Stream (ShardCount 1), one AWS::S3::Bucket, one AWS::Logs::LogGroup (RetentionInDays 1) with two AWS::Logs::LogStream children, two AWS::IAM::Role resources trusting firehose.amazonaws.com (identical except that one omits kinesis:GetRecords), and two AWS::KinesisFirehose::DeliveryStream resources with DeliveryStreamType KinesisStreamAsSource, a KinesisStreamSourceConfiguration (KinesisStreamARN + RoleARN via Fn::GetAtt) and a (legacy) S3DestinationConfiguration carrying BucketARN, RoleARN, Prefix, BufferingHints {IntervalInSeconds 0, SizeInMBs 1} and CloudWatchLoggingOptions. The stack is created with Capabilities CAPABILITY_IAM/CAPABILITY_NAMED_IAM.

AWS: CloudFormation CreateStack for this template is accepted and the stack settles in CREATE_COMPLETE, exposing its Outputs (bucket name, stream name, log group name, bad log stream name, bad role name); a stack only enters ROLLBACK_COMPLETE when a declared resource fails to create or stabilize. Every property the template uses is modelled and in range — Smithy (com.amazonaws.firehose): CreateDeliveryStream accepts DeliveryStreamType, KinesisStreamSourceConfiguration (KinesisStreamARN required, pattern ^arn:.*:kinesis:[a-zA-Z0-9\-]+:\d{12}:stream/[a-zA-Z0-9_.-]+$; RoleARN required, pattern ^arn:.*:iam::\d{12}:role/...$) and S3DestinationConfiguration (marked @deprecated but still accepted; RoleARN and BucketARN required, Prefix @length 0–1024, BufferingHints, CloudWatchLoggingOptions); BufferingHints.IntervalInSeconds @range min 0 max 900 and SizeInMBs @range min 1 max 128, so {0, 1} is valid. CreateDeliveryStream's modelled refusals are InvalidArgumentException / InvalidKMSResourceException / LimitExceededException / ResourceInUseException (all HTTP 400) — none applies to this request. Recorded behavior only for the stack trajectory itself: the two open issues named below record this very check getting PAST the deploy (they stop later, polling for delivery), which is the recorded evidence that CreateStack for this template is supposed to reach CREATE_COMPLETE.

Emulator: the create does not complete. The check's deploy helper polls DescribeStacks until the stack reaches a terminal create state and observes ROLLBACK_COMPLETE, then fails with `AssertionError: Stack <name> ended in ROLLBACK_COMPLETE`. Because the deploy is the check's first step, no Outputs are returned and nothing downstream (the permissioned sibling's delivery under good/, the Kinesis.AccessDenied delivery-error record, the absence of the denied marker under bad/, the restore-after-grant leg) is exercised. The replay output carries only the terminal stack status, not the failing resource's ResourceStatusReason, so the specific resource that fails to create/stabilize is not attributable from the replay — the divergence being filed is the stack trajectory (ROLLBACK_COMPLETE where AWS reaches CREATE_COMPLETE). This is a different stopping point from the two open issues that already name this check — 20261120T082455Z-firehose-kinesis-s3-the-cfn-deployed-kinesis-firehose.md and 20261121T221736Z-firehose-kinesis-s3-the-cfn-deployed-kinesis-firehose-cf9fce.md — both of which record the stack deploying successfully and the check then timing out waiting for delivery. Neither covers a stack that is rolled back, and the suite does not report this failure as owned by either.

Why: a client deploying a Kinesis-sourced Firehose pipeline by CloudFormation gets no pipeline at all: CreateStack is accepted, then the stack is rolled back and every resource it declared is deleted, so the bucket, stream, roles and delivery streams the client's application depends on never exist. Worse than the previously-recorded silent-delivery defect, this makes the whole template undeployable and hides which resource the emulator objected to, so there is nothing for the client to correct.

Fix: the CloudFormation resource-provisioning path for this template — the create/stabilize handler of the resource that fails (the AWS::KinesisFirehose::DeliveryStream provider for a KinesisStreamAsSource stream with a legacy S3DestinationConfiguration is the prime suspect, with AWS::Logs::LogStream and the Fn::GetAtt/Fn::Sub resolution of the role and bucket ARNs the other candidates) must create and stabilize the resource so the stack reaches CREATE_COMPLETE; and, whatever the cause, the failing resource's ResourceStatusReason must carry the service exception that caused the rollback so the refusal is diagnosable.

Traces: firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative.

## Evidence
Replay of simparity-behavioral-aws-v1 against this checkout.

### firehose__kinesis__s3
[mismatch] firehose__kinesis__s3.cfn-deploy-firehose-missing-kinesis-permission-negative
  Error: File "check_firehose__kinesis__s3_cfn_deploy_firehose_missing_kinesis_permission_negative.py", line 24, in run
    stack = _deploy_cfn(ctx, template)
  File "check_firehose__kinesis__s3_cfn_deploy_firehose_missing_kinesis_permission_negative.py", line 226, in _deploy_cfn
    raise AssertionError(f"Stack {name} ended in {stack['StackStatus']}")
  AssertionError: Stack sim2real-xsvc-stk-df6bd12eba84 ended in ROLLBACK_COMPLETE

Expected (AWS): CreateStack is accepted and DescribeStacks reports StackStatus CREATE_COMPLETE, with the template's five Outputs (BucketName, StreamName, LogGroupName, BadLogStreamName, BadRoleName) resolved.
Actual (emulator): DescribeStacks reports StackStatus ROLLBACK_COMPLETE, no Outputs are available, and the check aborts on its first step.

The stack name (sim2real-xsvc-stk-df6bd12eba84) and all generated bucket/stream/role/log-stream names are per-run values and are not part of the requirement; the requirement is that a stack declaring these resource types and properties reaches CREATE_COMPLETE, and that any resource failure is reported with the service exception in its ResourceStatusReason.

Supporting model citations: Smithy com.amazonaws.firehose — CreateDeliveryStream input members DeliveryStreamType, KinesisStreamSourceConfiguration, S3DestinationConfiguration (@deprecated, still accepted); BufferingHints.IntervalInSeconds @range 0–900, SizeInMBs @range 1–128; errors InvalidArgumentException/InvalidKMSResourceException/LimitExceededException/ResourceInUseException, all HTTP 400 — none of which this request violates.

## Steps to Reproduce
1. Boot the emulator from this checkout.
2. CloudFormation CreateStack with Capabilities ["CAPABILITY_IAM","CAPABILITY_NAMED_IAM"] and a template whose Resources are:
   - SourceStream: AWS::Kinesis::Stream, Properties {ShardCount: 1}
   - DestBucket: AWS::S3::Bucket
   - LogGroup: AWS::Logs::LogGroup, Properties {RetentionInDays: 1}
   - GoodLogStream / BadLogStream: AWS::Logs::LogStream, Properties {LogGroupName: {"Ref": "LogGroup"}}
   - GoodRole / BadRole: AWS::IAM::Role trusting firehose.amazonaws.com via sts:AssumeRole, each with one inline policy granting the S3 write actions on {"Fn::GetAtt": ["DestBucket","Arn"]} and {"Fn::Sub": "${DestBucket.Arn}/*"}, logs:PutLogEvents on "*", and Kinesis actions on {"Fn::GetAtt": ["SourceStream","Arn"]} — GoodRole with kinesis:DescribeStream/DescribeStreamSummary/GetShardIterator/ListShards/GetRecords, BadRole with the same list minus kinesis:GetRecords.
   - GoodDeliveryStream / BadDeliveryStream: AWS::KinesisFirehose::DeliveryStream, Properties {DeliveryStreamType: "KinesisStreamAsSource", KinesisStreamSourceConfiguration: {KinesisStreamARN: GetAtt SourceStream.Arn, RoleARN: GetAtt <role>.Arn}, S3DestinationConfiguration: {BucketARN: GetAtt DestBucket.Arn, RoleARN: GetAtt <role>.Arn, Prefix: "good/" | "bad/", BufferingHints: {IntervalInSeconds: 0, SizeInMBs: 1}, CloudWatchLoggingOptions: {Enabled: true, LogGroupName: Ref LogGroup, LogStreamName: Ref <log stream>}}}
   with Outputs BucketName/StreamName/LogGroupName/BadLogStreamName/BadRoleName.
3. Poll DescribeStacks on the stack. On AWS it settles CREATE_COMPLETE; observe that the emulator settles ROLLBACK_COMPLETE instead and returns no Outputs.
4. Call DescribeStackEvents on the stack to see which resource failed and whether its ResourceStatusReason names a service exception.
5. Re-replay the check in trace_ids to confirm a fix.
