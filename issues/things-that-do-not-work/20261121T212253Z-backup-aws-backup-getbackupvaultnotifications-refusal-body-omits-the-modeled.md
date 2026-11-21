Issue-Source: trace-sweep-divergence
Issue-Trace: backup-backup-vault-notifications
Issue-Step: task#2
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-backup-vault-notifications backup-vault-notifications-config
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|GetBackupVaultNotifications|4xx->4xx|$.Code
Issue-Commit: a45eebcdd8d9a9d0a1f0906c2cbe25c9ebaa13ba

# AWS Backup GetBackupVaultNotifications refusal body omits the modeled Code member (ERROR_2106)

> _Note: this issue was identified against the code base at commit `a45eebcdd8d9a9d0a1f0906c2cbe25c9ebaa13ba`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
GetBackupVaultNotifications

## Description
One net-new wire divergence on AWS Backup's vault-notification read path. GetBackupVaultNotifications for a vault that has no notification configuration is refused with the right HTTP status and the right sentence, but the emulator's error renderer drops the Code member that AWS puts in the body, so a client that branches on the Backup error code sees no code at all. ONE cause must be closed.

Cause 1 of 1: GetBackupVaultNotifications' not-found refusal body has no Code member (fixture backup-backup-vault-notifications, step task#2).
AWS: Smithy backup#GetBackupVaultNotifications takes BackupVaultName as an httpLabel (pattern ^[a-zA-Z0-9\-\_]{2,50}$) and models InvalidParameterValueException, MissingParameterValueException and ResourceNotFoundException, all httpError 400; every one of those shapes declares the members Code, Message, Type and Context (smithy backup#ResourceNotFoundException). When the named vault exists but carries no notification configuration, the recorded response is HTTP 400 whose body carries Code 'ERROR_2106' alongside the Message. 'ERROR_2106' is the error class's fixed identifier — it is not derived from the request — while the accompanying Message's variable part is the vault name taken from the request label.
Emulator: HTTP 400 with the expected Message (the replay reports no Message divergence) but $.Code absent (expected 'ERROR_2106', got None). The same omission is reproduced on the repeat reads in the cluster: backup-backup-vault-notifications#task#6, backup-vault-notifications-config#task#3 and backup-vault-notifications-config#task#7.
Why: AWS Backup clients dispatch on the body's Code, not on the prose, to tell "this vault simply has no notification configuration" (a benign ERROR_2106 to be treated as 'none configured' before PutBackupVaultNotifications) from other 400 classes. With Code absent the discriminator a caller reads is null, so the caller either treats the refusal as an unknown hard failure or has to string-match the sentence.
Fix: the backup crate's error renderer for the Backup JSON error body — the GetBackupVaultNotifications handler's not-found path must populate the exception shape's Code member ('ERROR_2106') as well as Message. This is the same renderer-drops-Code fault as open issue 20261103T061309Z-backup-aws-backup-plan-selection-and-vault-refusals-diverge.md (causes 3, 5, 9) and 20261114T123121Z-backup-aws-backup-restore-testing-plan-refusals-omit-the.md, but neither of those issues carries this operation's replay key, so this step is not exercised by closing them.
Traces: backup-backup-vault-notifications#task#2, backup-backup-vault-notifications#task#6, backup-vault-notifications-config#task#3, backup-vault-notifications-config#task#7.

## Evidence
Replay of trace set simparity-wire-aws-v1 (emulator up, /__admin/health 200).

Cluster service=backup|GetBackupVaultNotifications|status=400->400|keys=$.Code — expected status 400, actual status 400; detail: "$.Code: missing (expected 'ERROR_2106', got None)".
Representative: backup-backup-vault-notifications#task#2. Affected (4): backup-backup-vault-notifications#task#2, backup-backup-vault-notifications#task#6, backup-vault-notifications-config#task#3, backup-vault-notifications-config#task#7.
No Message, header or status divergence is reported for this cluster — the sole diverging field is the absent Code.

Model checked: backup#GetBackupVaultNotifications (GET /backup-vaults/{BackupVaultName}/notification-configuration, errors InvalidParameterValueException/MissingParameterValueException/ResourceNotFoundException all 400) and backup#ResourceNotFoundException / backup#InvalidParameterValueException, whose members are Code, Message, Type, Context.

## Steps to Reproduce
1. Boot the emulator from this checkout and replay the wire corpus (replay_wire_corpus, trace set simparity-wire-aws-v1).
2. Replay fixture backup-backup-vault-notifications, step task#2: GetBackupVaultNotifications against a vault that has no notification configuration.
3. Observe the 400 body: it carries the expected Message but no Code member; AWS's body carries Code 'ERROR_2106'.
4. The same assertion fails on backup-backup-vault-notifications#task#6 and on backup-vault-notifications-config#task#3 and #task#7; all four must pass after the fix.
