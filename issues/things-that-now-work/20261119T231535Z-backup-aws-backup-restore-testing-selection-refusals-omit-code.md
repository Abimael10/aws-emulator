Issue-Source: trace-sweep-divergence
Issue-Trace: backup-restore-testing-error-paths
Issue-Step: task#4
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-restore-testing-error-paths backup-restore-testing-not-found-errors backup-restore-testing-plan-lifecycle backup-restore-testing-selection-conditions-and-overrides backup-restore-testing-selection-lifecycle backup-restore-testing-selection-lifecycle-wildcard backup-restore-testing-selection-conditions-and-errors
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|CreateRestoreTestingSelection|4xx->4xx|$.Code
Issue-Key: backup|CreateRestoreTestingSelection|4xx->4xx|$.Message
Issue-Key: backup|CreateRestoreTestingSelection|4xx->4xx|$headers.x-amzn-errortype
Issue-Key: backup|GetRestoreTestingSelection|4xx->4xx|$.Code
Issue-Key: backup|GetRestoreTestingSelection|4xx->4xx|$.Message
Issue-Key: backup|GetRestoreTestingSelection|4xx->2xx|$.Code
Issue-Key: backup|GetRestoreTestingSelection|4xx->2xx|$.Message
Issue-Key: backup|GetRestoreTestingSelection|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: backup|GetRestoreTestingSelection|4xx->2xx|status
Issue-Commit: 16df41068e9592847de76789eb93312b3e770273

# AWS Backup restore testing selection refusals omit Code, misclassify the error, and survive deletion

> _Note: this issue was identified against the code base at commit `16df41068e9592847de76789eb93312b3e770273`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRestoreTestingSelection

## Description
Four net-new causes in the REFUSAL path of the AWS Backup restore-testing-SELECTION surface. The open issue 20261114T123121Z-backup-aws-backup-restore-testing-plan-refusals-omit-the.md records the same family of faults on the restore-testing PLAN operations (CreateRestoreTestingPlan / GetRestoreTestingPlan / DeleteRestoreTestingPlan); it names none of the SELECTION operations or the fixtures below, so it does not cover these.

Cause 1 of 4: CreateRestoreTestingSelection against a non-existent plan refuses with the wrong error class, wrong message and no Code.
AWS: Smithy CreateRestoreTestingSelection -> PUT /restore-testing/plans/{RestoreTestingPlanName}/selections, success 201; its modeled errors include ResourceNotFoundException (client, HTTP 400) and MissingParameterValueException (client, HTTP 400), both of which model Code, Message, Type, Context. Existence of the parent plan named in the httpLabel is checked BEFORE the body's selection is validated, so the refusal is ResourceNotFoundException: HTTP 400, body Code 'ERROR_RT_5000', Message 'Restore testing plan does not exist.' That message is entirely fixed text, with no variable part.
Emulator: HTTP 400 but x-amzn-errortype 'MissingParameterValueException', no Code member at all, and Message 'Missing Resource Selection.' - it validates the request body first and never reports the missing parent plan.
Why: a client gets the wrong diagnosis of its own failure: it is told a required parameter is missing when in fact the plan it addressed does not exist, so retry/create-parent logic keyed on ResourceNotFoundException (or on Code 'ERROR_RT_5000', the only stable machine-readable discriminator Backup offers here) never fires and the caller edits a request that was never malformed.
Fix: the CreateRestoreTestingSelection handler's validation ordering in the backup service crate, plus the shared refusal serializer - the parent-plan lookup must precede body validation and must emit ResourceNotFoundException with the Code member and AWS's fixed message.
Traces: backup-restore-testing-error-paths#task#4.

Cause 2 of 4: GetRestoreTestingSelection's not-found refusal omits Code and re-words Message.
AWS: Smithy GetRestoreTestingSelection errors are ResourceNotFoundException (client, HTTP 400) and ServiceUnavailableException (server, 500); ResourceNotFoundException models Code, Message, Type, Context. When the addressed plan does not exist the recorded refusal is HTTP 400 with Code 'ERROR_RT_5000' and Message 'Restore testing plan does not exist.' - all fixed text.
Emulator: HTTP 400 with no Code member and Message 'Cannot find restore testing plan', the emulator's own wording.
Why: without Code the client cannot distinguish a missing plan from a missing selection or from any other 400 on this operation, and assertions on AWS's documented message text fail even though the status and error class are right.
Fix: the GetRestoreTestingSelection not-found path and the backup error serializer - populate Code and AWS's fixed message.
Traces: backup-restore-testing-not-found-errors#task#2.

Cause 3 of 4: after DeleteRestoreTestingSelection, GetRestoreTestingSelection still answers 200 with the selection body instead of refusing.
AWS: once a selection is deleted, GetRestoreTestingSelection on that plan/selection pair must fail ResourceNotFoundException: HTTP 400, body Code 'ERROR_RT_5001', Message 'Restore testing selection does not exist.' (fixed text, no variable part), with an x-amzn-errortype header. Note the selection-level Code 'ERROR_RT_5001' is distinct from the plan-level 'ERROR_RT_5000'.
Emulator: the delete is accepted, but the following GetRestoreTestingSelection returns HTTP 200 and a full selection body - no 400, no Code, no Message, no x-amzn-errortype.
Why: deletion does not remove the selection from state, so teardown appears to succeed while the resource lives on: a delete-then-recreate of the same selection name collides with the duplicate refusal of Cause 4, and any client that treats a successful read as proof of existence concludes a deleted selection is still active and still in scope for restore testing.
Fix: the DeleteRestoreTestingSelection handler and the restore-testing-selection state module - the delete must remove (or tombstone) the record so every later read and mutation of that name takes the not-found path.
Traces: backup-restore-testing-plan-lifecycle#teardown#9 (also affects backup-restore-testing-selection-conditions-and-overrides#teardown#6, backup-restore-testing-selection-lifecycle#task#8 and backup-restore-testing-selection-lifecycle-wildcard#task#7).

