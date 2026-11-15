// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the DB CLUSTER STATE `CreateDBShardGroup` provisions an Aurora
//! Limitless Database DB shard group in.
//!
//! Every call goes over the wire through `db_instances::emulator`, the one mounting helper
//! this test target has, as a real awsQuery form POST carrying the SigV4 credential scope
//! the router keys on. The assertions read the XML the emulator answers with, so a handler
//! that computed nothing could not pass them.
//!
//! The rule under test: a DB shard group is provisioned INTO its primary DB cluster, so the
//! create is served only while that cluster is in a state that can take one — it is up, or
//! it is still being created and the shard group comes up with it — and a cluster in any
//! other state refuses it with the modelled `InvalidDBClusterStateFault` (HTTP 400,
//! `awsQueryError` code `InvalidDBClusterStateFault`) worded "You can't create the DB shard
//! group. The DB cluster <identifier> isn't in the AVAILABLE or CREATING state.", storing
//! nothing — so a following `DescribeDBShardGroups` of the identifier the refused create
//! named is the modelled `DBShardGroupNotFound`, and the identical call is served once the
//! cluster is back up.
//!
//! The table below spans what the rule generalises over: the state the primary cluster is in
//! when the create arrives (still provisioning, up, its stop running, and stopped), the kind
//! of Aurora cluster it is (an Aurora MySQL cluster, an Aurora PostgreSQL one, and a cluster
//! created for Limitless Database with the engine version the catalogue publishes for it),
//! each form `DBClusterIdentifier` may name it by, and the capacities the create asks for. A
//! `DBClusterIdentifier` that resolves to no cluster at all, and one whose delete has begun,
//! are the sibling suite's rows (`db_shard_group_primary_cluster`), so neither is restated
//! here.
//!
//! Nothing here waits out a span: a state a transition this service owes leads to is reached
//! by polling a REPORTING call through `lifecycle_poll_loops::poll_until` until the cluster
//! reports it.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_engine_versions::all_versions_of;
use crate::db_instances::{call, emulator, expect_error, expect_ok, optional_text, text, REGION};
use crate::lifecycle_poll_loops::poll_until;

/// The canonical account id every generated emulator presents, for the ARN form a row names
/// its primary cluster by.
const ACCOUNT: &str = "000000000000";

/// The `awsQueryError` code of `InvalidDBClusterStateFault`, as the model publishes it.
const INVALID_DB_CLUSTER_STATE: &str = "InvalidDBClusterStateFault";

/// The `awsQueryError` code `DescribeDBShardGroups` answers for an identifier nothing
/// created.
const DB_SHARD_GROUP_NOT_FOUND: &str = "DBShardGroupNotFound";

/// The status an accepted DB shard group create reports.
const CREATING: &str = "creating";

/// The status it reports once the transition this service owes it has come due.
const AVAILABLE: &str = "available";

/// The sentence `CreateDBShardGroup` refuses a primary cluster that cannot take a shard
/// group with: fixed text, with the DB cluster identifier the only variable part.
fn expected_refusal(db_cluster_identifier: &str) -> String {
    format!(
        "You can't create the DB shard group. The DB cluster {db_cluster_identifier} isn't \
         in the AVAILABLE or CREATING state."
    )
}

/// What the primary cluster is doing when the create arrives, and how a row gets it there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClusterState {
    /// Straight after `CreateDBCluster`, before the clock has settled the cluster.
    Creating,
    /// Polled until the cluster reports `available`.
    Available,
    /// Straight after `StopDBCluster`, which answers with the stop running. The stop leads
    /// to `stopped`, which refuses the create too, so this row's expectation holds whether
    /// or not the clock has settled it by the time the create arrives.
    Stopping,
    /// Polled until the cluster reports `stopped`.
    Stopped,
}

/// How a row names its primary cluster: `DBClusterIdentifier` is "the user-supplied DB
/// cluster identifier or the Amazon Resource Name (ARN) of the DB cluster", and "this
/// parameter isn't case-sensitive".
#[derive(Debug, Clone, Copy)]
enum NamedBy {
    /// The identifier exactly as the cluster was created with.
    AsCreated,
    /// The identifier in another case.
    OtherCase,
    /// The cluster's ARN.
    Arn,
}

