Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.get-metric-data-places-each-datapoint-at-its-written-timestamp-and-resolution cloudwatch.mstrx-e2e-find-metric-streams-excluding-namespace-lifecycle
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.get-metric-data-places-each-datapoint-at-its-written-timestamp-and-resolution|_poll_until#22|TimeoutError|raise TimeoutError(f"{description}: the run budget ended the wait. Last result: {last!r}")
Issue-Key: cloudwatch.mstrx-e2e-find-metric-streams-excluding-namespace-lifecycle|_drain#6|Timeout|time.sleep(2)
Issue-Commit: 02e30cec75724d494cfd983590fb8cff440247b8

# cloudwatch: 2 net-new causes — GetMetricData at Period=1 drops the regular-resolution datapoint instead of reporting it at its minute, and the CloudFormation metric-stream lifecycle reaches no verdict

> _Note: this issue was identified against the code base at commit `02e30cec75724d494cfd983590fb8cff440247b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricData

## Description
Two net-new root causes under the `### cloudwatch` heading (Cause 1 of 2, Cause 2 of 2). 33 checks fail under that heading; 31 are marked OWNED by open filings and are skipped — the two ids in trace_ids are the survivors.

Cause 1 of 2 — GetMetricData with Period=1 omits a regular-resolution datapoint written in the same minute, where AWS reports it at its minute bucket alongside the high-resolution point at its second. Failing check: cloudwatch.get-metric-data-places-each-datapoint-at-its-written-timestamp-and-resolution.
AWS: Smithy (com.amazonaws.cloudwatch) MetricDatum.StorageResolution — 'Valid values are 1 and 60. Setting this to 1 specifies this metric as a high-resolution metric, so that CloudWatch stores the metric with sub-minute resolution down to one second... This field is optional, if you do not specify it the default of 60 is used.' MetricStat.Period — 'For metrics with regular resolution, a period can be as short as one minute (60 seconds) and must be a multiple of 60. For high-resolution metrics ... the period can be 1, 5, 10, 20, 30, 60, or any multiple of 60.' The RETRIEVAL behavior for a mixed-resolution series is recorded behavior only: one PutMetricData publishes two data for the same metric inside one minute — one with no StorageResolution at second 2, one with StorageResolution=1 at second 7 — and a single GetMetricData query (MetricStat Period=1, Stat Sum, ScanBy TimestampAscending) over [minute, minute+60s) returns TWO values: the regular-resolution datum at the minute boundary (its 1-minute bucket) and the high-resolution datum at its own second. The same data read at Period=60 share one bucket (Sum = both values, SampleCount = 2) at the minute. Timestamps are per-run values; the requirement is the count and the placement (minute boundary for the regular datum, exact second for the high-resolution one).
Emulator: the Period=1 read never returns more than one value. The result observed repeatedly until the poll's budget ran out was {'Id': 'mix', 'Label': 'MixRes', 'Timestamps': [<minute+7s>], 'Values': [2.0], 'StatusCode': 'Complete', 'Messages': []} — only the StorageResolution=1 datum. The regular-resolution datum published in the same call (value 1.0 at second 2) is absent from the sub-minute window entirely rather than being reported at the minute boundary. It is not an ingestion delay: its high-resolution sibling from the SAME PutMetricData call is already readable, and every earlier assertion of the check (four back-/future-dated points placed in their own minutes, the single future bucket read alone, the empty-but-Complete no-data window) passed.
Why: a client reading a mixed-resolution custom metric at a sub-minute period silently loses every regular-resolution datum — a sub-minute dashboard or a 1-second-period alarm math expression sees a series that is missing points against the emulator and complete against AWS (and the SampleCount/Sum totals disagree across resolutions). Anything that validates its own publishing path by reading back at Period=1 concludes the regular datum was never stored.
Note for the fixer: the open filing 20261119T065938Z-cloudwatch-2-net-new-causes-associatedatasetkmskey-s-caller.md (Cause 2) asks the opposite-looking thing of GetMetricStatistics — a regular-resolution datum must NOT produce a datapoint at Period=1 there. Both are recorded behavior and they are consistent only if resolution is tracked per datum and applied per API: GetMetricStatistics(Period=1) excludes the regular datum, while GetMetricData(Period=1) still reports it in its 1-minute bucket. Do not fix one by generalising the other; this issue's key is only the GetMetricData read.
Fix: the GetMetricData retrieval/bucketing path for sub-minute periods (and the per-datum storage resolution recorded by PutMetricData's ingestion): a regular-resolution datum must be placed in, and returned at, its minute boundary for a sub-minute Period, not dropped.
Traces: cloudwatch.get-metric-data-places-each-datapoint-at-its-written-timestamp-and-resolution.

Cause 2 of 2 — the CloudFormation-deployed metric-stream lifecycle reaches no verdict: the check is cut at the harness's 60 s cap with no step completed. Failing check: cloudwatch.mstrx-e2e-find-metric-streams-excluding-namespace-lifecycle.
AWS: Smithy (com.amazonaws.cloudwatch) GetMetricStream returns Arn/Name/IncludeFilters/ExcludeFilters/FirehoseArn/RoleArn/State/CreationDate/LastUpdateDate/OutputFormat and errors ResourceNotFoundException (404)/InvalidParameterValueException (400); ListMetricStreams is paginated over Entries. Recorded behavior only for the scenario: a CloudFormation stack carrying two AWS::CloudWatch::MetricStream resources deploys, GetMetricStream on the exclude stream reports ExcludeFilters == [{'Namespace': 'AWS/EC2', 'MetricNames': ['CPUUtilization','NetworkIn','NetworkOut','DiskReadBytes','DiskWriteBytes']}] in template order and FirehoseArn equal to the stack's Firehose, a ListMetricStreams + GetMetricStream walk derives the one exclude stream, StopMetricStreams quiesces the Firehose so its bucket drains, and destroying the stack removes the streams.
Emulator: the check produced 'Check timed out after 60.0s' — no assertion, no response, nothing observable about the metric-stream surface. The scenario's own waits (stack deadline 900 s, drain 600 s, 90 s quiet window) cannot fit inside the cap, so the limiting factor may be the suite's per-check budget rather than the emulator; it is filed because it is the only surviving failure of its kind under this heading and no open issue carries its replay key (compare 20261116T122519Z-codebuild-ecr-ecs-elbv2-logs-both-container-ci.md and 20261120T073735Z-lambda-rds-secretsmanager-the-iam-auth-rds-proxy.md, which record the same budget-exhaustion shape for other services). Hypothesis: the stack's Firehose/MetricStream resources do not converge quickly enough for the deploy leg to complete inside the cap.
Why: the whole CloudFormation metric-stream path — filter fidelity, the ListMetricStreams walk, Stop/Start quiescing and stack deletion — is ungraded, so any divergence there is invisible.
Fix: whichever leg consumes the budget — the AWS::CloudWatch::MetricStream / AWS::KinesisFirehose::DeliveryStream providers' convergence in CloudFormation, so a two-stream stack reaches CREATE_COMPLETE promptly; confirm by timing the deploy leg alone before changing the retrieval code.
Traces: cloudwatch.mstrx-e2e-find-metric-streams-excluding-namespace-lifecycle.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator. Service heading `### cloudwatch` 678/711 (95%), 33 failed — 31 marked OWNED by open filings and skipped; the 2 ids in trace_ids survive.

Cause 1:
[error] cloudwatch.get-metric-data-places-each-datapoint-at-its-written-timestamp-and-resolution
  per_second = _poll_until(ctx, lambda: read([_query("mix", namespace, "MixRes", 1, "Sum")], minute, minute + timedelta(seconds=60))["mix"], lambda r: len(r["Values"]) == 2, timeout_s=60.0, description="both MixRes points readable at Period=1")
  TimeoutError: both MixRes points readable at Period=1: the run budget ended the wait. Last result: {'Id': 'mix', 'Label': 'MixRes', 'Timestamps': [datetime.datetime(2026, 11, 24, 3, 24, 7, tzinfo=tzlocal())], 'Values': [2.0], 'StatusCode': 'Complete', 'Messages': []}
Published by one PutMetricData: {'MetricName': 'MixRes', 'Timestamp': minute+2s, 'Value': 1.0} and {'MetricName': 'MixRes', 'Timestamp': minute+7s, 'Value': 2.0, 'StorageResolution': 1}.
Expected (recorded AWS) for GetMetricData MetricStat Period=1 over [minute, minute+60s): Timestamps == [minute, minute+7s], Values == [1.0, 2.0]; and at Period=60 the same data give Timestamps [minute], Sum [3.0], SampleCount [2.0].
Actual (emulator): Timestamps [minute+7s], Values [2.0] — the regular-resolution datum is missing from the sub-minute window; StatusCode 'Complete' (no Messages), so the response does not announce a partial result. The poll's wait was ended by the run budget, but the condition was already contradicted by every observation: the high-resolution sibling of the same PutMetricData call was readable, and the check's earlier Axis/future-bucket/empty-window assertions all passed.

Cause 2:
[mismatch] cloudwatch.mstrx-e2e-find-metric-streams-excluding-namespace-lifecycle
  Error: Check timed out after 60.0s
No response, status or message was observed; the first assertion (GetMetricStream ExcludeFilters in template order, FirehoseArn == the stack's Firehose) was never reached. The check's own budgets are _STACK_DEADLINE_S=900.0, _DRAIN_DEADLINE_S=600.0, _QUIET_S=90.0.

Smithy consulted: services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json — MetricDatum.StorageResolution doc, MetricStat.Period doc, GetMetricData/GetMetricStream/ListMetricStreams signatures and errors.

## Steps to Reproduce
Cause 1 (GetMetricData at Period=1):
1. Pick minute = now UTC truncated to the second 0 of the current minute; namespace = a fresh custom namespace.
2. cloudwatch PutMetricData(Namespace=ns, MetricData=[{'MetricName':'MixRes','Timestamp':minute+2s,'Value':1.0}, {'MetricName':'MixRes','Timestamp':minute+7s,'Value':2.0,'StorageResolution':1}]).
3. cloudwatch GetMetricData(MetricDataQueries=[{'Id':'mix','MetricStat':{'Metric':{'Namespace':ns,'MetricName':'MixRes'},'Period':1,'Stat':'Sum'},'ReturnData':True}], StartTime=minute, EndTime=minute+60s, ScanBy='TimestampAscending').
4. Expected: Timestamps == [minute, minute+7s] and Values == [1.0, 2.0], StatusCode 'Complete'. Actual: Timestamps == [minute+7s], Values == [2.0].
5. Repeat with Period=60 and two queries (Sum and SampleCount): both resolutions must share the single bucket at minute — Sum 3.0, SampleCount 2.0.
6. Re-replay cloudwatch.get-metric-data-places-each-datapoint-at-its-written-timestamp-and-resolution.

Cause 2 (metric-stream lifecycle):
1. Replay cloudwatch.mstrx-e2e-find-metric-streams-excluding-namespace-lifecycle; it is cut at 60 s with no assertion.
2. Time the deploy leg on its own: CreateStack for the mstrx-monitoring-streams template (two AWS::CloudWatch::MetricStream resources, one include-filtered and one exclude-filtered, over a Firehose delivery stream to S3 with an IAM role) and wait for CREATE_COMPLETE.
3. Then GetMetricStream(Name=<exclude stream>) and assert ExcludeFilters == [{'Namespace':'AWS/EC2','MetricNames':['CPUUtilization','NetworkIn','NetworkOut','DiskReadBytes','DiskWriteBytes']}] in that order and FirehoseArn == the stack's Firehose ARN; ListMetricStreams + GetMetricStream must derive exactly that one exclude stream.
4. StopMetricStreams on both, let the Firehose drain, then DeleteStack — GetMetricStream must then answer ResourceNotFoundException (404).
5. Re-replay the id to confirm the scenario reaches a verdict inside the suite's cap.
