// SPDX-License-Identifier: CC0-1.0

//! What `ListAWSDefaultServiceQuotas` publishes under a `QuotaCode`, for the ten
//! `ServiceCode`s whose default catalogs this change corrected.
//!
//! THE RULE: `QuotaCode` is the identity AWS publishes a quota under, so the row served
//! under a code carries the `QuotaName`, `Value`, `Unit`, `Adjustable`, `Period`,
//! `UsageMetric` and `Description` of the quota AWS publishes UNDER THAT CODE, never a
//! neighbouring quota's — and the code itself is one AWS minted (`L-` followed by eight
//! hex digits), never an identifier this emulator invented.
//!
//! The rows below span everything the rule generalises over rather than one recorded
//! response: eight services and ten service codes, counted planes and per-API rates,
//! values from 1 to 10,000, a size in kilobytes beside counts with no unit, adjustable and
//! not-adjustable rows of the SAME class (a burst multiplier published as adjustable
//! beside one published as fixed), windows in seconds, minutes and hours beside rows
//! published over no window at all, `AWS/Usage` `CallCount` and `ResourceCount` metrics
//! (including one whose usage `Class` is not `None`), and rows published with a sentence
//! beside rows whose published sentence no source available here states, which carry
//! none. One row — the API Gateway domain-name access associations — is a code no issue
//! cited, so the rule is asserted past the rows that motivated it.
//!
//! The second test walks the WHOLE catalog of each of the ten codes, so the code-shape and
//! one-row-per-code rules are asserted over every row, not only the pinned ones.
//!
//! SOURCE for every expectation: AWS's published Service Quotas data for that service
//! code, and the recorded `ListAWSDefaultServiceQuotas` response of that code for which
//! `QuotaCode` publishes which quota and for the text, window, metric and adjustability of
//! the rows it states.
//!
//! The harness (`client`, `emulator`, `default_quotas`) is the one `quota_workflow.rs`
//! owns.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};

use crate::quota_workflow::{client, default_quotas as defaults, emulator};

/// The `AWS/Usage` metric a published row is metered by.
///
/// The sibling suites' helper fixes the usage `Class` at `None`; a row of these catalogs
/// can publish a class of its own (Athena publishes its active-query count per query
/// class), so the class is a field here.
struct Metric {
    metric_name: &'static str,
    statistic: &'static str,
    usage_service: &'static str,
    class: &'static str,
    resource: &'static str,
    usage_type: &'static str,
}

impl Metric {
    /// The `MetricInfo` JSON this metric is published as.
    fn published(&self) -> Value {
        json!({
            "MetricNamespace": "AWS/Usage",
            "MetricName": self.metric_name,
            "MetricStatisticRecommendation": self.statistic,
            "MetricDimensions": {
                "Service": self.usage_service,
                "Class": self.class,
                "Resource": self.resource,
                "Type": self.usage_type,
            },
        })
    }
}

