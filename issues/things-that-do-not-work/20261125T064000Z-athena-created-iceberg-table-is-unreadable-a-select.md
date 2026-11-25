Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: athena.iceberg-time-travel-returns-the-snapshot-before-the-row-level-update
Issue-Shape-Id: com.amazonaws.athena#AmazonAthena
Issue-Key: athena.iceberg-time-travel-returns-the-snapshot-before-the-row-level-update|_run_sql#14|AssertionError|raise AssertionError(
Issue-Commit: e775a2fec8b674e0d0a3cde44ce8df5c9a1c0b14

# Athena-created Iceberg table is unreadable: a SELECT over a table created by CREATE TABLE ... TBLPROPERTIES ('table_type'='ICEBERG') settles FAILED with GENERIC_USER_ERROR 'Detected Iceberg type table without metadata location'

> _Note: this issue was identified against the code base at commit `e775a2fec8b674e0d0a3cde44ce8df5c9a1c0b14`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
StartQueryExecution

## Description
One net-new root cause in the Athena data plane (StartQueryExecution / GetQueryExecution), reached through an Iceberg table that Athena itself created.

=== Cause 1 of 1: the Iceberg CREATE TABLE registers a table with no Iceberg metadata, so every subsequent read of it is refused as a metastore-only Iceberg registration ===

AWS: an Iceberg table created through Athena with the documented bare-name form `CREATE TABLE <tbl> (id int, name string) LOCATION 's3://<bucket>/<prefix>/' TBLPROPERTIES ('table_type'='ICEBERG', 'format'='parquet')` on a workgroup pinned to "Athena engine version 3" is immediately usable: the CREATE settles SUCCEEDED and Athena materialises the Iceberg metadata tree under LOCATION and records the table's `metadata_location`. An `INSERT INTO "<db>"."<tbl>" VALUES (...)` then settles SUCCEEDED and the following `SELECT id, name FROM "<db>"."<tbl>" ORDER BY id` settles SUCCEEDED and returns exactly the inserted rows; the table's `"<tbl>$snapshots"` metadata table lists one commit per write as an unbroken parent chain, row-level `UPDATE`/`DELETE` commit new snapshots, and `FOR VERSION AS OF <snapshot_id>` still returns the older snapshot's rows. The text the emulator returns here is AWS's message for a DIFFERENT situation, as the message itself states: Athena emits `GENERIC_USER_ERROR: Detected Iceberg type table without metadata location. Please make sure an Iceberg-enabled compute engine such as Athena or EMR Spark is used to create the table, or the table is created by using the Iceberg open source AWS library iceberg-aws. Setting table_type parameter in Glue metastore to create an Iceberg table is not supported.` only for a table whose Glue properties carry `table_type=ICEBERG` while no `metadata_location` exists — i.e. a table registered directly in the Glue metastore rather than created by Athena. Its fixed text is the whole sentence above; it has no variable parts, and it is not a legal answer for a table Athena created. Smithy (services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json): QueryExecutionStatus.State (QueryExecutionState enum QUEUED/RUNNING/SUCCEEDED/FAILED/CANCELLED), QueryExecutionStatus.StateChangeReason (String, free text), AthenaError.ErrorCategory (1=System, 2=User, 3=Other) / ErrorType / ErrorMessage. The per-statement success/failure sequence above is recorded behavior only; the statement forms are Athena's documented Iceberg syntax.

Emulator: the Iceberg `CREATE TABLE ... LOCATION ... TBLPROPERTIES ('table_type'='ICEBERG', ...)` settles SUCCEEDED and the `INSERT INTO` of three rows also settles SUCCEEDED, but the very next read of the same table — `SELECT id, name FROM "<db>"."<tbl>" ORDER BY id` — settles FAILED with Status.StateChangeReason `GENERIC_USER_ERROR: Detected Iceberg type table without metadata location. Please make sure an Iceberg-enabled compute engine such as Athena or EMR Spark is used to create the table, or the table is created by using the Iceberg open source AWS library iceberg-aws. Setting table_type parameter in Glue metastore to create an Iceberg table is not supported.` The emulator records `table_type=ICEBERG` on the table it just created but never writes an Iceberg metadata tree under LOCATION and never sets `metadata_location`, so its own scan planner classifies an Athena-created Iceberg table as a metastore-only registration and refuses it. Because the read is refused, nothing downstream of it is reachable either: `$snapshots`, row-level UPDATE/DELETE and `FOR VERSION AS OF` time travel are all unexercisable.

Related open issue that does NOT cover this (the suite did not mark the check OWNED): 20261111T090701Z-athena-s-sql-engine-refuses-trino-hive-statements.md Cause 7 describes the earlier shape of this path, where the Iceberg CREATE TABLE was not registered at all and the following INSERT settled FAILED with `no such table: <db>.<tbl>`. That symptom is gone — the table now registers and the INSERT succeeds — and the refusal described here is a different one on the READ path, so it is filed separately. Fixed issue 20261119T231346Z-athena-getqueryresults-returns-a-stale-row-count-for.md concerns the row COUNT of an Iceberg read, not the refusal of one.

Why: a client that uses Athena's documented way to create and use an Iceberg table gets a user-category failure on the first read of a table it just created successfully, and the remedy the message prescribes ("make sure an Iceberg-enabled compute engine such as Athena ... is used to create the table") is the thing it already did, so there is no corrective action available. Everything Iceberg is for — ACID row-level UPDATE/DELETE, the snapshot commit log, and `FOR VERSION AS OF` time travel — is unreachable against the emulator while it works against AWS.

