Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudfront__kinesis.obs-cloudfront-3-realtime-log-role-without-put-record-delivers-nothing cloudfront__kinesis.realtime-log-config-delivers-edge-request-record-to-kinesis cloudfront__kinesis.xsvc-cloudfront-3-realtime-log-role-denied-putrecord-delivers-nothing
Issue-Key: cloudfront__kinesis.obs-cloudfront-3-realtime-log-role-without-put-record-delivers-nothing|_poll_until#31|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__kinesis.realtime-log-config-delivers-edge-request-record-to-kinesis|_poll_until#45|Timeout|time.sleep(interval_s)
Issue-Key: cloudfront__kinesis.xsvc-cloudfront-3-realtime-log-role-denied-putrecord-delivers-nothing|_poll_until#42|Timeout|time.sleep(interval_s)
Issue-Commit: 3346f0c8eb008cd7e8905968523e3939c8dd53a9

# cloudfront__kinesis: the CloudFront real-time log delivery path to Kinesis never reaches a verdict — all three real-time-log checks exhaust their 60 s budget with no log record ever appearing on the configured stream

> _Note: this issue was identified against the code base at commit `3346f0c8eb008cd7e8905968523e3939c8dd53a9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRealtimeLogConfig

## Description
Cause 1 of 1: a distribution whose DefaultCacheBehavior carries RealtimeLogConfigArn never produces an observable real-time log record on the Kinesis stream named by the config's KinesisStreamConfig, so no check of the real-time-logs feature — positive or negative — can reach an assertion.

AWS: CreateRealtimeLogConfig is modeled with required Name, SamplingRate, Fields and EndPoints (Smithy: CreateRealtimeLogConfig POST /2020-05-31/realtime-log-config -> 201, output RealtimeLogConfig; errors AccessDenied 403, InvalidArgument 400, RealtimeLogConfigAlreadyExists 409, TooManyRealtimeLogConfigs 400). The model says nothing about delivery — that is documented behavior only (https://docs.aws.amazon.com/AmazonCloudFront/latest/DeveloperGuide/real-time-logs.html): CreateRealtimeLogConfig validates neither the RoleARN's assumability nor its Kinesis permissions, the config is accepted, and once a distribution referencing it is Deployed, CloudFront delivers one tab-delimited log line per sampled viewer request to the Kinesis stream, with the fields in CloudFront's own canonical order as echoed by RealtimeLogConfig.Fields (so a GetRecords reader sees, per request, the configured field count, cs-uri-stem equal to the request path, sc-status equal to the status the edge returned, x-edge-response-result-type, a non-empty c-ip and a timestamp inside the request's clock window). SamplingRate=100 makes that one-request-one-line relationship deterministic. The negative side of the same documented rule: if the delivery role cannot PutRecord/PutRecords on the stream (no grant, or an explicit Deny), AWS silently delivers NOTHING to that stream while an equivalently configured control stream whose role does carry kinesis:PutRecord/PutRecords keeps receiving every line — the failure is a delivery-time failure, not a create-time refusal.

Emulator: all three replayed real-time-log checks end as 'Check timed out after 60.0s' — none produces a pass/fail verdict. Each of them (a) creates a Kinesis stream and waits for ACTIVE, (b) creates an IAM role trusted by cloudfront.amazonaws.com, (c) calls CreateRealtimeLogConfig with SamplingRate 100 and an explicit field list, (d) creates a distribution with RealtimeLogConfigArn on its DefaultCacheBehavior, waits for Status=Deployed, makes viewer request(s) through the edge, and then polls GetRecords on the stream for the delivered line(s). The suite's whole per-check budget is consumed inside that sequence, so the emulator never gets as far as serving a real-time log record: no line naming the probe key ever arrives, and the negative checks cannot even establish their control side (which must receive every line) before the budget runs out. The surviving symptom is therefore uniform across the positive case and both authorization-negative cases, which is what identifies it as one missing capability rather than three separate assertion divergences.

Why: a client that configures real-time logs gets an accepted CreateRealtimeLogConfig and a Deployed distribution and then waits forever for log lines that never arrive. Observability pipelines built on CloudFront real-time logs (edge request analytics, per-request status/result-type dashboards, consumers that read the stream with GetRecords) see an empty stream and cannot distinguish 'no traffic' from 'delivery broken'. The authorization semantics are equally unobservable: a caller cannot verify that a role without kinesis:PutRecord, or one with an explicit Deny, really suppresses delivery, so an operator cannot tell a correctly-locked-down config from a silently-broken one.

Fix: the CloudFront edge request path and its real-time-log delivery component — the code that, after serving a viewer request, must look up the behavior's RealtimeLogConfigArn, project the request onto the config's stored Fields in CloudFront's canonical order as a tab-delimited line, and PutRecord(s) it to the config's KinesisStreamConfig.StreamARN while assuming the config's RoleARN, dropping the delivery (and only the delivery) when that role is not authorized for kinesis:PutRecord/PutRecords on that stream. The distribution/edge serving path these checks traverse before the delivery step (create -> Deployed -> 200 at the edge for an OAC-protected S3 origin) must also complete inside a normal check budget, since the delivery assertion is unreachable until it does.

Traces: cloudfront__kinesis.realtime-log-config-delivers-edge-request-record-to-kinesis (positive: one edge request must yield exactly one log line with the configured field count, '/<probe-key>' as cs-uri-stem, '200' as sc-status, 'Miss' as x-edge-response-result-type, a non-empty c-ip and an in-window timestamp); cloudfront__kinesis.obs-cloudfront-3-realtime-log-role-without-put-record-delivers-nothing (role granted only kinesis:DescribeStream/DescribeStreamSummary -> stream must stay empty while the control stream logs all 3 rounds); cloudfront__kinesis.xsvc-cloudfront-3-realtime-log-role-denied-putrecord-delivers-nothing (explicit Deny on PutRecord -> same absence). Related but NOT covering this: 20261109T163411Z-cloudformation-cloudfront-iam-kinesis-cloudformation-does-not-recognize-aws-cloudfront-realtimelogconfig-so.md covers only CloudFormation's refusal of the AWS::CloudFront::RealtimeLogConfig resource type; 20261118T021609Z-cloudfront-firehose-iam-logs-s3-the-cloudfront-standard.md covers standard logging v2, a different delivery path.

## Evidence
Suite report, service heading '### cloudfront__kinesis' (0/3 passed, 3 failed):

[mismatch] cloudfront__kinesis.obs-cloudfront-3-realtime-log-role-without-put-record-delivers-nothing
  Error: Check timed out after 60.0s
[mismatch] cloudfront__kinesis.realtime-log-config-delivers-edge-request-record-to-kinesis
  Error: Check timed out after 60.0s
[mismatch] cloudfront__kinesis.xsvc-cloudfront-3-realtime-log-role-denied-putrecord-delivers-nothing
  Error: Check timed out after 60.0s

Expected (AWS, documented real-time-logs behavior): with SamplingRate=100 and Fields=[timestamp, c-ip, cs-uri-stem, sc-status, x-edge-response-result-type], a single viewer GET of /<probe-key> served 200 from the S3 origin yields exactly one Kinesis record line, tab-delimited, 5 fields, in the order RealtimeLogConfig.Fields reports; cs-uri-stem == '/<probe-key>', sc-status == '200', x-edge-response-result-type == 'Miss' (CachingDisabled), c-ip non-empty, timestamp within the request's clock window. (The literal key name, stream name, IP and timestamp are per-run values filling that template, not requirements.) For the two authorization-negative checks: the stream behind the under-privileged / explicitly-denied role receives zero lines over the absence window, while the control stream receives one line per request for every round.

Actual (emulator): no verdict at all — each check consumes the full 60.0 s budget inside the create-config -> distribution-Deployed -> edge-request -> GetRecords-poll sequence; no real-time log line naming the probe key is ever read from the stream, and the negative checks never establish the control-side deliveries their absence assertion is measured against.

## Steps to Reproduce
1. kinesis.CreateStream(StreamName=<s>, ShardCount=1); poll DescribeStreamSummary until StreamStatus == ACTIVE; note StreamARN.
2. iam.CreateRole trusted by cloudfront.amazonaws.com; iam.PutRolePolicy allowing kinesis:DescribeStream, kinesis:DescribeStreamSummary, kinesis:PutRecord, kinesis:PutRecords on that StreamARN.
3. cloudfront.CreateRealtimeLogConfig(Name=<n>, SamplingRate=100, Fields=[timestamp, c-ip, cs-uri-stem, sc-status, x-edge-response-result-type], EndPoints=[{StreamType: Kinesis, KinesisStreamConfig: {RoleARN: <role>, StreamARN: <stream>}}]) -> expect 201 and the field set echoed.
4. Create an S3 bucket, put a small object <probe-key>, create an OriginAccessControl, and cloudfront.CreateDistribution with that S3 origin, CachePolicyId 4135ea2d-6df8-44a3-9df3-4b5a84be39ad (managed CachingDisabled) and DefaultCacheBehavior.RealtimeLogConfigArn = the config ARN; allow the distribution's ARN in the bucket policy; poll GetDistribution until Status == Deployed.
5. kinesis.GetShardIterator(TRIM_HORIZON) on the stream's only shard BEFORE the request.
6. GET https://<distribution-domain>/<probe-key> through the edge; assert it answers 200 with the object's bytes.
7. Poll kinesis.GetRecords from the saved iterator: expect exactly one tab-delimited line naming '/<probe-key>' with sc-status '200' and x-edge-response-result-type 'Miss'. Observed: the stream stays empty and the whole sequence does not complete within the check budget.
8. Negative variant: repeat steps 2-7 with the role's policy limited to kinesis:DescribeStream/DescribeStreamSummary (or carrying an explicit Deny on kinesis:PutRecord/PutRecords) alongside a correctly-permissioned control config/stream on the same distribution — expect every request logged on the control stream and nothing on the broken one.
