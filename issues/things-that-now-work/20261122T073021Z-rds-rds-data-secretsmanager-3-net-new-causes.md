Issue-Severity: HIGH
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__rds-data__secretsmanager.aurora-mysql-backtrack-removes-a-later-data-api-write-from-both-surfaces rds__rds-data__secretsmanager.data-api-insert-is-read-by-a-native-postgres-session rds__rds-data__secretsmanager.data-api-rows-survive-cluster-snapshot-restore
Issue-Key: rds__rds-data__secretsmanager|ExecuteStatement|DatabaseErrorException
Issue-Key: rds__rds-data__secretsmanager.data-api-insert-is-read-by-a-native-postgres-session|_rows#2|UndefinedTable|cur.execute(statement, args)
Issue-Key: rds__rds-data__secretsmanager.data-api-rows-survive-cluster-snapshot-restore|_rows#19|UndefinedTable|cur.execute(statement)
Issue-Commit: 9f15073fe42cc4661a8a99924cff5eec339d1b59

# rds__rds-data__secretsmanager: 3 net-new causes — Aurora MySQL UTC_TIMESTAMP is unimplemented (SQLite 'no such function' leaks), the PostgreSQL wire has no information_schema, and a restored cluster's native volume lacks the table the Data API wrote before the snapshot

> _Note: this issue was identified against the code base at commit `9f15073fe42cc4661a8a99924cff5eec339d1b59`. If you are an issue fixer working on this, the code base you are currently working in can have drifted, so you might need to trace the issue back to the current code base._

## Operation
ExecuteStatement

## Description
Three net-new root causes in the SQL surface an Aurora cluster presents, reached now that a native session can finally connect to the published writer endpoint (the earlier blocker is the still-open 20261121T083106Z-rds-rds-data-secretsmanager-every-cross-surface-data.md, whose 22 PublishedUrlRejected replay keys no longer reproduce: the checks below now get real engine answers). Each cause is independent of the others and each has its own failing check.

