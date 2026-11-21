// SPDX-License-Identifier: CC0-1.0

//! What `ListAWSDefaultServiceQuotas` publishes for AWS Key Management Service, and the
//! COMPLETENESS of that enumeration.
//!
//! THE RULE: the default-quota listing of a `ServiceCode` carries ONE row per quota the
//! service publishes, each under AWS's own `QuotaCode`, and each row carries that
//! quota's own `QuotaName`, `Value`, `Unit`, `Adjustable`, `GlobalQuota`, `Period`,
//! `UsageMetric` and `Description` — never a neighbouring quota's. The enumeration
//! yields every published row however the caller pages it, and each row read back under
//! its own code is the record the listing served, on both read planes, which is what
//! makes `QuotaCode` the join key between the catalog and the increase request.
//!
//! The rows below span what the rule generalises over: the two KINDS of quota KMS
//! publishes (a counted resource plane with no window and no usage metric, and the
//! per-operation REQUEST RATE with both), published rates from 5 to 2000 requests per
//! second, adjustable rows and a row AWS publishes as fixed, and three operations whose
//! names share the `List` prefix so a row cannot be matched by its name alone. Two of
//! them (`ListKeys`, `UpdateCustomKeyStore`) are rows no issue reported missing, so the
//! rule is asserted past the cited cases.
//!
//! SOURCE for every expectation: the recorded `ListAWSDefaultServiceQuotas` response of
//! `kms`, which states each row's `QuotaCode`, `QuotaName`, default `Value`,
//! adjustability, window, usage metric and published sentence; the row tables live in
//! `src/state/quota_rows/kms.rs`, whose doc comments name the per-row sources. The
//! paging rule is asserted on two further service codes whose rows are keyed on AWS's
//! codes as well, so completeness is not a property of one catalog entry.
//!
//! The harness (`client`, `emulator`, `default_quotas`, `walk_pages`) is the one
//! `quota_workflow.rs` owns.

use std::collections::{BTreeSet, HashSet};

use serde_json::{json, Value};

use crate::quota_workflow::{
    client, default_quotas as defaults, emulator, service_inventory, walk_pages,
};

/// The `ServiceCode` whose published catalog this suite pins.
const KMS: &str = "kms";

/// The one label `ListServices` reports for [`KMS`], which every row of its listings
/// carries.
const KMS_SERVICE_NAME: &str = "AWS Key Management Service (AWS KMS)";

/// One quota AWS publishes for Key Management Service.
///
/// `pub(crate)` with the table below, so the suites that assert something about a KMS
/// row read the SAME published facts instead of each carrying a copy that can drift
/// (`default_quota_published_codes.rs` reads the rate rows out of it).
pub(crate) struct PublishedRow {
    pub(crate) quota_code: &'static str,
    pub(crate) quota_name: &'static str,
    pub(crate) value: f64,
    pub(crate) adjustable: bool,
    /// The API operation whose requests the row's rate throttles, for a row of KMS's
    /// per-operation request-rate class; `None` for a row that counts a plane instead,
    /// which publishes neither a window nor a usage metric.
    pub(crate) throttled_api: Option<&'static str>,
    pub(crate) description: &'static str,
}

/// The quotas this catalog publishes for [`KMS`], each under the `QuotaCode` AWS
/// publishes it as.
pub(crate) const KMS_ROWS: &[PublishedRow] = &[
    PublishedRow {
        quota_code: "L-0E66C9C0",
        quota_name: "On-demand rotations per CMK",
        value: 10.0,
        adjustable: false,
        throttled_api: None,
        description: "The maximum number of on-demand rotations permitted for each customer \
                      managed key. This quota does not apply to AWS managed keys.",
    },
    PublishedRow {
        quota_code: "L-171660E4",
        quota_name: "ListKeyRotations request rate",
        value: 100.0,
        adjustable: true,
        throttled_api: Some("ListKeyRotations"),
        description: "Maximum ListKeyRotations requests per second. When you reach this quota, \
                      KMS rejects requests for this operation for the remainder of the interval.",
    },
    PublishedRow {
        quota_code: "L-8A08DAEA",
        quota_name: "ListKeys request rate",
        value: 500.0,
        adjustable: true,
        throttled_api: Some("ListKeys"),
        description: "Maximum ListKeys requests per second. When you reach this quota, KMS \
                      rejects requests for this operation for the remainder of the interval.",
    },
    PublishedRow {
        quota_code: "L-FB6513A2",
        quota_name: "ListResourceTags request rate",
        value: 2000.0,
        adjustable: true,
        throttled_api: Some("ListResourceTags"),
        description: "Maximum ListResourceTags requests per second. When you reach this quota, \
                      KMS rejects requests for this operation for the remainder of the interval.",
    },
    PublishedRow {
        quota_code: "L-275D92F3",
        quota_name: "UpdateCustomKeyStore request rate",
        value: 5.0,
        adjustable: true,
        throttled_api: Some("UpdateCustomKeyStore"),
        description: "Maximum UpdateCustomKeyStore requests per second. When you reach this \
                      quota, KMS rejects requests for this operation for the remainder of the \
                      interval.",
    },
];

