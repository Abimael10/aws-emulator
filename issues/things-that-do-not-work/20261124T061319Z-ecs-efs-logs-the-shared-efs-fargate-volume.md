Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__efs__logs.ecs-efs-volume-shared-across-two-fargate-tasks-and-readonly-mount-write-denied
Issue-Key: ecs__efs__logs.ecs-efs-volume-shared-across-two-fargate-tasks-and-readonly-mount-write-denied|_poll_until#5|Timeout|return last
Issue-Commit: 14f7a4850e696cd0bba053c19b13f3f2b3c04149

# ecs__efs__logs: the shared-EFS Fargate volume scenario reaches no verdict — no observable step completes and the check is cut at the 60 s replay cap

> _Note: this issue was identified against the code base at commit `14f7a4850e696cd0bba053c19b13f3f2b3c04149`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
One net-new root cause. This failing check is NOT marked ALREADY FILED and is NOT marked OWNED by any open issue in this replay, so it is filed here. Open issue 20261116T030849Z-ecs-efs-logs-a-stopped-fargate-task-s.md describes the same check, but it is keyed on a LATER, DIFFERENT failure (`KeyError: 'exitCode'` on `assert _container(a_task)["exitCode"] == 0` at check line 68, i.e. the emulator did reach a STOPPED task record inside the budget at that time) and therefore does not cover this one; the related family 20261111T030649Z / 20261112T054758Z likewise key on assertions downstream of a task that settled. The check now never reaches ANY of those assertions at all.

Cause 1 of 1: the Fargate-task-over-EFS lifecycle never progresses far enough for a single graded step — the check is killed by the harness at its 60 s cap (`Error: Check timed out after 60.0s`) with no assertion reached and no diagnostic beyond the cap, so every claim this check makes (a shared EFS volume readable across task boundaries, a readOnly mountPoint refusing a write with EROFS, the refused write leaving no trace, EFS SizeInBytes metering the written bytes) is unverifiable.

AWS: Smithy `com.amazonaws.ecs#RunTask` -> `RunTaskResponse$tasks` (list of `Task`) and `com.amazonaws.ecs#DescribeTasks` (readonly) -> `DescribeTasksResponse$tasks`. A task RunTask accepted for `launchType=FARGATE` with an `awsvpc` networkConfiguration advances monotonically through `Task$lastStatus` PROVISIONING -> PENDING -> RUNNING -> STOPPED, and once the essential container's program ends the task settles to STOPPED with `stopCode` EssentialContainerExited and the container carrying `Container$exitCode`; polling DescribeTasks therefore terminates. The supporting control calls the check makes before that (CreateCluster, CreateFileSystem polled to LifeCycleState `available`, CreateMountTarget polled to `available`, CreateAccessPoint polled to `available`, CreateRole/PutRolePolicy, CreateLogGroup, RegisterTaskDefinition) each settle to their modeled terminal state; none of them is an operation that can poll forever. The per-step wall-clock of a real Fargate launch is recorded behavior only (the check budgets up to 900 s per task), but that every one of these poll loops CONVERGES is what the model states.

Emulator: nothing in the sequence converges inside the replay window. The check performs no graded assertion and is terminated by the harness: `Error: Check timed out after 60.0s`. There is no error code, no non-2xx and no partial output — the scenario simply makes no observable progress, which is strictly less than the previous replay of this same check, where the task did reach `lastStatus` STOPPED and the failure was the missing `exitCode` member. So a wait loop in this path (the DescribeTasks poll for a terminal `lastStatus`, or one of the EFS mount-target / access-point `available` polls, or the awslogs stream/event poll the check runs after each task) now never satisfies its predicate.

Why: a client relying on AWS's behavior launches a run-to-completion Fargate task and polls DescribeTasks (or uses the `tasks_stopped` waiter) for a terminal `lastStatus`. Against the emulator that poll never terminates, so the client hangs until its own timeout and never learns whether the work ran; everything downstream of task settlement — reading the container's log stream, reading its exit status, asserting what the EFS volume now holds — is unreachable. A non-terminating control plane is worse than a wrong answer: there is no value to compare and no failure to handle.

