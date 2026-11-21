// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the two DNS HOST NAMES a DB cluster publishes as its endpoints.
//!
//! The rule under test: `DBCluster$Endpoint` and `DBCluster$ReaderEndpoint` are host names
//! the CLUSTER mints from its own identifier — `<identifier>.cluster-<label>` for the writer
//! and `<identifier>.cluster-ro-<label>` for the reader, over ONE shared suffix — so the two
//! are non-empty, DISTINCT and of that form whatever the cluster's membership is, with the
//! port reported only in `DBCluster$Port` and the zone in `DBCluster$HostedZoneId`. Neither
//! name is read off a member instance: a cluster with no DB instance in it at all publishes
//! both, which is what lets a read/write-splitting client, a DSN builder or a
//! production-readiness audit tell a read path from a write path.
//!
//! A name a service hands a client is a name the client RESOLVES and then dials, so the
//! names of a cluster whose engine wire this emulator serves resolve to the interface the
//! cluster's own listeners are bound on, and the pair `<endpoint>` + `<Port>` is an address a
//! session really opens on — asserted here by resolving each published name and completing a
//! PostgreSQL login on it, and by looking the published pair up in the register an in-process
//! client of this emulator dials through.
//!
//! The rows span what the rule generalises over: the MEMBERSHIP a cluster has (none at all —
//! the Aurora Serverless v2 cluster nobody created an instance in — one member, and a writer
//! beside a replica), the engines whose wire this emulator serves (Aurora PostgreSQL, Aurora
//! MySQL) and the other cluster type (a Multi-AZ DB cluster), the identifier shapes a request
//! can name (short, long and hyphenated, and one written in another case, which is "stored as
//! a lowercase string"), every operation that answers with a cluster envelope (the create,
//! the describe and `DescribeDBClusterEndpoints`), and the cluster a RESTORE mints, whose
//! names follow the identifier the restore gave it rather than the source's.

use std::net::SocketAddr;

