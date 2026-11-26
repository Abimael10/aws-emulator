Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic
Issue-Key: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance|_drain#3|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10,
Issue-Key: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic|_drain#3|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10,
Issue-Commit: e1287cf409e72f27d41b5362b1ef9b2f278addc7

# autoscaling__ec2__sns__sqs: Auto Scaling notification configurations produce no observable notification — both PutNotificationConfiguration delivery checks reach no verdict (time out at 60s)

> _Note: this issue was identified against the code base at commit `e1287cf409e72f27d41b5362b1ef9b2f278addc7`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutNotificationConfiguration

## Description
Cause 1 of 1: the Auto Scaling notification path — PutNotificationConfiguration(TopicARN) -> SNS topic -> subscribed SQS queue — never yields an observable notification message, so both behavioral checks that grade Auto Scaling notification delivery hang and are cut off by the suite's 60s per-check budget without reaching any assertion verdict.
Failing checks: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance, autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic

AWS: recorded behavior only for the delivery itself (the Auto Scaling Smithy model describes the configuration surface, not the message). AWS behavior the two checks encode: after PutNotificationConfiguration(AutoScalingGroupName, TopicARN, NotificationTypes), (a) DescribeNotificationConfigurations lists exactly the configured (TopicARN, NotificationType) pairs; (b) configuring autoscaling:TEST_NOTIFICATION causes Auto Scaling to publish one TEST_NOTIFICATION message to that topic alone, whose JSON body carries AutoScalingGroupName equal to the configured group and AutoScalingGroupARN equal to that group's ARN, and a second, unconfigured live topic->queue pipe receives nothing; (c) a 0 -> 1 -> 0 SetDesiredCapacity cycle publishes autoscaling:EC2_INSTANCE_LAUNCH then autoscaling:EC2_INSTANCE_TERMINATE messages, each carrying EC2InstanceId equal to the instance the group actually moved, AutoScalingGroupName equal to the group, and an ActivityId that appears in DescribeScalingActivities for that group; and the terminated instance really reaches shutting-down/terminated in EC2. The variable parts of each message body (group name, group ARN, instance id, activity id) come from the request and from emulator state, never from a recorded run.

Emulator: neither check produces any graded assertion — both end as `Check timed out after 60.0s`, i.e. the flow they drive never advances far enough to assert. Nothing in the emulator's response stream shows a TEST_NOTIFICATION, an EC2_INSTANCE_LAUNCH or an EC2_INSTANCE_TERMINATE message arriving on the subscribed queue. Hypothesis: Auto Scaling stores the notification configuration but never publishes lifecycle or test notifications to the configured SNS topic, so each check blocks in its message-await loop until the budget expires. The suite output carries only the timeout, so the exact blocking step is not pinned by the replay; a fixer should re-run the two checks named below to see which await stalls.

Why: a client that wires ASG lifecycle events to SNS/SQS (the standard pattern for draining, CMDB updates, and lifecycle bookkeeping) sees a configuration that reads back correctly from DescribeNotificationConfigurations yet delivers nothing. Downstream consumers wait forever; a scale-out/scale-in is invisible to every subscriber, and a negative test ("the unconfigured topic must receive nothing") cannot be distinguished from a pipeline that is simply dead.

Fix: the Auto Scaling notification publication path — the emitter that PutNotificationConfiguration arms, invoked from the scaling-activity state machine (launch/terminate transitions) and from the configuration write itself for autoscaling:TEST_NOTIFICATION — plus its SNS publish call. DescribeNotificationConfigurations' read path appears to be a separate concern and is not implicated by the timeout alone.

Traces: the two failing check ids listed above are the only examples; both are grounded solely in a 60s timeout, with no response-level diff available from this replay.

## Evidence
autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance
  Error: Check timed out after 60.0s
  The check arms both notification types, SetDesiredCapacity 0 -> 1, polls for one InService instance, then awaits an autoscaling:EC2_INSTANCE_LAUNCH message on the subscribed queue and asserts EC2InstanceId == the launched instance id and AutoScalingGroupName == the group. No assertion was reached.

autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic
  Error: Check timed out after 60.0s
  The check proves two topic->queue pipes live with a canary publish first, configures autoscaling:TEST_NOTIFICATION on one topic, asserts DescribeNotificationConfigurations lists exactly that pair, then awaits the TEST_NOTIFICATION on the lit queue and asserts AutoScalingGroupName == group and AutoScalingGroupARN == the group's ARN, while the dark queue stays empty. No assertion was reached.

Expected: each await returns a message within its window carrying the group/instance identifiers above.
Actual:   the check makes no further progress and is cut off at the 60s budget; no notification is observed.

No open issue carries either check id: a search of the open issues for Auto Scaling SNS/SQS notification delivery (list_existing_issues query 'notification' and 'autoscaling-ec2-sns') returns nothing for this service grouping — the existing eventing issues are EC2/EventBridge, S3, CloudWatch-alarm and SES/SNS paths, not Auto Scaling notification configurations.

## Steps to Reproduce
1. Create an SNS topic and an SQS queue, subscribe the queue to the topic, and confirm the pipe end to end with a plain SNS Publish (both checks do this first).
2. Create a VPC/subnet, a launch template on an AL2023 AMI, and an Auto Scaling group with MinSize=0, MaxSize=1, DesiredCapacity=0.
3. Test-notification cause: PutNotificationConfiguration(AutoScalingGroupName=<group>, TopicARN=<topic>, NotificationTypes=["autoscaling:TEST_NOTIFICATION"]). DescribeNotificationConfigurations lists the pair. Then poll the subscribed queue — no TEST_NOTIFICATION message naming AutoScalingGroupName=<group> and AutoScalingGroupARN=<group arn> ever arrives.
4. Lifecycle cause: PutNotificationConfiguration with NotificationTypes=["autoscaling:EC2_INSTANCE_LAUNCH","autoscaling:EC2_INSTANCE_TERMINATE"], then SetDesiredCapacity=1, wait for one InService instance, then SetDesiredCapacity=0. Poll the queue — no EC2_INSTANCE_LAUNCH or EC2_INSTANCE_TERMINATE message naming the instance the group moved ever arrives.
5. Both checks block in step 3/4 and are terminated by the suite's 60s budget.

Re-replay: autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-launch-and-terminate-name-the-exact-instance, autoscaling__ec2__sns__sqs.deep-autoscaling-3-asg-notify-test-notification-reaches-only-the-configured-topic
