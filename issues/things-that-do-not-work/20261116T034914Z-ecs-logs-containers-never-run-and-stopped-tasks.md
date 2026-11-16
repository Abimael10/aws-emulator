Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__logs.ecs-task-awslogs-stream-name-and-stdout ecs__logs.log-workload-producers-awslogs-one-event-per-line-verbatim ecs__logs.log-workload-producers-awslogs-long-line-split-at-event-size-limit
Issue-Key: ecs__logs.ecs-task-awslogs-stream-name-and-stdout|run#13|AssertionError|assert container.get("exitCode") == 0, (
Issue-Key: ecs__logs.log-workload-producers-awslogs-one-event-per-line-verbatim|run#13|AssertionError|assert c.get("exitCode") == 0, (
Issue-Key: ecs__logs.log-workload-producers-awslogs-long-line-split-at-event-size-limit|run#16|AssertionError|assert c.get("exitCode") == 0, (
Issue-Commit: 3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1

# ecs__logs: containers never run and stopped tasks report no exitCode, so the awslogs driver delivers no events — stream naming, one-event-per-line and the 256 KiB event split are all unobservable

> _Note: this issue was identified against the code base at commit `3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: a task whose container stopped is reported with no exit status and the container's command never produced output, so nothing the awslogs log driver is supposed to deliver exists.
AWS: a stopped container is reported with the process's exit status in `exitCode` (Smithy: ecs#Container.exitCode : BoxedInteger, "The exit code returned from the container") plus `reason`, with the task's stoppedReason "Essential container in task exited". Because the container really runs, the awslogs driver (logConfiguration logDriver=awslogs with awslogs-group / awslogs-stream-prefix) creates the stream named <prefix>/<container-name>/<task-id> and forwards both stdout and stderr into it: each non-empty output line becomes exactly one byte-exact event, and a line too large for one event is split at the PutLogEvents event cap (each event is accounted as its UTF-8 message bytes plus 26 bytes of overhead against a 262144-byte cap, so the largest single-event message is 262118 bytes).
Emulator: each probe task reaches STOPPED with container exitCode=None/absent and reason None (stoppedReason 'Essential container in task exited'); the container's command never executed, so no stdout/stderr exists and the awslogs driver delivers no events to inspect — stream naming, per-line event fidelity and the over-cap split cannot be exercised at all.
Why: CloudWatch Logs from awslogs is the primary way a client sees what its container did. Here a task reports STOPPED with no exit code and an empty (or absent) log stream, so a client cannot read its application output, cannot alarm or filter on it, and cannot tell success from failure.
Fix: the RunTask/StartTask task-execution path (running the container image and capturing its stdout/stderr) plus the awslogs log-driver delivery path that names the stream <prefix>/<container>/<task-id> and PutLogEvents one event per line with the 262144-byte (message + 26 bytes) accounting, and the container exit reporting DescribeTasks serializes (Container.exitCode / Container.reason).
Traces: ecs__logs.ecs-task-awslogs-stream-name-and-stdout, ecs__logs.log-workload-producers-awslogs-one-event-per-line-verbatim, ecs__logs.log-workload-producers-awslogs-long-line-split-at-event-size-limit.

## Evidence
Expected (AWS): DescribeTasks on each stopped probe task reports its container with exitCode 0, and the awslogs stream <prefix>/<container>/<task-id> carries the container's stdout and stderr (one byte-exact event per non-empty line, with an over-cap line split at 262144 - 26 bytes). Actual (emulator): AssertionError: container did not exit 0: exitCode=None reason=None stoppedReason=Essential container in task exited; and AssertionError: container did not exit 0: exitCode=None reason='Essential container in task exited' (twice) — no exit code and no delivered log events.

## Steps to Reproduce
1. CreateLogGroup (assert it has no streams); create an execution role; CreateCluster.
2. RegisterTaskDefinition (FARGATE/awsvpc) with logConfiguration logDriver=awslogs, awslogs-group=<group>, awslogs-stream-prefix=ecs, and a container command that writes known lines to stdout and stderr (one variant emitting a single line larger than 262118 bytes) and exits 0.
3. RunTask and poll DescribeTasks until lastStatus=STOPPED; read containers[].exitCode → None.
4. DescribeLogStreams / GetLogEvents on the group → no events are delivered, so the <prefix>/<container>/<task-id> stream name, the one-event-per-line fidelity and the over-cap split cannot be checked.