/// What the create is answered with. A `DBClusterIdentifier` that resolves to NO cluster is
/// the sibling suite's row (`db_shard_group_primary_cluster`, which tables the resolution
/// forms and the not-found and deleting refusals), so it is not restated here.
#[derive(Debug, Clone, Copy)]
enum Expected {
    /// HTTP 200, reporting the shard group being provisioned.
    Served,
    /// The modelled `InvalidDBClusterStateFault`.
    RefusedForState,
}

/// One row of the cluster-state table.
struct Case {
    case: &'static str,
    cluster: &'static str,
    shard_group: &'static str,
    /// The engine the primary cluster runs; `CreateDBShardGroup` is "valid for: Aurora DB
    /// clusters only".
    engine: &'static str,
    /// Whether the cluster is created FOR Aurora Limitless Database, which is the kind of
    /// cluster a shard group belongs to in production.
    limitless: bool,
    state: ClusterState,
    named_by: NamedBy,
    /// `MaxACU`: "the maximum capacity of the DB shard group in Aurora capacity units".
    max_acu: f64,
    /// `MinACU`, for the rows that name one.
    min_acu: Option<f64>,
    expected: Expected,
}

/// `CreateDBCluster` for one row's primary cluster, with the members a Limitless cluster
/// carries when the row asks for one.
async fn create_cluster(router: &MultiServiceRouter, row: &Case) -> (u16, String) {
    let mut settings: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", row.cluster.to_string()),
        ("Engine", row.engine.to_string()),
        ("MasterUsername", "clusteradmin".to_string()),
        ("MasterUserPassword", "a-strong-password-1".to_string()),
    ];
    if row.limitless {
        // The engine version comes out of the catalogue this service publishes — the one
        // that reports `SupportsLimitlessDatabase` — exactly as a client picks one, rather
        // than being written here.
        let version = all_versions_of(router, row.engine)
            .await
            .into_iter()
            .find(|version| {
                optional_text(&version.scalars, "SupportsLimitlessDatabase").as_deref()
                    == Some("true")
            })
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: the catalogue must publish a {} version that supports Aurora \
                     Limitless Database",
                    row.case, row.engine
                )
            });
        settings.push(("EngineVersion", version.engine_version));
        settings.push(("ClusterScalabilityType", "limitless".to_string()));
        settings.push(("StorageType", "aurora-iopt1".to_string()));
    }
    call(router, "CreateDBCluster", &settings).await
}

/// `DescribeDBClusters` for one cluster — the REPORTING call every poll below makes.
async fn describe_cluster(router: &MultiServiceRouter, identifier: &str) -> (u16, String) {
    call(
        router,
        "DescribeDBClusters",
        &[("DBClusterIdentifier", identifier)],
    )
    .await
}

/// Poll `DescribeDBClusters` until the cluster reports `status`.
async fn wait_for_cluster_status(
    router: &MultiServiceRouter,
    case: &str,
    cluster: &str,
    status: &str,
) {
    poll_until(
        case,
        &format!("{cluster} reaching {status}"),
        || describe_cluster(router, cluster),
        |http_status, body| {
            http_status == 200 && optional_text(body, "Status").as_deref() == Some(status)
        },
    )
    .await;
}

/// `CreateDBShardGroup` for one row, naming the primary cluster as `named_cluster` spells
/// it.
async fn create_shard_group(
    router: &MultiServiceRouter,
    row: &Case,
    named_cluster: &str,
) -> (u16, String) {
    let mut params: Vec<(&str, String)> = vec![
        ("DBShardGroupIdentifier", row.shard_group.to_string()),
        ("DBClusterIdentifier", named_cluster.to_string()),
        ("MaxACU", row.max_acu.to_string()),
    ];
    if let Some(min_acu) = row.min_acu {
        params.push(("MinACU", min_acu.to_string()));
    }
    call(router, "CreateDBShardGroup", &params).await
}

/// `DescribeDBShardGroups` for one identifier.
async fn describe_shard_group(router: &MultiServiceRouter, shard_group: &str) -> (u16, String) {
    call(
        router,
        "DescribeDBShardGroups",
        &[("DBShardGroupIdentifier", shard_group)],
    )
    .await
}

