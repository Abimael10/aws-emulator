Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch.alarm-actions-disabled-suppresses-state-triggered-action
Issue-Shape-Id: com.amazonaws.cloudwatch#GraniteServiceVersion20100801
Issue-Key: cloudwatch.alarm-actions-disabled-suppresses-state-triggered-action|_receive#27|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=2)
Issue-Commit: 027692653b37f8059828c0218f5349555bde1430

# cloudwatch: a metric alarm forced into ALARM by SetAlarmState never delivers its AlarmActions SNS notification, so the DisableAlarmActions/EnableAlarmActions suppression contrast reaches no verdict (1 cause)

> _Note: this issue was identified against the code base at commit `027692653b37f8059828c0218f5349555bde1430`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetAlarmState

## Description
Cause 1 of 1: `SetAlarmState` flips the alarm's state but never executes the alarm's `AlarmActions`, so an SNS topic named as an AlarmAction publishes nothing and a queue subscribed to that topic receives nothing.

AWS: Smithy (com.amazonaws.cloudwatch#GraniteServiceVersion20100801): `SetAlarmState` takes required `AlarmName` (@length 1..255), required `StateValue` (enum), required `StateReason` (@length 0..1023) and optional `StateReasonData` (@length 0..4000); modeled errors are `InvalidFormatFault` (400, awsQueryError `InvalidFormat`) and `ResourceNotFound` (404, awsQueryError `ResourceNotFound`). A successful call is HTTP 200 with an empty body. Per AWS's documented behavior, temporarily setting an alarm's state via SetAlarmState EXECUTES the actions configured for the state being set — a forced transition into ALARM on an alarm created with `AlarmActions=[<topic arn>]` publishes to that topic exactly as a real evaluation-driven transition would. Recorded behavior for the notification body: the SNS `Message` is a JSON document carrying `AlarmName` (the alarm from the request), `NewStateValue` = `ALARM`, and `NewStateReason` = the `StateReason` string supplied on the SetAlarmState call; `DisableAlarmActions` then withholds delivery (and `DescribeAlarms` reports `ActionsEnabled: false`), and `EnableAlarmActions` restores it. The alarm name, topic/queue names and reason strings are values from the request and state, not part of the requirement.

Emulator: nothing is ever delivered. The check exhausts its whole budget — `Error: Check timed out after 60.0s` — in its step 0 positive control, which is the FIRST wait in the check (a 90 s window for exactly one notification) and runs BEFORE `ActionsEnabled` is touched at all. So the forced ALARM transition on a freshly created `PutMetricAlarm` alarm with `AlarmActions=[topic]`, an `sqs` subscription on that topic and a queue policy allowing `sns.amazonaws.com` to `sqs:SendMessage` produces no message on the queue inside ~60 s, where AWS delivers one. Because the positive control never completes, the suppressed leg (`DisableAlarmActions` → no delivery) and the restoration leg (`EnableAlarmActions` → exactly one delivery whose `NewStateReason` is the restored transition's) are never reached either, and no verdict on `ActionsEnabled` suppression exists.

Why: alarm→SNS→(SQS/Lambda/email) is the primary way customers learn an alarm fired. A client that drives an alarm with SetAlarmState — the standard way to test an alerting pipeline end to end, and what runbooks and CI smoke tests do — sees the alarm state change but never the notification, and concludes its topic policy, subscription or action wiring is broken. Conversely a customer validating that `DisableAlarmActions` suppresses paging gets a vacuous pass: nothing is delivered either way, so the suppression control is indistinguishable from the defect.

Fix: the `SetAlarmState` handler in the cloudwatch crate and the alarm action-dispatch path it must invoke (the same dispatcher a real evaluation-driven transition uses), gated on the alarm's `ActionsEnabled`. Related open issues that do NOT cover this one: `20261114T170523Z-cloudwatch-sns-a-cfn-deployed-alarm-s-sns.md` (a CFN-deployed alarm's AlarmAction notification never reaching SQS, filed under cloudwatch__sns), `20261116T060651Z-cloudwatch-sns-a-composite-alarm-whose-child-is.md` (composite alarms) and `20261116T055646Z-cloudwatch-sns-sqs-a-setalarmstate-forced-metric-alarm.md` (a forced state not being reclaimed by a later evaluation, filed under cloudwatch__sns__sqs). None of them is marked as owning this check, and none covers a plain metric alarm's forced-transition action dispatch under the `cloudwatch` service.

Traces: cloudwatch.alarm-actions-disabled-suppresses-state-triggered-action.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout.

[mismatch] cloudwatch.alarm-actions-disabled-suppresses-state-triggered-action
  Error: Check timed out after 60.0s

Expected: after CreateTopic + CreateQueue + SetQueueAttributes(Policy allowing sns.amazonaws.com sqs:SendMessage) + Subscribe(protocol=sqs), and PutMetricAlarm(..., AlarmActions=[topicArn], EvaluationPeriods=1, Period=60, TreatMissingData=missing), a SetAlarmState(StateValue=ALARM, StateReason=<reason>) delivers exactly one SNS message to the queue whose decoded `Message` has AlarmName=<the alarm>, NewStateValue='ALARM' and NewStateReason=<that same reason>. Then DisableAlarmActions + a fresh forced ALARM delivers nothing over a >=3x-latency window with ApproximateNumberOfMessages == 0, and EnableAlarmActions + a fresh forced ALARM delivers exactly that one restored notification.

Actual: no message at all. The check cannot get past its first wait (the pre-ActionsEnabled positive control) and is killed at the 60 s cap, so SetAlarmState's action dispatch never fired and the suppression/restoration contrast produced no observation.

## Steps to Reproduce
1. Build and boot the emulator from this checkout.
2. sns CreateTopic; sqs CreateQueue; sqs GetQueueAttributes(QueueArn); sqs SetQueueAttributes with a Policy allowing Principal {"Service":"sns.amazonaws.com"} to sqs:SendMessage on the queue ARN; sns Subscribe(TopicArn, Protocol=sqs, Endpoint=queueArn).
3. cloudwatch PutMetricAlarm with Namespace=<unique>, MetricName=Errors, Statistic=Average, Period=60, EvaluationPeriods=1, Threshold=1.0, ComparisonOperator=GreaterThanThreshold, TreatMissingData=missing, AlarmActions=[topicArn].
4. cloudwatch SetAlarmState(AlarmName=<alarm>, StateValue=ALARM, StateReason='baseline').
5. Poll sqs ReceiveMessage on the queue: AWS delivers one notification; the emulator delivers none.
6. Then DisableAlarmActions / EnableAlarmActions around further forced transitions to see the suppression contrast — or replay the check named in trace_ids.
