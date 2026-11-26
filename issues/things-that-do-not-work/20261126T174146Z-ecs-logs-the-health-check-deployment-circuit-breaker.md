Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back
Issue-Key: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back|<lambda>#4|Timeout|_swallow(lambda: logs.delete_log_group(logGroupName=name))
Issue-Commit: 5183cad56763ee54d3f0a63f14aa900b48a924f9

# ecs__logs: the health-check deployment-circuit-breaker rollback scenario still reaches no graded step — a revision-1 Fargate service never converges to a RUNNING, HEALTHY task with a COMPLETED PRIMARY deployment

> _Note: this issue was identified against the code base at commit `5183cad56763ee54d3f0a63f14aa900b48a924f9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateService

## Description
Cause 1 of 1: the ECS deployment circuit-breaker rollback scenario produces no observable step at all. A service created on a healthy task-definition revision never reaches the state the scenario's first stage needs — one task with lastStatus RUNNING and containers'/task healthStatus HEALTHY, and a PRIMARY deployment whose rolloutState is COMPLETED — so the replay is cut at the 60 s harness cap before any assertion is graded, and the revision-2 retry/failure and the rollback onto revision 1 are never exercised. Failing check: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back.

AWS: Smithy (com.amazonaws.ecs) — Deployment.rolloutState targets DeploymentRolloutState [COMPLETED, FAILED, IN_PROGRESS]; a deployment is IN_PROGRESS while converging, COMPLETED when the service reaches steady state, and FAILED when it cannot and the circuit breaker is enabled. Deployment.failedTasks is the number of consecutively failed tasks, where a task counts as failed if it cannot be launched, never reaches RUNNING, or fails one of its defined health checks. HealthCheck {command, interval, timeout, retries, startPeriod} is documented so that a non-zero exit code is a failure and `retries` consecutive failures make the container unhealthy; Container.healthStatus takes [HEALTHY, UNHEALTHY, UNKNOWN]. DeploymentCircuitBreaker.enable and .rollback are required booleans and rollback is documented as returning the service to the last completed deployment on failure (https://docs.aws.amazon.com/AmazonECS/latest/developerguide/deployment-circuit-breaker.html). On AWS the whole sequence is observable: revision 1 converges to one RUNNING/HEALTHY task with a PRIMARY COMPLETED deployment; UpdateService onto the revision whose probe always exits non-zero attempts at least two tasks, at least one of which reaches RUNNING and then reports healthStatus UNHEALTHY (or stops with a stoppedReason naming the container health check), the deployment reports failedTasks >= 1, and the service is then rolled back — the PRIMARY deployment becomes the revision-1 task definition with rolloutState COMPLETED, the service's own taskDefinition moves back, desiredCount is unchanged, and exactly one RUNNING/HEALTHY revision-1 task survives.

Emulator: nothing in that sequence is reached. The replay ends with 'Check timed out after 60.0s' and no assertion graded — the cap expires during the very first stage even though the check's own stage budgets are far larger. The open issue 20261126T114503Z-ecs-logs-the-health-check-circuit-breaker-rollback.md describes this same scenario reaching no verdict, and 20261120T145635Z-ecs-logs-a-failed-ecs-service-deployment-is-never-rolled.md records the same check failing at its rollback poll; neither carries this replay's key, so this is filed separately and both are named here rather than duplicated. Neighbouring open issues explain why no Fargate task converges (for example 20261126T031516Z-ecs-a-fargate-service-with-desiredcount-0-never.md and 20261125T221450Z-ecs-the-task-data-plane-never-produces-a.md) but none carries this check.

Why: a client that depends on the deployment circuit breaker to protect a service observes nothing it needs — not a converged good deployment, not a FAILED one, and not a rollback. `aws ecs wait services-stable` and any deploy pipeline that polls DescribeServices for a PRIMARY deployment with rolloutState COMPLETED blocks indefinitely, and the health-check-driven rollback AWS performs with no operator action cannot be verified at all.

Fix: the ECS service scheduler and its deployment state machine — the state behind CreateService / UpdateService / DescribeServices / DescribeTasks that takes a Fargate service from desiredCount to a RUNNING task with an evaluated container health check, drives rolloutState and failedTasks, and on a FAILED deployment with circuit-breaker rollback enabled creates the restoring deployment and brings it to PRIMARY/COMPLETED. This is service/task state, not the request parsing of any one operation.

Traces: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator, service heading `ecs__logs`.

[mismatch] ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back
  Error: Check timed out after 60.0s — no assertion reached.

Expected (AWS), in order:
  1. After CreateService (FARGATE, desiredCount 1, awsvpcConfiguration, revision 1, deploymentCircuitBreaker {enable: true, rollback: true}), DescribeTasks/ListTasks show one task with lastStatus RUNNING and healthStatus HEALTHY, and DescribeServices shows a PRIMARY deployment on revision 1 with rolloutState COMPLETED; the container's own awslogs stream contains its revision-1 marker line.
  2. After UpdateService onto revision 2 (identical except its HEALTHCHECK always exits non-zero), at least two revision-2 tasks are attempted, at least one reaches RUNNING and prints its revision-2 marker, the deployment reports failedTasks >= 1, and the failure is attributed to the probe (a task seen in healthStatus UNHEALTHY or a stoppedReason naming the container health check).
  3. The PRIMARY deployment then becomes the revision-1 task definition with rolloutState COMPLETED, the service's taskDefinition moves back to revision 1, desiredCount stays 1, one RUNNING/HEALTHY revision-1 task survives, and no revision-2 task is left RUNNING.

Actual (emulator): stage 1 never completes; the check is cut at the 60 s cap with no graded assertion, so neither the deployment rolloutState/failedTasks reporting nor the circuit-breaker rollback is observable.

Cluster, family, service and task ARNs and the per-run marker string are per-run values; the invariant is that the scenario produces an observable deployment/health state at all.

Smithy consulted: com.amazonaws.ecs Deployment.rolloutState (DeploymentRolloutState [COMPLETED, FAILED, IN_PROGRESS]), Deployment.failedTasks, DeploymentCircuitBreaker {enable, rollback}, HealthCheck {command, interval, timeout, retries, startPeriod}, HealthStatus [HEALTHY, UNHEALTHY, UNKNOWN], Container.healthStatus.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; the id in trace_ids reproduces.
2. Create a subnet + security group, a CloudWatch Logs log group and an ECS task-execution role; create a cluster.
3. Register two revisions of one task-definition family, byte-identical except for the environment value their container HEALTHCHECK exits with: revision 1 exits 0, revision 2 exits 1. Both containers echo a revision marker line to stdout through the awslogs driver into the log group.
4. CreateService with launchType FARGATE, desiredCount 1, awsvpcConfiguration, revision 1 and deploymentConfiguration.deploymentCircuitBreaker {enable: true, rollback: true}.
5. Poll DescribeTasks/ListTasks for one task with lastStatus RUNNING and healthStatus HEALTHY and DescribeServices for a PRIMARY deployment with rolloutState COMPLETED. Expected: both arrive. Actual: neither does, and the replay is cut at the 60 s cap.
6. (Unreached) UpdateService to revision 2; expect >= 2 attempted tasks, >= 1 reaching RUNNING, failedTasks >= 1, the failure attributed to the health probe, and then a PRIMARY deployment back on revision 1 with rolloutState COMPLETED, the service's taskDefinition restored and desiredCount unchanged.
7. Re-replay the id in trace_ids to confirm the fix.
