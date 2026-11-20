// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH scheduler release an `UpdateCluster` moves a cluster to.
//!
//! RULE under test: `UpdateSchedulerRequest$version` is "The scheduler version to update the
//! cluster to. You can only update to a newer version"
//! (`smithy_query(query="doc",shape="UpdateSchedulerRequest",member="version")`), so an
//! update is served only when the requested release label orders strictly AFTER the label the
//! cluster is already on. A request naming the same release, or an older one, is refused with
//! the modelled `ValidationException` and leaves the stored version where it was.
//!
//! Release labels order by their numeric components read left to right, a component a label
//! omits read as zero: so `25.5` and `25.05` label one release, `25.05` and `25.05.0` label
//! one release, and `9.1` is OLDER than `24.11` though it sorts after it as text.
//!
//! Driven over the awsJson1_0 wire through the same router the emulator serves from, on the
//! harness [`crate::cluster_lifecycle`] builds. Nothing here waits on the wall clock: a
//! transition this service owes is materialised before the next request is answered.

use serde_json::json;
use service_com_amazonaws_pcs_awsparallelcomputingservice::service::wire::CLIENT_ERROR_STATUS;
use smithy_server_runtime::MultiServiceRouter;

use crate::cluster_lifecycle::{call, emulator};

/// The service's fixed sentence for a scheduler version it will not move a cluster to.
const UNSUPPORTED_VERSION: &str = "AWS PCS doesn't support the scheduler version you provided.";

/// (case, the release the cluster is on, a release that is NEWER than it).
///
/// The rows differ along WHICH component moves (major, minor, patch), how far it moves, how
/// many components each label carries, and whether the label is one the "Valid Values" the
/// member prints names — a far-future major and a one-digit major are served too, because
/// that printed set is prose rather than a wire constraint.
const NEWER: &[(&str, &str, &str)] = &[
    ("minor-up", "25.05", "25.11"),
    ("major-up", "25.11", "26.05"),
    ("two-majors-up", "24.05", "26.05"),
    ("patch-added", "25.05", "25.05.1"),
    ("patch-up", "25.05.1", "25.05.2"),
    ("patch-dropped-for-a-newer-minor", "25.05.1", "25.11"),
    ("one-digit-minor-up", "25.5", "25.11"),
    ("short-major-up", "9.1", "24.11"),
    ("far-future-major", "26.05", "9999.1"),
];

/// (case, the release the cluster is on, a release that is NOT newer than it).
///
/// The rows cover the same release named the same way, the same release named with a leading
/// zero and with an explicit zero patch component, and an older release reached by each
/// component — including one whose label sorts AFTER the stored one as text.
const NOT_NEWER: &[(&str, &str, &str)] = &[
    ("same-label", "26.05", "26.05"),
    ("same-release-leading-zero", "25.05", "25.5"),
    ("same-release-zero-patch", "25.05", "25.05.0"),
    ("major-down", "26.05", "23.11"),
    ("minor-down", "25.11", "25.05"),
    ("patch-down", "25.05.1", "25.05"),
    ("far-future-major-down", "9999.1", "26.05"),
    ("older-label-that-sorts-after-as-text", "24.11", "9.1"),
];