use crate::db_cluster_snapshot_restore::cluster_endpoint;
use crate::db_clusters::{assert_endpoint_name_pair, the_cluster, SERVED_SUFFIX};
use crate::db_instance_engine_endpoint::{
    postgres_login, Transport, DATABASE, MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{
    call, emulator, emulator_with_engine, expect_ok, subtrees, text, texts, REGION,
};
use crate::lifecycle_poll_loops::poll_until;

/// `[(name, value.to_string()), ..]` — the query parameters of one call.
macro_rules! params {
    ($($name:expr => $value:expr),* $(,)?) => {
        [$(($name, $value.to_string())),*]
    };
}

/// The four settings a Multi-AZ DB cluster is required to be created with, with an `Iops`
/// inside the documented ratio of the allocated storage.
const MULTI_AZ_SETTINGS: &[(&str, &str)] = &[
    ("AllocatedStorage", "400"),
    ("DBClusterInstanceClass", "db.m5d.large"),
    ("Iops", "3000"),
    ("StorageType", "gp3"),
];

/// The instance class a member of an Aurora cluster is created with.
const AURORA_MEMBER_CLASS: &str = "db.r5.large";

/// One cluster of the table.
struct ClusterCase {
    case: &'static str,
    /// The `DBClusterIdentifier` the create names.
    identifier: &'static str,
    /// What it is stored and reported as: "this parameter is stored as a lowercase string".
    stored_identifier: &'static str,
    engine: &'static str,
    /// How many DB instances the row creates in the cluster — none, one, or a writer and a
    /// replica.
    members: usize,
    /// The instance class those members are created with.
    member_class: &'static str,
    /// The parameters the create adds beyond the identifier, engine and master user.
    extra: &'static [(&'static str, &'static str)],
}

const CLUSTERS: &[ClusterCase] = &[
    ClusterCase {
        // The shape the reader endpoint has to exist for: an Aurora Serverless v2 cluster
        // with no DB instance in it at all, so `DBClusterMembers` is empty and there is no
        // reader instance any name could be read off.
        case: "an Aurora PostgreSQL Serverless v2 cluster with zero members",
        identifier: "emu-cen-zero-members",
        stored_identifier: "emu-cen-zero-members",
        engine: "aurora-postgresql",
        members: 0,
        member_class: AURORA_MEMBER_CLASS,
        extra: &[
            ("ServerlessV2ScalingConfiguration.MinCapacity", "0.5"),
            ("ServerlessV2ScalingConfiguration.MaxCapacity", "1.0"),
        ],
    },
    ClusterCase {
        case: "an Aurora MySQL cluster with one member",
        identifier: "emu-cen-one-member",
        stored_identifier: "emu-cen-one-member",
        engine: "aurora-mysql",
        members: 1,
        member_class: AURORA_MEMBER_CLASS,
        extra: &[],
    },
    ClusterCase {
        case: "an Aurora PostgreSQL cluster with a writer and a replica",
        identifier: "emu-cen-two-members",
        stored_identifier: "emu-cen-two-members",
        engine: "aurora-postgresql",
        members: 2,
        member_class: AURORA_MEMBER_CLASS,
        extra: &[],
    },
    ClusterCase {
        case: "a Multi-AZ DB cluster, the other cluster type",
        identifier: "emu-cen-multiaz",
        stored_identifier: "emu-cen-multiaz",
        engine: "postgres",
        members: 0,
        member_class: AURORA_MEMBER_CLASS,
        extra: MULTI_AZ_SETTINGS,
    },
    ClusterCase {
        case: "an identifier of the published maximum length, written in another case",
        // `DBClusterIdentifier` is 1..63 characters for an Aurora cluster; this one is 63
        // and carries hyphens, and the mixed case is stored lowercased.
        identifier: "EMU-CEN-Aurora-Cluster-With-A-Very-Long-Identifier-000000000001",
        stored_identifier: "emu-cen-aurora-cluster-with-a-very-long-identifier-000000000001",
        engine: "aurora-postgresql",
        members: 0,
        member_class: AURORA_MEMBER_CLASS,
        extra: &[],
    },
];

/// Create the row's cluster and its members, and answer with the envelope the create
/// returned.
async fn create_cluster(
    router: &smithy_server_runtime::MultiServiceRouter,
    row: &ClusterCase,
) -> String {
    let mut request: Vec<(&str, String)> = params![
        "DBClusterIdentifier" => row.identifier,
        "Engine" => row.engine,
        "MasterUsername" => MASTER_USER,
        "MasterUserPassword" => MASTER_PASSWORD,
        "DatabaseName" => DATABASE,
    ]
    .to_vec();
    request.extend(
        row.extra
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    let created = the_cluster(
        row.case,
        &expect_ok(row.case, call(router, "CreateDBCluster", &request).await),
    );
    for member in 0..row.members {
        expect_ok(
            row.case,
            call(
                router,
                "CreateDBInstance",
                &params![
                    "DBInstanceIdentifier" => format!("{}-m{member}", row.stored_identifier),
                    "DBClusterIdentifier" => row.identifier,
                    "Engine" => row.engine,
                    "DBInstanceClass" => row.member_class,
                ],
            )
            .await,
        );
    }
    created
}

/// Poll the cluster until it reports `available`, which is the state a client dials it in.
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

/// The addresses `host` resolves to, asserting that every one of them is an address of THIS
/// machine's loopback interface — the interface the core binds its listeners on, and so the
/// only interface a name this emulator announces may lead to.
async fn resolved_to_loopback(case: &str, which: &str, host: &str, port: u16) -> Vec<SocketAddr> {
    let resolved: Vec<SocketAddr> = tokio::net::lookup_host((host, port))
        .await
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the {which} endpoint {host:?} must resolve, got {error}")
        })
        .collect();
    assert!(
        !resolved.is_empty() && resolved.iter().all(|address| address.ip().is_loopback()),
        "case {case:?}: the {which} endpoint {host:?} must resolve to this machine's loopback \
         interface, got {resolved:?}"
    );
    resolved
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_cluster_publishes_two_identifier_derived_endpoint_names_whatever_its_membership() {
    let router = emulator();
    for row in CLUSTERS {
        let created = create_cluster(&router, row).await;
        let settled = available_cluster(&router, row.case, row.identifier).await;

        let writer = text(&settled, "Endpoint");
        let reader = text(&settled, "ReaderEndpoint");
        assert_endpoint_name_pair(
            row.case,
            row.stored_identifier,
            REGION,
            SERVED_SUFFIX,
            &writer,
            &reader,
        );
        // The membership the row stood up, which the rule holds ACROSS: the two names are
        // the cluster's own either way.
        let members = subtrees(&settled, "DBClusterMember").len();
        assert_eq!(
            members, row.members,
            "case {:?}: expected {} member(s) in the cluster, got {members} in {settled}",
            row.case, row.members
        );
        // The port is the pair's, reported as an integer of its own, and the zone the names
        // sit in is reported beside them.
        let port = text(&settled, "Port");
        assert!(
            port.parse::<u16>().is_ok_and(|port| port != 0),
            "case {:?}: expected the port the cluster's engine listens on, got {port:?}",
            row.case
        );
        let zone = text(&settled, "HostedZoneId");
        assert!(
            zone.starts_with('Z') && zone.len() > 1,
            "case {:?}: expected the hosted zone the endpoints live in, got {zone:?}",
            row.case
        );

        // Minted ONCE: the create published the names the describe reports.
        for member in ["Endpoint", "ReaderEndpoint", "Port", "HostedZoneId"] {
            assert_eq!(
                text(&created, member),
                text(&settled, member),
                "case {:?}: the {member} the create published must be the one the describe \
                 reports",
                row.case
            );
        }

        // And the cluster's own two endpoints are reported as such: the writer entry carries
        // the writer name and the reader entry the reader name, so a client that reads the
        // endpoint report rather than the cluster envelope is told the same pair.
        // `DescribeDBClusterEndpoints` "only applies to Aurora DB clusters", so a Multi-AZ DB
        // cluster has no endpoint this report describes.
        if !row.engine.starts_with("aurora") {
            continue;
        }
        let reported = expect_ok(
            row.case,
            call(
                &router,
                "DescribeDBClusterEndpoints",
                &params!["DBClusterIdentifier" => row.identifier],
            )
            .await,
        );
        for (endpoint_type, expected) in [("WRITER", &writer), ("READER", &reader)] {
            let found: Vec<String> = subtrees(&reported, "DBClusterEndpointList")
                .into_iter()
                .filter(|entry| text(entry, "EndpointType") == endpoint_type)
                .map(|entry| text(&entry, "Endpoint"))
                .collect();
            assert_eq!(
                found,
                vec![expected.clone()],
                "case {:?}: expected the {endpoint_type} endpoint {expected:?} in {reported}",
                row.case
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restored_clusters_endpoint_names_follow_the_identifier_the_restore_gave_it() {
    let router = emulator();
    let case = "a cluster restored from a snapshot of another";
    let source = "emu-cen-restore-source";
    let restored = "emu-cen-restore-target";
    expect_ok(
        case,
        call(
            &router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => source,
                "Engine" => "aurora-postgresql",
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
                "DatabaseName" => DATABASE,
            ],
        )
        .await,
    );
    available_cluster(&router, case, source).await;
    let snapshot = format!("{source}-snap");
    expect_ok(
        case,
        call(
            &router,
            "CreateDBClusterSnapshot",
            &params![
                "DBClusterIdentifier" => source,
                "DBClusterSnapshotIdentifier" => snapshot,
            ],
        )
        .await,
    );
    // A snapshot is restored from once it is available, which the clock makes due: the
    // CONDITION is polled for, never a span.
    let snapshot_params = [("DBClusterSnapshotIdentifier", snapshot.as_str())];
    poll_until(
        case,
        "the cluster snapshot",
        || call(&router, "DescribeDBClusterSnapshots", &snapshot_params),
        |status, body| status == 200 && text(body, "Status") == "available",
    )
    .await;
    let answered = the_cluster(
        case,
        &expect_ok(
            case,
            call(
                &router,
                "RestoreDBClusterFromSnapshot",
                &params![
                    "DBClusterIdentifier" => restored,
                    "SnapshotIdentifier" => snapshot,
                    "Engine" => "aurora-postgresql",
                ],
            )
            .await,
        ),
    );
    // The restored cluster is a cluster of its own, so it mints its own pair of names: both
    // carry ITS identifier, and neither carries the source's.
    let settled = available_cluster(&router, case, restored).await;
    for (which, envelope) in [
        ("RestoreDBClusterFromSnapshot", &answered),
        ("DescribeDBClusters", &settled),
    ] {
        let writer = text(envelope, "Endpoint");
        let reader = text(envelope, "ReaderEndpoint");
        assert_endpoint_name_pair(
            &format!("{case} ({which})"),
            restored,
            REGION,
            SERVED_SUFFIX,
            &writer,
            &reader,
        );
        for (member, name) in [("Endpoint", &writer), ("ReaderEndpoint", &reader)] {
            assert!(
                !name.contains(source),
                "case {case:?} ({which}): the restored cluster's {member} must not name the \
                 source cluster, got {name:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn both_published_endpoint_names_lead_to_the_plane_they_name() {
    let (router, engine) = emulator_with_engine();
    let case = "an Aurora PostgreSQL cluster with no members";
    let identifier = "emu-cen-reachable";
    expect_ok(
        case,
        call(
            &router,
            "CreateDBCluster",
            &params![
                "DBClusterIdentifier" => identifier,
                "Engine" => "aurora-postgresql",
                "MasterUsername" => MASTER_USER,
                "MasterUserPassword" => MASTER_PASSWORD,
                "DatabaseName" => DATABASE,
                "ServerlessV2ScalingConfiguration.MinCapacity" => "0.5",
                "ServerlessV2ScalingConfiguration.MaxCapacity" => "1.0",
            ],
        )
        .await,
    );
    let settled = available_cluster(&router, case, identifier).await;
    let (writer, port) = cluster_endpoint(case, &settled);
    let reader = text(&settled, "ReaderEndpoint");
    assert_endpoint_name_pair(case, identifier, REGION, SERVED_SUFFIX, &writer, &reader);

    // Both names resolve to the interface the cluster's own listeners are bound on.
    for (which, name) in [("writer", &writer), ("reader", &reader)] {
        resolved_to_loopback(case, which, name, port).await;
    }

    // And the WRITER pair a client composes — the published name with the published `Port`
    // — is an address a session really opens on: the master user is admitted and the rows
    // the session writes are the rows it reads back.
    let mut session = postgres_login(
        &writer,
        port,
        // The in-band TLS upgrade this wire negotiates on the same port, which an RDS for
        // PostgreSQL endpoint of a current version requires.
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("case {case:?}: the published pair {writer}:{port} must serve a session, got {refusal:?}")
    });
    for statement in [
        "CREATE TABLE ledger (id integer PRIMARY KEY, note text)",
        "INSERT INTO ledger (id, note) VALUES (1, 'written through the published name')",
    ] {
        session.query(statement).await.unwrap_or_else(|refusal| {
            panic!("case {case:?}: {statement:?} must run on the writer endpoint, got {refusal:?}")
        });
    }
    let read = session
        .query("SELECT note FROM ledger WHERE id = 1")
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case:?}: the session must read its own row, got {refusal:?}")
        });
    assert_eq!(
        read.only().map(str::to_string).as_deref(),
        Some("written through the published name"),
        "case {case:?}: expected the row the session wrote, got {read:?}"
    );

    // A client running INSIDE this emulator reaches the same two addresses: each published
    // name, with the published port, resolves to a plane this process answers on — so an
    // endpoint read out of the control plane is an endpoint such a client can dial.
    let planes = service_common::dialable_data_planes::DialablePlanes::of(&*engine)
        .expect("the engine vends the records of the planes it has opened");
    for (which, name) in [("writer", &writer), ("reader", &reader)] {
        let dialed = planes.dialable(name, port);
        assert!(
            dialed.is_some(),
            "case {case:?}: the {which} pair {name}:{port} must resolve to a plane this \
             emulator serves, got {dialed:?}"
        );
    }
    // The names this emulator did NOT publish resolve to nothing, so the register answers
    // for the addresses the control plane reports and not for any name in the zone.
    let unpublished = format!("emu-cen-nothing.cluster-abcdefghijkl.{REGION}.{SERVED_SUFFIX}");
    assert!(
        planes.dialable(&unpublished, port + 1).is_none(),
        "case {case:?}: no plane answers at a port nothing was opened on"
    );
    assert!(
        texts(&settled, "ReadReplicaIdentifier").is_empty(),
        "case {case:?}: the cluster has no read replica, got {settled}"
    );
}
