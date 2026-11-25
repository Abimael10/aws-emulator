Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: cloudwatch__ecr__ecs__logs.deploy-ecs-task-execution-role-trust-policy-from-template
Issue-Key: cloudwatch__ecr__ecs__logs.deploy-ecs-task-execution-role-trust-policy-from-template|run#25|AssertionError|assert task["lastStatus"] == "RUNNING", (
Issue-Commit: 76a27d9bdd16e996b43ee8be0e1161c3fa717030

# cloudwatch__ecr__ecs__logs: a Fargate task whose essential container runs `sleep 60` is stopped immediately as "Essential container in task exited", so the CFN-deployed execution role is never exercised

> _Note: this issue was identified against the code base at commit `76a27d9bdd16e996b43ee8be0e1161c3fa717030`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: a placed Fargate task never reaches lastStatus RUNNING — it is settled as STOPPED with stoppedReason "Essential container in task exited" within a second of being created, although its only essential container's command is a long-lived `sh -c "sleep 60"`.

AWS: Smithy ecs#RunTask returns Task records whose `lastStatus` is documented as "The last known status for the task" (see the Task Lifecycle doc it links). A FARGATE/awsvpc task with cpu 256 / memory 512 and one essential container (public.ecr.aws/docker/library/busybox:1.36, command ["sh","-c","sleep 60"], awslogs logConfiguration) placed in a public subnet with a working execution role walks PROVISIONING -> PENDING -> RUNNING and REMAINS lastStatus RUNNING for as long as the container's command runs; `stoppedReason`/`stopCode` are absent while it runs. The sentence "Essential container in task exited" (with stopCode EssentialContainerExited) is AWS's fixed text for a task whose essential container has ACTUALLY terminated — it interpolates nothing from the request. Recorded behavior: DescribeTasks reports lastStatus RUNNING well inside the check's 180 s window.

Emulator: DescribeTasks reports lastStatus='STOPPED' with stoppedReason='Essential container in task exited' and the container carrying exitCode 0 essentially at creation time — the container's command is never actually run for its duration, so the task is reported as having exited before it could run. Nothing about the request is wrong: the stack, the role, its AmazonECSTaskExecutionRolePolicy attachment and the trust statement all read back exactly as AWS, and RunTask itself is accepted.

Why: a client cannot observe ANY running workload. The only real proof that a deployed task-execution role works is that ECS assumed it and the task reached RUNNING; with every task settling as exited immediately, deploy-then-run verification, service steady state, awslogs delivery and any in-guest behavior are all unreachable, and a caller polling for RUNNING instead sees a terminal failure with a stop reason AWS only uses for genuine container exits.

Fix: the ECS task-lifecycle component behind RunTask — the part that advances a placed task's lastStatus and actually runs the container definition's command (and only then records exitCode/stoppedReason/stopCode). Related open issues describe the same shape for other service groups and do NOT carry this check: 20261118T145001Z-ecs-2-net-new-causes-a-long-running.md, 20261118T134608Z-ec2-ecs-elbv2-a-fargate-task-whose-subnet.md and 20261120T071621Z-cloudwatch-ec2-ecs-2-net-new-causes-an.md.

Traces: cloudwatch__ecr__ecs__logs.deploy-ecs-task-execution-role-trust-policy-from-template.

## Evidence
Expected (AWS): after RegisterTaskDefinition(family=<f>, networkMode=awsvpc, requiresCompatibilities=[FARGATE], cpu=256, memory=512, executionRoleArn=<CFN role>, containerDefinitions=[{name:app, image:public.ecr.aws/docker/library/busybox:1.36, essential:true, command:["sh","-c","sleep 60"], logConfiguration:awslogs}]) and RunTask in a public subnet, DescribeTasks reports lastStatus 'RUNNING' with no stoppedReason.

Actual (emulator): AssertionError: ECS could not actually use the CFN-deployed execution role: expected RUNNING, got lastStatus='STOPPED' stoppedReason='Essential container in task exited'. All earlier assertions passed (role ARN, single Allow sts:AssumeRole statement for exactly ecs-tasks.amazonaws.com, AmazonECSTaskExecutionRolePolicy attached). The stack/cluster/task ids and the log-group name are per-run values; the divergence is the terminal STOPPED and the stop reason.

## Steps to Reproduce
1. CreateStack with a template that defines an IAM role trusting ecs-tasks.amazonaws.com with AmazonECSTaskExecutionRolePolicy attached, and output its name + ARN.
2. Create a VPC with one public subnet behind an internet gateway and a default-egress security group; create an ECS cluster and a CloudWatch Logs log group.
3. RegisterTaskDefinition as above (FARGATE, awsvpc, executionRoleArn = the deployed role, container command ["sh","-c","sleep 60"], awslogs driver to that log group).
4. RunTask(launchType=FARGATE, count=1, awsvpcConfiguration with that subnet/SG, assignPublicIp=ENABLED).
5. Poll DescribeTasks for up to 180 s until lastStatus is RUNNING or STOPPED. Expected RUNNING; observed STOPPED with stoppedReason 'Essential container in task exited' almost immediately.
