// SPDX-License-Identifier: CC0-1.0

//! What an accepted `UpdateApplication` does to the stored application's VERSION, and what
//! a later read then reports.
//!
//! The rule under test: every accepted update of an application mints the NEXT application
//! version — `ApplicationDetail$ApplicationVersionId` is documented as "the current
//! application version. Managed Service for Apache Flink updates the ApplicationVersionId
//! each time you update the application" and
//! `ApplicationDetail$ApplicationVersionUpdatedFrom` as "the previous application version
//! before the latest application update"
//! (`smithy_query(query="shape",shape="ApplicationDetail")`) — applies the updates the
//! request carries to the stored configuration, and re-mints the `ConditionalToken`, which
//! names the version now in force
//! (`UpdateApplication$ConditionalToken`: "you get the application's current
//! ConditionalToken using DescribeApplication"). An update that is REFUSED moves none of
//! them.
//!
//! The rows span the KINDS of accepted update — a feature switch, each of the three Flink
//! sub-configurations (one put at `CUSTOM` and one put back at `DEFAULT`), the execution
//! property groups, the service execution role and the runtime environment — driven as a
//! CHAIN, so what is asserted is that the version advances by exactly one per accepted
//! update however many of them a client makes, and over both kinds of application this API
//! runs (a Managed Service for Apache Flink application and a Studio notebook). Every call
//! goes over the wire through the same `MultiServiceRouter` the emulator boots.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use super::application_mode_and_maintenance_window::token_of;
use super::harness::{call, create_body, describe, error_code, router};

/// The applications the chain is driven against: both kinds this API creates, each with the
/// runtime a later step of the chain moves it to — which for a Studio notebook is the
/// generation it already runs on, the only one this API puts a notebook on
/// (`modeled_client_errors.rs` asserts the withdrawn generations are refused).
///
/// Columns: (case, the runtime it is created on, the runtime the chain's last step moves it
/// to).
const CHAINS: [(&str, &str, &str); 2] = [
    ("flink", "FLINK-1_15", "FLINK-1_20"),
    ("notebook", "ZEPPELIN-FLINK-3_0", "ZEPPELIN-FLINK-3_0"),
];

/// Where the described Flink configuration lives in an `ApplicationDetail`.
const FLINK: &str =
    "/ApplicationConfigurationDescription/FlinkApplicationConfigurationDescription";

/// Where the described snapshot switch lives in an `ApplicationDetail`.
const SNAPSHOTS: &str = "/ApplicationConfigurationDescription\
                         /ApplicationSnapshotConfigurationDescription/SnapshotsEnabled";

/// Where the described execution property groups live in an `ApplicationDetail`.
const PROPERTIES: &str =
    "/ApplicationConfigurationDescription/EnvironmentPropertyDescriptions\
     /PropertyGroupDescriptions";

/// One accepted update of the chain: the members it carries besides the one naming the
/// version in force, and what a read must report once it has landed, as (JSON pointer into
/// `ApplicationDetail`, value) pairs.
struct Accepted {
    case: &'static str,
    update: Value,
    reported: Vec<(String, Value)>,
}

