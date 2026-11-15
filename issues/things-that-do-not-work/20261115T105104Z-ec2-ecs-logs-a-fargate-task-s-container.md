Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task
Issue-Key: ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task|_await_listing#15|AssertionError|assert rows, f"no parseable `pid args` rows in {stream}: {msgs!r}"
Issue-Commit: f7526cbca79906f89a28c5d197946d676b42fe8d

# ec2__ecs__logs: a Fargate task's container sees an empty process table — `ps -o pid,args` in a running container prints no rows at all, so pidMode=task sharing and per-container PID isolation are both unobservable

> _Note: this issue was identified against the code base at commit `f7526cbca79906f89a28c5d197946d676b42fe8d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
ONE net-new root cause (Cause 1 of 1) in the ECS/Fargate container data plane.

Cause 1 of 1 — a container the emulator runs has no visible process table: the container's shell executes and its own stdout reaches CloudWatch Logs, but `ps` inside it lists nothing, not even the `ps` process itself or PID 1.

AWS: Smithy (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/smithy-model/..., TaskDefinition.pidMode) documents 'The process namespace to use for the containers in the task. The valid values are host or task. On Fargate for Linux containers, the only valid value is task. For example, monitoring sidecars might need pidMode to access information about other containers running in the same task. ... If task is specified, all containers within the specified task share the same process namespace. If no value is specified, the default is a private namespace for each container,' and 'This parameter is only supported for tasks that are hosted on Fargate if the tasks are using platform version 1.4.0 or later (Linux).' So on AWS: (a) in ANY running Linux container, `ps -o pid,args` lists at least the container's own processes — its entrypoint shell as PID 1 in a private namespace, plus the `ps` invocation itself; (b) with pidMode='task', the observer container additionally lists the sibling container's process, identifiable by the sibling's argv; (c) without pidMode, it lists only its own, and its PID 1 is its own shell. Recorded behavior for this scenario: the observer's awslogs stream contains BEGIN-PS, then one `pid args` row per visible process, then END-PS.

Emulator: the task reaches a state where the observer container's command runs to completion and its markers ship — the log stream contains exactly ['BEGIN-PS', 'END-PS'] — but zero rows between them. `ps -o pid,args` produced no output whatsoever, so not even the observer's own `ps` process or its PID 1 shell exists in the container's view. The task definition's pidMode='task' is accepted and read back correctly, and the twin registered without pidMode correctly omits the member, so the divergence is not in the control-plane representation: it is that the container runtime presents no /proc process table to the workload.

Why: a client relying on AWS's behaviour gets a container that cannot see itself. Every workload that reads the process table inside a Fargate task breaks silently rather than loudly: monitoring and APM sidecars (the exact use the model's documentation names), supervisors and init shims that reap or signal siblings, health probes that check 'is my daemon still running', `pgrep`/`pkill`-based entrypoints, and any diagnosis that inspects PID 1. Worse, pidMode is unverifiable in both directions — a task that requested namespace SHARING and a task that requested ISOLATION produce the identical empty listing, so neither the sharing nor the isolation the task definition asked for can be confirmed, and a security review that depends on per-container PID isolation has nothing to observe.

Fix: the ECS task data plane that executes a task's containers must present the container's processes to the workload — its own PID namespace (entrypoint as PID 1 plus every process it spawns, including the one reading the table) and, when the task definition sets pidMode='task', one namespace shared across the task's containers so each container's listing includes the siblings' processes. This belongs in the task-execution/container-runtime component that runs RunTask's containers, not in RegisterTaskDefinition (which already stores and projects pidMode correctly) and not in the awslogs delivery path (which already ships the container's stdout).

Traces: ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task.

## Evidence
Behavioral replay of suite simparity-behavioral-aws-v1 against this checkout (emulator serving).

[mismatch] ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task — AssertionError raised inside the check's `_await_listing` helper at `assert rows, f"no parseable \`pid args\` rows in {stream}: {msgs!r}"`:

  no parseable `pid args` rows in ecs/observer/<task-id>: ['BEGIN-PS', 'END-PS']

What succeeded before it, so is NOT part of this cause: the VPC/subnets/security group, the log group, the task execution role, the cluster, both RegisterTaskDefinition calls (the pidMode='task' definition reads back pidMode == 'task' and the twin registered without it has no pidMode member — both assertions passed), RunTask for both tasks, the Fargate platformVersion >= 1.4.0 assertion for both tasks, and the observer container exiting 0 in both tasks. The awslogs stream for the observer exists and carries the container's stdout.

The container ran: `sh -c "sleep 3; echo BEGIN-PS; ps -o pid,args; echo END-PS; sleep 3"` on image public.ecr.aws/docker/library/busybox:1.36, alongside a sibling container running `sh -c 'while :; do sleep 1; done' <marker>` whose argv carries a unique marker. The delivered stream is exactly the two echo lines: the `ps -o pid,args` between them emitted nothing.

Expected stream body between BEGIN-PS and END-PS: one or more lines of the form '<pid> <args>'. The check then requires, from those rows:
  - every observer listing (pidMode=task AND default) contains a row whose args contain 'ps -o pid,args' — the container always sees its own `ps`;
  - the pidMode=task observer's rows contain a row whose args contain the sibling's marker;
  - the default (no pidMode) observer's rows do NOT contain the marker;
  - in the default observer, the row with pid == 1 has args starting 'sh -c sleep 3' — its own shell is PID 1.
None of these four is reachable today because there are no rows. The marker string, the task ids and the log-stream names are per-run generated values; the requirement is that the process table is non-empty and that the sharing/isolation above holds.

Smithy grounding: smithy_query query=doc shape=TaskDefinition member=pidMode — 'If task is specified, all containers within the specified task share the same process namespace. If no value is specified, the default is a private namespace for each container'; 'On Fargate for Linux containers, the only valid value is task'; supported on Fargate only at platform version 1.4.0 or later (Linux).

## Steps to Reproduce
1) Build and boot the emulator from this checkout and replay suite simparity-behavioral-aws-v1, check ec2__ecs__logs.deep-ecs-1-guestdepth-pidmode-task.
2) Create a VPC with subnets and a security group, a CloudWatch Logs log group, and an ECS task execution role allowing logs:CreateLogStream and logs:PutLogEvents; create an ECS cluster.
3) ecs RegisterTaskDefinition twice with the SAME two containers, runtimePlatform={operatingSystemFamily:'LINUX', cpuArchitecture:'X86_64'}, executionRoleArn=<exec role>, awslogs logConfiguration on both containers, prefix 'ecs':
   - container 'sleeper' (essential false): image public.ecr.aws/docker/library/busybox:1.36, command ['sh','-c','while :; do sleep 1; done','<unique-marker>'] so the marker is the shell's $0 and appears in argv;
   - container 'observer' (essential true, dependsOn sleeper condition START): same image, command ['sh','-c','sleep 3; echo BEGIN-PS; ps -o pid,args; echo END-PS; sleep 3'].
   Register one definition with pidMode='task' and the twin with pidMode omitted.
4) ecs RunTask both definitions on FARGATE in those subnets with that security group; wait for the observer container to exit 0 and check platformVersion >= 1.4.0.
5) logs: read the observer's stream 'ecs/observer/<task-id>' for each task. Between the 'BEGIN-PS' and 'END-PS' messages there must be one or more '<pid> <args>' lines. Observed: zero lines in between (the stream is exactly ['BEGIN-PS','END-PS']).
6) From those rows assert: both observers list a process whose args contain 'ps -o pid,args'; the pidMode='task' observer lists a process whose args contain the sleeper's marker; the no-pidMode observer does NOT; and in the no-pidMode observer the row with pid 1 has args starting 'sh -c sleep 3'.
