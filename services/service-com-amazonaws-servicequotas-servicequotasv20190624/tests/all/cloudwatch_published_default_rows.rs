// SPDX-License-Identifier: CC0-1.0

//! What `ListAWSDefaultServiceQuotas` publishes for Amazon CloudWatch, and the fact that
//! each row's LIMIT and ADJUSTABILITY are its own.
//!
//! THE RULE: a default-quota row is published under AWS's own `QuotaCode` with the
//! `QuotaName`, `Value`, `Unit`, `Adjustable`, `GlobalQuota`, `Period`, `UsageMetric` and
//! `Description` AWS publishes for THAT quota. What a class of quotas shares — here the
//! per-API request-rate class's name form, its one-second window, the `AWS/Usage`
//! `CallCount` of the operation it throttles and its sentence — is shared; the requests
//! per second and the adjustability are not, so no row carries a rate or a flag borrowed
//! from a neighbour or from a class-wide default. Every published row is enumerated
//! however the caller pages the listing, and reads back under its own code on both read
//! planes, which is what makes `QuotaCode` the join key between the catalog and an
//! increase request.
//!
//! The rows below span what the rule generalises over: published rates of 3, 9, 10, 20
//! and 1000 per second (two rows that happen to share a rate, so one cannot be a default
//! for the other), rows AWS publishes as adjustable beside rows it publishes as fixed, and
//! both kinds of row this catalog carries for the service — the rate of one API operation,
//! with its call-count metric, and a rate on a metric-math function, with a window and no
//! metric. Two of them (`L-6FCAAA2E`, `L-722EAE58`) are rows no issue reported wrong, so
//! the rule is asserted past the cited cases.
//!
//! SOURCE for every expectation: the recorded `ListAWSDefaultServiceQuotas` response of
//! `monitoring`, which states each row's `QuotaCode`, `QuotaName`, published rate,
//! adjustability, window, usage metric and sentence; the row table lives in
//! `src/state/quota_rows/monitoring.rs`, whose doc comments name the per-row sources.
//!
//! The harness (`client`, `emulator`, `default_quotas`, `walk_pages`) is the one
//! `quota_workflow.rs` owns.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::quota_workflow::{client, default_quotas as defaults, emulator, walk_pages};

/// The `ServiceCode` whose published catalog this suite pins.
const CLOUDWATCH: &str = "monitoring";

/// The one label `ListServices` reports for [`CLOUDWATCH`], which every row of its
/// listings carries.
const CLOUDWATCH_SERVICE_NAME: &str = "Amazon CloudWatch";

/// One quota AWS publishes for Amazon CloudWatch.
struct PublishedRow {
    quota_code: &'static str,
    quota_name: &'static str,
    /// The quota's own published limit — per row, never the class's.
    value: f64,
    /// The quota's own published `Adjustable` flag — per row, never the class's.
    adjustable: bool,
    /// The API operation whose calls the row's rate throttles, for a row of the
    /// per-operation request-rate class; `None` for a rate on something other than one
    /// operation's calls, which publishes no call-count series.
    throttled_api: Option<&'static str>,
    description: &'static str,
}

/// The quotas this catalog publishes for [`CLOUDWATCH`], each under the `QuotaCode` AWS
/// publishes it as, in listing order.
const CLOUDWATCH_ROWS: &[PublishedRow] = &[
    PublishedRow {
        quota_code: "L-6FCAAA2E",
        quota_name: "Rate of GetMetricWidgetImage requests",
        value: 20.0,
        adjustable: true,
        throttled_api: Some("GetMetricWidgetImage"),
        description: "The maximum number of GetMetricWidgetImage requests that you can make, \
                      per second, in this account in the current region.",
    },
    PublishedRow {
        quota_code: "L-69C44FFD",
        quota_name: "Rate of ListDashboards requests",
        value: 10.0,
        adjustable: true,
        throttled_api: Some("ListDashboards"),
        description: "The maximum number of ListDashboards requests that you can make, per \
                      second, in this account in the current region.",
    },
    PublishedRow {
        quota_code: "L-6300B446",
        quota_name: "Rate of DescribeAnomalyDetectors requests",
        value: 10.0,
        adjustable: true,
        throttled_api: Some("DescribeAnomalyDetectors"),
        description: "The maximum number of DescribeAnomalyDetectors requests that you can \
                      make, per second, in this account in the current region.",
    },
    PublishedRow {
        quota_code: "L-E2310E7A",
        quota_name: "Rate of DescribeAlarmsForMetric requests",
        value: 9.0,
        adjustable: false,
        throttled_api: Some("DescribeAlarmsForMetric"),
        description: "The maximum number of DescribeAlarmsForMetric requests that you can \
                      make, per second, in this account in the current region.",
    },
    PublishedRow {
        quota_code: "L-CA907765",
        quota_name: "Rate of EnableAlarmActions requests",
        value: 3.0,
        adjustable: false,
        throttled_api: Some("EnableAlarmActions"),
        description: "The maximum number of EnableAlarmActions requests that you can make, per \
                      second, in this account in the current region.",
    },
    PublishedRow {
        quota_code: "L-722EAE58",
        quota_name: "Rate of ANOMALY_DETECTION_BAND usage in GetMetricData",
        value: 1000.0,
        adjustable: false,
        throttled_api: None,
        description: "The maximum number of times the ANOMALY_DETECTION_BAND function can be \
                      used in all GetMetricData requests, per second, in this account in the \
                      current region.",
    },
];