/// Take one row's primary cluster to the state the row names, making only the calls a
/// client would and polling for every state the clock leads to.
async fn arrange_cluster(router: &MultiServiceRouter, row: &Case) {
    expect_ok(row.case, create_cluster(router, row).await);
    match row.state {
        ClusterState::Creating => {}
        ClusterState::Available => {
            wait_for_cluster_status(router, row.case, row.cluster, "available").await;
        }
        ClusterState::Stopping => {
            wait_for_cluster_status(router, row.case, row.cluster, "available").await;
            let stopping = expect_ok(
                row.case,
                call(
                    router,
                    "StopDBCluster",
                    &[("DBClusterIdentifier", row.cluster)],
                )
                .await,
            );
            assert_eq!(
                text(&stopping, "Status"),
                "stopping",
                "case {:?}: an accepted stop must answer with the stop running, got {stopping}",
                row.case
            );
        }
        ClusterState::Stopped => {
            wait_for_cluster_status(router, row.case, row.cluster, "available").await;
            expect_ok(
                row.case,
                call(
                    router,
                    "StopDBCluster",
                    &[("DBClusterIdentifier", row.cluster)],
                )
                .await,
            );
            wait_for_cluster_status(router, row.case, row.cluster, "stopped").await;
        }
    }
}

#[tokio::test]
async fn a_shard_group_is_created_only_on_a_cluster_whose_state_can_take_one() {
    let table: &[Case] = &[
        Case {
            case: "a cluster still being created takes the shard group that comes up with it",
            cluster: "emu-sgcs-creating",
            shard_group: "emu-sgcs-on-creating",
            engine: "aurora-postgresql",
            limitless: true,
            state: ClusterState::Creating,
            named_by: NamedBy::AsCreated,
            max_acu: 16.0,
            min_acu: None,
            expected: Expected::Served,
        },
        Case {
            case: "an available Limitless cluster takes a shard group",
            cluster: "emu-sgcs-available",
            shard_group: "emu-sgcs-on-available",
            engine: "aurora-postgresql",
            limitless: true,
            state: ClusterState::Available,
            named_by: NamedBy::Arn,
            max_acu: 1000.0,
            min_acu: Some(24.0),
            expected: Expected::Served,
        },
        Case {
            case: "a cluster whose stop is running refuses it",
            cluster: "emu-sgcs-stopping",
            shard_group: "emu-sgcs-on-stopping",
            engine: "aurora-postgresql",
            limitless: true,
            state: ClusterState::Stopping,
            named_by: NamedBy::AsCreated,
            max_acu: 16.0,
            min_acu: None,
            expected: Expected::RefusedForState,
        },
        Case {
            case: "a stopped Limitless cluster refuses it",
            cluster: "emu-sgcs-stopped",
            shard_group: "emu-sgcs-on-stopped",
            engine: "aurora-postgresql",
            limitless: true,
            state: ClusterState::Stopped,
            named_by: NamedBy::AsCreated,
            max_acu: 12.5,
            min_acu: Some(0.5),
            expected: Expected::RefusedForState,
        },
        Case {
            case: "a stopped Aurora MySQL cluster named in another case refuses it",
            cluster: "emu-sgcs-stopped-mysql",
            shard_group: "emu-sgcs-on-stopped-mysql",
            engine: "aurora-mysql",
            limitless: false,
            state: ClusterState::Stopped,
            named_by: NamedBy::OtherCase,
            max_acu: 100.0,
            min_acu: None,
            expected: Expected::RefusedForState,
        },
        Case {
            case: "a stopped Aurora PostgreSQL cluster named by its ARN refuses it",
            cluster: "emu-sgcs-stopped-by-arn",
            shard_group: "emu-sgcs-on-stopped-by-arn",
            engine: "aurora-postgresql",
            limitless: false,
            state: ClusterState::Stopped,
            named_by: NamedBy::Arn,
            max_acu: 64.0,
            min_acu: Some(8.0),
            expected: Expected::RefusedForState,
        },
    ];

    for row in table {
        // Each row gets its own emulator, so one row's records and transitions cannot carry
        // into the next.
        let router = emulator();
        let case = row.case;
        arrange_cluster(&router, row).await;

        let named = match row.named_by {
            NamedBy::AsCreated => row.cluster.to_string(),
            NamedBy::OtherCase => row.cluster.to_ascii_uppercase(),
            NamedBy::Arn => format!("arn:aws:rds:{REGION}:{ACCOUNT}:cluster:{}", row.cluster),
        };
        let answer = create_shard_group(&router, row, &named).await;

        match row.expected {
            Expected::Served => {
                let created = expect_ok(case, answer);
                assert_eq!(
                    text(&created, "Status"),
                    CREATING,
                    "case {case:?}: an accepted create must report {CREATING:?}, got {created}"
                );
                assert_eq!(
                    text(&created, "DBClusterIdentifier"),
                    row.cluster,
                    "case {case:?}: the shard group must name the cluster the create \
                     resolved, got {created}"
                );
                // And it was PERSISTED: the shard group is read back through the report,
                // polled until the transition this service owes it has come due.
                let (described, _) = poll_until(
                    case,
                    &format!("{} reaching available", row.shard_group),
                    || describe_shard_group(&router, row.shard_group),
                    |http_status, body| {
                        http_status == 200
                            && optional_text(body, "Status").as_deref() == Some(AVAILABLE)
                    },
                )
                .await;
                assert_eq!(
                    text(&described, "DBClusterIdentifier"),
                    row.cluster,
                    "case {case:?}: the stored shard group must keep its primary cluster, \
                     got {described}"
                );
            }
            Expected::RefusedForState => {
                let refused = expect_error(case, answer, 400, INVALID_DB_CLUSTER_STATE);
                assert_eq!(
                    text(&refused, "Message"),
                    expected_refusal(row.cluster),
                    "case {case:?}: expected the sentence this refusal carries, got {refused}"
                );
                // A refused create stored NOTHING: the identifier it named is still one
                // nothing created.
                expect_error(
                    case,
                    describe_shard_group(&router, row.shard_group).await,
                    404,
                    DB_SHARD_GROUP_NOT_FOUND,
                );
            }
        }
    }
}

