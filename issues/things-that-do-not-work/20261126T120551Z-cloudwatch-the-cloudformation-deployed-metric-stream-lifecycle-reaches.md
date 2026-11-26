Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.mstrx-e2e-find-metric-streams-excluding-namespace-lifecycle
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.mstrx-e2e-find-metric-streams-excluding-namespace-lifecycle|_drain#6|Timeout|time.sleep(2)
Issue-Commit: 2aaadc19d9353b323ddc8d1ceaf32931386905fe

# cloudwatch: the CloudFormation-deployed metric-stream lifecycle reaches no verdict — the exclude/include MetricStream estate never becomes readable inside the budget

> _Note: this issue was identified against the code base at commit `2aaadc19d9353b323ddc8d1ceaf32931386905fe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStream

## Description
This issue carries 1 net-new root cause.

Cause 1 of 1: the metric-stream estate deployed by a CloudFormation stack (two AWS::CloudWatch::MetricStream resources behind a Firehose-to-S3 destination) never becomes readable/queryable, so the whole ListMetricStreams + GetMetricStream + StopMetricStreams lifecycle is cut off at the replay budget with no graded step. Failing check: cloudwatch.mstrx-e2e-find-metric-streams-excluding-namespace-lifecycle.

AWS: a metric stream created with an exclude filter reads back through GetMetricStream with ExcludeFilters as declared ([{Namespace: 'AWS/EC2', MetricNames: [...] }], names in declaration order), FirehoseArn equal to the destination delivery stream, RoleArn, OutputFormat and State 'running'; a stream created with only include filters reads back with IncludeFilters and NO ExcludeFilters member at all. ListMetricStreams pages the account's streams as Entries[].Name (Smithy cloudwatch 2010-08-01: ListMetricStreams is paginated on NextToken/MaxResults 1-500; GetMetricStream output members Arn/Name/IncludeFilters/ExcludeFilters/FirehoseArn/RoleArn/State/OutputFormat/StatisticsConfigurations/IncludeLinkedAccountsMetrics). StopMetricStreams moves each named stream to State 'stopped' and is a no-op for a name that does not exist. After the owning stack is deleted, GetMetricStream on the removed stream answers 404 ResourceNotFoundException (Smithy: GetMetricStream errors ResourceNotFoundException/404, with an empty message for this service), and DescribeStackResources on the deleted stack answers ValidationError 'Stack with id <stack-name> does not exist' (fixed text 'Stack with id ' + the requested stack name + ' does not exist'). Every one of these steps settles promptly — the check allows 900 s for the stack to reach a terminal status and 600 s for the destination bucket to drain.

Emulator: the check produces no graded step at all — the suite reports 'Error: Check timed out after 60.0s', i.e. the run is killed at the replay cap before the first assertion (the GetMetricStream read-back of ExcludeFilters/FirehoseArn/State) is reached. Nothing downstream of it (the ListMetricStreams/GetMetricStream derivation of the one exclude stream, the StopMetricStreams -> State 'stopped' transition, the bucket drain, and the post-destroy ResourceNotFoundException / 'Stack with id ... does not exist' refusals) is exercised. Hypothesis: the stack carrying the AWS::CloudWatch::MetricStream resources never reaches a terminal StackStatus (or its metric-stream resources are never provisioned so GetMetricStream blocks/retries), so the deploy phase's poll never completes; the recording only establishes that no step was reachable within the budget.

Why: a client cannot manage CloudWatch metric streams as infrastructure-as-code against this emulator, and cannot read a stream's filters, target Firehose or running/stopped state back at all: every call in the stream lifecycle sits behind a deploy that never settles, so an application or test suite that provisions a metric stream hangs instead of either succeeding or being refused. It also means the account-level metric-stream surface (ListMetricStreams paging, exclude-vs-include filter distinction, StopMetricStreams) is entirely ungraded.

Fix: the cloudwatch metric-stream state and its handlers (PutMetricStream/GetMetricStream/ListMetricStreams/StopMetricStreams) together with the AWS::CloudWatch::MetricStream resource handler that provisions them from a template — the resource must reach a terminal provisioned state and the stream must then be readable, so the lifecycle settles instead of hanging. Not the replay harness: the budget is generous relative to the recorded AWS timings.

Traces: cloudwatch.mstrx-e2e-find-metric-streams-excluding-namespace-lifecycle (the only grounding example; it is the sole failing check in this sweep not already owned by an open issue). Adjacent but NOT covering it: 20261109T183935Z-cloudformation-cloudwatch-2-net-new-causes-four-aws.md (four other AWS::CloudWatch::* types unrecognized by the template processor, and the AnomalyDetector Ref shape) and 20261117T121318Z-cloudformation-cloudwatch-cloudformation-3-net-new-causes-aws-cloudwatch-alarm.md (Alarm/Dashboard/AnomalyDetector resources); neither mentions MetricStream nor carries this check's replay key.

## Evidence
Expected (AWS): phase 'deploy' — GetMetricStream on the stack's exclude stream reports ExcludeFilters == [{'Namespace': 'AWS/EC2', 'MetricNames': ['CPUUtilization','NetworkIn','NetworkOut','DiskReadBytes','DiskWriteBytes']}], FirehoseArn == the stack's delivery-stream ARN, State == 'running'; the include stream reports IncludeFilters == [{'Namespace': 'AWS/EC2'}, {'Namespace': 'AWS/Lambda'}] and carries NO 'ExcludeFilters' member. phase 'solve' — ListMetricStreams + GetMetricStream walk visits exactly the two streams and selects the exclude one. phase 'cleanup' — StopMetricStreams leaves both streams State == 'stopped'. phase 'destroy' — DescribeStackResources on the deleted stack: ValidationError, message 'Stack with id <stack> does not exist'; GetMetricStream on the removed stream: ResourceNotFoundException with an empty message; a second delete is a clean no-op.

Actual (emulator): 'Error: Check timed out after 60.0s' — no phase verdict is recorded, so none of the above was observed. Stack name, stream names, bucket name and the Firehose ARN are per-run values; the divergence is that no step of the lifecycle completes.

## Steps to Reproduce
1. CreateStack with a template containing an S3 bucket, an IAM role trusting firehose.amazonaws.com, an AWS::KinesisFirehose::DeliveryStream with an S3DestinationConfiguration to that bucket, a role trusting streams.metrics.cloudwatch.amazonaws.com, and two AWS::CloudWatch::MetricStream resources: one with ExcludeFilters=[{Namespace: 'AWS/EC2', MetricNames: ['CPUUtilization','NetworkIn','NetworkOut','DiskReadBytes','DiskWriteBytes']}] plus StatisticsConfigurations with AdditionalStatistics ['p99'] and IncludeLinkedAccountsMetrics=false, and one with IncludeFilters=[{Namespace: 'AWS/EC2'},{Namespace: 'AWS/Lambda'}] and AdditionalStatistics ['p95'].
2. Poll DescribeStacks for a terminal StackStatus — observe that no terminal status is reached (the call never settles within the budget).
3. Call cloudwatch GetMetricStream(Name=<exclude stream name>) and compare ExcludeFilters / FirehoseArn / State ('running') with the template.
4. Call ListMetricStreams (paging NextToken) and GetMetricStream per entry; exactly the exclude stream must report ExcludeFilters.
5. StopMetricStreams(Names=[both]) and re-read GetMetricStream — both must report State 'stopped'.
6. DeleteStack, then DescribeStackResources on the stack (expect ValidationError 'Stack with id <stack> does not exist') and GetMetricStream on the exclude stream (expect ResourceNotFoundException with an empty message).
