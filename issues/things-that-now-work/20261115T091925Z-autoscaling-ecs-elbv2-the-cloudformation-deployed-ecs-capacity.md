Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ecs__elbv2.deploy-ecs-capacity-provider-autoscaling-cfn
Issue-Key: autoscaling__ecs__elbv2.deploy-ecs-capacity-provider-autoscaling-cfn|_poll_until#10|Timeout|last = probe()
Issue-Commit: f7b60fee7b066cbc6f1eedeac1b0cd0f17c7dcd0

# autoscaling__ecs__elbv2: the CloudFormation-deployed ECS capacity-provider scenario never converges — the check is killed at its 60 s cap with no verdict (1 cause)

> _Note: this issue was identified against the code base at commit `f7b60fee7b066cbc6f1eedeac1b0cd0f17c7dcd0`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCapacityProvider

## Description
One net-new root cause, grounded in the single failing check of this replay that no open issue carries its replay key for.

Cause 1 of 1: a CloudFormation-deployed ECS capacity provider over a 0-capacity Auto Scaling group never produces capacity, so the whole deploy-and-register path never reaches any observable terminal state and the check yields no verdict at all.

AWS: Smithy (ecs) CreateCapacityProvider.autoScalingGroupProvider = AutoScalingGroupProvider{autoScalingGroupArn (required), managedScaling: ManagedScaling{status: ManagedScalingStatus, targetCapacity @range 1..100, minimumScalingStepSize @range 1..10000, maximumScalingStepSize @range 1..10000, instanceWarmupPeriod @range 0..10000}}; the model documents that "When managed scaling is turned on, Amazon ECS manages the scale-in and scale-out actions of the Auto Scaling group. Amazon ECS manages a target tracking scaling policy using an Amazon ECS managed CloudWatch metric with the specified targetCapacity value as the target value for the metric." Beyond that the scenario is recorded behavior only: the stack declares AWS::EC2::LaunchTemplate, AWS::AutoScaling::AutoScalingGroup (MinSize 0 / DesiredCapacity 0 / MaxSize 3), AWS::ECS::CapacityProvider with ManagedScaling Status ENABLED and TargetCapacity 100, AWS::ECS::Cluster naming it in CapacityProviders and DefaultCapacityProviderStrategy, a bridge-mode AWS::ECS::TaskDefinition and an AWS::ECS::Service with DesiredCount 1 and that capacity-provider strategy. On AWS every CreateStack reaches a terminal StackStatus (CREATE_COMPLETE, CREATE_FAILED, ROLLBACK_COMPLETE or ROLLBACK_FAILED) — a stack is never left IN_PROGRESS forever — and ECS managed scaling then raises the ASG's DesiredCapacity above 0 for the unplaceable task so that an instance registers and ListContainerInstances reports at least one containerInstanceArn.

Emulator: the check never completes. The replay reports only `Error: Check timed out after 60.0s`, i.e. no stack status, no scaling activity and no container instance were ever observed. The check's own budgets are much larger than the harness cap (1300 s waiting for a terminal stack status, then 400 s polling DescribeAutoScalingGroups.DesiredCapacity > 0 and ListContainerInstances length >= 1), so the emulator parked the scenario in a non-terminal state rather than answering slowly. Hypothesis: either CloudFormation leaves the stack in CREATE_IN_PROGRESS because AWS::ECS::Service never reaches steady state (its task cannot be placed on a 0-capacity cluster), or the stack completes and ECS managed scaling never moves the ASG's DesiredCapacity; the replay output distinguishes neither, because no intermediate value is reported.

Why: a client that provisions EC2 capacity declaratively — the standard ECS-on-EC2 capacity-provider pattern — gets a CreateStack that never settles and a service whose tasks stay unplaceable forever, with no stack failure, no scaling activity and no error to observe. Any wait-for-stack or wait-for-steady-state call hangs until the client's own timeout, which is worse than an explicit refusal.

