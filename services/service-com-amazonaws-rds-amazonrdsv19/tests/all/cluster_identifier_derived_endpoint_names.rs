// SPDX-License-Identifier: CC0-1.0

//! The two endpoint names a DB CLUSTER mints from its own `DBClusterIdentifier`.
//!
//! The rule under test: a DB cluster publishes its write path and its read path as the two DNS
//! host NAMES it mints from its own identifier — `<DBClusterIdentifier>.cluster-<label>` in
//! `DBCluster$Endpoint` and `<DBClusterIdentifier>.cluster-ro-<label>` in
//! `DBCluster$ReaderEndpoint` — over ONE server-assigned label and ONE shared DNS suffix, with
//! the port reported only in the Integer `DBCluster$Port` and the zone only in
//! `DBCluster$HostedZoneId` (`smithy_query(query="shape",shape="DBCluster")`). Both are minted
//! by the CLUSTER, so both exist, keep that form and stay DISTINCT whatever the cluster's
//! membership is — a cluster with no Aurora Replica at all has no reader instance any name
//! could be read off — and the `cluster-`/`cluster-ro-` label is the only thing that tells a
//! read path from a write path. Neither member is ever a `host:port` authority: a client
//! composes the target itself from one name and the one `Port`, and a port glued into the host
//! would appear twice in every DSN it builds.
//!
//! The suffix is the zone the pair is REACHED in, not part of the rule: the zone the interface
//! this emulator answers the cluster's engine wire on is named in for a cluster whose data
//! plane it serves, and the endpoint domain of the control plane that owns the engine for a
//! cluster it serves none for. Either way the two names share it, and for a served cluster the
//! published pair is really dialled below, so a name that led nowhere could not pass.
//!
//! The rows span what the rule generalises over: the MEMBERSHIP the cluster has (no instance at
//! all, one member, a writer beside a replica), the engines and wires (Aurora PostgreSQL,
//! Aurora MySQL, the Multi-AZ DB cluster of a community engine, and an engine this emulator
//! serves no data plane for), and the identifier SHAPES a request can name (the shortest, a
//! long hyphenated one, and one written in another case, which is "stored as a lowercase
//! string"). Every row is read through each operation that answers with a cluster envelope —
//! the create, the describe and the modify — and through `DescribeDBClusterEndpoints`, the
//! report that publishes the same two addresses under their own entries.

use crate::db_clusters::{
    assert_endpoint_name_pair, assert_own_endpoint_host, the_cluster, SERVED_SUFFIX,
};
use crate::db_instance_engine_endpoint::{
    wire_login, Wire, DATABASE, MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, emulator, expect_ok, subtrees, text, REGION};
use crate::lifecycle_poll_loops::poll_until;

/// The instance class a member of an Aurora cluster is created with.
const AURORA_MEMBER_CLASS: &str = "db.r5.large";

/// The settings a Multi-AZ DB cluster is required to be created with, with an `Iops` inside the
/// documented ratio of the allocated storage.
const MULTI_AZ_SETTINGS: &[(&str, &str)] = &[
    ("AllocatedStorage", "400"),
    ("DBClusterInstanceClass", "db.m5d.large"),
    ("Iops", "3000"),
    ("StorageType", "gp3"),
];

/// One cluster of the table.
struct Row {
    case: &'static str,
    /// The `DBClusterIdentifier` the create names.
    identifier: &'static str,
    /// What it is stored and reported as: "this parameter is stored as a lowercase string", so
    /// the names are minted from THIS spelling.
    stored: &'static str,
    engine: &'static str,
    /// The wire a client opens a session on, for a row whose engine this emulator serves one
    /// for — `None` for the row it serves none for, which has no plane to dial.
    wire: Option<Wire>,
    /// The DNS suffix the pair shares: the zone a served cluster's listeners are named in, or
    /// the endpoint domain of the control plane that owns the engine.
    suffix: &'static str,
    /// How many DB instances the row creates in the cluster.
    members: usize,
    /// What the create sends beyond the identity, the engine, the database and the master user.
    extra: &'static [(&'static str, &'static str)],
}