/// The whole `ServiceQuota` record the row is published as at `region`, as the default
/// plane reports it.
fn expected_record(row: &PublishedRow, region: &str) -> Value {
    let mut record = json!({
        "ServiceCode": KMS,
        "ServiceName": KMS_SERVICE_NAME,
        // An AWS DEFAULT quota belongs to no account, so its ARN's account segment is
        // empty and the Region is the endpoint the catalog was read at.
        "QuotaArn": format!("arn:aws:servicequotas:{region}::{KMS}/{}", row.quota_code),
        "QuotaCode": row.quota_code,
        "QuotaName": row.quota_name,
        "Value": row.value,
        "Unit": "None",
        "Adjustable": row.adjustable,
        "GlobalQuota": false,
        "Description": row.description,
    });
    if let Some(api) = row.throttled_api {
        record["Period"] = json!({"PeriodValue": 1, "PeriodUnit": "SECOND"});
        record["UsageMetric"] = json!({
            "MetricNamespace": "AWS/Usage",
            "MetricName": "CallCount",
            "MetricStatisticRecommendation": "Sum",
            "MetricDimensions": {
                "Service": "KMS",
                "Resource": api,
                "Type": "API",
                "Class": "None",
            },
        });
    }
    record
}

/// The `QuotaCode`s a listing reports.
fn codes_of(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|quota| quota["QuotaCode"].as_str().map(str::to_string))
        .collect()
}

/// Every quota this catalog publishes for KMS is in the listing, with its own published
/// members, and the listing carries nothing else.
#[tokio::test]
async fn the_default_listing_publishes_every_row_of_the_kms_catalog() {
    let engine = emulator();
    let region = "us-west-2";
    let client = client(&engine, region);

    let listed = defaults(&client, KMS).await;

    for row in KMS_ROWS {
        let case = format!("{KMS}/{}", row.quota_code);
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
        KMS_ROWS
            .iter()
            .map(|row| row.quota_code.to_string())
            .collect(),
        "case {KMS}: expected exactly the published rows"
    );

    // A guard on the TABLE above, not on the emulator: the rows must span both kinds of
    // row and more than one published rate, so the assertions over them are not one shape
    // of row repeated. It fails only if a later edit narrows the table.
    let rates: Vec<&PublishedRow> = KMS_ROWS
        .iter()
        .filter(|row| row.throttled_api.is_some())
        .collect();
    let values: BTreeSet<String> = rates.iter().map(|row| row.value.to_string()).collect();
    assert!(
        rates.len() > 1 && values.len() == rates.len() && rates.len() < KMS_ROWS.len(),
        "case {KMS}: the table must span the rate class and the counted class, got {} rate rows \
         of {} with the values {values:?}",
        rates.len(),
        KMS_ROWS.len()
    );
}

/// A published row is a usable KEY: its code resolves the same record on the default and
/// the applied read plane at every endpoint, and the increase request answers it
/// according to the row's own `Adjustable` flag.
#[tokio::test]
async fn a_published_kms_row_is_the_key_both_planes_and_the_increase_request_answer() {
    let engine = emulator();

    // Two endpoints, because the catalog is published per Region and only the ARN's
    // Region segment may differ between them.
    for region in ["eu-central-1", "ap-southeast-2"] {
        let client = client(&engine, region);
        let listed = defaults(&client, KMS).await;

        for row in KMS_ROWS {
            let case = format!("{region} {KMS}/{}", row.quota_code);
            let pair = json!({"ServiceCode": KMS, "QuotaCode": row.quota_code});

            let default = client.ok("GetAWSDefaultServiceQuota", pair.clone()).await;
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

            let applied = client.ok("GetServiceQuota", pair.clone()).await;
            assert_eq!(
                (
                    &applied["Quota"]["QuotaName"],
                    &applied["Quota"]["Description"],
                    &applied["Quota"]["Value"],
                    &applied["Quota"]["Period"],
                    &applied["Quota"]["UsageMetric"],
                ),
                (
                    &default["Quota"]["QuotaName"],
                    &default["Quota"]["Description"],
                    &default["Quota"]["Value"],
                    &default["Quota"]["Period"],
                    &default["Quota"]["UsageMetric"],
                ),
                "case {case}: the applied plane must report the same published quota, got {applied}"
            );

            let mut increase = pair.clone();
            increase["DesiredValue"] = json!(row.value + 1.0);
            if row.adjustable {
                let requested = client.ok("RequestServiceQuotaIncrease", increase).await
                    ["RequestedQuota"]
                    .clone();
                assert_eq!(
                    (
                        &requested["ServiceCode"],
                        &requested["QuotaCode"],
                        &requested["Status"]
                    ),
                    (&json!(KMS), &json!(row.quota_code), &json!("PENDING")),
                    "case {case}: the request must be recorded against the quota asked for, got \
                     {requested}"
                );
            } else {
                let (status, error) = client.refused("RequestServiceQuotaIncrease", increase).await;
                assert_eq!(
                    (status, error.as_str()),
                    (400, "IllegalArgumentException"),
                    "case {case}: a quota AWS publishes as fixed cannot be increased"
                );
            }
        }
    }
}

