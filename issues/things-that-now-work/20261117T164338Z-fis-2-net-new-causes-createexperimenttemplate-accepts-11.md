Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: fis.create-experiment-template-parallel-action-limit-excludes-sequenced-action fis.update-safety-lever-state-reason-only-change-rejected
Issue-Shape-Id: com.amazonaws.fis#FaultInjectionSimulator
Issue-Key: fis.create-experiment-template-parallel-action-limit-excludes-sequenced-action|expect_error#6|AssertionError|raise AssertionError(f"expected the call to be refused, but it succeeded: {result!r}")
Issue-Key: fis.update-safety-lever-state-reason-only-change-rejected|_error#8|AssertionError|raise AssertionError(f"expected the call to be rejected, but it succeeded and returned {result!r}")
Issue-Commit: 077021ed17173fe4a565f7526f07b6650a74c582

# fis: 2 net-new causes — CreateExperimentTemplate accepts 11 parallel actions instead of refusing with ServiceQuotaExceededException, and UpdateSafetyLeverState accepts a reason-only change instead of refusing with ConflictException

> _Note: this issue was identified against the code base at commit `077021ed17173fe4a565f7526f07b6650a74c582`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateExperimentTemplate

## Description
Two independent net-new root causes in the FIS emulator, both missing refusals on modeled error paths. Each is numbered below with its own failing check id.

Cause 1 of 2: CreateExperimentTemplate does not enforce the parallel-action quota (10 actions that are not sequenced).
  AWS: Smithy (com-amazonaws-fis-faultinjectionsimulator): CreateExperimentTemplate declares error ServiceQuotaExceededException (client error, httpError 402) alongside ValidationException and ConflictException; `actions` is a required CreateExperimentTemplateActionInputMap whose action entries may carry `startAfter`. FIS caps the number of actions that run in parallel at 10; an action that declares `startAfter` is sequenced and does not count toward that cap. Recorded behavior: a CreateExperimentTemplate carrying 11 actions with no `startAfter` anywhere is refused with error code `ServiceQuotaExceededException` and message `The number of parallel actions cannot exceed 10` (the whole sentence is fixed text — the only variable part is the limit, 10, which is the account's parallel-action quota), and creates nothing (a ListExperimentTemplates sweep by the request's tag finds no template). Exactly 10 unsequenced actions is accepted, and 11 actions where one declares `startAfter` on another is also accepted.
  Emulator: the 11-unsequenced-action CreateExperimentTemplate returns HTTP 200 with a fully created experimentTemplate (id EXT…, all 11 action names present) instead of refusing. No quota check exists on the action map, and consequently no distinction is drawn between parallel and sequenced actions.
  Why: a client that probes or relies on the parallel-action quota sees a template AWS would never have created. Templates built against the emulator pass validation locally and then fail at CreateExperimentTemplate in a real account; negative-path tests that assert ServiceQuotaExceededException never see the error, and the refused create's "creates nothing" guarantee is untestable because the create succeeded.
  Fix: CreateExperimentTemplate's request validation — count the actions whose `startAfter` is absent or empty and refuse with ServiceQuotaExceededException (HTTP 402) and the message above when that count exceeds 10, before any state is written. UpdateExperimentTemplate should apply the same rule.
  Traces: check fis.create-experiment-template-parallel-action-limit-excludes-sequenced-action.

