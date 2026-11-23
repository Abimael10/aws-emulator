Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery
Issue-Key: cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery|_poll_for_notification#21|Timeout|envelope = _next_notification(sqs, queue_url, wait_s=wait_s)
Issue-Commit: 225b72df8882575de41dd997e8ca538d39a59320

# cloudwatch__sns: a CFN-deployed alarm's SNS AlarmAction notification never reaches the subscribed SQS queue, so the missing-topic-policy negative-delivery check makes no graded progress and is cut off at the harness cap

> _Note: this issue was identified against the code base at commit `225b72df8882575de41dd997e8ca538d39a59320`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetAlarmState

## Description
Cause 1 of 1: the CloudWatch alarm-action dispatcher does not deliver an ALARM notification to an SNS topic that a CloudFormation stack provisioned together with the alarm, its sqs subscription and its queue policy. The check's POSITIVE CONTROL — the stack that keeps the topic's default (unrestricted) access policy — never receives the notification, so the check cannot even reach the absence assertion it exists to make, and the whole run is cut off at the harness's 60 s cap with no verdict. Failing check id: cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery.

AWS: SetAlarmState is a modelled control-plane write (Smithy, cloudwatch 2010-08-01: SetAlarmState input AlarmName @length 1..255 required, StateValue enum required, StateReason @length 0..1023 required, StateReasonData @length 0..4000; errors InvalidFormatFault -> HTTP 400 awsQueryError code `InvalidFormat`, ResourceNotFound -> HTTP 404 awsQueryError code `ResourceNotFound`). A successful SetAlarmState that moves an alarm into ALARM makes the service execute the alarm's AlarmActions: for an SNS topic ARN this is an SNS Publish performed by the cloudwatch.amazonaws.com service principal (Smithy, sns 2010-03-31: Publish, output PublishResponse.MessageId; it declares AuthorizationErrorException -> HTTP 403, awsQueryError code `AuthorizationError`, for a principal the topic policy does not permit). The topic then fans the notification out to each confirmed subscription; an `sqs` subscription in envelope (non-raw) mode receives an SQS message whose Body is the SNS notification JSON envelope, whose `Message` member is the alarm-notification document carrying `AlarmName` = the alarm's name and `NewStateValue` = "ALARM". The names, ARNs, queue URLs, MessageIds and timestamps involved are all per-run values; the requirement is that one such notification arrives for a topic whose policy permits Publish. Recorded behavior only for the latency: the recording shows the control delivery completing well inside the window the check measures, since the earlier recording of this same check reached its final assertion.

Emulator: nothing arrives. After CREATE_COMPLETE on both stacks and SetAlarmState(StateValue="ALARM") on each alarm, the control queue — subscribed to a topic with the default policy, behind an AWS::SQS::QueuePolicy that allows sns.amazonaws.com sqs:SendMessage — returns no message at all. The check's control poll (up to 90 s, long-polling receive_message with MaxNumberOfMessages=10) never returns an envelope, so the check spins until the suite kills it: the only replay output is `Check timed out after 60.0s`, with no assertion and no exception from the check body. The check is therefore ungraded rather than merely wrong: neither the positive control nor the negative (no-Publish TopicPolicy) half is ever evaluated.

Why: a client that sets an alarm into ALARM and waits for the notification waits forever. Every alerting integration built on alarm -> SNS -> SQS is unobservable against the emulator: a healthy pipeline looks identical to a broken one, both producing silence, so a test cannot distinguish "my topic policy denies Publish" from "my alarm action works" and cannot regression-test either. It also masks the authorization defect that the related open issue describes — with no delivery at all, the absence assertion would now pass vacuously for the wrong reason.

Fix: the CloudWatch alarm-action dispatch path (the component that executes AlarmActions on a state transition and calls SNS Publish), together with the SNS topic fan-out to `sqs` subscriptions and the CloudFormation providers that must register the stack's AWS::SNS::Subscription and AWS::SQS::QueuePolicy into live SNS/SQS state so the dispatcher and the fan-out can see them. The delivery must occur for a topic whose access policy permits Publish; the refusal path (AuthorizationError, HTTP 403) belongs to the authorization step only.

Related open issue, which does NOT carry this failing check's replay key and does not cover it: 20261114T083920Z-cloudwatch-sns-a-cfn-deployed-aws-sns-topicpolicy.md was filed on this same check id when it failed its FINAL assertion (`assert bad_envelope is None`) because the restricted topic delivered too — i.e. the control delivery then worked and only the topic-policy authorization was missing. That symptom is gone: the check now never gets that far, failing one step EARLIER on the control delivery, with a different replay key (a 60 s timeout, not an AssertionError). The control-side absence filed here is a distinct cause from that issue's authorization gap and must be fixed for that issue's assertion to be reachable at all.

Traces: cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery.

## Evidence
Check cloudwatch__sns.cfn-alarm-missing-sns-topic-policy-no-delivery [mismatch]:
  Error: Check timed out after 60.0s
  (no traceback from the check body — the run was killed while polling, so no assertion in the check was ever evaluated)

