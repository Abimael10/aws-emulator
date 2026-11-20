// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a DB cluster created FOR Aurora Limitless Database reports about
//! that Limitless database, and for the parameter-group family its engine build publishes.
//!
//! The rules under test:
//!
//! * `DBCluster$LimitlessDatabase` is "the details for Aurora Limitless Database", so a cluster
//!   that operates as one — `ClusterScalabilityType` "when set to `limitless`, the cluster
//!   operates as an Aurora Limitless Database" — reports it on every read, with a `Status`
//!   from the published `LimitlessDatabaseStatus` set
//!   (`smithy_query(query="shape",shape="com.amazonaws.rds#LimitlessDatabaseStatus",
//!   model_path="services/service-com-amazonaws-rds-amazonrdsv19/smithy-model/com-amazonaws-rds-amazonrdsv19.json")`)
//!   that follows what RUNS on it: a DB shard group of that cluster is the compute the
//!   Limitless database runs on, so the status is `active` exactly while one of the cluster's
//!   shard groups is available and `not-in-use` while none is — before the first shard group is
//!   created, while it is still being provisioned, and after it has been deleted. A cluster
//!   that operates in the standard scalability mode runs no Limitless database and reports no
//!   such details at all.
//! * a DB parameter group family is one an engine version of this front door carries, and the
//!   Aurora Limitless Database build carries a family of its OWN inside its major-version line
//!   — the line's family with the published `-limitless` suffix — so that family is one
//!   `CreateDBClusterParameterGroup` stores a group of, one the account holds a default group
//!   of, one `DescribeEngineDefaultClusterParameters` serves, and the one a cluster created on
//!   that build is given.
//!
//! The rows below span what the rules generalise over: the scalability modes a cluster can be
//! created in, the states its shard group can be in (none created, still provisioning,
//! available, deleted), the Aurora engines a cluster runs, and — for the family — the
//! Limitless family beside the plain family of the same major-version line, which is a family
//! this suite does not write down but reads from the catalogue the service publishes.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper this
//! test target has, as a real awsQuery form POST carrying the SigV4 credential scope the router
//! keys on, and every assertion reads the XML the emulator answered with. Nothing sleeps for a
//! transition: each wait is a poll loop on a REPORTING call through `lifecycle_poll_loops`.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_engine_versions::all_versions_of;
use crate::db_instances::{call, emulator, expect_ok, optional_text, subtrees, text};
use crate::lifecycle_poll_loops::poll_until;

/// `LimitlessDatabase$Status` for a Limitless cluster whose shard group is up.
const ACTIVE: &str = "active";

/// `LimitlessDatabase$Status` for a Limitless cluster nothing runs on yet.
const NOT_IN_USE: &str = "not-in-use";

/// The published suffix a Limitless build's own DB parameter group family carries.
const LIMITLESS_FAMILY_SUFFIX: &str = "-limitless";

/// The engine version of `engine` the catalogue publishes as running Aurora Limitless
/// Database, picked the way a client picks one — off `DescribeDBEngineVersions`'
/// `SupportsLimitlessDatabase` — rather than written down here.
async fn limitless_version(
    router: &MultiServiceRouter,
    case: &str,
    engine: &str,
) -> crate::db_engine_versions::Version {
    all_versions_of(router, engine)
        .await
        .into_iter()
        .find(|version| {
            optional_text(&version.scalars, "SupportsLimitlessDatabase").as_deref() == Some("true")
        })
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the catalogue must publish a {engine} version that supports \
                 Aurora Limitless Database"
            )
        })
}

