Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__efs__logs.ecs-efs-volume-shared-across-two-fargate-tasks-and-readonly-mount-write-denied
Issue-Key: ecs__efs__logs.ecs-efs-volume-shared-across-two-fargate-tasks-and-readonly-mount-write-denied|run#5|KeyError|assert _container(a_task)["exitCode"] == 0, (
Issue-Commit: 3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1

# ecs__efs__logs: a STOPPED Fargate task's container carries no exitCode at all — DescribeTasks omits Container$exitCode for a container that ran and exited

> _Note: this issue was identified against the code base at commit `3b9b0467f2eb3d7399c7e850c9f1cb96948fddb1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
One net-new root cause. This failing check is NOT marked ALREADY FILED and is NOT marked OWNED by any open issue in this replay, so it is filed here. Two open issues describe the surrounding scenario and are named so a fixer can see the relationship, but neither covers this failure: 20261111T030649Z-ecs-efs-logs-an-efs-volume-shared-by.md is keyed on the check being killed at its budget with no container output at all, and 20261112T054758Z-ecs-efs-logs-a-fargate-container-has-no.md is keyed on the later `A-SIZE` assertion and explicitly records that the container at that time reported `exitCode` 0. The check now fails EARLIER and on a different member: the container record has no `exitCode` key whatsoever, so the KeyError aborts the check before any filesystem assertion is reached. That is a distinct, newly visible divergence in the task record DescribeTasks serialises.

Cause 1 of 1: DescribeTasks reports a task in `lastStatus` STOPPED whose `containers[0]` omits `exitCode` entirely, even though that container was started, ran its program and delivered its output to CloudWatch Logs.

AWS: Smithy `com.amazonaws.ecs#Container$exitCode` (target `BoxedInteger`) — "The exit code returned from the container". A Fargate task that reached RUNNING and then STOPPED because its essential container's process ended reports, for that container, `lastStatus` STOPPED and the integer status that process exited with; DescribeTasks on such a task (Smithy: DescribeTasks -> Tasks -> Task$containers) always carries it. For the `sh -c` program this check runs — a script whose last statement is an `echo` and which is not killed — the exit status is 0, and the task's own stop is `stopCode` `EssentialContainerExited` with the reason AWS words for that code. `exitCode` is absent only for a container that never ran (a task stopped with `TaskFailedToStart`, where `Container$reason` carries the explanation instead) — and this task is explicitly asserted not to be one of those (`_run_and_wait` already asserts `stopCode != "TaskFailedToStart"`, and the container's awslogs stream delivered the program's output, so the container demonstrably ran). The exit value for this particular program is recorded behavior only; the presence of the member for a container that ran is what the model states.

Emulator: the container run is abandoned and no status is ever attributed to the container, so the stored `ContainerRecord` keeps `exit_code = None` and the serialised `Container` has no `exitCode` member. The mechanism is in the ECS crate: `src/state/container_shell.rs` performs only the fixed catalog `BUILT_IN_COMMANDS = ["echo", "true", "false", ":", "exit", "export", "unset", "trap", "sleep", "wait"]` and declines every other command, every redirection and every command substitution, and its module contract states that such a run "CLAIMS NOTHING about the container (ContainerOutcome::Abandoned) ... it is not reported as the container's own exitCode". `src/state/container_runtime.rs::record_container_outcome` returns early for `ContainerOutcome::Abandoned` without writing `container.exit_code`, and `src/state/state.rs::TaskRecord::finish_work` documents and implements the same choice ("No container is given an `exitCode`"). The result is a task the emulator answers as STOPPED, with its containers STOPPED, and with no exit status for any of them.

Why: a client relying on AWS's behavior reads `task["containers"][0]["exitCode"]` to decide whether the work the task was placed to do succeeded — that is the only machine-readable success signal a run-to-completion (batch/job) task has. Against the emulator that key is simply missing, so every such client raises a KeyError / NPE / nil-dereference instead of getting a verdict, and defensive clients that default a missing `exitCode` treat a failed run as "no information" or as success. It is also strictly worse than a wrong value: a wrong `exitCode` can be compared, an absent one cannot, and it makes the task indistinguishable from a task that never started (where AWS's absence of `exitCode` genuinely means "nothing ran") even though the container ran and logged.

