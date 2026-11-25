Issue-Source: trace-sweep-divergence
Issue-Trace: backup-restore-testing-error-paths
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-restore-testing-error-paths backup-restore-testing-plan-duplicate-error backup-restore-testing-not-found-errors backup-restore-testing-plan-lifecycle backup-restore-testing-plan-update-lifecycle backup-restore-testing-selection-conditions-and-overrides
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|CreateRestoreTestingPlan|4xx->4xx|$.Code
Issue-Key: backup|CreateRestoreTestingPlan|4xx->4xx|$.Message
Issue-Key: backup|GetRestoreTestingPlan|4xx->4xx|$.Code
Issue-Key: backup|GetRestoreTestingPlan|4xx->4xx|$.Message
Issue-Key: backup|GetRestoreTestingPlan|4xx->2xx|$.Code
Issue-Key: backup|GetRestoreTestingPlan|4xx->2xx|$.Message
Issue-Key: backup|GetRestoreTestingPlan|4xx->2xx|$headers.x-amzn-errortype
Issue-Key: backup|GetRestoreTestingPlan|4xx->2xx|status
Issue-Commit: 89c70456cbc5c776860ae07cdd8fb6580c26b557

# AWS Backup restore testing plan refusals omit the modeled Code and reword Message, and a deleted plan still reads back 200

> _Note: this issue was identified against the code base at commit `89c70456cbc5c776860ae07cdd8fb6580c26b557`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateRestoreTestingPlan

## Description
Three net-new causes in the refusal path of the AWS Backup restore-testing-plan surface. The open issue 20261110T070620Z-backup-aws-backup-refusal-bodies-omit-the-modeled-code.md records the same Code-omission fault on other Backup operations (tiering configuration, restore jobs, MPA approval teams) but names none of the restore-testing operations or fixtures below, so it does not cover these.

Cause 1 of 3: CreateRestoreTestingPlan's duplicate-name refusal omits Code and re-words Message.
AWS: Smithy CreateRestoreTestingPlan errors include AlreadyExistsException (client, HTTP 400), whose members are Code, Message, CreatorRequestId, Arn, Type, Context — Code is part of the modeled body. The recorded refusal carries Code 'ERROR_RT_3000' and Message 'Restore testing plan [<plan-name>] already exists in account [<account-id>].' The fixed text is 'Restore testing plan [' ... '] already exists in account [' ... '].'; the two bracketed parts are variables — the plan name from the request and the account id from the caller's identity (the recording's own 'sim2real_probe_backup_rtplanerr_ad3e7057' / '000000000000' are that run's values, not part of the template).
Emulator: 400 with no Code member at all and Message 'Restore testing plan with the same name already exists' — emulator-authored wording that names neither the plan nor the account.
Why: a client that branches on the body's Code (the only stable machine-readable discriminator Backup offers for these refusals, since several distinct faults share AlreadyExistsException/400) sees null and cannot classify the failure; a client or operator that parses or logs the message gets no plan name or account id, so a duplicate-plan collision is indistinguishable from any other already-exists condition.
Fix: the CreateRestoreTestingPlan handler's duplicate-name validation and the backup service's error serializer — it must emit the Code member and AWS's message template for AlreadyExistsException.
Traces: backup-restore-testing-error-paths#task#3 (also affects backup-restore-testing-plan-duplicate-error#task#2).

Cause 2 of 3: GetRestoreTestingPlan's not-found refusal omits Code and re-words Message.
AWS: Smithy GetRestoreTestingPlan errors are ResourceNotFoundException (client, HTTP 400) and ServiceUnavailableException; ResourceNotFoundException models Code, Message, Type, Context. The recorded refusal carries Code 'ERROR_RT_5000' and Message 'Restore testing plan does not exist.' — that message is entirely fixed text, with no variable part.
Emulator: 400 with no Code and Message 'Cannot find restore testing plan' — the emulator's own wording.
Why: a client cannot tell a missing restore testing plan from any other 400 on this operation without the Code, and assertions on AWS's documented message text fail even though the status and error class are right.
Fix: the GetRestoreTestingPlan not-found path in the backup service crate plus the shared refusal serializer, which must populate Code and AWS's fixed message.
Traces: backup-restore-testing-error-paths#task#5 (also affects backup-restore-testing-not-found-errors#task#1).

