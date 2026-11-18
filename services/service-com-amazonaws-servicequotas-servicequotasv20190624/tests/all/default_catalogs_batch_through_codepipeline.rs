// SPDX-License-Identifier: CC0-1.0

//! What `ListAWSDefaultServiceQuotas` publishes for the eight service codes whose whole
//! default-quota catalog sits in AWS's own `QuotaCode` namespace, over the wire.
//!
//! Every call here is a real `POST /` with `X-Amz-Target: ServiceQuotasV20190624.<Op>`
//! dispatched through the same `MultiServiceRouter` the emulator serves with, over ONE
//! shared engine, so what is exercised is the wire behaviour.
//!
//! `QuotaCode` is the only key Service Quotas exposes — `GetAWSDefaultServiceQuota`,
//! `GetServiceQuota` and `RequestServiceQuotaIncrease` all take it — so the rules asserted
//! here are about the IDENTITY and the COMPLETENESS of a service's published catalog rather
//! than about one recorded listing:
//!
//! * a service's whole published catalog is served, every row keyed on the code AWS mints
//!   for that quota, with the service's own `ServiceCode`/`ServiceName`, a description on
//!   every row and one key per quota;
//! * a code built out of a quota's NAME is published nowhere and resolves nowhere, so a
//!   client cannot read back a quota this emulator invented, while the service code itself
//!   still resolves;
//! * a published row reports the `QuotaName`, `Value`, `Unit`, `Adjustable`, `GlobalQuota`,
//!   `Period` and `UsageMetric` AWS publishes for the quota, with a `QuotaArn` naming the
//!   Region it was read at and no account, identically through the listing and through
//!   `GetAWSDefaultServiceQuota`, and the same key resolves in the APPLIED plane;
//! * a published row carries the quota's own published WORDS — the `QuotaName` and
//!   `Description` AWS publishes under that code — and each service's whole catalog is
//!   described in the voice that service publishes, so no row is a paraphrase of AWS's text
//!   and no row loses the subject (the service's name, the throttled API) out of it;
//! * a `QuotaCode` resolves the quota AWS publishes under it and no other code claims that
//!   quota's name, and a metered rate row meters the API its own name advertises, so a
//!   client that looks a limit up by code cannot read a neighbouring quota's ceiling,
//!   description or usage series.
//!
//! Each rule is driven over a TABLE of the eight `ServiceCode`s, whose catalogs differ in
//! length (5 rows to 72) and in what they publish (job, keyspace, development-environment,
//! HSM-cluster, trail, package-repository, deployment and pipeline planes, per-API
//! throttles, measured sizes and time limits); the pinned rows span counts and measured
//! quotas (`None`, `Bytes`, `Seconds`, `Megabytes`, `Minutes`, `Days`), adjustable and
//! fixed rows, no window / a one-second rate window / the five-minute window a resource
//! count is published over, and unmetered rows beside both `AWS/Usage` metric shapes
//! (`CallCount` and `ResourceCount`). Ten of the sixteen pinned rows name a quota the issue
//! did not cite, so the rule is asserted beyond the reported instances.
//!
//! The harness (`client`, `emulator`, `default_quotas`, `walk_pages`) is the one
//! `quota_workflow.rs` owns, and "is this a code AWS mints" is asked through
//! `catalog_identity.rs`'s single copy of that question.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::catalog_identity::is_aws_minted_shape;
use crate::quota_workflow::{
    client, default_quotas as defaults, emulator, service_inventory, walk_pages, Client,
};

/// One service whose published default-quota catalog is keyed entirely on AWS's codes.
struct Catalog {
    service_code: &'static str,
    /// The display name AWS's own Service Quotas data reports for the code, which every
    /// row of the service repeats as its `ServiceName`.
    service_name: &'static str,
    /// How many quotas the service's published catalog holds.
    quota_count: usize,
    /// Codes no quota of this service is published under: the descriptive stand-ins a
    /// quota name yields (`src/state.rs::descriptive_quota_code`), which AWS publishes
    /// nothing under.
    withdrawn: &'static [&'static str],
}

