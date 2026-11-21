// SPDX-License-Identifier: CC0-1.0

//! What a `Cluster` reports about Availability Zone relocation and about its
//! maintenance window, end to end over the awsQuery wire.
//!
//! The rule under test: every member Amazon Redshift serializes on a `Cluster` is
//! reported on every answer that carries one, with its value derived from that
//! cluster's own stored configuration. Three of those members are not in this
//! crate's model snapshot and are declared in the overlay beside it (see
//! `smithy-model/LOCAL-ADDITIONS.md`):
//!
//! * `AvailabilityZoneRelocation` — the relocation option's enablement, which the
//!   snapshot's `AvailabilityZoneRelocationStatus` also reports;
//! * `AzDisasterRecoveryStatus` — the enablement of the cluster's
//!   Availability-Zone-level recovery, which for a provisioned cluster IS that
//!   relocation;
//! * `IsMaintenanceWindowActivated` — whether the cluster's preferred maintenance
//!   window is in force, which a deferral suspends for the span it covers.
//!
//! Both tests assert over a TABLE: the relocation one over every way a request can
//! say whether relocation is on, on the create answer and on the read, and the
//! maintenance one over the forms a deferral takes — in force, not yet begun,
//! already over, and taken off again.

use std::time::{Duration, SystemTime};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use service_com_amazonaws_redshift_redshiftserviceversion20121201::state;

use crate::cluster_lifecycle::{all_text, Answer, Emulator};

/// The members the overlay adds to `Cluster`, which is the whole of what it may
/// differ from the snapshot by.
const ADDED_MEMBERS: [&str; 3] = [
    "AvailabilityZoneRelocation",
    "AzDisasterRecoveryStatus",
    "IsMaintenanceWindowActivated",
];

/// A span long enough that no host's speed can move an instant across it: a
/// deferral this far ahead has not begun, and one this far back is over, however
/// long the calls in between take.
const WELL_CLEAR: Duration = Duration::from_secs(30 * 24 * 60 * 60);

/// `at` in the ISO 8601 form the deferral's time members are documented to take.
fn iso8601(at: SystemTime) -> String {
    DateTime::from(at)
        .fmt(Format::DateTime)
        .expect("an instant renders as ISO 8601")
}

/// The one value of `<tag>` a one-cluster answer carries, or `None` when the
/// answer carries no such element at all.
fn member(answer: &Answer, tag: &str) -> Option<String> {
    let found = all_text(&answer.body, tag);
    assert!(
        found.len() <= 1,
        "the answer describes one cluster, so it carries at most one <{tag}>, got {found:?}"
    );
    found.into_iter().next()
}

/// Assert the four members this suite is about, on one answer.
fn assert_members(
    case: &str,
    what: &str,
    answer: &Answer,
    relocation: &str,
    window_activated: bool,
) {
    for tag in [
        "AvailabilityZoneRelocationStatus",
        "AvailabilityZoneRelocation",
        "AzDisasterRecoveryStatus",
    ] {
        assert_eq!(
            member(answer, tag).as_deref(),
            Some(relocation),
            "case {case}: expected {what} to report <{tag}> as {relocation:?}, got {:?} with body {}",
            member(answer, tag),
            answer.body
        );
    }
    let expected = window_activated.to_string();
    assert_eq!(
        member(answer, "IsMaintenanceWindowActivated").as_deref(),
        Some(expected.as_str()),
        "case {case}: expected {what} to report <IsMaintenanceWindowActivated> as {expected:?}, got {:?} with body {}",
        member(answer, "IsMaintenanceWindowActivated"),
        answer.body
    );
}

