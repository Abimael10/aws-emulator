// SPDX-License-Identifier: CC0-1.0

//! The default-quota rows of `access-analyzer` through `amplify` that this change bound
//! to the `QuotaCode` AWS publishes them under, over the wire.
//!
//! THE RULE: a row served under a `QuotaCode` reports the quota AWS publishes UNDER THAT
//! CODE and nothing of a neighbouring quota's — its `QuotaName`, `Value`, `Unit`,
//! `Adjustable`, `GlobalQuota`, the `Period` it is measured over, the `UsageMetric` its
//! usage is published in and, where AWS publishes one, its `Description`, which is
//! SERIALIZED on the row rather than dropped. The single read and the listing report the
//! same record, and `Adjustable` is backed: a quota published as fixed refuses
//! `RequestServiceQuotaIncrease`, an adjustable one takes it.
//!
//! The rows below span what the rule generalises over rather than one recorded response:
//! nine service codes; a plain count, a count with a unit of its own, a size in
//! gigabytes, a per-API rate with a one-second window, a resource count metered over
//! five minutes and a quota metered over an hour; adjustable rows beside fixed ones;
//! values from 1 to 100,000; names in prose, in slug and in sentence form; and rows with
//! a usage metric beside rows AWS publishes none for.
//!
//! SOURCE for every expectation: AWS's published Service Quotas data for that service
//! code, and the recorded `ListAWSDefaultServiceQuotas` response of that code for which
//! `QuotaCode` publishes which quota and for the name, value, unit, adjustability,
//! window, metric and text it states — which is where `src/state/quota_rows/` takes each
//! row from. `smithy_query(query="shape", shape="QuotaDescription")` for the 0..350
//! length asserted below.
//!
//! The harness (`client`, `emulator`, `default_quotas`) is the one `quota_workflow.rs`
//! owns.

use serde_json::{json, Value};

use crate::quota_workflow::{client, default_quotas as defaults, emulator, Client};

/// The modelled maximum length of `ServiceQuota$Description`.
const DESCRIPTION_MAX_LENGTH: usize = 350;

