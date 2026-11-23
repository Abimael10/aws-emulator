Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.cross-region-snapshot-copy-kms-restore-round-trip rds.automated-backup-arn-param-malformed-rejected rds.restore-instance-mismatched-engine-version-rejected rds.modify-instance-pending-engine-version-target-queued-and-withdrawn rds.serverless-v2-open-connection-blocks-auto-pause rds.storage-autoscaling-triggers-on-usage-threshold rds.time-zone-parameter-changes-now-offset-in-new-session
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.cross-region-snapshot-copy-kms-restore-round-trip|run#36|AssertionError|assert east_copy["KmsKeyId"] == key_e_arn, (
Issue-Key: rds.automated-backup-arn-param-malformed-rejected|run#9|AssertionError|assert describe_message == (
Issue-Key: rds.restore-instance-mismatched-engine-version-rejected|run#18|AssertionError|assert message == expected, f"unexpected error message: {message!r} != {expected!r}"
Issue-Key: rds|ModifyDBInstance|InvalidParameterCombination
Issue-Key: rds.serverless-v2-open-connection-blocks-auto-pause|_sql_rows#18|OperationalError|cur.execute(statement)
Issue-Key: rds.storage-autoscaling-triggers-on-usage-threshold|_fill_storage_with_retry#20|Timeout|time.sleep(15)
Issue-Key: rds.time-zone-parameter-changes-now-offset-in-new-session|_poll#16|Timeout|val = fn()
Issue-Commit: 616e510b6c9fc5a742ae081415d2f4cf49efcb62

# rds: 7 net-new causes — a copied snapshot's KmsKeyId is echoed as a bare key id not the key ARN, two refusal messages diverge from AWS wording, postgres 14.20 is missing from the engine-version catalogue, and three data-plane checks never reach a verdict (Aurora Serverless v2 session dropped at TLS, storage autoscaling and the time-zone parameter)

> _Note: this issue was identified against the code base at commit `616e510b6c9fc5a742ae081415d2f4cf49efcb62`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CopyDBSnapshot

## Description
Seven net-new root causes in the rds service, none of them marked ALREADY FILED or OWNED by the replay report.

=== Cause 1 of 7: CopyDBSnapshot echoes the KmsKeyId it was given verbatim instead of resolving it to the key ARN ===
AWS: Smithy `DBSnapshot$KmsKeyId` — "If Encrypted is true, the Amazon Web Services KMS key identifier for the encrypted DB snapshot. The Amazon Web Services KMS key identifier is the key ARN, key ID, alias ARN, or alias name for the KMS key." `CopyDBSnapshot$KmsKeyId` accepts any of those four forms on input, and the resulting DBSnapshot reports the resolved KEY ARN (`arn:aws:kms:<region>:<account>:key/<key-id>`) regardless of which form was supplied — that is the only form that is unambiguous across regions, which matters precisely for a cross-region copy.
Emulator: whatever string the request carried is stored and reported unchanged. A CopyDBSnapshot given `KmsKeyId=<bare key id>` reports `KmsKeyId` as that bare key id, so the response does not name the region or account the key lives in.
Why: a client doing a cross-region copy cannot tell which region's key encrypted which copy — the whole point of the round trip — and any comparison against the ARN returned by kms:DescribeKey fails.
Fix: the `CopyDBSnapshot` handler (and the DB-snapshot state it writes): resolve the supplied key identifier to its key ARN in the destination region before recording it, the same way the create path must.
Traces: rds.cross-region-snapshot-copy-kms-restore-round-trip.

=== Cause 2 of 7: DescribeDBInstanceAutomatedBackups' malformed-ARN message names the whole ARN where AWS names only the ARN's resource-id segment ===
AWS: both DeleteDBInstanceAutomatedBackup and DescribeDBInstanceAutomatedBackups refuse a `DBInstanceAutomatedBackupsArn` that is not a valid automated-backup ARN with HTTP 400 `InvalidParameterValue`. The fixed text is "The provided ARN (<value>) is invalid for this parameter (DbInstanceAutomatedBackupsArn)." — note the member is spelled `DbInstanceAutomatedBackupsArn` in the message. The one variable part differs between the two operations: DeleteDBInstanceAutomatedBackup interpolates the FULL ARN as the caller sent it, while DescribeDBInstanceAutomatedBackups interpolates only the ARN's trailing resource-id segment (the `ab-...` identifier). The emulator already gets the delete side right, which is what isolates this to the describe handler.
Emulator: DescribeDBInstanceAutomatedBackups interpolates the full ARN, i.e. "The provided ARN (arn:aws:rds:<region>:<account>:auto-backup:<ab-id>) is invalid for this parameter (DbInstanceAutomatedBackupsArn)." where AWS says "The provided ARN (<ab-id>) is invalid for this parameter (DbInstanceAutomatedBackupsArn)."
Why: a client that parses the offending value out of the message (or that pins the refusal text) reads a different value from the one AWS reports for this operation.
Fix: the `DescribeDBInstanceAutomatedBackups` handler's ARN validation — interpolate the resource-id segment, not the whole ARN.
Traces: rds.automated-backup-arn-param-malformed-rejected.

