// SPDX-License-Identifier: CC0-1.0

//! EVERY door of a DB cluster reads the ONE set of databases that cluster holds — and only
//! that cluster's.
//!
//! RULE: an Aurora DB cluster holds one database per name on its own cluster volume, and every
//! surface of the cluster is an address onto that volume: the cluster's writer endpoint, its
//! reader endpoint, each member DB instance's own endpoint, and — for a cluster whose HTTP
//! endpoint is on — the RDS Data API, whose `ExecuteStatement` "runs a SQL statement against a
//! database" of that same cluster. So a row committed through any one of them is a row every
//! other one reads back under the same database name, while a cluster's volume is its OWN: a
//! second cluster created with the same `DatabaseName` holds none of the first's relations.
//!
//! The sibling suite [`crate::rds_cluster_one_database_behind_both_doors`] pins the crossing
//! between the Data API and the cluster's WRITER endpoint, over the databases a cluster holds
//! and the transactions either door opens. This one spans the other axis of the same rule: WHICH
//! door the client dials. Its rows are the three addresses a native client of one cluster can be
//! handed — the writer endpoint, the reader endpoint, and the endpoint of a member DB instance,
//! which "runs no engine of its own" — plus the Data API, with the cluster's volume carried
//! forward from row to row, so each door reads what every earlier door committed and the writable
//! ones add to it. The cluster-boundary case is the other half of the rule, and the one that
//! tells a shared volume from a shared process: two clusters, the same database name, no
//! relation in common.
//!
//! Only a cross-service suite can drive this: the cluster, its members and the addresses they
//! publish are Amazon RDS's, the HTTP door is the RDS Data API's and the credential its calls
//! authenticate with is Secrets Manager's — three services over the ONE engine
//! `core/router/src/lib.rs` mounts them on. The control-plane calls go over the wire on a router
//! with request authentication off, as every suite in this crate runs it; the native client is
//! the sibling suite's, written against the PostgreSQL protocol itself. Every wait is a poll for
//! the CONDITION under a generous deadline, and assembling the whole router costs stack (see
//! `boot_serve.rs`), so the requests are driven on a sized thread.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{block_of, element};
use crate::rds_cluster_lifecycle_calls::{await_available_instance, served as rds_served, REGION};
use crate::rds_cluster_one_database_behind_both_doors::{
    api_column, execute_statement, provisioned, Provisioned, Refusal, Session, BOOT_STACK_SIZE,
    DATABASE,
};

/// The engine the clusters below run, the one whose clusters serve the Data API.
const ENGINE: &str = "aurora-postgresql";

/// The DB instance class a member of an Aurora cluster is provisioned with, from the Aurora
/// instance classes `CreateDBInstanceMessage$DBInstanceClass` publishes.
const MEMBER_CLASS: &str = "db.r5.large";

/// The table every row below reads, created once through the Data API.
const TABLE: &str = "door_rows";

/// PostgreSQL's own code for a statement naming a relation the database does not hold
/// ("undefined_table", its "Error Codes" appendix).
const SQLSTATE_UNDEFINED_TABLE: &str = "42P01";

/// WHERE a door's address comes from, which is the only thing that differs between the rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Address {
    /// `DBCluster$Endpoint` with `DBCluster$Port`: the cluster's writer endpoint.
    ClusterWriter,
    /// `DBCluster$ReaderEndpoint`: the address that "load-balances connections across the Aurora
    /// Replicas".
    ClusterReader,
    /// `DBInstance$Endpoint` of a member DB instance of the cluster.
    MemberInstance,
}

/// One door of the cluster a native client dials.
struct Door {
    case: &'static str,
    address: Address,
    /// Whether the cluster SERVES WRITES through this door, which is what decides whether the
    /// row also commits through it — the read-only endpoint's refusal is the RDS crate's own
    /// subject, not this suite's.
    writes: bool,
}

