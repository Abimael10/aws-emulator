// SPDX-License-Identifier: CC0-1.0

//! The Billing and Cost Management Dashboards resources a stack declares, end to end: both
//! types are types a template may DECLARE, each declaration is deployed by calling the
//! service that owns it, the handles the stack publishes resolve there, a changed
//! declaration reaches the resource in place, a declaration that service refuses rolls the
//! stack back with its own words, and deleting the stack takes both away.
//!
//! RULE: a resource type AWS publishes is one a template may declare, and a declaration of
//! it is DEPLOYED by calling the service that owns the type — so `CreateStack` never
//! answers `Template format error: Unrecognized resource types`, the handle the stack
//! publishes (`Ref`) resolves in that service, the values the type publishes are the ones
//! that service reported, and a value that service refuses fails the resource rather than
//! the submission.
//!
//! The rows differ along what the rule generalises over: which of the two types the body
//! declares and how many of each, the widget count and the display a widget uses (a graph
//! and a table), whether the declaration carries a description, widget sizes or the type's
//! own `Tags`, whether a report names a widget selection and a date-range override, the
//! schedule state, and the template spelling (JSON with long-form intrinsics, YAML with the
//! short-form tags).
//!
//! Everything goes over the wire each service speaks through the one router `harness.rs`
//! builds — the CloudFormation calls over its awsQuery wire, the Billing and Cost
//! Management Dashboards calls over that service's own `awsJson1_0` wire — so what is
//! asserted is what a client that deploys the template and then calls that service
//! observes.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::resource_types::{
    is_published, is_recognized, AWS_RESOURCE_TYPES,
};
use service_com_amazonaws_cloudformation_cloudformation::state::template::{
    identity_is_service_assigned, published_attribute_names, replacement_properties, BCM_DASHBOARD,
    BCM_SCHEDULED_REPORT,
};

use crate::harness::{
    call, create_stack, failed_resource_reason, member, quoted_and_class, router,
    settled_stack_status, stack_event_reasons, REGION, SETTLE_DEADLINE, SETTLE_POLL_INTERVAL,
};
use crate::stack_lifecycle::{describe_stack, output_value};

/// The fixed text CloudFormation refuses a template declaring a type it does NOT know with,
/// which no row of this suite may ever see.
const UNRECOGNIZED_TYPES: &str = "Unrecognized resource types";

/// The execution role a scheduled report assumes, of the form `ServiceRoleArn`'s own
/// pattern admits (`^arn:aws[-a-z0-9]*:iam::[0-9]{12}:role/[a-zA-Z0-9+=,.@_/-]+$`). No role
/// has to exist for the report to be created: that service resolves the role only when it
/// resolves the report's health.
const EXECUTION_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/bcm-scheduled-report";

/// The key a stack of this suite carries its own tag under, which CloudFormation propagates
/// to every resource it creates.
const STACK_TAG_KEY: &str = "sim2real";

/// The tag-key namespace AWS reserves for itself, whose three `aws:cloudformation:*` keys
/// that service refuses to a caller — so a resource a stack created carries the stack's own
/// tags and none of those three.
const RESERVED_TAG_PREFIX: &str = "aws:";

/// The one value `DashboardType` publishes, which is what that service records a dashboard
/// a client created as (source: `smithy_query(query="shape", shape="DashboardType")` on
/// that service's own model).
const CUSTOM: &str = "CUSTOM";

/// The error code that service answers for a resource it does not hold, and the status that
/// answer carries: its model's `@httpError` on `ResourceNotFoundException` is 404, while
/// that service answers a not-found with HTTP 400 on every read
/// (`scaffolded::RESOURCE_NOT_FOUND_STATUS` in its own crate, which records why).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const NOT_FOUND_STATUS: u16 = 400;

