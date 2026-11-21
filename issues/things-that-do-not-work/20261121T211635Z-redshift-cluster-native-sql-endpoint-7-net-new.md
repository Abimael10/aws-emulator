Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift.rsql-ddl-dml-readback-visible-to-a-second-session redshift.rsql-not-null-violation-rejected redshift.rsql-distkey-changes-the-join-plan redshift.redshift-native-sql-rollback-leaves-only-the-committed-rows redshift.get-cluster-credentials-temporary-scoped-access redshift.redshift-native-sql-null-into-a-not-null-column-rejected-and-the-row-absent redshift.rsql-transaction-rollback-leaves-no-row
Issue-Shape-Id: com.amazonaws.redshift#RedshiftServiceVersion20121201
Issue-Key: redshift.rsql-ddl-dml-readback-visible-to-a-second-session|run#7|AssertionError|assert "Redshift" in engine, (
Issue-Key: redshift.rsql-not-null-violation-rejected|_rows#7|UndefinedTable|cur.execute(sql, args)
Issue-Key: redshift.rsql-distkey-changes-the-join-plan|run#9|InternalError_|cur.execute(
Issue-Key: redshift.redshift-native-sql-rollback-leaves-only-the-committed-rows|_rows#10|UndefinedTable|cur.execute(statement, args)
Issue-Key: redshift.get-cluster-credentials-temporary-scoped-access|_current_user#20|InternalError_|cursor.execute("SELECT current_user")
Issue-Key: redshift.redshift-native-sql-null-into-a-not-null-column-rejected-and-the-row-absent|run#14|AssertionError|assert code == "XX000", (
Issue-Key: redshift.rsql-transaction-rollback-leaves-no-row|_rows#9|SyntaxError|cur.execute(sql, args)
Issue-Commit: 8bae25eab1a2870033e48170871633376c17feec

# redshift cluster native SQL endpoint: 7 net-new causes — version() is not Redshift, no pg_table_def/information_schema/current_user, DISTSTYLE/DISTKEY/SORTKEY unparsed, NOT NULL refusal is SQLite's 23502, and BEGIN fails inside an open transaction