/// One published default-quota row.
struct Published {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    /// The published sentence, where the source states its text; `None` pins none,
    /// because a wrong assertion would be worse than no assertion.
    description: Option<&'static str>,
    /// `(PeriodValue, PeriodUnit)` of a quota published over a window.
    period: Option<(i32, &'static str)>,
    /// The usage metric, for a quota usage is published for.
    metric: Option<Metric>,
}

/// A `ResourceCount` metric: the largest count over the window is the one that matters.
const fn resource_count(usage_service: &'static str, resource: &'static str) -> Metric {
    Metric {
        metric_name: "ResourceCount",
        statistic: "Maximum",
        usage_service,
        class: "None",
        resource,
        usage_type: "Resource",
    }
}

/// A `CallCount` metric of one API: a rate is the calls added up over the window.
const fn call_count(usage_service: &'static str, api: &'static str) -> Metric {
    Metric {
        metric_name: "CallCount",
        statistic: "Sum",
        usage_service,
        class: "None",
        resource: api,
        usage_type: "API",
    }
}

/// The `ServiceCode`s whose whole catalogs the second test walks.
const COVERED_SERVICES: &[&str] = &[
    "apigateway",
    "appconfig",
    "application-autoscaling",
    "appmesh",
    "apprunner",
    "athena",
    "autoscaling",
    "backup",
    "codecommit",
    "codedeploy",
];

/// The rows this suite pins.
const PUBLISHED_ROWS: &[Published] = &[
    // apigateway — the developer-portal planes, published as NOT adjustable under codes
    // this catalog had bound to a documentation plane and to a domain-name count.
    Published {
        service_code: "apigateway",
        quota_code: "L-7D4D47CD",
        quota_name: "PortalProducts per account",
        value: 500.0,
        unit: "None",
        adjustable: false,
        description: Some("The maximum number of PortalProducts per account"),
        period: None,
        metric: None,
    },
    Published {
        service_code: "apigateway",
        quota_code: "L-B4D69DB7",
        quota_name: "Maximum Custom Domain Names per Portal",
        value: 1.0,
        unit: "None",
        adjustable: false,
        description: Some("The maximum number of Custom Domain Names per Portal"),
        period: None,
        metric: None,
    },
    // The API-key plane, and the stage-variable planes whose sentences this service
    // publishes as the quota's own phrase rather than in a counted-plane voice.
    Published {
        service_code: "apigateway",
        quota_code: "L-1D180A63",
        quota_name: "API keys",
        value: 10_000.0,
        unit: "None",
        adjustable: false,
        description: None,
        period: None,
        metric: None,
    },
    Published {
        service_code: "apigateway",
        quota_code: "L-95BA6EA5",
        quota_name: "Stage variables per stage",
        value: 100.0,
        unit: "None",
        adjustable: false,
        description: Some("Stage variables per stage"),
        period: None,
        metric: None,
    },
    Published {
        service_code: "apigateway",
        quota_code: "L-B2CF62DC",
        quota_name: "Stage Variable Value Length",
        value: 512.0,
        unit: "None",
        adjustable: false,
        description: Some("Length, in characters, of the value in a stage variable"),
        period: None,
        metric: None,
    },
    // A rate published over one second, with no usage metric beside it.
    Published {
        service_code: "apigateway",
        quota_code: "L-9ED1E49A",
        quota_name: "WebSocket new connections rate",
        value: 500.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "New connections per second per account (across all WebSocket APIs) per region",
        ),
        period: Some((1, "SECOND")),
        metric: None,
    },
    // A size in kilobytes, published as fixed.
    Published {
        service_code: "apigateway",
        quota_code: "L-F8A34CAF",
        quota_name: "Maximum Logo Size",
        value: 200.0,
        unit: "Kilobytes",
        adjustable: false,
        description: None,
        period: None,
        metric: None,
    },
    // A code NO issue cited, published with the sentence this service's newer planes
    // carry: the rule is the same one, and it holds here too.
    Published {
        service_code: "apigateway",
        quota_code: "L-4D98A8A5",
        quota_name: "Domain name access associations",
        value: 100.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of domain name access associations that you can create in \
             this account in the current region",
        ),
        period: None,
        metric: None,
    },
    // appconfig — resource planes published over a ONE-HOUR window and metered by the
    // resource type's own count.
    Published {
        service_code: "appconfig",
        quota_code: "L-F59D302B",
        quota_name: "Maximum number of deployment strategies",
        value: 20.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: Some((1, "HOUR")),
        metric: Some(resource_count("AWS AppConfig", "DeploymentStrategy")),
    },
    Published {
        service_code: "appconfig",
        quota_code: "L-FA210A1F",
        quota_name: "Maximum number of applications",
        value: 100.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: Some((1, "HOUR")),
        metric: Some(resource_count("AWS AppConfig", "Application")),
    },
    Published {
        service_code: "appconfig",
        quota_code: "L-B858C085",
        quota_name: "Maximum number of experiment definitions per application",
        value: 50.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: None,
        metric: None,
    },
    // application-autoscaling — the scalable-target planes, each named for the service
    // whose resources it scales, and the per-target scheduled-action plane.
    Published {
        service_code: "application-autoscaling",
        quota_code: "L-799ACCDF",
        quota_name: "Scalable targets for Amazon Keyspaces",
        value: 1500.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: None,
        metric: None,
    },
    Published {
        service_code: "application-autoscaling",
        quota_code: "L-8998F403",
        quota_name: "Scalable targets for AppStream",
        value: 500.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: None,
        metric: None,
    },
    Published {
        service_code: "application-autoscaling",
        quota_code: "L-D75CA9D2",
        quota_name: "Scalable targets for EMR",
        value: 500.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: None,
        metric: None,
    },
    Published {
        service_code: "application-autoscaling",
        quota_code: "L-95848B5F",
        quota_name: "Scheduled actions per scalable target",
        value: 200.0,
        unit: "None",
        adjustable: false,
        description: None,
        period: None,
        metric: None,
    },
    // appmesh — the two adjacent planes whose names and values this catalog had crossed.
    Published {
        service_code: "appmesh",
        quota_code: "L-AE1D9567",
        quota_name: "Weighted targets per route",
        value: 10.0,
        unit: "None",
        adjustable: false,
        description: None,
        period: None,
        metric: None,
    },
    Published {
        service_code: "appmesh",
        quota_code: "L-87E74146",
        quota_name: "Virtual gateways per mesh",
        value: 3.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: None,
        metric: None,
    },
    // apprunner — a counted plane and a per-VPC allowance published as fixed.
    Published {
        service_code: "apprunner",
        quota_code: "L-69F96A0C",
        quota_name: "Services",
        value: 30.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: None,
        metric: None,
    },
    Published {
        service_code: "apprunner",
        quota_code: "L-4064914D",
        quota_name: "VPC ingress connections",
        value: 1.0,
        unit: "None",
        adjustable: false,
        description: None,
        period: None,
        metric: None,
    },
    // athena — the burst multiplier of ONE API's calls, the active-query plane whose
    // usage class is the query class, a rate published as NOT adjustable and a burst
    // multiplier of the same class published AS adjustable.
    Published {
        service_code: "athena",
        quota_code: "L-7A12A1E6",
        quota_name: "Burst multiplier quota for CreateNamedQuery API",
        value: 4.0,
        unit: "None",
        adjustable: false,
        description: None,
        period: None,
        metric: None,
    },
    Published {
        service_code: "athena",
        quota_code: "L-3CE0BBA0",
        quota_name: "Active DDL queries",
        value: 20.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: Some((1, "SECOND")),
        metric: Some(Metric {
            metric_name: "ResourceCount",
            statistic: "Maximum",
            usage_service: "Athena",
            class: "DDL",
            resource: "ActiveQueryCount",
            usage_type: "Resource",
        }),
    },
    Published {
        service_code: "athena",
        quota_code: "L-C60BEAE0",
        quota_name: "Replenishment rate of GetWorkGroup API calls",
        value: 20.0,
        unit: "None",
        adjustable: false,
        description: None,
        period: Some((1, "SECOND")),
        metric: Some(call_count("Athena", "GetWorkGroup")),
    },
    Published {
        service_code: "athena",
        quota_code: "L-9D86AA19",
        quota_name: "Burst multiplier quota for GetQueryExecution API",
        value: 2.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: None,
        metric: None,
    },
    Published {
        service_code: "athena",
        quota_code: "L-3702C1C1",
        quota_name: "Workgroups per Region",
        value: 1000.0,
        unit: "None",
        adjustable: false,
        description: None,
        period: None,
        metric: None,
    },
    // autoscaling — the per-group planes published as fixed, and the group count, the one
    // plane of the service CloudWatch publishes a resource count for.
    Published {
        service_code: "autoscaling",
        quota_code: "L-CEE5E714",
        quota_name: "SNS topics per Auto Scaling group",
        value: 10.0,
        unit: "None",
        adjustable: false,
        description: Some("The maximum number of SNS topics per Auto Scaling group"),
        period: None,
        metric: None,
    },
    Published {
        service_code: "autoscaling",
        quota_code: "L-F786B2E5",
        quota_name: "Classic Load Balancers per Auto Scaling group",
        value: 50.0,
        unit: "None",
        adjustable: false,
        description: Some(
            "The maximum number of Classic Load Balancers per Auto Scaling group",
        ),
        period: None,
        metric: None,
    },
    Published {
        service_code: "autoscaling",
        quota_code: "L-CDE20ADC",
        quota_name: "Auto Scaling groups per region",
        value: 500.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: Some((5, "MINUTE")),
        metric: Some(resource_count("AutoScaling", "NumberOfAutoScalingGroup")),
    },
    // backup — a DEPTH rather than a count, the per-backup metadata tags published as
    // fixed, and the versions of a backup plan.
    Published {
        service_code: "backup",
        quota_code: "L-C0A8C14B",
        quota_name: "Maximum backup nest level",
        value: 10.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: None,
        metric: None,
    },
    Published {
        service_code: "backup",
        quota_code: "L-FB1D55EC",
        quota_name: "Metadata tags per backup",
        value: 50.0,
        unit: "None",
        adjustable: false,
        description: None,
        period: None,
        metric: None,
    },
    Published {
        service_code: "backup",
        quota_code: "L-9122A82F",
        quota_name: "Versions per backup plan",
        value: 2000.0,
        unit: "None",
        adjustable: true,
        description: None,
        period: None,
        metric: None,
    },
    // codecommit — the repository plane, published under the name a client matching a
    // quota by name resolves it by.
    Published {
        service_code: "codecommit",
        quota_code: "L-81790602",
        quota_name: "Allowed repositories",
        value: 5000.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "The maximum number of repositories that you can create in this account.",
        ),
        period: None,
        metric: None,
    },
    // codedeploy — two of the planes the catalog once published under invented
    // identifiers instead of AWS's codes.
    Published {
        service_code: "codedeploy",
        quota_code: "L-877B748B",
        quota_name: "Event notification triggers in a deployment group",
        value: 10.0,
        unit: "None",
        adjustable: true,
        description: Some(
            "Maximum number of event notification triggers in a deployment group",
        ),
        period: None,
        metric: None,
    },
    Published {
        service_code: "codedeploy",
        quota_code: "L-C77AFF36",
        quota_name: "Number of listeners for a traffic route during an Amazon ECS deployment",
        value: 1.0,
        unit: "None",
        adjustable: false,
        description: Some(
            "Maximum number of listeners for a traffic route during an Amazon ECS \
             deployment",
        ),
        period: None,
        metric: None,
    },
];