Cause 3 of 3: after DeleteRestoreTestingPlan, GetRestoreTestingPlan still answers 200 with the plan body instead of refusing.
AWS: Smithy DeleteRestoreTestingPlan is DELETE /restore-testing/plans/{RestoreTestingPlanName} with success code 204; GetRestoreTestingPlan on a name that does not exist must fail ResourceNotFoundException, HTTP 400, body Code 'ERROR_RT_5000' and Message 'Restore testing plan does not exist.' (fixed text). The recording shows exactly that read-after-delete refusal.
Emulator: the delete is accepted, but the subsequent GetRestoreTestingPlan returns 200 and a full plan body — no status 400, no Code, no Message, no x-amzn-errortype header.
Why: deletion does not actually remove the plan from state, so a client's cleanup/teardown appears to succeed while the resource lives on: a delete-then-recreate sequence collides with the AlreadyExistsException of Cause 1, and any code that treats a successful read as proof of existence concludes a deleted plan is still active.
Fix: the DeleteRestoreTestingPlan handler and the restore-testing-plan state module in the backup service crate — the delete must remove (or tombstone) the record so every later read and mutation of that name takes the not-found path.
Traces: backup-restore-testing-plan-lifecycle#teardown#11 (also affects backup-restore-testing-plan-update-lifecycle#task#7 and backup-restore-testing-selection-conditions-and-overrides#teardown#8).

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200).

Cause 1 — CreateRestoreTestingPlan, expected 400 / actual 400:
  $.Code: missing (expected 'ERROR_RT_3000', got None)
  $.Message: value_mismatch (expected 'Restore testing plan [<plan-name>] already exists in account [<account-id>].', got 'Restore testing plan with the same name already exists')
  fixtures: backup-restore-testing-error-paths#task#3; backup-restore-testing-plan-duplicate-error#task#2

Cause 2 — GetRestoreTestingPlan, expected 400 / actual 400:
  $.Code: missing (expected 'ERROR_RT_5000', got None)
  $.Message: value_mismatch (expected 'Restore testing plan does not exist.', got 'Cannot find restore testing plan')
  fixtures: backup-restore-testing-error-paths#task#5; backup-restore-testing-not-found-errors#task#1

Cause 3 — GetRestoreTestingPlan after DeleteRestoreTestingPlan, expected 400 / actual 200:
  status: expected 400, got 200
  $.Code: missing (expected 'ERROR_RT_5000', got None)
  $.Message: missing (expected 'Restore testing plan does not exist.', got None)
  $headers.x-amzn-errortype: missing (got None)
  fixtures: backup-restore-testing-plan-lifecycle#teardown#11; backup-restore-testing-plan-update-lifecycle#task#7; backup-restore-testing-selection-conditions-and-overrides#teardown#8

Model references (services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json): CreateRestoreTestingPlan errors {AlreadyExistsException 400, ConflictException 409, InvalidParameterValueException 400, LimitExceededException 400, MissingParameterValueException 400}; AlreadyExistsException members {Code, Message, CreatorRequestId, Arn, Type, Context}; GetRestoreTestingPlan errors {ResourceNotFoundException 400, ServiceUnavailableException 500}; ResourceNotFoundException members {Code, Message, Type, Context}; DeleteRestoreTestingPlan DELETE /restore-testing/plans/{RestoreTestingPlanName} success 204.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire corpus (simparity-wire-aws-v1).
3. Cause 1: replay backup-restore-testing-error-paths through step task#3 — CreateRestoreTestingPlan with an existing plan name; the 400 body has no Code and carries the emulator's 'Restore testing plan with the same name already exists' instead of AWS's '... already exists in account [...].' sentence.
4. Cause 2: replay backup-restore-testing-error-paths step task#5 — GetRestoreTestingPlan on an unknown plan name; the 400 body has no Code 'ERROR_RT_5000' and says 'Cannot find restore testing plan'.
5. Cause 3: replay backup-restore-testing-plan-lifecycle to completion — after DeleteRestoreTestingPlan, teardown#11's GetRestoreTestingPlan on the deleted name returns 200 with the plan body instead of 400 ResourceNotFoundException.
