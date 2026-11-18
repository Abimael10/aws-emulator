Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503
Issue-Key: ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503|run#13|AssertionError|assert task["lastStatus"] == "RUNNING", (
Issue-Commit: 3b95f364a9766d54572805b5303a28b612a84a5b

# ecs__elbv2__logs: the ALB's only app task never starts — DescribeTasks reports stopCode EssentialContainerExited immediately after RunTask

> _Note: this issue was identified against the code base at commit `3b95f364a9766d54572805b5303a28b612a84a5b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: the single app task behind the ALB is stopped as an exited essential container instead of reaching RUNNING, so none of the ALB target-health behaviour (fail-open 200 on an unhealthy sole target, 503 on an empty target group) can be exercised.
AWS: a Fargate task whose essential container runs an HTTP server reaches Task.lastStatus 'RUNNING' and is then registered as a target; Task.stopCode (Smithy enum TaskStopCode) / Task.stoppedReason remain unset while the server runs. 'EssentialContainerExited' + 'Essential container in task exited' is AWS's fixed text for the case where the container process itself exited.
Emulator: DescribeTasks reports lastStatus 'STOPPED' with stopCode 'EssentialContainerExited' and stoppedReason 'Essential container in task exited' right after RunTask, so the ALB never gets a target to classify.
Why: a client cannot observe ALB/ECS integration at all — target health, fail-open behaviour and the empty-group 503 all depend on a task that runs, and the reported stop code wrongly blames the customer's container.
Fix: the ECS task lifecycle/data-plane simulation behind RunTask and DescribeTasks (container execution and Task.stopCode/stoppedReason) in the ecs service crate.
Traces: ecs__elbv2__logs.alb-unhealthy-only-target-fails-open-empty-group-returns-503.

## Evidence
Expected: Task.lastStatus == 'RUNNING' for the ALB's sole app task.
Actual: AssertionError: the app task never started: stopCode=EssentialContainerExited reason=Essential container in task exited. Fixed text: stopCode 'EssentialContainerExited', stoppedReason 'Essential container in task exited'; the ARNs are per-run values.

## Steps to Reproduce
Create a VPC with two public subnets, an ALB security group and a task security group, a log group and an execution role; CreateCluster; RegisterTaskDefinition (FARGATE/awsvpc) with an essential HTTP-server container and awslogs; RunTask; poll DescribeTasks for RUNNING. Observe STOPPED with stopCode EssentialContainerExited instead.