/// The services this suite covers.
///
/// SOURCE: AWS's published Service Quotas data for each of these service codes (what `aws
/// service-quotas list-aws-default-service-quotas --service-code <code>` reports) and the
/// recorded listing of each — every row of it carries an `L-XXXXXXXX` `QuotaCode`, and the
/// count is the number of rows the service publishes. `withdrawn` names the codes that
/// recording reports as surplus entries, which are exactly the stand-ins this catalog used
/// to key these services' rows on.
const CATALOGS: &[Catalog] = &[
    Catalog {
        service_code: "batch",
        service_name: "AWS Batch",
        quota_count: 15,
        withdrawn: &["JobQueues", "ComputeEnvironments"],
    },
    Catalog {
        service_code: "cassandra",
        service_name: "Amazon Keyspaces (for Apache Cassandra)",
        quota_count: 27,
        withdrawn: &["TablesPerRegion"],
    },
    Catalog {
        service_code: "cloud9",
        service_name: "AWS Cloud9",
        quota_count: 5,
        withdrawn: &["Environments"],
    },
    Catalog {
        service_code: "cloudhsm",
        service_name: "AWS CloudHSM",
        quota_count: 9,
        withdrawn: &["ClustersPerRegion"],
    },
    Catalog {
        service_code: "cloudtrail",
        service_name: "AWS CloudTrail",
        quota_count: 72,
        withdrawn: &["TrailsPerRegion"],
    },
    Catalog {
        service_code: "codeartifact",
        service_name: "AWS CodeArtifact",
        quota_count: 17,
        withdrawn: &["DomainsPerAccount"],
    },
    Catalog {
        service_code: "codedeploy",
        service_name: "AWS CodeDeploy",
        quota_count: 28,
        withdrawn: &["ApplicationsPerRegion", "DeploymentGroupsPerApplication"],
    },
    Catalog {
        service_code: "codepipeline",
        service_name: "AWS CodePipeline",
        quota_count: 29,
        withdrawn: &["PipelinesPerRegion"],
    },
];

/// The `UsageMetric` a pinned row publishes, where it publishes one.
enum Metric {
    /// No usage metric at all.
    None,
    /// The `AWS/Usage` `CallCount` of the API a request rate throttles, under the
    /// `Service` dimension that count is published with.
    Calls {
        usage_service: &'static str,
        api: &'static str,
    },
    /// The `AWS/Usage` `ResourceCount` of a counted plane, under the `Service` dimension
    /// and the `Resource` series that count is published with.
    Resources {
        usage_service: &'static str,
        resource: &'static str,
    },
}