/// `CreateDBCluster` for a cluster of `engine`, in the scalability mode the row asks for.
async fn create_cluster(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    engine: &str,
    scalability: Option<&str>,
    engine_version: Option<&str>,
) -> String {
    let mut params: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", identifier.to_string()),
        ("Engine", engine.to_string()),
        ("MasterUsername", "clusteradmin".to_string()),
        ("MasterUserPassword", "an-example-password".to_string()),
    ];
    if let Some(scalability) = scalability {
        params.push(("ClusterScalabilityType", scalability.to_string()));
        // The storage type a Limitless cluster is created on, as the Aurora Limitless
        // Database create does.
        params.push(("StorageType", "aurora-iopt1".to_string()));
    }
    if let Some(version) = engine_version {
        params.push(("EngineVersion", version.to_string()));
    }
    expect_ok(case, call(router, "CreateDBCluster", &params).await)
}

/// `DescribeDBClusters` naming one cluster, read once.
async fn describe_cluster(router: &MultiServiceRouter, case: &str, identifier: &str) -> String {
    expect_ok(
        case,
        call(
            router,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", identifier)],
        )
        .await,
    )
}

/// `DescribeDBClusters` naming one cluster, polled until it reports `status`.
async fn awaited_cluster(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
    status: &str,
) -> String {
    poll_until(
        case,
        &format!("DB cluster {identifier} reaching {status}"),
        || async {
            call(
                router,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", identifier)],
            )
            .await
        },
        |http_status, body| http_status == 200 && text(body, "Status") == status,
    )
    .await
    .0
}

/// The `LimitlessDatabase` details the cluster in `xml` reports, as
/// `(Status, MinRequiredACU)` — `None` for a cluster that reports no such element.
///
/// Read off the element's own subtree: a `DBCluster` carries a `Status` of its own, so "the
/// one `<Status>`" is only unambiguous inside the element.
fn limitless_database(xml: &str) -> Option<(String, Option<String>)> {
    let entries = subtrees(xml, "LimitlessDatabase");
    assert!(
        entries.len() <= 1,
        "expected at most one LimitlessDatabase element in {xml}, found {}",
        entries.len()
    );
    entries
        .first()
        .map(|entry| (text(entry, "Status"), optional_text(entry, "MinRequiredACU")))
}

/// `CreateDBShardGroup` of one cluster.
async fn create_shard_group(
    router: &MultiServiceRouter,
    case: &str,
    shard_group: &str,
    cluster: &str,
) -> String {
    expect_ok(
        case,
        call(
            router,
            "CreateDBShardGroup",
            &[
                ("DBShardGroupIdentifier", shard_group),
                ("DBClusterIdentifier", cluster),
                ("MaxACU", "16"),
            ],
        )
        .await,
    )
}

/// `DescribeDBShardGroups` naming one shard group, polled until it reports `available`.
async fn awaited_shard_group(router: &MultiServiceRouter, case: &str, shard_group: &str) {
    poll_until(
        case,
        &format!("DB shard group {shard_group}"),
        || async {
            call(
                router,
                "DescribeDBShardGroups",
                &[("DBShardGroupIdentifier", shard_group)],
            )
            .await
        },
        |status, body| status == 200 && text(body, "Status") == "available",
    )
    .await;
}

/// One row of the scalability-mode table.
struct ClusterCase {
    case: &'static str,
    cluster: &'static str,
    engine: &'static str,
    /// What `ClusterScalabilityType` the create names, and `None` for a create that names
    /// none — which is the standard mode, the documented default.
    scalability: Option<&'static str>,
    /// Whether the cluster is created on the Limitless engine build the catalogue publishes.
    limitless_build: bool,
    /// The `LimitlessDatabase$Status` the cluster reports before any shard group exists, and
    /// `None` for a cluster that reports no Limitless details at all.
    reported: Option<&'static str>,
}

const CLUSTERS: &[ClusterCase] = &[
    ClusterCase {
        case: "an Aurora PostgreSQL cluster created for Limitless Database",
        cluster: "limitless-state-postgres",
        engine: "aurora-postgresql",
        scalability: Some("limitless"),
        limitless_build: true,
        reported: Some(NOT_IN_USE),
    },
    ClusterCase {
        case: "a cluster created in the standard scalability mode",
        cluster: "limitless-state-standard",
        engine: "aurora-postgresql",
        scalability: Some("standard"),
        limitless_build: false,
        reported: None,
    },
    ClusterCase {
        case: "a cluster whose create names no scalability mode at all",
        cluster: "limitless-state-default",
        engine: "aurora-postgresql",
        scalability: None,
        limitless_build: false,
        reported: None,
    },
    ClusterCase {
        case: "an Aurora MySQL cluster, which runs no Limitless database",
        cluster: "limitless-state-mysql",
        engine: "aurora-mysql",
        scalability: None,
        limitless_build: false,
        reported: None,
    },
];