/// The whole `ServiceQuota` record the row is published as at `region`, as the default
/// plane reports it.
fn expected_record(row: &PublishedRow, region: &str) -> Value {
    let mut record = json!({
        "ServiceCode": CLOUDWATCH,
        "ServiceName": CLOUDWATCH_SERVICE_NAME,
        // An AWS DEFAULT quota belongs to no account, so its ARN's account segment is
        // empty and the Region is the endpoint the catalog was read at.
        "QuotaArn": format!("arn:aws:servicequotas:{region}::{CLOUDWATCH}/{}", row.quota_code),
        "QuotaCode": row.quota_code,
        "QuotaName": row.quota_name,
        "Value": row.value,
        "Unit": "None",
        "Adjustable": row.adjustable,
        "GlobalQuota": false,
        "Description": row.description,
        // Every row of this catalog is a rate over one second.
        "Period": {"PeriodValue": 1, "PeriodUnit": "SECOND"},
    });
    if let Some(api) = row.throttled_api {
        record["UsageMetric"] = json!({
            "MetricNamespace": "AWS/Usage",
            "MetricName": "CallCount",
            "MetricStatisticRecommendation": "Sum",
            "MetricDimensions": {
                "Service": "CloudWatch",
                "Resource": api,
                "Type": "API",
                "Class": "None",
            },
        });
    }
    record
}

/// The `QuotaCode`s a listing reports, in listing order.
fn codes_of(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|quota| quota["QuotaCode"].as_str().map(str::to_string))
        .collect()
}

/// Every quota this catalog publishes for CloudWatch is in the listing with its own
/// published members, and the listing carries nothing else.
#[tokio::test]
async fn the_default_listing_publishes_every_row_of_the_cloudwatch_catalog() {
    let engine = emulator();
    let region = "us-west-2";
    let caller = client(&engine, region);

    let listed = defaults(&caller, CLOUDWATCH).await;

    for row in CLOUDWATCH_ROWS {
        let case = format!("{CLOUDWATCH}/{}", row.quota_code);
        let served = listed
            .iter()
            .find(|quota| quota["QuotaCode"] == json!(row.quota_code))
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: the listing must publish this quota, got the codes {:?}",
                    codes_of(&listed)
                )
            });
        assert_eq!(
            *served,
            expected_record(row, region),
            "case {case}: expected the row AWS publishes under this code, got {served}"
        );
    }

    // Nothing beyond the published rows: a padded listing would make the completeness
    // rule unfalsifiable.
    assert_eq!(
        codes_of(&listed),
        CLOUDWATCH_ROWS
            .iter()
            .map(|row| row.quota_code.to_string())
            .collect::<Vec<String>>(),
        "case {CLOUDWATCH}: expected exactly the published rows, in listing order"
    );

    // A guard on the TABLE above, not on the emulator: the rows must span more than one
    // published rate and both adjustabilities, or the assertions over them would be one
    // shape of row repeated. It fails only if a later edit narrows the table.
    let rates: BTreeSet<String> = CLOUDWATCH_ROWS
        .iter()
        .map(|row| row.value.to_string())
        .collect();
    let fixed = CLOUDWATCH_ROWS.iter().filter(|row| !row.adjustable).count();
    let metered = CLOUDWATCH_ROWS
        .iter()
        .filter(|row| row.throttled_api.is_some())
        .count();
    assert!(
        rates.len() >= 4
            && fixed > 0
            && fixed < CLOUDWATCH_ROWS.len()
            && metered > 0
            && metered < CLOUDWATCH_ROWS.len(),
        "case {CLOUDWATCH}: the table must span the class, got the rates {rates:?}, {fixed} \
         fixed rows and {metered} metered rows of {}",
        CLOUDWATCH_ROWS.len()
    );
}

/// A row's LIMIT is its own: two rows of one class published at different rates report
/// different `Value`s, and two that share a rate still each report their own — so no row
/// is filled in from a class-wide default.
#[tokio::test]
async fn each_published_row_reports_its_own_rate_and_adjustability() {
    let engine = emulator();
    let caller = client(&engine, "eu-central-1");

    let listed = defaults(&caller, CLOUDWATCH).await;
    for row in CLOUDWATCH_ROWS {
        let case = format!("{CLOUDWATCH}/{}", row.quota_code);
        let served = listed
            .iter()
            .find(|quota| quota["QuotaCode"] == json!(row.quota_code))
            .unwrap_or_else(|| panic!("case {case}: the listing must publish this quota"));
        assert_eq!(
            (&served["Value"], &served["Adjustable"]),
            (&json!(row.value), &json!(row.adjustable)),
            "case {case}: expected the rate and the flag AWS publishes for this quota, got \
             {served}"
        );
    }
}

