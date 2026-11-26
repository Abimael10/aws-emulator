// SPDX-License-Identifier: CC0-1.0

//! The NAME each alarm of the AWS-provided cloudwatch alarm template catalog is
//! published under, end to end over the restJson1 wire.
//!
//! The rule under test: a published template's `Name` is the name the catalog
//! publishes that alarm under — the `AWS-` prefix every published monitor
//! resource's name carries, the metric the alarm watches followed by the
//! direction a breach of it lies in, then the workflow family of the group that
//! publishes it (`AWS-<Metric><High|Low>-<Workflow>Workflows`) — so a client
//! that holds such a name resolves the template by it, which
//! `GetCloudWatchAlarmTemplate`'s `Identifier` admits as readily as an id ("Can
//! be either be its id or current name").
//!
//! The suites are TABLES: the first spans EVERY published template, deriving the
//! expected name from what that template itself reports (its metric, its
//! comparison operator and the name of the group it belongs to), so the rule is
//! asserted over the whole catalog rather than over one row; the second spells
//! out published names a client pins, including the two breach directions and a
//! metric whose name carries a dot.
//!
//! Every test drives the service the way a client does, through
//! `MultiServiceRouter::dispatch` with real HTTP requests, so what is asserted is
//! the emulator's wire behaviour and not an internal function's return value.

use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{is_aws_provided, percent_encoded, GROUPS};
use crate::wire::{call_in_region, medialive, DEFAULT_REGION};

/// The template collection (`smithy_query(query="operation",
/// operation="ListCloudWatchAlarmTemplates")`: GET
/// /prod/cloudwatch-alarm-templates).
const TEMPLATES: &str = "/prod/cloudwatch-alarm-templates";

/// The prefix every published monitor template resource's `Name` carries, which
/// the published group names themselves are published under.
const PUBLISHED_NAME_PREFIX: &str = "AWS-";

/// The `CloudWatchAlarmTemplateComparisonOperator` values that trip when the
/// statistic falls UNDER the threshold (`smithy_query(query="shape",
/// shape="CloudWatchAlarmTemplateComparisonOperator")`), which is what makes a
/// published name read as a LOW alarm rather than a high one.
const UNDER_THRESHOLD: [&str; 2] = ["LessThanThreshold", "LessThanOrEqualToThreshold"];

/// One listing, answering the entries under `member`.
async fn list(
    router: &MultiServiceRouter,
    region: &str,
    uri: &str,
    member: &str,
) -> Vec<Value> {
    let (status, body) = call_in_region(router, region, "GET", uri, None).await;
    assert_eq!(
        status, 200,
        "expected GET {uri} to answer 200, got {status}: {body}"
    );
    body[member]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("expected a {member} list from GET {uri}, got {body}"))
}