=== Cause 3 of 7: RestoreDBInstanceFromDBSnapshot's engine-mismatch refusal is emulator-authored wording ===
AWS: restoring a snapshot under an Engine incompatible with the snapshot's own engine is refused with `InvalidParameterCombination` whose message is the fixed sentence "Requested DBSnapshot and Engine are incompatible." — it has no variable parts.
Emulator: the same call is refused with the emulator's own sentence "The engine postgres isn't compatible with the engine of the source DB snapshot (mysql).", which interpolates the requested engine and the snapshot's engine. AWS does not name either engine in this refusal.
Why: a client matching on the documented refusal text does not match; the two engine names make it look like a different, parameterised error class.
Fix: the `RestoreDBInstanceFromDBSnapshot` handler's engine-compatibility validation — emit AWS's fixed message.
Traces: rds.restore-instance-mismatched-engine-version-rejected.

=== Cause 4 of 7: the postgres engine-version catalogue is missing the major-version target, so a queued major upgrade cannot be requested at all ===
AWS: `ModifyDBInstance` with an `EngineVersion` naming a supported postgres major version plus `AllowMajorVersionUpgrade=true` is ACCEPTED: it queues the target in `PendingModifiedValues.EngineVersion` while the live `EngineVersion` stays at the current version, and resubmitting the current version withdraws the pending change. A version is rejected `InvalidParameterCombination` "Cannot find version <v> for <engine>" only when that version is not offered for the engine.
Emulator: a postgres instance created at 13.23 (so 13.23 IS in the catalogue) is refused on the major-version modify: `ClientError (InvalidParameterCombination) when calling the ModifyDBInstance operation: Cannot find version 14.20 for postgres`. The postgres entry of the engine-version catalogue carries no 14.x build, so the queue-then-withdraw behaviour behind it is unreachable.
Why: no client can perform or rehearse a postgres major-version upgrade, and DescribeDBEngineVersions misreports which versions the service offers.
Fix: the engine-version catalogue the RDS crate validates `EngineVersion` against (and `DescribeDBEngineVersions` reports from) must offer the postgres major versions AWS offers, with their valid upgrade targets; the `ModifyDBInstance` pending-engine-version path is then exercisable.
Traces: rds.modify-instance-pending-engine-version-target-queued-and-withdrawn.

=== Cause 5 of 7: an Aurora Serverless v2 PostgreSQL session is dropped mid-statement — the TLS stream closes without a close_notify ===
AWS: an Aurora Serverless v2 PostgreSQL cluster accepts a session at its endpoint and holds it open; while a connection is open the cluster does not auto-pause, so a statement issued after the `SecondsUntilAutoPause` floor (300 s) has elapsed still succeeds on that same session.
Emulator: the session is established but a later `cur.execute(...)` fails with `psycopg.OperationalError: consuming input failed: SSL error: unexpected eof while reading` — the emulator closed the TLS stream under the open connection instead of serving the statement. Hypothesis: the serving side tears the connection down on its own idle/pause timer rather than treating an open connection as blocking auto-pause.
Why: a client that keeps a pooled connection open loses it without a protocol-level close, and the documented "an open connection blocks auto-pause" guarantee is inverted.
Fix: the Aurora Serverless v2 cluster's PostgreSQL data-plane session handling and the auto-pause state that governs it — an open session must keep the cluster resumed and must not be closed by the pause path.
Traces: rds.serverless-v2-open-connection-blocks-auto-pause.

=== Cause 6 of 7: storage autoscaling never fires, so the check reaches no verdict in its budget ===
AWS: with `MaxAllocatedStorage` set above `AllocatedStorage`, RDS autoscales storage once free space crosses the documented threshold (10% free), raising `AllocatedStorage` and passing the instance through `storage-optimization`.
Emulator: the check fills the instance past that threshold and then polls for the raised `AllocatedStorage`; nothing changes and the check is ended by its budget — `Error: Check timed out after 600.0s`. Recorded behavior only: the replay shows no autoscaling event within the window. Hypothesis: the usage-threshold trigger is not implemented, so `MaxAllocatedStorage` is stored and echoed but never acted on.
Why: a client cannot observe or rehearse storage autoscaling, and an instance that would have grown on AWS instead stays at its initial `AllocatedStorage`.
Fix: the DB-instance storage state and whatever evaluates usage against `MaxAllocatedStorage` — the autoscaling trigger and the `AllocatedStorage` / status transition it produces.
Traces: rds.storage-autoscaling-triggers-on-usage-threshold.

