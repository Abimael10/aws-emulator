Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.put-metric-data-timestamp-too-far-future-rejected cloudwatch.put-metric-data-timestamp-too-far-past-rejected
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.put-metric-data-timestamp-too-far-future-rejected|_poll_until#12|TimeoutError|raise TimeoutError(
Issue-Key: cloudwatch.put-metric-data-timestamp-too-far-past-rejected|_poll_until#11|TimeoutError|raise TimeoutError(
Issue-Commit: 55e0c2ae4aa0f0ccbc30c11c6116a1652cf54e8a

# cloudwatch: a PutMetricData datapoint published at a current timestamp is never returned by GetMetricStatistics at a sub-minute Period (200 with empty Datapoints for the whole read window)

> _Note: this issue was identified against the code base at commit `55e0c2ae4aa0f0ccbc30c11c6116a1652cf54e8a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
One net-new root cause (Cause 1 of 1 — it must be closed before this issue is submitted), grounded in the two failing checks listed under `### cloudwatch` that carry no ALREADY FILED and no OWNED marker.

=== Cause 1 of 1: a regular-resolution datapoint is invisible to a GetMetricStatistics read whose Period is finer than 60 s ===
Failing checks: cloudwatch.put-metric-data-timestamp-too-far-future-rejected, cloudwatch.put-metric-data-timestamp-too-far-past-rejected.

