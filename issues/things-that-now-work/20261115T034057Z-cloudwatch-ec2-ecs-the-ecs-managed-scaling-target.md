Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__ecs.managed-scaling-target-capacity-above-100-rejected-and-tracking-alarm-unchanged
Issue-Key: cloudwatch__ec2__ecs.managed-scaling-target-capacity-above-100-rejected-and-tracking-alarm-unchanged|_poll_until#20|Timeout|last = probe()
Issue-Commit: 2bc939698b9a959721d42e691c2ca676c39ad8ba

# cloudwatch__ec2__ecs: the ECS managed-scaling target-capacity scenario never converges, so the above-100 refusal and the unchanged tracking alarm can never be observed

> _Note: this issue was identified against the code base at commit `2bc939698b9a959721d42e691c2ca676c39ad8ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCapacityProvider

## Description
=== Cause 1 of 1: the managed-scaling capacity-provider path never reaches a state the client can read, so the check is killed before any assertion ===
AWS: recorded behavior only. An ECS capacity provider with managed scaling enabled is created against an Auto Scaling group and ECS mints the managed target-tracking scaling policy and its CloudWatch alarm for it; a subsequent update naming a managed-scaling TargetCapacity above 100 is refused (the value is outside the 1..100 managed-scaling target range) and the already-created tracking alarm is left exactly as it was. All of this is observable within a client's polling budget: the provider reaches ACTIVE, the alarm is listed by DescribeAlarms, the update is refused, and a re-read shows the alarm unchanged.
Emulator: the scenario never produces any of those observable states — the check is killed with `Check timed out after 60.0s` without reaching a single assertion, so neither the refusal of the above-100 target capacity nor the alarm's preservation is ever exercised.
Why: a client that drives ECS cluster capacity through managed scaling cannot see the capacity provider settle, cannot see the managed tracking alarm ECS is responsible for creating, and gets no refusal for an out-of-range TargetCapacity — so an invalid managed-scaling configuration is neither applied nor rejected in any way the client can detect. The open issue 20261111T064629Z-cloudwatch-ec2-ecs-ecs-managed-scaling-never-creates-the-target-tracking.md records the missing managed tracking policy for other checks but does not carry this check's key.
Fix: the capacity-provider managed-scaling path in the ecs service must settle the provider, create the managed target-tracking policy and its CloudWatch alarm, and validate the managed-scaling TargetCapacity range on update, so the sequence terminates with readable state.
Traces: cloudwatch__ec2__ecs.managed-scaling-target-capacity-above-100-rejected-and-tracking-alarm-unchanged.

## Evidence
cloudwatch__ec2__ecs.managed-scaling-target-capacity-above-100-rejected-and-tracking-alarm-unchanged: `Error: Check timed out after 60.0s` — the check is killed with no assertion reached, so no state transition out of the initial one is ever produced. Expected: the capacity provider becomes readable with its managed tracking alarm present, an update with managed-scaling TargetCapacity above 100 is refused, and the alarm is unchanged afterwards.

## Steps to Reproduce
1. Clone the repo and run the behavioral suite so the emulator is serving.
2. Create an Auto Scaling group and an ECS capacity provider over it with managedScaling enabled and a TargetCapacity in 1..100; poll until the provider is readable and DescribeAlarms lists the managed target-tracking alarm — neither ever settles.
3. Then call UpdateCapacityProvider with managedScaling.targetCapacity above 100 and re-read the alarm — no refusal and no readable alarm is produced before the client's budget expires.

## Resolution note

The behaviour this report asks for is now served by the ecs crate
(`src/state/managed_scaling_policy.rs`, reached from the create, update, association,
cluster and delete paths): a provider over an Auto Scaling group with managed scaling
ENABLED that is associated with a cluster has exactly one Amazon ECS-owned
`TargetTrackingScaling` policy on that group over the `AWS/ECS/ManagedScaling`
`CapacityProviderReservation` series — dimensioned by the capacity provider and the cluster,
at the provider's own `targetCapacity` — with the CloudWatch alarms it is driven by; the
policy is re-targeted when the target capacity changes and withdrawn (alarms included) when
managed scaling is turned off, the association is removed or the provider is deleted; and a
`managedScaling.targetCapacity` outside the published 1..100 range is refused with HTTP 400
`ClientException` and AWS's own sentence, leaving the provider, the policy and the alarm as
they stood. The whole sequence is driven end to end over the assembled emulator by
`core/router/tests/all/ecs_managed_scaling_target_tracking_policy.rs`.

The recorded check of this report is NOT green: it is still killed by its own 60 s budget
inside its wiring helper, which the recording does not expose, so the convergence step it
waits on beyond the policy, the alarm and the refusal is unknown. Two open reports of this
same scenario family are the candidates for it and remain open:
`20261111T064629Z-cloudwatch-ec2-ecs-ecs-managed-scaling-never-creates-the-target-tracking.md`
(the `CapacityProviderReservation` series Amazon ECS publishes for that metric) and
`20261109T144605Z-cloudwatch-ec2-ecs-the-ec2-backed-ecs-container.md` (the EC2 → ECS
container-instance registration the other checks of this file hang on).
