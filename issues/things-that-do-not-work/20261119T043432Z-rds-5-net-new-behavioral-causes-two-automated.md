Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.describe-instance-automated-backups rds.delete-automated-backup-by-arn rds.e2e-certificate-rotation-consequence-flow rds.modify-apply-immediately-false-pending-modified-values rds.point-in-time-restore-data-query-round-trip
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.describe-instance-automated-backups|run#14|AssertionError|assert err.get("Message") == (
Issue-Key: rds.delete-automated-backup-by-arn|run#19|AssertionError|assert message == f"Automated backups could not be found for: {resource_id}", message
Issue-Key: rds.e2e-certificate-rotation-consequence-flow|run#20|AssertionError|assert instance["CACertificateIdentifier"] == original_ca, (
Issue-Key: rds.modify-apply-immediately-false-pending-modified-values|_poll#28|Timeout|return val
Issue-Key: rds.point-in-time-restore-data-query-round-trip|_poll#43|Timeout|time.sleep(interval_s)
Issue-Commit: 4e15b1526da23add786776ef4ff6bf33d2b8b2af

# rds: 5 net-new behavioral causes — two automated-backup not-found messages are emulator-authored, RebootDBInstance applies a pending CA change AWS leaves stuck, and the deferred-modify and point-in-time-restore flows never settle

> _Note: this issue was identified against the code base at commit `4e15b1526da23add786776ef4ff6bf33d2b8b2af`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBInstanceAutomatedBackups, DeleteDBInstanceAutomatedBackup, RebootDBInstance, ModifyDBInstance, DescribeDBInstances

## Description
Five net-new root causes in the emulated RDS control plane, each grounded in a failing behavioral check of this suite that the replay report marked neither ALREADY FILED (unimplemented scaffold) nor OWNED by an open issue. They are independent and each needs its own fix; a fixer must close all five before submitting.

Cause 1 of 5: DescribeDBInstanceAutomatedBackups' not-found refusal is reworded.
AWS: Smithy DescribeDBInstanceAutomatedBackups takes DbiResourceId / DBInstanceIdentifier / DBInstanceAutomatedBackupsArn and models exactly one error, DBInstanceAutomatedBackupNotFoundFault (awsQueryError code DBInstanceAutomatedBackupNotFound, httpResponseCode 404, doc "No automated backup for this DB instance was found."). When the call selects by DBInstanceIdentifier and no automated-backup record exists for that identifier, the message is "DBInstanceAutomatedBackup for DBInstance <DBInstanceIdentifier> not found." — the fixed text is "DBInstanceAutomatedBackup for DBInstance " … " not found." and its only variable part is the DBInstanceIdentifier taken from the request. The recording asserts this same wording twice: for an identifier that never existed, and for a real instance after it is deleted and its backups are gone.
Emulator: the error code (DBInstanceAutomatedBackupNotFound) and Type (Sender) are right, but the message is "Automated backups could not be found for: <DBInstanceIdentifier>" — different fixed text in a different frame.
Why: a client that classifies or alarms on this refusal by its message text sees an unrecognised refusal for a condition AWS words one way everywhere, and parsers that lift the identifier out of AWS's sentence find nothing.
Fix: the refusal message in the DescribeDBInstanceAutomatedBackups handler's lookup/validation (services/service-com-amazonaws-rds-amazonrdsv19/src/ops/describe_db_instance_automated_backups.rs). The rest of this check (an active backup record describable by DBInstanceIdentifier, Status 'active', and the record disappearing once the instance is deleted) already holds.
Traces: rds.describe-instance-automated-backups.

