// SPDX-License-Identifier: CC0-1.0

//! What a description of an application's snapshot configuration reports.
//!
//! The rule under test: a described snapshot configuration reports the snapshot MODE the
//! application's snapshots are taken under beside whether they are taken at all, so every
//! response carrying an `ApplicationSnapshotConfigurationDescription` carries both members
//! — the mode being the one an application that never selected one runs under, since this
//! API revision models no member for choosing another. The mode is held on the stored
//! description, so every reader of one application agrees on it, an update that names only
//! `SnapshotsEnabled` carries it through, and a version the application has LEFT keeps
//! reporting the mode that version ran under.
//!
//! The rows span what the rule generalises over: the runtime family the application runs on
//! (the Flink family and the Zeppelin notebooks, read from the generated
//! `RUNTIME_ENVIRONMENT_VALUES`), how its mode was decided (named by the request, implied by
//! its runtime), whether its create configured snapshots at all, and which way it configured
//! them.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_kinesisanalyticsv2_kinesisanalytics_20180523::ops::types::RUNTIME_ENVIRONMENT_VALUES;
use service_com_amazonaws_kinesisanalyticsv2_kinesisanalytics_20180523::state::DEFAULT_SNAPSHOT_MODE;

use super::harness::{call, create_body, creates_applications_on, describe, router};

/// The `RuntimeEnvironment` family a Studio notebook runs on
/// (`smithy_query(query="shape",shape="RuntimeEnvironment")`: `ZEPPELIN-FLINK-<major>_<minor>`).
/// A notebook has no version history to read — `DescribeApplicationVersion` is "supported
/// only for Managed Service for Apache Flink" (its own doc) — so a row on one asserts the
/// describe family it IS served.
const NOTEBOOK_FAMILY: &str = "ZEPPELIN-";

/// The described snapshot configuration of one `ApplicationDetail`.
fn snapshot_description(detail: &Value) -> &Value {
    &detail["ApplicationConfigurationDescription"]["ApplicationSnapshotConfigurationDescription"]
}

/// The `(SnapshotsEnabled, SnapshotMode)` pair a detail describes, as the two wire values.
fn described(detail: &Value) -> (Option<bool>, Option<&str>) {
    let snapshots = snapshot_description(detail);
    (
        snapshots["SnapshotsEnabled"].as_bool(),
        snapshots["SnapshotMode"].as_str(),
    )
}

/// Assert that `detail` describes its snapshot configuration with both members: snapshots
/// switched the way the case expects, and the mode in force.
fn assert_describes_mode(case: &str, reader: &str, detail: &Value, snapshots_enabled: bool) {
    assert_eq!(
        described(detail),
        (Some(snapshots_enabled), Some(DEFAULT_SNAPSHOT_MODE)),
        "case {case:?} read through {reader}: expected SnapshotsEnabled {snapshots_enabled} \
         and SnapshotMode {DEFAULT_SNAPSHOT_MODE:?}, got {:?}",
        snapshot_description(detail)
    );
}

/// Create an application, asserting the call was accepted, and answer its detail.
async fn create_with(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    runtime: &str,
    mode: Option<&str>,
    configuration: Option<Value>,
) -> Value {
    let mut body = create_body(name, runtime);
    if let Some(mode) = mode {
        body["ApplicationMode"] = json!(mode);
    }
    if let Some(configuration) = configuration {
        body["ApplicationConfiguration"] = configuration;
    }
    let (status, created) = call(router, "CreateApplication", &body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateApplication to be accepted, got {status} {created}"
    );
    created["ApplicationDetail"].clone()
}

/// `DescribeApplicationVersion` on one version of `name`, asserting the call succeeded.
async fn describe_version(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    version_id: i64,
) -> Value {
    let (status, answered) = call(
        router,
        "DescribeApplicationVersion",
        &json!({"ApplicationName": name, "ApplicationVersionId": version_id}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?} v{version_id}: DescribeApplicationVersion got {status} {answered}"
    );
    answered["ApplicationVersionDetail"].clone()
}