/// Every cluster reports its relocation enablement on all three of the members
/// that carry it, and reports its maintenance window as activated — on the create
/// answer and on every later read alike.
#[tokio::test]
async fn a_clusters_relocation_is_reported_on_every_member_that_carries_it() {
    let emulator = Emulator::new();

    // One row per way a request can say whether relocation is on: asked for,
    // turned off, and left unsaid — the last taking whatever a silent request
    // gets, read from the service's own rule rather than written out here.
    let silent = state::availability_zone_relocation_status(None);
    let rows: Vec<(&str, Option<&str>, &str)> = vec![
        ("relocation asked for", Some("true"), "enabled"),
        ("relocation turned off", Some("false"), "disabled"),
        ("the option unnamed", None, silent.as_str()),
    ];

    for (index, (case, requested, expected)) in rows.into_iter().enumerate() {
        let identifier = format!("relocation-cluster-{index}");
        let mut params = vec![
            ("ClusterIdentifier", identifier.as_str()),
            ("NodeType", "ra3.xlplus"),
            ("MasterUsername", "admin"),
        ];
        if let Some(option) = requested {
            params.push(("AvailabilityZoneRelocation", option));
        }
        let created = emulator.call("CreateCluster", &params).await;
        assert_eq!(
            created.status, 200,
            "case {case}: expected CreateCluster served, got {} with body {}",
            created.status, created.body
        );
        assert_members(case, "the create answer", &created, expected, true);

        let described = emulator
            .call(
                "DescribeClusters",
                &[("ClusterIdentifier", identifier.as_str())],
            )
            .await;
        assert_eq!(
            described.status, 200,
            "case {case}: expected DescribeClusters served, got {} with body {}",
            described.status, described.body
        );
        assert_members(case, "the read", &described, expected, true);
    }
}