/// One published default-quota row, with the members AWS publishes for it.
struct Row {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    /// The published sentence, where a source available here states its text; `None`
    /// pins nothing, because a wrong assertion would be worse than no assertion.
    description: Option<&'static str>,
    /// `(PeriodValue, PeriodUnit)` of a quota published over a window.
    period: Option<(i32, &'static str)>,
    /// `(MetricName, Service, Resource, Type)` of the `AWS/Usage` series the quota's
    /// usage is published in, for a metered quota.
    metric: Option<(&'static str, &'static str, &'static str, &'static str)>,
}

/// The rows this suite pins: the codes these nine catalogs now publish a quota under.
fn rows() -> Vec<Row> {
    vec![
        // access-analyzer — the two ANALYZER planes, one fixed and one adjustable, whose
        // sentences name the plane each counts, and the two policy-generation planes:
        // one counted per hour, one measured in gigabytes.
        Row {
            service_code: "access-analyzer",
            quota_code: "L-2F63646F",
            quota_name: "Account level analyzer",
            value: 1.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "The maximum number of account level analyzers that can be created per analyzer \
                 type per AWS Account per Region.",
            ),
            period: None,
            metric: None,
        },
        Row {
            service_code: "access-analyzer",
            quota_code: "L-6F85FE0C",
            quota_name: "Organization level analyzer",
            value: 5.0,
            unit: "None",
            adjustable: true,
            description: Some(
                "The maximum number of organization level analyzers that can be created per \
                 analyzer type per AWS Account per Region.",
            ),
            period: None,
            metric: None,
        },
        Row {
            service_code: "access-analyzer",
            quota_code: "L-8750DAE0",
            quota_name: "Access previews per analyzer per hour",
            value: 1000.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "access-analyzer",
            quota_code: "L-94099EDD",
            quota_name: "Policy generation CloudTrail data size",
            value: 25.0,
            unit: "Gigabytes",
            adjustable: false,
            description: None,
            period: None,
            metric: None,
        },
        // A row of the same catalog this change did NOT touch, so the rule is asserted
        // past the rows that motivated it: a count of six figures.
        Row {
            service_code: "access-analyzer",
            quota_code: "L-5EFCE71D",
            quota_name: "CloudTrail log files processed per policy generation",
            value: 100_000.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "The maximum number of CloudTrail log files that can be processed per policy \
                 generation.",
            ),
            period: None,
            metric: None,
        },
        // The policy generations a day allows, published at AWS's own default and as
        // fixed like the rest of that plane.
        Row {
            service_code: "access-analyzer",
            quota_code: "L-FD4CD927",
            quota_name: "Policy generations per day",
            value: 50.0,
            unit: "None",
            adjustable: false,
            description: Some("The maximum number of policy generations per day."),
            period: None,
            metric: None,
        },
        // acm — the ACME-endpoint rates: a one-second window, that operation's own call
        // count, and the rate the whole sub-class is published at.
        Row {
            service_code: "acm",
            quota_code: "L-535A8003",
            quota_name: "Rate of DescribeAcmeExternalAccountBinding API requests",
            value: 50.0,
            unit: "None",
            adjustable: true,
            description: Some(
                "The maximum number of DescribeAcmeExternalAccountBinding API requests per \
                 second.",
            ),
            period: Some((1, "SECOND")),
            metric: Some((
                "CallCount",
                "Certificate Manager",
                "DescribeAcmeExternalAccountBinding",
                "API",
            )),
        },
        Row {
            service_code: "acm",
            quota_code: "L-8849A1A8",
            quota_name: "Rate of UpdateAcmeEndpoint API requests",
            value: 50.0,
            unit: "None",
            adjustable: true,
            description: Some("The maximum number of UpdateAcmeEndpoint API requests per second."),
            period: Some((1, "SECOND")),
            metric: Some((
                "CallCount",
                "Certificate Manager",
                "UpdateAcmeEndpoint",
                "API",
            )),
        },
        Row {
            service_code: "acm",
            quota_code: "L-A7230D7D",
            quota_name: "Rate of CreateAcmeEndpoint API requests",
            value: 50.0,
            unit: "None",
            adjustable: true,
            description: Some("The maximum number of CreateAcmeEndpoint API requests per second."),
            period: Some((1, "SECOND")),
            metric: Some((
                "CallCount",
                "Certificate Manager",
                "CreateAcmeEndpoint",
                "API",
            )),
        },
        // acm-pca — the same class worded as AWS Private CA publishes it, with no metric
        // block and a per-operation limit: 1, 5 and 20 requests a second.
        Row {
            service_code: "acm-pca",
            quota_code: "L-3549B861",
            quota_name: "Rate of GetPolicy requests",
            value: 5.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "The maximum number of GetPolicy requests that can be performed in an account in \
                 the current Region each second.",
            ),
            period: Some((1, "SECOND")),
            metric: None,
        },
        Row {
            service_code: "acm-pca",
            quota_code: "L-AF91D77C",
            quota_name: "Rate of ListTags requests",
            value: 20.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "The maximum number of ListTags requests that can be performed in an account in \
                 the current Region each second.",
            ),
            period: Some((1, "SECOND")),
            metric: None,
        },
        Row {
            service_code: "acm-pca",
            quota_code: "L-C0A08797",
            quota_name: "Rate of CreateCertificateAuthorityAuditReport requests",
            value: 1.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "The maximum number of CreateCertificateAuthorityAuditReport requests that can \
                 be performed in an account in the current Region each second.",
            ),
            period: Some((1, "SECOND")),
            metric: None,
        },
        // aco-automation — the throttle class, whose phrase precedes the operation it
        // throttles.
        Row {
            service_code: "aco-automation",
            quota_code: "L-D0368739",
            quota_name: "Throttle rate limit for GetAutomationEvent",
            value: 5.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "Maximum number of GetAutomationEvent requests you can make per second per \
                 account in the current region.",
            ),
            period: Some((1, "SECOND")),
            metric: None,
        },
        Row {
            service_code: "aco-automation",
            quota_code: "L-D0AFC926",
            quota_name: "Throttle rate limit for ListAutomationEvents",
            value: 5.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "Maximum number of ListAutomationEvents requests you can make per second per \
                 account in the current region.",
            ),
            period: Some((1, "SECOND")),
            metric: None,
        },
        Row {
            service_code: "aco-automation",
            quota_code: "L-D5E7759E",
            quota_name: "Throttle rate limit for UntagResource",
            value: 5.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "Maximum number of UntagResource requests you can make per second per account in \
                 the current region.",
            ),
            period: Some((1, "SECOND")),
            metric: None,
        },
        Row {
            service_code: "aco-automation",
            quota_code: "L-E33DC3FB",
            quota_name: "Throttle rate limit for ListRecommendedActions",
            value: 5.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "Maximum number of ListRecommendedActions requests you can make per second per \
                 account in the current region.",
            ),
            period: Some((1, "SECOND")),
            metric: None,
        },
        // agent-registry — a rate row at a limit of its own, beside the account's
        // REGISTRY plane: a resource count, so it is metered over five minutes rather
        // than over the class's one-second rate window.
        Row {
            service_code: "agent-registry",
            quota_code: "L-160B222D",
            quota_name: "GetRegistry requests per second",
            value: 5.0,
            unit: "Count/Second",
            adjustable: false,
            description: Some(
                "The maximum number of GetRegistry API requests per second per account per Region",
            ),
            period: Some((1, "SECOND")),
            metric: Some(("CallCount", "Agent Registry", "GetRegistry", "API")),
        },
        Row {
            service_code: "agent-registry",
            quota_code: "L-197AC8CB",
            quota_name: "Registries per Account",
            value: 10.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: Some((5, "MINUTE")),
            metric: Some(("ResourceCount", "Agent Registry", "Registry", "Resource")),
        },
        // aidevops — the planes this service names with the slug of what it counts, one
        // of them described with that slug read as prose, in `Count` and in the
        // countable `None`, adjustable and fixed.
        Row {
            service_code: "aidevops",
            quota_code: "L-3D8A06B8",
            quota_name: "concurrent-on-demand-invocations-per-agent-space",
            value: 10.0,
            unit: "Count",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "aidevops",
            quota_code: "L-510AF4A9",
            quota_name: "agent-spaces-per-account-per-region",
            value: 100.0,
            unit: "None",
            adjustable: true,
            description: Some("Agent spaces per account per Region"),
            period: None,
            metric: None,
        },
        Row {
            service_code: "aidevops",
            quota_code: "L-D774F842",
            quota_name: "memory-stores-per-agent-space",
            value: 50.0,
            unit: "Count",
            adjustable: false,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "aidevops",
            quota_code: "L-F1DF5F94",
            quota_name: "memories-per-memory-store",
            value: 200.0,
            unit: "Count",
            adjustable: false,
            description: None,
            period: None,
            metric: None,
        },
        // aiops — the two rows whose published sentences this evidence states, one of
        // them metered over an HOUR.
        Row {
            service_code: "aiops",
            quota_code: "L-4CD81CD5",
            quota_name: "Monthly investigations",
            value: 150.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "The maximum number of AI-assisted investigations per month in this account in \
                 the current region.",
            ),
            period: Some((1, "HOUR")),
            metric: Some((
                "ResourceCount",
                "Amazon AI Operations",
                "MonthlyAIAssistedInvestigations",
                "Resource",
            )),
        },
        Row {
            service_code: "aiops",
            quota_code: "L-5EC19707",
            quota_name: "Investigation groups",
            value: 1.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "The maximum number of investigation groups that can be created per account in a \
                 region.",
            ),
            period: None,
            metric: None,
        },
        // airflow — the LARGER-environment plane, beside the plane of the smaller
        // classes: two codes, two names, two defaults.
        Row {
            service_code: "airflow",
            quota_code: "L-4BA0CB82",
            quota_name: "Larger environments per account per Region (xlarge, 2xlarge)",
            value: 5.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "airflow",
            quota_code: "L-67D41C6B",
            quota_name: "Environments per account per Region (small, medium, large)",
            value: 10.0,
            unit: "None",
            adjustable: true,
            description: Some("The maximum number of environments per account per Region."),
            period: None,
            metric: None,
        },
        // amplify — the PER-APP class, with the class sentence around each plane's own
        // name, beside the cache size published in gigabytes. `Adjustable` is the row's
        // own: AWS publishes the branch plane as fixed and the webhook plane as
        // adjustable.
        Row {
            service_code: "amplify",
            quota_code: "L-2FC3A2FA",
            quota_name: "Maximum app creations per hour",
            value: 25.0,
            unit: "None",
            adjustable: false,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "amplify",
            quota_code: "L-4113FC04",
            quota_name: "Webhooks per app",
            value: 50.0,
            unit: "None",
            adjustable: true,
            description: Some(
                "The maximum number of webhooks per app that you can create in this account in \
                 the current Region.",
            ),
            period: None,
            metric: None,
        },
        Row {
            service_code: "amplify",
            quota_code: "L-A0FC4951",
            quota_name: "Environment cache artifact size",
            value: 5.0,
            unit: "Gigabytes",
            adjustable: false,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "amplify",
            quota_code: "L-A6716586",
            quota_name: "Branches per app",
            value: 50.0,
            unit: "None",
            adjustable: false,
            description: Some(
                "The maximum number of branches per app that you can create in this account in \
                 the current Region.",
            ),
            period: None,
            metric: None,
        },
        Row {
            service_code: "amplify",
            quota_code: "L-AD277529",
            quota_name: "Domains per app",
            value: 5.0,
            unit: "None",
            adjustable: true,
            description: Some(
                "The maximum number of domains per app that you can create in this account in \
                 the current Region.",
            ),
            period: None,
            metric: None,
        },
    ]
}

