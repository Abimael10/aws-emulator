Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: redshift.rsql-ddl-dml-readback-visible-to-a-second-session redshift.rsql-distkey-changes-the-join-plan redshift.redshift-native-sql-null-into-a-not-null-column-rejected-and-the-row-absent redshift.rsql-transaction-rollback-leaves-no-row redshift.get-cluster-credentials-temporary-scoped-access
Issue-Shape-Id: com.amazonaws.redshift#RedshiftServiceVersion20121201
Issue-Key: redshift.rsql-ddl-dml-readback-visible-to-a-second-session|_rows#7|UndefinedTable|cur.execute(sql, args)
Issue-Key: redshift.rsql-distkey-changes-the-join-plan|run#9|SyntaxError|cur.execute(
Issue-Key: redshift.redshift-native-sql-null-into-a-not-null-column-rejected-and-the-row-absent|run#15|AssertionError|assert _internal_code(message) == "8007", (
Issue-Key: redshift.rsql-transaction-rollback-leaves-no-row|run#9|AssertionError|assert _rows(ocur, f"SELECT id FROM {table} ORDER BY id") == [(1,)], (
Issue-Key: redshift.get-cluster-credentials-temporary-scoped-access|run#21|AssertionError|assert current_a == f"IAM:{db_user_a}", (
Issue-Commit: 9f15073fe42cc4661a8a99924cff5eec339d1b59

# redshift cluster native SQL endpoint: 5 net-new causes — pg_table_def still absent, DISTSTYLE/DISTKEY/SORTKEY unparsed, NOT NULL refusal carries no 'code: 8007' DETAIL, uncommitted rows are visible to a second session, and an AutoCreate credential logs in as 'IAMA:<user>'

> _Note: this issue was identified against the code base at commit `9f15073fe42cc4661a8a99924cff5eec339d1b59`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
CreateCluster (the cluster's native PostgreSQL-wire data plane on the published Endpoint.Address/Port)

## Description
Replay of simparity-behavioral-aws-v1 against the emulated Redshift cluster's NATIVE SQL endpoint (psycopg over the PostgreSQL wire to the Address/Port DescribeClusters publishes, not redshift-data). This bundle carries FIVE causes; ALL FIVE must be closed before submitting. The suite marked none of these checks ALREADY FILED or OWNED. Related but NOT covering them: open issue 20261121T211635Z-redshift-cluster-native-sql-endpoint-7-net-new.md (its causes 2/5/6 named the same three faces at earlier failure points — version() and the BEGIN-in-transaction refusal it filed are now past, so the checks fail further on; causes 4 and 5 below are different faces entirely), 20261029T193810Z-redshift-cluster-sql-engine-multi-statement-sql-refused.md, 20261104T045922Z-redshift-publishes-a-cluster-endpoint-but-serves-no.md.

=== Cause 1 of 5: the catalog view pg_table_def does not exist ===
AWS: recorded behavior only (no Smithy surface describes SQL). A Redshift database exposes pg_table_def with one row per column of a table, carrying the columns "column", type, "notnull", distkey, sortkey; SELECT "column", type, "notnull" FROM pg_table_def WHERE tablename = '<t>' returns one row per declared column with the Redshift type spellings, and the rows disappear when the table is dropped. Where a relation genuinely does not exist, Redshift refuses with SQLSTATE 42P01 and the PostgreSQL wording relation "<name>" does not exist (fixed text; <name> is the relation from the statement).
Emulator: the query is refused — psycopg.errors.UndefinedTable with the message 'no such table: pg_table_def' (SQLite's wording, not Redshift's).
Why: pg_table_def is THE Redshift way to inspect declared types, NOT NULL flags and distribution/sort keys; every introspection, migration-diff and schema-validation client fails outright, and the message it does get is not one any Redshift/PostgreSQL error handler recognises.
Fix: the cluster SQL engine's catalog component for the native endpoint — back pg_table_def with the engine's own table/column state, and route genuine missing-relation diagnostics through the PostgreSQL wording.
Traces: redshift.rsql-ddl-dml-readback-visible-to-a-second-session (psycopg.errors.UndefinedTable at _rows, check line 232).

=== Cause 2 of 5: DISTSTYLE / DISTKEY(...) / SORTKEY(...) in CREATE TABLE is still a parser error ===
AWS: recorded behavior only. Redshift's CREATE TABLE accepts the table-level distribution and sort clauses — CREATE TABLE t (fid INT NOT NULL, dkey INT NOT NULL, amt DECIMAL(9,2)) DISTSTYLE KEY DISTKEY(dkey) SORTKEY(fid), and DISTSTYLE EVEN — the statement succeeds and the keys are then reported per column by pg_table_def (distkey true for the DISTKEY column, sortkey 1 for the first sort column) and per table by svv_table_info.diststyle ('KEY(<col>)', 'EVEN', 'ALL').
Emulator: the statement is refused — psycopg.errors.SyntaxError 'near "KEY": syntax error'. The clause is never tokenised, so the table cannot be created at all (the error class has moved from InternalError_ to SyntaxError since the earlier filing, but the clause is still unparsed).
Why: DISTSTYLE/DISTKEY/SORTKEY appear in essentially every production Redshift schema; such DDL cannot be applied against the emulator, and nothing that depends on distribution (catalog-visible keys, colocated vs broadcast joins) can exist.
Fix: the cluster SQL engine's CREATE TABLE parser plus the catalog state it writes — accept the table-level and column-level DISTSTYLE/DISTKEY/SORTKEY forms, record them, and report them through pg_table_def and svv_table_info.
Traces: redshift.rsql-distkey-changes-the-join-plan (psycopg.errors.SyntaxError at check line 321).

=== Cause 3 of 5: a NULL into a NOT NULL column is refused without Redshift's 'code: 8007' DETAIL and with SQLite's message ===
AWS: recorded behavior only. Redshift refuses INSERT INTO t (id, val) VALUES (2, NULL) against a NOT NULL column as an internal_error (SQLSTATE XX000) whose first line names the offending column and whose DETAIL block carries Redshift's own numeric code on a line of the fixed form 'code: 8007' (plus context/query/location lines). Redshift deliberately does not surface PostgreSQL's 23502 here. The fixed parts are the XX000 class and the 'code: 8007' DETAIL line; the table and column names are values taken from the statement. The refused row must not land.
Emulator: the refusal carries the message 'NOT NULL constraint failed: <table>.<column>' — the embedded engine's (SQLite's) wording — with no DETAIL block at all, so no numeric Redshift code can be parsed out of it (the check parsed None where 8007 is required).
Why: Redshift clients classify this failure on the internal code 8007 in DETAIL; a bare constraint sentence is matched by no Redshift error handler, so retry/branching logic that distinguishes a Redshift internal refusal from a portable constraint violation takes the wrong branch and the offending column is not machine-readable.
Fix: the cluster SQL engine's error-translation layer for the native endpoint — map the embedded engine's constraint diagnostics onto Redshift's message/DETAIL shape (column name from the statement, 'code: 8007' DETAIL) before putting them on the wire.
Traces: redshift.redshift-native-sql-null-into-a-not-null-column-rejected-and-the-row-absent (AssertionError at check line 321).

