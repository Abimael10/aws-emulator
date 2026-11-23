// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the ADDRESS a custom DB-cluster endpoint publishes and the MEMBER a
//! session opened on it is routed to.
//!
//! The rule under test: a custom endpoint created by `CreateDBClusterEndpoint` is an address
//! a client CONNECTS to — "a custom endpoint for an Aurora cluster represents a set of DB
//! instances that you choose" — so for a cluster whose engine wire this emulator serves it
//! publishes an address of its OWN that answers that wire, and a session opened on it is
//! served by one of the members its `CustomEndpointType` and its membership lists select and
//! by no other: a session routed to a member the cluster does not write through may not
//! write, and one routed to the member it does write through may. The selection is read
//! again for every session, so a `ModifyDBClusterEndpoint` that changes the membership
//! changes which member the SAME published address reaches.
//!
//! Everything runs in ONE process: the cluster's and each endpoint's listeners are opened by
//! the calls below, and the client is the one the sibling endpoint suites drive
//! ([`crate::db_instance_engine_endpoint`]), written against each wire itself, so what is
//! exercised is what a stock driver speaks.
//!
//! The rows span what the rule generalises over: each published `CustomEndpointType`, a
//! membership given as static members, as exclusions and as neither, a group holding the
//! member the cluster writes through and a group holding one it does not, a membership
//! replaced under the endpoint's live address, and both wires this emulator serves a cluster's
//! data plane on (Aurora PostgreSQL and Aurora MySQL).
//!
//! Nothing here waits out a span of wall-clock: every wait is the shared poll loop
//! ([`crate::lifecycle_poll_loops::poll_until`]), which re-reads a REPORTING call until the
//! condition the case needs holds.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instance_engine_endpoint::{mysql_login, postgres_login, PostgresSession, Transport};
use crate::db_instances::{call, emulator, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// One cluster this suite stands endpoints in front of: the cluster, the member it writes
/// through (created first, so `DBClusterMembers` reports it as the writer) and a second
/// member it does not write through.
///
/// Two of them, because one DB cluster holds at most
/// `DB_CLUSTER_ENDPOINT_QUOTA_PER_CLUSTER` custom endpoints and the table below spans more
/// than that.
struct Cluster {
    identifier: &'static str,
    writer: &'static str,
    reader: &'static str,
}

const CLUSTERS: &[Cluster] = &[
    Cluster {
        identifier: "emu-ccer-cluster-a",
        writer: "emu-ccer-a-writer",
        reader: "emu-ccer-a-reader",
    },
    Cluster {
        identifier: "emu-ccer-cluster-b",
        writer: "emu-ccer-b-writer",
        reader: "emu-ccer-b-reader",
    },
];

/// The master user the clusters are created with, and its password: a test credential, and
/// the only place in this suite it appears.
const MASTER_USER: &str = "sim2real";
const MASTER_PASSWORD: &str = "Sim2RealPassw0rd";

/// The `DatabaseName` the clusters are created with, which every session here lands in.
const DATABASE: &str = "routingtruth";

/// The table each cluster's own writer endpoint seeds, which every session below reads.
const TABLE: &str = "routed_rows";

/// Create one of those clusters with its two members, seed the row every session reads, and
/// answer with the address the cluster's own writer endpoint publishes.
async fn a_cluster_with_two_members(
    router: &MultiServiceRouter,
    fixture: &Cluster,
) -> (String, u16) {
    expect_ok(
        fixture.identifier,
        call(
            router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", fixture.identifier),
                ("Engine", "aurora-postgresql"),
                ("MasterUsername", MASTER_USER),
                ("MasterUserPassword", MASTER_PASSWORD),
                ("DatabaseName", DATABASE),
            ],
        )
        .await,
    );
    for member in [fixture.writer, fixture.reader] {
        expect_ok(
            member,
            call(
                router,
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", member),
                    ("Engine", "aurora-postgresql"),
                    ("DBInstanceClass", "db.r5.large"),
                    ("DBClusterIdentifier", fixture.identifier),
                ],
            )
            .await,
        );
    }
    // A client connects once the resources report `available`, which the clock makes due and
    // the next read materialises.
    let cluster = poll_cluster_available(router, fixture.identifier).await;
    for member in [fixture.writer, fixture.reader] {
        poll_member_available(router, member).await;
    }
    let port: u16 = text(&cluster, "Port").parse().unwrap_or_else(|error| {
        panic!("the cluster publishes a port, got {cluster} ({error})")
    });
    let address = text(&cluster, "Endpoint");

    // The cluster's own writer endpoint seeds the one row every session below reads.
    let mut writer = session(fixture.identifier, &address, port).await;
    for statement in [
        format!("CREATE TABLE {TABLE} (id serial PRIMARY KEY, label text)"),
        format!("INSERT INTO {TABLE} (label) VALUES ('seed')"),
    ] {
        writer.query(&statement).await.unwrap_or_else(|refusal| {
            panic!(
                "the seed {statement:?} must run on {}'s writer endpoint, got {refusal:?}",
                fixture.identifier
            )
        });
    }
    (address, port)
}

