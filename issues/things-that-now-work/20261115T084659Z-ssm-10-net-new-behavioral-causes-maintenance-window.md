Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: ssm.delete-maintenance-window-then-get-fails ssm.register-deregister-maintenance-window-target ssm.get-ops-summary-invalid-aggregator-rejected ssm.automation-execute-aws-api-reaches-success ssm.send-automation-signal-nonexistent-step-rejected ssm.send-command-zero-target-invocations ssm.run-command-nonzero-exit-surfaces-as-failed-with-the-exact-response-code ssm.terminate-session-already-terminated-noop ssm.ssm-automation-branch-takes-one-path-and-the-other-branchs-effect-is-absent ssm.ssm-automation-execute-script-computes-its-output-in-the-managed-runtime
Issue-Shape-Id: com.amazonaws.ssm#AmazonSSM
Issue-Key: ssm.delete-maintenance-window-then-get-fails|run#12|AssertionError|assert update_code == "DoesNotExistException", (
Issue-Key: ssm|DeregisterTargetFromMaintenanceWindow|DoesNotExistException
Issue-Key: ssm.get-ops-summary-invalid-aggregator-rejected|run#13|AssertionError|assert len(clauses) == 2 and set(clauses) == expected_clauses, (
Issue-Key: ssm.automation-execute-aws-api-reaches-success|run#44|AssertionError|assert unknown_doc == (
Issue-Key: ssm.send-automation-signal-nonexistent-step-rejected|run#13|AssertionError|assert refused == (
Issue-Key: ssm.send-command-zero-target-invocations|run#17|AssertionError|assert code == "InvocationDoesNotExist", code
Issue-Key: ssm.run-command-nonzero-exit-surfaces-as-failed-with-the-exact-response-code|_legs#5|AssertionError|assert _STDERR_MARKER in bad["StandardErrorContent"], (
Issue-Key: ssm.terminate-session-already-terminated-noop|_error#9|AssertionError|raise AssertionError(
Issue-Key: ssm.ssm-automation-branch-takes-one-path-and-the-other-branchs-effect-is-absent|run#29|AssertionError|assert none_steps == [
Issue-Key: ssm.ssm-automation-execute-script-computes-its-output-in-the-managed-runtime|run#23|AssertionError|assert projected_runtime[0].startswith("3.11."), (
Issue-Commit: c49ab90e9518fca4a0d62f5c8e5e4d50d5a70ed9

# ssm: 10 net-new behavioral causes — maintenance-window mutation/deregistration state, Automation branch & executeScript runtime, Run Command invocation lookup and stderr, session termination, and three re-worded refusals

> _Note: this issue was identified against the code base at commit `c49ab90e9518fca4a0d62f5c8e5e4d50d5a70ed9`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateMaintenanceWindow

## Description
Ten independent net-new SSM fidelity defects found by replaying the behavioral suite against this checkout. This bundle carries 10 causes, numbered below; a fixer must close ALL TEN before submitting this issue. Smithy model used throughout: services/service-com-amazonaws-ssm-amazonssm/smithy-model/com-amazonaws-ssm-amazonssm.json.

Cause 1 of 10: UpdateMaintenanceWindow against a deleted WindowId answers 501 NotImplemented instead of DoesNotExistException.
- AWS: Smithy UpdateMaintenanceWindow input WindowId (required, MaintenanceWindowId, @length 20..20, @pattern ^mw-[0-9a-f]{17}$); modeled errors DoesNotExistException (client, httpError 400) and InternalServerError. After DeleteMaintenanceWindow, a mutation against the dead id is refused HTTP 400 with error code DoesNotExistException; the message's fixed text is AWS's maintenance-window not-found sentence with the requested WindowId substituted from the request (recorded behavior for this error class: "Maintenance window <WindowId> does not exist" — the id is a per-run value, the sentence is the requirement).
- Emulator: answers error code 'NotImplemented' (the operation is not served at all), so the modeled refusal contract of the operation is unreachable.
- Why: a client cannot distinguish "this window is gone" from "this emulator cannot update windows". Reconcilers that update a window and branch on DoesNotExistException to recreate it instead see a non-modeled, non-retryable server error and abort; and no caller can ever update a live window.
- Fix: implement the UpdateMaintenanceWindow handler in the ssm service crate (services/service-com-amazonaws-ssm-amazonssm/src/ops/update_maintenance_window.rs) over the maintenance-window store, including the absent-WindowId refusal path (400 DoesNotExistException).
- Traces: ssm.delete-maintenance-window-then-get-fails.
- Related open issue: 20261112T101725Z-ssm-maintenance-windows-diverge-deleted-window-still-readable.md covers the GetMaintenanceWindow read path and Cutoff validation on the WIRE corpus; it does not cover UpdateMaintenanceWindow being unserved.