=== Cause 4 of 5: read-committed isolation is not honoured — a second session sees another session's UNCOMMITTED rows ===
AWS: recorded behavior only. A Redshift cluster runs each session's explicit transaction in isolation: rows inserted by a transaction that has not committed are invisible to every other connection, which continues to see only the committed rows; after the writer ROLLBACKs, the row never appears anywhere.
Emulator: while the writing session's transaction is still open, a concurrent session's SELECT over the same table returns the uncommitted row as well as the committed one — the observer saw more rows than the committed set, so writes are visible across connections before COMMIT.
Why: any client with more than one connection (a connection pool, a reader alongside a writer, an ETL job reading while a load is in flight) reads rows that may be rolled back and never existed; dirty reads make every read-modify-write and every correctness test on the emulator unsound.
Fix: the native SQL endpoint's session/transaction and visibility layer — give each wire connection its own transaction scope over the engine (per-session transaction state and read-committed visibility) instead of sharing one engine handle whose writes land globally on execution.
Traces: redshift.rsql-transaction-rollback-leaves-no-row (AssertionError 'a concurrent session can see the UNCOMMITTED row -- read-committed isolation is not being honoured', check line 347).

=== Cause 5 of 5: a GetClusterCredentials AutoCreate login authenticates as 'IAMA:<DbUser>' instead of the plain 'IAM:<DbUser>' identity ===
AWS: Smithy GetClusterCredentialsMessage.DbUser documents that with Autocreate=True a new database user is created using the value for DbUser; the credential the call returns (ClusterCredentials.DbUser) is the login name, which for AutoCreate carries the 'IAMA:' marker. The marker is only the auto-create instruction consumed at login: the database user that exists, and that the SQL layer reports for SELECT current_user, is the plain 'IAM:<DbUser>' form, regardless of AutoCreate. The prefixes are fixed text; <DbUser> is the value from the request.
Emulator: ExecuteStatement/native login with the returned credential authenticates as 'IAMA:scopedusera' — the login string is stored verbatim as the database user, so current_user reports the IAMA-prefixed name and no 'IAM:<DbUser>' identity ever exists. (current_user itself now answers, which the earlier filing's cause 4 covered; the identity it reports is the net-new divergence.)
Why: every GRANT/REVOKE, every row-level or schema-level permission and every audit record a customer writes against the documented 'IAM:<user>' identity applies to a user the emulator never creates, and a client that pins its identity by current_user sees a name AWS never returns.
Fix: the GetClusterCredentials path and the native endpoint's authentication layer — strip the 'IAMA:' marker at login, create/resolve the database user as 'IAM:<DbUser>', and bind the session identity (current_user/session_user) to that plain form.
Traces: redshift.get-cluster-credentials-temporary-scoped-access (AssertionError at check line 80).

## Evidence
Suite: simparity-behavioral-aws-v1 — total 200/264 (75.8%); redshift 157/197 (80%), 40 failed (33 of them OWNED/ALREADY FILED and skipped).

Cause 1 — check_redshift_rsql_ddl_dml_readback_visible_to_a_second_session.py:232 (_rows)
  statement: a SELECT of "column", type, "notnull" FROM pg_table_def WHERE tablename = %s
  expected:  one row per declared column of the table just created
  actual:    psycopg.errors.UndefinedTable: no such table: pg_table_def

Cause 2 — check_redshift_rsql_distkey_changes_the_join_plan.py:321
  statement: CREATE TABLE <fact> (fid INT NOT NULL, dkey INT NOT NULL, amt DECIMAL(9,2)) DISTSTYLE KEY DISTKEY(dkey) SORTKEY(fid)
  expected:  accepted; pg_table_def reports {fid:(distkey False, sortkey 1), dkey:(True,0), amt:(False,0)} and svv_table_info reports KEY(dkey)/KEY(dkey)/EVEN for the three tables
  actual:    psycopg.errors.SyntaxError: near "KEY": syntax error

Cause 3 — check_redshift_redshift_native_sql_null_into_a_not_null_column_rejected_and_the_row_absent.py:321
  statement: INSERT of NULL into a NOT NULL VARCHAR column
  expected:  the refusal's DETAIL block carries Redshift's numeric code on a 'code: 8007' line (XX000 internal_error), first line naming the column
  actual:    AssertionError: "the refusal must carry Redshift's own numeric error code 8007 in its DETAIL block ... Parsed None out of 'NOT NULL constraint failed: sim2real_native_notnull.val'"
  (sim2real_native_notnull.val is this run's generated table/column; the requirement is the DETAIL 'code: 8007' line and the column name taken from the statement)

Cause 4 — check_redshift_rsql_transaction_rollback_leaves_no_row.py:347
  sequence:  session A commits row id=1; session A opens a transaction and INSERTs row id=2 without committing; session B (separate connection) SELECTs id FROM <t> ORDER BY id
  expected:  [(1,)]
  actual:    AssertionError: a concurrent session can see the UNCOMMITTED row -- read-committed isolation is not being honoured

Cause 5 — check_redshift_get_cluster_credentials_temporary_scoped_access.py:80
  sequence:  GetClusterCredentials(ClusterIdentifier=<c>, DbUser=scopedusera, DbName=dev, AutoCreate=True, DurationSeconds=900) -> DbUser 'IAMA:scopedusera'; log in over the wire with that name and password; SELECT current_user
  expected:  'IAM:scopedusera'
  actual:    AssertionError: ... the database's SQL layer instead saw 'IAMA:scopedusera'

(cluster ids, ports, generated table names, uuid suffixes and the scopedusera/scopeduserb names above are per-run values; the requirements are the catalog relations, the accepted DDL syntax, the refusal message/DETAIL shape, cross-session visibility and the identity prefix.)

## Steps to Reproduce
1. Clone the repo and boot the emulator. CreateCluster a single-node Redshift cluster (DBName devdb, master user adminuser), wait for 'available', read Endpoint.Address/Endpoint.Port from DescribeClusters and connect with psycopg using the master credentials.
2. Cause 1: CREATE TABLE t (id INT NOT NULL, label VARCHAR(64), amount DECIMAL(9,2)); then SELECT "column", type, "notnull" FROM pg_table_def WHERE tablename = 't' -> expect one row per column; observe psycopg.errors.UndefinedTable 'no such table: pg_table_def'.
3. Cause 2: CREATE TABLE f (fid INT NOT NULL, dkey INT NOT NULL, amt DECIMAL(9,2)) DISTSTYLE KEY DISTKEY(dkey) SORTKEY(fid) -> expect success with the keys visible in pg_table_def/svv_table_info; observe 'near "KEY": syntax error'. Repeat with DISTSTYLE EVEN and with DISTSTYLE KEY DISTKEY(dkey) alone.
4. Cause 3: CREATE TABLE nn (id INT NOT NULL, val VARCHAR(32) NOT NULL); INSERT INTO nn (id, val) VALUES (2, NULL) -> expect an XX000 refusal naming 'val' with a 'code: 8007' DETAIL line; observe 'NOT NULL constraint failed: nn.val' with no DETAIL. Repeat with a bound NULL parameter and confirm no row landed.
5. Cause 4: on connection A, CREATE TABLE tx (id INT); INSERT (1) and COMMIT. Then on A turn autocommit off and INSERT (2) without committing. On a second connection B, SELECT id FROM tx ORDER BY id -> expect [(1,)]; observe the uncommitted row 2 as well. Then ROLLBACK on A and confirm the row is gone.
6. Cause 5: GetClusterCredentials(ClusterIdentifier=<c>, DbUser=scopedusera, DbName=dev, AutoCreate=True, DurationSeconds=900); log in over the wire with the returned DbUser ('IAMA:scopedusera') and DbPassword and run SELECT current_user -> expect 'IAM:scopedusera'; observe 'IAMA:scopedusera'.
7. Re-replay every check id in trace_ids to confirm the fix.
