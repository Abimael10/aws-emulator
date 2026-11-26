Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back
Issue-Key: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back|<lambda>#4|Timeout|_swallow(lambda: ecs.delete_task_definitions(taskDefinitions=[arn]))))
Issue-Commit: 2aaadc19d9353b323ddc8d1ceaf32931386905fe

# ecs__logs: the health-check circuit-breaker rollback scenario now reaches no verdict at all — the service/task estate makes no observable progress inside the replay budget

> _Note: this issue was identified against the code base at commit `2aaadc19d9353b323ddc8d1ceaf32931386905fe`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateService

## Description
Cause 1 of 1: the ECS deployment circuit-breaker rollback scenario (a revision whose container starts but never passes its container health check must be retried, fail, and be rolled back onto the last good revision) produces no graded step at all. The replay is cut at the 60 s cap before any of the scenario's own poll stages can be judged, so neither the revision-1 convergence (one RUNNING task whose healthStatus is HEALTHY and a PRIMARY deployment with rolloutState COMPLETED), nor the revision-2 retry/failure, nor the rollback is observable. Failing check: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back.

AWS: Smithy (com.amazonaws.ecs) — Deployment.rolloutState targets DeploymentRolloutState with values [COMPLETED, FAILED, IN_PROGRESS] and is documented: a deployment starts IN_PROGRESS, becomes COMPLETED when the service reaches a steady state, and becomes FAILED when the service cannot reach steady state and the circuit breaker is on; Deployment.failedTasks is documented as the number of consecutively failed tasks, where a task counts as failed if the scheduler cannot launch it, it never reaches RUNNING, or it fails one of its defined health checks and is stopped. DeploymentCircuitBreaker.enable/rollback are required booleans, and rollback is documented as rolling the service back to the last completed deployment on failure (https://docs.aws.amazon.com/AmazonECS/latest/developerguide/deployment-circuit-breaker.html). On AWS the whole sequence is observable through DescribeServices / DescribeTasks / ListTasks: a service created on the healthy revision converges to one RUNNING, HEALTHY task with a PRIMARY deployment in rolloutState COMPLETED; an UpdateService onto the failing revision launches tasks that reach RUNNING, report healthStatus UNHEALTHY (or stop with a stoppedReason naming the container health check), are retried, and the deployment reports failedTasks >= 1; the service then gets a restoring deployment whose taskDefinition is the previous revision and whose rolloutState becomes COMPLETED, and the service's own taskDefinition moves back while desiredCount is unchanged.

Emulator: nothing in that sequence is reached. The replay ends with 'Check timed out after 60.0s' and no assertion is graded — the harness cap expires before the first stage (a service task that is both RUNNING and HEALTHY) completes, even though the check's own stage budgets are far larger (480 s convergence, 900 s circuit-breaker failure, 480 s rollback). This is a regression in how far the scenario gets: the open issue 20261120T145635Z-ecs-logs-a-failed-ecs-service-deployment-is-never-rolled.md records this same check previously failing at its FOURTH poll (the rollback poll), i.e. the earlier stages did complete then; that issue's replay key is the rollback-poll TimeoutError and does not cover this failure, so this is filed separately and that issue is named here rather than duplicated. Neighbouring open issues describe why no task converges (for example 20261126T031516Z-ecs-a-fargate-service-with-desiredcount-0-never.md and 20261125T221450Z-ecs-the-task-data-plane-never-produces-a.md), but none carries this check.

Why: a client that relies on the deployment circuit breaker to protect a service can observe nothing it needs — not a converged good deployment, not a FAILED one, and not the rollback. Automated deploy tooling that waits for 'services-stable' or for a rolled-back PRIMARY deployment blocks indefinitely, and a health-check-driven rollback, which AWS performs without operator action, cannot be exercised or verified at all.

Fix: the ECS service scheduler and its deployment state machine — the path that takes a Fargate service from desiredCount to a RUNNING task with an evaluated container health check, drives the deployment's rolloutState/failedTasks, and on a FAILED deployment with circuit-breaker rollback enabled creates the restoring deployment and brings it to PRIMARY/COMPLETED; it is the service/task/deployment state behind CreateService, UpdateService, DescribeServices and DescribeTasks, not the request parsing of any one operation.

Traces: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back.

## Evidence
Expected (AWS): after CreateService on the healthy revision, DescribeTasks shows one task with lastStatus RUNNING and healthStatus HEALTHY and DescribeServices shows a PRIMARY deployment with rolloutState 'COMPLETED'; after UpdateService onto the failing revision, at least two revision-2 tasks are attempted, at least one reaches RUNNING, the deployment reports failedTasks >= 1, the failure is attributed to the health probe (healthStatus 'UNHEALTHY' or a stoppedReason naming the container health check), and the PRIMARY deployment then becomes the revision-1 task definition with rolloutState 'COMPLETED' while the service's taskDefinition moves back and desiredCount stays 1.

Actual (replay): the check produces no graded step — 'Error: Check timed out after 60.0s' with no assertion reached, so not even the first stage (a RUNNING + HEALTHY task for the healthy revision) completed. Verdict recorded as mismatch under service heading 'ecs__logs'. Cluster, family, service and task ARNs and the per-run marker string are per-run values; the invariant is that the scenario produces no observable deployment/health state at all.

## Steps to Reproduce
1. Create a VPC subnet + security group, a CloudWatch Logs log group and an ECS task-execution role; create a cluster.
2. Register two task definitions in one family that are byte-identical except for the environment value their container HEALTHCHECK exits with: revision 1 exits 0 (healthy), revision 2 exits 1 (always failing). Both containers echo a revision marker line to stdout and use the awslogs driver into the log group.
3. CreateService with launchType FARGATE, desiredCount 1, awsvpcConfiguration, revision 1, and deploymentConfiguration.deploymentCircuitBreaker {enable: true, rollback: true}.
4. Poll DescribeTasks/ListTasks for one task with lastStatus RUNNING and healthStatus HEALTHY, and DescribeServices for a PRIMARY deployment with rolloutState COMPLETED — on the emulator this never arrives and the replay is cut at the 60 s cap.
5. (Unreached) UpdateService to revision 2, observe retried RUNNING-then-UNHEALTHY tasks and failedTasks >= 1, then poll DescribeServices for a PRIMARY deployment on revision 1 with rolloutState COMPLETED and one surviving RUNNING/HEALTHY revision-1 task.