Cause 2 of 10: DeregisterTargetFromMaintenanceWindow is not idempotent — a second call for an already-deregistered WindowTargetId is refused DoesNotExistException instead of succeeding.
- AWS: Smithy DeregisterTargetFromMaintenanceWindow input WindowId + WindowTargetId (both required), output echoes both; modeled errors DoesNotExistException, TargetInUseException, InternalServerError. Recorded behavior: deregistering a target that is already gone is a no-op that returns HTTP 200 echoing WindowId/WindowTargetId — the dead target is neither resurrected nor reported missing. (DoesNotExistException is modeled for an unknown WindowId, not for a target already removed from a live window.)
- Emulator: the second call raises DoesNotExistException with the message "Maintenance window <WindowId> has no target <WindowTargetId>" (ids are per-run values), so the call fails where AWS succeeds.
- Why: every idempotent teardown and retry path breaks: a cleanup that retries after a timeout, or a converging controller that deregisters unconditionally, now takes a hard error for an already-achieved desired state.
- Fix: the DeregisterTargetFromMaintenanceWindow handler (services/service-com-amazonaws-ssm-amazonssm/src/ops/deregister_target_from_maintenance_window.rs) — treat an absent WindowTargetId under an existing window as success and echo the request's ids; reserve DoesNotExistException for an unknown WindowId.
- Traces: ssm.register-deregister-maintenance-window-target.