const ROWS: &[Row] = &[
    Row {
        // The shape the reader name has to exist for: no DB instance in the cluster at all, so
        // `DBClusterMembers` is empty and no member instance could be read for a name.
        case: "an Aurora PostgreSQL Serverless v2 cluster with no member instance at all",
        identifier: "emu-cidn-zero-members",
        stored: "emu-cidn-zero-members",
        engine: "aurora-postgresql",
        wire: Some(Wire::Postgres),
        suffix: SERVED_SUFFIX,
        members: 0,
        extra: &[
            ("ServerlessV2ScalingConfiguration.MinCapacity", "0.5"),
            ("ServerlessV2ScalingConfiguration.MaxCapacity", "1.0"),
        ],
    },
    Row {
        case: "an Aurora MySQL cluster with one member, on the other wire",
        identifier: "emu-cidn-one-member",
        stored: "emu-cidn-one-member",
        engine: "aurora-mysql",
        wire: Some(Wire::MySql),
        suffix: SERVED_SUFFIX,
        members: 1,
        extra: &[],
    },
    Row {
        case: "an Aurora PostgreSQL cluster with a writer and a replica",
        identifier: "emu-cidn-two-members",
        stored: "emu-cidn-two-members",
        engine: "aurora-postgresql",
        wire: Some(Wire::Postgres),
        suffix: SERVED_SUFFIX,
        members: 2,
        extra: &[],
    },
    Row {
        case: "a Multi-AZ DB cluster, the other cluster type",
        identifier: "emu-cidn-multiaz",
        stored: "emu-cidn-multiaz",
        engine: "postgres",
        wire: Some(Wire::Postgres),
        suffix: SERVED_SUFFIX,
        members: 0,
        extra: MULTI_AZ_SETTINGS,
    },
    Row {
        // `DBClusterIdentifier` is 1..63 characters: the shortest identifier a request can
        // name, which leaves the labels nothing to lean on.
        case: "the shortest identifier a request can name",
        identifier: "d",
        stored: "d",
        engine: "aurora-postgresql",
        wire: Some(Wire::Postgres),
        suffix: SERVED_SUFFIX,
        members: 0,
        extra: &[],
    },
    Row {
        // The other bound: 63 characters, hyphenated, and written in another case, which is
        // stored lowercased — so the minted names follow the STORED spelling.
        case: "an identifier of the published maximum length, written in another case",
        identifier: "EMU-CIDN-Aurora-Cluster-With-A-Very-Long-Identifier-00000000001",
        stored: "emu-cidn-aurora-cluster-with-a-very-long-identifier-00000000001",
        engine: "aurora-postgresql",
        wire: Some(Wire::Postgres),
        suffix: SERVED_SUFFIX,
        members: 0,
        extra: &[],
    },
    Row {
        // The contrast row: an engine whose wire this emulator serves no data plane for keeps
        // the published AWS suffix, and the rule holds over it unchanged.
        case: "a cluster of an engine this emulator serves no data plane for",
        identifier: "emu-cidn-docdb",
        stored: "emu-cidn-docdb",
        engine: "docdb",
        wire: None,
        suffix: "docdb.amazonaws.com",
        members: 0,
        extra: &[],
    },
];

