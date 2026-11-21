Issue-Source: trace-sweep-divergence
Issue-Trace: backup-rbx-traj-second-selection-mistakes-and-recovery
Issue-Step: teardown#16
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-rbx-traj-second-selection-mistakes-and-recovery
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|DeleteBackupPlan|2xx->4xx|status
Issue-Commit: 6ccffbe251dc34c011571ac1370edc0cb588a460

# AWS Backup DeleteBackupPlan refuses 400 at teardown where AWS deletes the plan 200

> _Note: this issue was identified against the code base at commit `6ccffbe251dc34c011571ac1370edc0cb588a460`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DeleteBackupPlan

## Description
One net-new wire divergence on AWS Backup's plan-deletion path: a backup plan whose selections have all been removed cannot be deleted at all — the emulator keeps answering a 400 refusal where AWS performs the delete and answers 200. ONE cause must be closed.

Cause 1 of 1: DeleteBackupPlan refuses a plan AWS deletes (fixture backup-rbx-traj-second-selection-mistakes-and-recovery, step teardown#16).
AWS: Smithy backup#DeleteBackupPlan is DELETE /backup/plans/{BackupPlanId} with success status 200 and output members BackupPlanId, BackupPlanArn, DeletionDate and VersionId; its modeled client errors are InvalidParameterValueException, InvalidRequestException, MissingParameterValueException and ResourceNotFoundException (all HTTP 400). The model documents the only state precondition: "A backup plan can only be deleted after all associated selections of resources have been deleted." Once the plan holds no selections, the delete must succeed. The recorded teardown step asserts HTTP 200 for this DeleteBackupPlan and asserts no body members, so the requirement here is the success status and the recording of the deletion, not any particular generated id, ARN or DeletionDate value.
Emulator: 400 — the plan is never deleted, so the account keeps holding it and its name stays taken.
Why: a client (and every teardown/cleanup loop, CloudFormation-style reconciler or test harness) that has already removed all of a plan's selections and then deletes the plan gets a hard client error instead of the 200 plus DeletionDate it must record. There is no further request it can make to make progress: the plan becomes undeletable, the resources it schedules keep being backed up, and the name cannot be reused.
Fix: the DeleteBackupPlan handler's dependency gate and the selection state it reads — services/service-com-amazonaws-backup-cryocontrollerusermanager/src/ops/delete_backup_plan.rs refuses with InvalidRequestException whenever store.backup_selections_of(plan_id) is non-empty, so the delete depends entirely on the crate's selection state matching AWS's. Hypothesis: on this trajectory it does not — the same fixture's CreateBackupSelection steps show the emulator persisting a selection AWS refuses (task#5, 400 expected, 200 served) and refusing the selection AWS accepts (task#7/#8, 200 expected, 400 served), so at teardown the emulator still has selections attached that AWS does not and the dependency gate fires on a plan AWS considers free of selections. The open issue 20261103T061309Z-backup-aws-backup-plan-selection-and-vault-refusals-diverge.md covers those CreateBackupSelection/ListBackupSelections steps (and the ERROR_9201 wording of the dependency refusal itself in a different fixture) but carries no replay key for this teardown step, and 20261108T023447Z-backup-aws-backup-plan-and-selection-refusals-diverge-missing.md covers other refusal steps of the same fixture; neither closes this one. Whichever way the selection state is corrected, DeleteBackupPlan must end this trajectory with a 200 that records the deletion.
Traces: backup-rbx-traj-second-selection-mistakes-and-recovery#teardown#16.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, 583 steps, score 68.8%).

Cluster service=backup|DeleteBackupPlan|status=200->400|keys=status — fixture backup-rbx-traj-second-selection-mistakes-and-recovery, step teardown#16, verdict mismatch: status expected 200, got 400 (no body members asserted by the recording). affected_count 1; owned_by empty (no existing issue carries this replay key).

Model checked (services/service-com-amazonaws-backup-cryocontrollerusermanager/smithy-model/com-amazonaws-backup-cryocontrollerusermanager.json): backup#DeleteBackupPlan — input BackupPlanId (httpLabel, required); output BackupPlanId/BackupPlanArn/DeletionDate/VersionId; http DELETE /backup/plans/{BackupPlanId} code 200; errors InvalidParameterValueException, InvalidRequestException, MissingParameterValueException, ResourceNotFoundException (400), ServiceUnavailableException (500); doc: "A backup plan can only be deleted after all associated selections of resources have been deleted."

Code read: src/ops/delete_backup_plan.rs lines 178-199 — ResourceNotFoundException when live_backup_plan() is None, then InvalidRequestException with MESSAGE_PLAN_SELECTIONS_EXIST when backup_selections_of() is non-empty; the 200 path at lines 212-218 records deletion_date and returns the four output members.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus, trace set simparity-wire-aws-v1).
2. Replay fixture backup-rbx-traj-second-selection-mistakes-and-recovery in order: the plan is created, selections are created/refused/retried through task#5-task#13, the teardown deletes the plan's selections, and then step teardown#16 issues DELETE /backup/plans/{BackupPlanId}.
3. Observe step teardown#16: the emulator answers HTTP 400 where the recording asserts HTTP 200.
4. Expected after the fix: teardown#16 answers 200 and the plan's deletion is recorded (DeletionDate set, GetBackupPlan reporting it), with no dependency refusal once the plan's selections are gone.
