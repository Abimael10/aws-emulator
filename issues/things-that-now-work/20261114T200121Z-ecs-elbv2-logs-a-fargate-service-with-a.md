Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target
Issue-Key: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target|_poll_until#17|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 6b20299dda36b610e0eb6266197358af63fda05b

# ecs__elbv2__logs: a Fargate service with a container HEALTHCHECK never reaches one RUNNING task, so container-health-driven task replacement and the ALB target swap are never observable

> _Note: this issue was identified against the code base at commit `6b20299dda36b610e0eb6266197358af63fda05b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateService

## Description
Cause 1 of 1: an ALB-attached Fargate service whose single essential container declares a healthCheck never reports one RUNNING task, so the whole container-health → task-replacement → target-swap scenario is cut off at its first gate.
AWS:      recorded behavior only for the end-to-end flow. After ecs CreateService(launchType FARGATE, desiredCount 1, awsvpc in two subnets, loadBalancers wired to an ip target group, healthCheckGracePeriodSeconds 10, deploymentConfiguration {minimumHealthyPercent 0, maximumPercent 200}) with a container declaring healthCheck {command ['CMD-SHELL','test -f /tmp/healthy'], interval 5, timeout 2, retries 2, startPeriod 30}, ListTasks/DescribeTasks report exactly one task at lastStatus RUNNING with an awsvpc privateIPv4Address; elbv2 DescribeTargetHealth then reports that IP 'healthy'; DescribeTasks reports the container's healthStatus 'HEALTHY'; after the probe is made to fail the container goes UNHEALTHY, the service scheduler stops that task and starts a replacement, the old IP leaves the target group and the new IP becomes 'healthy', and HTTP through the ALB answers 200 before and after (ECS container health-check documentation: https://docs.aws.amazon.com/AmazonECS/latest/developerguide/healthcheck.html).
Emulator: the check never gets past its first wait. `_poll_until(... 'service <name> has 1 RUNNING task')` exhausts the run budget and raises TimeoutError: "service <name> has 1 RUNNING task: the run budget ended first; not graded". No task ever reaches RUNNING for the service, so no container healthStatus, no target registration, no replacement and no ALB response is ever produced.
Why:      a client cannot deploy a health-checked container behind an ALB at all, and cannot rely on the one self-healing behaviour ECS promises — that a container failing its own HEALTHCHECK is replaced and the load balancer follows the swap. The service reports created and then stalls silently, so the client's deploy wait hangs instead of failing with a reason.
Fix:      the ECS service scheduler/task lifecycle that must bring a Fargate task to lastStatus RUNNING with an awsvpc ENI, the container health-check evaluator that must report healthStatus HEALTHY/UNHEALTHY from the task definition's healthCheck, and the scheduler's replacement path that must then deregister the old target and register the replacement with ELBv2.
Nearest open issues, none of which carries this check's replay key: 20261107T222655Z-ecs-elbv2-logs-an-alb-attached-awsvpc-fargate.md, 20261108T200250Z-ecs-elbv2-logs-an-alb-fronted-awsvpc-fargate.md and 20261110T220525Z-ecs-elbv2-logs-2-net-new-causes-the.md.
Traces:   ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target.

## Evidence
#### [error] ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target
  File "...line 106, in run
    tasks = _poll_until(
  File "...line 351, in _poll_until
    raise TimeoutError(f"{description}: the run budget ended first; not graded")
  TimeoutError: service sim2real-xsvc-ecs-svc-<id> has 1 RUNNING task: the run budget ended first; not graded

Expected: ListTasks/DescribeTasks report one task at lastStatus RUNNING with an awsvpc privateIPv4Address, then that IP 'healthy' in the target group and the container healthStatus 'HEALTHY'; after the probe file is removed the task is replaced and the target set swaps. Actual: the service never reports a single RUNNING task, so nothing after it was graded. (The service name is per-run and is not part of the requirement.)

## Steps to Reproduce
1. In a public VPC with two subnets create an ALB security group (tcp/80 from the client CIDRs) and a task security group (tcp/80 from the ALB security group); create a CloudWatch log group and an ECS task execution role with logs:CreateLogStream/logs:PutLogEvents.
2. ecs CreateCluster; RegisterTaskDefinition with one essential container on containerPort 80, a command that serves HTTP and can be broken, awslogs logConfiguration, and healthCheck {command ['CMD-SHELL','test -f /tmp/healthy'], interval 5, timeout 2, retries 2, startPeriod 30}.
3. elbv2 CreateTargetGroup (TargetType ip) + CreateLoadBalancer + CreateListener forwarding to it.
4. ecs CreateService (FARGATE, desiredCount 1, the two subnets and the task security group, loadBalancers to the group, healthCheckGracePeriodSeconds 10, deploymentConfiguration minimumHealthyPercent 0 / maximumPercent 200).
5. Poll ListTasks/DescribeTasks for one task at lastStatus RUNNING.
6. Observe the poll never satisfies — the service never has one RUNNING task — so the container-health failure, the task replacement and the ALB target swap are never exercised.
