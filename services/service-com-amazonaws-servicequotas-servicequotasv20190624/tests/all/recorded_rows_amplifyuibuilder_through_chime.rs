// SPDX-License-Identifier: CC0-1.0

//! The default-quota catalogs of the service codes from `amplifyuibuilder` through
//! `chime`, over the wire.
//!
//! A `QuotaCode` is the identity a client resolves a limit by, and AWS alone mints into
//! the `L-XXXXXXXX` namespace: a row served under a code of that shape is a claim about
//! the quota AWS publishes under THAT code, and a row served under a code this emulator
//! made up is an identifier AWS rejects. The rules asserted here, each over a TABLE of
//! inputs rather than one quota:
//!
//! * a published row carries the quota's own published facts — `QuotaName`, `Value`,
//!   `Unit`, `Adjustable`, `GlobalQuota`, the `Description` where the quota's text is
//!   known, the `Period` it is measured over and the `UsageMetric` its usage is
//!   published in — reported identically by the single read and by the listing, with a
//!   `QuotaArn` of the form `arn:aws:servicequotas:<region>::<serviceCode>/<quotaCode>`;
//! * every row these service codes publish sits under an AWS-minted code, so a client
//!   that reads a code out of one of these listings holds a code AWS would accept;
//! * a code one of these services publishes NOTHING under resolves nothing — neither a
//!   descriptive stand-in this catalog once minted nor an AWS code whose published facts
//!   this catalog cannot cite — while the service code itself keeps resolving its
//!   listing;
//! * `Adjustable` is backed: a quota published as fixed refuses
//!   `RequestServiceQuotaIncrease`, an adjustable one takes it.
//!
//! The rows span what the rules generalise over: plain counts, document-size quotas with
//! units of their own (`Kilobytes`, `Bytes`), per-API rates with a one-second window and
//! a call-count series, a rate measured in `Count/Second`, a quota published over a
//! five-minute window, adjustable and fixed rows, values from 1 to 10240, and names in
//! prose, in sentence and in bracketed-plane form. The harness (`client`, `emulator`,
//! `default_quotas`) is the one `quota_workflow.rs` owns.

use serde_json::{json, Value};

use crate::quota_workflow::{client, default_quotas as defaults, emulator};

