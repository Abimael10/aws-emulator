Issue-Source: trace-sweep-divergence
Issue-Trace: rds-db-instance-core-lifecycle-and-features
Issue-Step: task#13
Issue-Severity: HIGH
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-db-instance-core-lifecycle-and-features rds-db-instance-target-error-paths rds-db-instance-snapshot-and-restore
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|DescribeDBInstanceAutomatedBackups|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|DescribeDBInstanceAutomatedBackups|2xx->2xx|$.DescribeDBInstanceAutomatedBackupsResponse.DescribeDBInstanceAutomatedBackupsResult.DBInstanceAutomatedBackups.DBInstanceAutomatedBackup.DBInstanceAutomatedBackupsArn
Issue-Key: rds|DescribeDBInstanceAutomatedBackups|2xx->2xx|$.DescribeDBInstanceAutomatedBackupsResponse.DescribeDBInstanceAutomatedBackupsResult.DBInstanceAutomatedBackups.DBInstanceAutomatedBackup.RestoreWindow
Issue-Key: rds|DescribeDBInstanceAutomatedBackups|2xx->2xx|$.DescribeDBInstanceAutomatedBackupsResponse.DescribeDBInstanceAutomatedBackupsResult.DBInstanceAutomatedBackups.DBInstanceAutomatedBackup.StorageThroughput
Issue-Key: rds|DescribeDBInstanceAutomatedBackups|2xx->2xx|$.DescribeDBInstanceAutomatedBackupsResponse.DescribeDBInstanceAutomatedBackupsResult.DBInstanceAutomatedBackups.DBInstanceAutomatedBackup.StorageType
Issue-Key: rds|DeleteDBInstanceAutomatedBackup|4xx->4xx|$.ErrorResponse.Error.Code
Issue-Key: rds|DeleteDBInstanceAutomatedBackup|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|DeleteDBInstanceAutomatedBackup|4xx->4xx|status
Issue-Commit: e146438f4a963fe449f9318754a4e29f075ac70d

# rds DB-instance automated-backup responses and refusals diverge from AWS (3 causes)

