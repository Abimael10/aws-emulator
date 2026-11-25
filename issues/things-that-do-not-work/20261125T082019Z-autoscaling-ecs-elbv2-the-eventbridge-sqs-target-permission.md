Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ecs__elbv2.eventbridge-rule-without-target-permission-no-invocation
Issue-Key: autoscaling__ecs__elbv2.eventbridge-rule-without-target-permission-no-invocation|_poll_for_message#21|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: fc90a6588ac011398c7d49cbe030c73842e4efe1

# autoscaling__ecs__elbv2: the EventBridge→SQS target-permission negative-delivery differential never reaches a verdict — no 'ECS Task State Change' event is ever published, so the 60 s cap is consumed by the negative leg alone (1 cause)

> _Note: this issue was identified against the code base at commit `fc90a6588ac011398c7d49cbe030c73842e4efe1`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: the ECS task lifecycle never publishes an `ECS Task State Change` envelope onto the EventBridge default bus, so the permission-boundary differential (no queue policy => no delivery, queue policy granting events.amazonaws.com sqs:SendMessage => delivery) can never be graded. The check's first, negative leg polls ReceiveMessage for the full 60 s it is allowed and the whole check is cut off at `Error: Check timed out after 60.0s` before the positive control (SetQueueAttributes with the policy, then a second RunTask/StopTask) is ever exercised. Failing check: autoscaling__ecs__elbv2.eventbridge-rule-without-target-permission-no-invocation.

AWS: a task placed by RunTask advances PENDING -> RUNNING -> STOPPED and ECS publishes one envelope per transition onto the default event bus with `source` = `aws.ecs` and `detail-type` = `ECS Task State Change`, `detail` carrying the Task (Smithy: ECS `Task`; member `version` is documented as "The version counter for the task. Every time a task experiences a change that starts a CloudWatch event, the version counter is incremented. If you replicate your Amazon ECS task state with CloudWatch Events, you can compare the version of a task reported by the Amazon ECS API actions with the version reported in CloudWatch Events for the task (inside the detail object)..."). Target-side authorization is NOT checked by EventBridge at wiring time (PutTargets succeeds against a queue with no resource policy) — it is checked only at delivery time, so with no queue policy the envelope is produced and the delivery is refused, leaving the queue empty; once a policy allowing the `events.amazonaws.com` service principal `sqs:SendMessage` on that queue ARN is set, the NEXT envelope is delivered and its `detail.taskArn` is the task that was actually run. The fixed text of the requirement is the `source` value `aws.ecs`, the `detail-type` value `ECS Task State Change` and the `lastStatus`/`desiredStatus` enum values; the cluster ARN, task ARN, task-definition ARN, `time` and `version` are values taken from the request and from the service's own state, never literals from one run.

Emulator: zero messages reach the rule's SQS target in either leg. Because nothing is ever published, the negative leg is indistinguishable from a correct refusal and burns the entire 60 s poll budget; the check terminates with `Error: Check timed out after 60.0s` and the positive control — the only leg that can prove the emulator distinguishes "no permission" from "no event" — is never reached. No assertion on the delivered envelope's `detail.taskArn` is reachable.

Why: a client that uses an SQS-targeted EventBridge rule to detect ECS task transitions sees an emulator in which tasks apparently never change state, and cannot tell a genuine IAM/resource-policy boundary from a service that emits nothing. Automation that blocks on the STOPPED envelope (post-stop cleanup, deployment gating, failure alarms) hangs forever instead of completing, and a test that asserts "nothing was delivered without the policy" passes vacuously for the wrong reason.

Fix: the ECS task lifecycle's event-emission component — whatever advances a task's `lastStatus`/`desiredStatus` (RunTask/StopTask placement and state machine) must serialize the Task with its incremented `version` into an `ECS Task State Change` envelope and hand it to the EventBridge default bus, so the existing rule-matching/target-delivery path evaluates the SQS queue policy and the differential has two distinguishable outcomes.

Related already-open issues describe the same silence for a different check and do not carry this failure's replay key (the suite reports it unowned): 20261120T181150Z-autoscaling-ecs-elbv2-a-fargate-task-s-running--r2.md and 20261122T172353Z-autoscaling-ecs-elbv2-the-asg-launch-event-delivery.md.

Traces: autoscaling__ecs__elbv2.eventbridge-rule-without-target-permission-no-invocation.

## Evidence
Expected (AWS):
- Leg 1 (queue has NO resource policy): PutTargets succeeds; RunTask + StopTask produce `ECS Task State Change` envelopes on the default bus; EventBridge refuses the SendMessage at delivery time, so ReceiveMessage on the queue yields nothing for the cluster under test. The check's 60 s negative poll ends having observed no message, and the check CONTINUES.
- Leg 2 (after SetQueueAttributes with Policy allowing Principal {"Service":"events.amazonaws.com"}, Action sqs:SendMessage on that queue ARN): a second RunTask/StopTask yields a message whose body is `{"source":"aws.ecs","detail-type":"ECS Task State Change","detail":{"clusterArn":<the cluster's arn>,"taskArn":<the second task's arn>,"lastStatus":...,"desiredStatus":...,"version":<incrementing>}}`, and `detail.taskArn` equals the task ARN returned by that RunTask.

Actual (emulator): no message is ever delivered in either leg. The run ends with `Error: Check timed out after 60.0s`, reported under `### autoscaling__ecs__elbv2` as [mismatch] eventbridge-rule-without-target-permission-no-invocation. Leg 2 is never executed, so the positive control is unverifiable and the negative leg's "nothing delivered" result is vacuous.

Fixed text vs variable parts: fixed — `source` = `aws.ecs`, `detail-type` = `ECS Task State Change`, the `lastStatus`/`desiredStatus` enum values; variable — cluster ARN, task ARN, task-definition ARN, `time`, `version`, queue URL/ARN and rule name (all taken from the request and service state).

## Steps to Reproduce
1. CreateCluster.
2. RegisterTaskDefinition: networkMode awsvpc, requiresCompatibilities [FARGATE], cpu 256, memory 512, one essential container running a short command, with an ecs-tasks.amazonaws.com execution role.
3. CreateQueue; GetQueueAttributes AttributeNames=[QueueArn]. Do NOT set any queue policy yet.
4. PutRule with EventPattern {"source":["aws.ecs"],"detail-type":["ECS Task State Change"],"detail":{"clusterArn":[<cluster arn>]}}, State ENABLED; PutTargets Id "1" -> the queue ARN (this must succeed — EventBridge does not validate target-side permissions at wiring time).
5. RunTask (launchType FARGATE, count 1, awsvpcConfiguration on a public subnet), then StopTask on the returned taskArn.
6. Poll ReceiveMessage for up to 60 s: expect no envelope for this cluster (delivery refused for lack of a queue policy).
7. SetQueueAttributes with Policy allowing Principal {"Service":"events.amazonaws.com"}, Action "sqs:SendMessage", Resource <queue arn>.
8. RunTask + StopTask again; poll ReceiveMessage: expect an `ECS Task State Change` envelope whose detail.clusterArn is the cluster and whose detail.taskArn is the second task's ARN.

Observed: step 6 never sees a message because none is ever produced, and the check is cut off at the 60 s cap before step 7, so steps 7-8 never run.
