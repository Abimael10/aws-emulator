Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back
Issue-Key: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back|_poll_until#4|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: f422e37ca967a244b72f2f746d1d1ab79e5db12d

# A failed ECS service deployment is never rolled back: the PRIMARY deployment never returns to the previous revision with rolloutState COMPLETED

> _Note: this issue was identified against the code base at commit `f422e37ca967a244b72f2f746d1d1ab79e5db12d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateService

## Description
Cause 1 of 1: after UpdateService moves a service to a revision whose container starts but never passes its container health check, the deployment's failure is observed but the service never rolls back — DescribeServices' PRIMARY deployment never becomes the previous (revision-1) task definition with rolloutState COMPLETED, so the service is left with no converged deployment.

AWS: recorded behavior only for the rollback timing; the mechanism is AWS's documented deployment circuit breaker (https://docs.aws.amazon.com/AmazonECS/latest/developerguide/deployment-circuit-breaker.html). With deploymentCircuitBreaker enable+rollback, a deployment whose tasks repeatedly fail their container health check is marked FAILED and ECS then starts a new deployment that restores the last known-good task definition; DescribeServices reports the PRIMARY deployment with taskDefinition = the previous revision and rolloutState = COMPLETED (rolloutStateReason naming the rollback), and the surviving task is the revision-1 task.

Emulator: the revision-2 deployment is retried and does fail (failedTasks >= 1, tasks observed RUNNING then UNHEALTHY / stopped on the health check), but the follow-up rollback deployment never appears: polling DescribeServices for a PRIMARY deployment whose taskDefinition is revision 1 and whose rolloutState is COMPLETED exhausts the budget with no such deployment.

Why: a client relying on the circuit breaker to protect a service sees a bad rollout stick — the service never returns to the last good revision and never reports a converged PRIMARY deployment, so automated deploys cannot detect 'rolled back, safe' and a rollout that AWS would self-heal needs manual intervention.

Fix: the ECS service deployment state machine — the circuit-breaker path that, on a FAILED deployment with rollback enabled, must create the restoring deployment and drive it to PRIMARY/COMPLETED; belongs with the service/deployment state behind UpdateService and DescribeServices. Related open issues cover neighbouring deployment-state defects but not this check: 20261116T130854Z-ecs-tasks-and-deployments-never-reach-a-failure.md.

Traces: ecs__logs.svcreplace-deployment-whose-container-never-passes-its-healthcheck-is-rejected-and-rolled-back.

## Evidence
Expected: once the revision-2 deployment fails its health probes, DescribeServices reports a PRIMARY deployment with taskDefinition == the revision-1 task definition ARN and rolloutState == 'COMPLETED', and the revision-1 container is the one still serving.
Actual (replay): TimeoutError: the PRIMARY deployment is revision 1 again, COMPLETED: the run budget ended first; not graded — the poll on _primary(...) never saw a revision-1 PRIMARY deployment in rolloutState COMPLETED. Cluster/service/task-definition names and revisions are per-run values; the invariant is that no rollback deployment is ever created.

## Steps to Reproduce
1. Create a cluster, log group and execution role; register two task definitions in one family that differ only in their container HEALTHCHECK outcome (revision 1 passes, revision 2 always fails).
2. CreateService on revision 1 with deploymentCircuitBreaker enable=true, rollback=true; wait for one RUNNING/HEALTHY task and a PRIMARY deployment with rolloutState COMPLETED.
3. UpdateService to revision 2. Observe the revision-2 tasks start, report UNHEALTHY and be retried, and the deployment report failedTasks >= 1.
4. Poll DescribeServices for the PRIMARY deployment: on AWS it becomes revision 1 again with rolloutState COMPLETED; here no such deployment ever appears.