Cause 2 of 5: DeleteDBInstanceAutomatedBackup addressed by DBInstanceAutomatedBackupsArn echoes the ARN in the not-found message instead of naming the backup's DbiResourceId.
AWS: Smithy DeleteDBInstanceAutomatedBackup input is {DbiResourceId, DBInstanceAutomatedBackupsArn}; errors DBInstanceAutomatedBackupNotFoundFault (code DBInstanceAutomatedBackupNotFound, 404) and InvalidDBInstanceAutomatedBackupStateFault (400). Deleting an automated backup a second time — by the SAME DBInstanceAutomatedBackupsArn that the first, successful delete used — is refused 404 DBInstanceAutomatedBackupNotFound with "Automated backups could not be found for: <DbiResourceId>": the fixed text is "Automated backups could not be found for: " and its single variable part is the DbiResourceId of the instance the backup belongs to (resolved from the ARN), never the ARN string itself.
Emulator: right code and right fixed text, but the variable part is the request's DBInstanceAutomatedBackupsArn reflected verbatim (an "arn:aws:rds:<region>:<account>:auto-backup:ab-<id>" value), so the message names the input instead of the backup's identity.
Why: the DbiResourceId is the id a client already holds from DescribeDBInstances / DescribeDBInstanceAutomatedBackups, so message-keyed correlation and idempotent-delete handling (treat 'already gone for resource X' as success) fail; echoing the request ARN also hides which resource the server actually looked for.
Fix: the not-found message in the DeleteDBInstanceAutomatedBackup handler (…/src/ops/delete_db_instance_automated_backup.rs) — resolve the ARN to the backup's DbiResourceId and format that, on the ARN-addressed path as well as the DbiResourceId one.
Traces: rds.delete-automated-backup-by-arn (every earlier leg passed: the retained backup after DeleteAutomatedBackups=false, its DBInstanceIdentifier, the auto-backup ARN shape, and the successful delete by ARN).

Cause 3 of 5: RebootDBInstance applies a pending CACertificateIdentifier that must stay pending and unapplied.
AWS: recorded behavior only — Smithy models ModifyDBInstance.CACertificateIdentifier ("The CA certificate identifier to use for the DB instance's server certificate.") and PendingModifiedValues.CACertificateIdentifier, but states nothing about when a parked CA is applied. The recording shows: ModifyDBInstance(CACertificateIdentifier=<another RSA CA from DescribeCertificates>, ApplyImmediately=false) on an available default-version postgres instance parks the value — the effective CACertificateIdentifier is unchanged and PendingModifiedValues.CACertificateIdentifier names the requested CA — and a plain RebootDBInstance (no ForceFailover) then returns the instance to 'available' with the ORIGINAL CA still effective and the pending CACertificateIdentifier still present, because that CA is not assignable on this engine/version; the rotation stays stuck rather than being applied by the reboot.
Emulator: the reboot is accepted (so this is no longer the InvalidDBInstanceState refusal that open issue 20261115T033800Z-rds-8-net-new-behavioral-causes-five-refusal.md describes as its Cause 6 — the replay now gets past the reboot), but it APPLIES the parked value: after the reboot DescribeDBInstances reports the pending CA as the effective CACertificateIdentifier.
Why: a client that rotates a CA and reboots is told the rotation succeeded where real AWS leaves the instance on the old CA, so CA-drift and compliance reporting concludes a fleet is rotated when it is not, and the pending entry that is the only signal of a stuck rotation is cleared.
Fix: the pending-value application path of RebootDBInstance (…/src/ops/reboot_db_instance.rs) — apply a pending CACertificateIdentifier only when that CA is assignable to the instance's engine and engine version; otherwise leave the effective CA and the pending entry untouched.
Traces: rds.e2e-certificate-rotation-consequence-flow.

Cause 4 of 5: the deferred ModifyDBInstance (ApplyImmediately=false) lifecycle never settles — the check exhausts the replay budget instead of failing an assertion.
AWS: Smithy ModifyDBInstance takes ApplyImmediately and returns DBInstance with PendingModifiedValues (PendingModifiedValues.DBInstanceClass: "The name of the compute and memory capacity class for the DB instance."). Recorded behavior: with ApplyImmediately=false an outage-causing DBInstanceClass change is parked (live class unchanged, DBInstanceStatus stays 'available'); a no-outage attribute (CopyTagsToSnapshot) is applied at once and never appears in PendingModifiedValues; and re-requesting the CURRENT class WITHDRAWS the queued change, so PendingModifiedValues no longer carries DBInstanceClass and the instance is never resized.
Emulator: the replay reports "Check timed out after 60.0s". Every wait in this check is a poll that returns the moment the emulator reports the expected value, so exhausting the budget means one of the polled conditions is never reached. Hypothesis: the withdraw step — ModifyDBInstance(DBInstanceClass=<the live class>, ApplyImmediately=false) — leaves the no-op change queued, so the poll for "DBInstanceClass absent from PendingModifiedValues" never succeeds; the other candidate is CopyTagsToSnapshot not being applied under ApplyImmediately=false (its poll waits for the live value to become true).
Why: clearing PendingModifiedValues is the standard 'the modify has landed / was withdrawn' probe, so a client's settle loop never returns and deferred-modify automation hangs rather than erring. A queued no-op class change also means a maintenance window would resize an instance the operator explicitly put back.
Fix: the ApplyImmediately=false branch of the ModifyDBInstance handler and the PendingModifiedValues it writes (…/src/ops/modify_db_instance.rs) — drop a pending entry whose requested value equals the live value, and apply no-outage attributes immediately instead of parking them.
Traces: rds.modify-apply-immediately-false-pending-modified-values. (This check id appears in the Issue-Trace-Ids of the open issue named in Cause 3, but its replay key there is the status-'modifying' assertion, which the replay no longer reaches; that issue does not cover this stall.)

