Issue-Severity: CRITICAL
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.capacity-provider-strategy-fargate-spot-task-placement ecs.delete-cluster-refused-with-running-count-then-inactive ecs.deploy-cfn-cluster-hosts-runtask-behavior ecs.deploy-ecs-taskdefinition-cfn-run-task-behaves ecs.e2e-capacity-provider-autoscaling-task-placement ecs.e2e-capacity-provider-fargate-spot-task-lifecycle ecs.e2e-disassociate-capacity-provider-in-use-rejected ecs.run-task-fargate-lifecycle-to-stopped ecs.run-task-lifecycle-transitions ecs.stop-task-sets-desired-stopped ecs.submit-task-state-change-updates-task-record ecs.task-protection-expires-after-ttl ecs.task-protection-enforced-during-scale-in ecs.health-check-start-period-suppresses-unhealthy-then-expires ecs.execute-command-fails-when-not-enabled-at-launch ecs.execute-command-requires-enabled-flag-rejected ecs.execute-command-session-credentials-round-trip ecs.execute-command-succeeds-when-enabled-at-runtime ecs.exec-disabled-task-session-rejected-while-enabled-streams ecs.exec-file-write-visible-to-second-session ecs.exec-interactive-stdout-round-trip ecs.exec-session-resize-propagates-to-pty ecs.exec-targets-the-named-container-not-a-sibling ecs.deploy-cfn-service-desired-count-materializes-tasks ecs.update-service-desired-count-keeps-the-primary-deployment ecs.awsvpc-task-attachment-details-match-network-configuration ecs.eni-attachment-reaches-attached ecs.deploy-ecs-cluster-service-cfn-reaches-steady-state ecs.cluster-task-counts-exact-through-run-and-stop ecs.essential-container-exit-stops-task-and-sidecar ecs.container-health-check-transitions-healthy-and-unhealthy
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.capacity-provider-strategy-fargate-spot-task-placement|run#10|AssertionError|assert task["lastStatus"] == "RUNNING", f"task never reached RUNNING: {task['lastStatus']}"
Issue-Key: ecs.delete-cluster-refused-with-running-count-then-inactive|run#16|AssertionError|assert running_status == "RUNNING", (
Issue-Key: ecs.deploy-cfn-cluster-hosts-runtask-behavior|run#21|AssertionError|assert last_status == "RUNNING", f"expected task lastStatus RUNNING, got {last_status!r}"
Issue-Key: ecs.deploy-ecs-taskdefinition-cfn-run-task-behaves|run#26|AssertionError|raise AssertionError(
Issue-Key: ecs.e2e-capacity-provider-autoscaling-task-placement|run#46|AssertionError|assert task["lastStatus"] == "RUNNING", f"task did not reach RUNNING: {task.get('stoppedReason')}"
Issue-Key: ecs.e2e-capacity-provider-fargate-spot-task-lifecycle|run#17|AssertionError|assert running_task["lastStatus"] == "RUNNING", (
Issue-Key: ecs.e2e-disassociate-capacity-provider-in-use-rejected|run#24|AssertionError|assert running["lastStatus"] == "RUNNING", f"task never reached RUNNING: {running.get('stoppedReason')}"
Issue-Key: ecs.run-task-fargate-lifecycle-to-stopped|run#12|AssertionError|assert running["lastStatus"] == "RUNNING", (
Issue-Key: ecs.run-task-lifecycle-transitions|run#7|AssertionError|assert task["lastStatus"] == "RUNNING", f"task never reached RUNNING: {task['lastStatus']}"
Issue-Key: ecs.stop-task-sets-desired-stopped|run#18|AssertionError|assert _last["lastStatus"] == "RUNNING", (
Issue-Key: ecs.submit-task-state-change-updates-task-record|_ec2_task_setup#2|AssertionError|raise AssertionError(
Issue-Key: ecs.task-protection-expires-after-ttl|run#18|AssertionError|raise AssertionError(
Issue-Key: ecs.task-protection-enforced-during-scale-in|run#33|AssertionError|assert final_states[protected_arn]["lastStatus"] == "RUNNING", (
Issue-Key: ecs.health-check-start-period-suppresses-unhealthy-then-expires|_statuses#10|AssertionError|assert task["lastStatus"] != "STOPPED", (
Issue-Key: ecs.execute-command-fails-when-not-enabled-at-launch|run#42|AssertionError|assert task2_running is not None and task2_running["lastStatus"] == "RUNNING", (
Issue-Key: ecs.execute-command-requires-enabled-flag-rejected|run#26|AssertionError|raise AssertionError(
Issue-Key: ecs.execute-command-session-credentials-round-trip|_wait_for_exec_agent_ready#18|AssertionError|raise AssertionError(f"task stopped before becoming ready for exec: {last_task.get('stoppedReason')}")
Issue-Key: ecs.execute-command-succeeds-when-enabled-at-runtime|run#7|AssertionError|raise AssertionError(f"task stopped before exec agent connected: {task.get('stoppedReason')}")
Issue-Key: ecs.exec-disabled-task-session-rejected-while-enabled-streams|_wait_for_exec_agent#11|AssertionError|raise AssertionError(
Issue-Key: ecs.exec-file-write-visible-to-second-session|_wait_for_exec_agent#10|AssertionError|raise AssertionError(
Issue-Key: ecs.exec-interactive-stdout-round-trip|_wait_for_exec_agent#9|AssertionError|raise AssertionError(
Issue-Key: ecs.exec-session-resize-propagates-to-pty|_wait_for_exec_agent#7|AssertionError|raise AssertionError(
Issue-Key: ecs.exec-targets-the-named-container-not-a-sibling|_wait_for_exec_agents#9|AssertionError|raise AssertionError(
Issue-Key: ecs.deploy-cfn-service-desired-count-materializes-tasks|run#18|AssertionError|assert all(s == "RUNNING" for s in statuses), f"expected both tasks RUNNING, got {statuses}"
Issue-Key: ecs.update-service-desired-count-keeps-the-primary-deployment|run#30|AssertionError|assert task["lastStatus"] == "RUNNING", (
Issue-Key: ecs.awsvpc-task-attachment-details-match-network-configuration|run#23|AssertionError|assert attached["status"] == "ATTACHED", f"ENI attachment ended {attached['status']!r}: {attached!r}"
Issue-Key: ecs.eni-attachment-reaches-attached|_poll_until#23|Timeout|time.sleep(interval_s)
Issue-Key: ecs.deploy-ecs-cluster-service-cfn-reaches-steady-state|run#12|AssertionError|assert clusters[0]["runningTasksCount"] >= svc["runningCount"], (
Issue-Key: ecs.cluster-task-counts-exact-through-run-and-stop|_poll_until#15|Timeout|time.sleep(interval_s)
Issue-Key: ecs.essential-container-exit-stops-task-and-sidecar|run#15|AssertionError|assert control["lastStatus"] == "RUNNING", (
Issue-Key: ecs.container-health-check-transitions-healthy-and-unhealthy|_poll_until#16|Timeout|time.sleep(interval_s)
Issue-Commit: f520e482964e34c3af4b1cbe916bb0f71c86032d

