Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__firehose__s3.obs-cloudwatch-2-metric-update-counts-good-stream-while-untrusted-role-stream-reports-publish-errors
Issue-Key: cloudwatch__firehose__s3.obs-cloudwatch-2-metric-update-counts-good-stream-while-untrusted-role-stream-reports-publish-errors|_poll#26|Timeout|time.sleep(_POLL_S)
Issue-Commit: 38ae27fa1c7e60e3a873e4340e685f95cd4783da

# cloudwatch__firehose__s3: the AWS/CloudWatch/MetricStreams monitoring metrics (MetricUpdate, PublishErrorRate) never converge for a running metric stream, so the trusted/untrusted stream differential is cut off at the 600 s budget

> _Note: this issue was identified against the code base at commit `38ae27fa1c7e60e3a873e4340e685f95cd4783da`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricStream

## Description
Cause 1 of 1: a pair of running metric streams on one namespace, differing only in whether their RoleArn's trust policy allows the metric-streams service principal, never reaches an observable state: the check makes no progress and is killed at its 600 s ceiling (its only barriers are (a) the trusted stream exporting >= 2 objects under its S3 prefix and (b) non-empty AWS/CloudWatch/MetricStreams series — MetricUpdate under MetricStreamName and with no dimensions, and PublishErrorRate for both streams).

AWS: Smithy models the control plane only — PutMetricStream(Name, FirehoseArn, RoleArn, OutputFormat required; IncludeFilters; Tags) returns Arn, and GetMetricStream reports State; the monitoring metrics are not modelled, so for the delivery and telemetry behavior this is recorded behavior only. The recording shows that after PutMetricStream and PutMetricData into the included namespace: the stream whose role trusts streams.metrics.cloudwatch.amazonaws.com delivers JSON records to its Firehose destination (and thus to S3) within the destination's buffering interval, and CloudWatch publishes, in namespace AWS/CloudWatch/MetricStreams, a per-minute MetricUpdate series both dimensioned by MetricStreamName and aggregated with no dimensions whose Sum reaches at least the number of published series, plus a PublishErrorRate series per stream, in [0,1], that is exactly 0.0 for the stream whose role can be assumed and greater than 0 for the stream whose role trusts only ec2.amazonaws.com; the untrusted stream's destination prefix stays empty. All of that is observable well inside the 600 s budget. No HTTP status or error code is involved: every API call in the check succeeds and the divergence is the absence of the delivered objects and/or the monitoring datapoints.

Emulator: 'Error: Check timed out after 600.0s'. The awaited state is never reached, so neither the trusted stream's export barrier nor any MetricUpdate / PublishErrorRate datapoint is ever satisfied, and the PublishErrorRate isolation assertion (the trusted stream reports only 0.0 while the untrusted one reports > 0) and the empty-untrusted-prefix assertion are never reached at all.

Why: a client that enables a metric stream has no way to tell whether it is working. The AWS/CloudWatch/MetricStreams metrics are the documented way to confirm that a stream is updating metrics and that its delivery role is usable; with no MetricUpdate series a healthy stream looks dead, and with no PublishErrorRate series a stream whose role CloudWatch cannot assume looks healthy. Any monitoring, alarming or least-privilege test built on those series silently passes or silently hangs instead of reporting the stream's true state.

Fix: the metric-stream publisher and its telemetry — after PutMetricStream, drive the stream's export of matching PutMetricData series to its Firehose destination on the buffering interval, and publish the stream's own AWS/CloudWatch/MetricStreams series: MetricUpdate (dimensioned by MetricStreamName and as the no-dimension aggregate) counting the series updated per minute, and PublishErrorRate per stream derived from whether the configured role can be assumed by the metric-streams principal (0.0 when it can, > 0 when it cannot), so GetMetricStatistics over that namespace returns datapoints.

Related but NOT the same as the open issue 20261111T040849Z-cloudwatch-firehose-s3-cloudwatch-metric-streams-deliver-through-a-role-cloudwatch.md (untrusted-role delivery, and a StatisticsConfiguration stream that never converges): that issue's Issue-Trace-Ids do not include this check, and this check's barrier is the AWS/CloudWatch/MetricStreams MetricUpdate / PublishErrorRate series rather than a StatisticsConfiguration.

Traces: cloudwatch__firehose__s3.obs-cloudwatch-2-metric-update-counts-good-stream-while-untrusted-role-stream-reports-publish-errors.

## Evidence
Check cloudwatch__firehose__s3.obs-cloudwatch-2-metric-update-counts-good-stream-while-untrusted-role-stream-reports-publish-errors [mismatch]:
Error: Check timed out after 600.0s

Expected (recorded AWS behavior), within the budget:
- the trusted stream's S3 prefix holds >= 2 delivered objects whose JSON records carry the published namespace, metric_name StreamedMetric, dimensions exactly {Shard}, value.sum == value.count * the published value, and metric_stream_name == that stream's name;
- GetMetricStatistics(Namespace=AWS/CloudWatch/MetricStreams, MetricName=MetricUpdate, Dimensions=[{MetricStreamName: <trusted stream>}], Period=60, Statistics=[Sum]) returns datapoints whose max Sum >= the number of published series, and the no-dimension aggregate is >= that per-stream count;
- GetMetricStatistics(MetricName=PublishErrorRate, Dimensions=[{MetricStreamName: <untrusted stream>}], Statistics=[Average]) returns datapoints in [0,1] with max > 0;
- GetMetricStatistics(MetricName=PublishErrorRate, Dimensions=[{MetricStreamName: <trusted stream>}]) returns datapoints, all exactly 0.0;
- the untrusted stream's S3 prefix is empty.

Actual: the check never gets past its first unmet barrier — no awaited condition is ever true and the harness kills it at 600.0 s, so none of the above series or objects are observed. Every API call (CreateBucket, IAM CreateRole/PutRolePolicy, CreateDeliveryStream -> ACTIVE, PutMetricStream, GetMetricStream State == 'running', PutMetricData) succeeds, so the gap is the delivery/telemetry side effect, not a refusal.

## Steps to Reproduce
1. Create an S3 bucket and two DirectPut Firehose delivery streams with ExtendedS3DestinationConfiguration pointing at two distinct prefixes of that bucket (BufferingHints SizeInMBs 1 / IntervalInSeconds 60, CompressionFormat UNCOMPRESSED); wait for both to be ACTIVE.
2. Create two IAM roles allowing firehose:PutRecord/PutRecordBatch on both delivery-stream ARNs: one whose AssumeRolePolicyDocument trusts streams.metrics.cloudwatch.amazonaws.com, one that trusts only ec2.amazonaws.com.
3. PutMetricStream twice on a fresh namespace (OutputFormat=json, IncludeFilters=[{Namespace: <ns>}]), one per Firehose/role pair; confirm GetMetricStream State == 'running' for both.
4. PutMetricData into that namespace, one datapoint per Shard dimension value (three series).
5. Poll ListObjectsV2 on the trusted stream's prefix for >= 2 objects, and GetMetricStatistics over Namespace AWS/CloudWatch/MetricStreams for MetricUpdate (per-stream and no-dimension, Sum, Period 60) and PublishErrorRate (per stream, Average), re-publishing every 20 s.
6. Observe that nothing ever converges: no delivered objects and/or no AWS/CloudWatch/MetricStreams datapoints appear, and the run is cut off at 600 s.
