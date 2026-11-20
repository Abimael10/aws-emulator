Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ec2__ecs.obs-ecs-3-reservation-metrics-match-registered-resources-and-pressure-exceeds-100
Issue-Key: cloudwatch__ec2__ecs.obs-ecs-3-reservation-metrics-match-registered-resources-and-pressure-exceeds-100|run#38|AssertionError|assert placed.get("containerInstanceArn") == ci_arn, (
Issue-Commit: a14965d6060559849ea419b74faa25e7836c4558

# cloudwatch__ec2__ecs: an ECS EC2-launch-type task reaches RUNNING with no containerInstanceArn, so a task is never bound to the container instance that hosts it

> _Note: this issue was identified against the code base at commit `a14965d6060559849ea419b74faa25e7836c4558`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
One net-new root cause. No open issue carries this check (20261120T071621Z-cloudwatch-ec2-ecs-2-net-new-causes-an.md covers a neighbouring managed-scaling refusal but not this placement binding).

Cause 1 of 1: a task placed on an EC2 capacity provider reports no container instance.
AWS: RunTask with a capacityProviderStrategy on an EC2-backed capacity provider makes the ECS scheduler place the task on a registered container instance; once the task reaches lastStatus RUNNING, DescribeTasks returns it with containerInstanceArn set to that instance's ARN — the same ARN ListContainerInstances reports for the cluster. Smithy (ecs): Task.containerInstanceArn is "The ARN of the container instances that host the task." Only a task that is not placed (e.g. still PROVISIONING) carries no containerInstanceArn; a RUNNING EC2-launch-type task always does. Recorded behavior only for the placement timing.
Emulator: managed scaling does launch and register exactly one container instance into the cluster, and the task does reach lastStatus RUNNING, but DescribeTasks returns containerInstanceArn = None for that RUNNING task — the task is never bound to the instance hosting it.
Why: every client that joins tasks to hosts breaks — nothing can tell which instance runs a task, so draining an instance, cordoning it, correlating task telemetry with instance telemetry, or computing a cluster's per-instance reservation is impossible. The same missing binding is what makes AWS/ECS CPUReservation / MemoryReservation and AWS/ECS/ManagedScaling CapacityProviderReservation derivable from registered resources, so the reservation metrics this check grades cannot be produced either.
Fix: the ECS RunTask placement path and the task state — placing a task onto a container instance must write containerInstanceArn into the task record that DescribeTasks serializes.
Traces: cloudwatch__ec2__ecs.obs-ecs-3-reservation-metrics-match-registered-resources-and-pressure-exceeds-100.

## Evidence
Check cloudwatch__ec2__ecs.obs-ecs-3-reservation-metrics-match-registered-resources-and-pressure-exceeds-100:
  expected: DescribeTasks(cluster, [task]) on the RUNNING task returns containerInstanceArn equal to the single ARN ListContainerInstances(status='ACTIVE') reports for the cluster.
  actual:   AssertionError: "task A placed on None, not 'arn:aws:ecs:<region>:<account>:container-instance/<cluster>/<container-instance-id>'" — the task's lastStatus was RUNNING (the preceding assert on lastStatus passed) and ListContainerInstances had already returned exactly one ACTIVE container instance.
(The ARN in the message is this run's generated id; the requirement is that the task's containerInstanceArn equals the ARN of whichever instance ListContainerInstances reports.)

## Steps to Reproduce
1. CreateCluster; create a launch template off the ECS-optimized AMI and an Auto Scaling group for it.
2. CreateCapacityProvider over that ASG with managedScaling ENABLED/targetCapacity 100, and associate it with the cluster.
3. RegisterTaskDefinition (cpu 256, memory 256) and RunTask on the cluster with capacityProviderStrategy naming that provider.
4. Poll ListContainerInstances(cluster, status='ACTIVE') until one instance registers.
5. Poll DescribeTasks(cluster, [taskArn]) until lastStatus is RUNNING, then read containerInstanceArn — it is absent instead of naming the registered instance.
