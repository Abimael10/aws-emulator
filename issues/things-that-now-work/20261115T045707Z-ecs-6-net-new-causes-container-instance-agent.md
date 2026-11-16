Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ecs.container-instance-agent-update-rejected-missing-version ecs.update-container-agent-triggers-update-state ecs.container-instance-register-deregister-lifecycle ecs.register-container-instance-malformed-identity-document-rejected ecs.execute-command-requires-enabled-flag ecs.op-contract-deleteCapacityProvider
Issue-Shape-Id: com.amazonaws.ecs#AmazonEC2ContainerServiceV20141113
Issue-Key: ecs.container-instance-agent-update-rejected-missing-version|run#13|AssertionError|assert message == expected_message, f"expected message {expected_message!r}, got {message!r}"
Issue-Key: ecs.update-container-agent-triggers-update-state|run#12|AssertionError|assert not reg.get("versionInfo"), (
Issue-Key: ecs.container-instance-register-deregister-lifecycle|run#28|AssertionError|assert mutated["failures"][0]["reason"] == "INACTIVE", mutated["failures"][0]
Issue-Key: ecs.register-container-instance-malformed-identity-document-rejected|run#13|AssertionError|assert message == "An identity document was provided, but not valid.", message
Issue-Key: ecs.execute-command-requires-enabled-flag|run#29|AssertionError|assert code2 != code, (
Issue-Key: ecs.op-contract-deleteCapacityProvider|run#23|AssertionError|assert err == "ClientException", f"expected ClientException, got {err}"
Issue-Commit: da3e7884167b3f933d6f1406ab01004ac62fd391

# ecs: 6 net-new causes — container-instance/agent refusal messages are the Smithy doc prose or an internal parse error, versionInfo is echoed back, a deregistered instance fails MISSING instead of INACTIVE, enableExecuteCommand is not enforced, and DeleteCapacityProvider misclassifies an unknown name

> _Note: this issue was identified against the code base at commit `da3e7884167b3f933d6f1406ab01004ac62fd391`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
RegisterContainerInstance, UpdateContainerAgent, UpdateContainerInstancesState, ExecuteCommand, DeleteCapacityProvider

## Description
Six net-new root causes under the `ecs` heading, each with its own grounded failing check (Cause 1 of 6 … Cause 6 of 6). All six survived the suite's OWNED/ALREADY-FILED marking, so none of the open ECS filings carries their replay keys.

Cause 1 of 6 — UpdateContainerAgent's MissingVersionException message is the modeled documentation prose instead of the API message.
AWS: Smithy (ecs) UpdateContainerAgent errors include MissingVersionException (client error, HTTP 400). Its modeled documentation reads "Amazon ECS can't determine the current version of the Amazon ECS container agent on the container instance and doesn't have enough information to proceed with an update…", but the wire `Error.Message` AWS returns for this error class is the fixed text `Unknown current version` (recorded behavior only — the model carries no message template). The message is entirely fixed text; it has no variable parts.
Emulator: the error code is right (MissingVersionException) but the message is the documentation sentence: `Amazon ECS cannot determine the current version of the Amazon ECS container agent on the container instance and does not have enough information to proceed with an update.`
Why: a client that branches on the refusal message (or an operator reading it) sees a sentence AWS never sends; the message is being generated from the model's doc trait rather than from the service's message table, so every error class built the same way is wrong in the same way.
Fix: the UpdateContainerAgent handler's error construction — the MissingVersionException message constant in that operation's refusal path.
Traces: ecs.container-instance-agent-update-rejected-missing-version.

