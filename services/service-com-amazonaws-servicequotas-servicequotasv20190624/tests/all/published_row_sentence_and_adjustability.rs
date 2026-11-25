// SPDX-License-Identifier: CC0-1.0

//! The published SENTENCE and the published ADJUSTABILITY of a default-quota row, over
//! the wire.
//!
//! THE RULE: a row `ListAWSDefaultServiceQuotas` publishes under a `QuotaCode` carries
//! every member AWS publishes for the quota THAT code names — including the
//! `Description` AWS words for it and the `Adjustable` flag AWS publishes for it — and no
//! member is filled from the shape most rows have where the quota's own fact is known. So
//! a row AWS publishes a sentence for publishes that sentence rather than nothing, and a
//! row AWS publishes as fixed is published as fixed rather than taking the adjustable
//! shape of its neighbours.
//!
//! The table spans the class rather than one recorded row: six `ServiceCode` values, one
//! of them (`bedrock-websearch`) a code whose catalog published nothing at all before and
//! one (`cloudwatch`) a sibling plane of a service integrated under another code; units
//! `Count/Second`, `Kilobytes`, `Bytes`, `Megabytes` and the unitless `None`; values from
//! 3 to 100000; rows published both ways round on `Adjustable`; rows with a published
//! sentence and rows with none; and one metered row. Three rows
//! (`airflow-serverless/L-053986BB`, `airflow-serverless/L-E488E4B4` and
//! `aoss/L-2005AF7E`) are quotas this suite's issue never cited.
//!
//! SOURCE for every expectation: the recorded `ListAWSDefaultServiceQuotas` response of
//! each of these service codes — AWS's published default-quota data for the code (what
//! `aws service-quotas list-aws-default-service-quotas --service-code <code>` reports),
//! the same source `src/state/quota_rows/` carries. The length bound a sentence is held
//! to is the model's own: `ServiceQuota$Description` targets `QuotaDescription`, a string
//! with `@length(min: 0, max: 350)` (`smithy_query` query="shape"
//! shape="QuotaDescription").
//!
//! The harness (`client`, `emulator`, `default_quotas`, `walk_pages`) is the one
//! `quota_workflow.rs` owns, and the minted-code shape is `catalog_identity.rs`'s.

use std::collections::BTreeMap;

use serde_json::json;

use crate::catalog_identity::is_aws_minted_shape;
use crate::quota_workflow::{client, default_quotas, emulator, walk_pages, Client};

/// The longest published sentence the model allows (`QuotaDescription` `@length` 0..350).
const DESCRIPTION_MAX: usize = 350;

/// One published row: the quota as AWS publishes it under its own code.
struct Row {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    /// Whether AWS publishes the quota as one an increase may be asked for.
    adjustable: bool,
    /// The sentence AWS publishes for the quota, or `None` where no source states one:
    /// a row whose published sentence is not citable carries no description at all.
    description: Option<&'static str>,
}

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
    }
}

