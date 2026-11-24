// SPDX-License-Identifier: CC0-1.0

//! The definition a read reports is a definition this service accepts back, member for
//! member.
//!
//! Three rules, all about the read path:
//!
//! 1. **A read states the delivery recorded against the report.** The modelled
//!    `ReportStatus` is "a two element dictionary with a `lastDelivery` and `lastStatus`
//!    key whose values describe the date and status of the last delivered report for a
//!    particular report definition" (`smithy_query(shape="ReportStatus")`), so every read
//!    states that dictionary: a report a delivery HAS been recorded against carries the
//!    instant of the delivery and the outcome it ended with, and a report none has been
//!    recorded against carries both keys EMPTY.
//! 2. **Every member a CALLER may submit holds a value the model admits for it.** Those
//!    members are constrained like every other: `BillingViewArn` is length 0..128 and each
//!    enum member is its own catalog — so a member the service holds no value for is left
//!    out of the response rather than filled with a stand-in outside those constraints.
//!    The delivery status is the exception the service OWNS: `lastDelivery` is length
//!    16..20 with the pattern `^[0-9]{8}[T][0-9]{6}([Z]|[+-][0-9]{4})$` and `lastStatus` is
//!    the `LastStatus` catalog `SUCCESS | ERROR_PERMISSIONS | ERROR_NO_BUCKET`
//!    (`smithy_query(shape="LastDelivery,LastStatus")`), which describe a DELIVERY, and the
//!    never-delivered report reports the empty spelling the recorded service answers.
//! 3. **A reported definition round-trips.** `ModifyReportDefinition` replaces a definition
//!    wholesale, so read-modify-write is this API's normal way to change one: the whole
//!    `ReportDefinition` a read serves, replayed without the service-populated delivery
//!    status, is accepted by both operations that take that shape — the boundary validator
//!    holds a request to the same constraints the read's own caller-submittable values
//!    satisfy (its refusals are pinned by `modelled_constraints.rs`), and the delivery
//!    status the service fills is not a caller's to send back.
//!
//! The table's rows differ along what the rules generalise over: whether a delivery has
//! been recorded against the report, how that delivery ended (every value of the modelled
//! catalog), the instant it was delivered, which optional members the stored definition
//! carries and in which form, and how many reports the account holds (one, and more than
//! one page's worth). The create leg of each row also replays a reported definition that
//! CLAIMS a delivery status, which the new report must not take as its own.

use std::collections::BTreeMap;
use std::time::SystemTime;

use serde_json::{json, Value};

use service_com_amazonaws_costandusagereportservice_awsorigamiservicegatewayservice as cur;

use cur::ops::types::{
    ReportDefinition, ADDITIONAL_ARTIFACT_VALUES, A_W_S_REGION_VALUES, COMPRESSION_FORMAT_VALUES,
    LAST_STATUS_VALUES, REPORT_FORMAT_VALUES, REPORT_VERSIONING_VALUES, SCHEMA_ELEMENT_VALUES,
    TIME_UNIT_VALUES,
};
use cur::state::{ReportDelivery, StoredReport};

use super::report_definition_lifecycle::{
    assert_delivery_status, at_epoch_seconds, error_code, is_report_timestamp, minimal_definition,
    recorded_deliveries, Cur,
};

