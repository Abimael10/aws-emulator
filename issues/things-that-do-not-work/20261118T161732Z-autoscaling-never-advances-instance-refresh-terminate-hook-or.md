Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.deep-autoscaling-1-asg-recon-refresh-max-healthy-surge autoscaling.deep-autoscaling-1-asg-recon-refresh-min-healthy-floor autoscaling.deep-autoscaling-1-asg-recon-refresh-without-max-healthy-no-surge autoscaling.instance-refresh-rollback-order autoscaling.attach-operations-accepted-during-instance-refresh autoscaling.lifecycle-hook-abandon-terminates-the-instance-instead-of-serving autoscaling.deep-autoscaling-2-asg-enforce-terminate-hook-abandon-retain-keeps-instance-outside-desired autoscaling.execute-policy-respects-cooldown autoscaling.set-desired-capacity-above-max-size-rejected-and-no-instance-is-launched autoscaling.set-desired-capacity-below-min-rejected
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: autoscaling.deep-autoscaling-1-asg-recon-refresh-max-healthy-surge|_poll#7|Timeout|time.sleep(interval_s)
Issue-Key: autoscaling.deep-autoscaling-1-asg-recon-refresh-min-healthy-floor|_poll#9|Timeout|time.sleep(interval_s)
Issue-Key: autoscaling.deep-autoscaling-1-asg-recon-refresh-without-max-healthy-no-surge|_poll#7|Timeout|time.sleep(interval_s)
Issue-Key: autoscaling.instance-refresh-rollback-order|_poll_until#26|Timeout|time.sleep(interval_s)
Issue-Key: autoscaling.attach-operations-accepted-during-instance-refresh|_poll_until#19|Timeout|last = probe()
Issue-Key: autoscaling.lifecycle-hook-abandon-terminates-the-instance-instead-of-serving|_poll#20|Timeout|time.sleep(interval_s)
Issue-Key: autoscaling.deep-autoscaling-2-asg-enforce-terminate-hook-abandon-retain-keeps-instance-outside-desired|_poll_until#15|Timeout|last = probe()
Issue-Key: autoscaling.execute-policy-respects-cooldown|_poll_until#11|Timeout|time.sleep(interval_s)
Issue-Key: autoscaling.set-desired-capacity-above-max-size-rejected-and-no-instance-is-launched|_holds_for#13|Timeout|time.sleep(interval_s)
Issue-Key: autoscaling.set-desired-capacity-below-min-rejected|_holds_for#20|Timeout|time.sleep(interval_s)
Issue-Commit: c9344e2779494e078eb181c7552bbea5ec625bd0

# autoscaling never advances instance refresh, terminate-hook or capacity-intent flows, so 10 behavioral checks exhaust the suite budget with no verdict