Cause 3 of 10: GetOpsSummary's ValidationException pattern clause renders the member @pattern with its trailing anchor, so the message text diverges from AWS's.
- AWS: Smithy OpsAggregator.AggregatorType targets OpsAggregatorType (@length min 1 max 20, @pattern ^(range|count|sum)$); an out-of-range value is refused with error code ValidationException. Recorded behavior: the message is the fixed prefix "2 validation errors detected: " followed by two clauses joined by "; ", in nondeterministic order; each clause is "Value '<value>' at 'aggregators.1.member.aggregatorType' failed to satisfy constraint: Member must " + either "have length less than or equal to 20" or "satisfy regular expression pattern: ^(range|count|sum)". The fixed text is the prefix, the "Value ... at ... failed to satisfy constraint: Member must ..." frame and the two constraint phrases; the variable parts are the rejected value and the member path, both taken from the request.
- Emulator: emits the same prefix, path and both clauses, but renders the regex clause as "... pattern: ^(range|count|sum)$" — it echoes the model pattern verbatim, including the trailing '$', where AWS's message stops at the closing parenthesis.
- Why: the message is the only machine-readable description of which constraint failed; clients and conformance tests that match AWS's constraint text (or diff it) fail against the emulator even though the status and error code are right.
- Fix: the shared constraint-violation message renderer used by SSM input validation (the ValidationException formatter feeding GetOpsSummary's aggregator validation) — render the pattern clause the way AWS does for this error class rather than echoing the Smithy @pattern string.
- Traces: ssm.get-ops-summary-invalid-aggregator-rejected.
- Related open issue: 20261031T024432Z-sagemaker-runtime-validationerror-constraint-message-renders-the-member.md is the same family of defect in another service (pattern anchors rendered differently from AWS) and does not cover ssm.

Cause 4 of 10: StartAutomationExecution with an unknown DocumentName carries the wrong message for AutomationDefinitionNotFoundException.
- AWS: Smithy StartAutomationExecution declares AutomationDefinitionNotFoundException (client, httpError 400, awsQueryError code AutomationDefinitionNotFound). Recorded behavior: the message is entirely fixed text for this error class — "Invalid input for field document name, expected valid document in account." — it contains no value from the request.
- Emulator: returns the right error code but an emulator-authored message that interpolates the requested name: "Document <DocumentName> does not exist." (the document name is the request's value).
- Why: clients and runbook tooling that match AWS's sentence (or assert on it in negative tests) mis-handle the refusal, and the emulator's wording collides with the SSM *document* API's not-found wording, blurring "no such automation definition" and "no such document version".
- Fix: StartAutomationExecution's document-resolution refusal path (services/service-com-amazonaws-ssm-amazonssm/src/ops/start_automation_execution.rs) — emit AWS's fixed sentence for AutomationDefinitionNotFoundException.
- Traces: ssm.automation-execute-aws-api-reaches-success.

Cause 5 of 10: SendAutomationSignal StopStep with an unknown StepExecutionId carries the wrong AutomationStepNotFoundException message.
- AWS: Smithy SendAutomationSignal declares AutomationStepNotFoundException (client, httpError 400). Recorded behavior for SignalType=StopStep with a Payload StepExecutionId that exists in no step of the live execution: HTTP 400, code AutomationStepNotFoundException, message "The step you are trying to stop doesn't exist." — entirely fixed text, no per-run value.
- Emulator: right code, wrong message — "The specified step name and execution ID don't exist. Verify the information and try again." (the shape's documentation sentence, not the StopStep refusal sentence).
- Why: the two sentences distinguish "the step id you asked me to stop is not in this execution" from the generic step-not-found condition; tooling that matches AWS's StopStep text to decide whether to re-resolve step ids and retry takes the wrong branch.
- Fix: SendAutomationSignal's StopStep step-resolution path (services/service-com-amazonaws-ssm-amazonssm/src/ops/send_automation_signal.rs) — render AWS's StopStep not-found sentence instead of the exception's doc string.
- Traces: ssm.send-automation-signal-nonexistent-step-rejected.
- Related open issues: 20261114T164648Z-ssm-2-net-new-causes-sendautomationsignal-stopstep-precondition.md covers the MISSING-StepName precondition message on the wire corpus and 20261105T123903Z-ssm-sendautomationsignal-stub.md the stub; neither covers this unknown-StepExecutionId message.

Cause 6 of 10: GetCommandInvocation for an instance with no invocation under a real command is refused InvalidInstanceId instead of InvocationDoesNotExist.
- AWS: Smithy GetCommandInvocation declares InvalidCommandId, InvalidInstanceId, InvalidPluginName and InvocationDoesNotExist (all client, httpError 400). For an existing CommandId whose target set produced no invocation for the requested, well-formed InstanceId (matching InstanceId's @pattern ^(^i-(\w{8}|\w{17})$)|(^mi-\w{17}$)$), AWS answers code InvocationDoesNotExist with the message "Invocation not found for <CommandId>, <InstanceId>" — fixed text "Invocation not found for " and ", ", with the CommandId and InstanceId taken from the request. InvalidInstanceId is AWS's refusal for an id that is not a managed node at all.
- Emulator: answers InvalidInstanceId for this case, i.e. it classifies "this command has no invocation for that instance" as "that instance id is invalid".
- Why: callers poll GetCommandInvocation to learn whether a target actually received a command; InvalidInstanceId tells them the instance is unusable (a fatal, non-retryable condition) rather than that the tag target simply matched nothing, so fan-out logic reports bogus instance errors instead of a zero-target command.
- Fix: GetCommandInvocation's lookup ordering (services/service-com-amazonaws-ssm-amazonssm/src/ops/get_command_invocation.rs) — validate the instance id shape, then resolve the (CommandId, InstanceId) invocation and raise InvocationDoesNotExist with AWS's message when the command exists but holds no such invocation.
- Traces: ssm.send-command-zero-target-invocations.

Cause 7 of 10: GetCommandInvocation returns an empty StandardErrorContent for a script that wrote to stderr and exited non-zero.
- AWS: Smithy GetCommandInvocation output carries StandardOutputContent (@length 0..24000) and StandardErrorContent (@length 0..8000) alongside ResponseCode. Recorded behavior: for an AWS-RunShellScript invocation whose script writes to stderr and exits with a non-zero code, the agent ships the captured stderr back and StandardErrorContent holds the text the script emitted on fd 2 (truncated to the modeled length).
- Emulator: reports the invocation but StandardErrorContent is '' — the script's stderr is never captured or returned.
- Why: stderr is the whole diagnostic payload of a failed Run Command. Operators and automation that read StandardErrorContent to explain a Failed invocation get nothing, so a failure is indistinguishable from a silent one and no failure cause is ever surfaced.
- Fix: the Run Command execution path and GetCommandInvocation's response projection — capture the guest process's stderr stream separately from stdout and populate StandardErrorContent (and keep ResponseCode) in services/service-com-amazonaws-ssm-amazonssm/src/ops/get_command_invocation.rs plus the invocation state that feeds it.
- Traces: ssm.run-command-nonzero-exit-surfaces-as-failed-with-the-exact-response-code.

Cause 8 of 10: TerminateSession on an already-Terminated session is served 200 instead of being refused.
- AWS: Smithy TerminateSession input SessionId (required, @length 1..96), output SessionId; the only modeled error is InternalServerError, so this refusal is recorded behavior only: a second TerminateSession on a session already in Terminated state is refused with error code ValidationException and the message "Session is not in a valid state" (entirely fixed text, no per-run value), and DescribeSessions(State=History) continues to hold exactly one Terminated record for that session.
- Emulator: the second call succeeds, returning HTTP 200 with the SessionId echoed, so the double-terminate is an idempotent no-op.
- Why: TerminateSession is not idempotent on AWS; a caller uses the refusal to learn the session was already closed (by a peer, or by the idle timeout) rather than closing it itself. An emulator that always succeeds hides lost races and lets a negative test that asserts AWS's refusal pass here while failing against AWS.
- Fix: TerminateSession's state check (services/service-com-amazonaws-ssm-amazonssm/src/ops/terminate_session.rs) — refuse a session whose state is already Terminated with 400 ValidationException and AWS's fixed message, and leave the single History record untouched.
- Traces: ssm.terminate-session-already-terminated-noop.

Cause 9 of 10: an aws:branch step with no matching Choice and no Default does not fall through to the next declared step.
- AWS: recorded behavior for Automation (schemaVersion 0.3) aws:branch: when no Choice matches and the step declares no Default, the branch step completes Success and the execution falls through to the NEXT DECLARED step only, which runs to Success and performs its side effect; the step declared after that remains Pending. GetAutomationExecution's StepExecutions therefore reads [(branch, Success), (next declared step, Success), (later step, Pending)].
- Emulator: the branch step reports Success but BOTH following steps stay Pending — the fall-through never executes, so the next declared step's side effect (here, a PutParameter the step performs) never happens even though the execution ends Success.
- Why: an execution reports overall Success while a step AWS would have run never ran, so a runbook silently skips work and a caller that reads step statuses or the step's side effect to confirm completion is misled in the most dangerous direction.
- Fix: the Automation step scheduler/branch evaluator in the ssm service crate — after an aws:branch with no matching Choice and no Default, continue execution at the next declared step instead of ending the step chain, and record its StepExecution status accordingly.
- Traces: ssm.ssm-automation-branch-takes-one-path-and-the-other-branchs-effect-is-absent.

Cause 10 of 10: aws:executeScript ignores the step's declared Runtime — the managed runtime reports an interpreter version the declared runtime cannot produce.
- AWS: recorded behavior for an aws:executeScript step declaring Runtime python3.11: the script really executes in that managed runtime, so platform.python_version() returned through the step's output begins with "3.11.". The requirement is that the reported version agrees with the step's declared Runtime; the patch component is a per-run value.
- Emulator: the step reports a projected runtime version of '3.14.0.alpha' for Runtime=python3.11 — the declared runtime is not honoured (and a version string of that form cannot come from a python3.11 interpreter), so the script output is synthesized rather than computed.
- Why: aws:executeScript is used to compute values a runbook then acts on. A caller that pins Runtime to get a specific interpreter's behavior (library availability, version-gated code paths) gets answers from something else entirely, and any output derived from the script cannot be trusted.
- Fix: the aws:executeScript action implementation in the ssm Automation runtime — dispatch to the interpreter named by the step's Runtime member and return that interpreter's real output, rather than reporting the host runtime / a synthesized value.
- Traces: ssm.ssm-automation-execute-script-computes-its-output-in-the-managed-runtime.
- Related open issue: 20261105T135904Z-ssm-automation-execution-apis-stop-is-never-applied.md covers Automation stop/refusal wording, not step execution semantics (causes 9 and 10).

## Evidence
Behavioral suite replay of this checkout: 246/585 checks passed (42.1%); 211 mismatched, 128 errored, bootstrap_signal=false (the emulator served every call below). Service heading 'ssm': 102/282 passed, 180 failed. Every cause below is grounded in a failing check that the report marked neither 'ALREADY FILED' nor 'OWNED by'.

Cause 1 — ssm.delete-maintenance-window-then-get-fails [mismatch]: "AssertionError: expected DoesNotExistException on UpdateMaintenanceWindow after delete, got 'NotImplemented'" (CreateMaintenanceWindow -> DeleteMaintenanceWindow -> UpdateMaintenanceWindow on the dead WindowId).

Cause 2 — ssm.register-deregister-maintenance-window-target [error]: "botocore.errorfactory.DoesNotExistException: An error occurred (DoesNotExistException) when calling the DeregisterTargetFromMaintenanceWindow operation: Maintenance window mw-a1eb74f8699636bbf has no target d2aa9714-5a38-499f-b89f-7970b93d8296" raised by the SECOND deregister of the same target; the window id and target uuid are per-run values, the divergence is that the call is refused at all.

Cause 3 — ssm.get-ops-summary-invalid-aggregator-rejected [mismatch]: code ValidationException is correct; message got "2 validation errors detected: Value 'not-a-real-aggregator-type' at 'aggregators.1.member.aggregatorType' failed to satisfy constraint: Member must have length less than or equal to 20; Value 'not-a-real-aggregator-type' at 'aggregators.1.member.aggregatorType' failed to satisfy constraint: Member must satisfy regular expression pattern: ^(range|count|sum)$" where AWS's regex clause ends "...pattern: ^(range|count|sum)". Only the trailing '$' diverges; the rejected value and member path are request-derived.

Cause 4 — ssm.automation-execute-aws-api-reaches-success [mismatch]: "starting an unknown automation must be refused, got ('AutomationDefinitionNotFoundException', 'Document sim2real-behav-no-such-automation does not exist.')"; expected ('AutomationDefinitionNotFoundException', 'Invalid input for field document name, expected valid document in account.'). The document name is the request's value; AWS's sentence carries none.

Cause 5 — ssm.send-automation-signal-nonexistent-step-rejected [mismatch]: got ('AutomationStepNotFoundException', "The specified step name and execution ID don't exist. Verify the information and try again."); expected ('AutomationStepNotFoundException', "The step you are trying to stop doesn't exist.") for SignalType=StopStep with Payload StepExecutionId set to a uuid that is in no step of the live execution.

Cause 6 — ssm.send-command-zero-target-invocations [mismatch]: after SendCommand on a tag target matching nothing (TargetCount 0, no invocations), GetCommandInvocation(CommandId=<real id>, InstanceId='i-0123456789abcdef0') returned code 'InvalidInstanceId'; expected 'InvocationDoesNotExist' with message "Invocation not found for <CommandId>, i-0123456789abcdef0".

Cause 7 — ssm.run-command-nonzero-exit-surfaces-as-failed-with-the-exact-response-code [mismatch]: "the agent must ship the script's stderr back; expected 'sim2real-behav-stderr-marker' inside StandardErrorContent, got ''" — the marker string is the probe's own value; the requirement is that whatever the script wrote to stderr appears in StandardErrorContent.

Cause 8 — ssm.terminate-session-already-terminated-noop [mismatch]: the second TerminateSession "succeeded and returned {'SessionId': 'emulator-...', 'ResponseMetadata': {'HTTPStatusCode': 200, ...}}"; expected ClientError ValidationException with Message 'Session is not in a valid state'. (The session id and timestamps in that response are per-run values.)

Cause 9 — ssm.ssm-automation-branch-takes-one-path-and-the-other-branchs-effect-is-absent [mismatch]: with no matching Choice and no Default the execution ended Success but StepExecutions read [('branch','Success'), ('LEFT','Pending'), ('RIGHT','Pending')]; expected [('branch','Success'), ('LEFT','Success'), ('RIGHT','Pending')] and the fall-through step's PutParameter side effect present.

Cause 10 — ssm.ssm-automation-execute-script-computes-its-output-in-the-managed-runtime [mismatch]: "the step declares Runtime=python3.11, so platform.python_version() inside the managed runtime must begin with '3.11.'; got '3.14.0.alpha'".

## Steps to Reproduce
Boot the emulator from this checkout and replay the behavioral suite (simparity-behavioral-aws-v1), or run the named checks individually:
1. Cause 1 — ssm.delete-maintenance-window-then-get-fails: CreateMaintenanceWindow(Schedule='rate(1 day)', Duration=2, Cutoff=1), DeleteMaintenanceWindow(WindowId), then UpdateMaintenanceWindow(WindowId) — expect 400 DoesNotExistException, observe 'NotImplemented'.
2. Cause 2 — ssm.register-deregister-maintenance-window-target: create a window, RegisterTargetWithMaintenanceWindow(ResourceType='INSTANCE', Targets=[{Key:'tag:Name',Values:['probe']}]), DeregisterTargetFromMaintenanceWindow once (succeeds), then call it AGAIN with the same ids — expect 200 echoing the ids, observe DoesNotExistException.
3. Cause 3 — ssm.get-ops-summary-invalid-aggregator-rejected: GetOpsSummary(Aggregators=[{AggregatorType:'not-a-real-aggregator-type', TypeName:'AWS:OpsItem', AttributeName:'Status'}]) and compare the ValidationException message clauses after the '2 validation errors detected: ' prefix.
4. Cause 4 — ssm.automation-execute-aws-api-reaches-success: StartAutomationExecution(DocumentName=<a name no document has>) and read Error.Message.
5. Cause 5 — ssm.send-automation-signal-nonexistent-step-rejected: start an Interactive automation of an aws:sleep document, SendAutomationSignal StartStep to put a step InProgress, then SendAutomationSignal(SignalType='StopStep', Payload={'StepExecutionId': [<random uuid>]}) and read Error.Message.
6. Cause 6 — ssm.send-command-zero-target-invocations: SendCommand(DocumentName='AWS-RunShellScript', Targets=[{Key:'tag:<unmatched>',Values:['<unmatched>']}]), then GetCommandInvocation(CommandId=<that id>, InstanceId='i-0123456789abcdef0') and read Error.Code/Message.
7. Cause 7 — ssm.run-command-nonzero-exit-...: launch an SSM-managed AL2023 instance, SendCommand AWS-RunShellScript with a script that echoes a marker to stderr and exits 42, then GetCommandInvocation and inspect StandardErrorContent.
8. Cause 8 — ssm.terminate-session-already-terminated-noop: StartSession on a connected target, TerminateSession, poll DescribeSessions(State='History') until Terminated, then TerminateSession again — expect ValidationException 'Session is not in a valid state', observe 200.
9. Cause 9 — ssm.ssm-automation-branch-...: run an Automation document whose aws:branch step declares Choices that the supplied input matches none of and declares no Default, with two further steps declared after it; inspect StepExecutions and the next declared step's parameter side effect.
10. Cause 10 — ssm.ssm-automation-execute-script-...: run an Automation document with an aws:executeScript step declaring Runtime 'python3.11' whose handler returns platform.python_version(), and read the step output.
