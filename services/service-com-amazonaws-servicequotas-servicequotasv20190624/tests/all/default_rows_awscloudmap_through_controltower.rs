// SPDX-License-Identifier: CC0-1.0

//! What the default-quota catalog publishes for the `AWSCloudMap`, `airflow-serverless`,
//! `aoss`, `axis`, `cloudshell`, `cloudwatch`, `codecatalyst`, `cognito-sync`,
//! `comprehendmedical` and `controltower` service codes, over the wire.
//!
//! THE RULE: for a `ServiceCode`, `ListAWSDefaultServiceQuotas` reports one row per
//! quota AWS publishes for that service, keyed on the `QuotaCode` AWS mints for the
//! quota and carrying the facts published UNDER THAT CODE — the quota name as AWS words
//! it, its default value, its unit, whether it is adjustable, whether it is counted
//! account-wide, the window and usage series AWS meters it over where it publishes them,
//! and its published description. A quota whose code or published facts this catalog
//! cannot cite is not published at all, so no row answers to a code outside AWS's minted
//! namespace and a code the catalog publishes nothing under resolves nothing.
//!
//! The table spans the class rather than one recorded row: ten `ServiceCode` values, one
//! of them a sibling plane of a service integrated under another code (`cloudwatch`
//! beside `monitoring`) and two of them codes this suite's issue found already publishing
//! part of their catalog; units `Bytes`, `Kilobytes`, `Megabytes`, `Gigabytes` and the
//! unitless `None`; values from 1 to 100000; rows published both ways round on
//! `Adjustable`; rows with and without a published description; and one metered row that
//! publishes a `Period` and a `UsageMetric` while every other row publishes neither.
//! Two rows (`airflow-serverless/L-053986BB` and `aoss/L-2005AF7E`'s unit) are quotas
//! this suite's issue never cited.
//!
//! SOURCE for every expectation: the recorded `ListAWSDefaultServiceQuotas` response of
//! each of these service codes — AWS's published default-quota data for the code (what
//! `aws service-quotas list-aws-default-service-quotas --service-code <code>` reports),
//! the same source `src/state/quota_rows/` carries — and, for each `ServiceName`, the
//! display name `ListServices` reports for the code.
//!
//! The harness (`client`, `emulator`, `default_quotas`, `service_inventory`,
//! `walk_pages`) is the one `quota_workflow.rs` owns, and the minted-code shape is
//! `catalog_identity.rs`'s.

use serde_json::{json, Value};

use crate::catalog_identity::is_aws_minted_shape;
use crate::quota_workflow::{
    client, default_quotas, emulator, service_inventory, walk_pages, Client,
};

