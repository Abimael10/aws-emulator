// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHEN a custom DB cluster endpoint becomes observable, and for the
//! duplicate-identifier walk a client drives on top of that.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope the
//! router keys on. The assertions read the XML the emulator answers with, so a handler that
//! computed nothing could not pass them.
//!
//! The rule under test: a resource this service provisions INTO another one reaches its
//! terminal status one span after THAT resource became ready, not one span after the call
//! that asked for it — the span is charged once per dependency level
//! (`crate::state::provisioning_to`). So a custom endpoint created on a cluster that is
//! already up is reported `available` by the FIRST `DescribeDBClusterEndpoints` after the
//! create, while the create itself still answers the `creating` it stored; a client that has
//! polled the cluster to `available` does not pay a second poll interval for the endpoint
//! hanging off it, and the create → duplicate-refused → delete → re-create walk converges in
//! the time AWS's own clients allow.
//!
//! The rows span what the rule generalises over: both Aurora wires (`aurora-postgresql` and
//! `aurora-mysql`), every published `EndpointType`, a membership given as static members, as
//! exclusions and as neither, a cluster with member instances and one with none, and an
//! identifier created, deleted and created again.
//!
//! Nothing here waits out a span of wall-clock: every wait is the shared poll loop
//! ([`crate::lifecycle_poll_loops::poll_until`]), which re-reads a REPORTING call until the
//! condition the case needs holds.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// The status an accepted create answers with.
const CREATING: &str = "creating";

/// The status a usable endpoint reports.
const AVAILABLE: &str = "available";

/// The master user the clusters below are created with, and its password: a test credential.
const MASTER_USER: &str = "emuadmin";
const MASTER_PASSWORD: &str = "emu-Secret-99";

/// One cluster this suite stands endpoints in front of.
struct Fixture {
    cluster: &'static str,
    engine: &'static str,
    /// The member instances created in it, writer first — a cluster with none is served by
    /// the cluster itself, which is a case of its own.
    members: &'static [&'static str],
}

const FIXTURES: &[Fixture] = &[
    Fixture {
        cluster: "emu-ceps-postgres",
        engine: "aurora-postgresql",
        members: &["emu-ceps-pg-writer", "emu-ceps-pg-reader"],
    },
    Fixture {
        cluster: "emu-ceps-mysql",
        engine: "aurora-mysql",
        members: &[],
    },
];

/// `CreateDBCluster` + its members, polled until every one of them is up — the state a client
/// is in when it creates a custom endpoint.
async fn a_settled_cluster(router: &MultiServiceRouter, fixture: &Fixture) {
    expect_ok(
        fixture.cluster,
        call(
            router,
            "CreateDBCluster",
            &[
                ("DBClusterIdentifier", fixture.cluster),
                ("Engine", fixture.engine),
                ("MasterUsername", MASTER_USER),
                ("MasterUserPassword", MASTER_PASSWORD),
            ],
        )
        .await,
    );
    for member in fixture.members.iter().copied() {
        expect_ok(
            member,
            call(
                router,
                "CreateDBInstance",
                &[
                    ("DBInstanceIdentifier", member),
                    ("Engine", fixture.engine),
                    ("DBInstanceClass", "db.r5.large"),
                    ("DBClusterIdentifier", fixture.cluster),
                ],
            )
            .await,
        );
    }
    let params = [("DBClusterIdentifier", fixture.cluster.to_string())];
    poll_until(
        fixture.cluster,
        "the cluster reaching available",
        || call(router, "DescribeDBClusters", &params),
        |status, body| {
            status == 200
                && subtrees(body, "DBCluster")
                    .first()
                    .is_some_and(|cluster| text(cluster, "Status") == AVAILABLE)
        },
    )
    .await;
    for member in fixture.members.iter().copied() {
        let params = [("DBInstanceIdentifier", member.to_string())];
        poll_until(
            member,
            "the member reaching available",
            || call(router, "DescribeDBInstances", &params),
            |status, body| {
                status == 200
                    && subtrees(body, "DBInstance")
                        .first()
                        .is_some_and(|member| text(member, "DBInstanceStatus") == AVAILABLE)
            },
        )
        .await;
    }
}

/// `CreateDBClusterEndpoint` with its three required members plus `extra`.
async fn create_endpoint(
    router: &MultiServiceRouter,
    identifier: &str,
    cluster: &str,
    endpoint_type: &str,
    extra: &[(&str, String)],
) -> (u16, String) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterEndpointIdentifier", identifier.to_string()),
        ("DBClusterIdentifier", cluster.to_string()),
        ("EndpointType", endpoint_type.to_string()),
    ];
    params.extend(extra.iter().cloned());
    call(router, "CreateDBClusterEndpoint", &params).await
}

