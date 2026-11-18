Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance
Issue-Key: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic|_drain#3|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10,
Issue-Key: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance|_drain#3|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10,
Issue-Commit: c2a45cb39008eec8053ec65a5a3a807cace171f5

# autoscaling__ec2__sns__sqs: Auto Scaling group notifications are never published to the configured SNS topic — neither TEST_NOTIFICATION nor EC2_INSTANCE_LAUNCH/_TERMINATE ever arrives (2 causes)

> _Note: this issue was identified against the code base at commit `c2a45cb39008eec8053ec65a5a3a807cace171f5`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutNotificationConfiguration

## Description
TWO net-new root causes, grounded in the two failing checks of the suite's `autoscaling__ec2__sns__sqs` service heading. The replay report marks neither ALREADY FILED nor OWNED by an open issue, so both are filed here. Both checks are killed at the harness's 60 s cap with no verdict: they first prove their SNS topic -> SQS queue pipes deliver with a canary publish (that control is not what fails), then block in a receive loop waiting for an Auto Scaling notification that never arrives.

Cause 1 of 2: PutNotificationConfiguration stores the configuration but publishes no autoscaling:TEST_NOTIFICATION to the named topic.
Failing check id: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic
AWS: configuring notifications on a group with PutNotificationConfiguration(AutoScalingGroupName, TopicARN, NotificationTypes=["autoscaling:TEST_NOTIFICATION"]) causes Auto Scaling to publish a test notification to that topic immediately after the configuration is accepted. The published SNS Message is a JSON document whose fields are the group's identity and the event name: Event = "autoscaling:TEST_NOTIFICATION", AutoScalingGroupName = the group named in the request, AutoScalingGroupARN = the ARN DescribeAutoScalingGroups reports for that group, plus the account/region/time fields Auto Scaling stamps on every notification. All of those values are taken from the request and from emulator state; none of them is a fixed literal. The notification goes ONLY to topics that a notification configuration names: an unconfigured topic must receive nothing, and configuring a second topic must then deliver to it too. (The service's Smithy model documents the operation and its NotificationTypes member but states no delivery payload; the citable requirement is the recorded behavior above. The model path for this crate was not resolvable from this checkout, so no member-level citation is quoted here.)
Emulator: the configuration round-trips — DescribeNotificationConfigurations lists exactly the configured (TopicARN, NotificationType) pairs, and the delete steps would remove them — but no message is ever published. The check's `_await_event` loop long-polls the lit queue for 300 s and never sees a body with Event == "autoscaling:TEST_NOTIFICATION", so the check is cut off at the suite's 60 s bound: `Check timed out after 60.0s`. Because the canary publish on the same topic->queue pipe already succeeded, the missing publish is Auto Scaling's, not SNS's or SQS's.
Why: a client that wires Auto Scaling notifications relies on the test notification as the acknowledgement that the pipe (topic policy, subscription, filter) works. Against the emulator that acknowledgement never comes, so the only observable outcome is an indefinite wait: setup code that blocks on the test notification hangs forever, and the whole negative half of the scenario (an unconfigured topic receives nothing) is unverifiable because nothing is delivered either way.
Fix: the PutNotificationConfiguration handler in the autoscaling service crate — after the configuration is stored it must publish the TEST_NOTIFICATION document to the configured topic through SNS; the notification body belongs with the group's notification state/serializer that renders Event, AutoScalingGroupName and AutoScalingGroupARN.

