// SPDX-License-Identifier: CC0-1.0

//! The `Description` a default-quota row publishes, and the quota the row belongs to,
//! over the wire.
//!
//! THE RULE: for a `ServiceCode` registered with Service Quotas, every row of `Quotas`
//! reports the quota AWS publishes under THAT row's `QuotaCode` — its `QuotaName`,
//! `Value`, `Unit`, `Adjustable`, `GlobalQuota` and, where AWS publishes one, its
//! `Description` and `Period` — all taken from the same AWS quota as the code that keys
//! the row, never from a neighbouring one. A service that publishes a sentence for its
//! rows publishes it in ONE form per class, worded around that row's own counted plane,
//! so no row carries another row's text.
//!
//! The first table pins the rows no other suite pins, and spans what the rule
//! generalises over rather than one recorded response: seven service codes; sentences in
//! each voice these services publish (`Number of …` with no leading article, `The maximum
//! number of … that you can create in this account in the current Region.` with and
//! without the clause a shared resource adds, the namespace sentence of a scalable
//! target, the per-target sentence closed with a full stop, and the burst-multiplier
//! sentence that states its arithmetic); counts beside a size in kilobytes and a timeout
//! in milliseconds; values from 1 to 1,000,000; adjustable and not-adjustable rows; a row
//! published over a window beside rows published over none; and rows whose published
//! sentence no source available here states, whose other published members the rule binds
//! just the same. Several rows are codes
//! this suite's issue never cited (`appmesh` `L-50F6C35A`, `apprunner` `L-F7ADEB8C`,
//! `application-autoscaling` `L-17D5F681`, `backup` `L-E43E0ED6`, `autoscaling`
//! `L-6B80B8FA`), so the rule is asserted past the rows that motivated it. The rows this
//! change corrected that `default_rows_apigateway_through_codedeploy.rs` already pins —
//! with their windows and usage metrics — stay pinned there, so one row is pinned in one
//! place.
//!
//! The second test is the invariant over the WHOLE catalog of every service code whose
//! rows AWS describes without exception: each row carries a sentence of its own, within
//! the modelled length of `QuotaDescription`, and no two rows of one service share one.
//!
//! SOURCE for every expectation: AWS's published Service Quotas data for that service
//! code, and the recorded `ListAWSDefaultServiceQuotas` response of that code for which
//! `QuotaCode` publishes which quota and for the text, window, unit, limit and
//! adjustability of the rows it states. `smithy_query(query="shape",
//! shape="QuotaDescription")` for the 0..350 length asserted below.
//!
//! The harness (`client`, `emulator`, `default_quotas`) is the one `quota_workflow.rs`
//! owns.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::quota_workflow::{client, default_quotas as defaults, emulator};

/// The modelled maximum length of `ServiceQuota$Description`.
const DESCRIPTION_MAX_LENGTH: usize = 350;

