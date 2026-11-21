// SPDX-License-Identifier: CC0-1.0

//! The facts a default-quota row carries under the `QuotaCode` the recording binds it
//! to, for the rows of these seven catalogs whose recorded text and numbers are stated.
//!
//! THE RULE: `ListAWSDefaultServiceQuotas` returns one row per `QuotaCode` the service
//! publishes, and the row under a code carries THAT code's `QuotaName`, `Value`, `Unit`,
//! `Adjustable`, `Period`, `UsageMetric` and `Description` — the sentence as the service
//! words it, never a paraphrase and never a neighbouring quota's — while a quota whose
//! own code no source states is published under NO code at all.
//!
//! Two halves of that rule are asserted, both over the served response rather than over
//! the seed table:
//!
//! * every pinned `(ServiceCode, QuotaCode)` pair reports the quota the recording
//!   publishes under it, member by member;
//! * every quota DISPLACED by one of those bindings — the quota the catalog used to
//!   publish under that code, whose own code no source states — appears under no code of
//!   its service, so the two quotas never trade places again.
//!
//! The table spans what the rule generalises over: a per-API request rate (`events`
//! `PutEvents`) and three data-plane rates published over a one-second window
//! (`firehose`), counted planes with no window (`fms`, `fsx`, `gamelift`, `glue`,
//! `imagebuilder`), a plane published in a unit of its own (`glue` label file size, in
//! megabytes), rows published WITHOUT a description because no source states their
//! sentence (`events`, `glue`), a name published without a closing period (`fsx`), values
//! from 20 to 500,000 and both adjustable and not-adjustable rows.
//!
//! SOURCE for every row: the recorded `ListAWSDefaultServiceQuotas` response of that
//! `ServiceCode`, as the project's issue record quotes it — which states which quota each
//! code publishes and the text of the members pinned here.
//!
//! The harness (`client`, `emulator`, `default_quotas`) is the one `quota_workflow.rs`
//! owns.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::quota_workflow::{client, default_quotas, emulator, Client};

