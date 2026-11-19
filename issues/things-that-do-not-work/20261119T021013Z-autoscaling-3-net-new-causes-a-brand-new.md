Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling.execute-policy-respects-cooldown autoscaling.zonal-balance-takes-precedence-over-the-termination-policy autoscaling.warm-pool-instance-reuse-returns-the-scaled-in-instance-to-the-pool
Issue-Shape-Id: com.amazonaws.autoscaling#AutoScaling_2011_01_01
Issue-Key: auto-scaling|ExecutePolicy|ScalingActivityInProgress
Issue-Key: autoscaling.zonal-balance-takes-precedence-over-the-termination-policy|run#25|AssertionError|assert sorted(survivors) == sorted([oldest, fat_newer]), (
Issue-Key: autoscaling.warm-pool-instance-reuse-returns-the-scaled-in-instance-to-the-pool|_poll#18|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 1dc698be9e80e0112562bfdd5ab001c28f556c88

# autoscaling: 3 net-new causes — a brand-new group is already in cooldown so the first ExecutePolicy is refused, scale-in ignores Availability-Zone balance, and a warm pool never reaches MinSize

> _Note: this issue was identified against the code base at commit `1dc698be9e80e0112562bfdd5ab001c28f556c88`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecutePolicy

## Description
Three independent net-new root causes in the Auto Scaling emulator, all surfaced by behavioral checks the suite lists under `### autoscaling`. None is marked ALREADY FILED or OWNED by an open issue. A fixer must close all three before submitting.

Cause 1 of 3: ExecutePolicy(HonorCooldown=true) is refused with ScalingActivityInProgress on a group that has never performed a scaling activity.
Failing check: autoscaling.execute-policy-respects-cooldown.
AWS: ExecutePolicy (operation ExecutePolicy, input ExecutePolicyType) models exactly two errors — ScalingActivityInProgressFault (client, httpError 400, awsQueryError code `ScalingActivityInProgress`, "The operation can't be performed because there are scaling activities in progress.") and ResourceContentionFault (500). Smithy: ExecutePolicyType$HonorCooldown — "Indicates whether Amazon EC2 Auto Scaling waits for the cooldown period to complete before executing the policy. Valid only if the policy type is SimpleScaling." A cooldown window exists only AFTER a scaling activity of the group; a group created at DesiredCapacity 0 has performed none, so recorded behavior shows AWS performing the FIRST ExecutePolicy(HonorCooldown=true) on a just-created group with a just-created SimpleScaling/ChangeInCapacity +1 policy with HTTP 200 and moving DesiredCapacity from 0 to 1. Only a SECOND execution inside the window the first one left behind may be blocked.
Emulator: that first call is refused 400 `ScalingActivityInProgress` with the message "Scaling activity is in progress and blocks the execution of this policy". That message is entirely fixed text (it names no group, policy or cooldown length) and is the emulator's COOLDOWN_IN_PROGRESS_MESSAGE, which is reachable only through the `request.honor_cooldown && policy_type == SimpleScaling && group.in_cooldown(now)` gate — so the group is treated as inside a cooldown before any scaling activity of it has ever run. Hypothesis: the cooldown window is written by every capacity application rather than by a completed scaling activity (`AutoScalingState::apply_capacity` sets `group.cooldown_until = now + cooldown` unconditionally, and group creation applies the requested capacity), so every group spends its first DefaultCooldown (300 s by default) seconds reporting a cooldown it never earned.
Why: a client that executes a SimpleScaling policy with HonorCooldown=true right after creating the group is refused and DesiredCapacity never moves, so no manual policy test, and no alarm-driven simple-scaling path that honours cooldown, can take effect during the first cooldown of a group's life. The client also cannot distinguish "blocked by a real activity" from "blocked by nothing".
Fix: the cooldown bookkeeping this gate reads — the cooldown writer in the group state (services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/state.rs, `apply_capacity` and `StoredGroup::in_cooldown`) and the gate itself in src/state/policy_execution.rs::execute_scaling_policy. A cooldown must begin only once a scaling activity has actually changed the group's capacity, not when a group is created.

