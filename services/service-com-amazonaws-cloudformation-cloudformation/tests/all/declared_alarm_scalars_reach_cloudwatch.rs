// SPDX-License-Identifier: CC0-1.0

//! The scalars an `AWS::CloudWatch::Alarm` and an `AWS::CloudWatch::CompositeAlarm` declare, as
//! they reach Amazon CloudWatch.
//!
//! RULE: a template's scalar is TEXT wherever its author quoted it and wherever it arrived
//! through a `Ref` to a parameter — of either declared type — while the member of the owning
//! service's call it reaches is modelled as an integer, a double or a boolean, so a declaration
//! is read as the TYPES that service's own model states for its members before the put is made.
//! A member whose keys are the REQUESTER's (a tag set) is left exactly as the template wrote
//! it, so a tag value that merely reads as a number is still the text it was declared as.
//!
//! What that buys: the alarm exists at all. An unread scalar is refused by the owning service's
//! own deserializer, which fails the resource and rolls the whole stack back — so a stack that
//! deploys monitoring whose cadence or threshold came from a parameter deploys nothing, and the
//! alarm a client then reads by the name the stack published is not there.
//!
//! The rows differ along what the rule generalises over: which call the member belongs to
//! (`PutMetricAlarm`, `PutCompositeAlarm`), the modelled type (integer, double, boolean), the
//! FORM the template states it in (a JSON number, quoted text, a `Ref` to a `Number` parameter,
//! a `Ref` to a `String` parameter), the depth the member sits at (a property of its own, a
//! member inside a `Metrics` query), and whether the alarm watches one metric or a metric math
//! expression.
//!
//! Every value is read back through Amazon CLOUDWATCH's own wire, not out of CloudFormation's
//! record of the declaration.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_and_ecs_service_resources::cloudwatch;
use crate::harness::{
    call, create_stack, member, router, settled_stack_status, stack_event_reasons, xml_blocks,
    xml_text, REGION,
};

/// The sentence a JSON deserializer answers for a member it was handed text for where it wanted
/// a number — the report that says the declaration never reached CloudWatch's own validation.
/// No case here may ever see it.
const TYPE_ERROR: &str = "expected number, got string";

/// The `OutputValue` the stack reports for `key`, read INSIDE the `Outputs` block so a member of
/// another list in the envelope cannot be swept up with it.
fn output(xml: &str, key: &str) -> Option<String> {
    let block = xml_blocks(xml, "Outputs").into_iter().next()?;
    xml_blocks(&block, "member")
        .into_iter()
        .find(|entry| xml_text(entry, "OutputKey").as_deref() == Some(key))
        .and_then(|entry| xml_text(&entry, "OutputValue"))
}

/// `DescribeStacks` for one stack.
async fn describe(router: &MultiServiceRouter, stack: &str) -> String {
    let (status, body) = call(router, "DescribeStacks", &[member("StackName", stack)]).await;
    assert_eq!(
        status, 200,
        "expected HTTP 200 from DescribeStacks for {stack}, got {status} with {body}"
    );
    body
}

/// The alarm Amazon CloudWatch holds under `name`, asked for exactly as a client reading a
/// stack's alarm output asks: by name, for the alarm type the row declared.
async fn held_alarm(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    alarm_type: &str,
    reported_in: &str,
) -> Value {
    let (status, document) = cloudwatch(
        router,
        "DescribeAlarms",
        &json!({ "AlarmNames": [name], "AlarmTypes": [alarm_type] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DescribeAlarms, got {status} with {document}"
    );
    let held: Vec<Value> = document
        .get(reported_in)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected CloudWatch to hold exactly the alarm {name:?} the stack \
         reports created, got {held:?}"
    );
    held.into_iter().next().unwrap_or(Value::Null)
}

/// One row: the template's alarm declaration, the parameters the stack is created with, and
/// what CloudWatch must then report for the members under test.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The resource type the row declares.
    resource_type: &'static str,
    /// The alarm type `DescribeAlarms` is asked for, and the output list it answers in.
    alarm_type: &'static str,
    reported_in: &'static str,
    /// The declared properties, as the template states them.
    properties: Value,
    /// The template's `Parameters` block, and the values the create supplies for it.
    parameters: &'static [(&'static str, &'static str, &'static str)],
    /// What CloudWatch must report, as `(member, value)` pairs — every expected value of the
    /// modelled TYPE rather than of the spelling the template used.
    reported: Vec<(&'static str, Value)>,
}

