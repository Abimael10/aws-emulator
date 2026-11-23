Issue-Source: trace-sweep-divergence
Issue-Trace: backup-restore-testing-plan-lifecycle
Issue-Step: task#3
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-restore-testing-plan-lifecycle backup-restore-testing-error-paths backup-restore-testing-selection-conditions-and-errors backup-restore-testing-plan-duplicate-error backup-restore-testing-plan-update-lifecycle
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|GetRestoreTestingPlan|2xx->2xx|$.RestoreTestingPlan.LastUpdateTime
Issue-Key: backup|CreateRestoreTestingPlan|2xx->2xx|$.RestoreTestingPlanArn
Issue-Key: backup|GetRestoreTestingPlan|2xx->2xx|$.RestoreTestingPlan.ScheduleExpressionTimezone
Issue-Key: backup|GetRestoreTestingPlan|2xx->2xx|$.RestoreTestingPlan.RecoveryPointSelection.SelectionWindowDays
Issue-Key: backup|GetRestoreTestingPlan|2xx->2xx|$.RestoreTestingPlan.ScheduleExpression
Issue-Commit: 89c70456cbc5c776860ae07cdd8fb6580c26b557

# AWS Backup restore testing plan reads omit LastUpdateTime and ScheduleExpressionTimezone, updates never persist, and the plan ARN omits its unique suffix

> _Note: this issue was identified against the code base at commit `89c70456cbc5c776860ae07cdd8fb6580c26b557`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetRestoreTestingPlan

## Description
Four net-new causes on the AWS Backup restore-testing-plan surface. The emulator answers all of these operations with 2xx, so every defect here is a silent body/state divergence rather than a refusal. No open issue covers the restore-testing surface (the closest, 20261110T070620Z-backup-aws-backup-refusal-bodies-omit-the-modeled-code.md, covers Code omission on other Backup operations only, and 20261103T061309Z-backup-aws-backup-plan-and-selection-read-bodies-diverge.md covers backup plans/selections, not restore testing plans).

Cause 1 of 4: GetRestoreTestingPlan never reports RestoreTestingPlan.LastUpdateTime.
AWS: Smithy RestoreTestingPlanForGet.LastUpdateTime : Timestamp ("The date and time that the restore testing plan was updated ... accurate to milliseconds"). The recording shows AWS populates LastUpdateTime on a read of a plan, including one that has only ever been created; the value is a millisecond-precision epoch timestamp taken from the plan's state (the time of its last create/update), never absent.
Emulator: the member is absent from the response body ($.RestoreTestingPlan.LastUpdateTime: missing, got None) while the rest of the plan body is returned with status 200.
Why: a client that reads a plan to decide whether its configuration is current (change detection, drift reporting, "last modified" display, or a poll that waits for an update to land) has no timestamp at all; code that dereferences the member sees a null for a field AWS always returns.
Fix: the restore-testing-plan read path in the backup service's GetRestoreTestingPlan handler and the plan record in its state module, which must carry and surface a last-update timestamp set at create time and refreshed on update.
Traces: backup-restore-testing-plan-lifecycle#task#3 (LastUpdateTime only), backup-restore-testing-plan-duplicate-error#task#3, backup-restore-testing-plan-update-lifecycle#task#5.

Cause 2 of 4: GetRestoreTestingPlan omits the defaulted ScheduleExpressionTimezone.
AWS: Smithy RestoreTestingPlanForGet.ScheduleExpressionTimezone : String, documented "Optional. This is the timezone in which the schedule expression is set. By default, ScheduleExpressions are in UTC." The recording shows AWS materializes that default into stored state and returns the fixed string 'Etc/UTC' on a read of a plan whose create request omitted the member.
Emulator: the member is absent from the body ($.RestoreTestingPlan.ScheduleExpressionTimezone: missing, expected 'Etc/UTC', got None).
Why: the timezone a plan's cron expression is interpreted in is unreadable, so a client cannot render or re-submit a plan round-trip faithfully; an UpdateRestoreTestingPlan built from a Get body drops the timezone instead of preserving it.
Fix: the restore-testing-plan create path's default materialization and the GetRestoreTestingPlan serializer in the backup service crate — the default belongs in state at create time, not only in the reply.
Traces: backup-restore-testing-plan-duplicate-error#task#3, backup-restore-testing-plan-update-lifecycle#task#5.

