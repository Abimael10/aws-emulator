Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.service-deployment-event-timestamps-monotonic-across-updates ecs.capacity-provider-removal-while-in-use-rejected
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.service-deployment-event-timestamps-monotonic-across-updates|run#29|AssertionError|assert len(events) >= 1, f"expected at least one service event, got {events}"
Issue-Key: ecs.capacity-provider-removal-while-in-use-rejected|_poll_until#14|Timeout|time.sleep(interval_s)
Issue-Commit: 9fd94122ad4d4e2209028b3c78eafc4b4c524940

# ecs: 2 net-new causes — DescribeServices never reports a service's events, and the in-use capacity-provider removal refusal path never settles

> _Note: this issue was identified against the code base at commit `9fd94122ad4d4e2209028b3c78eafc4b4c524940`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeServices

## Description
Two distinct, net-new root causes observed in the ECS emulator. Both are reached only AFTER the service/task plane has already produced a running task and service-deployment records, so neither is the already-filed non-convergence cause (20261125T221450Z-ecs-the-service-scheduler-never-converges-5-net.md / 20261125T221450Z-ecs-the-task-data-plane-never-produces-a.md); those issues do not cover these two and both must be closed separately.

=== Cause 1 of 2: DescribeServices returns an empty `events` list for a service that has been created and updated several times ===
Failing check ids: ecs.service-deployment-event-timestamps-monotonic-across-updates

