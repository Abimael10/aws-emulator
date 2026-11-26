Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery
Issue-Key: cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery|_poll_for_notification#21|Timeout|envelope = _next_notification(sqs, queue_url, wait_s=wait_s)
Issue-Commit: 7b9a9b4a10e559c27831638ce2012afc8db14bea

# cloudwatch__sns: the CFN-deployed alarm -> SNS -> SQS negative-delivery scenario reaches no verdict — SetAlarmState('ALARM') on the control stack's alarm never produces a notification (1 cause)

> _Note: this issue was identified against the code base at commit `7b9a9b4a10e559c27831638ce2012afc8db14bea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetAlarmState

## Description
Cause 1 of 1: a CloudFormation-deployed AWS::CloudWatch::Alarm driven to ALARM by SetAlarmState does not deliver its AlarmActions notification through the subscribed SNS topic to SQS, so the scenario that grades the SNS topic-policy permission boundary produces no observation at all and the check is still running when its deadline arrives.

AWS: Smithy (com.amazonaws.cloudwatch) SetAlarmState — "Temporarily sets the state of an alarm for testing purposes. When the updated state differs from the previous value, the action configured for the appropriate state is invoked. For example, if your alarm is configured to send an Amazon SNS message when an alarm is triggered, temporarily changing the alarm state to ALARM sends an SNS message." Its only modeled errors are InvalidFormatFault (400) and ResourceNotFound (404); the call returns 200 with an empty body. So on AWS the control stack (default topic policy) delivers, within seconds, one SQS message whose Body is the SNS envelope and whose envelope Message is the alarm JSON carrying AlarmName = the alarm's name and NewStateValue = 'ALARM'; the twin stack, whose AWS::SNS::TopicPolicy grants only SNS:Subscribe/Receive/GetTopicAttributes and omits Publish, delivers nothing — CloudWatch's publish to that topic is refused by the topic policy.

Emulator: SetAlarmState is accepted (200) for both alarms, but no notification is ever observed on either queue: the positive control's 90 s poll for an SQS message never returns one and the check ends with 'Check timed out after 60.0s' — no step of the alarm -> SNS -> SQS chain settles, so neither the required delivery nor the required absence can be graded. Because the control never delivers, the emulator also cannot be shown to honour (or ignore) the restrictive topic policy, which is the permission boundary the scenario exists to test.

Why: a client that wires an alarm's AlarmActions to an SNS topic with an SQS subscriber — the standard CloudWatch alerting path — receives nothing, and a client that relies on a topic policy omitting Publish to block alerting gets no evidence either way. Alerting appears configured and is silently inert, and an overly-permissive publish would be indistinguishable from a working one.

Fix: the CloudWatch alarm state-transition/action-invocation path (SetAlarmState must invoke the state's configured actions and publish to each AlarmActions topic ARN) and the SNS publish path's topic-policy authorization for the cloudwatch principal, plus the CFN AWS::CloudWatch::Alarm / AWS::SNS::TopicPolicy handlers that must make the deployed alarm and policy effective. Related open issues cover adjacent symptoms but not this check: 20261125T151059Z-cloudwatch-the-disablealarmactions-suppression-scenario-reaches-no-verdict.md (SetAlarmState alarm -> SNS -> SQS never observed) and 20261125T205149Z-cloudwatch-sns-sqs-a-metric-alarm-on-a.md (a breaching series never reaches ALARM or invokes AlarmActions).

Traces: the single failing check below.

## Evidence
Expected (AWS): after SetAlarmState(AlarmName=<control alarm>, StateValue='ALARM', StateReason=...), ReceiveMessage on the control stack's queue returns an SNS envelope whose Message parses to JSON with AlarmName = <control alarm> and NewStateValue = 'ALARM', within seconds; the twin queue whose topic policy omits Publish stays empty for the whole absence window.

Actual (emulator): neither queue ever yields a message. The replay reports, under ### cloudwatch__sns, '[mismatch] cfn-alarm-missing-sns-topic-policy-no-delivery — Error: Check timed out after 60.0s', i.e. the positive control assertion ("positive control: expected the notification to be delivered") is never reached and the negative assertion ("expected NO delivery when the SNS topic's access policy omits Publish permission") is never graded.

## Steps to Reproduce
1. CreateStack with a template containing AWS::SQS::Queue, AWS::SNS::Topic, AWS::SNS::Subscription (Protocol 'sqs', Endpoint = queue ARN), an AWS::SQS::QueuePolicy allowing sqs:SendMessage from sns.amazonaws.com, and an AWS::CloudWatch::Alarm whose AlarmActions = [{Ref: Topic}]; wait for CREATE_COMPLETE. Deploy a second identical stack that additionally declares an AWS::SNS::TopicPolicy granting only SNS:Subscribe/SNS:Receive/SNS:GetTopicAttributes (no Publish).
2. cloudwatch.SetAlarmState(AlarmName=<restricted stack's alarm>, StateValue='ALARM', StateReason='negative: no publish permission').
3. cloudwatch.SetAlarmState(AlarmName=<control stack's alarm>, StateValue='ALARM', StateReason='positive control').
4. sqs.ReceiveMessage(QueueUrl=<control stack queue>, WaitTimeSeconds=5) repeatedly: expect one SNS envelope whose Message has NewStateValue='ALARM'. Observed: none, on either queue, until the check's deadline.
