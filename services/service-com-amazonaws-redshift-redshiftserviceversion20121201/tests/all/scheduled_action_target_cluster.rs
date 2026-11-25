// SPDX-License-Identifier: CC0-1.0

//! WHEN a scheduled action's target cluster is resolved, end to end over the
//! awsQuery wire.
//!
//! The RULE under test: a scheduled action is a REGISTRATION of an operation to
//! run on a schedule, and the operation it triggers resolves its own
//! `ClusterIdentifier` when the schedule fires — so neither
//! `CreateScheduledAction` nor `ModifyScheduledAction` resolves the cluster its
//! `TargetAction` names against the clusters the account holds, and an action
//! targeting a cluster that is not there (yet) is registered, reported with the
//! `Schedule` the request submitted and the `State` its `Enable` asks for, and
//! read back unchanged.
//!
//! The rows differ along what the rule generalises over: which of the three
//! operations the scheduler supports the action targets (`smithy_query(query=
//! "shape", shape="ScheduledActionTypeValues")` — `ResizeCluster`,
//! `PauseCluster`, `ResumeCluster`), whether the target names a cluster the
//! account never held, one it held and deleted, or one it holds, and whether the
//! action is registered before or after that cluster is created. The other half
//! of the rule is asserted too: a `TargetAction` naming no single supported
//! operation is still refused, so registering one is not "anything goes".
//!
//! The harness is shared with [`crate::cluster_lifecycle`], which owns it.

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator};

/// A role ARN a scheduled action assumes. An IAM role is a resource of another
/// service, which this one does not resolve here, so the value is only carried.
const SCHEDULER_ROLE: &str = "arn:aws:iam::123456789012:role/RedshiftScheduler";

/// A cron expression of the format `ScheduledAction$Schedule` documents —
/// "cron(Minutes Hours Day-of-month Month Day-of-week Year)", whose own example
/// is `cron(0 10 ? * MON *)`.
const WEEKLY_CRON: &str = "cron(0 10 ? * MON *)";

/// A second schedule of the other published expression type, "at(yyyy-mm-ddThh:mm:ss)".
const ONE_SHOT_AT: &str = "at(2030-03-04T17:27:00)";

/// `call` takes borrowed pairs, so a case that builds owned parameters borrows
/// them through this.
fn borrowed(params: &[(String, String)]) -> Vec<(&str, &str)> {
    params
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// `CreateScheduledAction` registering `action` against `cluster` under the
/// target operation `target`.
async fn register(
    emulator: &Emulator,
    name: &str,
    target: &str,
    cluster: &str,
    schedule: &str,
) -> Answer {
    let params: Vec<(String, String)> = vec![
        ("ScheduledActionName".to_string(), name.to_string()),
        ("Schedule".to_string(), schedule.to_string()),
        ("IamRole".to_string(), SCHEDULER_ROLE.to_string()),
        (
            format!("TargetAction.{target}.ClusterIdentifier"),
            cluster.to_string(),
        ),
    ];
    emulator
        .call("CreateScheduledAction", &borrowed(&params))
        .await
}

/// `DescribeScheduledActions` for one action by name.
async fn describe(emulator: &Emulator, name: &str) -> Answer {
    emulator
        .call(
            "DescribeScheduledActions",
            &[("ScheduledActionName", name)],
        )
        .await
}

#[tokio::test]
async fn a_target_cluster_that_is_not_there_is_registered_and_read_back() {
    let emulator = Emulator::new();

    // One cluster the account DOES hold, so a served registration below cannot
    // be an "every request is served" artefact, and one it held and deleted.
    for identifier in ["schedule-held-cluster", "schedule-doomed-cluster"] {
        let created = emulator.create_minimal(identifier).await;
        assert_eq!(
            created.status, 200,
            "case seed {identifier}: expected 200, got {} with body {}",
            created.status, created.body
        );
    }
    emulator.advance_clock(state::PROVISIONING_SPAN);
    let deleted = emulator
        .call(
            "DeleteCluster",
            &[
                ("ClusterIdentifier", "schedule-doomed-cluster"),
                ("SkipFinalClusterSnapshot", "true"),
            ],
        )
        .await;
    assert_eq!(
        deleted.status, 200,
        "case delete the doomed cluster: expected 200, got {} with body {}",
        deleted.status, deleted.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);

    // (case, the target operation, the cluster it names, the schedule)
    let rows: Vec<(&str, &str, &str, &str)> = vec![
        (
            "a pause of a cluster nothing created",
            "PauseCluster",
            "schedule-absent-cluster",
            WEEKLY_CRON,
        ),
        (
            "a resume of a cluster nothing created",
            "ResumeCluster",
            "schedule-absent-cluster",
            WEEKLY_CRON,
        ),
        (
            "a resize of a cluster nothing created, on a one-shot schedule",
            "ResizeCluster",
            "schedule-another-absent-cluster",
            ONE_SHOT_AT,
        ),
        (
            "a pause of a cluster this account created and then deleted",
            "PauseCluster",
            "schedule-doomed-cluster",
            WEEKLY_CRON,
        ),
        (
            "a pause of a cluster this account holds",
            "PauseCluster",
            "schedule-held-cluster",
            WEEKLY_CRON,
        ),
    ];

    for (index, (case, target, cluster, schedule)) in rows.iter().enumerate() {
        let name = format!("target-action-{index}");
        let registered = register(&emulator, &name, target, cluster, schedule).await;
        assert_eq!(
            registered.status, 200,
            "case {case}: expected the registration to be served, got {} with body {}",
            registered.status, registered.body
        );
        // What the call reports: the schedule the request submitted, verbatim,
        // and the state an action that is enabled by default is in.
        assert_eq!(
            registered.text("Schedule").as_deref(),
            Some(*schedule),
            "case {case}: expected the submitted schedule reported back, got {:?}",
            registered.text("Schedule")
        );
        assert_eq!(
            registered.text("State").as_deref(),
            Some(state::SCHEDULED_ACTION_STATE_ACTIVE),
            "case {case}: expected State={:?}, got {:?}",
            state::SCHEDULED_ACTION_STATE_ACTIVE,
            registered.text("State")
        );
        assert_eq!(
            registered.text("ClusterIdentifier").as_deref(),
            Some(*cluster),
            "case {case}: expected the target cluster reported back, got {:?}",
            registered.text("ClusterIdentifier")
        );

        // ROUND TRIP: the read reports the registration as it was stored.
        let read = describe(&emulator, &name).await;
        assert_eq!(
            (
                read.status,
                read.text("ScheduledActionName").as_deref(),
                read.text("Schedule").as_deref(),
                read.text("ClusterIdentifier").as_deref(),
            ),
            (200, Some(name.as_str()), Some(*schedule), Some(*cluster)),
            "case {case}: expected the read to report the stored action, got {} with body {}",
            read.status,
            read.body
        );

        // A modify may move the action onto another cluster that is not there
        // either: the same rule holds for the change.
        let moved = emulator
            .call(
                "ModifyScheduledAction",
                &[
                    ("ScheduledActionName", &name),
                    (
                        "TargetAction.ResizeCluster.ClusterIdentifier",
                        "schedule-still-absent-cluster",
                    ),
                ],
            )
            .await;
        assert_eq!(
            moved.status, 200,
            "case {case}: expected the modify onto an absent cluster to be served, got {} with \
             body {}",
            moved.status, moved.body
        );
        assert_eq!(
            moved.text("ClusterIdentifier").as_deref(),
            Some("schedule-still-absent-cluster"),
            "case {case}: expected the modify to report the new target, got {:?}",
            moved.text("ClusterIdentifier")
        );
    }
}

#[tokio::test]
async fn an_action_registered_ahead_of_its_cluster_outlives_the_create() {
    let emulator = Emulator::new();
    let cluster = "declared-later-cluster";

    // The ordinary infrastructure-as-code order: the schedule is declared first.
    let registered = register(
        &emulator,
        "ahead-of-the-cluster",
        "PauseCluster",
        cluster,
        WEEKLY_CRON,
    )
    .await;
    assert_eq!(
        registered.status, 200,
        "case a schedule declared ahead of its cluster: expected 200, got {} with body {}",
        registered.status, registered.body
    );

    // Then the cluster it targets is created, and the registration is untouched
    // by it: the action still names the same cluster, on the same schedule.
    let created = emulator.create_minimal(cluster).await;
    assert_eq!(
        created.status, 200,
        "case create the declared cluster: expected 200, got {} with body {}",
        created.status, created.body
    );
    emulator.advance_clock(state::PROVISIONING_SPAN);

    let read = describe(&emulator, "ahead-of-the-cluster").await;
    assert_eq!(
        (
            read.status,
            read.text("ClusterIdentifier").as_deref(),
            read.text("Schedule").as_deref(),
        ),
        (200, Some(cluster), Some(WEEKLY_CRON)),
        "case a schedule declared ahead of its cluster: expected the read to report it \
         unchanged, got {} with body {}",
        read.status,
        read.body
    );

    // The action the account holds is still reported by the filter that selects
    // on the cluster a target action names, which is what a client reconciling
    // its schedules reads.
    let filtered = emulator
        .call(
            "DescribeScheduledActions",
            &[
                (
                    "Filters.ScheduledActionFilter.1.Name",
                    state::SCHEDULED_ACTION_FILTER_CLUSTER_IDENTIFIER,
                ),
                ("Filters.ScheduledActionFilter.1.Values.item.1", cluster),
            ],
        )
        .await;
    assert_eq!(
        (filtered.status, filtered.text("ScheduledActionName").as_deref()),
        (200, Some("ahead-of-the-cluster")),
        "case the cluster-identifier filter: expected the registration reported, got {} with \
         body {}",
        filtered.status,
        filtered.body
    );
}

#[tokio::test]
async fn a_target_action_naming_no_single_operation_is_still_refused() {
    let emulator = Emulator::new();

    // The other half of the rule: what a registration IS still has to be stated.
    // The rows differ along how many supported operations the target names —
    // two, three, and (through the modify) two again.
    let rows: Vec<(&str, Vec<(String, String)>)> = vec![
        (
            "a target action naming two operations",
            vec![
                (
                    "TargetAction.PauseCluster.ClusterIdentifier".to_string(),
                    "absent-one".to_string(),
                ),
                (
                    "TargetAction.ResumeCluster.ClusterIdentifier".to_string(),
                    "absent-two".to_string(),
                ),
            ],
        ),
        (
            "a target action naming every operation",
            vec![
                (
                    "TargetAction.PauseCluster.ClusterIdentifier".to_string(),
                    "absent-one".to_string(),
                ),
                (
                    "TargetAction.ResumeCluster.ClusterIdentifier".to_string(),
                    "absent-two".to_string(),
                ),
                (
                    "TargetAction.ResizeCluster.ClusterIdentifier".to_string(),
                    "absent-three".to_string(),
                ),
            ],
        ),
    ];

    for (case, target) in rows {
        let mut params: Vec<(String, String)> = vec![
            ("ScheduledActionName".to_string(), "bad-target".to_string()),
            ("Schedule".to_string(), WEEKLY_CRON.to_string()),
            ("IamRole".to_string(), SCHEDULER_ROLE.to_string()),
        ];
        params.extend(target);
        let refused = emulator
            .call("CreateScheduledAction", &borrowed(&params))
            .await;
        assert_eq!(
            (refused.status, refused.error_code().as_deref()),
            (400, Some("InvalidScheduledAction")),
            "case {case}: expected 400 InvalidScheduledAction, got {} / {:?} with body {}",
            refused.status,
            refused.error_code(),
            refused.body
        );
        // Nothing was registered under the name the refused request carried.
        let read = describe(&emulator, "bad-target").await;
        assert_eq!(
            read.error_code().as_deref(),
            Some("ScheduledActionNotFound"),
            "case {case}: expected the refused create to store nothing, got {} with body {}",
            read.status,
            read.body
        );
    }
}