const DOORS: &[Door] = &[
    Door {
        case: "the cluster's writer endpoint",
        address: Address::ClusterWriter,
        writes: true,
    },
    Door {
        case: "the cluster's reader endpoint",
        address: Address::ClusterReader,
        writes: false,
    },
    Door {
        case: "a member DB instance's own endpoint",
        address: Address::MemberInstance,
        writes: true,
    },
];

/// Add one member DB instance to `cluster` and wait until it reports `available`, answering the
/// address its own endpoint publishes.
async fn member_endpoint(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    instance: &str,
) -> (String, u16) {
    rds_served(
        router,
        case,
        "CreateDBInstance",
        &[
            ("DBInstanceIdentifier", instance),
            ("DBClusterIdentifier", cluster),
            ("Engine", ENGINE),
            ("DBInstanceClass", MEMBER_CLASS),
        ],
    )
    .await;
    // A member joins a cluster that is already up, so its own endpoint is ready one span later:
    // looked for again until it holds rather than waited out.
    let reported = await_available_instance(router, case, instance).await;
    let endpoint = block_of(&reported, "Endpoint").unwrap_or_else(|| {
        panic!("case {case:?}: the member must publish an endpoint, got {reported}")
    });
    let address = element(endpoint, "Address");
    let port: u16 = element(endpoint, "Port").parse().unwrap_or_else(|error| {
        panic!("case {case:?}: the member must publish a port, got {error} in {reported}")
    });
    (address, port)
}

/// The port the cluster's READ-ONLY listener answers on, read from Amazon RDS's own store.
///
/// The core assigns every listener its port and the model gives a DB cluster ONE `Port`, its
/// writer endpoint's, so the reader listener's port is not a value any control-plane member
/// carries: a suite that opens a session on that listener reads where the listener itself landed,
/// exactly as the RDS crate's own reader-endpoint suite does.
fn reader_port(engine: &Arc<InMemoryEngine>, case: &str, identifier: &str) -> u16 {
    let state = service_com_amazonaws_rds_amazonrdsv19::state::service_state(&**engine)
        .unwrap_or_else(|| panic!("case {case:?}: Amazon RDS's own store is on this engine"));
    let store = state.lock();
    let endpoint = store
        .cluster_engine_endpoint(&(REGION.to_string(), identifier.to_string()))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the cluster {identifier:?} must have opened an engine endpoint")
        });
    let reader = endpoint.reader.as_ref().unwrap_or_else(|| {
        panic!("case {case:?}: the cluster {identifier:?} must have opened its read-only listener")
    });
    u16::try_from(reader.port).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the read-only listener's port, got {} ({error})",
            reader.port
        )
    })
}

/// The first field of every row a native session answered, as the text the server sent.
fn column(case: &str, rows: &[Vec<Option<String>>]) -> Vec<String> {
    rows.iter()
        .map(|row| {
            row.first().cloned().flatten().unwrap_or_else(|| {
                panic!("case {case:?}: every row carries its first field, got {row:?}")
            })
        })
        .collect()
}

/// The ids [`TABLE`] holds, as the Data API reports them.
async fn ids_through_the_data_api(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &Provisioned,
) -> Vec<String> {
    let read = execute_statement(
        router,
        &cluster.cluster_arn,
        &cluster.secret_arn,
        Some(DATABASE),
        None,
        &format!("SELECT id FROM {TABLE} ORDER BY id"),
    )
    .await
    .served(case);
    api_column(case, &read)
}

/// Create [`TABLE`] through the cluster's Data API door and seed it, answering the ids it then
/// holds.
async fn seeded_through_the_data_api(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &Provisioned,
) -> Vec<String> {
    for statement in [
        format!("CREATE TABLE {TABLE} (id INT PRIMARY KEY, holder VARCHAR(40) NOT NULL)"),
        format!("INSERT INTO {TABLE} (id, holder) VALUES (1, 'through the data api')"),
    ] {
        execute_statement(
            router,
            &cluster.cluster_arn,
            &cluster.secret_arn,
            Some(DATABASE),
            None,
            &statement,
        )
        .await
        .served(case);
    }
    vec!["1".to_string()]
}

