Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.capprov-base-and-weight-place-containers-that-really-run ecs.deployment-circuit-breaker-rolls-back-to-last-good ecs.update-task-protection-extends-existing-expiration ecs.submit-attachment-state-changes-updates-attachment-status ecs.deploy-ecs-capacity-provider-cfn-usable-for-run-task
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.capprov-base-and-weight-place-containers-that-really-run|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Key: ecs.deployment-circuit-breaker-rolls-back-to-last-good|_poll_until#27|Timeout|time.sleep(interval_s)
Issue-Key: ecs.update-task-protection-extends-existing-expiration|run#16|Timeout|time.sleep(2)
Issue-Key: ecs.submit-attachment-state-changes-updates-attachment-status|_poll_until#14|Timeout|last = probe()
Issue-Key: ecs.deploy-ecs-capacity-provider-cfn-usable-for-run-task|run#24|KeyError|assert task["containerInstanceArn"] == container_instance_arn, (
Issue-Commit: e6dce30b243860da21c421a4fb52b6fdf5624066

# ecs: 5 net-new causes — capacity-provider base/weight placement, circuit-breaker rollback, task-protection expiry refresh, ENI attachment state changes and containerInstanceArn on a CFN capacity-provider task all fail to converge

> _Note: this issue was identified against the code base at commit `e6dce30b243860da21c421a4fb52b6fdf5624066`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Five net-new root causes in the ECS emulator, each grounded in a distinct failing behavioral check. All five are in the ECS service, so they are bundled here; a fixer must close ALL FIVE before submitting.

--- Cause 1 of 5: RunTask with a base/weight capacityProviderStrategy never produces placed, actually-running tasks ---
AWS: Smithy: RunTask.capacityProviderStrategy -> CapacityProviderStrategy (list of CapacityProviderStrategyItem{capacityProvider, weight, base}); RunTask returns tasks[] whose Task$capacityProviderName names the provider each task was placed on. AWS splits a count across the strategy deterministically: base=1 on one provider takes the first task, and a provider with weight=0 receives no share beyond its base, so count=3 over [{FARGATE_SPOT, base:1, weight:0},{FARGATE, base:0, weight:1}] must place exactly 1 task on FARGATE_SPOT and 2 on FARGATE, and each placed task's container must actually execute (exit 0 and write its marker and its own task ARN from the task-metadata endpoint).
Emulator: the check makes no observable progress and is cut off at the replay ceiling ('Check timed out after 60.0s'), so RunTask(count=3) with that strategy never yields three tasks that reach a terminal, container-executed outcome. No per-provider split and no in-container execution is ever observable.
Why: a client that sizes a fleet with base/weight (the standard FARGATE + FARGATE_SPOT cost split) cannot tell which provider its tasks landed on, and a client that waits for the tasks' work to complete waits forever.
Fix: the RunTask handler's capacity-provider placement path plus the task-lifecycle/data-plane state that advances a placed task to a container-executed terminal status.
Traces: ecs.capprov-base-and-weight-place-containers-that-really-run

--- Cause 2 of 5: the deployment circuit breaker never fails a crash-looping deployment nor rolls back to the last good revision ---
AWS: Smithy: DeploymentCircuitBreaker{enable (required), rollback (required), resetOnHealthyTask, thresholdConfiguration} — 'The deployment circuit breaker determines whether a service deployment will fail if the service can't reach a steady state. If it is turned on, a service deployment will transition to a failed state and stop launching new tasks. You can also configure Amazon ECS to roll back your service to the last completed deployment after a failure.' With enable=true and rollback=true, a revision whose container crash-loops must drive its deployment to rolloutState FAILED, stop launching new tasks, and leave the PRIMARY deployment back on the previous (last completed) revision with rolloutState COMPLETED.
Emulator: the check makes no observable progress and is cut off at the replay ceiling ('Check timed out after 60.0s'); the deployment never reaches a FAILED rolloutState and no rollback to the last good revision is ever observable.
Why: a client relying on the circuit breaker to abort a bad rollout gets a deployment that neither fails nor rolls back, so a broken revision is reported as in-flight indefinitely and automated rollout gates never fire.
Fix: the service-deployment state machine (circuit-breaker failure accounting and rollback to the last completed deployment) behind CreateService/UpdateService and DescribeServices' deployments[].rolloutState.
Traces: ecs.deployment-circuit-breaker-rolls-back-to-last-good

