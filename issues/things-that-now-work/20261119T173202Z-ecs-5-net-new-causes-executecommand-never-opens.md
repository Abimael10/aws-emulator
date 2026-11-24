Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.e2e-run-task-execute-command-session-lifecycle ecs.exec-disabled-task-session-rejected-while-enabled-streams ecs.exec-file-write-visible-to-second-session ecs.exec-interactive-stdout-round-trip ecs.exec-session-resize-propagates-to-pty ecs.exec-targets-the-named-container-not-a-sibling ecs.execute-command-server-managed-session ecs.execute-command-session-credentials-round-trip ecs.execute-command-session-expiry ecs.execute-command-succeeds-when-enabled-at-runtime ecs.execute-command-rejected-without-ssm-connection ecs.deploy-ecs-capacity-provider-cfn-usable-for-run-task ecs.deploy-ecs-capacityprovider-cfn-association-enforced ecs.ephemeral-storage-request-is-provisioned-inside-the-container ecs.discover-poll-endpoint-returns-agent-endpoint
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs|ExecuteCommand|TargetNotConnectedException
Issue-Key: ecs.execute-command-rejected-without-ssm-connection|run#12|AssertionError|assert not already_running, "exec agent connected despite lacking ssmmessages permissions"
Issue-Key: ecs.deploy-ecs-capacity-provider-cfn-usable-for-run-task|run#23|KeyError|assert task["launchType"] == "EC2", f"expected launchType EC2, got {task['launchType']!r}"
Issue-Key: ecs.deploy-ecs-capacityprovider-cfn-association-enforced|run#15|AssertionError|assert task["lastStatus"] == "RUNNING", f"task did not reach RUNNING: {task.get('stoppedReason')}"
Issue-Key: ecs.ephemeral-storage-request-is-provisioned-inside-the-container|_measured_gib#24|AssertionError|assert isinstance(code, int) and 0 < code < _DF_FAILED, (
Issue-Key: ecs.discover-poll-endpoint-returns-agent-endpoint|run#10|AssertionError|assert first["endpoint"] != first["telemetryEndpoint"], "endpoint and telemetryEndpoint must not be identical"
Issue-Commit: 23a6305fb4194ac652f562ce813f73dafa581013

# ecs: 5 net-new causes — ExecuteCommand never opens a session (TargetNotConnectedException for every exec-enabled task), the exec agent is RUNNING without ssmmessages permissions, Task.launchType is omitted, tasks stop with EssentialContainerExited instead of running, and DiscoverPollEndpoint returns one URL twice

> _Note: this issue was identified against the code base at commit `23a6305fb4194ac652f562ce813f73dafa581013`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteCommand, DescribeTasks, RunTask, DiscoverPollEndpoint

## Description
Five independent net-new root causes in the ECS emulator, all reachable from the behavioral suite. Each must be closed before this issue is submitted.