/// One published default-quota row, as the rule says it must be served.
struct Row {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    /// The published sentence, where a source available here states its text. `None`
    /// asserts NOTHING about the member: the report this suite answers says these
    /// services publish a sentence for every row, but no source available here states
    /// this row's, so pinning its absence would pin the gap rather than AWS's behaviour.
    description: Option<&'static str>,
    /// `(PeriodValue, PeriodUnit)` of a quota published over a window.
    period: Option<(i32, &'static str)>,
}

/// The rows this suite pins, one block per service code.
const ROWS: &[Row] = &[
    // apigateway — the three planes this catalog had bound to a neighbouring quota: the
    // size of one request's payload (in megabytes, and not adjustable), the account's
    // client certificates, and the longest an integration may take to answer (in
    // milliseconds). This service publishes no sentence for them.
    Row {
        service_code: "apigateway",
        quota_code: "L-46624B39",
        quota_name: "API Payload Size",
        value: 10.0,
        unit: "Megabytes",
        adjustable: false,
        description: None,
        period: None,
    },
    Row {
        service_code: "apigateway",
        quota_code: "L-824C9E42",
        quota_name: "Client certificates",
        value: 60.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: None,
    },
    Row {
        service_code: "apigateway",
        quota_code: "L-E5AE38E3",
        quota_name: "Maximum integration timeout in milliseconds",
        value: 29_000.0,
        unit: "Milliseconds",
        adjustable: true,
        description: None,
        period: None,
    },
    // appconfig — the environment plane, published over the resource planes' one-hour
    // window, and the size limit of a configuration this service does not host itself.
    Row {
        service_code: "appconfig",
        quota_code: "L-A52E46BE",
        quota_name: "Maximum number of environments per application",
        value: 20.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: Some((1, "HOUR")),
    },
    Row {
        service_code: "appconfig",
        quota_code: "L-A5FC0339",
        quota_name: "Configuration size limit in other configuration stores",
        value: 1024.0,
        unit: "Kilobytes",
        adjustable: true,
        description: None,
        period: None,
    },
    // application-autoscaling — the namespace sentence on a row the issue never cited,
    // the step-adjustment plane this catalog had bound to a scalable-target row, and the
    // per-policy plane published in the per-target voice instead.
    Row {
        service_code: "application-autoscaling",
        quota_code: "L-17D5F681",
        quota_name: "Scalable targets for Comprehend",
        value: 500.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of scalable targets that you can register for the \
             Comprehend namespace in this account in the current Region. A scalable \
             target identifies the resource that Application Auto Scaling can scale.",
        ),
        period: None,
    },
    Row {
        service_code: "application-autoscaling",
        quota_code: "L-9C25247C",
        quota_name: "Step adjustments per step scaling policy",
        value: 20.0,
        unit: "None",
        adjustable: true,
        description: Some("The maximum number of step adjustments per step scaling policy."),
        period: None,
    },
    Row {
        service_code: "application-autoscaling",
        quota_code: "L-B395C81B",
        quota_name: "Scaling policies per scalable target",
        value: 50.0,
        unit: "None",
        adjustable: false,
        description: Some("The maximum number of scaling policies per scalable target."),
        period: None,
    },
    // appmesh — the counted voice with no leading article, on the Envoy plane whose
    // sentence says CONCURRENTLY connected and on a row the issue never cited.
    Row {
        service_code: "appmesh",
        quota_code: "L-606A910B",
        quota_name: "Connected Envoy processes per virtual node",
        value: 50.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "Number of concurrently connected Envoy processes per virtual node",
        ),
        period: None,
    },
    Row {
        service_code: "appmesh",
        quota_code: "L-50F6C35A",
        quota_name: "Virtual routers per mesh",
        value: 200.0,
        unit: "None",
        adjustable: true,
        description: Some("Number of virtual routers per mesh"),
        period: None,
    },
    // apprunner — the create-in-this-account voice, with the clause a resource one
    // service SHARES with another adds to it, on a row the issue never cited and on the
    // configuration row whose sentence the recording states.
    Row {
        service_code: "apprunner",
        quota_code: "L-F7ADEB8C",
        quota_name: "VPC connectors",
        value: 10.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of VPC connectors that you can create in this account in \
             the current Region. You can use a single VPC connector in multiple services.",
        ),
        period: None,
    },
    Row {
        service_code: "apprunner",
        quota_code: "L-A0B46A0C",
        quota_name: "Observability configurations",
        value: 10.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of observability configurations that you can create in \
             this account in the current Region. You can use a single observability \
             configuration in multiple services.",
        ),
        period: None,
    },
    // athena — the burst-multiplier sentence, which names the row's own API in each of
    // its three places.
    Row {
        service_code: "athena",
        quota_code: "L-FE3693EE",
        quota_name: "Burst multiplier quota for ListQueryExecutions API",
        value: 2.0,
        unit: "None",
        adjustable: false,
        description: Some(
            "Multiplier factor to scale burst capacity for ListQueryExecutions API based \
             on the rate quota (Burst = Rate * BurstMultiplierQuota). ListQueryExecutions \
             Burst is the maximum number of ListQueryExecutions API tokens you can \
             accumulate in the account in the region",
        ),
        period: None,
    },
    // autoscaling — the launch-configuration plane, published in the account's voice
    // rather than in this service's per-group one.
    Row {
        service_code: "autoscaling",
        quota_code: "L-6B80B8FA",
        quota_name: "Launch configurations per region",
        value: 200.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of launch configurations allowed for your AWS account",
        ),
        period: None,
    },
    // backup — the counted voice on the largest default in these catalogs, on the plane
    // counted for ONE resource, on the framework controls of the account, on a row the
    // issue never cited, and on the plane published as not adjustable.
    Row {
        service_code: "backup",
        quota_code: "L-514878B6",
        quota_name: "Recovery points per backup vault",
        value: 1_000_000.0,
        unit: "None",
        adjustable: true,
        description: Some("Number of recovery points per backup vault"),
        period: None,
    },
    Row {
        service_code: "backup",
        quota_code: "L-366B61FD",
        quota_name: "Concurrent backup jobs per resource",
        value: 1.0,
        unit: "None",
        adjustable: false,
        description: Some("Number of concurrent backup jobs per resource"),
        period: None,
    },
    Row {
        service_code: "backup",
        quota_code: "L-B4021FB0",
        quota_name: "Framework controls per Region per account",
        value: 50.0,
        unit: "None",
        adjustable: true,
        description: Some("Number of framework controls per Region per account"),
        period: None,
    },
    Row {
        service_code: "backup",
        quota_code: "L-E43E0ED6",
        quota_name: "Frameworks per Region per account",
        value: 10.0,
        unit: "None",
        adjustable: true,
        description: Some("Number of frameworks per Region per account"),
        period: None,
    },
    Row {
        service_code: "backup",
        quota_code: "L-FFD6444F",
        quota_name: "Tags per backup plan",
        value: 50.0,
        unit: "None",
        adjustable: false,
        description: Some("Number of tags per backup plan"),
        period: None,
    },
];