/// The strings of a JSON list member.
fn strings(value: &Value) -> Vec<String> {
    value
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// One optional string member of a JSON definition.
fn text(definition: &Value, member: &str) -> Option<String> {
    definition[member].as_str().map(str::to_string)
}

/// A stored report holding `definition` under `report_name`, created at `created_at`, with
/// `last_delivery` recorded against it.
///
/// Seeding is how a delivered report is set up at all: nothing in this service's modelled
/// surface delivers a report, so the API cannot put a delivery on one.
fn stored_report(
    report_name: &str,
    definition: &Value,
    created_at: SystemTime,
    last_delivery: Option<ReportDelivery>,
) -> StoredReport {
    StoredReport {
        definition: ReportDefinition {
            report_name: report_name.to_string(),
            time_unit: definition["TimeUnit"].as_str().unwrap_or_default().to_string(),
            format: definition["Format"].as_str().unwrap_or_default().to_string(),
            compression: definition["Compression"].as_str().unwrap_or_default().to_string(),
            additional_schema_elements: strings(&definition["AdditionalSchemaElements"]),
            s3_bucket: definition["S3Bucket"].as_str().unwrap_or_default().to_string(),
            s3_prefix: definition["S3Prefix"].as_str().unwrap_or_default().to_string(),
            s3_region: definition["S3Region"].as_str().unwrap_or_default().to_string(),
            additional_artifacts: definition.get("AdditionalArtifacts").map(strings),
            refresh_closed_reports: definition["RefreshClosedReports"].as_bool(),
            report_versioning: text(definition, "ReportVersioning"),
            billing_view_arn: text(definition, "BillingViewArn"),
            // Both are facts about the report the service holds, never the request's.
            report_status: None,
            report_creation_date: None,
        },
        tags: BTreeMap::new(),
        created_at,
        last_delivery,
    }
}

/// The definitions the rows are built from: which optional members a stored definition
/// carries, and in which form.
fn definitions() -> Vec<(&'static str, Value)> {
    let mut bare_billing_view = minimal_definition("round-trip-report");
    bare_billing_view["BillingViewArn"] = json!("my-billing.view_1");
    bare_billing_view["RefreshClosedReports"] = json!(true);
    vec![
        ("required members only", minimal_definition("round-trip-report")),
        (
            "every optional member stated",
            json!({
                "ReportName": "round-trip-report",
                "TimeUnit": "HOURLY",
                "Format": "Parquet",
                "Compression": "Parquet",
                "AdditionalSchemaElements": ["RESOURCES", "SPLIT_COST_ALLOCATION_DATA"],
                "S3Bucket": "round.trip-bucket",
                "S3Prefix": "teams/finance/cur/",
                "S3Region": "eu-west-1",
                "AdditionalArtifacts": ["ATHENA"],
                "RefreshClosedReports": false,
                "ReportVersioning": "OVERWRITE_REPORT",
                "BillingViewArn": "arn:aws:billing::123456789012:billingview/123456789012",
            }),
        ),
        (
            "an empty prefix and an empty artifact list",
            json!({
                "ReportName": "round-trip-report",
                "TimeUnit": "MONTHLY",
                "Format": "textORcsv",
                "Compression": "ZIP",
                "AdditionalSchemaElements": [],
                "S3Bucket": "bucket_with_underscores",
                "S3Prefix": "",
                "S3Region": "ap-southeast-2",
                "AdditionalArtifacts": [],
            }),
        ),
        (
            "a billing view named without the ARN prefix",
            bare_billing_view,
        ),
    ]
}

/// One seeded report per (definition, delivery) pair, each under its own name, so one
/// account holds every row of the table at once — more than one page's worth.
fn every_row_seeded() -> Vec<(String, Option<ReportDelivery>, StoredReport)> {
    let mut seeded = Vec::new();
    for (definition_index, (definition_case, definition)) in definitions().into_iter().enumerate() {
        for (delivery_case, delivery) in recorded_deliveries() {
            let report_name = format!("row-{definition_index}-{}", seeded.len());
            seeded.push((
                format!("{definition_case} / {delivery_case}"),
                delivery.clone(),
                stored_report(
                    &report_name,
                    &definition,
                    at_epoch_seconds(1_767_225_600),
                    delivery,
                ),
            ));
        }
    }
    seeded
}

/// Every member of a served definition holds a value the model's constraints on that
/// member admit.
///
/// The catalogs and bounds are the modelled ones: the enum catalogs this crate generates
/// from the model (`smithy_query(shape="TimeUnit,ReportFormat,CompressionFormat,AWSRegion,\
/// ReportVersioning,SchemaElement,AdditionalArtifact,LastStatus")`), `BillingViewArn`'s
/// length 0..128, `ReportName`'s length 0..256 and the `LastDelivery` length 16..20 with
/// its `^[0-9]{8}[T][0-9]{6}([Z]|[+-][0-9]{4})$` form.
fn assert_members_are_admissible(case: &str, reported: &Value) {
    for (member, catalog) in [
        ("TimeUnit", TIME_UNIT_VALUES),
        ("Format", REPORT_FORMAT_VALUES),
        ("Compression", COMPRESSION_FORMAT_VALUES),
        ("S3Region", A_W_S_REGION_VALUES),
        ("ReportVersioning", REPORT_VERSIONING_VALUES),
    ] {
        let value = reported[member].as_str().unwrap_or_default();
        assert!(
            catalog.contains(&value),
            "case {case:?}: expected {member} inside its modelled catalog {catalog:?}, got \
             {value:?}"
        );
    }
    for (member, catalog) in [
        ("AdditionalSchemaElements", SCHEMA_ELEMENT_VALUES),
        ("AdditionalArtifacts", ADDITIONAL_ARTIFACT_VALUES),
    ] {
        for entry in strings(&reported[member]) {
            assert!(
                catalog.contains(&entry.as_str()),
                "case {case:?}: expected every {member} entry inside its modelled catalog \
                 {catalog:?}, got {entry:?}"
            );
        }
    }

    let name = reported["ReportName"].as_str().unwrap_or_default();
    assert!(
        (1..=256).contains(&name.chars().count()),
        "case {case:?}: expected a ReportName inside the modelled length 0..256 its pattern \
         admits, got {name:?}"
    );
    let billing_view = reported["BillingViewArn"].as_str().unwrap_or_default();
    assert!(
        (1..=128).contains(&billing_view.chars().count()),
        "case {case:?}: expected a BillingViewArn inside the modelled length 0..128 its pattern \
         admits, got {billing_view:?}"
    );
    let created = reported["ReportCreationDate"].as_str().unwrap_or_default();
    assert!(
        is_report_timestamp(created),
        "case {case:?}: expected a ReportCreationDate stated as yyyymmddThhmmssZ, got {created:?}"
    );

    // The member the service fills from the report's delivery history: present on every
    // read, holding the values a delivery has for a report one was recorded against and the
    // empty spelling for a report none was.
    let status = reported
        .get("ReportStatus")
        .cloned()
        .unwrap_or(Value::Null);
    let last_delivery = status["lastDelivery"].as_str().unwrap_or_default();
    let last_status = status["lastStatus"].as_str().unwrap_or_default();
    if last_delivery.is_empty() || last_status.is_empty() {
        assert_eq!(
            (last_delivery, last_status),
            ("", ""),
            "case {case:?}: a report with no delivery recorded states both keys empty, got \
             {status:?}"
        );
    } else {
        assert!(
            (16..=20).contains(&last_delivery.chars().count()) && is_report_timestamp(last_delivery),
            "case {case:?}: expected a lastDelivery inside the modelled length 16..20 and its \
             pattern, got {status:?}"
        );
        assert!(
            LAST_STATUS_VALUES.contains(&last_status),
            "case {case:?}: expected a lastStatus inside the modelled catalog \
             {LAST_STATUS_VALUES:?}, got {status:?}"
        );
    }
}

#[tokio::test]
async fn a_reported_definition_states_the_delivery_recorded_against_the_report() {
    for (definition_case, definition) in definitions() {
        for (delivery_case, delivery) in recorded_deliveries() {
            let case = format!("{definition_case} / {delivery_case}");
            let cur = Cur::new();
            cur.seed(vec![stored_report(
                "round-trip-report",
                &definition,
                at_epoch_seconds(1_767_225_600),
                delivery.clone(),
            )]);

            let listed = cur.describe_all().await;
            let reported = listed.first().cloned().unwrap_or(Value::Null);
            assert_eq!(
                listed.len(),
                1,
                "case {case:?}: expected the seeded report to be listed, got {listed:?}"
            );
            assert_delivery_status(&case, &reported, delivery.as_ref());
        }
    }
}

#[tokio::test]
async fn every_member_a_read_fills_holds_a_value_the_model_admits() {
    // One account holding every row at once, so the rule is asserted on each element of
    // each page rather than on the first element of the first one.
    let seeded = every_row_seeded();
    let cur = Cur::new();
    cur.seed(
        seeded
            .iter()
            .map(|(_, _, report)| report.clone())
            .collect(),
    );

    let listed = cur.describe_all().await;
    assert_eq!(
        listed.len(),
        seeded.len(),
        "expected every seeded report to be listed, got {listed:?}"
    );
    for (index, reported) in listed.iter().enumerate() {
        let (case, delivery, _) = &seeded[index];
        let case = format!("{case} / element {index}");
        assert_members_are_admissible(&case, reported);
        assert_delivery_status(&case, reported, delivery.as_ref());
    }
}

#[tokio::test]
async fn a_reported_definition_is_accepted_back_unchanged() {
    for (definition_case, definition) in definitions() {
        for (delivery_case, delivery) in recorded_deliveries() {
            let case = format!("{definition_case} / {delivery_case}");
            let cur = Cur::new();
            cur.seed(vec![stored_report(
                "round-trip-report",
                &definition,
                at_epoch_seconds(1_767_225_600),
                delivery.clone(),
            )]);

            let listed = cur.describe_all().await;
            let reported = listed.first().cloned().unwrap_or(Value::Null);
            assert_eq!(
                listed.len(),
                1,
                "case {case:?}: expected the seeded report to be listed, got {listed:?}"
            );
            let report_name = reported["ReportName"]
                .as_str()
                .unwrap_or_default()
                .to_string();

            // Read-modify-write: the definition just read, replayed without the delivery
            // status the service fills for itself. Every value a caller may submit is one
            // the modelled constraints admit, so the boundary validator takes the whole of
            // it back.
            let mut submitted = reported.clone();
            submitted
                .as_object_mut()
                .expect("a report definition is an object")
                .remove("ReportStatus");
            let (status, body) = cur
                .call(
                    "ModifyReportDefinition",
                    &json!({ "ReportName": report_name, "ReportDefinition": submitted }),
                )
                .await;
            assert_eq!(
                (status, error_code(&body)),
                (200, "<no __type>"),
                "case {case:?}: the definition the service reported must be one it accepts \
                 back, got {status} {body:?}"
            );
            assert_eq!(
                cur.describe_all().await,
                listed,
                "case {case:?}: replaying the reported definition must leave the report as it \
                 was, got {reported:?}"
            );

            // The same definition under a free name, through the create path: accepted,
            // and the new report has no delivery of its own to report.
            let mut copy = submitted.clone();
            copy["ReportName"] = json!(format!("{report_name}-copy"));
            let (status, body) = cur.put(&copy, None).await;
            assert_eq!(
                (status, error_code(&body)),
                (200, "<no __type>"),
                "case {case:?}: the reported definition must be accepted as a new report, got \
                 {status} {body:?}"
            );

            let created = cur
                .describe_all()
                .await
                .into_iter()
                .find(|listed| listed["ReportName"] == copy["ReportName"])
                .unwrap_or(Value::Null);
            assert_delivery_status(&format!("{case} / the new report"), &created, None);
            // Every member of the definition is the one the service reported, so the copy
            // differs from it only in the name and in the facts the service holds.
            let definition_members = [
                "TimeUnit",
                "Format",
                "Compression",
                "AdditionalSchemaElements",
                "S3Bucket",
                "S3Prefix",
                "S3Region",
                "AdditionalArtifacts",
                "RefreshClosedReports",
                "ReportVersioning",
                "BillingViewArn",
            ];
            for member in definition_members {
                assert_eq!(
                    created[member], reported[member],
                    "case {case:?}: expected {member} {:?} on the new report, got {created:?}",
                    reported[member]
                );
            }
        }
    }
}