# ecs: the task data plane never produces a RUNNING task — 5 net-new causes (immediate EssentialContainerExited, tasks wedged in PROVISIONING, ENI attachment ends DELETED, cluster task counts never aggregated, non-essential exit stops the task)

> _Note: this issue was identified against the code base at commit `f520e482964e34c3af4b1cbe916bb0f71c86032d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RunTask

## Description
Five distinct root causes in the ECS task data plane. Each one independently prevents a task from ever being observed RUNNING, which is the precondition of most of this service's behavioural surface. Related but NOT covering open issues: 20261118T145001Z-ecs-2-net-new-causes-a-long-running.md and 20261104T062603Z-ecs-containers-never-execute-no-in-container-behaviour.md describe a similar family; the suite did not mark any of the checks below as owned by them, so they are filed here with their own replay keys.

Cause 1 of 5: RunTask/StartTask tasks go straight to STOPPED with stopCode EssentialContainerExited and never pass through RUNNING.
AWS: Smithy: Task.lastStatus moves PROVISIONING -> PENDING -> ACTIVATING -> RUNNING for a task whose container command is long-running (e.g. `sleep 300`, nginx), and Task.stopCode (enum com.amazonaws.ecs#TaskStopCode: TaskFailedToStart, EssentialContainerExited, UserInitiated, ServiceSchedulerInitiated, SpotInterruption, TerminationNotice, InfrastructureHealth) is only set once the task has stopped. EssentialContainerExited is the code for an essential container that RAN and then exited; a container that could not be started at all is TaskFailedToStart. The recordings show DescribeTasks reporting lastStatus RUNNING for these task definitions.
Emulator: every such task is reported, within a second of RunTask, as lastStatus='STOPPED', desiredStatus='STOPPED', stopCode='EssentialContainerExited', stoppedReason='Essential container in task exited', with the container carrying exitCode 0 and lastStatus 'STOPPED'. The container's command is never actually run, so the task terminates immediately regardless of the image or command.
Why: a client that waits for RUNNING (the ecs tasks-running waiter, every deploy script, every exec/observability flow) never proceeds; a client that reads stopCode mis-diagnoses a never-started task as an application exit.
Fix: the RunTask/StartTask handler and the task state machine that advances Task.lastStatus — the task must stay RUNNING while its essential container is alive, and stopCode must only be written on a real stop.
Traces: ecs.capacity-provider-strategy-fargate-spot-task-placement, ecs.delete-cluster-refused-with-running-count-then-inactive, ecs.deploy-cfn-cluster-hosts-runtask-behavior, ecs.deploy-ecs-taskdefinition-cfn-run-task-behaves, ecs.e2e-capacity-provider-autoscaling-task-placement, ecs.e2e-capacity-provider-fargate-spot-task-lifecycle, ecs.e2e-disassociate-capacity-provider-in-use-rejected, ecs.run-task-fargate-lifecycle-to-stopped, ecs.run-task-lifecycle-transitions, ecs.stop-task-sets-desired-stopped, ecs.submit-task-state-change-updates-task-record, ecs.task-protection-expires-after-ttl, ecs.task-protection-enforced-during-scale-in, ecs.health-check-start-period-suppresses-unhealthy-then-expires, ecs.execute-command-fails-when-not-enabled-at-launch, ecs.execute-command-requires-enabled-flag-rejected, ecs.execute-command-session-credentials-round-trip, ecs.execute-command-succeeds-when-enabled-at-runtime, ecs.exec-disabled-task-session-rejected-while-enabled-streams, ecs.exec-file-write-visible-to-second-session, ecs.exec-interactive-stdout-round-trip, ecs.exec-session-resize-propagates-to-pty, ecs.exec-targets-the-named-container-not-a-sibling.

Cause 2 of 5: tasks created by a service's desired count are wedged in PROVISIONING and never advance.
AWS: Smithy: Task.lastStatus for a Fargate awsvpc task advances PROVISIONING -> PENDING -> RUNNING once its ENI is attached; PROVISIONING is a transient state, not a resting one.
Emulator: DescribeTasks keeps returning lastStatus 'PROVISIONING' for the whole poll budget for tasks the service scheduler placed (both tasks of a desiredCount=2 service, and the task placed by UpdateService).
Why: a client cannot distinguish a stuck emulator from a slow placement; waiters never return and the service never reports runningCount.
Fix: the service scheduler / task state machine that advances a PROVISIONING task once its network attachment resolves.
Traces: ecs.deploy-cfn-service-desired-count-materializes-tasks, ecs.update-service-desired-count-keeps-the-primary-deployment.

Cause 3 of 5: the awsvpc ENI attachment of a task ends in DELETED and never reports ATTACHED.
AWS: Smithy: Attachment.status documents the valid values PRECREATED, CREATED, ATTACHING, ATTACHED, DETACHING, DETACHED, DELETED, FAILED; for a RUNNING awsvpc task the ElasticNetworkInterface attachment reports ATTACHED, with details carrying subnetId, networkInterfaceId, macAddress and privateIPv4Address, and it only reaches DELETED after the task stops.
Emulator: the attachment of type 'ElasticNetworkInterface' is observed with status 'DELETED' from the first DescribeTasks, never ATTACHING and never ATTACHED, while the details members are populated.
Why: a client that waits for the ENI to be ATTACHED before using the task's private IP (service discovery, ALB target registration, reachability probes) never proceeds, and one that reads DELETED concludes the network was torn down.
Fix: the task attachment state machine and the DescribeTasks serializer that projects Attachment.status.
Traces: ecs.awsvpc-task-attachment-details-match-network-configuration, ecs.eni-attachment-reaches-attached.

Cause 4 of 5: cluster-level task counters are not aggregated from the cluster's tasks.
AWS: Smithy: Cluster.runningTasksCount and Cluster.pendingTasksCount are the counts of tasks in the cluster in the RUNNING and PENDING states, so they are always consistent with the sum of the cluster's services' runningCount plus standalone tasks.
Emulator: DescribeClusters reports runningTasksCount=0 while DescribeServices on the same cluster reports runningCount=1, and the counts do not move across a RunTask/StopTask pair.
Why: a client that sizes capacity or gates a cluster delete on the cluster-level counters reads zero load for a loaded cluster.
Fix: the cluster state projection that derives runningTasksCount/pendingTasksCount/activeServicesCount, in the DescribeClusters handler.
Traces: ecs.deploy-ecs-cluster-service-cfn-reaches-steady-state, ecs.cluster-task-counts-exact-through-run-and-stop.

Cause 5 of 5: a NON-essential container exiting stops the whole task, and container health status never transitions.
AWS: Smithy: ContainerDefinition.essential — when essential is false, the exit of that container does not stop the task; only the exit of an essential container stops it, and that stop carries stopCode EssentialContainerExited. Container.healthStatus transitions HEALTHY/UNHEALTHY as the container's healthCheck runs.
Emulator: a task whose sidecar is declared essential=false is reported lastStatus='STOPPED' stopCode='EssentialContainerExited' stoppedReason='Essential container in task exited' when the non-essential container exits, and container healthStatus stays UNKNOWN so no HEALTHY/UNHEALTHY transition is ever observable.
Why: a client relying on sidecars (log routers, init containers) sees its whole task killed, and health-check-driven deployment logic has no signal.
Fix: the essential/non-essential stop logic in the task state machine and the container health-check evaluator.
Traces: ecs.essential-container-exit-stops-task-and-sidecar, ecs.container-health-check-transitions-healthy-and-unhealthy.

## Evidence
Cause 1 — expected lastStatus 'RUNNING'; got, e.g. for ecs.run-task-fargate-lifecycle-to-stopped: "task never reached RUNNING (stopped early: Essential container in task exited)"; for ecs.run-task-lifecycle-transitions: "task never reached RUNNING: STOPPED"; for ecs.exec-interactive-stdout-round-trip: "task stopped before it was execable: 'Essential container in task exited'". The fixed text of the emulator's stoppedReason is "Essential container in task exited" and the stopCode is "EssentialContainerExited"; AWS writes that pair only for an essential container that actually ran and exited.
Cause 2 — expected both tasks RUNNING, got ['PROVISIONING', 'PROVISIONING'] (ecs.deploy-cfn-service-desired-count-materializes-tasks); "the placed task is 'PROVISIONING', expected 'RUNNING'" (ecs.update-service-desired-count-keeps-the-primary-deployment).
Cause 3 — "ENI attachment ended 'DELETED'": {'type': 'ElasticNetworkInterface', 'status': 'DELETED', details: subnetId/networkInterfaceId/macAddress/privateIPv4Address present}. Expected status 'ATTACHED' while the task is RUNNING.
Cause 4 — "cluster runningTasksCount=0 < service runningCount=1".
Cause 5 — "a non-essential container exiting must NOT stop the task, got lastStatus='STOPPED' stopCode='EssentialContainerExited' stoppedReason='Essential container in task exited'". Container records show healthStatus 'UNKNOWN' throughout.

## Steps to Reproduce
1. CreateCluster; RegisterTaskDefinition with a FARGATE awsvpc revision whose single essential container runs a long-lived command (e.g. sleep 300).
2. RunTask on that cluster with an awsvpcConfiguration; poll DescribeTasks. Cause 1: lastStatus becomes STOPPED with stopCode EssentialContainerExited within ~1s instead of reaching RUNNING. Cause 3: the ElasticNetworkInterface attachment reports status DELETED, never ATTACHED.
3. CreateService with desiredCount 2 on the same task definition and poll ListTasks/DescribeTasks. Cause 2: both tasks stay PROVISIONING.
4. DescribeClusters while DescribeServices reports runningCount >= 1. Cause 4: runningTasksCount is 0.
5. RegisterTaskDefinition with two containers, the sidecar essential=false and exiting quickly. RunTask. Cause 5: the task stops with EssentialContainerExited; container healthStatus never leaves UNKNOWN.
