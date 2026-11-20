// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CONFIGURATION a read of a workgroup reports: the whole set of
//! settings the workgroup has in effect, not only the members a create happened to name.
//!
//! The rule: `GetWorkGroup` serves a workgroup's effective configuration — one
//! `SystemTableStatuses` entry per system table Athena publishes, with whether this
//! workgroup has it enabled; the `QuerySchedulingType` the workgroup schedules queries by;
//! and, for a create that carried no `Configuration` at all, Athena's own default settings
//! rather than nothing configured. A read derives all of it from the stored record, so two
//! reads of one workgroup report one configuration.
//!
//! Driven over the wire (awsJson1_1, `x-amz-target: AmazonAthena.<Operation>`) through the
//! same router the emulator boots, so every assertion is about what an SDK client observes.
//! Each test states the rule and asserts it over a TABLE of inputs whose rows differ along
//! what the rule generalises over — whether the create carried a configuration at all, what
//! it carried, how the workgroup was made (created, updated, or Athena's own default
//! workgroup) — rather than over one recorded request.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The wire harness is shared by the suites of this test binary.
use crate::harness::{call, router as build_router};

/// The system tables a read reports a per-workgroup status for, and the one table AWS
/// serves on every workgroup. RECORDED BEHAVIOUR: the on-disk model declares no
/// `SystemTableStatuses` member, so the catalog is this service's own
/// (`crate::state::SYSTEM_TABLES`) and is written out here rather than read from the model.
const SYSTEM_TABLES: [&str; 1] = ["QUERY_EXECUTIONS"];

/// How a workgroup nothing else selected for schedules the queries submitted to it.
/// Recorded behaviour as well, and spelled out here so the expectation does not move with
/// the code under test.
const QUERY_SCHEDULING_TYPE: &str = "DEFAULT";

/// The `Configuration` `GetWorkGroup` reports for `name`.
async fn configuration(router: &MultiServiceRouter, name: &str) -> Value {
    let (status, body) = call(router, "GetWorkGroup", &json!({ "WorkGroup": name })).await;
    assert_eq!(
        status, 200,
        "GetWorkGroup({name}) must answer 200, got {status} with {body:?}"
    );
    body["WorkGroup"]["Configuration"].clone()
}

/// Assert the configuration `read` carries one status per system table of [`SYSTEM_TABLES`],
/// in that order, each enabled exactly as `enabled` says, and the scheduling type.
fn assert_reported_tables(case: &str, read: &Value, enabled: bool) {
    let statuses = read["SystemTableStatuses"]
        .as_array()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a SystemTableStatuses list, got {read:?}")
        })
        .clone();
    assert_eq!(
        statuses.len(),
        SYSTEM_TABLES.len(),
        "case {case:?}: expected one entry per system table, got {statuses:?}"
    );
    for (status, table) in statuses.iter().zip(SYSTEM_TABLES) {
        assert_eq!(
            status,
            &json!({ "TableName": table, "Enabled": enabled }),
            "case {case:?}: expected {table} reported with Enabled {enabled}, got {status:?}"
        );
    }
    assert_eq!(
        read["QuerySchedulingType"],
        json!(QUERY_SCHEDULING_TYPE),
        "case {case:?}: expected QuerySchedulingType {QUERY_SCHEDULING_TYPE:?}, got {read:?}"
    );
}

/// One row of the create table: the `Configuration` the create carries (null for a create
/// that carries none at all), and the two settings Athena defaults for a create that
/// configured nothing.
struct CreateCase {
    case: &'static str,
    configuration: Value,
    /// `EnforceWorkGroupConfiguration` as the read must report it.
    enforce: bool,
    /// `PublishCloudWatchMetricsEnabled` as the read must report it.
    publish_metrics: bool,
}

/// The rows: a create with no configuration at all, one with a configuration that names no
/// switch, one that names the overriding switch off, a Spark configuration, and one that
/// names every switch at once.
fn create_cases() -> Vec<CreateCase> {
    vec![
        CreateCase {
            case: "a create with no Configuration member at all",
            configuration: Value::Null,
            enforce: true,
            publish_metrics: true,
        },
        CreateCase {
            case: "a configuration that names no switch",
            configuration: json!({
                "ResultConfiguration": { "OutputLocation": "s3://athena-results/effective/" },
            }),
            enforce: false,
            publish_metrics: false,
        },
        CreateCase {
            case: "a configuration that turns the overriding switch off",
            configuration: json!({ "EnforceWorkGroupConfiguration": false }),
            enforce: false,
            publish_metrics: false,
        },
        CreateCase {
            case: "a Spark enabled workgroup's configuration",
            configuration: json!({
                "EngineVersion": { "SelectedEngineVersion": "PySpark engine version 3" },
                "ExecutionRole": "arn:aws:iam::000000000000:role/AthenaSparkExecution",
                "AdditionalConfiguration": "{\"spark.sql.shuffle.partitions\":\"8\"}",
            }),
            enforce: false,
            publish_metrics: false,
        },
        CreateCase {
            case: "a configuration that names every switch at once",
            configuration: json!({
                "EnforceWorkGroupConfiguration": true,
                "PublishCloudWatchMetricsEnabled": true,
                "RequesterPaysEnabled": true,
                "EnableMinimumEncryptionConfiguration": true,
                "BytesScannedCutoffPerQuery": 10_000_000_i64,
            }),
            enforce: true,
            publish_metrics: true,
        },
    ]
}

