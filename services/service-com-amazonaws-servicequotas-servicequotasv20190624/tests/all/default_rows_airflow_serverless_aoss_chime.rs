// SPDX-License-Identifier: CC0-1.0

//! What the default-quota catalog publishes for the `airflow-serverless`, `aoss`,
//! `chime` and `airflow` service codes, over the wire.
//!
//! THE RULE: for a `ServiceCode`, `ListAWSDefaultServiceQuotas` reports one row per
//! quota AWS publishes for that service, each keyed on the `QuotaCode` AWS mints for
//! the quota and carrying the facts published UNDER THAT CODE — the quota name as AWS
//! words it, its default value, its unit, whether it is adjustable, whether it is
//! counted account-wide, and its published description. A quota whose code or published
//! facts this catalog cannot cite is not published at all, so no row of such a service
//! answers to a code outside AWS's minted namespace, and a client that resolves a code
//! out of the listing reads the same record back through the single reads.
//!
//! The table spans the class rather than one recorded row: rows differ in `ServiceCode`
//! (four of them, two of which are sibling codes of one product family), in `Unit`
//! (`Kilobytes`, `Megabytes`, `Bytes` and the unitless `None`), in the magnitude of the
//! published value (from 1 to 76800), in adjustability, and in whether a description is
//! published. Two rows (`chime/L-FE9F896D` and `airflow/L-67D41C6B`) are quotas the
//! issue behind this suite never cited.
//!
//! SOURCE for every expectation: the recorded `ListAWSDefaultServiceQuotas` response of
//! each of these service codes — AWS's published default-quota data for the code (what
//! `aws service-quotas list-aws-default-service-quotas --service-code <code>` reports),
//! the same source `src/state.rs::seed_quota_catalog` carries.
//!
//! The harness (`client`, `emulator`, `default_quotas`, `walk_pages`) is the one
//! `quota_workflow.rs` owns.

use serde_json::json;
use service_com_amazonaws_servicequotas_servicequotasv20190624::state::descriptive_quota_code;

// The minted-code rule lives in `catalog_identity.rs`, the catalog walks in
// `quota_workflow.rs`, so this suite carries no second copy of either.
use crate::catalog_identity::is_aws_minted_shape;
use crate::quota_workflow::{client, default_quotas, emulator, walk_pages, Client};

/// One published row: the quota as AWS publishes it under its own code.
struct Row {
    service_code: &'static str,
    /// The display name the inventory gives the code, which every row of the service
    /// repeats as its `ServiceName`.
    service_name: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    /// The description AWS publishes, or `None` where the evidence states none: a row
    /// whose published sentence is not citable carries no description at all.
    description: Option<&'static str>,
}

/// The table this suite pins.
const ROWS: &[Row] = &[
    // The serverless Airflow plane: a size quota measured in kilobytes, whose published
    // sentence states the measure in words, beside a storage quota measured in
    // megabytes that publishes no sentence.
    Row {
        service_code: "airflow-serverless",
        service_name: "Amazon MWAA Serverless (airflow-serverless)",
        quota_code: "L-053986BB",
        quota_name: "Maximum task instance XCom data size",
        value: 100.0,
        unit: "Kilobytes",
        adjustable: true,
        description: Some("Maximum task instance XCom data size in kilobytes"),
    },
    Row {
        service_code: "airflow-serverless",
        service_name: "Amazon MWAA Serverless (airflow-serverless)",
        quota_code: "L-E488E4B4",
        quota_name: "Maximum code storage per account",
        value: 76_800.0,
        unit: "Megabytes",
        adjustable: true,
        description: Some("Maximum total size (MiB) of workflow code stored per account."),
    },
    // The definition DOCUMENT of a workflow, the second kilobyte-measured size of this
    // service's catalog, beside the account's run plane — a plain count of the same
    // magnitude as the version plane, which is what keeps a row's facts tied to its own
    // code rather than to a neighbour's.
    Row {
        service_code: "airflow-serverless",
        service_name: "Amazon MWAA Serverless (airflow-serverless)",
        quota_code: "L-B3B5D226",
        quota_name: "Maximum workflow definition size",
        value: 50.0,
        unit: "Kilobytes",
        adjustable: true,
        description: None,
    },
    Row {
        service_code: "airflow-serverless",
        service_name: "Amazon MWAA Serverless (airflow-serverless)",
        quota_code: "L-643FB251",
        quota_name: "Maximum concurrent workflow runs per account",
        value: 100.0,
        unit: "None",
        adjustable: true,
        description: None,
    },
    // OpenSearch Serverless: a plain count of one capacity model's collections, and a
    // policy-document size published in bytes and as NOT adjustable.
    Row {
        service_code: "aoss",
        service_name: "Amazon OpenSearch Serverless",
        quota_code: "L-1A85D401",
        quota_name: "Collections per Collection Group (Classic)",
        value: 1_500.0,
        unit: "None",
        adjustable: false,
        description: Some("The maximum number of collections per collection group."),
    },
    Row {
        service_code: "aoss",
        service_name: "Amazon OpenSearch Serverless",
        quota_code: "L-2005AF7E",
        quota_name: "Encryption policy size per policy",
        value: 10_240.0,
        unit: "Bytes",
        adjustable: false,
        description: Some("The maximum size of an encryption policy in bytes."),
    },
    // The second policy-size plane of the same service, and the two OCU ceilings of its
    // capacity model — one published as fixed, the other as adjustable, so one service's
    // rows differ in adjustability rather than carrying one flag throughout.
    Row {
        service_code: "aoss",
        service_name: "Amazon OpenSearch Serverless",
        quota_code: "L-877395DB",
        quota_name: "Network policy size per policy",
        value: 10_240.0,
        unit: "Bytes",
        adjustable: false,
        description: None,
    },
    Row {
        service_code: "aoss",
        service_name: "Amazon OpenSearch Serverless",
        quota_code: "L-50FA809B",
        quota_name: "Default indexing MAX OCU setting",
        value: 10.0,
        unit: "None",
        adjustable: false,
        description: None,
    },
    Row {
        service_code: "aoss",
        service_name: "Amazon OpenSearch Serverless",
        quota_code: "L-FBAA6E20",
        quota_name: "Default indexing MAX Collection Group OCU setting",
        value: 96.0,
        unit: "None",
        adjustable: true,
        description: None,
    },
    // Chime's rates: the voice plane's calls per second (adjustable) and the meetings
    // plane's management rate (fixed), each published under the SDK-prefixed name AWS
    // words it with, plus one burst allowance this suite's issue never cited.
    Row {
        service_code: "chime",
        service_name: "Amazon Chime",
        quota_code: "L-B6F2C623",
        quota_name: "Amazon Chime SDK SIP trunking and voice - calls per second (CPS)",
        value: 1.0,
        unit: "None",
        adjustable: true,
        description: None,
    },
    Row {
        service_code: "chime",
        service_name: "Amazon Chime",
        quota_code: "L-F147F728",
        quota_name: "Amazon Chime SDK meetings - All meeting management API rate in \
                     requests per second",
        value: 10.0,
        unit: "None",
        adjustable: false,
        description: None,
    },
    Row {
        service_code: "chime",
        service_name: "Amazon Chime",
        quota_code: "L-FE9F896D",
        quota_name: "CreateMeeting API burst rate",
        value: 20.0,
        unit: "None",
        adjustable: true,
        description: None,
    },
    // The sibling code of the serverless Airflow plane, whose own environment count is
    // published with the sentence that service words it with.
    Row {
        service_code: "airflow",
        service_name: "Amazon Managed Workflows for Apache Airflow",
        quota_code: "L-67D41C6B",
        quota_name: "Environments per account per Region (small, medium, large)",
        value: 10.0,
        unit: "None",
        adjustable: true,
        description: Some("The maximum number of environments per account per Region."),
    },
];

