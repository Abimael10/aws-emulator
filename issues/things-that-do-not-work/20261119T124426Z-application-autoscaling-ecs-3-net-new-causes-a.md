Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__ecs.target-tracking-converges-to-the-target-and-stops application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum
Issue-Key: application-autoscaling__ecs.target-tracking-converges-to-the-target-and-stops|run#40|AssertionError|assert len(set(readings)) == 1, (
Issue-Key: application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum|_arbitrate#22|AssertionError|assert len(set(settle)) == 1, (
Issue-Commit: d0b314452d42d902975a5c2c289d754b82616615

# application-autoscaling__ecs: 3 net-new causes — a converged target-tracking policy runs on to MaxCapacity, simultaneous step policies are summed instead of arbitrated, and a step policy re-applies its adjustment every evaluation

> _Note: this issue was identified against the code base at commit `d0b314452d42d902975a5c2c289d754b82616615`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScalingPolicy

## Description
Three distinct defects in the application-autoscaling scaling engine as it drives ecs:service:DesiredCount. Each cause is independent and must be closed separately.

Cause 1 of 3: a TargetTrackingScaling policy whose tracked metric is sitting AT its TargetValue keeps scaling out until it is clamped at MaxCapacity. Failing check: application-autoscaling__ecs.target-tracking-converges-to-the-target-and-stops.
AWS: Smithy — PutScalingPolicy.TargetTrackingScalingPolicyConfiguration.TargetValue (required, double) is 'The target value for the metric'; ScaleOutCooldown/ScaleInCooldown are the only timers and were both set to 0 by the check, so no timer is involved. The model states no control algebra, so for the arithmetic this is recorded behavior only: AWS's target-tracking controller is proportional and comes to rest once the metric is at (or within the controller's tolerance of) the target, leaving capacity unchanged while the metric is held there, and it must not end on a bound when the fixed point is strictly inside (MinCapacity, MaxCapacity).
Emulator: with a closed-loop metric of 400/desiredCount against TargetValue=50.0 (fixed point 8, bounds Min=1/Max=20), the emulator did reach 8 and then moved capacity to 20 — exactly MaxCapacity — and held it. Observed desiredCount over the 120 s post-convergence freeze window: [8, 8, 20, 20, 20, 20]. At 8 the tracked metric is 400/8 = 50.0, i.e. the target itself, so the only correct action is none. Hypothesis: the controller treats metric >= target as a breach with no deadband and/or does not compute the proportional new capacity, so each evaluation scales out again until the MaxCapacity clamp absorbs it.
Why: a client using target tracking to size an ECS service gets a target that never settles — it ratchets to MaxCapacity and stays there, so capacity planning, cost projections and any test that asserts 'converged, therefore stable' are all wrong, and the ONLY thing that stops the emulator is the bound the customer set as a safety limit.
Fix: the target-tracking evaluator of the application-autoscaling scaling engine (the component that computes a new capacity from TargetValue and the tracked metric and decides whether to act), not the clamp.
Traces: application-autoscaling__ecs.target-tracking-converges-to-the-target-and-stops.

Cause 2 of 3: two step-scaling policies on the SAME scalable target and dimension triggered at the same time have both adjustments applied (the sum), instead of the one producing the largest resulting capacity winning and the other being suppressed. Failing check: application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum.
AWS: Smithy — StepScalingPolicyConfiguration (AdjustmentType=ChangeInCapacity, StepAdjustments, Cooldown) per policy; the model states no arbitration rule, so this is recorded behavior only plus AWS's documented step-scaling rule (step-scaling-policy-overview): when more than one policy is triggered at the same time, Application Auto Scaling applies the policy that provides the LARGEST resulting capacity and the other leaves no scaling activity. From desiredCount 5 with two triggered scale-out policies of +2 and +9, the single correct outcome is 14, and DescribeScalingActivities must contain an activity attributable to the +9 policy and NONE attributable to the +2 policy.
Emulator: desiredCount moved in increments of 11 = 2 + 9, i.e. both adjustments applied together on each evaluation: observed [25, 25, 25, 25, 25, 36, 36, 36, 36, 36, 47, ...] across the 45 s settle window. The sum, not the largest.
Why: a client that deliberately layers a gentle and an aggressive step policy on one dimension (the standard pattern) gets double the capacity change AWS would make on every simultaneous trigger, and its activity ledger attributes work to a policy real AWS suppresses.
Fix: the step-scaling arbitration in the application-autoscaling scaling engine — when several policies on one (ResourceId, ScalableDimension) are triggered in the same evaluation it must select the single adjustment yielding the largest resulting capacity and record one activity for it only.
Traces: application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum.