/// Every address the cluster publishes reads the one volume the cluster holds: each door reads
/// back what the Data API and every earlier door committed, and what it commits itself is read
/// through the Data API.
#[test]
fn every_door_of_a_cluster_reads_the_one_volume_the_cluster_holds() {
    std::thread::Builder::new()
        .name("rds-cluster-every-door".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(Arc::clone(&engine));
            router.set_request_authentication(false);
            // `enable_all`, which is what the emulator's own boot path builds: a cluster create
            // opens the cluster's engine listener through the core, and binding a socket needs
            // the runtime's I/O driver.
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(async {
                const CLUSTER: &str = "every-door-cluster";
                const MEMBER: &str = "every-door-member";
                let cluster = provisioned(&router, CLUSTER, "every-door-secret").await;
                let member = member_endpoint(&router, "the cluster's member", CLUSTER, MEMBER).await;

                // The rows the HTTP door commits, which every door below must already hold.
                let mut expected =
                    seeded_through_the_data_api(&router, "the cluster's Data API door", &cluster)
                        .await;

                for door in DOORS {
                    let case = door.case;
                    let (address, port) = match door.address {
                        Address::ClusterWriter => (cluster.address.clone(), cluster.port),
                        Address::ClusterReader => (
                            cluster.address.clone(),
                            reader_port(&engine, case, CLUSTER),
                        ),
                        Address::MemberInstance => (member.0.clone(), member.1),
                    };
                    let mut session = Session::open(case, &address, port, DATABASE).await;
                    let read = session
                        .served(case, &format!("SELECT id FROM {TABLE} ORDER BY id"))
                        .await;
                    assert_eq!(
                        column(case, &read),
                        expected,
                        "case {case:?}: this door must read every row committed to the cluster's \
                         volume, got {read:?}"
                    );
                    if !door.writes {
                        continue;
                    }
                    // What this door commits is committed IN THE CLUSTER, so the HTTP door reads
                    // it and so does every door after this one.
                    let next = expected.len() + 1;
                    session
                        .served(
                            case,
                            &format!(
                                "INSERT INTO {TABLE} (id, holder) VALUES ({next}, 'through a door')"
                            ),
                        )
                        .await;
                    expected.push(next.to_string());
                    let through_the_api =
                        ids_through_the_data_api(&router, case, &cluster).await;
                    assert_eq!(
                        through_the_api, expected,
                        "case {case:?}: the Data API must read the row this door committed, got \
                         {through_the_api:?}"
                    );
                }

                // Every door, once more, now that each has written: one volume, one set of rows,
                // whichever address the client was handed.
                for door in DOORS {
                    let case = door.case;
                    let (address, port) = match door.address {
                        Address::ClusterWriter => (cluster.address.clone(), cluster.port),
                        Address::ClusterReader => (
                            cluster.address.clone(),
                            reader_port(&engine, case, CLUSTER),
                        ),
                        Address::MemberInstance => (member.0.clone(), member.1),
                    };
                    let mut session = Session::open(case, &address, port, DATABASE).await;
                    let read = session
                        .served(case, &format!("SELECT id FROM {TABLE} ORDER BY id"))
                        .await;
                    assert_eq!(
                        column(case, &read),
                        expected,
                        "case {case:?}: every door reads the rows every other door committed, got \
                         {read:?}"
                    );
                }
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}

/// A cluster's volume is the CLUSTER's: a second cluster created with the same database name
/// holds none of the first's relations, through either of its doors, and the first holds none of
/// the second's.
#[test]
fn a_second_cluster_with_the_same_database_name_holds_none_of_the_firsts_relations() {
    std::thread::Builder::new()
        .name("rds-cluster-own-volume".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
            let mut router = build_router(engine);
            router.set_request_authentication(false);
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a current-thread runtime");
            runtime.block_on(async {
                const SEEDED: &str = "the cluster the rows were written to";
                const NEIGHBOUR: &str = "a second cluster holding the same database name";
                let seeded = provisioned(&router, "own-volume-seeded", "own-volume-seeded-secret")
                    .await;
                let neighbour =
                    provisioned(&router, "own-volume-other", "own-volume-other-secret").await;
                let expected = seeded_through_the_data_api(&router, SEEDED, &seeded).await;

                // The neighbouring cluster serves the same database NAME and none of its rows:
                // the relation is not there, in PostgreSQL's own words, through both of its doors.
                let refused = execute_statement(
                    &router,
                    &neighbour.cluster_arn,
                    &neighbour.secret_arn,
                    Some(DATABASE),
                    None,
                    &format!("SELECT id FROM {TABLE}"),
                )
                .await;
                assert_eq!(
                    refused.status, 400,
                    "case {NEIGHBOUR:?}: a relation this cluster does not hold must be refused, \
                     got HTTP {} with {}",
                    refused.status, refused.body
                );
                let reported: serde_json::Value = serde_json::from_str(&refused.body)
                    .unwrap_or_else(|error| {
                        panic!("case {NEIGHBOUR:?}: the Data API answers JSON, got {error}")
                    });
                let message = reported["message"].as_str().unwrap_or_default().to_string();
                assert_eq!(
                    message,
                    format!(
                        "ERROR: relation \"{TABLE}\" does not exist; SQLState: \
                         {SQLSTATE_UNDEFINED_TABLE}"
                    ),
                    "case {NEIGHBOUR:?}: expected the engine's own undefined-relation sentence, \
                     got {}",
                    refused.body
                );
                let mut session = Session::open(
                    NEIGHBOUR,
                    &neighbour.address,
                    neighbour.port,
                    DATABASE,
                )
                .await;
                let refusal = session
                    .query(NEIGHBOUR, &format!("SELECT id FROM {TABLE}"))
                    .await
                    .expect_err(&format!(
                        "case {NEIGHBOUR:?}: a relation this cluster's volume does not hold must \
                         be refused"
                    ));
                assert_eq!(
                    refusal,
                    Refusal {
                        code: SQLSTATE_UNDEFINED_TABLE.to_string(),
                        message: format!("relation \"{TABLE}\" does not exist"),
                    },
                    "case {NEIGHBOUR:?}: expected PostgreSQL's own refusal, got {refusal:?}"
                );

                // And a relation the neighbour creates under the same name is its own: the first
                // cluster still answers exactly the rows committed to ITS volume.
                session
                    .served(
                        NEIGHBOUR,
                        &format!("CREATE TABLE {TABLE} (id INT PRIMARY KEY, holder VARCHAR(40))"),
                    )
                    .await;
                session
                    .served(
                        NEIGHBOUR,
                        &format!("INSERT INTO {TABLE} (id, holder) VALUES (7, 'the neighbours')"),
                    )
                    .await;
                let still_held = ids_through_the_data_api(&router, SEEDED, &seeded).await;
                assert_eq!(
                    still_held, expected,
                    "case {SEEDED:?}: the rows of this cluster's volume are its own, got \
                     {still_held:?}"
                );
                let mut first = Session::open(SEEDED, &seeded.address, seeded.port, DATABASE).await;
                let natively = first
                    .served(SEEDED, &format!("SELECT id FROM {TABLE} ORDER BY id"))
                    .await;
                assert_eq!(
                    column(SEEDED, &natively),
                    expected,
                    "case {SEEDED:?}: its engine endpoint reads the same rows its Data API door \
                     does, got {natively:?}"
                );
            });
        })
        .expect("the suite's thread starts")
        .join()
        .expect("the suite's thread finishes");
}