/// A deferral deactivates the cluster's maintenance window for the span it
/// covers, and only for that span.
#[tokio::test]
async fn a_deferral_deactivates_the_maintenance_window_for_the_span_it_covers() {
    let emulator = Emulator::new();
    let identifier = "deferred-window-cluster";
    let case = "a serving cluster";

    let created = emulator.create_minimal(identifier).await;
    assert_eq!(
        created.status, 200,
        "case {case}: expected CreateCluster served, got {} with body {}",
        created.status, created.body
    );
    // `ModifyClusterMaintenance` takes no configuration change from a cluster the
    // service is still working on, so the deferral rows wait for the cluster to
    // be serving first.
    let (serving, _) = emulator
        .poll_until(
            case,
            "DescribeClusters",
            &[("ClusterIdentifier", identifier)],
            |answer| answer.text("ClusterStatus").as_deref() == Some(state::STATUS_AVAILABLE),
        )
        .await;
    assert_eq!(
        member(&serving, "IsMaintenanceWindowActivated").as_deref(),
        Some("true"),
        "case {case}: expected a cluster nobody deferred to report its window activated, got body {}",
        serving.body
    );

    let now = SystemTime::now();
    let well_ahead = iso8601(now + WELL_CLEAR);
    let further_ahead = iso8601(now + WELL_CLEAR + WELL_CLEAR);
    let well_back = iso8601(now - WELL_CLEAR - WELL_CLEAR);
    let less_far_back = iso8601(now - WELL_CLEAR);

    // Each row is one `ModifyClusterMaintenance` request and whether the window
    // is in force afterwards: a deferral covering now suspends it, and one that
    // has not begun, one already over and no deferral at all leave it in force.
    type DeferralRow<'a> = (&'a str, Vec<(&'a str, &'a str)>, bool);
    let rows: Vec<DeferralRow> = vec![
        (
            "a deferral described by a duration",
            vec![("DeferMaintenance", "true"), ("DeferMaintenanceDuration", "7")],
            false,
        ),
        (
            "a deferral described by an end time",
            vec![
                ("DeferMaintenance", "true"),
                ("DeferMaintenanceEndTime", well_ahead.as_str()),
            ],
            false,
        ),
        (
            "a deferral that has not begun",
            vec![
                ("DeferMaintenance", "true"),
                ("DeferMaintenanceStartTime", well_ahead.as_str()),
                ("DeferMaintenanceEndTime", further_ahead.as_str()),
            ],
            true,
        ),
        (
            "a deferral already over",
            vec![
                ("DeferMaintenance", "true"),
                ("DeferMaintenanceStartTime", well_back.as_str()),
                ("DeferMaintenanceEndTime", less_far_back.as_str()),
            ],
            true,
        ),
        (
            "the deferral taken off",
            vec![("DeferMaintenance", "false")],
            true,
        ),
    ];

    for (case, parameters, activated) in rows {
        let mut params: Vec<(&str, &str)> = vec![("ClusterIdentifier", identifier)];
        params.extend(parameters.iter().copied());
        let answer = emulator.call("ModifyClusterMaintenance", &params).await;
        assert_eq!(
            answer.status, 200,
            "case {case}: expected ModifyClusterMaintenance served, got {} with body {}",
            answer.status, answer.body
        );
        let expected = activated.to_string();
        assert_eq!(
            member(&answer, "IsMaintenanceWindowActivated").as_deref(),
            Some(expected.as_str()),
            "case {case}: expected the modify answer to report {expected:?}, got {:?} with body {}",
            member(&answer, "IsMaintenanceWindowActivated"),
            answer.body
        );

        // The read reports what the write persisted: one cluster record, one
        // answer to this question.
        let described = emulator
            .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
            .await;
        assert_eq!(
            member(&described, "IsMaintenanceWindowActivated").as_deref(),
            Some(expected.as_str()),
            "case {case}: expected the read to report {expected:?}, got {:?} with body {}",
            member(&described, "IsMaintenanceWindowActivated"),
            described.body
        );
        // Relocation is untouched by a maintenance change: the member set one
        // rule fills in is reported whole on every answer.
        assert_eq!(
            member(&described, "AzDisasterRecoveryStatus"),
            member(&described, "AvailabilityZoneRelocationStatus"),
            "case {case}: expected the AZ recovery status to follow the relocation status, got body {}",
            described.body
        );
    }

    // A deferral whose span ENDS with nobody calling: the window is in force
    // again at the next read, because the report materialises what the clock
    // already owed rather than repeating what the last write computed. The
    // deferral's own end is moved into the past in the store — the span it was
    // written with is a wall-clock one, so nothing here waits for it.
    let case = "a deferral whose end passed with nobody calling";
    let deferred = emulator
        .call(
            "ModifyClusterMaintenance",
            &[
                ("ClusterIdentifier", identifier),
                ("DeferMaintenance", "true"),
                ("DeferMaintenanceDuration", "7"),
            ],
        )
        .await;
    assert_eq!(
        member(&deferred, "IsMaintenanceWindowActivated").as_deref(),
        Some("false"),
        "case {case}: expected the deferral to suspend the window first, got body {}",
        deferred.body
    );
    {
        let store = state::service_state(&*emulator.engine).expect("the Redshift store");
        let mut store = store.lock();
        let stored = store
            .clusters
            .iter_mut()
            .find(|stored| stored.cluster.cluster_identifier.as_deref() == Some(identifier))
            .expect("the cluster this case created");
        for window in stored
            .cluster
            .deferred_maintenance_windows
            .iter_mut()
            .flatten()
        {
            window.defer_maintenance_end_time = Some(DateTime::from(SystemTime::now() - WELL_CLEAR));
        }
    }
    let described = emulator
        .call("DescribeClusters", &[("ClusterIdentifier", identifier)])
        .await;
    assert_eq!(
        member(&described, "IsMaintenanceWindowActivated").as_deref(),
        Some("true"),
        "case {case}: expected the read to report the window in force again, got {:?} with body {}",
        member(&described, "IsMaintenanceWindowActivated"),
        described.body
    );
}

/// One shape declaration out of a model file, as JSON.
fn shapes_of(path: &str) -> serde_json::Map<String, serde_json::Value> {
    let text = std::fs::read_to_string(format!("{}/{path}", env!("CARGO_MANIFEST_DIR")))
        .unwrap_or_else(|error| panic!("case {path}: the model file reads back: {error}"));
    let parsed: serde_json::Value = serde_json::from_str(&text)
        .unwrap_or_else(|error| panic!("case {path}: the model file parses: {error}"));
    parsed["shapes"]
        .as_object()
        .unwrap_or_else(|| panic!("case {path}: a Smithy model declares shapes"))
        .clone()
}

