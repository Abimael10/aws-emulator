Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.service-converges-running-count-to-desired ecs.service-reaches-steady-state ecs.service-cpu-utilization-metric-published ecs.service-first-rollout-completes-with-matching-deployment-and-task-records ecs.service-first-rollout-events-name-each-started-task-and-the-steady-state ecs.service-rolling-update-retires-the-old-task-under-its-own-deployment ecs.service-scale-to-zero-stops-its-task-in-place-and-starts-no-replacement ecs.service-scales-and-force-deletes-to-inactive ecs.service-update-scales-desired-count ecs.service-update-task-definition-triggers-new-deployment ecs.concurrent-service-update-deployment-converges ecs.minimum-healthy-percent-floor-holds-during-rolling-update ecs.deep-ecs-3-deploy-fail-maximum-percent-ceiling-never-exceeded-during-rollout ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes ecs.deployment-circuit-breaker-rolls-back-to-last-good ecs.e2e-blue-green-deployment-full-cycle ecs.e2e-capacity-provider-driven-scaling-flow ecs.e2e-service-deploy-scale-observe-teardown ecs.e2e-task-protection-scale-in-survival ecs.e2e-task-protection-scale-in-survival-and-expiry ecs.task-set-create-scale-and-stabilize ecs.service-delete-requires-zero-desired-count-rejected ecs.e2e-delete-cluster-with-active-service-rejected ecs.ecs-delete-cluster-with-active-service-rejected ecs.service-deployment-events-ordered ecs.service-deployment-event-timestamps-monotonic-across-updates ecs.service-deployment-history-records-success ecs.service-deployment-stop-freezes-terminal ecs.service-desired-count-zero-emits-no-task-started-event ecs.stop-service-deployment-terminal-state-rejected ecs.service-deployment-circuit-breaker-rollback-ordering ecs.task-protection-default-state-is-disabled ecs.task-protection-enable-and-get-roundtrip ecs.task-protection-enable-disable-on-service-task ecs.task-protection-expires-in-minutes-out-of-range-rejected ecs.task-protection-expires-range-validation ecs.task-protection-requires-service-membership-rejected ecs.get-task-protection-returns-current-state ecs.update-task-protection-batch-partial-failure ecs.update-task-protection-disable-allows-immediate-scale-in ecs.update-task-protection-disable-clears-immediately ecs.update-task-protection-extends-existing-expiration ecs.update-task-protection-idempotent-reapply-same-window ecs.update-task-protection-overwrite-extends-expiry ecs.update-task-protection-reapply-extends-expiration ecs.update-task-protection-standalone-task-rejected ecs.execute-command-on-stopped-task-rejected ecs.execute-command-server-managed-session ecs.execute-command-session-expiry ecs.execute-command-without-exec-enabled-rejected ecs.e2e-run-task-execute-command-session-lifecycle ecs.stop-task-wrong-cluster-rejected ecs.deploy-ecs-runtask-unassociated-capacity-provider-rejected
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.service-converges-running-count-to-desired|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-reaches-steady-state|_poll_until#13|Timeout|return last
Issue-Key: ecs.service-cpu-utilization-metric-published|_poll_until#11|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s; last state={last!r}")
Issue-Key: ecs.service-first-rollout-completes-with-matching-deployment-and-task-records|_poll#9|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-first-rollout-events-name-each-started-task-and-the-steady-state|_poll#10|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-rolling-update-retires-the-old-task-under-its-own-deployment|_poll#7|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-scale-to-zero-stops-its-task-in-place-and-starts-no-replacement|_poll#8|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-scales-and-force-deletes-to-inactive|_poll_until#11|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-update-scales-desired-count|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-update-task-definition-triggers-new-deployment|_poll_until#12|Timeout|time.sleep(interval_s)
Issue-Key: ecs.concurrent-service-update-deployment-converges|_poll_until#12|Timeout|return last
Issue-Key: ecs.minimum-healthy-percent-floor-holds-during-rolling-update|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Key: ecs.deep-ecs-3-deploy-fail-maximum-percent-ceiling-never-exceeded-during-rollout|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Key: ecs.deployment-circuit-breaker-rolls-back-to-last-good|_poll_until#24|Timeout|time.sleep(interval_s)
Issue-Key: ecs.e2e-blue-green-deployment-full-cycle|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Key: ecs.e2e-capacity-provider-driven-scaling-flow|_poll_until#40|Timeout|time.sleep(interval_s)
Issue-Key: ecs.e2e-service-deploy-scale-observe-teardown|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: ecs.e2e-task-protection-scale-in-survival|_poll_until#14|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s; last={last!r}")
Issue-Key: ecs.e2e-task-protection-scale-in-survival-and-expiry|_poll_until#16|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s (last={last!r})")
Issue-Key: ecs.task-set-create-scale-and-stabilize|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-delete-requires-zero-desired-count-rejected|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: ecs.e2e-delete-cluster-with-active-service-rejected|_poll_until#13|Timeout|time.sleep(interval_s)
Issue-Key: ecs.ecs-delete-cluster-with-active-service-rejected|_poll_until#11|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-deployment-events-ordered|_poll_until#18|Timeout|return last
Issue-Key: ecs.service-deployment-event-timestamps-monotonic-across-updates|_wait_for_running_task#11|Timeout|time.sleep(5)
Issue-Key: ecs.service-deployment-history-records-success|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-deployment-stop-freezes-terminal|_poll_until#13|Timeout|return val
Issue-Key: ecs.service-desired-count-zero-emits-no-task-started-event|_poll_until#16|AssertionError|raise AssertionError(f"condition not met within {timeout_s}s; last value: {last}")
Issue-Key: ecs.stop-service-deployment-terminal-state-rejected|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: ecs.service-deployment-circuit-breaker-rollback-ordering|_poll_deployment_statuses#13|Timeout|time.sleep(interval_s)
Issue-Key: ecs.task-protection-default-state-is-disabled|_poll_until#2|Timeout|time.sleep(interval_s)
Issue-Key: ecs.task-protection-enable-and-get-roundtrip|_poll_until#10|Timeout|time.sleep(interval_s)
Issue-Key: ecs.task-protection-enable-disable-on-service-task|_poll_until#9|Timeout|time.sleep(interval_s)
Issue-Key: ecs.task-protection-expires-in-minutes-out-of-range-rejected|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Key: ecs.task-protection-expires-range-validation|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Key: ecs.task-protection-requires-service-membership-rejected|_poll_until#5|Timeout|time.sleep(interval_s)
Issue-Key: ecs.get-task-protection-returns-current-state|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: ecs.update-task-protection-batch-partial-failure|_wait_for_running_tasks#6|Timeout|time.sleep(2)
Issue-Key: ecs.update-task-protection-disable-allows-immediate-scale-in|_wait_for_running_tasks#6|Timeout|time.sleep(2)
Issue-Key: ecs.update-task-protection-disable-clears-immediately|_run_service_task_until_running#5|Timeout|time.sleep(5)
Issue-Key: ecs.update-task-protection-extends-existing-expiration|_wait_for_running_tasks#6|Timeout|time.sleep(2)
Issue-Key: ecs.update-task-protection-idempotent-reapply-same-window|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: ecs.update-task-protection-overwrite-extends-expiry|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: ecs.update-task-protection-reapply-extends-expiration|_run_service_task_until_running#5|Timeout|time.sleep(5)
Issue-Key: ecs.update-task-protection-standalone-task-rejected|run#7|AssertionError|assert task["lastStatus"] == "RUNNING"
Issue-Key: ecs.execute-command-on-stopped-task-rejected|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Key: ecs.execute-command-server-managed-session|_poll_until#27|Timeout|time.sleep(interval_s)
Issue-Key: ecs.execute-command-session-expiry|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Key: ecs.execute-command-without-exec-enabled-rejected|run#27|Timeout|time.sleep(5)
Issue-Key: ecs.e2e-run-task-execute-command-session-lifecycle|_poll_until#18|Timeout|time.sleep(interval_s)
Issue-Key: ecs.stop-task-wrong-cluster-rejected|_poll_until#17|Timeout|time.sleep(interval_s)
Issue-Key: ecs.deploy-ecs-runtask-unassociated-capacity-provider-rejected|_poll_until#19|Timeout|time.sleep(interval_s)
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# ecs: the service scheduler never converges — 5 net-new causes (runningCount stays 0 with failedTasks growing unbounded, no service-deployment records or events, task-protection APIs unobservable, exec sessions unusable, refusal checks never reached)

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeServices

