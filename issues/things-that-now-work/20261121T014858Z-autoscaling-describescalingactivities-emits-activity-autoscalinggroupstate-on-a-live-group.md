Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted
Issue-Step: task#12
Issue-Severity: LOW
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|DescribeScalingActivities|2xx->2xx|$.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[*]
Issue-Commit: a277ee47ed04075090b028f1683b0172a7857865

# DescribeScalingActivities emits Activity.AutoScalingGroupState on a live group, which AWS omits

> _Note: this issue was identified against the code base at commit `a277ee47ed04075090b028f1683b0172a7857865`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeScalingActivities

## Description
One cause.

Cause 1 of 1: DescribeScalingActivities serializes an `AutoScalingGroupState` member on every Activity it returns, hard-set to `InService`. AWS does not emit that member for an activity of a group that is still live when the caller did not ask for deleted groups.

AWS: Smithy (com.amazonaws.autoscaling, 2011-01-01) models `Activity.AutoScalingGroupState` as an OPTIONAL member (target AutoScalingGroupState, @length min 1 max 32) documented as "The state of the Auto Scaling group, which is either InService or Deleted"; `DescribeScalingActivitiesType` has a separate optional `IncludeDeletedGroups` input member, and the only required Activity members are ActivityId, AutoScalingGroupName, Cause, StartTime and StatusCode. The recorded response for a DescribeScalingActivities call on a live Auto Scaling group (no IncludeDeletedGroups opt-in) returns each Activity WITHOUT an AutoScalingGroupState element at all — status 200, no error. Recorded behavior only for the exact emission rule. Hypothesis: AWS populates AutoScalingGroupState only on the deleted-group-aware read path (IncludeDeletedGroups), where it distinguishes `InService` from `Deleted`; a constant `InService` would also make the `Deleted` value unreachable, so a client that uses the member to tell a surviving group from a torn-down one can never observe a deleted group.

Emulator: the replayed step answers 200 with the activity list in the right order and the right Description/StatusCode/Progress, but every Activity carries an extra `AutoScalingGroupState` element with the value `InService` — including the terminate activity of a group that was scaled to zero and is deleted later in the same fixture. The field is constant, not derived from the group's state.

Why: a client that reads DescribeScalingActivities and branches on whether AutoScalingGroupState is present (or on its value) sees a member AWS never sent for this request shape. Strict response validators and diffing/record-replay tooling flag the unmodelled-for-this-call element, and code that treats `AutoScalingGroupState == "Deleted"` as the signal that an activity belongs to a torn-down group never fires, because the emulator only ever reports `InService`.

Fix: the DescribeScalingActivities response serializer / projection in the autoscaling crate (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/ops, with the activity record in src/state.rs): omit AutoScalingGroupState unless the state is actually being reported, and derive its value (`InService` vs `Deleted`) from the group's existence rather than hard-coding `InService`.

Traces: fixture autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted step task#12 ($.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[0]).

Note: this step's diff also shows the recorded `Details` blob naming Availability Zone ID `usw2-az2` for us-west-2a while the emulator says `usw2-az1`. AZ-ID-to-AZ-name mapping is per-account (other fixtures in this same corpus record `usw2-az1` for the same subnet and AZ), so that half of the diff is a per-account value and is deliberately NOT part of this report. No open issue covers the AutoScalingGroupState emission; the nearest open autoscaling issues (20261111T102954Z-autoscaling-scaling-activities-do-not-record-real-launch.md, 20261115T223640Z-autoscaling-auto-scaling-standby-suspend-process-and-group-lifecycle.md) are about launch-failure activity content and group lifecycle/instance projection, not this extra member.

## Evidence
Step: fixture `autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted`, step `task#12`, operation DescribeScalingActivities, verdict mismatch, expected status 200 / actual status 200.

Expected (recorded AWS) $.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[0]:
  { ActivityId: <shaped>, AutoScalingGroupName: <group>, AutoScalingGroupARN: <group arn>, Progress: '100', Cause: <shaped>, Description: 'Terminating EC2 instance: <instance-id>', StartTime: <shaped>, EndTime: <shaped>, Details: '{"Availability Zone ID":...,"Subnet ID":...,"Availability Zone":"us-west-2a"}', StatusCode: 'Successful' }
  -- NO AutoScalingGroupState element.

Actual (emulator) member[0]: the same members PLUS 'AutoScalingGroupState': 'InService'.

Fixed text vs values: the requirement is the ABSENCE of the AutoScalingGroupState element on this request shape (and, where it is emitted, that its value be the group's real state, one of the modelled strings `InService`/`Deleted`). The group name, group ARN, instance id, activity id and the timestamps in the recording are per-run values from the request and emulator state, not requirements. Smithy citation: Activity.AutoScalingGroupState — optional, @length 1..32, doc "The state of the Auto Scaling group, which is either InService or Deleted"; DescribeScalingActivitiesType.IncludeDeletedGroups — optional boolean.

## Steps to Reproduce
1. Create a launch template and an Auto Scaling group backed by it with a desired capacity of 1 in one subnet, and let the instance reach InService.
2. UpdateAutoScalingGroup the group to min 0 / max 0 / desired 0 so the instance is terminated by a scale-in activity.
3. Call DescribeScalingActivities for that group WITHOUT IncludeDeletedGroups, while the group still exists.
4. Inspect the first Activity in DescribeScalingActivitiesResult.Activities: AWS returns no AutoScalingGroupState element; the emulator returns AutoScalingGroupState=InService.

Replay: fixture `autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted`, step `task#12`.
