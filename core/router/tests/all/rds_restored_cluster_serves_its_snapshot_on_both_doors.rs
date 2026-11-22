// SPDX-License-Identifier: CC0-1.0

//! A cluster RESTORED from a DB cluster snapshot serves the snapshot's data on EVERY door of
//! it.
//!
//! RULE: a DB cluster snapshot is a copy of the cluster's storage, and a cluster restored from
//! it is loaded with that copy — into the storage the restored cluster's own databases are
//! held in, which every surface of a cluster opens one connection onto per database name. So
//! rows written through ANY door of the source cluster are rows EVERY door of the restored
//! cluster serves: what a Data API `ExecuteStatement` committed is what a native session on
//! the restored cluster's engine endpoint selects, and what a native session committed is what
//! an `ExecuteStatement` on the restored cluster reads. A copy installed where only the door
//! that wrote it could reach it would be data the client cannot read back through the driver
//! it wrote with.
//!
//! The sibling suite [`crate::rds_cluster_snapshot_restore_lifecycle`] pins what a restored
//! cluster serves THROUGH THE DATA API, over the instants a snapshot can be taken at, and
//! [`crate::rds_cluster_one_database_behind_both_doors`] pins that the two doors of ONE
//! cluster address one database. This one pins the two together: the door the rows were
//! written through, and the door they are read back through AFTER a snapshot and a restore.
//!
//! The two tests differ along the door that WROTE the rows, and each one's rows along WHICH
//! database of the cluster holds them — the one the cluster was created with, addressed by name
//! and addressed by no name at all ("the connection's default database"), and the database the
//! PostgreSQL engine itself comes with — and along what is read back: a row's value and how
//! many rows the table holds, through both doors in turn. Beside them: a row committed AFTER
//! the snapshot, which the restored cluster must hold on neither door, and a write ON the
//! restored cluster, which must reach the source on neither of its own.
//!
//! Each test drives ONE door of the source cluster and no other, which is what makes the read
//! after the restore a reading of the snapshot's own copy rather than of whatever the other
//! door happened to have open at the time.
//!
//! Only a cross-service suite can pin this: the cluster, its snapshot, the restore and the
//! engine endpoint behind them are Amazon RDS's, the HTTP endpoint is the RDS Data API's and
//! the credential its calls authenticate with is Secrets Manager's. The harness is the two
//! suites' own — the cluster create with a Data API endpoint and a secret, the
//! `ExecuteStatement` client and the PostgreSQL session from
//! [`crate::rds_cluster_one_database_behind_both_doors`], the control-plane calls and polls
//! from [`crate::rds_cluster_lifecycle_calls`] — reused rather than copied.
//!
//! Every control-plane call goes over the wire on a router with request authentication off, as
//! every suite in this crate runs it, every wait is a poll for the CONDITION under a generous
//! deadline, and the whole router is assembled on a sized thread (see `boot_serve.rs`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::element;
use crate::rds_cluster_lifecycle_calls::{await_available_cluster, served, take_snapshot, ENGINE};
use crate::rds_cluster_one_database_behind_both_doors::{
    api_column, execute_statement, provisioned, Provisioned, Session, BOOT_STACK_SIZE, DATABASE,
};

/// The database the PostgreSQL engine itself comes with, which a cluster running it therefore
/// has as well — the second database name both doors address.
const ENGINE_DATABASE: &str = "postgres";

/// How long a poll waits for a condition the service owes before giving up: generous, so a
/// loaded host does not fail a condition that does arrive, and far short of a hang.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long a poll waits between two reads. The condition, not the clock, is what ends the
/// loop; this only keeps it from spinning.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// One table written through one door of the source cluster, read back through BOTH doors of
/// the cluster restored from the snapshot that carried it.
struct Carried {
    /// What this row is a case of.
    case: &'static str,
    /// The `database` member this row's Data API calls carry, or `None` for a call that names
    /// none — which addresses the database the cluster was created with.
    database: Option<&'static str>,
    /// The database this row's native sessions log in on, which is that same database of the
    /// cluster.
    native_database: &'static str,
    /// The table this row writes, which no other row names.
    table: &'static str,
}

