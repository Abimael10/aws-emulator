Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.describe-services-duplicate-name-in-list-rejected ecs.put-cluster-capacity-providers-duplicate-name-in-list-rejected ecs.put-account-setting-default-service-long-arn-format-disabled-refused ecs.register-task-definition-healthy-dependency-without-health-check-rejected ecs.register-task-definition-rejects-invalid-linux-parameters ecs.service-first-rollout-events-name-each-started-task-and-the-steady-state ecs.service-desired-count-zero-emits-no-task-started-event ecs.service-name-reuse-while-the-deleted-service-drains-rejected ecs.service-scale-to-zero-stops-its-task-in-place-and-starts-no-replacement ecs.service-rolling-update-retires-the-old-task-under-its-own-deployment
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.describe-services-duplicate-name-in-list-rejected|_error#14|AssertionError|raise AssertionError(f"expected a rejection, but the call SUCCEEDED with {result!r}")
Issue-Key: ecs.put-cluster-capacity-providers-duplicate-name-in-list-rejected|_error#7|AssertionError|raise AssertionError(f"expected a rejection, but the call SUCCEEDED with {result!r}")
Issue-Key: ecs.put-account-setting-default-service-long-arn-format-disabled-refused|expect_error#7|AssertionError|raise AssertionError(f"expected {(code, message)!r}, but the call succeeded: {result!r}")
Issue-Key: ecs.register-task-definition-healthy-dependency-without-health-check-rejected|_error#3|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded: {result!r}")
Issue-Key: ecs.register-task-definition-rejects-invalid-linux-parameters|expect_error#3|AssertionError|raise AssertionError(f"expected {code!r} ({message!r}) but the call succeeded: {result!r}")
Issue-Key: ecs.service-first-rollout-events-name-each-started-task-and-the-steady-state|run#15|AssertionError|assert sorted(started) == sorted(running), (
Issue-Key: ecs.service-desired-count-zero-emits-no-task-started-event|_poll_until#19|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s; last value: {last}")
Issue-Key: ecs.service-name-reuse-while-the-deleted-service-drains-rejected|run#13|AssertionError|assert (code, message) == want, f"recreate while draining: expected {want!r}, got {(code, message)!r}"
Issue-Key: ecs.service-scale-to-zero-stops-its-task-in-place-and-starts-no-replacement|run#19|AssertionError|assert seen == want, f"the scaled-in task (stopCode, stoppedReason): expected {want!r}, got {seen!r}"
Issue-Key: ecs.service-rolling-update-retires-the-old-task-under-its-own-deployment|run#31|AssertionError|assert seen == want, f"the old task (startedBy, stopCode, stoppedReason): expected {want!r}, got {seen!r}"
Issue-Commit: 384e9c06f8b850c3e6c1129231f41c38ae5339ba

# ECS accepts five requests AWS refuses and misreports service task-start events and scheduler stop reasons (8 causes)

