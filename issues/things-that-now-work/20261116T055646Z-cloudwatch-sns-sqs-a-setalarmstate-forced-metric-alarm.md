Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__sns__sqs.alarm-composite-and-actions-set-alarm-state-fires-action-then-evaluation-overwrites
Issue-Key: cloudwatch__sns__sqs.alarm-composite-and-actions-set-alarm-state-fires-action-then-evaluation-overwrites|_wait_for#23|AssertionError|raise AssertionError(f"timed out after {budget_s:.0f}s waiting for {what}; last={last!r}")
Issue-Commit: 911c830f7d5d4e7f71004d0c38e62fd15ce28281

# cloudwatch__sns__sqs: a SetAlarmState-forced metric-alarm state is never reclaimed by a real evaluation — DescribeAlarms keeps serving the pre-force state record verbatim

> _Note: this issue was identified against the code base at commit `911c830f7d5d4e7f71004d0c38e62fd15ce28281`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetAlarmState

## Description
Cause 1 of 1: after SetAlarmState forces a metric alarm to ALARM, the alarm's state record is never re-authored by the evaluator, so the forced state is neither visible nor reclaimed and no evaluator-authored ALARM -> OK transition is ever produced.

AWS: Smithy (cloudwatch 2010-08-01) SetAlarmState documents it as 'Temporarily sets the state of an alarm for testing purposes. When the updated state differs from the previous value, the action configured for the appropriate state is invoked' and 'Metric alarms returns to their actual state quickly, often within seconds.' So on a metric alarm whose live datapoints are below the threshold, the forced ALARM fires the AlarmActions with the caller-supplied StateReason, and the NEXT evaluation of the real data overwrites it: the alarm's state record is rewritten with StateValue 'OK', an evaluator-authored StateReason whose fixed text is 'Threshold Crossed: <N> out of the last <N> datapoints [<value> (<datapoint timestamp>)] was not greater than the threshold (<threshold>) (minimum <N> datapoint for ALARM -> OK transition).' — the bracketed datapoints, threshold and counts coming from the alarm's configuration and the datapoints actually evaluated — a fresh StateReasonData (new queryDate/startDate and the newly evaluated datapoints), a fresh StateUpdatedTimestamp, and the OKActions are invoked with OldStateValue 'ALARM' / NewStateValue 'OK'. SetAlarmState writes no datapoint into the metric.

Emulator: the forced notification is delivered correctly (NewStateValue 'ALARM', OldStateValue 'OK', NewStateReason = the supplied reason), but for the following 300 s — while the check keeps publishing a below-threshold datapoint (6.0, Period 10, EvaluationPeriods 1, Threshold 50.0, GreaterThanThreshold) every 5 s — DescribeAlarms keeps returning the state record written by the alarm's FIRST INSUFFICIENT_DATA -> OK evaluation, unchanged: the same StateReason (still citing the first datapoint's timestamp), the same StateReasonData (same queryDate/startDate/recentDatapoints) and the same StateUpdatedTimestamp (equal to that StateReasonData queryDate). The forced ALARM therefore never appears in DescribeAlarms and the alarm never performs an ALARM -> OK transition, so the OK action is never fired after the force. Hypothesis: SetAlarmState invokes the actions without persisting the forced state onto the alarm, leaving the evaluator to see an unchanged OK and write no new state record.

Why: SetAlarmState is the documented way to test alarm wiring. A client that forces ALARM gets the paging notification but can never observe the forced state through DescribeAlarms/alarm history, and — worse — never gets the recovery: the alarm is never seen returning to OK, the OK action never fires, so downstream automation (incident auto-resolve, chatops 'resolved' messages, Auto Scaling/Application Auto Scaling policies keyed on the OK transition) stays latched in the alarmed branch after a test that on AWS self-heals within seconds. Any code that asserts 'the forced state is temporary' hangs.