/// The rows of the table. The declared values are the types each member's own model states
/// (`smithy_query(query="closure",operation="PutMetricAlarm")`:
/// `Period`/`EvaluationPeriods`/`DatapointsToAlarm` integers, `Threshold` a double,
/// `ActionsEnabled` and a query's `ReturnData` booleans;
/// `smithy_query(query="operation",operation="PutCompositeAlarm")`: `ActionsEnabled` a boolean
/// and the two suppression periods integers).
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a percentile alarm whose cadence arrives through a Ref to a Number parameter",
            stack: "alarm-scalars-number-parameter",
            resource_type: "AWS::CloudWatch::Alarm",
            alarm_type: "MetricAlarm",
            reported_in: "MetricAlarms",
            properties: json!({
                "AlarmName": {"Fn::Sub": "${AWS::StackName}-service-latency"},
                "Namespace": {"Ref": "MetricNamespace"},
                "MetricName": "Time",
                "Dimensions": [
                    {"Name": "Camera_name", "Value": "ALL"},
                    {"Name": "Operation", "Value": "FrameQuartzLag"},
                ],
                "ExtendedStatistic": "p90",
                "Period": {"Ref": "AlarmPeriod"},
                "EvaluationPeriods": 3,
                "Threshold": 15000,
                "ComparisonOperator": "GreaterThanThreshold",
                "TreatMissingData": "notBreaching",
            }),
            parameters: &[
                ("MetricNamespace", "String", "Flint/Frames"),
                ("AlarmPeriod", "Number", "30"),
            ],
            reported: vec![
                ("Namespace", json!("Flint/Frames")),
                ("MetricName", json!("Time")),
                ("ExtendedStatistic", json!("p90")),
                ("Period", json!(30)),
                ("EvaluationPeriods", json!(3)),
                ("Threshold", json!(15000.0)),
                ("ComparisonOperator", json!("GreaterThanThreshold")),
                ("TreatMissingData", json!("notBreaching")),
                (
                    "Dimensions",
                    json!([
                        {"Name": "Camera_name", "Value": "ALL"},
                        {"Name": "Operation", "Value": "FrameQuartzLag"},
                    ]),
                ),
            ],
        },
        Row {
            case: "every number and the switch quoted, with a threshold through a Ref to a \
                   String parameter",
            stack: "alarm-scalars-quoted",
            resource_type: "AWS::CloudWatch::Alarm",
            alarm_type: "MetricAlarm",
            reported_in: "MetricAlarms",
            properties: json!({
                "AlarmName": "alarm-scalars-quoted-errors",
                "Namespace": "AWS/Lambda",
                "MetricName": "Errors",
                "Statistic": "Sum",
                "Period": "60",
                "EvaluationPeriods": "2",
                "DatapointsToAlarm": "1",
                "Threshold": {"Ref": "ErrorBudget"},
                "ComparisonOperator": "GreaterThanOrEqualToThreshold",
                "ActionsEnabled": "false",
                "Tags": [{"Key": "Period", "Value": "60"}],
            }),
            parameters: &[("ErrorBudget", "String", "0.5")],
            reported: vec![
                ("Period", json!(60)),
                ("EvaluationPeriods", json!(2)),
                ("DatapointsToAlarm", json!(1)),
                ("Threshold", json!(0.5)),
                ("Statistic", json!("Sum")),
                ("ActionsEnabled", json!(false)),
            ],
        },
        Row {
            case: "a metric math alarm whose query states its own cadence and designation as \
                   text",
            stack: "alarm-scalars-metric-math",
            resource_type: "AWS::CloudWatch::Alarm",
            alarm_type: "MetricAlarm",
            reported_in: "MetricAlarms",
            properties: json!({
                "AlarmName": "alarm-scalars-metric-math-backlog",
                "ComparisonOperator": "LessThanThreshold",
                "EvaluationPeriods": "2",
                "Threshold": "1",
                "Metrics": [
                    {
                        "Id": "visible",
                        "MetricStat": {
                            "Metric": {
                                "Namespace": "AWS/SQS",
                                "MetricName": "ApproximateNumberOfMessagesVisible",
                                "Dimensions": [{"Name": "QueueName", "Value": "orders"}],
                            },
                            "Period": {"Ref": "QueryPeriod"},
                            "Stat": "Average",
                        },
                        "ReturnData": "false",
                    },
                    {
                        "Id": "doubled",
                        "Expression": "visible * 2",
                        "ReturnData": "true",
                    },
                ],
            }),
            parameters: &[("QueryPeriod", "Number", "300")],
            reported: vec![
                ("EvaluationPeriods", json!(2)),
                ("Threshold", json!(1.0)),
                ("ComparisonOperator", json!("LessThanThreshold")),
            ],
        },
        Row {
            case: "a composite alarm whose switch and suppression periods are quoted",
            stack: "alarm-scalars-composite",
            resource_type: "AWS::CloudWatch::CompositeAlarm",
            alarm_type: "CompositeAlarm",
            reported_in: "CompositeAlarms",
            properties: json!({
                "AlarmName": "alarm-scalars-composite-rollup",
                "AlarmRule": "ALARM(alarm-scalars-quoted-errors)",
                "AlarmDescription": "the roll-up over the stack's own alarms",
                "ActionsEnabled": "true",
                "ActionsSuppressorWaitPeriod": {"Ref": "WaitPeriod"},
                "ActionsSuppressorExtensionPeriod": "60",
            }),
            parameters: &[("WaitPeriod", "Number", "120")],
            reported: vec![
                ("AlarmRule", json!("ALARM(alarm-scalars-quoted-errors)")),
                ("ActionsEnabled", json!(true)),
                ("ActionsSuppressorWaitPeriod", json!(120)),
                ("ActionsSuppressorExtensionPeriod", json!(60)),
            ],
        },
    ]
}