/// The table this suite pins.
const ROWS: &[Row] = &[
    // The serverless Airflow plane: a timeout whose sentence states the measure the row's
    // own `Unit` does not, a per-workflow version count, an account-wide run count, and a
    // kilobyte-measured document size — each with the sentence AWS words for it.
    row(
        "airflow-serverless",
        "L-2C1A393B",
        "Maximum task execution timeout",
        60.0,
        "None",
        true,
        Some("Maximum task execution timeout in minutes"),
    ),
    row(
        "airflow-serverless",
        "L-430BC7B5",
        "Maximum workflow versions per workflow",
        50.0,
        "None",
        true,
        Some("Maximum number of versions that can be created per workflow"),
    ),
    row(
        "airflow-serverless",
        "L-643FB251",
        "Maximum concurrent workflow runs per account",
        100.0,
        "None",
        true,
        Some("Maximum number of concurrent workflow runs per account"),
    ),
    row(
        "airflow-serverless",
        "L-B3B5D226",
        "Maximum workflow definition size",
        50.0,
        "Kilobytes",
        true,
        Some("Maximum workflow definition size in kilobytes"),
    ),
    // Two rows of the same service this suite's issue never cited, whose sentences the
    // catalog already carried: the rule is about every row of the service, not the cited
    // ones.
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
        "L-E488E4B4",
        "Maximum code storage per account",
        76_800.0,
        "Megabytes",
        true,
        Some("Maximum total size (MiB) of workflow code stored per account."),
    ),
    // The geospatial job planes: one sentence per job type, both rows published as fixed
    // at the same value under different codes, so the code is what tells the two apart.
    row(
        "axis",
        "L-DC290044",
        "Maximum concurrent EarthObservationJobs",
        3.0,
        "None",
        false,
        Some(
            "The maximum number of total concurrent EarthObservationJobs per AWS account per \
             region",
        ),
    ),
    row(
        "axis",
        "L-962C3D55",
        "Maximum concurrent VectorEnrichmentJobs",
        3.0,
        "None",
        false,
        Some(
            "The maximum number of total concurrent VectorEnrichmentJobs per AWS account per \
             region",
        ),
    ),
    // The metered row: the CloudWatch Omni credit plane, published as fixed with the
    // sentence that names the month its name names.
    row(
        "cloudwatch",
        "L-111E0C2D",
        "Monthly Omni agent credits",
        100_000.0,
        "None",
        false,
        Some("The maximum number of Omni agent credits per account per region per month."),
    ),
    // The web search tool's query RATE, measured in queries per second: the first row the
    // `bedrock-websearch` code publishes at all. No source states its sentence, so it
    // publishes none — the flag and the sentence are separate facts of a row.
    row(
        "bedrock-websearch",
        "L-39158EC4",
        "Rate of Bedrock Web Search Tool queries",
        10.0,
        "Count/Second",
        true,
        None,
    ),
    // CodeCatalyst's package and team planes: every one published as FIXED, which is the
    // opposite of the shape a row takes when the flag is not stated per row.
    row(
        "codecatalyst",
        "L-861435F6",
        "Number of assets in a version",
        150.0,
        "None",
        false,
        None,
    ),
    row(
        "codecatalyst",
        "L-AF962739",
        "Upstream package repositories searched",
        25.0,
        "None",
        false,
        None,
    ),
    row(
        "codecatalyst",
        "L-B22DB613",
        "Number of users in a team",
        1_000.0,
        "None",
        false,
        None,
    ),
    row(
        "codecatalyst",
        "L-C5F8A35A",
        "Number of direct upstream repositories",
        10.0,
        "None",
        false,
        None,
    ),
    // One further service whose rows are published both ways round, which this suite's
    // issue cited only for its sentences: the two byte-measured policy sizes of one
    // class, published as fixed, each sentence naming its OWN kind of policy.
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
        "L-877395DB",
        "Network policy size per policy",
        10_240.0,
        "Bytes",
        false,
        Some("The maximum size of a network policy in bytes."),
    ),
];

/// The service codes whose whole published set this suite walks.
const SERVICE_CODES: &[&str] = &[
    "airflow-serverless",
    "aoss",
    "axis",
    "bedrock-websearch",
    "cloudwatch",
    "codecatalyst",
];

/// Every row `service_code` publishes, keyed by `QuotaCode`.
async fn rows_by_code(
    caller: &Client,
    service_code: &str,
) -> BTreeMap<String, serde_json::Value> {
    default_quotas(caller, service_code)
        .await
        .into_iter()
        .filter_map(|quota| {
            quota["QuotaCode"]
                .as_str()
                .map(|code| (code.to_string(), quota.clone()))
        })
        .collect()
}