=== Cause 1 of 5: ExecuteCommand has no success path — every call against an exec-enabled RUNNING task is refused with TargetNotConnectedException, so no exec session is ever opened ===
AWS: Smithy ExecuteCommand (input cluster, container, command [required], interactive [required], task [required]) returns ExecuteCommandResponse{clusterArn, containerArn, containerName, interactive, session: Session{sessionId, streamUrl, tokenValue}, taskArn} at HTTP 200 for a RUNNING task that was run with enableExecuteCommand=true and whose per-container ExecuteCommandAgent managed agent reports lastStatus=RUNNING. TargetNotConnectedException (client, httpError 400) is modelled for the configuration failures its own documentation lists — incorrect IAM permissions, the SSM agent not installed/not running, or a missing Session Manager VPC endpoint — not for a correctly configured, connected task.
Emulator: every ExecuteCommand call in this suite is refused `TargetNotConnectedException: The execute command failed due to an internal error. Wait and try again.` (HTTP 400), including tasks launched with enableExecuteCommand=true that the emulator's own DescribeTasks reports as RUNNING with the ExecuteCommandAgent agent present. No Session is ever minted, so no stream URL/token exists, and ten checks that open, stream, resize, re-enter or expire a session cannot proceed past the first call. Note: the open issue 20261117T043620Z-ecs-3-net-new-causes-executecommand-can-never.md describes the handler returning the "not enabled" InvalidParameterException unconditionally, and 20261116T215400Z-ecs-executecommand-on-an-exec-enabled-task-whose-agent.md describes the agent-not-running case answering TargetNotConnectedException where InvalidParameterException is due; neither covers this: the refusal is now TargetNotConnectedException for the CONNECTED case and the handler still has no 200+Session branch.
Why: ECS Exec is unusable. A client cannot open an interactive session, cannot read a container's stdout, cannot write a file visible to a second session, and cannot even negatively test exec: a consumer that branches on TargetNotConnectedException (wait/retry, check VPC endpoints) retries forever against a task that is in fact connected.
Fix: the ExecuteCommand handler (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/execute_command.rs) and the exec/managed-agent part of the ECS state: for a RUNNING, exec-enabled task whose ExecuteCommandAgent is RUNNING, mint a Session (sessionId, streamUrl, tokenValue) and answer 200, serving that stream; keep TargetNotConnectedException for the genuinely unconnected case only.
Traces: ecs.e2e-run-task-execute-command-session-lifecycle, ecs.exec-disabled-task-session-rejected-while-enabled-streams, ecs.exec-file-write-visible-to-second-session, ecs.exec-interactive-stdout-round-trip, ecs.exec-session-resize-propagates-to-pty, ecs.exec-targets-the-named-container-not-a-sibling, ecs.execute-command-server-managed-session, ecs.execute-command-session-credentials-round-trip, ecs.execute-command-session-expiry, ecs.execute-command-succeeds-when-enabled-at-runtime.

=== Cause 2 of 5: the ExecuteCommandAgent managed agent is reported RUNNING for a task whose task role has no ssmmessages permissions ===
AWS: recorded behavior only (the managedAgents representation is modelled; the gating is documented ECS Exec behavior). ECS Exec requires the TASK ROLE to allow ssmmessages:CreateControlChannel/CreateDataChannel/OpenControlChannel/OpenDataChannel. Without them the agent can never connect, so containers[*].managedAgents[ExecuteCommandAgent].lastStatus never becomes RUNNING, and ExecuteCommand is refused with InvalidParameterException carrying the fixed text "The execute command failed because execute command was not enabled when the task was run or the execute command agent isn't running. Wait and try again or run a new task with execute command enabled and try again." (all fixed text; no request or state values).
Emulator: for a RUNNING, exec-enabled task whose task role grants no ssmmessages action, DescribeTasks reports an ExecuteCommandAgent whose lastStatus is RUNNING. Replay detail: `AssertionError: exec agent connected despite lacking ssmmessages permissions`. The agent status is synthesized from the exec flag alone, with no reference to the task role's policy.
Why: the IAM precondition for ECS Exec is invisible. A client validating least privilege sees a "connected" agent it has not authorized, and a consumer diagnosing a real exec failure is pointed away from the missing ssmmessages grant — the one thing it must fix.
Fix: the managed-agent state the ECS emulator attaches to a task (and the ExecuteCommand refusal that reads it): drive ExecuteCommandAgent to RUNNING only when the task role authorizes the ssmmessages channel actions, otherwise leave it non-RUNNING and refuse ExecuteCommand with the InvalidParameterException message above.
Traces: ecs.execute-command-rejected-without-ssm-connection.

=== Cause 3 of 5: a task placed through a capacity-provider strategy carries no launchType member at all ===
AWS: Smithy Task.launchType ("The infrastructure where your task runs on"); the recording shows a task placed by RunTask with capacityProviderStrategy naming an EC2/ASG-backed capacity provider coming back from DescribeTasks with launchType = "EC2" alongside its containerInstanceArn.
Emulator: the task object returned for such a placement has no launchType key — the test's access raises `KeyError: 'launchType'`. The member is omitted rather than set from the provider's infrastructure.
Why: launchType is how a client tells an EC2-hosted task from a Fargate one (it drives placement reasoning, cost attribution, and whether containerInstanceArn/ENI fields are meaningful). An omitted member breaks any typed consumer that reads task.launchType and silently mis-buckets tasks in untyped ones.
Fix: the task representation in the ECS state/serializer for RunTask/DescribeTasks — when placement resolves through a capacity provider, record and emit the resulting launchType (EC2 for an EC2/ASG-backed provider, FARGATE for a Fargate provider) on the Task.
Traces: ecs.deploy-ecs-capacity-provider-cfn-usable-for-run-task.

