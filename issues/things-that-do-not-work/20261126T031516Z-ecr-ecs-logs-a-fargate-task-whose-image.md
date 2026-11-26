Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task
Issue-Key: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task|run#12|AssertionError|assert task["lastStatus"] == "RUNNING", (
Issue-Commit: 01f237389f2ba129bcfe6809f1b810fd15e9edea

# ecr__ecs__logs: a Fargate task whose image comes from a pull-through-cache repository goes straight to STOPPED/EssentialContainerExited instead of RUNNING, so the cached pull is never observable

> _Note: this issue was identified against the code base at commit `01f237389f2ba129bcfe6809f1b810fd15e9edea`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
1 net-new cause, failing the pull-through-cache check of this service group; no open issue's replay keys carry it.

Cause 1 of 1: a task launched from a pull-through-cache repository URI with a long-lived container command never reaches lastStatus RUNNING — it is reported STOPPED with stopCode EssentialContainerExited and stoppedReason 'Essential container in task exited' immediately, so neither the cached image pull nor the log delivery behind it can be observed.

AWS: Smithy: ecs Task.lastStatus / Container.exitCode and the RunTask lifecycle — a task whose essential container runs a command that does not exit (`sleep`-style) transitions PROVISIONING -> PENDING -> RUNNING and stays RUNNING; stopCode EssentialContainerExited is reserved for a task whose essential container actually ran and exited, and stoppedReason then names that container's exit. Pulling through an ECR pull-through cache rule is transparent to the task: on first reference the upstream image is cached under <prefix>/<upstream path> and the task runs normally. recorded behavior only for the exact cache-population timing; the requirement asserted here is that the task reaches RUNNING off the private cache URI.

Emulator: DescribeTasks reports lastStatus=STOPPED, stopCode=EssentialContainerExited, stoppedReason='Essential container in task exited' for a container whose command keeps it alive, so the container was never actually executed; the pull-through URI used was '<registryId>.dkr.ecr.<region>.localhost/<cache-prefix>/docker/library/busybox:latest'. RunTask answers 200 — the divergence is in the task record, not a status or error code.

Why: a client cannot verify that an ECR pull-through cache rule actually serves images to ECS, nor use a cached image to run anything: every such task is immediately terminal with a stop reason that misattributes the failure to the container's own exit, which sends a user debugging their image instead of the pull path.

Fix: the ECS task lifecycle / container runtime component that starts a task's containers and sets lastStatus, stopCode and stoppedReason — a container whose command is still running must keep the task RUNNING, and a task that could not pull must report the pull failure rather than EssentialContainerExited. Related open issues describe the same symptom under other service groups and the cache repository's URI shape, and are not marked as owning this check: 20261125T214101Z-cloudwatch-ecr-ecs-logs-a-fargate-task-whose.md, 20261125T215426Z-ecr-ecs-elbv2-a-fargate-task-whose-container.md, 20261125T054057Z-ecr-ecs-logs-createrepository-s-repositoryuri-is-the.md.

Traces: ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task.

## Evidence
Expected (AWS): the task launched from the pull-through-cache repository URI reaches lastStatus RUNNING (its essential container runs a long-lived command), and the image is cached under the rule's prefix.

Actual (emulator): 'AssertionError: task did not reach RUNNING off the private ECR URI 000000000000.dkr.ecr.us-west-2.localhost/sim2real-xsvc-ptc-1ef083cdb0b4/docker/library/busybox:latest: lastStatus=STOPPED stopCode=EssentialContainerExited stoppedReason=Essential container in task exited'. The account id, region, generated cache prefix and repository name differ per run and are not the requirement; the invariant that fails is that the task is terminal at once with an essential-container-exit reason for a container that should still be running.

## Steps to Reproduce
1. CreatePullThroughCacheRule for the public upstream registry with a prefix, and let it mint the private cache repository.
2. RegisterTaskDefinition(requiresCompatibilities=[FARGATE], networkMode=awsvpc) whose essential container image is '<repositoryUri>/docker/library/busybox:latest' with a long-lived command.
3. RunTask on a cluster with awsvpc networkConfiguration: answers 200.
4. Poll DescribeTasks: lastStatus goes to STOPPED with stopCode EssentialContainerExited and stoppedReason 'Essential container in task exited' instead of reaching RUNNING.
5. Re-replay ecr__ecs__logs.ecr-pull-through-cache-image-pulled-by-ecs-task to confirm the fix.