/// The tables the DATA API writes: each database name a call can address.
const THROUGH_THE_API: &[Carried] = &[
    Carried {
        case: "a table the Data API created in the database its calls name",
        database: Some(DATABASE),
        native_database: DATABASE,
        table: "carried_api_named_rows",
    },
    Carried {
        case: "a table the Data API created in the cluster's own database, named by no call",
        database: None,
        native_database: DATABASE,
        table: "carried_api_default_rows",
    },
    Carried {
        case: "a table the Data API created in the engine's own database",
        database: Some(ENGINE_DATABASE),
        native_database: ENGINE_DATABASE,
        table: "carried_api_engine_rows",
    },
];

/// The tables a NATIVE SESSION writes: the same two databases of the cluster.
const THROUGH_A_SESSION: &[Carried] = &[
    Carried {
        case: "a table a native session created in the cluster's own database",
        database: Some(DATABASE),
        native_database: DATABASE,
        table: "carried_native_rows",
    },
    Carried {
        case: "a table a native session created in the engine's own database",
        database: Some(ENGINE_DATABASE),
        native_database: ENGINE_DATABASE,
        table: "carried_native_engine_rows",
    },
];

/// The statements that seed one carried table: two rows under a known key.
fn seeding(table: &str) -> [String; 2] {
    [
        format!("CREATE TABLE {table} (id INT PRIMARY KEY, holder VARCHAR(40) NOT NULL)"),
        format!("INSERT INTO {table} (id, holder) VALUES (1, 'first'), (2, 'second')"),
    ]
}

/// The cluster a restore created, as both doors reach it.
struct Restored {
    /// The ARN a Data API call addresses it by.
    arn: String,
    /// The host and the port its engine endpoint publishes.
    address: String,
    port: u16,
}

/// Restore `snapshot` into a new cluster called `identifier`, wait for it to report
/// `available`, turn its Data API HTTP endpoint on and wait until that endpoint answers — the
/// sequence a client runs after a restore.
async fn restore(
    router: &MultiServiceRouter,
    case: &str,
    snapshot: &str,
    identifier: &str,
    secret_arn: &str,
) -> Restored {
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
    let available = await_available_cluster(router, case, identifier).await;
    let port: u16 = element(&available, "Port").parse().unwrap_or_else(|error| {
        panic!("case {case:?}: the restored cluster publishes a port, got {error}")
    });
    let address = element(&available, "Endpoint");
    // A restore publishes no Data API endpoint of its own: the client turns one on, exactly as
    // it does on a cluster it created.
    served(router, case, "EnableHttpEndpoint", &[("ResourceArn", &arn)]).await;
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let answered =
            execute_statement(router, &arn, secret_arn, Some(DATABASE), None, "SELECT 1").await;
        if answered.status == 200 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the restored cluster's Data API endpoint must answer, last got HTTP \
             {} with {}",
            answered.status,
            answered.body
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
    Restored { arn, address, port }
}

/// Read one carried table back through BOTH doors of `restored`: the row under key 2 and how
/// many rows the table holds, which must be what the snapshot carried either way.
async fn read_through_both_doors(
    router: &MultiServiceRouter,
    carried: &Carried,
    restored: &Restored,
    secret_arn: &str,
) {
    let case = carried.case;
    let table = carried.table;
    // One door: a native session on the restored cluster's engine endpoint.
    let mut session =
        Session::open(case, &restored.address, restored.port, carried.native_database).await;
    let read = session
        .served(case, &format!("SELECT holder FROM {table} WHERE id = 2"))
        .await;
    assert_eq!(
        read,
        vec![vec![Some("second".to_string())]],
        "case {case:?}: a native session on the restored cluster must read the row the snapshot \
         carried, got {read:?}"
    );
    let counted = session
        .served(case, &format!("SELECT count(*) FROM {table}"))
        .await;
    assert_eq!(
        counted,
        vec![vec![Some("2".to_string())]],
        "case {case:?}: a native session on the restored cluster must read every row the snapshot \
         carried, got {counted:?}"
    );
    // And the other door: an `ExecuteStatement` on the restored cluster, which must hold
    // exactly the same table and rows.
    let read = execute_statement(
        router,
        &restored.arn,
        secret_arn,
        carried.database,
        None,
        &format!("SELECT holder FROM {table} WHERE id = 2"),
    )
    .await
    .served(case);
    assert_eq!(
        api_column(case, &read),
        vec!["second".to_string()],
        "case {case:?}: the Data API on the restored cluster must read the row the snapshot \
         carried, got {read}"
    );
    let counted = execute_statement(
        router,
        &restored.arn,
        secret_arn,
        carried.database,
        None,
        &format!("SELECT count(*) FROM {table}"),
    )
    .await
    .served(case);
    assert_eq!(
        api_column(case, &counted),
        vec!["2".to_string()],
        "case {case:?}: the Data API on the restored cluster must read every row the snapshot \
         carried, got {counted}"
    );
}

