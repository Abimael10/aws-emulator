Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: backup.vault-lock-min-retention-exceeds-max-rejected
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|PutBackupVaultLockConfiguration|InvalidParameterValueException
Issue-Commit: 991354efed443a569b426643157f0d67ea150cda

# PutBackupVaultLockConfiguration min>max refusal uses an emulator-invented message that echoes the submitted bounds

> _Note: this issue was identified against the code base at commit `991354efed443a569b426643157f0d67ea150cda`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
PutBackupVaultLockConfiguration

## Description
One net-new root cause in the `backup` group (services/service-com-amazonaws-backup-cryocontrollerusermanager).

Cause 1 of 1: the MinRetentionDays-exceeds-MaxRetentionDays refusal of PutBackupVaultLockConfiguration is answered with an emulator-invented sentence that interpolates the two submitted values, instead of AWS's fixed retention-range sentence. Failing check: backup.vault-lock-min-retention-exceeds-max-rejected.

AWS: Smithy (com.amazonaws.backup#CryoControllerUserManager): PutBackupVaultLockConfiguration is PUT /backup-vaults/{BackupVaultName}/vault-lock; MinRetentionDays and MaxRetentionDays both target Long with NO @range, so the relation between them is NOT a modeled constraint and is a service-side rule; the modeled refusal class is InvalidParameterValueException (HTTP 400). The MinRetentionDays doc states "The shortest minimum retention period you can specify is 1 day." Recorded behavior: when the request's MinRetentionDays is greater than its MaxRetentionDays, the call is refused InvalidParameterValueException (HTTP 400) with the single sentence "Backup vault Lock configuration retention days must be between 1 and 36,500, and minimum must be less than or equal to maximum." That sentence is ENTIRELY fixed text — including the thousands separator in "36,500" and the trailing period — and it has NO variable parts: neither member name nor either submitted value appears in it. AWS uses this one sentence for the whole retention-days rule class (out-of-range bound and inverted bounds alike). The vault is left unlocked and no bounds are stored.

Emulator: the same request is refused with the right code (InvalidParameterValueException matched) but the message "MinRetentionDays (30) must not exceed MaxRetentionDays (7)" — a sentence of the emulator's own wording that names the two member names and echoes BOTH submitted values from the request. That is a different message template, not just different values: AWS's template contains no request-derived substitution at all. The divergence is in the min<=max relation check of the PutBackupVaultLockConfiguration handler; it is NOT the shared out-of-range formatter already described as cause 2 of open issue 20261029T175130Z-backup-aws-backup-vault-lock-and-tiering-refusals-use.md (that one covers the "{member} must be between {min} and {max} days, got {value}" template used for ChangeableForDays/MinRetentionDays/MaxRetentionDays bounds), and this check is marked neither ALREADY FILED nor OWNED in the replay report, so it is a separate relation-check branch that must be closed on its own.

Why: a client or compliance test pinned to AWS's documented refusal wording treats a correctly-refused vault-lock request as an unknown failure; tooling that surfaces the message to an operator shows text no AWS account ever returns, and anything grepping AWS's sentence (or asserting that the refusal leaks no submitted values) misbehaves even though the status and error code are correct.

Fix: the retention-bounds validation of the PutBackupVaultLockConfiguration handler in the AWS Backup crate — its min>max branch must emit AWS's single fixed retention-days sentence (no member names, no echoed request values) under the InvalidParameterValueException class, consistent with the bounds branch.

Traces: backup.vault-lock-min-retention-exceeds-max-rejected.

## Evidence
Suite: backup 34/52 (65%), 18 failed. All other backup-group failures are marked ALREADY FILED (unimplemented scaffold stubs) or OWNED by an open issue; this is the only net-new one.

[mismatch] backup.vault-lock-min-retention-exceeds-max-rejected
  File "check_backup_vault_lock_min_retention_exceeds_max_rejected.py", line 41, in run -> expect_error(...)
  AssertionError: expected message 'Backup vault Lock configuration retention days must be between 1 and 36,500, and minimum must be less than or equal to maximum.', got 'MinRetentionDays (30) must not exceed MaxRetentionDays (7)'

The error CODE matched (expect_error asserts the code before the message, and that assertion passed), so the divergence is confined to the message text. Request that triggered it: PutBackupVaultLockConfiguration(BackupVaultName=<fresh vault>, MinRetentionDays=30, MaxRetentionDays=7, ChangeableForDays=3).

Expected message: fixed text in full — "Backup vault Lock configuration retention days must be between 1 and 36,500, and minimum must be less than or equal to maximum." — with no variable parts.
Actual message: "MinRetentionDays (<submitted min>) must not exceed MaxRetentionDays (<submitted max>)"; the fixed parts are "MinRetentionDays (", ") must not exceed MaxRetentionDays (", ")", and the variable parts are the two values taken from the request (30 and 7 in this run).

The check never reached its later assertions (vault still Locked=false with no stored MinRetentionDays/MaxRetentionDays after the rejection, and the equal-bounds positive control at MinRetentionDays=MaxRetentionDays=7 locking the same vault), so those behaviors are unverified rather than known-good.

## Steps to Reproduce
1. backup.CreateBackupVault(BackupVaultName=v) on a fresh vault name.
2. backup.PutBackupVaultLockConfiguration(BackupVaultName=v, MinRetentionDays=30, MaxRetentionDays=7, ChangeableForDays=3).
   Expect: InvalidParameterValueException (HTTP 400) with the message 'Backup vault Lock configuration retention days must be between 1 and 36,500, and minimum must be less than or equal to maximum.'
   Actual: InvalidParameterValueException with 'MinRetentionDays (30) must not exceed MaxRetentionDays (7)'.
3. backup.DescribeBackupVault(BackupVaultName=v) must still report Locked=false with no MinRetentionDays/MaxRetentionDays keys.
4. Positive control on the SAME vault: backup.PutBackupVaultLockConfiguration(BackupVaultName=v, MinRetentionDays=7, MaxRetentionDays=7, ChangeableForDays=3) must succeed; DescribeBackupVault then reports Locked=true, MinRetentionDays=7, MaxRetentionDays=7.