/// The overlay model restates the snapshot's `Cluster` exactly, and differs from
/// it by exactly the three members AWS answers with and the snapshot does not
/// declare.
///
/// This is the guard on the overlay (see `smithy-model/LOCAL-ADDITIONS.md`): a
/// snapshot refreshed with one of those members, with a member the overlay does
/// not restate, or with a changed target or list item element name, fails here —
/// and the local declaration should then give way to the modelled shape.
#[test]
fn the_overlay_restates_the_snapshots_cluster_shape() {
    let snapshot = shapes_of("smithy-model/com-amazonaws-redshift-redshiftserviceversion20121201.json");
    let overlay = shapes_of("smithy-model/local-additions.json");

    let members_of = |shapes: &serde_json::Map<String, serde_json::Value>| {
        shapes["com.amazonaws.redshift#Cluster"]["members"]
            .as_object()
            .expect("Cluster declares members")
            .clone()
    };
    let snapshot_members = members_of(&snapshot);
    let overlay_members = members_of(&overlay);

    // Every member the snapshot declares is restated, at the SAME target: the
    // overlay generates the one `Cluster` type of this crate, so a member it
    // dropped would leave the wire a member short.
    for (name, declared) in &snapshot_members {
        let restated = overlay_members.get(name).unwrap_or_else(|| {
            panic!("case {name}: the overlay must restate every member the snapshot declares")
        });
        assert_eq!(
            restated["target"], declared["target"],
            "case {name}: expected the overlay to target {:?}, got {:?}",
            declared["target"], restated["target"]
        );
    }

    // And it adds exactly the three, so a snapshot that starts declaring one of
    // them — or an overlay that invents a fourth — fails here.
    let mut added: Vec<&String> = overlay_members
        .keys()
        .filter(|name| !snapshot_members.contains_key(*name))
        .collect();
    added.sort();
    let mut expected: Vec<String> = ADDED_MEMBERS.iter().map(|name| (*name).to_string()).collect();
    expected.sort();
    let added: Vec<String> = added.into_iter().cloned().collect();
    assert_eq!(
        added, expected,
        "case the added members: expected exactly {expected:?}, got {added:?}"
    );

    // Every OTHER shape the overlay restates keeps the snapshot's type, and a
    // list keeps the item target and `@xmlName` the awsQuery writer reads off it.
    for (name, restated) in overlay.iter().filter(|(name, _)| {
        *name != "com.amazonaws.redshift#Cluster"
    }) {
        let declared = snapshot
            .get(name)
            .unwrap_or_else(|| panic!("case {name}: the overlay may only restate shapes the snapshot declares"));
        assert_eq!(
            restated["type"], declared["type"],
            "case {name}: expected type {:?}, got {:?}",
            declared["type"], restated["type"]
        );
        if restated["type"] == "list" {
            assert_eq!(
                restated["member"]["target"], declared["member"]["target"],
                "case {name}: expected the item target {:?}, got {:?}",
                declared["member"]["target"], restated["member"]["target"]
            );
            assert_eq!(
                restated["member"]["traits"]["smithy.api#xmlName"],
                declared["member"]["traits"]["smithy.api#xmlName"],
                "case {name}: expected the item element name {:?}, got {:?}",
                declared["member"]["traits"]["smithy.api#xmlName"],
                restated["member"]["traits"]["smithy.api#xmlName"]
            );
        }
        if restated["type"] == "enum" {
            let values = |shape: &serde_json::Value| {
                let mut found: Vec<String> = shape["members"]
                    .as_object()
                    .map(|members| {
                        members
                            .values()
                            .filter_map(|member| {
                                member["traits"]["smithy.api#enumValue"]
                                    .as_str()
                                    .map(str::to_string)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                found.sort();
                found
            };
            assert_eq!(
                values(restated),
                values(declared),
                "case {name}: expected the enum's values {:?}, got {:?}",
                values(declared),
                values(restated)
            );
        }
    }
}