/// The chain of accepted updates, in the order it is driven: each row changes a DIFFERENT
/// part of the application, so the version advance is asserted for an accepted update rather
/// than for one shape of request, and the expectations of the earlier rows are re-asserted
/// after every later one (an update replaces what it names and leaves the rest standing).
///
/// `runtime_update` is the runtime the last row moves the application to.
fn chain(runtime_update: &str) -> Vec<Accepted> {
    vec![
        Accepted {
            case: "snapshots-on",
            update: json!({
                "ApplicationConfigurationUpdate": {
                    "ApplicationSnapshotConfigurationUpdate": {"SnapshotsEnabledUpdate": true},
                },
            }),
            reported: vec![(SNAPSHOTS.to_string(), json!(true))],
        },
        Accepted {
            case: "monitoring-custom",
            update: json!({
                "ApplicationConfigurationUpdate": {
                    "FlinkApplicationConfigurationUpdate": {
                        "MonitoringConfigurationUpdate": {
                            "ConfigurationTypeUpdate": "CUSTOM",
                            "LogLevelUpdate": "DEBUG",
                            "MetricsLevelUpdate": "TASK",
                        },
                    },
                },
            }),
            reported: vec![
                (
                    format!("{FLINK}/MonitoringConfigurationDescription/ConfigurationType"),
                    json!("CUSTOM"),
                ),
                (
                    format!("{FLINK}/MonitoringConfigurationDescription/LogLevel"),
                    json!("DEBUG"),
                ),
                (
                    format!("{FLINK}/MonitoringConfigurationDescription/MetricsLevel"),
                    json!("TASK"),
                ),
            ],
        },
        Accepted {
            case: "checkpoints-custom",
            update: json!({
                "ApplicationConfigurationUpdate": {
                    "FlinkApplicationConfigurationUpdate": {
                        "CheckpointConfigurationUpdate": {
                            "ConfigurationTypeUpdate": "CUSTOM",
                            "CheckpointIntervalUpdate": 120_000,
                            "MinPauseBetweenCheckpointsUpdate": 10_000,
                        },
                    },
                },
            }),
            reported: vec![
                (
                    format!("{FLINK}/CheckpointConfigurationDescription/ConfigurationType"),
                    json!("CUSTOM"),
                ),
                (
                    format!("{FLINK}/CheckpointConfigurationDescription/CheckpointInterval"),
                    json!(120_000),
                ),
                (
                    format!(
                        "{FLINK}/CheckpointConfigurationDescription\
                         /MinPauseBetweenCheckpoints"
                    ),
                    json!(10_000),
                ),
            ],
        },
        Accepted {
            // A sub-configuration put BACK at `DEFAULT` runs on the service's own values
            // again, whatever the caller set while it was `CUSTOM`: the
            // `CheckpointConfigurationUpdate$ConfigurationTypeUpdate` doc states them —
            // "if this value is set to DEFAULT, the application will use the following
            // values, even if they are set to other values using APIs or application code:
            // CheckpointingEnabled true, CheckpointInterval 60000,
            // MinPauseBetweenCheckpoints 5000".
            case: "checkpoints-reverted",
            update: json!({
                "ApplicationConfigurationUpdate": {
                    "FlinkApplicationConfigurationUpdate": {
                        "CheckpointConfigurationUpdate": {"ConfigurationTypeUpdate": "DEFAULT"},
                    },
                },
            }),
            reported: vec![
                (
                    format!("{FLINK}/CheckpointConfigurationDescription/ConfigurationType"),
                    json!("DEFAULT"),
                ),
                (
                    format!("{FLINK}/CheckpointConfigurationDescription/CheckpointInterval"),
                    json!(60_000),
                ),
                (
                    format!(
                        "{FLINK}/CheckpointConfigurationDescription\
                         /MinPauseBetweenCheckpoints"
                    ),
                    json!(5_000),
                ),
            ],
        },
        Accepted {
            case: "parallelism-custom",
            update: json!({
                "ApplicationConfigurationUpdate": {
                    "FlinkApplicationConfigurationUpdate": {
                        "ParallelismConfigurationUpdate": {
                            "ConfigurationTypeUpdate": "CUSTOM",
                            "ParallelismUpdate": 4,
                            "ParallelismPerKPUUpdate": 2,
                            "AutoScalingEnabledUpdate": false,
                        },
                    },
                },
            }),
            reported: vec![
                (
                    format!("{FLINK}/ParallelismConfigurationDescription/ConfigurationType"),
                    json!("CUSTOM"),
                ),
                (
                    format!("{FLINK}/ParallelismConfigurationDescription/Parallelism"),
                    json!(4),
                ),
                (
                    format!("{FLINK}/ParallelismConfigurationDescription/ParallelismPerKPU"),
                    json!(2),
                ),
                (
                    format!("{FLINK}/ParallelismConfigurationDescription/AutoScalingEnabled"),
                    json!(false),
                ),
            ],
        },
        Accepted {
            case: "environment-properties",
            update: json!({
                "ApplicationConfigurationUpdate": {
                    "EnvironmentPropertyUpdates": {
                        "PropertyGroups": [{
                            "PropertyGroupId": "producer.config",
                            "PropertyMap": {"flink.stream.initpos": "LATEST"},
                        }],
                    },
                },
            }),
            reported: vec![
                (
                    format!("{PROPERTIES}/0/PropertyGroupId"),
                    json!("producer.config"),
                ),
                (
                    format!("{PROPERTIES}/0/PropertyMap/flink.stream.initpos"),
                    json!("LATEST"),
                ),
            ],
        },
        Accepted {
            case: "execution-role",
            update: json!({
                "ServiceExecutionRoleUpdate": "arn:aws:iam::000000000000:role/kda-next-role",
            }),
            reported: vec![(
                "/ServiceExecutionRole".to_string(),
                json!("arn:aws:iam::000000000000:role/kda-next-role"),
            )],
        },
        Accepted {
            case: "runtime-environment",
            update: json!({"RuntimeEnvironmentUpdate": runtime_update}),
            reported: vec![("/RuntimeEnvironment".to_string(), json!(runtime_update))],
        },
    ]
}

