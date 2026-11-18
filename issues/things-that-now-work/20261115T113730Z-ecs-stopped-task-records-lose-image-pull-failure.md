Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record ecs-fargate-run-task-reports-container-exit-code
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].startedAt
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].stopCode
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].stoppedReason
Issue-Key: ecs|StopTask|2xx->2xx|$.task.attachments[*]
Issue-Key: ecs|StopTask|2xx->2xx|$.task.containers[*]
Issue-Key: ecs|StopTask|2xx->2xx|$.task.overrides.containerOverrides[*]
Issue-Key: ecs|StopTask|2xx->2xx|$.task.stopCode
Issue-Key: ecs|StopTask|2xx->2xx|$.task.stoppedReason
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].connectivityAt
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].executionStoppedAt
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].pullStartedAt
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].pullStoppedAt
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].stoppedAt
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].stoppingAt
Issue-Related: 20261110T175605Z-ecs-fargate-runtask-and-describetasks-task-records-omit.md
Issue-Commit: 3fd6090c6180e74798ad464a124672539521deef

# ECS stopped-task records lose image-pull failure fidelity and Fargate provisioning fields

> _Note: this issue was identified against the code base at commit `3fd6090c6180e74798ad464a124672539521deef`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
Three net-new causes in the ECS task read/stop path, all about the terminal task record a client reads back after a Fargate task fails to start or exits. All three are ECS and all three are about the same emitted Task structure.

