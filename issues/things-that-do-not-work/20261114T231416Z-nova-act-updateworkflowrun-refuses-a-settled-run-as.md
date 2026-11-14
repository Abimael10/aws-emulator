Issue-Source: trace-sweep-divergence
Issue-Trace: nova-act-workflow-run-lifecycle
Issue-Step: task#8
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: nova-act-workflow-run-lifecycle
Issue-Shape-Id: com.amazonaws.novaact#AmazonNovaAgentsDataPlane
Issue-Key: nova-act|UpdateWorkflowRun|4xx->4xx|$.message
Issue-Key: nova-act|UpdateWorkflowRun|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: nova-act|UpdateWorkflowRun|4xx->4xx|status
Issue-Commit: fc41187e4d936c85570d866c4bbccc081bf40c6e

# nova-act UpdateWorkflowRun refuses a settled run as ConflictException 409 instead of ValidationException 400

> _Note: this issue was identified against the code base at commit `fc41187e4d936c85570d866c4bbccc081bf40c6e`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateWorkflowRun

## Description
Cause 1 of 1: a status write on a workflow run that has already left RUNNING is refused with the WRONG error class — the emulator answers HTTP 409 / ConflictException where AWS answers HTTP 400 / ValidationException, and consequently with a different body shape and a different message. Fixture nova-act-workflow-run-lifecycle, step task#8.

AWS: recorded behavior only. The Smithy model (services/service-com-amazonaws-novaact-amazonnovaagentsdataplane/smithy-model/com-amazonaws-novaact-amazonnovaagentsdataplane.json; smithy_query operation=UpdateWorkflowRun) binds PUT /workflow-definitions/{workflowDefinitionName}/workflow-runs/{workflowRunId}, marks the operation @idempotent, documents UpdateWorkflowRunRequest.status only as "The new status to set for the workflow run" over WorkflowRunStatus (RUNNING | SUCCEEDED | FAILED | TIMED_OUT | DELETING), and models BOTH ValidationException (httpError 400, required members message + reason, optional fieldList) and ConflictException (httpError 409, required members message + resourceId + resourceType). The model does not say which of the two a settled run's status write draws, so the recording decides it: for a run already in SUCCEEDED, asked to move to FAILED, the recorded response is HTTP 400 with x-amzn-errortype "ValidationException:http://internal.amazon.com/coral/com.amazonaws.novaact/" and message exactly "WorkflowRun in unexpected state". That message is ENTIRELY fixed text — it interpolates no run id, no definition name, neither the current nor the requested status, and no timestamp or request id — so none of it is environment- or run-specific, and the whole sentence is the requirement. Hypothesis: AWS treats a status this run's current status does not admit as a bad input value for the status member (the same class and the same sentence shape UpdateAct uses for an act, per the sibling open issue below) rather than as a resource-state conflict; what is REQUIRED by the recording regardless of the rationale is the 400 / ValidationException classification and that message text.

