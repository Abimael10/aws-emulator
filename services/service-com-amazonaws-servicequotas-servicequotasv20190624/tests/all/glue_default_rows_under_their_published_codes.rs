// SPDX-License-Identifier: CC0-1.0

//! Which quota the `glue` default-quota catalog publishes under which `QuotaCode`.
//!
//! THE RULE: a row published under a `QuotaCode` carries the `QuotaName`, `Value`, `Unit`,
//! `Adjustable`, `GlobalQuota`, `Period`, `UsageMetric` and `Description` of the quota the
//! service publishes UNDER THAT CODE — the service's own wording, never a paraphrase and
//! never a neighbouring quota's. Two consequences follow, and both are asserted here: a
//! quota whose own code no source states is published under NO code rather than under
//! another quota's, and a code whose quota no source states carries no row at all.
//!
//! The table below spans what the rule generalises over rather than repeating one row: a
//! counted account plane and a counted per-resource plane, a row published in a unit of its
//! own, an adjustable row and a not-adjustable one, a per-second request rate metered by an
//! API's call count beside planes published over no window, a description in the service's
//! terse `Catalog: …` form beside its sentence form, and values from 3 to 10,000,000.
//!
//! Each row is read BOTH through `ListAWSDefaultServiceQuotas` and through
//! `GetAWSDefaultServiceQuota`, because the code is the only key the single reads take.
//! The harness (`client`, `emulator`, `default_quotas`, `walk_pages`) is the one
//! `quota_workflow.rs` owns.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::quota_workflow::{client, default_quotas, emulator, walk_pages, Client};