## Description
Five distinct root causes above the ECS service scheduler. Related but NOT covering open issues: 20261124T140523Z-ecs-a-deployment-alarm-rollout-makes-no-observable.md and 20261125T030453Z-ecs-the-draining-name-reuse-refusal-is-unobservable.md; the suite did not mark any check below as owned by either, so they are filed here with their own replay keys.

Cause 1 of 5: a service's PRIMARY deployment never converges — runningCount stays 0, rolloutState stays IN_PROGRESS, and failedTasks grows without bound.
AWS: Smithy: Deployment.rolloutState (enum DeploymentRolloutState: COMPLETED, FAILED, IN_PROGRESS) — "When a service deployment is started, it begins in an IN_PROGRESS state. When the service reaches a steady state, the deployment transitions to a COMPLETED state." Smithy: Deployment.failedTasks — "The number of consecutively failed tasks in the deployment... Once a service deployment has one or more successfully running tasks, the failed task count resets to zero and stops being evaluated." So for a healthy task definition, AWS drives runningCount up to desiredCount, sets rolloutState COMPLETED with rolloutStateReason naming steady state, and leaves failedTasks at 0.
Emulator: DescribeServices reports desiredCount N with runningCount 0 and pendingCount 0 (or 1) indefinitely, the PRIMARY deployment stuck at rolloutState 'IN_PROGRESS' / rolloutStateReason 'ECS deployment <id> in progress.', and failedTasks climbing to values such as 178, 504 and 510 — i.e. the scheduler relaunches forever and never caps or resets the counter.
Why: every waiter on steady state (services-stable, CFN/CDK service resources, autoscaling that reads runningCount) hangs; a client reading failedTasks sees an ever-growing number that AWS would have frozen.
Fix: the service scheduler's convergence loop and the Deployment projection in the DescribeServices handler (rolloutState/failedTasks/runningCount).
Traces: ecs.service-converges-running-count-to-desired, ecs.service-reaches-steady-state, ecs.service-cpu-utilization-metric-published, ecs.service-first-rollout-completes-with-matching-deployment-and-task-records, ecs.service-first-rollout-events-name-each-started-task-and-the-steady-state, ecs.service-rolling-update-retires-the-old-task-under-its-own-deployment, ecs.service-scale-to-zero-stops-its-task-in-place-and-starts-no-replacement, ecs.service-scales-and-force-deletes-to-inactive, ecs.service-update-scales-desired-count, ecs.service-update-task-definition-triggers-new-deployment, ecs.concurrent-service-update-deployment-converges, ecs.minimum-healthy-percent-floor-holds-during-rolling-update, ecs.deep-ecs-3-deploy-fail-maximum-percent-ceiling-never-exceeded-during-rollout, ecs.deep-ecs-3-deploy-fail-metric-alarm-rolls-back-to-rev1-quiet-twin-completes, ecs.deployment-circuit-breaker-rolls-back-to-last-good, ecs.e2e-blue-green-deployment-full-cycle, ecs.e2e-capacity-provider-driven-scaling-flow, ecs.e2e-service-deploy-scale-observe-teardown, ecs.e2e-task-protection-scale-in-survival, ecs.e2e-task-protection-scale-in-survival-and-expiry, ecs.task-set-create-scale-and-stabilize, ecs.service-delete-requires-zero-desired-count-rejected, ecs.e2e-delete-cluster-with-active-service-rejected, ecs.ecs-delete-cluster-with-active-service-rejected.