#[tokio::test]
async fn the_create_a_stopped_cluster_refused_is_served_once_it_is_started_again() {
    // The whole sequence a client drives: the cluster goes down, the create it refuses
    // leaves nothing behind, and the IDENTICAL call is served once the cluster is back up.
    let row = Case {
        case: "a stopped Limitless cluster, started again",
        cluster: "emu-sgcs-restarted",
        shard_group: "emu-sgcs-on-restarted",
        engine: "aurora-postgresql",
        limitless: true,
        state: ClusterState::Stopped,
        named_by: NamedBy::AsCreated,
        max_acu: 16.0,
        min_acu: Some(4.0),
        expected: Expected::RefusedForState,
    };
    let case = row.case;
    let router = emulator();
    arrange_cluster(&router, &row).await;

    let refused = expect_error(
        case,
        create_shard_group(&router, &row, row.cluster).await,
        400,
        INVALID_DB_CLUSTER_STATE,
    );
    assert_eq!(
        text(&refused, "Message"),
        expected_refusal(row.cluster),
        "case {case:?}: expected the sentence this refusal carries, got {refused}"
    );
    expect_error(
        case,
        describe_shard_group(&router, row.shard_group).await,
        404,
        DB_SHARD_GROUP_NOT_FOUND,
    );

    // The cluster is started again, and the create that was refused is now served.
    expect_ok(
        case,
        call(
            &router,
            "StartDBCluster",
            &[("DBClusterIdentifier", row.cluster)],
        )
        .await,
    );
    wait_for_cluster_status(&router, case, row.cluster, "available").await;
    let created = expect_ok(case, create_shard_group(&router, &row, row.cluster).await);
    assert_eq!(
        text(&created, "Status"),
        CREATING,
        "case {case:?}: the create of a started cluster must be accepted, got {created}"
    );
    let (described, _) = poll_until(
        case,
        &format!("{} reaching available", row.shard_group),
        || describe_shard_group(&router, row.shard_group),
        |http_status, body| {
            http_status == 200 && optional_text(body, "Status").as_deref() == Some(AVAILABLE)
        },
    )
    .await;
    assert_eq!(
        text(&described, "DBClusterIdentifier"),
        row.cluster,
        "case {case:?}: the shard group the started cluster took must be stored against it, \
         got {described}"
    );
}