#[tokio::test]
async fn a_cluster_reports_the_limitless_details_its_scalability_mode_gives_it() {
    let router = emulator();
    for row in CLUSTERS {
        let case = row.case;
        let version = match row.limitless_build {
            true => Some(limitless_version(&router, case, row.engine).await.engine_version),
            false => None,
        };
        let created = create_cluster(
            &router,
            case,
            row.cluster,
            row.engine,
            row.scalability,
            version.as_deref(),
        )
        .await;
        // The create's own answer carries the details, before the cluster has settled.
        assert_eq!(
            limitless_database(&created).map(|(status, _)| status),
            row.reported.map(str::to_string),
            "case {case:?}: got {created}"
        );
        // ROUND TRIP: the same details on a later read of the settled cluster.
        let settled = awaited_cluster(&router, case, row.cluster, "available").await;
        assert_eq!(
            limitless_database(&settled),
            row.reported.map(|status| (status.to_string(), None)),
            "case {case:?}: got {settled}"
        );
    }
}

#[tokio::test]
async fn the_limitless_status_follows_the_shard_group_that_runs_it() {
    let router = emulator();
    let case = "a Limitless cluster through its first shard group";
    let version = limitless_version(&router, case, "aurora-postgresql")
        .await
        .engine_version;
    create_cluster(
        &router,
        case,
        "limitless-run-cluster",
        "aurora-postgresql",
        Some("limitless"),
        Some(&version),
    )
    .await;
    // A SECOND Limitless cluster, which no shard group is created in: what runs on one
    // cluster's Limitless database says nothing about another's.
    create_cluster(
        &router,
        case,
        "limitless-idle-cluster",
        "aurora-postgresql",
        Some("limitless"),
        Some(&version),
    )
    .await;
    awaited_cluster(&router, case, "limitless-run-cluster", "available").await;
    awaited_cluster(&router, case, "limitless-idle-cluster", "available").await;

    // A shard group that is still being provisioned is not one the Limitless database runs
    // on yet.
    let provisioning = create_shard_group(
        &router,
        case,
        "limitless-run-shards",
        "limitless-run-cluster",
    )
    .await;
    assert_eq!(
        text(&provisioning, "Status"),
        "creating",
        "case {case:?}: an accepted create answers with the shard group being provisioned, \
         got {provisioning}"
    );
    let while_provisioning = describe_cluster(&router, case, "limitless-run-cluster").await;
    assert_eq!(
        limitless_database(&while_provisioning),
        Some((NOT_IN_USE.to_string(), None)),
        "case {case:?}: got {while_provisioning}"
    );

    // Once the shard group is up, the cluster's Limitless database is in use.
    awaited_shard_group(&router, case, "limitless-run-shards").await;
    let running = poll_until(
        case,
        "the Limitless database reaching active",
        || async {
            call(
                &router,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", "limitless-run-cluster")],
            )
            .await
        },
        |status, body| {
            status == 200
                && limitless_database(body) == Some((ACTIVE.to_string(), None))
        },
    )
    .await
    .0;
    assert_eq!(
        text(&running, "ClusterScalabilityType"),
        "limitless",
        "case {case:?}: got {running}"
    );
    let idle = describe_cluster(&router, case, "limitless-idle-cluster").await;
    assert_eq!(
        limitless_database(&idle),
        Some((NOT_IN_USE.to_string(), None)),
        "case {case:?}: another cluster's shard group runs nothing on this one, got {idle}"
    );

    // And a cluster whose shard group has been deleted runs nothing again.
    expect_ok(
        case,
        call(
            &router,
            "DeleteDBShardGroup",
            &[("DBShardGroupIdentifier", "limitless-run-shards")],
        )
        .await,
    );
    let gone = poll_until(
        case,
        "the Limitless database leaving active",
        || async {
            call(
                &router,
                "DescribeDBClusters",
                &[("DBClusterIdentifier", "limitless-run-cluster")],
            )
            .await
        },
        |status, body| {
            status == 200
                && limitless_database(body) == Some((NOT_IN_USE.to_string(), None))
        },
    )
    .await
    .0;
    assert_eq!(
        text(&gone, "DBClusterIdentifier"),
        "limitless-run-cluster",
        "case {case:?}: got {gone}"
    );
}

