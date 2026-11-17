// SPDX-License-Identifier: CC0-1.0

//! What a resource of a type AMAZON CLOUDWATCH LOGS owns reports on its `CREATE_FAILED` event,
//! end to end over the awsQuery wire with that service mounted beside CloudFormation on one
//! engine.
//!
//! RULE under test: **the provider of a type whose owning service refused the call re-raises a
//! refusal of the REQUEST as the resource-provider plugin's invalid-request exception — its
//! wording names the TYPE the handler manages and carries the SDK exception it wrapped, which
//! is that service's own sentence followed by its SDK's diagnostic of the call (the service as
//! its SDK names it, the HTTP status it answered and the id of the call) — while a refusal of
//! any OTHER class reaches the event as that service's own sentence alone.**
//!
//! Why it matters: a client reading a rolled-back deployment diagnoses it from the failed
//! resource's `ResourceStatusReason`. Deploy tooling, CI log scrapers and support escalations
//! parse that field to attribute the failure to a downstream API call — which resource class
//! failed, which service refused it and with what status — so a reason that carries only the
//! bare sentence matches nothing and attributes the failure to nothing.
//!
//! The rows differ along what the rule generalises over: WHICH type of the owning service
//! failed (a metric filter, a subscription filter and a log stream), which call of it was
//! refused, and the CLASS the refusal falls in — a refused request, and a resource the
//! declaration names that service does not hold — which is what decides whether the plugin's
//! own wording is part of the message at all. The recorded instance that led here is one row
//! of the refused-request class; every other row is one it did not cite.
//!
//! The POSITIVE CONTROL deploys the same two resources with nothing wrong with them and reads
//! the filter back through CloudWatch Logs's own wire: the wording above is what a REFUSAL
//! reports, and a declaration that service accepts still reaches it and still completes.

use serde_json::{json, Value};

use crate::harness::{assert_sdk_diagnostic, failed_resource_reason, quoted_and_class};
use crate::logs_group_children_resources::logs;
use crate::stack_lifecycle::{create_stack, router, settled_stack, xml_text, ACCOUNT_ID, REGION};

/// The name Amazon CloudWatch Logs's SDK states for the service in the diagnostic it appends
/// to the exception it raises for a refused call.
const LOGS_SERVICE: &str = "CloudWatchLogs";

/// The HTTP status that service answers a refused request with, from the `httpError` its model
/// declares for `InvalidParameterException`
/// (`smithy_query(query="operation",operation="PutMetricFilter,PutSubscriptionFilter")`).
const REFUSED_REQUEST_STATUS: u16 = 400;

/// The `HandlerErrorCode` classes these rows are reported under — all values of the model's own
/// `HandlerErrorCode` enum.
const INVALID_REQUEST: &str = "InvalidRequest";
const NOT_FOUND: &str = "NotFound";

/// The status a rolled-back create settles on, and the status a resource its provider could
/// not create records.
const ROLLBACK_COMPLETE: &str = "ROLLBACK_COMPLETE";
const CREATE_FAILED: &str = "CREATE_FAILED";

/// The transformation a declared metric filter states, which is beside the point of every row:
/// what the rows are about is the refusal, not the metric.
fn transformations() -> Value {
    json!([{
        "MetricName": "Latency",
        "MetricNamespace": "Platform/Logs",
        "MetricValue": "1"
    }])
}

/// One row: the stack to deploy, the resource the owning service refuses, and what its reason
/// must report.
struct Row {
    case: &'static str,
    stack: &'static str,
    template: String,
    logical_id: &'static str,
    /// A fragment of the owning service's own sentence the reported message must carry.
    sentence: &'static str,
    /// The type the plugin's invalid-request exception must name, for a row whose refusal falls
    /// in that class — `None` for a row of any other class, whose message must then be the
    /// service's sentence alone, with neither that wording nor a diagnostic.
    wrapped_type: Option<&'static str>,
    /// The `HandlerErrorCode` class the refusal falls in.
    class: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a metric filter whose declared pattern that service cannot read",
            stack: "logs-reason-bad-pattern",
            template: json!({"Resources": {
                "Group": {
                    "Type": "AWS::Logs::LogGroup",
                    "Properties": {"LogGroupName": "logs-reason-bad-pattern-group"}
                },
                "Filter": {
                    "Type": "AWS::Logs::MetricFilter",
                    "Properties": {
                        "LogGroupName": {"Ref": "Group"},
                        "FilterPattern": "{ $.latency = }",
                        "MetricTransformations": transformations()
                    }
                }
            }})
            .to_string(),
            logical_id: "Filter",
            sentence: "Invalid metric filter pattern",
            wrapped_type: Some("AWS::Logs::MetricFilter"),
            class: INVALID_REQUEST,
        },
        Row {
            case: "a subscription filter whose destination is no destination of that service",
            stack: "logs-reason-bad-destination",
            template: json!({"Resources": {
                "Group": {
                    "Type": "AWS::Logs::LogGroup",
                    "Properties": {"LogGroupName": "logs-reason-bad-destination-group"}
                },
                "Filter": {
                    "Type": "AWS::Logs::SubscriptionFilter",
                    "Properties": {
                        "LogGroupName": {"Ref": "Group"},
                        "FilterPattern": "MARKER",
                        "DestinationArn":
                            format!("arn:aws:sns:{REGION}:{ACCOUNT_ID}:logs-reason-topic")
                    }
                }
            }})
            .to_string(),
            logical_id: "Filter",
            sentence: "destinationArn must be the ARN of",
            wrapped_type: Some("AWS::Logs::SubscriptionFilter"),
            class: INVALID_REQUEST,
        },
        Row {
            case: "a metric filter on a log group that service does not hold",
            stack: "logs-reason-absent-group",
            template: json!({"Resources": {
                "Filter": {
                    "Type": "AWS::Logs::MetricFilter",
                    "Properties": {
                        "LogGroupName": "logs-reason-no-such-group",
                        "FilterPattern": "ERROR",
                        "MetricTransformations": transformations()
                    }
                }
            }})
            .to_string(),
            logical_id: "Filter",
            sentence: "The specified log group does not exist.",
            wrapped_type: None,
            class: NOT_FOUND,
        },
        Row {
            case: "a log stream inside a log group that service does not hold",
            stack: "logs-reason-absent-stream-group",
            template: json!({"Resources": {
                "Stream": {
                    "Type": "AWS::Logs::LogStream",
                    "Properties": {
                        "LogGroupName": "logs-reason-no-such-stream-group",
                        "LogStreamName": "logs-reason-stream"
                    }
                }
            }})
            .to_string(),
            logical_id: "Stream",
            sentence: "The specified log group does not exist.",
            wrapped_type: None,
            class: NOT_FOUND,
        },
    ]
}