/// ONE `DescribeDBClusterEndpoints` for `identifier` — the first read a client makes after a
/// create, which is what the rule is about — and the endpoints it reported.
async fn describe_once(router: &MultiServiceRouter, case: &str, identifier: &str) -> Vec<String> {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeDBClusterEndpoints",
            &[("DBClusterEndpointIdentifier", identifier)],
        )
        .await,
    );
    subtrees(&body, "DBClusterEndpointList")
}

/// The one endpoint that read reported, failing the case when it reported none.
async fn described_once(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    describe_once(router, case, identifier)
        .await
        .into_iter()
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: the report must carry {identifier}"))
}

/// Create an endpoint on a cluster whose own span has elapsed and assert the FIRST read after
/// the create reports it `available`, answering with that report.
async fn available_to_the_first_read(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    cluster: &str,
    endpoint_type: &str,
    extra: &[(&str, String)],
) -> String {
    let created = expect_ok(
        case,
        create_endpoint(router, identifier, cluster, endpoint_type, extra).await,
    );
    assert_eq!(
        text(&created, "Status"),
        CREATING,
        "case {case:?}: an accepted create answers the status it stored, got {created}"
    );
    let described = described_once(router, case, identifier).await;
    assert_eq!(
        optional_text(&described, "Status").as_deref(),
        Some(AVAILABLE),
        "case {case:?}: an endpoint created on a cluster that is already up is available to \
         the first read after the create, got {described}"
    );
    described
}

/// Settle the CLUSTER's own span: an endpoint created on it and polled to `available` can
/// only report that once the instant one span after the cluster became ready has passed, so
/// every later create on the same cluster is due the moment it is accepted.
///
/// This is what makes the assertion above a statement about the SPAN and not about how fast
/// the host runs: it waits for a condition, never for a duration.
async fn past_the_clusters_span(router: &MultiServiceRouter, fixture: &Fixture) {
    let identifier = format!("{}-span", fixture.cluster);
    expect_ok(
        &identifier,
        create_endpoint(router, &identifier, fixture.cluster, "ANY", &[]).await,
    );
    let params = [("DBClusterEndpointIdentifier", identifier.clone())];
    poll_until(
        &identifier,
        "the first endpoint of the cluster reaching available",
        || call(router, "DescribeDBClusterEndpoints", &params),
        |status, body| {
            status == 200
                && subtrees(body, "DBClusterEndpointList")
                    .first()
                    .is_some_and(|endpoint| {
                        optional_text(endpoint, "Status").as_deref() == Some(AVAILABLE)
                    })
        },
    )
    .await;
}