Cause 2 of 5: service deployment records and service events are never produced, and a stopped deployment is mislabelled ROLLBACK_FAILED.
AWS: Smithy: ListServiceDeployments/DescribeServiceDeployments expose a ServiceDeployment per rollout with ordered createdAt/startedAt/finishedAt, and Service.events carries ordered, monotonically-timestamped messages naming each started task and the steady state. StopServiceDeployment on a deployment with no previous revision leaves the deployment in a STOPPED family state; ROLLBACK_FAILED with the reason "...has no previous service revision." is the rollback path, not the stop path.
Emulator: the event/deployment-record probes find nothing within budget (last value None, empty event lists), and StopServiceDeployment yields status 'ROLLBACK_FAILED' with statusReason "This service deployment couldn't be rolled back because service <serviceArn> has no previous service revision." where the fixed text is that sentence and <serviceArn> is the request's service — AWS does not report a rollback failure for a plain stop.
Why: clients that drive deployments off service events or the deployment history see an empty history, and one that reads ROLLBACK_FAILED treats a successful stop as a failed rollback.
Fix: the service event emitter and the ServiceDeployment state machine behind ListServiceDeployments/DescribeServiceDeployments/StopServiceDeployment.
Traces: ecs.service-deployment-events-ordered, ecs.service-deployment-event-timestamps-monotonic-across-updates, ecs.service-deployment-history-records-success, ecs.service-deployment-stop-freezes-terminal, ecs.service-desired-count-zero-emits-no-task-started-event, ecs.stop-service-deployment-terminal-state-rejected, ecs.service-deployment-circuit-breaker-rollback-ordering.

