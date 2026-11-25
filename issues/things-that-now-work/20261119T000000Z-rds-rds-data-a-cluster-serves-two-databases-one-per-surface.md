Issue-Severity: MEDIUM
Issue-Corpus: simparity-behavioral-aws-v1
Issue-Corpus-Kind: behavioral
Issue-Trace-Ids: rds__rds-data__secretsmanager.data-api-rows-survive-cluster-snapshot-restore
Issue-Key: rds__rds-data__secretsmanager.data-api-rows-survive-cluster-snapshot-restore|_wait_until#19|Timeout|return last
Issue-Commit: 063461253139ad0d8135fbc578dd7b032b690175

# rds + rds-data: a DB cluster serves TWO databases, one per surface — a row written through the Data API is invisible to a native session on the same cluster (1 cause)

> _Split out of `issues/things-that-now-work/20261111T060457Z-rds-rds-data-secretsmanager-aurora-cluster-snapshot-restore-lifecycle-never-converges-a.md`, whose cause is fixed: every status that check polls settles (`DBClusterSnapshot$Status`, `DBCluster$Status`, `MasterUserSecret$SecretStatus`, `DBInstance$DBInstanceStatus`), and a DB cluster snapshot now CARRIES the databases the cluster served through the Data API while a restore installs them on the cluster it creates (`services/service-com-amazonaws-rdsdata-rdsdataservice/src/state/cluster_copies.rs`, `services/service-com-amazonaws-rds-amazonrdsv19/src/state/data_api_cluster_data.rs`), driven end to end in `core/router/tests/all/rds_cluster_snapshot_restore_lifecycle.rs`. That moved the check seven poll loops on, from `_wait_until#10` to `_wait_until#19`. This report carries the replay key that still does NOT pass, re-filed here so it is not recorded as working, and what that fix established about it; a later fixer starts from here rather than from scratch._

## Operation
ExecuteStatement (rds-data) and the DB cluster engine endpoint (rds data plane)

## Description
One cause, and it is a SHAPE of the emulator rather than a wrong answer from one operation.

AWS: an Aurora DB cluster holds ONE set of databases. The RDS Data API is an HTTP front end over that cluster's engine, so a row committed through `ExecuteStatement` is a row a session on the cluster's own endpoint (`Endpoint`/`Port`, a stock `psql`/`libpq`/psycopg client) then reads, and a row committed over that endpoint is a row the Data API reads. One storage volume, two surfaces.

Emulator: there are two stores, and nothing connects them.
* The Data API owns its own databases, keyed by the ARN of the cluster a call addresses: `ComAmazonawsRdsdataRdsdataserviceState::clusters` in `services/service-com-amazonaws-rdsdata-rdsdataservice/src/state.rs` (one in-process SQLite database per `(cluster ARN, database name)`).
* The RDS data plane owns the cluster's volume: `DbEngineEndpoint::database` / `data_plane::sql_engine::InstanceDatabase` in `services/service-com-amazonaws-rds-amazonrdsv19/src/state/data_plane/`, which is what the cluster's PostgreSQL and MySQL wires serve and what a DB cluster snapshot copies.

So a `CREATE TABLE`/`INSERT` through the Data API leaves nothing a native session can see, and a table a native session created is not a relation the Data API can read: each surface answers `relation "…" does not exist` (SQLSTATE 42P01) for the other's objects. Nothing is refused and no status is wrong — the data simply is not where the other surface looks.

Why: this is the contract of the Data API ("you can run SQL statements on an Aurora DB cluster … without managing connections"), so any workflow that mixes the two surfaces diverges: a client that seeds through the Data API and then connects a driver, an ORM migration run one way and read the other, and the snapshot/restore data check below, which reads the restored rows through BOTH.

Fix: make a DB cluster hold ONE database per name, served by both surfaces. The two candidate shapes, both wider than one operation:
* the Data API resolves the cluster's database through the service that OWNS the cluster's volume (a shared handle in the engine, the way `services/common/src/dialable_data_planes.rs` publishes a plane one service opened for another to reach), and runs its statements against that database instead of one of its own; or
* the Data API opens an in-process connection to the cluster's own engine endpoint — the listener `crate::state::open_engine_endpoint` already opened and the control plane already publishes — and speaks the engine's wire to it, which is what AWS's front end does.
Either way, the snapshot and restore copy already in place (`DbClusterSnapshotRecord::storage` plus `data_api_storage`) collapses to the single volume, and the per-surface copy can go.

Traces: rds__rds-data__secretsmanager.data-api-rows-survive-cluster-snapshot-restore (mismatch, times out at `_wait_until#19`).

## Evidence
Replay against the branch that served the parent report: the check still ends `Error: Check timed out after 60.0s`, now at `_wait_until#19` rather than `_wait_until#10` — the Data API legs all converge, and what does not is the leg that reads the restored rows through a native psycopg session.

What already passes, in the owning crates' own suites: the Data API half of the data contract end to end in `core/router/tests/all/rds_cluster_snapshot_restore_lifecycle.rs` (the restored cluster answers exactly the rows committed before the snapshot, over three snapshot instants and two databases), and the native half of the same rule over the PostgreSQL wire in `services/service-com-amazonaws-rds-amazonrdsv19/tests/all/db_cluster_snapshot_restore.rs`. Each is correct about its OWN surface, which is the cause: there are two surfaces.

## Steps to Reproduce
1. Boot the emulator; point rds, rds-data and secretsmanager clients at it.
2. CreateDBCluster(Engine="aurora-postgresql", DatabaseName=<db>, MasterUsername=<user>, ManageMasterUserPassword=True, EnableHttpEndpoint=True) + CreateDBInstance(DBInstanceClass="db.serverless"); poll until the cluster, the instance and the managed secret are settled.
3. Through the Data API: `CREATE TABLE t (id int PRIMARY KEY)` and `INSERT INTO t VALUES (1)`.
4. Connect a PostgreSQL driver to the cluster's reported `Endpoint`/`Port` with the managed secret's credentials and run `SELECT id FROM t`: expect the row written in step 3; observe `relation "t" does not exist`.
5. The mirror image: `CREATE TABLE u (id int PRIMARY KEY)` over that session, then `SELECT id FROM u` through the Data API — expect the relation, observe the same undefined-relation refusal.
