Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__rds.xsvc-rds-2-disabled-cmk-rejected-for-encrypted-create-and-snapshot-copy
Issue-Key: kms__rds.xsvc-rds-2-disabled-cmk-rejected-for-encrypted-create-and-snapshot-copy|run#42|AssertionError|assert msg == f"The target KMS key [{key['Arn']}] does not exist, is not enabled or you do not have permissions to access it.", (
Issue-Commit: 3c85b1e84177fad89070056cfc3db9067a84c48d

# kms__rds: CopyDBSnapshot's KMSKeyNotAccessibleFault message says 'Specified KMS key [...]' where AWS says 'The target KMS key [...]'

> _Note: this issue was identified against the code base at commit `3c85b1e84177fad89070056cfc3db9067a84c48d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CopyDBSnapshot

## Description
Cause 1 of 1: the KMS-inaccessible refusal message for CopyDBSnapshot (the DB *instance* snapshot copy) carries the wording of the DB *cluster* snapshot copy path.

  AWS: Smithy (services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json) models CopyDBSnapshot with error KMSKeyNotAccessibleFault, errorKind client, httpError 400, awsQueryError code KMSKeyNotAccessibleFault — documented only as "An error occurred accessing an Amazon Web Services KMS key.", so the SENTENCE is recorded behavior only. On the copy path of a DB instance snapshot, AWS words the fault as a statement about the TARGET key: fixed text "The target KMS key [" ... "] does not exist, is not enabled or you do not have permissions to access it." Its single variable part is the KmsKeyId the request carried, echoed in the spelling the request used (key id, key ARN, alias name or alias ARN); neither the source snapshot, the source key, nor the reason (absent / disabled / not permitted) is named. The status is 400 and the code is KMSKeyNotAccessibleFault, and no target snapshot is left behind — the emulator already gets all three of those right.

  Emulator: the same refusal is worded "Specified KMS key [<the request's KmsKeyId>] does not exist, is not enabled or you do not have permissions to access it." The leading fixed text is wrong ("Specified KMS key" instead of "The target KMS key"); the remainder of the sentence and the interpolated key reference are correct. This is the wording AWS uses for CopyDBClusterSnapshot, and the two copy paths share one message builder: services/service-com-amazonaws-rds-amazonrdsv19/src/state/export_tasks.rs:211 `copy_kms_key_not_accessible_message`, called from services/service-com-amazonaws-rds-amazonrdsv19/src/ops/copy_db_snapshot.rs:287. The sibling CREATE path is distinct and correct (`kms_key_not_accessible_message`, export_tasks.rs:194, "The specified KMS key [...]"), which is why CreateDBInstance(StorageEncrypted=True) under the same disabled CMK passes its message assertion in the same check. Related: the fixed issue 20261117T122437Z-kms-rds-copydbclustersnapshot-s-kmskeynotaccessiblefault-message-says-the.md established the 'Specified KMS key [...]' wording for the CLUSTER snapshot copy; generalising it to the one shared helper left the INSTANCE snapshot copy with the cluster path's sentence. That issue does not cover this operation.

  Why: a client that distinguishes a key-state failure from the other conditions KMSKeyNotAccessibleFault covers by matching RDS's refusal text — or that surfaces the sentence to an operator or logs/alarms on it — reads wording AWS never sends for CopyDBSnapshot, and cannot tell the instance-snapshot copy refusal apart from the cluster-snapshot one.

  Fix: the CopyDBSnapshot handler's KMSKeyNotAccessibleFault construction and the message builder it calls — the instance-snapshot copy path needs its own sentence rather than reusing the cluster copy path's, so each of the three wordings (create, instance-snapshot copy, cluster-snapshot copy) is stated once. Citations: services/service-com-amazonaws-rds-amazonrdsv19/src/ops/copy_db_snapshot.rs:283-294; services/service-com-amazonaws-rds-amazonrdsv19/src/state/export_tasks.rs:201-216.

  Traces: kms__rds.xsvc-rds-2-disabled-cmk-rejected-for-encrypted-create-and-snapshot-copy.

## Evidence
Check kms__rds.xsvc-rds-2-disabled-cmk-rejected-for-encrypted-create-and-snapshot-copy [mismatch].

The error code (KMSKeyNotAccessibleFault) and HTTP status (400) assertions pass, as does the assertion that DescribeDBSnapshots on the target reports DBSnapshotNotFound. The message assertion fails:

  AssertionError: expected the exact KMSKeyNotAccessibleFault message AWS returns, got 'Specified KMS key [arn:aws:kms:<region>:<account>:key/<key-id>] does not exist, is not enabled or you do not have permissions to access it.'

Expected (fixed text, with the request's KmsKeyId as the only variable part):
  'The target KMS key [<KmsKeyId as supplied>] does not exist, is not enabled or you do not have permissions to access it.'
Got:
  'Specified KMS key [<KmsKeyId as supplied>] does not exist, is not enabled or you do not have permissions to access it.'

Fixed text that diverges: the sentence opener — AWS 'The target KMS key [' vs the emulator's 'Specified KMS key ['. Everything after the closing bracket is identical. The region, account and key id inside the brackets are per-run values taken from the request; they are not part of the requirement.

In the SAME check, CreateDBInstance(StorageEncrypted=True, KmsKeyId=<same disabled CMK>) is refused with the correct message 'The specified KMS key [<arn>] does not exist, is not enabled or you do not have permissions to access it.', so only the copy path's sentence is wrong.

## Steps to Reproduce
1. kms.create_key(KeyUsage='ENCRYPT_DECRYPT') -> note KeyId and Arn.
2. Create a DB subnet group and an unencrypted DB instance (StorageEncrypted=False); wait for 'available'.
3. rds.create_db_snapshot(DBSnapshotIdentifier=<snap>, DBInstanceIdentifier=<instance>); wait for Status='available' and assert Encrypted is False.
4. kms.disable_key(KeyId=<key id>) and poll kms.describe_key until KeyMetadata.KeyState == 'Disabled'.
5. rds.copy_db_snapshot(SourceDBSnapshotIdentifier=<snap>, TargetDBSnapshotIdentifier=<copy>, KmsKeyId=<key arn>).
   Expect: HTTP 400, Error.Code 'KMSKeyNotAccessibleFault', Error.Message 'The target KMS key [<key arn>] does not exist, is not enabled or you do not have permissions to access it.'
   Actual: same status and code, message 'Specified KMS key [<key arn>] does not exist, is not enabled or you do not have permissions to access it.'
6. rds.describe_db_snapshots(DBSnapshotIdentifier=<copy>) -> DBSnapshotNotFound (already correct).
7. For contrast on the same wiring: rds.create_db_instance(StorageEncrypted=True, KmsKeyId=<key arn>, ...) -> 400 KMSKeyNotAccessibleFault with 'The specified KMS key [<key arn>] ...' (passes today), and rds.copy_db_cluster_snapshot(..., KmsKeyId=<key arn>) -> 'Specified KMS key [<key arn>] ...' (correct for the CLUSTER path).
8. kms.enable_key and repeat step 5 -> the copy must be accepted and reach 'available' encrypted under that key.