/// Read the cluster until it reports `available`, and answer with its `<DBCluster>`.
async fn poll_cluster_available(router: &MultiServiceRouter, identifier: &str) -> String {
    let params = [("DBClusterIdentifier", identifier.to_string())];
    let (body, _) = poll_until(
        identifier,
        "the cluster reaching available",
        || call(router, "DescribeDBClusters", &params),
        |status, body| {
            status == 200
                && subtrees(body, "DBCluster")
                    .first()
                    .is_some_and(|cluster| text(cluster, "Status") == "available")
        },
    )
    .await;
    subtrees(&body, "DBCluster")
        .into_iter()
        .next()
        .expect("the poll settled on a report carrying the cluster")
}

/// Read one member until it reports `available`.
async fn poll_member_available(router: &MultiServiceRouter, identifier: &str) {
    let params = [("DBInstanceIdentifier", identifier.to_string())];
    poll_until(
        identifier,
        "the member reaching available",
        || call(router, "DescribeDBInstances", &params),
        |status, body| {
            status == 200
                && subtrees(body, "DBInstance")
                    .first()
                    .is_some_and(|member| text(member, "DBInstanceStatus") == "available")
        },
    )
    .await;
}

/// How a row names the membership of the endpoint it creates, against the members of the
/// cluster it stands in front of.
#[derive(Clone, Copy, Debug)]
enum Membership {
    /// Neither list: "all other eligible instances are reachable through the custom
    /// endpoint".
    Any,
    /// `StaticMembers` naming the member the cluster WRITES through: "list of DB instance
    /// identifiers that are part of the custom endpoint group".
    StaticWriter,
    /// `StaticMembers` naming the member it does not write through.
    StaticReader,
    /// `ExcludedMembers` naming the member it writes through: the instances that are NOT part
    /// of the group.
    ExcludedWriter,
}

impl Membership {
    /// The request parameters this membership is sent as, against `fixture`'s members.
    fn params(self, fixture: &Cluster) -> Vec<(&'static str, String)> {
        match self {
            Self::Any => Vec::new(),
            Self::StaticWriter => {
                vec![("StaticMembers.member.1", fixture.writer.to_string())]
            }
            Self::StaticReader => {
                vec![("StaticMembers.member.1", fixture.reader.to_string())]
            }
            Self::ExcludedWriter => {
                vec![("ExcludedMembers.member.1", fixture.writer.to_string())]
            }
        }
    }
}

/// One row of the routing table: an endpoint created in front of one of the clusters, and
/// whether a session opened on the address it publishes may WRITE — which is what says WHICH
/// member it was routed to.
struct Row {
    case: &'static str,
    identifier: &'static str,
    /// Which of [`CLUSTERS`] the endpoint stands in front of.
    cluster: usize,
    /// The `EndpointType` the create names, which the endpoint reports as its
    /// `CustomEndpointType`.
    endpoint_type: &'static str,
    membership: Membership,
    /// Whether a session on the endpoint's address may write: true exactly when the endpoint
    /// routes to the member the cluster writes through.
    writes: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "a READER endpoint over the member the cluster does not write through",
        identifier: "emu-ccer-reader-static",
        cluster: 0,
        endpoint_type: "READER",
        membership: Membership::StaticReader,
        writes: false,
    },
    Row {
        case: "a READER endpoint over every eligible member",
        identifier: "emu-ccer-reader-any",
        cluster: 0,
        endpoint_type: "READER",
        membership: Membership::Any,
        writes: false,
    },
    Row {
        case: "a WRITER endpoint",
        identifier: "emu-ccer-writer",
        cluster: 0,
        endpoint_type: "WRITER",
        membership: Membership::Any,
        writes: true,
    },
    Row {
        case: "an ANY endpoint pinned to the member the cluster writes through",
        identifier: "emu-ccer-any-writer",
        cluster: 1,
        endpoint_type: "ANY",
        membership: Membership::StaticWriter,
        writes: true,
    },
    Row {
        case: "an ANY endpoint pinned to the member it does not write through",
        identifier: "emu-ccer-any-reader",
        cluster: 1,
        endpoint_type: "ANY",
        membership: Membership::StaticReader,
        writes: false,
    },
    Row {
        case: "an ANY endpoint that leaves the writing member out of its group",
        identifier: "emu-ccer-any-excluded",
        cluster: 1,
        endpoint_type: "ANY",
        membership: Membership::ExcludedWriter,
        writes: false,
    },
];