/// A published row is a usable KEY: its code resolves the same record on the default and
/// the applied read plane at every endpoint, and the increase request answers it
/// according to the row's own `Adjustable` flag.
#[tokio::test]
async fn a_published_cloudwatch_row_is_the_key_both_planes_and_the_increase_request_answer() {
    let engine = emulator();

    // Two endpoints, because the catalog is published per Region and only the ARN's
    // Region segment may differ between them.
    for region in ["us-east-2", "ap-southeast-1"] {
        let caller = client(&engine, region);
        let listed = defaults(&caller, CLOUDWATCH).await;

        for row in CLOUDWATCH_ROWS {
            let case = format!("{region} {CLOUDWATCH}/{}", row.quota_code);
            let pair = json!({"ServiceCode": CLOUDWATCH, "QuotaCode": row.quota_code});

            let default = caller.ok("GetAWSDefaultServiceQuota", pair.clone()).await;
            assert_eq!(
                default["Quota"],
                expected_record(row, region),
                "case {case}: the single read must report the published row, got {default}"
            );
            assert!(
                listed.contains(&default["Quota"]),
                "case {case}: the listing must carry the record the single read returned, got \
                 the codes {:?}",
                codes_of(&listed)
            );

            let applied = caller.ok("GetServiceQuota", pair.clone()).await;
            for member in ["QuotaName", "Description", "Value", "Period", "UsageMetric"] {
                assert_eq!(
                    applied["Quota"][member], default["Quota"][member],
                    "case {case}: with nothing applied both planes must agree on {member}, got \
                     {applied}"
                );
            }

            let mut increase = pair.clone();
            increase["DesiredValue"] = json!(row.value + 1.0);
            if row.adjustable {
                let requested = caller.ok("RequestServiceQuotaIncrease", increase).await
                    ["RequestedQuota"]
                    .clone();
                assert_eq!(
                    (
                        &requested["ServiceCode"],
                        &requested["QuotaCode"],
                        &requested["DesiredValue"],
                    ),
                    (
                        &json!(CLOUDWATCH),
                        &json!(row.quota_code),
                        &json!(row.value + 1.0),
                    ),
                    "case {case}: the request must be recorded against the quota asked for, got \
                     {requested}"
                );
            } else {
                let (status, error) = caller.refused("RequestServiceQuotaIncrease", increase).await;
                assert_eq!(
                    (status, error.as_str()),
                    (400, "IllegalArgumentException"),
                    "case {case}: a quota AWS publishes as fixed cannot be increased"
                );
            }
        }
    }
}

/// The whole published catalog comes out of the enumeration however it is paged, and the
/// applied listing enumerates the same keys.
#[tokio::test]
async fn the_cloudwatch_catalog_survives_every_page_size() {
    let engine = emulator();
    let caller = client(&engine, "us-east-1");

    let whole = defaults(&caller, CLOUDWATCH).await;

    // The lower bound of the modelled `MaxResults` @range 1..100, two sizes that divide
    // this catalog unevenly, and the upper bound.
    for page_size in [1_i64, 2, 5, 100] {
        let walked = walk_pages(
            &caller,
            "ListAWSDefaultServiceQuotas",
            &json!({"ServiceCode": CLOUDWATCH}),
            "Quotas",
            Some(page_size),
        )
        .await;
        assert_eq!(
            walked, whole,
            "case {CLOUDWATCH} MaxResults={page_size}: expected the whole published catalog, got \
             {} of {} rows",
            walked.len(),
            whole.len()
        );
    }

    let applied = walk_pages(
        &caller,
        "ListServiceQuotas",
        &json!({"ServiceCode": CLOUDWATCH}),
        "Quotas",
        Some(2),
    )
    .await;
    assert_eq!(
        codes_of(&applied),
        codes_of(&whole),
        "case {CLOUDWATCH}: both listings must publish the same keys"
    );
}

/// A code this catalog publishes nothing under is still refused, so a client cannot
/// mistake a fabricated row for a published limit.
#[tokio::test]
async fn a_cloudwatch_code_nothing_publishes_is_refused() {
    let engine = emulator();
    let caller = client(&engine, "us-west-1");

    // Rows: a well-formed code in AWS's namespace this catalog publishes nothing under,
    // and a code derived from a CloudWatch quota NAME.
    for (quota_code, why) in [
        ("L-00000000", "a well-formed code nothing publishes"),
        ("AlarmsPerRegion", "a code derived from a quota name"),
    ] {
        for operation in ["GetAWSDefaultServiceQuota", "GetServiceQuota"] {
            let (status, error) = caller
                .refused(
                    operation,
                    json!({"ServiceCode": CLOUDWATCH, "QuotaCode": quota_code}),
                )
                .await;
            assert_eq!(
                (status, error.as_str()),
                (400, "NoSuchResourceException"),
                "case {CLOUDWATCH}/{quota_code} ({why}): {operation} must refuse it"
            );
        }
    }
}