/// The service codes whose whole published set this suite walks.
const SERVICE_CODES: &[&str] = &["airflow-serverless", "aoss", "chime", "airflow"];

/// Every published row of `service_code`, keyed by `QuotaCode`.
async fn listed_codes(caller: &Client, service_code: &str) -> Vec<String> {
    default_quotas(caller, service_code)
        .await
        .iter()
        .filter_map(|quota| quota["QuotaCode"].as_str().map(str::to_string))
        .collect()
}

/// Each row reports the facts AWS publishes under its own code, identically through the
/// listing and the single default read, and the value in force reads the same while
/// nothing is applied over it.
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
                quota["ServiceName"].as_str(),
                quota["QuotaCode"].as_str(),
                quota["QuotaName"].as_str(),
                quota["Value"].as_f64(),
                quota["Unit"].as_str(),
                quota["Adjustable"].as_bool(),
                quota["GlobalQuota"].as_bool(),
            ),
            (
                Some(row.service_code),
                Some(row.service_name),
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
            None => assert!(
                quota["Description"].is_null(),
                "case {case}: a row whose published text is not citable carries none, got {quota}"
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

/// Every row these services publish sits in AWS's minted `L-` namespace, and nothing
/// answers to the descriptive code a quota's own name would yield: a client never reads
/// a code AWS would reject out of one of these listings.
#[tokio::test]
async fn these_services_publish_only_codes_aws_mints() {
    let engine = emulator();
    let caller = client(&engine, "us-west-2");

    for service_code in SERVICE_CODES {
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
        }
    }

    for row in ROWS {
        let stand_in = descriptive_quota_code(row.quota_name);
        let case = format!("{}/{stand_in}", row.service_code);
        let codes = listed_codes(&caller, row.service_code).await;
        assert!(
            !codes.contains(&stand_in),
            "case {case}: a code built from the quota's name is not one AWS publishes, got \
             {codes:?}"
        );
        for operation in ["GetAWSDefaultServiceQuota", "GetServiceQuota"] {
            let (status, error) = caller
                .refused(
                    operation,
                    json!({"ServiceCode": row.service_code, "QuotaCode": stand_in.clone()}),
                )
                .await;
            assert_eq!(
                (status, error.as_str()),
                (400, "NoSuchResourceException"),
                "case {case}: {operation} must refuse a code the service publishes nothing under"
            );
        }
    }
}

/// Two codes of one product family publish their own quotas: neither serves a row of
/// the other, and a code of one resolves nothing under the other.
#[tokio::test]
async fn a_sibling_code_never_publishes_the_other_codes_rows() {
    let engine = emulator();
    let caller = client(&engine, "ap-southeast-2");

    let serverless = listed_codes(&caller, "airflow-serverless").await;
    let managed = listed_codes(&caller, "airflow").await;
    assert!(
        !serverless.is_empty() && !managed.is_empty(),
        "case airflow family: both codes must publish a catalog, got {serverless:?} and \
         {managed:?}"
    );

    for (code, own, sibling) in [
        ("airflow-serverless", &serverless, &managed),
        ("airflow", &managed, &serverless),
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

/// The published set does not depend on how it is paged: the same rows, in the same
/// order, whatever page size the walk asks for.
#[tokio::test]
async fn the_published_set_is_the_same_however_it_is_paged() {
    let engine = emulator();
    let caller = client(&engine, "ap-northeast-1");

    for service_code in SERVICE_CODES {
        let whole = default_quotas(&caller, service_code).await;
        for size in [1_i64, 2, 100] {
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