/// `CreateDBClusterEndpoint` for one row, polled until the endpoint reports `available`, and
/// the address it publishes.
async fn an_endpoint(router: &MultiServiceRouter, row: &Row, fixture: &Cluster) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterEndpointIdentifier", row.identifier.to_string()),
        ("DBClusterIdentifier", fixture.identifier.to_string()),
        ("EndpointType", row.endpoint_type.to_string()),
    ];
    params.extend(row.membership.params(fixture));
    expect_ok(
        row.case,
        call(router, "CreateDBClusterEndpoint", &params).await,
    );
    published_address(router, row.case, row.identifier).await
}

/// The address the endpoint `identifier` publishes once it reports `available`.
async fn published_address(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    let params = [("DBClusterEndpointIdentifier", identifier.to_string())];
    let (body, _) = poll_until(
        case,
        &format!("{identifier} reaching available"),
        || call(router, "DescribeDBClusterEndpoints", &params),
        |status, body| {
            status == 200
                && subtrees(body, "DBClusterEndpointList")
                    .first()
                    .is_some_and(|endpoint| {
                        optional_text(endpoint, "Status").as_deref() == Some("available")
                    })
        },
    )
    .await;
    let endpoint = subtrees(&body, "DBClusterEndpointList")
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the report must carry {identifier}"));
    text(&endpoint, "Endpoint")
}

/// The `host` and `port` an endpoint's published `host:port` names, which is the pair a
/// client dials: the modelled endpoint shape carries no port member of its own, so the
/// address carries the port of the listener opened for it.
fn dialable(case: &str, address: &str) -> (String, u16) {
    let (host, port) = address
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("case {case:?}: expected <host>:<port>, got {address:?}"));
    let port: u16 = port
        .parse()
        .unwrap_or_else(|error| panic!("case {case:?}: expected a port, got {port:?} ({error})"));
    (host.trim_matches(['[', ']']).to_string(), port)
}

/// One session on `address:port`, logged in as the cluster's master user.
async fn session(case: &str, address: &str, port: u16) -> PostgresSession {
    postgres_login(
        address,
        port,
        Transport::Tls,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!(
            "case {case:?}: the master user must be admitted on {address}:{port}, got {refusal:?}"
        )
    })
}

/// Whether a session on one endpoint may WRITE, read by asking it to write: the row it
/// inserts is removed again when the write is served, so the rows every other case reads are
/// the ones the seed left.
async fn serves_writes(case: &str, address: &str) -> bool {
    let (host, port) = dialable(case, address);
    let mut session = session(case, &host, port).await;
    // Every session reaches the cluster's VOLUME, whichever member it was routed to: the rows
    // the writer endpoint seeded are the rows it reads.
    let seeded = session
        .query(&format!("SELECT count(*) FROM {TABLE}"))
        .await
        .unwrap_or_else(|refusal| {
            panic!("case {case:?}: a session on a custom endpoint reads the cluster's rows, got {refusal:?}")
        });
    assert_eq!(
        seeded.only(),
        Some("1"),
        "case {case:?}: expected the row the cluster's writer endpoint seeded"
    );
    let written = session
        .query(&format!("INSERT INTO {TABLE} (label) VALUES ('{case}')"))
        .await;
    if written.is_ok() {
        session
            .query(&format!("DELETE FROM {TABLE} WHERE label = '{case}'"))
            .await
            .unwrap_or_else(|refusal| {
                panic!("case {case:?}: the write it served must be undone, got {refusal:?}")
            });
    }
    written.is_ok()
}

#[tokio::test]
async fn a_custom_endpoint_publishes_its_own_address_and_routes_to_the_members_it_selects() {
    let router = emulator();
    let mut clusters: Vec<(String, u16)> = Vec::new();
    for fixture in CLUSTERS {
        clusters.push(a_cluster_with_two_members(&router, fixture).await);
    }

    let mut addresses: Vec<(&str, String)> = Vec::new();
    for row in ROWS {
        let fixture = &CLUSTERS[row.cluster];
        let (cluster_address, cluster_port) = clusters[row.cluster].clone();
        let address = an_endpoint(&router, row, fixture).await;
        // The address is the endpoint's OWN: no two of them collapse onto one, and none of
        // them is the address the cluster publishes for its writer endpoint, or a session
        // could not be routed to the members one endpoint selects.
        for (other, held) in &addresses {
            assert_ne!(
                *held, address,
                "case {:?}: every custom endpoint publishes an address of its own, got {address:?} \
                 for {other:?} as well",
                row.case
            );
        }
        assert_ne!(
            address,
            format!("{cluster_address}:{cluster_port}"),
            "case {:?}: a custom endpoint is not the cluster's own endpoint, got {address:?}",
            row.case
        );
        addresses.push((row.case, address.clone()));

        let writes = serves_writes(row.case, &address).await;
        assert_eq!(
            writes, row.writes,
            "case {:?}: expected a session on {address:?} to {} writes, got {writes}",
            row.case,
            if row.writes { "serve" } else { "refuse" }
        );
    }
}

