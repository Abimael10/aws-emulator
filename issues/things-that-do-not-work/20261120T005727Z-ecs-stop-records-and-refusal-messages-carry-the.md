Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record
Issue-Step: task#11
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record ecs-fargate-run-task-reports-container-exit-code ecs-capacity-provider-management ecs-cluster-capacity-provider-lifecycle ecs-container-instance-and-attribute-errors
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
Issue-Key: ecs|UpdateCapacityProvider|4xx->4xx|$.message
Issue-Key: ecs|PutAttributes|4xx->4xx|$.message
Issue-Related: 20261110T175605Z-ecs-fargate-runtask-and-describetasks-task-records-omit.md
Issue-Commit: 16caeccf34fd30fafc2febabf9312168f85b2a49

# ECS stop records and refusal messages carry the emulator's own wording instead of AWS's

> _Note: this issue was identified against the code base at commit `16caeccf34fd30fafc2febabf9312168f85b2a49`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
Five ECS answers report the emulator's own notion of what happened rather than AWS's: a task that could never pull its image is recorded as having started and exited, a stopped task's record omits the provisioned-capacity members, and two refusals carry the emulator's prose instead of AWS's message for that error class. 5 causes.

Cause 1 of 5: a Fargate task whose image tag does not exist is recorded as STOPPED with stopCode EssentialContainerExited and a startedAt, instead of TaskFailedToStart with the pull error.
AWS: Smithy: Task$stopCode targets TaskStopCode [TaskFailedToStart, EssentialContainerExited, UserInitiated, ServiceSchedulerInitiated, SpotInterruption, TerminationNotice, InfrastructureHealth]; Task$startedAt is "the time when the task transitioned from the PENDING state to the RUNNING state" and Task$stoppedReason is "The reason that the task was stopped". A task that never pulled its image never reached RUNNING, so AWS reports stopCode 'TaskFailedToStart', NO startedAt at all, and a stoppedReason whose fixed text is the CannotPullContainerError template — 'CannotPullContainerError: pull image manifest has been retried <n> time(s): failed to resolve ref <imageRef>: <imageRef>: not found' — with the retry count and the requested image reference taken from the request.
Emulator: stopCode 'EssentialContainerExited', stoppedReason 'Essential container in task exited', and startedAt present.
Why: the single signal a client uses to distinguish "my image/tag is wrong" from "my container ran and exited" is inverted — CI and deployment tooling that retries or rolls back on TaskFailedToStart/CannotPullContainerError never fires, and the presence of startedAt makes the task look like it ran.
Fix: the task-lifecycle state machine for a task whose image cannot be resolved (the RunTask/task-progression component that sets stopCode, stoppedReason and the startedAt transition).
Traces: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#11.

Cause 2 of 5: StopTask on that same never-started task returns the same wrong stop record and additionally populates members AWS leaves empty for a task that never started.
AWS: the StopTask response echoes the task with stopCode 'TaskFailedToStart' and the CannotPullContainerError stoppedReason; for a task that never started, the recording carries NO `attachments` entry, NO `containers` entry and NO `overrides.containerOverrides` entry.
Emulator: returns stopCode 'EssentialContainerExited' / 'Essential container in task exited' plus an ElasticNetworkInterface attachment, a container entry carrying `exitCode: 0`, and a containerOverrides entry.
Why: a client reading the StopTask response concludes the container ran to completion with exit status 0, the exact opposite of the pull failure AWS reports; an ENI attachment is also reported for a task that never got one.
Fix: as cause 1 — the stop path must render the never-started task's record, not a synthetic exited-container record.
Traces: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#17.

Cause 3 of 5: DescribeTasks omits a stopped Fargate task's provisionedCpu / provisionedMemory and its container's imageDigest, and serializes every task timestamp at sub-millisecond precision.
AWS: recorded behavior only for the members' presence — the recording carries `tasks[0].provisionedCpu` and `tasks[0].provisionedMemory` as the strings the task was provisioned with (the task's cpu/memory, e.g. '256'/'512') and `tasks[0].containers[0].imageDigest` for a container whose image was pulled. Smithy: Task$connectivityAt / createdAt / executionStoppedAt / pullStartedAt / pullStoppedAt / startedAt / stoppedAt / stoppingAt are Timestamps, serialized elsewhere in this corpus with at most millisecond precision (e.g. 1791557500.895).
Emulator: provisionedCpu, provisionedMemory and containers[0].imageDigest are absent from the response, and all eight timestamp members are emitted as 7-decimal floats, failing their shape assertions. Hypothesis: the task state never records the provisioned sizes or the resolved image digest, and timestamps are serialized from raw f64 clock readings. The timestamp divergence is the precision/shape, not the instants.
Why: a client that bills or sizes from provisionedCpu/provisionedMemory, or that pins the exact image a task ran by imageDigest, gets nothing to read.
Fix: the task state (record the provisioned cpu/memory and the resolved image digest) and the Timestamp serializer used by the task rendering.
Traces: ecs-fargate-run-task-reports-container-exit-code#task#12.

