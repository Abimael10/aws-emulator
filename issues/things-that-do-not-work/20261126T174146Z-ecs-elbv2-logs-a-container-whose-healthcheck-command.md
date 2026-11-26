Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target
Issue-Key: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target|_poll_until#28|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: 5183cad56763ee54d3f0a63f14aa900b48a924f9

# ecs__elbv2__logs: a container whose HEALTHCHECK command starts failing never reports healthStatus UNHEALTHY, so the service never replaces the task and the ALB never drops its target

> _Note: this issue was identified against the code base at commit `5183cad56763ee54d3f0a63f14aa900b48a924f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
Cause 1 of 1: an ECS container health check is not re-evaluated once it has passed. After the probe command is made to start failing, DescribeTasks keeps reporting the container's healthStatus as HEALTHY and the task is never stopped, so none of the chain that hangs off it happens: no replacement task, no deregistration of the stopped task's address from the attached target group, no new healthy target. Failing check: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target.

AWS: Smithy (com.amazonaws.ecs) — RegisterTaskDefinition containerDefinitions[].healthCheck {command (required), interval, timeout, retries, startPeriod}; the shape's documentation states an exit code of 0 indicates success and a non-zero exit code indicates failure, that `retries` is "the number of times to retry a failed health check before the container is considered unhealthy", and that after startPeriod "any subsequent failures count toward the maximum number of retries". DescribeTasks output tasks[].containers[].healthStatus targets HealthStatus with values [HEALTHY, UNHEALTHY, UNKNOWN], documented HEALTHY = "the container health check has passed successfully", UNHEALTHY = "the container health check has failed". The health check is therefore periodic for the life of the container: once the command begins exiting non-zero, healthStatus must flip to UNHEALTHY after `retries` consecutive failures, the service scheduler must stop and replace the task, the stopped task's ip must leave DescribeTargetHealth for the target group, and the replacement's ip must reach TargetHealth.State 'healthy' (https://docs.aws.amazon.com/AmazonECS/latest/developerguide/healthcheck.html).

Emulator: the probe is evaluated to HEALTHY once and then never again. In this replay the earlier stages all completed — the task reached RUNNING, its ip reached 'healthy' in the target group, containers[].healthStatus reached HEALTHY, HTTP through the load balancer answered 200 with the container's own body, and the container's /cgi-bin/break script ran and confirmed it had removed the sentinel file the healthCheck command tests for (`<marker>-BREAK-OK` in the response body). With the sentinel gone, `CMD-SHELL test -f /tmp/healthy` can only exit non-zero, yet polling DescribeTasks for healthStatus in (UNHEALTHY, GONE) — interval 5 s, retries 2, i.e. UNHEALTHY expected within ~15 s — never observed anything but HEALTHY, and the task was never stopped; the poll ran until the replay budget ended (TimeoutError: "container healthStatus UNHEALTHY on <taskArn>: the run budget ended first; not graded").

This is a NEW failure point, past the one the open issue 20261124T120000Z-ecs-elbv2-logs-a-container-s-cgi-endpoint-is-never-executed.md records: that issue's symptom is the /cgi-bin script never being executed (404), and in this replay it IS executed and confirms the sentinel removal. That issue does not carry this failure and is named here rather than duplicated; the related open issue 20261120T095328Z-ecs-elbv2-logs-an-alb-fronted-fargate-container.md (the container never answering a non-root path) is likewise past.

Why: a client relying on ECS container health checks to self-heal a service gets a permanently wedged estate. A container that has stopped serving correctly stays RUNNING, stays HEALTHY in DescribeTasks and stays a registered, 'healthy' ALB target, so traffic keeps being routed to it; deploy and recovery tooling that waits for UNHEALTHY, for a replacement taskArn, or for the old target to leave the target group waits forever. The whole point of declaring healthCheck in a task definition — detection of a container that degrades after startup — is unobservable.

Fix: the ECS container-health evaluation loop in the task/container state (the component that runs a container's healthCheck command on its `interval` for the life of the container, counts `retries` consecutive non-zero exits, publishes containers[].healthStatus for DescribeTasks, and hands an UNHEALTHY essential container to the service scheduler to stop and replace) — not the request parsing of RegisterTaskDefinition or DescribeTasks, and not the elbv2 target-health reader, which only follows from the task being stopped.

Traces: ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator, service heading `ecs__elbv2__logs`.

[error] ecs__elbv2__logs.ecs-container-health-check-failure-makes-the-service-replace-the-task-and-the-alb-drop-the-old-target
  TimeoutError: container healthStatus UNHEALTHY on arn:aws:ecs:<region>:<account>:task/<cluster>/<taskId>: the run budget ended first; not graded

Stages that DID complete before it (so the divergence is isolated to the health re-evaluation):
  - CreateService (FARGATE, desiredCount 1, loadBalancers -> ip target group, healthCheckGracePeriodSeconds 10, minimumHealthyPercent 0 / maximumPercent 200) produced one RUNNING task with an awsvpc private ip.
  - DescribeTargetHealth reported that ip 'healthy'.
  - DescribeTasks reported containers[].healthStatus HEALTHY for the task.
  - GET / through the ALB answered 200 containing the container's own marker body.
  - GET /cgi-bin/break through the ALB answered 200 containing '<marker>-BREAK-OK', i.e. the container's script ran and deleted /tmp/healthy, the file the healthCheck command `["CMD-SHELL", "test -f /tmp/healthy"]` tests.

Expected (AWS): with interval 5, timeout 2, retries 2 and startPeriod 30 already elapsed, containers[].healthStatus becomes UNHEALTHY within ~15 s of the sentinel's removal; the service stops that task and starts a replacement with a different taskArn; DescribeTargetHealth no longer lists the old ip as healthy and lists the replacement's ip as healthy; HTTP through the ALB answers 200 before and after.

Actual (emulator): healthStatus stays HEALTHY indefinitely, the task is never stopped, and no replacement task is created, so the poll never observes UNHEALTHY or the task's disappearance.

Task/cluster ARNs, the per-run marker, the generated ip and the ALB host:port are per-run values; the invariant is that a failing healthCheck command must drive healthStatus to UNHEALTHY and a service-driven replacement.

Smithy consulted: com.amazonaws.ecs HealthCheck {command, interval, timeout, retries, startPeriod} and its documentation, HealthStatus enum [HEALTHY, UNHEALTHY, UNKNOWN], Container.healthStatus; com.amazonaws.elasticloadbalancingv2 DescribeTargetHealth TargetHealthDescriptions[].TargetHealth.State.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; the id in trace_ids reproduces.
2. Create a VPC with two public subnets, a security group open on 80 to the ALB, a log group and an ECS task-execution role; create a cluster.
3. RegisterTaskDefinition (FARGATE, awsvpc) with one essential container that serves HTTP on 80, writes a sentinel file /tmp/healthy at startup and exposes a /cgi-bin/break script that deletes it, with healthCheck {command: ["CMD-SHELL", "test -f /tmp/healthy"], interval: 5, timeout: 2, retries: 2, startPeriod: 30} and the awslogs driver.
4. Create an ip target group, an ALB and an HTTP listener forwarding to it; CreateService with desiredCount 1 and loadBalancers binding container `app`:80 to the target group.
5. Wait for one RUNNING task, its ip 'healthy' in DescribeTargetHealth, containers[].healthStatus HEALTHY, and HTTP 200 through the ALB.
6. GET /cgi-bin/break through the ALB and confirm the response says the sentinel was removed.
7. Poll DescribeTasks for containers[].healthStatus. Expected: UNHEALTHY within ~15 s, then the task stops and a new taskArn appears. Actual: healthStatus stays HEALTHY and the task is never replaced.
8. Re-replay the id in trace_ids to confirm the fix.