/// One row of a recorded catalog: the quota a `QuotaCode` publishes.
struct Row {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    /// The published sentence, where a source states its text; `None` pins that the row
    /// is published with NO description rather than one the catalog words itself.
    description: Option<&'static str>,
    /// `(PeriodValue, PeriodUnit)` for a row published over a window.
    period: Option<(i32, &'static str)>,
}

/// The rows the recording binds to these codes.
const ROWS: &[Row] = &[
    // events — the event-INGEST rate and the API-destination plane of an account, under
    // the codes the recording binds them to, plus the policy-size sentence as the
    // service words it.
    Row {
        service_code: "events",
        quota_code: "L-9B653E91",
        quota_name: "PutEvents throttle limit in transactions per second",
        value: 10_000.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: Some((1, "SECOND")),
    },
    Row {
        service_code: "events",
        quota_code: "L-FB1C3A6D",
        quota_name: "Api destinations",
        value: 3000.0,
        unit: "None",
        adjustable: true,
        description: Some("The maximum number of API destinations per account per Region."),
        period: None,
    },
    Row {
        service_code: "events",
        quota_code: "L-FC354966",
        quota_name: "Event bus policy size",
        value: 10_240.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "Maximum policy size, in characters. This policy size increases each time you \
             grant access to another account. You can see your current policy and its size \
             by using the DescribeEventBus API.",
        ),
        period: None,
    },
    // firehose — the three data-plane rates of ONE delivery stream, each published over
    // the one-second window of its class and with the sentence the recording states.
    Row {
        service_code: "firehose",
        quota_code: "L-17D4EADE",
        quota_name: "Rate of records",
        value: 500_000.0,
        unit: "None",
        adjustable: false,
        description: Some(
            "The maximum capacity in records per second for a delivery stream in the \
             current Region.",
        ),
        period: Some((1, "SECOND")),
    },
    Row {
        service_code: "firehose",
        quota_code: "L-7681C790",
        quota_name: "Rate of Put requests",
        value: 2000.0,
        unit: "None",
        adjustable: false,
        description: Some(
            "The maximum number of combined PutRecord and PutRecordBatch requests per \
             second for a delivery stream in the current Region.",
        ),
        period: Some((1, "SECOND")),
    },
    Row {
        service_code: "firehose",
        quota_code: "L-937C879B",
        quota_name: "Rate of data",
        value: 5.0,
        unit: "None",
        adjustable: false,
        description: Some(
            "The maximum capacity in mebibyte per second for a delivery stream in the \
             current Region.",
        ),
        period: Some((1, "SECOND")),
    },
    // fms — the organizational units ONE policy may have in scope, under a code this
    // catalog had bound to an API rate, and the policy-scope sentences as the service
    // words them.
    Row {
        service_code: "fms",
        quota_code: "L-C80E7B9E",
        quota_name: "Organizational units in scope per policy per Region",
        value: 20.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of organizational units that can be in scope of a Firewall \
             Manager policy for any Region.",
        ),
        period: None,
    },
    Row {
        service_code: "fms",
        quota_code: "L-1608788B",
        quota_name: "Partner rule groups per AWS WAF policy",
        value: 1.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of partner rule groups that can be applied to a single \
             policy, limited by Web Application Firewall WebACL limits.",
        ),
        period: None,
    },
    Row {
        service_code: "fms",
        quota_code: "L-25F99602",
        quota_name: "Amazon VPC instances in scope of a common security group policy",
        value: 100.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of Amazon VPC instances that you can have in scope per \
             Firewall Manager common security group policy per account. This number \
             represents the combined count of VPCs that you own and VPCs that are shared \
             with you.",
        ),
        period: None,
    },
    // fsx — the Lustre persistent_1 file-system plane, whose name the service publishes
    // WITHOUT a closing period, and two capacity sentences as it words them.
    Row {
        service_code: "fsx",
        quota_code: "L-9AFA1F09",
        quota_name: "Lustre Persistent_1 file systems",
        value: 100.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of Amazon FSx for Lustre persistent_1 file systems that you \
             can create in this account",
        ),
        period: None,
    },
    Row {
        service_code: "fsx",
        quota_code: "L-15D9FE87",
        quota_name: "Lustre Cache_1 storage capacity",
        value: 100_800.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum amount of storage capacity (in GiB) that you can configure in this \
             account for all Amazon File Cache caches with cache type Lustre and deployment \
             type Cache_1.",
        ),
        period: None,
    },
    Row {
        service_code: "fsx",
        quota_code: "L-FD89CA8A",
        quota_name: "Windows throughput capacity",
        value: 10_240.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The total amount of throughput capacity (in MBps) allowed for all Amazon FSx \
             for Windows file systems in this account.",
        ),
        period: None,
    },
    // gamelift — the game servers of ONE game server group, under a code this catalog
    // had bound to the matchmaking rule-set plane, and a timeout published in seconds.
    Row {
        service_code: "gamelift",
        quota_code: "L-51AF299A",
        quota_name: "Game servers per game server group",
        value: 1000.0,
        unit: "None",
        adjustable: true,
        description: Some("The maximum number of game servers allowed per game server group."),
        period: None,
    },
    Row {
        service_code: "gamelift",
        quota_code: "L-24BE0A39",
        quota_name: "Player session timeout",
        value: 60.0,
        unit: "Seconds",
        adjustable: true,
        description: Some(
            "The maximum time (in seconds) for a reserved player session to timeout pending \
             a valid player connection.",
        ),
        period: None,
    },
    Row {
        service_code: "gamelift",
        quota_code: "L-90D24F1B",
        quota_name: "Builds per region",
        value: 1000.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of game server builds allowed (in any status) per region.",
        ),
        period: None,
    },
    // glue — the task DPUs an account may use at once and its trigger plane, under the
    // codes the recording binds them to, the label-file SIZE in megabytes, and the
    // column-statistics plane with the sentence the recording carries for it.
    Row {
        service_code: "glue",
        quota_code: "L-08F3B322",
        quota_name: "Max task DPUs per account",
        value: 1000.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum compute capacity in data processing units (DPUs) you can use \
             concurrently in your account.",
        ),
        period: None,
    },
    Row {
        service_code: "glue",
        quota_code: "L-F1653A6D",
        quota_name: "Max triggers per account",
        value: 1000.0,
        unit: "None",
        adjustable: true,
        description: Some("The maximum number of triggers in your account."),
        period: None,
    },
    Row {
        service_code: "glue",
        quota_code: "L-B78896B9",
        quota_name: "Label file size",
        value: 10.0,
        unit: "Megabytes",
        adjustable: true,
        description: Some(
            "The maximum file size of an individual label file that can be imported.",
        ),
        period: None,
    },
    Row {
        service_code: "glue",
        quota_code: "L-3890A802",
        quota_name: "Number of column statistics tasks running concurrently per account",
        value: 50.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of column statistics tasks running concurrently in your \
             account.",
        ),
        period: None,
    },
    // imagebuilder — the concurrent resource-state updates of an account and the
    // components of ONE image recipe, under the codes the recording binds them to, and
    // the parameter sentences as the service words them.
    Row {
        service_code: "imagebuilder",
        quota_code: "L-5E848392",
        quota_name: "Concurrent resource state update",
        value: 50.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of concurrent resource state updates that can be in \
             progress in this account in the current Region.",
        ),
        period: None,
    },
    Row {
        service_code: "imagebuilder",
        quota_code: "L-59EC76F6",
        quota_name: "Components per image recipe",
        value: 20.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of components that can be associated with a single image \
             recipe.",
        ),
        period: None,
    },
    Row {
        service_code: "imagebuilder",
        quota_code: "L-E49825CC",
        quota_name: "Parameters per image workflow",
        value: 25.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of parameters that a single image workflow can contain.",
        ),
        period: None,
    },
    Row {
        service_code: "imagebuilder",
        quota_code: "L-A58CFBED",
        quota_name: "Parameters per component",
        value: 25.0,
        unit: "None",
        adjustable: true,
        description: Some("The maximum number of parameters that a single component can contain."),
        period: None,
    },
];

