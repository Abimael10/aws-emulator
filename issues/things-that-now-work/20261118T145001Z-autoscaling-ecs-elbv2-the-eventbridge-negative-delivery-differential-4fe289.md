Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ecs__elbv2.autoscaling-event-rule-mismatched-pattern-no-delivery
Issue-Key: autoscaling__ecs__elbv2.autoscaling-event-rule-mismatched-pattern-no-delivery|_poll_for_message#26|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 3b95f364a9766d54572805b5303a28b612a84a5b

# autoscaling__ecs__elbv2: the EventBridge negative-delivery differential for ASG launch events never completes — the check exhausts its budget

> _Note: this issue was identified against the code base at commit `3b95f364a9766d54572805b5303a28b612a84a5b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetDesiredCapacity

## Description
Cause 1 of 1: the ASG-launch/EventBridge-pattern differential never reaches a verdict against the emulator — the replay ends with 'Check timed out after 60.0s' with no state ever observed, where the recording completed.
AWS: SetDesiredCapacity(AutoScalingGroupName, DesiredCapacity=1, HonorCooldown=false) makes the group launch an instance, which DescribeAutoScalingGroups then reports under AutoScalingGroups[0].Instances, and AWS publishes an event with source 'aws.autoscaling' and detail-type 'EC2 Instance Launch Successful' carrying detail.AutoScalingGroupName. An EventBridge rule whose pattern names detail-type 'EC2 Instance Terminate Successful' must NOT receive that launch event (no delivery to the SQS target); re-PutRule'ing the same rule with detail-type 'EC2 Instance Launch Successful' must then deliver the next launch. Recorded behavior only for the end-to-end timing; the per-API shapes are the standard autoscaling/events/sqs ones.
Emulator: the check makes no progress at all inside its budget — neither the negative control (no delivery for the mismatched pattern) nor the positive control (delivery after correcting the pattern) is reached, so the emulator never advances past the first awaited state (an ASG instance appearing, and/or SQS receive-message long polls returning).
Why: a client cannot rely on either half of EventBridge pattern matching for Auto Scaling lifecycle events: it can neither confirm that a wrong detail-type is filtered out nor that a corrected rule delivers, and an automation that waits on an ASG launch event hangs instead of failing.
Fix: whichever of the awaited components does not advance — the Auto Scaling group's instance-launch simulation behind SetDesiredCapacity/DescribeAutoScalingGroups, and the EventBridge publication of aws.autoscaling 'EC2 Instance Launch Successful' events to an SQS target. Hypothesis: the ASG never materialises an instance, so nothing downstream can ever be emitted.
Traces: autoscaling__ecs__elbv2.autoscaling-event-rule-mismatched-pattern-no-delivery.

## Evidence
Expected: SetDesiredCapacity(1) is followed by DescribeAutoScalingGroups reporting at least one entry in Instances, then no SQS message for the mismatched-pattern rule, then — after PutRule with detail-type 'EC2 Instance Launch Successful' — a delivered envelope whose detail-type == 'EC2 Instance Launch Successful' and whose detail.AutoScalingGroupName is the group's name.
Actual: Error: Check timed out after 60.0s — the replay produced no observation of a launched instance or any delivery/non-delivery verdict.

## Steps to Reproduce
CreateLaunchTemplate; CreateAutoScalingGroup(Min=0,Max=2,Desired=0) in a default-VPC subnet; CreateQueue and SetQueueAttributes with an events.amazonaws.com SendMessage policy; PutRule with pattern {source:[aws.autoscaling], detail-type:[EC2 Instance Terminate Successful], detail:{AutoScalingGroupName:[<asg>]}} and PutTargets to the queue; SetDesiredCapacity(1, HonorCooldown=false); poll DescribeAutoScalingGroups for Instances and ReceiveMessage for a delivery; then PutRule the same rule with detail-type 'EC2 Instance Launch Successful', scale to 0 and back to 1 and poll again.