/// One call of that service over the `awsJson1_0` wire its model declares: the operation in
/// `X-Amz-Target` after its service shape's own name, signed for `bcm-dashboards` — that
/// model's own `sigv4Name` — in the Region the stack's resources live in.
async fn bcm(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://bcm-dashboards.{REGION}.amazonaws.com/"))
        .header("host", format!("bcm-dashboards.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AWSBCMDashboardsService.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bcm-dashboards/\
                 aws4_request, SignedHeaders=host, Signature=0123456789abcdef"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed Billing and Cost Management Dashboards request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// That service's own read of one dashboard, by the ARN it minted.
async fn get_dashboard(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    bcm(router, "GetDashboard", &json!({"arn": arn})).await
}

/// That service's own read of one scheduled report, unwrapped from the member its read
/// answers one in.
async fn get_scheduled_report(router: &MultiServiceRouter, arn: &str) -> (u16, Value) {
    let (status, answered) = bcm(router, "GetScheduledReport", &json!({"arn": arn})).await;
    let report = answered
        .get("scheduledReport")
        .cloned()
        .unwrap_or(answered);
    (status, report)
}

/// The tags that service holds on one resource, by the ARN either kind is addressed by.
async fn resource_tags(router: &MultiServiceRouter, arn: &str) -> BTreeMap<String, String> {
    let (status, answered) = bcm(router, "ListTagsForResource", &json!({"resourceArn": arn})).await;
    assert_eq!(
        status, 200,
        "ListTagsForResource must answer the tags of {arn:?}, got {status} with {answered}"
    );
    answered
        .get("resourceTags")
        .and_then(Value::as_array)
        .map(|tags| {
            tags.iter()
                .filter_map(|tag| {
                    Some((
                        tag.get("key").and_then(Value::as_str)?.to_string(),
                        tag.get("value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Every dashboard ARN that service lists for the account that a STACK could have created:
/// one under an ARN of the form that service mints for a dashboard a client creates.
///
/// The account's own MANAGED dashboards are listed beside them — that service publishes a
/// catalogue of them under ARNs of its own — and no stack created or can delete one, so
/// they are not what a case about a stack's resources asks about.
async fn listed_custom_dashboards(router: &MultiServiceRouter) -> Vec<String> {
    let (_, page) = bcm(router, "ListDashboards", &json!({"maxResults": 100})).await;
    page.get("dashboards")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("arn").and_then(Value::as_str))
                .filter(|arn| is_minted_arn(arn, "dashboard"))
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// A widget config whose data comes from a cost-and-usage query over a RELATIVE period and
/// is displayed as a graph, spelled the way a template spells a type's properties.
///
/// `metric` is a `MetricName` valid for a cost-and-usage query (the model's note on
/// `CostAndUsageQuery$metrics` admits `AmortizedCost`, `BlendedCost`, `NetAmortizedCost`,
/// `NetUnblendedCost`, `NormalizedUsageAmount`, `UnblendedCost` and `UsageQuantity`),
/// `visual` a `VisualType` value and `granularity` a `Granularity` value (sources: the
/// `CostAndUsageQuery`, `VisualType` and `Granularity` shapes of that model). The graph
/// display map is keyed BY the metric, which is a value rather than a member name.
fn graph_config(metric: &str, visual: &str, granularity: &str) -> Value {
    json!({
        "QueryParameters": {
            "CostAndUsage": {
                "Metrics": [metric],
                "TimeRange": {
                    "StartTime": {"Type": "RELATIVE", "Value": "-P3M"},
                    "EndTime": {"Type": "RELATIVE", "Value": "P0D"}
                },
                "Granularity": granularity
            }
        },
        "DisplayConfig": {"Graph": {metric: {"VisualType": visual}}}
    })
}

/// A widget config over an ABSOLUTE period displayed as a table — the other arm of both
/// unions a config is built from.
fn table_config(metric: &str) -> Value {
    json!({
        "QueryParameters": {
            "CostAndUsage": {
                "Metrics": [metric],
                "TimeRange": {
                    "StartTime": {"Type": "ABSOLUTE", "Value": "2025-01-01"},
                    "EndTime": {"Type": "ABSOLUTE", "Value": "2025-07-01"}
                },
                "Granularity": "DAILY"
            }
        },
        "DisplayConfig": {"Table": {}}
    })
}

/// What one row expects of one dashboard the stack declares.
struct DeclaredDashboard {
    /// The output key whose value is the dashboard's `Ref` — the ARN that service minted.
    ref_output: &'static str,
    name: &'static str,
    description: Option<&'static str>,
    /// The titles its widgets must carry, in declaration order.
    widget_titles: &'static [&'static str],
    /// The tags its own declaration asks for, beside the stack's.
    declared_tags: &'static [(&'static str, &'static str)],
}

/// What one row expects of one scheduled report the stack declares.
struct DeclaredReport {
    ref_output: &'static str,
    name: &'static str,
    /// The output key carrying the ARN of the dashboard the report reports on.
    dashboard_ref_output: &'static str,
    schedule_expression: &'static str,
    /// The schedule state the report must carry: the declaration's own, or the `ENABLED`
    /// that service resolves an unstated one to.
    state: &'static str,
}

/// One row: a template body, and the resources it declares.
struct Row {
    case: &'static str,
    stack_name: &'static str,
    template: String,
    dashboards: Vec<DeclaredDashboard>,
    reports: Vec<DeclaredReport>,
}

fn rows() -> Vec<Row> {
    vec![
        // One dashboard with one graph widget and nothing optional, with every published
        // return value read back through `Outputs`.
        Row {
            case: "one dashboard, one graph widget, every return value read back",
            stack_name: "bcm-dashboard-round-trip",
            template: json!({
                "AWSTemplateFormatVersion": "2010-09-09",
                "Resources": {
                    "Board": {
                        "Type": BCM_DASHBOARD,
                        "Properties": {
                            "Name": "Cost Overview",
                            "Widgets": [{
                                "Title": "Monthly Spend",
                                "Configs": [graph_config("UnblendedCost", "LINE", "MONTHLY")]
                            }]
                        }
                    }
                },
                "Outputs": {
                    "BoardRef": {"Value": {"Ref": "Board"}},
                    "BoardArn": {"Value": {"Fn::GetAtt": ["Board", "Arn"]}},
                    "BoardType": {"Value": {"Fn::GetAtt": ["Board", "Type"]}},
                    "BoardCreatedAt": {"Value": {"Fn::GetAtt": ["Board", "CreatedAt"]}},
                    "BoardUpdatedAt": {"Value": {"Fn::GetAtt": ["Board", "UpdatedAt"]}}
                }
            })
            .to_string(),
            dashboards: vec![DeclaredDashboard {
                ref_output: "BoardRef",
                name: "Cost Overview",
                description: None,
                widget_titles: &["Monthly Spend"],
                declared_tags: &[],
            }],
            reports: Vec::new(),
        },
        // Two widgets — one graph, one table — a description, declared sizes and the type's
        // own `Tags`, with a scheduled report ON that dashboard whose schedule is explicitly
        // disabled and which names the report's own description.
        Row {
            case: "a described dashboard with two widgets and Tags, and a report on it",
            stack_name: "bcm-scheduled-report-round-trip",
            template: json!({
                "Resources": {
                    "Board": {
                        "Type": BCM_DASHBOARD,
                        "Properties": {
                            "Name": "Finops Board",
                            "Description": "Spend and usage.",
                            "Widgets": [
                                {
                                    "Title": "Amortized Spend",
                                    "Description": "Amortized cost by month.",
                                    "Width": 6,
                                    "Height": 9,
                                    "HorizontalOffset": 0,
                                    "Configs": [graph_config("AmortizedCost", "BAR", "DAILY")]
                                },
                                {
                                    "Title": "Usage Table",
                                    "Configs": [table_config("UsageQuantity")]
                                }
                            ],
                            "Tags": [{"Key": "team", "Value": "finops"}]
                        }
                    },
                    "Report": {
                        "Type": BCM_SCHEDULED_REPORT,
                        "Properties": {
                            "Name": "Monthly Finops Report",
                            "Description": "First of the month.",
                            "DashboardArn": {"Ref": "Board"},
                            "ScheduledReportExecutionRoleArn": EXECUTION_ROLE_ARN,
                            "ScheduleConfig": {
                                "ScheduleExpression": "cron(0 8 1 * ? *)",
                                "State": "DISABLED"
                            },
                            "Tags": [{"Key": "tier", "Value": "gold"}]
                        }
                    }
                },
                "Outputs": {
                    "BoardRef": {"Value": {"Ref": "Board"}},
                    "ReportRef": {"Value": {"Ref": "Report"}},
                    "ReportCreatedAt": {"Value": {"Fn::GetAtt": ["Report", "CreatedAt"]}},
                    "ReportUpdatedAt": {"Value": {"Fn::GetAtt": ["Report", "UpdatedAt"]}}
                }
            })
            .to_string(),
            dashboards: vec![DeclaredDashboard {
                ref_output: "BoardRef",
                name: "Finops Board",
                description: Some("Spend and usage."),
                widget_titles: &["Amortized Spend", "Usage Table"],
                declared_tags: &[("team", "finops")],
            }],
            reports: vec![DeclaredReport {
                ref_output: "ReportRef",
                name: "Monthly Finops Report",
                dashboard_ref_output: "BoardRef",
                schedule_expression: "cron(0 8 1 * ? *)",
                state: "DISABLED",
            }],
        },
        // The YAML spelling with the short-form intrinsics, a report whose schedule states no
        // state at all — so it carries the `ENABLED` that service resolves — and no widget
        // selection.
        Row {
            case: "the YAML spelling, a report with no declared schedule state",
            stack_name: "bcm-dashboard-yaml",
            template: format!(
                "\
Resources:
  Board:
    Type: {BCM_DASHBOARD}
    Properties:
      Name: Yaml Board
      Widgets:
        - Title: Net Spend
          Configs:
            - QueryParameters:
                CostAndUsage:
                  Metrics:
                    - NetUnblendedCost
                  TimeRange:
                    StartTime:
                      Type: RELATIVE
                      Value: -P3M
                    EndTime:
                      Type: RELATIVE
                      Value: P0D
                  Granularity: MONTHLY
              DisplayConfig:
                Graph:
                  NetUnblendedCost:
                    VisualType: STACK
  Report:
    Type: {BCM_SCHEDULED_REPORT}
    Properties:
      Name: Yaml Report
      DashboardArn: !Ref Board
      ScheduledReportExecutionRoleArn: {EXECUTION_ROLE_ARN}
      ScheduleConfig:
        ScheduleExpression: cron(0 7 ? * MON *)
Outputs:
  BoardRef:
    Value: !Ref Board
  ReportRef:
    Value: !Ref Report
"
            ),
            dashboards: vec![DeclaredDashboard {
                ref_output: "BoardRef",
                name: "Yaml Board",
                description: None,
                widget_titles: &["Net Spend"],
                declared_tags: &[],
            }],
            reports: vec![DeclaredReport {
                ref_output: "ReportRef",
                name: "Yaml Report",
                dashboard_ref_output: "BoardRef",
                schedule_expression: "cron(0 7 ? * MON *)",
                state: "ENABLED",
            }],
        },
        // Two dashboards in one stack, each with its own report: each resource publishes its
        // OWN ARN and is its own record in the owning service.
        Row {
            case: "two dashboards in one stack, each with its own report",
            stack_name: "bcm-dashboard-pair",
            template: json!({
                "Resources": {
                    "First": {
                        "Type": BCM_DASHBOARD,
                        "Properties": {
                            "Name": "Pair First",
                            "Widgets": [{
                                "Title": "Blended Spend",
                                "Configs": [graph_config("BlendedCost", "LINE", "HOURLY")]
                            }]
                        }
                    },
                    "Second": {
                        "Type": BCM_DASHBOARD,
                        "Properties": {
                            "Name": "Pair Second",
                            "Widgets": []
                        }
                    },
                    "FirstReport": {
                        "Type": BCM_SCHEDULED_REPORT,
                        "Properties": {
                            "Name": "Pair First Report",
                            "DashboardArn": {"Ref": "First"},
                            "ScheduledReportExecutionRoleArn": EXECUTION_ROLE_ARN,
                            "ScheduleConfig": {"ScheduleExpression": "cron(0/15 * * * ? *)"}
                        }
                    },
                    "SecondReport": {
                        "Type": BCM_SCHEDULED_REPORT,
                        "Properties": {
                            "Name": "Pair Second Report",
                            "DashboardArn": {"Ref": "Second"},
                            "ScheduledReportExecutionRoleArn": EXECUTION_ROLE_ARN,
                            "ScheduleConfig": {
                                "ScheduleExpression": "cron(0 6 ? * SUN *)",
                                "State": "ENABLED"
                            }
                        }
                    }
                },
                "Outputs": {
                    "FirstRef": {"Value": {"Ref": "First"}},
                    "SecondRef": {"Value": {"Ref": "Second"}},
                    "FirstReportRef": {"Value": {"Ref": "FirstReport"}},
                    "SecondReportRef": {"Value": {"Ref": "SecondReport"}}
                }
            })
            .to_string(),
            dashboards: vec![
                DeclaredDashboard {
                    ref_output: "FirstRef",
                    name: "Pair First",
                    description: None,
                    widget_titles: &["Blended Spend"],
                    declared_tags: &[],
                },
                DeclaredDashboard {
                    ref_output: "SecondRef",
                    name: "Pair Second",
                    description: None,
                    widget_titles: &[],
                    declared_tags: &[],
                },
            ],
            reports: vec![
                DeclaredReport {
                    ref_output: "FirstReportRef",
                    name: "Pair First Report",
                    dashboard_ref_output: "FirstRef",
                    schedule_expression: "cron(0/15 * * * ? *)",
                    state: "ENABLED",
                },
                DeclaredReport {
                    ref_output: "SecondReportRef",
                    name: "Pair Second Report",
                    dashboard_ref_output: "SecondRef",
                    schedule_expression: "cron(0 6 ? * SUN *)",
                    state: "ENABLED",
                },
            ],
        },
    ]
}

/// Submit `template` as `stack_name` under the stack's own tag, and answer the
/// `CreateStack` body: a type AWS publishes is ADMITTED, so this never sees the
/// template-format refusal.
async fn submitted(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    let (status, body) = create_stack(router, stack_name, template, &stack_tag(stack_name)).await;
    assert!(
        !body.contains(UNRECOGNIZED_TYPES),
        "case {case:?}: expected the two types Amazon Web Services publishes to be recognised, \
         got {body}"
    );
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CreateStack — {BCM_DASHBOARD} and \
         {BCM_SCHEDULED_REPORT} are types a template may declare — got {status} with body {body}"
    );
    body
}

/// The stack-level tag every stack of this suite carries, which CloudFormation propagates
/// to the resources it creates.
fn stack_tag(stack_name: &str) -> Vec<(String, String)> {
    vec![
        member("Tags.member.1.Key", STACK_TAG_KEY),
        member("Tags.member.1.Value", stack_name),
    ]
}

/// Deploy `template` as `stack_name` and answer the `DescribeStacks` body of the COMPLETED
/// stack, quoting the stack's own failure reasons when the create did not complete.
async fn deployed(
    router: &MultiServiceRouter,
    case: &str,
    stack_name: &str,
    template: &str,
) -> String {
    submitted(router, case, stack_name, template).await;
    let reached = settled_stack_status(case, router, REGION, stack_name).await;
    assert_eq!(
        reached, "CREATE_COMPLETE",
        "case {case:?}: expected the create to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(router, REGION, stack_name).await
    );
    let (status, described) = describe_stack(router, stack_name).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeStacks, got {status} with {described}"
    );
    described
}

/// One output of a settled stack, or an explanation naming the body it is missing from.
fn published(case: &str, described: &str, key: &str) -> String {
    output_value(described, key)
        .unwrap_or_else(|| panic!("case {case:?}: no {key} output in {described}"))
}

/// Whether `arn` is an ARN of the form that service mints for `resource_type`: the ARN
/// patterns of its model spell an EMPTY region segment, the canonical account, and a
/// `<type>/<id>` resource segment whose id is 32 lowercase hexadecimal characters.
fn is_minted_arn(arn: &str, resource_type: &str) -> bool {
    let segments: Vec<&str> = arn.split(':').collect();
    if segments.len() != 6 || segments[0] != "arn" || segments[2] != "bcm-dashboards" {
        return false;
    }
    if !segments[1].starts_with("aws") || !segments[3].is_empty() {
        return false;
    }
    if segments[4].len() != 12 || !segments[4].chars().all(|c| c.is_ascii_digit()) {
        return false;
    }
    let Some((kind, id)) = segments[5].split_once('/') else {
        return false;
    };
    kind == resource_type
        && id.len() == 32
        && id
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
}

/// The titles of the widgets one reported dashboard carries, in stored order.
fn reported_widget_titles(dashboard: &Value) -> Vec<String> {
    dashboard
        .get("widgets")
        .and_then(Value::as_array)
        .map(|widgets| {
            widgets
                .iter()
                .map(|widget| {
                    widget
                        .get("title")
                        .and_then(Value::as_str)
                        .unwrap_or("<none>")
                        .to_string()
                })
                .collect()
        })
        .unwrap_or_default()
}

/// THE RULE, the positive direction: every resource the stack reports as created is a
/// record that service holds, under the ARN the stack published, configured as the template
/// declared — and the values the type publishes are the ones that service reported.
#[tokio::test]
async fn a_deployed_stack_publishes_the_resources_the_owning_service_created() {
    for row in rows() {
        let router = router();
        let case = row.case;
        let described = deployed(&router, case, row.stack_name, &row.template).await;

        for declared in &row.dashboards {
            let arn = published(case, &described, declared.ref_output);
            assert!(
                is_minted_arn(&arn, "dashboard"),
                "case {case:?}: expected the published handle to be a dashboard ARN that \
                 service mints, got {arn:?}"
            );

            let (status, reported) = get_dashboard(&router, &arn).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the owning service to report the dashboard the stack \
                 created under {arn:?}, got {status} with {reported}"
            );
            assert_eq!(
                reported.get("arn").and_then(Value::as_str),
                Some(arn.as_str()),
                "case {case:?}: expected the dashboard to carry the published ARN, got {reported}"
            );
            assert_eq!(
                reported.get("name").and_then(Value::as_str),
                Some(declared.name),
                "case {case:?}: expected the declared name, got {reported}"
            );
            assert_eq!(
                reported.get("description").and_then(Value::as_str),
                declared.description,
                "case {case:?}: expected the declared description, got {reported}"
            );
            let titles = reported_widget_titles(&reported);
            assert_eq!(
                titles, declared.widget_titles,
                "case {case:?}: expected the declared widgets in declaration order, got \
                 {reported}"
            );

            // ...and the values the type publishes are that service's own rather than
            // stand-ins.
            if declared.ref_output == "BoardRef" && row.stack_name == "bcm-dashboard-round-trip" {
                assert_eq!(
                    published(case, &described, "BoardArn"),
                    arn,
                    "case {case:?}: expected the published Arn attribute to be the ARN that \
                     service minted"
                );
                assert_eq!(
                    published(case, &described, "BoardType"),
                    CUSTOM,
                    "case {case:?}: expected the published Type to be the kind that service \
                     records, got {reported}"
                );
                for (key, member) in [("BoardCreatedAt", "createdAt"), ("BoardUpdatedAt", "updatedAt")] {
                    let value = published(case, &described, key);
                    assert_published_instant(case, &value, &reported, member);
                }
            }

            // The tags the resource carries are the declaration's own beside the stack's,
            // and never one of the three keys that service reserves.
            let held = resource_tags(&router, &arn).await;
            assert_eq!(
                held.get(STACK_TAG_KEY).map(String::as_str),
                Some(row.stack_name),
                "case {case:?}: expected the stack's own tag on the dashboard, got {held:?}"
            );
            for (key, value) in declared.declared_tags {
                assert_eq!(
                    held.get(*key).map(String::as_str),
                    Some(*value),
                    "case {case:?}: expected the declared tag {key:?}, got {held:?}"
                );
            }
            assert!(
                held.keys().all(|key| !key.starts_with(RESERVED_TAG_PREFIX)),
                "case {case:?}: expected no reserved tag key on the dashboard, got {held:?}"
            );
        }

        for declared in &row.reports {
            let arn = published(case, &described, declared.ref_output);
            assert!(
                is_minted_arn(&arn, "scheduled-report"),
                "case {case:?}: expected the published handle to be a scheduled-report ARN that \
                 service mints, got {arn:?}"
            );
            let dashboard_arn = published(case, &described, declared.dashboard_ref_output);

            let (status, reported) = get_scheduled_report(&router, &arn).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected the owning service to report the scheduled report the \
                 stack created under {arn:?}, got {status} with {reported}"
            );
            assert_eq!(
                (
                    reported.get("arn").and_then(Value::as_str),
                    reported.get("name").and_then(Value::as_str),
                    reported.get("dashboardArn").and_then(Value::as_str),
                    reported
                        .get("scheduledReportExecutionRoleArn")
                        .and_then(Value::as_str),
                ),
                (
                    Some(arn.as_str()),
                    Some(declared.name),
                    Some(dashboard_arn.as_str()),
                    Some(EXECUTION_ROLE_ARN),
                ),
                "case {case:?}: expected the report's declared configuration, got {reported}"
            );
            assert_eq!(
                (
                    reported
                        .pointer("/scheduleConfig/scheduleExpression")
                        .and_then(Value::as_str),
                    reported
                        .pointer("/scheduleConfig/state")
                        .and_then(Value::as_str),
                ),
                (Some(declared.schedule_expression), Some(declared.state)),
                "case {case:?}: expected the declared schedule, got {reported}"
            );
            let held = resource_tags(&router, &arn).await;
            assert_eq!(
                held.get(STACK_TAG_KEY).map(String::as_str),
                Some(row.stack_name),
                "case {case:?}: expected the stack's own tag on the report, got {held:?}"
            );

            // The instants the type publishes are that service's own, written at the
            // precision a published return value carries.
            if row.stack_name == "bcm-scheduled-report-round-trip" {
                for (key, member) in [
                    ("ReportCreatedAt", "createdAt"),
                    ("ReportUpdatedAt", "updatedAt"),
                ] {
                    let value = published(case, &described, key);
                    assert_published_instant(case, &value, &reported, member);
                }
            }

        }
    }
}

/// How many fractional digits the instant a published return value carries has: AWS writes
/// one at MILLISECOND precision, trailing zeros included.
const PUBLISHED_FRACTION_DIGITS: usize = 3;

/// Assert the published attribute `key` is the instant the reported record carries in
/// `member`, written the way a published return value writes one: an ISO-8601 UTC instant
/// with exactly [`PUBLISHED_FRACTION_DIGITS`] fractional digits, denoting the same instant
/// as the value that service reported to within one millisecond.
///
/// Asserted as that INVARIANT rather than against a second rendering of the same value, so
/// the case cannot pass by re-deriving whatever the implementation happens to do.
fn assert_published_instant(case: &str, published: &str, reported: &Value, member: &str) {
    let fraction = published
        .strip_suffix('Z')
        .and_then(|body| body.split_once('.').map(|(_, fraction)| fraction))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a published UTC instant with a fraction, got {published:?}")
        });
    assert_eq!(
        fraction.len(),
        PUBLISHED_FRACTION_DIGITS,
        "case {case:?}: expected {PUBLISHED_FRACTION_DIGITS} fractional digits, got {published:?}"
    );
    let parsed =
        aws_smithy_types::DateTime::from_str(published, aws_smithy_types::date_time::Format::DateTime)
            .unwrap_or_else(|error| {
                panic!("case {case:?}: expected {published:?} to be an instant, got {error}")
            })
            .as_secs_f64();
    let seconds = reported
        .get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: no {member} on the reported record {reported}"));
    let behind = seconds - parsed;
    assert!(
        (0.0..=0.001).contains(&behind),
        "case {case:?}: expected the published {published:?} to be the instant the record \
         reports ({seconds}) at its milliseconds, it is {behind} seconds away"
    );
}

/// A changed declaration reaches the resource IN PLACE: the ARNs the stack publishes do not
/// move, and the owning service reports what the new template declares — including a
/// renamed dashboard, since that service renames one by the ARN it minted.
#[tokio::test]
async fn a_changed_declaration_reaches_the_resources_in_place() {
    let router = router();
    let case = "a renamed dashboard, a replaced widget set and a changed schedule";
    let stack_name = "bcm-dashboard-updated";
    let before = json!({
        "Resources": {
            "Board": {
                "Type": BCM_DASHBOARD,
                "Properties": {
                    "Name": "Before Board",
                    "Description": "Before.",
                    "Widgets": [{
                        "Title": "Before Widget",
                        "Configs": [graph_config("UnblendedCost", "LINE", "MONTHLY")]
                    }],
                    "Tags": [{"Key": "stage", "Value": "before"}]
                }
            },
            "Report": {
                "Type": BCM_SCHEDULED_REPORT,
                "Properties": {
                    "Name": "Before Report",
                    "DashboardArn": {"Ref": "Board"},
                    "ScheduledReportExecutionRoleArn": EXECUTION_ROLE_ARN,
                    "ScheduleConfig": {"ScheduleExpression": "cron(0 8 1 * ? *)"}
                }
            }
        },
        "Outputs": {
            "BoardRef": {"Value": {"Ref": "Board"}},
            "ReportRef": {"Value": {"Ref": "Report"}}
        }
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &before).await;
    let board = published(case, &described, "BoardRef");
    let report = published(case, &described, "ReportRef");

    let after = json!({
        "Resources": {
            "Board": {
                "Type": BCM_DASHBOARD,
                "Properties": {
                    "Name": "After Board",
                    "Description": "After.",
                    "Widgets": [
                        {
                            "Title": "After Widget",
                            "Configs": [table_config("AmortizedCost")]
                        },
                        {
                            "Title": "Second Widget",
                            "Configs": [graph_config("UsageQuantity", "BAR", "DAILY")]
                        }
                    ],
                    "Tags": [{"Key": "stage", "Value": "after"}]
                }
            },
            "Report": {
                "Type": BCM_SCHEDULED_REPORT,
                "Properties": {
                    "Name": "After Report",
                    "DashboardArn": {"Ref": "Board"},
                    "ScheduledReportExecutionRoleArn": EXECUTION_ROLE_ARN,
                    "ScheduleConfig": {
                        "ScheduleExpression": "cron(0 9 2 * ? *)",
                        "State": "DISABLED"
                    }
                }
            }
        },
        "Outputs": {
            "BoardRef": {"Value": {"Ref": "Board"}},
            "ReportRef": {"Value": {"Ref": "Report"}}
        }
    })
    .to_string();

    let (status, body) = call(
        &router,
        "UpdateStack",
        &[
            member("StackName", stack_name),
            member("TemplateBody", &after),
            member("Tags.member.1.Key", STACK_TAG_KEY),
            member("Tags.member.1.Value", stack_name),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {body}"
    );
    let reached = settled_stack_status(case, &router, REGION, stack_name).await;
    assert_eq!(
        reached, "UPDATE_COMPLETE",
        "case {case:?}: expected the update to complete, got {reached} with failure reasons {:?}",
        stack_event_reasons(&router, REGION, stack_name).await
    );
    let described = describe_stack(&router, stack_name).await.1;

    // The handles did not move: the resources were changed, not replaced.
    assert_eq!(
        (
            published(case, &described, "BoardRef"),
            published(case, &described, "ReportRef")
        ),
        (board.clone(), report.clone()),
        "case {case:?}: expected the same ARNs after the update"
    );

    let (status, reported) = get_dashboard(&router, &board).await;
    assert_eq!(
        (
            status,
            reported.get("name").and_then(Value::as_str),
            reported.get("description").and_then(Value::as_str),
        ),
        (200, Some("After Board"), Some("After.")),
        "case {case:?}: expected the changed declaration on the dashboard, got {reported}"
    );
    assert_eq!(
        reported_widget_titles(&reported),
        vec!["After Widget".to_string(), "Second Widget".to_string()],
        "case {case:?}: expected the declared widget set, got {reported}"
    );
    let held = resource_tags(&router, &board).await;
    assert_eq!(
        held.get("stage").map(String::as_str),
        Some("after"),
        "case {case:?}: expected the re-valued declared tag, got {held:?}"
    );

    let (status, reported) = get_scheduled_report(&router, &report).await;
    assert_eq!(
        (
            status,
            reported.get("name").and_then(Value::as_str),
            reported
                .pointer("/scheduleConfig/scheduleExpression")
                .and_then(Value::as_str),
            reported
                .pointer("/scheduleConfig/state")
                .and_then(Value::as_str),
        ),
        (
            200,
            Some("After Report"),
            Some("cron(0 9 2 * ? *)"),
            Some("DISABLED")
        ),
        "case {case:?}: expected the changed declaration on the report, got {reported}"
    );
}

/// Deleting the stack takes both resources away from the owning service, which is what
/// makes a stack's resources the stack's own rather than leftovers.
#[tokio::test]
async fn deleting_the_stack_takes_both_resources_away() {
    let router = router();
    let case = "a dashboard and its report removed with the stack";
    let stack_name = "bcm-dashboard-deleted";
    let template = json!({
        "Resources": {
            "Board": {
                "Type": BCM_DASHBOARD,
                "Properties": {
                    "Name": "Deleted Board",
                    "Widgets": [{
                        "Title": "Spend",
                        "Configs": [graph_config("UnblendedCost", "LINE", "MONTHLY")]
                    }]
                }
            },
            "Report": {
                "Type": BCM_SCHEDULED_REPORT,
                "Properties": {
                    "Name": "Deleted Report",
                    "DashboardArn": {"Ref": "Board"},
                    "ScheduledReportExecutionRoleArn": EXECUTION_ROLE_ARN,
                    "ScheduleConfig": {"ScheduleExpression": "cron(0 8 1 * ? *)"}
                }
            }
        },
        "Outputs": {
            "BoardRef": {"Value": {"Ref": "Board"}},
            "ReportRef": {"Value": {"Ref": "Report"}}
        }
    })
    .to_string();

    let described = deployed(&router, case, stack_name, &template).await;
    let board = published(case, &described, "BoardRef");
    let report = published(case, &described, "ReportRef");
    for (kind, arn) in [("dashboard", &board), ("scheduled report", &report)] {
        let status = if kind == "dashboard" {
            get_dashboard(&router, arn).await.0
        } else {
            get_scheduled_report(&router, arn).await.0
        };
        assert_eq!(
            status, 200,
            "case {case:?}: expected the {kind} {arn:?} to exist before the delete, got {status}"
        );
    }

    let (status, deleted) = call(&router, "DeleteStack", &[member("StackName", stack_name)]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DeleteStack, got {status} with body {deleted}"
    );

    // Wait for the CONDITION — both records released — under the harness's own generous
    // deadline, so a loaded host cannot decide the outcome.
    let deadline = std::time::Instant::now() + SETTLE_DEADLINE;
    loop {
        let dashboard_status = get_dashboard(&router, &board).await.0;
        let report_status = get_scheduled_report(&router, &report).await.0;
        if dashboard_status != 200 && report_status != 200 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "case {case:?}: expected both records to be released, the dashboard answered \
             {dashboard_status} and the report {report_status}"
        );
        tokio::time::sleep(SETTLE_POLL_INTERVAL).await;
    }

    for (kind, arn) in [("dashboard", &board), ("scheduled report", &report)] {
        let (status, answered) = if kind == "dashboard" {
            get_dashboard(&router, arn).await
        } else {
            bcm(&router, "GetScheduledReport", &json!({"arn": arn})).await
        };
        assert_eq!(
            status, NOT_FOUND_STATUS,
            "case {case:?}: expected the {kind} to be gone with the stack, got {status} with \
             {answered}"
        );
        let code = answered
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            code.ends_with(RESOURCE_NOT_FOUND),
            "case {case:?}: expected {RESOURCE_NOT_FOUND} for the removed {kind}, got {answered}"
        );
    }
    assert!(
        listed_custom_dashboards(&router).await.is_empty(),
        "case {case:?}: expected no dashboard the stack created left in the account, got {:?}",
        listed_custom_dashboards(&router).await
    );
}

/// One refusal row: a widget title the owning service refuses, and why.
struct Refused {
    case: &'static str,
    stack_name: &'static str,
    title: String,
}

/// The rows differ along WHICH rule of `WidgetTitle` the title breaks — its modelled
/// `@pattern` `^(?!.* {2})[a-zA-Z0-9_-][ a-zA-Z0-9_-]*[a-zA-Z0-9_-]$` and its
/// `@length(2..50)` (source: `smithy_query(query="shape", shape="Widget")` on that
/// service's own model) — so the refusal is the owning service's whatever the fault is.
fn refused_titles() -> Vec<Refused> {
    vec![
        Refused {
            case: "two consecutive spaces, which the title's negative lookahead forbids",
            stack_name: "bcm-dashboard-double-space",
            title: "Monthly  Spend".to_string(),
        },
        Refused {
            case: "a leading space, which the title's first character class forbids",
            stack_name: "bcm-dashboard-leading-space",
            title: " Monthly Spend".to_string(),
        },
        Refused {
            case: "a punctuation character the title's character class forbids",
            stack_name: "bcm-dashboard-punctuation",
            title: "Spend/Usage".to_string(),
        },
        Refused {
            case: "one character, below the title's minimum length",
            stack_name: "bcm-dashboard-too-short",
            title: "S".to_string(),
        },
        Refused {
            case: "one character past the title's maximum length",
            stack_name: "bcm-dashboard-too-long",
            title: "S".repeat(51),
        },
    ]
}

/// THE RULE, the refusal direction: a declaration the owning service refuses fails the
/// RESOURCE and rolls the stack back with that service's own words — it is never a
/// template-format error, and nothing is left behind in that service.
#[tokio::test]
async fn a_widget_title_the_owning_service_refuses_rolls_the_stack_back() {
    for row in refused_titles() {
        let router = router();
        let case = row.case;
        let template = json!({
            "Resources": {
                "Board": {
                    "Type": BCM_DASHBOARD,
                    "Properties": {
                        "Name": "Refused Board",
                        "Widgets": [{
                            "Title": row.title,
                            "Configs": [graph_config("UnblendedCost", "LINE", "MONTHLY")]
                        }]
                    }
                }
            }
        })
        .to_string();

        // The body is ADMITTED: the type is one a template may declare, so the fault is the
        // declared VALUE and not the resource type.
        submitted(&router, case, row.stack_name, &template).await;
        let reached = settled_stack_status(case, &router, REGION, row.stack_name).await;
        assert_eq!(
            reached, "ROLLBACK_COMPLETE",
            "case {case:?}: expected the refused create to roll back, got {reached} with \
             reasons {:?}",
            stack_event_reasons(&router, REGION, row.stack_name).await
        );

        let reason = failed_resource_reason(
            case,
            &router,
            REGION,
            row.stack_name,
            "Board",
            "CREATE_FAILED",
        )
        .await;
        let (quoted, _) = quoted_and_class(case, &reason);
        assert!(
            !quoted.contains(UNRECOGNIZED_TYPES),
            "case {case:?}: expected the owning service's refusal, got {reason:?}"
        );
        assert!(
            quoted.contains("title"),
            "case {case:?}: expected the refusal to name the member that broke the rule, got \
             {reason:?}"
        );

        // Nothing was created: the account holds no dashboard the stack could have made.
        let listed = listed_custom_dashboards(&router).await;
        assert!(
            listed.is_empty(),
            "case {case:?}: expected the refused create to leave no dashboard, got {listed:?}"
        );
    }
}

/// The catalogue entries the rule rests on: both types are published and recognised, a
/// near-miss spelling of either still is not, each is identified by the ARN its owning
/// service mints, and each reports the return values its own section publishes.
#[test]
fn the_catalogue_publishes_both_bcm_types_and_what_they_return() {
    for resource_type in [BCM_DASHBOARD, BCM_SCHEDULED_REPORT] {
        assert!(
            AWS_RESOURCE_TYPES.contains(&resource_type),
            "expected {resource_type:?} in the published catalogue, it is absent"
        );
        assert!(
            is_published(resource_type) && is_recognized(resource_type),
            "expected {resource_type:?} to be a type a template may declare, it is not"
        );
        // Each create takes no identifier and answers the ARN that service minted, so the
        // identity in force is the one it answered.
        assert!(
            identity_is_service_assigned(resource_type),
            "expected {resource_type:?} to take the identity its owning service assigns"
        );
        // Every property either type publishes is a member of its own update, so neither has
        // a create-only property.
        assert_eq!(
            replacement_properties(resource_type),
            &[] as &[&str],
            "expected {resource_type:?} to have no create-only property, got {:?}",
            replacement_properties(resource_type)
        );
    }
    // The catalogue is a list of names, not a shape test: a name it does not carry stays
    // unrecognised however close it is to one it does.
    for misspelled in [
        "AWS::BCM::Dashboards",
        "AWS::BCM::ScheduledReports",
        "AWS::Bcm::Dashboard",
        "AWS::BCMDashboards::Dashboard",
        "AWS::BCM::Widget",
    ] {
        assert!(
            !is_recognized(misspelled),
            "expected {misspelled:?} to stay unrecognised, it is recognised"
        );
    }
    assert_eq!(
        published_attribute_names(BCM_DASHBOARD),
        &["Arn", "CreatedAt", "Type", "UpdatedAt"],
        "expected the dashboard's published return values, got {:?}",
        published_attribute_names(BCM_DASHBOARD)
    );
    assert_eq!(
        published_attribute_names(BCM_SCHEDULED_REPORT),
        &["Arn", "CreatedAt", "UpdatedAt"],
        "expected the report's published return values, got {:?}",
        published_attribute_names(BCM_SCHEDULED_REPORT)
    );
}