/// The quotas DISPLACED by the bindings above: the quota this catalog used to publish
/// under one of those codes, whose own `QuotaCode` no source states. Each is published
/// under no code of its service — `(ServiceCode, the QuotaName it was published as)`.
const DISPLACED_QUOTA_NAMES: &[(&str, &str)] = &[
    ("events", "RemoveTargets throttle limit in transactions per second"),
    (
        "events",
        "ListTargetsByRule throttle limit in transactions per second",
    ),
    ("fms", "Rate of ListPolicies requests"),
    ("fsx", "ONTAP volumes per file system"),
    ("gamelift", "Matchmaking rule sets"),
    ("glue", "Partitions per table"),
    ("glue", "Connections per account"),
    ("glue", "Rate of StartCrawler requests"),
    ("glue", "Databases per account"),
    ("glue", "Workflows per account"),
    ("glue", "Development endpoints per account"),
    ("glue", "Machine learning transforms per account"),
    ("glue", "Rate of GetTable requests"),
    ("imagebuilder", "Lifecycle policies"),
    ("imagebuilder", "Concurrent image builds"),
];

/// Every pinned catalog, as `ServiceCode -> QuotaCode -> the served row`.
async fn pinned_catalogs(caller: &Client) -> BTreeMap<&'static str, BTreeMap<String, Value>> {
    let mut catalogs: BTreeMap<&'static str, BTreeMap<String, Value>> = BTreeMap::new();
    for service_code in ROWS
        .iter()
        .map(|row| row.service_code)
        .chain(DISPLACED_QUOTA_NAMES.iter().map(|(service, _)| *service))
    {
        if catalogs.contains_key(service_code) {
            continue;
        }
        let listed = default_quotas(caller, service_code).await;
        assert!(
            !listed.is_empty(),
            "case {service_code}: the service publishes a default-quota catalog, got none"
        );
        let mut by_code = BTreeMap::new();
        for quota in listed {
            let code = quota["QuotaCode"]
                .as_str()
                .unwrap_or_else(|| panic!("case {service_code}: a row without a code: {quota}"))
                .to_string();
            by_code.insert(code, quota);
        }
        catalogs.insert(service_code, by_code);
    }
    catalogs
}

/// A row published under a `QuotaCode` carries THAT code's published facts.
#[tokio::test]
async fn a_row_carries_the_facts_published_under_its_quota_code() {
    let engine = emulator();
    let caller = client(&engine, "eu-west-1");
    let catalogs = pinned_catalogs(&caller).await;

    for row in ROWS {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let served = catalogs[row.service_code]
            .get(row.quota_code)
            .unwrap_or_else(|| panic!("case {case}: expected the code to be published, got none"));

        assert_eq!(
            served["QuotaName"],
            json!(row.quota_name),
            "case {case}: expected the name published under the code, {:?}, got {:?}",
            row.quota_name,
            served["QuotaName"]
        );
        assert_eq!(
            served["Value"],
            json!(row.value),
            "case {case}: expected the value published under the code, {}, got {:?}",
            row.value,
            served["Value"]
        );
        assert_eq!(
            served["Unit"],
            json!(row.unit),
            "case {case}: expected the unit published under the code, {:?}, got {:?}",
            row.unit,
            served["Unit"]
        );
        assert_eq!(
            served["Adjustable"],
            json!(row.adjustable),
            "case {case}: expected Adjustable {}, got {:?}",
            row.adjustable,
            served["Adjustable"]
        );
        match row.description {
            Some(description) => assert_eq!(
                served["Description"],
                json!(description),
                "case {case}: expected the sentence the service publishes, {description:?}, got \
                 {:?}",
                served["Description"]
            ),
            None => assert_eq!(
                served["Description"],
                Value::Null,
                "case {case}: a row whose published sentence no source states carries none, got \
                 {:?}",
                served["Description"]
            ),
        }
        match row.period {
            Some((period_value, period_unit)) => assert_eq!(
                served["Period"],
                json!({"PeriodValue": period_value, "PeriodUnit": period_unit}),
                "case {case}: expected the window the quota is published over, \
                 {period_value} {period_unit}, got {:?}",
                served["Period"]
            ),
            None => assert_eq!(
                served["Period"],
                Value::Null,
                "case {case}: a quota published over no window carries no Period, got {:?}",
                served["Period"]
            ),
        }
    }
}

/// A quota whose own `QuotaCode` no source states is published under NO code: the quota
/// a re-bound code displaced never reappears under a neighbouring code.
#[tokio::test]
async fn a_displaced_quota_is_published_under_no_code() {
    let engine = emulator();
    let caller = client(&engine, "ap-northeast-1");
    let catalogs = pinned_catalogs(&caller).await;

    for (service_code, quota_name) in DISPLACED_QUOTA_NAMES {
        let case = format!("{service_code}/{quota_name}");
        let published: Vec<&String> = catalogs[*service_code]
            .iter()
            .filter(|(_, quota)| quota["QuotaName"] == json!(quota_name))
            .map(|(code, _)| code)
            .collect();
        assert!(
            published.is_empty(),
            "case {case}: a quota whose own code no source states is published under no code, \
             got it under {published:?}"
        );
    }
}
