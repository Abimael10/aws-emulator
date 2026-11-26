// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for `ModifyCurrentDBClusterCapacity`: the capacity an Aurora Serverless
//! v1 DB cluster runs on.
//!
//! The rule under test, asserted over TABLES rather than over one request:
//!
//! * the operation applies to a DB cluster in the `serverless` engine mode — Aurora
//!   Serverless v1 — and to one only, so a cluster of any other kind is refused on its TYPE
//!   with the Query API's `InvalidParameterValue`, whatever that cluster's engine, scaling
//!   configuration or status;
//! * `Capacity` is held to the values the cluster's ENGINE publishes, which differ between
//!   Aurora MySQL and Aurora PostgreSQL, and a value outside them is the modelled
//!   `InvalidDBClusterCapacityFault`;
//! * an accepted call reports the capacity the cluster is ON beside the capacity it "scales
//!   to next", and the cluster really reaches that capacity: `DescribeDBClusters` reports it
//!   as `DBCluster$Capacity` once the scaling the clock owes has settled;
//! * `SecondsBeforeTimeout` and `TimeoutAction` describe the CALL, so each is held to its
//!   own published domain and reported back as the request named it or as the documented
//!   default.
//!
//! The capacity values, the bounds and the two timeout actions are written out HERE, from
//! the members' own documentation rather than from the code under test, and each engine is
//! driven on a capacity the recorded case did not name. Every call goes over the wire
//! through the harness in `db_instances.rs`, so each assertion reads the XML the emulator
//! itself answered with.

use smithy_server_runtime::MultiServiceRouter;

use crate::db_instances::{
    call, emulator, expect_error, expect_ok, optional_text, text, wait_for_settle,
};

/// The Query API's own code for a member value this service will not serve.
const INVALID_PARAMETER_VALUE: &str = "InvalidParameterValue";

/// The engine mode an Aurora Serverless v1 cluster runs in
/// (`CreateDBClusterMessage$EngineMode`: "`serverless` … only applies for Aurora Serverless
/// v1 DB clusters").
const SERVERLESS_V1_MODE: &str = "serverless";

/// The sentence the operation refuses a cluster it does not apply to with: the fixed text
/// AWS answers such a call with, spelled out here rather than read from the code under test.
/// It names no identifier, capacity or engine, so every refused cluster is answered the same
/// sentence.
const CAPACITY_NOT_MODIFIABLE: &str =
    "Only Aurora Serverless clusters can have their capacity modified.";

/// The `TimeoutAction` values the operation publishes, the first of which is its documented
/// default ("`ForceApplyCapacityChange`, the default").
const TIMEOUT_ACTIONS: [&str; 2] = ["ForceApplyCapacityChange", "RollbackCapacityChange"];

/// `SecondsBeforeTimeout` "Default: The default is 300."
const DEFAULT_SECONDS_BEFORE_TIMEOUT: &str = "300";

/// One Aurora engine, with the Serverless v1 capacity values it publishes.
struct EngineRow {
    /// `CreateDBCluster$Engine`.
    engine: &'static str,
    /// The identifier prefix this row's clusters are created under.
    prefix: &'static str,
    /// The capacity values the engine accepts, from
    /// `ModifyCurrentDBClusterCapacityMessage$Capacity`: "For Aurora MySQL, valid capacity
    /// values are 1, 2, 4, 8, 16, 32, 64, 128, and 256. For Aurora PostgreSQL, valid
    /// capacity values are 2, 4, 8, 16, 32, 64, 192, and 384."
    capacities: &'static [i32],
    /// Values that engine does NOT accept: one between two published values, one past the
    /// largest it publishes, and — for Aurora PostgreSQL — a capacity the OTHER Aurora
    /// engine publishes, which is what makes the list per engine rather than per service.
    rejected: &'static [i32],
}

/// The two Aurora engines, each with the capacity values its own documentation publishes.
const ENGINES: &[EngineRow] = &[
    EngineRow {
        engine: "aurora-mysql",
        prefix: "emu-sv1-my",
        capacities: &[1, 2, 4, 8, 16, 32, 64, 128, 256],
        rejected: &[3, 192, 512],
    },
    EngineRow {
        engine: "aurora-postgresql",
        prefix: "emu-sv1-pg",
        capacities: &[2, 4, 8, 16, 32, 64, 192, 384],
        rejected: &[1, 128, 512],
    },
];