Emulator: answers HTTP 409 with x-amzn-errortype "ConflictException" and message "workflow run <runId> is SUCCEEDED and cannot be moved to FAILED" (the run id, the current status and the requested status are interpolated per request — only the generic template is cited here, never one run's values). The branch is reachable only for a non-running run, so every later status write on a finished run — including re-sending the status it already holds, which the operation's @idempotent contract makes a plausible client retry — takes it. Because the chosen error shape is ConflictException, the body also carries resourceId and resourceType ("workflow-run") and carries NO `reason` member, whereas AWS's ValidationException body for this case carries reason and no resourceId/resourceType. So the divergence is three-fold: HTTP status (400 -> 409), error code (ValidationException -> ConflictException), and message text. Note the service already holds the correct sentence as a private constant, RUN_IN_UNEXPECTED_STATE = "Workflow run in unexpected state", but renders it only on a different branch (requesting DELETING for a still-running run) and with "Workflow run" where the recording words it "WorkflowRun"; the settled-run branch does not use it at all.

Why: a client relying on AWS's behavior mis-handles the refusal at every layer. Generated SDK clients dispatch on the error code, so code written to `catch ValidationException` around UpdateWorkflowRun never fires and the failure escapes as an unmodeled/unexpected ConflictException; code that reads ValidationException.reason gets a body with no reason member at all. Retry and alarm policies that treat 409 as a benign lost race (back off and re-read) but 400 as a permanent caller error will loop or mis-route. Conformance tests, CLI golden output and log rules keyed on AWS's text see a sentence AWS never emits. The @idempotent retry case is the worst of it: a client re-sending the status the run already holds gets a hard conflict rather than AWS's validation refusal.

Fix: the settled-run guard in the UpdateWorkflowRun handler — the classification of a status write on a run that has left RUNNING. It must raise the operation's ValidationException (400) with reason InvalidStatus and AWS's fixed sentence, the same class the DELETING branch immediately above it already raises, instead of constructing a ConflictException with an interpolated run/status sentence. The fixed sentence belongs with the service's other state-transition refusal messages rather than only as this operation's private constant, since UpdateAct states the same clause for an act. No change to the not-found resolution or to the successful transition path is implied.

Traces: fixture nova-act-workflow-run-lifecycle step task#8 (the cluster's only member).

Related open issue, which does NOT cover this cause: 20261106T074031Z-nova-act-updateact-terminal-state-refusal-uses-a.md concerns UpdateAct's terminal-state refusal, where the emulator already answers the correct 400/ValidationException/InvalidStatus and only the message text diverges. That issue is about a message; this one is about the error CLASS and HTTP status of a different operation, and names a divergence that issue's replay key does not exercise.

## Evidence
Wire replay of simparity-wire-aws-v1, fixture nova-act-workflow-run-lifecycle, step task#8, verdict mismatch (divergence_key service=nova-act|UpdateWorkflowRun|status=400->409|keys=$.message,$headers.x-amzn-errortype,status, affected_count 1).

HTTP status:
  Expected: 400
  Actual:   409
Error code (x-amzn-errortype header):
  Expected: "ValidationException:http://internal.amazon.com/coral/com.amazonaws.novaact/"
  Actual:   "ConflictException"
Error message:
  Expected: "WorkflowRun in unexpected state"
  Actual:   "workflow run dc4ffd9b-f952-4a25-9a97-5bac08d7416a is SUCCEEDED and cannot be moved to FAILED"

Replay detail verbatim:
  status: expected 400, got 409
  $.message: value_mismatch (expected 'WorkflowRun in unexpected state', got 'workflow run dc4ffd9b-f952-4a25-9a97-5bac08d7416a is SUCCEEDED and cannot be moved to FAILED')
  $headers.x-amzn-errortype: value_mismatch (expected 'ValidationException:http://internal.amazon.com/coral/com.amazonaws.novaact/', got 'ConflictException')

Fixed vs variable parts. Expected message: 100% fixed text, "WorkflowRun in unexpected state" — no variable part, nothing taken from the request or from state. Actual message: template "workflow run {workflowRunId} is {currentStatus} and cannot be moved to {requestedStatus}"; the uuid dc4ffd9b-f952-4a25-9a97-5bac08d7416a in the recording is a per-run generated id and is NOT part of the requirement — the divergence is the template and the error class, not that one id. The errortype and status differences contain no per-run value at all, so this is not a clock/request-id artifact.

Body shape consequence (from the model): AWS's ValidationException requires message + reason (ValidationExceptionReason) and may carry fieldList; the emulator's ConflictException requires message + resourceId + resourceType. A client reading `reason` finds no such member in the emulator's body.

Code citations (source of the diverging classification):
  services/service-com-amazonaws-novaact-amazonnovaagentsdataplane/src/ops/update_workflow_run.rs lines 245-257 — `if !current.is_running()` returns ConflictException { message: format!("workflow run {} is {} and cannot be moved to {}", ...), resource_id, resource_type: RUN_RESOURCE_TYPE }.
  services/service-com-amazonaws-novaact-amazonnovaagentsdataplane/src/ops/update_workflow_run.rs line 91 — const RUN_IN_UNEXPECTED_STATE: &str = "Workflow run in unexpected state", the fixed sentence the service already holds but renders only on the DELETING branch (lines 230-237), and worded "Workflow run" rather than the recorded "WorkflowRun".
  services/service-com-amazonaws-novaact-amazonnovaagentsdataplane/src/ops/update_workflow_run.rs lines 9-17 — the module header documents the ConflictException-for-a-settled-run rule as intended behavior, so the defect is the documented design of this guard and not a stray branch.

Control: the same service's UpdateAct ValidationException matched on BOTH status and x-amzn-errortype in this same corpus run (divergence there was $.message only), so the error serializer renders the coral-suffixed errortype correctly; the only thing wrong here is which error the handler chooses.

## Steps to Reproduce
1. Clone this checkout, boot the emulator from it and replay the wire corpus simparity-wire-aws-v1.
2. Replay fixture nova-act-workflow-run-lifecycle and stop at step task#8 (UpdateWorkflowRun).
3. Equivalently, by hand against a nova-act client pinned to the fixture's region: CreateWorkflowDefinition with a name matching ^[a-zA-Z0-9_-]{1,40}$, then CreateWorkflowRun under it to get a workflowRunId.
4. Call UpdateWorkflowRun on that definition/run with status SUCCEEDED to drive the run out of RUNNING. Expect 200 with an empty body.
5. Call UpdateWorkflowRun again on the SAME workflowDefinitionName / workflowRunId with a different status, e.g. FAILED.
6. Observe the emulator answer HTTP 409 with x-amzn-errortype "ConflictException", a body carrying resourceId/resourceType and no reason member, and message "workflow run <runId> is SUCCEEDED and cannot be moved to FAILED".
7. The step passes only when the response is HTTP 400 with x-amzn-errortype "ValidationException:http://internal.amazon.com/coral/com.amazonaws.novaact/", reason InvalidStatus, and the fixed message "WorkflowRun in unexpected state".
8. Re-run step 5 with the status the run already holds (SUCCEEDED) to confirm the @idempotent retry path takes the same corrected 400/ValidationException refusal rather than a 409.
9. Re-check the neighbouring branch is untouched: on a run still in RUNNING, call UpdateWorkflowRun with status DELETING and confirm it still answers 400 / ValidationException / reason InvalidStatus.