/// The service codes whose EVERY published row AWS describes, which the invariant below
/// walks in full.
const FULLY_DESCRIBED_SERVICES: &[&str] = &[
    "application-autoscaling",
    "appmesh",
    "apprunner",
    "autoscaling",
    "backup",
];

/// Every pinned code reports the quota published under it, sentence and window included,
/// through the listing and through the single default read alike.
#[tokio::test]
async fn a_published_row_carries_the_facts_of_its_own_quota() {
    let engine = emulator();
    let client = client(&engine, "us-east-1");

    let mut listings: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for row in ROWS {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let quota = client
            .ok(
                "GetAWSDefaultServiceQuota",
                json!({"ServiceCode": row.service_code, "QuotaCode": row.quota_code}),
            )
            .await["Quota"]
            .clone();

        assert_eq!(
            (
                quota["QuotaName"].clone(),
                quota["Value"].clone(),
                quota["Unit"].clone(),
                quota["Adjustable"].clone(),
            ),
            (
                json!(row.quota_name),
                json!(row.value),
                json!(row.unit),
                json!(row.adjustable),
            ),
            "case {case}: expected the quota AWS publishes under this code, got {quota}"
        );
        if let Some(description) = row.description {
            assert_eq!(
                quota["Description"],
                json!(description),
                "case {case}: expected this row's own published sentence, got {quota}"
            );
        }
        match row.period {
            Some((value, unit)) => assert_eq!(
                quota["Period"],
                json!({"PeriodValue": value, "PeriodUnit": unit}),
                "case {case}: expected the window this quota is published over, got \
                 {quota}"
            ),
            None => assert!(
                quota.get("Period").is_none_or(Value::is_null),
                "case {case}: this quota is published over no window, got {quota}"
            ),
        }

        let listed = match listings.get(row.service_code) {
            Some(listed) => listed,
            None => {
                let listed = defaults(&client, row.service_code).await;
                listings.entry(row.service_code).or_insert(listed)
            }
        };
        assert!(
            listed.contains(&quota),
            "case {case}: the listing must carry the record the single read returned, \
             got {listed:?}"
        );
    }
}

/// Over the WHOLE catalog of each fully described service: every row publishes a sentence
/// of its own, within the modelled length, and no two rows of one service share one.
#[tokio::test]
async fn every_row_of_a_described_catalog_publishes_its_own_sentence() {
    let engine = emulator();
    let client = client(&engine, "eu-west-2");

    let mut rows = 0_usize;
    for service_code in FULLY_DESCRIBED_SERVICES {
        let listed = defaults(&client, service_code).await;
        assert!(
            !listed.is_empty(),
            "case {service_code}: the service must publish its catalog, got {listed:?}"
        );

        let mut sentences: BTreeSet<String> = BTreeSet::new();
        for quota in &listed {
            let code = quota["QuotaCode"].as_str().unwrap_or_default();
            let case = format!("{service_code}/{code}");
            let description = quota["Description"].as_str().unwrap_or_default();
            assert!(
                (1..=DESCRIPTION_MAX_LENGTH).contains(&description.chars().count()),
                "case {case}: every row of this service publishes a sentence within the \
                 modelled length, got {description:?}"
            );
            assert!(
                sentences.insert(description.to_string()),
                "case {case}: a row's sentence is its own, got a second row published \
                 with {description:?} in {listed:?}"
            );
            rows += 1;
        }
    }

    assert!(
        rows >= 54,
        "case catalog: the invariant must be asserted over every row of the five \
         services, got {rows} rows"
    );
}
