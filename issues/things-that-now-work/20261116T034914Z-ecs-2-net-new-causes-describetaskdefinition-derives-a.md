Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults ecs.ephemeral-storage-request-is-provisioned-inside-the-container
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults|run#18|AssertionError|assert sorted(got["compatibilities"]) == ["EC2", "FARGATE"], (
Issue-Key: ecs.ephemeral-storage-request-is-provisioned-inside-the-container|_measured_gib#24|AssertionError|assert isinstance(code, int) and 0 < code < _DF_FAILED, (
Issue-Commit: 3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1

# ecs: 2 net-new causes — DescribeTaskDefinition derives a bogus MANAGED_INSTANCES compatibility, and a stopped task's container carries no exitCode so task-level ephemeralStorage is never provisioned

> _Note: this issue was identified against the code base at commit `3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTaskDefinition

## Description
Two net-new root causes in ECS. Neither failing check was marked ALREADY FILED or OWNED by the replay report.

Cause 1 of 2: the `compatibilities` list the emulator computes for a registered task definition contains MANAGED_INSTANCES for a definition that is not Managed-Instances-capable.
AWS: `compatibilities` is DERIVED by ECS from the definition's own contents — it lists exactly the launch types the definition's parameters are valid for (Smithy: ecs#TaskDefinition.compatibilities : CompatibilityList, "Amazon ECS validates the task definition parameters with those supported by the launch type"; ecs#Compatibility enum values EC2, FARGATE, EXTERNAL, MANAGED_INSTANCES). Recorded behavior: for a two-container FireLens task definition (awsvpc, FARGATE-compatible, with an awsfirelens-logged app and a fluent-bit router) RegisterTaskDefinition/DescribeTaskDefinition report compatibilities sorted as ["EC2", "FARGATE"] — MANAGED_INSTANCES is NOT derived for it.
Emulator: DescribeTaskDefinition returns `compatibilities` = ['EC2', 'FARGATE', 'MANAGED_INSTANCES'] — the full enum rather than the set actually derived from the definition, so the list is unconditionally over-broad.
Why: a client reads `compatibilities` to decide which launch types a revision may be run under (and CDK/CFN and deployment tooling gate on it). An unconditional MANAGED_INSTANCES makes every revision look Managed-Instances-runnable, so a client will drive a launch the real service refuses, and any equality/diff assertion on the derived list fails.
Fix: the derivation of `compatibilities` in the RegisterTaskDefinition handler's task-definition projection (the same derived view DescribeTaskDefinition serializes) — derive the list from the definition's networkMode/requiresCompatibilities/volumes/linuxParameters rather than emitting every enum member.
Traces: ecs.ecsdeploy-firelens-task-definition-round-trips-with-router-defaults (asserts sorted(compatibilities) == ["EC2", "FARGATE"]).

Cause 2 of 2: a task whose container stopped is reported with no `exitCode`, and a task-level ephemeralStorage request is never provisioned inside the container.
AWS: when a container exits, DescribeTasks reports it with lastStatus STOPPED and the process's exit status in `exitCode` (Smithy: ecs#Container.exitCode : BoxedInteger, "The exit code returned from the container"), alongside `reason` and the task's stoppedReason "Essential container in task exited". A task-level ephemeralStorage request is actually provisioned, so a container's own `df` sees a filesystem of about the requested size (Smithy: ecs#EphemeralStorage.sizeInGiB — the minimum supported value is 21 GiB and the maximum 200 GiB), and a default-storage twin sees a smaller one.
Emulator: the probe task reaches STOPPED with stoppedReason 'Essential container in task exited' but the container object carries exitCode=None and reason=None — the container's command never ran, so neither the control (no ephemeralStorage) probe nor the 50 GiB probe can report its measured filesystem size as its exit code.
Why: a client that reads a task's exitCode to decide success/failure (batch jobs, one-shot migrations, CI steps) always sees nothing and cannot distinguish success from failure; and a workload sized against a requested ephemeral disk runs on default storage with no indication.
Why this is not already covered: the open issues 20261104T062603Z-ecs-containers-never-execute-no-in-container-behaviour.md and 20261105T143800Z-ecs-2-net-new-causes-containers-never-execute.md describe this family, but the replay report did not mark this check as OWNED by either — its failure has moved from a harness timeout to a concrete DescribeTasks response with exitCode absent, so the exit-code reporting path is now reachable and still wrong.
Fix: the RunTask/StartTask task-execution path (actually invoking the container image and propagating task-level ephemeralStorage into it) and the container exit reporting DescribeTasks serializes (Container.exitCode / Container.reason).
Traces: ecs.ephemeral-storage-request-is-provisioned-inside-the-container.

## Evidence
Cause 1 — Expected (AWS/recorded): DescribeTaskDefinition on the FireLens revision returns compatibilities whose sorted value is ["EC2", "FARGATE"]. Actual (emulator): AssertionError: compatibilities are COMPUTED from the definition; got ['EC2', 'FARGATE', 'MANAGED_INSTANCES'].
Cause 2 — Expected (AWS): the control probe container exits with its measured filesystem size in GiB as its exit code (0 < code < 250), and the 50 GiB task's probe measures a larger filesystem and at least 80% of the request. Actual (emulator): AssertionError: the control (no ephemeralStorage) probe must report its filesystem size in GiB as its exit code, got None (reason=None, stoppedReason='Essential container in task exited') — DescribeTasks reports the container STOPPED with no exitCode and no reason.

## Steps to Reproduce
Cause 1:
1. RegisterTaskDefinition with two containers (an app with logConfiguration logDriver=awsfirelens and no options, and a fluent-bit router with firelensConfiguration), networkMode=awsvpc, requiresCompatibilities=[FARGATE].
2. DescribeTaskDefinition on the returned taskDefinitionArn and read `compatibilities` → it contains MANAGED_INSTANCES in addition to EC2 and FARGATE.
Cause 2:
1. CreateCluster; RegisterTaskDefinition (FARGATE/awsvpc) twice: once with task-level ephemeralStorage sizeInGiB=50 and once without, each container running a `df`-based probe that exits with the measured filesystem size in GiB.
2. RunTask both revisions and poll DescribeTasks until lastStatus=STOPPED.
3. Read containers[0].exitCode → absent/None in both tasks (stoppedReason is 'Essential container in task exited'), so no filesystem size is ever reported and the requested 50 GiB cannot be observed.
