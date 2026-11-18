Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.suspending-replace-unhealthy-withholds-the-replacement-until-it-is-resumed autoscaling.refresh-ignore-preference-spares-the-scale-in-protected-instance autoscaling.lifecycle-hook-full-lifecycle
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.suspending-replace-unhealthy-withholds-the-replacement-until-it-is-resumed|run#21|AssertionError|assert subject_id_during_suspension == [subject_before], (
Issue-Key: autoscaling.refresh-ignore-preference-spares-the-scale-in-protected-instance|run#23|AssertionError|assert protected[subject] in subject_members, (
Issue-Key: autoscaling.lifecycle-hook-full-lifecycle|run#37|AssertionError|assert released_state == "InService", (
Issue-Commit: c9344e2779494e078eb181c7552bbea5ec625bd0

# autoscaling reconciliation state diverges: a suspended ReplaceUnhealthy still replaces, an Ignore refresh destroys the protected instance, and a completed lifecycle action never admits the instance

> _Note: this issue was identified against the code base at commit `c9344e2779494e078eb181c7552bbea5ec625bd0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartInstanceRefresh

## Description
Three net-new causes where the emulator's group reconciliation reaches a different state than AWS. Each must be closed separately; they are bundled only because all three live in one service's instance-state engine.

Cause 1 of 3 — suspending the ReplaceUnhealthy process does not withhold the replacement.
AWS: Smithy: SuspendProcesses/ResumeProcesses take ScalingProcesses (the process names the service documents, including ReplaceUnhealthy); the effect is recorded behavior only — while ReplaceUnhealthy is suspended, a member marked Unhealthy is NOT terminated and replaced, so the group keeps the same instance id; resuming the process then performs the withheld replacement. An otherwise identical group with nothing suspended does replace its instance, which is what makes the suspension observable.
Emulator: the suspended group replaces its instance anyway — after marking the member Unhealthy, the group's membership moves to a new instance id while ReplaceUnhealthy is still listed in SuspendedProcesses.
Why: process suspension is the documented way to freeze an Auto Scaling group during maintenance or an investigation; a client that suspends ReplaceUnhealthy and then inspects or repairs the instance finds it terminated under it, and nothing it can call will hold the group still.
Fix: the health-check/replacement reconciliation path in the autoscaling engine — consult the group's SuspendedProcesses before acting on an unhealthy member, and perform the deferred replacement on ResumeProcesses.
Traces: autoscaling.suspending-replace-unhealthy-withholds-the-replacement-until-it-is-resumed.

Cause 2 of 3 — an instance refresh with ScaleInProtectedInstances='Ignore' replaces the protected instance.
AWS: Smithy: RefreshPreferences.ScaleInProtectedInstances is an enum documented as choosing 'the behavior that you want Amazon EC2 Auto Scaling to use if instances protected from scale in are found'; recorded behavior for 'Ignore' is that the refresh leaves a scale-in-protected member untouched (it survives in the group with its original instance id) and still completes successfully, whereas the same refresh with 'Refresh' replaces it.
Emulator: with ScaleInProtectedInstances='Ignore' the protected instance is gone from the group after the refresh — its id is absent from DescribeAutoScalingGroups' Instances, replaced by freshly launched ids, i.e. the preference is parsed and accepted but not honoured.
Why: scale-in protection plus 'Ignore' is how a caller pins a stateful or quarantined instance across a rolling replacement; honouring the request as 'Refresh' destroys exactly the instance the caller protected, and the loss is silent because the refresh reports success.
Fix: the instance-refresh engine's instance-selection step — exclude members with ProtectedFromScaleIn when the refresh's ScaleInProtectedInstances preference is 'Ignore'.
Traces: autoscaling.refresh-ignore-preference-spares-the-scale-in-protected-instance.

Cause 3 of 3 — after CompleteLifecycleAction(CONTINUE) the instance is parked in 'Pending:Proceed' and never reaches InService.
AWS: Smithy: CompleteLifecycleAction takes LifecycleHookName, AutoScalingGroupName and LifecycleActionResult (required); recorded behavior only for the state machine — a launch hook holds the instance at Pending:Wait, a heartbeat extends that hold, and CompleteLifecycleAction with LifecycleActionResult='CONTINUE' releases it so the instance transitions Pending:Proceed -> InService within the group's normal settling window, where DescribeAutoScalingGroups/DescribeAutoScalingInstances report LifecycleState 'InService'.
Emulator: the instance stops at LifecycleState 'Pending:Proceed' and stays there for the whole observation window — the transition to InService is never performed, so a released instance is never admitted to the group.
Why: every client that completes a lifecycle action and then waits for InService (deployment gates, warm-up logic, load-balancer registration that keys off InService) hangs; the group also never reaches its desired in-service capacity even though the hook was satisfied.
Fix: the lifecycle-state advance in the autoscaling engine — after CompleteLifecycleAction releases a held instance, carry 'Pending:Proceed' through to 'InService'. (Open issue 20261111T132217Z-autoscaling-lifecycle-hooks-are-stored-but-never-executed.md concerns hooks never parking an instance at all and does not carry this check; this check shows the opposite end of the same state machine — the instance IS parked and released but never admitted.)
Traces: autoscaling.lifecycle-hook-full-lifecycle.

The instance ids and group names quoted in the evidence are one run's generated values; the requirement is which instance identity survives and which LifecycleState is reported.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout (service heading '### autoscaling'):
Cause 1 — autoscaling.suspending-replace-unhealthy-withholds-the-replacement-until-it-is-resumed: AssertionError 'the suspended group replaced its instance anyway: [i-00000000000000003], expected only i-00000000000000002' (the control group with nothing suspended did replace, which is the barrier the check uses).
Cause 2 — autoscaling.refresh-ignore-preference-spares-the-scale-in-protected-instance: AssertionError "'Ignore' must spare the scale-in-protected instance i-00000000000000003, but it is gone from <group>: ['i-00000000000000006', 'i-00000000000000008']".
Cause 3 — autoscaling.lifecycle-hook-full-lifecycle: after PutLifecycleHook, a heartbeat and CompleteLifecycleAction(LifecycleActionResult='CONTINUE'), AssertionError 'instance never resolved to InService after CompleteLifecycleAction, last observed state Pending:Proceed'.

## Steps to Reproduce
1. Replay the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout.
2. Cause 1: create two identical groups at desired=1, SuspendProcesses(ScalingProcesses=['ReplaceUnhealthy']) on one, mark each group's instance Unhealthy with SetInstanceHealth, then compare the instance ids in DescribeAutoScalingGroups for both groups; then ResumeProcesses and re-read the suspended group.
3. Cause 2: create a group at desired=2, SetInstanceProtection on one member, StartInstanceRefresh with Preferences.ScaleInProtectedInstances='Ignore', wait for the refresh to reach a terminal status and check whether the protected instance id is still a member (twin with 'Refresh' as the contrast).
4. Cause 3: PutLifecycleHook with LifecycleTransition autoscaling:EC2_INSTANCE_LAUNCHING, scale the group out, RecordLifecycleActionHeartbeat, then CompleteLifecycleAction with LifecycleActionResult='CONTINUE' and poll DescribeAutoScalingInstances for LifecycleState.