AWS: Smithy (com.amazonaws.cloudwatch#GraniteServiceVersion20100801) — GetMetricStatistics takes required Namespace (length 1..255, pattern ^[^:]), MetricName (1..255), Dimensions (0..30), StartTime, EndTime, Period (integer, range min 1) and Statistics (1..5); its modelled errors are InvalidParameterValue, InvalidParameterCombination, MissingParameter, InternalServiceError. GetMetricStatisticsInput$Period documents "The granularity, in seconds, of the returned data points. For metrics with regular resolution, a period can be as short as one minute (60 seconds) and must be a multiple of 60. For high-resolution metrics that are collected at intervals of less than one minute, the period can be 1, 5, 10, 20, 30, 60, or any multiple of 60", plus the tiering rule that only applies when StartTime is more than 3 hours old. Recorded behavior for the read exercised here: PutMetricData of ONE MetricDatum (MetricName, Timestamp = now, a numeric Value, Unit Count, StorageResolution NOT set) into a fresh namespace, immediately followed by GetMetricStatistics(Namespace=<that namespace>, MetricName=<that metric>, Dimensions=[], StartTime=<ts − 2 min>, EndTime=<ts + 2 min>, Period=10, Statistics=["Sum"]), answers HTTP 200 with exactly ONE Datapoint whose Sum equals the published Value, within seconds of the publish. Period=10 is not refused, an empty Dimensions list is not refused, and StartTime is well inside the 3-hour band, so no retention/period tier is in play.

Emulator: that read answers HTTP 200 with `Datapoints: []` — every poll, for the whole polling window (~57 s, polled every 3 s), with only `Label` echoing the metric name (content-length 52). The published datum is never surfaced at Period=10, so the positive control of both checks never completes and the later ListMetrics assertion is never reached. What ALREADY PASSED in both checks isolates the divergence to the metric READ path, not to PutMetricData's validation: the refusal legs both matched AWS exactly — a datum timestamped 3 h in the future is refused InvalidParameterValue with "The parameter MetricData.member.1.Timestamp must specify a time no more than two hours in the future." and a datum timestamped 20 days back is refused InvalidParameterValue with "The parameter MetricData.member.1.Timestamp must specify a time within the past two weeks." (fixed text as quoted; the variable part is the member index of the offending MetricData entry). The subsequent accepted PutMetricData also returned 200.

Why: a client cannot tell "this series has no data in this window" from "my store will not answer at the period you asked for". Any caller that publishes a custom metric and reads it back at a sub-minute period — a publish-then-verify step in a deployment or test harness, a dashboard/widget whose refresh resolves to Period<60, an alarm or scaling policy evaluated at a high-resolution period over a namespace that also carries regular-resolution metrics — sees its data as permanently missing against the emulator while the identical code reads the datapoint on real CloudWatch. Downstream that is indistinguishable from no traffic: alarms stay INSUFFICIENT_DATA, verification loops hang until their timeout, totals read as zero rather than as unavailable.

Fix: the storage-granularity gate in the CloudWatch metric read path — the shared window fold behind GetMetricStatistics (and GetMetricData), i.e. the read-side period bucketing in the cloudwatch service's state plus the GetMetricStatistics handler, must still report a datum whose stored aggregation span is coarser than the requested period instead of selecting it out of the result set. Fix it once in the shared fold so GetMetricData inherits it.

RELATED OPEN ISSUES (named per instruction; neither covers these two check ids, neither carries their replay key): 20261118T191219Z-cloudwatch-logs-a-regular-resolution-metric-datapoint-is.md describes the same read-path gate but is grounded only in cloudwatch__logs.emf-log-to-metric-extraction-exactness (EMF publisher, Period=1) under the `cloudwatch__logs` heading; 20261119T065938Z-cloudwatch-2-net-new-causes-associatedatasetkmskey-s-caller.md Cause 2 of 2 pulls the other way — its check cloudwatch.metric-data-high-resolution-period records `Datapoints == []` at Period=1 for a datum published WITHOUT StorageResolution. Hypothesis (neither the model nor these recordings state the rule): the gate is not a blanket "requested period < 60 ⇒ drop the datum", because the recordings here require a regular-resolution datum to be SERVED at Period=10 while that other recording requires it to be ABSENT at Period=1; a fix must satisfy both recordings rather than flipping the behavior wholesale.

Traces: cloudwatch.put-metric-data-timestamp-too-far-future-rejected, cloudwatch.put-metric-data-timestamp-too-far-past-rejected.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator (overall 1108/1252, service heading `### cloudwatch` 675/711, 36 failed — 34 of those marked OWNED/ALREADY FILED, the 2 ids below survive).

[error] cloudwatch.put-metric-data-timestamp-too-far-future-rejected
  File "check_cloudwatch_put_metric_data_timestamp_too_far_future_rejected.py", line 47, in run -> present = _poll_until(...)
  TimeoutError: the check's time budget ran out after 57.1s, before the 60.0s wait ended (last={'Label': 'AcceptedCurrentTimestamp', 'Datapoints': [], 'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.0', 'content-length': '52', ...}}})

[error] cloudwatch.put-metric-data-timestamp-too-far-past-rejected
  File "check_cloudwatch_put_metric_data_timestamp_too_far_past_rejected.py", line 46, in run -> present = _poll_until(...)
  TimeoutError: the check's time budget ran out after 57.1s, before the 60.0s wait ended (last={'Label': 'AcceptedCurrentTimestamp', 'Datapoints': [], 'ResponseMetadata': {'HTTPStatusCode': 200, 'HTTPHeaders': {'content-type': 'application/x-amz-json-1.0', 'content-length': '52', ...}}})

The harness labels a budget-forced stop "not graded", but the graded content of the probe is unambiguous and is the emulator's own answer: for ~19 consecutive polls over 57 s the emulator served HTTP 200 with `Datapoints: []` for a datapoint it had just accepted with a 200. Request ids and timestamps in those responses are per-run and are not part of the requirement.

Expected (recorded AWS), with namespace/metric name/value taken from the request: GetMetricStatistics(..., Period=10, Statistics=["Sum"]) over a ±2 min window around the publish instant -> 200 with exactly one Datapoint whose Sum == the published Value (9.0 in the future-timestamp check, 42.0 in the past-timestamp check — per-run values filling the template, not requirements), followed by ListMetrics(Namespace) eventually listing exactly that one metric name.
Actual (emulator): 200 with `Datapoints: []`, indefinitely.

Already matching AWS in both checks (asserted and passed before the failing read): PutMetricData refuses a Timestamp 3 h in the future with code InvalidParameterValue / message "The parameter MetricData.member.1.Timestamp must specify a time no more than two hours in the future." and a Timestamp 20 days in the past with code InvalidParameterValue / message "The parameter MetricData.member.1.Timestamp must specify a time within the past two weeks."

Smithy consulted: services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json — GetMetricStatistics input signature + modelled errors, GetMetricStatisticsInput$Period documentation.

## Steps to Reproduce
1. Clone the repo and boot the emulator; replay the two ids in trace_ids.
2. Pick a fresh namespace, e.g. Sim2RealPast-<unique>. Call cloudwatch.PutMetricData(Namespace=<ns>, MetricData=[{MetricName:'AcceptedCurrentTimestamp', Timestamp: <now, UTC>, Value: 42.0, Unit:'Count'}]) — no StorageResolution. Expect 200 (this already works).
3. Call cloudwatch.GetMetricStatistics(Namespace=<ns>, MetricName='AcceptedCurrentTimestamp', Dimensions=[], StartTime=<now − 2 min>, EndTime=<now + 2 min>, Period=10, Statistics=['Sum']). EXPECT 200 with exactly one Datapoint, Sum == 42.0. OBSERVE 200 with Datapoints: [] — and it stays empty however long the call is retried (the checks poll every 3 s for ~57 s).
4. Repeat step 3 with Period=60, everything else identical — the datapoint is returned, which isolates the defect to the sub-minute read rather than to ingestion.
5. Then confirm the rest of each check: ListMetrics(Namespace=<ns>) must eventually list exactly ['AcceptedCurrentTimestamp'] (the metric of the rejected datum must never appear).
6. Guard against regressing the sibling recording while fixing this: cloudwatch.metric-data-high-resolution-period requires GetMetricStatistics(Period=1) on a datum published WITHOUT StorageResolution to return Datapoints == [] while its StorageResolution=1 twin returns its value.
7. Re-replay cloudwatch.put-metric-data-timestamp-too-far-future-rejected and cloudwatch.put-metric-data-timestamp-too-far-past-rejected to confirm the fix.
