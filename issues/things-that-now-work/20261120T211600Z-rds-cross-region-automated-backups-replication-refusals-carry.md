Issue-Source: trace-sweep-divergence
Issue-Trace: rds-automated-backups-replication-errors
Issue-Step: task#1
Issue-Severity: MEDIUM
Issue-Corpus: simparity-wire-aws-v1
Issue-Corpus-Kind: wire
Issue-Trace-Ids: rds-automated-backups-replication-errors rds-read-replica-and-instance-promotion
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds|StartDBInstanceAutomatedBackupsReplication|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Key: rds|StopDBInstanceAutomatedBackupsReplication|4xx->4xx|$.ErrorResponse.Error.Message
Issue-Commit: f85f98f2ce22a299384412ddd2d0ac00fcbe668d

# rds cross-region automated-backups-replication refusals carry the generic DBInstanceNotFound sentence instead of AWS's source-region-qualified wording

> _Note: this issue was identified against the code base at commit `f85f98f2ce22a299384412ddd2d0ac00fcbe668d`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartDBInstanceAutomatedBackupsReplication

## Description
Both cross-region DB-instance automated-backups-replication operations address their subject by an ARN (SourceDBInstanceArn), so when the instance named by that ARN does not exist AWS words the refusal in terms of the ARN's two components - the instance identifier and the SOURCE REGION the ARN points at. The emulator instead reuses the generic, region-less DBInstance-not-found sentence it serves from its shared instance lookup, so the source region never appears. Status (404) and error code (DBInstanceNotFound) are correct in both cases; only the message text diverges. 2 causes, both in the rds automated-backups-replication path, and a fixer must close both before submitting.

Cause 1 of 2: StartDBInstanceAutomatedBackupsReplication's DBInstanceNotFound message.
AWS: Smithy StartDBInstanceAutomatedBackupsReplication input = {SourceDBInstanceArn (required), BackupRetentionPeriod, KmsKeyId, PreSignedUrl, Tags}; modeled errors include DBInstanceNotFoundFault (errorKind client, httpError 404, awsQueryError code DBInstanceNotFound, httpResponseCode 404), documented "DBInstanceIdentifier doesn't refer to an existing DB instance." The model carries no message template, so the sentence is recorded behavior only: the fixed text is "Requested DB Instance " + " not found in source region ", and it has exactly TWO variable parts, both derived from the request's SourceDBInstanceArn - the DB instance identifier (the ARN's resource id) and the region segment of that ARN. Nothing in the message comes from emulator state.
Emulator: answers 404 with Error.Code DBInstanceNotFound (correct) but Error.Message "DBInstance <identifier> not found." - the generic instance-not-found sentence. The fixed text is wrong in whole (different frame, no "Requested" prefix, trailing period) and the source-region variable part is absent entirely.
Why: this is the only signal a caller gets about WHICH region the replication source was looked up in. A client setting up cross-region backup replication cannot distinguish "the ARN names an instance that does not exist" from "the ARN names the wrong region", so operators retry against the same wrong region, and message-keyed branching/alarming on AWS's documented wording never matches. Parsers that lift the region out of AWS's sentence find nothing.
Fix: the DBInstanceNotFound refusal message in the StartDBInstanceAutomatedBackupsReplication handler's ARN resolution/validation path (services/service-com-amazonaws-rds-amazonrdsv19/src/ops/) - it must format its own region-qualified template from the parsed SourceDBInstanceArn rather than delegating to the shared generic instance-not-found message. Status and code need no change.
Traces: rds-automated-backups-replication-errors#task#1 (representative), rds-read-replica-and-instance-promotion#task#7.

Cause 2 of 2: StopDBInstanceAutomatedBackupsReplication's DBInstanceNotFound message.
AWS: Smithy StopDBInstanceAutomatedBackupsReplication input = {SourceDBInstanceArn (required)}; modeled errors DBInstanceNotFoundFault (client, httpError 404, awsQueryError code DBInstanceNotFound, httpResponseCode 404) and InvalidDBInstanceStateFault (400). Same recorded sentence as Cause 1: fixed text "Requested DB Instance " + " not found in source region ", with the instance identifier and the region both taken from the request's SourceDBInstanceArn.
Emulator: 404 / DBInstanceNotFound (correct) with "DBInstance <identifier> not found." - the same generic sentence, again dropping the source region.
Why: as Cause 1. A teardown path that stops replication for an ARN it holds is told only that an instance is missing, never that it was searched for in a particular source region, so reconciliation loops cannot tell a stale ARN from a wrong-region ARN.
Fix: the DBInstanceNotFound refusal message in the StopDBInstanceAutomatedBackupsReplication handler's ARN resolution path (same crate). This is a SEPARATE handler from Cause 1 - fixing one does not fix the other, though both should use one shared region-qualified template.
Traces: rds-automated-backups-replication-errors#task#2 (representative), rds-read-replica-and-instance-promotion#task#8.