/// One row of the `glue` catalog, as the service publishes it under its own code.
struct Row {
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    description: &'static str,
    /// `(PeriodValue, PeriodUnit)` for a quota published over a window.
    period: Option<(i32, &'static str)>,
    /// The API whose `AWS/Usage` `CallCount` meters the row, for a request rate.
    call_count: Option<&'static str>,
}

/// The rows this suite pins.
///
/// SOURCE: the recorded `ListAWSDefaultServiceQuotas` response of `glue` states which
/// quota is published under each of these codes together with that quota's name, value and
/// sentence; the per-table partition maximum is the one the "AWS Glue endpoints and
/// quotas" table of the AWS General Reference publishes for that plane. The rows include
/// codes the catalog was NOT filed about (`L-B78896B9`, `L-CB69DFA0`, `L-4F4B522D`,
/// `L-11FA2C1A`) so the rule is asserted beyond the cited cases.
const ROWS: &[Row] = &[
    // The machine-learning planes of an account: how many transforms it may hold, and how
    // many task runs ONE of those transforms may run at once.
    Row {
        quota_code: "L-04CEE988",
        quota_name: "Number of machine learning transforms",
        value: 100.0,
        unit: "None",
        adjustable: true,
        description: "The maximum number of machine learning transforms for this account.",
        period: None,
        call_count: None,
    },
    Row {
        quota_code: "L-E15CE20A",
        quota_name: "Concurrent machine learning task runs per transform",
        value: 3.0,
        unit: "None",
        adjustable: true,
        description: "The maximum number of concurrent task runs per machine learning \
                      transform for this account.",
        period: None,
        call_count: None,
    },
    // A plane counted per TABLE rather than per account, and the largest value this
    // catalog publishes.
    Row {
        quota_code: "L-2C3F5401",
        quota_name: "Max partitions per table",
        value: 10_000_000.0,
        unit: "None",
        adjustable: true,
        description: "The maximum number of partitions per table.",
        period: None,
        call_count: None,
    },
    Row {
        quota_code: "L-4256D6D2",
        quota_name: "Max connection per account",
        value: 1000.0,
        unit: "None",
        adjustable: true,
        description: "The maximum number of connections in your account.",
        period: None,
        call_count: None,
    },
    Row {
        quota_code: "L-F1653A6D",
        quota_name: "Max triggers per account",
        value: 1000.0,
        unit: "None",
        adjustable: true,
        description: "The maximum number of triggers in your account.",
        period: None,
        call_count: None,
    },
    // A CONCURRENCY plane of an account, whose sentence the service words around what runs
    // concurrently rather than around what is held.
    Row {
        quota_code: "L-3890A802",
        quota_name: "Number of column statistics tasks running concurrently per account",
        value: 50.0,
        unit: "None",
        adjustable: true,
        description: "The maximum number of column statistics tasks running concurrently in \
                      your account.",
        period: None,
        call_count: None,
    },
    Row {
        quota_code: "L-11FA2C1A",
        quota_name: "Number of crawlers per account",
        value: 1000.0,
        unit: "None",
        adjustable: true,
        description: "The maximum number of crawlers in your account.",
        period: None,
        call_count: None,
    },
    // A SIZE published in megabytes rather than a count.
    Row {
        quota_code: "L-B78896B9",
        quota_name: "Label file size",
        value: 10.0,
        unit: "Megabytes",
        adjustable: true,
        description: "The maximum file size of an individual label file that can be imported.",
        period: None,
        call_count: None,
    },
    // A per-second request rate the service publishes as NOT adjustable, metered by that
    // API's call count.
    Row {
        quota_code: "L-CB69DFA0",
        quota_name: "Rate of GetJobRun requests",
        value: 100.0,
        unit: "None",
        adjustable: false,
        description: "The maximum number of GetJobRun requests you can make per second in \
                      this account in the current Region.",
        period: Some((1, "SECOND")),
        call_count: Some("GetJobRun"),
    },
    // A Data Catalog plane, whose sentence the service publishes in its terse form.
    Row {
        quota_code: "L-4F4B522D",
        quota_name: "Global s3 path max length",
        value: 2000.0,
        unit: "None",
        adjustable: true,
        description: "Catalog: global s3 path max length",
        period: None,
        call_count: None,
    },
];

/// The quotas DISPLACED by the bindings above: each is a quota this catalog used to publish
/// under one of those codes, and no source available here states the code `glue` publishes
/// it under — so it is published under no code of the service at all, by the `QuotaName` it
/// was served as.
const DISPLACED_QUOTA_NAMES: &[&str] = &[
    "Databases per account",
    "Workflows per account",
    "Development endpoints per account",
    "Machine learning transforms per account",
    "Triggers per account",
    "Partitions per table",
    "Connections per account",
    "Rate of GetTable requests",
    "Rate of StartCrawler requests",
];

/// The `QuotaCode` this catalog used to serve a quota that belongs under another code: no
/// source available here binds any `glue` quota to it, so it carries no row at all rather
/// than a second copy of the quota now published under `L-04CEE988`.
///
/// The rest of the default-quota-set gap (the recorded codes whose quota no source states)
/// is reported by the count equation and the `GAP_CODE_COUNT` tripwire of
/// `aws_minted_catalog_coverage.rs`, so that shortfall is not asserted here as a contract.
const GAP_CODES: &[&str] = &["L-5E4153CA"];

/// The `glue` catalog, keyed by `QuotaCode`, asserting each code is published once.
async fn catalog_by_code(caller: &Client) -> BTreeMap<String, Value> {
    let listed = default_quotas(caller, "glue").await;
    assert!(
        !listed.is_empty(),
        "case glue: the service publishes a default-quota catalog, got none"
    );
    let mut by_code: BTreeMap<String, Value> = BTreeMap::new();
    for quota in listed {
        let code = quota["QuotaCode"]
            .as_str()
            .unwrap_or_else(|| panic!("case glue: a row without a code: {quota}"))
            .to_string();
        if let Some(first) = by_code.insert(code.clone(), quota.clone()) {
            panic!("case glue/{code}: a code is published once, got {first} and {quota}");
        }
    }
    by_code
}

/// Every pinned row carries the facts the service publishes under its own code, through
/// the listing and through the single read alike.
#[tokio::test]
async fn a_glue_row_carries_the_facts_published_under_its_code() {
    let engine = emulator();
    let region = "eu-west-1";
    let caller = client(&engine, region);

    let catalog = catalog_by_code(&caller).await;
    for row in ROWS {
        let case = format!("glue/{}", row.quota_code);
        let listed = catalog
            .get(row.quota_code)
            .unwrap_or_else(|| panic!("case {case}: expected the code to be published"));
        let read = caller
            .ok(
                "GetAWSDefaultServiceQuota",
                json!({"ServiceCode": "glue", "QuotaCode": row.quota_code}),
            )
            .await["Quota"]
            .clone();
        assert_eq!(
            &read, listed,
            "case {case}: the listing carries the record the single read returns, got {read} \
             and {listed}"
        );

        assert_eq!(
            read["QuotaName"],
            json!(row.quota_name),
            "case {case}: expected the quota name published under this code, got {read}"
        );
        assert_eq!(
            read["Description"],
            json!(row.description),
            "case {case}: expected the sentence published under this code, got {read}"
        );
        assert_eq!(
            read["Value"],
            json!(row.value),
            "case {case}: expected the default value published under this code, got {read}"
        );
        assert_eq!(
            read["Unit"],
            json!(row.unit),
            "case {case}: expected the published unit, got {read}"
        );
        assert_eq!(
            read["Adjustable"],
            json!(row.adjustable),
            "case {case}: expected the published adjustability, got {read}"
        );
        assert_eq!(
            read["ServiceCode"],
            json!("glue"),
            "case {case}: a row reports the service it was read for, got {read}"
        );
        match row.period {
            Some((value, unit)) => assert_eq!(
                read["Period"],
                json!({"PeriodValue": value, "PeriodUnit": unit}),
                "case {case}: expected the published window, got {read}"
            ),
            None => assert!(
                read.get("Period").is_none_or(Value::is_null),
                "case {case}: this quota is published over no window, got {read}"
            ),
        }
        match row.call_count {
            Some(api) => assert_eq!(
                read["UsageMetric"]["MetricDimensions"]["Resource"],
                json!(api),
                "case {case}: a rate row is metered by the call count of the API it \
                 throttles, got {read}"
            ),
            None => assert!(
                read.get("UsageMetric").is_none_or(Value::is_null),
                "case {case}: this quota publishes no usage metric, got {read}"
            ),
        }

        // The applied plane is keyed on the same code, so a client that read a code out of
        // the default catalog resolves its value in force under the same quota.
        let applied = caller
            .ok(
                "GetServiceQuota",
                json!({"ServiceCode": "glue", "QuotaCode": row.quota_code}),
            )
            .await["Quota"]
            .clone();
        assert_eq!(
            (applied["QuotaName"].clone(), applied["Value"].clone()),
            (json!(row.quota_name), json!(row.value)),
            "case {case}: the applied read resolves the same quota at its default value, got \
             {applied}"
        );
    }
}

/// A quota whose own `QuotaCode` no source states is published under NO code: neither its
/// name nor its sentence appears anywhere in the catalog.
#[tokio::test]
async fn a_displaced_glue_quota_is_published_under_no_code() {
    let engine = emulator();
    let caller = client(&engine, "us-east-2");

    let catalog = catalog_by_code(&caller).await;
    for name in DISPLACED_QUOTA_NAMES {
        let carried: Vec<&String> = catalog
            .iter()
            .filter(|(_, quota)| quota["QuotaName"] == json!(name))
            .map(|(code, _)| code)
            .collect();
        assert!(
            carried.is_empty(),
            "case glue/{name:?}: a quota whose own code no source states is published under \
             no code, got it under {carried:?}"
        );
    }

    // A sentence belongs to one quota of the service, so no two rows share one: a row that
    // borrowed a neighbour's text would show up here.
    let mut by_description: BTreeMap<&str, &String> = BTreeMap::new();
    for (code, quota) in &catalog {
        let Some(description) = quota["Description"].as_str() else {
            panic!("case glue/{code}: every published row carries its sentence, got {quota}")
        };
        if let Some(first) = by_description.insert(description, code) {
            panic!(
                "case glue/{code}: a published sentence belongs to one quota, got \
                 {description:?} under {code} and under {first}"
            );
        }
    }
}

/// A code whose quota no source states carries no row, and resolves nothing: not in either
/// listing, and not through a read that takes a `QuotaCode`.
#[tokio::test]
async fn a_glue_code_with_no_stated_quota_resolves_nothing() {
    let engine = emulator();
    let caller = client(&engine, "ap-south-1");

    for gap_code in GAP_CODES {
        let case = format!("glue/{gap_code}");
        for operation in ["ListAWSDefaultServiceQuotas", "ListServiceQuotas"] {
            let listed = walk_pages(
                &caller,
                operation,
                &json!({"ServiceCode": "glue"}),
                "Quotas",
                None,
            )
            .await;
            assert!(
                !listed
                    .iter()
                    .any(|quota| quota["QuotaCode"] == json!(gap_code)),
                "case {case}: {operation} publishes no row under a code whose quota no source \
                 states, got one"
            );
        }

        for operation in ["GetAWSDefaultServiceQuota", "GetServiceQuota"] {
            let (status, error) = caller
                .refused(
                    operation,
                    json!({"ServiceCode": "glue", "QuotaCode": gap_code}),
                )
                .await;
            assert_eq!(
                (status, error.as_str()),
                (400, "NoSuchResourceException"),
                "case {case}: {operation} must refuse a code this service publishes nothing \
                 under"
            );
        }
    }
}