/// How a row names the membership of the endpoint it creates.
#[derive(Clone, Copy, Debug)]
enum Membership {
    /// Neither list: "all other eligible instances are reachable through the custom
    /// endpoint".
    None,
    /// `StaticMembers`: "list of DB instance identifiers that are part of the custom
    /// endpoint group".
    Static(&'static str),
    /// `ExcludedMembers`: the instances that are not part of it.
    Excluded(&'static str),
}

impl Membership {
    fn params(self) -> Vec<(&'static str, String)> {
        match self {
            Self::None => Vec::new(),
            Self::Static(named) => vec![("StaticMembers.member.1", named.to_string())],
            Self::Excluded(named) => vec![("ExcludedMembers.member.1", named.to_string())],
        }
    }
}

/// One row of the provisioning table: an endpoint created on one of [`FIXTURES`].
struct Row {
    case: &'static str,
    identifier: &'static str,
    /// Which fixture the endpoint is created on.
    fixture: usize,
    endpoint_type: &'static str,
    membership: Membership,
}

const ROWS: &[Row] = &[
    Row {
        case: "a READER endpoint pinned to a static member",
        identifier: "emu-ceps-reader-static",
        fixture: 0,
        endpoint_type: "READER",
        membership: Membership::Static("emu-ceps-pg-reader"),
    },
    Row {
        case: "a WRITER endpoint that excludes a member",
        identifier: "emu-ceps-writer-excluded",
        fixture: 0,
        endpoint_type: "WRITER",
        membership: Membership::Excluded("emu-ceps-pg-reader"),
    },
    Row {
        case: "an ANY endpoint over every eligible member",
        identifier: "emu-ceps-any",
        fixture: 0,
        endpoint_type: "ANY",
        membership: Membership::None,
    },
    Row {
        case: "a READER endpoint on an Aurora MySQL cluster with no member instance",
        identifier: "emu-ceps-mysql-reader",
        fixture: 1,
        endpoint_type: "READER",
        membership: Membership::None,
    },
    Row {
        case: "an ANY endpoint named in mixed case",
        identifier: "EMU-CEPS-MySQL-Any",
        fixture: 1,
        endpoint_type: "ANY",
        membership: Membership::None,
    },
];

#[tokio::test]
async fn a_custom_endpoint_of_a_settled_cluster_is_available_to_the_first_read_after_its_create() {
    let router = emulator();
    for fixture in FIXTURES {
        a_settled_cluster(&router, fixture).await;
        past_the_clusters_span(&router, fixture).await;
    }

    for row in ROWS {
        let fixture = &FIXTURES[row.fixture];
        let described = available_to_the_first_read(
            &router,
            row.case,
            row.identifier,
            fixture.cluster,
            row.endpoint_type,
            &row.membership.params(),
        )
        .await;
        // The endpoint that read reported is the one the row asked for, so the status above
        // is this endpoint's and not another's.
        assert_eq!(
            optional_text(&described, "DBClusterEndpointIdentifier").as_deref(),
            Some(row.identifier.to_ascii_lowercase().as_str()),
            "case {:?}: the report must carry the endpoint the create named, got {described}",
            row.case
        );
        assert_eq!(
            optional_text(&described, "CustomEndpointType").as_deref(),
            Some(row.endpoint_type.to_ascii_uppercase().as_str()),
            "case {:?}: expected the requested type, got {described}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_duplicate_identifier_walk_converges_one_read_per_stage() {
    let router = emulator();
    let fixture = &Fixture {
        cluster: "emu-cedup-cluster",
        engine: "aurora-postgresql",
        members: &[],
    };
    a_settled_cluster(&router, fixture).await;
    past_the_clusters_span(&router, fixture).await;

    // 1. The endpoint is created and is available to the first read after the create.
    let named = "EMU-CEdup-Endpoint";
    let identifier = named.to_ascii_lowercase();
    let described = available_to_the_first_read(
        &router,
        "the first create",
        named,
        fixture.cluster,
        "ANY",
        &[],
    )
    .await;
    let address = text(&described, "Endpoint");

    // 2. A second create under the identifier in use is refused the modelled fault, naming
    //    the identifier the REQUEST carried and the address the endpoint already holding it
    //    publishes.
    let refused = expect_error(
        "the duplicate create",
        create_endpoint(&router, named, fixture.cluster, "ANY", &[]).await,
        400,
        "DBClusterEndpointAlreadyExistsFault",
    );
    assert_eq!(
        text(&refused, "Message"),
        format!("An endpoint with the identifier {named} already exists: {address}"),
        "the refusal names the identifier that collided and the endpoint holding it, got \
         {refused}"
    );

    // 3. And it left exactly ONE endpoint under that identifier.
    let reported = describe_once(&router, "after the duplicate create", &identifier).await;
    assert_eq!(
        reported.len(),
        1,
        "a refused create leaves one endpoint under the identifier, got {reported:?}"
    );

    // 4. The delete releases the name: it is answered `deleting` and the endpoint then
    //    leaves the report.
    let deleted = expect_ok(
        "the delete",
        call(
            &router,
            "DeleteDBClusterEndpoint",
            &[("DBClusterEndpointIdentifier", identifier.as_str())],
        )
        .await,
    );
    assert_eq!(
        text(&deleted, "Status"),
        "deleting",
        "an accepted delete answers the status it stored, got {deleted}"
    );
    let params = [("DBClusterEndpointIdentifier", identifier.clone())];
    poll_until(
        "the delete",
        "the endpoint leaving the report",
        || call(&router, "DescribeDBClusterEndpoints", &params),
        |status, body| status == 200 && subtrees(body, "DBClusterEndpointList").is_empty(),
    )
    .await;

    // 5. The identical create then succeeds, and is available to the first read again — so
    //    is a second, new identifier.
    available_to_the_first_read(
        &router,
        "the create after the delete",
        named,
        fixture.cluster,
        "ANY",
        &[],
    )
    .await;
    available_to_the_first_read(
        &router,
        "a second identifier",
        "emu-cedup-second",
        fixture.cluster,
        "READER",
        &[],
    )
    .await;
}