/// Every published row comes out of the enumeration however it is paged: the walk yields
/// the same rows, in the same order, with no duplicate and no omission.
///
/// The service codes span catalogs of different sizes and kinds — KMS's metered rates,
/// Amazon MSK's counted resource planes and IAM Access Analyzer's larger set — so the
/// rule is about the enumeration rather than about one catalog entry.
#[tokio::test]
async fn the_whole_published_catalog_survives_every_page_size() {
    let engine = emulator();
    let client = client(&engine, "us-east-1");

    for service_code in [KMS, "kafka", "access-analyzer"] {
        let whole = defaults(&client, service_code).await;
        assert!(
            !whole.is_empty(),
            "case {service_code}: the service must publish its default quotas"
        );

        let mut seen: HashSet<String> = HashSet::new();
        for quota in &whole {
            let code = quota["QuotaCode"]
                .as_str()
                .unwrap_or_else(|| panic!("case {service_code}: a row without a code: {quota}"));
            assert!(
                seen.insert(code.to_string()),
                "case {service_code}: one row per published code, got {code:?} twice"
            );
        }

        // The lower bound of the modelled `MaxResults` @range 1..100, two sizes that
        // divide no catalog evenly, and the upper bound.
        for page_size in [1_i64, 2, 7, 100] {
            let walked = walk_pages(
                &client,
                "ListAWSDefaultServiceQuotas",
                &json!({"ServiceCode": service_code}),
                "Quotas",
                Some(page_size),
            )
            .await;
            assert_eq!(
                walked, whole,
                "case {service_code} MaxResults={page_size}: expected the whole published catalog, \
                 got {} of {} rows",
                walked.len(),
                whole.len()
            );
        }

        // The applied listing enumerates the same keys: a row missing from one plane
        // would be a limit a client cannot reach from the other.
        let applied = walk_pages(
            &client,
            "ListServiceQuotas",
            &json!({"ServiceCode": service_code}),
            "Quotas",
            Some(3),
        )
        .await;
        assert_eq!(
            codes_of(&applied),
            codes_of(&whole),
            "case {service_code}: both listings publish the same keys"
        );
    }
}

/// The listing is reached by the enumeration a client actually drives: `ListServices`
/// names the code, and the code answers with its published rows.
#[tokio::test]
async fn the_kms_catalog_is_reachable_from_the_service_inventory() {
    let engine = emulator();
    let client = client(&engine, "us-west-1");

    let services = service_inventory(&client).await;
    let entry = services
        .iter()
        .find(|service| service["ServiceCode"] == json!(KMS))
        .unwrap_or_else(|| panic!("case {KMS}: the inventory must list the service"));
    assert_eq!(
        entry["ServiceName"],
        json!(KMS_SERVICE_NAME),
        "case {KMS}: expected the published service label, got {entry}"
    );

    let listed = defaults(&client, KMS).await;
    let labels: BTreeSet<&str> = listed
        .iter()
        .filter_map(|quota| quota["ServiceName"].as_str())
        .collect();
    assert_eq!(
        labels,
        BTreeSet::from([KMS_SERVICE_NAME]),
        "case {KMS}: every row carries the one published service label, got {labels:?}"
    );
}

/// A well-formed `QuotaCode` this catalog publishes nothing under is still refused, so a
/// client cannot mistake a fabricated row for a published limit.
#[tokio::test]
async fn a_kms_code_nothing_publishes_is_refused() {
    let engine = emulator();
    let client = client(&engine, "us-east-1");

    // Rows: a well-formed AWS code KMS publishes nothing under, a code another service
    // publishes, and a code derived from a KMS quota NAME.
    for (quota_code, why) in [
        ("L-00000000", "a well-formed code nothing publishes"),
        ("L-F678F1CE", "a code another service publishes"),
        ("OnDemandRotationsPerCMK", "a code derived from a quota name"),
    ] {
        let (status, error) = client
            .refused(
                "GetAWSDefaultServiceQuota",
                json!({"ServiceCode": KMS, "QuotaCode": quota_code}),
            )
            .await;
        assert_eq!(
            (status, error.as_str()),
            (400, "NoSuchResourceException"),
            "case {KMS}/{quota_code} ({why}): expected a refusal"
        );
    }
}