Fix: the CloudWatch alarm state component — SetAlarmState must persist the forced StateValue/StateReason/StateReasonData and StateUpdatedTimestamp onto the alarm (and record the transition) in addition to invoking the actions, and the alarm evaluator must re-author the state record on its next evaluation of the real datapoints (new StateValue, evaluator-authored StateReason citing the evaluated datapoints, new StateReasonData queryDate and new StateUpdatedTimestamp) and invoke the OKActions for the resulting ALARM -> OK transition.

Traces: cloudwatch__sns__sqs.alarm-composite-and-actions-set-alarm-state-fires-action-then-evaluation-overwrites. This is NOT covered by the open issue 20261109T183935Z-cloudwatch-sns-sqs-3-net-new-causes-the.md (Trigger.Statistic casing, EvaluationState during warm-up, AWS/SNS NumberOfMessagesPublished), which that check does not touch.

## Evidence
Check cloudwatch__sns__sqs.alarm-composite-and-actions-set-alarm-state-fires-action-then-evaluation-overwrites.
Setup: PutMetricAlarm(Statistic='Maximum', Period=10, EvaluationPeriods=1, Threshold=50.0, ComparisonOperator='GreaterThanThreshold', TreatMissingData='ignore', AlarmActions=[topic], OKActions=[topic]); PutMetricData value 6.0 every 5 s; alarm reaches OK and the INSUFFICIENT_DATA->OK notification is delivered (positive control passes). SetAlarmState(StateValue='ALARM', StateReason='sim2real: forced by SetAlarmState, no datapoint breached') then delivers the ALARM notification with that exact NewStateReason and OldStateValue 'OK' (also passes).

Expected (AWS): within seconds, DescribeAlarms shows the alarm back at OK with a NEW evaluator-authored state record — StateReason 'Threshold Crossed: 1 out of the last 1 datapoints [6.0 (<fresh timestamp>)] was not greater than the threshold (50.0) (minimum 1 datapoint for ALARM -> OK transition).', a StateReasonData with a fresh queryDate, and an advanced StateUpdatedTimestamp — followed by the ALARM->OK notification on OKActions.

Actual (emulator): AssertionError: timed out after 300s waiting for real evaluation to overwrite the forced ALARM back to OK; last DescribeAlarms entry is the pre-force record verbatim — StateValue 'OK', StateReason 'Threshold Crossed: 1 out of the last 1 datapoints [6.0 (16/11/26 05:16:00)] was not greater than the threshold (50.0) (minimum 1 datapoint for ALARM -> OK transition).', StateReasonData queryDate '2026-11-16T05:16:15.764+0000' with recentDatapoints [6.0], StateUpdatedTimestamp 2026-11-16T05:16:15.764 — i.e. identical to the first OK evaluation, written before SetAlarmState was called and never rewritten in the following 300 s. (The literal timestamps/names here are this run's values; the requirement is that the record be re-authored, not these numbers.)

## Steps to Reproduce
1. Create an SNS topic subscribed by an SQS queue; PutMetricAlarm on a fresh namespace/dimension with Statistic=Maximum, Period=10, EvaluationPeriods=1, Threshold=50, GreaterThanThreshold, TreatMissingData=ignore, AlarmActions=[topic], OKActions=[topic].
2. PutMetricData value 6.0 (below threshold) repeatedly until DescribeAlarms reports OK; note StateReason, StateReasonData.queryDate and StateUpdatedTimestamp.
3. Call SetAlarmState(AlarmName, StateValue='ALARM', StateReason='<any text>'). The ALARM notification arrives.
4. Keep publishing 6.0 and poll DescribeAlarms: on AWS the alarm returns to OK within seconds with a NEW StateReason/StateReasonData.queryDate/StateUpdatedTimestamp and an ALARM->OK notification is delivered; on the emulator the three fields stay exactly as recorded in step 2 for at least 300 s and no ALARM->OK notification is delivered.