/// One published default-quota row, with the members AWS publishes for it.
struct Row {
    service_code: &'static str,
    quota_code: &'static str,
    quota_name: &'static str,
    value: f64,
    unit: &'static str,
    adjustable: bool,
    /// The published description, where the evidence states its text; `None` pins none,
    /// because a wrong assertion would be worse than no assertion.
    description: Option<&'static str>,
    /// `(PeriodValue, PeriodUnit)` for a quota AWS publishes over a window.
    period: Option<(i32, &'static str)>,
    /// `(MetricName, Service, Resource, Type)` of the `AWS/Usage` series the quota's
    /// usage is published in, for a metered quota.
    metric: Option<(&'static str, &'static str, &'static str, &'static str)>,
}

/// The rows this suite pins.
///
/// SOURCE: AWS's published Service Quotas data for each `ServiceCode` (what `aws
/// service-quotas list-aws-default-service-quotas --service-code <code>` reports), and
/// the recorded default-quota listing of that code for which `QuotaCode` names which
/// quota — which is where `src/state/quota_rows/` takes each row from.
fn rows() -> Vec<Row> {
    vec![
        // A document-SIZE quota, measured in kilobytes and published as fixed, with its
        // own published sentence.
        Row {
            service_code: "amplifyuibuilder",
            quota_code: "L-D500C0BA",
            quota_name: "Theme size",
            value: 350.0,
            unit: "Kilobytes",
            adjustable: false,
            description: Some("The maximum size (in KB) of the theme."),
            period: None,
            metric: None,
        },
        Row {
            service_code: "amplifyuibuilder",
            quota_code: "L-E4AD9560",
            quota_name: "Views per app",
            value: 1000.0,
            unit: "None",
            adjustable: false,
            description: None,
            period: None,
            metric: None,
        },
        // The same class of fact in BYTES rather than kilobytes.
        Row {
            service_code: "aoss",
            quota_code: "L-2005AF7E",
            quota_name: "Encryption policy size per policy",
            value: 10240.0,
            unit: "Bytes",
            adjustable: false,
            description: None,
            period: None,
            metric: None,
        },
        // The per-API request-rate class of CloudWatch Application Signals: a one-second
        // window and the operation's own `AWS/Usage` call count, under the display name
        // `AWS/Usage` publishes the feature as.
        Row {
            service_code: "application-signals",
            quota_code: "L-EB837E0B",
            quota_name: "Rate of StartDiscovery requests",
            value: 5.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: Some((1, "SECOND")),
            metric: Some((
                "CallCount",
                "CloudWatch Application Signals",
                "StartDiscovery",
                "API",
            )),
        },
        Row {
            service_code: "application-signals",
            quota_code: "L-D898488F",
            quota_name: "Rate of ListAuditFindings requests",
            value: 1.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: Some((1, "SECOND")),
            metric: Some((
                "CallCount",
                "CloudWatch Application Signals",
                "ListAuditFindings",
                "API",
            )),
        },
        Row {
            service_code: "appstudio",
            quota_code: "L-B07DC829",
            quota_name: "Applications",
            value: 20.0,
            unit: "None",
            adjustable: false,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "appstudio",
            quota_code: "L-45B0D852",
            quota_name: "Published applications",
            value: 6.0,
            unit: "None",
            adjustable: false,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "auditmanager",
            quota_code: "L-0255B75F",
            quota_name: "Custom controls",
            value: 500.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "auditmanager",
            quota_code: "L-724DD74D",
            quota_name: "Controls per framework",
            value: 1400.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "auditmanager",
            quota_code: "L-8935A6F1",
            quota_name: "Custom frameworks",
            value: 100.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "auditmanager",
            quota_code: "L-BEA222D4",
            quota_name: "Accounts in scope across all assessments",
            value: 250.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        // The scaling-plan count, re-keyed off a stand-in code onto AWS's own: same
        // published quota, the code AWS resolves it by.
        Row {
            service_code: "autoscaling-plans",
            quota_code: "L-BD401546",
            quota_name: "Scaling plans",
            value: 100.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "autoscaling-plans",
            quota_code: "L-6538FA5E",
            quota_name: "Target tracking configurations per scaling instruction",
            value: 10.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "autoscaling-plans",
            quota_code: "L-7FAA513E",
            quota_name: "Scaling instructions per scaling plan",
            value: 500.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        // The throttle class of the AWS Marketplace Discovery plane, whose published
        // names name the plane in brackets before the operation.
        Row {
            service_code: "aws-marketplace",
            quota_code: "L-CD4DC1E4",
            quota_name: "[AWS Marketplace Discovery] GetOffer throttle quota",
            value: 5.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "aws-marketplace",
            quota_code: "L-96311249",
            quota_name: "[AWS Marketplace Discovery] SearchListings throttle quota",
            value: 10.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "aws-marketplace",
            quota_code: "L-68F80CEA",
            quota_name: "[AWS Marketplace Discovery] GetProduct throttle quota",
            value: 5.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "aws-marketplace",
            quota_code: "L-DF8B8977",
            quota_name: "[AWS Marketplace Discovery] ListFulfillmentOptions throttle quota",
            value: 5.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        // A rate measured in calls per second rather than counted, beside a count
        // published over the five-minute window a resource count is reported on.
        Row {
            service_code: "aws-mcp",
            quota_code: "L-7A518ECF",
            quota_name: "Rate of retrieve_skill tool requests",
            value: 10.0,
            unit: "Count/Second",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "aws-mcp",
            quota_code: "L-E5DA7401",
            quota_name: "Concurrent sessions per account",
            value: 180.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: Some((5, "MINUTE")),
            metric: None,
        },
        // The B2B Data Interchange resource planes, each published under AWS's own code
        // rather than under the code that service's own refusal reports. The profile
        // limit is the documented five.
        Row {
            service_code: "b2bi",
            quota_code: "L-05F6A9EF",
            quota_name: "Profiles",
            value: 5.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "b2bi",
            quota_code: "L-19B5E505",
            quota_name: "Capabilities",
            value: 100.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "b2bi",
            quota_code: "L-CD5D2786",
            quota_name: "Partnerships",
            value: 700.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "b2bi",
            quota_code: "L-E15C983E",
            quota_name: "Transformers",
            value: 500.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        // Amazon Chime's media and meeting planes: a rate published across a whole API
        // category, two burst allowances and the voice plane's calls per second.
        Row {
            service_code: "chime",
            quota_code: "L-F147F728",
            quota_name: "Amazon Chime SDK meetings - All meeting management API rate in \
                         requests per second",
            value: 10.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "chime",
            quota_code: "L-FE9F896D",
            quota_name: "CreateMeeting API burst rate",
            value: 20.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "chime",
            quota_code: "L-398B2BD6",
            quota_name: "DeleteAttendee API burst rate",
            value: 20.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
        Row {
            service_code: "chime",
            quota_code: "L-B6F2C623",
            quota_name: "SIP trunking and voice - calls per second (CPS)",
            value: 1.0,
            unit: "None",
            adjustable: true,
            description: None,
            period: None,
            metric: None,
        },
    ]
}

/// The service codes whose recorded default-quota listings publish every row under an
/// AWS-minted code, so no row of theirs may carry a code this emulator minted.
///
/// SOURCE: the recorded `ListAWSDefaultServiceQuotas` response of each code, which keys
/// every row it carries on an `L-` code (and, for `autoscaling-plans` and `b2bi`, reports
/// this catalog's own code for the same quota as a surplus entry).
const MINTED_CODE_SERVICES: &[&str] = &[
    "amplifyuibuilder",
    "aoss",
    "application-signals",
    "appstudio",
    "auditmanager",
    "autoscaling-plans",
    "aws-marketplace",
    "aws-mcp",
    "b2bi",
    "chime",
];

/// `(ServiceCode, QuotaCode, why)` triples these services publish NOTHING under: the
/// codes their recorded listings report as surplus entries, and AWS codes of their own
/// namespace whose published name or value this catalog cannot cite.
const UNPUBLISHED_QUOTA_CODES: &[(&str, &str, &str)] = &[
    (
        "b2bi",
        "ProfilesPerAccount",
        "the code the service's own refusal reports, surplus once AWS's own code for the \
         quota is citable",
    ),
    (
        "autoscaling-plans",
        "ScalingPlansPerRegion",
        "a stand-in re-keyed onto the AWS code for the same quota",
    ),
    (
        "amplifyuibuilder",
        "L-A25B72F2",
        "an AWS code whose published size limit this catalog carries no value for",
    ),
    (
        "amplifyuibuilder",
        "L-429DD3BA",
        "an AWS code whose published per-app count this catalog carries no value for",
    ),
    (
        "aoss",
        "L-877395DB",
        "an AWS code whose published policy-size limit this catalog carries no value for",
    ),
    (
        "application-signals",
        "L-A3B40D72",
        "an AWS code whose published request rate this catalog carries no value for",
    ),
    (
        "auditmanager",
        "L-92B50F18",
        "an AWS code whose published assessment count this catalog carries no value for",
    ),
    (
        "aws-mcp",
        "L-24E93970",
        "an AWS code whose published tool rate this catalog carries no value for",
    ),
];

/// RULE: a published row reports the facts AWS publishes under its own code, through the
/// single read and the listing alike, at the Region it was read at.
#[tokio::test]
async fn a_published_row_carries_the_facts_aws_publishes_under_its_code() {
    let engine = emulator();
    let region = "eu-central-1";
    let caller = client(&engine, region);

    for row in rows() {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        let quota = caller
            .ok(
                "GetAWSDefaultServiceQuota",
                json!({"ServiceCode": row.service_code, "QuotaCode": row.quota_code}),
            )
            .await["Quota"]
            .clone();

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
        match row.description {
            Some(description) => assert_eq!(
                quota["Description"],
                json!(description),
                "case {case}: a description is the quota's own published text, got {quota}"
            ),
            None => assert_eq!(
                quota["Description"],
                Value::Null,
                "case {case}: a row whose published text is not citable carries none, got \
                 {quota}"
            ),
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
                "case {case}: a quota whose usage series this catalog cannot cite reports no \
                 metric, got {quota}"
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

/// RULE: every row these services publish sits in AWS's minted `L-` namespace, so a
/// client never reads a code AWS would reject out of one of their listings.
#[tokio::test]
async fn every_row_of_these_services_is_published_under_an_aws_minted_code() {
    let engine = emulator();
    let caller = client(&engine, "us-west-2");

    for service_code in MINTED_CODE_SERVICES {
        let listed = defaults(&caller, service_code).await;
        assert!(
            !listed.is_empty(),
            "case {service_code}: expected the service's published catalog, got no rows"
        );
        for quota in &listed {
            let code = quota["QuotaCode"].as_str().unwrap_or_default();
            let case = format!("{service_code}/{code}");
            let minted = code.len() == 10
                && code.starts_with("L-")
                && code[2..]
                    .chars()
                    .all(|char| char.is_ascii_digit() || char.is_ascii_uppercase());
            assert!(
                minted,
                "case {case}: every row of this service is published under an AWS-minted \
                 L-XXXXXXXX code, got {code:?} for {quota}"
            );
        }
    }
}

/// RULE: a code one of these services publishes nothing under resolves nothing, while
/// the service code itself keeps resolving its listing.
#[tokio::test]
async fn a_code_these_services_publish_nothing_under_resolves_nothing() {
    let engine = emulator();
    let caller = client(&engine, "us-east-1");

    for (service_code, quota_code, why) in UNPUBLISHED_QUOTA_CODES {
        let case = format!("{service_code}/{quota_code}");
        let listed = defaults(&caller, service_code).await;
        let codes: Vec<&str> = listed
            .iter()
            .filter_map(|quota| quota["QuotaCode"].as_str())
            .collect();
        assert!(
            !codes.contains(quota_code),
            "case {case} ({why}): an unpublished code must not be listed, got {codes:?}"
        );

        for operation in ["GetAWSDefaultServiceQuota", "GetServiceQuota"] {
            let (status, error) = caller
                .refused(
                    operation,
                    json!({"ServiceCode": service_code, "QuotaCode": quota_code}),
                )
                .await;
            assert_eq!(
                (status, error.as_str()),
                (400, "NoSuchResourceException"),
                "case {case} ({why}): {operation} must refuse a quota this service does not \
                 publish"
            );
        }

        // The service code still resolves: it is the QUOTA that is unpublished.
        assert!(
            !listed.is_empty(),
            "case {case}: the service must still serve its published catalog, got no rows"
        );
    }
}

/// RULE: `Adjustable` decides whether the service takes an increase request — a row
/// published as fixed refuses one, an adjustable row accepts it.
#[tokio::test]
async fn adjustability_decides_whether_an_increase_is_accepted() {
    let engine = emulator();
    let caller = client(&engine, "ap-southeast-2");

    for row in rows() {
        let case = format!("{}/{}", row.service_code, row.quota_code);
        // Above the value in force, so the only thing left to refuse the request for is
        // the quota's adjustability.
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
