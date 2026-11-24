Issue-Source: trace-sweep-divergence
Issue-Trace: autoscaling-asgdx-traj-standby-park-and-return
Issue-Step: task#13
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: autoscaling-asgdx-traj-standby-park-and-return autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals autoscaling-group-dependent-operations-on-nonexistent-group autoscaling-instance-level-operations-negative asglf-activity-arch-mismatch autoscaling-asgdx-traj-suspend-all-processes-and-resume-all autoscaling-asgdx-traj-suspended-launch-leaves-desired-unfilled autoscaling-group-configuration-and-topology-negative autoscaling-deletion-protection-blocks-delete-until-cleared
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling|ExitStandby|2xx->4xx|$.ExitStandbyResponse
Issue-Key: autoscaling|ExitStandby|2xx->4xx|status
Issue-Key: autoscaling|EnterStandby|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|EnterStandby|2xx->2xx|$.EnterStandbyResponse.EnterStandbyResult.Activities.member.Cause
Issue-Key: autoscaling|EnterStandby|2xx->2xx|$.EnterStandbyResponse.EnterStandbyResult.Activities.member.Details
Issue-Key: autoscaling|ExitStandby|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|DescribeScalingActivities|2xx->2xx|$.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[*].Cause
Issue-Key: autoscaling|DescribeScalingActivities|2xx->2xx|$.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[*].Description
Issue-Key: autoscaling|DescribeScalingActivities|2xx->2xx|$.DescribeScalingActivitiesResponse.DescribeScalingActivitiesResult.Activities.member[*].Details
Issue-Key: autoscaling|DescribeAutoScalingGroups|2xx->2xx|$.DescribeAutoScalingGroupsResponse.DescribeAutoScalingGroupsResult.AutoScalingGroups.member.Instances
Issue-Key: autoscaling|DescribeAutoScalingGroups|2xx->2xx|$.DescribeAutoScalingGroupsResponse.DescribeAutoScalingGroupsResult.AutoScalingGroups.member.SuspendedProcesses.member[*]
Issue-Key: autoscaling|SuspendProcesses|2xx->4xx|$.SuspendProcessesResponse
Issue-Key: autoscaling|SuspendProcesses|2xx->4xx|status
Issue-Key: autoscaling|SuspendProcesses|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: autoscaling|DeleteAutoScalingGroup|4xx->2xx|$.ErrorResponse
Issue-Key: autoscaling|DeleteAutoScalingGroup|4xx->2xx|status
Issue-Related: 20261111T102954Z-autoscaling-scaling-activities-do-not-record-real-launch.md
Issue-Commit: c2a51bbc379e00e6c722f42efee8d77cde344066

# Auto Scaling standby, suspend-process and group-lifecycle fidelity diverge (10 causes)

> _Note: this issue was identified against the code base at commit `c2a51bbc379e00e6c722f42efee8d77cde344066`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExitStandby

## Description
Replay of the wire corpus against the emulator's autoscaling surface found 10 NET-NEW root causes (none of them carried an owning open issue or a stub) in the instance-standby state machine, the suspend/resume process plane, scaling-activity recording and group-lifecycle refusals. All ten must be closed before this issue is submitted.

Related but NON-COVERING open issues (they own OTHER clusters of the same replay, not these): 20261111T102954Z-autoscaling-scaling-activities-do-not-record-real-launch.md, 20261110T100847Z-autoscaling-refusal-fidelity-wrong-codes-and-emulator-authored.md, 20261102T114505Z-autoscaling-auto-scaling-group-scoped-operations-refuse-an-existing.md, 20261022T112459Z-autoscaling-group-round-trip-create-skips-min-max.md.

