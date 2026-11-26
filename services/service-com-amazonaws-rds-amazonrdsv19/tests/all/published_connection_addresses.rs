// SPDX-License-Identifier: CC0-1.0

//! WHAT a client of this emulator is handed to open a session on a resource whose engine wire
//! this emulator serves, across every resource KIND of this control plane that publishes one.
//!
//! The rule under test: the member a client COMPOSES ITS CONNECTION FROM publishes a host this
//! emulator ANSWERS on — the host in the member and the port in the shape's own port member
//! where it has one (`DBCluster$Endpoint` beside `DBCluster$Port`,
//! `DBInstance$Endpoint$Address` beside `DBInstance$Endpoint$Port`), and the `host:port`
//! authority where the shape carries no port member of its own (`DBClusterEndpoint$Endpoint`).
//! A host a client is handed is one it RESOLVES and then dials, so it is either the address the
//! listener the core opened for that resource is answered at or a name in the zone that
//! interface is named in — the loopback zone, whose names RFC 6761 reserves for it, which is
//! where a DB cluster announces the pair it mints from its own identifier. The port is the one
//! that resource's own listener landed on.
//!
//! The rows span what the rule generalises over: the resource kinds that publish a connection
//! address (a DB cluster, a DB instance, a custom endpoint of a cluster), both engine wires
//! this emulator speaks (PostgreSQL and MySQL), both cluster types (an Aurora cluster and a
//! Multi-AZ DB cluster) and a cluster with no member instance at all. Each published pair is
//! then looked up in the register an in-process client of this emulator dials through, and
//! DIALLED over a real TCP connection on which a row is round-tripped — so a member carrying
//! an address nothing answers on could not pass.

use std::net::IpAddr;

use crate::db_cluster_engine_endpoint::MULTI_AZ_SETTINGS;
use crate::db_clusters::the_cluster;
use crate::db_instance_engine_endpoint::{
    endpoint_of, wire_login as login, Wire, DATABASE, MASTER_PASSWORD, MASTER_USER,
};
use crate::db_instances::{call, emulator_with_engine, expect_ok, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// WHICH resource of this control plane the published address is read off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// A DB cluster, whose `Endpoint` is a host beside the cluster's one `Port`.
    Cluster,
    /// A DB instance, whose `Endpoint` carries an `Address` and a `Port` of its own.
    Instance,
    /// A CUSTOM endpoint created in front of a cluster, whose modelled shape has no port
    /// member at all, so the whole authority is published in `Endpoint`.
    CustomEndpoint,
}

/// One resource of the table.
struct Row {
    case: &'static str,
    kind: Kind,
    identifier: &'static str,
    engine: &'static str,
    wire: Wire,
    /// The parameters the create adds beyond the identifier, engine, database and master user.
    extra: &'static [(&'static str, &'static str)],
}

const RESOURCES: &[Row] = &[
    Row {
        case: "an Aurora PostgreSQL cluster with no member instance at all",
        kind: Kind::Cluster,
        identifier: "emu-pca-aurora-pg",
        engine: "aurora-postgresql",
        wire: Wire::Postgres,
        extra: &[],
    },
    Row {
        case: "an Aurora MySQL cluster, the other wire",
        kind: Kind::Cluster,
        identifier: "emu-pca-aurora-mysql",
        engine: "aurora-mysql",
        wire: Wire::MySql,
        extra: &[],
    },
    Row {
        case: "a Multi-AZ DB cluster, the other cluster type",
        kind: Kind::Cluster,
        identifier: "emu-pca-multiaz-pg",
        engine: "postgres",
        wire: Wire::Postgres,
        extra: MULTI_AZ_SETTINGS,
    },
    Row {
        case: "a custom endpoint in front of an Aurora PostgreSQL cluster",
        kind: Kind::CustomEndpoint,
        identifier: "emu-pca-custom-pg",
        engine: "aurora-postgresql",
        wire: Wire::Postgres,
        extra: &[],
    },
    Row {
        case: "a PostgreSQL DB instance",
        kind: Kind::Instance,
        identifier: "emu-pca-instance-pg",
        engine: "postgres",
        wire: Wire::Postgres,
        extra: &[],
    },
    Row {
        case: "a MySQL DB instance, the other wire",
        kind: Kind::Instance,
        identifier: "emu-pca-instance-mysql",
        engine: "mysql",
        wire: Wire::MySql,
        extra: &[],
    },
];