/// The nine service codes these rows belong to.
const COVERED_SERVICES: &[&str] = &[
    "access-analyzer",
    "acm",
    "acm-pca",
    "aco-automation",
    "agent-registry",
    "aidevops",
    "aiops",
    "airflow",
    "amplify",
];

/// The record the single default read returns for one pair.
async fn default_quota(caller: &Client, service_code: &str, quota_code: &str) -> Value {
    caller
        .ok(
            "GetAWSDefaultServiceQuota",
            json!({"ServiceCode": service_code, "QuotaCode": quota_code}),
        )
        .await["Quota"]
        .clone()
}

/// RULE: the row served under a code carries that quota's own published facts, the
/// `Description` included, through the listing and the single read alike.
#[tokio::test]
async fn a_code_is_served_with_the_facts_aws_publishes_under_it() {
    let engine = emulator();
    let region = "us-west-2";
    let caller = client(&engine, region);

    for row in rows() {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let quota = default_quota(&caller, row.service_code, row.quota_code).await;

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
            "case {case}: expected the facts published under this code, got {quota}"
        );
        assert_eq!(
            quota["QuotaArn"],
            json!(format!(
                "arn:aws:servicequotas:{region}::{}/{}",
                row.service_code, row.quota_code
            )),
            "case {case}: a default quota's ARN names the Region and no account, got {quota}"
        );
        if let Some(description) = row.description {
            assert_eq!(
                quota["Description"],
                json!(description),
                "case {case}: a description is the quota's own published text, serialized on the \
                 row, got {quota}"
            );
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
            Some((metric_name, service, resource, usage_type)) => assert_eq!(
                (
                    quota["UsageMetric"]["MetricNamespace"].as_str(),
                    quota["UsageMetric"]["MetricName"].as_str(),
                    quota["UsageMetric"]["MetricDimensions"]["Service"].as_str(),
                    quota["UsageMetric"]["MetricDimensions"]["Resource"].as_str(),
                    quota["UsageMetric"]["MetricDimensions"]["Type"].as_str(),
                ),
                (
                    Some("AWS/Usage"),
                    Some(metric_name),
                    Some(service),
                    Some(resource),
                    Some(usage_type),
                ),
                "case {case}: expected the series the quota's usage is published in, got {quota}"
            ),
            None => assert_eq!(
                quota["UsageMetric"],
                Value::Null,
                "case {case}: a quota CloudWatch publishes no usage for reports no metric, got \
                 {quota}"
            ),
        }

        // The listing carries exactly the record the single read returned.
        let listed = defaults(&caller, row.service_code).await;
        assert!(
            listed.contains(&quota),
            "case {case}: the listing must carry the record the single read returned, got \
             {listed:?}"
        );
    }
}

