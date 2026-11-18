Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__events__lambda.xsvc-autoscaling-3-launch-hook-lambda-error-times-out-to-abandon
Issue-Key: autoscaling__events__lambda.xsvc-autoscaling-3-launch-hook-lambda-error-times-out-to-abandon|run#22|AssertionError|assert (d.get("Origin"), d.get("Destination")) == ("EC2", "AutoScalingGroup"), (
Issue-Commit: bdfe4d3535def78e1e4eb1982c0c71176ab283a9

# autoscaling__events__lambda: the 'EC2 Instance-launch Lifecycle Action' event detail omits Origin and Destination, so a plain scale-out launch cannot be told from a warm-pool one

> _Note: this issue was identified against the code base at commit `bdfe4d3535def78e1e4eb1982c0c71176ab283a9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutLifecycleHook

## Description
Cause 1 of 1: the EventBridge event Auto Scaling publishes when a launch lifecycle hook parks an instance ('EC2 Instance-launch Lifecycle Action', source aws.autoscaling) carries a detail object with no Origin and no Destination member, so a consumer cannot tell which lifecycle transition path the instance is on. Failing check id: autoscaling__events__lambda.xsvc-autoscaling-3-launch-hook-lambda-error-times-out-to-abandon.

AWS: recorded behavior only — the Smithy model for EC2 Auto Scaling (services/service-com-amazonaws-autoscaling-autoscaling-*/smithy-model/*.json) models the control-plane API (PutLifecycleHook, LifecycleHook.HeartbeatTimeout / DefaultResult, CompleteLifecycleAction) and does not model the EventBridge event payload at all, so the requirement is AWS's documented lifecycle-hook event schema: the detail of 'EC2 Instance-launch Lifecycle Action' carries, besides LifecycleActionToken, AutoScalingGroupName, LifecycleHookName, EC2InstanceId, LifecycleTransition and NotificationMetadata, the two members Origin and Destination naming where the instance is coming from and going to. 'Origin' and 'Destination' are fixed member names; their values come from the group's state for that transition — for an instance launched by a plain scale-out (no warm pool) they are Origin='EC2' and Destination='AutoScalingGroup' (a warm-pool path instead reports values such as 'WarmPool'). The same two members are required on the launch-outcome events ('EC2 Instance Launch Successful' / 'EC2 Instance Launch Unsuccessful') the same scenario asserts later.

Emulator: the Lambda target wired to that detail-type does receive exactly one lifecycle-action event for the launching instance, but its detail is {'LifecycleActionToken', 'AutoScalingGroupName', 'LifecycleHookName', 'EC2InstanceId', 'LifecycleTransition': 'autoscaling:EC2_INSTANCE_LAUNCHING', 'NotificationMetadata'} — both Origin and Destination are absent (d.get('Origin'), d.get('Destination') are None, None). The check fails on that first assertion, so the downstream behaviour of the same scenario (hook timing out to DefaultResult=ABANDON, termination before InService, 'EC2 Instance Launch Unsuccessful' with StatusCode='Cancelled' and an abandonment StatusMessage, and the directly-completed control instance reaching InService) is not exercised by this run and its fidelity is unknown.

Why: a client that routes or branches on the lifecycle-action event gets the wrong answer. An EventBridge rule whose pattern filters detail.Origin / detail.Destination (the documented way to handle warm-pool launches differently from plain scale-out, and to ignore WarmPool->AutoScalingGroup transitions) never matches, so its target is never invoked; a hook handler that reads event['detail']['Origin'] raises a KeyError on a payload AWS always populates.

Why this is not covered by an existing issue: the open issue 20261111T132217Z-autoscaling-events-lambda-a-launch-lifecycle-hook-is.md describes the earlier state in which no lifecycle-action event was published at all (the launch was announced only 'EC2 Instance Launch Successful'). The event is now published and delivered; the remaining divergence is the incomplete detail payload, which that issue does not name.

Fix: the Auto Scaling lifecycle-hook event emitter — the code in the autoscaling service's group/launch reconciliation that builds the 'EC2 Instance-launch Lifecycle Action' detail (and the launch Successful/Unsuccessful details) before putting it on the event bus — must populate Origin and Destination from the transition it is reconciling instead of omitting them.

Traces: autoscaling__events__lambda.xsvc-autoscaling-3-launch-hook-lambda-error-times-out-to-abandon.

## Evidence
Expected (AWS): the single 'EC2 Instance-launch Lifecycle Action' event delivered for the launching instance has detail.Origin == 'EC2' and detail.Destination == 'AutoScalingGroup' for a plain scale-out launch (fixed member names; the values follow the transition the group is performing), alongside LifecycleActionToken, AutoScalingGroupName, LifecycleHookName, EC2InstanceId, LifecycleTransition='autoscaling:EC2_INSTANCE_LAUNCHING' and the hook's NotificationMetadata.
Actual (emulator): detail = {'LifecycleActionToken': '<token>', 'AutoScalingGroupName': '<group>', 'LifecycleHookName': 'sim2real-launch-hook', 'EC2InstanceId': '<instance-id>', 'LifecycleTransition': 'autoscaling:EC2_INSTANCE_LAUNCHING', 'NotificationMetadata': '<the hook metadata>'} — no Origin, no Destination. Replay detail: AssertionError: a plain scale-out launch is Origin=EC2 Destination=AutoScalingGroup: {...} (assertion on (d.get("Origin"), d.get("Destination")) == ("EC2", "AutoScalingGroup")). The token, group name and instance id in that payload are per-run values; the divergence is the two missing members.

## Steps to Reproduce
1. Create a VPC/subnet, a launch template, and an Auto Scaling group G (MinSize 0, MaxSize 1, DesiredCapacity 0) with autoscaling:PutLifecycleHook(LifecycleHookName=H, LifecycleTransition='autoscaling:EC2_INSTANCE_LAUNCHING', HeartbeatTimeout=30, DefaultResult='ABANDON', NotificationMetadata=<json>).
2. Create a Lambda that logs the whole event it receives, and an EventBridge rule matching {"source": ["aws.autoscaling"], "detail-type": ["EC2 Instance-launch Lifecycle Action"]} targeting it.
3. autoscaling:SetDesiredCapacity(G, 1) and wait for the instance to appear in DescribeAutoScalingGroups.
4. Read the Lambda's logged event for that instance id: event['detail'] has no 'Origin' and no 'Destination' key, where AWS reports 'EC2' and 'AutoScalingGroup'.
5. Re-replay autoscaling__events__lambda.xsvc-autoscaling-3-launch-hook-lambda-error-times-out-to-abandon.
