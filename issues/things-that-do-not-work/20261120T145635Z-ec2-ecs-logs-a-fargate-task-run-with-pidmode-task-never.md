Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task
Issue-Key: ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task|_poll_until#13|TimeoutError|raise TimeoutError(f"{description}: the run budget ended first; not graded")
Issue-Commit: f422e37ca967a244b72f2f746d1d1ab79e5db12d

# A Fargate task run with pidMode=task never reaches lastStatus STOPPED, so DescribeTasks never settles the task

> _Note: this issue was identified against the code base at commit `f422e37ca967a244b72f2f746d1d1ab79e5db12d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: a Fargate task started with RunTask (pidMode=task, a short-lived observer container plus a sleeper container) is accepted — RunTask returns a task with no failures — but the task never reaches a terminal lastStatus. DescribeTasks keeps reporting a non-STOPPED lastStatus until the run budget is exhausted, so the task's outcome can never be read.

AWS: recorded behavior only for the lifecycle timing (the Smithy model carries no timing trait). RunTask accepts a Fargate task definition and the task then advances PROVISIONING -> PENDING -> RUNNING -> DEACTIVATING/STOPPING -> STOPPED once its essential container exits; DescribeTasks reports lastStatus STOPPED with stopCode/stoppedReason and the containers' exitCode, which is what a caller polls for. A task whose containers have run to completion must become STOPPED — AWS never leaves a one-shot task non-terminal.

Emulator: DescribeTasks for the task ARN returned by RunTask never reports lastStatus STOPPED; the check's poll on lastStatus in {STOPPED} runs out the whole budget on task arn:aws:ecs:<region>:<account>:task/<cluster>/<id>. Because the task never settles, neither the pidMode=task process-namespace observation nor the twin task's control can be graded.

Why: a client that launches a one-shot ECS task and waits for it (the standard batch/one-off pattern, and what every CLI 'run-task --wait'/waiter does) hangs forever: it never sees a terminal status, a stopCode, a stoppedReason or an exitCode, so it cannot tell success from failure.

Fix: the ECS task lifecycle state machine behind RunTask/DescribeTasks (task state progression and terminalization when the essential container exits) in the ECS service's task state, not the check. Related open issues name neighbouring symptoms of the same guest/task lifecycle area but do not carry this check: 20261115T105104Z-ec2-ecs-logs-a-fargate-task-s-container.md (empty process table) and 20261116T034914Z-ec2-ecs-logs-a-stopped-fargate-task-s.md (STOPPED task reports no exitCode).

Traces: ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task.

## Evidence
Expected: after RunTask returns a task (no failures), DescribeTasks eventually reports that task with lastStatus 'STOPPED' (with stopCode and the containers' exitCode) once the containers exit.
Actual (replay): TimeoutError: task arn:aws:ecs:us-west-2:<account>:task/<cluster>/<task-id> lastStatus in ['STOPPED']: the run budget ended first; not graded — raised from _await_task/_poll_until after polling DescribeTasks for the whole budget. The task ARN, cluster name and ids are per-run values; the invariant is that the task never becomes terminal.

## Steps to Reproduce
1. Create a cluster, a log group and an ecs-tasks.amazonaws.com execution role.
2. RegisterTaskDefinition for FARGATE with pidMode=task: one 'sleeper' container writing a marker process and one short-lived observer container that runs `ps` and exits; register a twin definition without pidMode.
3. RunTask each definition in an awsvpc network configuration; assert no failures are returned.
4. Poll DescribeTasks for lastStatus == 'STOPPED'. The task stays non-terminal for the whole poll window; on AWS it reaches STOPPED with the containers' exit codes.
