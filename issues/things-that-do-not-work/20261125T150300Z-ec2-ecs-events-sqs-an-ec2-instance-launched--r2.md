Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs__events__sqs.xsvc-ecs-2-container-instance-draining-event-reaches-draining-rule-not-active-rule
Issue-Key: ec2__ecs__events__sqs.xsvc-ecs-2-container-instance-draining-event-reaches-draining-rule-not-active-rule|_poll_until#11|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 0970c67cd532b68a1462dc1d98bdb119ad45d0ac

# ec2__ecs__events__sqs: an EC2 instance launched with ECS-agent user data never registers as a container instance, so the draining-vs-active event differential reaches no verdict

> _Note: this issue was identified against the code base at commit `0970c67cd532b68a1462dc1d98bdb119ad45d0ac`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ListContainerInstances

## Description
Cause 1 of 1: an instance launched into an ECS cluster with the agent's `ECS_CLUSTER` user data reaches `running` on the EC2 side but never appears as a container instance, so no container-instance state change is ever produced and the DRAINING-rule / ACTIVE-rule negative-delivery differential cannot be graded.

AWS: Smithy (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113) models ListContainerInstances/DescribeContainerInstances over `ContainerInstance`, whose `status` carries ACTIVE and DRAINING; the registration itself is performed by the in-guest ECS agent calling RegisterContainerInstance, so the behavior is recorded, not modelled. Recorded behavior: an EC2 instance booted from an ECS-optimized AMI with `ECS_CLUSTER=<cluster>` written to /etc/ecs/ecs.config registers itself within the launch window and is returned by ListContainerInstances with `status` ACTIVE; UpdateContainerInstancesState to DRAINING then publishes one EventBridge event with source `aws.ecs` and detail-type `ECS Container Instance State Change` whose detail reports the new status, and that event matches only a rule whose pattern names DRAINING — a rule patterned on ACTIVE must receive nothing.

Emulator: the instance reaches `running`, but ListContainerInstances never reports it for the cluster. The replay stops at the bounded registration wait: `TimeoutError: EC2 instance i-<id> registers as a container instance: the run budget ended first; not graded`. The instance id is a per-run value; the requirement is that an instance launched with that user data become a container instance of the named cluster. Because no container instance exists, nothing can be moved to DRAINING and no `ECS Container Instance State Change` event is published to either rule, so both the positive and the negative delivery leg are unobservable.

Why: a client that builds the documented ECS-on-EC2 capacity path — launch instances with ECS_CLUSTER user data, wait for them in ListContainerInstances, then drain one — hangs forever on step two. Nothing can be scheduled onto EC2 capacity, and any automation that reacts to container-instance state changes (draining lifecycle hooks, capacity-provider scale-in, SQS-fed drain handlers) never fires, while the emulator reports a healthy cluster and healthy instances.

Fix: the ECS container-instance registration path — the guest-side agent bootstrap that should consume the instance's user data / ecs.config and the ECS state that ListContainerInstances reads — plus the `ECS Container Instance State Change` event emission on UpdateContainerInstancesState so the DRAINING detail reaches an eventbridge rule patterned on it.

Traces: check ec2__ecs__events__sqs.xsvc-ecs-2-container-instance-draining-event-reaches-draining-rule-not-active-rule.

## Evidence
Expected: a t3.micro launched into the run's network with user data setting ECS_CLUSTER=<cluster> appears in ListContainerInstances(cluster) with status ACTIVE within the registration window; UpdateContainerInstancesState(..., status=DRAINING) then delivers exactly one `aws.ecs` / `ECS Container Instance State Change` event, whose detail reports DRAINING, to the SQS target of the DRAINING-patterned rule and NOTHING to the ACTIVE-patterned rule's queue.

Actual: the container instance never registers. `TimeoutError: EC2 instance i-0303146cec6504e39 registers as a container instance: the run budget ended first; not graded` (raised from the bounded _poll_until over the ListContainerInstances/DescribeContainerInstances lookup). No state change and no event are produced. Verdict: error.

## Steps to Reproduce
1. Create an ECS cluster and two EventBridge rules on source `aws.ecs`, detail-type `ECS Container Instance State Change` — one patterned on detail.status DRAINING, one on ACTIVE — each targeting its own SQS queue.
2. Launch a t3.micro into the run's VPC/subnet with an instance profile for ECS and base64 user data writing `ECS_CLUSTER=<cluster>` to /etc/ecs/ecs.config; wait for `running`.
3. Poll ListContainerInstances(cluster)/DescribeContainerInstances for a container instance whose ec2InstanceId is that instance.
4. Observe it never appears, so UpdateContainerInstancesState to DRAINING is never reached and neither queue receives an event.