=== Cause 4 of 5: a placed task stops immediately with stoppedReason "Essential container in task exited" and no exitCode instead of running its container's command ===
AWS: recorded behavior only. A task whose essential container runs a long-lived command (`sh -c "sleep 300"`, nginx, a `df` probe) reaches lastStatus=RUNNING and stays there until stopped; the container's command actually executes, and when a container does exit the task's container carries the real exitCode and the task's stoppedReason reflects it.
Emulator: such tasks go straight to STOPPED with stoppedReason "Essential container in task exited" and no exit code — a FARGATE_SPOT task from a CFN-set default capacity-provider strategy never reaches RUNNING (`AssertionError: task did not reach RUNNING: Essential container in task exited`), and a container whose command reports its filesystem size through its exit status yields no code at all (`...probe must report its filesystem size in GiB as its exit code, got None (reason=None, stoppedReason='Essential container in task exited')`). The container's command is never executed. The resolved issue 20261111T145804Z-ecs-task-data-plane-container-commands-never-execute.md covered the earlier form of this (exit 127); the current form is an immediate essential-container exit with NO exitCode, and the open issue 20261104T062603Z-ecs-containers-never-execute-no-in-container-behaviour.md does not carry these checks.
Why: no task-hosted behavior can be observed or tested: services never reach steady state, capacity-provider placement cannot be verified, and any in-container assertion (ephemeral storage size, process state, filesystem) is unreachable. A client waiting for RUNNING instead gets an immediate STOPPED with a reason that is not attributable to anything it configured.
Fix: the ECS task data plane — run the container definition's command and keep the task RUNNING while it lives; record the real exitCode on the container when it exits and derive stoppedReason from it, rather than settling every task on EssentialContainerExited with no code.
Traces: ecs.deploy-ecs-capacityprovider-cfn-association-enforced, ecs.ephemeral-storage-request-is-provisioned-inside-the-container.

=== Cause 5 of 5: DiscoverPollEndpoint returns the same URL for endpoint and telemetryEndpoint ===
AWS: Smithy DiscoverPollEndpoint output {endpoint, telemetryEndpoint, serviceConnectEndpoint}. These are DISTINCT endpoints: the agent polls its control endpoint and ships telemetry to a different one; the recording asserts both are https URLs, that they differ, and that repeated calls for the same container instance return the same pair.
Emulator: for a registered container instance, endpoint and telemetryEndpoint are identical. Replay detail: `AssertionError: endpoint and telemetryEndpoint must not be identical`. (The resolved issue 20261110T175605Z-ecs-discoverpollendpoint-publishes-real-aws-agent-and-telemetry.md made both hosts served; they are now served but collapsed onto one value.)
Why: an agent that uses the response to address two different planes sends telemetry to the control endpoint; any consumer that distinguishes the two cannot, and a test that asserts the pair's shape fails on an otherwise working emulator.
Fix: the DiscoverPollEndpoint handler (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/discover_poll_endpoint.rs): emit two distinct, stable https endpoints per container instance — the agent endpoint and a separate telemetry endpoint — the mapping stable across repeated calls.
Traces: ecs.discover-poll-endpoint-returns-agent-endpoint.

## Evidence
Cause 1 — ExecuteCommand.
Expected (AWS): ExecuteCommand(cluster, task, container, command="/bin/sh", interactive=true) on a RUNNING, exec-enabled task whose ExecuteCommandAgent is RUNNING -> HTTP 200 with session{sessionId, streamUrl, tokenValue}.
Actual (emulator): HTTP 400 `TargetNotConnectedException: An error occurred (TargetNotConnectedException) when calling the ExecuteCommand operation: The execute command failed due to an internal error. Wait and try again.` — identical for all ten exec checks (session lifecycle, interactive stdout, file write across sessions, resize, container targeting, server-managed session, session credentials, session expiry, enabled-at-runtime, and the enabled leg of the disabled/enabled differential).