#[tokio::test]
async fn the_limitless_build_publishes_a_parameter_group_family_of_its_own() {
    let router = emulator();
    let case = "the Limitless Database parameter group family";
    let published = limitless_version(&router, case, "aurora-postgresql").await;
    let family = published.parameter_group_family.clone();
    assert!(
        family.ends_with(LIMITLESS_FAMILY_SUFFIX),
        "case {case:?}: the Limitless build's family must be its line's family with the \
         Limitless suffix, got {family:?} in {}",
        published.scalars
    );
    // The plain family of the same major-version line is published too, by the line's other
    // builds, and is a DIFFERENT family: the Limitless one is not a rename of it.
    let line = family
        .strip_suffix(LIMITLESS_FAMILY_SUFFIX)
        .expect("checked just above")
        .to_string();
    assert!(
        all_versions_of(&router, "aurora-postgresql")
            .await
            .iter()
            .any(|version| version.parameter_group_family == line),
        "case {case:?}: the major-version line's own family {line:?} must still be published"
    );

    // A cluster created on that build is given the DEFAULT group of that family.
    let created = create_cluster(
        &router,
        case,
        "limitless-family-cluster",
        "aurora-postgresql",
        Some("limitless"),
        Some(&published.engine_version),
    )
    .await;
    assert_eq!(
        text(&created, "DBClusterParameterGroup"),
        format!("default.{family}"),
        "case {case:?}: got {created}"
    );

    // The family is one a caller creates its own groups in, and the group round-trips through
    // `DescribeDBClusterParameterGroups`.
    let group = expect_ok(
        case,
        call(
            &router,
            "CreateDBClusterParameterGroup",
            &[
                ("DBClusterParameterGroupName", "limitless-own-group"),
                ("DBParameterGroupFamily", family.as_str()),
                ("Description", "Aurora Limitless Database parameters"),
            ],
        )
        .await,
    );
    let read_back = expect_ok(
        case,
        call(
            &router,
            "DescribeDBClusterParameterGroups",
            &[("DBClusterParameterGroupName", "limitless-own-group")],
        )
        .await,
    );
    assert_eq!(
        (
            text(&group, "DBParameterGroupFamily"),
            text(&read_back, "DBParameterGroupFamily"),
        ),
        (family.clone(), family.clone()),
        "case {case:?}: got {group} and {read_back}"
    );

    // The account holds the DEFAULT group of that family, as it does of every published
    // family, and the engine defaults of the family are served rather than refused.
    let defaults = expect_ok(
        case,
        call(
            &router,
            "DescribeDBClusterParameterGroups",
            &[("DBClusterParameterGroupName", format!("default.{family}"))],
        )
        .await,
    );
    assert_eq!(
        text(&defaults, "DBParameterGroupFamily"),
        family,
        "case {case:?}: got {defaults}"
    );
    let engine_defaults = expect_ok(
        case,
        call(
            &router,
            "DescribeEngineDefaultClusterParameters",
            &[("DBParameterGroupFamily", family.as_str())],
        )
        .await,
    );
    assert!(
        !subtrees(&engine_defaults, "Parameter").is_empty(),
        "case {case:?}: the family's engine defaults must be served, got {engine_defaults}"
    );
}