> _Note: this issue was identified against the code base at commit `384e9c06f8b850c3e6c1129231f41c38ae5339ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterTaskDefinition

## Description
Eight net-new root causes: five request classes AWS refuses are answered 200 by the emulator (so a client's mistake is silently persisted), one refusal carries the wrong sentence, and two representation causes concern what a service reports about the tasks its scheduler started and stopped.

Cause 1 of 8: DescribeServices naming the same service twice.
AWS: 400 InvalidParameterException 'Malformed request with duplicate service names.' — fixed text, no variable part (Smithy: DescribeServicesRequest.services is a @required StringList with no uniqueItems trait, and InvalidParameterException is modeled on the operation, so the duplicate rule is recorded behavior).
Emulator: 200, describing the service once (status ACTIVE) and reporting no failure.
Why: a caller that accidentally duplicates a name gets a list whose length does not match its request and never learns the request was malformed.
Fix: the DescribeServices input validation (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/describe_services.rs).
Traces: ecs.describe-services-duplicate-name-in-list-rejected.

Cause 2 of 8: PutClusterCapacityProviders with one provider named twice in capacityProviders.
AWS: 400 InvalidParameterException 'The specified capacity provider was not found. Specify a valid capacity provider and try again.' — AWS treats the duplicated name as an unresolvable provider; fixed text, no variable part. The cluster is left with no providers associated.
Emulator: 200, and the cluster is left with capacityProviders ['FARGATE','FARGATE'] and attachmentsStatus UPDATE_IN_PROGRESS.
Why: the cluster is persisted with a duplicated association that AWS can never hold, so every later read of that cluster diverges.
Fix: the PutClusterCapacityProviders provider-resolution validation (src/ops/put_cluster_capacity_providers.rs).
Traces: ecs.put-cluster-capacity-providers-duplicate-name-in-list-rejected.

Cause 3 of 8: PutAccountSettingDefault turning a long-ARN setting back off.
AWS: 400 InvalidParameterException 'You can no longer disable Long Arn settings' — fixed text, no variable part (Smithy: SettingName includes serviceLongArnFormat; InvalidParameterException is modeled on the operation). Long-ARN format is permanently on.
Emulator: 200, echoing setting {name: serviceLongArnFormat, value: 'disabled'} and persisting it.
Why: the account is left in a state AWS does not allow, and downstream ARN-format behaviour keyed to the setting diverges for the rest of the run.
Fix: the PutAccountSettingDefault value validation (src/ops/put_account_setting_default.rs).
Traces: ecs.put-account-setting-default-service-long-arn-format-disabled-refused.

Cause 4 of 8: RegisterTaskDefinition with a dependsOn condition HEALTHY on a container that has no healthCheck.
AWS: 400 ClientException 'A dependency container with HEALTHY condition must have health check configured' — fixed text, no variable part; Smithy documents the rule on ContainerDependency.condition ('HEALTHY ... requires that the dependent container has health checks configured'), and ClientException is modeled on RegisterTaskDefinition. No revision is created.
Emulator: 200, registering revision 1 ACTIVE with the dependsOn echoed back.
Why: a task definition that can never start is accepted, so the failure surfaces much later (or never, in an emulator that does not run containers) instead of at registration.
Fix: the RegisterTaskDefinition dependsOn validation (src/ops/register_task_definition.rs).
Traces: ecs.register-task-definition-healthy-dependency-without-health-check-rejected.

Cause 5 of 8: RegisterTaskDefinition with a CAP_-prefixed Linux capability in linuxParameters.capabilities.add.
AWS: 400 ClientException 'Unrecognized Linux capabilities in add: [CAP_SYS_ADMIN]' — template 'Unrecognized Linux capabilities in add: [' + the rejected capability names from the request + ']'; AWS accepts the unprefixed spellings only (recorded behavior only).
Emulator: 200, registering the definition with linuxParameters.capabilities.add ['CAP_SYS_ADMIN'] echoed back.
Fix: the RegisterTaskDefinition linuxParameters.capabilities validation (src/ops/register_task_definition.rs).
Traces: ecs.register-task-definition-rejects-invalid-linux-parameters.

Cause 6 of 8: a service's events never name the tasks its scheduler started.
AWS: when a service's scheduler starts tasks, the service's events list carries one start event per task, naming it — '(service <serviceName>) has started 1 tasks: (task <taskId>).' — alongside the deployment/steady-state lines (recorded behavior only; Smithy models ServiceEvent with only id/createdAt/message, so the message text is recorded). A service created with desiredCount 0 and then raised to 1 therefore has exactly one such event.
Emulator: the events list only ever holds '(service <name>) has reached a steady state.' and '(service <name>) (deployment <id>) deployment completed.'; no start event is ever emitted, so the set of task ids named by start events is empty while DescribeTasks reports the tasks RUNNING.
Why: operators and tooling read service events to attribute each task launch to its service and deployment; with no start events that attribution is impossible, and a check that counts launches sees none.
Fix: the service scheduler's event emission in the ECS crate (the state module that advances a service's deployment and appends its events).
Traces: ecs.service-first-rollout-events-name-each-started-task-and-the-steady-state (start events named [] while tasks ...0001/...0002 were RUNNING), ecs.service-desired-count-zero-emits-no-task-started-event.

Cause 7 of 8: CreateService reusing the name of a service that is still DRAINING after DeleteService.
AWS: 400 InvalidParameterException 'Unable to Start a service that is still Draining.' — fixed text, no variable part (recorded behavior only).
Emulator: 400 InvalidParameterException 'Creation of service was not idempotent.' — the right status and class, but the idempotency sentence instead of the draining sentence, so the caller is told its request conflicted with a prior identical create rather than that the old service is still draining and the name will become free.
Fix: the CreateService name-conflict branch (src/ops/create_service.rs) — distinguish a DRAINING predecessor from a non-idempotent re-create.
Traces: ecs.service-name-reuse-while-the-deleted-service-drains-rejected.

Cause 8 of 8: the stoppedReason of a task the service scheduler retires.
AWS: for a task stopped by the service scheduler, stopCode is ServiceSchedulerInitiated and stoppedReason is 'Scaling activity initiated by (deployment <deploymentId>)' — fixed text with the deployment id in PARENTHESES; the id is the deployment that started the task (so on a rolling update the retired task names its OWN deployment, not the new one) (recorded behavior only).
Emulator: two divergences from one template defect — on scale-in it answers 'Scaling activity initiated by deployment <deploymentId>' (the parentheses around the deployment clause are missing), and on a rolling replacement it answers an entirely different sentence, 'Task stopped by deployment <newDeploymentId>', attributing the stop to the NEW deployment.
Why: tooling that parses stoppedReason to attribute a stop to a deployment either fails to parse it or attributes the retirement to the wrong deployment.
Fix: the scheduler's task-stop reason construction in the ECS crate's state module (both the scale-in and the rolling-replacement paths must build the one AWS template from the task's own deployment id).
Traces: ecs.service-scale-to-zero-stops-its-task-in-place-and-starts-no-replacement, ecs.service-rolling-update-retires-the-old-task-under-its-own-deployment.

## Evidence
Behavioral replay of simparity-behavioral-aws-v1 against this checkout; all ten checks are listed under '### ecs' and none is marked ALREADY FILED or OWNED.
Cause 1 — ecs.describe-services-duplicate-name-in-list-rejected: 'expected a rejection, but the call SUCCEEDED with {...services: [one ACTIVE service...]}'; expected ('InvalidParameterException', 'Malformed request with duplicate service names.').
Cause 2 — ecs.put-cluster-capacity-providers-duplicate-name-in-list-rejected: 'expected a rejection, but the call SUCCEEDED with {cluster: {... capacityProviders: ['FARGATE','FARGATE'], attachmentsStatus: 'UPDATE_IN_PROGRESS'}}'; expected ('InvalidParameterException', 'The specified capacity provider was not found. Specify a valid capacity provider and try again.').
Cause 3 — ecs.put-account-setting-default-service-long-arn-format-disabled-refused: "expected ('InvalidParameterException', 'You can no longer disable Long Arn settings'), but the call succeeded: {'setting': {'name': 'serviceLongArnFormat', 'value': 'disabled', ...}, HTTPStatusCode 200}".
Cause 4 — ecs.register-task-definition-healthy-dependency-without-health-check-rejected: 'expected the call to be rejected, but it succeeded' with revision 1 ACTIVE and app.dependsOn [{containerName: sidecar, condition: HEALTHY}] while sidecar carries no healthCheck; expected ClientException 'A dependency container with HEALTHY condition must have health check configured'.
Cause 5 — ecs.register-task-definition-rejects-invalid-linux-parameters: "expected 'ClientException' ('Unrecognized Linux capabilities in add: [CAP_SYS_ADMIN]') but the call succeeded" with linuxParameters {'capabilities': {'add': ['CAP_SYS_ADMIN']}} registered.
Cause 6 — ecs.service-first-rollout-events-name-each-started-task-and-the-steady-state: 'the start events must name each RUNNING task exactly once: named [], running [<two task ids>]'. ecs.service-desired-count-zero-emits-no-task-started-event: condition not met; last value was only ['(service <name>) has reached a steady state.', '(service <name>) (deployment ecs-svc/...) deployment completed.', ...] — no start event. Task ids and the service name are per-run values.
Cause 7 — ecs.service-name-reuse-while-the-deleted-service-drains-rejected: recreate while draining: expected ('InvalidParameterException', 'Unable to Start a service that is still Draining.'), got ('InvalidParameterException', 'Creation of service was not idempotent.').
Cause 8 — ecs.service-scale-to-zero-stops-its-task-in-place-and-starts-no-replacement: the scaled-in task (stopCode, stoppedReason): expected ('ServiceSchedulerInitiated', 'Scaling activity initiated by (deployment <deploymentId>)'), got ('ServiceSchedulerInitiated', 'Scaling activity initiated by <deploymentId>' without the parentheses). ecs.service-rolling-update-retires-the-old-task-under-its-own-deployment: expected ('ecs-svc/...0001', 'ServiceSchedulerInitiated', 'Scaling activity initiated by (deployment ecs-svc/...0001)'), got (..., 'Task stopped by deployment ecs-svc/...0002'). The deployment ids are per-run values filling the template.

## Steps to Reproduce
1. git_clone this checkout and boot the emulator.
2. Cause 1: CreateService, then DescribeServices with services=[name, name].
3. Cause 2: CreateCluster, then PutClusterCapacityProviders with capacityProviders=['FARGATE','FARGATE'].
4. Cause 3: PutAccountSettingDefault name=serviceLongArnFormat value=disabled.
5. Cause 4: RegisterTaskDefinition with two containers where the second dependsOn the first with condition HEALTHY and the first has no healthCheck.
6. Cause 5: RegisterTaskDefinition with linuxParameters.capabilities.add=['CAP_SYS_ADMIN'].
7. Cause 6: CreateService with desiredCount 0, UpdateService to desiredCount 1, then DescribeServices and read events; compare with the RUNNING task ids from ListTasks/DescribeTasks.
8. Cause 7: CreateService, DeleteService (leaving it DRAINING), then CreateService again with the same name.
9. Cause 8: scale a one-task service to desiredCount 0 and read the stopped task's stopCode/stoppedReason; separately UpdateService onto a new task-definition revision and read the retired task's startedBy/stopCode/stoppedReason.
10. Re-replay the ten check ids in trace_ids.