/// Create the row's cluster and its members, answering the envelope the create published.
async fn create_cluster(
    router: &smithy_server_runtime::MultiServiceRouter,
    row: &Row,
) -> String {
    let mut request: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", row.identifier.to_string()),
        ("Engine", row.engine.to_string()),
        ("DatabaseName", DATABASE.to_string()),
        ("MasterUsername", MASTER_USER.to_string()),
        ("MasterUserPassword", MASTER_PASSWORD.to_string()),
    ];
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
                &[
                    ("DBInstanceIdentifier", format!("{}-m{member}", row.stored)),
                    ("DBClusterIdentifier", row.identifier.to_string()),
                    ("Engine", row.engine.to_string()),
                    ("DBInstanceClass", AURORA_MEMBER_CLASS.to_string()),
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

/// Assert the whole rule on one cluster envelope, whichever operation answered with it, and
/// answer with the pair it published.
fn assert_published_pair(row: &Row, which: &str, cluster: &str) -> (String, String, u16) {
    let case = format!("{} ({which})", row.case);
    let writer = text(cluster, "Endpoint");
    let reader = text(cluster, "ReaderEndpoint");
    let published_port = text(cluster, "Port");
    let port: u16 = published_port.parse().unwrap_or_else(|_| {
        panic!("case {case:?}: expected the port the engine listens on, got {published_port:?}")
    });
    assert!(
        port != 0,
        "case {case:?}: expected a port a client can dial, got {port}"
    );

    // The two names the CLUSTER mints: its stored identifier, the role label, one shared
    // server-assigned label and one shared suffix — and no port in either.
    assert_endpoint_name_pair(&case, row.stored, REGION, row.suffix, &writer, &reader);
    for (member, name) in [("Endpoint", &writer), ("ReaderEndpoint", &reader)] {
        assert!(
            !name.ends_with(&format!(":{port}")) && !name.ends_with(&published_port),
            "case {case:?}: <{member}> leaves the port to <Port>, got {name:?}"
        );
        // A served cluster's names have to stay hosts this emulator answers on, which is what
        // makes the pair an address rather than a shape.
        if row.wire.is_some() {
            assert_own_endpoint_host(&case, member, name);
        }
    }
    // The zone the names live in is reported beside them rather than glued onto either.
    let zone = text(cluster, "HostedZoneId");
    assert!(
        zone.starts_with('Z') && zone.len() > 1,
        "case {case:?}: expected the hosted zone the endpoints live in, got {zone:?}"
    );
    (writer, reader, port)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_cluster_publishes_the_two_names_it_mints_from_its_identifier() {
    let router = emulator();
    for row in ROWS {
        let created = create_cluster(&router, row).await;
        let from_create = assert_published_pair(row, "CreateDBCluster", &created);

        let settled = available_cluster(&router, row.case, row.identifier).await;
        let from_describe = assert_published_pair(row, "DescribeDBClusters", &settled);
        assert_eq!(
            from_create, from_describe,
            "case {:?}: the pair the create published must be the one the describe reports",
            row.case
        );

        // The membership the row stood up, which the rule holds ACROSS: both names are the
        // cluster's own whether or not an instance was ever created in it.
        let members = subtrees(&settled, "DBClusterMember").len();
        assert_eq!(
            members, row.members,
            "case {:?}: expected {} member(s) in the cluster, got {members} in {settled}",
            row.case, row.members
        );

        // A MUTATION answers with the same envelope, and the pair is minted ONCE: a
        // modification that touches no endpoint leaves it as it stood.
        let modified = the_cluster(
            row.case,
            &expect_ok(
                row.case,
                call(
                    &router,
                    "ModifyDBCluster",
                    &[
                        ("DBClusterIdentifier", row.identifier),
                        ("BackupRetentionPeriod", "7"),
                        ("ApplyImmediately", "true"),
                    ],
                )
                .await,
            ),
        );
        let from_modify = assert_published_pair(row, "ModifyDBCluster", &modified);
        assert_eq!(
            from_modify, from_describe,
            "case {:?}: a modification that touches no endpoint must leave the published pair \
             as it stood",
            row.case
        );

        // `DescribeDBClusterEndpoints` publishes the same two addresses under their own
        // entries, so a client that reads the endpoint report rather than the cluster envelope
        // is told the same pair. The report "only applies to Aurora DB clusters".
        if row.engine.starts_with("aurora") {
            let reported = expect_ok(
                row.case,
                call(
                    &router,
                    "DescribeDBClusterEndpoints",
                    &[("DBClusterIdentifier", row.identifier)],
                )
                .await,
            );
            for (endpoint_type, expected) in
                [("WRITER", &from_describe.0), ("READER", &from_describe.1)]
            {
                let found: Vec<String> = subtrees(&reported, "DBClusterEndpointList")
                    .into_iter()
                    .filter(|entry| text(entry, "EndpointType") == endpoint_type)
                    .map(|entry| text(&entry, "Endpoint"))
                    .collect();
                assert_eq!(
                    found,
                    vec![expected.clone()],
                    "case {:?}: expected the {endpoint_type} entry to publish {expected:?} in \
                     {reported}",
                    row.case
                );
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_published_name_leads_to_the_plane_it_names() {
    let router = emulator();
    // One row per wire this emulator serves, the first of them the cluster with no member
    // instance at all: what the name has to do beyond carrying the right labels is lead a
    // client to the plane that answers the session.
    for row in ROWS.iter().filter(|row| row.wire.is_some()) {
        let wire = row.wire.expect("the row names a wire");
        create_cluster(&router, row).await;
        let settled = available_cluster(&router, row.case, row.identifier).await;
        let (writer, reader, port) = assert_published_pair(row, "DescribeDBClusters", &settled);

        // Both names resolve, and to the interface the cluster's own listeners are bound on.
        for (member, name) in [("Endpoint", &writer), ("ReaderEndpoint", &reader)] {
            let resolved: Vec<std::net::SocketAddr> =
                tokio::net::lookup_host((name.as_str(), port))
                    .await
                    .unwrap_or_else(|error| {
                        panic!(
                            "case {:?}: the <{member}> {name:?} must resolve, got {error}",
                            row.case
                        )
                    })
                    .collect();
            assert!(
                !resolved.is_empty() && resolved.iter().all(|address| address.ip().is_loopback()),
                "case {:?}: the <{member}> {name:?} must resolve to the interface this \
                 emulator's listeners are bound on, got {resolved:?}",
                row.case
            );
        }

        // And the WRITER pair a client composes — the published name with the published `Port`
        // — is an address a session really opens on: the master user is admitted and the row
        // the session writes is the row it reads back.
        let mut session = wire_login(row.case, wire, &writer, port, DATABASE).await;
        session
            .run(
                row.case,
                "CREATE TABLE minted_name (id integer PRIMARY KEY, note varchar(64))",
            )
            .await;
        session
            .run(
                row.case,
                "INSERT INTO minted_name (id, note) VALUES (1, 'dialled the minted name')",
            )
            .await;
        let read = session
            .run(row.case, "SELECT note FROM minted_name WHERE id = 1")
            .await;
        assert_eq!(
            read.only(),
            Some("dialled the minted name"),
            "case {:?}: expected the row the session wrote, got {read:?}",
            row.case
        );
    }
}