/// RULE: each row reports, under its own code, the sentence and the adjustability AWS
/// publishes for that quota — through the listing and the single default read alike — and
/// the value in force reads the same members while nothing is applied over it.
#[tokio::test]
async fn a_row_carries_the_sentence_and_the_adjustability_aws_publishes_for_its_quota() {
    let engine = emulator();
    let caller = client(&engine, "eu-west-1");

    for row in ROWS {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let pair = json!({"ServiceCode": row.service_code, "QuotaCode": row.quota_code});
        let quota = caller.ok("GetAWSDefaultServiceQuota", pair.clone()).await["Quota"].clone();

        assert_eq!(
            (
                quota["QuotaCode"].as_str(),
                quota["QuotaName"].as_str(),
                quota["Value"].as_f64(),
                quota["Unit"].as_str(),
                quota["Adjustable"].as_bool(),
            ),
            (
                Some(row.quota_code),
                Some(row.quota_name),
                Some(row.value),
                Some(row.unit),
                Some(row.adjustable),
            ),
            "case {case}: expected the facts AWS publishes under this code, got {quota}"
        );
        match row.description {
            Some(sentence) => assert_eq!(
                quota["Description"].as_str(),
                Some(sentence),
                "case {case}: expected the sentence AWS publishes for this quota, got {quota}"
            ),
            None => assert!(
                quota["Description"].is_null(),
                "case {case}: a row whose published sentence is not citable carries none, got \
                 {quota}"
            ),
        }

        // The listing carries exactly the record the single read returned.
        let listed = rows_by_code(&caller, row.service_code).await;
        assert_eq!(
            listed.get(row.quota_code),
            Some(&quota),
            "case {case}: the listing must carry the record the single read returned, got {listed:?}"
        );

        // With nothing applied over it, the value in force agrees on every member the
        // applied view repeats.
        let applied = caller.ok("GetServiceQuota", pair).await["Quota"].clone();
        for member in ["QuotaCode", "QuotaName", "Value", "Unit", "Adjustable"] {
            assert_eq!(
                applied[member], quota[member],
                "case {case}: with nothing applied the value in force must agree on {member}, got \
                 {applied}"
            );
        }
    }
}

/// RULE, as an invariant over each of these services' WHOLE published set: a published
/// sentence is the quota's own — never the empty string, never longer than the model
/// allows, never spread over lines, and never the same sentence as another row of the
/// same service, which is what a sentence bound to the wrong code looks like.
#[tokio::test]
async fn a_published_sentence_is_one_rows_own_and_inside_the_modelled_bound() {
    let engine = emulator();
    let caller = client(&engine, "us-east-2");

    let mut described = 0_usize;
    let mut undescribed = 0_usize;
    for service_code in SERVICE_CODES {
        let listed = default_quotas(&caller, service_code).await;
        assert!(
            !listed.is_empty(),
            "case {service_code}: expected the service's published catalog, got no rows"
        );

        let mut sentences: BTreeMap<String, String> = BTreeMap::new();
        for quota in &listed {
            let code = quota["QuotaCode"].as_str().unwrap_or_default().to_string();
            let case = format!("{service_code}/{code}");
            match quota["Description"].as_str() {
                None => {
                    undescribed += 1;
                    assert!(
                        quota["Description"].is_null(),
                        "case {case}: a row publishes a sentence or none at all, got {quota}"
                    );
                }
                Some(sentence) => {
                    described += 1;
                    assert!(
                        !sentence.is_empty()
                            && sentence.chars().count() <= DESCRIPTION_MAX
                            && !sentence.contains('\n'),
                        "case {case}: a published sentence is one non-empty line of at most \
                         {DESCRIPTION_MAX} characters, got {sentence:?}"
                    );
                    if let Some(other) = sentences.insert(sentence.to_string(), code.clone()) {
                        panic!(
                            "case {case}: a sentence names ONE quota, but {other} publishes it \
                             too: {sentence:?}"
                        );
                    }
                }
            }
        }
    }

    // The sweep must actually span both states the rule distinguishes, or it would pass
    // on a catalog that published no sentence anywhere.
    assert!(
        described >= ROWS.len() / 2 && undescribed >= 1,
        "case the swept services: expected rows both with and without a published sentence, got \
         {described} described and {undescribed} undescribed"
    );
}

/// RULE: a service code whose quotas AWS publishes answers with them. The code whose
/// catalog was empty publishes its row under AWS's own minted code, through the listing
/// however it is paged, and resolves by that code on the single read.
#[tokio::test]
async fn a_newly_published_service_code_answers_with_its_rows() {
    let engine = emulator();
    let caller = client(&engine, "ap-south-1");

    for service_code in SERVICE_CODES {
        let whole = default_quotas(&caller, service_code).await;
        for quota in &whole {
            let code = quota["QuotaCode"].as_str().unwrap_or_default();
            assert!(
                is_aws_minted_shape(code),
                "case {service_code}/{code}: every row of this service is published under the \
                 code AWS mints for the quota, got {quota}"
            );
        }

        // The published set does not depend on how it is paged.
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
