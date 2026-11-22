// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the system variables a MySQL-family engine endpoint reports its
//! READ-ONLY state through.
//!
//! The rule under test: a system variable this server HAS answers the value its own state
//! holds for it and never SQL NULL, and the state the MySQL family's read-only variables
//! report is whether the endpoint the session arrived on serves writes — so a session on a
//! writing endpoint reads 0 from each of them and a session on one that serves reads only
//! reads 1. `@@innodb_read_only` is one of those variables ("whether the MySQL instance
//! opens InnoDB in read-only mode"), beside `@@read_only` and `@@super_read_only`; it is the
//! variable the standard writer/reader discrimination of a MySQL-family cluster reads, so a
//! NULL — which is neither a writer nor a reader — leaves a client unable to route at all.
//!
//! The same state is written the way each statement class writes it: a `@@` reference
//! answers the integer 1 or 0, and `SHOW VARIABLES` renders it as `ON` or `OFF`.
//!
//! The rows span what the rule generalises over: every variable of the family that reports
//! the state, each scope a client may name it in, every MySQL-family engine this emulator
//! serves a wire for (RDS for MySQL, MariaDB and Aurora MySQL), and both kinds of endpoint
//! the state distinguishes — one that writes (a standalone instance, a cluster's writer
//! endpoint) and one that serves reads only (a read replica, a cluster's reader endpoint).
//!
//! The clients are the wire clients the sibling endpoint suites drive
//! ([`crate::db_instance_engine_endpoint`]), so what is exercised is what a stock driver
//! speaks, in ONE process.

use std::sync::Arc;

use crate::db_clusters::{describe_one, the_cluster};
use crate::db_instance_engine_endpoint::{
    available_instance, endpoint_of, mysql_login, MySqlSession, Transport, DATABASE,
    MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{
    call, emulator, emulator_with_engine, expect_ok, text, wait_for_settle, REGION,
};

/// The variables a MySQL-family server reports its read-only state through.
///
/// Source: the MySQL Reference Manual's server system variables — `read_only` ("the server
/// permits no client updates"), `super_read_only` (the same, for users holding `SUPER` as
/// well) and `innodb_read_only` ("whether the MySQL instance opens InnoDB in read-only
/// mode"). Each is a boolean variable of the server, so each answers 1 or 0 through a `@@`
/// reference.
const READ_ONLY_VARIABLES: &[&str] = &["read_only", "super_read_only", "innodb_read_only"];

/// The forms a client may write a reference to one of those variables in: the bare name and
/// the explicit global scope they are defined in.
const SCOPES: &[&str] = &["", "global."];

/// What each variable answers on an endpoint that serves writes and on one that does not: a
/// MySQL boolean variable read through `@@` is the integer its state is.
const WHEN_WRITABLE: &str = "0";
const WHEN_READ_ONLY: &str = "1";

/// The same state as `SHOW VARIABLES` renders it.
const SHOWN_WHEN_WRITABLE: &str = "OFF";
const SHOWN_WHEN_READ_ONLY: &str = "ON";

/// Assert the whole rule on one session: every read-only variable, in every scope form,
/// answers the integer this endpoint's state is — never NULL — and `SHOW VARIABLES` renders
/// the same state as `ON`/`OFF`.
async fn the_read_only_variables_report(case: &str, session: &mut MySqlSession, read_only: bool) {
    let expected = if read_only {
        WHEN_READ_ONLY
    } else {
        WHEN_WRITABLE
    };
    let shown = if read_only {
        SHOWN_WHEN_READ_ONLY
    } else {
        SHOWN_WHEN_WRITABLE
    };
    for variable in READ_ONLY_VARIABLES {
        for scope in SCOPES {
            let sql = format!("SELECT @@{scope}{variable}");
            let answer = session.query(&sql).await.unwrap_or_else(|refusal| {
                panic!("case {case:?}: {sql:?} must run, got {refusal:?}")
            });
            assert_eq!(
                answer.rows.len(),
                1,
                "case {case:?}: {sql:?} must answer with one row, got {answer:?}"
            );
            let value = answer.rows.first().and_then(|row| row.first()).cloned().flatten();
            assert_eq!(
                value.as_deref(),
                Some(expected),
                "case {case:?}: expected {sql:?} to answer {expected:?}, got {value:?}"
            );
        }
        let sql = format!("SHOW VARIABLES LIKE '{variable}'");
        let listed = session
            .query(&sql)
            .await
            .unwrap_or_else(|refusal| panic!("case {case:?}: {sql:?} must run, got {refusal:?}"));
        assert_eq!(
            listed.rows,
            vec![vec![
                Some((*variable).to_string()),
                Some(shown.to_string())
            ]],
            "case {case:?}: expected {sql:?} to render the state as {shown:?}, got {listed:?}"
        );
    }
}

/// Open a MySQL session as the master user on `address:port`, in the database every resource
/// of this suite was created with.
async fn login(case: &str, address: &str, port: u16, database: &str) -> MySqlSession {
    mysql_login(
        address,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(database),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case:?}: the master user must be admitted on {address}:{port}, got {refusal:?}")
    })
}

/// One DB instance this suite opens a session on, and whether its endpoint serves writes.
struct InstanceRow {
    case: &'static str,
    engine: &'static str,
    identifier: &'static str,
    /// The instance this row REPLICATES, which is what makes its endpoint read-only; `None`
    /// for an instance of its own.
    source: Option<&'static str>,
}