--- Cause 3 of 5: re-applying UpdateTaskProtection does not move expirationDate forward, and GetTaskProtection never reports a refreshed date ---
AWS: Smithy: UpdateTaskProtection{cluster (required), tasks (required), protectionEnabled (required), expiresInMinutes} -> protectedTasks[] of ProtectedTask{taskArn, protectionEnabled, expirationDate: 'The epoch time when protection for the task will expire.'}; GetTaskProtection reports the same ProtectedTask. expiresInMinutes is counted from the moment of the call, so calling UpdateTaskProtection twice with the SAME expiresInMinutes separated by elapsed wall-clock time must yield a second expirationDate later than the first by about the elapsed time, and GetTaskProtection must report that later date. Throughout the interval protectionEnabled must stay true and the protection must not expire.
Emulator: the check makes no observable progress and is cut off at the replay ceiling ('Check timed out after 60.0s'), so neither the stable-protection hold nor the refreshed expirationDate is ever observable — the emulator never gets a protected RUNNING service task through the sequence.
Why: a client that keeps a task protected from scale-in by periodically re-applying UpdateTaskProtection with the same expiresInMinutes cannot see its protection window extend, so it cannot tell whether its task is still protected and may lose a task mid-work.
Fix: the UpdateTaskProtection/GetTaskProtection handlers and the task-protection expiry state they keep (expirationDate must be recomputed from the time of each call), together with the service task-convergence path they read.
Traces: ecs.update-task-protection-extends-existing-expiration

--- Cause 4 of 5: SubmitAttachmentStateChanges does not move an awsvpc task's ENI attachment from ATTACHING to ATTACHED ---
AWS: Smithy: SubmitAttachmentStateChanges{cluster, attachments (required, AttachmentStateChanges)} -> {acknowledgment}. Attachment$status documents the valid values PRECREATED, CREATED, ATTACHING, ATTACHED, DETACHING, DETACHED, DELETED, FAILED. When the ECS agent reports an ElasticNetworkInterface attachment as ATTACHED, the recorded behavior is that the call is acknowledged and DescribeTasks then reports that task's attachments[].status as ATTACHED (no longer ATTACHING).
Emulator: the check makes no observable progress and is cut off at the replay ceiling ('Check timed out after 60.0s'); the registered container instance / awsvpc task never gets far enough for an attachment status transition to be submitted and observed, so DescribeTasks never shows the agent-reported ATTACHED status.
Why: an agent-side or agent-emulating client that reports ENI attachment progress gets no state change applied, so an awsvpc task's networking never appears attached and anything gated on attachments[].status never proceeds.
Fix: the SubmitAttachmentStateChanges handler and the task/container-instance attachment state it must update (surfaced by DescribeTasks attachments[]), plus the container-instance registration path the check depends on.
Traces: ecs.submit-attachment-state-changes-updates-attachment-status

