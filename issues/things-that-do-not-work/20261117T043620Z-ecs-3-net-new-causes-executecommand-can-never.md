Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.execute-command-requires-enabled-flag ecs.daemon-managed-rollout ecs.deployment-read-ops-on-missing-cluster-not-found
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.execute-command-requires-enabled-flag|run#29|AssertionError|assert code2 != code, (
Issue-Key: ecs|DescribeDaemonRevisions|InvalidParameterException
Issue-Key: ecs|DescribeDaemonDeployments|InvalidParameterException
Issue-Commit: 2f916589b2ea3e738a5dcf2ef5358049c7617ada

# ECS: 3 net-new causes — ExecuteCommand can never succeed, and the daemon deployment/revision reads refuse ARNs AWS answers with a MISSING failure

> _Note: this issue was identified against the code base at commit `2f916589b2ea3e738a5dcf2ef5358049c7617ada`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteCommand, DescribeDaemonDeployments, DescribeDaemonRevisions

## Description
Three net-new root causes in the ECS emulator, all reachable from the behavioral suite. They are independent and must each be closed before this issue is submitted.

=== Cause 1 of 3: ExecuteCommand has NO success path and no TargetNotConnectedException path — it refuses every call with the "execute command was not enabled" InvalidParameterException, whatever the task's enableExecuteCommand and whatever its ExecuteCommandAgent reports ===

AWS: Smithy ExecuteCommand (input cluster/task/container/command/interactive) returns ExecuteCommandResponse carrying clusterArn, containerArn, containerName, interactive and `session` (Session{sessionId, streamUrl, tokenValue}), HTTP 200. Its modeled errors include both InvalidParameterException (400) and TargetNotConnectedException (400). The documented split is: a task that was NOT run with execute-command enabled (or whose ExecuteCommandAgent is not running) earns InvalidParameterException with the fixed message "The execute command failed because execute command was not enabled when the task was run or the execute command agent isn't running. Wait and try again or run a new task with execute command enabled and try again."; a task whose flag IS enabled and whose agent is not reachable earns TargetNotConnectedException; a RUNNING task whose flag is enabled and whose per-container ExecuteCommandAgent managed agent is RUNNING gets a 200 with a session. The whole message above is fixed text — it carries no value from the request or from state.

