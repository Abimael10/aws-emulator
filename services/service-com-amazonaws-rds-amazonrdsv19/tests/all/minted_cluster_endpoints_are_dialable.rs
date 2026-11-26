// SPDX-License-Identifier: CC0-1.0

//! What a client is handed to DIAL by every operation that brings a DB CLUSTER into being.
//!
//! The rule under test: an endpoint member a client DIALS carries the ADDRESS this emulator
//! answers that cluster's data plane at — the host the core reports the listener it opened for
//! that cluster on, beside the one `DBCluster$Port` ("the port that the database engine is
//! listening on") — because the client resolves and dials exactly what the control plane
//! published and the only hosts that reach this process are its OWN. So the published host is
//! that address itself and never a name this service announced beneath it: such a name is a
//! host of nobody's, answered only by a resolver that honours a reservation the client's host
//! need not honour, and a client that is handed one reaches no listener at all.
//!
//! The rows span what the rule generalises over: the OPERATIONS that mint a cluster — the
//! create, the restore of a snapshot, the point-in-time restore of a live cluster, and the
//! clone that restore makes with `RestoreType=copy-on-write` — and both wires this emulator
//! speaks. Each minted cluster's published pair is checked against the listener this service
//! holds for it, and then DIALLED over a real TCP connection with a row round-tripped through
//! it, so a member naming an address this emulator does not answer on could not pass.
//!
//! The harness (`call`, `emulator_with_engine`, the wire clients and the poll loop) is the one
//! the sibling suites of this target own.

use std::sync::Arc;

use service_com_amazonaws_rds_amazonrdsv19::state as st;

use crate::db_clusters::{assert_served_endpoint_pair, the_cluster};
use crate::db_instance_engine_endpoint::{
    wire_login as login, Wire, DATABASE, MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, emulator_with_engine, expect_ok, text, REGION};
use crate::lifecycle_poll_loops::poll_until;

/// The backup retention every source cluster is created with, which is what gives it a
/// restorable window for the point-in-time rows at all.
const SOURCE_RETENTION: &str = "5";

/// One engine of the table: the wire its clusters speak and the identifier fragment the
/// clusters of that engine are named after.
struct EngineRow {
    case: &'static str,
    engine: &'static str,
    wire: Wire,
    tag: &'static str,
}

const ENGINES: &[EngineRow] = &[
    EngineRow {
        case: "Aurora PostgreSQL",
        engine: "aurora-postgresql",
        wire: Wire::Postgres,
        tag: "pg",
    },
    EngineRow {
        case: "Aurora MySQL, the other wire",
        engine: "aurora-mysql",
        wire: Wire::MySql,
        tag: "my",
    },
];

/// Create `identifier` as a cluster of `engine` with a restorable window, and answer with the
/// envelope the create published.
async fn create_source(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
) -> String {
    let request = [
        ("DBClusterIdentifier", identifier.to_string()),
        ("Engine", engine.to_string()),
        ("DatabaseName", DATABASE.to_string()),
        ("MasterUsername", MASTER_USER.to_string()),
        ("MasterUserPassword", MASTER_PASSWORD.to_string()),
        ("BackupRetentionPeriod", SOURCE_RETENTION.to_string()),
        ("PreferredBackupWindow", "03:00-03:30".to_string()),
    ];
    the_cluster(
        case,
        &expect_ok(case, call(router, "CreateDBCluster", &request).await),
    )
}

/// Poll the cluster until it reports `available`, which is the state a client dials it in, and
/// answer with the envelope the read served.
///
/// The CONDITION is waited for rather than a span: the loop is the one this test target
/// shares, whose deadline is generous beside the span a transition this service owes becomes
/// due after.
async fn available_cluster(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let params = [("DBClusterIdentifier", identifier)];
    let (body, _polls) = poll_until(
        case,
        "the cluster",
        || call(router, "DescribeDBClusters", &params),
        |status, body| status == 200 && text(body, "Status") == "available",
    )
    .await;
    the_cluster(case, &body)
}

/// Take a snapshot of `cluster`, poll it to `available`, and answer with its identifier.
async fn available_snapshot(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    cluster: &str,
) -> String {
    let snapshot = format!("{cluster}-snap");
    expect_ok(
        case,
        call(
            router,
            "CreateDBClusterSnapshot",
            &[
                ("DBClusterIdentifier", cluster.to_string()),
                ("DBClusterSnapshotIdentifier", snapshot.clone()),
            ],
        )
        .await,
    );
    let params = [("DBClusterSnapshotIdentifier", snapshot.as_str())];
    poll_until(
        case,
        "the cluster snapshot",
        || call(router, "DescribeDBClusterSnapshots", &params),
        |status, body| status == 200 && text(body, "Status") == "available",
    )
    .await;
    snapshot
}