Cause 1 of 10: ExitStandby refuses a legitimate return-from-standby (fixture autoscaling-asgdx-traj-standby-park-and-return, step task#13).
AWS: Smithy: ExitStandby input {AutoScalingGroupName (required), InstanceIds}, output ExitStandbyAnswer{Activities: Activity list}; the only modeled error is ResourceContentionFault (500). For an instance the group holds in Standby the call succeeds 200 and returns one Activity whose Description is "Moving EC2 instance out of Standby: <instance-id>", StatusCode PreInService, Progress 30 and Details the placement JSON. Recorded response: 200 with that ExitStandbyResponse envelope.
Emulator: answers 400 and no ExitStandbyResponse at all.
Why: a client that parks an instance and then returns it to service can never complete the round trip — the instance is stranded and the group never regains capacity.
Fix: the ExitStandby handler plus the lifecycle-state transition it reads (the instance must actually be in Standby for it to accept; see Cause 3).
Traces: autoscaling-asgdx-traj-standby-park-and-return#task#13.

Cause 2 of 10: EnterStandby's min-size refusal carries emulator-authored prose instead of AWS's message (fixture autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals, step task#8).
AWS: Smithy: EnterStandby input requires AutoScalingGroupName and ShouldDecrementDesiredCapacity; the refusal is the awsQuery ValidationError (400, Type Sender). The message is "AutoScalingGroup <group-name> has min-size=<min>, max-size=<max>, and desired-size=<desired>. To place into standby <n> instance, please update the AutoScalingGroup sizes appropriately." Fixed text: everything outside the angle brackets, including the literal "has min-size=", ", max-size=", ", and desired-size=" and the second sentence; variable parts: the group name from the request and min/max/desired plus the instance count from the group's state.
Emulator: same 400/ValidationError but the message is "The desired capacity of the Auto Scaling group after the instances are placed into Standby (0) is below the minimum capacity of the group (1). Either set ShouldDecrementDesiredCapacity flag to false or lower group's min size." — wording the service never emits, and it never names the group.
Why: operators and tooling that match on AWS's message (or scrape the reported min/max/desired) do not recognize the refusal.
Fix: the EnterStandby handler's validation message template.
Traces: autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals#task#8 (cluster also covers autoscaling-asgdx-traj-standby-park-and-return#task#12, autoscaling-group-dependent-operations-on-nonexistent-group#task#9, autoscaling-instance-level-operations-negative#task#4).

Cause 3 of 10: EnterStandby's returned Activity has the wrong Cause and drops Details (fixture autoscaling-asgdx-traj-standby-park-and-return, step task#9).
AWS: Smithy: Activity.Cause is a required XmlStringMaxLen1023 and Activity.Details an XmlString. For a standby move AWS words Cause as "At <timestamp> instance <instance-id> was moved to standby in response to a user request, shrinking the capacity from <old> to <new>." and Details as the placement JSON object with keys "Availability Zone ID", "Subnet ID" and "Availability Zone" taken from where the instance is running. Fixed text: the "At ", " instance ", " was moved to standby in response to a user request, shrinking the capacity from ", " to " literals and the Details key names; variable parts: the timestamp, instance id, the two capacity numbers and the AZ/subnet values from state.
Emulator: Cause is "a user request explicitly moved an instance of the group to Standby: changing the desired capacity from 1 to 0." (its own phrasing, no timestamp, no instance id) and Details is absent entirely.
Fix: the EnterStandby handler's activity-record construction (Cause template + Details placement JSON).
Traces: autoscaling-asgdx-traj-standby-park-and-return#task#9.

Cause 4 of 10: ExitStandby's refusal for an instance that is not in Standby uses emulator prose and leaks an internal state name (fixture autoscaling-asgdx-traj-standby-park-and-return, step task#14).
AWS: Smithy: LifecycleState enum includes both EnteringStandby and Standby; the refusal is ValidationError (400, Type Sender) with message "The instance <instance-id> is not in Standby." Fixed text: "The instance " and " is not in Standby."; the only variable part is the instance id from the request.
Emulator: 400/ValidationError with "Instance i-<id> is in EnteringStandby state, not in Standby state, and cannot be moved." — different wording, and it exposes that the emulator left the instance parked in the transient EnteringStandby state.
Why: a client cannot distinguish "not in standby" from an emulator-internal stuck state, and message matching fails.
Fix: the ExitStandby handler's validation message, and the state transition that should settle EnteringStandby -> Standby.
Traces: autoscaling-asgdx-traj-standby-park-and-return#task#14 (cluster also covers autoscaling-group-dependent-operations-on-nonexistent-group#task#10, autoscaling-instance-level-operations-negative#task#5).

Cause 5 of 10: DescribeScalingActivities does not record the standby round trip (fixture autoscaling-asgdx-traj-standby-park-and-return, step task#16).
AWS: Smithy: DescribeScalingActivities returns ActivitiesType{Activities (required), NextToken}, newest activity first. After a park-and-return AWS holds one activity per transition: the exit with Description "Moving EC2 instance out of Standby: <instance-id>" and the enter with Description "Moving EC2 instance to Standby: <instance-id>", each with the standby Cause wording of Cause 3/its exit counterpart ("...was moved out of standby in response to a user request, increasing the capacity from <old> to <new>.") and the placement Details JSON, plus a NextToken when the page is capped.
Emulator: the newest activity is the enter-standby record (Description "Moving EC2 instance to Standby: ...") — no exit-standby activity exists at all — the second is the original launch ("Launching a new EC2 instance: ..."), every Cause is the emulator's own phrasing, every Details is absent and NextToken is omitted.
Fix: the standby/exit-standby handlers' activity emission and the DescribeScalingActivities projection (Details + pagination token).
Traces: autoscaling-asgdx-traj-standby-park-and-return#task#16.

Cause 6 of 10: a group whose launch failed keeps a phantom instance in its Instances list (fixture asglf-activity-arch-mismatch, step task#12).
AWS: Smithy: AutoScalingGroup.Instances is a list of Instance{LifecycleState: LifecycleState enum, ...}. When every launch attempt fails, AWS reports the group with an EMPTY Instances list — the recorded response carries no Instances member content.
Emulator: reports one Instance member (LifecycleState "Terminating", with InstanceType/LaunchTemplate/ImageId filled in) that never leaves the list.
Why: a client waiting for "no instances" (or counting healthy capacity) after a failed launch never converges, and scale-in logic sees capacity that does not exist.
Fix: the group's instance-collection state — a failed/terminated launch must be reaped out of Instances rather than left in Terminating.
Traces: asglf-activity-arch-mismatch#task#12 (cluster also covers asglf-activity-arch-mismatch#task#14).

Cause 7 of 10: suspending all processes omits RemoveFromLoadBalancerLowPriority (fixture autoscaling-asgdx-traj-suspend-all-processes-and-resume-all, step task#7).
AWS: Smithy: SuspendProcesses input ScalingProcessQuery.ScalingProcesses documents Launch, Terminate, AddToLoadBalancer, AlarmNotification, AZRebalance, HealthCheck, InstanceRefresh, ReplaceUnhealthy, ScheduledActions and states "If you omit this property, all processes are specified"; the live service additionally suspends RemoveFromLoadBalancerLowPriority, which the recording shows as a SuspendedProcess member alongside the others (recorded behavior for that member name). SuspendedProcess carries ProcessName + SuspensionReason.
Emulator: the suspended set stops short — at the index where AWS reports {ProcessName: RemoveFromLoadBalancerLowPriority} the emulator reports ReplaceUnhealthy, i.e. RemoveFromLoadBalancerLowPriority is never suspended or reported.
Fix: the SuspendProcesses handler's process-name table (and the DescribeAutoScalingGroups projection of SuspendedProcesses).
Traces: autoscaling-asgdx-traj-suspend-all-processes-and-resume-all#task#7.

Cause 8 of 10: SuspendProcesses refuses a valid request with 400 (fixture autoscaling-asgdx-traj-suspend-all-processes-and-resume-all, step task#8).
AWS: Smithy: SuspendProcesses output is empty (Unit) — success is 200 with just SuspendProcessesResponse/ResponseMetadata/RequestId; its only modeled client error is ResourceInUseFault (400). The recorded response for this step is 200.
Emulator: answers 400 with no SuspendProcessesResponse, so the suspension never takes effect.
Why: a client suspending the full process set (including the name from Cause 7) gets a hard failure where AWS succeeds, so suspend/resume automation aborts.
Fix: the SuspendProcesses handler's process-name validation (it rejects a name AWS accepts).
Traces: autoscaling-asgdx-traj-suspend-all-processes-and-resume-all#task#8.

Cause 9 of 10: the illegal-process-name refusal message is missing its terminating period (fixture autoscaling-asgdx-traj-suspended-launch-leaves-desired-unfilled, step task#6).
AWS: 400 ValidationError with message "Request contained illegal suspendable process names: [<names>].Valid names are :[AZRebalance, AddToLoadBalancer, AlarmNotification, HealthCheck, InstanceRefresh, Launch, ReplaceUnhealthy, ScheduledActions, Terminate]." Fixed text: the two sentences including the odd ".Valid names are :[" spacing AND the final "." after the closing bracket; variable part: only the offending names echoed from the request.
Emulator: identical status and code but the message ends "...ScheduledActions, Terminate]" — the final period is dropped.
Why: exact-match assertions on the refusal text fail.
Fix: the SuspendProcesses validation message template.
Traces: autoscaling-asgdx-traj-suspended-launch-leaves-desired-unfilled#task#6 (cluster also covers autoscaling-group-configuration-and-topology-negative#task#3, autoscaling-group-dependent-operations-on-nonexistent-group#task#5).

Cause 10 of 10: DeleteAutoScalingGroup on a group that no longer exists answers 200 instead of refusing (fixture autoscaling-deletion-protection-blocks-delete-until-cleared, step task#9).
AWS: Smithy: DeleteAutoScalingGroup{AutoScalingGroupName (required), ForceDelete} with modeled client errors ResourceInUseFault and ScalingActivityInProgressFault; a name that resolves to no group is the awsQuery ValidationError (400, Type Sender) with message "AutoScalingGroup name not found - AutoScalingGroup '<group-name>' not found". Fixed text: "AutoScalingGroup name not found - AutoScalingGroup '" and "' not found"; variable part: the group name from the request.
Emulator: answers 200 with an empty success body, so a delete of an absent group looks like a delete of a live one.
Why: idempotency/cleanup logic that distinguishes "deleted now" from "already gone", and any test asserting the second delete fails, silently gets the wrong answer.
Fix: the DeleteAutoScalingGroup handler's group lookup — resolve the name against state and raise ValidationError when it is absent.
Traces: autoscaling-deletion-protection-blocks-delete-until-cleared#task#9.

## Evidence
Replay summary: trace_set simparity-wire-aws-v1, emulator_up true, 1044 steps, 795 passed, 245 mismatched, score 76.1%.

Cause 1 — autoscaling-asgdx-traj-standby-park-and-return#task#13 ExitStandby: expected 200 with {'ExitStandbyResult': {'Activities': {'member': {... 'Description': 'Moving EC2 instance out of Standby: i-...', 'Progress': '30', 'StatusCode': 'PreInService', 'Details': '{"Availability Zone ID":...,"Subnet ID":...,"Availability Zone":...}'}}}}; actual 400, $.ExitStandbyResponse missing.

Cause 2 — autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals#task#8 EnterStandby (400 vs 400): $.ErrorResponse.Error.Message expected 'AutoScalingGroup <name> has min-size=1, max-size=1, and desired-size=1. To place into standby 1 instance, please update the AutoScalingGroup sizes appropriately.'; got "The desired capacity of the Auto Scaling group after the instances are placed into Standby (0) is below the minimum capacity of the group (1). Either set ShouldDecrementDesiredCapacity flag to false or lower group's min size."

Cause 3 — autoscaling-asgdx-traj-standby-park-and-return#task#9 EnterStandby (200 vs 200): Activities.member.Cause expected 'At <ts> instance <id> was moved to standby in response to a user request, shrinking the capacity from 1 to 0.'; got 'a user request explicitly moved an instance of the group to Standby: changing the desired capacity from 1 to 0.'. Activities.member.Details expected the placement JSON; got None.

Cause 4 — autoscaling-asgdx-traj-standby-park-and-return#task#14 ExitStandby (400 vs 400): Message expected 'The instance <id> is not in Standby.'; got 'Instance <id> is in EnteringStandby state, not in Standby state, and cannot be moved.'

Cause 5 — autoscaling-asgdx-traj-standby-park-and-return#task#16 DescribeScalingActivities (200 vs 200): member[0].Description expected 'Moving EC2 instance out of Standby: <id>'; got 'Moving EC2 instance to Standby: <id>'. member[1].Description expected 'Moving EC2 instance to Standby: <id>'; got 'Launching a new EC2 instance: <id>'. member[0]/[1].Cause carry emulator phrasing; member[0]/[1].Details missing; NextToken missing.

Cause 6 — asglf-activity-arch-mismatch#task#12 DescribeAutoScalingGroups (200 vs 200): AutoScalingGroups.member.Instances expected empty; got member {'InstanceId': 'i-...', 'LifecycleState': 'Terminating', ...}.

Cause 7 — autoscaling-asgdx-traj-suspend-all-processes-and-resume-all#task#7 DescribeAutoScalingGroups (200 vs 200): SuspendedProcesses.member[6] expected {'ProcessName': 'RemoveFromLoadBalancerLowPriority', 'SuspensionReason': <shaped>}; got {'ProcessName': 'ReplaceUnhealthy', ...}.

Cause 8 — autoscaling-asgdx-traj-suspend-all-processes-and-resume-all#task#8 SuspendProcesses: expected 200 with SuspendProcessesResponse/ResponseMetadata; got 400 with no SuspendProcessesResponse.

Cause 9 — autoscaling-asgdx-traj-suspended-launch-leaves-desired-unfilled#task#6 SuspendProcesses (400 vs 400): Message expected '...Launch, ReplaceUnhealthy, ScheduledActions, Terminate].'; got '...ScheduledActions, Terminate]' (no trailing '.').

Cause 10 — autoscaling-deletion-protection-blocks-delete-until-cleared#task#9 DeleteAutoScalingGroup: expected 400 ErrorResponse {Error:{Type:'Sender', Code:'ValidationError', Message:"AutoScalingGroup name not found - AutoScalingGroup '<name>' not found"}}; got 200 and no ErrorResponse.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire fixture corpus (trace set simparity-wire-aws-v1).
3. Standby plane: replay autoscaling-asgdx-traj-standby-park-and-return — step task#9 (EnterStandby activity Cause/Details), task#13 (ExitStandby should be 200), task#14 (ExitStandby refusal message), task#16 (DescribeScalingActivities standby records); and autoscaling-asgdx-traj-remove-instance-min-size-and-in-use-refusals step task#8 (EnterStandby min-size refusal message).
4. Suspend plane: replay autoscaling-asgdx-traj-suspend-all-processes-and-resume-all steps task#7 and task#8, and autoscaling-asgdx-traj-suspended-launch-leaves-desired-unfilled step task#6.
5. Instance collection: replay asglf-activity-arch-mismatch step task#12 (and task#14).
6. Group lifecycle: replay autoscaling-deletion-protection-blocks-delete-until-cleared step task#9 (DeleteAutoScalingGroup on an absent group).
7. Compare each response's status, error code and message against the recorded fixture response.