Fix: the ECS task data plane behind RunTask/DescribeTasks — the task-state progression and its terminal settlement (`state::container_runtime` / `TaskRecord` work-completion path) together with whatever the task's EFS volume attachment waits on in the EFS mount-target/access-point lifecycle, and the awslogs delivery the check polls between tasks. Each poll loop this scenario drives must reach its modeled terminal state within the replay window; see Traces for the check and for the open issues whose keys record the earlier, further-advancing behavior.

Traces: ecs__efs__logs.ecs-efs-volume-shared-across-two-fargate-tasks-and-readonly-mount-write-denied.

## Evidence
Cause 1 — Expected (real AWS): the check completes its body. Concretely, after RunTask of an awsvpc/FARGATE task definition with an `efsVolumeConfiguration` (transitEncryption ENABLED, authorizationConfig.accessPointId set) mounted at /mnt/shared, DescribeTasks polling returns a task whose `lastStatus` becomes STOPPED with `stopCode` EssentialContainerExited and whose `containers[0]` carries `exitCode`; the check then grades its EFS sharing, readOnly-refusal (`Read-only file system`) and no-trace assertions and finishes well inside its own 900 s-per-task budget.

Actual (emulator): no assertion is reached. The replay report records the whole check as:

    #### [mismatch] ecs__efs__logs.ecs-efs-volume-shared-across-two-fargate-tasks-and-readonly-mount-write-denied
    Error: Check timed out after 60.0s

There is no traceback into the check body — unlike the previous replay of this same check, which failed at `check_ecs__efs__logs_...py", line 68, in run / assert _container(a_task)["exitCode"] == 0 / KeyError: 'exitCode'` and is filed as 20261116T030849Z-ecs-efs-logs-a-stopped-fargate-task-s.md. That earlier key proves the emulator used to reach a STOPPED task record plus delivered awslogs output inside the same cap, so the current absence of ANY graded step is a progression stall in this path rather than the check being inherently too long for the window.

The operations in the stalled path and their models, for reference: `RunTask` (input `taskDefinition` required; output `tasks`/`failures`) and `DescribeTasks` (readonly; input `tasks` required; output `tasks`/`failures`) in com-amazonaws-ecs-amazonec2containerservicev20141113; plus EFS CreateMountTarget / CreateAccessPoint whose records the check polls to LifeCycleState `available`, and CloudWatch Logs GetLogEvents on the container's awslogs stream.

## Steps to Reproduce
1. CreateCluster, and CreateLogGroup for the awslogs driver.
2. CreateFileSystem; CreateMountTarget in the task subnet, polled until LifeCycleState `available`; CreateAccessPoint rooted at /data with a POSIX user, polled until `available`.
3. CreateRole/PutRolePolicy for a task role granting elasticfilesystem:ClientMount+ClientWrite on the file system ARN conditioned on the access point ARN, and an execution role granting the ecr:* / logs:CreateLogStream / logs:PutLogEvents set.
4. RegisterTaskDefinition: networkMode=awsvpc, requiresCompatibilities=[FARGATE], cpu=256, memory=512, one essential busybox container with an awslogs logConfiguration, volumes=[{name: shared, efsVolumeConfiguration: {fileSystemId: <fs>, transitEncryption: ENABLED, authorizationConfig: {accessPointId: <ap>, iam: ENABLED}}}], mountPoints=[{sourceVolume: shared, containerPath: /mnt/shared, readOnly: false}]. Register a second revision with readOnly: true.
5. RunTask (launchType=FARGATE, count=1, awsvpc networkConfiguration, assignPublicIp ENABLED) with the container override command `sh -c 'set -e; echo "$MARKER" > /mnt/shared/payload; sync; echo "A-SIZE=$(wc -c < /mnt/shared/payload)"; echo A-DONE'`.
6. Poll DescribeTasks(cluster=<cluster>, tasks=[<taskArn>]) for a terminal `lastStatus`, then GetLogEvents on the container's awslogs stream for the `A-DONE` line.
   Observed: the sequence never yields a graded step; the check is terminated by the harness after 60 s with `Check timed out after 60.0s` and no assertion is ever evaluated.
   Expected: each poll converges — the task settles to STOPPED/EssentialContainerExited with an `exitCode` on its container and the container's output is readable — so the scenario's EFS-sharing, readOnly-refusal and no-trace assertions can be graded.
