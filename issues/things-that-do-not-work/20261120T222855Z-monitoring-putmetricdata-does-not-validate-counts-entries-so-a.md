Issue-Source: trace-sweep-divergence
Issue-Trace: cw-pmd-counts-negative-rejected
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cw-pmd-counts-negative-rejected
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|PutMetricData|4xx->2xx|$
Issue-Key: monitoring|PutMetricData|4xx->2xx|$headers.x-amzn-query-error
Issue-Key: monitoring|PutMetricData|4xx->2xx|status
Issue-Key: monitoring|GetMetricStatistics|2xx->2xx|$.Datapoints[*]
Issue-Commit: 68256a8854554c92742ca734b435f5200fba6c83

# PutMetricData does not validate Counts entries, so a datum AWS rejects is ingested and then surfaces in GetMetricStatistics

> _Note: this issue was identified against the code base at commit `68256a8854554c92742ca734b435f5200fba6c83`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricData

## Description
This issue bundles 2 net-new root causes, both in the cloudwatch (monitoring, 2010-08-01) emulator and both reached from the same fixture. Both must be closed before submitting. The open issue 20261117T121318Z-cloudwatch-9-net-new-causes-putmetricdata-accepts-an.md also concerns PutMetricData input validation, but its causes are the empty-MetricData batch and the colon-prefixed dimension name; neither it nor any other open issue carries the replay keys below, so the Counts validation and the state pollution that follows it are not covered.

Cause 1 of 2: PutMetricData accepts a MetricDatum whose Counts array contains a non-positive value, instead of refusing the batch.
AWS: Smithy models MetricDatum$Counts as a list of DatapointValue (double) paired positionally with MetricDatum$Values, documented as "Each number in the Count array is the number of times the corresponding value in the Values array occurred during the period" — a number of occurrences, so it must be strictly greater than 0. PutMetricData declares InvalidParameterValueException (errorKind client, httpError 400, awsQueryError code InvalidParameterValue / 400). A Counts entry that is not greater than 0 is refused with HTTP 400, __type com.amazon.coral.service#InvalidParameterValueException, response header x-amzn-query-error: 'InvalidParameterValue;Sender', and the message 'The parameter MetricData.member.<n>.Counts must contain only values greater than 0.' — fixed text 'The parameter ' and ' must contain only values greater than 0.', with the AWS-Query member path of the offending datum ('MetricData.member.<n>.Counts', the index taken from the request) as the only variable part. The whole batch is rejected, so nothing is stored.
Emulator: the call SUCCEEDS — HTTP 200 with an empty JSON body ({}), no __type, no message, and no x-amzn-query-error header at all.
Why: a client that submits a malformed Values/Counts histogram believes CloudWatch accepted it. AWS's 400 is the only signal that the batch was bad, and the emulator instead admits nonsense counts into the metric store, where they corrupt every later aggregation of that metric (see Cause 2).
Fix: PutMetricData's input validation — the per-datum Values/Counts check, before the ingest path, raising the InvalidParameterValueException envelope (status, __type, x-amzn-query-error and the message above).
Trace: cw-pmd-counts-negative-rejected#task#3 (operation PutMetricData).

Cause 2 of 2: because the invalid datum is ingested, GetMetricStatistics reports a datapoint for a period AWS leaves empty, and aggregates it incoherently.
AWS: GetMetricStatistics returns Datapoints aggregated from the data actually stored for the metric. Since the PutMetricData call of Cause 1 is refused in full, nothing is stored for that metric/period and the response carries an empty Datapoints list (recorded behavior only: the model constrains the request, not which datapoints exist). Datapoint members are modelled per aggregation — SampleCount is the number of samples contributing to the period and Sum is their total, so a datapoint whose SampleCount is 0 while Sum, Minimum and Maximum are populated is not a shape AWS can produce.
Emulator: Datapoints carries one extra entry derived from the rejected datum, with SampleCount 0.0 and a negative Sum alongside a populated Minimum/Maximum and Unit 'None' — an internally inconsistent aggregate (zero samples, non-zero sum).
Why: the client's very next read reports metric data that AWS says does not exist, with a SampleCount/Sum pair that cannot be reconciled. Anything that divides Sum by SampleCount (the standard way to recover an average) divides by zero, and alarm evaluation or dashboards over that metric see a value the real service never serves.
Fix: primarily the Counts validation of Cause 1, which keeps the datum out of the metric store; additionally the metric aggregation in GetMetricStatistics' state/aggregation path should never emit a datapoint with SampleCount 0, so a period with no contributing samples yields no datapoint rather than a zero-sample aggregate.
Trace: cw-pmd-counts-negative-rejected#task#4 (operation GetMetricStatistics).

Note on values: the concrete numbers, timestamps, namespace and metric names in the recording fill the templates above; they are not part of the requirement. The requirements are the 400 status, the InvalidParameterValueException __type, the 'InvalidParameterValue;Sender' query-error code, the message sentence with the request's member path, and the absence of any datapoint for a refused batch.

## Evidence
Cause 1 — PutMetricData, fixture cw-pmd-counts-negative-rejected step task#3.
Expected: status 400; body {'__type': 'com.amazon.coral.service#InvalidParameterValueException', 'message': 'The parameter MetricData.member.1.Counts must contain only values greater than 0.'}; header x-amzn-query-error: 'InvalidParameterValue;Sender'.
Actual: status 200; body {} (empty_body — no __type, no message); x-amzn-query-error missing (expected 'InvalidParameterValue;Sender', got None).

Cause 2 — GetMetricStatistics, fixture cw-pmd-counts-negative-rejected step task#4.
Expected: $.Datapoints[0] absent (no datapoint for the refused batch).
Actual: $.Datapoints[0] extra — {'Timestamp': <per-run>, 'SampleCount': 0.0, 'Sum': -1.0, 'Minimum': 1.0, 'Maximum': 2.0, 'Unit': 'None'}. The timestamp is per-run; the divergence is that a datapoint exists at all, and that it reports SampleCount 0.0 together with a non-zero Sum.

## Steps to Reproduce
1. Pick a fresh namespace N and metric name M.
2. Call PutMetricData(Namespace=N, MetricData=[{MetricName: M, Values: [1.0, 2.0], Counts: [-1.0, 1.0], Unit: 'None'}]) — i.e. a Counts array containing a value that is not greater than 0.
3. Read the HTTP status, the response body's __type and message, and the x-amzn-query-error response header. Expect 400 / com.amazon.coral.service#InvalidParameterValueException / 'The parameter MetricData.member.1.Counts must contain only values greater than 0.' / 'InvalidParameterValue;Sender'. The emulator returns 200 with an empty body (Cause 1).
4. Call GetMetricStatistics(Namespace=N, MetricName=M, StartTime=<before the datum>, EndTime=<after it>, Period=60, Statistics=['SampleCount','Sum','Minimum','Maximum']) and read Datapoints. Expect an empty list, because the publish in step 2 was refused; the emulator returns one datapoint with SampleCount 0.0 and Sum -1.0 (Cause 2).
5. Repeat step 2 with Counts=[1.0, 1.0] as a positive control — that call succeeds with 200 on AWS and on the emulator, so the refusal in step 3 is attributable to the non-positive Counts entry alone.
