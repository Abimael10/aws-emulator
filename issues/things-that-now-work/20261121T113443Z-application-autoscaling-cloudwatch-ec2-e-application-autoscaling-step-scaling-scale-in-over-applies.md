Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-suspended-scale-in-holds-while-scale-out-still-fires
Issue-Key: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-suspended-scale-in-holds-while-scale-out-still-fires|run#38|AssertionError|assert fallen == peak - 1, f'desiredCount fell {peak} -> {fallen}; one step of -1 expected'
Issue-Commit: bf168f2125883f6ad6bec616d1c212f3f21e9ddc

# application-autoscaling step-scaling scale-in over-applies: resuming DynamicScalingIn drops an ECS desiredCount 3 → 1 in one go where the single -1 StepAdjustment allows only 3 → 2

> _Note: this issue was identified against the code base at commit `bf168f2125883f6ad6bec616d1c212f3f21e9ddc`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutScalingPolicy

## Description
Cause 1 of 1: a StepScaling policy's negative ChangeInCapacity adjustment is applied more than the policy declares on the scale-IN direction, so an ECS scalable target's desiredCount undershoots the arithmetic the registered policy permits.

AWS: Smithy PutScalingPolicy with PolicyType=StepScaling and StepScalingPolicyConfiguration{AdjustmentType=ChangeInCapacity, Cooldown=0, StepAdjustments=[{MetricIntervalUpperBound: 0.0, ScalingAdjustment: -1}]} on ServiceNamespace=ecs, ScalableDimension=ecs:service:DesiredCount. Application Auto Scaling applies exactly ONE matching step adjustment per alarm action: new capacity = current capacity + ScalingAdjustment, clamped to the target's MinCapacity/MaxCapacity, and it records one scaling activity whose Description is `Setting desired count to <new capacity>.` and whose Cause names the alarm and the policy. From capacity 3 with MinCapacity=1 and a single -1 step, the first scale-in after RegisterScalableTarget clears SuspendedState.DynamicScalingInSuspended therefore reaches 2 — never the floor in one action.

Emulator: with DynamicScalingInSuspended correctly honoured during the hold and scale-out correctly reaching MaxCapacity=3, the first scale-in observed after the suspension is lifted takes desiredCount straight from 3 to 1 — two steps' worth in one transition (equivalently, it collapses to MinCapacity instead of applying the declared step). The activity `Setting desired count to 2.` that AWS would record for that step never exists, so the intermediate capacity is neither served nor reported. This is the scale-IN mirror of the open issue 20261119T173202Z-application-autoscaling-cloudwatch-ec2-e-application-autoscaling-a-step-scaling-policy-over-applies.md (scale-out over-applies, filed from a different check in application-autoscaling__cloudwatch__ec2__ecs__sqs); that issue does not carry this check's replay key.

Why: a client sizes step adjustments to bound how fast capacity can leave a service — one task at a time, with MinCapacity as a floor it is never supposed to hit on a single breach. Here one alarm action removes twice the capacity the policy asked for, dropping the service to its floor; a consumer reconciling observed capacity against (previous capacity + the step adjustments it saw fire) finds an unexplained deficit, and the per-step scaling activities it audits are missing.

Fix: the step-scaling execution path of the application-autoscaling emulator (the alarm-action handler that turns an ALARM transition into a capacity change and writes it through to ECS UpdateService): apply exactly one matching StepAdjustment against the target's current capacity per alarm action, clamp to MinCapacity/MaxCapacity only, and record one activity per application with AWS's Description and Cause.

Traces: application-autoscaling__cloudwatch__ec2__ecs__elbv2.step-scaling-suspended-scale-in-holds-while-scale-out-still-fires.

## Evidence
Expected (AWS): scalable target on ecs/service:DesiredCount with MinCapacity=1, MaxCapacity=3; a +1 step policy and a -1 step policy (both Cooldown=0) each driven by their own CloudWatch alarm. After the scale-out leg has taken desiredCount to 3 and DynamicScalingInSuspended is set back to false, the in alarm's next action takes desiredCount 3 → 2 and records the activity `Setting desired count to 2.` naming the in alarm and the in policy.
Actual (emulator): the first observed scale-in took desiredCount 3 → 1. Replay: `AssertionError: desiredCount fell 3 -> 1; one step of -1 expected` (check_application_autoscaling__cloudwatch__ec2__ecs__elbv2_step_scaling_suspended_scale_in_holds_while_scale_out_still_fires.py line 392). The capacities 3/2/1 are this run's values; the requirement is the arithmetic: each alarm action moves capacity by exactly the one matching ScalingAdjustment.

## Steps to Reproduce
1. Create an ECS cluster and a service with desiredCount=2.
2. RegisterScalableTarget(ServiceNamespace='ecs', ResourceId='service/<cluster>/<service>', ScalableDimension='ecs:service:DesiredCount', MinCapacity=1, MaxCapacity=3, SuspendedState={'DynamicScalingInSuspended': True}).
3. PutScalingPolicy OUT: StepScaling, ChangeInCapacity, Cooldown=0, StepAdjustments=[{MetricIntervalLowerBound: 0.0, ScalingAdjustment: 1}]; PutScalingPolicy IN: StepScaling, ChangeInCapacity, Cooldown=0, StepAdjustments=[{MetricIntervalUpperBound: 0.0, ScalingAdjustment: -1}].
4. PutMetricAlarm for each policy on one custom metric (GreaterThanThreshold 50 → OUT policy ARN, LessThanThreshold 50 → IN policy ARN).
5. Publish a low value until the IN alarm is ALARM and confirm desiredCount holds at 2 while scale-in is suspended.
6. Publish a high value until the OUT alarm is ALARM and poll DescribeServices until desiredCount reaches 3.
7. RegisterScalableTarget again with SuspendedState={'DynamicScalingInSuspended': False}, then publish the low value until the IN alarm is ALARM.
8. Poll DescribeServices: desiredCount goes to 1. Expected: 2 (one -1 step), with the activity `Setting desired count to 2.` recorded and naming the in alarm and in policy.