/// The identifier of the cluster a `Kind::CustomEndpoint` row creates its endpoint on.
fn cluster_of(row: &Row) -> String {
    format!("{}-cluster", row.identifier)
}

/// Create the row's resource and answer with the `host`, `port` pair a client composes its
/// connection from, once that resource reports the state a client dials it in.
async fn published_address(
    router: &smithy_server_runtime::MultiServiceRouter,
    row: &Row,
) -> (String, u16) {
    match row.kind {
        Kind::Cluster => {
            create_cluster(router, row, row.identifier).await;
            let settled = available_cluster(router, row.case, row.identifier).await;
            let endpoint = text(&settled, "Endpoint");
            let port = text(&settled, "Port").parse::<u16>().unwrap_or_else(|error| {
                panic!(
                    "case {:?}: the cluster must publish the port its engine listens on, got \
                     {:?}: {error}",
                    row.case,
                    text(&settled, "Port")
                )
            });
            // The member is a HOST: the cluster has one `Port` for both of its endpoints, so
            // the connection endpoint carries no port of its own.
            assert!(
                !endpoint.contains(':'),
                "case {:?}: expected a bare host in <Endpoint>, got {endpoint:?}",
                row.case
            );
            (endpoint, port)
        }
        Kind::Instance => {
            let created = create_instance(router, row).await;
            let published = endpoint_of(&created);
            let settled = available_instance(router, row.case, row.identifier).await;
            assert_eq!(
                endpoint_of(&settled),
                published,
                "case {:?}: the endpoint the create published must be the one the describe \
                 reports",
                row.case
            );
            published
        }
        Kind::CustomEndpoint => {
            let cluster = cluster_of(row);
            create_cluster(router, row, &cluster).await;
            available_cluster(router, row.case, &cluster).await;
            expect_ok(
                row.case,
                call(
                    router,
                    "CreateDBClusterEndpoint",
                    &[
                        ("DBClusterEndpointIdentifier", row.identifier),
                        ("DBClusterIdentifier", cluster.as_str()),
                        ("EndpointType", "ANY"),
                    ],
                )
                .await,
            );
            let params = [("DBClusterEndpointIdentifier", row.identifier)];
            let (settled, _polls) = poll_until(
                row.case,
                "the custom endpoint",
                || call(router, "DescribeDBClusterEndpoints", &params),
                |status, body| status == 200 && text(body, "Status") == "available",
            )
            .await;
            // The modelled `DBClusterEndpoint` has no port member, so the whole authority is
            // published here and a client splits it as it would any `host:port`.
            let address = text(&settled, "Endpoint");
            let (host, port) = address.rsplit_once(':').unwrap_or_else(|| {
                panic!(
                    "case {:?}: expected the endpoint's own <host>:<port>, got {address:?}",
                    row.case
                )
            });
            let port = port.parse::<u16>().unwrap_or_else(|error| {
                panic!(
                    "case {:?}: expected the port that listener was assigned, got {port:?}: \
                     {error}",
                    row.case
                )
            });
            (host.trim_matches(['[', ']']).to_string(), port)
        }
    }
}

/// `CreateDBCluster` for `identifier`, with the row's engine and master user.
async fn create_cluster(
    router: &smithy_server_runtime::MultiServiceRouter,
    row: &Row,
    identifier: &str,
) {
    let mut request: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", identifier.to_string()),
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
    expect_ok(row.case, call(router, "CreateDBCluster", &request).await);
}