Cause 3 of 5: the task-protection API family is unobservable.
AWS: Smithy: GetTaskProtection/UpdateTaskProtection operate on a task that is a member of a service; a never-protected task reports protectionEnabled false, an enabled one reports expirationDate, expiresInMinutes outside [1, 2880] is refused InvalidParameterException, and a standalone (non-service) task is refused. Each of these is a modelled, synchronous answer.
Emulator: every one of these checks exhausts its budget without a verdict, and ecs.update-task-protection-standalone-task-rejected fails its assertion outright — the protection plane never answers about a service task because none exists (see Cause 1) and the refusal paths are never reached.
Why: a client cannot read or set scale-in protection at all, and gets no refusal for an out-of-range window or a standalone task.
Fix: the GetTaskProtection/UpdateTaskProtection handlers and their validation, plus the service-membership lookup they depend on.
Traces: ecs.task-protection-default-state-is-disabled, ecs.task-protection-enable-and-get-roundtrip, ecs.task-protection-enable-disable-on-service-task, ecs.task-protection-expires-in-minutes-out-of-range-rejected, ecs.task-protection-expires-range-validation, ecs.task-protection-requires-service-membership-rejected, ecs.get-task-protection-returns-current-state, ecs.update-task-protection-batch-partial-failure, ecs.update-task-protection-disable-allows-immediate-scale-in, ecs.update-task-protection-disable-clears-immediately, ecs.update-task-protection-extends-existing-expiration, ecs.update-task-protection-idempotent-reapply-same-window, ecs.update-task-protection-overwrite-extends-expiry, ecs.update-task-protection-reapply-extends-expiration, ecs.update-task-protection-standalone-task-rejected.