/// The template one row deploys: its parameter declarations, the alarm, and the output the
/// stack publishes the alarm's name in ("Ref returns the alarm name").
fn template(row: &Row) -> String {
    let mut declared = json!({
        "Resources": {
            "Alarm": {"Type": row.resource_type, "Properties": row.properties},
        },
        "Outputs": {"AlarmName": {"Value": {"Ref": "Alarm"}}},
    });
    if !row.parameters.is_empty() {
        let mut parameters = serde_json::Map::new();
        for (name, kind, _) in row.parameters {
            parameters.insert((*name).to_string(), json!({"Type": kind}));
        }
        declared["Parameters"] = Value::Object(parameters);
    }
    declared.to_string()
}

/// The `Parameters.member.N.*` request members one row's parameter values are sent as.
fn stack_parameters(row: &Row) -> Vec<(String, String)> {
    let mut members = Vec::new();
    for (position, (name, _, value)) in row.parameters.iter().enumerate() {
        let index = position + 1;
        members.push(member(
            &format!("Parameters.member.{index}.ParameterKey"),
            name,
        ));
        members.push(member(
            &format!("Parameters.member.{index}.ParameterValue"),
            value,
        ));
    }
    members
}

#[tokio::test]
async fn a_declared_alarms_scalars_reach_cloudwatch_as_the_types_it_models() {
    for row in rows() {
        let case = row.case;
        let router = router();
        // The composite row's rule names the metric alarm the quoted row declares, so that
        // alarm is deployed first in the same account: a composite alarm's rule is read against
        // the alarms CloudWatch holds.
        if row.resource_type == "AWS::CloudWatch::CompositeAlarm" {
            let child = rows()
                .into_iter()
                .find(|other| other.stack == "alarm-scalars-quoted")
                .expect("the quoted metric alarm is one of the rows");
            let (status, created) = create_stack(
                &router,
                child.stack,
                &template(&child),
                &stack_parameters(&child),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 creating the child alarm's stack, got \
                 {status} with {created}"
            );
            let reached = settled_stack_status(case, &router, REGION, child.stack).await;
            assert_eq!(
                reached, "CREATE_COMPLETE",
                "case {case:?}: expected the child alarm's stack to complete, got {reached} \
                 with {:?}",
                stack_event_reasons(&router, REGION, child.stack).await
            );
        }

        let (status, created) =
            create_stack(&router, row.stack, &template(&row), &stack_parameters(&row)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {created}"
        );
        let reached = settled_stack_status(case, &router, REGION, row.stack).await;
        let reasons = stack_event_reasons(&router, REGION, row.stack).await;
        assert_eq!(
            reached, "CREATE_COMPLETE",
            "case {case:?}: expected the create to complete, got {reached} with {reasons:?}"
        );
        for reason in &reasons {
            assert!(
                !reason.contains(TYPE_ERROR),
                "case {case:?}: expected no deserializer type error, got {reason:?}"
            );
        }

        let described = describe(&router, row.stack).await;
        let name = output(&described, "AlarmName").unwrap_or_default();
        assert!(
            !name.is_empty(),
            "case {case:?}: expected the stack to publish the alarm's name, got {described}"
        );
        let held = held_alarm(&router, case, &name, row.alarm_type, row.reported_in).await;
        for (reported, expected) in &row.reported {
            let value = held.get(*reported).cloned();
            assert_eq!(
                value.as_ref(),
                Some(expected),
                "case {case:?}: expected CloudWatch to report {reported} as {expected}, got \
                 {value:?} in {held}"
            );
        }
        // A tag set's values are the requester's text, whatever they read as, so a tag whose
        // value spells a number is still that text on the alarm the stack created.
        if row.properties.get("Tags").is_some() {
            let (status, tags) = cloudwatch(
                &router,
                "ListTagsForResource",
                &json!({"ResourceARN": held.get("AlarmArn").and_then(Value::as_str)}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from ListTagsForResource, got {status} with \
                 {tags}"
            );
            let held_tags: Vec<Value> = tags
                .get("Tags")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            // Beside the ones CloudFormation propagates, the declared tag is there with its
            // value exactly as the template wrote it.
            assert!(
                held_tags.contains(&json!({"Key": "Period", "Value": "60"})),
                "case {case:?}: expected the declared tag to stay text, got {held_tags:?}"
            );
        }
        // A metric math alarm's query array is what it evaluates, so the cadence and the
        // designation inside each query are the types that call models them as too.
        if row.properties.get("Metrics").is_some() {
            let queries = held
                .get("Metrics")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            let cadence = queries
                .first()
                .and_then(|query| query.pointer("/MetricStat/Period").cloned());
            assert_eq!(
                cadence,
                Some(json!(300)),
                "case {case:?}: expected the query's cadence as the number 300, got \
                 {cadence:?} in {held}"
            );
            let designations: Vec<Option<Value>> = queries
                .iter()
                .map(|query| query.get("ReturnData").cloned())
                .collect();
            assert_eq!(
                designations,
                vec![Some(json!(false)), Some(json!(true))],
                "case {case:?}: expected the two designations as booleans, got \
                 {designations:?} in {held}"
            );
        }
    }
}