/// The sentence a capacity outside `accepted` is refused with, as
/// `InvalidDBClusterCapacityFault` is documented ("`Capacity` isn't a valid Aurora
/// Serverless DB cluster capacity. Valid capacity values are 2, 4, 8, …, and 256.").
fn invalid_capacity_sentence(accepted: &[i32]) -> String {
    let (last, leading) = accepted.split_last().expect("an engine publishes capacities");
    let listed = leading
        .iter()
        .map(i32::to_string)
        .collect::<Vec<String>>()
        .join(", ");
    format!(
        "Capacity isn't a valid Aurora Serverless DB cluster capacity. Valid capacity values \
         are {listed}, and {last}."
    )
}

/// `CreateDBCluster` for an Aurora cluster, with `extra` beyond its master user.
async fn create_cluster(
    router: &MultiServiceRouter,
    identifier: &str,
    engine: &str,
    extra: &[(&str, String)],
) -> (u16, String) {
    let mut request: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", identifier.to_string()),
        ("Engine", engine.to_string()),
        ("MasterUsername", "emuadmin".to_string()),
        ("MasterUserPassword", "a-strong-password".to_string()),
    ];
    request.extend(extra.iter().cloned());
    call(router, "CreateDBCluster", &request).await
}

/// An Aurora Serverless v1 cluster, available and in the `serverless` engine mode.
///
/// No available engine version of this service's catalogue publishes that mode, so the mode
/// is reached by MODIFYING a cluster into it — the way the Data API suite reaches one — and
/// the modification is one the cluster's instances restart for, so the cluster is waited out
/// twice.
async fn create_serverless_v1_cluster(
    router: &MultiServiceRouter,
    identifier: &str,
    engine: &str,
    min_capacity: Option<i32>,
) {
    expect_ok(
        identifier,
        create_cluster(router, identifier, engine, &[]).await,
    );
    wait_for_settle();
    let mut request: Vec<(&str, String)> = vec![
        ("DBClusterIdentifier", identifier.to_string()),
        ("EngineMode", SERVERLESS_V1_MODE.to_string()),
    ];
    if let Some(floor) = min_capacity {
        request.push(("ScalingConfiguration.MinCapacity", floor.to_string()));
    }
    expect_ok(identifier, call(router, "ModifyDBCluster", &request).await);
    wait_for_settle();
}

/// `DBCluster$Capacity` as `DescribeDBClusters` reports it for `identifier`, or `None` for a
/// cluster that reports none at all.
async fn reported_capacity(
    router: &MultiServiceRouter,
    case: &str,
    identifier: &str,
) -> Option<String> {
    let body = expect_ok(
        case,
        call(
            router,
            "DescribeDBClusters",
            &[("DBClusterIdentifier", identifier)],
        )
        .await,
    );
    optional_text(&body, "Capacity")
}

/// `ModifyCurrentDBClusterCapacity` for `identifier`, with `extra` beyond it.
async fn modify_capacity(
    router: &MultiServiceRouter,
    identifier: &str,
    extra: &[(&str, String)],
) -> (u16, String) {
    let mut request: Vec<(&str, String)> = vec![("DBClusterIdentifier", identifier.to_string())];
    request.extend(extra.iter().cloned());
    call(router, "ModifyCurrentDBClusterCapacity", &request).await
}