/// The address the listener this emulator opened for the cluster `identifier` answers on, read
/// from this service's own store — what the published pair has to be.
fn listener_address(
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    identifier: &str,
) -> (String, String) {
    let state = st::service_state(&**engine).expect("this service's store");
    let store = state.lock();
    let endpoint = store
        .cluster_engine_endpoint(&(REGION.to_string(), identifier.to_string()))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the cluster must have opened an engine endpoint")
        });
    (
        endpoint.endpoint.host.clone(),
        endpoint.endpoint.port.to_string(),
    )
}

/// Assert the whole rule on one minted cluster: the pair it published is the address of the
/// listener this emulator opened for it, and a session really opens on that pair.
async fn assert_dialable(
    router: &smithy_server_runtime::MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    identifier: &str,
    wire: Wire,
    minted: &str,
) {
    let settled = available_cluster(router, case, identifier).await;
    // The create's own answer and the read agree on the pair: one address per cluster,
    // whichever call a client read it from.
    for member in ["Endpoint", "ReaderEndpoint", "Port"] {
        assert_eq!(
            text(minted, member),
            text(&settled, member),
            "case {case:?}: the {member} the minting call published must be the one the \
             describe reports"
        );
    }
    let writer = text(&settled, "Endpoint");
    let reader = text(&settled, "ReaderEndpoint");
    let port = text(&settled, "Port");
    // A host of this emulator's own, for both members, and no name announced beneath one.
    assert_served_endpoint_pair(case, &writer, &reader);
    // And it is the address of THIS cluster's own listener, read where that listener lives.
    let listener = listener_address(engine, case, identifier);
    assert_eq!(
        (writer.clone(), port.clone()),
        listener,
        "case {case:?}: expected the address the cluster's own listener answers on ({:?}), got \
         {writer}:{port}",
        listener
    );

    // The pair is an address a session really opens on: a client dials it over TCP, is
    // admitted, and reads back the row it wrote.
    let dialed_port: u16 = port
        .parse()
        .unwrap_or_else(|_| panic!("case {case:?}: expected a port a client can dial, got {port:?}"));
    let mut session = login(case, wire, &writer, dialed_port, DATABASE).await;
    // A table of this cluster's own: a cluster restored from another carries the tables that
    // other one held, and what is under test here is the ADDRESS rather than the data.
    let table = format!("dialled_{}", identifier.replace('-', "_"));
    session
        .run(
            case,
            &format!("CREATE TABLE {table} (id integer PRIMARY KEY, note varchar(64))"),
        )
        .await;
    session
        .run(
            case,
            &format!("INSERT INTO {table} (id, note) VALUES (1, 'dialled the published pair')"),
        )
        .await;
    let read = session
        .run(case, &format!("SELECT note FROM {table} WHERE id = 1"))
        .await;
    assert_eq!(
        read.only(),
        Some("dialled the published pair"),
        "case {case:?}: expected the row the session wrote, got {read:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_operation_that_mints_a_cluster_publishes_a_pair_a_client_can_dial() {
    let (router, engine) = emulator_with_engine();
    for row in ENGINES {
        let source = format!("emu-mced-src-{}", row.tag);
        let case = format!("{}: the create", row.case);
        let created = create_source(&router, &case, &source, row.engine).await;
        assert_dialable(&router, &engine, &case, &source, row.wire, &created).await;

        // A cluster restored FROM A SNAPSHOT of that one: a cluster of its own, so it is
        // reached on a listener of its own.
        let snapshot = available_snapshot(&router, &case, &source).await;
        let restored = format!("emu-mced-snap-{}", row.tag);
        let case = format!("{}: the restore of a snapshot", row.case);
        let answered = the_cluster(
            &case,
            &expect_ok(
                &case,
                call(
                    &router,
                    "RestoreDBClusterFromSnapshot",
                    &[
                        ("DBClusterIdentifier", restored.clone()),
                        ("SnapshotIdentifier", snapshot.clone()),
                        ("Engine", row.engine.to_string()),
                    ],
                )
                .await,
            ),
        );
        assert_dialable(&router, &engine, &case, &restored, row.wire, &answered).await;

        // And the two point-in-time restores of the LIVE source: the full copy, and the CLONE
        // `RestoreType=copy-on-write` makes.
        for (label, target, restore_type) in [
            (
                "the point-in-time restore",
                format!("emu-mced-pitr-{}", row.tag),
                "full-copy",
            ),
            (
                "the copy-on-write clone",
                format!("emu-mced-clone-{}", row.tag),
                "copy-on-write",
            ),
        ] {
            let case = format!("{}: {label}", row.case);
            let answered = the_cluster(
                &case,
                &expect_ok(
                    &case,
                    call(
                        &router,
                        "RestoreDBClusterToPointInTime",
                        &[
                            ("DBClusterIdentifier", target.clone()),
                            ("SourceDBClusterIdentifier", source.clone()),
                            ("UseLatestRestorableTime", "true".to_string()),
                            ("RestoreType", restore_type.to_string()),
                        ],
                    )
                    .await,
                ),
            );
            assert_dialable(&router, &engine, &case, &target, row.wire, &answered).await;
        }
    }
}
