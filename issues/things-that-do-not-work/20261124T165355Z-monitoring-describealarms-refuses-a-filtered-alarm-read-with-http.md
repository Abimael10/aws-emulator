Issue-Source: trace-sweep-divergence
Issue-Trace: cloudwatch-alarm-firing-describe-filter-mistakes
Issue-Step: task#5
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: cloudwatch-alarm-firing-describe-filter-mistakes cloudwatch-albos-alarm-lookup-and-state-mistakes cloudwatch-s3tfa-traj-describe-alarms-filter-mistakes-then-scoped-hit
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: monitoring|DescribeAlarms|2xx->4xx|status
Issue-Related: 20261029T203422Z-monitoring-cloudwatch-alarm-writes-and-pagination-skip-validation-or.md
Issue-Commit: 061fcc8ecfb1ea3767cdd9cc8542bb3501498b07

# DescribeAlarms refuses a filtered alarm read with HTTP 400 instead of answering 200 with the matching alarms

> _Note: this issue was identified against the code base at commit `061fcc8ecfb1ea3767cdd9cc8542bb3501498b07`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeAlarms

## Description
Cause 1 of 1: a scoped DescribeAlarms read is refused with HTTP 400 ValidationException where AWS answers HTTP 200 and reports the alarms the filters select.

AWS: recorded behavior only. The Smithy model (smithy_query query="operation" operation="DescribeAlarms" on services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json) declares DescribeAlarms with the selector members AlarmNames (list, @length 0..100), AlarmNamePrefix (@length 1..255), AlarmTypes, StateValue, ActionPrefix, ChildrenOfAlarmName/ParentsOfAlarmName and the paging members MaxRecords (@range 1..100) / NextToken, and its ONLY modeled error is InvalidNextToken (400). No modeled constraint and no modeled error covers a combination of selectors, so a request that satisfies every member constraint is answered 200. The recording confirms this: for the same request the service returns 200 with all three result lists present — CompositeAlarms: [], LogAlarms: [] and MetricAlarms carrying the fully populated metric alarm (AlarmName/AlarmArn, AlarmDescription, Namespace, MetricName, ExtendedStatistic, Period, EvaluationPeriods, Threshold, ComparisonOperator, TreatMissingData, StateValue ALARM, StateReason, the state/configuration timestamps and the empty action lists). Because the reported alarm carries the FULL metric-alarm projection, the recorded request is not a ChildrenOfAlarmName/ParentsOfAlarmName relationship read (those report only Name/ARN, and for children also StateValue and StateUpdatedTimestamp), i.e. AWS accepts the plain selector combination this step sends.

Emulator: the step is answered HTTP 400. The three expected output lists are absent entirely from the body (CompositeAlarms, LogAlarms and MetricAlarms all missing), so a caller gets a refusal in place of the alarm inventory. The handler has exactly two refusal paths of its own and both produce 400 ValidationException via crate::scaffolded::Scaffolded::validation: (a) the relationship-exclusivity refusal "<ChildrenOfAlarmName|ParentsOfAlarmName> requests cannot be used with the following parameters: ActionPrefix, AlarmNamePrefix, AlarmNames, AlarmTypes, StateValue", and (b) the fixed-text refusal "AlarmNamePrefix cannot be combined with AlarmNames". Hypothesis: path (b) fires here — the handler turns the advisory prose on DescribeAlarmsInput$AlarmNamePrefix ("If this parameter is specified, you cannot specify AlarmNames") into a hard wire refusal, while the recording shows the real service accepting AlarmNames together with AlarmNamePrefix and simply applying the filters. The 400 is the defect either way: for this request shape the required response is 200 with the three lists, and no error code or error message at all.

Why: a client that reads alarm state through DescribeAlarms (console-style inventory, an operator narrowing a lookup after a mis-typed filter, any polling loop that scopes by name plus prefix, or an SDK paginator) sees a ValidationException where AWS returns data. The alarm is never observed, so state-dependent logic — waiting for ALARM, enumerating alarms before editing them, or confirming an alarm exists — fails against the emulator although the same call succeeds against AWS. It is a false refusal of a read, so it cannot be worked around by retrying.