#[tokio::test]
async fn a_read_reports_the_system_tables_and_the_scheduling_type_of_every_work_group() {
    let router = build_router();

    for (index, case) in create_cases().into_iter().enumerate() {
        let name = format!("effective-{index}");
        let mut request = json!({ "Name": name });
        if !case.configuration.is_null() {
            request["Configuration"] = case.configuration.clone();
        }
        let (status, created) = call(&router, "CreateWorkGroup", &request).await;
        assert_eq!(
            status, 200,
            "case {:?}: the workgroup must be created, got {status} with {created:?}",
            case.case
        );

        let read = configuration(&router, &name).await;
        // Nothing enables a system table, so every workgroup created through this API
        // reports its system tables disabled.
        assert_reported_tables(case.case, &read, false);
        // The settings the create did not carry: Athena's own defaults where it carried no
        // configuration at all, off where it carried one that left them unset.
        assert_eq!(
            read["EnforceWorkGroupConfiguration"],
            json!(case.enforce),
            "case {:?}: expected EnforceWorkGroupConfiguration {}, got {read:?}",
            case.case,
            case.enforce
        );
        assert_eq!(
            read["PublishCloudWatchMetricsEnabled"],
            json!(case.publish_metrics),
            "case {:?}: expected PublishCloudWatchMetricsEnabled {}, got {read:?}",
            case.case,
            case.publish_metrics
        );
        // Reported BESIDE the configuration the create carried, not instead of it.
        if let Some(sent) = case.configuration.as_object() {
            for (member, value) in sent {
                if member == "EngineVersion" {
                    // Resolved rather than echoed, and tested on its own elsewhere.
                    continue;
                }
                assert_eq!(
                    &read[member], value,
                    "case {:?}: expected the sent {member} {value:?}, got {:?}",
                    case.case, read[member]
                );
            }
        }
    }
}

#[tokio::test]
async fn an_update_leaves_the_reported_system_tables_and_scheduling_type_in_place() {
    let router = build_router();

    // (case, the ConfigurationUpdates to send) — an update of a member that has nothing to
    // do with the system tables, one of a switch, and one that removes a member.
    let updates: Vec<(&str, Value)> = vec![
        (
            "an update of the scan cutoff",
            json!({ "BytesScannedCutoffPerQuery": 10_000_000_i64 }),
        ),
        (
            "an update of two switches",
            json!({
                "RequesterPaysEnabled": true,
                "EnableMinimumEncryptionConfiguration": true,
            }),
        ),
        (
            "an update that removes the scan cutoff",
            json!({ "RemoveBytesScannedCutoffPerQuery": true }),
        ),
    ];

    for (index, (case, configuration_updates)) in updates.into_iter().enumerate() {
        let name = format!("effective-updated-{index}");
        let (status, created) = call(&router, "CreateWorkGroup", &json!({ "Name": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the workgroup must be created, got {status} with {created:?}"
        );
        let before = configuration(&router, &name).await;

        let (status, updated) = call(
            &router,
            "UpdateWorkGroup",
            &json!({ "WorkGroup": name, "ConfigurationUpdates": configuration_updates }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: UpdateWorkGroup must answer 200, got {status} with {updated:?}"
        );

        let after = configuration(&router, &name).await;
        assert_reported_tables(case, &after, false);
        assert_eq!(
            after["SystemTableStatuses"], before["SystemTableStatuses"],
            "case {case:?}: an update must leave the system-table statuses where they were, \
             got {after:?} after {before:?}"
        );
        assert_eq!(
            after["QuerySchedulingType"], before["QuerySchedulingType"],
            "case {case:?}: an update must leave the scheduling type where it was, got \
             {after:?} after {before:?}"
        );
        // The defaults the create was given survive an update that does not name them.
        assert_eq!(
            after["EnforceWorkGroupConfiguration"], before["EnforceWorkGroupConfiguration"],
            "case {case:?}: an update that does not name the overriding switch must leave \
             it, got {after:?} after {before:?}"
        );
    }
}

#[tokio::test]
async fn two_reads_of_one_work_group_report_one_configuration() {
    let router = build_router();
    let name = "effective-is-derived";
    let (status, created) = call(&router, "CreateWorkGroup", &json!({ "Name": name })).await;
    assert_eq!(
        status, 200,
        "the workgroup must be created, got {status} with {created:?}"
    );

    // The configuration is DERIVED from the record on every read, so a read reports it
    // without writing anything a later read would see differently.
    let first = configuration(&router, name).await;
    let second = configuration(&router, name).await;
    assert_eq!(
        first, second,
        "expected two reads to report one configuration, got {first:?} then {second:?}"
    );
}

#[tokio::test]
async fn a_create_cannot_set_the_reported_system_tables_itself() {
    let router = build_router();
    let name = "effective-not-settable";
    // `SystemTableStatuses` and `QuerySchedulingType` are the service's own projection of
    // the workgroup: a request carrying them sets nothing, so the read reports what the
    // workgroup has rather than what the request asked for.
    let (status, created) = call(
        &router,
        "CreateWorkGroup",
        &json!({
            "Name": name,
            "Configuration": {
                "SystemTableStatuses": [{ "TableName": "QUERY_EXECUTIONS", "Enabled": true }],
                "QuerySchedulingType": "SOMETHING_ELSE",
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "the workgroup must be created, got {status} with {created:?}"
    );
    let read = configuration(&router, name).await;
    assert_reported_tables("a create that carried the reported members", &read, false);
}
