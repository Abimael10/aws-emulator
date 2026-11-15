Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__ecs.managed-scaling-target-capacity-above-100-rejected-and-tracking-alarm-unchanged
Issue-Key: cloudwatch__ec2__ecs.managed-scaling-target-capacity-above-100-rejected-and-tracking-alarm-unchanged|_poll_until#20|Timeout|time.sleep(interval_s)
Issue-Commit: a49cdab94fc41177458b6e53730040285f53d8d6

# ECS CreateCapacityProvider with managedScaling ENABLED never creates the ASG target-tracking policy or its CapacityProviderReservation alarm, so the managed-scaling target-capacity scenario never progresses

> _Note: this issue was identified against the code base at commit `a49cdab94fc41177458b6e53730040285f53d8d6`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCapacityProvider

## Description
One net-new root cause.

Cause 1 of 1: an ECS capacity provider created with autoScalingGroupProvider.managedScaling.status=ENABLED does not produce the ECS-owned target-tracking scaling policy on the backing Auto Scaling group, nor the CloudWatch CapacityProviderReservation alarms that policy owns, so the whole managed-scaling wiring is absent and the check never reaches its UpdateCapacityProvider assertion before the 60s budget expires.

AWS: Smithy (ecs) models CreateCapacityProvider/UpdateCapacityProvider with AutoScalingGroupProvider.managedScaling (ManagedScaling: status, targetCapacity, minimumScalingStepSize, maximumScalingStepSize, instanceWarmupPeriod). AWS documents that when managed scaling is enabled ECS creates and owns a target-tracking scaling policy on the ASG whose metric is CapacityProviderReservation (dimensions CapacityProviderName and ClusterName) with TargetValue equal to targetCapacity, and CloudWatch creates the matching AlarmHigh (GreaterThanThreshold) and AlarmLow alarms for it. UpdateCapacityProvider with targetCapacity outside 1..100 is refused with the message "The managed scaling target capacity value is invalid. Specify an integer greater than 0 and less than or equal to 100 and try again." and leaves the policy's TargetValue and the alarms at their previous values; a valid update propagates to both. The policy/alarm creation is recorded behavior only; the member names and bounds are from the model.

Emulator: after CreateCapacityProvider(managedScaling={status: ENABLED, targetCapacity: 100}) and associating it with the cluster, autoscaling.DescribePolicies for the ASG never yields exactly one TargetTrackingScaling policy with alarms, so cloudwatch.DescribeAlarms is never reachable; the check's wiring predicate never becomes true and the replay reports "Error: Check timed out after 60.0s". Nothing is ever created, so the refusal boundary this check pins (targetCapacity=101 refused, wiring unchanged, 100->80 and 90 propagated) is unobservable.

Why: a client that enables ECS managed scaling gets a capacity provider that never scales — no policy, no alarms — while the control plane reports the provider as created with managed scaling enabled. Capacity never follows CapacityProviderReservation, and the operator has no alarm to observe or to key their own automation on. An open issue 20261111T064629Z-cloudwatch-ec2-ecs-ecs-managed-scaling-never-creates-the-target-tracking.md describes the same family but does not carry this check's replay key.

Fix: the ECS capacity-provider handler and the managed-scaling state it owns — on create/update with managedScaling ENABLED it must create/update the ASG TargetTrackingScaling policy keyed on the CapacityProviderReservation metric and its CloudWatch AlarmHigh/AlarmLow, and must validate targetCapacity against 1..100 with AWS's message before touching them.

Traces: cloudwatch__ec2__ecs.managed-scaling-target-capacity-above-100-rejected-and-tracking-alarm-unchanged.

## Evidence
Expected (AWS): after CreateCapacityProvider with managedScaling ENABLED and targetCapacity 100, autoscaling.DescribePolicies on the backing ASG returns exactly one TargetTrackingScaling policy with TargetValue 100 and a non-empty Alarms list, and cloudwatch.DescribeAlarms on those names returns exactly one GreaterThanThreshold alarm on metric CapacityProviderReservation with dimensions CapacityProviderName/ClusterName; UpdateCapacityProvider(targetCapacity=101) is then refused with "The managed scaling target capacity value is invalid. Specify an integer greater than 0 and less than or equal to 100 and try again." leaving both unchanged.

Actual (emulator): Error: Check timed out after 60.0s — the target-tracking policy and its alarms never appear, so the wiring predicate never becomes true and the refusal boundary is never exercised.

## Steps to Reproduce
1. Create an ECS cluster, a launch template from the ECS-optimized AMI, and an Auto Scaling group in a private subnet.
2. Call ecs.CreateCapacityProvider(name=<cp>, autoScalingGroupProvider={autoScalingGroupArn: <asg arn>, managedScaling: {status: 'ENABLED', targetCapacity: 100}, managedTerminationProtection: 'DISABLED'}) and associate it with the cluster.
3. Poll autoscaling.DescribePolicies(AutoScalingGroupName=<asg>) for a single TargetTrackingScaling policy and read its Alarms; then cloudwatch.DescribeAlarms(AlarmNames=<those>) for the GreaterThanThreshold alarm.
4. AWS yields the policy (TargetValue 100) and the CapacityProviderReservation AlarmHigh. The emulator never creates either, so the poll never completes.
5. Then call ecs.UpdateCapacityProvider with managedScaling.targetCapacity=101 and confirm the documented refusal leaves the policy and alarm untouched.
