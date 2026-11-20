// SPDX-License-Identifier: CC0-1.0

//! The defaults an application is described under depend on the KIND of application the
//! service runs it as.
//!
//! The rule under test: `CheckpointingEnabled`, `SnapshotsEnabled` and `AutoScalingEnabled`
//! switch features of a Managed Service for Apache Flink application — the model documents
//! each as describing "whether checkpointing is enabled for a Managed Service for Apache
//! Flink application", "whether snapshots are enabled for a Managed Service for Apache Flink
//! application" and "whether the Managed Service for Apache Flink service can increase the
//! parallelism of the application in response to increased throughput"
//! (`smithy_query(query="doc",shape="CheckpointConfiguration",member="CheckpointingEnabled")`
//! / `shape="ApplicationSnapshotConfiguration",member="SnapshotsEnabled"` /
//! `shape="ParallelismConfiguration",member="AutoScalingEnabled"`) — while an
//! application the service runs in `INTERACTIVE` mode is a Studio notebook rather than such
//! an application (`CreateApplication$ApplicationMode`). So an application whose request
//! said nothing about any of them runs them only when it STREAMS, and every operation that
//! returns `ApplicationConfigurationDescription` reports the one stored answer.
//!
//! The rows span every `RuntimeEnvironment` the enum admits a create for, both ways the
//! mode is decided (named by the request, implied by the runtime), and a configuration that
//! switches each feature on explicitly.
//!
//! The same split decides WHICH sub-configurations a description carries at all: a
//! description reports the sub-configurations the application HAS, not one entry per
//! sub-configuration the model declares. The service completes a description with its own
//! values for the sub-configurations it APPLIES to that kind of application, and one it does
//! not apply to the kind is described only when the application's own create or update
//! supplied it. System rollback and encryption at rest configure a Managed Service for
//! Apache Flink application (`ApplicationSystemRollbackConfiguration`: "Describes the system
//! rollback configuration for a Managed Service for Apache Flink application"), so a Studio
//! notebook carries neither unless it was configured with one, while the sub-configurations
//! every kind runs under are described for both kinds.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_kinesisanalyticsv2_kinesisanalytics_20180523::ops::types::RUNTIME_ENVIRONMENT_VALUES;

use super::harness::{
    call, create_body, creates_applications_on, describe, router, CUSTOMER_KEY,
};

/// The `RuntimeEnvironment` family a Studio notebook runs on, as the enum spells it
/// (`smithy_query(query="shape",shape="RuntimeEnvironment")`: `ZEPPELIN-FLINK-<major>_<minor>`).
const NOTEBOOK_FAMILY: &str = "ZEPPELIN-";

/// Whether an application created on `runtime` is a Managed Service for Apache Flink
/// application rather than a Studio notebook: the KIND of application, which decides both
/// which defaults its switches run under and which sub-configurations its description
/// carries at all. Read off the runtime here because a create's `ApplicationMode` has to be
/// the one its runtime runs applications in — the other pairing is refused — so the Zeppelin
/// family is exactly the interactive kind.
fn is_flink_application(runtime: &str) -> bool {
    !runtime.starts_with(NOTEBOOK_FAMILY)
}

/// The `CheckpointingEnabled` of the described checkpoint configuration.
fn checkpointing_enabled(detail: &Value) -> Option<bool> {
    detail
        .pointer(
            "/ApplicationConfigurationDescription/FlinkApplicationConfigurationDescription\
             /CheckpointConfigurationDescription/CheckpointingEnabled",
        )
        .and_then(Value::as_bool)
}

/// The `SnapshotsEnabled` of the described snapshot configuration.
fn snapshots_enabled(detail: &Value) -> Option<bool> {
    detail
        .pointer(
            "/ApplicationConfigurationDescription\
             /ApplicationSnapshotConfigurationDescription/SnapshotsEnabled",
        )
        .and_then(Value::as_bool)
}

