Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name
Issue-Key: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name|_drain_raw#20|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Commit: 07a12ce4140be4c807d532cfc03706c508ac8640

# autoscaling__ec2__events__sqs: the group-name-filtered ASG launch-event differential never reaches a verdict — no 'EC2 Instance Launch Successful' event is observed and the check is cut at its budget

> _Note: this issue was identified against the code base at commit `07a12ce4140be4c807d532cfc03706c508ac8640`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateAutoScalingGroup

## Description
Cause 1 of 1: the scenario that proves an EventBridge rule filtered on detail.AutoScalingGroupName delivers only the named group's launch event never makes observable progress; the check exhausts its whole budget while polling and is cut off with "Check timed out after 60.0s", so no assertion in it is ever graded.

AWS: recorded behavior only (Auto Scaling's EventBridge events are not in the Smithy model). The recording this check is built on shows that, when a group with DesiredCapacity 1 launches its instance, Auto Scaling publishes exactly one event per launch to the account's default event bus with source `aws.autoscaling` and detail-type `EC2 Instance Launch Successful`, whose detail carries AutoScalingGroupName (the launching group) and EC2InstanceId (the instance the group lists in DescribeAutoScalingGroups.Instances) and whose `resources` array is exactly the group ARN plus arn:aws:ec2:<region>:<account>:instance/<instance id>. A rule whose pattern pins detail.AutoScalingGroupName to one group receives that group's event and never a sibling group's; a rule pinning a name prefix receives both; and the same launch reaches both rules as ONE event (identical `id`).

Emulator: neither queue ever satisfies its collect predicate — the control queue never reports launch events for both groups and the filtered queue never reports one for its group — so the check is still polling when its run budget ends and the suite records "Check timed out after 60.0s" with no step-level diagnosis. Nothing in the replay output names a status, an error code or a message, because no graded call ever refused: the awaited state simply never arrives.

Hypothesis: Auto Scaling publishes no `EC2 Instance Launch Successful` event onto the default event bus at all, so no rule can match it; the open issues 20261118T061315Z-autoscaling-ec2-sns-sqs-auto-scaling-group-notifications.md (ASG notifications never published) and 20261122T172353Z-autoscaling-ecs-elbv2-the-asg-launch-event-delivery.md (the same launch-event differential, under a different service heading, also reaching no verdict) describe the same shape of gap but neither carries this check's replay key, so neither covers it. A second possible contributor, in the same budget, is that the two groups never actually place an instance for an event to describe.

Why: a client that drives anything off scale-out — a lifecycle automation, a registration hook, a per-group routing rule, an audit pipeline — receives nothing, and cannot distinguish "the rule's filter excluded the event" from "the bus published nothing". Pattern precision on detail.AutoScalingGroupName is therefore untestable and silently unimplemented.

Fix: the Auto Scaling group reconciliation/launch path's event emission into the EventBridge default bus (source aws.autoscaling, detail-type 'EC2 Instance Launch Successful', detail.AutoScalingGroupName/EC2InstanceId, resources = group ARN + instance ARN, one event id delivered to every matching rule), plus the EC2 launch it reports on.

Traces: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name (mismatch).

## Evidence
Check: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name
Verdict: mismatch — "Error: Check timed out after 60.0s"

Expected (recorded AWS behavior): after creating two groups (<base>-a, <base>-b) each with DesiredCapacity 1, the prefix-filtered control queue receives launch events for BOTH groups and the queue behind the rule pinned to detail.AutoScalingGroupName == <base>-a receives exactly that group's event; each envelope has source 'aws.autoscaling', detail-type 'EC2 Instance Launch Successful', detail.EC2InstanceId equal to the id the group lists, resources == {group ARN, arn:aws:ec2:<region>:<account>:instance/<instance id>}, and the event reaching both rules carries the same `id`.

Actual (emulator): both _collect polls (control: both group names present; filtered: the named group present) are still unsatisfied when the run budget ends; the check is terminated before any of those assertions executes. No HTTP status, error code or error message is produced by the emulator — the failure is the absence of the events, not a refusal.

## Steps to Reproduce
1. Create a VPC/subnet, resolve the AL2023 AMI from SSM, and create one launch template per group.
2. Create two SQS queues and two EventBridge rules targeting them: rule A pinned to {source: [aws.autoscaling], detail-type: ['EC2 Instance Launch Successful'], detail.AutoScalingGroupName: [<group-a>]}, rule B pinned to the same source/detail-type with detail.AutoScalingGroupName: [{prefix: <base>-}]. Grant events.amazonaws.com SendMessage on both queues and confirm both rules are live with a control event.
3. CreateAutoScalingGroup for <group-a> and <group-b>, each MinSize 0 / MaxSize 1 / DesiredCapacity 1 on the launch template.
4. Poll both queues for the launch events. Observe that neither queue ever receives an 'EC2 Instance Launch Successful' event, so the differential never converges and the scenario ends only when the client's budget does.
