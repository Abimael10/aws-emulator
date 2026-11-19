// SPDX-License-Identifier: CC0-1.0

//! The Aurora DB cluster snapshot-and-restore lifecycle a client POLLS, and the data a
//! restored cluster serves, end to end on the ASSEMBLED emulator.
//!
//! RULE: every status an Amazon RDS resource is answered in while the service is still
//! working on it is a TRANSIENT status the clock settles, so each reaches its terminal value
//! in finite time with no further call — a DB cluster snapshot reaches `available`, a
//! restored DB cluster reaches `available`, a cluster's managed master user secret reaches
//! `active`, a DB instance reaches `available`. And a DB cluster snapshot is a copy of the
//! DATA the cluster held when it was taken — every database the cluster serves, whichever
//! surface wrote it — so a cluster restored from that snapshot serves exactly that data:
//! what was committed before the snapshot is on it, what was committed after is not, and
//! restoring one snapshot twice gives two clusters whose later writes are their own.
//!
//! The suite lives beside the router because the lifecycle spans three services: Amazon RDS
//! holds the cluster, the snapshot and the instance, Secrets Manager holds the managed
//! master user credential a Data API call authenticates with, and the RDS Data API owns the
//! databases the rows are written to and read back from. A crate-local suite can only ever
//! see one of the three.
//!
//! The rows differ along what the rule generalises over: the KIND of resource polled (a
//! snapshot, a cluster, a managed secret, a DB instance), whether the resource was created
//! outright or restored from a snapshot, WHEN the snapshot was taken relative to each write
//! (before anything, after two rows, after a third row and a second database), how many
//! databases the cluster holds, and how many clusters one snapshot is restored into.
//!
//! Sources: `smithy_query(query="operation", operation="CreateDBClusterSnapshot")` and
//! `smithy_query(query="operation", operation="RestoreDBClusterFromSnapshot")` on the rds
//! model for the two lifecycles, `smithy_query(query="shape", shape="MasterUserSecret")` for
//! `SecretStatus`, and `smithy_query(query="operation", operation="ExecuteStatement")` on the
//! rdsdata model for the call that reads the rows back.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, blocks_of, element, form_encode, opt_element};

/// The Region every request below is signed for.
const REGION: &str = "us-west-2";

/// The engine the whole lifecycle runs on, the one whose clusters serve the Data API.
const ENGINE: &str = "aurora-postgresql";

/// The database every cluster below is created with, and the one most statements run
/// against.
const DATABASE: &str = "simparitydb";

/// A SECOND database, created through the Data API itself: the rule is about every database
/// a cluster holds, so one of the rows below writes into one the create never named.
const SECOND_DATABASE: &str = "ledgerdb";

/// The master user every cluster below is created with.
const MASTER_USER: &str = "simparity";

/// The DB instance class an Aurora Serverless v2 writer is provisioned with
/// (`smithy_query(query="doc", shape="CreateDBInstanceMessage", member="DBInstanceClass")`).
const SERVERLESS_CLASS: &str = "db.serverless";

/// How often a poll looks again, and how long it keeps looking: every condition below is
/// owed by the CLOCK, so each poll waits for the CONDITION under a generous deadline rather
/// than for a fixed span.
const LOOK: Duration = Duration::from_millis(25);
const DEADLINE: Duration = Duration::from_secs(120);

/// The terminal values this lifecycle's statuses settle on: `DBClusterSnapshot$Status` and
/// `DBCluster$Status` (`available`), `MasterUserSecret$SecretStatus` (`active`) and
/// `DBInstance$DBInstanceStatus` (`available`).
const STATUS_AVAILABLE: &str = "available";
const SECRET_ACTIVE: &str = "active";

/// PostgreSQL's own code for a statement naming a relation the database does not hold
/// ("undefined_table", its "Error Codes" appendix) — what a restored cluster answers for a
/// table its snapshot did not carry.
const SQLSTATE_UNDEFINED_TABLE: &str = "42P01";

/// The table the rows are written to, in each database.
const TABLE: &str = "sim2real_snap_restore";

/// The emulator as it is ASSEMBLED, on one engine: the hops Amazon RDS makes to Secrets
/// Manager and the ones the Data API makes back to Amazon RDS are dispatched into this same
/// router.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One Amazon RDS awsQuery call, answered with the HTTP status and the XML body.
async fn rds(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2014-10-31");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("rds.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("rds"))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Amazon RDS call that must be served, answered with its XML body.
async fn served(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = rds(router, action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served, got {status} with {body}"
    );
    body
}

