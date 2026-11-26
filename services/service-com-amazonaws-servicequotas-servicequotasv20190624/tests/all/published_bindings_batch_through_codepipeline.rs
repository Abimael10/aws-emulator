// SPDX-License-Identifier: CC0-1.0

//! Which quota a `QuotaCode` resolves, and in whose words, for the eight service codes
//! whose default-quota catalogs were keyed by hand: `batch`, `cassandra`, `cloud9`,
//! `cloudhsm`, `cloudtrail`, `codeartifact`, `codedeploy` and `codepipeline`.
//!
//! THE RULE: a default-quota row is identified by the `QuotaCode` AWS mints for the quota,
//! and every member of that row — `QuotaName`, `Description`, `Value`, `Unit`,
//! `Adjustable`, `Period` and `UsageMetric` — is the one AWS publishes for THAT quota,
//! in AWS's own words. `QuotaCode` is the only key Service Quotas exposes
//! (`GetAWSDefaultServiceQuota`, `GetServiceQuota` and `RequestServiceQuotaIncrease` all
//! take it), so a row that carries a neighbouring quota's name, ceiling or unit makes every
//! client that resolves a limit by code read the wrong limit.
//!
//! The suite drives the rule over a TABLE whose rows differ in what the rule generalises
//! over rather than in which quota they name: eight services; counted planes, measured
//! sizes (`Bytes`, `Megabytes`, `Gigabytes`), per-second rates that carry a `Period` and an
//! `AWS/Usage` `CallCount` series, a rate that carries a window and no series, lower bounds
//! beside upper ones, adjustable rows beside fixed ones, and values from 1 to 1,000,000.
//! Twelve of its rows name a quota the report that prompted this suite did not cite.
//!
//! Each row is checked through all three readers of a quota — the listing, the single
//! default read and the APPLIED read — because they are three views of one catalog, and a
//! service's voice is checked over EVERY row of that service rather than over the pinned
//! ones, so a re-authored sentence cannot hide on a row no table names.
//!
//! The harness (`client`, `emulator`, `default_quotas`, `default_quotas_by_code`) and the
//! `AWS/Usage` series constructors are the ones `quota_workflow.rs` owns.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::quota_workflow::{
    call_count, client, default_quotas, default_quotas_by_code, emulator, Client, Metric,
};

/// The quotas-page reference Amazon Keyspaces ends its descriptions with.
const KEYSPACES_QUOTAS_PAGE: &str =
    "For more information, see https://docs.aws.amazon.com/keyspaces/latest/devguide/quotas.html";

/// The sentence CloudTrail publishes on every row of its per-API throttle class.
const CLOUDTRAIL_THROTTLE_DESCRIPTION: &str =
    "The maximum number of operation requests you can make per second without being throttled.";