Cause 2 of 3: scale-in selection applies TerminationPolicies globally and ignores Availability-Zone balance, so the wrong instance is terminated.
Failing check: autoscaling.zonal-balance-takes-precedence-over-the-termination-policy.
AWS: Smithy CreateAutoScalingGroupType$TerminationPolicies — "A policy or a list of policies that are used to select the instance to terminate. These policies are executed in the order that you list them", referring to "Configure termination policies for Amazon EC2 Auto Scaling". That rule is applied AFTER the zone step: Amazon EC2 Auto Scaling first identifies the Availability Zone holding the most instances (with at least one instance not protected from scale-in) and only then applies the configured termination policy WITHIN that zone. With a group spread 2/1 over two zones and TerminationPolicies=["OldestInstance"], AWS terminates the older of the two instances in the fuller zone and spares the globally-oldest instance that sits alone in the thinner zone.
Emulator: the globally-oldest instance in the thin zone is terminated instead, leaving both fat-zone instances in service. The emulator's own module documentation states the rule it implements: "selection here is ONE ordered pass: the candidates … are ordered by the FIRST policy the group lists" — there is no zone step anywhere in it.
Why: a client loses the wrong instance, and the group ends up zonally imbalanced (one zone emptied) exactly where AWS keeps it balanced. Repeated scale-in drains a whole Availability Zone, destroying the failure-domain spread the group was configured for, and any workload pinned to the surviving zone's instance is terminated instead of the redundant one.
Fix: the scale-in victim selection — services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/state/termination_policies.rs (`scale_in_selection`), reached from `AutoScalingState::scale_in_selection` in src/state.rs. Candidates must be narrowed to the Availability Zone(s) with the most eligible instances before the group's TerminationPolicies order them.

Cause 3 of 3: a warm pool never acquires the instances its MinSize asks for, so no instance is ever reported in a Warmed:* lifecycle state.
Failing check: autoscaling.warm-pool-instance-reuse-returns-the-scaled-in-instance-to-the-pool.
AWS: Smithy PutWarmPool input — MinSize (WarmPoolMinSize, range min 0), PoolState (WarmPoolState), InstanceReusePolicy; DescribeWarmPool answers WarmPoolConfiguration plus the pool's Instances. Recorded behavior: after PutWarmPool(AutoScalingGroupName=<group>, MinSize=1, PoolState=Stopped) the group acquires one warm-pool instance, which is reported with a `Warmed:*` LifecycleState (Warmed:Stopped for PoolState=Stopped) alongside the group's InService members, for a plain pool and for one carrying InstanceReusePolicy{ReuseOnScaleIn: true} alike.
Emulator: neither the control group (pool without a reuse policy) nor the subject group (pool with ReuseOnScaleIn) ever reports an instance whose LifecycleState starts with `Warmed:`; the check never gets past its setup wait for "1 warm-pool instance" and the harness ends it with TimeoutError "setup: <group-name> settled with 1 warm-pool instance: the run budget ended first; not graded" (the group name is a per-run value). Nothing after that wait — the scale-in differential between a reusing and a non-reusing pool — is ever exercised.
Why: a client cannot observe warm-pool pre-provisioning at all: PutWarmPool is accepted but the pool stays empty, so warm-pool scale-out latency, PoolState and instance reuse are all unobservable, and any workflow that waits for a warm instance before scaling hangs.
Related open issues that do NOT cover this check (the suite marks it neither ALREADY FILED nor OWNED): 20261113T064920Z-autoscaling-warm-pool-operations-diverge-phantom-warmed-instances.md and 20261113T130916Z-autoscaling-2-net-new-causes-a-scale-out.md.
Fix: warm-pool convergence toward the pool's target size — services/service-com-amazonaws-autoscaling-autoscaling-2011-01-01/src/state/warm_pools.rs, as driven by `AutoScalingState::materialize_due` in src/state.rs, and the PutWarmPool handler in src/ops/put_warm_pool.rs that records the pool.

## Evidence
Cause 1 — autoscaling.execute-policy-respects-cooldown, [error]:
  expected: ExecutePolicy(AutoScalingGroupName=<group>, PolicyName=<simple policy, ChangeInCapacity +1, Cooldown=300>, HonorCooldown=True) on a group created moments earlier at MinSize=0/MaxSize=5/DesiredCapacity=0 → HTTP 200, DesiredCapacity becomes 1.
  actual: botocore.errorfactory.ScalingActivityInProgressFault: An error occurred (ScalingActivityInProgress) when calling the ExecutePolicy operation: Scaling activity is in progress and blocks the execution of this policy
  The whole message is fixed text; it is the emulator's cooldown-blocked sentence, which only the HonorCooldown=true branch can produce (src/state/policy_execution.rs: COOLDOWN_IN_PROGRESS_MESSAGE, `if request.honor_cooldown && policy_type == POLICY_TYPE_SIMPLE_SCALING && in_cooldown`). No scaling activity of that group had run at that point.

