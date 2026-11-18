Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: backup__kms__s3.refarch-bkp-cold-storage-within-90-days-of-delete-rejected
Issue-Key: backup__kms__s3.refarch-bkp-cold-storage-within-90-days-of-delete-rejected|run#7|AssertionError|assert message == (
Issue-Commit: 064209f8aee416a732a3da5258609e59028b5808

# CreateBackupPlan's lifecycle refusal message drops AWS's "Error in some rules due to : " rule-error prefix

> _Note: this issue was identified against the code base at commit `064209f8aee416a732a3da5258609e59028b5808`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateBackupPlan

## Description
One net-new root cause behind one failing check in the `backup__kms__s3` group. The check is marked neither ALREADY FILED nor OWNED in the replay report, and no open backup issue covers it: the nearest open issues — 20261029T175130Z-backup-aws-backup-vault-lock-and-tiering-refusals-use.md (vault-lock / CreateTieringConfiguration refusal wording), 20261103T061309Z-backup-aws-backup-plan-selection-and-vault-refusals-diverge.md and 20261105T091756Z-backup-aws-backup-updatebackupplan-refusals-and-response-versionid-diverge.md — name other operations and other refusal classes, not the CreateBackupPlan rule-lifecycle refusal.

Cause 1 of 1: the per-rule lifecycle refusal is emitted as the bare rule error, without the envelope sentence AWS wraps rule-level validation errors in.

AWS: Smithy (services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json): CreateBackupPlan models InvalidParameterValueException (HTTP 400, "Indicates that something is wrong with a parameter's value"); Lifecycle.DeleteAfterDays doc states "This value must be at least 90 days after the number of days specified in MoveToColdStorageAfterDays", and the Lifecycle shape doc states "Backups transitioned to cold storage must be stored in cold storage for a minimum of 90 days. Therefore, on the console, the retention setting must be 90 days greater than the transition to cold after days setting." The 90-day rule itself is modelled prose, not a @range, so the wire message is recorded behavior: AWS refuses such a plan with InvalidParameterValueException and the message "Error in some rules due to : Invalid lifecycle. DeleteAfterDays cannot be less than 90 days apart from MoveToColdStorageAfterDays". That message is entirely FIXED text in two parts — the rule-error envelope "Error in some rules due to : " that AWS prepends to any per-rule validation failure in BackupPlan.Rules, followed by the rule error "Invalid lifecycle. DeleteAfterDays cannot be less than 90 days apart from MoveToColdStorageAfterDays". No value from the request (no rule name, no plan name, no submitted day counts) appears in it. Nothing is created: ListBackupPlans does not list the refused plan name. A plan whose interval is at or above 90 days is accepted and round-trips MoveToColdStorageAfterDays/DeleteAfterDays unchanged.

Emulator: the same CreateBackupPlan is refused with the correct status and the correct code InvalidParameterValueException, but the message is only the inner rule error — "Invalid lifecycle. DeleteAfterDays cannot be less than 90 days apart from MoveToColdStorageAfterDays" — with the fixed envelope "Error in some rules due to : " missing. Replay: AssertionError: unexpected rejection message for the 30d/60d lifecycle: 'Invalid lifecycle. DeleteAfterDays cannot be less than 90 days apart from MoveToColdStorageAfterDays'. The rest of the check's expectations hold (the plan is not created; the 30/120 control is accepted and round-trips).

Why: a client that surfaces or matches the refusal text gets a sentence no AWS account returns. The envelope is how AWS marks a failure as coming from one of the plan's RULES rather than from a top-level plan parameter, so tooling that keys on that prefix to attribute the error to BackupPlan.Rules (and compliance/regression tests pinned to AWS's wording) mis-handle an otherwise correctly refused request.

Fix: the backup-plan body validation shared by CreateBackupPlan and UpdateBackupPlan — the rule-lifecycle branch of `crate::state::rejected_backup_plan_body` (reached from the handler's `RejectedBackupPlanBody::Value` arm) must word a per-rule validation failure with AWS's rule-error envelope prefixed to the rule error, as fixed text with no interpolated request values; the same envelope must apply to every per-rule refusal that path produces, not only the lifecycle one.

Traces: backup__kms__s3.refarch-bkp-cold-storage-within-90-days-of-delete-rejected.

## Evidence
Suite: backup__kms__s3 11/13 (85%), 2 failed; this is the only net-new one (the other, refarch-bkp-plan-update-adds-copy-action-and-supersedes-version, is ALREADY FILED against the unimplemented ListBackupPlanVersions).

Cause 1 — [mismatch] backup__kms__s3.refarch-bkp-cold-storage-within-90-days-of-delete-rejected:
  AssertionError: unexpected rejection message for the 30d/60d lifecycle: 'Invalid lifecycle. DeleteAfterDays cannot be less than 90 days apart from MoveToColdStorageAfterDays'
  Request: CreateBackupPlan(BackupPlan={BackupPlanName=<unique>, Rules=[{RuleName=<unique>, TargetBackupVaultName=<vault>, ScheduleExpression='cron(0 0 * * ? *)', Lifecycle={MoveToColdStorageAfterDays: 30, DeleteAfterDays: 60}}]}) — interval 30 days, below the 90-day minimum.
  Expected (AWS): HTTP 400, Code InvalidParameterValueException, Message 'Error in some rules due to : Invalid lifecycle. DeleteAfterDays cannot be less than 90 days apart from MoveToColdStorageAfterDays' (all fixed text; the envelope 'Error in some rules due to : ' plus the rule error).
  Actual (emulator): HTTP 400, Code InvalidParameterValueException (both correct), Message 'Invalid lifecycle. DeleteAfterDays cannot be less than 90 days apart from MoveToColdStorageAfterDays' — the envelope prefix is absent.
  Unaffected by this defect and already correct: the refused plan name is absent from ListBackupPlans, and the positive control CreateBackupPlan with {MoveToColdStorageAfterDays: 30, DeleteAfterDays: 120} is accepted, is listed, and GetBackupPlan round-trips both numbers.

## Steps to Reproduce
1. backup.CreateBackupVault(BackupVaultName=v).
2. backup.CreateBackupPlan(BackupPlan={BackupPlanName=p, Rules=[{RuleName=r, TargetBackupVaultName=v, ScheduleExpression='cron(0 0 * * ? *)', Lifecycle={'MoveToColdStorageAfterDays': 30, 'DeleteAfterDays': 60}}]}) — any interval strictly under 90 days between the two members.
3. Read the ClientError: expect Code 'InvalidParameterValueException' and Message exactly 'Error in some rules due to : Invalid lifecycle. DeleteAfterDays cannot be less than 90 days apart from MoveToColdStorageAfterDays'. The emulator returns the same code but omits the leading 'Error in some rules due to : '.
4. Confirm the refusal created nothing: ListBackupPlans (paginated) must not list BackupPlanName p.
5. Control: the same plan with DeleteAfterDays=120 must be accepted, appear in ListBackupPlans, and GetBackupPlan must report Lifecycle {'MoveToColdStorageAfterDays': 30, 'DeleteAfterDays': 120}.
