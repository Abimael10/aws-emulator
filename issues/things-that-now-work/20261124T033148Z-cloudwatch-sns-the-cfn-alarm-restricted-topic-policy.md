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

## Resolution

* Cause 1 — "the two-stack alarm-notification permission scenario never reaches a verdict" — is
  DECLINED: every leg the report asks about is served, and the report's own hypothesis (that the
  CloudFormation deploys do not converge quickly enough) does not hold. Timed on the emulator a
  client STARTS (the built binary, authentication on, the consumer long-polling), with the two
  templates this report describes: the control stack reaches `CREATE_COMPLETE` 0.73 s after
  `CreateStack` and the restricted stack 0.68 s after its own; `SetAlarmState` answers in 12 ms and
  60 ms; the control queue holds the notification 3.5 ms after the force — its `Body` is the SNS
  envelope and `json.loads(Body)["Message"]` carries `AlarmName` = the control alarm and
  `NewStateValue` = `ALARM` — and the restricted queue holds nothing. The whole scenario up to the
  absence window takes 1.49 s. What is left of the check's 60 s cap is the check's own absence
  window, which it holds on the restricted queue for at least as long as the control took and in
  practice for tens of seconds: that window is wall-clock time no emulator change can shorten,
  which is why the cut lands in `_poll_for_notification`, the helper BOTH of the check's polls go
  through. The same reason is already recorded for this trace id in
  `issues/things-that-now-work/20261114T170523Z-cloudwatch-sns-a-cfn-deployed-alarm-s-sns.md`,
  `issues/things-that-now-work/20261114T083920Z-cloudwatch-sns-a-cfn-deployed-aws-sns-topicpolicy.md`
  and `issues/things-that-now-work/20261029T182452Z-cloudwatch-sns-alarm-actions-cfn-alarms-are-never.md`.
* What this change adds is the one axis of that chain no test pinned: WHICH transitions a deployed
  estate owes its topic, and that a withheld publish is not retried by a later evaluation.
  `core/router/tests/all/cloudwatch_cfn_alarm_action_lists_fire_once_per_transition.rs` deploys four
  stacks — a topic, the queue subscribed to it, the queue policy that lets SNS write to it and an
  alarm whose action lists the row decides — and observes three transitions of each deployed alarm:
  the first evaluation out of `INSUFFICIENT_DATA`, the force into `ALARM`, and the evaluation that
  takes the forced state back. Its rows span which list names the topic (`AlarmActions` only,
  `OKActions` only, both) and whether the policy the same stack declares grants the publish, and each
  row's queue must hold exactly the transitions that list is owed and nothing more once the alarm is
  back in the state its own evaluation decides.
* No handler changed, because each already does what this report asks for: the dispatch and the
  topic-policy reading
  (`services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/state/alarm_actions.rs`),
  the subscribed-queue hand-over
  (`services/service-com-amazonaws-sns-amazonsimplenotificationservice/src/ops/publish/service_delivery.rs`)
  and the `AWS::SNS::Topic` / `AWS::SNS::TopicPolicy` / `AWS::SNS::Subscription` / `AWS::SQS::Queue` /
  `AWS::SQS::QueuePolicy` / `AWS::CloudWatch::Alarm` providers
  (`services/service-com-amazonaws-cloudformation-cloudformation/src/state/provisioner/application_services.rs`,
  `.../provisioner/cloudwatch_alarms.rs`).
