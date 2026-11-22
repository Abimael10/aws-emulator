Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ecs__elbv2.autoscaling-event-rule-mismatched-pattern-no-delivery
Issue-Key: autoscaling__ecs__elbv2.autoscaling-event-rule-mismatched-pattern-no-delivery|_poll_for_message#26|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: a54ade7ee60c9fa02ca1ca5cbef236b2fbd7f50e

# autoscaling__ecs__elbv2: the ASG launch-event delivery differential never reaches a verdict — neither the scale-out nor the 'EC2 Instance Launch Successful' delivery is observable before the check's cap

> _Note: this issue was identified against the code base at commit `a54ade7ee60c9fa02ca1ca5cbef236b2fbd7f50e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetDesiredCapacity

## Description
Cause 1 of 1: an Auto Scaling scale-out and the EventBridge publication it must produce make no observable progress, so the mismatched-pattern negative-delivery differential is killed with no verdict (failing check: autoscaling__ecs__elbv2.autoscaling-event-rule-mismatched-pattern-no-delivery).

AWS: SetDesiredCapacity on a group backed by a launch template launches a member instance, and DescribeAutoScalingGroups reports it under AutoScalingGroups[].Instances within seconds; scaling back to DesiredCapacity 0 terminates it and empties that list. Smithy: autoscaling SetDesiredCapacity takes AutoScalingGroupName + DesiredCapacity + HonorCooldown and returns an empty output — the model states nothing about eventing, so for the event itself this is recorded/documented behavior only: Auto Scaling publishes an event to the default bus with source 'aws.autoscaling', detail-type 'EC2 Instance Launch Successful' and detail.AutoScalingGroupName set to the group that scaled, and a rule whose EventPattern matches that detail-type delivers it to its SQS target (a rule whose pattern names only 'EC2 Instance Terminate Successful' must deliver nothing for the same launch). The check's structure is: (a) wrong pattern → nothing may arrive, (b) PutRule the corrected pattern on the SAME rule, (c) scale out again → the launch event must arrive with detail-type 'EC2 Instance Launch Successful'.

Emulator: the check is terminated by the harness ('Check timed out after 60.0s') with no leg completing, so neither the launch convergence nor any delivery is observable. The negative leg cannot be distinguished from a bus that publishes nothing and the positive control never runs — the scenario reaches no verdict at all.

Why: a client that scales a group and waits on DescribeAutoScalingGroups.Instances, or that routes 'EC2 Instance Launch Successful' through an EventBridge rule to a queue, waits forever: scale-out looks like it never happens and launch-driven automation (instance registration, draining, notification fan-out) never fires. Negative-delivery tests written against AWS also become meaningless, because silence is indistinguishable from a bus that never publishes.

Fix: the Auto Scaling group reconciliation that materialises/terminates group members on a DesiredCapacity change, and the Auto Scaling → EventBridge emitter for 'EC2 Instance Launch Successful'/'EC2 Instance Terminate Successful' (source aws.autoscaling, detail.AutoScalingGroupName), so a matching rule's SQS target receives the envelope and a non-matching rule receives nothing. Related but distinct open issues: 20261118T173313Z-autoscaling-ec2-events-sqs-no-ec2-instance-launch.md (same event family under a different service token) and 20261117T155926Z-autoscaling-ecs-elbv2-the-eventbridge-negative-delivery-differential.md (ECS Task State Change, not the ASG launch event); neither carries this check's replay key.

Traces: autoscaling__ecs__elbv2.autoscaling-event-rule-mismatched-pattern-no-delivery.

## Evidence
Expected (AWS): after SetDesiredCapacity(DesiredCapacity=1, HonorCooldown=false), DescribeAutoScalingGroups reports >=1 entry in Instances; with a rule whose EventPattern is {source:[aws.autoscaling], detail-type:[EC2 Instance Terminate Successful], detail:{AutoScalingGroupName:[<group>]}} nothing is delivered to the SQS target for that launch; after PutRule replaces detail-type with 'EC2 Instance Launch Successful' and the group scales 0→1 again, the queue receives an envelope whose detail-type is exactly 'EC2 Instance Launch Successful' and whose detail.AutoScalingGroupName is the group name.

Actual (emulator): 'Error: Check timed out after 60.0s' — the suite reports the check as a mismatch with no assertion reached. No launch convergence and no delivered or withheld event could be observed, so the differential produced no verdict.

## Steps to Reproduce
1. ssm get-parameter /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 and create a launch template with that ImageId, t3.micro.
2. autoscaling create-auto-scaling-group with that launch template, MinSize=0, MaxSize=2, DesiredCapacity=0 in a subnet of the shared VPC.
3. sqs create-queue, read its QueueArn, and set a queue policy allowing events.amazonaws.com sqs:SendMessage from the rule ARN.
4. events put-rule with EventPattern {"source":["aws.autoscaling"],"detail-type":["EC2 Instance Terminate Successful"],"detail":{"AutoScalingGroupName":["<asg>"]}}, State=ENABLED, then events put-targets with the queue ARN.
5. autoscaling set-desired-capacity --desired-capacity 1 --no-honor-cooldown; poll describe-auto-scaling-groups until Instances is non-empty (AWS: seconds).
6. Poll receive-message: nothing may arrive (the pattern's detail-type does not match).
7. events put-rule on the SAME rule name with detail-type 'EC2 Instance Launch Successful'; set-desired-capacity 0, wait for Instances to empty, drain the queue, then set-desired-capacity 1 again.
8. Poll receive-message: an envelope with detail-type 'EC2 Instance Launch Successful' and detail.AutoScalingGroupName=<asg> must arrive. Observe instead that nothing in steps 5-8 becomes observable and the scenario is cut off with no verdict.
