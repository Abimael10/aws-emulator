Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.automated-backups-replication-nonexistent-source-instance-rejected
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.automated-backups-replication-nonexistent-source-instance-rejected|run#7|AssertionError|assert err_start.get("Code") == _ERR_START_NOT_FOUND, (
Issue-Commit: cd00ce55523bfa99c95686848d372910b65cd86b

# Implement rds StartDBInstanceAutomatedBackupsReplication: unimplemented scaffold stub

> _Note: this issue was identified against the code base at commit `cd00ce55523bfa99c95686848d372910b65cd86b`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartDBInstanceAutomatedBackupsReplication

## Description
`StartDBInstanceAutomatedBackupsReplication` is an unmodified scaffold stub, so it answers HTTP 501 and persists nothing. Implement it in `services/service-com-amazonaws-rds-amazonrdsv19/src/ops/start_db_instance_automated_backups_replication.rs`: replace the `Err(crate::scaffolded::Scaffolded::not_implemented(..))` with a real handler that reads and writes the service's state, then DELETE that file's `//! SCAFFOLDED, NOT IMPLEMENTED:` header line.

That header line is the stub census — the one record of which operations are still scaffolds. Deleting it without implementing the handler, or leaving it in place on a handler you did implement, corrupts that record rather than closing this issue. A stub's 501 is also CORRECT for a stub: this issue asks you to implement the operation, never to change the error it returns while unimplemented.

1 recorded unit(s) fail on their `StartDBInstanceAutomatedBackupsReplication` call, so each one unblocks only when `StartDBInstanceAutomatedBackupsReplication` works: `rds.automated-backups-replication-nonexistent-source-instance-rejected`.

## Evidence
The handler carries the scaffold marker `//! SCAFFOLDED, NOT IMPLEMENTED:` at the top of `services/service-com-amazonaws-rds-amazonrdsv19/src/ops/start_db_instance_automated_backups_replication.rs`. Replaying the corpus, the emulator refused the `StartDBInstanceAutomatedBackupsReplication` call: File "run.py", line 456, in <lambda>
    body = calls.in_phase(calls.BODY, lambda: fn(ctx))
                                              ^^^^^^^
  File "check_rds_automated_backups_replication_nonexistent_source_instance_rejected.py", line 45, in run
    assert err_start.get("Code") == _ERR_START_NOT_FOUND, (
           ^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^^
AssertionError: Start on a nonexistent source instance: expected DBInstanceNotFound, got {'Type': 'Receiver', 'Code': 'NotImplemented', 'Message': 'operation StartDBInstanceAutomatedBackupsReplication is scaffolded but not implemented in this emulator'}

## Steps to Reproduce
Boot the rds emulator
Call StartDBInstanceAutomatedBackupsReplication and observe HTTP 501 with no state persisted
Replay any of the blocked units above and observe it fail on that refusal

## Resolution

`StartDBInstanceAutomatedBackupsReplication` and its inverse
`StopDBInstanceAutomatedBackupsReplication` are implemented over a destination-Region record of
the replicated automated backups, reported by `DescribeDBInstanceAutomatedBackups` in that
Region and removable by `DeleteDBInstanceAutomatedBackup` once the replication is stopped.

Remaining known divergence, for whoever picks up the wording next: the call's
`DBInstanceNotFound` refusal carries this front door's sentence for a missing DB instance,
`DBInstance <identifier> not found.`, with the identifier taken from the request's
`SourceDBInstanceArn`. The recorded unit still diverges on that sentence's TEXT (its status
404 and its code `DBInstanceNotFound` match), and the replay withholds the recorded wording,
so AWS's exact sentence for this operation is not yet known. These frames were each
implemented and replayed, and none matched: `DBInstance <arn> not found.`,
`DBInstance not found: <arn>`, `DBInstance not found: <identifier>`,
`The source instance could not be found: <arn>`,
`The source instance could not be found: <identifier>` and
`The source DB instance could not be found: <arn>`.