/// Every accepted update mints the next application version, records the version it moved
/// on from, applies what it carried to the stored configuration and re-mints the token — and
/// the reader reports all of it.
#[tokio::test]
async fn every_accepted_update_mints_the_next_version_and_applies_it() {
    for (chain_case, runtime, runtime_update) in CHAINS {
        let router = router();
        let name = "version-advance";
        let request = create_body(name, runtime);
        let (status, created) = call(&router, "CreateApplication", &request).await;
        assert_eq!(
            status, 200,
            "case {chain_case:?}: CreateApplication got {status} {created}"
        );
        let created = &created["ApplicationDetail"];
        assert_eq!(
            created["ApplicationVersionId"].as_i64(),
            Some(1),
            "case {chain_case:?}: expected a created application to be on its first \
             version, got {created}"
        );
        assert_eq!(
            created.get("ApplicationVersionUpdatedFrom"),
            None,
            "case {chain_case:?}: expected no previous version before the first update, \
             got {created}"
        );

        // Every token this application has been named by, so no accepted update can hand
        // back one that named an earlier version.
        let mut tokens = vec![token_of(created, chain_case)];
        // What a read must report, accumulated over the chain: a later row REPLACES an
        // earlier expectation at the same path (the checkpoint configuration, put at
        // `CUSTOM` and then back at `DEFAULT`) and leaves every other one standing.
        let mut reported: BTreeMap<String, Value> = BTreeMap::new();
        let updates = chain(runtime_update);
        let accepted_updates = updates.len() as i64;

        for (step, accepted) in updates.into_iter().enumerate() {
            let case = format!("{chain_case}/{}", accepted.case);
            // The version in force before this update: the create minted 1, and every
            // accepted update since minted the next one.
            let in_force = step as i64 + 1;
            let mut request = accepted.update;
            request["ApplicationName"] = json!(name);
            // The two members an update addresses a version by — "you must provide the
            // CurrentApplicationVersionId or the ConditionalToken" — used in turn, so the
            // advance is asserted for both ways of naming the version in force. A Studio
            // notebook's version is addressed by its ID alone
            // (`cloud_watch_logging_options.rs` asserts that rule), so the notebook chain
            // names the ID throughout.
            if step % 2 == 0 || runtime.starts_with("ZEPPELIN-") {
                request["CurrentApplicationVersionId"] = json!(in_force);
            } else {
                request["ConditionalToken"] = json!(tokens
                    .last()
                    .expect("the create minted the first token")
                    .clone());
            }

            let (status, answered) = call(&router, "UpdateApplication", &request).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected {request} to be accepted, got {status} {answered}"
            );
            let updated = answered["ApplicationDetail"].clone();
            assert_eq!(
                updated["ApplicationVersionId"].as_i64(),
                Some(in_force + 1),
                "case {case:?}: expected the update to mint version {}, got {updated}",
                in_force + 1
            );
            assert_eq!(
                updated["ApplicationVersionUpdatedFrom"].as_i64(),
                Some(in_force),
                "case {case:?}: expected the version the update moved on from to be \
                 {in_force}, got {updated}"
            );
            let minted = token_of(&updated, &case);
            assert!(
                !tokens.contains(&minted),
                "case {case:?}: expected a token naming the new version, but {minted:?} \
                 already named an earlier one, got {updated}"
            );
            tokens.push(minted);

            // The detail the write echoed IS the stored one, so a client that polls the
            // reader until its change is visible sees it on the next call.
            let described = describe(&router, name).await;
            assert_eq!(
                described, updated,
                "case {case:?}: expected the echoed detail to be the stored one, describe \
                 reported {described}"
            );

            reported.extend(accepted.reported);
            for (path, value) in &reported {
                assert_eq!(
                    described.pointer(path),
                    Some(value),
                    "case {case:?}: expected {path} to report {value}, got {described}"
                );
            }
        }

        // One version per accepted update, over the whole chain.
        let described = describe(&router, name).await;
        assert_eq!(
            described["ApplicationVersionId"].as_i64(),
            Some(accepted_updates + 1),
            "case {chain_case:?}: expected {accepted_updates} accepted updates to leave \
             the application on version {}, got {described}",
            accepted_updates + 1
        );
        assert_eq!(
            described["ApplicationVersionUpdatedFrom"].as_i64(),
            Some(accepted_updates),
            "case {chain_case:?}: expected the last superseded version to be \
             {accepted_updates}, got {described}"
        );
    }
}