--- Cause 5 of 5: a task placed via a CloudFormation ASG-backed capacity provider carries no containerInstanceArn ---
AWS: Smithy: RunTask -> Tasks -> Task has member containerInstanceArn (the ARN of the container instance hosting the task); it is present for every EC2/ASG-backed (non-Fargate) task, and it must equal the ARN of the ACTIVE container instance registered by the capacity provider's Auto Scaling group. recorded behavior only for the CFN wiring: a stack with an AWS::ECS::CapacityProvider over an ASG plus ClusterCapacityProviderAssociations is usable, and RunTask with that capacityProviderStrategy places the task on an instance from that ASG.
Emulator: the replay fails with KeyError: 'containerInstanceArn' at the assertion `task['containerInstanceArn'] == container_instance_arn` — the Task object returned/described for the capacity-provider-placed task omits the member entirely rather than naming the hosting container instance.
Why: a client cannot tell which host (or which capacity provider's ASG instance) is running a task, so host-scoped operations — draining the instance, correlating a task with instance metrics, instance-level debugging — have nothing to key on, and code indexing task['containerInstanceArn'] raises instead of reading an ARN.
Fix: the RunTask placement path's task record and the DescribeTasks serializer must populate Task$containerInstanceArn for EC2/capacity-provider-backed placements. Related open issues describe the same omission under other service headings but do not carry this check's replay key: 20261120T152111Z-cloudwatch-ec2-ecs-an-ec2-launch-type-ecs-task-that-reaches.md and 20261120T152132Z-cloudwatch-ec2-ecs-an-ecs-ec2-launch-type.md.
Traces: ecs.deploy-ecs-capacity-provider-cfn-usable-for-run-task

## Evidence
Cause 1 — ecs.capprov-base-and-weight-place-containers-that-really-run [mismatch]
  Expected: RunTask(count=3, capacityProviderStrategy=[{FARGATE_SPOT, base:1, weight:0},{FARGATE, base:0, weight:1}]) returns 3 tasks, Counter(task.capacityProviderName) == {FARGATE_SPOT:1, FARGATE:2}, and every task's container exits 0 having logged its marker and its own task ARN.
  Actual: 'Check timed out after 60.0s' — no placement split and no container execution observable.

Cause 2 — ecs.deployment-circuit-breaker-rolls-back-to-last-good [mismatch]
  Expected: with deploymentCircuitBreaker{enable:true, rollback:true}, the crash-looping revision's deployment reaches rolloutState FAILED and the PRIMARY deployment returns to the previous revision with rolloutState COMPLETED.
  Actual: 'Check timed out after 60.0s' — the deployment never fails and never rolls back.

Cause 3 — ecs.update-task-protection-extends-existing-expiration [mismatch]
  Expected: resp1 = UpdateTaskProtection(protectionEnabled=true, expiresInMinutes=10) -> protectedTasks[0].protectionEnabled == true with expirationDate E1; GetTaskProtection keeps reporting protectionEnabled true and unexpired throughout the hold; a second UpdateTaskProtection with the same expiresInMinutes yields E2 > E1 by about the elapsed time, and GetTaskProtection reports E2.
  Actual: 'Check timed out after 60.0s' — the first protected RUNNING service task never settles, so neither E1's stability nor the refreshed E2 is observable.

Cause 4 — ecs.submit-attachment-state-changes-updates-attachment-status [mismatch]
  Expected: after RunTask(launchType=EC2, awsvpcConfiguration) on a registered container instance, SubmitAttachmentStateChanges reporting the ElasticNetworkInterface attachment as ATTACHED is acknowledged and DescribeTasks reports attachments[].status == 'ATTACHED' (was 'ATTACHING').
  Actual: 'Check timed out after 60.0s' — no acknowledgment/attachment transition observable.

Cause 5 — ecs.deploy-ecs-capacity-provider-cfn-usable-for-run-task [error]
  Expected: task['containerInstanceArn'] == the ARN of the ACTIVE container instance registered by the capacity provider's ASG.
  Actual: File "check_ecs_deploy_ecs_capacity_provider_cfn_usable_for_run_task.py", line 91, in run / assert task["containerInstanceArn"] == container_instance_arn / KeyError: 'containerInstanceArn' — the member is absent from the Task.

## Steps to Reproduce
Replay the behavioral suite and re-run exactly these checks:
1. ecs.capprov-base-and-weight-place-containers-that-really-run — create a cluster with capacityProviders [FARGATE, FARGATE_SPOT], register an awsvpc Fargate task definition that logs a marker and its task ARN, then RunTask(count=3) with capacityProviderStrategy [{capacityProvider: FARGATE_SPOT, base: 1, weight: 0}, {capacityProvider: FARGATE, base: 0, weight: 1}]; assert 3 tasks, the 1/2 provider split via Task$capacityProviderName, and each container's exit 0 plus logged marker.
2. ecs.deployment-circuit-breaker-rolls-back-to-last-good — create a service with deploymentCircuitBreaker{enable: true, rollback: true}, UpdateService to a crash-looping revision, and poll DescribeServices for the failed deployment and the rollback to the last good revision.
3. ecs.update-task-protection-extends-existing-expiration — create a service with one RUNNING task, call UpdateTaskProtection(tasks=[task], protectionEnabled=true, expiresInMinutes=10), poll GetTaskProtection across a wall-clock hold, then call UpdateTaskProtection again with the same expiresInMinutes and compare expirationDate values.
4. ecs.submit-attachment-state-changes-updates-attachment-status — register a container instance, RunTask(launchType=EC2) with awsvpcConfiguration, then SubmitAttachmentStateChanges moving the ENI attachment ATTACHING -> ATTACHED and read DescribeTasks attachments[].status.
5. ecs.deploy-ecs-capacity-provider-cfn-usable-for-run-task — deploy the CFN stack with AWS::ECS::CapacityProvider over an ASG plus ClusterCapacityProviderAssociations, wait for an ACTIVE container instance, RunTask with that capacityProviderStrategy and read task['containerInstanceArn'].
