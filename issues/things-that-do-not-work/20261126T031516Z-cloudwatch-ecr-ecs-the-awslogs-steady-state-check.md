Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecr__ecs.ecs-service-stabilizes-and-logs-to-cloudwatch
Issue-Key: cloudwatch__ecr__ecs.ecs-service-stabilizes-and-logs-to-cloudwatch|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Commit: 01f237389f2ba129bcfe6809f1b810fd15e9edea

# cloudwatch__ecr__ecs: the awslogs steady-state check reaches no verdict — a desiredCount=1 Fargate service never gets a RUNNING task, so no container stdout can be delivered to CloudWatch Logs

> _Note: this issue was identified against the code base at commit `01f237389f2ba129bcfe6809f1b810fd15e9edea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
1 net-new cause, failing one check of this service group that no open issue's replay keys carry.

Cause 1 of 1: a FARGATE service (desiredCount=1) whose single essential container is `sh -c "echo <marker>; sleep 3600"` with an awslogs logConfiguration never reports a task in lastStatus RUNNING and never reports runningCount == desiredCount, so the check is killed at the harness 60 s ceiling before the awslogs delivery assertion is reached.

AWS: Smithy: ecs Service.runningCount — 'The number of tasks in the cluster that are in the RUNNING state'; Service.desiredCount — 'The desired number of instantiations of the task definition to keep running on the service'. On AWS the service scheduler places the task, it reaches RUNNING, DescribeServices reports runningCount == desiredCount == 1, and the container's stdout line is delivered by the awslogs driver into the configured log group under the awslogs-stream-prefix stream. The requirement is the convergence and the subsequent delivery, not any single run's timing.

Emulator: 'Check timed out after 60.0s'. The check polls ListTasks(cluster, serviceName, desiredStatus=RUNNING) + DescribeTasks for a task with lastStatus RUNNING (with a 60 s grace window that tolerates an early STOPPED task as IAM propagation) and never gets one, so GetLogEvents is never reached. CreateService itself answers 200 with status ACTIVE and desiredCount 1 — no status or error-code divergence, the service simply never converges.

Why: a client cannot observe either half of the contract this check covers: that an ECS service reaches a steady state it reports truthfully, and that a container's stdout lands in the CloudWatch Logs group the task definition names. Both are the normal way users verify an ECS deployment, and both are unverified today.

Fix: the ECS service scheduler / task lifecycle component (place desiredCount tasks, advance them to RUNNING, keep runningCount in step) and, behind it, the awslogs log-driver delivery path for a running container. Related open issues in this group describe neighbouring causes but are not marked as owning this check: 20261125T220211Z-cloudwatch-ecr-ecs-fargate-tasks-with-a-long-lived-container-command.md (tasks stop immediately with 'Essential container in task exited') and 20261111T131404Z-cloudwatch-ecr-ecs-a-fargate-container-s-image.md.

Traces: cloudwatch__ecr__ecs.ecs-service-stabilizes-and-logs-to-cloudwatch.

## Evidence
Expected (AWS): the Fargate service with desiredCount=1 gets one task in lastStatus RUNNING, DescribeServices reports runningCount == desiredCount == 1, and the marker line the container echoes appears as an event in the configured awslogs group/stream.

Actual (emulator): check cloudwatch__ecr__ecs.ecs-service-stabilizes-and-logs-to-cloudwatch fails with 'Error: Check timed out after 60.0s' while polling for a RUNNING task; no log-group assertion is ever evaluated. Log-group name, cluster/service names, the generated marker and the harness port differ per run and are not the requirement.

## Steps to Reproduce
1. Create a CloudWatch Logs group and an ECS task execution role, then CreateCluster.
2. RegisterTaskDefinition(requiresCompatibilities=[FARGATE], networkMode=awsvpc, cpu=256, memory=512, executionRoleArn=<role>) with one essential container image public.ecr.aws/docker/library/busybox:latest, command `sh -c "echo container-log-marker; sleep 3600"` and logConfiguration awslogs -> the log group with awslogs-stream-prefix=ecs.
3. CreateService(desiredCount=1, launchType=FARGATE, awsvpc networkConfiguration): answers 200, status ACTIVE.
4. Poll ListTasks(desiredStatus=RUNNING)+DescribeTasks: no task reaches lastStatus RUNNING; DescribeServices never reports runningCount 1.
5. Re-replay cloudwatch__ecr__ecs.ecs-service-stabilizes-and-logs-to-cloudwatch to confirm the fix.