> _Note: this issue was identified against the code base at commit `c9344e2779494e078eb181c7552bbea5ec625bd0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartInstanceRefresh

## Description
Three net-new causes, each a reconciliation flow the emulator never advances: every check below polls a state AWS reaches and is cut at the suite's 60 s cap with no verdict, so the behavior behind it is wholly unexercised. An emulator settles these transitions in-process, so a 60 s budget is ample; the checks' own deadlines are 120-900 s. Each cause must be closed separately; they are bundled only because all three are stalled reconciliation loops in one service.

Cause 1 of 3 — an instance refresh never progresses to a terminal status.
AWS: Smithy: StartInstanceRefresh takes Preferences -> RefreshPreferences (MinHealthyPercentage 0..100, MaxHealthyPercentage 100..200, InstanceWarmup, SkipMatching, AutoRollback, ...) and returns InstanceRefreshId; DescribeInstanceRefreshes reports the refresh's Status. Recorded behavior: the refresh walks InProgress -> (RollbackInProgress) -> a terminal Successful/RollbackSuccessful, replacing members while honouring the Min/MaxHealthyPercentage window (surging above desired when MaxHealthyPercentage is set, replacing in place when it is omitted), and attach operations other than AttachInstances/DetachInstances keep succeeding while it runs.
Emulator: none of these flows reaches a terminal status (or the membership/capacity shape the window implies) inside the budget — each check polls DescribeInstanceRefreshes/DescribeAutoScalingGroups until the harness kills it at 60 s, so the refresh engine's progression, its healthy-percentage window, its rollback ordering and its concurrency rules are all unverifiable.
Why: any client that starts a refresh and waits for Successful (deployment pipelines, CloudFormation rolling updates, operators) never gets an answer; the group also never finishes replacing its instances.
Fix: the instance-refresh engine in the autoscaling service (status progression and the Min/MaxHealthyPercentage replacement plan) — advance a started refresh to a terminal status without waiting on wall-clock.
Traces: autoscaling.deep-autoscaling-1-asg-recon-refresh-max-healthy-surge, autoscaling.deep-autoscaling-1-asg-recon-refresh-min-healthy-floor, autoscaling.deep-autoscaling-1-asg-recon-refresh-without-max-healthy-no-surge, autoscaling.instance-refresh-rollback-order, autoscaling.attach-operations-accepted-during-instance-refresh.

Cause 2 of 3 — terminate-hook ABANDON flows never settle.
AWS: recorded behavior only (the model describes PutLifecycleHook's shape, not the state machine): a terminating hook parks the instance at Terminating:Wait; ABANDON then terminates it without it ever reaching InService, and with the group's InstanceLifecyclePolicy.RetentionTriggers.TerminateHookAbandon='retain' the instance is instead parked in Terminating:Retained — running, outside desired capacity and replaced — until TerminateInstanceInAutoScalingGroup clears it through the hook again.
Emulator: neither flow produces an observable transition inside the budget; both checks poll the instance's LifecycleState and are cut at 60 s, so ABANDON's effect and the retain/terminate retention trigger are never exercised.
Why: a client that uses a terminate hook to drain work, or TerminateHookAbandon='retain' to quarantine a failed instance, cannot observe either outcome; drain/quarantine logic hangs.
Fix: the lifecycle-hook execution path and the InstanceLifecyclePolicy.RetentionTriggers handling in the autoscaling engine. (Open issue 20261111T132217Z-autoscaling-lifecycle-hooks-are-stored-but-never-executed.md records that hooks never park an instance; it does not carry these checks, which additionally pin ABANDON's terminate-vs-retain outcome.)
Traces: autoscaling.lifecycle-hook-abandon-terminates-the-instance-instead-of-serving, autoscaling.deep-autoscaling-2-asg-enforce-terminate-hook-abandon-retain-keeps-instance-outside-desired.

Cause 3 of 3 — capacity-intent flows (cooldown-gated ExecutePolicy, SetDesiredCapacity bounds) never materialize their effect.
AWS: Smithy: SetDesiredCapacity and ExecutePolicy(HonorCooldown) are modeled without 400s, so the observable contract is recorded behavior: a second ExecutePolicy(HonorCooldown=true) inside a SimpleScaling policy's cooldown is skipped (so a later HonorCooldown=false call lands DesiredCapacity on 2, not 3), a SetDesiredCapacity above MaxSize is refused and launches nothing while the same call within MaxSize does launch an instance, and a SetDesiredCapacity below MinSize is refused while the group's instance set stays put.
Emulator: each of these checks polls for the launched/withheld instance (or for the capacity to settle) and is cut at the 60 s cap, so the cooldown gate and the SetDesiredCapacity bound behavior — including the positive controls that must succeed — are never observed.
Why: the scaling actions a client drives capacity with cannot be confirmed: a caller that sets desired capacity (or executes a policy) and waits for the instances never converges, and a refusal that must leave the group untouched cannot be distinguished from one that quietly changed it.
Fix: the capacity reconciliation loop in the autoscaling engine (launch/terminate toward DesiredCapacity) and the SimpleScaling cooldown bookkeeping on ExecutePolicy.
Traces: autoscaling.execute-policy-respects-cooldown, autoscaling.set-desired-capacity-above-max-size-rejected-and-no-instance-is-launched, autoscaling.set-desired-capacity-below-min-rejected.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the emulator built from this checkout (service heading '### autoscaling'). All ten checks below report, as their only failure detail, 'Error: Check timed out after 60.0s' — the call phase never returned the state the check polls for, so no assertion was reached:
Cause 1 — autoscaling.deep-autoscaling-1-asg-recon-refresh-max-healthy-surge (DESIRED=2, Preferences MinHealthyPercentage=100/MaxHealthyPercentage=200/InstanceWarmup=0/SkipMatching=false), autoscaling.deep-autoscaling-1-asg-recon-refresh-min-healthy-floor (DESIRED=4, MIN_HEALTHY_PCT=75), autoscaling.deep-autoscaling-1-asg-recon-refresh-without-max-healthy-no-surge (MinHealthyPercentage=100, no MaxHealthyPercentage), autoscaling.instance-refresh-rollback-order (refresh with an explicit DesiredConfiguration on a version-pinned group, expecting InProgress -> RollbackInProgress -> RollbackSuccessful), autoscaling.attach-operations-accepted-during-instance-refresh (AttachLoadBalancers/AttachTrafficSources/AttachLoadBalancerTargetGroups during a refresh on a min=2/max=3/desired=2 group).
Cause 2 — autoscaling.lifecycle-hook-abandon-terminates-the-instance-instead-of-serving, autoscaling.deep-autoscaling-2-asg-enforce-terminate-hook-abandon-retain-keeps-instance-outside-desired (TerminateHookAbandon='retain', expecting Terminating:Retained).
Cause 3 — autoscaling.execute-policy-respects-cooldown (SimpleScaling policy on a min=0/max=5/desired=0 group), autoscaling.set-desired-capacity-above-max-size-rejected-and-no-instance-is-launched (min=0/max=2/desired=1), autoscaling.set-desired-capacity-below-min-rejected.
The emulator's other autoscaling reads answer promptly in the same run (281 checks pass), so the stall is in these reconciliation flows, not in request handling.

## Steps to Reproduce
1. Replay the behavioral suite (simparity-behavioral-aws-v1) against the emulator built from this checkout; the ten checks named above report 'Check timed out after 60.0s'.
2. Cause 1: create a group at desired=2 with a launch template, StartInstanceRefresh with Preferences {MinHealthyPercentage:100, MaxHealthyPercentage:200, InstanceWarmup:0, SkipMatching:false}, then poll DescribeInstanceRefreshes for a terminal Status and DescribeAutoScalingGroups for the surge/replace shape.
3. Cause 2: PutLifecycleHook with LifecycleTransition autoscaling:EC2_INSTANCE_TERMINATING, scale in (and separately set InstanceLifecyclePolicy.RetentionTriggers.TerminateHookAbandon='retain'), CompleteLifecycleAction with LifecycleActionResult='ABANDON', then poll the instance's LifecycleState.
4. Cause 3: PutScalingPolicy (SimpleScaling, Cooldown set) then ExecutePolicy twice with HonorCooldown=true/false and poll DesiredCapacity; and on a min=0/max=2/desired=1 group call SetDesiredCapacity above MaxSize and then within MaxSize, polling the instance set after each.