/// One published row: the quota as AWS publishes it under its own code.
struct Row {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    /// The description AWS publishes, or `None` where the evidence states none: a row
    /// whose published sentence is not citable carries no description at all.
    description: Option<&'static str>,
    /// `(PeriodValue, PeriodUnit)` where AWS publishes a window for the quota.
    period: Option<(i64, &'static str)>,
    /// `(MetricName, Service dimension, Resource dimension, Type dimension)` where AWS
    /// publishes a usage series for the quota.
    metric: Option<(&'static str, &'static str, &'static str, &'static str)>,
}

/// A row published with neither a window nor a usage series, which is every row of these
/// services but the metered one.
const fn row(
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    description: Option<&'static str>,
) -> Row {
    Row {
        service_code,
        quota_code,
        quota_name,
        value,
        unit,
        adjustable,
        description,
        period: None,
        metric: None,
    }
}

/// The table this suite pins.
const ROWS: &[Row] = &[
    // Cloud Map publishes the discovery call as two planes — the rate an account may
    // keep up and the burst above it — beside the revision-aware call's own rate and a
    // plain count of namespaces.
    row(
        "AWSCloudMap",
        "L-0BA10AAE",
        "DiscoverInstancesRevision operation per account rate",
        3_000.0,
        "None",
        true,
        None,
    ),
    row(
        "AWSCloudMap",
        "L-0FE3F50E",
        "Namespaces per Region",
        50.0,
        "None",
        true,
        None,
    ),
    row(
        "AWSCloudMap",
        "L-514A639A",
        "DiscoverInstances operation per account steady rate",
        1_000.0,
        "None",
        true,
        None,
    ),
    row(
        "AWSCloudMap",
        "L-76CF203B",
        "DiscoverInstances operation per account burst rate",
        2_000.0,
        "None",
        true,
        None,
    ),
    // The serverless Airflow plane: the XCom size this suite's issue never cited, with
    // its own published sentence, beside a second kilobyte-measured size, a timeout, a
    // per-workflow version count, an account-wide run count and the code storage whose
    // sentence states its measure in mebibytes.
    row(
        "airflow-serverless",
        "L-053986BB",
        "Maximum task instance XCom data size",
        100.0,
        "Kilobytes",
        true,
        Some("Maximum task instance XCom data size in kilobytes"),
    ),
    row(
        "airflow-serverless",
        "L-2C1A393B",
        "Maximum task execution timeout",
        60.0,
        "None",
        true,
        None,
    ),
    row(
        "airflow-serverless",
        "L-430BC7B5",
        "Maximum workflow versions per workflow",
        50.0,
        "None",
        true,
        None,
    ),
    row(
        "airflow-serverless",
        "L-643FB251",
        "Maximum concurrent workflow runs per account",
        100.0,
        "None",
        true,
        None,
    ),
    row(
        "airflow-serverless",
        "L-B3B5D226",
        "Maximum workflow definition size",
        50.0,
        "Kilobytes",
        true,
        None,
    ),
    row(
        "airflow-serverless",
        "L-E488E4B4",
        "Maximum code storage per account",
        76_800.0,
        "Megabytes",
        true,
        Some("Maximum total size (MiB) of workflow code stored per account."),
    ),
    // OpenSearch Serverless: a collection count published as FIXED with its own
    // sentence, two policy-document sizes in bytes, and the two OCU ceilings of its
    // capacity model — one fixed, one adjustable.
    row(
        "aoss",
        "L-1A85D401",
        "Collections per Collection Group (Classic)",
        1_500.0,
        "None",
        false,
        Some("The maximum number of collections per collection group."),
    ),
    row(
        "aoss",
        "L-2005AF7E",
        "Encryption policy size per policy",
        10_240.0,
        "Bytes",
        false,
        Some("The maximum size of an encryption policy in bytes."),
    ),
    row(
        "aoss",
        "L-50FA809B",
        "Default indexing MAX OCU setting",
        10.0,
        "None",
        false,
        None,
    ),
    row(
        "aoss",
        "L-877395DB",
        "Network policy size per policy",
        10_240.0,
        "Bytes",
        false,
        None,
    ),
    row(
        "aoss",
        "L-FBAA6E20",
        "Default indexing MAX Collection Group OCU setting",
        96.0,
        "None",
        true,
        None,
    ),
    // The geospatial job planes, both published as fixed at the same value under
    // different codes: the code is what tells the two apart.
    row(
        "axis",
        "L-962C3D55",
        "Maximum concurrent VectorEnrichmentJobs",
        3.0,
        "None",
        false,
        None,
    ),
    row(
        "axis",
        "L-DC290044",
        "Maximum concurrent EarthObservationJobs",
        3.0,
        "None",
        false,
        None,
    ),
    // CloudShell: a gigabyte-measured directory size and a retention window published as
    // fixed, beside a monthly usage allowance and a concurrency that are adjustable.
    row(
        "cloudshell",
        "L-91D31FC1",
        "Home directory size",
        1.0,
        "Gigabytes",
        false,
        None,
    ),
    row(
        "cloudshell",
        "L-937D704D",
        "Monthly usage",
        200.0,
        "None",
        true,
        None,
    ),
    row(
        "cloudshell",
        "L-AFB6CE78",
        "Data retention",
        120.0,
        "None",
        false,
        None,
    ),
    row(
        "cloudshell",
        "L-C2A56602",
        "Concurrent shells",
        10.0,
        "None",
        true,
        None,
    ),
    // The ONE metered row of this table: the CloudWatch Omni credit plane, published over
    // an hourly window and metered by the credits-used series of its own plane.
    Row {
        service_code: "cloudwatch",
        quota_code: "L-111E0C2D",
        quota_name: "Monthly Omni agent credits",
        value: 100_000.0,
        unit: "None",
        adjustable: false,
        description: None,
        period: Some((1, "HOUR")),
        metric: Some((
            "ResourceCount",
            "CloudWatch Omni",
            "OmniAgentCreditsUsed",
            "Resource",
        )),
    },
    // CodeCatalyst's package and team planes.
    row(
        "codecatalyst",
        "L-861435F6",
        "Number of assets in a version",
        150.0,
        "None",
        true,
        None,
    ),
    row(
        "codecatalyst",
        "L-AF962739",
        "Upstream package repositories searched",
        25.0,
        "None",
        true,
        None,
    ),
    row(
        "codecatalyst",
        "L-B22DB613",
        "Number of users in a team",
        1_000.0,
        "None",
        true,
        None,
    ),
    row(
        "codecatalyst",
        "L-C5F8A35A",
        "Number of direct upstream repositories",
        10.0,
        "None",
        true,
        None,
    ),
    // Cognito Sync: a dataset NAME bounded in bytes and a dataset bounded in megabytes,
    // beside the counts and the wait this service publishes.
    row(
        "cognito-sync",
        "L-239B0C0A",
        "Bulk publish wait time",
        24.0,
        "None",
        false,
        None,
    ),
    row(
        "cognito-sync",
        "L-2625A4A0",
        "Datasets per identity",
        20.0,
        "None",
        true,
        None,
    ),
    row(
        "cognito-sync",
        "L-4CD747A5",
        "Dataset name size",
        128.0,
        "Bytes",
        false,
        None,
    ),
    row(
        "cognito-sync",
        "L-97A9518B",
        "Records per dataset",
        1_024.0,
        "None",
        true,
        None,
    ),
    row(
        "cognito-sync",
        "L-C5EE478A",
        "Dataset size",
        1.0,
        "Megabytes",
        true,
        None,
    ),
    // Comprehend Medical publishes per-operation throughput: transactions per second for
    // its job operations and CHARACTERS per second for a synchronous inference one, with
    // the window stated in the quota's own name rather than in a `Period`.
    row(
        "comprehendmedical",
        "L-23625ADE",
        "Transactions per second (TPS) for the StartSNOMEDCTInferenceJob operation",
        5.0,
        "None",
        false,
        None,
    ),
    row(
        "comprehendmedical",
        "L-2D8DD360",
        "Characters per second (CPS) for the InferICD10CM operation",
        40_000.0,
        "None",
        true,
        None,
    ),
    row(
        "comprehendmedical",
        "L-3645AB89",
        "Transactions per second (TPS) for the StopSNOMEDCTInferenceJob operation",
        5.0,
        "None",
        true,
        None,
    ),
    row(
        "comprehendmedical",
        "L-63264B74",
        "Transactions per second (TPS) for the StartPHIDetectionJob operation",
        5.0,
        "None",
        true,
        None,
    ),
    // Control Tower's two concurrency planes and its per-OU account count.
    row(
        "controltower",
        "L-04D09B14",
        "Concurrent account operations quota",
        5.0,
        "None",
        true,
        None,
    ),
    row(
        "controltower",
        "L-6579BCE4",
        "Concurrent organization units (OUs) operations quota",
        1.0,
        "None",
        false,
        None,
    ),
    row(
        "controltower",
        "L-E9464183",
        "Number of accounts in a single OU quota",
        1_000.0,
        "None",
        false,
        None,
    ),
];

/// The `ServiceName` every row of each of these codes carries, as `ListServices` reports
/// the display name of the code.
const SERVICE_NAMES: &[(&str, &str)] = &[
    ("AWSCloudMap", "AWS Cloud Map"),
    ("airflow-serverless", "Amazon MWAA Serverless (airflow-serverless)"),
    ("aoss", "Amazon OpenSearch Serverless"),
    // The display name the inventory reports for this code is another service's code
    // rather than a product name, and that is still the name its rows carry.
    ("axis", "sagemaker-geospatial"),
    ("cloudshell", "AWS CloudShell"),
    ("cloudwatch", "Amazon CloudWatch Omni"),
    ("codecatalyst", "Amazon CodeCatalyst"),
    ("cognito-sync", "Amazon Cognito Sync"),
    ("comprehendmedical", "Amazon Comprehend Medical"),
    ("controltower", "AWS Control Tower"),
];

/// `(ServiceCode, QuotaCode, why)` triples in the published GAP: codes whose quota these
/// services' recorded listings name but whose published facts this catalog cannot cite,
/// so nothing may be served under them.
const GAP_QUOTA_CODES: &[(&str, &str, &str)] = &[
    (
        "airflow-serverless",
        "L-D905802C",
        "an AWS code whose published quota name and value this catalog carries none of",
    ),
    (
        "aoss",
        "L-1D4405ED",
        "an AWS code whose published SAML-provider count this catalog carries no value for",
    ),
    (
        "AWSCloudMap",
        "L-2DA90E5C",
        "an AWS code whose published per-service instance count this catalog carries no \
         value for",
    ),
    (
        "codecatalyst",
        "L-1EE0DDBE",
        "an AWS code whose published SSO-group count this catalog carries no value for",
    ),
];

/// Every `QuotaCode` `service_code` publishes.
async fn listed_codes(caller: &Client, service_code: &str) -> Vec<String> {
    default_quotas(caller, service_code)
        .await
        .iter()
        .filter_map(|quota| quota["QuotaCode"].as_str().map(str::to_string))
        .collect()
}

/// RULE: a published row reports the facts AWS publishes under its own code — through
/// the single default read and the listing alike — and the value in force reads the same
/// while nothing is applied over it.
#[tokio::test]
async fn a_published_row_carries_the_facts_aws_publishes_under_its_code() {
    let engine = emulator();
    let region = "eu-central-1";
    let caller = client(&engine, region);

    for row in ROWS {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let pair = json!({"ServiceCode": row.service_code, "QuotaCode": row.quota_code});
        let quota = caller.ok("GetAWSDefaultServiceQuota", pair.clone()).await["Quota"].clone();

        assert_eq!(
            (
                quota["ServiceCode"].as_str(),
                quota["QuotaCode"].as_str(),
                quota["QuotaName"].as_str(),
                quota["Value"].as_f64(),
                quota["Unit"].as_str(),
                quota["Adjustable"].as_bool(),
                quota["GlobalQuota"].as_bool(),
            ),
            (
                Some(row.service_code),
                Some(row.quota_code),
                Some(row.quota_name),
                Some(row.value),
                Some(row.unit),
                Some(row.adjustable),
                Some(false),
            ),
            "case {case}: expected the facts AWS publishes under this code, got {quota}"
        );
        assert_eq!(
            quota["QuotaArn"],
            json!(format!(
                "arn:aws:servicequotas:{region}::{}/{}",
                row.service_code, row.quota_code
            )),
            "case {case}: a default quota's ARN names the Region and no account, got {quota}"
        );
        match row.description {
            Some(description) => assert_eq!(
                quota["Description"],
                json!(description),
                "case {case}: a description is the quota's own published text, got {quota}"
            ),
            None => assert_eq!(
                quota["Description"],
                Value::Null,
                "case {case}: a row whose published text is not citable carries none, got {quota}"
            ),
        }
        match row.period {
            Some((value, unit)) => assert_eq!(
                quota["Period"],
                json!({"PeriodValue": value, "PeriodUnit": unit}),
                "case {case}: expected the window AWS publishes the quota over, got {quota}"
            ),
            None => assert_eq!(
                quota["Period"],
                Value::Null,
                "case {case}: a quota AWS publishes no window for reports none, got {quota}"
            ),
        }
        match row.metric {
            Some((metric_name, usage_service, resource, usage_type)) => assert_eq!(
                (
                    quota["UsageMetric"]["MetricNamespace"].as_str(),
                    quota["UsageMetric"]["MetricName"].as_str(),
                    quota["UsageMetric"]["MetricDimensions"]["Service"].as_str(),
                    quota["UsageMetric"]["MetricDimensions"]["Resource"].as_str(),
                    quota["UsageMetric"]["MetricDimensions"]["Type"].as_str(),
                    quota["UsageMetric"]["MetricDimensions"]["Class"].as_str(),
                ),
                (
                    Some("AWS/Usage"),
                    Some(metric_name),
                    Some(usage_service),
                    Some(resource),
                    Some(usage_type),
                    Some("None"),
                ),
                "case {case}: expected the series the quota's usage is published in, got {quota}"
            ),
            None => assert_eq!(
                quota["UsageMetric"],
                Value::Null,
                "case {case}: a quota whose usage series this catalog cannot cite reports no \
                 metric, got {quota}"
            ),
        }

        // The listing carries exactly the record the single read returned, so a client
        // that enumerates the service and one that resolves the code see one quota.
        let listed = default_quotas(&caller, row.service_code).await;
        assert!(
            listed.contains(&quota),
            "case {case}: the listing must carry the record the single read returned, got \
             {listed:?}"
        );

        // With nothing applied over it, the value in force is the published default.
        let applied = caller.ok("GetServiceQuota", pair).await["Quota"].clone();
        for member in ["QuotaCode", "QuotaName", "Value", "Unit", "Adjustable"] {
            assert_eq!(
                applied[member], quota[member],
                "case {case}: with nothing applied the value in force must agree on {member}, \
                 got {applied}"
            );
        }
    }
}

/// RULE: every row of these services carries the `ServiceName` the inventory reports for
/// its code, and every row sits in AWS's minted `L-` namespace — so a client that
/// resolves a code out of one of these listings never reads one AWS would reject.
#[tokio::test]
async fn every_row_names_its_own_service_under_a_code_aws_mints() {
    let engine = emulator();
    let caller = client(&engine, "us-west-2");

    let inventory = service_inventory(&caller).await;
    for (service_code, service_name) in SERVICE_NAMES {
        let reported = inventory
            .iter()
            .find(|service| service["ServiceCode"].as_str() == Some(service_code))
            .unwrap_or_else(|| panic!("case {service_code}: the code must be in the inventory"));
        assert_eq!(
            reported["ServiceName"],
            json!(service_name),
            "case {service_code}: expected the display name AWS reports for the code, got \
             {reported}"
        );

        let listed = default_quotas(&caller, service_code).await;
        assert!(
            !listed.is_empty(),
            "case {service_code}: expected the service's published catalog, got no rows"
        );
        for quota in &listed {
            let code = quota["QuotaCode"].as_str().unwrap_or_default();
            assert!(
                is_aws_minted_shape(code),
                "case {service_code}/{code}: every row of this service is published under the \
                 code AWS mints for the quota, got {quota}"
            );
            assert_eq!(
                quota["ServiceName"],
                json!(service_name),
                "case {service_code}/{code}: a row names its own service, got {quota}"
            );
        }
    }
}

/// RULE: a code in the published gap resolves NOTHING — it is absent from the listing and
/// refused by both single reads — while the service code itself keeps serving its
/// catalog.
#[tokio::test]
async fn a_code_in_the_published_gap_resolves_nothing() {
    let engine = emulator();
    let caller = client(&engine, "us-east-1");

    for (service_code, quota_code, why) in GAP_QUOTA_CODES {
        let case = format!("{service_code}/{quota_code}");
        let codes = listed_codes(&caller, service_code).await;
        assert!(
            !codes.contains(&(*quota_code).to_string()),
            "case {case} ({why}): a code in the gap must not be listed, got {codes:?}"
        );
        assert!(
            !codes.is_empty(),
            "case {case}: the service must still serve its published catalog, got no rows"
        );

        for operation in ["GetAWSDefaultServiceQuota", "GetServiceQuota"] {
            let (status, error) = caller
                .refused(
                    operation,
                    json!({"ServiceCode": service_code, "QuotaCode": quota_code}),
                )
                .await;
            assert_eq!(
                (status, error.as_str()),
                (400, "NoSuchResourceException"),
                "case {case} ({why}): {operation} must refuse a quota this service does not \
                 publish"
            );
        }
    }
}

/// RULE: two codes of one product family publish their own quotas. The CloudWatch Omni
/// plane and Amazon CloudWatch are separate service codes, so neither listing carries a
/// row of the other and a code of one resolves nothing under the other.
#[tokio::test]
async fn a_sibling_code_never_publishes_the_other_codes_rows() {
    let engine = emulator();
    let caller = client(&engine, "ap-southeast-2");

    let omni = listed_codes(&caller, "cloudwatch").await;
    let cloudwatch = listed_codes(&caller, "monitoring").await;
    assert!(
        !omni.is_empty() && !cloudwatch.is_empty(),
        "case cloudwatch family: both codes must publish a catalog, got {omni:?} and \
         {cloudwatch:?}"
    );

    for (code, own, sibling) in [
        ("cloudwatch", &omni, &cloudwatch),
        ("monitoring", &cloudwatch, &omni),
    ] {
        for quota_code in sibling.iter() {
            assert!(
                !own.contains(quota_code),
                "case {code}/{quota_code}: a code publishes its own quotas only, got {own:?}"
            );
            let (status, error) = caller
                .refused(
                    "GetAWSDefaultServiceQuota",
                    json!({"ServiceCode": code, "QuotaCode": quota_code}),
                )
                .await;
            assert_eq!(
                (status, error.as_str()),
                (400, "NoSuchResourceException"),
                "case {code}/{quota_code}: a sibling's quota code must resolve nothing here"
            );
        }
    }
}

/// RULE: the published set does not depend on how it is paged — the same rows, in the
/// same order, whatever page size the walk asks for.
#[tokio::test]
async fn the_published_set_is_the_same_however_it_is_paged() {
    let engine = emulator();
    let caller = client(&engine, "ap-northeast-1");

    for (service_code, _) in SERVICE_NAMES {
        let whole = default_quotas(&caller, service_code).await;
        for size in [1_i64, 3, 100] {
            let paged = walk_pages(
                &caller,
                "ListAWSDefaultServiceQuotas",
                &json!({"ServiceCode": service_code}),
                "Quotas",
                Some(size),
            )
            .await;
            assert_eq!(
                paged,
                whole,
                "case {service_code} MaxResults={size}: paging must yield the one published set \
                 of {} quotas, got {}",
                whole.len(),
                paged.len()
            );
        }
    }
}