AWS: Smithy — DescribeServices output `services` targets `Services`, whose member `Service` carries `events: ServiceEvents` (list of `ServiceEvent { id: String, createdAt: Timestamp, message: String }`, doc: "The details for an event that's associated with a service."). Real ECS appends a ServiceEvent for each scheduler action on the service — at minimum one event when the service's deployment reaches steady state, and further events as tasks are started/registered and as subsequent UpdateService calls mint new deployments. Each event has a unique `id`, a `createdAt`, and a `message`; the list is returned newest-first and ids are never duplicated. The message is a template, not a literal: AWS's fixed text for the steady-state event is `(service <serviceName>) has reached a steady state.` and for a task launch `(service <serviceName>) has started 1 tasks: (task <taskId>).` — the fixed words are AWS's, while `<serviceName>` comes from the request/state and `<taskId>` from the task the scheduler actually started. No event is a per-run value in itself: the LIST being non-empty is the requirement.
Emulator: after CreateService(desiredCount=1, FARGATE) reached a RUNNING task and after two further UpdateService calls each minted a non-PENDING service-deployment record (the check's ListServiceDeployments polls all succeeded, so deployments ARE recorded), DescribeServices(cluster, [service]).services[0]["events"] is `[]`. The check fails at `assert len(events) >= 1, f"expected at least one service event, got {events}"` with `AssertionError: expected at least one service event, got []`. The emulator therefore records service deployments but never records any ServiceEvent, so the ordering/uniqueness guarantees (newest-first `createdAt`, unique `id`) can never be observed either.
Why: a client relying on AWS's behavior loses the service event timeline entirely. Deployment tooling, CLIs and operators read `DescribeServices.services[].events` to learn why a service is not converging ("unable to place a task", "has reached a steady state", "has started N tasks"); anything that waits for the steady-state event, diffs the event list, or de-duplicates by event `id` sees a permanently empty history and either hangs or concludes nothing ever happened. `aws ecs wait services-stable`-style polling and dashboards built on the event feed are silently blind.
Fix: the service scheduler's state for ECS services — the `events` collection on the stored Service record must be appended to whenever the scheduler acts (deployment created, tasks started/stopped, steady state reached), with a unique id and the action's timestamp — and the DescribeServices handler/serializer must emit that collection newest-first instead of an empty list.
Traces: ecs.service-deployment-event-timestamps-monotonic-across-updates (assertion at line 93 of the check: `expected at least one service event, got []`).

=== Cause 2 of 2: the PutClusterCapacityProviders in-use refusal is never reached — the scenario never settles and the check is killed with no verdict ===
Failing check ids: ecs.capacity-provider-removal-while-in-use-rejected

AWS: Smithy — PutClusterCapacityProviders declares `ResourceInUseException` (client error, HTTP 400, doc: "The specified resource is in-use and can't be removed.") among its errors. Real ECS refuses a PutClusterCapacityProviders call that would drop a capacity provider a running service still depends on, answering HTTP 400 with error code `ResourceInUseException` and the message `The specified capacity provider is in use and cannot be removed.` — that message is entirely fixed text for this error class, with no request- or state-derived parts. The refusal must have no effect: DescribeClusters must still list the provider (here `FARGATE`) in `capacityProviders`. Once the dependent service is deleted and INACTIVE, the identical call must succeed and the cluster's `capacityProviders` must settle to the requested (empty) set.
Emulator: the check never gets as far as issuing the removal — it is killed with `Error: Check timed out after 60.0s`. The scenario's preceding steps are CreateCluster, PutClusterCapacityProviders(["FARGATE"]) with a default strategy, RegisterTaskDefinition, CreateService(capacityProviderStrategy=[FARGATE], desiredCount=1) and then a poll of `DescribeServices(...).services[0]["runningCount"] >= 1`. The emulator never advances that path to a settled state inside the window, so the whole in-use refusal contract — the 400/`ResourceInUseException`, its exact message, the no-effect guarantee on `capacityProviders`, and the positive control that the same removal succeeds after the service is gone — is completely unexercised and unimplemented-by-observation.
Why: a client that depends on ECS protecting an in-use capacity provider gets no protection signal at all. Either the call hangs (as here) or, if it eventually answers, the refusal class and the no-effect guarantee are unverified: infrastructure code that catches `ResourceInUseException` to retry after draining a service instead sees a timeout, and code that assumes a failed PutClusterCapacityProviders left the association untouched may proceed on a cluster whose capacity-provider list was silently mutated.
Fix: the PutClusterCapacityProviders handler and its validation (the dependency check that scans the cluster's services for a still-running dependant before replacing the association), plus whatever in the capacity-provider/service state keeps this scenario from settling so that the refusal can be reached at all. The refusal must be raised before the association is mutated, and the accepted removal must converge the cluster's `capacityProviders` to the requested set.
Traces: ecs.capacity-provider-removal-while-in-use-rejected (`Check timed out after 60.0s`; the refusal assertions at lines 43908-43923 of the check were never reached).

## Evidence
Cause 1 — ecs.service-deployment-event-timestamps-monotonic-across-updates
Expected (AWS): DescribeServices(cluster, [service]).services[0]["events"] is a non-empty list of ServiceEvent{id, createdAt, message}, newest-first by createdAt, with unique ids; e.g. a steady-state event whose fixed text is "(service <serviceName>) has reached a steady state." with <serviceName> taken from the request.
Actual (emulator): services[0]["events"] == []
Traceback:
  File "check_ecs_service_deployment_event_timestamps_monotonic_across_updates.py", line 93, in run
    assert len(events) >= 1, f"expected at least one service event, got {events}"
  AssertionError: expected at least one service event, got []
Note the preceding steps SUCCEEDED: a task reached RUNNING and ListServiceDeployments returned >= 3 non-PENDING deployment records, so deployments are recorded while events are not.

Cause 2 — ecs.capacity-provider-removal-while-in-use-rejected
Expected (AWS): PutClusterCapacityProviders(cluster, capacityProviders=[], defaultCapacityProviderStrategy=[]) while a running service still uses FARGATE -> HTTP 400, Error.Code == "ResourceInUseException", Error.Message == "The specified capacity provider is in use and cannot be removed." (fixed text, no variable parts), and DescribeClusters still shows "FARGATE" in clusters[0]["capacityProviders"]; after DeleteService(force=True) settles to INACTIVE the same call succeeds and capacityProviders converges to [].
Actual (emulator): Error: Check timed out after 60.0s — the run never reached the removal call; the setup path (CreateCluster -> PutClusterCapacityProviders(["FARGATE"]) -> RegisterTaskDefinition -> CreateService(FARGATE strategy, desiredCount=1) -> poll runningCount >= 1) never settled, so no status, code or message was ever produced for the in-use removal.

## Steps to Reproduce
Cause 1 (DescribeServices.events empty):
1. CreateCluster.
2. RegisterTaskDefinition three times in one family: networkMode=awsvpc, requiresCompatibilities=[FARGATE], cpu="256", memory="512", one essential container running `sleep 3600`.
3. CreateService(cluster, serviceName, taskDefinition=rev1, desiredCount=1, launchType=FARGATE, awsvpcConfiguration with a subnet + default SG, assignPublicIp=ENABLED); wait until a task in the service reports lastStatus==RUNNING.
4. Wait until ListServiceDeployments returns >= 1 record whose status has left PENDING, then UpdateService(taskDefinition=rev2) and UpdateService(taskDefinition=rev3), each time waiting for the deployment count to grow and the latest deployment to leave PENDING.
5. Call DescribeServices(cluster, [serviceName]) and read services[0]["events"].
   -> Emulator: [] . AWS: at least one ServiceEvent (unique id, createdAt, message), newest-first.
Replay: ecs.service-deployment-event-timestamps-monotonic-across-updates

Cause 2 (in-use capacity-provider removal never settles / never refused):
1. CreateCluster.
2. PutClusterCapacityProviders(cluster, capacityProviders=["FARGATE"], defaultCapacityProviderStrategy=[{capacityProvider: "FARGATE", weight: 1}]).
3. RegisterTaskDefinition: networkMode=awsvpc, requiresCompatibilities=[FARGATE], cpu="256", memory="512", one essential container running `sleep 3600`.
4. CreateService(cluster, serviceName, taskDefinition, desiredCount=1, capacityProviderStrategy=[{capacityProvider: "FARGATE", weight: 1}], awsvpcConfiguration); poll DescribeServices until runningCount >= 1.
   -> Emulator: this never settles; the check is killed at the 60s cap here.
5. Then call PutClusterCapacityProviders(cluster, capacityProviders=[], defaultCapacityProviderStrategy=[]).
   -> AWS: HTTP 400 ResourceInUseException, message "The specified capacity provider is in use and cannot be removed.", and DescribeClusters still lists "FARGATE".
6. DeleteService(force=True), wait for status INACTIVE, repeat step 5.
   -> AWS: succeeds and clusters[0]["capacityProviders"] converges to [].
Replay: ecs.capacity-provider-removal-while-in-use-rejected
