Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: autoscaling__ecs__elbv2.eventbridge-rule-without-target-permission-no-invocation
Issue-Key: autoscaling__ecs__elbv2.eventbridge-rule-without-target-permission-no-invocation|_poll_for_message#21|Timeout|resp = sqs.receive_message(QueueUrl=queue_url, MaxNumberOfMessages=10, WaitTimeSeconds=5)
Issue-Commit: 861d02363efbdafabe99fc79477849a03b9381e9

# autoscaling__ecs__elbv2: the EventBridge negative-delivery differential for an SQS target with no queue policy never reaches a verdict — no 'ECS Task State Change' event is ever published, so the check is cut at the 60 s harness budget

> _Note: this issue was identified against the code base at commit `861d02363efbdafabe99fc79477849a03b9381e9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
One root cause, grounded in the single failing check the suite lists under `### autoscaling__ecs__elbv2` that is marked neither ALREADY FILED nor OWNED.

Cause 1 of 1: ECS publishes no `ECS Task State Change` event for a task's lifecycle transitions, so an EventBridge rule matching source `aws.ecs` with an SQS target delivers nothing — neither in the negative phase (queue has no resource policy) nor in the positive control that follows (policy granting events.amazonaws.com sqs:SendMessage added). The check therefore makes no observable progress and is killed by the harness, so the permission-boundary differential it exists to grade is unreachable.
  AWS: Smithy (ecs) models RunTask -> RunTaskResponse{tasks:[Task{taskArn, clusterArn, lastStatus, desiredStatus, ...}]} and StopTask -> StopTaskResponse{task}; the model says nothing about event emission, so for the eventing contract this is recorded behavior only. Recorded behavior: a task started with RunTask and then stopped emits one EventBridge envelope per lifecycle transition with the fixed text `source: "aws.ecs"` and `detail-type: "ECS Task State Change"`, and a `detail` whose member names are fixed (taskArn, clusterArn, lastStatus, desiredStatus) while their values are per-run values taken from the request and from ECS state. EventBridge does not validate target-side permissions at wiring time: PutTargets with an SQS target whose queue has no resource policy answers 200, and the refusal happens only at delivery time, so the queue receives nothing while the policy is absent. Once a queue policy allowing Principal {"Service": "events.amazonaws.com"} to sqs:SendMessage on that queue ARN is set with SetQueueAttributes, the NEXT matching envelope is delivered to the queue and its detail.taskArn is the task that was actually run. Both phases complete well inside the collection windows the check uses.
  Emulator: nothing is ever delivered in either phase. The replay output for the check is `Error: Check timed out after 60.0s` — the harness kills the check before any assertion is reported, i.e. before the negative-control poll (60 s of ReceiveMessage looking for an envelope whose detail.clusterArn is the cluster) can even return its 'no delivery' verdict, let alone the positive control after the queue policy is granted. No `aws.ecs` / `ECS Task State Change` envelope reaches the rule's target at any point, so the emulator's 'no delivery' in the negative phase is indistinguishable from its 'no delivery' in the positive phase and says nothing about permission enforcement.
  Why: a client that wires an EventBridge rule on ECS task lifecycle events and waits for the envelope waits forever against the emulator instead of receiving the event AWS sends. Worse for correctness work, the permission boundary is unobservable: granting or withholding the SQS queue policy makes no difference, so code (and tests) that rely on a missing target-side policy being the reason an event did not arrive — dead-letter wiring, least-privilege rollouts, task-completion handlers — get a silent, permanently empty queue and cannot tell a permission denial from a service that emits no events at all.
  Fix: ECS's event emission for task state transitions (the task state machine must publish an `aws.ecs` / `ECS Task State Change` event per transition onto the EventBridge bus) and the EventBridge rule-matching / SQS target-delivery path that carries it, including consulting the target queue's resource policy at delivery time rather than at PutTargets time. Related open issue 20261108T173529Z-autoscaling-ecs-elbv2-a-fargate-task-s-running.md records the same family of defect (no ECS Task State Change event published) for a DIFFERENT check id and does not carry this check's replay key; 20261107T080429Z-autoscaling-ecs-elbv2-6-net-new-causes-asg.md owns the sibling mismatched-pattern negative-delivery check, not this one.
  Traces: autoscaling__ecs__elbv2.eventbridge-rule-without-target-permission-no-invocation.

## Evidence
Expected (AWS): put_targets with an SQS target whose queue has no resource policy answers 200; run_task + stop_task emit `source: "aws.ecs"` / `detail-type: "ECS Task State Change"` envelopes; while the queue policy is absent the rule's SQS target receives NO envelope whose detail.clusterArn is the cluster (the negative control completes and asserts non-delivery); after set_queue_attributes installs a policy allowing Principal {"Service":"events.amazonaws.com"} to sqs:SendMessage on that queue ARN, the next run_task/stop_task's envelope IS delivered and its detail.taskArn equals the task just run.

Actual (emulator): `Error: Check timed out after 60.0s`. The check is killed at the harness budget with no assertion reached — ReceiveMessage returns no matching envelope at any time, in the no-policy phase and in the with-policy positive control alike, so no `ECS Task State Change` envelope is ever published or delivered and the no-permission vs. permission-granted differential never reaches a verdict.

## Steps to Reproduce
1. ec2: pick the shared VPC's public subnet and its default security group.
2. iam create-role (trust ecs-tasks.amazonaws.com) + attach AmazonECSTaskExecutionRolePolicy.
3. ecs create-cluster; ecs register-task-definition --network-mode awsvpc --requires-compatibilities FARGATE --cpu 256 --memory 512 with one essential container running `sleep 20` and that execution role.
4. sqs create-queue; read its QueueArn with get-queue-attributes. Do NOT set any queue policy yet.
5. events put-rule --event-pattern '{"source":["aws.ecs"],"detail-type":["ECS Task State Change"],"detail":{"clusterArn":["<cluster arn>"]}}' --state ENABLED; events put-targets --rule <rule> --targets Id=1,Arn=<queue arn> (expect 200).
6. ecs run-task --launch-type FARGATE --network-configuration awsvpcConfiguration={subnets=[<subnet>],securityGroups=[<sg>],assignPublicIp=ENABLED}, then ecs stop-task on it; poll sqs receive-message for up to 60 s for an envelope whose detail.clusterArn is the cluster -> AWS: none (correct non-delivery).
7. sqs set-queue-attributes --attributes Policy='{"Version":"2012-10-17","Statement":[{"Sid":"AllowEventBridgeSend","Effect":"Allow","Principal":{"Service":"events.amazonaws.com"},"Action":"sqs:SendMessage","Resource":"<queue arn>"}]}'.
8. ecs run-task + stop-task again; poll sqs receive-message -> AWS: an envelope arrives with source aws.ecs, detail-type 'ECS Task State Change' and detail.taskArn == the second task. Emulator: nothing arrives in step 6 or step 8 and the check is cut at the 60 s budget.
Replay autoscaling__ecs__elbv2.eventbridge-rule-without-target-permission-no-invocation.