Cause 2 — exec agent vs ssmmessages.
Expected (AWS): with a task role granting no ssmmessages:* action, DescribeTasks never reports containers[*].managedAgents[ExecuteCommandAgent].lastStatus == "RUNNING", and ExecuteCommand answers InvalidParameterException "The execute command failed because execute command was not enabled when the task was run or the execute command agent isn't running. Wait and try again or run a new task with execute command enabled and try again."
Actual (emulator): the agent is reported RUNNING. Replay: `AssertionError: exec agent connected despite lacking ssmmessages permissions` (check line 55).

Cause 3 — Task.launchType.
Expected (AWS): DescribeTasks for a task placed via capacityProviderStrategy on an ASG-backed provider reports launchType "EC2" and containerInstanceArn = the ASG-registered instance.
Actual (emulator): the task has no launchType member. Replay: `KeyError: 'launchType'` at `assert task["launchType"] == "EC2"` (check line 90).

Cause 4 — tasks stop instead of running.
Expected (AWS): a task whose essential container runs a long-lived command reaches lastStatus RUNNING; a container that exits reports its exitCode.
Actual (emulator): `AssertionError: task did not reach RUNNING: Essential container in task exited` (check_ecs_deploy_ecs_capacityprovider_cfn_association_enforced.py line 99) and `AssertionError: the control (no ephemeralStorage) probe must report its filesystem size in GiB as its exit code, got None (reason=None, stoppedReason='Essential container in task exited')` (check_ecs_ephemeral_storage_request_is_provisioned_inside_the_container.py line 218).

Cause 5 — DiscoverPollEndpoint.
Expected (AWS): endpoint and telemetryEndpoint are both https and differ, stable across calls.
Actual (emulator): the two values are equal. Replay: `AssertionError: endpoint and telemetryEndpoint must not be identical` (check line 38).

## Steps to Reproduce
Cause 1 (ExecuteCommand):
1. CreateCluster; RegisterTaskDefinition with a task role and one long-lived container; RunTask(enableExecuteCommand=true).
2. Wait until DescribeTasks reports lastStatus RUNNING (and the ExecuteCommandAgent managed agent present/RUNNING).
3. ExecuteCommand(cluster, task, container, command="/bin/sh", interactive=true).
4. Observe HTTP 400 TargetNotConnectedException "The execute command failed due to an internal error. Wait and try again." Expected: HTTP 200 with a session whose streamUrl/tokenValue can be opened.

Cause 2 (exec agent without ssmmessages):
1. Create a task role whose policy grants NO ssmmessages:* actions.
2. RunTask(enableExecuteCommand=true) with that task role; wait for RUNNING.
3. DescribeTasks and read containers[*].managedAgents[ExecuteCommandAgent].lastStatus.
4. Observe RUNNING. Expected: not RUNNING, and ExecuteCommand refused with the InvalidParameterException not-enabled/agent-not-running message.

Cause 3 (launchType):
1. CreateStack with an AWS::ECS::CapacityProvider over an ASG plus AWS::ECS::ClusterCapacityProviderAssociations; wait for an ACTIVE container instance.
2. RegisterTaskDefinition (networkMode bridge, one container, command sh -c "sleep 300").
3. RunTask(cluster, taskDefinition, capacityProviderStrategy=[{capacityProvider:<cp>, weight:1}]).
4. DescribeTasks on the returned taskArn: the task carries no launchType. Expected launchType "EC2".

Cause 4 (essential container exits):
1. CreateStack with an AWS::ECS::Cluster + ClusterCapacityProviderAssociations whose DefaultCapacityProviderStrategy names FARGATE_SPOT.
2. RegisterTaskDefinition with a long-lived container; RunTask with no launchType/strategy so the cluster default applies.
3. Poll DescribeTasks: the task goes to STOPPED with stoppedReason "Essential container in task exited" and no container exitCode. Expected: RUNNING, the command executing.
4. Repeat with a container whose command exits with its filesystem size as the exit code: no exitCode is reported at all.

Cause 5 (DiscoverPollEndpoint):
1. CreateCluster; RegisterContainerInstance with a valid instance identity document.
2. DiscoverPollEndpoint(cluster, containerInstance).
3. Compare endpoint and telemetryEndpoint: they are identical. Expected: two distinct https endpoints, stable across repeated calls.
