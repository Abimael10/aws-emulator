Issue-Source: trace-sweep-divergence
Issue-Trace: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record
Issue-Step: task#16
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].attachments[*].status
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].containers[*].lastStatus
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].desiredStatus
Issue-Key: ecs|DescribeTasks|2xx->2xx|$.tasks[*].lastStatus
Issue-Key: ecs|ListTasks|2xx->2xx|$.taskArns[*]
Issue-Key: ecs|StopTask|2xx->2xx|$.task.executionStoppedAt
Issue-Key: ecs|StopTask|2xx->2xx|$.task.lastStatus
Issue-Key: ecs|StopTask|2xx->2xx|$.task.stoppedAt
Issue-Key: ecs|DescribeServices|2xx->2xx|$
Issue-Related: 20261110T175605Z-ecs-service-deployments-never-reflect-the-circuit-breaker.md
Issue-Related: 20261115T113730Z-ecs-stopped-task-records-lose-image-pull-failure.md
Issue-Commit: 1d4ff0ec19313d445ee1c97e91766984f5ed6bfa

# ECS tasks and deployments never reach a failure terminal state: a task that cannot pull its image stays RUNNING forever

> _Note: this issue was identified against the code base at commit `1d4ff0ec19313d445ee1c97e91766984f5ed6bfa`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeTasks

## Description
Five net-new causes with one shared fault: the emulator's task plane has no failure path. A task whose container image cannot be pulled, and a deployment whose tasks never become healthy, are both reported as healthy and RUNNING indefinitely, so no client can ever observe the failure AWS reports.

Cause 1 of 5: DescribeTasks reports a task whose image tag does not exist as RUNNING, with no stop fields.
AWS: Smithy ECS DescribeTasks -> tasks: Tasks (list of Task). Task carries lastStatus, desiredStatus, stopCode (TaskStopCode enum: TaskFailedToStart, EssentialContainerExited, UserInitiated, ServiceSchedulerInitiated, SpotInterruption, TerminationNotice, InfrastructureHealth), stoppedReason, stoppingAt, stoppedAt, executionStoppedAt, startedAt, containers[].lastStatus and attachments[].status. Recorded behavior for the image-pull failure: after the agent exhausts its pull retries the task transitions to lastStatus/desiredStatus STOPPED with stopCode 'TaskFailedToStart', stoppedReason 'CannotPullContainerError: pull image manifest has been retried <n> time(s): failed to resolve ref <image>: <image>: not found' (the fixed text is 'CannotPullContainerError: pull image manifest has been retried ', ' time(s): failed to resolve ref ' and ': not found'; the retry count and the image reference from the task definition are the variable parts), containers[0].lastStatus STOPPED, the ENI attachment status DELETED, stoppingAt/stoppedAt/executionStoppedAt set, and startedAt never set because the task never started.
Emulator: DescribeTasks answers 200 with $.tasks[0].lastStatus / desiredStatus 'RUNNING', containers[0].lastStatus 'RUNNING', attachments[0].status 'ATTACHED', stopCode / stoppedReason / stoppedAt / stoppingAt / executionStoppedAt all absent, and startedAt present.
Why: a client that waits for a task to reach STOPPED and reads stoppedReason to report a pull failure waits forever and never learns why its deployment is broken; tasks-stopped waiters and CI gates hang instead of failing.
Fix: the task state machine — model the image-pull failure as a start failure that drives the task to STOPPED with stopCode TaskFailedToStart, the CannotPullContainerError reason and the stopping/stopped/executionStopped timestamps, and detach the ENI.
Traces: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#16 (DescribeTasks).

Cause 2 of 5: ListTasks keeps listing the unpullable task as a live task.
AWS: Smithy ECS ListTasks -> taskArns: StringList. Recorded behavior: once the task has stopped, the default (RUNNING desired-status) listing for the cluster is empty.
Emulator: ListTasks answers 200 with $.taskArns[0] present (the task ARN) where the recording has no entry, on four consecutive steps.
Why: a reconcile loop counting live tasks sees phantom capacity and never scales or fails out.
Fix: the ListTasks handler's desiredStatus filtering, together with the task state machine of cause 1.
Traces: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#12 (cluster also covers task#13, task#14, task#15).