/// A cluster on `version`, settled `ACTIVE`, named for `case` — its generated id.
async fn cluster_on(router: &MultiServiceRouter, case: &str, version: &str) -> String {
    let (status, created) = call(
        router,
        "CreateCluster",
        &json!({
            "clusterName": format!("ver-{case}"),
            "scheduler": {"type": "SLURM", "version": version},
            "size": "SMALL",
            "networking": {
                "subnetIds": ["subnet-0123456789abcdef0"],
                "securityGroupIds": ["sg-0123456789abcdef0"],
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case}: expected a cluster on {version}, got {created}"
    );
    let id = created["cluster"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();

    // The transition the create owes is materialised before the NEXT request is answered, so
    // this read is what settles the cluster rather than any wait.
    let (status, settled) = call(router, "GetCluster", &json!({"clusterIdentifier": &id})).await;
    assert_eq!(status, 200, "case {case}: GetCluster got {settled}");
    assert_eq!(
        settled["cluster"]["status"],
        json!("ACTIVE"),
        "case {case}: expected the cluster settled before the update, got {settled}"
    );
    assert_eq!(
        settled["cluster"]["scheduler"]["version"],
        json!(version),
        "case {case}: expected the cluster on {version}, got {settled}"
    );
    id
}

#[tokio::test]
async fn update_cluster_moves_a_cluster_to_a_newer_scheduler_release() {
    let router = emulator();

    for (case, stored, requested) in NEWER {
        let id = cluster_on(&router, case, stored).await;
        let (status, updated) = call(
            &router,
            "UpdateCluster",
            &json!({"clusterIdentifier": &id, "scheduler": {"version": requested}}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case}: expected the update from {stored} to {requested} to be served, got \
             {updated}"
        );
        assert_eq!(
            updated["cluster"]["scheduler"]["version"],
            json!(requested),
            "case {case}: expected the requested release, got {updated}"
        );
        assert_eq!(
            updated["cluster"]["status"],
            json!("UPDATING"),
            "case {case}: expected the transient status, got {updated}"
        );

        let (status, settled) =
            call(&router, "GetCluster", &json!({"clusterIdentifier": &id})).await;
        assert_eq!(status, 200, "case {case}: GetCluster got {settled}");
        assert_eq!(
            settled["cluster"]["scheduler"]["version"],
            json!(requested),
            "case {case}: expected {requested} to persist, got {settled}"
        );
        assert_eq!(
            settled["cluster"]["status"],
            json!("ACTIVE"),
            "case {case}: expected the cluster to settle back, got {settled}"
        );
    }
}

#[tokio::test]
async fn update_cluster_refuses_a_scheduler_release_that_is_not_newer() {
    let router = emulator();

    for (case, stored, requested) in NOT_NEWER {
        let id = cluster_on(&router, case, stored).await;
        let (status, refused) = call(
            &router,
            "UpdateCluster",
            &json!({"clusterIdentifier": &id, "scheduler": {"version": requested}}),
        )
        .await;
        assert_eq!(
            status, CLIENT_ERROR_STATUS,
            "case {case}: expected the update from {stored} to {requested} refused, got {status} \
             with {refused}"
        );
        assert_eq!(
            refused["__type"],
            json!("ValidationException"),
            "case {case}: got {refused}"
        );
        assert_eq!(
            refused["message"],
            json!(UNSUPPORTED_VERSION),
            "case {case}: expected the service's own sentence, got {refused}"
        );

        // Nothing was written: the cluster is on the release it was on, and the refused call
        // did not churn it to the transient status either.
        let (status, unchanged) =
            call(&router, "GetCluster", &json!({"clusterIdentifier": &id})).await;
        assert_eq!(status, 200, "case {case}: GetCluster got {unchanged}");
        assert_eq!(
            unchanged["cluster"]["scheduler"]["version"],
            json!(stored),
            "case {case}: expected the stored release untouched, got {unchanged}"
        );
        assert_eq!(
            unchanged["cluster"]["status"],
            json!("ACTIVE"),
            "case {case}: expected the cluster left ACTIVE, got {unchanged}"
        );
    }
}

#[tokio::test]
async fn an_update_that_carries_no_scheduler_leaves_the_stored_release_alone() {
    let router = emulator();
    let id = cluster_on(&router, "no-scheduler-member", "25.05").await;

    // The ordering rule gates the `scheduler` member only: an update that names other
    // members is served whatever release the cluster is on, and keeps that release.
    let (status, updated) = call(
        &router,
        "UpdateCluster",
        &json!({
            "clusterIdentifier": &id,
            "slurmConfiguration": {"scaleDownIdleTimeInSeconds": 600},
        }),
    )
    .await;
    assert_eq!(status, 200, "case slurm-only update: got {updated}");
    assert_eq!(
        updated["cluster"]["scheduler"]["version"],
        json!("25.05"),
        "case slurm-only update: expected the stored release kept, got {updated}"
    );
}
