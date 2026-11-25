Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.alarm-actions-disabled-suppresses-state-triggered-action
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.alarm-actions-disabled-suppresses-state-triggered-action|_receive#27|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Commit: e5aaacd45122de0ad72e17122715047bd1f281f6

# cloudwatch: the DisableAlarmActions suppression scenario reaches no verdict — the SetAlarmState alarm -> SNS -> SQS notification is never observed inside the per-check cap

> _Note: this issue was identified against the code base at commit `e5aaacd45122de0ad72e17122715047bd1f281f6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetAlarmState

## Description
One net-new root cause under the `### cloudwatch` heading (Cause 1 of 1). Every other failure under that heading is marked OWNED by an open issue; this id is marked neither ALREADY FILED nor OWNED, so no open issue carries its replay key.

Cause 1 of 1 — the ActionsEnabled suppression scenario never reaches a verdict. Failing check: cloudwatch.alarm-actions-disabled-suppresses-state-triggered-action.
AWS: Smithy: SetAlarmState (com.amazonaws.cloudwatch) takes AlarmName/StateValue/StateReason (all required) and models only InvalidFormatFault (400) and ResourceNotFound (404); its documentation states the behavioral requirement — "Temporarily sets the state of an alarm for testing purposes. When the updated state differs from the previous value, the action configured for the appropriate state is invoked. For example, if your alarm is configured to send an Amazon SNS message when an alarm is triggered, temporarily changing the alarm state to ALARM sends an SNS message." DisableAlarmActions / EnableAlarmActions model no errors and flip the alarm's ActionsEnabled; while ActionsEnabled is false no configured action may be invoked, and once it is true again the next differing transition must invoke it. The recorded scenario is: PutMetricAlarm with AlarmActions=[<topic>], an SQS queue subscribed to that topic with a policy allowing sns.amazonaws.com to SendMessage; (0) SetAlarmState(StateValue='ALARM') must deliver exactly one notification whose JSON Message carries AlarmName == the alarm and NewStateValue == 'ALARM' (positive control, which also measures the delivery latency); (1) after DisableAlarmActions, DescribeAlarms must report ActionsEnabled false and a fresh OK -> ALARM transition must deliver NOTHING over a window of at least max(60 s, 3x the measured latency), with ApproximateNumberOfMessages == 0; (2) after EnableAlarmActions, DescribeAlarms must report ActionsEnabled true and the next fresh OK -> ALARM transition must deliver exactly one notification, whose NewStateReason is that transition's reason and which is the FIRST notification since the baseline (a suppressed transition's notification, if one had been sent, would precede it).
Emulator: the check produced `Error: Check timed out after 60.0s`. No assertion was reached, so no status, error code or message was observed from the emulator and none of the three legs was graded. Hypothesis: the baseline alarm-action delivery (SetAlarmState -> AlarmActions publish -> SNS -> SQS) does not arrive, so the 90 s positive-control poll consumes the whole budget; a sibling finding on the same delivery path is open as 20261120T231756Z-cloudwatch-sns-sqs-an-evaluation-driven-metric-alarm.md (an evaluation-driven alarm delivers no AlarmActions notification) and does not carry this check. The alternative reading is the suite's per-check budget: this check's own mandatory waits (a 90 s baseline poll plus a >=60 s absence window) cannot fit inside a 60 s cap, which is the same budget-exhaustion shape already filed for a neighbouring heading as 20261124T033148Z-cloudwatch-sns-the-cfn-alarm-restricted-topic-policy.md. It is filed because it is the heading's only surviving failure and because the delivery leg it blocks is unverified either way.
Why: the ActionsEnabled boundary is ungraded. A client that disables alarm actions during a maintenance window has no evidence the emulator withholds notifications, and a client that re-enables them has no evidence the next transition notifies at all — an emulator that notifies while ActionsEnabled is false (or that never notifies) reads identically from this suite.
Fix: the SetAlarmState handler's action-invocation path and the alarm-action delivery component it calls (the state-transition publish to AlarmActions, and its ActionsEnabled gate), in the cloudwatch service crate's SetAlarmState op and its alarm_actions state module; confirm by timing the baseline leg alone (PutMetricAlarm + SetAlarmState + one ReceiveMessage) before touching the suppression logic.
Traces: cloudwatch.alarm-actions-disabled-suppresses-state-triggered-action.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator. Service heading `### cloudwatch`: 685/711 (96%), 26 failed; 25 of those are marked OWNED by open issues, leaving this one.

[mismatch] cloudwatch.alarm-actions-disabled-suppresses-state-triggered-action
  Error: Check timed out after 60.0s

Nothing from the emulator was observed — no HTTP status, no error code, no message. The check's sequence is: CreateTopic, CreateQueue, SetQueueAttributes(Policy allowing sns.amazonaws.com SendMessage with ArnEquals aws:SourceArn), Subscribe(Protocol='sqs'), PutMetricAlarm(AlarmActions=[<topic ARN>], Period=60, EvaluationPeriods=1, Threshold=1.0, ComparisonOperator='GreaterThanThreshold', TreatMissingData='missing'), then SetAlarmState ALARM (baseline, polled up to 90 s for one message), SetAlarmState OK, DisableAlarmActions, DescribeAlarms (ActionsEnabled must be false), SetAlarmState ALARM (absence held for max(60 s, 3x baseline latency), plus ApproximateNumberOfMessages == 0), EnableAlarmActions, DescribeAlarms (ActionsEnabled must be true), SetAlarmState OK, SetAlarmState ALARM, then the restored notification must be the only one since the baseline.

Expected: one baseline notification (Message.AlarmName == the alarm, Message.NewStateValue == 'ALARM'); nothing at all while ActionsEnabled is false; exactly one notification after EnableAlarmActions, carrying that transition's NewStateReason and NewStateValue 'ALARM'.
Actual: no verdict — the run was ended by the 60 s per-check cap before any of those steps was reported. Topic/queue/alarm names and the measured latency are per-run values, not part of the requirement.

## Steps to Reproduce
1. Replay cloudwatch.alarm-actions-disabled-suppresses-state-triggered-action; it is cut at 60 s with no assertion reached.
2. Against the booted emulator, time the baseline leg alone: sns CreateTopic, sqs CreateQueue, sqs SetQueueAttributes with a Policy allowing sns.amazonaws.com sqs:SendMessage on the queue ARN, sns Subscribe(Protocol='sqs', Endpoint=<queue ARN>), cloudwatch PutMetricAlarm(AlarmName=<n>, Namespace=<ns>, MetricName='Errors', Statistic='Average', Period=60, EvaluationPeriods=1, Threshold=1.0, ComparisonOperator='GreaterThanThreshold', AlarmActions=[<topic ARN>], TreatMissingData='missing').
3. cloudwatch SetAlarmState(AlarmName=<n>, StateValue='ALARM', StateReason='baseline') and poll sqs ReceiveMessage: one envelope must arrive whose json.loads(Body)['Message'] has AlarmName == <n> and NewStateValue == 'ALARM'. Record how long it took.
4. SetAlarmState(StateValue='OK'), cloudwatch DisableAlarmActions(AlarmNames=[<n>]), confirm DescribeAlarms reports ActionsEnabled false, then SetAlarmState(StateValue='ALARM', StateReason='suppressed'): no message may arrive over at least max(60 s, 3x the time from step 3), and GetQueueAttributes(ApproximateNumberOfMessages) must read 0.
5. cloudwatch EnableAlarmActions(AlarmNames=[<n>]), confirm ActionsEnabled true, SetAlarmState OK then SetAlarmState ALARM with a distinct StateReason: exactly one notification must arrive and it must be the first since step 3, carrying that StateReason as NewStateReason.
6. Re-replay the check id to confirm the scenario reaches a verdict inside the suite's cap.