Cause 4 of 5: ExecuteCommand's session plane never answers.
AWS: Smithy: ExecuteCommand returns a Session (sessionId, streamUrl, tokenValue) for a RUNNING task launched with enableExecuteCommand true, and refuses with InvalidParameterException when execute-command was not enabled or the task is not RUNNING. The refusal and the success are both prompt, synchronous answers.
Emulator: these checks exhaust their budget with no answer at all — neither a Session nor a refusal is produced.
Why: ecs exec is unusable and the negative cases give no error a client can branch on.
Fix: the ExecuteCommand handler and the managed-agent/session plane it needs.
Traces: ecs.execute-command-on-stopped-task-rejected, ecs.execute-command-server-managed-session, ecs.execute-command-session-expiry, ecs.execute-command-without-exec-enabled-rejected, ecs.e2e-run-task-execute-command-session-lifecycle.

Cause 5 of 5: two refusal paths never return a verdict at all.
AWS: Smithy: StopTask with a task ARN belonging to another cluster is refused InvalidParameterException; RunTask naming a capacity provider not associated with the cluster is refused ClientException/InvalidParameterException. Both are immediate refusals.
Emulator: both checks exhaust their budget without the call answering, so the refusal is never produced.
Why: a client gets a hang instead of the modelled error, and cannot distinguish a wrong-cluster/unassociated-provider mistake from a slow call.
Fix: the StopTask and RunTask handlers' cluster/capacity-provider validation.
Traces: ecs.stop-task-wrong-cluster-rejected, ecs.deploy-ecs-runtask-unassociated-capacity-provider-rejected.

## Evidence
Cause 1 — DescribeServices last state: {'desiredCount': 1, 'runningCount': 0, 'pendingCount': 1, deployments[0]: {'status': 'PRIMARY', 'runningCount': 0, 'failedTasks': 178, 'rolloutState': 'IN_PROGRESS', 'rolloutStateReason': 'ECS deployment ecs-svc/0000000000000000001 in progress.'}} after 300s; another service shows 'desiredCount': 3, 'runningCount': 0, 'failedTasks': 510 after 480s. AWS would report rolloutState COMPLETED with runningCount == desiredCount and failedTasks 0.
Cause 2 — "condition not met within 49.64s; last value: None" (no task-started event); StopServiceDeployment returned status 'ROLLBACK_FAILED', statusReason "This service deployment couldn't be rolled back because service arn:aws:ecs:<region>:<account>:service/<cluster>/<service> has no previous service revision." (fixed text as quoted; the ARN is the request's service). Expected a STOPPED-family terminal state with no rollback reason.
Cause 3 — all 15 listed checks end as "Check timed out after 60.0s" except ecs.update-task-protection-standalone-task-rejected, which fails with a bare AssertionError.
Cause 4 — all five checks end as "Check timed out after 60.0s".
Cause 5 — both checks end as "Check timed out after 60.0s" with no response recorded for StopTask / RunTask.

## Steps to Reproduce
1. CreateCluster, RegisterTaskDefinition (FARGATE, long-running command), CreateService desiredCount 1..3.
2. Poll DescribeServices. Cause 1: runningCount stays 0, deployments[0].rolloutState stays IN_PROGRESS and failedTasks climbs past 100.
3. Call ListServiceDeployments / DescribeServices events after an UpdateService, and StopServiceDeployment on the current deployment. Cause 2: no deployment records or events; the stopped deployment reports ROLLBACK_FAILED with a 'no previous service revision' reason.
4. Call GetTaskProtection / UpdateTaskProtection (including expiresInMinutes out of [1,2880] and a standalone RunTask task). Cause 3: no answer.
5. RunTask with enableExecuteCommand true, then ExecuteCommand; also ExecuteCommand on a task launched without it. Cause 4: no Session and no refusal.
6. StopTask with a task ARN from a different cluster; RunTask with a capacityProviderStrategy naming a provider not associated with the cluster. Cause 5: neither call returns its refusal.