Cause 1 of 3: the Data API's aurora-mysql dialect does not implement the MySQL UTC_TIMESTAMP function, and the refusal leaks the backing SQLite engine.
AWS: Smithy: rds-data ExecuteStatement (POST /Execute, success 200) carries DatabaseErrorException (client, HTTP 400) for an error the database itself raises. On Aurora MySQL, UTC_TIMESTAMP([fsp]) is a built-in date/time function (MySQL 8.0 reference, 'Date and Time Functions'): `SELECT UTC_TIMESTAMP(6)` is a successful statement that returns one row, one column, the engine's current UTC datetime with 6 fractional-second digits, which the Data API returns as 200 with records = [[{"stringValue": "YYYY-MM-DD HH:MM:SS.ffffff"}]] (the datetime itself is a clock value, never a requirement). No error is modelled for it, because it is not an error.
Emulator: the call is refused with HTTP 400 DatabaseErrorException and the message `no such function: UTC_TIMESTAMP in SELECT UTC_TIMESTAMP(6) at offset 7`. The fixed text of that message is the emulator's own (`no such function: ` ... ` in ` ... ` at offset `) — it is SQLite's unknown-function wording, not MySQL's; the variable parts are the function name, the statement text and a byte offset. MySQL's own refusal for a genuinely unknown function is error 1305 ER_SP_DOES_NOT_EXIST, `FUNCTION <db>.<name> does not exist`, so neither the wording nor the fact of the refusal matches AWS. The backing engine identity is disclosed to the caller.
Why: any client that asks an Aurora MySQL cluster for its own clock — which is how a workflow timestamps a write, computes a backtrack target, or compares its clock to the engine's — gets a hard 400 instead of a row, and a client that branches on the message learns it is talking to SQLite. The failing check cannot even choose a backtrack instant.
Fix: the SQL dialect/function registry behind rds-data ExecuteStatement for aurora-mysql clusters (the Data API's statement execution path in services/service-com-amazonaws-rdsdata-rdsdataservice, which today hands MySQL text to a SQLite engine); the built-in must exist, and any unknown-function refusal must be re-expressed in the engine's own wording rather than passed through.
Traces: rds__rds-data__secretsmanager.aurora-mysql-backtrack-removes-a-later-data-api-write-from-both-surfaces.

Cause 2 of 3: the cluster's native PostgreSQL wire serves no information_schema.
AWS: recorded behavior only for the engine wire (it is not a Smithy-modelled surface). Aurora PostgreSQL is PostgreSQL: the SQL-standard information_schema views exist in every database, so `SELECT column_name, data_type FROM information_schema.columns WHERE table_schema = 'public' AND table_name = $1 ORDER BY ordinal_position` returns one row per column of the named table — for a table declared (id BIGINT PRIMARY KEY, label TEXT NOT NULL, qty INT) the rows ('id','bigint'), ('label','text'), ('qty','integer'). The relation names and the catalog's type spellings are the requirement; the table name comes from the request.
Emulator: the native session's query is refused with PostgreSQL SQLSTATE 42P01 undefined_table, `relation "information_schema.columns" does not exist`. The relation name is the fixed part of that message; nothing about it is per-run. This is the same hole as the already-open 20261119T210508Z-rds-data-the-data-api-s-postgresql-engine.md reports for pg_catalog.pg_tables, but on the OTHER surface — that issue is scoped to the Data API's engine and carries no key for the native wire, so it does not cover this.
Why: every driver, ORM, migration tool and admin client introspects information_schema (or pg_catalog) before it does anything else; against this endpoint they all fail at connect-time introspection, so no standard PostgreSQL tooling can be pointed at an emulated Aurora cluster.
Fix: the system-catalog/information_schema layer of the RDS data plane's PostgreSQL engine (services/service-com-amazonaws-rds-amazonrdsv19/src/state/data_plane/, the SQL engine backing a served engine endpoint) — the DDL executed on a database must be reflected in queryable catalog views.
Traces: rds__rds-data__secretsmanager.data-api-insert-is-read-by-a-native-postgres-session.

Cause 3 of 3: RestoreDBClusterFromSnapshot reinstates the Data API's copy of the databases but not the volume the native wire serves, so a table written through the Data API before the snapshot does not exist for a native session on the restored cluster.
AWS: recorded behavior only for the cross-surface contract (the Data API is an HTTP front end over the cluster's one volume, per the Aurora Data API documentation). A DB cluster snapshot captures the cluster's single storage volume; a cluster restored from it serves exactly the committed pre-snapshot data on BOTH surfaces. So after the Data API commits `CREATE TABLE t (id BIGINT PRIMARY KEY, label TEXT NOT NULL)` plus two rows, snapshots, then commits a third row, the restored cluster answers rows 1 and 2 and not row 3 — through rds-data ExecuteStatement (HTTP 200) and through a native psycopg session on the restored cluster's Endpoint/Port alike.
Emulator: the Data API read of the restored cluster is correct (that assertion passes and returns exactly the two pre-snapshot rows), and the native read of the same table on the same restored cluster is refused with SQLSTATE 42P01, `relation "sim2real_snap_restore" does not exist` (the relation name is the table from the request; the message text is PostgreSQL's). The restore therefore installs only the Data API's per-ARN store and leaves the engine endpoint's volume empty. The open 20261119T000000Z-rds-rds-data-a-cluster-serves-two-databases-one-per-surface.md describes this two-store shape, but its replay key for this check is a `_wait_until#19` Timeout that no longer reproduces — the check now runs to the native read — so it does not carry this failure.
Why: a client that seeds a cluster through the Data API and verifies, or resumes work, over a driver after a restore finds its schema and data gone, with no error from the restore itself; backup/restore cannot be trusted for anything but the surface that wrote the data.
Fix: the cluster snapshot/restore data path and the ownership of a cluster's databases — services/service-com-amazonaws-rds-amazonrdsv19 (DbClusterSnapshotRecord storage and the restore that installs it onto a new cluster's engine endpoint) together with the Data API's per-cluster store in services/service-com-amazonaws-rdsdata-rdsdataservice/src/state.rs: one volume per cluster, served by both surfaces, so one copy is restored.
Traces: rds__rds-data__secretsmanager.data-api-rows-survive-cluster-snapshot-restore.

## Evidence
Replay of the behavioral suite on the cloned branch; group rds__rds-data__secretsmanager scored 14/23 (61%).

Cause 1 — check aurora-mysql-backtrack-removes-a-later-data-api-write-from-both-surfaces:
Expected: ExecuteStatement(sql="SELECT UTC_TIMESTAMP(6)") -> HTTP 200, one row holding the engine's UTC datetime as a stringValue.
Actual: botocore.errorfactory.DatabaseErrorException: An error occurred (DatabaseErrorException) when calling the ExecuteStatement operation: no such function: UTC_TIMESTAMP in SELECT UTC_TIMESTAMP(6) at offset 7
Raised from the check's _data_api_clock helper, which reads the engine clock before choosing a backtrack target.

Cause 2 — check data-api-insert-is-read-by-a-native-postgres-session:
Expected: the catalog query returns [('id','bigint'), ('label','text'), ('qty','integer')] for the table the Data API created. The preceding assertion — that a native session reads exactly the rows the Data API wrote, updated and deleted — PASSES, so data is now shared; only the catalog is missing.
Actual: psycopg.errors.UndefinedTable: relation "information_schema.columns" does not exist (check line 228, in _rows -> cur.execute).

Cause 3 — check data-api-rows-survive-cluster-snapshot-restore:
Expected: a native session on the restored cluster's writer returns the two pre-snapshot rows and not the post-snapshot row.
Actual: psycopg.errors.UndefinedTable: relation "sim2real_snap_restore" does not exist (check line 280, in _rows). The Data API SELECT on the same restored cluster immediately above returned exactly the two expected rows, so the divergence is confined to the restored cluster's native volume.

## Steps to Reproduce
1. Boot the emulator; point rds, rds-data, secretsmanager and ec2 clients at it.
2. CreateDBCluster(Engine="aurora-mysql", DatabaseName="simparitydb", MasterUsername="simparity", ManageMasterUserPassword=True, EnableHttpEndpoint=True, ServerlessV2ScalingConfiguration={MinCapacity:0.5,MaxCapacity:4}) + CreateDBInstance(DBInstanceClass="db.serverless"); wait for 'available'.
3. Cause 1: rds-data ExecuteStatement(resourceArn=<cluster ARN>, secretArn=<managed secret>, database="simparitydb", sql="SELECT UTC_TIMESTAMP(6)") — expect 200 with the UTC datetime; observe 400 DatabaseErrorException 'no such function: UTC_TIMESTAMP ...'.
4. Repeat step 2 with Engine="aurora-postgresql". Through the Data API: CREATE TABLE t (id BIGINT PRIMARY KEY, label TEXT NOT NULL, qty INT) and INSERT a row.
5. Cause 2: connect psycopg to the cluster's published Endpoint/Port with the managed secret's credentials and run SELECT column_name, data_type FROM information_schema.columns WHERE table_schema='public' AND table_name='t' ORDER BY ordinal_position — expect three catalog rows; observe SQLSTATE 42P01 relation "information_schema.columns" does not exist.
6. Cause 3: on an aurora-postgresql cluster, write a table plus two rows through the Data API, CreateDBClusterSnapshot, write a third row, RestoreDBClusterFromDBClusterSnapshot plus its writer instance, wait for 'available'. Read the table through rds-data ExecuteStatement on the restored cluster (two rows, correct), then read the same table over a native psycopg session on the restored cluster's Endpoint/Port — observe relation "<table>" does not exist.