Cause 2 of 6 — RegisterContainerInstance echoes the client-supplied versionInfo back instead of discarding it.
AWS: Smithy (ecs) RegisterContainerInstance input carries `versionInfo` (VersionInfo) and output `containerInstance` (ContainerInstance). Recorded behavior only: on a manual registration that supplies a synthetic `versionInfo`, the returned containerInstance — and an independent DescribeContainerInstances read of it — carries NO versionInfo, because the agent version is server-recorded from the agent's own reporting, not accepted from the caller. That is precisely why UpdateContainerAgent then refuses with MissingVersionException/"Unknown current version" on such an instance.
Emulator: the supplied versionInfo (agentVersion/agentHash/dockerVersion) is stored and echoed verbatim on the RegisterContainerInstance response.
Why: the agent-version state becomes caller-controlled, so the whole agent-update state machine is reachable from a client that merely claims a version: UpdateContainerAgent stops refusing, agentUpdateStatus transitions that AWS never performs become observable, and any tooling that reads versionInfo to decide whether an agent needs updating is told a value AWS would not have recorded.
Fix: the RegisterContainerInstance handler's input handling and the container-instance record in ECS state — versionInfo must not be populated from the request.
Traces: ecs.update-container-agent-triggers-update-state.

Cause 3 of 6 — UpdateContainerInstancesState on a deregistered (INACTIVE) container instance fails with reason MISSING instead of INACTIVE.
AWS: Smithy (ecs) UpdateContainerInstancesState output = { containerInstances: ContainerInstances, failures: Failures }, each Failure { arn, reason, detail }. Recorded behavior only: after DeregisterContainerInstance the record survives as an independently describable INACTIVE container instance (DescribeContainerInstances still returns it, with status INACTIVE, and no failure), and a mutation on that id comes back in `failures` with reason `INACTIVE`. `MISSING` is the reason reserved for an id the service does not know at all, so the two are not interchangeable.
Emulator: the failure is `{"arn": "arn:aws:ecs:<region>:<account>:container-instance/<container-instance-id>", "reason": "MISSING"}` — the reason is the wrong discriminator (the arn/id in that payload is a per-run value, not part of the requirement).
Why: a client distinguishes "this instance is deregistered/dead for scheduling" from "this id does not exist" entirely by the failure reason; collapsing INACTIVE into MISSING makes a deregistered host look like a typo'd ARN and breaks reconciliation loops that retry on MISSING but not on INACTIVE.
Fix: the UpdateContainerInstancesState handler's instance resolution — a known-but-INACTIVE instance must be classified INACTIVE, not treated as unknown.
Traces: ecs.container-instance-register-deregister-lifecycle.

Cause 4 of 6 — RegisterContainerInstance leaks an internal identity-document parse error instead of AWS's InvalidParameterException message.
AWS: Smithy (ecs) RegisterContainerInstance errors include InvalidParameterException (client, HTTP 400). Recorded behavior only, and the message text is fixed for each case: a document that IS supplied but is not a valid signed instance identity document is refused with InvalidParameterException and the message `An identity document was provided, but not valid.`; omitting the document/signature pair entirely is refused with InvalidParameterException and the message `The identity document and identity document signature were not valid.` Neither message interpolates anything from the request.
Emulator: the code is right (InvalidParameterException) but the message for a malformed document is `accountId cannot be blank.` — an internal field-level parse complaint about the document's JSON body.
Why: the message names a request member (`accountId`) the caller never sent, so a client is pointed at the wrong input; it also reveals that validation is happening by destructuring the document rather than by validating the document/signature pair, which is the boundary AWS actually enforces.
Fix: the RegisterContainerInstance handler's identity-document validation — refuse the pair as a unit with the two fixed messages above, before any field-level parse.
Traces: ecs.register-container-instance-malformed-identity-document-rejected.

Cause 5 of 6 — ExecuteCommand's outcome does not depend on the task's enableExecuteCommand flag: the same refusal is returned with the flag true as with it false.
AWS: Smithy (ecs) ExecuteCommand errors include InvalidParameterException and TargetNotConnectedException (both client, HTTP 400). AWS checks the flag strictly before target connectivity: a RUNNING task launched WITHOUT enableExecuteCommand is refused with InvalidParameterException and the message `The execute command failed because execute command was not enabled when the task was run or the execute command agent isn't running. Wait and try again or run a new task with execute command enabled and try again.` (fixed text, no request-derived parts); once the flag IS enabled, the call either succeeds with a Session or is refused with TargetNotConnectedException when the managed agent is not connected — i.e. a different outcome.
Emulator: for two tasks that differ ONLY in enableExecuteCommand (same cluster, same container instance, same task definition, both advanced to RUNNING), ExecuteCommand returns the identical `InvalidParameterException` in both cases, so the flag is not part of the decision at all.
Why: the flag is a security-relevant boundary — a client cannot verify that exec is disabled-by-default and enabled-by-request, and cannot tell a not-enabled refusal from a not-connected one, so it retries the wrong failure forever.
Fix: the ExecuteCommand handler's preconditions — evaluate the task's enableExecuteCommand first (InvalidParameterException with the message above), then the ExecuteCommandAgent/target connectivity (TargetNotConnectedException), and let an enabled+connected task produce a Session. Adjacent open filing that does NOT carry this check's replay key: 20261104T062603Z-ecs-exec-the-executecommandagent-managed-agent-never-reaches.md.
Traces: ecs.execute-command-requires-enabled-flag.