Expected: after CreateStack of the control template (AWS::SQS::Queue + AWS::SNS::Topic + AWS::SNS::Subscription(Protocol="sqs", Endpoint=Fn::GetAtt[Queue,Arn]) + AWS::SQS::QueuePolicy allowing Principal {Service: sns.amazonaws.com} Action sqs:SendMessage + AWS::CloudWatch::Alarm with AlarmActions=[{"Ref":"Topic"}]) reaches CREATE_COMPLETE, cloudwatch SetAlarmState(AlarmName=<control alarm>, StateValue="ALARM", StateReason="positive control") returns 200 and the queue named by the stack's QueueUrl output receives one SQS message whose Body parses as the SNS notification envelope; json.loads(envelope["Message"]) carries AlarmName == <control alarm> and NewStateValue == "ALARM". (The alarm name, topic ARN, queue URL, MessageId and timestamps are per-run values filling that template; the requirement is that exactly such a notification arrives.)
Actual: the control queue stays empty. receive_message(MaxNumberOfMessages=10, WaitTimeSeconds=5) returns no Messages for the entire poll, the check never leaves _poll_for_notification, and the suite terminates it with `Check timed out after 60.0s`. Consequently the negative half (the second stack, identical except for an AWS::SNS::TopicPolicy whose single statement allows only ["SNS:Subscribe","SNS:Receive","SNS:GetTopicAttributes"] and omits SNS:Publish) is never polled and the divergence it targets is never graded.

## Steps to Reproduce
1. CreateStack a template with AWS::SQS::Queue, AWS::SNS::Topic, AWS::SNS::Subscription (Protocol="sqs", Endpoint={"Fn::GetAtt":["Queue","Arn"]}, TopicArn={"Ref":"Topic"}), AWS::SQS::QueuePolicy allowing {Service: sns.amazonaws.com} sqs:SendMessage on the queue with Condition ArnEquals aws:SourceArn={"Ref":"Topic"}, and AWS::CloudWatch::Alarm (Namespace "sim2real/behavioral", MetricName "check", Statistic Average, Period 60, EvaluationPeriods 1, Threshold 1, ComparisonOperator GreaterThanThreshold, TreatMissingData notBreaching, AlarmActions=[{"Ref":"Topic"}]), with Outputs QueueUrl={"Ref":"Queue"} and TopicArn={"Ref":"Topic"}. Wait for CREATE_COMPLETE. This is the positive control.
2. Call cloudwatch SetAlarmState(AlarmName=<the alarm>, StateValue="ALARM", StateReason="positive control").
3. Long-poll the stack's QueueUrl output with sqs receive_message(MaxNumberOfMessages=10, WaitTimeSeconds=5) for up to 90 s and parse the first message Body as JSON.
Observed: no message ever arrives; the poll never returns an envelope and the check is cut off at the 60 s harness cap.
Expected: one SNS notification envelope arrives whose Message document has AlarmName = the alarm's name and NewStateValue = "ALARM".
4. (Blocked by the above) Deploy the same template plus an AWS::SNS::TopicPolicy on the same topic allowing only ["SNS:Subscribe","SNS:Receive","SNS:GetTopicAttributes"], force its alarm to ALARM first, and assert its queue stays empty for max(60 s, control latency) — the publish must be refused with AuthorizationError (HTTP 403, awsQueryError code `AuthorizationError`).

## Resolution

* Cause 1 — "the control delivery never happens" — is DECLINED: it does happen, and this report's
  reading of where the check stopped is what does not hold. The notification reaches the queue the
  control stack's `QueueUrl` output names, within a second of `SetAlarmState`, on the emulator a
  client STARTS (the built binary, request authentication on, calls signed and the consumer
  long-polling): the message's `Body` is the SNS notification envelope and `json.loads(Body)["Message"]`
  carries `AlarmName` = the deployed alarm and `NewStateValue` = `ALARM`. The replay key's line is in
  `_poll_for_notification`, the helper BOTH of the check's polls go through, so it does not say which
  poll was cut off — and the check's own absence half holds the restricted queue empty for
  `max(60 s, control latency)` on top of deploying two stacks, which exceeds the check's 60 s
  wall-clock budget however fast the delivery is. That is the same reason already recorded for this
  trace id in `issues/things-that-now-work/20261114T083920Z-cloudwatch-sns-a-cfn-deployed-aws-sns-topicpolicy.md`
  and `issues/things-that-now-work/20261029T182452Z-cloudwatch-sns-alarm-actions-cfn-alarms-are-never.md`.
* What this change adds is the one axis of that chain no test pinned: the emulator AS SERVED.
  `core/router/tests/all/cloudwatch_cfn_alarm_notification_on_a_started_emulator.rs` starts the built
  binary, deploys the estate this report describes — a queue, a topic, the `sqs` subscription, an
  `AWS::SQS::QueuePolicy` whose grant to `sns.amazonaws.com` is scoped by `aws:SourceArn`, and the
  alarm whose `AlarmActions` is that topic — forces each alarm into `ALARM` and long-polls each
  subscribed queue. Its rows span whether the template declares the queue's and the topic's names or
  leaves CloudFormation to generate them, and whether the stack declares a topic policy that
  withholds the publish: the two granted rows receive exactly one notification each and record the
  action as `Succeeded`, and the two withholding rows receive nothing and record it as `Failed` with
  the refused `SNS:Publish` and the topic ARN.
* The dispatch, the fan-out and the CloudFormation providers this report names are unchanged,
  because each already does what it asks for: `settle_alarms` /
  `sns_topic_authorization::publish_refusal`
  (`services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/src/state/alarm_actions.rs`),
  the subscribed-queue hand-over
  (`services/service-com-amazonaws-sns-amazonsimplenotificationservice/src/ops/publish/service_delivery.rs`)
  and the `AWS::SNS::Subscription` / `AWS::SQS::QueuePolicy` / `AWS::SNS::TopicPolicy` providers
  (`services/service-com-amazonaws-cloudformation-cloudformation/src/state/provisioner/application_services.rs`).
