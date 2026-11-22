// SPDX-License-Identifier: CC0-1.0

//! The `glue` default-quota rows whose `QuotaCode` the catalog had bound to a neighbouring
//! quota: the account-wide partition, spare-capacity, machine-learning-concurrency and
//! Data Catalog path planes.
//!
//! THE RULE: for a `ServiceCode`, the row published under a `QuotaCode` carries the
//! `QuotaName`, `Value`, `Unit`, `Adjustable`, `Period`, `UsageMetric` and `Description` of
//! the quota the service publishes UNDER THAT CODE — worded as the service words it, never
//! a paraphrase and never a neighbouring quota's — and a quota whose own code no source
//! states is published under no code at all.
//!
//! The table spans what the rule generalises over rather than repeating one row: an
//! account-wide plane beside the per-resource plane of the same resource (the account's
//! partitions against one table's), a concurrency plane beside the per-resource
//! concurrency plane of the same resource, a description in the service's terse
//! `Catalog: …` form beside its sentence form, a name published WITH a closing period
//! beside names published without one, a row published over no window beside a per-second
//! request rate metered by an API's call count, and values from 3 to 20,000,000.
//!
//! Each row is read BOTH through `ListAWSDefaultServiceQuotas` and through
//! `GetAWSDefaultServiceQuota`, because the code is the only key a single read takes. The
//! harness (`client`, `emulator`, `default_quotas`) is the one `quota_workflow.rs` owns.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::quota_workflow::{client, default_quotas, emulator, Client};

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
/// SOURCE: the recorded `ListAWSDefaultServiceQuotas` response of `glue` states which quota
/// is published under each of these codes together with that quota's own name, value and
/// sentence. The per-resource planes (`L-2C3F5401`, `L-E15CE20A`), the terse Data Catalog
/// row (`L-4F4B522D`) and the request rate (`L-D987EC31`) are codes this change does not
/// rebind, so the rule is asserted beyond the rebound rows as well.
const ROWS: &[Row] = &[
    // The PARTITION plane of the whole account, beside the partition plane of ONE table:
    // the same resource counted on two planes, each under its own code.
    Row {
        quota_code: "L-FEBBFA7A",
        quota_name: "Max partitions per account",
        value: 20_000_000.0,
        unit: "None",
        adjustable: true,
        description: "The maximum number of partitions in your account.",
        period: None,
        call_count: None,
    },
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
    // The machine-learning task runs the ACCOUNT may run at once, beside the runs ONE
    // transform may run at once.
    Row {
        quota_code: "L-83A59AA6",
        quota_name: "Total concurrent machine learning task runs for transforms per account",
        value: 30.0,
        unit: "None",
        adjustable: true,
        description: "The total number of concurrent machine learning transform task runs \
                      for machine learning transforms for this account.",
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
    // A compute-capacity plane whose NAME the service publishes with a closing period,
    // where most of this catalog's names carry none.
    Row {
        quota_code: "L-096DBB95",
        quota_name: "Max spare compute capacity consumed in data processing units (DPUs) per \
                     account.",
        value: 4000.0,
        unit: "None",
        adjustable: true,
        description: "The maximum spare compute capacity in data processing units (DPUs) you \
                      can use concurrently in your account.",
        period: None,
        call_count: None,
    },
    // Two Data Catalog planes, whose sentences the service publishes in its terse form.
    Row {
        quota_code: "L-FB210BD9",
        quota_name: "Global maximum paths",
        value: 30_000.0,
        unit: "None",
        adjustable: true,
        description: "Catalog: global maximum paths",
        period: None,
        call_count: None,
    },
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
    // A per-second request rate metered by the call count of the API it throttles, so the
    // table is not only of planes published over no window.
    Row {
        quota_code: "L-D987EC31",
        quota_name: "Rate of GetPartition requests",
        value: 100.0,
        unit: "None",
        adjustable: true,
        description: "The maximum number of GetPartition requests you can make per second in \
                      this account in the current Region.",
        period: Some((1, "SECOND")),
        call_count: Some("GetPartition"),
    },
];

/// The quotas DISPLACED by the bindings above: each is a quota this catalog used to publish
/// under one of those codes, and no source available here states the code `glue` publishes
/// it under — so it is published under no code of the service at all, by the `QuotaName` it
/// was served as.
const DISPLACED_QUOTA_NAMES: &[&str] = &[
    "Table versions per table",
    "Rate of BatchCreatePartition requests",
    "Rate of UpdatePartition requests",
    "Rate of UpdateJob requests",
];

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

/// Every pinned row carries the facts the service publishes under its own code, through the
/// listing and through the single read alike.
#[tokio::test]
async fn a_glue_account_plane_carries_the_facts_published_under_its_code() {
    let engine = emulator();
    let caller = client(&engine, "us-west-2");

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
async fn a_glue_quota_displaced_from_an_account_plane_code_is_published_under_no_code() {
    let engine = emulator();
    let caller = client(&engine, "eu-central-1");

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