Cause 6 of 6 — DeleteCapacityProvider on an unknown provider name is refused with the wrong error class.
AWS: Smithy (ecs) DeleteCapacityProvider errors include both ClientException and InvalidParameterException (both client, HTTP 400). Recorded behavior only: an unknown capacity-provider name is refused with `ClientException` and the message `Failed to get info for provided CapacityProvider` (fixed text; the name is not interpolated).
Emulator: `InvalidParameterException` for the same call.
Why: ECS uses ClientException for "the thing you named could not be resolved" and InvalidParameterException for "what you sent is malformed"; swapping them makes a non-existent provider indistinguishable from bad input, and client retry/branching keyed on the code takes the wrong path.
Fix: the DeleteCapacityProvider handler's not-found classification (the same name-resolution path that the operation's happy path uses to reach DEPROVISIONING/INACTIVE). Adjacent open filing that does NOT carry this check's replay key: 20261029T175650Z-ecs-7-net-new-root-causes-in-state.md.
Traces: ecs.op-contract-deleteCapacityProvider.

## Evidence
Replay of suite simparity-behavioral-aws-v1 against the booted emulator; service heading `ecs` 358/480 (75%), 122 failed — 116 of those marked OWNED/ALREADY FILED, the six ids below survive.

Cause 1 — [mismatch] ecs.container-instance-agent-update-rejected-missing-version
  check_ecs_container_instance_agent_update_rejected_missing_version.py:50 -> assert message == expected_message
  AssertionError: expected message 'Unknown current version', got 'Amazon ECS cannot determine the current version of the Amazon ECS container agent on the container instance and does not have enough information to proceed with an update.'
  Expected: code MissingVersionException, message "Unknown current version". Actual: code MissingVersionException, message = the modeled documentation sentence.

Cause 2 — [mismatch] ecs.update-container-agent-triggers-update-state
  check_ecs_update_container_agent_triggers_update_state.py:56 -> assert not reg.get("versionInfo")
  AssertionError: expected RegisterContainerInstance to discard the client-supplied versionInfo {'agentVersion': '1.0.0', 'agentHash': '00…0', 'dockerVersion': 'DockerVersion: 1.0.0'}, got it echoed back as the same object.
  Expected: no versionInfo on the response or on a later DescribeContainerInstances read. Actual: the caller's value echoed verbatim.

Cause 3 — [mismatch] ecs.container-instance-register-deregister-lifecycle
  check_ecs_container_instance_register_deregister_lifecycle.py:87 -> assert mutated["failures"][0]["reason"] == "INACTIVE"
  AssertionError: {'arn': 'arn:aws:ecs:us-west-2:000000000000:container-instance/00000000000000000000000000000001', 'reason': 'MISSING'}
  Expected: failures[0].reason == "INACTIVE" for UpdateContainerInstancesState(status=DRAINING) on a deregistered instance. Actual: reason "MISSING". (The earlier assertions — the instance still describable with status INACTIVE and absent from ListContainerInstances — passed, so only the failure reason diverges.)

Cause 4 — [mismatch] ecs.register-container-instance-malformed-identity-document-rejected
  check_ecs_register_container_instance_malformed_identity_document_rejected.py:67 -> assert message == "An identity document was provided, but not valid."
  AssertionError: accountId cannot be blank.
  Expected: InvalidParameterException / "An identity document was provided, but not valid." Actual: InvalidParameterException / "accountId cannot be blank."