Cause 2 of 2: UpdateSafetyLeverState accepts a call that repeats the lever's current status and only changes the reason.
  AWS: Smithy: UpdateSafetyLeverState (PATCH /safetyLevers/{id}/state) declares error ConflictException (client error, httpError 409); its input `state` (UpdateSafetyLeverStateInput) carries the target status and a reason. The lever's reason is not independently writable: a request whose status equals the lever's current status is a conflict. Recorded behavior: with the account-global lever `default` engaged, UpdateSafetyLeverState with state.status=`engaged` and a different reason is refused with error code `ConflictException` and message `Cannot update Safety Lever reason without a status change` (the entire sentence is fixed text; no request or state values are interpolated), and the stored reason is left exactly as the prior accepted status change set it. A call that genuinely changes the status applies its own reason normally.
  Emulator: the reason-only call returns HTTP 200 with safetyLever.state = {status: engaged, reason: <the new reason from the request>} — the update is applied and the stored reason is overwritten, so the subsequent GetSafetyLever shows the reason the refused call carried rather than the reason of the last real status change.
  Why: a client reading the lever's reason to learn why the lever was engaged gets a reason that AWS would never have stored, and safety-lever automation that depends on the conflict (to detect that another actor already engaged the lever, or to avoid clobbering an audit reason) silently proceeds. Error handling for ConflictException on this operation is never exercised.
  Fix: the UpdateSafetyLeverState handler's validation/state transition (services/service-com-amazonaws-fis-faultinjectionsimulator/src/ops — update_safety_lever_state) — compare the requested status to the lever's stored status and, when equal, return ConflictException (HTTP 409) with the message above and leave the stored state, including its reason, untouched.
  Traces: check fis.update-safety-lever-state-reason-only-change-rejected.

Neither cause is covered by the open FIS issues (target resolution, experiment record timestamps/wording, action catalog rows, the invalid stop-condition source / experimentReportConfiguration pair in 20261117T121318Z-…, or the engaged-lever StartExperiment issue 20261117T135333Z-…): those name different operations and different refusals.

## Evidence
Cause 1 — check fis.create-experiment-template-parallel-action-limit-excludes-sequenced-action
Expected (AWS): ClientError ServiceQuotaExceededException, message "The number of parallel actions cannot exceed 10", HTTP 402, and no template created.
Actual (emulator): HTTP 200 and a created template —
  AssertionError: expected the call to be refused, but it succeeded: {'id': 'EXT000000000001', 'arn': 'arn:aws:fis:…:experiment-template/EXT000000000001', 'description': 'FIS parallel action quota', 'actions': {'wait0'…'wait10' (11 unsequenced aws:fis:wait actions)}, 'stopConditions': [{'source': 'none'}], …}
(The 10-action positive control and the 11-with-one-sequenced control are asserted later in the same check and were never reached.)

Cause 2 — check fis.update-safety-lever-state-reason-only-change-rejected
Expected (AWS): ClientError ConflictException, message "Cannot update Safety Lever reason without a status change", HTTP 409, stored reason unchanged.
Actual (emulator): HTTP 200 with the new reason applied —
  AssertionError: expected the call to be rejected, but it succeeded and returned {'ResponseMetadata': {'HTTPStatusCode': 200, …}, 'safetyLever': {'id': 'default', 'arn': 'arn:aws:fis:…:safety-lever/default', 'state': {'status': 'engaged', 'reason': '<the reason carried by the refused request>'}}}

## Steps to Reproduce
Cause 1:
1. Create an IAM role usable as a FIS experiment role.
2. Call fis.CreateExperimentTemplate with clientToken, description, roleArn, stopConditions=[{source: "none"}], targets={}, and actions = 11 entries named wait0..wait10, each {actionId: "aws:fis:wait", parameters: {duration: "PT1M"}}, none carrying startAfter.
3. AWS refuses with ServiceQuotaExceededException / "The number of parallel actions cannot exceed 10" and creates nothing; the emulator returns 200 with the template. Controls: the same call with 10 actions must succeed, and with 11 actions where one declares startAfter on another must also succeed.

Cause 2:
1. Call fis.UpdateSafetyLeverState(id="default", state={status: "disengaged", reason: <baseline>}) and wait for it to settle.
2. Call fis.UpdateSafetyLeverState(id="default", state={status: "engaged", reason: R1}) and wait for status "engaged"; GetSafetyLever shows reason R1.
3. Call fis.UpdateSafetyLeverState(id="default", state={status: "engaged", reason: R2}) with R2 != R1.
4. AWS refuses with ConflictException / "Cannot update Safety Lever reason without a status change" and GetSafetyLever still reports reason R1; the emulator returns 200 and GetSafetyLever reports R2.