#[tokio::test]
async fn a_serverless_v1_cluster_reaches_each_capacity_its_engine_publishes() {
    let router = emulator();

    for row in ENGINES {
        // Two clusters per engine: one whose scaling configuration names no floor, which
        // runs on the smallest capacity its engine publishes, and one configured with a
        // floor of its own, which runs on that.
        let published_floor = *row.capacities.first().expect("a published capacity");
        let configured_floor = row.capacities[2];
        for (why, min_capacity, floor) in [
            ("no configured floor", None, published_floor),
            (
                "a configured floor",
                Some(configured_floor),
                configured_floor,
            ),
        ] {
            let identifier = format!(
                "{}-{}",
                row.prefix,
                if min_capacity.is_some() { "cfg" } else { "pub" }
            );
            create_serverless_v1_cluster(&router, &identifier, row.engine, min_capacity).await;

            let case = format!("{} with {why}", row.engine);
            let reported = reported_capacity(&router, &case, &identifier).await;
            assert_eq!(
                reported.as_deref(),
                Some(floor.to_string().as_str()),
                "case {case:?}: expected the cluster to run on {floor} ACUs, got {reported:?}"
            );

            // Every capacity the engine publishes, in turn: the response reports the one the
            // cluster is ON and the one it scales to next, and the cluster has reached the
            // latter once the scaling the clock owes has settled.
            let mut current = floor;
            for capacity in row.capacities {
                let case = format!("{} scaled to {capacity} ACUs", row.engine);
                let body = expect_ok(
                    &case,
                    modify_capacity(
                        &router,
                        &identifier,
                        &[("Capacity", capacity.to_string())],
                    )
                    .await,
                );
                for (member, expected) in [
                    ("DBClusterIdentifier", identifier.clone()),
                    ("CurrentCapacity", current.to_string()),
                    ("PendingCapacity", capacity.to_string()),
                    (
                        "SecondsBeforeTimeout",
                        DEFAULT_SECONDS_BEFORE_TIMEOUT.to_string(),
                    ),
                    ("TimeoutAction", TIMEOUT_ACTIONS[0].to_string()),
                ] {
                    let answered = text(&body, member);
                    assert_eq!(
                        answered, expected,
                        "case {case:?}: expected {member} {expected:?}, got {answered:?} in \
                         {body}"
                    );
                }

                wait_for_settle();
                let reported = reported_capacity(&router, &case, &identifier).await;
                assert_eq!(
                    reported.as_deref(),
                    Some(capacity.to_string().as_str()),
                    "case {case:?}: expected the cluster to report {capacity} ACUs once the \
                     scaling settled, got {reported:?}"
                );
                current = *capacity;
            }
        }
    }
}

#[tokio::test]
async fn a_capacity_outside_the_engines_published_values_is_refused() {
    let router = emulator();

    for row in ENGINES {
        let identifier = format!("{}-bad", row.prefix);
        create_serverless_v1_cluster(&router, &identifier, row.engine, None).await;
        let floor = row.capacities.first().expect("a published capacity").to_string();

        for capacity in row.rejected {
            let case = format!("{} asked for {capacity} ACUs", row.engine);
            let body = expect_error(
                &case,
                modify_capacity(
                    &router,
                    &identifier,
                    &[("Capacity", capacity.to_string())],
                )
                .await,
                400,
                "InvalidDBClusterCapacityFault",
            );
            let message = text(&body, "Message");
            let expected = invalid_capacity_sentence(row.capacities);
            assert_eq!(
                message, expected,
                "case {case:?}: expected the refusal {expected:?}, got {message:?} in {body}"
            );

            // And the refused call scaled nothing: the cluster is still on the capacity it
            // was running on.
            wait_for_settle();
            let reported = reported_capacity(&router, &case, &identifier).await;
            assert_eq!(
                reported.as_deref(),
                Some(floor.as_str()),
                "case {case:?}: expected the refused call to leave the cluster on {floor} \
                 ACUs, got {reported:?}"
            );
        }
    }
}

/// A cluster the operation does not apply to, and how it is created.
struct IneligibleRow {
    case: &'static str,
    identifier: &'static str,
    engine: &'static str,
    extra: &'static [(&'static str, &'static str)],
}

/// The settings a Multi-AZ DB cluster requires, as `CreateDBCluster` documents them for that
/// cluster type ("This setting is required to create a Multi-AZ DB cluster").
const MULTI_AZ_SETTINGS: &[(&str, &str)] = &[
    ("AllocatedStorage", "100"),
    ("DBClusterInstanceClass", "db.m6gd.xlarge"),
    ("StorageType", "io1"),
    ("Iops", "1000"),
];