/// The whole emulator on one engine, on a thread sized for assembling it, with the body of one
/// test driven on a runtime that has the I/O driver a listener needs.
fn on_the_assembled_emulator<Body>(name: &'static str, body: impl FnOnce(MultiServiceRouter) -> Body + Send + 'static)
where
    Body: std::future::Future<Output = ()>,
{
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(move || {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            // `enable_all`, which is what the emulator's own boot path builds: a cluster create
            // and a restore each open an engine listener through the core, and binding a socket
            // needs the runtime's I/O driver.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(body(router));
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}

/// The rows the DATA API wrote, and nothing else, are read back through BOTH doors of the
/// cluster restored from the snapshot that carried them — the door that wrote them and the
/// native session that did not. A row committed after the snapshot is on neither door, and a
/// write on the restored cluster reaches neither door of the source.
///
/// No native session is opened on the SOURCE cluster at all, before the snapshot or after it:
/// the snapshot's only copy of these rows is the one taken off the door that wrote them, which
/// is the case the rule is about.
#[test]
fn a_restored_cluster_serves_on_both_doors_what_the_data_api_wrote() {
    on_the_assembled_emulator("rds-restored-cluster-api-rows", |router| async move {
        let source: Provisioned =
            provisioned(&router, "snapshot-api-rows-src", "snapshot-api-rows-secret").await;

        for carried in THROUGH_THE_API {
            for statement in seeding(carried.table) {
                execute_statement(
                    &router,
                    &source.cluster_arn,
                    &source.secret_arn,
                    carried.database,
                    None,
                    &statement,
                )
                .await
                .served(carried.case);
            }
        }

        take_snapshot(
            &router,
            "the snapshot of the source cluster",
            "snapshot-api-rows-src",
            "snapshot-api-rows-snap",
        )
        .await;

        // A row committed AFTER the snapshot, which the restored cluster must therefore not
        // hold on either door.
        const AFTERWARDS: &str = "a row committed after the snapshot was taken";
        execute_statement(
            &router,
            &source.cluster_arn,
            &source.secret_arn,
            Some(DATABASE),
            None,
            "INSERT INTO carried_api_named_rows (id, holder) VALUES (3, 'afterwards')",
        )
        .await
        .served(AFTERWARDS);

        let restored = restore(
            &router,
            "the restore of that snapshot",
            "snapshot-api-rows-snap",
            "snapshot-api-rows-dst",
            &source.secret_arn,
        )
        .await;
        assert_ne!(
            restored.arn, source.cluster_arn,
            "a restore creates a resource of its own, got {} for both",
            restored.arn
        );

        for carried in THROUGH_THE_API {
            read_through_both_doors(&router, carried, &restored, &source.secret_arn).await;
        }

        // The row committed after the snapshot is on neither door of the restored cluster.
        let mut restored_session =
            Session::open(AFTERWARDS, &restored.address, restored.port, DATABASE).await;
        let ids = restored_session
            .served(
                AFTERWARDS,
                "SELECT id FROM carried_api_named_rows ORDER BY id",
            )
            .await;
        assert_eq!(
            ids,
            vec![vec![Some("1".to_string())], vec![Some("2".to_string())]],
            "case {AFTERWARDS:?}: a native session on the restored cluster must read what the \
             snapshot held and nothing committed after it, got {ids:?}"
        );
        let through_the_api = execute_statement(
            &router,
            &restored.arn,
            &source.secret_arn,
            Some(DATABASE),
            None,
            "SELECT id FROM carried_api_named_rows ORDER BY id",
        )
        .await
        .served(AFTERWARDS);
        assert_eq!(
            api_column(AFTERWARDS, &through_the_api),
            vec!["1".to_string(), "2".to_string()],
            "case {AFTERWARDS:?}: the Data API on the restored cluster must read what the snapshot \
             held and nothing committed after it, got {through_the_api}"
        );

        // And a write ON the restored cluster is the restored cluster's alone: the source
        // serves its own rows afterwards, on both of its doors.
        const OWN_WRITES: &str = "a row written on the restored cluster";
        restored_session
            .served(
                OWN_WRITES,
                "INSERT INTO carried_api_named_rows (id, holder) VALUES (9, 'only here')",
            )
            .await;
        let on_the_restored = execute_statement(
            &router,
            &restored.arn,
            &source.secret_arn,
            Some(DATABASE),
            None,
            "SELECT id FROM carried_api_named_rows ORDER BY id",
        )
        .await
        .served(OWN_WRITES);
        assert_eq!(
            api_column(OWN_WRITES, &on_the_restored),
            vec!["1".to_string(), "2".to_string(), "9".to_string()],
            "case {OWN_WRITES:?}: the Data API on the restored cluster must read the row a session \
             on it wrote, got {on_the_restored}"
        );
        let on_the_source = execute_statement(
            &router,
            &source.cluster_arn,
            &source.secret_arn,
            Some(DATABASE),
            None,
            "SELECT id FROM carried_api_named_rows ORDER BY id",
        )
        .await
        .served(OWN_WRITES);
        assert_eq!(
            api_column(OWN_WRITES, &on_the_source),
            vec!["1".to_string(), "2".to_string(), "3".to_string()],
            "case {OWN_WRITES:?}: a write on the restored cluster must not reach the source, got \
             {on_the_source}"
        );
        let mut source_session =
            Session::open(OWN_WRITES, &source.address, source.port, DATABASE).await;
        let on_the_source = source_session
            .served(
                OWN_WRITES,
                "SELECT id FROM carried_api_named_rows ORDER BY id",
            )
            .await;
        assert_eq!(
            on_the_source,
            vec![
                vec![Some("1".to_string())],
                vec![Some("2".to_string())],
                vec![Some("3".to_string())],
            ],
            "case {OWN_WRITES:?}: a session on the source must read the source's own rows, got \
             {on_the_source:?}"
        );
    });
}

/// And the same rule the other way round: the rows a NATIVE SESSION wrote, and nothing else,
/// are read back through both doors of the cluster restored from the snapshot that carried
/// them.
#[test]
fn a_restored_cluster_serves_on_both_doors_what_a_native_session_wrote() {
    on_the_assembled_emulator("rds-restored-cluster-native-rows", |router| async move {
        let source: Provisioned = provisioned(
            &router,
            "snapshot-native-rows-src",
            "snapshot-native-rows-secret",
        )
        .await;

        for carried in THROUGH_A_SESSION {
            let mut session = Session::open(
                carried.case,
                &source.address,
                source.port,
                carried.native_database,
            )
            .await;
            for statement in seeding(carried.table) {
                session.served(carried.case, &statement).await;
            }
        }

        take_snapshot(
            &router,
            "the snapshot of the source cluster",
            "snapshot-native-rows-src",
            "snapshot-native-rows-snap",
        )
        .await;

        let restored = restore(
            &router,
            "the restore of that snapshot",
            "snapshot-native-rows-snap",
            "snapshot-native-rows-dst",
            &source.secret_arn,
        )
        .await;

        for carried in THROUGH_A_SESSION {
            read_through_both_doors(&router, carried, &restored, &source.secret_arn).await;
        }
    });
}