Cause 5 — [mismatch] ecs.execute-command-requires-enabled-flag
  check_ecs_execute_command_requires_enabled_flag.py:110 -> assert code2 != code
  AssertionError: enabling enableExecuteCommand must change ExecuteCommand's outcome -- got the SAME 'InvalidParameterException' with the flag True as with it False, so the earlier rejection cannot be attributed to the flag rather than to some other property of this container instance
  Expected: flag False -> InvalidParameterException with the not-enabled message; flag True -> a different outcome (Session, or TargetNotConnectedException). Actual: InvalidParameterException both times.

Cause 6 — [mismatch] ecs.op-contract-deleteCapacityProvider
  check_ecs_op_contract_deleteCapacityProvider.py:76 -> assert err == "ClientException"
  AssertionError: expected ClientException, got InvalidParameterException
  Expected: ClientException / "Failed to get info for provided CapacityProvider". Actual: InvalidParameterException.

Smithy consulted (model services/service-com-amazonaws-ecs-amazonec2containerservicev20141113/smithy-model/com-amazonaws-ecs-amazonec2containerservicev20141113.json): UpdateContainerAgent errors {AccessDenied, Client, ClusterNotFound, InvalidParameter, MissingVersion, NoUpdateAvailable, Server, UpdateInProgress} and MissingVersionException's documentation (the sentence the emulator is emitting as the message); RegisterContainerInstance input {instanceIdentityDocument, instanceIdentityDocumentSignature, totalResources, versionInfo, …} errors {AccessDenied, Client, ClusterNotFound, InvalidParameter, Server}; UpdateContainerInstancesState output {containerInstances, failures}; ExecuteCommand errors {…, InvalidParameterException, TargetNotConnectedException}; DeleteCapacityProvider errors {AccessDenied, Client, ClusterNotFound, InvalidParameter, Server, UnsupportedFeature, UpdateInProgress}. The exact message texts and the ClientException-for-unknown-name classification are recorded behavior only.

## Steps to Reproduce
1. Clone the repo and replay the behavioral suite; all six ids in trace_ids reproduce.
2. Cause 1: CreateCluster; RegisterContainerInstance with a genuine signed identity document and NO versionInfo; call UpdateContainerAgent(cluster, containerInstance). Expect MissingVersionException with message "Unknown current version"; observe the modeled documentation sentence instead, and confirm agentUpdateStatus did not transition.
3. Cause 2: RegisterContainerInstance on the same cluster passing versionInfo={agentVersion:"1.0.0", agentHash:<40 zeros>, dockerVersion:"DockerVersion: 1.0.0"}. Expect no versionInfo on the response and none on a follow-up DescribeContainerInstances; observe it echoed back.
4. Cause 3: DeregisterContainerInstance(force=true) that instance, confirm DescribeContainerInstances still returns it with status INACTIVE, then call UpdateContainerInstancesState(containerInstances=[that arn], status="DRAINING"). Expect containerInstances=[] and failures[0].reason=="INACTIVE"; observe reason "MISSING".
5. Cause 4: RegisterContainerInstance(cluster, instanceIdentityDocument="not-a-valid-instance-identity-document", instanceIdentityDocumentSignature="not-a-valid-signature"). Expect InvalidParameterException / "An identity document was provided, but not valid."; observe "accountId cannot be blank." Then call it with the pair omitted entirely and expect InvalidParameterException / "The identity document and identity document signature were not valid."
6. Cause 5: RunTask the same task definition twice on the same cluster, once with enableExecuteCommand=false and once with true, advance both to RUNNING, and call ExecuteCommand(interactive=true, command="/bin/sh") on each. Expect the not-enabled InvalidParameterException for the first and a DIFFERENT outcome for the second; observe the identical InvalidParameterException.
7. Cause 6: CreateCapacityProvider, DeleteCapacityProvider it (expect DEPROVISIONING/INACTIVE), then DeleteCapacityProvider(capacityProvider="sim2real-does-not-exist-cp"). Expect ClientException / "Failed to get info for provided CapacityProvider"; observe InvalidParameterException.
8. Re-replay the six ids in trace_ids to confirm the fix.
