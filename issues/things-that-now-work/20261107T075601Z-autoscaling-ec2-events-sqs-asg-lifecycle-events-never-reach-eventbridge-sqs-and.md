Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ec2__events__sqs.events-ec2-lifecycle-asg-desired-above-max-rejected-then-launch-event-names-group autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name autoscaling__ec2__events__sqs.xsvc-autoscaling-1-scale-in-terminate-event
Issue-Key: autoscaling__ec2__events__sqs.events-ec2-lifecycle-asg-desired-above-max-rejected-then-launch-event-names-group|run#23|AssertionError|assert message == expected, f"expected message {expected!r}, got {message!r}"
Issue-Key: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name|_drain_raw#20|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Key: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-scale-in-terminate-event|_drain_raw#19|Timeout|msgs = sqs.receive_message(QueueUrl=url, MaxNumberOfMessages=10,
Issue-Commit: 620c90c23a321396910fd66a692ef2fcecf33f7f

# ASG lifecycle events never reach EventBridge/SQS, and the SetDesiredCapacity refusal message carries the group name

> _Note: this issue was identified against the code base at commit `620c90c23a321396910fd66a692ef2fcecf33f7f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetDesiredCapacity

## Description
Two net-new root causes behind the surviving autoscaling__ec2__events__sqs failures.

Cause 1 of 2: no Auto Scaling lifecycle event is ever delivered to an EventBridge rule, so every event-driven assertion times out.
AWS: a group's launch and scale-in emit "EC2 Instance Launch Successful" / "EC2 Instance Terminate Successful" events on the default event bus with source aws.autoscaling and a detail naming the AutoScalingGroupName and the EC2InstanceId; a rule whose pattern filters on that group name delivers them to its SQS target (recorded behavior).
Emulator: nothing is delivered — the checks wait on the queue and end "Check timed out after 60.0s" — so neither the group-name filter nor the scale-in terminate event can be observed.
Why: every event-driven integration built on ASG lifecycle events (the documented way to react to scaling) silently receives nothing, and a negative-delivery assertion cannot be distinguished from a broken bus.
Fix: the autoscaling launch/terminate path's event emission into the EventBridge bus (and the bus-to-SQS delivery it feeds).
Traces: autoscaling__ec2__events__sqs.xsvc-autoscaling-1-filtered-by-group-name, autoscaling__ec2__events__sqs.xsvc-autoscaling-1-scale-in-terminate-event.

Cause 2 of 2: SetDesiredCapacity's above-max ValidationError appends the group name where AWS ends the sentence.
AWS: ValidationError, message exactly "New SetDesiredCapacity value <n> is above max value <max> for the AutoScalingGroup." — fixed text with a terminating period; the only variable parts are the requested value and the group's MaxSize.
Emulator: "New SetDesiredCapacity value 2 is above max value 1 for the AutoScalingGroup: <group name>".
Why: a client that classifies the refusal by its documented sentence (and then asserts the subsequent launch event names the group) misreads the refusal.
Fix: SetDesiredCapacity validation message.
Traces: autoscaling__ec2__events__sqs.events-ec2-lifecycle-asg-desired-above-max-rejected-then-launch-event-names-group.

## Evidence
From the behavioral replay:
- events-ec2-lifecycle-asg-desired-above-max-rejected-then-launch-event-names-group [mismatch]: AssertionError: expected message 'New SetDesiredCapacity value 2 is above max value 1 for the AutoScalingGroup.', got 'New SetDesiredCapacity value 2 is above max value 1 for the AutoScalingGroup: sim2real-evlc-asg-...'.
- xsvc-autoscaling-1-filtered-by-group-name [mismatch]: Error: Check timed out after 60.0s.
- xsvc-autoscaling-1-scale-in-terminate-event [mismatch]: Error: Check timed out after 60.0s.

## Steps to Reproduce
Replay the behavioral suite and re-run the checks in trace_ids: create an ASG, an EventBridge rule filtered on the group name with an SQS target, then scale the group out and in and poll the queue for the launch/terminate events; also call SetDesiredCapacity above MaxSize and compare the ValidationError message.

## Resolution
Cause 2 is fixed: SetDesiredCapacity now refuses a capacity outside the group's own bounds
with one shared sentence that names the requested value and the bound it crossed and ends
at "for the AutoScalingGroup." (the group's name is not part of it), pinned by the
capacity-bound table in
services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/tests/all/group_size_and_health_check_refusal_wording.rs.

Cause 1's behaviour holds in the current tree: an instance a group launches or scales in is
published onto the default event bus (source aws.autoscaling, detail-type "EC2 Instance
Launch Successful"/"EC2 Instance Terminate Successful", the group and the instance in the
detail) and EventBridge delivers it to the SQS target of a rule whose pattern filters on
detail.AutoScalingGroupName. Pinned by the group-filter table in
services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/tests/all/instance_lifecycle_events.rs
(every filter form, both steps, two groups, and a group the account does not hold), and by
the two replayed units that now pass —
events-ec2-lifecycle-asg-desired-above-max-rejected-then-launch-event-names-group and
xsvc-autoscaling-1-scale-in-terminate-event.

Surviving replay key, NOT the behaviour above and not owned by this service any more:
`xsvc-autoscaling-1-filtered-by-group-name|_drain_raw#20|Timeout` still ends "Check timed
out after 60.0s" while its sibling in the same fixture, which asserts exactly the reported
delivery, passes; no form of the group-name filter could be made to fail. The event-path
capabilities that cross-service fixture can still be waiting on belong to other reports:
the EC2 crate publishes no `aws.ec2` "EC2 Instance State-change Notification" at all
(20261110T160612Z-ec2-events-sqs-ec2-instance-state-change-notifications.md), and the
lifecycle-hook event family has no path while CompleteLifecycleAction and
RecordLifecycleActionHeartbeat are scaffold stubs.