Fix: the Athena query-execution engine's Iceberg DDL and table-read paths in services/service-com-amazonaws-athena-amazonathena. The CREATE TABLE handler for `table_type=ICEBERG` must materialise the table's Iceberg metadata under the requested LOCATION and record its `metadata_location` in the catalog entry it writes; the scan planner's "Iceberg table without metadata location" guard must then accept an Athena-created Iceberg table instead of refusing it; and the Iceberg table state must carry a commit log so that writes produce snapshots readable through `<tbl>$snapshots`, row-level UPDATE/DELETE commit new snapshots, and `FOR VERSION AS OF <snapshot_id>` resolves an earlier snapshot.

Traces: athena.iceberg-time-travel-returns-the-snapshot-before-the-row-level-update (fails at the first post-INSERT SELECT, before the time-travel legs are reached).

## Evidence
Behavioral suite simparity-behavioral-aws-v1, 398/443 passed; this is the only failing check not already filed or owned.

[mismatch] athena.iceberg-time-travel-returns-the-snapshot-before-the-row-level-update
  AssertionError raised in the check's _run_sql helper:
  "statement reached FAILED, expected SUCCEEDED: 'SELECT id, name FROM \"<db>\".\"<tbl>\" ORDER BY id' -- GENERIC_USER_ERROR: Detected Iceberg type table without metadata location. Please make sure an Iceberg-enabled compute engine such as Athena or EMR Spark is used to create the table, or the table is created by using the Iceberg open source AWS library iceberg-aws. Setting table_type parameter in Glue metastore to create an Iceberg table is not supported."
  (<db>/<tbl> are the check's generated database and table names; the recorded run used sim2real_behav_db_7d4504662dc9 / sim2real_behav_ice_83d8336b30e4 — generated names, not part of the requirement.)

Statements that preceded it in the SAME check and all settled SUCCEEDED in the emulator, proving the refusal is self-inflicted:
  1. CreateWorkGroup with Configuration.EngineVersion.SelectedEngineVersion = "Athena engine version 3"
  2. CREATE DATABASE `<db>`
  3. CREATE TABLE <tbl> (id int, name string) LOCATION 's3://<data-bucket>/ice/' TBLPROPERTIES ('table_type'='ICEBERG', 'format'='parquet')
  4. INSERT INTO "<db>"."<tbl>" VALUES (1, 'one-<marker>'), (2, 'two-<marker>'), (3, 'three-<marker>')

Expected (AWS): step 5, SELECT id, name FROM "<db>"."<tbl>" ORDER BY id -> State SUCCEEDED, GetQueryResults rows [["1","one-<marker>"],["2","two-<marker>"],["3","three-<marker>"]] after the header row.
Actual (emulator): State FAILED with the GENERIC_USER_ERROR StateChangeReason quoted above; no rows.

Smithy consulted: QueryExecutionStatus (State: QueryExecutionState enum; StateChangeReason: String, unconstrained) and AthenaError (ErrorCategory @range 1-3 with 1=System/2=User/3=Other, ErrorType @range 0-9999, Retryable @default false, ErrorMessage: String) in services/service-com-amazonaws-athena-amazonathena/smithy-model/com-amazonaws-athena-amazonathena.json. The model constrains neither message text nor the Iceberg semantics, so the per-statement outcomes are recorded behavior only.

## Steps to Reproduce
1. Clone the repo, build and boot the emulator, and point a botocore `athena` client (plus `s3`) at it.
2. Create two buckets: one for query results (`s3://<results>/results/`) and one for the Iceberg table data (`s3://<data>/ice/`).
3. CreateWorkGroup with Configuration = {ResultConfiguration: {OutputLocation: 's3://<results>/results/'}, EngineVersion: {SelectedEngineVersion: 'Athena engine version 3'}}.
4. StartQueryExecution `CREATE DATABASE `<db>`` in that workgroup and poll GetQueryExecution to SUCCEEDED.
5. StartQueryExecution, with QueryExecutionContext.Database = <db> and the same workgroup, the bare-name Iceberg DDL: `CREATE TABLE <tbl> (id int, name string) LOCATION 's3://<data>/ice/' TBLPROPERTIES ('table_type'='ICEBERG', 'format'='parquet')`. It settles SUCCEEDED.
6. StartQueryExecution `INSERT INTO "<db>"."<tbl>" VALUES (1, 'one'), (2, 'two'), (3, 'three')`. It settles SUCCEEDED.
7. StartQueryExecution `SELECT id, name FROM "<db>"."<tbl>" ORDER BY id` and poll GetQueryExecution: observe State FAILED with Status.StateChangeReason `GENERIC_USER_ERROR: Detected Iceberg type table without metadata location. ...` instead of SUCCEEDED with the three inserted rows.
8. After a fix, re-replay behavioral check athena.iceberg-time-travel-returns-the-snapshot-before-the-row-level-update end to end: the post-INSERT SELECT must return the three rows, `SELECT ... FROM "<db>"."<tbl>$snapshots"` must list the commits as one parent chain, a row-level `UPDATE ... WHERE id = 2` and `DELETE ... WHERE id = 3` must commit, and `SELECT ... FOR VERSION AS OF <root snapshot id>` must still return the pre-update rows.
