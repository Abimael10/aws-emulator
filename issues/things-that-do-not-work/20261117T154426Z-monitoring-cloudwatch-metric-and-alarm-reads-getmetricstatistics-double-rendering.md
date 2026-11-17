Issue-Source: trace-sweep-divergence
Issue-Trace: cloudwatch-metric-alarm-explicit-actions-enabled-and-missing-data-tags-metric-data-and-delete
Issue-Step: task#12
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudwatch-metric-alarm-explicit-actions-enabled-and-missing-data-tags-metric-data-and-delete cwp-describe-alarms-prefix-pagination
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|GetMetricStatistics|2xx->2xx|$.GetMetricStatisticsResponse.GetMetricStatisticsResult.Datapoints.member.Maximum
Issue-Key: monitoring|DescribeAlarms|2xx->2xx|$.MetricAlarms[*]
Issue-Commit: addb8a7148576d7d08097a9e3c6488eb5c13818f

# CloudWatch metric and alarm reads: GetMetricStatistics double rendering and DescribeAlarms continuation page

> _Note: this issue was identified against the code base at commit `addb8a7148576d7d08097a9e3c6488eb5c13818f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetMetricStatistics

## Description
2 causes, one shared fault: CloudWatch READ responses are assembled from the right state but serialized/paged wrongly — a modeled `double` is written in integral form on the awsQuery wire, and a paginated DescribeAlarms continuation re-serves items the previous page already returned instead of the exhausted (empty) page. Both are read-path defects in the CloudWatch crate; the operation handlers accept the requests and answer with the correct HTTP status.

SHARED AWS CONTRACT — Smithy: com.amazonaws.cloudwatch#GraniteServiceVersion20100801, protocols [awsJson1_0, awsQuery, rpcv2Cbor] with protocol modifier awsQueryCompatible, xmlNamespace 'http://monitoring.amazonaws.com/doc/2010-08-01/'. On the awsQuery wire a success is '<OperationNameResponse><OperationNameResult>…</OperationNameResult><ResponseMetadata><RequestId>…</RequestId></ResponseMetadata></OperationNameResponse>'; members typed `double` keep their decimal form in the XML text. Paginated list operations are @paginated and a continuation token is honoured by returning the items AFTER the ones already delivered.

Cause 1 of 2: GetMetricStatistics over awsQuery serializes Datapoint double members without their decimal form. AWS: Smithy GetMetricStatistics output Datapoints -> Datapoint, whose SampleCount/Average/Sum/Minimum/Maximum all target DatapointValue, type `double`; on the query wire the XML text of a double-typed member keeps a decimal point, so a datapoint whose maximum is a whole number is rendered with a '.0' fractional part (e.g. '95.0'). Emulator: the same element is rendered in integral form ('95') — the double is formatted as an integer whenever its fractional part is zero. Why: a query-protocol client binding the element to a strict decimal/double type sees an integer where the model declares a double, and any client comparing the value it published with the value it reads back sees a different textual value for the same number. Fix: double value formatting in the CloudWatch crate's awsQuery XML response serializer (it must apply to every double-typed member, Datapoint statistics included, not only alarm thresholds). The open issue 20261113T222916Z-monitoring-cloudwatch-awsquery-xml-serializer-renders-doubles-and-error.md records the same formatting fault for DescribeAlarms' MetricAlarm.Threshold and does NOT own this cluster (the replay reports it unowned): the fix must be made in the shared serializer so Datapoint statistics are covered too. Traces: cloudwatch-metric-alarm-explicit-actions-enabled-and-missing-data-tags-metric-data-and-delete#task#12.

Cause 2 of 2: DescribeAlarms, continued with the NextToken the emulator itself issued for a prefix-scoped listing, answers 200 but re-serves the alarms the first page already returned instead of the exhausted page. AWS: Smithy DescribeAlarms is @paginated(inputToken NextToken, outputToken NextToken, pageSize MaxRecords) with output members CompositeAlarms/MetricAlarms/LogAlarms and MaxRecords @range(min 1, max 100); replaying a returned NextToken yields the NEXT slice of the same AlarmNamePrefix-scoped listing, so once every matching alarm has been delivered the response is 200 with all three lists present and EMPTY and no NextToken. Emulator: 200, but MetricAlarms carries the same prefix-matched alarms again (two extra entries where AWS returns none), i.e. the continuation token does not advance the cursor past the items already served. Why: a client walking DescribeAlarms pages either loops forever or double-counts alarms — the same alarm is processed twice, and a walk that terminates on an empty page never terminates. Fix: DescribeAlarms' pagination cursor in the CloudWatch crate — the token must encode the position reached in the prefix-filtered listing and the handler must resume after it, not restart the scan. Related open issue 20261029T203422Z-monitoring-cloudwatch-alarm-writes-and-pagination-skip-validation-or.md (cause 6) recorded the EARLIER state of this same step, in which the continuation was refused with 400; the token is now accepted, so the refusal is gone and what remains is a distinct, net-new cursor defect that the replay reports as unowned. Traces: cwp-describe-alarms-prefix-pagination#task#7.

## Evidence
Replay of simparity-wire-aws-v1 against the emulator built from this checkout: 882 steps, 796 passed, score 90.2%, 27 divergence clusters, bootstrap_signal false. These are the only two clusters with no owning open issue and no blocking stub.

Cause 1 — fixture cloudwatch-metric-alarm-explicit-actions-enabled-and-missing-data-tags-metric-data-and-delete, step task#12, GetMetricStatistics, status 200 expected and 200 received:
  $.GetMetricStatisticsResponse.GetMetricStatisticsResult.Datapoints.member.Maximum: value_mismatch (expected '95.0', got '95')
  cluster members: task#12 (affected_count 1)
  Model citation: Smithy Datapoint.Maximum -> DatapointValue, type double; service protocols include awsQuery (protocol_modifiers [awsQueryCompatible]).

Cause 2 — fixture cwp-describe-alarms-prefix-pagination, step task#7, DescribeAlarms, status 200 expected and 200 received:
  $.MetricAlarms[0]: extra (expected None, got the prefix-matched alarm '<prefix>-a')
  $.MetricAlarms[1]: extra (expected None, got the prefix-matched alarm '<prefix>-b')
  Both returned entries are the alarms created earlier in the same fixture and already delivered by the preceding page; the recorded response for this step carries empty CompositeAlarms/MetricAlarms/LogAlarms. Generated alarm names, ARNs, CreationIds and timestamps in the returned entries are per-run values and are not part of the requirement.
  cluster members: task#7 (affected_count 1)
  Model citation: Smithy DescribeAlarms paginated {inputToken: NextToken, outputToken: NextToken, pageSize: MaxRecords}; output CompositeAlarms/MetricAlarms/LogAlarms.

## Steps to Reproduce
1. Clone the repo, boot the emulator built from this checkout, and run replay_wire_corpus() over simparity-wire-aws-v1.
2. Cause 1: POST a SigV4-signed application/x-www-form-urlencoded body 'Action=PutMetricData&Version=2010-08-01&Namespace=<ns>&MetricData.member.1.MetricName=<metric>&MetricData.member.1.Value=95' to the monitoring endpoint, then POST 'Action=GetMetricStatistics&Version=2010-08-01&Namespace=<ns>&MetricName=<metric>&StartTime=<t0>&EndTime=<t1>&Period=60&Statistics.member.1=Maximum' (fixture cloudwatch-metric-alarm-explicit-actions-enabled-and-missing-data-tags-metric-data-and-delete step task#12). Read the Maximum element of Datapoints.member: the emulator writes '95' where AWS writes '95.0'.
3. Cause 2: create two alarms sharing an AlarmNamePrefix, call DescribeAlarms with that AlarmNamePrefix and MaxRecords=2 until a NextToken comes back, then call DescribeAlarms again with the same prefix and that NextToken (fixture cwp-describe-alarms-prefix-pagination step task#7). The emulator answers 200 with the same two alarms again; AWS answers 200 with empty CompositeAlarms/MetricAlarms/LogAlarms and no NextToken.
4. Both steps are replayed by replay_wire_corpus as the only two unowned monitoring clusters.
