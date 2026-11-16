Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs__logs.deep-ecs-1-guestdepth-firelens-fluentbit-routes-app-output ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task
Issue-Key: ec2__ecs__logs.deep-ecs-1-guestdepth-firelens-fluentbit-routes-app-output|run#15|AssertionError|assert app.get("exitCode") == 0, (
Issue-Key: ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task|run#14|AssertionError|assert obs.get("exitCode") == 0, (
Issue-Commit: 3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1

# ec2__ecs__logs: a stopped Fargate task's container is reported with no exitCode and its command never runs, so FireLens sidecar log routing and pidMode=task are unobservable

> _Note: this issue was identified against the code base at commit `3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
Cause 1 of 1: a task that reaches STOPPED is reported with no container exit status, and the container's command never executed, so neither of these two in-guest behaviours can be produced.
AWS: when a task's essential container exits, DescribeTasks reports that container with lastStatus STOPPED and the process's exit status in `exitCode` (Smithy: ecs#Container.exitCode : BoxedInteger, "The exit code returned from the container"), plus `reason` where the agent has one, and the task's stoppedReason "Essential container in task exited". Because the container really runs, the modeled features it exercises are observable: with logConfiguration logDriver=awsfirelens the fluent-bit sidecar receives the app's stdout/stderr and writes them as JSON records (log, source, ecs_task_arn) under its own stream, and with task-level pidMode=task (Smithy: ecs#TaskDefinition.pidMode / ecs#PidMode, values host and task) an observer container sees a sibling container's process in its own `ps`.
Emulator: both probe tasks reach lastStatus STOPPED with stoppedReason 'Essential container in task exited', but the container object has no `exitCode` member at all (the harness reads None) and no `reason`; the app/observer commands produced no output, so no FireLens JSON records and no cross-container `ps` visibility exist to inspect.
Why: a client cannot tell whether its container succeeded or failed — every stopped task looks the same — and two task-definition features it configured (awsfirelens log routing and pidMode) are silently no-ops while the control plane accepts and echoes them.
Fix: the RunTask/StartTask task-execution path (invoking the container image, wiring the awsfirelens log router sidecar to the app's stdout/stderr, and applying task-level pidMode) and the container exit reporting that DescribeTasks serializes (Container.exitCode / Container.reason).
Traces: ec2__ecs__logs.deep-ecs-1-guestdepth-firelens-fluentbit-routes-app-output, ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task.

## Evidence
Expected (AWS): DescribeTasks on the stopped task reports the app/observer container with exitCode 0 (and the FireLens sidecar's stream carries the app's lines as JSON records; the pidMode=task observer's `ps` lists the sleeper's marker process). Actual (emulator): AssertionError: firelens app did not exit 0: {... 'name': 'app', 'lastStatus': 'STOPPED', 'healthStatus': 'UNKNOWN', 'managedAgents': [], 'cpu': '0'} stoppedReason='Essential container in task exited' — the container structure has no exitCode member; and AssertionError: pidMode=task observer did not exit 0: {... 'name': 'observer', 'lastStatus': 'STOPPED' ...} stoppedReason='Essential container in task exited'. In both cases the only variable parts are the generated cluster/task/container ARNs; the absent exitCode is invariant.

## Steps to Reproduce
1. CreateCluster; create a log group and an execution role.
2. RegisterTaskDefinition A: app container (public.ecr.aws/docker/library/busybox) with logConfiguration logDriver=awsfirelens plus a fluent-bit router container with firelensConfiguration type=fluentbit; the app echoes marker lines then exits 0.
3. RegisterTaskDefinition B: task-level pidMode=task with a sleeper container and an observer container that greps the sleeper's marker out of `ps` and exits 0 on success.
4. RunTask each and poll DescribeTasks until lastStatus=STOPPED.
5. Read containers[].exitCode for 'app' and 'observer' → absent/None in both, and no FireLens records or cross-container `ps` output exist.
