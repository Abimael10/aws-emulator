Issue-Source: trace-sweep-divergence
Issue-Trace: backup-on-demand-dynamodb-backup-job-lands-recovery-point
Issue-Step: task#8
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: backup-on-demand-dynamodb-backup-job-lands-recovery-point
Issue-Shape-Id: com.amazonaws.backup#CryoControllerUserManager
Issue-Key: backup|DescribeBackupJob|2xx->2xx|$.BackupSizeInBytes
Issue-Key: backup|DescribeBackupJob|2xx->2xx|$.BytesTransferred
Issue-Key: backup|ListRecoveryPointsByBackupVault|2xx->2xx|$.RecoveryPoints[*].BackupSizeInBytes
Issue-Commit: 2db451d8fc0a1629d422cad36403ee8c23c6ccb3

# AWS Backup reports a computed byte count for a DynamoDB backup instead of the resource-type-correct 0 (2 causes)

> _Note: this issue was identified against the code base at commit `2db451d8fc0a1629d422cad36403ee8c23c6ccb3`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeBackupJob

## Description
For an on-demand AWS Backup job over a plain (non-advanced-features) Amazon DynamoDB table, every size field the emulator serves is populated from a byte count it computed over the protected resource's own item payload, rather than from AWS's resource-type-specific rule. Two read operations expose the same stored value, so both diverge together.

Cause 1 of 2: DescribeBackupJob returns a non-zero BackupSizeInBytes and BytesTransferred for a completed DynamoDB on-demand backup job.
AWS: Smithy: BackupJob.BackupSizeInBytes (target Long) documents that the value is resource-type dependent and that "the value returned may show a value of 0, which may differ from the anticipated value"; the enumerated rules populate a recovery-point size only "For Amazon DynamoDB with advanced features". BackupJob.BytesTransferred (target Long) is "The size in bytes transferred to a backup vault at the time that the job status was queried." Recorded behavior confirms the rule for this resource class: for a plain DynamoDB table, the completed job reports BackupSizeInBytes = 0 and BytesTransferred = 0. Status is 200 in both AWS and the emulator; only the body values diverge.
Emulator: answers 200 with $.BackupSizeInBytes = 35 and $.BytesTransferred = 35 — the serialized size of the backed-up table's items — where AWS reports 0 for each.
Why: a client that polls DescribeBackupJob to report or gate on transferred/backup bytes reads a figure AWS never produces for this resource type. Code that treats a non-zero BytesTransferred as "this resource type reports progress" (and a zero as "size reporting is unavailable for DynamoDB, fall back to the table's own metrics") takes the wrong branch, and any assertion or dashboard pinned to AWS's 0 fails against the emulator. The size is also leaked from the emulator's internal storage representation, so it varies with however the emulator happens to encode items — a number no AWS client can predict.
Fix: the backup-job state that records size/progress when a backup job completes (the StartBackupJob completion path in the backup service's state), plus the DescribeBackupJob handler that projects it. The size must follow the resource-type rule the model documents — 0 for a plain DynamoDB protected resource — not a count over the emulator's stored item bytes.
Traces: backup-on-demand-dynamodb-backup-job-lands-recovery-point#task#8.

Cause 2 of 2: ListRecoveryPointsByBackupVault reports the same non-zero size on the recovery point the job created.
AWS: Smithy: RecoveryPointByBackupVault.BackupSizeInBytes (target Long), "The size, in bytes, of a backup." Recorded behavior only for the DynamoDB case: the recovery point landed by an on-demand backup of a plain DynamoDB table lists BackupSizeInBytes = 0, consistent with the job's own reported size. Status 200.
Emulator: answers 200 with $.RecoveryPoints[0].BackupSizeInBytes = 35, the same computed item-byte count the backup job stored.
Why: the recovery-point listing is the durable record a client inventories after the job is gone, so the wrong size persists past the job's lifetime; cost/size reporting built on vault listings reads a value AWS does not emit, and the job view and the recovery-point view agree only on the wrong number.
Fix: the recovery-point record created when a backup job completes (backup service state) and the ListRecoveryPointsByBackupVault serializer that emits BackupSizeInBytes — it must carry the resource-type-correct size, not the emulator's internal payload byte count.
Traces: backup-on-demand-dynamodb-backup-job-lands-recovery-point#task#9.

Neither cause is covered by an existing open issue: the open backup issues concern vault-lock/tiering refusal wording (20261029T175130Z-backup-aws-backup-vault-lock-and-tiering-refusals-use.md), plan/selection/vault refusals (20261103T061309Z-backup-aws-backup-plan-selection-and-vault-refusals-diverge.md) and restore-testing plan/selection read bodies (20261114T123121Z..., 20261119T231535Z...), none of which touch backup-job or recovery-point size reporting.

## Evidence
Replay of trace set simparity-wire-aws-v1, fixture backup-on-demand-dynamodb-backup-job-lands-recovery-point.

Cause 1 — step task#8, DescribeBackupJob, expected status 200 / actual 200:
  $.BackupSizeInBytes: value_mismatch (expected 0, got 35)
  $.BytesTransferred: value_mismatch (expected 0, got 35)

Cause 2 — step task#9, ListRecoveryPointsByBackupVault, expected status 200 / actual 200:
  $.RecoveryPoints[0].BackupSizeInBytes: value_mismatch (expected 0, got 35)

The expected 0 is not a recording artifact: Smithy's BackupJob.BackupSizeInBytes documentation states the field is resource-type dependent, that it "may show a value of 0", and that a populated recovery-point size applies "For Amazon DynamoDB with advanced features" — not for the plain DynamoDB table this fixture backs up. The actual 35 is the byte length of the emulator's own stored representation of the table's items, i.e. an internal implementation value, which is why it is identical across all three fields.

## Steps to Reproduce
1. Replay fixture backup-on-demand-dynamodb-backup-job-lands-recovery-point against the emulator.
2. The fixture creates a DynamoDB table, puts an item, creates a backup vault, and calls StartBackupJob for the table ARN, then polls to completion.
3. Step task#8: call DescribeBackupJob with the returned BackupJobId. Observe BackupSizeInBytes and BytesTransferred are 35 (the emulator's item byte count); AWS returns 0 for both for a plain DynamoDB protected resource.
4. Step task#9: call ListRecoveryPointsByBackupVault for that vault. Observe RecoveryPoints[0].BackupSizeInBytes is 35; AWS returns 0.
5. Both steps answer HTTP 200 in AWS and in the emulator — only the size values diverge.
