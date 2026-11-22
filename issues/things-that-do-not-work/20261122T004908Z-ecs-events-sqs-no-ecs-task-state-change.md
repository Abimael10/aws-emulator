Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs__events__sqs.events-workload-state-change-ecs-exit-code-rules-route-each-stopped-task-and-not-delivered-to-the-other
Issue-Key: ecs__events__sqs.events-workload-state-change-ecs-exit-code-rules-route-each-stopped-task-and-not-delivered-to-the-other|_poll_until#25|Timeout|time.sleep(interval_s)
Issue-Commit: 29c63349654daecf518dc89fab6ca09926c7725f

# ecs__events__sqs: no 'ECS Task State Change' event is published for a Fargate task's RUNNING/STOPPED transitions, so exit-code-scoped EventBridge rules deliver nothing and the selective-routing differential reaches no verdict

> _Note: this issue was identified against the code base at commit `29c63349654daecf518dc89fab6ca09926c7725f`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Cause 1 of 1: ECS publishes no `ECS Task State Change` events for a task's lifecycle, so three EventBridge rules whose patterns differ only in `detail.containers.exitCode` / `detail.lastStatus` all receive nothing, and the check is cut off at the suite's 60 s budget with `Error: Check timed out after 60.0s`.

AWS: two Fargate tasks launched from one task definition with container command overrides `sh -c "sleep 5; exit 7"` and `sh -c "sleep 5; exit 0"` each advance to lastStatus STOPPED, and ECS publishes one `ECS Task State Change` envelope per transition onto the default event bus (source `aws.ecs`, detail-type `ECS Task State Change`, `resources` = exactly the one task ARN). EventBridge then routes each STOPPED envelope ONLY to the rule whose pattern matches that task's `detail.containers[].exitCode`, and both RUNNING envelopes to the rule matching `detail.lastStatus: ["RUNNING"]`. The STOPPED envelope's `detail` agrees with DescribeTasks on the Task shape (Smithy: ecs Container$exitCode, Task$stopCode targeting enum TaskStopCode, Task$stoppedReason, Task$version); for an essential container that exits, `stopCode` is `EssentialContainerExited`. Task$version is documented as incremented on each change that starts a CloudWatch/EventBridge event, so every RUNNING envelope's `detail.version` is strictly lower than the STOPPED envelope's, and a RUNNING envelope's containers carry no `exitCode` member at all. The requirement is the rule and the fixed text — the `aws.ecs`/`ECS Task State Change` source/detail-type pair, the lastStatus values, the `EssentialContainerExited` stop code, the one-task `resources` list and the version ordering; the cluster/task ARNs, exit codes, stoppedReason text and version numbers are values from the request and the service's own state.

Emulator: none of the three SQS rule targets ever receives a message. The check's drain loop polls all three queues until the harness budget is exhausted and it ends with `Error: Check timed out after 60.0s`, so neither the positive deliveries nor the negative controls (the exitCode [0] rule must receive nothing for the task that exited 7, and vice versa) are evaluated, and the DescribeTasks-vs-event field agreement on exitCode/stopCode/stoppedReason/version is never checked.

Why: an event-driven consumer that reacts to a task's exit status — alerting only on non-zero exits, fanning stopped tasks out by exit code, or correlating RUNNING→STOPPED `version` ordering — receives no events at all. Worse, because the negative control is also silent, a client cannot distinguish a correctly scoped rule pattern from a broken one: an empty queue looks the same whether the rule filtered the event out or the event was never published.

Fix: the ECS task-lifecycle component that advances a task through RUNNING and STOPPED must, on each transition, serialize the task as an `ECS Task State Change` envelope (source `aws.ecs`, detail-type `ECS Task State Change`, `resources` = [task ARN], `detail` = the Task record with an incremented `version`, carrying `containers[].exitCode`, `stopCode` and `stoppedReason` on STOPPED and omitting `exitCode` on RUNNING) and publish it to the default event bus so the existing EventBridge pattern-matching and target-delivery path can route it per rule.

Note: open issues 20261120T181150Z-ecs-events-sqs-ecs-task-state-change-events.md and 20261120T154211Z-ecs-events-sqs-ecs-task-state-change-events-carrying-container-exit.md record the same silence, and 20261104T062603Z-ecs-events-sqs-ecs-never-publishes-deployment-or.md records it for deployment events; the suite reports this failure as unowned (its current replay key is not carried by any of them), so it is filed here.

Traces: ecs__events__sqs.events-workload-state-change-ecs-exit-code-rules-route-each-stopped-task-and-not-delivered-to-the-other.

## Evidence
Expected (AWS): the rule with pattern `detail.containers.exitCode: [7]` receives exactly the STOPPED envelope of the task that exited 7 and nothing for the other task; the rule with `detail.containers.exitCode: [0]` receives exactly the STOPPED envelope of the task that exited 0; the rule with `detail.lastStatus: ["RUNNING"]` receives both tasks' RUNNING envelopes. Each STOPPED envelope has `resources` == [that task's ARN], `detail.lastStatus` == "STOPPED", `detail.containers[].exitCode` equal to DescribeTasks' value, `detail.stopCode` == "EssentialContainerExited" equal to DescribeTasks', `detail.stoppedReason` equal to DescribeTasks', and `detail.version` strictly greater than every RUNNING envelope's version; RUNNING envelopes' containers carry no `exitCode`.

Actual (emulator): all three queues stay empty for the whole wait. The check reports `Error: Check timed out after 60.0s` while still draining the three SQS targets, before any routing, field-agreement or version-ordering assertion is reached — so both the positive deliveries and the "not delivered to the other rule" negative controls are unevaluated.

## Steps to Reproduce
1. Create a VPC with a public subnet, security group and internet gateway; CreateCluster; RegisterTaskDefinition (FARGATE, awsvpc, cpu 256, memory 512, one essential container named "work").
2. CreateQueue three times; PutRule + PutTargets three times against the default bus, all scoped to {source:["aws.ecs"], detail-type:["ECS Task State Change"], detail.clusterArn:[<cluster arn>]} plus, respectively, detail {lastStatus:["STOPPED"], containers:{exitCode:[7]}}, detail {lastStatus:["STOPPED"], containers:{exitCode:[0]}}, and detail {lastStatus:["RUNNING"]}.
3. RunTask twice on the cluster with containerOverrides command `sh -c "sleep 5; exit 7"` and `sh -c "sleep 5; exit 0"`.
4. Poll DescribeTasks until both tasks report lastStatus STOPPED with the expected containers[].exitCode, then poll ReceiveMessage on all three queues.
5. No queue ever receives a message; the check is killed at the harness cap instead of reaching a routing verdict.