Cause 2 — autoscaling.zonal-balance-takes-precedence-over-the-termination-policy, [mismatch]:
  AssertionError: scale-in chose the wrong instance: survivors ['i-00000000000000002', 'i-00000000000000003'], expected ['i-00000000000000001', 'i-00000000000000003']. Killing i-00000000000000001 means zonal balance was ignored and OldestInstance was applied globally; killing i-00000000000000003 means the zone was chosen but OldestInstance was not applied inside it.
  (The ids are per-run values; the requirement is the shape of the choice: with 2 instances in zone B and 1 older instance in zone A and TerminationPolicies=["OldestInstance"], the terminated instance is the older of the two in zone B. The emulator terminated the globally-oldest instance, i.e. the thin zone's sole member.)

Cause 3 — autoscaling.warm-pool-instance-reuse-returns-the-scaled-in-instance-to-the-pool, [error]:
  TimeoutError: setup: sim2real-behav-as-grp-3e4a92c0c55c settled with 1 warm-pool instance: the run budget ended first; not graded
  Raised from the setup wait that polls DescribeAutoScalingInstances for an instance whose LifecycleState begins with "Warmed:" after PutWarmPool(MinSize=1, PoolState=Stopped) on both groups. No such instance was ever reported for either group (the group name in the message is a per-run value).

## Steps to Reproduce
Cause 1:
1. ec2:CreateLaunchTemplate with a valid ImageId and InstanceType t3.micro.
2. autoscaling:CreateAutoScalingGroup(AutoScalingGroupName=G, LaunchTemplate=that template, MinSize=0, MaxSize=5, DesiredCapacity=0, VPCZoneIdentifier=<one subnet>).
3. autoscaling:PutScalingPolicy(AutoScalingGroupName=G, PolicyName=P, PolicyType=SimpleScaling, AdjustmentType=ChangeInCapacity, ScalingAdjustment=1, Cooldown=300).
4. autoscaling:ExecutePolicy(AutoScalingGroupName=G, PolicyName=P, HonorCooldown=True) — the first execution ever made on G.
   AWS: 200, then DescribeAutoScalingGroups reports DesiredCapacity 1. Emulator: 400 ScalingActivityInProgress "Scaling activity is in progress and blocks the execution of this policy", DesiredCapacity stays 0.

Cause 2:
1. Create a VPC with two subnets in two different Availability Zones (thin = A, fat = B) and a launch template.
2. CreateAutoScalingGroup(MinSize=0, MaxSize=3, DesiredCapacity=1, VPCZoneIdentifier=<subnet A>, TerminationPolicies=["OldestInstance"]); wait for the single InService instance — this is the globally oldest instance and it lives in A.
3. SuspendProcesses(ScalingProcesses=["AZRebalance"]); UpdateAutoScalingGroup(VPCZoneIdentifier=<subnet B>); SetDesiredCapacity 2, wait; SetDesiredCapacity 3, wait — the two newer instances land in B, strictly ordered by LaunchTime.
4. UpdateAutoScalingGroup(VPCZoneIdentifier="<subnet A>,<subnet B>"); ResumeProcesses(["AZRebalance"]); confirm the split is B=2, A=1.
5. SetDesiredCapacity 2.
   AWS: the OLDER of the two B instances is terminated; A's instance survives. Emulator: A's instance (the globally oldest) is terminated and both B instances survive.

Cause 3:
1. Create a launch template and two Auto Scaling groups in the same subnet with MinSize=0, MaxSize=2, DesiredCapacity=1.
2. PutWarmPool(AutoScalingGroupName=<control>, MinSize=1, PoolState="Stopped").
3. PutWarmPool(AutoScalingGroupName=<subject>, MinSize=1, PoolState="Stopped", InstanceReusePolicy={"ReuseOnScaleIn": true}).
4. Poll DescribeAutoScalingInstances (or DescribeWarmPool) for each group until one instance reports a LifecycleState beginning with "Warmed:".
   AWS: each group reports one Warmed:Stopped instance. Emulator: no Warmed:* instance ever appears, so the subsequent SetDesiredCapacity(0) reuse differential can never be reached.