/// One published row: the quota a code resolves and every member AWS publishes for it.
struct Published {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    /// `(PeriodValue, PeriodUnit)` for a quota published over a window.
    period: Option<(i32, &'static str)>,
    /// The `AWS/Usage` series the quota is metered by, where AWS publishes one.
    metric: Option<Metric>,
    description: &'static str,
}

/// The bindings this suite pins.
///
/// SOURCE: AWS's published Service Quotas data for each `ServiceCode` and the recorded
/// default-quota listing of it, which is what fixes WHICH quota a `QuotaCode` names and the
/// words, ceiling and unit that quota is published with.
const PUBLISHED: &[Published] = &[
    // AWS Batch: the job-queue planes and the measured job limits, in Batch's own
    // "Maximum …" voice.
    Published {
        service_code: "batch",
        quota_code: "L-F8102809",
        quota_name: "Compute environments per job queue limit.",
        value: 3.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "Maximum number of compute environments per job queue.",
    },
    Published {
        service_code: "batch",
        quota_code: "L-C997A649",
        quota_name: "Share identifiers per job queue limit.",
        value: 500.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "Maximum number of share identifiers per job queue.",
    },
    Published {
        service_code: "batch",
        quota_code: "L-61E3E54E",
        quota_name: "Service environment",
        value: 50.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "Maximum number of service environments per account, per region.",
    },
    Published {
        service_code: "batch",
        quota_code: "L-65F8BA2C",
        quota_name: "Submitted state jobs limit",
        value: 1_000_000.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "Maximum number of jobs in SUBMITTED state.",
    },
    Published {
        service_code: "batch",
        quota_code: "L-F9381E33",
        quota_name: "Maximum array size limit",
        value: 10_000.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "Maximum array size for array jobs.",
    },
    // Amazon Keyspaces: a throughput ceiling stated in both units per second, and the row
    // size, which is measured in megabytes.
    Published {
        service_code: "cassandra",
        quota_code: "L-6632AD49",
        quota_name: "Max write capacity for a table using change-data-capture (CDC)",
        value: 40_000.0,
        unit: "None",
        adjustable: true,
        period: None,
        metric: None,
        description: "The maximum write throughput, in WCUs and WRUs per second, provisioned \
                      for a table using change-data-capture (CDC). For more information, see \
                      https://docs.aws.amazon.com/keyspaces/latest/devguide/quotas.html",
    },
    Published {
        service_code: "cassandra",
        quota_code: "L-66AD5703",
        quota_name: "Max row size",
        value: 1.0,
        unit: "Megabytes",
        adjustable: true,
        period: None,
        metric: None,
        description: "The maximum size of a row, excluding static column data.",
    },
    Published {
        service_code: "cassandra",
        quota_code: "L-0659E12E",
        quota_name: "Max clustering key size",
        value: 850.0,
        unit: "Bytes",
        adjustable: false,
        period: None,
        metric: None,
        description: "The maximum combined size of all clustering columns in a row. Up to 4 \
                      bytes of additional storage are added to the raw size of each clustering \
                      column for metadata. For more information, see \
                      https://docs.aws.amazon.com/keyspaces/latest/devguide/quotas.html",
    },
    Published {
        service_code: "cassandra",
        quota_code: "L-17766544",
        quota_name: "Table-level read throughput quota",
        value: 40_000.0,
        unit: "None",
        adjustable: true,
        period: None,
        metric: None,
        description: "The maximum read throughput, in RCUs and RRUs per second, provisioned \
                      for a single table. For more information, see \
                      https://docs.aws.amazon.com/keyspaces/latest/devguide/quotas.html",
    },
    // AWS Cloud9: the two planes its environments are counted on, which are described in
    // the voice of the plane they count.
    Published {
        service_code: "cloud9",
        quota_code: "L-5D26553C",
        quota_name: "SSH development environments for this user",
        value: 100.0,
        unit: "None",
        adjustable: true,
        period: None,
        metric: None,
        description: "The maximum number of AWS Cloud9 SSH development environments that you \
                      can create for this user in the current Region.",
    },
    Published {
        service_code: "cloud9",
        quota_code: "L-C1302C17",
        quota_name: "EC2 development environments for this user",
        value: 100.0,
        unit: "None",
        adjustable: true,
        period: None,
        metric: None,
        description: "The maximum number of AWS Cloud9 EC2 development environments that you \
                      can create for this user in the current Region.",
    },
    Published {
        service_code: "cloud9",
        quota_code: "L-A29EDECE",
        quota_name: "SSH development environments for this account",
        value: 200.0,
        unit: "None",
        adjustable: true,
        period: None,
        metric: None,
        description: "The maximum number of AWS Cloud9 SSH development environments that you \
                      can create in this account in the current Region.",
    },
    // AWS CloudHSM: the concurrency and password bounds, and the cluster plane's user
    // count, beside the password LENGTH bound they are read with.
    Published {
        service_code: "cloudhsm",
        quota_code: "L-0B35A0E6",
        quota_name: "Number of concurrent clients",
        value: 900.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The maximum number of concurrent clients per HSM.",
    },
    Published {
        service_code: "cloudhsm",
        quota_code: "L-194EB2B8",
        quota_name: "Minimum length of a password",
        value: 7.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The minimum number of characters for a password.",
    },
    Published {
        service_code: "cloudhsm",
        quota_code: "L-F9BBB650",
        quota_name: "Length of a password",
        value: 32.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The maximum number of characters for a password.",
    },
    Published {
        service_code: "cloudhsm",
        quota_code: "L-F847CBBF",
        quota_name: "Users per CloudHSM cluster",
        value: 1024.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The maximum number of users that you can create in a CloudHSM cluster.",
    },
    // AWS CloudTrail: the trail-logging throttles, published at one transaction per second
    // with the window and the usage series of the API each one throttles.
    Published {
        service_code: "cloudtrail",
        quota_code: "L-9552D44E",
        quota_name: "Transactions per second (TPS) for the StopLogging API",
        value: 1.0,
        unit: "None",
        adjustable: false,
        period: Some((1, "SECOND")),
        metric: Some(call_count("CloudTrail", "StopLogging")),
        description: CLOUDTRAIL_THROTTLE_DESCRIPTION,
    },
    Published {
        service_code: "cloudtrail",
        quota_code: "L-773FB33E",
        quota_name: "Transactions per second (TPS) for the StartLogging API",
        value: 1.0,
        unit: "None",
        adjustable: false,
        period: Some((1, "SECOND")),
        metric: Some(call_count("CloudTrail", "StartLogging")),
        description: CLOUDTRAIL_THROTTLE_DESCRIPTION,
    },
    Published {
        service_code: "cloudtrail",
        quota_code: "L-9387CED7",
        quota_name: "Transactions per second (TPS) for the LookupEvents API",
        value: 2.0,
        unit: "None",
        adjustable: false,
        period: Some((1, "SECOND")),
        metric: Some(call_count("CloudTrail", "LookupEvents")),
        description: CLOUDTRAIL_THROTTLE_DESCRIPTION,
    },
    // AWS CodeArtifact: the asset and repository planes, and the account's own write rate,
    // which is published over a window but metered by no series.
    Published {
        service_code: "codeartifact",
        quota_code: "L-AA0DC56D",
        quota_name: "Asset file size",
        value: 5.0,
        unit: "Gigabytes",
        adjustable: true,
        period: None,
        metric: None,
        description: "The maximum size of an asset file.",
    },
    Published {
        service_code: "codeartifact",
        quota_code: "L-5B2BD2E1",
        quota_name: "Assets per package version",
        value: 350.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The maximum number of assets of a package version.",
    },
    Published {
        service_code: "codeartifact",
        quota_code: "L-D42B1EF2",
        quota_name: "Direct upstreams per repository",
        value: 10.0,
        unit: "None",
        adjustable: true,
        period: None,
        metric: None,
        description: "The maximum number of direct upstream repositories of a repository.",
    },
    Published {
        service_code: "codeartifact",
        quota_code: "L-86608C96",
        quota_name: "Repositories per domain",
        value: 1000.0,
        unit: "None",
        adjustable: true,
        period: None,
        metric: None,
        description: "The maximum number of repositories that can be created in a domain.",
    },
    Published {
        service_code: "codeartifact",
        quota_code: "L-A649E766",
        quota_name: "Write requests per second from a single AWS account",
        value: 100.0,
        unit: "None",
        adjustable: true,
        period: Some((1, "SECOND")),
        metric: None,
        description: "The maximum number of write requests per second from a single AWS \
                      account.",
    },
    // AWS CodeDeploy: the account's instance ceiling, a name's length, and the two rows
    // whose published sentence states what the limit is counted from and what it prevents.
    Published {
        service_code: "codedeploy",
        quota_code: "L-464411D9",
        quota_name: "Number of instances used by concurrent deployments that are in progress \
                     per account",
        value: 1000.0,
        unit: "None",
        adjustable: true,
        period: None,
        metric: None,
        description: "Maximum number of instances used by concurrent deployments that are in \
                      progress per account",
    },
    Published {
        service_code: "codedeploy",
        quota_code: "L-D4ED2A8C",
        quota_name: "Size of deployment group name",
        value: 100.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "Maximum number of characters in the name of a deployment group",
    },
    Published {
        service_code: "codedeploy",
        quota_code: "L-9231961D",
        quota_name: "Minutes until a deployment fails if a lifecycle event doesn't start",
        value: 5.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "Maximum number of minutes until a deployment fails if a lifecycle event \
                      doesn't start after (1) a deployment is triggered by using the console \
                      or the AWS CLI create-deployment command, or (2) the previous lifecycle \
                      event is completed.",
    },
    Published {
        service_code: "codedeploy",
        quota_code: "L-A8B8B32B",
        quota_name: "Concurrent deployments per deployment group",
        value: 1.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "Maximum number of concurrent deployments in a deployment group. This \
                      limit prevents concurrent deployments of the same application to the \
                      same deployment group.",
    },
    // AWS CodePipeline: the per-pipeline action ceiling beside the account's pipeline
    // counts, and a LOWER bound published in the same voice as the upper ones.
    Published {
        service_code: "codepipeline",
        quota_code: "L-1402209C",
        quota_name: "Total actions per pipeline",
        value: 1000.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The maximum number of actions allowed in a pipeline.",
    },
    Published {
        service_code: "codepipeline",
        quota_code: "L-78D200AD",
        quota_name: "Total pipelines",
        value: 1000.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The maximum number of pipelines per region in an AWS account",
    },
    Published {
        service_code: "codepipeline",
        quota_code: "L-C5885C1C",
        quota_name: "Total pipelines with change detection set to periodically checking for \
                     source changes",
        value: 300.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The maximum number of pipelines with change detection set to \
                      periodically checking for source changes per region in an AWS account",
    },
    Published {
        service_code: "codepipeline",
        quota_code: "L-2B3011E2",
        quota_name: "Total parallel actions per stage",
        value: 100.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The maximum number of parallel actions allowed in a stage.",
    },
    Published {
        service_code: "codepipeline",
        quota_code: "L-C57FFB43",
        quota_name: "Minimum actions",
        value: 1.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The minimum number of actions allowed in a stage.",
    },
    Published {
        service_code: "codepipeline",
        quota_code: "L-C8CB041B",
        quota_name: "Minimum stages per pipeline",
        value: 2.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: None,
        description: "The minimum number of stages allowed in a pipeline.",
    },
];

/// The published members of a row, as the wire reports them.
fn members(quota: &Value) -> Value {
    json!({
        "QuotaName": quota["QuotaName"],
        "Value": quota["Value"],
        "Unit": quota["Unit"],
        "Adjustable": quota["Adjustable"],
        "Period": quota["Period"],
        "UsageMetric": quota["UsageMetric"],
        "Description": quota["Description"],
    })
}

/// The members the table says AWS publishes for a row.
fn expected_members(row: &Published) -> Value {
    json!({
        "QuotaName": row.quota_name,
        "Value": row.value,
        "Unit": row.unit,
        "Adjustable": row.adjustable,
        "Period": row
            .period
            .map(|(value, unit)| json!({"PeriodValue": value, "PeriodUnit": unit}))
            .unwrap_or(Value::Null),
        "UsageMetric": row
            .metric
            .as_ref()
            .map(Metric::published)
            .unwrap_or(Value::Null),
        "Description": row.description,
    })
}

/// One `GetAWSDefaultServiceQuota` / `GetServiceQuota` read of a pair.
async fn read(caller: &Client, operation: &str, service_code: &str, quota_code: &str) -> Value {
    caller
        .ok(
            operation,
            json!({"ServiceCode": service_code, "QuotaCode": quota_code}),
        )
        .await["Quota"]
        .clone()
}

/// A `QuotaCode` resolves the quota AWS publishes under it, with that quota's own name,
/// ceiling, unit, adjustability, window, usage series and words — identically through the
/// listing, the single default read and the applied read.
#[tokio::test]
async fn a_quota_code_resolves_the_quota_aws_publishes_under_it() {
    let engine = emulator();
    let caller = client(&engine, "us-east-2");

    // One walk of each service's catalog, shared by every row of that service.
    let mut listings: BTreeMap<&str, BTreeMap<String, Value>> = BTreeMap::new();
    for row in PUBLISHED {
        if !listings.contains_key(row.service_code) {
            listings.insert(
                row.service_code,
                default_quotas_by_code(&caller, row.service_code).await,
            );
        }
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let listing = &listings[row.service_code];
        let listed = listing.get(row.quota_code).unwrap_or_else(|| {
            panic!("case {case}: the service publishes a row under this code, got none")
        });
        let default_read = read(
            &caller,
            "GetAWSDefaultServiceQuota",
            row.service_code,
            row.quota_code,
        )
        .await;
        let applied = read(
            &caller,
            "GetServiceQuota",
            row.service_code,
            row.quota_code,
        )
        .await;

        let expected = expected_members(row);
        for (source, quota) in [
            ("listing", listed),
            ("default read", &default_read),
            ("applied read", &applied),
        ] {
            assert_eq!(
                members(quota),
                expected,
                "case {case}: the {source} must publish the quota AWS publishes under this \
                 code, expected {expected}, got {:?}",
                members(quota)
            );
        }

        // And that quota is published under ONE code: a shifted binding leaves its name on
        // two of them, so a client reading either resolves a ceiling that is not the one it
        // asked for.
        let claimants: Vec<&String> = listing
            .iter()
            .filter(|(_, listed)| listed["QuotaName"] == json!(row.quota_name))
            .map(|(code, _)| code)
            .collect();
        assert_eq!(
            claimants,
            vec![row.quota_code],
            "case {case}: exactly one code publishes {:?}, got {claimants:?}",
            row.quota_name
        );
    }
}

/// The wording each of these services publishes its rows with, over EVERY row of the
/// service rather than over the codes a report named.
///
/// A `Description` is the text a console shows and a client asserts on, so the voice is a
/// published fact of the whole catalog: Cloud9 names the plane the environments are counted
/// on and no other, CloudHSM states what can be CREATED on a plane it provisions,
/// CodeDeploy states every limit as a count, and CodePipeline states what a stage, a
/// pipeline or a condition ALLOWS while counting the account's own planes per Region.
#[tokio::test]
async fn every_row_carries_its_service_published_voice() {
    let engine = emulator();
    let caller = client(&engine, "eu-west-3");

    for quota in default_quotas(&caller, "cloud9").await {
        let case = format!("cloud9/{}", quota["QuotaCode"]);
        let described = quota["Description"].as_str().unwrap_or_default();
        let name = quota["QuotaName"].as_str().unwrap_or_default();
        let expected_plane = if name.ends_with("for this user") {
            "for this user in the current Region."
        } else {
            "in this account in the current Region."
        };
        assert!(
            described.ends_with(expected_plane) && described.contains("AWS Cloud9"),
            "case {case}: a row counted on {expected_plane:?} names the service and that one \
             plane, got {described:?}"
        );
    }

    for quota in default_quotas(&caller, "cloudhsm").await {
        let case = format!("cloudhsm/{}", quota["QuotaCode"]);
        let described = quota["Description"].as_str().unwrap_or_default();
        let name = quota["QuotaName"].as_str().unwrap_or_default();
        // The clusters, HSMs, keys and users of a plane are provisioned by the caller; a
        // password length and a client count are bounds on what is already there.
        let provisioned = ["Clusters", "HSMs", "Keys", "Users"]
            .iter()
            .any(|plane| name.starts_with(plane));
        assert_eq!(
            described.contains("that you can create"),
            provisioned,
            "case {case}: a provisioned plane states what can be created and a bound does \
             not, got {described:?}"
        );
    }

    for quota in default_quotas(&caller, "codedeploy").await {
        let case = format!("codedeploy/{}", quota["QuotaCode"]);
        let described = quota["Description"].as_str().unwrap_or_default();
        assert!(
            described.starts_with("Maximum number of "),
            "case {case}: this service states every limit as a count, got {described:?}"
        );
    }

    for quota in default_quotas(&caller, "codepipeline").await {
        let case = format!("codepipeline/{}", quota["QuotaCode"]);
        let described = quota["Description"].as_str().unwrap_or_default();
        if !described.contains(" allowed in a ") {
            continue;
        }
        assert!(
            ["stage.", "pipeline.", "condition."]
                .iter()
                .any(|plane| described.ends_with(plane)),
            "case {case}: a row of this class names the plane it is allowed in, got \
             {described:?}"
        );
    }

    // Keyspaces refers the reader to its quotas page from every row whose limit that page
    // details, and states a throughput limit in both of the units a table is measured in.
    for quota in default_quotas(&caller, "cassandra").await {
        let case = format!("cassandra/{}", quota["QuotaCode"]);
        let described = quota["Description"].as_str().unwrap_or_default();
        let name = quota["QuotaName"].as_str().unwrap_or_default();
        if !name.contains("throughput") && !name.contains("write capacity") {
            continue;
        }
        assert!(
            described.ends_with(KEYSPACES_QUOTAS_PAGE)
                && (described.contains("RCUs and RRUs per second")
                    || described.contains("WCUs and WRUs per second")),
            "case {case}: a throughput row states both units per second and refers to the \
             service's quotas page, got {described:?}"
        );
    }
}

/// Every per-API throttle CloudTrail publishes meters the API its own name advertises, over
/// the one-second window such a rate is published for.
///
/// The `Resource` dimension is the series CloudWatch publishes that operation's calls
/// under, so a throttle row whose metric meters another operation sends every client that
/// builds an `AWS/Usage` alarm from the catalog to the wrong series.
#[tokio::test]
async fn every_cloudtrail_throttle_meters_its_own_api() {
    let engine = emulator();
    let caller = client(&engine, "ap-south-1");

    let mut throttles = 0_usize;
    for quota in default_quotas(&caller, "cloudtrail").await {
        let case = format!("cloudtrail/{}", quota["QuotaCode"]);
        let name = quota["QuotaName"].as_str().unwrap_or_default();
        let Some(api) = name
            .strip_prefix("Transactions per second (TPS) for the ")
            .and_then(|rest| rest.strip_suffix(" API"))
        else {
            continue;
        };
        throttles += 1;
        assert_eq!(
            (
                quota["UsageMetric"]["MetricName"].clone(),
                quota["UsageMetric"]["MetricDimensions"]["Resource"].clone(),
                quota["Period"].clone(),
                quota["Description"].clone()
            ),
            (
                json!("CallCount"),
                json!(api),
                json!({"PeriodValue": 1, "PeriodUnit": "SECOND"}),
                json!(CLOUDTRAIL_THROTTLE_DESCRIPTION)
            ),
            "case {case}: a throttle of {api} meters that API's calls over one second, got \
             {quota}"
        );
    }
    assert!(
        throttles > 50,
        "CloudTrail's catalog is dominated by per-API throttles, got {throttles}"
    );
}