Fix: the ECS managed-scaling component for EC2_AUTOSCALING capacity providers (the path that reacts to unplaceable provisioning tasks by raising the Auto Scaling group's desired capacity) together with the Auto Scaling desired-capacity/instance state it must write, and the CloudFormation AWS::ECS::Service resource handler's stabilization, which must always drive the stack to a terminal status instead of waiting unbounded.

Related open issues that do NOT carry this check: 20261114T034444Z-autoscaling-ecs-elbv2-2-net-new-causes-a.md (same managed-scaling symptom for the API-only check ecs-capacity-provider-scales-asg-desired-count), 20261115T073242Z-cloudformation-ecs-2-net-new-causes-managed-scaling.md and 20261115T003554Z-cloudwatch-ec2-ecs-ecs-createcapacityprovider-with-managedscaling-enabled-never-creates-the.md (no target-tracking policy on the capacity provider's ASG). This check is the CloudFormation-deployed variant and is reported unowned by the suite, so it is filed here; a fixer should expect the managed-scaling fix and the CFN stabilization fix to be needed together before it can pass.

Traces: autoscaling__ecs__elbv2.deploy-ecs-capacity-provider-autoscaling-cfn.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator.

[mismatch] autoscaling__ecs__elbv2.deploy-ecs-capacity-provider-autoscaling-cfn
  Error: Check timed out after 60.0s

Expected (AWS): CreateStack reaches a terminal StackStatus (the check waits up to 1300 s for CREATE_COMPLETE / CREATE_FAILED / ROLLBACK_COMPLETE / ROLLBACK_FAILED and requires CREATE_COMPLETE), and then within 400 s DescribeAutoScalingGroups(AutoScalingGroupNames=[<asg>])['AutoScalingGroups'][0]['DesiredCapacity'] > 0 and ListContainerInstances(cluster=<cluster>)['containerInstanceArns'] has at least one entry.
Actual (emulator): no verdict at all — the scenario is still non-terminal when the harness kills the check at its 60 s cap; neither a stack status nor a raised DesiredCapacity nor a registered container instance is ever observed. Every other CloudFormation deploy check in this suite reached a terminal stack status inside the same cap (they fail with assertions, not timeouts), so the stall is in this path, not in the harness budget alone.

Smithy consulted (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/smithy-model/com-amazonaws-ecs-amazonec2containerservicev20141113.json): AutoScalingGroupProvider{autoScalingGroupArn required, managedScaling}, ManagedScaling{status, targetCapacity @range 1..100, minimumScalingStepSize/maximumScalingStepSize @range 1..10000, instanceWarmupPeriod @range 0..10000} and its documentation of ECS-managed target tracking on the Auto Scaling group.

## Steps to Reproduce
1. Clone the repo and replay suite simparity-behavioral-aws-v1; the check id in trace_ids reproduces.
2. CreateStack with a template that declares: a security group and instance role/profile; an AWS::EC2::LaunchTemplate; an AWS::AutoScaling::AutoScalingGroup with MinSize 0, DesiredCapacity 0, MaxSize 3 across two subnets; an AWS::ECS::CapacityProvider over that group with ManagedScaling Status ENABLED, TargetCapacity 100, MinimumScalingStepSize 1, MaximumScalingStepSize 1, InstanceWarmupPeriod 60; an AWS::ECS::Cluster naming the provider in CapacityProviders and DefaultCapacityProviderStrategy; a bridge-mode AWS::ECS::TaskDefinition; and an AWS::ECS::Service with DesiredCount 1 and that capacity-provider strategy.
3. Poll DescribeStacks(StackName=<name>)['Stacks'][0]['StackStatus'] every 5 s. Expected: it reaches a terminal status (CREATE_COMPLETE on AWS). Actual: no terminal status is observed before the harness cap.
4. Then poll DescribeAutoScalingGroups(AutoScalingGroupNames=[<asg>]) and ListContainerInstances(cluster=<cluster>). Expected: DesiredCapacity > 0 and at least one containerInstanceArn. Actual: DesiredCapacity stays 0 and no container instance registers.
5. Re-replay autoscaling__ecs__elbv2.deploy-ecs-capacity-provider-autoscaling-cfn to confirm the fix.

## Resolution note

The code base has drifted since the commit this report was written against, and the
behaviour it asks for is now served: the recorded check replays green, and both halves of
the scenario are served by existing components rather than by anything added here —
CloudFormation drives a `CreateStack` to a terminal `StackStatus` for this template
(`CREATE_COMPLETE`), and the Amazon ECS managed-scaling component
(`services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/state/managed_capacity_scaling.rs`)
raises the Auto Scaling group's desired capacity for the tasks the cluster cannot place,
whereupon Amazon EC2 Auto Scaling launches the instances and their agents register them as
container instances of the cluster
(`.../src/state/ec2_capacity.rs`), which is where the parked tasks are then placed.

What was missing was the PROOF, so this change adds it:
`core/router/tests/all/cfn_deployed_managed_scaling_provider_acquires_its_capacity.rs`
deploys the declarative pattern of this report — a launch template, an Auto Scaling group
with `MinSize`/`DesiredCapacity` 0, a capacity provider over it with managed scaling
`ENABLED`, a cluster naming the provider, a `bridge` task definition and a service whose
tasks cannot be placed — over the assembled emulator, and asserts the stack reaches a
terminal `CREATE_COMPLETE`, the group's `DesiredCapacity` is raised to at least the
shortfall taken under the provider's two scaling step sizes and never past the group's own
`MaxSize`, `ListContainerInstances` reports the registered capacity as `ACTIVE`, and the
service's `runningCount` reaches the declared `DesiredCount`. Its rows span what the rule
generalises over: how many tasks are asked for, whether the capacity-provider strategy is
the cluster's default or the service's own, and the two step sizes (a maximum that caps a
two-task shortfall, and a minimum that is a floor under a one-task one).