/// The published groups of `region`, as `Id` -> `Name`.
async fn published_groups(router: &MultiServiceRouter, region: &str) -> BTreeMap<String, String> {
    list(router, region, GROUPS, "cloudWatchAlarmTemplateGroups")
        .await
        .iter()
        .filter(|group| is_aws_provided(group))
        .map(|group| {
            (
                group["id"].as_str().unwrap_or_default().to_string(),
                group["name"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Every published template of `region`, in the order the listing reports them.
async fn published_templates(router: &MultiServiceRouter, region: &str) -> Vec<Value> {
    let templates = list(router, region, TEMPLATES, "cloudWatchAlarmTemplates").await;
    let published: Vec<Value> = templates
        .into_iter()
        .filter(is_aws_provided)
        .collect();
    assert!(
        !published.is_empty(),
        "expected the published catalog to hold templates, got none"
    );
    published
}

/// `GetCloudWatchAlarmTemplate` on `identifier`
/// (`smithy_query(query="operation", operation="GetCloudWatchAlarmTemplate")`:
/// GET /prod/cloudwatch-alarm-templates/{Identifier}).
async fn get_template(
    router: &MultiServiceRouter,
    region: &str,
    identifier: &str,
) -> (u16, Value) {
    call_in_region(
        router,
        region,
        "GET",
        &format!("{TEMPLATES}/{}", percent_encoded(identifier)),
        None,
    )
    .await
}

/// One member of a reported template, as a string.
fn reported(template: &Value, member: &str) -> String {
    template[member].as_str().unwrap_or_default().to_string()
}

/// The name the rule says this published template is published under, derived
/// from what the template itself reports and from the `Name` of the group that
/// publishes it.
fn expected_name(template: &Value, group_name: &str) -> String {
    let direction = if UNDER_THRESHOLD.contains(&reported(template, "comparisonOperator").as_str())
    {
        "Low"
    } else {
        "High"
    };
    let family = group_name
        .strip_prefix(PUBLISHED_NAME_PREFIX)
        .unwrap_or(group_name);
    format!(
        "{PUBLISHED_NAME_PREFIX}{metric}{direction}-{family}",
        metric = reported(template, "metricName"),
    )
}

/// Rule: every published alarm template carries the name the catalog publishes
/// it under — the published prefix, its metric, the direction a breach lies in,
/// then the workflow family of its group — that name is unique across the
/// published catalog, and it resolves the same template through
/// `GetCloudWatchAlarmTemplate`.
///
/// The rows are EVERY published template, which differ along the metric they
/// watch, the comparison operator they breach on and the group that publishes
/// them; the template the issue cites is one of them.
#[tokio::test]
async fn every_published_template_is_named_as_the_catalog_publishes_it() {
    let router = medialive();
    let groups = published_groups(&router, DEFAULT_REGION).await;
    let templates = published_templates(&router, DEFAULT_REGION).await;

    let mut names: BTreeSet<String> = BTreeSet::new();
    for template in &templates {
        let id = reported(template, "id");
        let case = format!("published template {id}");
        let group_id = reported(template, "groupId");
        let group_name = groups.get(&group_id).unwrap_or_else(|| {
            panic!("case {case:?}: expected groupId {group_id:?} to name a published group")
        });

        let name = reported(template, "name");
        assert_eq!(
            name,
            expected_name(template, group_name),
            "case {case:?}: expected the published name of the {:?} alarm in {group_name}, \
             got {name:?}",
            reported(template, "metricName")
        );
        assert!(
            names.insert(name.clone()),
            "case {case:?}: expected every published name to be unique in the region, \
             got {name:?} twice"
        );

        // The name resolves that very template, which is what a client holding
        // an AWS-published name does with it.
        let (status, read) = get_template(&router, DEFAULT_REGION, &name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the published name {name:?} to resolve, got {status}: {read}"
        );
        assert_eq!(
            read["arn"], template["arn"],
            "case {case:?}: expected {name:?} to resolve the template that carries it, got {:?}",
            read["arn"]
        );
        assert_eq!(
            read["name"],
            json!(name),
            "case {case:?}: expected the resolved template to report the published name, got {:?}",
            read["name"]
        );
    }

    // A published name is a template's, not a group's: no published group is
    // published under a name one of the templates carries.
    for (group_id, group_name) in &groups {
        assert!(
            !names.contains(group_name),
            "case {group_id:?}: expected the group name {group_name:?} to name no template"
        );
    }
}

/// Rule: the published names a client pins resolve the alarms the catalog
/// publishes under them — the metric in the name is the metric the resolved
/// template watches, the direction in it is the side of the threshold its
/// comparison operator breaches on, and the family in it is the `Name` of the
/// group the template belongs to.
///
/// The rows differ along the breach direction the name reports (a high alarm and
/// a low one), the workflow family publishing it, and the FORM of the metric
/// name (one carrying a dot, which travels percent-encoded in the
/// `@httpLabel`). The name the issue cites is one row of them.
#[tokio::test]
async fn a_published_name_resolves_the_alarm_published_under_it() {
    let router = medialive();
    let groups = published_groups(&router, DEFAULT_REGION).await;

    // (the published name a client holds, the metric it names, the group it
    // belongs to, whether its alarm breaches UNDER the threshold)
    let table: [(&str, &str, &str, bool); 5] = [
        (
            "AWS-ChannelInputErrorSecondsHigh-MediaLiveUdpAndRtmpWorkflows",
            "ChannelInputErrorSeconds",
            "AWS-MediaLiveUdpAndRtmpWorkflows",
            false,
        ),
        (
            "AWS-InputLossSecondsHigh-MediaLiveToMediaPackageWorkflows",
            "InputLossSeconds",
            "AWS-MediaLiveToMediaPackageWorkflows",
            false,
        ),
        (
            "AWS-SourceConnectedLow-MediaConnectCdiJpegxsWorkflows",
            "SourceConnected",
            "AWS-MediaConnectCdiJpegxsWorkflows",
            true,
        ),
        (
            "AWS-4xxErrorRateHigh-CloudFrontGeneralWorkflows",
            "4xxErrorRate",
            "AWS-CloudFrontGeneralWorkflows",
            false,
        ),
        (
            "AWS-Avail.FillRateLow-MediaTailorAdInsertionWorkflows",
            "Avail.FillRate",
            "AWS-MediaTailorAdInsertionWorkflows",
            true,
        ),
    ];

    for (name, metric, group_name, under_threshold) in table {
        let case = name;
        let (status, read) = get_template(&router, DEFAULT_REGION, name).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected the published name to resolve, got {status}: {read}"
        );
        assert_eq!(
            read["name"],
            json!(name),
            "case {case:?}: expected the resolved template to report that name, got {:?}",
            read["name"]
        );
        assert_eq!(
            reported(&read, "metricName"),
            metric,
            "case {case:?}: expected the alarm on {metric:?}, got {:?}",
            read["metricName"]
        );
        let operator = reported(&read, "comparisonOperator");
        assert_eq!(
            UNDER_THRESHOLD.contains(&operator.as_str()),
            under_threshold,
            "case {case:?}: expected the direction in the name to match {operator:?}"
        );
        let group_id = reported(&read, "groupId");
        assert_eq!(
            groups.get(&group_id).map(String::as_str),
            Some(group_name),
            "case {case:?}: expected the template to belong to {group_name}, got group \
             {group_id:?}"
        );
    }
}