=== Cause 7 of 7: a time-zone parameter change is never reflected in a new session, so the check reaches no verdict in its budget ===
AWS: modifying the engine's time-zone parameter in the instance's DB parameter group and applying it changes the offset `now()` returns in a NEW session opened after the apply.
Emulator: the check never observes the changed offset and is ended by its budget — `Error: Check timed out after 60.0s`. Recorded behavior only. Hypothesis: a parameter-group value is recorded on the control plane but is not propagated into the served engine's session defaults, so a new session keeps the original zone.
Why: a client that configures an instance's time zone through its parameter group sees no effect on the data it reads back, so every timestamp is wrong by the intended offset.
Fix: the parameter-group apply path and the served engine's session initialisation — an applied dynamic parameter must take effect for sessions opened after the apply.
Traces: rds.time-zone-parameter-changes-now-offset-in-new-session.

## Evidence
Cause 1: AssertionError: the east copy must be encrypted with the key it was given ('arn:aws:kms:us-east-1:<account>:key/<key-id>'), got '<key-id>' — i.e. the bare key id was echoed where the key ARN is required. (The particular account/key-id are per-run values; the requirement is the ARN form.)
Cause 2: AssertionError on the DescribeDBInstanceAutomatedBackups message. Expected "The provided ARN (<ab-resource-id>) is invalid for this parameter (DbInstanceAutomatedBackupsArn)."; got "The provided ARN (arn:aws:rds:<region>:<account>:auto-backup:<ab-resource-id>) is invalid for this parameter (DbInstanceAutomatedBackupsArn)." Error code InvalidParameterValue is correct on both operations, and the DeleteDBInstanceAutomatedBackup message (which DOES carry the full ARN) passes.
Cause 3: AssertionError: unexpected error message: "The engine postgres isn't compatible with the engine of the source DB snapshot (mysql)." != 'Requested DBSnapshot and Engine are incompatible.'
Cause 4: botocore.exceptions.ClientError: An error occurred (InvalidParameterCombination) when calling the ModifyDBInstance operation: Cannot find version 14.20 for postgres — on an instance created at postgres 13.23 with AllowMajorVersionUpgrade requested.
Cause 5: psycopg.OperationalError: consuming input failed: SSL error: unexpected eof while reading, raised from cur.execute(statement) on an already-established session.
Cause 6: Error: Check timed out after 600.0s, with the instance's AllocatedStorage never rising above its initial value after the fill crossed the 10%-free threshold.
Cause 7: Error: Check timed out after 60.0s, with a new session's now() offset never moving to the configured zone.

## Steps to Reproduce
Cause 1: create a KMS key in region B, CreateDBSnapshot in region A, then CopyDBSnapshot in region B with SourceDBSnapshotIdentifier=<source snapshot ARN>, SourceRegion=<A>, KmsKeyId=<bare key id>; poll the copy to available and compare its KmsKeyId with kms:DescribeKey's KeyMetadata.Arn — they must be equal.
Cause 2: DescribeDBInstanceAutomatedBackups(DBInstanceAutomatedBackupsArn="arn:aws:rds:<region>:<account>:auto-backup:ab-<40 chars>") — expect InvalidParameterValue whose message parenthesises only the `ab-...` segment. Compare with DeleteDBInstanceAutomatedBackup on the same ARN, which must parenthesise the full ARN.
Cause 3: CreateDBInstance(Engine=mysql), CreateDBSnapshot, then RestoreDBInstanceFromDBSnapshot(DBSnapshotIdentifier=<that snapshot>, Engine=postgres) — expect InvalidParameterCombination "Requested DBSnapshot and Engine are incompatible." and no instance created.
Cause 4: CreateDBInstance(Engine=postgres, EngineVersion=13.23), poll to available, then ModifyDBInstance(EngineVersion=14.20, AllowMajorVersionUpgrade=true, ApplyImmediately=false) — expect acceptance with PendingModifiedValues.EngineVersion=14.20 and the live EngineVersion unchanged; also check DescribeDBEngineVersions(Engine=postgres) lists a 14.x build.
Cause 5: CreateDBCluster(Engine=aurora-postgresql) with ServerlessV2ScalingConfiguration SecondsUntilAutoPause=300 and a writer member, poll to available, open a PostgreSQL session, hold it idle ~360 s, then issue a statement on the SAME session — it must succeed rather than fail with an SSL EOF.
Cause 6: CreateDBInstance(Engine=postgres, AllocatedStorage=20, MaxAllocatedStorage=<higher>), poll to available, fill storage to ~92% used, then poll DescribeDBInstances for a raised AllocatedStorage.
Cause 7: create a DB parameter group, set the engine's time-zone parameter to Asia/Karachi, apply it to the instance, wait for the apply, then open a NEW session and read now() — its UTC offset must be +05:00.