/// One Data API `ExecuteStatement`, answered with the HTTP status and the parsed body.
async fn execute(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/Execute")
        .header("host", format!("rds-data.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("rds-data"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// A cluster a Data API caller addresses: its ARN and the ARN of the secret holding the
/// credentials a call authenticates with.
#[derive(Debug, Clone)]
struct Addressed {
    arn: String,
    secret_arn: String,
}

impl Addressed {
    /// Run `sql` against `database` and answer the HTTP status and the parsed response.
    async fn statement_in(
        &self,
        router: &MultiServiceRouter,
        database: &str,
        sql: &str,
    ) -> (u16, Value) {
        execute(
            router,
            &json!({
                "resourceArn": self.arn,
                "secretArn": self.secret_arn,
                "database": database,
                "sql": sql,
            }),
        )
        .await
    }

    /// Run `sql` against `database`; it must be served.
    async fn run_in(
        &self,
        router: &MultiServiceRouter,
        case: &str,
        database: &str,
        sql: &str,
    ) -> Value {
        let (status, answered) = self.statement_in(router, database, sql).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {sql} on {database:?} must be served, got {status} with {answered}"
        );
        answered
    }

    /// Run `sql` against [`DATABASE`]; it must be served.
    async fn run(&self, router: &MultiServiceRouter, case: &str, sql: &str) -> Value {
        self.run_in(router, case, DATABASE, sql).await
    }

    /// Poll until the Data API ANSWERS a statement on this cluster: the classes a cluster
    /// that is still starting up is refused with are transient, so the call is retried under
    /// a generous deadline.
    async fn await_data_api(&self, router: &MultiServiceRouter, case: &str) {
        let deadline = Instant::now() + DEADLINE;
        loop {
            let (status, answered) = self.statement_in(router, DATABASE, "SELECT 1").await;
            if status == 200 {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: the Data API must answer on {:?}, last got {status} with \
                 {answered}",
                self.arn
            );
            tokio::time::sleep(LOOK).await;
        }
    }

    /// The ids the table in `database` holds, in key order.
    async fn ids_in(&self, router: &MultiServiceRouter, case: &str, database: &str) -> Vec<i64> {
        let answered = self
            .run_in(
                router,
                case,
                database,
                &format!("SELECT id FROM {TABLE} ORDER BY id"),
            )
            .await;
        ids(&answered)
    }

    /// Assert that this cluster serves NO such table in `database`: the data a snapshot did
    /// not carry is data the restored cluster does not have, which is a refusal and never an
    /// empty answer.
    async fn assert_no_table_in(&self, router: &MultiServiceRouter, case: &str, database: &str) {
        let (status, refused) = self
            .statement_in(router, database, &format!("SELECT id FROM {TABLE}"))
            .await;
        assert_eq!(
            status, 400,
            "case {case:?}: a table the snapshot did not carry must be refused in {database:?}, \
             got {status} with {refused}"
        );
        let message = refused
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert!(
            message.contains(TABLE) || message.contains(database),
            "case {case:?}: expected the refusal to name the relation or the database \
             ({SQLSTATE_UNDEFINED_TABLE} for a relation nothing created), got {message:?}"
        );
    }
}

/// Poll `DescribeDBClusters` until the cluster reports `available`.
async fn await_available_cluster(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = served(
            router,
            case,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", identifier)],
        )
        .await;
        let reported = opt_element(&read, "Status").unwrap_or_default();
        if reported == STATUS_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the DB cluster {identifier:?} must reach {STATUS_AVAILABLE:?}, last \
             reported {reported:?} in {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Poll `DescribeDBClusters` until the cluster's managed master user secret reports
/// `active`, answering that secret's ARN.
async fn await_active_secret(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = served(
            router,
            case,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", identifier)],
        )
        .await;
        if let Some(secret) = block_of(&read, "MasterUserSecret") {
            if opt_element(secret, "SecretStatus").unwrap_or_default() == SECRET_ACTIVE {
                return element(secret, "SecretArn");
            }
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the managed secret of {identifier:?} must reach {SECRET_ACTIVE:?}, \
             last read {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Poll `DescribeDBInstances` until the DB instance reports `available`.
async fn await_available_instance(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = served(
            router,
            case,
            "DescribeDBInstances",
            &[("DBInstanceIdentifier", identifier)],
        )
        .await;
        let reported = opt_element(&read, "DBInstanceStatus").unwrap_or_default();
        if reported == STATUS_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the DB instance {identifier:?} must reach {STATUS_AVAILABLE:?}, last \
             reported {reported:?} in {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Poll `DescribeDBClusterSnapshots` until the snapshot reports `available`.
async fn await_available_snapshot(router: &MultiServiceRouter, case: &str, identifier: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let read = served(
            router,
            case,
            "DescribeDBClusterSnapshots",
            &[("DBClusterSnapshotIdentifier", identifier)],
        )
        .await;
        let reported = opt_element(&read, "Status").unwrap_or_default();
        if reported == STATUS_AVAILABLE {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the DB cluster snapshot {identifier:?} must reach \
             {STATUS_AVAILABLE:?}, last reported {reported:?} in {read}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Create an Aurora Serverless v2 cluster with RDS managing the master user password and the
/// Data API turned on, and wait until that credential is usable.
async fn create_source_cluster(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Addressed {
    let created = served(
        router,
        case,
        "CreateDBCluster",
        &[
            ("DBClusterIdentifier", identifier),
            ("Engine", ENGINE),
            ("DatabaseName", DATABASE),
            ("MasterUsername", MASTER_USER),
            ("ManageMasterUserPassword", "true"),
            ("EnableHttpEndpoint", "true"),
            ("BackupRetentionPeriod", "1"),
            ("ServerlessV2ScalingConfiguration.MinCapacity", "0.5"),
            ("ServerlessV2ScalingConfiguration.MaxCapacity", "1.0"),
        ],
    )
    .await;
    let addressed = Addressed {
        arn: element(&created, "DBClusterArn"),
        secret_arn: await_active_secret(router, case, identifier).await,
    };
    await_available_cluster(router, case, identifier).await;
    addressed.await_data_api(router, case).await;
    addressed
}

/// Add a writer DB instance to `cluster` and wait until it is available.
async fn add_writer(router: &MultiServiceRouter, case: &str, cluster: &str, instance: &str) {
    served(
        router,
        case,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", instance),
            ("DBClusterIdentifier", cluster),
            ("Engine", ENGINE),
            ("DBInstanceClass", SERVERLESS_CLASS),
        ],
    )
    .await;
    await_available_instance(router, case, instance).await;
}

/// Take a DB cluster snapshot of `cluster` and wait until it is available.
async fn take_snapshot(router: &MultiServiceRouter, case: &str, cluster: &str, snapshot: &str) {
    served(
        router,
        case,
        "CreateDBClusterSnapshot",
        &[
            ("DBClusterSnapshotIdentifier", snapshot),
            ("DBClusterIdentifier", cluster),
        ],
    )
    .await;
    await_available_snapshot(router, case, snapshot).await;
}

/// Restore `snapshot` into a NEW cluster called `identifier`, wait for every status the
/// lifecycle owes, give the restored cluster a managed master user password of its own and
/// turn its Data API endpoint on — the sequence a client runs after a restore — and answer
/// the restored cluster as a Data API caller addresses it.
async fn restore_cluster(
    router: &MultiServiceRouter,
    case: &str,
    snapshot: &str,
    identifier: &str,
) -> Addressed {
    let restored = served(
        router,
        case,
        "RestoreDBClusterFromSnapshot",
        &[
            ("DBClusterIdentifier", identifier),
            ("SnapshotIdentifier", snapshot),
            ("Engine", ENGINE),
        ],
    )
    .await;
    let arn = element(&restored, "DBClusterArn");
    await_available_cluster(router, case, identifier).await;
    served(
        router,
        case,
        "ModifyDBCluster",
        &[
            ("DBClusterIdentifier", identifier),
            ("ManageMasterUserPassword", "true"),
            ("ApplyImmediately", "true"),
        ],
    )
    .await;
    let secret_arn = await_active_secret(router, case, identifier).await;
    await_available_cluster(router, case, identifier).await;
    served(
        router,
        case,
        "EnableHttpEndpoint",
        &[("ResourceArn", arn.as_str())],
    )
    .await;
    let addressed = Addressed { arn, secret_arn };
    addressed.await_data_api(router, case).await;
    addressed
}

/// The `id` column of every record of an answered `SELECT`, in the order the answer reports
/// them.
fn ids(answered: &Value) -> Vec<i64> {
    answered
        .get("records")
        .and_then(Value::as_array)
        .map(|records| {
            records
                .iter()
                .filter_map(|record| {
                    record
                        .as_array()
                        .and_then(|fields| fields.first())
                        .and_then(|field| field.get("longValue"))
                        .and_then(Value::as_i64)
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The whole lifecycle a client drives, in order, with every status it polls for: the
/// snapshot, the restored cluster, the restored cluster's managed secret and its writer all
/// reach their terminal value, and the restored cluster serves the rows the snapshot held
/// and not the one committed after it.
#[tokio::test]
async fn an_aurora_cluster_snapshot_and_restore_converge_and_carry_the_snapshots_rows() {
    let router = build_router();
    let source_cluster = "emu-snaprestore-src";
    let restored_cluster = "emu-snaprestore-dst";
    let snapshot = "emu-snaprestore-snap";
    let case = "the snapshot-and-restore lifecycle";

    let source = create_source_cluster(&router, case, source_cluster).await;
    add_writer(&router, case, source_cluster, "emu-snaprestore-src-1").await;

    // The rows the snapshot must carry, committed through the Data API before it is taken.
    for sql in [
        &format!("CREATE TABLE {TABLE} (id int PRIMARY KEY, label text)"),
        &format!("INSERT INTO {TABLE} (id, label) VALUES (1, 'before')"),
        &format!("INSERT INTO {TABLE} (id, label) VALUES (2, 'before')"),
    ] {
        source.run(&router, case, sql).await;
    }

    take_snapshot(&router, case, source_cluster, snapshot).await;

    // A row committed AFTER the snapshot, which the restored cluster must therefore not
    // carry.
    source
        .run(
            &router,
            case,
            &format!("INSERT INTO {TABLE} (id, label) VALUES (3, 'after')"),
        )
        .await;

    let restored = restore_cluster(&router, case, snapshot, restored_cluster).await;
    assert_ne!(
        restored.arn, source.arn,
        "case {case:?}: a restore creates a resource of its own, got {} for both",
        restored.arn
    );
    add_writer(&router, case, restored_cluster, "emu-snaprestore-dst-1").await;

    let reported = restored.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2],
        "case {case:?}: expected the rows committed before the snapshot on the restored \
         cluster, got {reported:?}"
    );
    // And the source cluster still carries all three: the restore is a cluster of its own,
    // so writing to one is not writing to the other.
    let reported = source.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2, 3],
        "case {case:?}: expected every committed row on the source cluster, got {reported:?}"
    );
    restored
        .run(
            &router,
            case,
            &format!("INSERT INTO {TABLE} (id, label) VALUES (4, 'restored')"),
        )
        .await;
    let reported = source.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2, 3],
        "case {case:?}: a write to the restored cluster is the restored cluster's alone, got \
         {reported:?} on the source"
    );

    // Both clusters are reported in the account's listing in their terminal status: the polls
    // above waited for a status the service really settled on, not for one read.
    let listed = served(&router, case, "DescribeDBClusters", &[]).await;
    for block in blocks_of(&listed, "DBCluster") {
        let identifier = opt_element(block, "DBClusterIdentifier").unwrap_or_default();
        if identifier != source_cluster && identifier != restored_cluster {
            continue;
        }
        let reported = opt_element(block, "Status").unwrap_or_default();
        assert_eq!(
            reported, STATUS_AVAILABLE,
            "case {identifier:?}: expected {STATUS_AVAILABLE:?} in the listing, got {reported:?}"
        );
    }
}

/// One checkpoint of the source cluster: what is committed before the snapshot is taken, and
/// what a cluster restored from that snapshot must then serve.
struct Checkpoint {
    case: &'static str,
    /// What runs on the source cluster BEFORE the snapshot, as `(database, statement)`.
    writes: &'static [(&'static str, &'static str)],
    /// The identifiers of the snapshot and of the cluster it is restored into.
    snapshot: &'static str,
    restored: &'static str,
    /// The ids the restored cluster's table must answer with in [`DATABASE`], or `None` when
    /// it must hold no such table at all.
    expected: Option<&'static [i64]>,
    /// The same for [`SECOND_DATABASE`].
    expected_second: Option<&'static [i64]>,
}

/// What a restored cluster serves is what its snapshot HELD, over the instants a snapshot
/// can be taken at: before anything was written, after two rows, and after a third row and a
/// second database the cluster was not created with.
#[tokio::test]
async fn what_a_restored_cluster_serves_is_what_its_snapshot_held() {
    let router = build_router();
    let source_cluster = "emu-snapheld-src";
    let source = create_source_cluster(&router, "the source cluster", source_cluster).await;

    let checkpoints: &[Checkpoint] = &[
        Checkpoint {
            case: "a snapshot taken before anything was written",
            writes: &[],
            snapshot: "emu-snapheld-none",
            restored: "emu-snapheld-dst-none",
            expected: None,
            expected_second: None,
        },
        Checkpoint {
            case: "a snapshot taken after two rows",
            writes: &[
                (
                    DATABASE,
                    "CREATE TABLE sim2real_snap_restore (id int PRIMARY KEY, label text)",
                ),
                (
                    DATABASE,
                    "INSERT INTO sim2real_snap_restore (id, label) VALUES (1, 'first')",
                ),
                (
                    DATABASE,
                    "INSERT INTO sim2real_snap_restore (id, label) VALUES (2, 'second')",
                ),
            ],
            snapshot: "emu-snapheld-two",
            restored: "emu-snapheld-dst-two",
            expected: Some(&[1, 2]),
            expected_second: None,
        },
        Checkpoint {
            case: "a snapshot taken after a third row and a second database",
            writes: &[
                (
                    DATABASE,
                    "INSERT INTO sim2real_snap_restore (id, label) VALUES (3, 'third')",
                ),
                (DATABASE, "CREATE DATABASE ledgerdb"),
                (
                    SECOND_DATABASE,
                    "CREATE TABLE sim2real_snap_restore (id int PRIMARY KEY, label text)",
                ),
                (
                    SECOND_DATABASE,
                    "INSERT INTO sim2real_snap_restore (id, label) VALUES (7, 'ledger')",
                ),
            ],
            snapshot: "emu-snapheld-three",
            restored: "emu-snapheld-dst-three",
            expected: Some(&[1, 2, 3]),
            expected_second: Some(&[7]),
        },
    ];

    for checkpoint in checkpoints {
        let case = checkpoint.case;
        for (database, sql) in checkpoint.writes {
            source.run_in(&router, case, database, sql).await;
        }
        take_snapshot(&router, case, source_cluster, checkpoint.snapshot).await;
        let restored =
            restore_cluster(&router, case, checkpoint.snapshot, checkpoint.restored).await;

        for (database, expected) in [
            (DATABASE, checkpoint.expected),
            (SECOND_DATABASE, checkpoint.expected_second),
        ] {
            match expected {
                Some(expected) => {
                    let reported = restored.ids_in(&router, case, database).await;
                    assert_eq!(
                        reported, expected,
                        "case {case:?}: expected {expected:?} in {database:?} on the restored \
                         cluster, got {reported:?}"
                    );
                }
                None => {
                    restored.assert_no_table_in(&router, case, database).await;
                }
            }
        }
    }

    // One snapshot restores MORE THAN ONCE, and each restored cluster's later writes are its
    // own: the copy a snapshot holds is not consumed by being restored.
    let case = "a second cluster restored from the same snapshot";
    let again = restore_cluster(&router, case, "emu-snapheld-two", "emu-snapheld-dst-again").await;
    let reported = again.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2],
        "case {case:?}: expected the same snapshot to restore again, got {reported:?}"
    );
    again
        .run(
            &router,
            case,
            "INSERT INTO sim2real_snap_restore (id, label) VALUES (9, 'only here')",
        )
        .await;
    let reported = again.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2, 9],
        "case {case:?}: expected the write on this cluster, got {reported:?}"
    );
    let first = Addressed {
        arn: element(
            &served(
                &router,
                case,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", "emu-snapheld-dst-two")],
            )
            .await,
            "DBClusterArn",
        ),
        secret_arn: await_active_secret(&router, case, "emu-snapheld-dst-two").await,
    };
    let reported = first.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2],
        "case {case:?}: a write to one restored cluster is not a write to the other, got \
         {reported:?}"
    );
    // And the source cluster is untouched by every restore above.
    let reported = source.ids_in(&router, case, DATABASE).await;
    assert_eq!(
        reported,
        vec![1, 2, 3],
        "case {case:?}: expected the source cluster's own rows, got {reported:?}"
    );
}
