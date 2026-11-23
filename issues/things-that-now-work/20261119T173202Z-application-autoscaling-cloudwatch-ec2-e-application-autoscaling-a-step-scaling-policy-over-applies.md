Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__cloudwatch__ec2__ecs__sqs.step-scaling-never-fed-alarm-stays-insufficient-while-fed-twin-scales
Issue-Key: application-autoscaling__cloudwatch__ec2__ecs__sqs.step-scaling-never-fed-alarm-stays-insufficient-while-fed-twin-scales|run#33|AssertionError|assert desired_after <= _START_COUNT + 1 + 2, (
Issue-Commit: 23a6305fb4194ac652f562ce813f73dafa581013

# application-autoscaling: a step-scaling policy over-applies its ChangeInCapacity step — desiredCount overshoots the +2 step (1→2→5) while the other policy is inside its 3600s cooldown

> _Note: this issue was identified against the code base at commit `23a6305fb4194ac652f562ce813f73dafa581013`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScalingPolicy

## Description
Cause 1 of 1: a StepScaling policy's ChangeInCapacity adjustment is applied more than the policy declares (or a policy inside its Cooldown acts again), so an ECS scalable target's desiredCount overshoots the arithmetic the registered policies permit.

AWS: Smithy PutScalingPolicy with PolicyType=StepScaling and StepScalingPolicyConfiguration{AdjustmentType=ChangeInCapacity, MetricAggregationType=Maximum, Cooldown, StepAdjustments:[{MetricIntervalLowerBound:0.0, ScalingAdjustment:N}]}. Application Auto Scaling applies exactly ONE step adjustment per alarm action: the step whose interval contains the breach, i.e. current capacity + N, clamped to the scalable target's MinCapacity/MaxCapacity. A policy whose Cooldown has not elapsed since its last scale-out does not scale out again (at most it nets the earlier step into the new one). Two policies on the same ResourceId/ScalableDimension therefore bound the reachable capacity: from capacity C, a +2 policy firing while a +1 policy is inside a 3600 s cooldown can reach C+2 at most (C+1+2 counting the netted prior step), never more.

Emulator: starting from desiredCount=1 with MinCapacity=1/MaxCapacity=6, the +1 control policy correctly scales 1→2 on its alarm. The +2 subject policy (Cooldown=0) then fires once, and desiredCount is observed at 5 — two more than the single +2 step allows from 2. The emulator either replays the step adjustment more than once per alarm action or re-applies the control policy's +1 inside its 3600 s Cooldown; the ECS service's desiredCount then exceeds what any single declared step permits. Replay detail: `AssertionError: desiredCount became 5; the +2 subject policy from 2 gives at most 4`.

Why: a client that sizes MaxCapacity and step sizes to bound cost and blast radius gets more capacity than the policy it wrote can ask for. An autoscaling consumer reconciling observed capacity against (initial capacity + sum of the step adjustments it saw fire) finds an unexplained surplus, and cooldown — the only control a step policy has against runaway scale-out — is not observable.

Fix: the step-scaling execution path of the application-autoscaling emulator (the alarm-action handler that turns an ALARM transition into a capacity change): apply exactly one matching StepAdjustment per alarm action against the target's current capacity, and suppress (or net) a policy whose Cooldown window has not elapsed, before writing the new desiredCount through to ECS UpdateService.

Traces: application-autoscaling__cloudwatch__ec2__ecs__sqs.step-scaling-never-fed-alarm-stays-insufficient-while-fed-twin-scales.

## Evidence
Expected (AWS): with a scalable target registered on ecs/service:DesiredCount (MinCapacity 1, MaxCapacity 6) at desiredCount 1, a +1 ChangeInCapacity policy (Cooldown 3600) scales it to 2; then a +2 ChangeInCapacity policy (Cooldown 0) on the same target scales it to 4 at most — 3 if the control policy's cooldown nets its prior +1 into the step.
Actual (emulator): after the second (subject) alarm reached ALARM, DescribeServices reported desiredCount = 5. Replay: `AssertionError: desiredCount became 5; the +2 subject policy from 2 gives at most 4` (check_application_autoscaling__cloudwatch__ec2__ecs__sqs_step_scaling_never_fed_alarm_stays_insufficient_while_fed_twin_scales.py line 381). The numbers 1/2/5 are this run's capacities; the requirement is the inequality: reached capacity <= previous capacity + the single matching ScalingAdjustment, with a cooled-down policy contributing nothing new.

## Steps to Reproduce
1. Create an ECS cluster and a service with desiredCount=1; RegisterScalableTarget(ServiceNamespace=ecs, ResourceId=service/<cluster>/<service>, ScalableDimension=ecs:service:DesiredCount, MinCapacity=1, MaxCapacity=6).
2. PutScalingPolicy A: StepScaling, ChangeInCapacity, Cooldown=3600, StepAdjustments=[{MetricIntervalLowerBound:0.0, ScalingAdjustment:1}].
3. PutScalingPolicy B: StepScaling, ChangeInCapacity, Cooldown=0, StepAdjustments=[{MetricIntervalLowerBound:0.0, ScalingAdjustment:2}].
4. PutMetricAlarm for each policy (Period 10, EvaluationPeriods 1, Statistic Maximum, GreaterThanThreshold 50, AlarmActions=[policy ARN]) on two dimensions of one high-resolution custom metric.
5. Publish 75 on A's dimension until its alarm is ALARM; poll DescribeServices until desiredCount rises — it is 2 (correct).
6. Repoint B's alarm at the fed dimension and publish 75 until B's alarm is ALARM.
7. Poll DescribeServices: desiredCount reaches 5. Expected: at most 4 (2 + the single +2 step), with policy A silent inside its 3600 s cooldown.