/// Apply one `UpdateApplication` to `name` against the version it is on now, and answer the
/// detail the update reported.
async fn update(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    current: i64,
    configuration_update: Value,
) -> Value {
    let (status, updated) = call(
        router,
        "UpdateApplication",
        &json!({
            "ApplicationName": name,
            "CurrentApplicationVersionId": current,
            "ApplicationConfigurationUpdate": configuration_update,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?} v{current}: UpdateApplication got {status} {updated}"
    );
    updated["ApplicationDetail"].clone()
}

/// The mode a described snapshot configuration reports is the legacy marker, pinned as the
/// WIRE STRING rather than read off the service's own constant: the recorded service reports
/// that marker for an application whose mode was never selected, so a change to the constant
/// is a failure here even though the table above would follow it.
#[tokio::test]
async fn the_reported_mode_is_the_legacy_marker_on_the_wire() {
    /// The snapshot mode of an application that never selected one, as the service spells it.
    const LEGACY_MODE: &str = "APPLICATION_SNAPSHOTS_LEGACY";

    let router = router();
    let name = "legacy-marker";
    let created = create_with(&router, LEGACY_MODE, name, "FLINK-1_20", None, None).await;
    assert_eq!(
        snapshot_description(&created)["SnapshotMode"].as_str(),
        Some(LEGACY_MODE),
        "expected the create to report the legacy snapshot mode, got {:?}",
        snapshot_description(&created)
    );
    let described = describe(&router, name).await;
    assert_eq!(
        snapshot_description(&described)["SnapshotMode"].as_str(),
        Some(LEGACY_MODE),
        "expected the describe to report the legacy snapshot mode, got {:?}",
        snapshot_description(&described)
    );
    assert_eq!(
        DEFAULT_SNAPSHOT_MODE, LEGACY_MODE,
        "expected the mode the rest of this suite reads to be the legacy marker, got \
         {DEFAULT_SNAPSHOT_MODE:?}"
    );
}

/// Every runtime this API creates applications for describes its snapshot configuration with
/// the mode in force, whatever its create said about snapshots — on the create's own answer
/// and on every later read of the same application.
#[tokio::test]
async fn every_described_snapshot_configuration_reports_the_mode_in_force() {
    // (what the create says about snapshots, the ApplicationConfiguration it carries,
    // whether snapshots then run for a Flink application, whether they run for a notebook)
    let configurations: [(&str, Option<Value>, bool, bool); 3] = [
        ("no configuration at all", None, true, false),
        (
            "snapshots switched on",
            Some(json!({"ApplicationSnapshotConfiguration": {"SnapshotsEnabled": true}})),
            true,
            true,
        ),
        (
            "snapshots switched off",
            Some(json!({"ApplicationSnapshotConfiguration": {"SnapshotsEnabled": false}})),
            false,
            false,
        ),
    ];

    let router = router();
    for runtime in RUNTIME_ENVIRONMENT_VALUES
        .iter()
        .filter(|runtime| creates_applications_on(runtime))
    {
        let notebook = runtime.starts_with(NOTEBOOK_FAMILY);
        for (index, (configured, configuration, flink_snapshots, notebook_snapshots)) in
            configurations.iter().enumerate()
        {
            let case = format!("{runtime} created with {configured}");
            let name = format!("snapshot-mode-{}-{index}", runtime.to_lowercase());
            let snapshots_enabled = if notebook {
                *notebook_snapshots
            } else {
                *flink_snapshots
            };

            let created =
                create_with(&router, &case, &name, runtime, None, configuration.clone()).await;
            assert_describes_mode(&case, "CreateApplication", &created, snapshots_enabled);

            let described = describe(&router, &name).await;
            assert_describes_mode(&case, "DescribeApplication", &described, snapshots_enabled);

            // A per-version read is served for the Flink family only, so the notebook rows
            // stop at the two describe-shaped answers above.
            if !notebook {
                let version = describe_version(&router, &case, &name, 1).await;
                assert_describes_mode(
                    &case,
                    "DescribeApplicationVersion",
                    &version,
                    snapshots_enabled,
                );
            }
        }
    }
}

/// The mode an application runs under is decided for the application, not for the request:
/// a create that NAMES its mode and one that leaves it to the runtime describe the same
/// snapshot mode.
#[tokio::test]
async fn the_mode_is_reported_however_the_application_mode_was_decided() {
    // (case, runtime, the ApplicationMode the create names, whether snapshots then run)
    let rows: [(&str, &str, Option<&str>, bool); 4] = [
        (
            "a streaming runtime naming its mode",
            "FLINK-1_20",
            Some("STREAMING"),
            true,
        ),
        (
            "a streaming runtime naming no mode",
            "FLINK-1_8",
            None,
            true,
        ),
        (
            "a notebook runtime naming its mode",
            "ZEPPELIN-FLINK-3_0",
            Some("INTERACTIVE"),
            false,
        ),
        (
            "a notebook runtime naming no mode",
            "ZEPPELIN-FLINK-3_0",
            None,
            false,
        ),
    ];

    let router = router();
    for (case, runtime, named_mode, snapshots_enabled) in rows {
        let name = format!("mode-{}", case.replace(' ', "-"));
        let created = create_with(&router, case, &name, runtime, named_mode, None).await;
        assert_describes_mode(case, "CreateApplication", &created, snapshots_enabled);
        let described = describe(&router, &name).await;
        assert_describes_mode(case, "DescribeApplication", &described, snapshots_enabled);
    }
}

/// An update carries the stored mode through, whatever it names: one that switches snapshots
/// the other way reports the same mode with the new switch, one that touches another
/// sub-configuration leaves both members as they stood — and the version the application has
/// LEFT keeps reporting the configuration it ran under.
#[tokio::test]
async fn an_update_carries_the_stored_mode_through() {
    // (case, the ApplicationConfigurationUpdate, whether snapshots run after it)
    let rows: [(&str, Value, bool); 3] = [
        (
            "an update that switches snapshots off",
            json!({"ApplicationSnapshotConfigurationUpdate": {"SnapshotsEnabledUpdate": false}}),
            false,
        ),
        (
            "an update that switches snapshots on",
            json!({"ApplicationSnapshotConfigurationUpdate": {"SnapshotsEnabledUpdate": true}}),
            true,
        ),
        (
            "an update that names another sub-configuration",
            json!({
                "EnvironmentPropertyUpdates": {
                    "PropertyGroups": [
                        {"PropertyGroupId": "producer", "PropertyMap": {"topic": "orders"}}
                    ]
                }
            }),
            true,
        ),
    ];

    let router = router();
    for (index, (case, configuration_update, snapshots_enabled)) in rows.into_iter().enumerate() {
        let name = format!("snapshot-mode-update-{index}");
        let created = create_with(&router, case, &name, "FLINK-1_19", None, None).await;
        assert_describes_mode(case, "CreateApplication", &created, true);

        let updated = update(&router, case, &name, 1, configuration_update).await;
        assert_describes_mode(case, "UpdateApplication", &updated, snapshots_enabled);

        let described = describe(&router, &name).await;
        assert_describes_mode(case, "DescribeApplication", &described, snapshots_enabled);

        let in_force = updated["ApplicationVersionId"]
            .as_i64()
            .unwrap_or_else(|| panic!("case {case:?}: expected a version id, got {updated}"));
        let current_version = describe_version(&router, case, &name, in_force).await;
        assert_describes_mode(
            case,
            "DescribeApplicationVersion on the version in force",
            &current_version,
            snapshots_enabled,
        );

        // The version the update left behind is reported as it RAN: snapshots on, since
        // every one of these applications was created with the streaming defaults.
        let superseded = describe_version(&router, case, &name, 1).await;
        assert_describes_mode(
            case,
            "DescribeApplicationVersion on the superseded version",
            &superseded,
            true,
        );
    }
}
