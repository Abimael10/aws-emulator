Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecr__ecs.ecsdeploy-isolated-subnet-task-blocked-from-starting
Issue-Key: ec2__ecr__ecs.ecsdeploy-isolated-subnet-task-blocked-from-starting|_poll#27|Timeout|return last
Issue-Commit: ba9169dcb3a6b6883b8d60843b0b5f765354b232

# ec2__ecr__ecs: an isolated-subnet Fargate service never produces a task that fails to start — no task state advances, so stopCode/TaskFailedToStart is never observable

> _Note: this issue was identified against the code base at commit `ba9169dcb3a6b6883b8d60843b0b5f765354b232`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
1 cause.

Cause 1 of 1: a Fargate task placed in a subnet with no route to the ECR registry is never blocked from starting, and no ECS task state advances at all, so neither the routed positive-control task's RUNNING state nor the isolated task's terminal STOPPED state is ever reported.

AWS: a Fargate task launched into a subnet with no route to the registry cannot complete the ECR authorization/image pull, so the ECS scheduler terminates it: DescribeTasks eventually reports the task with lastStatus=STOPPED, stopCode=TaskFailedToStart and a stoppedReason describing the pull failure (the CannotPullContainerError class, naming the image reference taken from the task definition in the request). Smithy (com.amazonaws.ecs): Task.stopCode targets the enum TaskStopCode whose values are TaskFailedToStart, EssentialContainerExited, UserInitiated, ServiceSchedulerInitiated, SpotInterruption, TerminationNotice, InfrastructureHealth; Task.stoppedReason is "The reason that the task was stopped"; Task.startedAt is documented as the time the task transitioned PENDING->RUNNING, so a task that never started carries no startedAt. For an awsvpc task, Task.attachments carries exactly one ElasticNetworkInterface attachment whose details include the subnetId it was placed in, and the owning service's runningCount stays 0. A routed task in the same VPC with a public image, by contrast, reaches lastStatus=RUNNING. The fixed parts of this contract are the status/stopCode/stoppedReason classes and the attachment shape; the subnet ids, task ARNs, family names and timestamps are values from the request and from state, never fixed text.

Emulator: nothing transitions. The check polls DescribeTasks/ListTasks for the control task to reach RUNNING or STOPPED, then for the isolated service to record any STOPPED task after UpdateService raises desiredCount to 1, and the whole check ends with `Error: Check timed out after 60.0s` — no task ever reaches RUNNING, no task ever reaches STOPPED, so stopCode/stoppedReason/startedAt/attachments can never be inspected and network isolation has no effect on task placement at all.

Why: image-pull failure caused by networking is the most common real ECS failure. A client that scales a service and then reads lastStatus/stopCode/stoppedReason to decide whether a rollout is healthy hangs forever against the emulator instead of either seeing a RUNNING task or diagnosing TaskFailedToStart, and a deployment validator cannot tell a correctly routed subnet from an unroutable one.

Fix: the ECS task lifecycle/runtime in the ecs service state machine — the step that advances a task from PROVISIONING/PENDING to RUNNING, and the image-pull step that must consult the task's awsvpc network placement (subnet routing) and the ECR repository's image index and then record lastStatus=STOPPED with stopCode=TaskFailedToStart, a stoppedReason of the CannotPullContainerError class, no startedAt, one ElasticNetworkInterface attachment carrying the placed subnetId, and runningCount 0 on the owning service.

Note: the open issue 20261104T062603Z-ec2-ecr-ecs-a-task-in-an-isolated.md describes a similar isolated-subnet/unpullable-tag symptom, and 20261105T202641Z-ec2-ecr-ecs-no-ecs-task-state-ever.md describes task state never advancing; the replay report did not mark this check as owned by either, so it is filed here and those issues are named for the fixer to deduplicate against.

Traces: ec2__ecr__ecs.ecsdeploy-isolated-subnet-task-blocked-from-starting.

## Evidence
Failing check ec2__ecr__ecs.ecsdeploy-isolated-subnet-task-blocked-from-starting, verdict mismatch, detail: `Error: Check timed out after 60.0s`.

Expected (AWS): (a) the routed control task launched by RunTask into a public subnet with assignPublicIp=ENABLED reaches lastStatus=RUNNING; (b) after UpdateService raises desiredCount to 1 on the isolated service, ListTasks(desiredStatus=STOPPED) returns a task and DescribeTasks reports lastStatus=STOPPED, stopCode=TaskFailedToStart, taskDefinitionArn equal to the service's task definition, no startedAt, exactly one ElasticNetworkInterface attachment whose details.subnetId is one of the service's subnets, both container entries STOPPED with no exitCode, and DescribeServices reporting runningCount=0.

Actual (emulator): no polled condition is ever satisfied within the budget — no task reaches RUNNING, and the isolated service records no STOPPED task, so none of stopCode, stoppedReason, startedAt or the ENI attachment can be read.

## Steps to Reproduce
1. Create a VPC with two isolated subnets (no route to the registry/no NAT) and one public subnet with an internet gateway, plus a security group.
2. CreateRepository in ECR, register a Fargate awsvpc task definition with a 'service' and a 'log-router' container referencing that repository's image, and create an ECS service on the two isolated subnets with desiredCount 0.
3. RunTask (FARGATE, public subnet, assignPublicIp=ENABLED) with a task definition using a public image -> poll DescribeTasks: it never reaches lastStatus=RUNNING.
4. UpdateService(desiredCount=1) on the isolated service -> poll ListTasks(serviceName, desiredStatus=STOPPED): no stopped task is ever recorded, so DescribeTasks never reports stopCode=TaskFailedToStart. Both polls exhaust the budget.