Cause 5 of 5: the point-in-time-restore round trip never progresses — LatestRestorableTime.
AWS: Smithy DBInstance.LatestRestorableTime — "The latest time to which a database in this DB instance can be restored with point-in-time restore." On an instance created with a non-zero BackupRetentionPeriod, DescribeDBInstances publishes LatestRestorableTime and it advances past a committed write, which is the precondition for RestoreDBInstanceToPointInTime; the check's later legs then require the window's lower and upper bounds to be refused with InvalidParameterValue ("The specified instance cannot be restored to a time earlier than <lower bound> because its backup retention period is set to <n> days." / "The specified instance cannot be restored to a time later than <LatestRestorableTime>."), the RestoreTime/UseLatestRestorableTime combination to be refused ("If UseLatestRestoreTime is not true, the RestoreTime parameter must be specified." / "If UseLatestRestoreTime is true, the RestoreTime parameter must be omitted."), and the restored instance to hold the rows committed before the window.
Emulator: the replay reports "Check timed out after 60.0s". Hypothesis: the first long poll — DescribeDBInstances reporting a LatestRestorableTime at or after the committed write — is never satisfied, so the check never reaches any restore call; a stalled SQL session to the source instance is the alternative candidate.
Why: without a published, advancing LatestRestorableTime a client cannot determine the recovery window at all, so every point-in-time recovery path is unusable and a wait-for-window loop never returns.
Fix: LatestRestorableTime in the DB instance state and the DescribeDBInstances serializer (it must be published for any instance with BackupRetentionPeriod >= 1 and advance with committed writes), and then the window-bound validation plus the real restore body of RestoreDBInstanceToPointInTime. Open issue 20261109T135215Z-rds-restoredbinstancetopointintime-stub.md records RestoreDBInstanceToPointInTime as an unimplemented scaffold answering 501 — this stall is UPSTREAM of that call, so implementing the stub alone will not make this check pass, and this issue is not covered by it.
Traces: rds.point-in-time-restore-data-query-round-trip.

Identifiers, ARNs, account ids, hostnames, CA identifiers and timestamps quoted anywhere below are per-run values that fill the templates; only the fixed text and the request/state-derived variable parts named above are requirements.

## Evidence
Cause 1 — rds.describe-instance-automated-backups (check line 114):
  DescribeDBInstanceAutomatedBackups(DBInstanceIdentifier=<never-created id>)
  expected: DBInstanceAutomatedBackupNotFound / "DBInstanceAutomatedBackup for DBInstance <id> not found."
  actual:   {'Type': 'Sender', 'Code': 'DBInstanceAutomatedBackupNotFound', 'Message': 'Automated backups could not be found for: sim2real-behav-dbi-missing-149a424cac6e'}
  (AssertionError: unexpected DBInstanceAutomatedBackupNotFound message: ...)
  The same message is asserted again on the post-delete leg of the check.