Cause 3 of 5: StopTask's returned task does not carry the task's real stop record.
AWS: Smithy ECS StopTask -> output task: Task. Recorded behavior: stopping a task that already failed to start returns the task with lastStatus STOPPED, stopCode 'TaskFailedToStart' and the CannotPullContainerError stoppedReason (the pre-existing failure, not the caller's reason), stoppedAt/executionStoppedAt set, and no containers or attachments left on the record.
Emulator: StopTask answers 200 with $.task.lastStatus 'DEACTIVATING', stopCode 'UserInitiated', stoppedReason set to the caller's reason string, stoppedAt/executionStoppedAt absent, and an extra attachments[0] (ATTACHED ENI), containers[0] and overrides.containerOverrides[0] on the record.
Why: the response tells the caller it stopped a healthy task on request, erasing the real failure cause a client reads from stopCode/stoppedReason.
Fix: the StopTask handler — preserve an existing terminal stop record instead of overwriting it with UserInitiated, and serialize the stopped task's shape.
Traces: ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#17 (StopTask).

Cause 4 of 5: a deployment with the circuit breaker enabled and rollback disabled never reports a failed rollout.
AWS: Smithy ECS DescribeServices -> services: Services; Service.deployments[].rolloutState targets DeploymentRolloutState (COMPLETED, FAILED, IN_PROGRESS) with rolloutStateReason. Recorded behavior: when deploymentCircuitBreaker.enable is true and rollback is false and the deployment's tasks cannot start, the PRIMARY deployment settles at rolloutState 'FAILED' with rolloutStateReason 'ECS deployment circuit breaker: tasks failed to start.' and runningCount 0 on both the service and the deployment.
Emulator: DescribeServices answers 200 but the service never leaves a healthy state — runningCount 1, deployments[0].rolloutState 'COMPLETED', rolloutStateReason 'ECS deployment <deployment id> completed.' — and the fixture's poll for the failed state is capped at 180s without ever matching.
Why: a deployment that AWS reports as failed is reported as succeeded, so a client gates a release on a deployment that never actually ran a task; it is also the slowest possible failure mode, since every waiter burns its full timeout.
Fix: the service scheduler / deployment state — honour deploymentCircuitBreaker when tasks fail to start and drive the deployment to FAILED with the circuit-breaker reason. Related but not covering: open issue 20261110T175605Z-ecs-service-deployments-never-reflect-the-circuit-breaker.md carries other steps of the same fixture; this cluster is unowned.
Traces: ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop#task#18 (DescribeServices).

Cause 5 of 5: the tasks of the circuit-broken deployment are never drained.
AWS: Smithy ECS ListTasks -> taskArns: StringList. Recorded behavior: after the circuit breaker stops the deployment, the cluster's RUNNING task listing is empty.
Emulator: ListTasks answers 200 and keeps returning one task ARN; the fixture polls 19 times over the 180s cap and the list never empties.
Why: the cluster never returns to a quiescent state, so teardown and drain checks cannot complete.
Fix: same scheduler/task state as cause 4 — stop and deregister the failed deployment's tasks.
Traces: ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop#task#23 (ListTasks).

## Evidence
Replay of fixture corpus simparity-wire-aws-v1 (score 91.6%, emulator up).

Cause 1 — ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#16, DescribeTasks, expected 200 / actual 200: $.tasks[0].lastStatus and $.tasks[0].desiredStatus value_mismatch (expected 'STOPPED', got 'RUNNING'); $.tasks[0].containers[0].lastStatus (expected 'STOPPED', got 'RUNNING'); $.tasks[0].attachments[0].status (expected 'DELETED', got 'ATTACHED'); $.tasks[0].stopCode missing (expected 'TaskFailedToStart'); $.tasks[0].stoppedReason missing (expected 'CannotPullContainerError: pull image manifest has been retried 7 time(s): failed to resolve ref <image>: <image>: not found'); $.tasks[0].stoppingAt / stoppedAt / executionStoppedAt missing; $.tasks[0].startedAt unexpected_present. The retry count and image reference are variable parts of the message; the recorded timestamps are one run's values.
Cause 2 — ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#12, ListTasks, 200/200: $.taskArns[0]: extra (expected None, got the task ARN). Cluster also covers task#13, task#14, task#15.
Cause 3 — ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record#task#17, StopTask, 200/200: $.task.lastStatus (expected 'STOPPED', got 'DEACTIVATING'); $.task.stopCode (expected 'TaskFailedToStart', got 'UserInitiated'); $.task.stoppedReason (expected the CannotPullContainerError text, got the caller's reason string); $.task.stoppedAt and $.task.executionStoppedAt missing; $.task.attachments[0], $.task.containers[0] and $.task.overrides.containerOverrides[0] extra.
Cause 4 — ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop#task#18, DescribeServices, 200/200: poll capped at 180s; expected services[0].runningCount 0 and deployments[0] {rolloutState 'FAILED', rolloutStateReason 'ECS deployment circuit breaker: tasks failed to start.', runningCount 0}, got runningCount 1 with deployments[0].rolloutState 'COMPLETED' / rolloutStateReason 'ECS deployment <id> completed.' and stabilityStatus STEADY_STATE.
Cause 5 — ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop#task#23, ListTasks, 200/200: poll: 1 item(s), not 0 after 19 attempt(s) — $.taskArns expected [], got one task ARN.

## Steps to Reproduce
1. Replay fixture ecs-ecsnrt-traj-fargate-unpullable-tag-stopped-record against the emulator: it runs a FARGATE task whose task definition names an image tag that does not exist. At step task#12 ListTasks still lists the task, and at step task#16 DescribeTasks reports lastStatus 'RUNNING' with no stopCode/stoppedReason.
2. Continue the same fixture to step task#17 and call StopTask on that task: the response reports stopCode 'UserInitiated' and the caller's reason instead of the pre-existing TaskFailedToStart / CannotPullContainerError record.
3. Replay fixture ecs-ecsdep-traj-circuit-breaker-rollback-vs-stop to step task#18: create a service with deploymentCircuitBreaker {enable: true, rollback: false} on a task definition whose tasks cannot start, then poll DescribeServices — the deployment settles at rolloutState 'COMPLETED' and never reaches 'FAILED'.
4. In the same fixture, step task#23 polls ListTasks for an empty cluster; the failed deployment's task is never drained.
