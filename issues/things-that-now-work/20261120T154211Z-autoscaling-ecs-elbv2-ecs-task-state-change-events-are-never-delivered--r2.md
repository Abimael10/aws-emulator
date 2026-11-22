Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ecs__elbv2.ecs-task-state-change-event-delivered-to-eventbridge-rule
Issue-Key: autoscaling__ecs__elbv2.ecs-task-state-change-event-delivered-to-eventbridge-rule|_collect_task_events#27|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: a14965d6060559849ea419b74faa25e7836c4558

# ECS Task State Change events are never delivered to an EventBridge rule target for a real Fargate task lifecycle

> _Note: this issue was identified against the code base at commit `a14965d6060559849ea419b74faa25e7836c4558`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: ECS does not emit the `ECS Task State Change` event stream that EventBridge rules match on, so a rule targeting an SQS queue receives nothing for the whole life of a task.

AWS: a task started with RunTask advances through the task lifecycle and ECS publishes one `ECS Task State Change` event to the default event bus per transition — the RUNNING transition and then the STOPPED transition — with `source` = `aws.ecs`, `detail-type` = `ECS Task State Change`, and a `detail` object that is the Task shape (Smithy: ECS `Task` with `lastStatus`, `desiredStatus`, `clusterArn`, `taskArn`, `taskDefinitionArn`, `containers`, and the monotonically increasing `version` counter the model documents as "incremented" on "a change that starts a CloudWatch event"). An EventBridge rule whose pattern matches that source/detail-type delivers each event to its target, and the events arrive in chronological order (RUNNING before STOPPED).

Emulator: nothing is ever delivered. The check waits on the target SQS queue for the RUNNING and STOPPED events and the whole check exhausts its budget — `Check timed out after 60.0s` — because no `ECS Task State Change` event is ever put on the bus, so the rule never fires and the queue stays empty.

Why: a client that observes task lifecycle through EventBridge (the documented way to drive deployment automation, task-failure alarms, and cleanup handlers) sees an emulator in which tasks never change state. Nothing downstream of the event bus ever runs, and a consumer that blocks until the STOPPED event arrives hangs instead of completing — exactly the failure mode here.

Fix: the ECS task state machine's event emission — the component that transitions a task's `lastStatus`/`desiredStatus` must publish an `ECS Task State Change` envelope (source `aws.ecs`, detail-type `ECS Task State Change`, detail = the serialized Task with the incremented `version`) onto the default event bus so the EventBridge rule-matching path can route it. Related open issues describe the same silence from other entry points (20261104T062603Z-ecs-events-sqs-ecs-never-publishes-deployment-or.md, 20261104T060239Z-cloudwatch-ecr-ecs-fargate-tasks-never-leave-running.md); neither carries this check's replay key.

Traces: autoscaling__ecs__elbv2.ecs-task-state-change-event-delivered-to-eventbridge-rule

## Evidence
Expected: two messages on the rule's SQS target, in order — `{"source":"aws.ecs","detail-type":"ECS Task State Change","detail":{"lastStatus":"RUNNING",...}}` then the same envelope with `"lastStatus":"STOPPED"` and a higher `detail.version`. The fixed text is the `source`/`detail-type` pair and the `lastStatus` values; the taskArn, clusterArn, timestamps and version numbers are values taken from the request and the emulator's own state.

Actual: zero messages ever appear on the queue; the check never gets past the first wait and ends with `Check timed out after 60.0s`.

## Steps to Reproduce
1. CreateCluster; RegisterTaskDefinition (awsvpc, FARGATE, cpu 256 / memory 512, one container with a short-lived command).
2. CreateQueue; PutRule with an event pattern selecting source `aws.ecs` and detail-type `ECS Task State Change`; PutTargets pointing at the queue.
3. RunTask on the cluster.
4. Poll ReceiveMessage on the queue for the RUNNING and then the STOPPED `ECS Task State Change` event. No message ever arrives.
