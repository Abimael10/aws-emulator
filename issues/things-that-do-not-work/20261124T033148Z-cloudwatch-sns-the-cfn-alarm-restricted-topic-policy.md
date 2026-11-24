Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery
Issue-Key: cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery|_poll_for_notification#21|Timeout|envelope = _next_notification(sqs, queue_url, wait_s=wait_s)
Issue-Commit: 02e30cec75724d494cfd983590fb8cff440247b8

# cloudwatch__sns: the CFN alarm / restricted-topic-policy delivery scenario reaches no verdict — the check is cut at the 60 s cap before the positive control's notification is observable

> _Note: this issue was identified against the code base at commit `02e30cec75724d494cfd983590fb8cff440247b8`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetAlarmState

## Description
One net-new root cause under the `### cloudwatch__sns` heading (Cause 1 of 1). It is the only failure under that heading and is marked neither ALREADY FILED nor OWNED; no open issue carries its replay key.

Cause 1 of 1 — the two-stack alarm-notification permission scenario never reaches a verdict. Failing check: cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery.
AWS: recorded behavior only (the CloudWatch model does not describe notification delivery). The scenario is: deploy two identical CloudFormation stacks, each with an SNS topic, an SQS queue subscribed to it and an AWS::CloudWatch::Alarm whose AlarmActions name the topic — one with the default topic policy, one whose topic access policy omits Publish. Drive the restricted alarm to ALARM first via SetAlarmState(StateValue='ALARM', StateReason=...), then the control alarm. The control's queue MUST receive the notification envelope whose Message carries AlarmName == the control alarm and NewStateValue == 'ALARM'; the restricted stack's queue MUST receive nothing, held over an absence window at least as long as the control took.
Emulator: the check produced 'Check timed out after 60.0s' — no assertion was reached and no response, status or message was observed, so neither the positive control's delivery nor the denied path's absence was graded. The check's own waits (a 90 s control poll plus a ≥60 s absence window, after two CloudFormation deploys) cannot fit inside the cap, so the suite's per-check budget may be the limiting factor rather than the emulator; it is filed because it is the heading's only surviving failure and the same budget-exhaustion shape has been filed before for other services (20261116T122519Z-codebuild-ecr-ecs-elbv2-logs-both-container-ci.md, 20261120T073735Z-lambda-rds-secretsmanager-the-iam-auth-rds-proxy.md). Hypothesis: the two CloudFormation stack deploys (topic + queue + subscription + alarm each) do not converge quickly enough for the alarm legs to be reached inside the cap.
Why: the alarm→SNS→SQS authorization boundary is ungraded — an emulator that delivers an alarm notification through a topic whose policy omits Publish (or that never delivers one at all) reads as untested, and a client cannot tell from this suite whether its topic-policy hardening has any effect.
Fix: whichever leg consumes the budget — the CloudFormation providers for AWS::SNS::Topic/AWS::SNS::TopicPolicy/AWS::SQS::Queue/AWS::CloudWatch::Alarm so a small stack reaches CREATE_COMPLETE promptly, then the alarm-action publish path's topic-policy authorization; confirm by timing the two deploys alone before touching the delivery code.
Traces: cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator. Service heading `### cloudwatch__sns` 15/16 (94%), 1 failed — this id, not marked ALREADY FILED or OWNED.

[mismatch] cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery
  Error: Check timed out after 60.0s

Nothing from the emulator was observed. The check deploys the control stack (default topic policy) and the restricted stack (topic policy without Publish), calls cloudwatch SetAlarmState(AlarmName=<bad alarm>, StateValue='ALARM', StateReason='negative: no publish permission') then SetAlarmState(AlarmName=<good alarm>, StateValue='ALARM', StateReason='positive control'), polls the control queue for up to 90 s and then holds an absence window of max(60 s, control latency) on the restricted queue.

Expected: the control queue receives an envelope whose json.loads(Message) has AlarmName == the control alarm and NewStateValue == 'ALARM'; the restricted queue receives nothing.
Actual: no verdict; the run was ended by the 60 s per-check cap before any of those steps was reported.

## Steps to Reproduce
1. Replay cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery; it is cut at 60 s with no assertion.
2. Time each leg against the booted emulator: CreateStack for the control template (AWS::SNS::Topic + AWS::SQS::Queue + subscription + AWS::CloudWatch::Alarm with AlarmActions = the topic, default topic policy) and wait for CREATE_COMPLETE; repeat for the restricted template whose AWS::SNS::TopicPolicy omits Publish.
3. cloudwatch SetAlarmState(AlarmName=<restricted alarm>, StateValue='ALARM', StateReason=...), then SetAlarmState(AlarmName=<control alarm>, StateValue='ALARM', StateReason=...).
4. Poll the control stack's QueueUrl: a notification must arrive whose Message has AlarmName == the control alarm and NewStateValue == 'ALARM'.
5. Poll the restricted stack's QueueUrl for at least as long as the control took: nothing may arrive.
6. Re-replay the id to confirm the scenario reaches a verdict inside the suite's cap.