/// The cluster kinds with no Aurora Serverless v1 capacity to set: a provisioned Aurora
/// cluster of either engine, an Aurora SERVERLESS V2 cluster of either engine (which runs
/// the `provisioned` engine mode and scales inside a capacity range instead), and a
/// non-Aurora Multi-AZ DB cluster of either engine. Each row is refused the SAME sentence,
/// which is what makes the refusal text free of the engine, the identifier and the capacity.
const INELIGIBLE: &[IneligibleRow] = &[
    IneligibleRow {
        case: "a provisioned aurora-mysql cluster",
        identifier: "emu-sv1-prov",
        engine: "aurora-mysql",
        extra: &[],
    },
    IneligibleRow {
        case: "a provisioned aurora-postgresql cluster",
        identifier: "emu-sv1-prov-pg",
        engine: "aurora-postgresql",
        extra: &[],
    },
    IneligibleRow {
        case: "an Aurora Serverless v2 aurora-postgresql cluster",
        identifier: "emu-sv1-v2",
        engine: "aurora-postgresql",
        extra: &[
            ("ServerlessV2ScalingConfiguration.MinCapacity", "0.5"),
            ("ServerlessV2ScalingConfiguration.MaxCapacity", "16.0"),
        ],
    },
    IneligibleRow {
        case: "an Aurora Serverless v2 aurora-mysql cluster",
        identifier: "emu-sv1-v2-my",
        engine: "aurora-mysql",
        extra: &[
            ("ServerlessV2ScalingConfiguration.MinCapacity", "1.0"),
            ("ServerlessV2ScalingConfiguration.MaxCapacity", "128.0"),
        ],
    },
    IneligibleRow {
        case: "a non-Aurora Multi-AZ DB cluster",
        identifier: "emu-sv1-multiaz",
        engine: "mysql",
        extra: MULTI_AZ_SETTINGS,
    },
    IneligibleRow {
        case: "a non-Aurora postgres Multi-AZ DB cluster",
        identifier: "emu-sv1-multiaz-pg",
        engine: "postgres",
        extra: MULTI_AZ_SETTINGS,
    },
];

#[tokio::test]
async fn a_cluster_that_is_not_serverless_v1_is_refused_on_its_type() {
    let router = emulator();

    for row in INELIGIBLE {
        let extra: Vec<(&str, String)> = row
            .extra
            .iter()
            .map(|(name, value)| (*name, value.to_string()))
            .collect();
        expect_ok(
            row.case,
            create_cluster(&router, row.identifier, row.engine, &extra).await,
        );
    }
    // The clusters are available when the calls arrive, so the refusal below is their TYPE
    // answering rather than a status the call would have to wait out.
    wait_for_settle();

    for row in INELIGIBLE {
        // With a capacity and without one: the type is what the call is refused for either
        // way, so the refusal does not depend on the request carrying a capacity.
        for (why, extra) in [
            ("no capacity", Vec::new()),
            ("a capacity", vec![("Capacity", "8".to_string())]),
        ] {
            let case = format!("{} with {why}", row.case);
            let body = expect_error(
                &case,
                modify_capacity(&router, row.identifier, &extra).await,
                400,
                INVALID_PARAMETER_VALUE,
            );
            let message = text(&body, "Message");
            assert_eq!(
                message, CAPACITY_NOT_MODIFIABLE,
                "case {case:?}: expected the refusal {CAPACITY_NOT_MODIFIABLE:?}, got \
                 {message:?} in {body}"
            );
        }

        // Such a cluster reports no capacity at all: the member is "the current capacity of
        // an Aurora Serverless v1 DB cluster", which this one is not.
        let reported = reported_capacity(&router, row.case, row.identifier).await;
        assert_eq!(
            reported, None,
            "case {:?}: expected no Capacity on a cluster that is not an Aurora Serverless v1 \
             one, got {reported:?}",
            row.case
        );
    }
}

