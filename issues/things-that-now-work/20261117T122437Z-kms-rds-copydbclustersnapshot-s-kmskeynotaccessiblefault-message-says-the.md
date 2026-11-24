Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: kms__rds.copy-db-cluster-snapshot-disabled-cmk-rejected-then-enabled-succeeds
Issue-Key: kms__rds.copy-db-cluster-snapshot-disabled-cmk-rejected-then-enabled-succeeds|run#29|AssertionError|assert message == expected_msg, f"refusal message was {message!r}"
Issue-Commit: 745763357b0410355460567821d75d30e5935144

# kms__rds: CopyDBClusterSnapshot's KMSKeyNotAccessibleFault message says 'The target KMS key [...]' where AWS says 'Specified KMS key [...]'

> _Note: this issue was identified against the code base at commit `745763357b0410355460567821d75d30e5935144`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CopyDBClusterSnapshot

## Description
Cause 1 of 1: the KMS-inaccessible refusal message for CopyDBClusterSnapshot is emulator-authored.
  AWS: recorded behavior only — copying a DB cluster snapshot into an encrypted copy under a CMK that is disabled (or absent, or not permitted) is refused with KMSKeyNotAccessibleFault and the message 'Specified KMS key [<kmsKeyId as supplied>] does not exist, is not enabled or you do not have permissions to access it.' — fixed text 'Specified KMS key [' / '] does not exist, is not enabled or you do not have permissions to access it.' with the key ARN taken from the request. No target snapshot is left behind, and the identical call succeeds once the key is re-enabled.
  Emulator: the right code is returned, but the message is 'The target KMS key [<key arn>] does not exist, is not enabled or you do not have permissions to access it.' — the leading fixed text differs ('The target KMS key' instead of 'Specified KMS key').
  Why: a client that matches RDS's refusal text (to distinguish a key-state failure from other KMSKeyNotAccessibleFault causes) or surfaces it to an operator reads wording AWS never sends.
  Fix: the CopyDBClusterSnapshot handler's KMS-key refusal message (error construction for KMSKeyNotAccessibleFault).
  Traces: kms__rds.copy-db-cluster-snapshot-disabled-cmk-rejected-then-enabled-succeeds.

## Evidence
copy-db-cluster-snapshot-disabled-cmk-rejected-then-enabled-succeeds — code KMSKeyNotAccessibleFault matches; AssertionError: refusal message was 'The target KMS key [arn:aws:kms:us-west-2:<acct>:key/<id>] does not exist, is not enabled or you do not have permissions to access it.' Expected 'Specified KMS key [arn:aws:kms:us-west-2:<acct>:key/<id>] does not exist, is not enabled or you do not have permissions to access it.'

## Steps to Reproduce
1. Create a DB cluster and a cluster snapshot; create a CMK and kms.disable_key on it.
2. rds.copy_db_cluster_snapshot(SourceDBClusterSnapshotIdentifier=<snap>, TargetDBClusterSnapshotIdentifier=<copy>, KmsKeyId=<that key arn>) -> expect KMSKeyNotAccessibleFault with message 'Specified KMS key [<arn>] does not exist, is not enabled or you do not have permissions to access it.' and DescribeDBClusterSnapshots(<copy>) -> DBClusterSnapshotNotFoundFault.
3. kms.enable_key and repeat -> expect the copy to succeed, encrypted under that key.