Cause 2 — rds.delete-automated-backup-by-arn (check line 131):
  DeleteDBInstanceAutomatedBackup(DBInstanceAutomatedBackupsArn=<arn already deleted by the previous call>)
  expected: DBInstanceAutomatedBackupNotFound / "Automated backups could not be found for: <DbiResourceId of the source instance>"
  actual:   DBInstanceAutomatedBackupNotFound / 'Automated backups could not be found for: arn:aws:rds:us-west-2:000000000000:auto-backup:ab-A8956UWTANNX9357EY8AD2UOF1'
  (AssertionError at `assert message == f"Automated backups could not be found for: {resource_id}"`, where resource_id is the instance's DbiResourceId; the error code assertion passed)

Cause 3 — rds.e2e-certificate-rotation-consequence-flow (check line 79):
  ModifyDBInstance(CACertificateIdentifier='rds-ca-rsa4096-g1', ApplyImmediately=False) -> parked correctly
  RebootDBInstance -> instance back to 'available'
  expected: effective CACertificateIdentifier still 'rds-ca-rsa2048-g1' with PendingModifiedValues.CACertificateIdentifier still 'rds-ca-rsa4096-g1'
  actual:   effective CACertificateIdentifier 'rds-ca-rsa4096-g1'
  (AssertionError: effective CA after reboot was 'rds-ca-rsa4096-g1', expected the reboot to leave the unsupported pending CA 'rds-ca-rsa4096-g1' unapplied and stay on 'rds-ca-rsa2048-g1')

Cause 4 — rds.modify-apply-immediately-false-pending-modified-values:
  verdict mismatch, detail "Error: Check timed out after 60.0s". The check's waits are polls with no fixed sleeps
  (pending-class poll 300 s, CopyTagsToSnapshot poll 300 s, withdraw poll 600 s), so the budget is only exhausted
  when a polled condition never becomes true; the suspected one is `"DBInstanceClass" not in PendingModifiedValues`
  after ModifyDBInstance(DBInstanceClass=<the live class>, ApplyImmediately=False).

Cause 5 — rds.point-in-time-restore-data-query-round-trip:
  verdict mismatch, detail "Error: Check timed out after 60.0s". The check writes one row over SQL, then polls
  `(_instance(rds, src_id) or {}).get("LatestRestorableTime")` until it is non-null and >= the write time
  (timeout 2400 s, interval 30 s) before issuing any RestoreDBInstanceToPointInTime call; the budget is spent at or
  before that poll.

## Steps to Reproduce
All against the emulator's rds endpoint (service token 'rds', crate services/service-com-amazonaws-rds-amazonrdsv19).

Cause 1: DescribeDBInstanceAutomatedBackups(DBInstanceIdentifier='does-not-exist'). Expect 404 DBInstanceAutomatedBackupNotFound with exactly "DBInstanceAutomatedBackup for DBInstance does-not-exist not found.". Repeat after CreateDBInstance(mysql, BackupRetentionPeriod=1) -> wait available -> DeleteDBInstance(SkipFinalSnapshot=true) and the backups have gone: same message with that instance's identifier.

Cause 2: CreateDBInstance(Engine=mysql, BackupRetentionPeriod=1) -> wait available -> note DbiResourceId -> DeleteDBInstance(SkipFinalSnapshot=true, DeleteAutomatedBackups=false) -> wait until the instance is gone -> DescribeDBInstanceAutomatedBackups(DbiResourceId=<id>) and take DBInstanceAutomatedBackupsArn -> DeleteDBInstanceAutomatedBackup(DBInstanceAutomatedBackupsArn=<arn>) (succeeds) -> call it AGAIN with the same ARN. The 404 message must name the DbiResourceId, not the ARN.

Cause 3: CreateDBInstance(Engine=postgres, db.t3.micro, default engine version) -> wait available -> note CACertificateIdentifier -> DescribeCertificates and pick another CertificateType='CA' identifier containing 'rsa' -> ModifyDBInstance(CACertificateIdentifier=<that CA>, ApplyImmediately=false) -> confirm the effective CA is unchanged and PendingModifiedValues.CACertificateIdentifier names it -> RebootDBInstance -> wait available -> DescribeDBInstances: the effective CACertificateIdentifier must still be the original and the pending entry must still name the new CA.

Cause 4: CreateDBInstance(Engine=postgres, DBInstanceClass=db.t3.micro, CopyTagsToSnapshot=false, BackupRetentionPeriod=0) -> wait available -> ModifyDBInstance(DBInstanceClass=db.t3.small, ApplyImmediately=false) (pending set, live class unchanged, status 'available') -> ModifyDBInstance(CopyTagsToSnapshot=true, ApplyImmediately=false): the live CopyTagsToSnapshot must become true and must NOT appear in PendingModifiedValues, while the pending DBInstanceClass survives -> ModifyDBInstance(DBInstanceClass=db.t3.micro, ApplyImmediately=false): PendingModifiedValues must stop carrying DBInstanceClass and the instance must stay available on db.t3.micro.

Cause 5: CreateDBInstance(Engine=postgres, BackupRetentionPeriod>=1) -> wait available -> insert a row over SQL -> poll DescribeDBInstances for LatestRestorableTime: it must be present and at or after the write. Then RestoreDBInstanceToPointInTime with a RestoreTime two days earlier (InvalidParameterValue naming the lower bound and the retention period), with a RestoreTime six hours in the future (InvalidParameterValue naming LatestRestorableTime), with neither and with both of RestoreTime/UseLatestRestorableTime, and finally with UseLatestRestorableTime=true: the restored instance must hold the row committed before the window.