Related but NOT covering: open issue 20261119T013917Z-rds-db-instance-automated-backup-responses-and-refusals.md and open issue 20261119T043432Z-rds-5-net-new-behavioral-causes-two-automated.md both concern automated-backup message wording, but for DescribeDBInstanceAutomatedBackups / DeleteDBInstanceAutomatedBackup; neither mentions the two *AutomatedBackupsReplication operations and neither carries a replay key for these steps (the replay report shows these clusters unowned).

The identifier and region quoted in the evidence below are the per-run values that fill the template; only the fixed text and the two ARN-derived variable parts named above are requirements.

## Evidence
Replay of trace set simparity-wire-aws-v1 (81 divergence clusters; 79 already owned by open issues or blocked by an unimplemented-operation stub, these 2 net-new).

Cause 1 - fixture rds-automated-backups-replication-errors step task#1, operation StartDBInstanceAutomatedBackupsReplication, verdict mismatch, status 404 -> 404:
  $.ErrorResponse.Error.Message: value_mismatch
    expected: 'Requested DB Instance nonexistent-db-probe-x9z not found in source region us-east-1'
    actual:   'DBInstance nonexistent-db-probe-x9z not found.'
  Fixed text expected: 'Requested DB Instance ' ... ' not found in source region ' ...
  Variable parts: 'nonexistent-db-probe-x9z' = instance identifier from SourceDBInstanceArn; 'us-east-1' = region segment of SourceDBInstanceArn.
  affected (2): rds-automated-backups-replication-errors#task#1, rds-read-replica-and-instance-promotion#task#7
  status 404 and Error.Code DBInstanceNotFound both matched; the message is the only diverging key.

Cause 2 - fixture rds-automated-backups-replication-errors step task#2, operation StopDBInstanceAutomatedBackupsReplication, verdict mismatch, status 404 -> 404:
  $.ErrorResponse.Error.Message: value_mismatch
    expected: 'Requested DB Instance nonexistent-db-probe-x9z not found in source region us-east-1'
    actual:   'DBInstance nonexistent-db-probe-x9z not found.'
  affected (2): rds-automated-backups-replication-errors#task#2, rds-read-replica-and-instance-promotion#task#8
  status 404 and Error.Code DBInstanceNotFound both matched; the message is the only diverging key.

Model citations: Smithy StartDBInstanceAutomatedBackupsReplication errors = [DBInstanceAutomatedBackupQuotaExceededFault 400, DBInstanceNotFoundFault httpError 404 / awsQueryError DBInstanceNotFound, InvalidDBInstanceAutomatedBackupStateFault 400, InvalidDBInstanceStateFault 400, KMSKeyNotAccessibleFault 400, StorageTypeNotSupportedFault 400], input member SourceDBInstanceArn required. StopDBInstanceAutomatedBackupsReplication errors = [DBInstanceNotFoundFault httpError 404 / awsQueryError DBInstanceNotFound, InvalidDBInstanceStateFault 400], input member SourceDBInstanceArn required. Shape DBInstanceNotFoundFault doc: "DBInstanceIdentifier doesn't refer to an existing DB instance."

## Steps to Reproduce
All against the emulator's rds endpoint (Query protocol, service token 'rds', crate services/service-com-amazonaws-rds-amazonrdsv19).

Cause 1: call StartDBInstanceAutomatedBackupsReplication with SourceDBInstanceArn = 'arn:aws:rds:us-east-1:<account>:db:nonexistent-db-probe-x9z' (an instance that was never created, in a region other than the caller's). Expect HTTP 404, Error.Code DBInstanceNotFound and Error.Message exactly 'Requested DB Instance nonexistent-db-probe-x9z not found in source region us-east-1' (instance id and region taken from the ARN). Observe the generic 'DBInstance nonexistent-db-probe-x9z not found.' instead. Replay fixture rds-automated-backups-replication-errors step task#1 (also rds-read-replica-and-instance-promotion step task#7).

Cause 2: call StopDBInstanceAutomatedBackupsReplication with the same SourceDBInstanceArn. Expect the same 404 / DBInstanceNotFound and the same region-qualified sentence; observe the same generic sentence. Replay fixture rds-automated-backups-replication-errors step task#2 (also rds-read-replica-and-instance-promotion step task#8).