/// Whether `code` is an identifier AWS minted for a quota: `L-` and eight upper-case hex
/// digits, which is the shape of every code AWS publishes a quota under.
fn aws_minted_code(code: &str) -> bool {
    match code.strip_prefix("L-") {
        Some(digits) => {
            digits.len() == 8 && digits.chars().all(|digit| digit.is_ascii_hexdigit())
        }
        None => false,
    }
}

/// Every pinned code reports the quota published under it — name, value, unit,
/// adjustability, window, usage metric and, where the source states it, sentence —
/// through the listing, through the single default read and through the applied read.
#[tokio::test]
async fn a_published_row_reports_the_quota_bound_to_its_code() {
    let engine = emulator();
    let client = client(&engine, "us-east-2");

    let mut listings: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for row in PUBLISHED_ROWS {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let pair = json!({"ServiceCode": row.service_code, "QuotaCode": row.quota_code});
        let quota = client.ok("GetAWSDefaultServiceQuota", pair.clone()).await["Quota"].clone();

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
                "case {case}: expected the published sentence, got {quota}"
            );
        }
        match row.period {
            Some((value, unit)) => assert_eq!(
                quota["Period"],
                json!({"PeriodValue": value, "PeriodUnit": unit}),
                "case {case}: expected the window this quota is published over, got {quota}"
            ),
            None => assert!(
                quota.get("Period").is_none_or(Value::is_null),
                "case {case}: this quota is published over no window, got {quota}"
            ),
        }
        match &row.metric {
            Some(metric) => assert_eq!(
                quota["UsageMetric"],
                metric.published(),
                "case {case}: expected the usage metric published for this quota, got {quota}"
            ),
            None => assert!(
                quota.get("UsageMetric").is_none_or(Value::is_null),
                "case {case}: this quota publishes no usage metric, got {quota}"
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
            "case {case}: the listing must carry the record the single read returned, got \
             {listed:?}"
        );

        // The applied plane is keyed on the same code, so a client that read the code out
        // of the default catalog resolves the same quota's value in force.
        let applied = client.ok("GetServiceQuota", pair).await["Quota"].clone();
        assert_eq!(
            (applied["QuotaName"].clone(), applied["Value"].clone()),
            (json!(row.quota_name), json!(row.value)),
            "case {case}: the applied read must resolve the same quota, got {applied}"
        );
    }
}