Emulator: ops/execute_command.rs validates the ARN segments, the cluster, the task and the container name, and then unconditionally ends in `Err(InvalidParameterException{message: EXECUTE_COMMAND_NOT_AVAILABLE_MESSAGE})` (the handler's final expression, with a comment calling that precondition "the one this emulator can never meet"). It never reads the task's enableExecuteCommand, never reads the container's managedAgents, never returns TargetNotConnectedException and never mints a Session. The replay shows the two outcomes being IDENTICAL: the same task definition on the same container instance, run once with enableExecuteCommand=false and once with enableExecuteCommand=true, both answer InvalidParameterException with that same not-enabled message — and in the enabled run the emulator had already accepted a SubmitTaskStateChange managedAgents report and was reporting containers[app].managedAgents[ExecuteCommandAgent].lastStatus == "RUNNING" through DescribeTasks, so the emulator's own state says the precondition IS met. Replay detail: `AssertionError: enabling enableExecuteCommand must change ExecuteCommand's outcome -- got the SAME 'InvalidParameterException' with the flag True as with it False`.

Why: a client cannot open, stream or even negatively test an exec session, and — worse — it cannot attribute the refusal: the flag is not an observable boundary, so a consumer debugging "why can't I exec?" is told to re-run the task with the flag enabled when it already did. Any code that branches on TargetNotConnectedException (retry/wait) versus InvalidParameterException (reconfigure) takes the wrong branch every time.

Note: the open issue 20261104T062603Z-ecs-exec-the-executecommandagent-managed-agent-never-reaches.md covers the emulator never DRIVING ExecuteCommandAgent to RUNNING on its own. This cause is distinct and survives that fix: here the agent is already reported RUNNING in the emulator's own DescribeTasks answer and ExecuteCommand still refuses, because the handler has no success branch and no connectivity branch at all.

Fix: the ExecuteCommand handler (services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/src/ops/execute_command.rs) plus the exec/managed-agent part of the ECS state: gate the refusal on the task's recorded enableExecuteCommand and on the container's ExecuteCommandAgent status, mint a Session for the enabled+agent-running case, and answer TargetNotConnectedException for enabled-but-not-connected.

Traces: ecs.execute-command-requires-enabled-flag.

=== Cause 2 of 3: the daemon-scoped ARN syntax test encodes this emulator's own id minting (19 decimal digits) as a wire requirement, so DescribeDaemonRevisions / DescribeDaemonDeployments REFUSE a well-formed ARN whose id they did not mint instead of reporting it as a MISSING failure ===

AWS: DescribeDaemonRevisions (input daemonRevisionArns, required) and DescribeDaemonDeployments (input daemonDeploymentArns, required) both model an output pair `{<resource>List, failures: Failures}`. Failure is {arn, reason, detail}, and the ECS Describe* convention — and what the recording shows — is that an ARN that COULD name a resource of this account but names none is reported as a per-entry failure with reason "MISSING", at HTTP 200, so the entries that DID resolve are still returned. InvalidParameterException ("The specified parameter isn't valid") is for an entry that is not shaped like such an ARN at all. AWS's daemon revision/deployment ids are opaque; they are NOT a fixed-width decimal counter, and the recording exercises exactly that: an ARN built from a real revision's ARN with only its id replaced by the nil UUID 00000000-0000-0000-0000-000000000000 (same partition/region/account/cluster/daemon, a well-formed id) must come back as `daemonRevisions: []` + `failures: [{arn: <that ARN>, reason: "MISSING"}]`.

Emulator: state/daemon_scheduling.rs makes the id token part of the ARN's SHAPE — `is_daemon_sequence_id` requires `token.len() == DAEMON_SEQUENCE_ID_WIDTH (19)` and all ASCII digits — and `is_daemon_scoped_arn` folds that into `is_daemon_revision_arn` / `is_daemon_deployment_arn`. ops/describe_daemon_revisions.rs then refuses the WHOLE call with InvalidParameterException / message "The Daemon Revision ARN is invalid." (DAEMON_REVISION_ARN_INVALID_MESSAGE) for any entry failing that test, and ops/describe_daemon_deployments.rs does the same with "Request parameter(s) are invalid." (DAEMON_DEPLOYMENT_ARNS_INVALID_MESSAGE). Replay detail: `AssertionError: ECS Managed Daemon rollout not exercisable in this account/region: An error occurred (InvalidParameterException) when calling the DescribeDaemonRevisions operation: The Daemon Revision ARN is invalid.` In both messages the entire sentence is fixed text — the divergence is the status/code/message being produced at all, not any value inside it.

Why: the emulator's private id format becomes a client-visible contract. Any ARN a client holds that this process did not mint — one carried over from a real AWS run, from a fixture, or from another emulator instance — is rejected as malformed instead of reported missing, and the rejection is all-or-nothing: a batch that names nine resolvable revisions and one unknown one returns none of the nine. A client that distinguishes "gone" (reason MISSING, reconcile) from "bad input" (InvalidParameterException, stop and fix the caller) is told the wrong one.

Fix: the daemon ARN validation in the ECS state (state/daemon_scheduling.rs: is_daemon_scoped_arn / is_daemon_sequence_id) — the id token must be accepted as an opaque identifier rather than matched against this emulator's counter width, leaving "names nothing" to the per-entry failures channel the two handlers already build.

Traces: ecs.daemon-managed-rollout.

=== Cause 3 of 3: an entry whose ARN carries a cluster this store does not hold refuses the whole DescribeDaemonDeployments / DescribeDaemonRevisions call, where AWS reports it as a per-entry failure ===

AWS: the same two operations' modeled output carries `failures: Failures`, and the recording shows that a daemon-deployment/daemon-revision ARN that is well formed but names a cluster that does not exist comes back at HTTP 200 as `daemonDeployments: []` (resp. `daemonRevisions: []`) with a non-empty `failures` list for that ARN — the not-found convention every other ECS Describe* follows. The call is not refused.

Emulator: both handlers run `guard.names_a_missing_cluster(...)` AFTER the shape test and, if any entry's cluster is unknown, return InvalidParameterException for the whole request — ops/describe_daemon_deployments.rs with message "Request parameter(s) are invalid.", ops/describe_daemon_revisions.rs with "The Daemon Revision ARN is invalid." Both handlers' own doc comments state this as the intended rule ("an entry whose CLUSTER this account holds no record of ... earns the same refusal"), so it is a deliberate rule that does not match AWS. Replay detail: `AssertionError: daemon deployment/revision ops not exercisable here: An error occurred (InvalidParameterException) when calling the DescribeDaemonDeployments operation: Request parameter(s) are invalid.` Both messages are entirely fixed text.

Why: a deleted or mistyped cluster turns a read of N deployments into a hard 400 that loses the N-1 answers the caller could have had, and a client that polls a rollout across a cluster teardown sees a parameter error instead of an empty result plus a MISSING failure — so it stops rather than reconciling.

Fix: the cluster-resolution step of the two read handlers (ops/describe_daemon_deployments.rs, ops/describe_daemon_revisions.rs): an entry whose cluster is unknown must be pushed onto `failures` (reason MISSING) with the call still answering 200, not short-circuited into InvalidParameterException.

Traces: ecs.deployment-read-ops-on-missing-cluster-not-found.

## Evidence
Cause 1 — ExecuteCommand.
Expected (AWS): with task.enableExecuteCommand=true and containers[*].managedAgents[ExecuteCommandAgent].lastStatus=RUNNING, ExecuteCommand answers HTTP 200 with session{sessionId, streamUrl, tokenValue}; enabled-but-unreachable answers TargetNotConnectedException; only not-enabled/agent-not-running answers InvalidParameterException with the fixed message "The execute command failed because execute command was not enabled when the task was run or the execute command agent isn't running. Wait and try again or run a new task with execute command enabled and try again." The outcome therefore CHANGES when the flag changes.
Actual (emulator): both runs answer the identical InvalidParameterException with that same not-enabled message. Flag=false run: correct code and message (that leg passed). Flag=true run, after SubmitTaskStateChange reported managedAgents=[{containerName:app, managedAgentName:ExecuteCommandAgent, status:RUNNING}] and DescribeTasks confirmed lastStatus==RUNNING: still InvalidParameterException, same message. Replay: `AssertionError: enabling enableExecuteCommand must change ExecuteCommand's outcome -- got the SAME 'InvalidParameterException' with the flag True as with it False, so the earlier rejection cannot be attributed to the flag rather than to some other property of this container instance`. Source: ops/execute_command.rs ends in an unconditional Err(InvalidParameterException{EXECUTE_COMMAND_NOT_AVAILABLE_MESSAGE}).

Cause 2 — daemon-scoped ARN id syntax.
Expected (AWS): DescribeDaemonRevisions(daemonRevisionArns=[<real revision ARN with its id replaced by 00000000-0000-0000-0000-000000000000>]) -> HTTP 200, daemonRevisions: [], failures: [{arn: <that ARN>, reason: "MISSING"}].
Actual (emulator): HTTP 400 InvalidParameterException, message "The Daemon Revision ARN is invalid." Replay: `AssertionError: ECS Managed Daemon rollout not exercisable in this account/region: An error occurred (InvalidParameterException) when calling the DescribeDaemonRevisions operation: The Daemon Revision ARN is invalid.` Source: state/daemon_scheduling.rs `is_daemon_sequence_id` requires exactly DAEMON_SEQUENCE_ID_WIDTH=19 ASCII digits; ops/describe_daemon_revisions.rs returns InvalidParameterException when any entry fails is_daemon_revision_arn (same structure in ops/describe_daemon_deployments.rs via is_daemon_deployment_arn).

Cause 3 — unknown cluster in a daemon-scoped ARN.
Expected (AWS): DescribeDaemonDeployments(daemonDeploymentArns=[<real deployment ARN with its cluster segment swapped for a nonexistent cluster name>]) -> HTTP 200, daemonDeployments: [], failures non-empty for that ARN; likewise DescribeDaemonRevisions -> daemonRevisions: [], failures non-empty.
Actual (emulator): HTTP 400 InvalidParameterException, message "Request parameter(s) are invalid." for DescribeDaemonDeployments (and "The Daemon Revision ARN is invalid." for DescribeDaemonRevisions). Replay: `AssertionError: daemon deployment/revision ops not exercisable here: An error occurred (InvalidParameterException) when calling the DescribeDaemonDeployments operation: Request parameter(s) are invalid.` Source: the `guard.names_a_missing_cluster(...)` early return in ops/describe_daemon_deployments.rs and ops/describe_daemon_revisions.rs.

## Steps to Reproduce
Cause 1 (ExecuteCommand):
1. CreateCluster; RegisterContainerInstance into it; RegisterTaskDefinition with a task role and one container named "app".
2. RunTask(launchType=EC2, enableExecuteCommand=false); SubmitTaskStateChange(status=RUNNING) so the task is RUNNING.
3. ExecuteCommand(cluster, task, container="app", command="/bin/sh", interactive=true) — note the code/message (InvalidParameterException, "...execute command was not enabled when the task was run or the execute command agent isn't running..."). This leg is correct.
4. RunTask the SAME task definition on the SAME cluster with enableExecuteCommand=true; SubmitTaskStateChange(status=RUNNING, managedAgents=[{containerName:"app", managedAgentName:"ExecuteCommandAgent", status:"RUNNING"}]); confirm via DescribeTasks that containers[app].managedAgents[ExecuteCommandAgent].lastStatus == "RUNNING".
5. ExecuteCommand against that second task: the emulator returns the identical InvalidParameterException/not-enabled message instead of a 200 with a session (or TargetNotConnectedException). Expected: the outcome differs from step 3.

Cause 2 (daemon ARN id syntax):
1. CreateCluster; create a Managed Instances capacity provider; RegisterDaemonTaskDefinition; CreateDaemon(clusterArn, daemonName, capacityProviderArns, daemonTaskDefinitionArn).
2. Poll ListDaemonDeployments(daemonArn) until a deployment appears; DescribeDaemonDeployments on it and take targetDaemonRevision.arn.
3. Call DescribeDaemonRevisions(daemonRevisionArns=[<that ARN with its last "/"-segment replaced by "00000000-0000-0000-0000-000000000000">]).
4. Observe HTTP 400 InvalidParameterException "The Daemon Revision ARN is invalid." Expected: HTTP 200 with daemonRevisions: [] and failures: [{arn, reason: "MISSING"}]. The same shape test rejects a non-19-digit id for DescribeDaemonDeployments.

Cause 3 (unknown cluster):
1. Set up a daemon as in Cause 2 and capture daemonArn, daemonDeploymentArn and the target daemonRevisionArn.
2. Rebuild each of those ARNs with the cluster segment replaced by a cluster name that does not exist.
3. Call DescribeDaemonDeployments(daemonDeploymentArns=[<bogus-cluster deployment ARN>]) and DescribeDaemonRevisions(daemonRevisionArns=[<bogus-cluster revision ARN>]).
4. Observe HTTP 400 InvalidParameterException ("Request parameter(s) are invalid." / "The Daemon Revision ARN is invalid."). Expected: HTTP 200 with the empty list and a per-entry failures entry.