Fix: the ECS container data plane behind RunTask/DescribeTasks — `state::container_runtime` (the outcome recording, `record_container_outcome`, and `TaskRecord::finish_work`) together with `state::container_shell`, plus the `Container` view the task record serialises for DescribeTasks. A container the emulator started and whose task it then moves to STOPPED must carry an `exitCode`; the status must be the one performing the container's program really produced, which is why this depends on the container actually being able to perform the program it was given (the mounted-filesystem gap filed as 20261112T054758Z-ecs-efs-logs-a-fargate-container-has-no.md) rather than on inventing a value. Withholding the member is not an available option for a container that reached STOPPED after running.

Traces: ecs__efs__logs.ecs-efs-volume-shared-across-two-fargate-tasks-and-readonly-mount-write-denied.

## Evidence
Cause 1 — Expected (real AWS): after RunTask of an awsvpc/FARGATE task definition whose single essential container runs `sh -c '<script ending in echo A-DONE>'`, polling DescribeTasks until `lastStatus == "STOPPED"` returns a task whose `containers[0]` contains `exitCode` (Smithy `Container$exitCode`, BoxedInteger — "The exit code returned from the container"), with value 0 for a script that ran to completion, alongside `lastStatus` STOPPED; the task itself carries `stopCode` `EssentialContainerExited`.

Actual (emulator): `containers[0]` has NO `exitCode` member. The check aborts at

    File "check_ecs__efs__logs_ecs_efs_volume_shared_across_two_fargate_tasks_and_readonly_mount_write_denied.py", line 68, in run
      assert _container(a_task)["exitCode"] == 0, (
    KeyError: 'exitCode'

where `_container(task)` is `task["containers"][0]` and `a_task` is the DescribeTasks record captured at the first poll that observed `lastStatus == "STOPPED"`. The container had already run: the preceding `_stream_lines(...)` call returned successfully, i.e. the container's `A-DONE` line was delivered to the configured awslogs stream, and `_run_and_wait` had already asserted `stopCode != "TaskFailedToStart"`. So this is not the AWS case in which `exitCode` is legitimately absent (a container that never started).

Note on the earlier filing: 20261112T054758Z-ecs-efs-logs-a-fargate-container-has-no.md records this same container as reporting `exitCode` 0 and the check failing later, on `assert f"A-SIZE={len(marker) + 1}" in a_log`. The current behavior is different — the member is gone — which is why this replay's failure is a KeyError on the member name rather than a value mismatch, and why the member's absence is the requirement being stated here.

Source of the behavior, for reference (ECS crate `services/service-com-amazonaws-ecs-amazonec2containerservicev20141113`):
- `src/state/container_shell.rs`: `BUILT_IN_COMMANDS = ["echo", "true", "false", ":", "exit", "export", "unset", "trap", "sleep", "wait"]`; module doc — "A run that reached such a command CLAIMS NOTHING about the container ([`ContainerOutcome::Abandoned`]) ... it is not reported as the container's own `exitCode`"; "It is the WHOLE RUN that claims nothing, not the one command".
- `src/state/container_runtime.rs::record_container_outcome`: `let ContainerOutcome::Exited(status) = outcome else { self.end_container_run(container_arn); return; };` — the abandoned path never assigns `container.exit_code`.
- `src/state/state.rs::TaskRecord::finish_work` (around line 4116): "No container is given an `exitCode` ... The agent's own report (`SubmitContainerStateChange`) stays the only writer of one."
- `src/ops/describe_tasks.rs` serialises the stored record via `task.view(include_tags)`, so the absent stored value becomes an absent wire member.

## Steps to Reproduce
1. Create an EFS file system, a mount target in the task subnet and an access point rooted at /data, each polled to `available`.
2. RegisterTaskDefinition: `networkMode=awsvpc`, `requiresCompatibilities=[FARGATE]`, `cpu=256`, `memory=512`, one essential busybox container named `app` with an `awslogs` logConfiguration pointing at a log group, `volumes=[{name: shared, efsVolumeConfiguration: {fileSystemId: <fs>, transitEncryption: ENABLED, authorizationConfig: {accessPointId: <ap>, iam: ENABLED}}}]` and `mountPoints=[{sourceVolume: shared, containerPath: /mnt/shared, readOnly: false}]`.
3. RunTask with `launchType=FARGATE`, `count=1`, an awsvpc networkConfiguration, and the container override `command = ["sh", "-c", 'set -e; echo "$MARKER" > /mnt/shared/payload; sync; echo "A-SIZE=$(wc -c < /mnt/shared/payload)"; echo "A-MODE=$(ls -l /mnt/shared/payload | cut -c1-10)"; echo A-DONE']` plus `environment=[{name: MARKER, value: <any value>}]`.
4. Poll `DescribeTasks(cluster=<cluster>, tasks=[<taskArn>])` until `tasks[0]["lastStatus"] == "STOPPED"`; confirm `tasks[0].get("stopCode") != "TaskFailedToStart"`.
5. Read the container's awslogs stream (`<prefix>/app/<taskId>`) with GetLogEvents and confirm the container's output (`A-DONE`) was delivered — i.e. the container really ran.
6. Read `tasks[0]["containers"][0]`.
   Observed: the dict has no `exitCode` key (`KeyError: 'exitCode'`).
   Expected: `exitCode` present, value 0 for this script.

## Resolution note (added by the fix that closed this report)

STATUS: cause 1 is SERVED, and this report's recorded check CANNOT be verified by replay — it
is cut at the harness's 60 s cap before any assertion of it runs, for the budget reason
measured below.

The mechanism this report names is gone: the container's session performs the redirections,
the command substitutions and the file utilities that program is written out of, so the run
ENDS with a status instead of being abandoned, and that status is the container's `exitCode`.
Driven end to end over the calls the check makes — an Amazon EFS file system, a mount target
in the task's subnet and an access point rooted at `/data`, each polled to `available`; a
`FARGATE`/`awsvpc` revision with the `efsVolumeConfiguration` volume mounted at `/mnt/shared`
and an `awslogs` logConfiguration; `RunTask` with the recorded container override; then
`DescribeTasks` and `GetLogEvents` — this checkout answers:

* `lastStatus` `STOPPED` with `stopCode` `EssentialContainerExited` and `stoppedReason`
  `Essential container in task exited`;
* `containers[0]` carrying `exitCode` 0 on the record of the FIRST read that reports
  `STOPPED`, which is the read the check captures and the member its `KeyError` was raised
  for;
* the container's own lines in `<prefix>/app/<taskId>` (`A-SIZE=<marker bytes + 1>`,
  `A-MODE=-rw-r--r--`, `A-DONE`), so the container demonstrably ran — the premise this
  report's rule is about;