/// RULE: a `Description` a service publishes is SERIALIZED on the row — every row of
/// these catalogs that has one reports it as text within the modelled length, and the
/// rows whose text the evidence states carry exactly it.
#[tokio::test]
async fn a_published_description_is_serialized_on_the_row() {
    let engine = emulator();
    let caller = client(&engine, "eu-west-1");

    let mut described = 0_usize;
    for service_code in COVERED_SERVICES {
        let listed = defaults(&caller, service_code).await;
        assert!(
            !listed.is_empty(),
            "case {service_code}: expected the service's published catalog, got no rows"
        );
        for quota in &listed {
            let code = quota["QuotaCode"].as_str().unwrap_or_default();
            let case = format!("{service_code}/{code}");
            match quota.get("Description") {
                None | Some(Value::Null) => {}
                Some(description) => {
                    let text = description.as_str().unwrap_or_else(|| {
                        panic!("case {case}: a description is text, got {description}")
                    });
                    assert!(
                        (1..=DESCRIPTION_MAX_LENGTH).contains(&text.chars().count()),
                        "case {case}: a published sentence sits within the modelled length, got \
                         {text:?}"
                    );
                    described += 1;
                }
            }
        }
    }

    // The rows these catalogs describe, so a change that drops the member again fails
    // here rather than passing a vacuous walk.
    assert!(
        described >= rows().iter().filter(|row| row.description.is_some()).count(),
        "case catalog: expected every described row of the nine catalogs to carry its sentence, \
         got {described}"
    );
}

/// RULE: `Adjustable` is backed by the write path — a quota published as fixed refuses
/// an increase request, an adjustable one takes it, at a value above the one in force.
#[tokio::test]
async fn adjustability_decides_whether_an_increase_is_accepted() {
    let engine = emulator();
    let caller = client(&engine, "ap-northeast-1");

    for row in rows() {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let desired = row.value * 2.0;
        let body = json!({
            "ServiceCode": row.service_code,
            "QuotaCode": row.quota_code,
            "DesiredValue": desired,
        });

        if row.adjustable {
            let accepted = caller.ok("RequestServiceQuotaIncrease", body).await;
            let requested = &accepted["RequestedQuota"];
            assert_eq!(
                (
                    requested["QuotaCode"].as_str(),
                    requested["DesiredValue"].as_f64(),
                ),
                (Some(row.quota_code), Some(desired)),
                "case {case}: an adjustable quota takes the increase it published room for, got \
                 {accepted}"
            );
            continue;
        }

        let (status, error) = caller.refused("RequestServiceQuotaIncrease", body).await;
        assert_eq!(
            (status, error.as_str()),
            (400, "IllegalArgumentException"),
            "case {case}: a quota AWS publishes as not adjustable refuses an increase"
        );
    }
}
