Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.automated-backups-replication-nonexistent-source-instance-rejected rds.deep-rds-sql-truth-mysql-replica-write-rejected-read-only
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.automated-backups-replication-nonexistent-source-instance-rejected|run#11|AssertionError|assert err_stop.get("Code") == _ERR_STOP_NOT_REPLICATING, (
Issue-Key: rds.deep-rds-sql-truth-mysql-replica-write-rejected-read-only|run#22|AssertionError|assert rep_host != src_host, f"replica and source must be distinct endpoints; both {rep_host!r}"
Issue-Commit: b38efef85af881a9c357f09f754c4d7783936268

# rds: 2 net-new causes — StopDBInstanceAutomatedBackupsReplication resolves the source instance before the replication and answers DBInstanceNotFound where AWS answers InvalidDBInstanceState, and a mysql read replica publishes the source instance's endpoint address

> _Note: this issue was identified against the code base at commit `b38efef85af881a9c357f09f754c4d7783936268`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StopDBInstanceAutomatedBackupsReplication

## Description
Two independent, net-new RDS divergences, both grounded in failing behavioral checks the suite lists under the `rds` heading, and neither carried by an open issue's replay key.

Named neighbours that do NOT cover these: 20261120T222703Z-rds-2-net-new-causes-startdbinstanceautomatedbackupsreplication-s-dbinstancenotfound.md is keyed on the START leg's message of the same check (`err_start.get("Message")`), and that leg now PASSES — the Start refusal already reads `Requested DB Instance <id> not found in source region <region>`; the check now fails one assertion later, on the STOP leg's error CODE, which that issue only mentions in prose and does not key. 20261118T181500Z-rds-the-published-endpoint-of-a-served-engine-is-the-core-listeners.md describes the served-endpoint family in its cause 1, but its trace list and Issue-Keys do not include the replica check below, so that check is recorded nowhere.