> _Note: this issue was identified against the code base at commit `8bae25eab1a2870033e48170871633376c17feec`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCluster (the cluster's native SQL data plane on the published endpoint)

## Description
Replay of simparity-behavioral-aws-v1 against the emulated Redshift cluster's NATIVE SQL endpoint (psycopg over the PostgreSQL wire to the Address/Port that DescribeClusters publishes, not redshift-data). This bundle carries SEVEN causes, all in the cluster SQL engine and its catalog behind that endpoint; ALL SEVEN must be closed before submitting. Related but NOT covering any of them: the open issues 20261029T193810Z-redshift-cluster-sql-engine-multi-statement-sql-refused.md (pg_user/stv_blocklist, COPY, ENCODE, INTERVAL, via ExecuteStatement), 20261114T083409Z-redshift-cluster-sql-engine-3-net-new-causes.md and 20261116T000944Z-redshift-cluster-sql-engine-2-net-new-causes.md. None of the seven faces below is named in those issues, and the suite marked none of these checks ALREADY FILED or OWNED.

=== Cause 1 of 7: SELECT version() does not identify the engine as Redshift ===
AWS: recorded behavior only (no Smithy surface describes SQL). A Redshift cluster answers SELECT version() with a string that contains 'Redshift' (Redshift reports a PostgreSQL-compatibility banner followed by its own 'Redshift <version>' marker). Clients and drivers branch on that marker.
Emulator: the endpoint answers SQL but version() returns a bare engine version with no Redshift marker — observed '8.0.2'.
Why: every client, driver and ORM that detects Redshift (to pick Redshift dialect, to avoid unsupported PostgreSQL features, to choose DISTKEY-aware DDL) mis-detects the server and then sends the wrong SQL for the rest of the session.
Fix: the native SQL endpoint's version/server-identity reporting (version() and the startup server_version parameter) — report a Redshift-identifying banner.
Traces: redshift.rsql-ddl-dml-readback-visible-to-a-second-session.

=== Cause 2 of 7: the catalog view pg_table_def does not exist ===
AWS: recorded behavior only. A Redshift database exposes pg_table_def, with one row per column of a table and the columns "column", type, "notnull", distkey, sortkey — e.g. SELECT "column", type, "notnull" FROM pg_table_def WHERE tablename = '<t>' returns ('id','integer',true), ('label','character varying(64)',false), ('amount','numeric(9,2)',false) for CREATE TABLE <t> (id INT NOT NULL, label VARCHAR(64), amount DECIMAL(9,2)), and the rows disappear when the table is dropped.
Emulator: the query is refused — SQLSTATE 42P01 with message 'no such table: pg_table_def' (SQLite's wording; real Redshift would say 'relation "pg_table_def" does not exist' if it truly had no such relation).
Why: pg_table_def is THE Redshift way to inspect a table's declared types, NOT NULL flags and distribution/sort keys; every introspection, migration-diff and schema-validation tool fails outright, and nothing about declared DDL can be verified by a client.
Fix: the cluster SQL engine's catalog component — back pg_table_def with the engine's own table/column state (including distkey/sortkey and the Redshift type spellings).
Traces: redshift.rsql-not-null-violation-rejected, redshift.rsql-ddl-dml-readback-visible-to-a-second-session, redshift.rsql-distkey-changes-the-join-plan.

=== Cause 3 of 7: information_schema.columns does not exist ===
AWS: recorded behavior only. Redshift serves the SQL-standard information_schema views; SELECT column_name, data_type, is_nullable, character_maximum_length FROM information_schema.columns WHERE table_name = '<t>' ORDER BY ordinal_position returns ('id','integer','NO',NULL), ('val','character varying','NO',32) for CREATE TABLE <t> (id INT NOT NULL, val VARCHAR(32) NOT NULL).
Emulator: refused — SQLSTATE 42P01, 'no such table: information_schema.columns'; the qualified name is also treated as one opaque identifier rather than schema.table.
Why: information_schema is the portable catalog every framework (SQLAlchemy, Flyway/Liquibase, BI tools) reads before touching data; with it missing, such a client cannot discover a single column.
Fix: the cluster SQL engine's catalog component and its schema-qualified name resolution — expose information_schema.columns (and its sibling views) over the engine's own DDL state.
Traces: redshift.redshift-native-sql-rollback-leaves-only-the-committed-rows.

=== Cause 4 of 7: current_user is not implemented ===
AWS: recorded behavior only. SELECT current_user on a Redshift connection returns the name of the authenticated database user (for a GetClusterCredentials login, the DbUser that was minted).
Emulator: the statement is refused with an internal error — 'no such column: current_user in SELECT current_user at offset 7' — i.e. the keyword is parsed as a column reference instead of the current-user function.
Why: current_user (and session_user/current_database) is the standard way a client confirms which identity a credential logged it in as; every connection-pool validation query and every multi-tenant check that pins its identity fails. It also makes the GetClusterCredentials scoping contract unobservable.
Fix: the cluster SQL engine's expression layer — implement the current_user / session_user / current_database niladic functions bound to the authenticated session's identity.
Traces: redshift.get-cluster-credentials-temporary-scoped-access.

=== Cause 5 of 7: DISTSTYLE / DISTKEY(...) / SORTKEY(...) in CREATE TABLE is a parser error ===
AWS: recorded behavior only. Redshift's CREATE TABLE accepts the distribution and sort clauses — CREATE TABLE t (fid INT NOT NULL, dkey INT NOT NULL, amt DECIMAL(9,2)) DISTSTYLE KEY DISTKEY(dkey) SORTKEY(fid), and DISTSTYLE EVEN — the statement succeeds, and the keys are then reported per column by pg_table_def (distkey=true for the DISTKEY column, sortkey=1 for the first sort column) and per table by svv_table_info.diststyle.
Emulator: the statement is refused with an internal error of the form 'near "KEY": syntax error in <the whole statement> at offset <position of the keyword>' — the clause is never tokenised, so no such table can be created at all.
Why: DISTSTYLE/DISTKEY/SORTKEY appear in essentially every real Redshift schema; the DDL of a production warehouse cannot be applied against the emulator, and nothing that depends on distribution (colocated vs broadcast joins, catalog-visible keys) can exist.
Fix: the cluster SQL engine's CREATE TABLE parser plus the catalog state it writes — accept the table-level DISTSTYLE/DISTKEY/SORTKEY clauses (and the column-level DISTKEY/SORTKEY forms), record them, and report them through pg_table_def/svv_table_info.
Traces: redshift.rsql-distkey-changes-the-join-plan.

=== Cause 6 of 7: a NULL into a NOT NULL column is refused with PostgreSQL's 23502 and SQLite's message instead of Redshift's XX000 / code 8007 ===
AWS: recorded behavior only. Redshift refuses INSERT INTO t (id, val) VALUES (2, NULL) against a NOT NULL column with SQLSTATE XX000 (internal_error), an ErrorResponse whose first line names the offending column, and a DETAIL block carrying Redshift's own numeric code on a 'code: 8007' line. Redshift deliberately does NOT use PostgreSQL's 23502 here. The same refusal must apply to a bound NULL parameter, and the refused row must not land.
Emulator: SQLSTATE '23502' with the message 'NOT NULL constraint failed: <table>.<column>' — SQLite's wording and SQLSTATE; there is no 'code: 8007' DETAIL line. (The table/column in the observed run were sim2real_native_notnull.val; those are per-run values — the fixed parts are the SQLSTATE XX000, the 'code: 8007' DETAIL, and that the first line names the column from the statement.)
Why: Redshift clients classify this failure on XX000 + code 8007; a 23502 with foreign text is matched by no Redshift error handler, and any code path that distinguishes a Redshift internal refusal from a PostgreSQL constraint violation takes the wrong branch.
Fix: the cluster SQL engine's error-translation layer for the native endpoint — map the embedded engine's constraint diagnostics onto Redshift's SQLSTATE/code/message shape (column name taken from the statement) before putting them on the wire.
Traces: redshift.redshift-native-sql-null-into-a-not-null-column-rejected-and-the-row-absent.

=== Cause 7 of 7: BEGIN is refused when the engine already holds a transaction open, so a client cannot drive an explicit transaction ===
AWS: recorded behavior only. Over the PostgreSQL wire a Redshift session is always in a well-defined transaction state: a driver that leaves autocommit (psycopg setting autocommit=False) emits BEGIN before its next statement and the server accepts it; the session can then INSERT, read its own uncommitted work, have a second session see none of it (read-committed), and ROLLBACK — including the DDL done inside the transaction.
Emulator: the BEGIN the driver emits after a non-autocommit switch is refused with a syntax-class error 'cannot start a transaction within a transaction' — the engine still considers an earlier transaction open and rejects the new one, so the whole explicit-transaction sequence dies at its first statement.
Why: explicit transactions are the foundation of every writer; a client that turns autocommit off cannot execute anything, and the isolation/rollback contract (uncommitted rows invisible to other sessions, rollback discarding rows and DDL) can never be exercised.
Fix: the native SQL endpoint's session/transaction state machine — track each connection's transaction status as the PostgreSQL protocol defines it (idle vs in-transaction, reported in ReadyForQuery), so an implicit statement does not leave a transaction open and a client's BEGIN/COMMIT/ROLLBACK is honoured per session.
Traces: redshift.rsql-transaction-rollback-leaves-no-row.

## Evidence
Suite: simparity-behavioral-aws-v1 — redshift 155/197 (79%), 42 failed (41 of them OWNED/already filed and skipped; the 7 below plus CreateSnapshotCopyGrant are net-new).

Cause 1 — check_redshift_rsql_ddl_dml_readback_visible_to_a_second_session.py:299
  expected: SELECT version() -> a string containing 'Redshift'
  actual:   AssertionError: endpoint answered SQL but does not identify as Redshift: '8.0.2'

Cause 2 — check_redshift_rsql_not_null_violation_rejected.py:225 (_rows)
  statement: SELECT "column", "notnull" FROM pg_table_def WHERE tablename = %s
  actual:    psycopg.errors.UndefinedTable: no such table: pg_table_def
  (the same view is read by rsql-ddl-dml-readback-visible-to-a-second-session:'SELECT "column", type, "notnull" FROM pg_table_def ...' and by rsql-distkey-changes-the-join-plan:'SELECT "column", distkey, sortkey FROM pg_table_def ...')

Cause 3 — check_redshift_redshift_native_sql_rollback_leaves_only_the_committed_rows.py:243 (_rows)
  statement: SELECT column_name, data_type, is_nullable, character_maximum_length FROM information_schema.columns WHERE table_name = %s ORDER BY ordinal_position
  expected:  [('id','integer','NO',None), ('val','character varying','NO',32)]
  actual:    psycopg.errors.UndefinedTable: no such table: information_schema.columns

Cause 4 — check_redshift_get_cluster_credentials_temporary_scoped_access.py:260 (_current_user)
  statement: SELECT current_user
  actual:    psycopg.errors.InternalError_: no such column: current_user in SELECT current_user at offset 7

Cause 5 — check_redshift_rsql_distkey_changes_the_join_plan.py:321
  statement: CREATE TABLE <fact> (fid INT NOT NULL, dkey INT NOT NULL, amt DECIMAL(9,2)) DISTSTYLE KEY DISTKEY(dkey) SORTKEY(fid)
  actual:    psycopg.errors.InternalError_: near "KEY": syntax error in CREATE TABLE rsql_fact_7bafc12029 (...) DISTSTYLE KEY DISTKEY(dkey) SORTKEY(fid) at offset 100
  (table name and offset are per-run; the following 'DISTSTYLE KEY DISTKEY(dkey)' and 'DISTSTYLE EVEN' creates are equally affected)

Cause 6 — check_redshift_redshift_native_sql_null_into_a_not_null_column_rejected_and_the_row_absent.py:316
  statement: INSERT INTO <t> (id, val) VALUES (2, NULL)   -- val declared VARCHAR(32) NOT NULL
  expected:  SQLSTATE 'XX000', first line of the ErrorResponse names 'val', DETAIL contains a line 'code: 8007'; same for the bound-parameter form
  actual:    AssertionError: ... The engine raised '23502': 'NOT NULL constraint failed: sim2real_native_notnull.val'

Cause 7 — check_redshift_rsql_transaction_rollback_leaves_no_row.py:267 (_rows)
  sequence:  autocommit=True CREATE TABLE/INSERT/SELECT, a second session reads and commits, then conn.autocommit=False and INSERT ... (psycopg emits BEGIN)
  expected:  the BEGIN is accepted; the writer sees its own uncommitted row, the observer does not, ROLLBACK discards the row and the in-transaction CREATE TABLE
  actual:    psycopg.errors.SyntaxError: cannot start a transaction within a transaction

(cluster ids, endpoint ports, generated table names, uuid suffixes and byte offsets above are per-run examples; the requirements are the catalog relations, the accepted syntax, the SQLSTATE/code/message shape and the per-session transaction state.)

## Steps to Reproduce
1. Clone the repo and boot the emulator. CreateCluster a single-node Redshift cluster (DBName as the check sets it), wait for 'available', and read Endpoint.Address/Endpoint.Port from DescribeClusters.
2. Connect to that endpoint with psycopg (the master user/password used at create).
3. Cause 1: SELECT version() -> expect a string containing 'Redshift'; observe '8.0.2'.
4. Cause 2: CREATE TABLE t (id INT NOT NULL, label VARCHAR(64), amount DECIMAL(9,2)); then SELECT "column", type, "notnull" FROM pg_table_def WHERE tablename = 't' -> expect three rows; observe 'no such table: pg_table_def'.
5. Cause 3: SELECT column_name, data_type, is_nullable, character_maximum_length FROM information_schema.columns WHERE table_name = 't' ORDER BY ordinal_position -> expect the declared columns; observe 'no such table: information_schema.columns'.
6. Cause 4: SELECT current_user -> expect the logged-in DB user; observe 'no such column: current_user'.
7. Cause 5: CREATE TABLE f (fid INT NOT NULL, dkey INT NOT NULL, amt DECIMAL(9,2)) DISTSTYLE KEY DISTKEY(dkey) SORTKEY(fid) -> expect success and distkey/sortkey visible in pg_table_def and svv_table_info; observe 'near "KEY": syntax error'.
8. Cause 6: CREATE TABLE nn (id INT NOT NULL, val VARCHAR(32) NOT NULL); INSERT INTO nn (id, val) VALUES (2, NULL) -> expect SQLSTATE XX000 naming 'val' with a 'code: 8007' DETAIL line; observe SQLSTATE 23502 'NOT NULL constraint failed: nn.val'. Repeat with a bound NULL parameter.
9. Cause 7: with autocommit=True do a CREATE TABLE + INSERT + SELECT, then set autocommit=False and INSERT again -> expect the driver's BEGIN to be accepted, the row invisible to a second session, and rollback() to discard the row and any in-transaction DDL; observe 'cannot start a transaction within a transaction'.
10. Re-replay every check id in trace_ids to confirm the fix.
