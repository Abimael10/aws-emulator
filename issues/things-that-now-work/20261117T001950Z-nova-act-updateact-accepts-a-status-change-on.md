Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: nova-act.update-act-after-terminal-status-rejected
Issue-Shape-Id: com.amazonaws.novaact#AmazonNovaAgentsDataPlane
Issue-Key: nova-act.update-act-after-terminal-status-rejected|_error_response#11|AssertionError|raise AssertionError("expected the call to be rejected, but it succeeded")
Issue-Commit: f9d8a0ff2d3b1f9755e8264ff6b218fda6df919c

# nova-act UpdateAct accepts a status change on an already-terminal act instead of refusing it with ValidationException "Act in unexpected state"

> _Note: this issue was identified against the code base at commit `f9d8a0ff2d3b1f9755e8264ff6b218fda6df919c`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
UpdateAct

## Description
Cause 1 of 1: UpdateAct on an act that has already FINISHED succeeds and overwrites the recorded outcome, where AWS refuses the call. Failing check: nova-act.update-act-after-terminal-status-rejected (group "overly-permissive").

AWS: recorded behavior only for the transition rule. The Smithy model (services/service-com-amazonaws-novaact-amazonnovaagentsdataplane/smithy-model/com-amazonaws-novaact-amazonnovaagentsdataplane.json; smithy_query operation=UpdateAct) binds PUT /workflow-definitions/{workflowDefinitionName}/workflow-runs/{workflowRunId}/sessions/{sessionId}/acts/{actId}, marks the operation @idempotent, documents UpdateActRequest.status only as "The new status to set for the act." over ActStatus (RUNNING | PENDING_CLIENT_ACTION | PENDING_HUMAN_ACTION | SUCCEEDED | FAILED | TIMED_OUT), and models ValidationException (httpError 400, required members message + reason) alongside ConflictException (409) and ResourceNotFoundException (404). The model states no transition rule, so the recording decides it: for an act already in SUCCEEDED, asked to move to FAILED (a validly-shaped in-enum status, so the refusal can only be attributed to the act's terminality), AWS answers HTTP 400 with error code ValidationException and message exactly "Act in unexpected state", and the act is left SUCCEEDED — a subsequent ListActs still reports status SUCCEEDED for it. That message is ENTIRELY fixed text: it interpolates no act id, session id, run id, current or requested status, no timestamp and no request id, so the whole sentence is the requirement and nothing in it is environment- or run-specific. Hypothesis: AWS applies two independent checks to UpdateAct.status — a permitted-value check on what a caller may submit (whose refusal names the admitted set, see the already-fixed issue below) and a STATE check that an act which has finished admits no further status change at all (this sentence, the same clause the sibling UpdateWorkflowRun recording words "WorkflowRun in unexpected state"). What is REQUIRED by this check regardless of the rationale is the 400 / ValidationException refusal, that message text, and that the stored act keeps the status it already held. Positive control inside the same check: an act still in a non-finished status CAN be marked FAILED, with the identical call shape, and ListActs then reports it as FAILED — so the requirement is not "UpdateAct is closed", only that a finished act is frozen.

Emulator: the call SUCCEEDS. The replay fails at the point the check asserts a rejection — AssertionError: "expected the call to be rejected, but it succeeded" (check_nova_act_update_act_after_terminal_status_rejected.py line 125, raised from _error_response for the second UpdateAct) — so no status, no error code and no message are emitted at all where AWS emits 400 / ValidationException / "Act in unexpected state". The act is then silently moved out of its recorded outcome (SUCCEEDED -> FAILED) and the request's `error` member is attached to an act that had already succeeded. The guard is the transition predicate: an already-terminal act is treated as admitting any "reported outcome", so the only terminal-act refusal left is one that names a status outside {FAILED, SUCCEEDED, the held status} — a SUCCEEDED -> FAILED write never reaches it.

Why: a client relying on AWS's behavior loses the integrity of a finished act. Code that reports an outcome for an act (a retried or duplicated worker, a late timeout reporter, a crash-recovery path re-reporting work) gets its write ACCEPTED here and the authoritative outcome of an act is rewritten, where against AWS the stale report is rejected and the first outcome stands; the act's error information and endedAt are rewritten with it. Nothing surfaces the refusal: code written to `catch ValidationException` around UpdateAct never fires, so a conformance test, a CLI golden-output test or a log/alarm rule keyed on AWS's "Act in unexpected state" sentence sees a 200 with an empty body. Because the emulator is MORE permissive than AWS, an application developed against it passes locally and fails in production on the exact path that matters — reporting an outcome twice.

Fix: the terminal-state guard of the UpdateAct handler and the transition predicate it consults — the classification of a status write against an act whose current status is finished. A finished act must refuse a move to a DIFFERENT status with the operation's ValidationException (400, reason InvalidStatus) carrying AWS's fixed sentence, instead of admitting the finishing statuses; and the refusal must leave the stored act untouched (status, endedAt and error unchanged), so the effect is absent as well as the response correct. The permitted-value refusal on the same member (the sentence that names the admitted set) is a separate branch and is not what this cause asks to change. Re-sending the status the act already holds is not exercised by this check, so leave that @idempotent no-op path as it is.

Context, which does NOT cover this cause: 20261106T074031Z-nova-act-updateact-terminal-state-refusal-uses-a.md is FIXED and concerned the MESSAGE of UpdateAct's terminal refusal (it required "Status must be one of [FAILED, SUCCEEDED]" in place of "Act in unexpected state"). That fix also widened which statuses a finished act admits, which is why the SUCCEEDED -> FAILED write is now accepted with no refusal at all; both refusals must coexist (the permitted-value sentence for a status a caller may not submit, the "Act in unexpected state" sentence for an act that has finished). Open issue 20261105T122750Z-nova-act-listacts-reports-a-constant-act-status.md concerns ListActs reporting a constant status and does not exercise this guard; open issue 20261114T231416Z-nova-act-updateworkflowrun-refuses-a-settled-run-as.md is the same clause for a workflow RUN, a different operation.

Traces: behavioral check nova-act.update-act-after-terminal-status-rejected (the cluster's only member).

## Evidence
Behavioral suite simparity-behavioral-aws-v1, check nova-act.update-act-after-terminal-status-rejected, verdict mismatch (not marked ALREADY FILED and not OWNED by any open issue).

Replay detail verbatim:
  File "check_nova_act_update_act_after_terminal_status_rejected.py", line 53, in run
    error = _error_response(
  File "check_nova_act_update_act_after_terminal_status_rejected.py", line 125, in _error_response
    raise AssertionError("expected the call to be rejected, but it succeeded")
  AssertionError: expected the call to be rejected, but it succeeded

Expected vs actual for the second UpdateAct (act already SUCCEEDED, request status=FAILED with an `error` member):
  HTTP status:   expected 400, actual 200 (success)
  Error code:    expected "ValidationException", actual none (no error emitted)
  Error message: expected "Act in unexpected state", actual none
  Effect:        expected absent (ListActs still reports the act as SUCCEEDED), actual the act is moved to FAILED and the request's error is attached

Fixed vs variable parts of the expected message: "Act in unexpected state" is 100% fixed text — no act id, session id, run id, current or requested status, timestamp or request id is interpolated, so this is not a per-run-value difference and the whole sentence is the requirement. The expected HTTP status and error code carry no per-run value either.

Positive control from the same check (passes against AWS, and is what distinguishes this from "UpdateAct must reject FAILED"): a freshly created act in the same session, same call shape, status=FAILED with the same error member, is accepted and ListActs then reports it FAILED. So the divergence is strictly the missing terminality guard.

Code citations (source of the over-permissive acceptance):
  services/service-com-amazonaws-novaact-amazonnovaagentsdataplane/src/state.rs lines 396-410 — ActStatus::admits: `requested == self || !self.is_terminal() || requested.is_reported_outcome()`, i.e. a terminal act admits SUCCEEDED and FAILED (is_reported_outcome, lines 388-394), so SUCCEEDED -> FAILED is admitted.
  services/service-com-amazonaws-novaact-amazonnovaagentsdataplane/src/ops/update_act.rs lines 224-242 — the only state guard, `if !current.admits(requested)`, returning ValidationException via crate::state::status_transition::invalid_status(&admitted); it is not reached for a finished act moved to a reported outcome.
  services/service-com-amazonaws-novaact-amazonnovaagentsdataplane/src/ops/update_act.rs line 244 — `act.move_to(requested, now)` then records the request's error (lines 248-250), which is how the recorded outcome is overwritten.
  services/service-com-amazonaws-novaact-amazonnovaagentsdataplane/src/ops/update_act.rs lines 10-16 — the module header documents "a finished act admits only the outcomes a caller reports for one (SUCCEEDED, FAILED) and the status it already holds" as intended behavior, so the defect is this guard's documented design and not a stray branch.

## Steps to Reproduce
1. Clone this checkout, boot the emulator from it and replay the behavioral suite simparity-behavioral-aws-v1.
2. Replay check nova-act.update-act-after-terminal-status-rejected.
3. Equivalently, by hand against a nova-act client pinned to us-east-1 (the region the check uses): CreateWorkflowDefinition with a name matching ^[a-zA-Z0-9_-]{1,40}$; CreateWorkflowRun under it with a modelId from ListModels; CreateSession under that run; CreateAct in that session.
4. Call UpdateAct on that definition/run/session/act with status=SUCCEEDED. Expect 200 with an empty body.
5. Call UpdateAct again on the SAME workflowDefinitionName / workflowRunId / sessionId / actId with status=FAILED and an `error` member.
6. Observe the emulator answer HTTP 200 with an empty body (no error at all), and a following ListActs report that act with status FAILED.
7. The check passes only when step 5 answers HTTP 400 with error code ValidationException and message exactly "Act in unexpected state", and ListActs still reports that act as SUCCEEDED (the effect is absent).
8. Confirm the positive control still holds: CreateAct a fresh act in the same session, call UpdateAct with status=FAILED and the same error member, and check it is accepted and ListActs reports it FAILED.
9. Confirm the @idempotent no-op path is untouched: re-send status=SUCCEEDED for the act that already holds SUCCEEDED and check it still answers 200 with an empty body.