Cause 2 of 2: a scaling transition publishes no autoscaling:EC2_INSTANCE_LAUNCH / autoscaling:EC2_INSTANCE_TERMINATE notification for the instance it moved.
Failing check id: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance
AWS: with NotificationTypes ["autoscaling:EC2_INSTANCE_LAUNCH", "autoscaling:EC2_INSTANCE_TERMINATE"] configured, scaling a group 0 -> 1 publishes one EC2_INSTANCE_LAUNCH notification and scaling 1 -> 0 publishes one EC2_INSTANCE_TERMINATE notification. Each body names Event, the EC2InstanceId of the exact instance the group moved, AutoScalingGroupName and AutoScalingGroupARN of that group, and an ActivityId that is one of the ids DescribeScalingActivities reports for the group — the launch and the terminate carry DIFFERENT ActivityIds, one per scaling activity. Every one of those is a value from the request/state (instance id, group name, group ARN, activity id), not a literal. The terminated instance is really gone in EC2 (DescribeInstances reports shutting-down or terminated). Recorded behavior for the payload; the model states only the operation surface.
Emulator: nothing is published for either transition — the check blocks waiting first for the launch notification and is cut off: `Check timed out after 60.0s`. The wait sits behind the group reaching 1 InService instance, so this cause may be compounded by the group never converging to an InService member (see the related OPEN issue named below, which covers the convergence symptom on OTHER checks and does not carry this check's replay key); regardless, no EC2_INSTANCE_LAUNCH or EC2_INSTANCE_TERMINATE notification is ever delivered to the configured topic.
Why: EC2_INSTANCE_LAUNCH/_TERMINATE notifications are how a consumer learns which instance a group added or removed and which activity did it. A client that drives bootstrap, registration or cleanup off those notifications never fires against the emulator, and a capacity change looks like it produced no lifecycle event at all.
Fix: the autoscaling scaling-transition path (SetDesiredCapacity / the group's capacity reconciliation and its scaling-activity recording) in the autoscaling service crate must emit the configured notification types to SNS as each activity completes, with the moved instance's id and the activity's ActivityId; the group's notification state is the same component as in Cause 1.

Traces: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic, autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance. Related OPEN issues named for context only — they are grounded in other checks and do not cover these two: 20261117T145304Z-autoscaling-cloudformation-sns-the-aws-autoscaling-autoscalinggroup-resource.md (the CFN resource handler never provisions NotificationConfigurations at all), 20261117T145304Z-autoscaling-sns-autoscaling-putnotificationconfiguration-s-10-topic-limitexceeded-message-is.md (the 10-topic LimitExceeded wording), 20261112T022447Z-autoscaling-putnotificationconfiguration-accepts-a-malformed-topicarn-instead-of.md (malformed TopicARN admission), 20261111T093414Z-autoscaling-a-desiredcapacity-0-group-never-reaches-an.md (a DesiredCapacity>0 group never reaches an InService instance).

## Evidence
Suite report, service heading `### autoscaling__ec2__sns__sqs` (0/2 passed, 2 failed):

[mismatch] autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic
  Error: Check timed out after 60.0s
[mismatch] autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance
  Error: Check timed out after 60.0s

Cause 1 — expected (AWS): within seconds of PutNotificationConfiguration(group, TopicARN=lit_topic, NotificationTypes=["autoscaling:TEST_NOTIFICATION"]), the queue subscribed to lit_topic receives one SNS envelope whose TopicArn is lit_topic and whose Message parses to a JSON object with Event == "autoscaling:TEST_NOTIFICATION", AutoScalingGroupName == the group named in the request and AutoScalingGroupARN == that group's ARN from DescribeAutoScalingGroups; the queue subscribed to the unconfigured topic receives nothing. Actual (emulator): DescribeNotificationConfigurations lists exactly [(lit_topic, "autoscaling:TEST_NOTIFICATION")] — the configuration is stored — but the subscribed queue never receives any message, so the check's `_await_event(..., "autoscaling:TEST_NOTIFICATION", timeout_s=300)` long-poll never returns and the check is killed at the 60 s cap. Control: the check first ran `_publish_until_delivered` on BOTH topic->queue pipes (a direct sns:Publish observed on the queue) before anything was asserted, so SNS delivery and the queue policy are proven working; the absent publisher is Auto Scaling.

Cause 2 — expected (AWS): after SetDesiredCapacity(group, 1) the queue receives Event == "autoscaling:EC2_INSTANCE_LAUNCH" with EC2InstanceId == the id the group's Instances list reports InService, AutoScalingGroupName == the group, AutoScalingGroupARN == the group ARN and ActivityId ∈ {DescribeScalingActivities ActivityIds}; after SetDesiredCapacity(group, 0) it receives Event == "autoscaling:EC2_INSTANCE_TERMINATE" naming the SAME EC2InstanceId with a DIFFERENT ActivityId, and EC2 reports that instance shutting-down/terminated. Actual (emulator): no notification of either type is ever delivered; the check never gets past waiting for the launch event and is killed at the 60 s cap with no verdict.

No HTTP status or error code diverges on these checks — every call returns 200; the divergence is the total absence of the published notifications (and, on Cause 2, of the converged group behind them).

## Steps to Reproduce
Cause 1 (TEST_NOTIFICATION):
1. sns:CreateTopic T1 and T2; sqs:CreateQueue Q1 and Q2; subscribe Q1 to T1 and Q2 to T2 with a queue policy allowing sns:SendMessage.
2. Control: sns:Publish a marker to T1 and to T2 and receive it on Q1/Q2 — both pipes deliver.
3. ec2: create a VPC/subnet and a launch template; autoscaling:CreateAutoScalingGroup with MinSize=0, MaxSize=1, DesiredCapacity=0.
4. autoscaling:PutNotificationConfiguration(AutoScalingGroupName=<group>, TopicARN=T1, NotificationTypes=["autoscaling:TEST_NOTIFICATION"]).
5. sqs:ReceiveMessage on Q1 (long poll). EXPECT one envelope from T1 whose Message has Event="autoscaling:TEST_NOTIFICATION", AutoScalingGroupName=<group>, AutoScalingGroupARN=<group ARN>. OBSERVE nothing ever arrives (DescribeNotificationConfigurations does list the configuration).
6. Then PutNotificationConfiguration for T2 as well and poll Q2 — EXPECT a TEST_NOTIFICATION there too; OBSERVE nothing.

Cause 2 (EC2_INSTANCE_LAUNCH / _TERMINATE):
1. Same topic/queue pipe and group as above (MinSize=0, MaxSize=1, DesiredCapacity=0), with PutNotificationConfiguration(NotificationTypes=["autoscaling:EC2_INSTANCE_LAUNCH", "autoscaling:EC2_INSTANCE_TERMINATE"]) on T1.
2. autoscaling:SetDesiredCapacity(<group>, 1) and poll DescribeAutoScalingGroups until one instance is InService; note its InstanceId.
3. sqs:ReceiveMessage on Q1. EXPECT Event="autoscaling:EC2_INSTANCE_LAUNCH" naming that EC2InstanceId, the group name/ARN and an ActivityId listed by DescribeScalingActivities. OBSERVE nothing is delivered.
4. autoscaling:SetDesiredCapacity(<group>, 0) and poll Q1 again. EXPECT Event="autoscaling:EC2_INSTANCE_TERMINATE" naming the same instance with a different ActivityId, and ec2:DescribeInstances reporting it shutting-down/terminated. OBSERVE nothing is delivered.
Replay: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic, autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance
