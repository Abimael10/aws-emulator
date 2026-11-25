// SPDX-License-Identifier: CC0-1.0

//! What EVERY answer that carries a `Cluster` reports about Availability Zone
//! relocation and about the cluster's maintenance window, end to end over the
//! awsQuery wire.
//!
//! The rule under test: an answer carrying a `Cluster` reports the whole member
//! set Amazon Redshift serializes on one — the relocation option's enablement,
//! the Availability-Zone disaster-recovery status that option IS for a
//! provisioned cluster, and whether the preferred maintenance window is in force
//! — each derived from THAT cluster's own stored configuration, whichever
//! operation answers and whatever state the cluster is in.
//!
//! A sibling suite (`crate::cluster_relocation_and_maintenance_members`) pins
//! the three members on the create answer and on the read, and the deferral rule
//! behind the window member. This one pins the member SET across the rest of the
//! surface: every other operation that answers with a cluster — the
//! configuration, maintenance and role changes, the lifecycle writes, the delete
//! — and across the two paths that decide the relocation value, a `ModifyCluster`
//! and a restore.
//!
//! The members: `AvailabilityZoneRelocationStatus` is the one this crate's model
//! snapshot declares (`smithy_query(query="shape", shape="Cluster")`), and
//! `AvailabilityZoneRelocation`, `AzDisasterRecoveryStatus` and
//! `IsMaintenanceWindowActivated` are the three the overlay model beside it
//! declares because Amazon Redshift answers with them and the snapshot does not
//! declare them (`smithy-model/LOCAL-ADDITIONS.md`).

use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{Answer, Emulator, HARNESS_MASTER_PASSWORD};

/// The members that carry one cluster's relocation enablement: the modelled one
/// first, then the two the overlay adds. ONE stored setting feeds all three, so
/// no answer may report a cluster as relocatable on one of them and not on
/// another.
const RELOCATION_MEMBERS: [&str; 3] = [
    "AvailabilityZoneRelocationStatus",
    "AvailabilityZoneRelocation",
    "AzDisasterRecoveryStatus",
];

/// The member reporting whether the cluster's preferred maintenance window is in
/// force.
const WINDOW_MEMBER: &str = "IsMaintenanceWindowActivated";

/// The node type every cluster in this suite is created with, which is one of
/// the types `DescribeOrderableClusterOptions` offers.
const NODE_TYPE: &str = "ra3.xlplus";

/// Assert that `answer` was served, that it reports this cluster's relocation
/// enablement on every member carrying it, and that it reports the maintenance
/// window as `window_activated`.
fn assert_member_set(case: &str, answer: &Answer, relocation: &str, window_activated: bool) {
    assert_eq!(
        answer.status, 200,
        "case {case}: expected the answer served, got {} with body {}",
        answer.status, answer.body
    );
    for member in RELOCATION_MEMBERS {
        assert_eq!(
            answer.text(member).as_deref(),
            Some(relocation),
            "case {case}: expected <{member}> to report {relocation:?}, got {:?} with body {}",
            answer.text(member),
            answer.body
        );
    }
    let expected = window_activated.to_string();
    assert_eq!(
        answer.text(WINDOW_MEMBER).as_deref(),
        Some(expected.as_str()),
        "case {case}: expected <{WINDOW_MEMBER}> to report {expected:?}, got {:?} with body {}",
        answer.text(WINDOW_MEMBER),
        answer.body
    );
}

/// Create a cluster, with the relocation option stated exactly as `relocation`
/// says — absent for a request that leaves the option unsaid.
async fn create(emulator: &Emulator, identifier: &str, relocation: Option<&str>) -> Answer {
    let mut params = vec![
        ("ClusterIdentifier", identifier),
        ("NodeType", NODE_TYPE),
        ("MasterUsername", "admin"),
        ("MasterUserPassword", HARNESS_MASTER_PASSWORD),
    ];
    if let Some(option) = relocation {
        params.push(("AvailabilityZoneRelocation", option));
    }
    emulator.call("CreateCluster", &params).await
}

/// The cluster as `DescribeClusters` reports it once it reports `status`.
///
/// The wait is on the CONDITION, looked for again through the harness's own poll
/// loop until it holds or a deadline far longer than the span this service owes
/// the transition in has passed — so a slow or loaded host only costs it more
/// polls.
async fn when_reported(emulator: &Emulator, case: &str, identifier: &str, status: &str) -> Answer {
    let (answer, _polls) = emulator
        .poll_until(
            case,
            "DescribeClusters",
            &[("ClusterIdentifier", identifier)],
            |answer| answer.text("ClusterStatus").as_deref() == Some(status),
        )
        .await;
    answer
}

/// Every operation that answers with a `Cluster` reports that cluster's own
/// relocation enablement and its maintenance-window state.
#[tokio::test]
async fn every_operation_that_answers_with_a_cluster_reports_the_member_set() {
    let emulator = Emulator::new();
    let identifier = "member-set-cluster";

    // Created with relocation turned OFF, so every answer below has to report
    // this cluster's own setting rather than the one a silent request gets.
    let created = create(&emulator, identifier, Some("false")).await;
    assert_member_set("the create answer", &created, "disabled", true);
    let serving = when_reported(
        &emulator,
        "the cluster coming up",
        identifier,
        state::STATUS_AVAILABLE,
    )
    .await;
    assert_member_set("the read of a serving cluster", &serving, "disabled", true);

    // One row per operation that answers with a `Cluster` beside the create and
    // the read: a configuration change, a maintenance change, a role change, a
    // restart, a pause, a resume, and the delete. The last field is the state
    // the row leaves the cluster in, waited for before the next row drives it —
    // and `None` where the row leaves the cluster serving, or takes it away.
    type Row<'a> = (&'a str, &'a str, Vec<(&'a str, &'a str)>, Option<&'a str>);
    let rows: Vec<Row> = vec![
        (
            "a configuration change",
            "ModifyCluster",
            vec![("AllowVersionUpgrade", "false")],
            None,
        ),
        (
            "a maintenance change",
            "ModifyClusterMaintenance",
            vec![("DeferMaintenance", "false")],
            None,
        ),
        (
            "a role change",
            "ModifyClusterIamRoles",
            vec![(
                "AddIamRoles.IamRoleArn.1",
                "arn:aws:iam::123456789012:role/member-set-probe",
            )],
            Some(state::STATUS_AVAILABLE),
        ),
        ("a restart", "RebootCluster", vec![], Some(state::STATUS_AVAILABLE)),
        ("a pause", "PauseCluster", vec![], Some(state::STATUS_PAUSED)),
        ("a resume", "ResumeCluster", vec![], Some(state::STATUS_AVAILABLE)),
        (
            "the delete",
            "DeleteCluster",
            vec![("SkipFinalClusterSnapshot", "true")],
            None,
        ),
    ];

    for (case, action, extra, settles_to) in rows {
        let mut params: Vec<(&str, &str)> = vec![("ClusterIdentifier", identifier)];
        params.extend(extra.iter().copied());
        let answer = emulator.call(action, &params).await;
        assert_member_set(case, &answer, "disabled", true);
        if let Some(status) = settles_to {
            let read = when_reported(&emulator, case, identifier, status).await;
            assert_member_set(
                &format!("{case}, read back once it reports {status}"),
                &read,
                "disabled",
                true,
            );
        }
    }
}

/// A `ModifyCluster` that speaks about the relocation option moves every member
/// carrying it, and one that leaves the option unsaid moves none of them.
#[tokio::test]
async fn a_relocation_change_moves_every_member_that_carries_it() {
    let emulator = Emulator::new();
    let identifier = "relocation-change-cluster";

    let created = create(&emulator, identifier, Some("true")).await;
    assert_member_set("the create answer", &created, "enabled", true);
    when_reported(
        &emulator,
        "the cluster coming up",
        identifier,
        state::STATUS_AVAILABLE,
    )
    .await;

    // One row per way a modify can speak about the option: turning it off,
    // leaving it unsaid after that, turning it back on, and leaving it unsaid
    // again — so each row's expectation is the value the last row that SPOKE
    // asked for. Every row carries another change as well, so the call is a real
    // modify however it speaks about relocation.
    let rows: Vec<(&str, Option<&str>, &str)> = vec![
        ("relocation turned off", Some("false"), "disabled"),
        ("the option left unsaid after being turned off", None, "disabled"),
        ("relocation turned back on", Some("true"), "enabled"),
        ("the option left unsaid after being turned on", None, "enabled"),
    ];

    for (case, requested, expected) in rows {
        let mut params = vec![
            ("ClusterIdentifier", identifier),
            ("AllowVersionUpgrade", "true"),
        ];
        if let Some(option) = requested {
            params.push(("AvailabilityZoneRelocation", option));
        }
        let answer = emulator.call("ModifyCluster", &params).await;
        assert_member_set(&format!("{case}, the modify answer"), &answer, expected, true);

        // The read reports what the write persisted: one cluster record, one
        // answer to this question.
        let read = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
            .await;
        assert_member_set(&format!("{case}, the read"), &read, expected, true);
    }
}

/// A restored cluster reports the member set too, with the relocation the
/// restore request asked for — or the one the cluster the snapshot was taken
/// from had, for a request that asked for nothing.
#[tokio::test]
async fn a_restored_cluster_reports_the_member_set_from_its_own_configuration() {
    let emulator = Emulator::new();
    let source = "member-set-restore-source";
    let snapshot = "member-set-restore-snapshot";

    let created = create(&emulator, source, Some("true")).await;
    assert_member_set("the source cluster", &created, "enabled", true);
    when_reported(
        &emulator,
        "the source cluster coming up",
        source,
        state::STATUS_AVAILABLE,
    )
    .await;

    let taken = emulator
        .call(
            "CreateClusterSnapshot",
            &[
                ("SnapshotIdentifier", snapshot),
                ("ClusterIdentifier", source),
            ],
        )
        .await;
    assert_eq!(
        taken.status, 200,
        "case the snapshot of the source cluster: expected 200, got {} with body {}",
        taken.status, taken.body
    );
    // Only a snapshot the clock has already made available holds a cluster to
    // rebuild; the wait is on that condition, not on a span.
    emulator
        .poll_until(
            "the snapshot being taken",
            "DescribeClusterSnapshots",
            &[("SnapshotIdentifier", snapshot)],
            |answer| answer.text("Status").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;

    // One row per way the restore can speak about the option: not at all, which
    // takes the source cluster's own setting, and turning it off, which is the
    // restored cluster's own setting from then on.
    let rows: Vec<(&str, &str, Option<&str>, &str)> = vec![
        (
            "a restore that names no relocation option",
            "member-set-restored-silent",
            None,
            "enabled",
        ),
        (
            "a restore that turns relocation off",
            "member-set-restored-off",
            Some("false"),
            "disabled",
        ),
    ];

    for (case, target, requested, expected) in rows {
        let mut params = vec![
            ("ClusterIdentifier", target),
            ("SnapshotIdentifier", snapshot),
        ];
        if let Some(option) = requested {
            params.push(("AvailabilityZoneRelocation", option));
        }
        let answer = emulator.call("RestoreFromClusterSnapshot", &params).await;
        assert_member_set(&format!("{case}, the restore answer"), &answer, expected, true);

        let read = when_reported(&emulator, case, target, state::STATUS_AVAILABLE).await;
        assert_member_set(&format!("{case}, the read"), &read, expected, true);
    }
}
