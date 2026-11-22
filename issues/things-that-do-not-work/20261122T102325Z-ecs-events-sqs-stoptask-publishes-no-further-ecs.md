Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__events__sqs.a-runtask-that-failed-placement-emits-no-task-state-change-event
Issue-Key: ecs__events__sqs.a-runtask-that-failed-placement-emits-no-task-state-change-event|run#38|AssertionError|assert later, (
Issue-Commit: aa409114eda17ea1bc8a6cd65487c38f901b417a

# ecs__events__sqs: StopTask publishes no further 'ECS Task State Change' event — a placed task's terminal STOPPED transition never reaches the default bus after its first state change

> _Note: this issue was identified against the code base at commit `aa409114eda17ea1bc8a6cd65487c38f901b417a`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopTask

## Description
Cause 1 of 1: ECS publishes at most the first 'ECS Task State Change' event for a placed task and no event for any subsequent transition. After StopTask on that still-transitioning control task, an EventBridge rule scoped to {source:['aws.ecs'], detail-type:['ECS Task State Change'], detail.clusterArn:[<cluster arn>]} with an SQS target receives nothing at all for the whole hold, so the task's terminal STOPPED transition is never published.

AWS: ECS publishes one 'ECS Task State Change' envelope per task lifecycle transition onto the default event bus — source 'aws.ecs', detail-type 'ECS Task State Change', resources = [that task's ARN], detail = the Task record. Smithy (ecs) StopTask is documented as 'Stops a running task... the equivalent of docker stop is issued to the containers running in the task... a stop signal value and a default 30-second timeout, after which the SIGKILL value is sent and the containers are forcibly stopped', so a StopTask on a PENDING/RUNNING task drives it to lastStatus STOPPED and that transition is published, carrying Task$stopCode and Task$stoppedReason (the reason supplied to StopTask) and a Task$version strictly greater than the preceding envelope's — Task$version is documented as incremented on each change that starts a CloudWatch/EventBridge event. The requirement is the per-transition publication and the fixed source/detail-type pair plus the STOPPED lastStatus; the cluster and task ARNs, the stoppedReason text and the version numbers are values from the request and the service's own state.

Emulator: the positive control passes — one 'ECS Task State Change' message for the placed control task does arrive on the queue, with the right source, detail-type and clusterArn, so the ECS -> EventBridge -> SQS path is live in this run. The synchronous InvalidParameterException for the EC2-launch-type RunTask on an empty cluster and the ListTasks check also pass. But after StopTask(cluster, task=<control task>, reason='sim2real control task stop'), a 29 s drain of the queue (3x the measured delivery latency) returns ZERO messages — not one further state change for that same task ARN. The task's STOPPED transition produces no envelope, so the only event the emulator ever emits for a task is its first one.

Why: an event-driven consumer that waits for a task's terminal 'ECS Task State Change' (lastStatus STOPPED, with stopCode/stoppedReason and the container exit codes) after calling StopTask waits forever and must fall back to polling DescribeTasks. Worse, it makes absence unreadable: a rule that is supposed to receive nothing (the negative control here — no event for a RunTask that placed nothing) cannot be distinguished from a delivery path that has simply gone silent, so a client cannot verify that its rule pattern is scoped correctly.

Fix: the ECS task-lifecycle state machine behind StopTask must advance the task through its remaining states to STOPPED and, on EACH transition, serialize the task as an 'ECS Task State Change' envelope (source 'aws.ecs', detail-type 'ECS Task State Change', resources = [task ARN], detail = the Task record with an incremented version, carrying stopCode/stoppedReason and containers[].exitCode on STOPPED) and publish it to the default event bus so the existing EventBridge matching/target-delivery path can route it.

Related open issues record total silence of 'ECS Task State Change' publication rather than this partial, first-event-only behaviour, and do not carry this check's replay key: 20261122T004908Z-ecs-events-sqs-no-ecs-task-state-change.md, 20261120T181150Z-ecs-events-sqs-ecs-task-state-change-events.md and 20261120T154211Z-ecs-events-sqs-ecs-task-state-change-events-carrying-container-exit.md.

Traces: ecs__events__sqs.a-runtask-that-failed-placement-emits-no-task-state-change-event

## Evidence
Check: ecs__events__sqs.a-runtask-that-failed-placement-emits-no-task-state-change-event [mismatch]

Expected (AWS): after StopTask on the placed FARGATE control task, the SQS target of the cluster-scoped rule keeps receiving that task's own 'ECS Task State Change' envelopes — at minimum its terminal lastStatus 'STOPPED' envelope with resources == [control task ARN] — so the hold proves the delivery path stayed live while no envelope naming any OTHER task ARN arrives.

Actual (emulator): the queue receives nothing during the hold:

  AssertionError: the queue received NOTHING during the 29s hold, not even a further state change for the control task arn:aws:ecs:<region>:<account>:task/<cluster>/<task-id> — the delivery path may have died, in which case the absence of stranger events is not attributable to the failed placement

(raised from check_ecs__events__sqs_a_runtask_that_failed_placement_emits_no_task_state_change_event.py line 336.) Earlier beats of the same check succeeded: one state-change envelope for the control task was delivered with source 'aws.ecs', detail-type 'ECS Task State Change' and the expected detail.clusterArn, the EC2-launch-type RunTask was refused synchronously with InvalidParameterException / 'No Container Instances were found in your cluster.', and ListTasks reported only the control task. Only the post-StopTask transitions are missing.

## Steps to Reproduce
1. Create a VPC/subnet, a cluster, and a FARGATE awsvpc task definition.
2. CreateQueue and SetQueueAttributes with a policy allowing events.amazonaws.com sqs:SendMessage; PutRule on the default bus with pattern {source:['aws.ecs'], detail-type:['ECS Task State Change'], detail:{clusterArn:[<cluster arn>]}} and PutTargets with that queue.
3. RunTask(launchType=FARGATE, count=1, awsvpcConfiguration with assignPublicIp DISABLED); poll ReceiveMessage until an 'ECS Task State Change' event for the placed task arrives (it does) and record the latency L.
4. RunTask(launchType=EC2) on the same (instance-less) cluster and confirm the synchronous InvalidParameterException; confirm ListTasks still reports only the control task.
5. StopTask(cluster, task=<control task>, reason='...'), then drain the queue for max(3L, 15) s: no message of any kind arrives, and in particular no envelope with detail.lastStatus 'STOPPED' for the control task.