Fix: the request-admission logic of the DescribeAlarms handler — the selector-combination checks that run before the store is consulted. The combination of AlarmNames with AlarmNamePrefix (and, more generally, any selector combination the model does not constrain) must not short-circuit into a 400; the filters should be applied and the normal 200 projection (CompositeAlarms / LogAlarms / MetricAlarms plus NextToken when a page is cut) returned. Only the model's own constraints and InvalidNextToken may produce a 4xx from this operation; the relationship-exclusivity refusal must stay confined to requests that actually carry ChildrenOfAlarmName or ParentsOfAlarmName.

Traces: cloudwatch-alarm-firing-describe-filter-mistakes#task#5 (representative), cloudwatch-albos-alarm-lookup-and-state-mistakes#task#5, cloudwatch-s3tfa-traj-describe-alarms-filter-mistakes-then-scoped-hit#task#7. Code citations for the two refusal paths: services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/ops/describe_alarms.rs lines 221-232 (relationship exclusivity) and lines 234-240 (AlarmNamePrefix/AlarmNames exclusivity), with the excluded-selector list at lines 474-488. Related but NOT covering this failure: open issue 20261116T185151Z-monitoring-cloudwatch-describealarmcontributors-refuses-a-contributor-read-with-http.md (same shape of defect — a 400 where AWS answers 200 — but for DescribeAlarmContributors) and open issue 20261029T203422Z-monitoring-cloudwatch-alarm-writes-and-pagination-skip-validation-or.md (alarm WRITES that skip validation, i.e. the opposite direction).

## Evidence
Replay step cloudwatch-alarm-firing-describe-filter-mistakes#task#5 (operation DescribeAlarms, service monitoring):

Expected (recorded AWS response): status 200, body
  CompositeAlarms: []
  LogAlarms: []
  MetricAlarms: [ { AlarmName/AlarmArn of the latency alarm, AlarmDescription 'P90 FrameQuartzLag over 15s', Namespace, MetricName 'Time', ExtendedStatistic 'p90', Period 60, EvaluationPeriods 3, Threshold 15000.0, ComparisonOperator 'GreaterThanThreshold', TreatMissingData 'breaching', StateValue 'ALARM', StateReason 'Threshold Crossed: no datapoints were received for 3 periods and 3 missing datapoints were treated as [Breaching].', ActionsEnabled false, empty OKActions/AlarmActions/InsufficientDataActions, Dimensions [Camera_name=ALL, Operation=FrameQuartzLag], and the state/configuration timestamps } ]
(the alarm name, ARN, namespace, dimension values and timestamps above are per-run values of that recording, not part of the requirement; the requirement is status 200 with the three lists present and the matching alarm reported in MetricAlarms with its full projection)

Actual (emulator): status 400. $.CompositeAlarms missing (expected [], got None); $.LogAlarms missing (expected [], got None); $.MetricAlarms missing (expected the one-alarm list, got None). The only 400s this handler raises are ValidationException refusals with fixed text and no request-derived parts: "AlarmNamePrefix cannot be combined with AlarmNames" (no variable part) and "<relationship member> requests cannot be used with the following parameters: ActionPrefix, AlarmNamePrefix, AlarmNames, AlarmTypes, StateValue" (variable part: which relationship member the request used). AWS sends no error code and no message for this request at all.

Same divergence on 2 further steps (same divergence_key service=monitoring|DescribeAlarms|status=200->400|keys=$.CompositeAlarms,$.LogAlarms,$.MetricAlarms,status): cloudwatch-albos-alarm-lookup-and-state-mistakes#task#5 and cloudwatch-s3tfa-traj-describe-alarms-filter-mistakes-then-scoped-hit#task#7.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus simparity-wire-aws-v1 (replay_wire_corpus).
2. Fixture cloudwatch-alarm-firing-describe-filter-mistakes, step task#5: the fixture creates a metric alarm that transitions to ALARM, makes two filter mistakes on DescribeAlarms, then issues the scoped DescribeAlarms read at task#5.
3. Observe the emulator answers HTTP 400 (ValidationException) with no CompositeAlarms/LogAlarms/MetricAlarms members, while the recording has HTTP 200 with CompositeAlarms: [], LogAlarms: [] and the matching metric alarm in MetricAlarms.
4. The same refusal reproduces at cloudwatch-albos-alarm-lookup-and-state-mistakes#task#5 and cloudwatch-s3tfa-traj-describe-alarms-filter-mistakes-then-scoped-hit#task#7.
