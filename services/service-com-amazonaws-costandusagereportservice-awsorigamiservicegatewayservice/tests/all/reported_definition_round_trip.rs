// SPDX-License-Identifier: CC0-1.0

//! The definition a read reports, and the definition a caller supplies, are accepted back
//! member for member.
//!
//! Two rules, both about the read path:
//!
//! 1. **A read states the report's delivery status on every read.** The modelled
//!    `ReportStatus` is "a two element dictionary with a `lastDelivery` and `lastStatus`
//!    key" (`smithy_query(shape="ReportStatus")`), and the dictionary and both of its keys
//!    are unconditional on this read path: only the two VALUES depend on the report's
//!    delivery history, which is the delivered instant and the outcome it ended with once a
//!    delivery has been recorded and the empty string until one has.
//! 2. **The members the service populates are the service's, and the rest round-trips.**
//!    `ModifyReportDefinition` replaces a definition wholesale, so read-modify-write is
//!    this API's normal way to change one: every member of a reported definition a caller
//!    supplies is a member `PutReportDefinition` and `ModifyReportDefinition` take back.
//!    The never-delivered sentinel is the one output-only value among them — the modelled
//!    constraints on the two `ReportStatus` members describe the values a DELIVERY has
//!    (`lastDelivery` length 16..20 and the pattern
//!    `^[0-9]{8}[T][0-9]{6}([Z]|[+-][0-9]{4})$`, `lastStatus` the `LastStatus` catalog
//!    `SUCCESS | ERROR_PERMISSIONS | ERROR_NO_BUCKET`,
//!    `smithy_query(shape="LastDelivery,LastStatus")`), and the boundary validator holds
//!    every request to them (its refusals are pinned by `modelled_constraints.rs`), so the
//!    replay below carries the sentinel back no more than a caller would.
//!
//! The table's rows differ along what the rules generalise over: whether a delivery has
//! been recorded against the report, how that delivery ended (every value of the modelled
//! catalog), the instant it was delivered, and which optional members the stored
//! definition carries and in which form. The create leg of each row also replays a
//! reported definition that CLAIMS a delivery status, which the new report must not take
//! as its own.

use std::collections::BTreeMap;
use std::time::SystemTime;

use serde_json::{json, Value};

use service_com_amazonaws_costandusagereportservice_awsorigamiservicegatewayservice as cur;

use cur::ops::types::ReportDefinition;
use cur::state::{ReportDelivery, StoredReport};

use super::report_definition_lifecycle::{
    at_epoch_seconds, error_code, is_report_timestamp, minimal_definition, never_delivered,
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

/// A stored report holding `definition`, created at `created_at`, with `last_delivery`
/// recorded against it.
///
/// Seeding is how a delivered report is set up at all: nothing in this service's modelled
/// surface delivers a report, so the API cannot put a delivery on one.
fn stored_report(
    definition: &Value,
    created_at: SystemTime,
    last_delivery: Option<ReportDelivery>,
) -> StoredReport {
    StoredReport {
        definition: ReportDefinition {
            report_name: definition["ReportName"].as_str().unwrap_or_default().to_string(),
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

/// The reported definition as a caller supplies it back: the never-delivered sentinel is
/// an output-only value, so a replay drops it exactly as a caller that has never had a
/// delivery reported would, and carries a recorded delivery status back as it was read.
fn as_supplied_back(reported: &Value) -> Value {
    let mut supplied = reported.clone();
    if supplied.get("ReportStatus") == Some(&never_delivered()) {
        supplied
            .as_object_mut()
            .expect("a report definition is an object")
            .remove("ReportStatus");
    }
    supplied
}

#[tokio::test]
async fn a_reported_definition_states_a_delivery_status_on_every_read() {
    for (definition_case, definition) in definitions() {
        for (delivery_case, delivery) in recorded_deliveries() {
            let case = format!("{definition_case} / {delivery_case}");
            let cur = Cur::new();
            cur.seed(vec![stored_report(
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
            let status = reported.get("ReportStatus").cloned();

            match &delivery {
                None => assert_eq!(
                    status,
                    Some(never_delivered()),
                    "case {case:?}: no delivery was recorded, so the read-back must state the \
                     delivery status with both keys empty, got {reported:?}"
                ),
                Some(recorded) => {
                    let status = status.unwrap_or(Value::Null);
                    let last_delivery = status["lastDelivery"].as_str().unwrap_or_default();
                    assert!(
                        is_report_timestamp(last_delivery),
                        "case {case:?}: expected lastDelivery stated as yyyymmddThhmmssZ, got \
                         {status:?}"
                    );
                    assert_eq!(
                        status["lastStatus"],
                        json!(recorded.status),
                        "case {case:?}: expected lastStatus {:?}, got {status:?}",
                        recorded.status
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn a_reported_definition_is_accepted_back_unchanged() {
    for (definition_case, definition) in definitions() {
        for (delivery_case, delivery) in recorded_deliveries() {
            let case = format!("{definition_case} / {delivery_case}");
            let cur = Cur::new();
            cur.seed(vec![stored_report(
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

            // The never-delivered sentinel is an output-only value: the modelled
            // constraints on the two `ReportStatus` members describe the values a delivery
            // has, and the boundary validator holds a request to them like any other
            // constrained member, so replaying the dictionary the service states for a
            // report nothing has delivered is refused rather than stored.
            if reported.get("ReportStatus") == Some(&never_delivered()) {
                let (status, body) = cur
                    .call(
                        "ModifyReportDefinition",
                        &json!({ "ReportName": report_name, "ReportDefinition": reported }),
                    )
                    .await;
                assert_eq!(
                    (status, error_code(&body)),
                    (400, "ValidationException"),
                    "case {case:?}: the never-delivered sentinel is not a value the modelled \
                     constraints admit on the way in, got {status} {body:?}"
                );
                assert_eq!(
                    cur.describe_all().await,
                    listed,
                    "case {case:?}: a refused update must leave the report as it was, got \
                     {reported:?}"
                );
            }

            // Read-modify-write: the definition just read, replayed as a caller supplies it.
            let supplied = as_supplied_back(&reported);
            let (status, body) = cur
                .call(
                    "ModifyReportDefinition",
                    &json!({ "ReportName": report_name, "ReportDefinition": supplied }),
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
            let mut copy = supplied.clone();
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
            assert_eq!(
                created.get("ReportStatus"),
                Some(&never_delivered()),
                "case {case:?}: nothing delivered the new report, so it must state the delivery \
                 status with both keys empty, got {created:?}"
            );
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