> _Note: this issue was identified against the code base at commit `e146438f4a963fe449f9318754a4e29f075ac70d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBInstanceAutomatedBackups

## Description
The rds DB-instance automated-backup surface (DescribeDBInstanceAutomatedBackups / DeleteDBInstanceAutomatedBackup) now answers, but its not-found message, its backup envelope and its parameter-format gate all diverge from AWS. 3 causes, all in the rds automated-backup path. The open issue 20261104T053605Z-rds-describedbinstanceautomatedbackups-stub.md ('Implement rds DescribeDBInstanceAutomatedBackups: unimplemented scaffold stub') does not cover these: the operation is implemented and serving, and that issue carries no replay key for these steps.

Cause 1 of 3: DescribeDBInstanceAutomatedBackups not-found message text.
AWS: an automated-backup lookup for a DbiResourceId/DBInstanceIdentifier that has no retained automated backup is refused with HTTP 404 and Error.Code DBInstanceAutomatedBackupNotFound (Smithy: DescribeDBInstanceAutomatedBackups errors = DBInstanceAutomatedBackupNotFoundFault, errorKind client, httpError 404, awsQueryError code DBInstanceAutomatedBackupNotFound). The message (recorded behavior only - the model carries no message template) is the fixed text 'DBInstanceAutomatedBackup for DBInstance ' + the DB-instance resource id taken from the request + ' not found.' - everything except that identifier is fixed text, and the identifier is the value the caller sent.
Emulator: serves the right status (404) and the right code, but its own sentence: 'Automated backups could not be found for: <resource-id>'. The fixed text is wrong in whole - different wording, different ordering, and no trailing 'not found.' clause.
Why: clients and SDK wrappers that surface or match on the error message (log scrapers, retry/branching logic, test assertions on AWS's documented wording) see text that never appears on AWS, so an error that is correctly classified is still reported wrongly to the user.
Fix: the DescribeDBInstanceAutomatedBackups handler's not-found path in the rds service crate - the message template for DBInstanceAutomatedBackupNotFound, not its status or code.
Traces: rds-db-instance-core-lifecycle-and-features#task#13 (representative), rds-db-instance-target-error-paths#task#10.

Cause 2 of 3: the DBInstanceAutomatedBackup envelope is not built from the source instance's configuration.
AWS: the automated-backup record mirrors 'the database instance properties that existed at the time you deleted the source instance' (Smithy: shape DBInstanceAutomatedBackup doc). Specifically StorageType is 'The storage type associated with the automated backup' - the source instance's storage type, so a gp2 source instance yields StorageType gp2; StorageThroughput is 'The storage throughput for the automated backup' and is 0 for a storage type that has no provisioned throughput (gp2); RestoreWindow ('The earliest and latest time a DB instance can be restored to', members EarliestTime/LatestTime) is omitted while the backup has no restorable window yet; and DBInstanceAutomatedBackupsArn is an auto-backup ARN whose resource id is the fixed prefix 'ab-' followed by a long (recording: 58-character) string of lowercase ASCII letters and digits.
Emulator: for the same source instance it reports StorageType 'gp3' where AWS reports the source's 'gp2', StorageThroughput 125 (a gp3 default) where AWS reports 0, emits a populated RestoreWindow whose EarliestTime and LatestTime are both the current clock instead of omitting it, and mints an 'ab-' resource id of 26 UPPERCASE characters instead of the long lowercase form. Only the format/derivation is the requirement here - the literal id and the timestamps in any one recording are per-run values and are not.
Why: a client that reads back an automated backup to learn how to restore gets storage attributes that contradict the instance it actually created (restoring with the reported gp3/125 throughput is not what the source had), is told a restore window exists when none does, and sees auto-backup ARNs whose casing/shape no AWS parser or ARN-matching policy accepts.
Fix: the automated-backup record builder in the rds service's state/model layer (the projection that copies instance storage attributes into DBInstanceAutomatedBackup) plus the auto-backup resource-id generator; RestoreWindow belongs in the same projection as a conditionally-serialized member.
Traces: rds-db-instance-snapshot-and-restore#task#9.

Cause 3 of 3: DeleteDBInstanceAutomatedBackup skips the DbiResourceId format gate and reports not-found instead.
AWS: DbiResourceId is validated for shape before any lookup, so a value that is not a DB resource id is refused with HTTP 400 and Error.Code InvalidParameterValue (recorded behavior only for this gate - Smithy models only DBInstanceAutomatedBackupNotFoundFault/404 and InvalidDBInstanceAutomatedBackupStateFault/400 on DeleteDBInstanceAutomatedBackup, so the parameter-format refusal comes from the service-common InvalidParameterValue class). The message is entirely fixed text, including the wire parameter name: 'The parameter dbi-resource-id is not a valid DbiResourceId. The DbiResourceId must begin with db- prefix; remaining part must contain only ASCII letters and digits; and must not be empty.' - no part of it is taken from the request.
Emulator: performs the lookup first and refuses with HTTP 404, Error.Code DBInstanceAutomatedBackupNotFound and message 'Automated backups could not be found for: <value>' - wrong status class (404 vs 400), wrong error code, and a message that echoes the bad value instead of stating the format rule.
Why: a 400 is a caller-input error and a 404 is a missing-resource error, so a client cannot tell a typo'd/wrongly-formatted id from a backup that genuinely does not exist; retry-on-404 and resource-reconciliation logic will keep polling for a resource that can never exist, and the operator never sees the format rule AWS states.
Fix: input validation for DeleteDBInstanceAutomatedBackup (the DbiResourceId format check must run before state lookup) in the rds service crate.
Traces: rds-db-instance-snapshot-and-restore#task#10.

## Evidence
Replay of trace set simparity-wire-aws-v1 (80 divergence clusters; 77 already owned by open issues, these 3 net-new).

Cause 1 - fixture rds-db-instance-core-lifecycle-and-features step task#13, operation DescribeDBInstanceAutomatedBackups, verdict mismatch, status 404 -> 404:
  $.ErrorResponse.Error.Message: value_mismatch
    expected: 'DBInstanceAutomatedBackup for DBInstance db-FB3E8C9BE05F4898877F8D8AB8 not found.'
    actual:   'Automated backups could not be found for: db-FB3E8C9BE05F4898877F8D8AB8'
  affected (2): rds-db-instance-core-lifecycle-and-features#task#13, rds-db-instance-target-error-paths#task#10

Cause 2 - fixture rds-db-instance-snapshot-and-restore step task#9, operation DescribeDBInstanceAutomatedBackups, verdict mismatch, status 200 -> 200:
  ...DBInstanceAutomatedBackup.StorageType: value_mismatch (expected 'gp2', got 'gp3')
  ...DBInstanceAutomatedBackup.StorageThroughput: value_mismatch (expected '0', got '125')
  ...DBInstanceAutomatedBackup.RestoreWindow: type_mismatch (expected '', got {'EarliestTime': ..., 'LatestTime': ...} with both set to the same current instant)
  ...DBInstanceAutomatedBackup.DBInstanceAutomatedBackupsArn: value_mismatch - AWS resource id 'ab-' + 58 lowercase letters/digits; emulator 'ab-' + 26 uppercase characters (per-run id values themselves are not the requirement, the charset/length form is)

Cause 3 - fixture rds-db-instance-snapshot-and-restore step task#10, operation DeleteDBInstanceAutomatedBackup, verdict mismatch, status 400 -> 404:
  status: expected 400, got 404
  $.ErrorResponse.Error.Code: value_mismatch (expected 'InvalidParameterValue', got 'DBInstanceAutomatedBackupNotFound')
  $.ErrorResponse.Error.Message: value_mismatch
    expected: 'The parameter dbi-resource-id is not a valid DbiResourceId. The DbiResourceId must begin with db- prefix; remaining part must contain only ASCII letters and digits; and must not be empty.'
    actual:   'Automated backups could not be found for: nonexistent-resource-id'

Model citations: Smithy DescribeDBInstanceAutomatedBackups errors = [DBInstanceAutomatedBackupNotFoundFault httpError 404, awsQueryError DBInstanceAutomatedBackupNotFound]; DeleteDBInstanceAutomatedBackup errors = [DBInstanceAutomatedBackupNotFoundFault 404, InvalidDBInstanceAutomatedBackupStateFault 400]; shape DBInstanceAutomatedBackup members StorageType/StorageThroughput/RestoreWindow/DBInstanceAutomatedBackupsArn; shape RestoreWindow members EarliestTime/LatestTime.

## Steps to Reproduce
Cause 1: call DescribeDBInstanceAutomatedBackups (Query protocol, rds) with DbiResourceId set to a db- resource id that has no retained automated backup. Observe 404 / DBInstanceAutomatedBackupNotFound with message 'Automated backups could not be found for: <id>' instead of 'DBInstanceAutomatedBackup for DBInstance <id> not found.'. Replay fixture rds-db-instance-core-lifecycle-and-features step task#13 (also rds-db-instance-target-error-paths step task#10).

Cause 2: create a DB instance with StorageType gp2, take/retain its automated backup, then call DescribeDBInstanceAutomatedBackups for it. Observe StorageType gp3, StorageThroughput 125, a populated RestoreWindow with EarliestTime == LatestTime, and an 'ab-' resource id of 26 uppercase characters. Replay fixture rds-db-instance-snapshot-and-restore step task#9.

Cause 3: call DeleteDBInstanceAutomatedBackup with DbiResourceId='nonexistent-resource-id' (no db- prefix). Observe 404 DBInstanceAutomatedBackupNotFound instead of 400 InvalidParameterValue with the dbi-resource-id format message. Replay fixture rds-db-instance-snapshot-and-restore step task#10.