/// `CreateDBInstance` for the row's instance, answering the envelope the create returned.
async fn create_instance(
    router: &smithy_server_runtime::MultiServiceRouter,
    row: &Row,
) -> String {
    let mut request: Vec<(&str, String)> = vec![
        ("DBInstanceIdentifier", row.identifier.to_string()),
        ("Engine", row.engine.to_string()),
        ("DBInstanceClass", "db.t3.micro".to_string()),
        ("AllocatedStorage", "20".to_string()),
        ("DBName", DATABASE.to_string()),
        ("MasterUsername", MASTER_USER.to_string()),
        ("MasterUserPassword", MASTER_PASSWORD.to_string()),
    ];
    request.extend(
        row.extra
            .iter()
            .map(|(name, value)| (*name, (*value).to_string())),
    );
    expect_ok(row.case, call(router, "CreateDBInstance", &request).await)
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

/// Poll the DB instance until it reports `available`, answering its `<DBInstance>` subtree.
async fn available_instance(
    router: &smithy_server_runtime::MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> String {
    let params = [("DBInstanceIdentifier", identifier)];
    let (body, _polls) = poll_until(
        case,
        "the DB instance",
        || call(router, "DescribeDBInstances", &params),
        |status, body| status == 200 && text(body, "DBInstanceStatus") == "available",
    )
    .await;
    subtrees(&body, "DBInstance")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the report must carry the instance: {body}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_published_connection_address_is_one_this_emulator_answers_on() {
    let (router, engine) = emulator_with_engine();
    for row in RESOURCES {
        let (host, port) = published_address(&router, row).await;

        // What the rule says: the host is one this emulator answers on — an address of the
        // interface the core bound its listeners on, or a name in the zone that interface is
        // named in, which is what a DB cluster publishes — and the port is the one that
        // resource's own listener landed on.
        crate::db_clusters::assert_own_endpoint_host(row.case, "published", &host);
        let resolved: Vec<std::net::SocketAddr> =
            tokio::net::lookup_host((host.as_str(), port))
                .await
                .unwrap_or_else(|error| {
                    panic!(
                        "case {:?}: the published host {host:?} must resolve, got {error}",
                        row.case
                    )
                })
                .collect();
        assert!(
            !resolved.is_empty()
                && resolved
                    .iter()
                    .all(|address| IpAddr::is_loopback(&address.ip())),
            "case {:?}: expected a host that resolves to the interface the core bound, got \
             {resolved:?} for {host:?}",
            row.case
        );
        assert!(
            port > 0,
            "case {:?}: expected the port the resource's own listener answers on, got {port}",
            row.case
        );

        // An in-process client of this emulator reaches the published pair: the register it
        // dials through answers for that address, served by this service.
        let planes = service_common::dialable_data_planes::DialablePlanes::of(&*engine)
            .expect("the engine vends the records of the planes it has opened");
        let dialed = planes.dialable(&host, port);
        let served_by = dialed.as_ref().map(|dialed| dialed.served_by.clone());
        assert_eq!(
            served_by.as_deref(),
            Some("rds"),
            "case {:?}: the published pair {host}:{port} must resolve to a plane this emulator \
             serves, got {dialed:?}",
            row.case
        );

        // And a client on this machine really opens a session there and reads back what it
        // wrote, which is the whole point of the member it was handed.
        let mut session = login(row.case, row.wire, &host, port, DATABASE).await;
        session
            .run(
                row.case,
                "CREATE TABLE published_address (id integer PRIMARY KEY, note varchar(64))",
            )
            .await;
        session
            .run(
                row.case,
                "INSERT INTO published_address (id, note) VALUES (1, 'dialled what was published')",
            )
            .await;
        let read = session
            .run(row.case, "SELECT note FROM published_address WHERE id = 1")
            .await;
        assert_eq!(
            read.only(),
            Some("dialled what was published"),
            "case {:?}: expected the row the session wrote, got {read:?}",
            row.case
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_clusters_reader_endpoint_names_the_interface_its_read_only_listener_is_reached_on() {
    let (router, _engine) = emulator_with_engine();
    // The reader member is the name the CLUSTER mints under its `cluster-ro-` label, which is
    // what tells a read path from a write path, in the zone a resolver answers with the
    // interface the cluster's listeners are bound on — so it stays an address a client of this
    // emulator resolves.
    let row = &RESOURCES[0];
    let identifier = "emu-pca-reader";
    create_cluster(&router, row, identifier).await;
    let settled = available_cluster(&router, row.case, identifier).await;
    let writer = text(&settled, "Endpoint");
    let reader = text(&settled, "ReaderEndpoint");
    assert_ne!(
        writer, reader,
        "case {:?}: a cluster's two connection endpoints must be distinct, got {writer:?} for \
         both",
        row.case
    );
    crate::db_clusters::assert_own_endpoint_host(row.case, "reader", &reader);
    assert!(
        reader.starts_with(&format!("{identifier}.cluster-ro-")),
        "case {:?}: expected a reader endpoint of the form <identifier>.cluster-ro-<label>…, \
         got {reader:?}",
        row.case
    );
    // A spelling of the loopback interface and nothing of AWS's own: a name in a zone AWS
    // serves would send the client out of the emulated environment.
    assert!(
        service_common::data_plane_addressing::names_the_loopback_interface(&reader),
        "case {:?}: expected a name of the loopback interface, got {reader:?}",
        row.case
    );
}
