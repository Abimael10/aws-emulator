// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP coverage of what a maintenance put leaves standing, and of
//! the resource its refusals name.
//!
//! Two rules, both about `PutTableMaintenanceConfiguration` and the read beside
//! it:
//!
//! 1. A put settles the members its configuration value NAMES. Every member of a
//!    `TableMaintenanceConfigurationValue` is optional in the model — the
//!    status, the settings, and each member inside them — and a member the value
//!    leaves out is one the request says nothing about, so the value in force
//!    for it stays: the one the last put that named it left, else the task's own
//!    service default. A put carrying only a status settles the status and leaves
//!    the settings the task runs under where they were.
//! 2. The maintenance operations address ONE resource — the table the bucket
//!    ARN, the namespace and the name together name — so a refusal for an
//!    address nothing is stored at names the TABLE, whichever part of the
//!    address resolves to nothing, rather than a container the lookup passed
//!    through.
//!
//! The rows span what each rule generalises over: both maintenance tasks, every
//! member of each task's settings, which members the second put names (a status
//! alone, one settings member alone, a value naming nothing at all), the
//! statuses, the compaction strategy, the Region, and — for the second rule —
//! each level of the address that can resolve to nothing, over all three
//! operations of the surface.
//!
//! The defaults a table is maintained under, the whole value space of a single
//! put and the modeled refusals live in `table_maintenance_configuration.rs`,
//! whose helpers this suite drives rather than restating.

use serde_json::{json, Value};
use service_com_amazonaws_s3tables_s3tablebuckets as s3tables;

use s3tables::state::TableMaintenanceType;

use super::harness::Emulator;
use super::table_maintenance_configuration::{create_table, reported_task, Reported};

/// The fixed sentence this service states a table the store does not hold with.
const NO_TABLE: &str = "The specified table does not exist.";

/// One row of the partial-put table: the configuration put first, the
/// configuration put over it, and what the read must report for the task
/// afterwards.
struct Row {
    label: &'static str,
    region: &'static str,
    kind: TableMaintenanceType,
    first: Value,
    second: Value,
    expected: Reported,
}

/// A compaction configuration as a read reports it.
fn compaction(status: &str, target: i64, strategy: &str) -> Reported {
    Reported::Compaction(
        Some(status.to_string()),
        Some(target),
        Some(strategy.to_string()),
    )
}

/// A snapshot management configuration as a read reports it.
fn snapshots(status: &str, minimum: i64, age: i64) -> Reported {
    Reported::SnapshotManagement(Some(status.to_string()), Some(minimum), Some(age))
}