/// The `AutoScalingEnabled` of the described parallelism configuration.
fn auto_scaling_enabled(detail: &Value) -> Option<bool> {
    detail
        .pointer(
            "/ApplicationConfigurationDescription/FlinkApplicationConfigurationDescription\
             /ParallelismConfigurationDescription/AutoScalingEnabled",
        )
        .and_then(Value::as_bool)
}

/// One member of the described parallelism configuration, for the values that are the same
/// for every kind of application.
fn parallelism_member<'a>(detail: &'a Value, member: &str) -> &'a Value {
    &detail["ApplicationConfigurationDescription"]["FlinkApplicationConfigurationDescription"]
        ["ParallelismConfigurationDescription"][member]
}

/// One member of the described checkpoint configuration, for the values that are the same
/// for every kind of application.
fn checkpoint_member<'a>(detail: &'a Value, member: &str) -> &'a Value {
    &detail["ApplicationConfigurationDescription"]["FlinkApplicationConfigurationDescription"]
        ["CheckpointConfigurationDescription"][member]
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

/// Update one application with a configuration change that names NEITHER switch, and
/// answer the detail the update reported.
async fn update_unrelated_configuration(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    version: i64,
) -> Value {
    let (status, updated) = call(
        router,
        "UpdateApplication",
        &json!({
            "ApplicationName": name,
            "CurrentApplicationVersionId": version,
            "ApplicationConfigurationUpdate": {
                "EnvironmentPropertyUpdates": {
                    "PropertyGroups": [
                        {"PropertyGroupId": "producer", "PropertyMap": {"topic": "orders"}}
                    ]
                }
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected UpdateApplication to be accepted, got {status} {updated}"
    );
    updated["ApplicationDetail"].clone()
}

/// Every runtime this API still creates an application on, described with the defaults its
/// kind of application runs under — on create, on describe, and on an update that names
/// neither switch.
#[tokio::test]
async fn each_runtime_is_described_with_the_defaults_its_kind_of_application_runs() {
    let router = router();
    for runtime in RUNTIME_ENVIRONMENT_VALUES
        .iter()
        .filter(|runtime| creates_applications_on(runtime))
    {
        let case = format!("{runtime} with no configuration at all");
        let name = format!("defaults-{}", runtime.to_lowercase());
        let expected = is_flink_application(runtime);

        let created = create_with(&router, &case, &name, runtime, None, None).await;
        assert_eq!(
            checkpointing_enabled(&created),
            Some(expected),
            "case {case:?}: expected the create to report CheckpointingEnabled {expected}, \
             got {created}"
        );
        assert_eq!(
            snapshots_enabled(&created),
            Some(expected),
            "case {case:?}: expected the create to report SnapshotsEnabled {expected}, \
             got {created}"
        );
        // The rest of the DEFAULT checkpoint table is the same for every kind of
        // application: the two intervals the `ConfigurationType` doc publishes.
        for (member, value) in [
            ("ConfigurationType", json!("DEFAULT")),
            ("CheckpointInterval", json!(60_000)),
            ("MinPauseBetweenCheckpoints", json!(5_000)),
        ] {
            assert_eq!(
                checkpoint_member(&created, member),
                &value,
                "case {case:?}: expected {member} to be {value}, got {created}"
            );
        }

        let described = describe(&router, &name).await;
        assert_eq!(
            described, created,
            "case {case:?}: expected the describe to repeat the created detail, got {described}"
        );

        let updated = update_unrelated_configuration(&router, &case, &name, 1).await;
        assert_eq!(
            (checkpointing_enabled(&updated), snapshots_enabled(&updated)),
            (Some(expected), Some(expected)),
            "case {case:?}: expected an unrelated update to carry both switches through \
             unchanged, got {updated}"
        );
        let redescribed = describe(&router, &name).await;
        assert_eq!(
            (
                checkpointing_enabled(&redescribed),
                snapshots_enabled(&redescribed)
            ),
            (Some(expected), Some(expected)),
            "case {case:?}: expected the stored switches to survive the update, got \
             {redescribed}"
        );
    }
}

/// The mode the application RUNS in is what decides, whether the request named it or left
/// it to the runtime.
#[tokio::test]
async fn the_mode_the_application_runs_in_decides_the_defaults() {
    // (case, runtime, the ApplicationMode the create names, the mode the application runs
    // in, whether the two features run)
    let rows: [(&str, &str, Option<&str>, &str, bool); 4] = [
        (
            "a streaming runtime naming its mode",
            "FLINK-1_20",
            Some("STREAMING"),
            "STREAMING",
            true,
        ),
        (
            "a streaming runtime naming no mode",
            "FLINK-1_8",
            None,
            "STREAMING",
            true,
        ),
        (
            "a notebook runtime naming its mode",
            "ZEPPELIN-FLINK-3_0",
            Some("INTERACTIVE"),
            "INTERACTIVE",
            false,
        ),
        (
            "a notebook runtime naming no mode",
            "ZEPPELIN-FLINK-3_0",
            None,
            "INTERACTIVE",
            false,
        ),
    ];

    let router = router();
    for (case, runtime, named_mode, mode, expected) in rows {
        let name = format!("mode-{}", case.replace(' ', "-"));
        let created = create_with(&router, case, &name, runtime, named_mode, None).await;
        assert_eq!(
            created["ApplicationMode"].as_str(),
            Some(mode),
            "case {case:?}: expected the application to run in {mode}, got {created}"
        );
        assert_eq!(
            (checkpointing_enabled(&created), snapshots_enabled(&created)),
            (Some(expected), Some(expected)),
            "case {case:?}: expected both switches to be {expected}, got {created}"
        );
    }
}

/// A request that switches a feature on is reported as given, on every kind of
/// application: the per-kind values are DEFAULTS, not a ceiling.
#[tokio::test]
async fn a_request_that_switches_a_feature_on_is_reported_as_given() {
    // (case, runtime, the ApplicationConfiguration the create carries, expected
    // CheckpointingEnabled, expected SnapshotsEnabled)
    let rows: [(&str, &str, Value, bool, bool); 4] = [
        (
            "a notebook that enables snapshots",
            "ZEPPELIN-FLINK-3_0",
            json!({"ApplicationSnapshotConfiguration": {"SnapshotsEnabled": true}}),
            false,
            true,
        ),
        (
            "a notebook with a CUSTOM checkpoint configuration that enables checkpointing",
            "ZEPPELIN-FLINK-3_0",
            json!({
                "FlinkApplicationConfiguration": {
                    "CheckpointConfiguration": {
                        "ConfigurationType": "CUSTOM",
                        "CheckpointingEnabled": true
                    }
                }
            }),
            true,
            false,
        ),
        (
            "a streaming application that disables snapshots",
            "FLINK-1_19",
            json!({"ApplicationSnapshotConfiguration": {"SnapshotsEnabled": false}}),
            true,
            false,
        ),
        (
            "a streaming application with a CUSTOM checkpoint configuration that disables \
             checkpointing",
            "FLINK-2_2",
            json!({
                "FlinkApplicationConfiguration": {
                    "CheckpointConfiguration": {
                        "ConfigurationType": "CUSTOM",
                        "CheckpointingEnabled": false
                    }
                }
            }),
            false,
            true,
        ),
    ];

    let router = router();
    for (index, (case, runtime, configuration, checkpointing, snapshots)) in
        rows.into_iter().enumerate()
    {
        // The index keys the name, so two rows on one runtime are two applications.
        let name = format!("explicit-{}-{index}", runtime.to_lowercase());
        let created =
            create_with(&router, case, &name, runtime, None, Some(configuration)).await;
        assert_eq!(
            (checkpointing_enabled(&created), snapshots_enabled(&created)),
            (Some(checkpointing), Some(snapshots)),
            "case {case:?}: expected (CheckpointingEnabled, SnapshotsEnabled) to be \
             ({checkpointing}, {snapshots}), got {created}"
        );
        let described = describe(&router, &name).await;
        assert_eq!(
            (
                checkpointing_enabled(&described),
                snapshots_enabled(&described)
            ),
            (Some(checkpointing), Some(snapshots)),
            "case {case:?}: expected the describe to report the stored values, got {described}"
        );
    }
}

/// An update that names a switch moves THAT one and leaves the other at its stored value,
/// on an application whose defaults turned both off.
#[tokio::test]
async fn an_update_that_names_one_switch_moves_only_that_one() {
    let router = router();
    let name = "notebook-switches";
    let created = create_with(
        &router,
        "a notebook",
        name,
        "ZEPPELIN-FLINK-3_0",
        None,
        None,
    )
    .await;
    assert_eq!(
        (checkpointing_enabled(&created), snapshots_enabled(&created)),
        (Some(false), Some(false)),
        "expected a notebook to run neither feature, got {created}"
    );

    let (status, updated) = call(
        &router,
        "UpdateApplication",
        &json!({
            "ApplicationName": name,
            "CurrentApplicationVersionId": 1,
            "ApplicationConfigurationUpdate": {
                "ApplicationSnapshotConfigurationUpdate": {"SnapshotsEnabledUpdate": true}
            }
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected the update to be accepted, got {status} {updated}"
    );
    let detail = updated["ApplicationDetail"].clone();
    assert_eq!(
        snapshots_enabled(&detail),
        Some(true),
        "expected the update to enable snapshots, got {detail}"
    );
    assert_eq!(
        checkpointing_enabled(&detail),
        Some(false),
        "expected checkpointing to stay at the value the notebook runs under, got {detail}"
    );

    let described = describe(&router, name).await;
    assert_eq!(
        (snapshots_enabled(&described), checkpointing_enabled(&described)),
        (Some(true), Some(false)),
        "expected the describe to report what the update stored, got {described}"
    );
}

/// Whether the service may scale an application out is defaulted the same way: on for a
/// Managed Service for Apache Flink application, off for a Studio notebook — while the rest
/// of the `DEFAULT` parallelism table is the same for every kind of application.
#[tokio::test]
async fn each_runtime_is_described_with_the_autoscaling_its_kind_of_application_runs() {
    let router = router();
    for runtime in RUNTIME_ENVIRONMENT_VALUES
        .iter()
        .filter(|runtime| creates_applications_on(runtime))
    {
        let case = format!("{runtime} with no parallelism configuration");
        let name = format!("autoscaling-{}", runtime.to_lowercase());
        let expected = is_flink_application(runtime);

        let created = create_with(&router, &case, &name, runtime, None, None).await;
        assert_eq!(
            auto_scaling_enabled(&created),
            Some(expected),
            "case {case:?}: expected the create to report AutoScalingEnabled {expected}, \
             got {created}"
        );
        for (member, value) in [
            ("ConfigurationType", json!("DEFAULT")),
            ("Parallelism", json!(1)),
            ("ParallelismPerKPU", json!(1)),
            ("CurrentParallelism", json!(1)),
        ] {
            assert_eq!(
                parallelism_member(&created, member),
                &value,
                "case {case:?}: expected {member} to be {value}, got {created}"
            );
        }

        let described = describe(&router, &name).await;
        assert_eq!(
            auto_scaling_enabled(&described),
            Some(expected),
            "case {case:?}: expected the describe to report the stored value, got {described}"
        );

        let updated = update_unrelated_configuration(&router, &case, &name, 1).await;
        assert_eq!(
            auto_scaling_enabled(&updated),
            Some(expected),
            "case {case:?}: expected an unrelated update to carry the switch through \
             unchanged, got {updated}"
        );
    }
}

/// A request that names `AutoScalingEnabled` in a `CUSTOM` parallelism configuration is
/// reported as given, on every kind of application: the per-kind value is a DEFAULT, not a
/// ceiling — and a configuration marked `DEFAULT` reports the service's table whatever it
/// carried, which is what that `ConfigurationType` means.
#[tokio::test]
async fn a_request_that_names_autoscaling_is_reported_as_given() {
    // (case, runtime, the ParallelismConfiguration the create carries, expected
    // AutoScalingEnabled)
    let rows: [(&str, &str, Value, bool); 5] = [
        (
            "a notebook that switches autoscaling on",
            "ZEPPELIN-FLINK-3_0",
            json!({"ConfigurationType": "CUSTOM", "AutoScalingEnabled": true}),
            true,
        ),
        (
            "a notebook with a CUSTOM configuration that names only the parallelism",
            "ZEPPELIN-FLINK-3_0",
            json!({"ConfigurationType": "CUSTOM", "Parallelism": 2}),
            false,
        ),
        (
            "a notebook whose DEFAULT configuration asks for autoscaling",
            "ZEPPELIN-FLINK-3_0",
            json!({"ConfigurationType": "DEFAULT", "AutoScalingEnabled": true}),
            false,
        ),
        (
            "a streaming application that switches autoscaling off",
            "FLINK-1_19",
            json!({"ConfigurationType": "CUSTOM", "AutoScalingEnabled": false}),
            false,
        ),
        (
            "a streaming application with a CUSTOM configuration that names only the \
             parallelism",
            "FLINK-2_2",
            json!({"ConfigurationType": "CUSTOM", "ParallelismPerKPU": 2}),
            true,
        ),
    ];

    let router = router();
    for (index, (case, runtime, parallelism, expected)) in rows.into_iter().enumerate() {
        let name = format!("autoscaling-named-{index}");
        let configuration = json!({"FlinkApplicationConfiguration": {
            "ParallelismConfiguration": parallelism
        }});
        let created =
            create_with(&router, case, &name, runtime, None, Some(configuration)).await;
        assert_eq!(
            auto_scaling_enabled(&created),
            Some(expected),
            "case {case:?}: expected AutoScalingEnabled to be {expected}, got {created}"
        );
        let described = describe(&router, &name).await;
        assert_eq!(
            auto_scaling_enabled(&described),
            Some(expected),
            "case {case:?}: expected the describe to report the stored value, got {described}"
        );
    }
}

// ---------------------------------------------------------------------------
// WHICH sub-configurations a description carries
// ---------------------------------------------------------------------------

/// The two description blocks whose PRESENCE is keyed on the kind of application, spelled as
/// `ApplicationConfigurationDescription` declares them
/// (`smithy_query(query="shape",shape="ApplicationConfigurationDescription")`).
const ROLLBACK: &str = "ApplicationSystemRollbackConfigurationDescription";
const ENCRYPTION: &str = "ApplicationEncryptionConfigurationDescription";

/// The `ApplicationConfigurationDescription` of one `ApplicationDetail`.
fn described_configuration(detail: &Value) -> Value {
    detail["ApplicationConfigurationDescription"].clone()
}

/// One configuration form a create can carry: the whole `ApplicationConfiguration`, plus
/// what it supplies for the two blocks whose presence is under test.
struct PresenceCase {
    form: &'static str,
    /// The `ApplicationConfiguration` the create carries, or `None` for none at all.
    configuration: Option<Value>,
    /// The `ApplicationSystemRollbackConfiguration` the create supplied, if any.
    rollback: Option<Value>,
    /// The `ApplicationEncryptionConfiguration` the create supplied, if any.
    encryption: Option<Value>,
}

/// The configuration forms every runtime is driven through: no `ApplicationConfiguration` at
/// all, one carrying only sub-configurations other than the two under test, one carrying just
/// one of them, and one carrying both — with both values `RollbackEnabled` takes and both
/// `KeyType` values (`smithy_query(query="shape",shape="KeyType")`: AWS_OWNED_KEY |
/// CUSTOMER_MANAGED_KEY), so what is asserted is which blocks the application HAS rather than
/// one supplied value.
fn presence_cases() -> Vec<PresenceCase> {
    vec![
        PresenceCase {
            form: "no application configuration at all",
            configuration: None,
            rollback: None,
            encryption: None,
        },
        PresenceCase {
            form: "other sub-configurations only",
            configuration: Some(json!({
                "ApplicationSnapshotConfiguration": {"SnapshotsEnabled": true},
                "FlinkApplicationConfiguration": {
                    "MonitoringConfiguration": {
                        "ConfigurationType": "CUSTOM",
                        "LogLevel": "DEBUG",
                        "MetricsLevel": "TASK"
                    }
                }
            })),
            rollback: None,
            encryption: None,
        },
        PresenceCase {
            form: "system rollback alone, turned off",
            configuration: Some(json!({
                "ApplicationSystemRollbackConfiguration": {"RollbackEnabled": false}
            })),
            rollback: Some(json!({"RollbackEnabled": false})),
            encryption: None,
        },
        PresenceCase {
            form: "encryption alone, under the AWS-owned key",
            configuration: Some(json!({
                "ApplicationEncryptionConfiguration": {"KeyType": "AWS_OWNED_KEY"}
            })),
            rollback: None,
            encryption: Some(json!({"KeyType": "AWS_OWNED_KEY"})),
        },
        PresenceCase {
            form: "both of them, rollback on and a key of the caller's",
            configuration: Some(json!({
                "ApplicationSystemRollbackConfiguration": {"RollbackEnabled": true},
                "ApplicationEncryptionConfiguration": {
                    "KeyType": "CUSTOMER_MANAGED_KEY",
                    "KeyId": CUSTOMER_KEY
                }
            })),
            rollback: Some(json!({"RollbackEnabled": true})),
            encryption: Some(json!({
                "KeyType": "CUSTOMER_MANAGED_KEY",
                "KeyId": CUSTOMER_KEY
            })),
        },
    ]
}

/// The system-rollback block an application of this kind must be described with when its
/// configuration supplied `supplied`: the supplied one, else the service's own default for a
/// Managed Service for Apache Flink application — it rolls one back only when asked — else
/// nothing at all for a Studio notebook, which the service does not roll back.
fn expected_rollback(is_flink_application: bool, supplied: Option<&Value>) -> Option<Value> {
    supplied
        .cloned()
        .or_else(|| is_flink_application.then(|| json!({"RollbackEnabled": false})))
}

/// The encryption block an application of this kind must be described with when its
/// configuration supplied `supplied`: the supplied one, else the AWS-owned key a Managed
/// Service for Apache Flink application with no key of its own is encrypted under — reported
/// with no `KeyId`, since the caller named none — else nothing at all for a Studio notebook.
fn expected_encryption(is_flink_application: bool, supplied: Option<&Value>) -> Option<Value> {
    supplied
        .cloned()
        .or_else(|| is_flink_application.then(|| json!({"KeyType": "AWS_OWNED_KEY"})))
}

/// A created application is described with the sub-configurations it HAS: the ones the
/// service applies to its kind of application, plus the ones its own configuration supplied
/// — and with no block it never had.
#[tokio::test]
async fn each_kind_describes_only_the_sub_configurations_it_has() {
    let router = router();
    for runtime in RUNTIME_ENVIRONMENT_VALUES
        .iter()
        .filter(|runtime| creates_applications_on(runtime))
    {
        let applies = is_flink_application(runtime);
        for (index, supplied) in presence_cases().into_iter().enumerate() {
            let case = format!("{runtime} created with {}", supplied.form);
            let name = format!("blocks-{}-{index}", runtime.to_lowercase());
            let created = create_with(
                &router,
                &case,
                &name,
                runtime,
                None,
                supplied.configuration.clone(),
            )
            .await;
            let config = described_configuration(&created);

            let rollback = expected_rollback(applies, supplied.rollback.as_ref());
            assert_eq!(
                config.get(ROLLBACK),
                rollback.as_ref(),
                "case {case:?}: expected {ROLLBACK} to be {rollback:?}, got {config}"
            );
            let encryption = expected_encryption(applies, supplied.encryption.as_ref());
            assert_eq!(
                config.get(ENCRYPTION),
                encryption.as_ref(),
                "case {case:?}: expected {ENCRYPTION} to be {encryption:?}, got {config}"
            );

            // The sub-configurations EVERY kind of application runs under are still
            // described for both kinds, so the rule is about which blocks the application
            // has and not about describing less for a notebook.
            for always in [
                "FlinkApplicationConfigurationDescription",
                "ApplicationSnapshotConfigurationDescription",
            ] {
                assert!(
                    config[always].is_object(),
                    "case {case:?}: expected {always} to be described, got {config}"
                );
            }

            // What a later reader sees is what was stored: a read neither invents a block
            // nor drops one, however often it is called.
            let described = described_configuration(&describe(&router, &name).await);
            assert_eq!(
                described, config,
                "case {case:?}: expected the describe to repeat the created description, \
                 got {described}"
            );
            let reread = described_configuration(&describe(&router, &name).await);
            assert_eq!(
                reread, described,
                "case {case:?}: expected a second read to report the same description, \
                 got {reread}"
            );
        }
    }
}

/// A block an application did not have at create time and then CONFIGURED by an update is
/// described from then on — on either kind of application — while one it still has not
/// configured stays absent for a Studio notebook and keeps the service's own values for a
/// Managed Service for Apache Flink application.
#[tokio::test]
async fn an_update_describes_the_block_it_configures_and_invents_no_other() {
    let router = router();
    for runtime in RUNTIME_ENVIRONMENT_VALUES
        .iter()
        .filter(|runtime| creates_applications_on(runtime))
    {
        let applies = is_flink_application(runtime);
        let name = format!("configured-blocks-{}", runtime.to_lowercase());

        // Nothing was configured, so a notebook has neither block and a Managed Service for
        // Apache Flink application has both, at the service's own values.
        let created = create_with(&router, runtime, &name, runtime, None, None).await;
        let config = described_configuration(&created);
        let unconfigured_rollback = expected_rollback(applies, None);
        let unconfigured_encryption = expected_encryption(applies, None);
        assert_eq!(
            config.get(ROLLBACK),
            unconfigured_rollback.as_ref(),
            "case {runtime}/create: expected {ROLLBACK} to be {unconfigured_rollback:?}, \
             got {config}"
        );
        assert_eq!(
            config.get(ENCRYPTION),
            unconfigured_encryption.as_ref(),
            "case {runtime}/create: expected {ENCRYPTION} to be {unconfigured_encryption:?}, \
             got {config}"
        );

        // An update that configures system rollback: the application HAS one from now on,
        // so every later read describes it, whichever kind it is.
        let (status, updated) = call(
            &router,
            "UpdateApplication",
            &json!({
                "ApplicationName": name,
                "CurrentApplicationVersionId": 1,
                "ApplicationConfigurationUpdate": {
                    "ApplicationSystemRollbackConfigurationUpdate": {
                        "RollbackEnabledUpdate": true
                    }
                }
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {runtime}/update: expected UpdateApplication to be accepted, \
             got {status} {updated}"
        );
        let config = described_configuration(&updated["ApplicationDetail"]);
        assert_eq!(
            config.get(ROLLBACK),
            Some(&json!({"RollbackEnabled": true})),
            "case {runtime}/update: expected the configured rollback to be described, \
             got {config}"
        );
        assert_eq!(
            config.get(ENCRYPTION),
            unconfigured_encryption.as_ref(),
            "case {runtime}/update: expected {ENCRYPTION} to be {unconfigured_encryption:?}, \
             got {config}"
        );

        // An update about something else configures neither block: the rollback the
        // application has keeps its value, and the block it still has not is not invented.
        let updated = update_unrelated_configuration(&router, runtime, &name, 2).await;
        let config = described_configuration(&updated);
        assert_eq!(
            config.get(ROLLBACK),
            Some(&json!({"RollbackEnabled": true})),
            "case {runtime}/unrelated update: expected the rollback the application has, \
             got {config}"
        );
        assert_eq!(
            config.get(ENCRYPTION),
            unconfigured_encryption.as_ref(),
            "case {runtime}/unrelated update: expected {ENCRYPTION} to be \
             {unconfigured_encryption:?}, got {config}"
        );

        let described = described_configuration(&describe(&router, &name).await);
        assert_eq!(
            described, config,
            "case {runtime}: expected the describe to repeat the updated description, \
             got {described}"
        );
    }
}