#[tokio::test]
async fn the_type_refusal_does_not_wait_for_the_cluster_to_come_up() {
    let router = emulator();

    // No amount of waiting turns a cluster of another kind into an Aurora Serverless v1 one,
    // so the TYPE is answered before the cluster's status: a call issued while the cluster is
    // still coming up is refused with the same sentence rather than with a state fault. No
    // settle is waited out here, which is what makes the point.
    for row in INELIGIBLE {
        let extra: Vec<(&str, String)> = row
            .extra
            .iter()
            .map(|(name, value)| (*name, value.to_string()))
            .collect();
        let identifier = format!("{}-new", row.identifier);
        expect_ok(
            row.case,
            create_cluster(&router, &identifier, row.engine, &extra).await,
        );

        let case = format!("{} straight after the create", row.case);
        let body = expect_error(
            &case,
            modify_capacity(&router, &identifier, &[("Capacity", "8".to_string())]).await,
            400,
            INVALID_PARAMETER_VALUE,
        );
        let message = text(&body, "Message");
        assert_eq!(
            message, CAPACITY_NOT_MODIFIABLE,
            "case {case:?}: expected the refusal {CAPACITY_NOT_MODIFIABLE:?}, got {message:?} \
             in {body}"
        );
    }
}

#[tokio::test]
async fn the_calls_own_timeout_members_are_held_to_their_published_domains() {
    let router = emulator();
    let identifier = "emu-sv1-timeout";
    create_serverless_v1_cluster(&router, identifier, "aurora-mysql", None).await;

    // `SecondsBeforeTimeout`: "Specify a value between 10 and 600 seconds" — each published
    // bound is served, and one second past each is refused.
    for seconds in [10, 300, 600] {
        let case = format!("SecondsBeforeTimeout {seconds}");
        let body = expect_ok(
            &case,
            modify_capacity(
                &router,
                identifier,
                &[
                    ("Capacity", "4".to_string()),
                    ("SecondsBeforeTimeout", seconds.to_string()),
                ],
            )
            .await,
        );
        let answered = text(&body, "SecondsBeforeTimeout");
        assert_eq!(
            answered,
            seconds.to_string(),
            "case {case:?}: expected the call to report {seconds} seconds, got {answered:?} \
             in {body}"
        );
        wait_for_settle();
    }
    for seconds in [9, 601] {
        let case = format!("SecondsBeforeTimeout {seconds}");
        expect_error(
            &case,
            modify_capacity(
                &router,
                identifier,
                &[
                    ("Capacity", "4".to_string()),
                    ("SecondsBeforeTimeout", seconds.to_string()),
                ],
            )
            .await,
            400,
            INVALID_PARAMETER_VALUE,
        );
    }

    // `TimeoutAction`: "either ForceApplyCapacityChange or RollbackCapacityChange" — both
    // are served and reported back, and any other value is refused.
    for action in TIMEOUT_ACTIONS {
        let case = format!("TimeoutAction {action}");
        let body = expect_ok(
            &case,
            modify_capacity(
                &router,
                identifier,
                &[
                    ("Capacity", "8".to_string()),
                    ("TimeoutAction", action.to_string()),
                ],
            )
            .await,
        );
        let answered = text(&body, "TimeoutAction");
        assert_eq!(
            answered, action,
            "case {case:?}: expected the call to report {action:?}, got {answered:?} in {body}"
        );
        wait_for_settle();
    }
    expect_error(
        "an unpublished TimeoutAction",
        modify_capacity(
            &router,
            identifier,
            &[("TimeoutAction", "PauseCapacityChange".to_string())],
        )
        .await,
        400,
        INVALID_PARAMETER_VALUE,
    );
}

#[tokio::test]
async fn an_identifier_that_names_no_cluster_is_answered_the_not_found_fault() {
    let router = emulator();

    // Both published forms of the reference — "the user-supplied DB cluster identifier or
    // the Amazon Resource Name (ARN)" — name the same absent cluster, and the fault names
    // the identifier the reference carries.
    for (case, reference) in [
        ("an identifier naming nothing", "emu-sv1-absent"),
        (
            "an ARN naming nothing",
            "arn:aws:rds:us-west-2:000000000000:cluster:emu-sv1-absent",
        ),
    ] {
        let body = expect_error(
            case,
            modify_capacity(&router, reference, &[("Capacity", "8".to_string())]).await,
            404,
            "DBClusterNotFoundFault",
        );
        let message = text(&body, "Message");
        let expected = "DBCluster emu-sv1-absent not found.";
        assert_eq!(
            message, expected,
            "case {case:?}: expected the fault {expected:?}, got {message:?} in {body}"
        );
    }
}