#[tokio::test]
async fn an_aurora_mysql_custom_endpoint_serves_the_mysql_wire_at_the_address_it_publishes() {
    let router = emulator();
    let cluster = "emu-ccer-mysql";
    let writer_member = "emu-ccer-mysql-writer";
    let reader_member = "emu-ccer-mysql-reader";
    expect_ok(
        cluster,
        call(
            &router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", cluster),
                ("Engine", "aurora-mysql"),
                ("MasterUsername", MASTER_USER),
                ("MasterUserPassword", MASTER_PASSWORD),
                ("DatabaseName", DATABASE),
            ],
        )
        .await,
    );
    for member in [writer_member, reader_member] {
        expect_ok(
            member,
            call(
                &router,
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", member),
                    ("Engine", "aurora-mysql"),
                    ("DBInstanceClass", "db.r5.large"),
                    ("DBClusterIdentifier", cluster),
                ],
            )
            .await,
        );
    }
    poll_cluster_available(&router, cluster).await;
    for member in [writer_member, reader_member] {
        poll_member_available(&router, member).await;
    }

    // A READER endpoint over the member the cluster does not write through — the shape a
    // client hands a read-only workload.
    let identifier = "emu-ccer-mysql-ep";
    expect_ok(
        identifier,
        call(
            &router,
            "CreateDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", identifier),
                ("DBClusterIdentifier", cluster),
                ("EndpointType", "READER"),
                ("StaticMembers.member.1", reader_member),
            ],
        )
        .await,
    );
    let address = published_address(&router, identifier, identifier).await;
    let (host, port) = dialable(identifier, &address);

    // The MySQL wire really is served at that address: a stock client completes the
    // handshake, logs in as the cluster's master user and runs a statement.
    let mut session = mysql_login(
        &host,
        port,
        Transport::Plaintext,
        MASTER_USER,
        MASTER_PASSWORD,
        Some(DATABASE),
    )
    .await
    .unwrap_or_else(|refusal| {
        panic!("the master user must be admitted on {host}:{port}, got {refusal:?}")
    });
    let answer = session
        .query("SELECT 1")
        .await
        .unwrap_or_else(|refusal| panic!("a session on {host}:{port} must run a query, got {refusal:?}"));
    assert_eq!(
        answer.only(),
        Some("1"),
        "expected the engine's own answer on the custom endpoint, got {answer:?}"
    );
    // And it is routed to a member the cluster does not write through, so it writes nothing.
    let refused = session
        .query("CREATE TABLE routed_elsewhere (id INT)")
        .await;
    assert!(
        refused.is_err(),
        "a session on a READER endpoint over a non-writing member writes nothing, got {refused:?}"
    );
}

#[tokio::test]
async fn a_modified_membership_changes_which_member_the_same_address_reaches() {
    let router = emulator();
    let fixture = &CLUSTERS[0];
    a_cluster_with_two_members(&router, fixture).await;

    // An ANY endpoint whose group holds only the member the cluster does NOT write through:
    // its sessions write nothing.
    let row = Row {
        case: "an ANY endpoint pinned to the member it does not write through",
        identifier: "emu-ccer-moved",
        cluster: 0,
        endpoint_type: "ANY",
        membership: Membership::StaticReader,
        writes: false,
    };
    let address = an_endpoint(&router, &row, fixture).await;
    assert!(
        !serves_writes("before the membership moves", &address).await,
        "a session routed to a member the cluster does not write through writes nothing"
    );

    // The same endpoint, with the WRITING member in its group instead: the address is
    // unchanged — "it remains the same for the whole life of the endpoint" — and the next
    // session on it reaches that member.
    expect_ok(
        "the modification",
        call(
            &router,
            "ModifyDBClusterEndpoint",
            &[
                ("DBClusterEndpointIdentifier", row.identifier.to_string()),
                ("StaticMembers.member.1", fixture.writer.to_string()),
            ],
        )
        .await,
    );
    let moved = published_address(&router, "after the membership moves", row.identifier).await;
    assert_eq!(
        moved, address,
        "a modification keeps the endpoint's published address, got {moved:?} for {address:?}"
    );
    assert!(
        serves_writes("after the membership moves", &moved).await,
        "a session routed to the member the cluster writes through serves writes"
    );
}