Cause 3 of 4: UpdateRestoreTestingPlan reports success but no field of the plan actually changes.
AWS: Smithy UpdateRestoreTestingPlan (PUT /restore-testing/plans/{RestoreTestingPlanName}, 200) takes RestoreTestingPlanForUpdate with RecoveryPointSelection, ScheduleExpression, ScheduleExpressionTimezone and StartWindowHours, and returns UpdateTime (required). RestoreTestingRecoveryPointSelection.SelectionWindowDays is documented "integers from 1 to 365. If not included, the value defaults to 30". The recording shows that after an accepted update, GetRestoreTestingPlan reports the SUBMITTED values — the new ScheduleExpression and the new SelectionWindowDays — plus a refreshed LastUpdateTime.
Emulator: the update is accepted, but the following GetRestoreTestingPlan still reports the pre-update state: ScheduleExpression is the original value (got 'cron(0 5 ? * * *)' where the updated plan's expression was expected), RecoveryPointSelection.SelectionWindowDays is still the create-time default 30 where the updated value was expected, and LastUpdateTime is still absent.
Why: a client cannot modify a restore testing plan at all. The write is acknowledged, so there is nothing to retry on, and every later read lies about the plan's schedule and selection window — a restore test runs on the old configuration while the caller believes the new one is in force.
Fix: the UpdateRestoreTestingPlan handler in the backup service crate must merge the submitted members into the stored plan record (and stamp the update time) instead of validating and discarding them.
Traces: backup-restore-testing-plan-update-lifecycle#task#5.

Cause 4 of 4: CreateRestoreTestingPlan returns a restore-testing-plan ARN built from the plan name alone, with no unique suffix.
AWS: Smithy CreateRestoreTestingPlan output RestoreTestingPlanArn : String (required); the ARN is also echoed by RestoreTestingPlanForGet.RestoreTestingPlanArn ("An Amazon Resource Name (ARN) that uniquely identifies a restore testing plan"). Recorded behavior only: the fixture asserts the ARN's SHAPE rather than a literal value, and AWS's shape for this resource is arn:aws:backup:<region>:<account-id>:restore-testing-plan:<plan-name>-<unique-suffix> — the trailing per-resource suffix is what makes the ARN uniquely identify the plan rather than just its name.
Emulator: returns arn:aws:backup:<region>:<account-id>:restore-testing-plan:<plan-name> with the suffix absent, which fails the recorded shape assertion ($.RestoreTestingPlanArn: value_mismatch against '<shaped-not-asserted>').
Why: the ARN is derivable from the name, so a client cannot distinguish a recreated plan from its predecessor, and any client or policy that parses/validates the AWS ARN shape for this resource rejects the emulator's value. Note the region, account id and plan name in the recorded ARN are environment values that fill the template; only the shape is the requirement.
Fix: the restore-testing-plan ARN construction used by the CreateRestoreTestingPlan handler (and whatever shares it for Get/Update/List) in the backup service crate — the per-plan unique suffix must be generated at create time and stored with the plan so every read returns the same ARN.
Traces: backup-restore-testing-error-paths#task#2 (also affects backup-restore-testing-selection-conditions-and-errors#setup#2).

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200).

Cause 1 — GetRestoreTestingPlan, expected 200 / actual 200:
  $.RestoreTestingPlan.LastUpdateTime: missing (expected a millisecond epoch timestamp, got None)
  fixtures: backup-restore-testing-plan-lifecycle#task#3; backup-restore-testing-plan-duplicate-error#task#3; backup-restore-testing-plan-update-lifecycle#task#5

Cause 2 — GetRestoreTestingPlan, expected 200 / actual 200:
  $.RestoreTestingPlan.ScheduleExpressionTimezone: missing (expected 'Etc/UTC', got None)
  fixtures: backup-restore-testing-plan-duplicate-error#task#3; backup-restore-testing-plan-update-lifecycle#task#5

Cause 3 — GetRestoreTestingPlan after an accepted UpdateRestoreTestingPlan, expected 200 / actual 200:
  $.RestoreTestingPlan.RecoveryPointSelection.SelectionWindowDays: value_mismatch (expected the updated value 45, got the create-time default 30)
  $.RestoreTestingPlan.ScheduleExpression: value_mismatch (expected the updated expression 'cron(0 6 ? * * *)', got the original 'cron(0 5 ? * * *)')
  $.RestoreTestingPlan.LastUpdateTime: missing
  $.RestoreTestingPlan.ScheduleExpressionTimezone: missing (expected 'Etc/UTC')
  fixture: backup-restore-testing-plan-update-lifecycle#task#5

Cause 4 — CreateRestoreTestingPlan, expected 201 / actual 201:
  $.RestoreTestingPlanArn: value_mismatch against the recorded ARN shape; emulator returned 'arn:aws:backup:us-west-2:000000000000:restore-testing-plan:<plan-name>' with no unique suffix
  fixtures: backup-restore-testing-error-paths#task#2; backup-restore-testing-selection-conditions-and-errors#setup#2

Model references (services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json): CreateRestoreTestingPlan (PUT /restore-testing/plans, 201) output RestoreTestingPlanArn required; GetRestoreTestingPlan (GET /restore-testing/plans/{RestoreTestingPlanName}, 200) output RestoreTestingPlanForGet with LastUpdateTime and ScheduleExpressionTimezone; UpdateRestoreTestingPlan (PUT /restore-testing/plans/{RestoreTestingPlanName}, 200) input RestoreTestingPlanForUpdate {RecoveryPointSelection, ScheduleExpression, ScheduleExpressionTimezone, StartWindowHours}, output UpdateTime required; RestoreTestingRecoveryPointSelection.SelectionWindowDays defaults to 30.

## Steps to Reproduce
1. git clone the repo and boot the emulator built from it.
2. Replay the wire corpus (simparity-wire-aws-v1).
3. Cause 4: replay backup-restore-testing-error-paths step task#2 — CreateRestoreTestingPlan 201; inspect RestoreTestingPlanArn and note it is exactly 'arn:aws:backup:<region>:<account>:restore-testing-plan:<name>' with no unique suffix.
4. Causes 1 and 2: replay backup-restore-testing-plan-lifecycle step task#3 and backup-restore-testing-plan-duplicate-error step task#3 — GetRestoreTestingPlan 200; the body has no LastUpdateTime and no ScheduleExpressionTimezone ('Etc/UTC').
5. Cause 3: replay backup-restore-testing-plan-update-lifecycle through step task#5 — UpdateRestoreTestingPlan is accepted with a new ScheduleExpression and RecoveryPointSelection.SelectionWindowDays, then GetRestoreTestingPlan still reports the pre-update ScheduleExpression and SelectionWindowDays=30 and still no LastUpdateTime.