/// Hold the quoted message of a refused REQUEST to the exception the type's provider raises:
/// the plugin's own wording naming `resource_type`, then the owning service's sentence, then
/// that service's SDK diagnostic of the call — the last of which is the shape every suite about
/// a refused resource reads, so it is held to the harness's own reader of it.
fn assert_wrapped(case: &str, quoted: &str, resource_type: &str, sentence: &str) {
    let opener = format!("Invalid request provided: {resource_type}. ");
    let exception = quoted.strip_prefix(&opener).unwrap_or_else(|| {
        panic!("case {case:?}: expected the message to open with {opener:?}, got {quoted:?}")
    });
    assert!(
        exception.contains(sentence),
        "case {case:?}: expected the service's own sentence carrying {sentence:?}, got \
         {exception:?}"
    );
    assert_sdk_diagnostic(case, exception, LOGS_SERVICE, REFUSED_REQUEST_STATUS);
}

#[tokio::test]
async fn a_refused_logs_resource_reports_the_exception_its_provider_raised() {
    for row in rows() {
        let router = router();
        let (status, created) = create_stack(&router, row.stack, &row.template, &[]).await;
        assert_eq!(
            status, 200,
            "case {:?}: CreateStack must answer 200, got {status} {created}",
            row.case
        );
        let body = settled_stack(&router, row.case, row.stack).await;
        assert_eq!(
            xml_text(&body, "StackStatus").as_deref(),
            Some(ROLLBACK_COMPLETE),
            "case {:?}: expected the refused create to be rolled back, got {body}",
            row.case
        );

        let reason = failed_resource_reason(
            row.case,
            &router,
            REGION,
            row.stack,
            row.logical_id,
            CREATE_FAILED,
        )
        .await;
        let (quoted, class) = quoted_and_class(row.case, &reason);
        assert_eq!(
            class, row.class,
            "case {:?}: expected the {} class, got {class:?} in {reason:?}",
            row.case, row.class
        );
        match row.wrapped_type {
            Some(resource_type) => {
                assert_wrapped(row.case, &quoted, resource_type, row.sentence);
            }
            None => {
                assert_eq!(
                    quoted, row.sentence,
                    "case {:?}: expected the owning service's own sentence alone, got {quoted:?}",
                    row.case
                );
                assert!(
                    !quoted.contains("Invalid request provided"),
                    "case {:?}: expected no invalid-request wording on a refusal of another \
                     class, got {quoted:?}",
                    row.case
                );
            }
        }
    }
}

/// The control: the same two types with nothing wrong with them are created in CloudWatch Logs
/// itself, so the wording above is what a refusal reports and not something every deployment
/// of these types now carries.
#[tokio::test]
async fn a_declaration_that_service_accepts_still_reaches_it() {
    let case = "a group and a metric filter that service accepts";
    let router = router();
    let group = "logs-reason-accepted-group";
    let template = json!({"Resources": {
        "Group": {
            "Type": "AWS::Logs::LogGroup",
            "Properties": {"LogGroupName": group}
        },
        "Filter": {
            "Type": "AWS::Logs::MetricFilter",
            "Properties": {
                "FilterName": "logs-reason-accepted-filter",
                "LogGroupName": {"Ref": "Group"},
                "FilterPattern": "{ $.latency = 1 }",
                "MetricTransformations": transformations()
            }
        }
    }})
    .to_string();

    let (status, created) = create_stack(&router, "logs-reason-accepted", &template, &[]).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must answer 200, got {status} {created}"
    );
    let body = settled_stack(&router, case, "logs-reason-accepted").await;
    assert_eq!(
        xml_text(&body, "StackStatus").as_deref(),
        Some("CREATE_COMPLETE"),
        "case {case:?}: expected the accepted declaration to complete, got {body}"
    );

    let (status, answered) = logs(
        &router,
        "DescribeMetricFilters",
        &json!({"logGroupName": group}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DescribeMetricFilters must answer 200, got {status} {answered}"
    );
    let reported: Vec<&str> = answered
        .get("metricFilters")
        .and_then(Value::as_array)
        .map(|filters| {
            filters
                .iter()
                .filter_map(|filter| filter.get("filterName").and_then(Value::as_str))
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        reported,
        vec!["logs-reason-accepted-filter"],
        "case {case:?}: expected the declared filter to be on the group, got {answered}"
    );
}