Cause 4 of 5: UpdateCapacityProvider on a reserved built-in provider answers the does-not-exist message instead of AWS's reserved-provider refusal.
AWS: Smithy: UpdateCapacityProvider models ClientException, InvalidParameterException, ClusterNotFoundException, UnsupportedFeatureException, ServerException. AWS refuses an attempt to modify a reserved provider (FARGATE / FARGATE_SPOT) with HTTP 400 and the fixed message 'The specified capacity provider is reserved and cannot be modified by this operation. Specify a valid name or ARN and try again.' — all fixed text, no per-request values.
Emulator: HTTP 400 with 'The specified capacity provider does not exist. Specify a valid name or ARN and try again.' — the not-found message for a provider that does exist.
Why: a client that provisions capacity providers idempotently reads "does not exist" as a cue to CREATE the provider and retries into a different failure, instead of learning the provider is reserved and must be left alone.
Fix: the UpdateCapacityProvider handler's reserved-provider precondition, which must be checked before the existence lookup. Related but NOT covering: open issue 20261118T112931Z-ecs-2-net-new-causes-updatecapacityprovider-reports-the.md describes this same refusal but carries none of this cluster's replay keys.
Traces: ecs-capacity-provider-management#task#6.

Cause 5 of 5: PutAttributes with a short-form targetId and no targetType is refused with the emulator's own prose instead of AWS's message.
AWS: Smithy: PutAttributes input `attributes` is @required (a list of Attribute); modeled errors include InvalidParameterException, TargetNotFoundException, ClusterNotFoundException. AWS refuses the request with HTTP 400 and the fixed message 'TargetType is required when targetId is not a fully qualified ARN.' — all fixed text.
Emulator: HTTP 400 with 'attributes.targetType is required when targetId is the short form id of a resource'.
Why: a client that matches AWS's message to repair the request by adding targetType does not recognise the refusal, and the wording is not AWS's.
Fix: the PutAttributes handler's attribute validation. Related but NOT covering: open issues 20261114T034444Z-ecs-putattributes-stub.md and 20261118T011827Z-ecs-the-attribute-apis-refusals-diverge-from-aws.md — PutAttributes answers now, and neither carries this cluster's replay key.
Traces: ecs-container-instance-and-attribute-errors#task#6.

## Evidence
Cause 1 — ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#11, DescribeTasks, 200 vs 200: `$.tasks[0].stopCode` expected 'TaskFailedToStart', got 'EssentialContainerExited'; `$.tasks[0].stoppedReason` expected 'CannotPullContainerError: pull image manifest has been retried 7 time(s): failed to resolve ref public.ecr.aws/docker/library/busybox:<tag>: public.ecr.aws/docker/library/busybox:<tag>: not found', got 'Essential container in task exited'; `$.tasks[0].startedAt` expected absent, got 1795135506.1035087. (Cluster also covers task#16.)
Cause 2 — same fixture #task#17, StopTask, 200 vs 200: `$.task.stopCode` and `$.task.stoppedReason` as above; `$.task.attachments[0]` expected None, got an ElasticNetworkInterface attachment; `$.task.containers[0]` expected None, got a container with 'exitCode': 0; `$.task.overrides.containerOverrides[0]` expected None, got {'name': 'app'}.
Cause 3 — ecs-fargate-run-task-reports-container-exit-code#task#12, DescribeTasks, 200 vs 200: `$.tasks[0].provisionedCpu` expected '256', got None; `$.tasks[0].provisionedMemory` expected '512', got None; `$.tasks[0].containers[0].imageDigest` missing; `$.tasks[0].connectivityAt`, `createdAt`, `executionStoppedAt`, `pullStartedAt`, `pullStoppedAt`, `startedAt`, `stoppedAt`, `stoppingAt` all fail their shape assertions (e.g. got 1795135362.903892).
Cause 4 — ecs-capacity-provider-management#task#6, UpdateCapacityProvider, 400 vs 400: `$.message` expected 'The specified capacity provider is reserved and cannot be modified by this operation. Specify a valid name or ARN and try again.', got 'The specified capacity provider does not exist. Specify a valid name or ARN and try again.' (cluster also covers ecs-cluster-capacity-provider-lifecycle#task#11).
Cause 5 — ecs-container-instance-and-attribute-errors#task#6, PutAttributes, 400 vs 400: `$.message` expected 'TargetType is required when targetId is not a fully qualified ARN.', got 'attributes.targetType is required when targetId is the short form id of a resource'.
The image reference, retry count, cpu/memory strings and instants above are the recorded run's values filling the templates; the requirements are the stopCode value, the CannotPullContainerError message template, the presence of the provisioned/digest members, and the two fixed refusal messages.

## Steps to Reproduce
1. Replay fixture ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record: run a Fargate task whose image tag does not exist, then DescribeTasks (task#11) and StopTask (task#17) — the emulator reports stopCode EssentialContainerExited / 'Essential container in task exited', a startedAt, an ENI attachment and a container with exitCode 0, where AWS reports TaskFailedToStart with the CannotPullContainerError reason and none of those members.
2. Replay ecs-fargate-run-task-reports-container-exit-code step task#12 (DescribeTasks on the stopped task) — provisionedCpu, provisionedMemory and containers[0].imageDigest are absent and all task timestamps carry 7 decimal places.
3. Replay ecs-capacity-provider-management step task#6: UpdateCapacityProvider naming a reserved provider (FARGATE/FARGATE_SPOT) -> expect the 'is reserved and cannot be modified by this operation' message, emulator returns the 'does not exist' message.
4. Replay ecs-container-instance-and-attribute-errors step task#6: PutAttributes with a short-form targetId and no targetType -> expect 'TargetType is required when targetId is not a fully qualified ARN.'
