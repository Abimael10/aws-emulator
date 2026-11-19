Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.alarm-missing-data-and-m-of-n-period-window-slides-by-minute-and-never-evaluates-a-partial-period
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.alarm-missing-data-and-m-of-n-period-window-slides-by-minute-and-never-evaluates-a-partial-period|run#13|AssertionError|assert start + timedelta(seconds=_PERIOD) <= item_ts, (
Issue-Commit: 13e968f2d712b86cbfec63dd795b2bc21b7f9b4c

# cloudwatch: metric-alarm evaluation grades a window that has not closed — the sliding window's end is rounded UP to the next minute, so the still-open minute is aggregated into the evaluated datapoint

> _Note: this issue was identified against the code base at commit `13e968f2d712b86cbfec63dd795b2bc21b7f9b4c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutMetricAlarm

## Description
Cause 1 of 1: the alarm evaluator's sliding window ends at the minute boundary AFTER the evaluation instant, so every evaluation reads a period that has not finished.
AWS: Smithy (services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801) models PutMetricAlarm.Period, EvaluationPeriods, DatapointsToAlarm and the EvaluationWindow union (SlidingWindow default / WallClockWindow); the windowing rule itself is documented prose (https://docs.aws.amazon.com/AmazonCloudWatch/latest/monitoring/AlarmThatSendsEmail.html), so recorded behavior: a default (sliding) metric alarm with Period=300 over a series published once per minute re-anchors its evaluated window every minute, and the window it grades is always a COMPLETE one — its start is minute-aligned (second == 0, microsecond == 0), its length is exactly Period, and start + Period is at or before the instant of the evaluation that read it. The consequences the recording shows for Period=300 over a 60 s series: StateReasonData.evaluatedDatapoints carries exactly one datapoint whose sampleCount is 5.0 (five complete one-minute samples — fewer means a partial period was graded), whose timestamp equals StateReasonData.startDate, and whose value is the Sum of exactly those five published minutes; successive evaluated windows start a whole number of minutes apart with at least one 60 s step (minute re-anchoring, not fixed 300 s buckets), and not every start sits on a 300 s epoch boundary. The query API is the contrast and is NOT at issue: GetMetricStatistics(Period=300) buckets the same data on 300 s epoch boundaries.
Emulator: an evaluation recorded in DescribeAlarmHistory at 12:07:17.777 carries a StateReasonData window starting 12:03:00, i.e. ending 12:08:00 — 43 s in the future at the moment the evaluation ran. sampleCount for that window is 5.0 and the Sum matches five published minutes 12:03..12:07, so the still-open current minute (12:07, published mid-minute) was aggregated as if complete: the window end is rounded UP to the next minute boundary instead of being the last CLOSED minute boundary. Nothing stops the alarm transitioning on data that the period has not finished collecting.
Why: a client reconciling an alarm's decision against GetMetricStatistics reads an evaluated range that reaches into the future and can never be reproduced — the Sum/Average/SampleCount the alarm acted on includes a partially filled minute, so for Sum- and SampleCount-based alarms the value is systematically understated for breach-on-high alarms (and a breach can be declared early for breach-on-low ones) and the state flaps as the open minute fills. Alarm-driven automation (scaling, rollback, paging) fires on an aggregate that was never a real period. The StateReasonData a client or operator reads to justify a transition names a window the metric store cannot yet answer for.
Fix: the look-back/windowing in the CloudWatch alarm evaluation engine (services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/state/alarm_evaluation.rs) and the StateReasonData it writes — the newest gradeable window must be the latest minute-anchored span of length Period whose END is at or before the evaluation instant (so the open minute is excluded), while keeping the minute re-anchoring. Nearest open issue, which this is not covered by: 20261029T055514Z-cloudwatch-a-slidingwindow-default-metric-alarm-never-transitions.md asks for exactly this minute-anchored sliding window in place of the old period-anchored `newest_graded_period = floor(now/period) - 1`; it carries a different check id and describes the OPPOSITE error (grading only closed, period-anchored windows, so a sliding alarm never transitioned inside the open period). The window is now sliding but it overshoots past `now`.
Traces: cloudwatch.alarm-missing-data-and-m-of-n-period-window-slides-by-minute-and-never-evaluates-a-partial-period.

## Evidence
Replay of simparity-behavioral-aws-v1 against the emulator built from this checkout.

cloudwatch.alarm-missing-data-and-m-of-n-period-window-slides-by-minute-and-never-evaluates-a-partial-period [mismatch]:
AssertionError: "the evaluation at datetime.datetime(2026, 11, 16, 12, 7, 17, 777000, tzinfo=tzlocal()) read a window ending datetime.datetime(2026, 11, 16, 12, 8, tzinfo=datetime.timezone.utc), which had not closed yet" — the assertion is `start + timedelta(seconds=Period) <= item_ts`, where item_ts is the DescribeAlarmHistory item's own Timestamp and start is that item's HistoryData.newState.stateReasonData.startDate.

Setup: PutMetricData publishes one datapoint per whole minute into a unique namespace (alternating 100.0 / 1.0, each minute published exactly once because Sum and SampleCount are not idempotent under re-publishing); the alarm is Period=300, Statistic=Sum, EvaluationPeriods=1, DatapointsToAlarm=1, Threshold=250.0, ComparisonOperator=GreaterThanThreshold, TreatMissingData='missing', ActionsEnabled=false. DescribeAlarmHistory(HistoryItemType='StateUpdate', ScanBy='TimestampAscending') is paginated and every item carrying stateReasonData.evaluatedDatapoints is inspected.

What passed first on the offending item, and so is NOT part of this finding: stateReasonData.period == 300 and statistic == 'Sum'; exactly one evaluatedDatapoint whose timestamp equals startDate; startDate minute-aligned (second and microsecond zero); sampleCount == 5.0; every minute of [startDate, +300 s) present in the check's own publish ledger; the datapoint value equal to the Sum of those five published minutes. Those passing assertions are what pins the defect: the five aggregated minutes include the minute containing the evaluation instant, which was still open.

Expected: for an evaluation recorded at time T, the graded window is the latest minute-anchored 300 s span with start + 300 s <= T (here [12:02:00, 12:07:00)), with sampleCount 5.0 over the five minutes 12:02..12:06.
Actual: the window [12:03:00, 12:08:00) — end 43 s after T.

The clock times, namespace and alarm name above are this run's values; the requirement is the relation start + Period <= (evaluation timestamp), together with the minute re-anchoring the same check asserts (successive window starts a whole number of minutes apart, at least one 60 s step, and not all starts on a 300 s epoch boundary).

## Steps to Reproduce
1. Build and boot the emulator from this checkout; point a botocore cloudwatch client at it.
2. Pick a unique namespace NS and metric M. Once per minute, PutMetricData(Namespace=NS, MetricData=[{MetricName:M, Timestamp:<that whole minute>, Value:100.0 or 1.0 alternating, Unit:'Count'}]) — each minute exactly once.
3. PutMetricAlarm(AlarmName=A, Namespace=NS, MetricName=M, Statistic='Sum', Period=300, EvaluationPeriods=1, DatapointsToAlarm=1, Threshold=250.0, ComparisonOperator='GreaterThanThreshold', TreatMissingData='missing', ActionsEnabled=False).
4. Keep publishing until DescribeAlarmHistory(AlarmName=A, HistoryItemType='StateUpdate', ScanBy='TimestampAscending') holds at least four items whose HistoryData.newState.stateReasonData carries evaluatedDatapoints.
5. For each such item compare the item's Timestamp T with stateReasonData.startDate: startDate + 300 s must be <= T. Observed: startDate + 300 s lands after T (the window ends at the minute boundary following T), and the single evaluatedDatapoint's sampleCount is 5.0 including the minute still being published into.
6. Also confirm the invariants that must survive the fix: successive startDates differ by whole minutes with at least one 60 s step, not every startDate is on a 300 s epoch boundary, and GetMetricStatistics(Period=300) still buckets the same data on 300 s epoch boundaries.
7. Re-replay the check id in trace_ids; it must pass.