/// An update that is REFUSED mints no version: the version in force, the version it moved
/// on from, the token naming it and the whole stored configuration are exactly as they were.
#[tokio::test]
async fn a_refused_update_mints_no_version() {
    for (chain_case, runtime, _) in CHAINS {
        let router = router();
        let name = "version-refused";
        let created = create_body(name, runtime);
        let (status, created) = call(&router, "CreateApplication", &created).await;
        assert_eq!(
            status, 200,
            "case {chain_case:?}: CreateApplication got {status} {created}"
        );

        // One accepted update first, so there IS a superseded version and a consumed
        // version id for the refusals below to leave alone.
        let (status, accepted) = call(
            &router,
            "UpdateApplication",
            &json!({
                "ApplicationName": name,
                "CurrentApplicationVersionId": 1,
                "ApplicationConfigurationUpdate": {
                    "ApplicationSnapshotConfigurationUpdate": {"SnapshotsEnabledUpdate": true},
                },
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {chain_case:?}: expected the first update to be accepted, got \
             {status} {accepted}"
        );
        let before = describe(&router, name).await;
        assert_eq!(
            (
                before["ApplicationVersionId"].as_i64(),
                before["ApplicationVersionUpdatedFrom"].as_i64()
            ),
            (Some(2), Some(1)),
            "case {chain_case:?}: expected the accepted update to leave version 2, \
             superseding 1, got {before}"
        );
        let in_force = token_of(&before, chain_case);
        // A Studio notebook's version is addressed by `CurrentApplicationVersionId` alone, so
        // a token addresses no version there at all and the refusal is about the member that
        // is missing rather than about an address that is stale
        // (`cloud_watch_logging_options.rs` asserts that rule).
        let token_refusal = if runtime.starts_with("ZEPPELIN-") {
            "InvalidArgumentException"
        } else {
            "ConcurrentModificationException"
        };

        // Each row breaks one requirement of an accepted update and names the modelled
        // error that requirement is refused with: a version other than the one in force is
        // `ConcurrentModificationException` ("attempting to modify an application without
        // using the current application ID"), while an input the operation cannot act on at
        // all — neither member naming a version, or an element of the configuration the
        // application does not hold — is `InvalidArgumentException`. Both travel with this
        // service's client-error status, whatever `@httpError` the shape carries
        // (`modeled_client_errors.rs` asserts that). Codes from
        // `smithy_query(query="operation",operation="UpdateApplication")`.
        let refusals: [(&str, Value, u16, &str); 5] = [
            (
                "a consumed version id",
                json!({"ApplicationName": name, "CurrentApplicationVersionId": 1}),
                400,
                "ConcurrentModificationException",
            ),
            (
                "a version id no version carried",
                json!({"ApplicationName": name, "CurrentApplicationVersionId": 999}),
                400,
                "ConcurrentModificationException",
            ),
            (
                "a token naming no version",
                json!({
                    "ApplicationName": name,
                    "ConditionalToken": "not-the-token-in-force",
                }),
                400,
                token_refusal,
            ),
            (
                "neither member",
                json!({"ApplicationName": name}),
                400,
                "InvalidArgumentException",
            ),
            (
                "an element the application does not hold",
                json!({
                    "ApplicationName": name,
                    "CurrentApplicationVersionId": 2,
                    "ApplicationConfigurationUpdate": {
                        "VpcConfigurationUpdates": [{
                            "VpcConfigurationId": "no-such-vpc-configuration",
                            "SubnetIdUpdates": ["subnet-0123456789abcdef0"],
                        }],
                    },
                }),
                400,
                "InvalidArgumentException",
            ),
        ];

        for (refused, refusal, expected_status, expected_code) in refusals {
            let case = format!("{chain_case}/{refused}");
            let (status, answered) = call(&router, "UpdateApplication", &refusal).await;
            assert_eq!(
                (status, error_code(&answered).as_str()),
                (expected_status, expected_code),
                "case {case:?}: expected {refusal} to be refused with \
                 {expected_status} {expected_code}, got {status} {answered}"
            );
            let after = describe(&router, name).await;
            assert_eq!(
                after, before,
                "case {case:?}: expected the refused update to leave the application \
                 exactly as it was, got {after}"
            );
            assert_eq!(
                token_of(&after, chain_case),
                in_force,
                "case {case:?}: expected the token in force to be untouched, got {after}"
            );
        }
    }
}