=== Cause 1 of 2: StopDBInstanceAutomatedBackupsReplication checks the source instance's existence before the replication, so a non-replicating ARN is refused DBInstanceNotFound instead of InvalidDBInstanceState ===
AWS: Smithy (com.amazonaws.rds#AmazonRDSv19) models StopDBInstanceAutomatedBackupsReplication with input `SourceDBInstanceArn` (required) and exactly two errors — DBInstanceNotFoundFault (awsQueryError code `DBInstanceNotFound`, HTTP 404) and InvalidDBInstanceStateFault (awsQueryError code `InvalidDBInstanceState`, HTTP 400, doc "The DB instance isn't in a valid state."). The question this operation asks is about the DESTINATION region it was called in: is the ARN's instance replicating its automated backups INTO this region? Recorded behavior: for an ARN naming an instance that does not exist at all, the Stop is refused HTTP 400 `InvalidDBInstanceState` with message `DB Instance <source-db-instance-arn> is not replicating to the current region.` Fixed text: `DB Instance ` … ` is not replicating to the current region.` (trailing period included). Variable part: the request's `SourceDBInstanceArn`, reproduced whole — the ARN, not the bare identifier. AWS does not reach a source-instance lookup here, which is why the identical ARN that the Start leg refuses `DBInstanceNotFound` (404) the Stop leg refuses `InvalidDBInstanceState` (400): the two legs resolve different things.
Emulator: the handler resolves the source instance FIRST and returns DBInstanceNotFoundFault — observed Error = {'Type': 'Sender', 'Code': 'DBInstanceNotFound', 'Message': 'Requested DB Instance <identifier> not found in source region <region>'} (HTTP 404), i.e. the Start leg's source-region sentence with the bare identifier. The InvalidDBInstanceStateFault branch is only reached for an instance that DOES exist, and when it is reached it words the message from `backup_replication_not_enabled_message(&identifier)` — built from the bare DB instance identifier, not from the request ARN — so even that branch's text diverges from AWS's sentence.
Why: a client tearing down cross-region automated-backup replication branches on exactly this distinction. `InvalidDBInstanceState` means "nothing to stop here, you are already in the desired state" and is the code such a client swallows as idempotent success; `DBInstanceNotFound` (404) means "your ARN is wrong" and is the code it escalates. Today a stop of a replication that is not configured — including a repeat stop, and a stop issued from the wrong destination region — is reported as a bad ARN, so idempotent cleanup paths abort and retry loops fail closed.
Fix: in the StopDBInstanceAutomatedBackupsReplication handler, order the checks so the destination region's replicated-backup record is tested BEFORE the source instance is looked up, and refuse with InvalidDBInstanceStateFault whenever no replication into the calling region exists — whether the source instance is missing or merely not replicating. That refusal's message belongs to this operation and must be worded from the request's `SourceDBInstanceArn`, so it needs its own message constructor rather than the identifier-based not-enabled constant in state. DBInstanceNotFoundFault stays modeled but is not the answer for this input.
Traces: failing check `rds.automated-backups-replication-nonexistent-source-instance-rejected` (AssertionError on the stop leg's error code). Handler: services/service-com-amazonaws-rds-amazonrdsv19/src/ops/stop_db_instance_automated_backups_replication.rs:185-225 (the `db_instances` lookup at 185 precedes the `replicated_instance_backup_of` test at 211); message constructors in services/service-com-amazonaws-rds-amazonrdsv19/src/state.rs (`source_db_instance_not_found_message`, `backup_replication_not_enabled_message`).

=== Cause 2 of 2: a mysql read replica publishes the same Endpoint.Address as the source instance it replicates ===
AWS: Smithy models `Endpoint$Address` as "the DNS address of the DB instance" and `Endpoint$Port` as "the port that the database engine is listening on". The address is derived from the instance's own identifier, so one DB instance's address is never another's; a read replica created by CreateDBInstanceReadReplica is a distinct DB instance with a distinct DNS address, which is precisely what lets a client send writes to the source and reads to the replica and observe the replica refusing a write with MySQL ER_OPTION_PREVENTS_STATEMENT (1290), "The MySQL server is running with the --read-only option so it cannot execute this statement".
Emulator: DescribeDBInstances reports `Endpoint.Address` = `127.0.0.1` for BOTH the source and the replica — the core listener's bind interface rather than a per-instance address — so the replica and its source are indistinguishable by endpoint. The check cannot open a session against the replica at all and fails at `assert rep_host != src_host` with "replica and source must be distinct endpoints; both '127.0.0.1'", before any read-only enforcement is exercised.
Why: a client that points a reader at the replica's endpoint silently reaches the writer instead. Reads are served by the primary (no read scaling), and — worse for correctness — a write mistakenly issued to the "replica" SUCCEEDS instead of being refused read-only, so an application that relies on the replica being read-only loses that guarantee and can corrupt data it believed was unreachable for writes.
Fix: the published endpoint of a served-engine DB instance must be per-instance, in the RDS control plane's endpoint representation for served engines (the same place DescribeDBInstances/CreateDBInstanceReadReplica fill `Endpoint`), so every live instance — source and replica alike — announces its own dialable address on the engine's port. See 20261118T181500Z-rds-the-published-endpoint-of-a-served-engine-is-the-core-listeners.md cause 1 for the listener constraint a fixer will hit; this check's replay key is not on that issue and must be closed with it.
Traces: failing check `rds.deep-rds-sql-truth-mysql-replica-write-rejected-read-only`.

## Evidence
Cause 1 — check rds.automated-backups-replication-nonexistent-source-instance-rejected:
  setup: DescribeDBInstances(MaxRecords=20) succeeds (positive control). A DB instance identifier that does not exist is chosen and an ARN built for it: arn:aws:rds:<region>:<account>:db:<ghost-identifier>.
  step 1 (passes today): StartDBInstanceAutomatedBackupsReplication(SourceDBInstanceArn=<ghost-arn>) -> HTTP 404, Code="DBInstanceNotFound", Message="Requested DB Instance <ghost-identifier> not found in source region <region>". This is correct.
  step 2 (the divergence): StopDBInstanceAutomatedBackupsReplication(SourceDBInstanceArn=<the same ghost-arn>)
    expected: HTTP 400, Error.Code = "InvalidDBInstanceState", Error.Message = "DB Instance <ghost-arn> is not replicating to the current region." (fixed text "DB Instance " / " is not replicating to the current region."; the single variable is the request's SourceDBInstanceArn, reproduced in full).
    actual:   HTTP 404, Error.Code = "DBInstanceNotFound", Error.Message = "Requested DB Instance <ghost-identifier> not found in source region <region>".
  replay output: AssertionError: Stop on the same ARN: expected InvalidDBInstanceState, got {'Type': 'Sender', 'Code': 'DBInstanceNotFound', 'Message': 'Requested DB Instance sim2real-dbi-20488076d0ae not found in source region us-west-2'}  (the identifier and region shown are this run's generated values, not part of the requirement).
  code path: stop_db_instance_automated_backups_replication.rs returns DBInstanceNotFoundFault at the `store.db_instances.get(...)` miss (line 185ff) and so never reaches the `replicating` test (line 211ff) that would produce InvalidDBInstanceStateFault.

Cause 2 — check rds.deep-rds-sql-truth-mysql-replica-write-rejected-read-only:
  setup: a mysql db.t3.micro source instance on port 3306 polled to available, then CreateDBInstanceReadReplica of it, also polled to available.
  expected: DescribeDBInstances on the two instances returns two DIFFERENT Endpoint.Address values (one DNS name per instance), each dialable, so that an INSERT on the replica's endpoint is refused with MySQL error 1290 (ER_OPTION_PREVENTS_STATEMENT, "The MySQL server is running with the --read-only option so it cannot execute this statement") while the same INSERT on the source's endpoint succeeds.
  actual:   both instances report Endpoint.Address = "127.0.0.1" — the core listener's bind interface — so replica and source are the same host and the read-only enforcement is never reached.
  replay output: AssertionError: replica and source must be distinct endpoints; both '127.0.0.1'

## Steps to Reproduce
Cause 1:
1. Boot the emulator and point an rds client at it; confirm DescribeDBInstances answers (positive control).
2. Build an ARN for a DB instance identifier that does not exist: arn:aws:rds:<region>:<account>:db:<ghost-identifier>.
3. Call StopDBInstanceAutomatedBackupsReplication(SourceDBInstanceArn=<that ARN>).
4. Observe HTTP 404 / Code=DBInstanceNotFound instead of HTTP 400 / Code=InvalidDBInstanceState with message "DB Instance <that ARN> is not replicating to the current region."
5. Also check the existing-instance path, which diverges in message text only: create a DB instance, do NOT start automated-backup replication, then call StopDBInstanceAutomatedBackupsReplication with its ARN — the code is InvalidDBInstanceState but the message is built from the bare identifier instead of the request ARN.
6. Repeat 3 after a successful Start+Stop pair (a second Stop) and confirm it too answers InvalidDBInstanceState.
7. Re-replay behavioral check rds.automated-backups-replication-nonexistent-source-instance-rejected.

Cause 2:
1. CreateDBInstance(Engine=mysql, DBInstanceClass=db.t3.micro, Port=3306, BackupRetentionPeriod>=1) and poll to available.
2. CreateDBInstanceReadReplica naming that instance as the source and poll the replica to available.
3. DescribeDBInstances for both and compare Endpoint.Address — today both are "127.0.0.1"; each must be its own per-instance DNS address, and each must be dialable on 3306.
4. Open a native mysql session to the replica's endpoint and issue an INSERT: it must be refused with error 1290 ("The MySQL server is running with the --read-only option so it cannot execute this statement"), while the same INSERT on the source's endpoint succeeds.
5. Re-replay behavioral check rds.deep-rds-sql-truth-mysql-replica-write-rejected-read-only.
