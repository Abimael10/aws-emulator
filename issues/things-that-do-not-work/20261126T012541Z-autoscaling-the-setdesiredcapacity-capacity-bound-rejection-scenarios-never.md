Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.set-desired-capacity-above-max-size-rejected-and-no-instance-is-launched autoscaling.set-desired-capacity-below-min-rejected
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.set-desired-capacity-above-max-size-rejected-and-no-instance-is-launched|_holds_for#13|Timeout|time.sleep(interval_s)
Issue-Key: autoscaling.set-desired-capacity-below-min-rejected|_holds_for#20|Timeout|time.sleep(interval_s)
Issue-Commit: bf205090aef11dede75ce528b7b57e0b8feca92b

# autoscaling: the SetDesiredCapacity capacity-bound rejection scenarios never reach a verdict — the group's capacity change is not observable inside a client's budget, so the above-MaxSize / below-MinSize ValidationError refusals and their no-effect guarantee are never exercised

> _Note: this issue was identified against the code base at commit `bf205090aef11dede75ce528b7b57e0b8feca92b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
SetDesiredCapacity

## Description
1 net-new root cause on the Auto Scaling capacity-bound path.

Cause 1 of 1: the two checks that exercise SetDesiredCapacity's capacity-bound refusals make no observable progress — both are cut off with `Check timed out after 60.0s`, with no HTTP status, error code or message from the emulator to diagnose. Each check must first stand up a group that holds its requested capacity as InService instances (one check creates min=0/max=2/desired=1 and polls for 1 InService; the other creates min=1/max=3/desired=1 and polls for 1 InService), then issue the out-of-bounds call and hold the observation to prove no launch or termination followed. Neither check ever reaches the refusal.
Failing check ids for this cause: autoscaling.set-desired-capacity-above-max-size-rejected-and-no-instance-is-launched, autoscaling.set-desired-capacity-below-min-rejected.

AWS: a group created with a DesiredCapacity inside its bounds reaches that many InService instances, and DescribeAutoScalingGroups reports them, promptly enough for a client to then observe a capacity call's effect. On top of that, three refusals are required, each HTTP 400 with error code ValidationError (the awsQuery common error; Smithy autoscaling#SetDesiredCapacity models only ScalingActivityInProgressFault -> 400 ScalingActivityInProgress and ResourceContentionFault -> 500 ResourceContention, and autoscaling#UpdateAutoScalingGroup adds only ServiceLinkedRoleFailure, so the bound violation is not a modeled error shape but the common ValidationError):
  - SetDesiredCapacity with DesiredCapacity above MaxSize: message `New SetDesiredCapacity value <requested> is above max value <MaxSize> for the AutoScalingGroup.` — the sentence is fixed text; <requested> is the request's DesiredCapacity and <MaxSize> is the group's stored MaxSize.
  - SetDesiredCapacity with DesiredCapacity below MinSize: message `New SetDesiredCapacity value <requested> is below min value <MinSize> for the AutoScalingGroup.` — same split: fixed sentence, request value and stored MinSize interpolated.
  - UpdateAutoScalingGroup with DesiredCapacity below MinSize: message `Desired capacity:<requested> must be between the specified min size:<MinSize> and max size:<MaxSize>` — fixed text with the request's desired capacity and the group's stored bounds.
In every case the refusal is a no-op: DesiredCapacity, MinSize/MaxSize and the group's instance set are unchanged, and no instance is launched or terminated. An in-bounds call on the same group does launch and the new instance reaches InService at the group and `running` at EC2. The exact message wording is recorded behavior; the status/code classification is the protocol's common ValidationError.

Emulator: no response at all is obtained for either scenario — the replay reports only `Error: Check timed out after 60.0s`, so the ValidationError code, the message text and the no-effect guarantee are all unobservable, as is the positive control (an in-bounds SetDesiredCapacity launching a second instance).

Hypothesis (not stated by the model or the recording): the group's capacity reconciliation does not surface instances as InService quickly enough for the scenario to proceed, i.e. instance transitions are paced on a fixed wall clock, so the setup phase consumes the whole client budget. This is the same shape as open issue 20261120T045442Z-autoscaling-lifecycle-hook-scenarios-never-converge-inside-a.md ("every instance transition is withheld for a fixed wall-clock span"), which does not carry either of these checks' replay keys, so the bound-check path is still uncovered.

Why: a client that guards its capacity changes against the group's own Min/Max bounds gets neither the refusal it codes against nor a group that converges — autoscaling-driven capacity management hangs, and a request above MaxSize or below MinSize cannot be distinguished from an accepted one.

Fix: the Auto Scaling group capacity reconciliation/state that drives instances to InService (so a group converges to its DesiredCapacity within a client's budget), plus the bound validation in the SetDesiredCapacity and UpdateAutoScalingGroup handlers that must emit the three ValidationError refusals above without mutating group state.

Traces: autoscaling.set-desired-capacity-above-max-size-rejected-and-no-instance-is-launched, autoscaling.set-desired-capacity-below-min-rejected.

## Evidence
Expected (AWS):
1. Group min=0/max=2/desired=1 reaches 1 InService instance; SetDesiredCapacity(DesiredCapacity=5, HonorCooldown=false) -> HTTP 400 ValidationError, message "New SetDesiredCapacity value 5 is above max value 2 for the AutoScalingGroup." (values filled from the request and the group's MaxSize); DesiredCapacity stays 1 and the instance set is unchanged; SetDesiredCapacity(2) then launches a second instance that becomes InService and `running` at EC2.
2. Group min=1/max=3/desired=1 holding its one InService instance; SetDesiredCapacity(0) -> HTTP 400 ValidationError, "New SetDesiredCapacity value 0 is below min value 1 for the AutoScalingGroup."; UpdateAutoScalingGroup(DesiredCapacity=0) -> HTTP 400 ValidationError, "Desired capacity:0 must be between the specified min size:1 and max size:3"; MinSize and DesiredCapacity unchanged, the original instance still InService; SetDesiredCapacity(2) then launches a second InService instance.

Actual (emulator): both scenarios produce no response to compare at all —
  autoscaling.set-desired-capacity-above-max-size-rejected-and-no-instance-is-launched: Error: Check timed out after 60.0s
  autoscaling.set-desired-capacity-below-min-rejected: Error: Check timed out after 60.0s
No HTTP status, no error code, no message, and no instance-state observation is ever reached. (Instance ids and group names in any single replay are per-run values, not requirements.)

## Steps to Reproduce
1. CreateLaunchTemplate with an Amazon Linux 2023 ImageId and InstanceType t3.micro.
2. CreateAutoScalingGroup with that launch template, MinSize=0, MaxSize=2, DesiredCapacity=1, HealthCheckGracePeriod=0, VPCZoneIdentifier of two available subnets; poll DescribeAutoScalingGroups until one instance has LifecycleState InService — observe how long this takes, or that it does not happen inside a client budget.
3. Call SetDesiredCapacity(AutoScalingGroupName=<group>, DesiredCapacity=5, HonorCooldown=false): expect HTTP 400 ValidationError "New SetDesiredCapacity value 5 is above max value 2 for the AutoScalingGroup." and an unchanged DesiredCapacity/instance set.
4. Create a second group with MinSize=1, MaxSize=3, DesiredCapacity=1; once it holds 1 InService instance, call SetDesiredCapacity(DesiredCapacity=0) and expect ValidationError "New SetDesiredCapacity value 0 is below min value 1 for the AutoScalingGroup.", then UpdateAutoScalingGroup(DesiredCapacity=0) and expect ValidationError "Desired capacity:0 must be between the specified min size:1 and max size:3"; neither call may change MinSize or DesiredCapacity or terminate the instance.
5. On each group, an in-bounds SetDesiredCapacity(2) must launch one additional instance that reaches InService and `running` at EC2.
6. Re-replay autoscaling.set-desired-capacity-above-max-size-rejected-and-no-instance-is-launched and autoscaling.set-desired-capacity-below-min-rejected to confirm the fix.
