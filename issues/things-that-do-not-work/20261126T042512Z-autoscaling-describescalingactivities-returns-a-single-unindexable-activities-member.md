Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted
Issue-Step: task#12
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|DescribeScalingActivities|2xx->2xx|$.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[*]
Issue-Commit: a215090ba01b28fa11c950b182968bfda49903ba

# autoscaling DescribeScalingActivities returns a single, unindexable Activities member for a scale-to-zero group instead of AWS's full activity list

> _Note: this issue was identified against the code base at commit `a215090ba01b28fa11c950b182968bfda49903ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeScalingActivities

## Description
1 net-new cause on autoscaling, found by replaying the simparity-wire-aws-v1 wire corpus. The replay reports this cluster with stub_path empty and owned_by empty, so no filed issue carries its replay key.

Cause 1 of 1 — after a group that launched one instance under a launch template, served a terminating lifecycle hook and was then scaled to zero, DescribeScalingActivities does not expose the group's activity history as the indexable list AWS serves: the emulator answers 200 with exactly ONE Activities.member (the terminate activity), so the position AWS fills at Activities.member[0] does not resolve at all, and the rest of the group's recorded history is absent from the response.

AWS: recorded behavior only — the autoscaling Smithy model is not present in this checkout (the repo is a multi-service root and has no services/service-com-amazonaws-autoscaling-autoscaling/smithy-model/*.json), so this cause is grounded in the recording, not in the model. The recording shows DescribeScalingActivities for a group whose capacity went 1 -> 0 returning a MULTI-ELEMENT Activities list, most-recent-first, whose first element is the termination activity: Description is the fixed text 'Terminating EC2 instance: ' followed by the instance id (variable, from state), StatusCode 'Successful', Progress '100', AutoScalingGroupName the requested group name and AutoScalingGroupARN that group's ARN (both variable, from the request/state), Cause the activity-cause narrative, StartTime/EndTime timestamps, and Details a JSON object whose keys are the fixed 'Availability Zone ID', 'Subnet ID' and 'Availability Zone' with values taken from the instance's placement. The launch activity that preceded it is still listed behind it — AWS does not drop earlier activities from the group's history when the group scales back to zero.

Emulator: HTTP 200 with DescribeScalingActivitiesResult.Activities carrying a single, non-indexable member — the terminate activity (ActivityId 00000002-..., Description 'Terminating EC2 instance: <instance-id>', StatusCode 'Successful', Progress '100'). Because only one member is serialized, the comparison reports $.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[0] as missing: there is no element at that index. The member the emulator does serve matches AWS's terminate activity field-for-field on every asserted member except Details, whose 'Availability Zone ID' differs (see the note below).

Hypothesis: the scale-to-zero path rewrites or truncates the group's activity collection rather than appending the terminate activity to it, so the earlier launch activity is lost and the surviving single activity is serialized as a lone element rather than as a list.

Note on a value deliberately NOT treated as a requirement: within the same asserted member, Details reports 'Availability Zone ID':'usw2-az1' where this recording has 'usw2-az2', both for 'Availability Zone':'us-west-2a' and the same subnet. The AZ-name-to-AZ-id mapping is account-scoped in AWS, and other fixtures in this very corpus record 'usw2-az1' for the same AZ name and subnet, so that literal is a per-account/per-run value and is not part of the requirement. The requirement here is the Details object's fixed key set and that its values come from the instance's actual placement.

Why: a client that reads a group's scaling-activity history to reconstruct what happened — confirming that the launch it requested and the subsequent termination both occurred, correlating the terminate activity's ActivityId with its own capacity change, or walking the list to find the activity that matches an instance id — sees only the single most recent activity and cannot index into the list at all. The group's history reads as if it never launched anything, so audit, diagnosis and any list-walking client logic silently see an incomplete record while the call reports success.

Fix: the autoscaling scale-in / scale-to-zero path's activity state (it must append the termination activity to the group's existing activity collection instead of replacing it) and the DescribeScalingActivities response serializer, which must emit Activities as a list of members so every recorded activity is addressable by position.

Related but NOT covering this cluster: the open issue 20261111T102954Z-autoscaling-scaling-activities-do-not-record-real-launch.md ('autoscaling scaling activities do not record real launch outcomes') covers failed launches reported Successful, missing standby activities, missing Details/StatusMessage and the deleted-group refusal; the replay does not list it as owning this cluster, and none of its causes is about the activity list being truncated to one member on a scale-to-zero group.

Traces: autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted#task#12.

## Evidence
Replay of trace set simparity-wire-aws-v1 against the emulator built from this checkout (emulator_up true, bootstrap_signal false, 90 fixtures replayed, 1066 steps, 66 mismatched, score 93.8%). This cluster: stub_path empty, owned_by empty, affected_count 1.

autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted#task#12 — DescribeScalingActivities, expected status 200, actual status 200.

Diverging path: $.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[0]: missing.

Expected (recorded) at Activities.member[0]:
  AutoScalingGroupName 'sim2real-probe-asg-245e1f96'
  AutoScalingGroupARN  arn:aws:autoscaling:us-west-2:000000000000:autoScalingGroup:<uuid>:autoScalingGroupName/sim2real-probe-asg-245e1f96
  Description          'Terminating EC2 instance: i-02c9c1aa24487de7d'
  StatusCode           'Successful'
  Progress             '100'
  Details              {"Availability Zone ID":"usw2-az2","Subnet ID":"subnet-000000000000000b0","Availability Zone":"us-west-2a"}
  ActivityId/Cause/StartTime/EndTime present but shape-asserted only

Actual: Activities.member is a SINGLE object, not a list, so no element exists at index 0. The object served is
  ActivityId '00000002-0000-4000-8000-000000000002', Description 'Terminating EC2 instance: i-02c9c1aa24487de7d', StatusCode 'Successful', Progress '100', Details {"Availability Zone ID":"usw2-az1","Subnet ID":"subnet-000000000000000b0","Availability Zone":"us-west-2a"}, plus Cause/StartTime/EndTime/AutoScalingGroupARN.

So the status, the error-free verdict and the terminate activity's own fields agree; what diverges is the membership and shape of the Activities collection — AWS serves an indexable multi-element history, the emulator serves one lone member. The only field-level difference inside the served member is Details['Availability Zone ID'] ('usw2-az1' vs 'usw2-az2'), which is an account-scoped AZ id and is explicitly NOT claimed as a requirement here (other fixtures in this corpus record 'usw2-az1' for the same AZ name and subnet).

## Steps to Reproduce
1. Create a launch template, then CreateAutoScalingGroup on it with MinSize 0 / MaxSize 1 / DesiredCapacity 1 in a single subnet, and wait for the instance to reach InService.
2. PutLifecycleHook a terminating hook on the group.
3. UpdateAutoScalingGroup the group to min 0 / max 0 / desired 0 so the instance is terminated and the group scales to zero.
4. Call DescribeScalingActivities for the group (replay step autoscaling-group-launch-template-in-service-terminating-hook-scale-to-zero-and-deleted#task#12).
5. Observe DescribeScalingActivitiesResult.Activities contains a single member (the 'Terminating EC2 instance: <instance-id>' activity) serialized as a lone element, so Activities.member[0] does not resolve and the group's earlier launch activity is absent; AWS returns the activity history as an indexable, most-recent-first list that still includes the launch activity.