Cause 4 of 4: CreateRestoreTestingSelection's duplicate-name refusal omits Code and re-words Message.
AWS: CreateRestoreTestingSelection models AlreadyExistsException (client, HTTP 400) with members Code, Message, CreatorRequestId, Arn, Type, Context. The recorded refusal carries Code 'ERROR_RT_3001' and Message 'Restore testing plan [<plan-name>] in account [<account-id>] already contains selection [<selection-name>].' The fixed text is 'Restore testing plan [' ... '] in account [' ... '] already contains selection [' ... '].'; the three bracketed parts are variables - the plan name and selection name from the request and the account id from the caller's identity. (The recording's own probe names and account 000000000000 are that run's values, not part of the template.)
Emulator: HTTP 400 with no Code member and Message 'Restore testing selection with the same name already exists' - emulator-authored wording that names neither the plan, the account nor the selection.
Why: a client that branches on Code cannot separate a duplicate SELECTION ('ERROR_RT_3001') from a duplicate PLAN ('ERROR_RT_3000'), since both are AlreadyExistsException/400; and an operator parsing or logging the message gets no plan, account or selection name, so the collision cannot be attributed.
Fix: the CreateRestoreTestingSelection duplicate-name validation and the backup error serializer - emit the Code member and AWS's message template for AlreadyExistsException.
Traces: backup-restore-testing-selection-conditions-and-errors#task#5 (also affects backup-restore-testing-selection-conditions-and-errors#task#6).

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up: /__admin/health answered 200 and the data listener accepts).

Cause 1 - CreateRestoreTestingSelection on an unknown plan, expected 400 / actual 400:
  $.Code: missing (expected 'ERROR_RT_5000', got None)
  $.Message: value_mismatch (expected 'Restore testing plan does not exist.', got 'Missing Resource Selection.')
  $headers.x-amzn-errortype: shape_mismatch (got 'MissingParameterValueException')
  fixture: backup-restore-testing-error-paths#task#4

Cause 2 - GetRestoreTestingSelection on an unknown plan, expected 400 / actual 400:
  $.Code: missing (expected 'ERROR_RT_5000', got None)
  $.Message: value_mismatch (expected 'Restore testing plan does not exist.', got 'Cannot find restore testing plan')
  fixture: backup-restore-testing-not-found-errors#task#2

Cause 3 - GetRestoreTestingSelection after DeleteRestoreTestingSelection, expected 400 / actual 200:
  status: expected 400, got 200
  $.Code: missing (expected 'ERROR_RT_5001', got None)
  $.Message: missing (expected 'Restore testing selection does not exist.', got None)
  $headers.x-amzn-errortype: missing (got None)
  fixtures: backup-restore-testing-plan-lifecycle#teardown#9; backup-restore-testing-selection-conditions-and-overrides#teardown#6; backup-restore-testing-selection-lifecycle#task#8; backup-restore-testing-selection-lifecycle-wildcard#task#7

Cause 4 - CreateRestoreTestingSelection with an existing selection name, expected 400 / actual 400:
  $.Code: missing (expected 'ERROR_RT_3001', got None)
  $.Message: value_mismatch (expected 'Restore testing plan [<plan-name>] in account [<account-id>] already contains selection [<selection-name>].', got 'Restore testing selection with the same name already exists')
  fixtures: backup-restore-testing-selection-conditions-and-errors#task#5; backup-restore-testing-selection-conditions-and-errors#task#6

Model references (services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json): CreateRestoreTestingSelection PUT /restore-testing/plans/{RestoreTestingPlanName}/selections code 201, errors {AlreadyExistsException 400, InvalidParameterValueException 400, LimitExceededException 400, MissingParameterValueException 400, ResourceNotFoundException 400, ServiceUnavailableException 500}; GetRestoreTestingSelection GET /restore-testing/plans/{RestoreTestingPlanName}/selections/{RestoreTestingSelectionName} code 200, errors {ResourceNotFoundException 400, ServiceUnavailableException 500}; ResourceNotFoundException members {Code, Message, Type, Context}; MissingParameterValueException members {Code, Message, Type, Context}; AlreadyExistsException members {Code, Message, CreatorRequestId, Arn, Type, Context}.

## Steps to Reproduce
1. git clone the repository and boot the emulator built from that checkout.
2. Replay the wire corpus simparity-wire-aws-v1.
3. Cause 1: replay backup-restore-testing-error-paths step task#4 - CreateRestoreTestingSelection naming a plan that does not exist; the 400 comes back as MissingParameterValueException 'Missing Resource Selection.' with no Code, instead of ResourceNotFoundException with Code 'ERROR_RT_5000' and 'Restore testing plan does not exist.'
4. Cause 2: replay backup-restore-testing-not-found-errors step task#2 - GetRestoreTestingSelection on an unknown plan; the 400 body has no Code and says 'Cannot find restore testing plan'.
5. Cause 3: replay backup-restore-testing-plan-lifecycle through teardown#9 - after DeleteRestoreTestingSelection, GetRestoreTestingSelection on the deleted selection returns 200 with the selection body instead of 400 ResourceNotFoundException / Code 'ERROR_RT_5001'.
6. Cause 4: replay backup-restore-testing-selection-conditions-and-errors step task#5 - CreateRestoreTestingSelection re-using an existing selection name; the 400 body has no Code 'ERROR_RT_3001' and carries the emulator's generic duplicate wording.
