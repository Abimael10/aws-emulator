Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds.db-capacity-reservation-nonexistent-identifier-rejected rds.aurora-mysql-reader-endpoint-write-refused-read-only rds.aurora-writer-reader-endpoint-query-routing
Issue-Shape-Id: com.amazonaws.rds#AmazonRDSv19
Issue-Key: rds.db-capacity-reservation-nonexistent-identifier-rejected|run#7|AssertionError|assert err_describe.get("Message") == (
Issue-Key: rds.aurora-mysql-reader-endpoint-write-refused-read-only|_connect_reader_replica#26|Timeout|candidate = _my_connect_with_retry(reader_host, reader_port, password)
Issue-Key: rds.aurora-writer-reader-endpoint-query-routing|_connect_reader_replica#35|Timeout|time.sleep(interval_s)
Issue-Commit: 22c0b68190b6d53f08de46118af925b6725f9997

# rds: 2 net-new causes — the DB-capacity-reservation not-found refusal is emulator-authored prose on Describe/Modify/Cancel, and an Aurora ReaderEndpoint connection never lands on a read-only replica

## Resolution

Cause 1 is FIXED and its replay unit passes: the one not-found wording helper of the DB
capacity reservation state now carries AWS's sentence
("The specified DBCapacityReservation '<the value the request sent>' does not exist."), and
`ModifyDBCapacityReservation` and `CancelDBCapacityReservation` — both still 501 scaffolds at
the commit above, which is why the check could not even reach their wording — are implemented,
so all three operations that resolve a `DBCapacityReservationIdentifier` raise the modelled
404 fault with that sentence.

Cause 2 is PARTLY IMPLEMENTED and otherwise BLOCKED on the read-only core; its two replay
units still fail, because the one piece that is blocked is the one the checks need first.

What the cause asks for, and where each part stands:

* reader-endpoint ROUTING — already implemented: a DB cluster opens a SECOND, read-only
  listener (`src/state/data_plane.rs`, `EndpointRole::Reader`), its sessions are served by the
  first non-writer member (`src/state/data_plane/cluster_endpoint.rs`, `serving_member`) and
  serve no writes (`serves_reads_only`), and the member a session landed on is what
  `@@aurora_server_id` answers.
* the reader node's READ-ONLY ENGINE STATE — already implemented: `pg_is_in_recovery()` is
  true and `@@innodb_read_only` / `@@read_only` / `@@super_read_only` are 1 on such a session
  (`tests/all/engine_read_only_system_variables.rs`), and the reader reads the rows the writer
  committed, both members sharing one cluster volume.
* the reader's REFUSAL of a write — FIXED here: a MySQL-family read-only session was refused
  with `ER_OPTION_PREVENTS_STATEMENT` (1290, "The MySQL server is running with the
  --read-only option …") whatever the engine, which is the sentence an RDS for MySQL read
  replica sends; an Aurora MySQL node that is not the cluster's writer sends
  `ER_READ_ONLY_MODE` (1836, "Running in read-only mode"). The refusal now follows the engine
  family that serves the endpoint (`src/state/data_plane/read_only.rs`), with both rows in
  `tests/all/read_only_engine_endpoints.rs`. Aurora PostgreSQL already answered SQLSTATE
  25006.
* the published ADDRESS of the reader endpoint — BLOCKED. `DBCluster` carries ONE `Port` for
  both endpoint hosts, so the reader endpoint is reachable only when the two hosts resolve to
  different interfaces at the same port — and the core binds every listener on one
  operator-chosen interface at a port it assigns itself
  (`vendor/server/src/listeners.rs:88` `bind_tokio`, `:230` `tcp_plaintext`), then DROPS the
  accepted connection's local address (`vendor/server/src/listeners.rs:319`
  `let (stream, _peer) = ...`), handing `NativeHandler::serve` the stream alone (`:136`).
  Neither wire carries the host the client dialled in band, so a handler cannot tell a reader
  connection from a writer one on the single published `host:Port` pair, and a service may not
  bind a socket or choose a port of its own (`vendor/engine/src/lib.rs:21-32`). Required core
  change: hand a native handler the local address each accepted connection arrived on (and
  accept on the loopback block), or let a service open a second listener at an already-assigned
  port on another loopback address — then the cluster can publish two distinct loopback hosts
  beside one `Port` and serve the reader role on the one the client dialled.

> _Note: this issue was identified against the code base at commit `22c0b68190b6d53f08de46118af925b6725f9997`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
DescribeDBCapacityReservations, DescribeDBClusters

## Description
This issue bundles 2 distinct net-new root causes in the emulated RDS surface. They are independent and a fixer must close BOTH before submitting.

Smithy consulted: services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json.