/// The rows the rule is graded on.
///
/// Every settings member of both tasks is left out by some row's second put and
/// named by another's, so each member is graded both ways; the compaction rows
/// stay inside the 64 MB to 512 MB window S3 Tables compacts towards, which
/// `maintenance_request_refusals.rs` is what pins.
fn rows() -> Vec<Row> {
    vec![
        Row {
            label: "a status over a whole compaction configuration",
            region: "us-east-1",
            kind: TableMaintenanceType::IcebergCompaction,
            first: json!({ "status": "enabled", "settings": {
                "icebergCompaction": { "targetFileSizeMB": 256, "strategy": "binpack" }
            }}),
            second: json!({ "status": "disabled" }),
            expected: compaction("disabled", 256, "binpack"),
        },
        Row {
            label: "a target file size over a whole compaction configuration",
            region: "eu-west-1",
            kind: TableMaintenanceType::IcebergCompaction,
            first: json!({ "status": "enabled", "settings": {
                "icebergCompaction": { "targetFileSizeMB": 256, "strategy": "binpack" }
            }}),
            second: json!({ "settings": {
                "icebergCompaction": { "targetFileSizeMB": 64 }
            }}),
            expected: compaction("enabled", 64, "binpack"),
        },
        Row {
            label: "a strategy over a whole compaction configuration",
            region: "ap-south-1",
            kind: TableMaintenanceType::IcebergCompaction,
            first: json!({ "status": "disabled", "settings": {
                "icebergCompaction": { "targetFileSizeMB": 512, "strategy": "sort" }
            }}),
            second: json!({ "settings": { "icebergCompaction": { "strategy": "z-order" } } }),
            expected: compaction("disabled", 512, "z-order"),
        },
        Row {
            label: "a value naming nothing at all over a compaction configuration",
            region: "us-west-2",
            kind: TableMaintenanceType::IcebergCompaction,
            first: json!({ "status": "enabled", "settings": {
                "icebergCompaction": { "targetFileSizeMB": 128, "strategy": "auto" }
            }}),
            second: json!({}),
            expected: compaction("enabled", 128, "auto"),
        },
        Row {
            label: "a strategy alone over a task no put had named",
            region: "ca-central-1",
            kind: TableMaintenanceType::IcebergCompaction,
            // Nothing was in force but the task's own default, so the member the
            // second put leaves out is that default's target file size.
            first: json!({}),
            second: json!({ "settings": { "icebergCompaction": { "strategy": "binpack" } } }),
            expected: compaction("enabled", 512, "binpack"),
        },
        Row {
            label: "a status over a whole snapshot management configuration",
            region: "us-east-2",
            kind: TableMaintenanceType::IcebergSnapshotManagement,
            first: json!({ "status": "enabled", "settings": {
                "icebergSnapshotManagement": {
                    "minSnapshotsToKeep": 3, "maxSnapshotAgeHours": 48,
                }
            }}),
            second: json!({ "status": "disabled" }),
            expected: snapshots("disabled", 3, 48),
        },
        Row {
            label: "a snapshot count over a whole snapshot management configuration",
            region: "eu-north-1",
            kind: TableMaintenanceType::IcebergSnapshotManagement,
            first: json!({ "status": "disabled", "settings": {
                "icebergSnapshotManagement": {
                    "minSnapshotsToKeep": 3, "maxSnapshotAgeHours": 48,
                }
            }}),
            second: json!({ "settings": {
                "icebergSnapshotManagement": { "minSnapshotsToKeep": 7 }
            }}),
            expected: snapshots("disabled", 7, 48),
        },
        Row {
            label: "a snapshot age over a whole snapshot management configuration",
            region: "sa-east-1",
            kind: TableMaintenanceType::IcebergSnapshotManagement,
            first: json!({ "status": "enabled", "settings": {
                "icebergSnapshotManagement": {
                    "minSnapshotsToKeep": 2, "maxSnapshotAgeHours": 1,
                }
            }}),
            second: json!({ "settings": {
                "icebergSnapshotManagement": { "maxSnapshotAgeHours": 720 }
            }}),
            expected: snapshots("enabled", 2, 720),
        },
    ]
}

/// The configuration a read reports for `kind`, after asserting the read
/// succeeded.
async fn read_task(
    emulator: &Emulator,
    region: &str,
    bucket_arn: &str,
    namespace: &str,
    table: &str,
    kind: TableMaintenanceType,
    case: &str,
) -> Reported {
    let read = emulator
        .get_table_maintenance_configuration(region, bucket_arn, namespace, table)
        .await;
    reported_task(&read, case, kind)
}

#[tokio::test]
async fn a_put_settles_the_members_its_value_names_and_leaves_the_rest_in_force() {
    let emulator = Emulator::new();
    let namespace = "events";
    let table = "user_events";

    for (index, row) in rows().into_iter().enumerate() {
        let case = format!("{label} in {region}", label = row.label, region = row.region);
        let (bucket_arn, _) = create_table(
            &emulator,
            row.region,
            &format!("maintenance-partial-{index}"),
            namespace,
            table,
        )
        .await;

        for (which, value) in [("first", &row.first), ("second", &row.second)] {
            let put = emulator
                .put_table_maintenance_configuration(
                    row.region,
                    &bucket_arn,
                    namespace,
                    table,
                    row.kind.as_wire(),
                    value,
                )
                .await;
            assert_eq!(
                put.status, 204,
                "case {case:?}: the {which} put must answer the modeled 204, got {status} {body}",
                status = put.status,
                body = put.body
            );
        }

        let actual = read_task(
            &emulator,
            row.region,
            &bucket_arn,
            namespace,
            table,
            row.kind,
            &case,
        )
        .await;
        assert_eq!(
            actual, row.expected,
            "case {case:?}: expected {expected:?}, got {actual:?}",
            expected = row.expected
        );

        // The put settles ONE task: the other still runs under its own default.
        for other in TableMaintenanceType::ALL
            .iter()
            .copied()
            .filter(|kind| *kind != row.kind)
        {
            let expected = match other {
                TableMaintenanceType::IcebergCompaction => Reported::default_compaction(),
                TableMaintenanceType::IcebergSnapshotManagement => {
                    Reported::default_snapshot_management()
                }
            };
            let untouched = read_task(
                &emulator,
                row.region,
                &bucket_arn,
                namespace,
                table,
                other,
                &case,
            )
            .await;
            assert_eq!(
                untouched, expected,
                "case {case:?}: {kind} was not addressed, so expected {expected:?}, got \
                 {untouched:?}",
                kind = other.as_wire()
            );
        }
    }
}