Cause 3 of 3: a step-scaling policy's adjustment is re-applied on every subsequent evaluation while its alarm merely REMAINS in ALARM, so capacity ratchets without bound instead of one trigger producing one outcome. Failing check: application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum.
AWS: recorded behavior only (the Smithy model describes StepAdjustments and Cooldown but no invocation rule). A CloudWatch alarm invokes its AlarmActions on a STATE TRANSITION, not continuously while it stays in ALARM, so one breach produces one step adjustment and desiredCount then holds; the recording shows capacity settling at a single value and staying there with the alarm still in ALARM.
Emulator: with the four alarms held in a fixed state and nothing new transitioning, desiredCount kept stepping every few seconds — [25, 25, 25, 25, 25, 36, 36, 36, 36, 36, 47, 47, ... 47] — a fresh adjustment per evaluation window, which is also why the capacity observed was already far above the start of 5. The capacity never settles while the metric is held.
Why: a client whose metric legitimately stays above a threshold (a sustained load plateau) sees desiredCount climb on every period until MaxCapacity, instead of the single step AWS makes — unbounded over-provisioning and a scaling-activity log full of actions real AWS never takes.
Fix: the alarm-to-policy invocation path and the step-scaling evaluator of the application-autoscaling engine — a policy must be driven by the alarm's state TRANSITION (and then be subject to Cooldown), not re-fired on each evaluation while the state is unchanged.
Traces: application-autoscaling__ecs.two-policies-triggered-together-scale-by-the-largest-not-the-sum.

Related but distinct open issues for this service, neither of which carries these checks' replay keys: 20261111T010150Z-application-autoscaling-ecs-step-scaling-action-inside-the-cooldown-never-nets.md (capacity FROZEN inside a cooldown — the opposite symptom) and 20261109T144605Z-application-autoscaling-ecs-2-net-new-causes-an.md.

## Evidence
Cause 1 — expected (AWS): with a closed-loop metric of 400/desiredCount, TargetValue=50.0, MinCapacity=1, MaxCapacity=20, ScaleOutCooldown=ScaleInCooldown=0, capacity converges near the fixed point 8 and then STOPS: six reads 20 s apart all return the same value, and that value is not MaxCapacity.
Actual (emulator): ECS DescribeServices desiredCount moved through [8, 8, 20, 20, 20, 20] over the 120 s after convergence while the metric was held at ~50.0. AssertionError: "ECS desiredCount for '<service>' moved through [8, 8, 20, 20, 20, 20] over the 120s after it converged, while the metric was held at approximately the target value 50.0; a converged target tracking policy must stop, not oscillate". 20 is exactly the registered MaxCapacity.

Causes 2 and 3 — expected (AWS): from desiredCount 5, with StepScaling policies of +2 and +9 (Cooldown=0 on both) whose alarms transition to ALARM together, desiredCount becomes 14 once and holds for the 45 s settle window; DescribeScalingActivities has an activity naming the +9 policy, none naming the +2 policy, and an activity 'Setting desired count to 14.'.
Actual (emulator): desiredCount moved through [25, 25, 25, 25, 25, 36, 36, 36, 36, 36, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47, 47] over the 45 s settle window — increments of exactly 11 (= 2 + 9, both policies applied) repeated per evaluation while no alarm changed state. AssertionError: "leg 1: ECS desiredCount for '<service>' moved through [...] over the 45s after the arbitration. Two triggers must produce ONE outcome; a second adjustment arriving during this window is the apply-both implementation caught in the act".

## Steps to Reproduce
Cause 1:
1. Create an ECS cluster, task definition and service with desiredCount=2; RegisterScalableTarget(ServiceNamespace=ecs, ResourceId=service/<cluster>/<service>, ScalableDimension=ecs:service:DesiredCount, MinCapacity=1, MaxCapacity=20).
2. Publish a CloudWatch custom metric equal to 400 / (current desiredCount) every 10 s (closed loop).
3. PutScalingPolicy(PolicyType=TargetTrackingScaling, TargetValue=50.0, CustomizedMetricSpecification on that metric, ScaleOutCooldown=0, ScaleInCooldown=0).
4. Wait for desiredCount to rise and reach ~8 (the fixed point 400/50).
5. Keep publishing and read DescribeServices desiredCount six times, 20 s apart: AWS returns the same value every time; the emulator moves to 20 (MaxCapacity) and stays.

Causes 2 and 3:
1. Create an ECS cluster/task definition/service with desiredCount=5; RegisterScalableTarget on ecs:service:DesiredCount with MinCapacity=1, MaxCapacity=60.
2. PutScalingPolicy four StepScaling policies with AdjustmentType=ChangeInCapacity, Cooldown=0: +2 and +9 on MetricIntervalLowerBound=0.0, -3 and -9 on MetricIntervalUpperBound=0.0.
3. PutMetricAlarm one alarm per policy (Period=10, EvaluationPeriods=1, DatapointsToAlarm=1, Threshold=50.0), AlarmActions=[that policy's ARN], on a per-arm dimension of one custom metric.
4. Publish 55.0 on the two scale-out arms and 70.0 on the two scale-in arms until the two scale-out alarms are both in ALARM (they transition within a few seconds of each other).
5. Poll DescribeServices desiredCount every 2 s for 45 s: AWS shows one move to 14 and no further change; the emulator shows repeated +11 steps (sum of both adjustments, re-applied each evaluation).