/// Over the WHOLE catalog of each covered service: every row is published under a code AWS
/// minted, a code resolves exactly one row, no two rows of one service share a name, and
/// the row the listing reports is the row the single read returns.
#[tokio::test]
async fn every_row_of_a_covered_catalog_is_one_aws_minted_code() {
    let engine = emulator();
    let client = client(&engine, "eu-central-1");

    let mut rows = 0_usize;
    for service_code in COVERED_SERVICES {
        let listed = defaults(&client, service_code).await;
        assert!(
            !listed.is_empty(),
            "case {service_code}: the service must publish its catalog, got {listed:?}"
        );

        let mut codes: BTreeSet<String> = BTreeSet::new();
        let mut names: BTreeSet<String> = BTreeSet::new();
        for quota in &listed {
            let code = quota["QuotaCode"].as_str().unwrap_or_default().to_string();
            let case = format!("{service_code}/{code}");
            assert!(
                aws_minted_code(&code),
                "case {case}: a row is published under a code AWS minted, got {quota}"
            );
            assert!(
                codes.insert(code.clone()),
                "case {case}: a QuotaCode identifies one quota of a service, got a second \
                 row under it in {listed:?}"
            );
            let name = quota["QuotaName"].as_str().unwrap_or_default().to_string();
            assert!(
                names.insert(name.clone()),
                "case {case}: a quota name identifies one quota of a service, got a second \
                 row named {name:?} in {listed:?}"
            );
            let read = client
                .ok(
                    "GetAWSDefaultServiceQuota",
                    json!({"ServiceCode": service_code, "QuotaCode": code}),
                )
                .await["Quota"]
                .clone();
            assert_eq!(
                &read, quota,
                "case {case}: the single read must return the row the listing reported, got \
                 {read}"
            );
            rows += 1;
        }
    }

    assert!(
        rows > PUBLISHED_ROWS.len(),
        "case catalog: the rule must be asserted over the whole catalog of each covered \
         service, got {rows} rows"
    );
}