---
Cause 1 of 2: the DB-capacity-reservation not-found refusal is emulator-authored prose, on all three operations that take a DBCapacityReservationIdentifier.
AWS: Smithy DescribeDBCapacityReservations models exactly one error, DBCapacityReservationNotFoundFault (errorKind client, httpError 404, awsQueryError code 'DBCapacityReservationNotFound', httpResponseCode 404); ModifyDBCapacityReservation and CancelDBCapacityReservation model the same fault (alongside InvalidDBCapacityReservationStateFault and, for Modify, InstanceQuotaExceededFault / InsufficientDBInstanceCapacityFault). DBCapacityReservationIdentifier is @length 1..63, pattern ^[a-z](?:-?[a-z0-9]+){0,15}$, so a well-formed identifier that names nothing is a not-found, not a validation error. For that condition AWS words the message "The specified DBCapacityReservation '<identifier>' does not exist." — the FIXED text is "The specified DBCapacityReservation '" … "' does not exist." (single quotes and trailing period included) and its ONLY variable part is the value the request sent in DBCapacityReservationIdentifier. An unfiltered DescribeDBCapacityReservations on the same API still answers 200 (the check's positive control).
Emulator: the HTTP status and the error code are correct (DBCapacityReservationNotFound), but the message is re-authored as "DBCapacityReservation <identifier> not found." — a different frame, no quoting of the identifier. The same wording is served by Describe, and the check asserts it for Modify and Cancel too, so one shared helper words all three.
Why: a client that classifies, logs or alarms on this refusal by its message text sees an unrecognised sentence for a condition AWS words one way, and a parser that lifts the identifier out of AWS's quoted sentence finds nothing to extract. The code is right, so only text-matching consumers break — but they break on every reservation operation.
Fix: the single not-found wording helper the capacity-reservation state exposes and its three callers (the DescribeDBCapacityReservations, ModifyDBCapacityReservation and CancelDBCapacityReservation handlers) — all three must emit AWS's sentence.
Related open issue that does NOT carry this check's replay key: 20261124T234610Z-rds-db-capacity-reservations-a-reservation-aws-creates.md (a WIRE issue whose Cause 2 is the same helper reached through DescribeDBCapacityReservations only; the replay report did not mark this behavioral check OWNED by it, and this check additionally pins Modify and Cancel to the same wording).
Traces: rds.db-capacity-reservation-nonexistent-identifier-rejected.

---
Cause 2 of 2: a connection opened on an Aurora cluster's ReaderEndpoint never lands on a read-only replica, so both reader-endpoint data-plane checks exhaust their budget without reaching an observation.
AWS: Smithy DBCluster.ReaderEndpoint is documented as "The reader endpoint for the DB cluster. The reader endpoint for a DB cluster load-balances connections across the Aurora Replicas that are available in a DB cluster" — a connection made on it is served by a non-writer member whenever the cluster has one. Recorded behavior only for the engine-level facts (the model states none of them): on Aurora PostgreSQL that node reports pg_is_in_recovery() = true, serves a row the writer committed (members share one storage volume) and refuses an INSERT with SQLSTATE 25006 (read_only_sql_transaction); on Aurora MySQL it reports @@aurora_server_id = the reader member's own DBInstanceIdentifier and @@innodb_read_only = 1 and refuses an INSERT with errno 1836, message "Running in read-only mode". The writer Endpoint's node reports pg_is_in_recovery() = false / @@innodb_read_only = 0 and accepts the write.
Emulator: both checks end as "Error: Check timed out after 60.0s". Neither check contains a fixed-duration hold: every wait is a predicate poll that returns the instant its predicate holds, so the budget is exhausted only because no response ever satisfies one. Hypothesis (the recording shows the timeout, not a response body): the reader endpoint is served by the writer node, so every FRESH connection the checks open to ReaderEndpoint reports pg_is_in_recovery() = false / @@aurora_server_id = the writer, and their _connect_reader_replica loops (900 s and 1500 s deadlines) spin past the 60 s check budget. The only other state consistent with a predicate-only poll never settling is the cluster/members never reaching Status 'available' with a non-empty ReaderEndpoint. Either way the requirement is the same: the reader endpoint must be served by an available non-writer member.
Why: read/write splitting is unaddressable. Every client that sends reads to ReaderEndpoint and writes to Endpoint — the standard Aurora scaling pattern — silently sends both to the writer, so a write issued against the reader is ACCEPTED where AWS refuses it (25006 / errno 1836), load never spreads, and no caller can detect the misconfiguration.
Fix: reader-endpoint routing in the RDS cluster data plane — the listener/route the cluster publishes as ReaderEndpoint must be served by an available member with IsClusterWriter=false, with that member's read-only engine state (pg_is_in_recovery / @@innodb_read_only / @@aurora_server_id) and its read-only refusal of a write.
Related open issues that do NOT carry these two checks' replay keys: 20261118T210044Z-rds-2-net-new-causes-describedbclusters-publishes-an.md (DescribeDBClusters publishes an identical writer and reader endpoint) and 20261121T105515Z-rds-5-net-new-behavioral-causes-aurora-cluster.md (Aurora cluster/custom endpoints published on an unserved host); the replay report marked neither of these checks OWNED.
Traces: rds.aurora-mysql-reader-endpoint-write-refused-read-only, rds.aurora-writer-reader-endpoint-query-routing.

## Evidence
Cause 1 — check rds.db-capacity-reservation-nonexistent-identifier-rejected [mismatch]. The positive control (unfiltered DescribeDBCapacityReservations(MaxRecords=20)) passed, and the Error.Code assertion (DBCapacityReservationNotFound) passed; only the message diverged.
  File "check_rds_db_capacity_reservation_nonexistent_identifier_rejected.py", line 44, in run
    assert err_describe.get("Message") == (
  AssertionError: DBCapacityReservation sim2real-dbcr-6ec8769ee5ef not found.
  Expected (AWS): "The specified DBCapacityReservation 'sim2real-dbcr-6ec8769ee5ef' does not exist."
  Actual (emulator): "DBCapacityReservation sim2real-dbcr-6ec8769ee5ef not found."
  The identifier 'sim2real-dbcr-6ec8769ee5ef' is this run's generated value filling the template; the requirement is the fixed text "The specified DBCapacityReservation '" + <DBCapacityReservationIdentifier from the request> + "' does not exist." The check asserts the same code and the same sentence for ModifyDBCapacityReservation(CapacityPerAvailabilityZone=2) and CancelDBCapacityReservation on the same ghost identifier.

Cause 2 — checks rds.aurora-mysql-reader-endpoint-write-refused-read-only and rds.aurora-writer-reader-endpoint-query-routing, both [mismatch] with "Error: Check timed out after 60.0s".
  Shape of each check (no fixed-duration wait anywhere): create an Aurora cluster (aurora-mysql / aurora-postgresql Serverless v2) with a writer member and a PromotionTier=15 reader member; poll both members to 'available' and the cluster to Status 'available' with a non-empty ReaderEndpoint; connect on Endpoint and assert the node is writable (@@innodb_read_only 0 / pg_is_in_recovery() false), CREATE TABLE + INSERT one row; then open fresh connections on ReaderEndpoint until one lands on the reader member.
  Expected (AWS): the reader connection reports @@aurora_server_id == <reader member id> and @@innodb_read_only == 1 (Aurora MySQL) or pg_is_in_recovery() == true (Aurora PostgreSQL); it SELECTs the writer's committed row; an INSERT on it is refused with MySQL errno 1836 and message "Running in read-only mode" (Aurora MySQL) or SQLSTATE 25006 (Aurora PostgreSQL); the refused row exists on neither the reader nor the writer.
  Actual (emulator): no observation is ever reached — the reader-selection loops (`_connect_reader_replica`, timeout_s=1500 interval 2 s for MySQL, timeout_s=900 interval 20 s for PostgreSQL) never see a non-writer node, and the check ends at the 60 s replay budget. Hosts, ports and generated cluster/member ids are per-run values and are not the requirement.

## Steps to Reproduce
1. git_clone, then run_behavioral_suite against the booted emulator.
2. Cause 1: call rds.describe_db_capacity_reservations(DBCapacityReservationIdentifier='<a well-formed identifier matching ^[a-z](?:-?[a-z0-9]+){0,15}$ that was never created>'). Observe HTTP 404 with Error.Code 'DBCapacityReservationNotFound' (correct) and Error.Message "DBCapacityReservation <that value> not found." where AWS words "The specified DBCapacityReservation '<that value>' does not exist.". Repeat with rds.modify_db_capacity_reservation(DBCapacityReservationIdentifier=<same>, CapacityPerAvailabilityZone=2) and rds.cancel_db_capacity_reservation(DBCapacityReservationIdentifier=<same>) and observe the same re-authored sentence. Confirm rds.describe_db_capacity_reservations(MaxRecords=20) still answers 200.
3. Cause 2: CreateDBSubnetGroup over the run's private subnets; CreateDBCluster(Engine='aurora-mysql', default EngineVersion, BackupRetentionPeriod=1) and then CreateDBInstance twice into it — a writer, and a second member with PromotionTier=15. Poll both to 'available' and the cluster to Status 'available'.
4. Read DBCluster.Endpoint, DBCluster.ReaderEndpoint and DBCluster.Port. Open a MySQL connection on Endpoint: SELECT @@innodb_read_only (0 expected), CREATE TABLE t(id INT PRIMARY KEY, val VARCHAR(32)), INSERT one row.
5. Open a FRESH connection on ReaderEndpoint and SELECT @@aurora_server_id — observe it is not the PromotionTier=15 member's identifier (and @@innodb_read_only is 0), repeat as the check does and observe no connection ever lands on the reader, so the subsequent assertions (row visible, INSERT refused with errno 1836 "Running in read-only mode") are never reached. Repeat on aurora-postgresql with pg_is_in_recovery() / SQLSTATE 25006.
6. Replay the three check ids in trace_ids to confirm a fix.