/// One published row, with every member AWS publishes for it.
struct Row {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    /// `(PeriodValue, PeriodUnit)` — the window the quota is published over, for a row
    /// that is published over one.
    period: Option<(i32, &'static str)>,
    metric: Metric,
}

/// The rows this suite pins, two per service so the table spans the eight catalogs rather
/// than repeating one.
///
/// SOURCE: AWS's published Service Quotas data for each `ServiceCode` and the recorded
/// default-quota listing of each code for which `QuotaCode` names which quota. Only six of
/// the sixteen rows name a quota the issue cited.
const ROWS: &[Row] = &[
    Row {
        service_code: "batch",
        quota_code: "L-F9381E33",
        quota_name: "Maximum array size limit",
        value: 10_000.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "batch",
        quota_code: "L-E985971C",
        quota_name: "Transactions per second for SubmitJob limit",
        value: 50.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "cassandra",
        quota_code: "L-0659E12E",
        quota_name: "Max clustering key size",
        value: 850.0,
        unit: "Bytes",
        adjustable: false,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "cassandra",
        quota_code: "L-677FFD22",
        quota_name: "Max TTL",
        value: 630_720_000.0,
        unit: "Seconds",
        adjustable: true,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "cloud9",
        quota_code: "L-65F36924",
        quota_name: "Members per development environment",
        value: 8.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "cloud9",
        quota_code: "L-A29EDECE",
        quota_name: "SSH development environments for this account",
        value: 200.0,
        unit: "None",
        adjustable: true,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "cloudhsm",
        quota_code: "L-4B16B391",
        quota_name: "Clusters per AWS Region and AWS account",
        value: 4.0,
        unit: "None",
        adjustable: true,
        period: Some((5, "MINUTE")),
        metric: Metric::Resources {
            usage_service: "CloudHSM",
            resource: "Cluster",
        },
    },
    Row {
        service_code: "cloudhsm",
        quota_code: "L-F847CBBF",
        quota_name: "Client connections per HSM",
        value: 900.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "cloudtrail",
        quota_code: "L-9387CED7",
        quota_name: "Transactions per second (TPS) for the LookupEvents API",
        value: 2.0,
        unit: "None",
        adjustable: false,
        period: Some((1, "SECOND")),
        metric: Metric::Calls {
            usage_service: "CloudTrail",
            api: "LookupEvents",
        },
    },
    Row {
        service_code: "cloudtrail",
        quota_code: "L-3BB16389",
        quota_name: "Summarized query results size",
        value: 3.0,
        unit: "Megabytes",
        adjustable: false,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "codeartifact",
        quota_code: "L-37894301",
        quota_name: "GetAuthorizationToken requests per second",
        value: 40.0,
        unit: "None",
        adjustable: false,
        period: Some((1, "SECOND")),
        metric: Metric::None,
    },
    Row {
        service_code: "codeartifact",
        quota_code: "L-5D176989",
        quota_name: "Upstream repositories searched",
        value: 25.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "codedeploy",
        quota_code: "L-86A1A2E1",
        quota_name: "Minutes a deployment lifecycle event can run",
        value: 60.0,
        unit: "Minutes",
        adjustable: false,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "codedeploy",
        quota_code: "L-877B748B",
        quota_name: "Event notification triggers in a deployment group",
        value: 10.0,
        unit: "None",
        adjustable: true,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "codepipeline",
        quota_code: "L-8DF1BAAD",
        quota_name: "Maximum timeout for a manual approval action",
        value: 7.0,
        unit: "Days",
        adjustable: false,
        period: None,
        metric: Metric::None,
    },
    Row {
        service_code: "codepipeline",
        quota_code: "L-0097A9B4",
        quota_name: "Active executions per pipeline",
        value: 50.0,
        unit: "None",
        adjustable: false,
        period: None,
        metric: Metric::None,
    },
];

/// The `UsageMetric` a row's [`Metric`] publishes, as the wire reports it.
fn metric_json(metric: &Metric) -> Value {
    match metric {
        Metric::None => Value::Null,
        Metric::Calls { usage_service, api } => json!({
            "MetricNamespace": "AWS/Usage",
            "MetricName": "CallCount",
            "MetricStatisticRecommendation": "Sum",
            "MetricDimensions": {
                "Service": usage_service,
                "Class": "None",
                "Resource": api,
                "Type": "API",
            },
        }),
        Metric::Resources {
            usage_service,
            resource,
        } => json!({
            "MetricNamespace": "AWS/Usage",
            "MetricName": "ResourceCount",
            "MetricStatisticRecommendation": "Maximum",
            "MetricDimensions": {
                "Service": usage_service,
                "Class": "None",
                "Resource": resource,
                "Type": "Resource",
            },
        }),
    }
}

/// Every `QuotaCode` a listing of `service_code` reports.
async fn listed_codes(caller: &Client, operation: &str, service_code: &str) -> BTreeSet<String> {
    walk_pages(
        caller,
        operation,
        &json!({"ServiceCode": service_code}),
        "Quotas",
        None,
    )
    .await
    .iter()
    .filter_map(|quota| quota["QuotaCode"].as_str().map(str::to_string))
    .collect()
}

/// Every covered service serves its whole published catalog, keyed only on codes AWS
/// mints, each row described and reachable by a key of its own.
#[tokio::test]
async fn every_covered_service_serves_its_published_catalog() {
    let engine = emulator();

    // Two endpoints, because the catalog is published per Region and the rule is about
    // the catalog rather than about one Region's view of it.
    for region in ["us-east-2", "eu-west-3"] {
        let caller = client(&engine, region);
        let inventory: BTreeMap<String, String> = service_inventory(&caller)
            .await
            .iter()
            .filter_map(|service| {
                Some((
                    service["ServiceCode"].as_str()?.to_string(),
                    service["ServiceName"].as_str()?.to_string(),
                ))
            })
            .collect();

        for catalog in CATALOGS {
            let case = format!("{region} {}", catalog.service_code);
            let quotas = defaults(&caller, catalog.service_code).await;

            assert_eq!(
                quotas.len(),
                catalog.quota_count,
                "case {case}: expected the whole published catalog ({} quotas), got {} ({:?})",
                catalog.quota_count,
                quotas.len(),
                quotas
                    .iter()
                    .filter_map(|quota| quota["QuotaCode"].as_str())
                    .collect::<Vec<_>>()
            );

            let mut names: BTreeSet<&str> = BTreeSet::new();
            for quota in &quotas {
                let code = quota["QuotaCode"].as_str().unwrap_or_default();
                let row = format!("{case} {code}");
                assert!(
                    is_aws_minted_shape(code),
                    "case {row}: every published row is keyed on the code AWS assigns the \
                     quota, got {code:?} in {quota}"
                );
                assert_eq!(
                    (
                        quota["ServiceCode"].as_str(),
                        quota["ServiceName"].as_str()
                    ),
                    (Some(catalog.service_code), Some(catalog.service_name)),
                    "case {row}: expected the service's own code and published name, got {quota}"
                );
                assert!(
                    !quota["Description"].as_str().unwrap_or_default().is_empty(),
                    "case {row}: every published row carries the quota's description, got \
                     {quota}"
                );
                let name = quota["QuotaName"].as_str().unwrap_or_default();
                assert!(
                    names.insert(name),
                    "case {row}: a quota name identifies one quota of a service, got a second \
                     row named {name:?}"
                );
                assert_eq!(
                    quota["QuotaArn"],
                    json!(format!(
                        "arn:aws:servicequotas:{region}::{}/{code}",
                        catalog.service_code
                    )),
                    "case {row}: a default quota's ARN names the Region and no account, got \
                     {quota}"
                );
            }

            let codes: BTreeSet<&str> = quotas
                .iter()
                .filter_map(|quota| quota["QuotaCode"].as_str())
                .collect();
            assert_eq!(
                codes.len(),
                quotas.len(),
                "case {case}: every published quota has a key of its own, got {codes:?}"
            );

            assert_eq!(
                inventory.get(catalog.service_code).map(String::as_str),
                Some(catalog.service_name),
                "case {case}: ListServices must report the same display name the quotas carry, \
                 got {:?}",
                inventory.get(catalog.service_code)
            );
        }
    }
}

/// A code built out of a quota NAME is published nowhere and resolves nowhere, while the
/// service itself still resolves.
#[tokio::test]
async fn a_code_derived_from_a_quota_name_resolves_nowhere() {
    let engine = emulator();
    let caller = client(&engine, "us-east-1");

    for catalog in CATALOGS {
        for withdrawn in catalog.withdrawn {
            let case = format!("{}/{withdrawn}", catalog.service_code);

            for operation in ["ListAWSDefaultServiceQuotas", "ListServiceQuotas"] {
                let listed = listed_codes(&caller, operation, catalog.service_code).await;
                assert!(
                    !listed.contains(*withdrawn),
                    "case {case}: {operation} must not publish a code derived from a quota \
                     name, got {listed:?}"
                );
            }

            for operation in [
                "GetAWSDefaultServiceQuota",
                "GetServiceQuota",
                "RequestServiceQuotaIncrease",
            ] {
                let mut body =
                    json!({"ServiceCode": catalog.service_code, "QuotaCode": withdrawn});
                if operation == "RequestServiceQuotaIncrease" {
                    body["DesiredValue"] = json!(10_000.0);
                }
                let (status, error) = caller.refused(operation, body).await;
                assert_eq!(
                    (status, error.as_str()),
                    (400, "NoSuchResourceException"),
                    "case {case}: {operation} must refuse a code this service publishes \
                     nothing under"
                );
            }
        }

        // The SERVICE still resolves: it is the invented quota that is unpublished.
        let response = caller
            .ok(
                "ListAWSDefaultServiceQuotas",
                json!({"ServiceCode": catalog.service_code}),
            )
            .await;
        assert!(
            response["Quotas"].is_array(),
            "case {}: the service code must still resolve its listing, got {response}",
            catalog.service_code
        );
    }
}

/// A published row reports the facts AWS publishes for that quota, identically through the
/// listing and the single read, and the applied plane resolves the same key.
#[tokio::test]
async fn a_published_row_reports_the_facts_aws_publishes() {
    let engine = emulator();
    let region = "ap-south-1";
    let caller = client(&engine, region);

    for row in ROWS {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let pair = json!({"ServiceCode": row.service_code, "QuotaCode": row.quota_code});
        let quota = caller.ok("GetAWSDefaultServiceQuota", pair.clone()).await["Quota"].clone();

        assert_eq!(
            quota["QuotaName"],
            json!(row.quota_name),
            "case {case}: expected the published quota name, got {quota}"
        );
        assert_eq!(
            quota["Value"],
            json!(row.value),
            "case {case}: expected the published default value, got {quota}"
        );
        assert_eq!(
            quota["Unit"],
            json!(row.unit),
            "case {case}: expected the published unit, got {quota}"
        );
        assert_eq!(
            quota["Adjustable"],
            json!(row.adjustable),
            "case {case}: expected the published adjustability, got {quota}"
        );
        assert_eq!(
            quota["GlobalQuota"],
            json!(false),
            "case {case}: AWS counts these quotas per Region, got {quota}"
        );
        assert!(
            !quota["Description"].as_str().unwrap_or_default().is_empty(),
            "case {case}: a published row carries the quota's description, got {quota}"
        );
        assert_eq!(
            quota["QuotaArn"],
            json!(format!(
                "arn:aws:servicequotas:{region}::{}/{}",
                row.service_code, row.quota_code
            )),
            "case {case}: an AWS default quota's ARN names the Region and no account, got \
             {quota}"
        );
        match row.period {
            Some((value, unit)) => assert_eq!(
                quota["Period"],
                json!({"PeriodValue": value, "PeriodUnit": unit}),
                "case {case}: expected the window AWS publishes the quota over, got {quota}"
            ),
            None => assert!(
                quota.get("Period").is_none_or(Value::is_null),
                "case {case}: this quota is published over no window, got {quota}"
            ),
        }
        let metric = metric_json(&row.metric);
        if metric.is_null() {
            assert!(
                quota.get("UsageMetric").is_none_or(Value::is_null),
                "case {case}: this quota publishes no usage metric, got {quota}"
            );
        } else {
            assert_eq!(
                quota["UsageMetric"], metric,
                "case {case}: expected the usage metric the service publishes, got {quota}"
            );
        }

        // The listing carries exactly the record the single read returned.
        let listed = defaults(&caller, row.service_code).await;
        assert!(
            listed.contains(&quota),
            "case {case}: the listing must carry the record the single read returned, got \
             {listed:?}"
        );

        // The applied plane is keyed on the same code, so a client that read a code out of
        // the default catalog resolves its value in force too.
        let applied = caller.ok("GetServiceQuota", pair).await["Quota"].clone();
        assert_eq!(
            (
                applied["QuotaCode"].clone(),
                applied["QuotaName"].clone(),
                applied["Value"].clone()
            ),
            (
                json!(row.quota_code),
                json!(row.quota_name),
                json!(row.value)
            ),
            "case {case}: the applied read resolves the same quota at its default value, got \
             {applied}"
        );
    }
}

/// The `QuotaName` and `Description` one published row carries.
///
/// A row's TEXT is the part of it a console shows and a client matches on, so it is pinned
/// beside the numeric facts above rather than left to a "non-empty" check: a paraphrase
/// passes that check and still shows a user words AWS never returns.
struct Text {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    description: &'static str,
}

/// The `Description` every row of AWS CloudTrail's per-API throttle class carries.
///
/// The class describes the throttle rather than the operation, so the sentence names no API
/// and is one string for all of it; the API a row throttles is named by its `QuotaName` and
/// by the `Resource` dimension of its usage metric.
const CLOUDTRAIL_THROTTLE_DESCRIPTION: &str =
    "The maximum number of operation requests you can make per second without being throttled.";

/// The rows whose published text this suite pins, spanning the eight catalogs.
///
/// SOURCE: AWS's published Service Quotas data for each `ServiceCode` and the recorded
/// default-quota listing of each code, which is what fixes both which quota a `QuotaCode`
/// names and the words that quota is published with. Nine of the twenty-two rows name a
/// quota no report cited, so the rule is pinned beyond the reported instances.
const TEXTS: &[Text] = &[
    Text {
        service_code: "batch",
        quota_code: "L-144F0CA5",
        quota_name: "Compute environment limit",
        description: "Maximum number of compute environments per account, per region.",
    },
    Text {
        service_code: "batch",
        quota_code: "L-4CEA37AD",
        quota_name: "Job queue limit",
        description: "Maximum number of job queues per account, per region.",
    },
    Text {
        service_code: "batch",
        quota_code: "L-6B86CF1E",
        quota_name: "Job payload size limit",
        description: "Maximum job payload size (for SubmitJob API operations), measured in KiB.",
    },
    Text {
        service_code: "batch",
        quota_code: "L-80D92D24",
        quota_name: "Service environments per job queue",
        description: "Maximum number of service environments per job queue.",
    },
    Text {
        service_code: "batch",
        quota_code: "L-AD075A76",
        quota_name: "Job dependencies limit",
        description: "Maximum number of job dependencies per job.",
    },
    Text {
        service_code: "batch",
        quota_code: "L-E6A2743D",
        quota_name: "Job definition size limit",
        description: "Maximum job definition size (for RegisterJobDefinition API operations), \
                      measured in KiB.",
    },
    Text {
        service_code: "cassandra",
        quota_code: "L-7A88D508",
        quota_name: "Max number of UDTs per AWS Region",
        description: "The maximum number of UDTs that can be created per region across all \
                      keyspaces.",
    },
    Text {
        service_code: "cassandra",
        quota_code: "L-96FEFC6D",
        quota_name: "Max number of UDTs per table",
        description: "The maximum number of UDTs that a table can reference.",
    },
    Text {
        service_code: "cassandra",
        quota_code: "L-F90953AC",
        quota_name: "Max amount of direct child UDTs per UDT",
        description: "The maximum number of child UDTs supported for a UDT.",
    },
    Text {
        service_code: "cloud9",
        quota_code: "L-65F36924",
        quota_name: "Members per development environment",
        description: "The maximum number of users that can participate in a single AWS Cloud9 \
                      development environment in this account in the current Region.",
    },
    Text {
        service_code: "cloud9",
        quota_code: "L-A29EDECE",
        quota_name: "SSH development environments for this account",
        description: "The maximum number of AWS Cloud9 SSH development environments that you \
                      can create in this account in the current Region.",
    },
    Text {
        service_code: "cloud9",
        quota_code: "L-8ECD6BDB",
        quota_name: "EC2 development environments for this account",
        description: "The maximum number of AWS Cloud9 EC2 development environments that you \
                      can create in this account in the current Region.",
    },
    Text {
        service_code: "cloudhsm",
        quota_code: "L-36814031",
        quota_name: "Keys per CloudHSM cluster",
        description: "The maximum number of keys in a cluster of hsm1.medium HSMs.",
    },
    Text {
        service_code: "cloudhsm",
        quota_code: "L-F9BBB650",
        quota_name: "Length of a password",
        description: "The maximum number of characters for a password.",
    },
    Text {
        service_code: "cloudtrail",
        quota_code: "L-102075A9",
        quota_name: "Transactions per second (TPS) for the GetImport API",
        description: CLOUDTRAIL_THROTTLE_DESCRIPTION,
    },
    Text {
        service_code: "cloudtrail",
        quota_code: "L-A4CD5068",
        quota_name: "Transactions per second (TPS) for the ListImportFailures API",
        description: CLOUDTRAIL_THROTTLE_DESCRIPTION,
    },
    Text {
        service_code: "codeartifact",
        quota_code: "L-3E27C79F",
        quota_name: "PublishPackageVersion requests per second",
        description: "The maximum number of calls that can be made to PublishPackageVersion \
                      per second.",
    },
    Text {
        service_code: "codeartifact",
        quota_code: "L-5D176989",
        quota_name: "Upstream repositories searched",
        description: "The maximum number of upstream repositories searched when resolving a \
                      package.",
    },
    Text {
        service_code: "codeartifact",
        quota_code: "L-BD902DFC",
        quota_name: "Requests without authentication token per IP address",
        description: "The maximum number of requests per second without an authentication \
                      token from a single IP address.",
    },
    Text {
        service_code: "codeartifact",
        quota_code: "L-DD7208D3",
        quota_name: "Domains per AWS account",
        description: "The maximum number of domains that can be created per AWS account.",
    },
    Text {
        service_code: "codedeploy",
        quota_code: "L-877B748B",
        quota_name: "Event notification triggers in a deployment group",
        description: "Maximum number of event notification triggers in a deployment group",
    },
    Text {
        service_code: "codedeploy",
        quota_code: "L-C77AFF36",
        quota_name: "Number of listeners for a traffic route during an Amazon ECS deployment",
        description: "Maximum number of listeners for a traffic route during an Amazon ECS \
                      deployment",
    },
    Text {
        service_code: "codepipeline",
        quota_code: "L-0097A9B4",
        quota_name: "Active executions per pipeline",
        description: "The maximum number of active executions allowed in a PARALLEL execution \
                      mode pipeline.",
    },
    Text {
        service_code: "codepipeline",
        quota_code: "L-FE939BB2",
        quota_name: "Total webhooks",
        description: "The maximum number of webhooks per region in an AWS account",
    },
];

/// Rows the report named as carrying ANOTHER quota's facts: the code, the quota it is
/// published under and the numeric fact that identifies it.
///
/// SOURCE: the recorded default-quota listing of each of these codes. They are the
/// bindings a client resolves a limit through, so they are pinned as a set of their own —
/// one swapped row reports a password length as a subnet count or a per-second API rate as
/// a resource count.
const BINDINGS: &[(&str, &str, &str, f64)] = &[
    ("batch", "L-6B86CF1E", "Job payload size limit", 30.0),
    (
        "batch",
        "L-80D92D24",
        "Service environments per job queue",
        1.0,
    ),
    ("batch", "L-F9381E33", "Maximum array size limit", 10_000.0),
    ("cassandra", "L-7A88D508", "Max number of UDTs per AWS Region", 256.0),
    ("cassandra", "L-96FEFC6D", "Max number of UDTs per table", 50.0),
    ("cloudhsm", "L-F9BBB650", "Length of a password", 32.0),
    (
        "codeartifact",
        "L-BD902DFC",
        "Requests without authentication token per IP address",
        600.0,
    ),
    (
        "codedeploy",
        "L-31E782CD",
        "Number of instances that can be passed to the BatchGetOnPremisesInstances API action",
        100.0,
    ),
    ("codepipeline", "L-519D5A90", "Total custom actions", 50.0),
    ("codepipeline", "L-A0A99E23", "Total stages per pipeline", 50.0),
];

/// Every published row of `service_code`, keyed on its `QuotaCode`.
async fn rows_by_code(caller: &Client, service_code: &str) -> BTreeMap<String, Value> {
    defaults(caller, service_code)
        .await
        .into_iter()
        .filter_map(|quota| Some((quota["QuotaCode"].as_str()?.to_string(), quota)))
        .collect()
}

/// A published row carries the quota's own published WORDS — its `QuotaName` and
/// `Description` as AWS publishes them for that code — through the listing, through the
/// single default read and through the applied read alike.
#[tokio::test]
async fn a_published_row_carries_the_text_aws_publishes() {
    let engine = emulator();
    let caller = client(&engine, "eu-central-1");

    let mut listings: BTreeMap<&str, BTreeMap<String, Value>> = BTreeMap::new();
    for text in TEXTS {
        if !listings.contains_key(text.service_code) {
            listings.insert(
                text.service_code,
                rows_by_code(&caller, text.service_code).await,
            );
        }
        let case = format!("{}/{}", text.service_code, text.quota_code);
        let listed = listings[text.service_code].get(text.quota_code);
        let listed = listed.unwrap_or_else(|| {
            panic!("case {case}: the service must publish a row under this code")
        });

        let pair = json!({"ServiceCode": text.service_code, "QuotaCode": text.quota_code});
        let read = caller.ok("GetAWSDefaultServiceQuota", pair.clone()).await["Quota"].clone();
        let applied = caller.ok("GetServiceQuota", pair).await["Quota"].clone();

        for (source, quota) in [("listing", listed), ("default read", &read)] {
            assert_eq!(
                quota["QuotaName"],
                json!(text.quota_name),
                "case {case}: the {source} must name the quota AWS publishes under this code, \
                 got {quota}"
            );
            assert_eq!(
                quota["Description"],
                json!(text.description),
                "case {case}: the {source} must carry the description AWS publishes for this \
                 quota, got {quota}"
            );
        }
        assert_eq!(
            (
                applied["QuotaName"].clone(),
                applied["Description"].clone()
            ),
            (json!(text.quota_name), json!(text.description)),
            "case {case}: the applied read must carry the same text, got {applied}"
        );
    }
}

/// A row's `QuotaCode` resolves the quota AWS publishes under it: the name and the numeric
/// ceiling a client reads for a code are that quota's own, not a neighbouring row's.
#[tokio::test]
async fn a_quota_code_resolves_the_quota_aws_publishes_under_it() {
    let engine = emulator();
    let caller = client(&engine, "us-west-1");

    for (service_code, quota_code, quota_name, value) in BINDINGS {
        let case = format!("{service_code}/{quota_code}");
        let quota = caller
            .ok(
                "GetAWSDefaultServiceQuota",
                json!({"ServiceCode": service_code, "QuotaCode": quota_code}),
            )
            .await["Quota"]
            .clone();
        assert_eq!(
            (quota["QuotaName"].clone(), quota["Value"].clone()),
            (json!(quota_name), json!(value)),
            "case {case}: expected the quota AWS publishes under this code at its own default, \
             got {quota}"
        );

        // And no OTHER row of the service claims that quota's name: a swapped row would
        // leave the name on two codes at once.
        let rows = rows_by_code(&caller, service_code).await;
        let claimants: Vec<&String> = rows
            .iter()
            .filter(|(_, row)| row["QuotaName"] == json!(quota_name))
            .map(|(code, _)| code)
            .collect();
        assert_eq!(
            claimants,
            vec![quota_code],
            "case {case}: exactly one code publishes this quota, got {claimants:?}"
        );
    }
}

/// The API a metered rate row names in its `QuotaName` is the API its usage metric meters.
///
/// Every `AWS/Usage` `CallCount` row of these catalogs is a per-operation rate, and the
/// `Resource` dimension is the series CloudWatch publishes that operation's calls under, so
/// a row whose metric meters a DIFFERENT operation than its name advertises sends a client
/// to watch the wrong series. Driven over every row of all eight catalogs rather than over
/// a list of codes.
#[tokio::test]
async fn a_metered_rate_row_meters_the_api_its_name_names() {
    let engine = emulator();
    let caller = client(&engine, "ca-central-1");

    let mut metered = 0_usize;
    for catalog in CATALOGS {
        for quota in defaults(&caller, catalog.service_code).await {
            let metric = quota["UsageMetric"].clone();
            if metric["MetricName"] != json!("CallCount") {
                continue;
            }
            metered += 1;
            let code = quota["QuotaCode"].as_str().unwrap_or_default();
            let case = format!("{}/{code}", catalog.service_code);
            let name = quota["QuotaName"].as_str().unwrap_or_default();
            let api = name
                .strip_prefix("Transactions per second (TPS) for the ")
                .and_then(|rest| rest.strip_suffix(" API"))
                .unwrap_or_else(|| {
                    panic!("case {case}: a call-count row names the API it throttles, got {name:?}")
                });
            assert_eq!(
                metric["MetricDimensions"]["Resource"],
                json!(api),
                "case {case}: expected the usage series of {api}, got {quota}"
            );
            assert_eq!(
                (
                    metric["MetricDimensions"]["Type"].clone(),
                    metric["MetricStatisticRecommendation"].clone(),
                    quota["Period"].clone()
                ),
                (
                    json!("API"),
                    json!("Sum"),
                    json!({"PeriodValue": 1, "PeriodUnit": "SECOND"})
                ),
                "case {case}: a per-second API rate is the calls added up over one second, got \
                 {quota}"
            );
        }
    }
    assert!(
        metered > 50,
        "the covered catalogs are dominated by metered per-API rates, got {metered} of them"
    );
}

/// The sentence each service publishes its rows with, asserted over the WHOLE catalog of
/// the services whose published voice the recorded listings fix.
///
/// A description is the text a console shows, so the rule is about every row of the
/// service rather than about the handful of codes a report happened to name: Batch states
/// its limits in the "Maximum …" voice of its own quota table, CodeDeploy in "Maximum
/// number of …", CloudTrail publishes one sentence for its whole throttle class, Cloud9
/// names the SERVICE in every environment row, and CodeArtifact's rate rows state the
/// calls that can be made to the operation they throttle.
#[tokio::test]
async fn every_row_is_described_in_the_voice_its_service_publishes() {
    let engine = emulator();
    let caller = client(&engine, "eu-west-1");

    for (service_code, opening) in [("batch", "Maximum "), ("codedeploy", "Maximum number of ")] {
        for quota in defaults(&caller, service_code).await {
            let case = format!("{service_code}/{}", quota["QuotaCode"]);
            let described = quota["Description"].as_str().unwrap_or_default();
            assert!(
                described.starts_with(opening),
                "case {case}: this service states its limits as {opening:?}…, got \
                 {described:?}"
            );
        }
    }

    for quota in defaults(&caller, "cloud9").await {
        let case = format!("cloud9/{}", quota["QuotaCode"]);
        let described = quota["Description"].as_str().unwrap_or_default();
        assert!(
            described.contains("AWS Cloud9"),
            "case {case}: every Cloud9 row names the service in its description, got \
             {described:?}"
        );
    }

    for quota in defaults(&caller, "codeartifact").await {
        let case = format!("codeartifact/{}", quota["QuotaCode"]);
        let name = quota["QuotaName"].as_str().unwrap_or_default();
        let Some(api) = name.strip_suffix(" requests per second") else {
            continue;
        };
        assert_eq!(
            quota["Description"],
            json!(format!(
                "The maximum number of calls that can be made to {api} per second."
            )),
            "case {case}: a rate row states the calls that can be made to {api}, got {quota}"
        );
    }

    let cloudtrail = defaults(&caller, "cloudtrail").await;
    let mut throttles = 0_usize;
    for quota in &cloudtrail {
        let case = format!("cloudtrail/{}", quota["QuotaCode"]);
        // CloudTrail publishes its WHOLE default set as one a client cannot have raised.
        assert_eq!(
            quota["Adjustable"],
            json!(false),
            "case {case}: CloudTrail publishes no raisable quota, got {quota}"
        );
        if quota["UsageMetric"]["MetricName"] != json!("CallCount") {
            continue;
        }
        throttles += 1;
        assert_eq!(
            quota["Description"],
            json!(CLOUDTRAIL_THROTTLE_DESCRIPTION),
            "case {case}: every throttle row carries the class's one sentence, got {quota}"
        );
    }
    assert!(
        throttles > 50,
        "CloudTrail's catalog is dominated by per-API throttles, got {throttles} of \
         {} rows",
        cloudtrail.len()
    );
}