* the read-only revision's write refused with `Read-only file system` while its read still
  answers, with that arm's container reporting its own `exitCode` too.

Why the replay verdict stays `failed`: the key it now reports for this check is
`_poll_until#5|Timeout|return last` — the check is killed inside a poll rather than on a
comparison — which is the key the open report
20261124T061319Z-ecs-efs-logs-the-shared-efs-fargate-volume.md owns. Where those 60 s go was
measured by the earlier report of this same check
(20261104T054216Z-ecs-efs-logs-the-shared-efs-fargate-scenario.md, now closed): the check's
own source polls with `interval_s=10`, so the file system, the mount target, the access point
and each task arm each cost one look plus one 10 s sleep, and the VPC, subnet, gateway and
roles it builds before `CreateCluster` come on top — six sequential sleeps meet the cap
whatever the emulator answers, and that report records that collapsing every clock-owed span
of the ECS and Amazon EFS crates to 1 ms leaves the same verdict. This fix re-measured it
rather than assuming it: cutting the span a standalone task's work is reported running for
(`state::CONTAINER_WORK_SPAN`, 3 s) to one transition span — 2.5 s off every arm, and the
whole scenario to 13.6 s of emulator-owed time — left the replay line byte-identical, so the
span was left as it was.

What this fix adds, since the behaviour is already served, is the one assertion no suite made
and the one this report's client needs:
`services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/tests/all/stopped_task_carries_its_containers_exit_status.rs`
pins, over a table of programs, that the record a task is read as at the FIRST read reporting
`lastStatus` `STOPPED` already carries its container's `exitCode` — the suites that poll until
a status has arrived would pass just as well if a polling client had to keep reading for the
verdict. The EFS and `awslogs` halves of the recorded scenario stay pinned by
`core/router/tests/all/ecs_efs_volume_mounts.rs`,
`core/router/tests/all/ecs_efs_shared_volume_handover.rs` (whose four arms are the recorded
arms, each asserting its container's `exitCode`) and
`core/router/tests/all/ecs_read_only_mount_write_refusal.rs`.