#[tokio::test]
async fn a_maintenance_call_on_an_address_nothing_is_stored_at_names_the_table() {
    let emulator = Emulator::new();
    let region = "us-west-2";
    let namespace = "events";
    let table = "user_events";
    let (bucket_arn, _) =
        create_table(&emulator, region, "maintenance-addressing", namespace, table).await;

    // (the case, the bucket ARN the call addresses, the namespace, the table
    // name) — one row per level of the address that can resolve to nothing,
    // each inside the Region the call is made in, so the Region is not what any
    // row is about.
    let rows: Vec<(&str, String, &str, &str)> = vec![
        (
            "a bucket this account does not hold",
            super::harness::expected_bucket_arn(region, "maintenance-addressing-absent"),
            namespace,
            table,
        ),
        (
            "the resident bucket's name under another account",
            bucket_arn.replace("000000000000", "123456789012"),
            namespace,
            table,
        ),
        (
            "a namespace the bucket does not hold",
            bucket_arn.clone(),
            "absent_namespace",
            table,
        ),
        (
            "a table the namespace does not hold",
            bucket_arn.clone(),
            namespace,
            "absent_table",
        ),
    ];

    for (case, arn, requested_namespace, requested_table) in rows {
        for (operation, reply) in [
            (
                "GetTableMaintenanceConfiguration",
                emulator
                    .get_table_maintenance_configuration(
                        region,
                        &arn,
                        requested_namespace,
                        requested_table,
                    )
                    .await,
            ),
            (
                "GetTableMaintenanceJobStatus",
                emulator
                    .get_table_maintenance_job_status(
                        region,
                        &arn,
                        requested_namespace,
                        requested_table,
                    )
                    .await,
            ),
            (
                "PutTableMaintenanceConfiguration",
                emulator
                    .put_table_maintenance_configuration(
                        region,
                        &arn,
                        requested_namespace,
                        requested_table,
                        TableMaintenanceType::IcebergCompaction.as_wire(),
                        &json!({ "status": "disabled" }),
                    )
                    .await,
            ),
        ] {
            assert_eq!(
                reply.status, 404,
                "case {case:?} on {operation}: expected 404, got {status} {body}",
                status = reply.status,
                body = reply.body
            );
            assert_eq!(
                reply.error_code.as_deref(),
                Some("NotFoundException"),
                "case {case:?} on {operation}: expected NotFoundException, got {actual:?}",
                actual = reply.error_code
            );
            assert_eq!(
                reply.message(),
                NO_TABLE,
                "case {case:?} on {operation}: expected {NO_TABLE:?}, got {actual:?}",
                actual = reply.message()
            );
        }
    }

    // The resident table still answers, and under its own default: the refusals
    // above named no resource and changed none.
    let reported = read_task(
        &emulator,
        region,
        &bucket_arn,
        namespace,
        table,
        TableMaintenanceType::IcebergCompaction,
        "after the refusals",
    )
    .await;
    assert_eq!(
        reported,
        Reported::default_compaction(),
        "after the refusals: the resident table must still run under the published default, got \
         {reported:?}"
    );
}