/// The rows span the MySQL-family engines whose wire this emulator serves a DB instance for,
/// and both states: an instance of its own writes, a read replica serves reads only.
const INSTANCES: &[InstanceRow] = &[
    InstanceRow {
        case: "an RDS for MySQL instance",
        engine: "mysql",
        identifier: "emu-rovars-mysql",
        source: None,
    },
    InstanceRow {
        case: "a read replica of it",
        engine: "mysql",
        identifier: "emu-rovars-mysql-replica",
        source: Some("emu-rovars-mysql"),
    },
    InstanceRow {
        case: "an RDS for MariaDB instance, the other MySQL-family engine",
        engine: "mariadb",
        identifier: "emu-rovars-mariadb",
        source: None,
    },
];

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_read_only_variable_of_a_db_instance_reports_whether_it_serves_writes() {
    let router = emulator();
    for row in INSTANCES {
        let endpoint = match row.source {
            // An instance of its own: created here, with the automated backups a replica of
            // it is built from.
            None => {
                available_instance(
                    &router,
                    row.identifier,
                    row.engine,
                    &[("BackupRetentionPeriod", "1".to_string())],
                )
                .await
            }
            // A read replica of the instance the row names, which the row before it created.
            Some(source) => {
                let created = expect_ok(
                    row.case,
                    call(
                        &router,
                        "CreateDBInstanceReadReplica",
                        &[
                            ("DBInstanceIdentifier", row.identifier),
                            ("SourceDBInstanceIdentifier", source),
                        ],
                    )
                    .await,
                );
                let endpoint = endpoint_of(&created);
                // The replica comes up on its own: this waits for the CONDITION a client has
                // to see before it connects, read again until it holds.
                let request = [("DBInstanceIdentifier", row.identifier.to_string())];
                crate::lifecycle_poll_loops::poll_until(
                    row.case,
                    "the read replica reaching available",
                    || call(&router, "DescribeDBInstances", &request),
                    |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
                )
                .await;
                endpoint
            }
        };
        let mut session = login(row.case, &endpoint.0, endpoint.1, DATABASE).await;
        the_read_only_variables_report(row.case, &mut session, row.source.is_some()).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_aurora_mysql_clusters_two_endpoints_report_which_of_them_writes() {
    let (router, engine) = emulator_with_engine();
    let case = "an Aurora MySQL cluster";
    let identifier = "emu-rovars-aurora";
    let created = the_cluster(
        case,
        &expect_ok(
            case,
            call(
                &router,
                "CreateDBCluster",
                &[
                    ("DBClusterIdentifier", identifier.to_string()),
                    ("Engine", "aurora-mysql".to_string()),
                    ("DatabaseName", DATABASE.to_string()),
                    ("MasterUsername", MASTER_USER.to_string()),
                    ("MasterUserPassword", MASTER_PASSWORD.to_string()),
                ],
            )
            .await,
        ),
    );
    assert!(
        !text(&created, "Endpoint").is_empty(),
        "case {case:?}: the cluster must publish a writer endpoint, got {created}"
    );
    // A client connects once the cluster reports `available`, which the clock makes due.
    wait_for_settle();
    let settled = describe_one(&router, case, identifier).await;
    let status = text(&settled, "Status");
    assert_eq!(
        status, "available",
        "case {case:?}: the cluster must settle to available before a client connects, got \
         {status:?}"
    );
    let writer_port: u16 = text(&settled, "Port").parse().unwrap_or_else(|_| {
        panic!(
            "case {case:?}: the cluster must publish a port, got {:?}",
            text(&settled, "Port")
        )
    });
    let writer = (text(&settled, "Endpoint"), writer_port);
    let reader = (
        text(&settled, "ReaderEndpoint"),
        reader_listener_port(&engine, case, identifier),
    );

    // The writer endpoint is "the connection endpoint for the primary instance of the DB
    // cluster", so its sessions write; the reader endpoint load-balances across the
    // cluster's read-only replicas, so its sessions do not.
    let mut writes = login(case, &writer.0, writer.1, DATABASE).await;
    the_read_only_variables_report("the cluster's writer endpoint", &mut writes, false).await;
    let mut reads = login(case, &reader.0, reader.1, DATABASE).await;
    the_read_only_variables_report("the cluster's reader endpoint", &mut reads, true).await;
}

/// The port the cluster's READ-ONLY listener answers on, read from this service's own store.
///
/// The core assigns every listener its port and a DB cluster has ONE modelled `Port` — its
/// writer endpoint's — so the second listener's port is not a value any member of the control
/// plane carries: a suite that opens a session on that listener reads the port where the
/// listener itself lives.
fn reader_listener_port(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    identifier: &str,
) -> u16 {
    let state = service_com_amazonaws_rds_amazonrdsv19::state::service_state(&**engine)
        .expect("this service's store");
    let store = state.lock();
    let endpoint = store
        .cluster_engine_endpoint(&(REGION.to_string(), identifier.to_string()))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the cluster must have opened an engine endpoint")
        });
    let reader = endpoint.reader.as_ref().unwrap_or_else(|| {
        panic!("case {case:?}: the cluster must have opened its read-only listener")
    });
    u16::try_from(reader.port).unwrap_or_else(|_| {
        panic!(
            "case {case:?}: expected the read-only listener's port, got {}",
            reader.port
        )
    })
}