Cause 1 of 3 — DescribeTasks reports a pull failure as a container exit (fixture ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record, step task#11).
AWS: Smithy: DescribeTasks output Task.stopCode targets enum TaskStopCode whose values are [TaskFailedToStart, EssentialContainerExited, UserInitiated, ServiceSchedulerInitiated, SpotInterruption, TerminationNotice, InfrastructureHealth]; Task.stoppedReason is 'The reason that the task was stopped'; Task.startedAt is 'the time when the task transitioned from the PENDING state to the RUNNING state'. A task whose image reference cannot be resolved never leaves PENDING, so AWS classifies it stopCode=TaskFailedToStart and words stoppedReason with the agent's pull diagnostic — fixed text 'CannotPullContainerError: pull image manifest has been retried N time(s): failed to resolve ref <image>: <image>: not found', whose variable parts are the retry count and the image reference taken from the container definition in the request. Because the task never ran, Task.startedAt is absent.
Emulator: answers 200 with stopCode='EssentialContainerExited', stoppedReason='Essential container in task exited' (a container-exit classification), and populates startedAt.
Why: a client that branches on stopCode cannot distinguish 'never started — fix your image/registry/role' from 'ran and exited', so pull/permission faults are retried as application exits and never surfaced; a startedAt on a task that never ran also makes start-latency and reachability accounting wrong.
Fix: the task lifecycle model behind RunTask/StartTask in ECS state — the transition that terminates a task must classify unresolved-image failures as TaskFailedToStart, carry the pull diagnostic as stoppedReason, and leave startedAt unset when the task never reached RUNNING; DescribeTasks' serializer then just reflects it.
Traces: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#11 (and #task#16, same cluster).

Cause 2 of 3 — StopTask returns the same mis-classified terminal record (fixture ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record, step task#17).
AWS: Smithy: StopTask output is the same Task shape, so stopCode/stoppedReason carry the same requirement as Cause 1 — TaskFailedToStart plus the 'CannotPullContainerError: ...' reason for a task that could not pull its image. Recorded behavior only: in the recording the stopped record for such a task carries no attachments entry, no containers entry and no overrides.containerOverrides entry, because no ENI or container was ever realised for it.
Emulator: answers 200 with stopCode='EssentialContainerExited' and stoppedReason='Essential container in task exited', and additionally emits an attachments[0] (ElasticNetworkInterface, status DELETED), a containers[0] with lastStatus STOPPED and exitCode 0, and an overrides.containerOverrides[0] where the recording has none.
Why: StopTask is the call a client makes to settle a stuck task, so this is where the pull failure should become visible; instead the response both mis-labels the cause and asserts a container ran to completion with exit code 0, which reads as success.
Fix: the StopTask handler and the shared task-record projection in ECS state — derive the stop code/reason from the recorded failure cause, and do not synthesize container/attachment/override members for a task that never had them.
Traces: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#17.

Cause 3 of 3 — DescribeTasks omits the Fargate provisioned size and the container image digest (fixture ecs-fargate-run-task-reports-container-exit-code, step task#12).
AWS: Smithy: Container.imageDigest is 'The container image manifest digest' and is populated once the image has been pulled. The provisionedCpu/provisionedMemory members the recording asserts are not present in the on-disk ECS model, so for those two: recorded behavior only — the recording shows a Fargate task reporting provisionedCpu and provisionedMemory as the CPU-unit and MiB strings matching the task size the task was launched with (the request's/task definition's cpu and memory).
Emulator: answers 200 but omits tasks[0].provisionedCpu, tasks[0].provisionedMemory and tasks[0].containers[0].imageDigest entirely.
Why: a client cannot tell what capacity Fargate actually provisioned for the task (cost/size reconciliation) and cannot pin or audit the exact image manifest that ran, so digest-based deployment verification is impossible.
Fix: the DescribeTasks serializer plus the task record in ECS state — record the resolved manifest digest at pull time and the provisioned task size at launch, and emit all three members.
Note: the timestamp members flagged in the same cluster (createdAt, pullStartedAt/pullStoppedAt, connectivityAt, startedAt, stoppedAt, stoppingAt, executionStoppedAt) are per-run values the fixture does not assert and are NOT part of this issue.
Traces: ecs-fargate-run-task-reports-container-exit-code#task#12.

Related open issues that do NOT carry these replay keys (the replay reports these clusters as unowned): 20261111T131404Z-ec2-ecr-ecs-a-fargate-task-whose-container.md and 20261111T030957Z-ecr-ecs-elbv2-container-image-pull-failures-are.md describe the same family of behaviour from behavioral checks; 20261104T060239Z-ec2-ecr-ecs-a-fargate-task-that-cannot.md covers a task that never reaches a terminal state at all.

## Evidence
Cause 1 — DescribeTasks, expected 200 / actual 200:
  $.tasks[0].stopCode: expected 'TaskFailedToStart', got 'EssentialContainerExited'
  $.tasks[0].stoppedReason: expected 'CannotPullContainerError: pull image manifest has been retried 7 time(s): failed to resolve ref public.ecr.aws/docker/library/busybox:<absent-tag>: public.ecr.aws/docker/library/busybox:<absent-tag>: not found', got 'Essential container in task exited'
  $.tasks[0].startedAt: expected absent, got a timestamp
  (cluster also covers ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#16)

Cause 2 — StopTask, expected 200 / actual 200:
  $.task.stopCode: expected 'TaskFailedToStart', got 'EssentialContainerExited'
  $.task.stoppedReason: expected the 'CannotPullContainerError: ...not found' text, got 'Essential container in task exited'
  $.task.attachments[0]: expected absent, got an ElasticNetworkInterface attachment with status DELETED
  $.task.containers[0]: expected absent, got a container with lastStatus 'STOPPED' and exitCode 0
  $.task.overrides.containerOverrides[0]: expected absent, got {'name': 'app'}

Cause 3 — DescribeTasks, expected 200 / actual 200:
  $.tasks[0].provisionedCpu: expected the task's CPU-unit string (recording: '256'), got None
  $.tasks[0].provisionedMemory: expected the task's MiB string (recording: '512'), got None
  $.tasks[0].containers[0].imageDigest: expected present, got None

## Steps to Reproduce
1. Replay fixture ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record: RunTask a Fargate task whose container image names a tag that does not exist in the registry, wait for it to stop, then DescribeTasks (step task#11) and StopTask (step task#17). Observe stopCode EssentialContainerExited with 'Essential container in task exited' instead of TaskFailedToStart with the CannotPullContainerError text, startedAt set, and synthesized container/attachment/override members on the StopTask response.
2. Replay fixture ecs-fargate-run-task-reports-container-exit-code: RunTask a Fargate task with a resolvable image, let it exit, then DescribeTasks (step task#12). Observe provisionedCpu, provisionedMemory and containers[0].imageDigest absent from the task record.
