// SPDX-License-Identifier: CC0-1.0

//! The WHOLE LIFECYCLE of the alarm an `AWS::CloudWatch::Alarm` declares, read through Amazon
//! CloudWatch's own wire at every step of it.
//!
//! RULE under test: **a resource a stack reports created exists, for as long as the stack
//! manages it, in the service that OWNS it, under the identifier the stack publishes for it —
//! so the alarm an `AWS::CloudWatch::Alarm` declares is the ONE entry
//! `DescribeAlarms(AlarmNames=[<the published name>])` reports, carrying the declaration's own
//! configuration; a deployment that changes a property the owning call can take re-puts THAT
//! alarm, leaving one entry of the same name and ARN with the new configuration; a deployment
//! that changes the name the alarm takes its identity from creates the new alarm and leaves
//! the old one exactly where its `UpdateReplacePolicy` says (`Retain` keeps it, any other
//! value removes it); and the stack's own deletion removes the alarm it still manages, after
//! which that name resolves to nothing.**
//!
//! The reported case deployed one such alarm, polled the stack to `CREATE_COMPLETE` and then
//! read CloudWatch for the name the stack published: `MetricAlarms` came back EMPTY. An alarm
//! a client cannot read back is one it cannot evaluate, `SetAlarmState` on, or attach actions
//! to — monitoring a template deploys declaratively that does not exist — and nothing further
//! in a deploy-then-observe flow is reachable, neither the update half nor the teardown half.
//!
//! The rows differ along what the rule generalises over rather than restating that one
//! template:
//!
//! * the `UpdateReplacePolicy` the declaration carries, over every value the attribute takes
//!   (`state::deletion_policy` carries them) plus a declaration stating none, which is what
//!   decides the fate of the alarm a replacement leaves behind;
//! * WHICH properties the in-place deployment changes — the threshold and the comparison, the
//!   metric and the statistic and the dimension set, and the description and the
//!   missing-data treatment on their own;
//! * the configuration itself: three namespaces and metrics, four statistics, three
//!   comparison operators, a period of a minute and of five, one and several evaluation
//!   periods with and without an "M out of N" count, a threshold spelled as an integer and as
//!   a fraction, no dimension, one and two, and a property the second revision DROPS.
//!
//! Nothing is waited out: the stack is polled to the status it settles on, and every read of
//! the alarm goes over CloudWatch's own `awsJson1_0` wire, paged to the end, so "no alarm"
//! means no entry on any page.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::{deletion_policy, status};

use crate::declared_alarm_is_readable_in_cloudwatch::alarms_named;
use crate::dynamodb_table_resources::failure_reasons;
use crate::harness::{call, create_stack, member, router, settled_stack, xml_text, REGION};
use crate::stack_lifecycle::{output_value, ACCOUNT_ID};

/// The type under test, and the logical id it is declared under in every revision below.
const ALARM_TYPE: &str = "AWS::CloudWatch::Alarm";
const ALARM_ID: &str = "Watch";

/// The output keys each revision publishes the alarm's identity under: `Ref` of the resource
/// ("Ref returns the alarm name") and `Fn::GetAtt … Arn`.
const NAME_OUTPUT: &str = "AlarmName";
const ARN_OUTPUT: &str = "AlarmArn";

/// The configuration one revision declares: properties of `AWS::CloudWatch::Alarm` that are
/// members of `PutMetricAlarm` under the same name
/// (`smithy_query(query="operation",operation="PutMetricAlarm")`), each of which `MetricAlarm`
/// reports back under that name too.
#[derive(Debug, Clone, Copy)]
struct Config {
    namespace: &'static str,
    metric: &'static str,
    /// One of `Statistic`'s values (`smithy_query(query="shape",shape="Statistic")`).
    statistic: &'static str,
    period: i64,
    evaluation_periods: i64,
    /// "The number of data points that must be breaching to trigger the alarm."
    /// (`MetricAlarm$DatapointsToAlarm`) — `None` is a declaration that states none.
    datapoints_to_alarm: Option<i64>,
    threshold: f64,
    /// One of `ComparisonOperator`'s values
    /// (`smithy_query(query="shape",shape="ComparisonOperator")`).
    comparison: &'static str,
    /// One of `StandardUnit`'s values
    /// (`smithy_query(query="shape",shape="StandardUnit")`), or none.
    unit: Option<&'static str>,
    /// The metric's dimensions, as `(Name, Value)` pairs; the empty slice is a metric with
    /// none.
    dimensions: &'static [(&'static str, &'static str)],
    /// "The valid values are breaching, notBreaching, ignore, and missing."
    /// (`MetricAlarm$TreatMissingData`).
    treat_missing_data: &'static str,
    description: Option<&'static str>,
}

/// One revision's template: the alarm under the physical name `name`, carrying `config` as its
/// declared configuration and `policy` as its `UpdateReplacePolicy` when the row declares one,
/// publishing the name and the ARN the stack holds for it.
fn template(name: &str, config: &Config, policy: Option<&str>) -> String {
    let mut properties = json!({
        "AlarmName": name,
        "Namespace": config.namespace,
        "MetricName": config.metric,
        "Statistic": config.statistic,
        "Period": config.period,
        "EvaluationPeriods": config.evaluation_periods,
        "Threshold": config.threshold,
        "ComparisonOperator": config.comparison,
        "TreatMissingData": config.treat_missing_data,
        "Dimensions": config
            .dimensions
            .iter()
            .map(|(key, value)| json!({"Name": key, "Value": value}))
            .collect::<Vec<Value>>(),
    });
    if let Some(count) = config.datapoints_to_alarm {
        properties["DatapointsToAlarm"] = json!(count);
    }
    if let Some(unit) = config.unit {
        properties["Unit"] = json!(unit);
    }
    if let Some(description) = config.description {
        properties["AlarmDescription"] = json!(description);
    }
    let mut declaration = json!({"Type": ALARM_TYPE, "Properties": properties});
    if let (Some(policy), Some(members)) = (policy, declaration.as_object_mut()) {
        members.insert(
            "UpdateReplacePolicy".to_string(),
            Value::String(policy.to_string()),
        );
    }
    json!({
        "Resources": {ALARM_ID: declaration},
        "Outputs": {
            NAME_OUTPUT: {"Value": {"Ref": ALARM_ID}},
            ARN_OUTPUT: {"Value": {"Fn::GetAtt": [ALARM_ID, "Arn"]}},
        },
    })
    .to_string()
}

/// The `(Name, Value)` pairs one reported alarm carries, in wire order.
fn reported_dimensions(alarm: &Value) -> Vec<(String, String)> {
    alarm
        .get("Dimensions")
        .and_then(Value::as_array)
        .map(|dimensions| {
            dimensions
                .iter()
                .map(|dimension| {
                    (
                        dimension
                            .get("Name")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        dimension
                            .get("Value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The ONE alarm CloudWatch holds under `name`, asserted to be exactly one — which is what the
/// reported case found none of.
async fn the_one_alarm(
    router: &MultiServiceRouter,
    case: &str,
    phase: &str,
    name: &str,
) -> Value {
    let reported = alarms_named(router, case, name).await;
    assert_eq!(
        reported.len(),
        1,
        "case {case:?} ({phase}): expected DescribeAlarms {name:?} to report exactly one \
         MetricAlarm, got {reported:?}"
    );
    reported.into_iter().next().expect("one entry")
}

/// Every member of `config` as CloudWatch reports it for the alarm held under `name`: the
/// declaration's configuration, and the ARN the alarm's name puts it at — "The ARN format of
/// an alarm is arn:aws:cloudwatch:Region:account-id:alarm:alarm-name".
///
/// A property the declaration does NOT state is reported by none: the deployment configures
/// what the template declares and nothing else.
fn assert_configuration(case: &str, phase: &str, alarm: &Value, name: &str, config: &Config) {
    let expected_arn = format!("arn:aws:cloudwatch:{REGION}:{ACCOUNT_ID}:alarm:{name}");
    for (member, expected, actual) in [
        ("AlarmName", Some(name), alarm.get("AlarmName").and_then(Value::as_str)),
        (
            "AlarmArn",
            Some(expected_arn.as_str()),
            alarm.get("AlarmArn").and_then(Value::as_str),
        ),
        (
            "Namespace",
            Some(config.namespace),
            alarm.get("Namespace").and_then(Value::as_str),
        ),
        (
            "MetricName",
            Some(config.metric),
            alarm.get("MetricName").and_then(Value::as_str),
        ),
        (
            "Statistic",
            Some(config.statistic),
            alarm.get("Statistic").and_then(Value::as_str),
        ),
        (
            "ComparisonOperator",
            Some(config.comparison),
            alarm.get("ComparisonOperator").and_then(Value::as_str),
        ),
        (
            "TreatMissingData",
            Some(config.treat_missing_data),
            alarm.get("TreatMissingData").and_then(Value::as_str),
        ),
        (
            "Unit",
            config.unit,
            alarm.get("Unit").and_then(Value::as_str),
        ),
        (
            "AlarmDescription",
            config.description,
            alarm.get("AlarmDescription").and_then(Value::as_str),
        ),
    ] {
        assert_eq!(
            actual, expected,
            "case {case:?} ({phase}): expected {member} {expected:?} on {name:?}, got \
             {actual:?} in {alarm:?}"
        );
    }
    for (member, expected, actual) in [
        (
            "Period",
            Some(config.period),
            alarm.get("Period").and_then(Value::as_i64),
        ),
        (
            "EvaluationPeriods",
            Some(config.evaluation_periods),
            alarm.get("EvaluationPeriods").and_then(Value::as_i64),
        ),
        (
            "DatapointsToAlarm",
            config.datapoints_to_alarm,
            alarm.get("DatapointsToAlarm").and_then(Value::as_i64),
        ),
    ] {
        assert_eq!(
            actual, expected,
            "case {case:?} ({phase}): expected {member} {expected:?} on {name:?}, got \
             {actual:?} in {alarm:?}"
        );
    }
    assert_eq!(
        alarm.get("Threshold").and_then(Value::as_f64),
        Some(config.threshold),
        "case {case:?} ({phase}): expected Threshold {} on {name:?}, got {:?} in {alarm:?}",
        config.threshold,
        alarm.get("Threshold")
    );
    let expected_dimensions: Vec<(String, String)> = config
        .dimensions
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect();
    let dimensions = reported_dimensions(alarm);
    assert_eq!(
        dimensions, expected_dimensions,
        "case {case:?} ({phase}): expected the declared dimensions {expected_dimensions:?} on \
         {name:?}, got {dimensions:?}"
    );
}

/// One row: the `UpdateReplacePolicy` the declaration carries, the configuration the create
/// declares, the configuration the IN-PLACE deployment declares for the same name, and the
/// name the REPLACING deployment asks for.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// `None` is a declaration that states no `UpdateReplacePolicy` at all, which gets the
    /// default, `Delete`.
    policy: Option<&'static str>,
    first: &'static str,
    second: &'static str,
    created: Config,
    updated: Config,
    /// Whether the alarm the replacement left behind must survive it — true exactly for
    /// `Retain`.
    retains_the_old: bool,
}

/// The rows: every value `UpdateReplacePolicy` takes and a declaration stating none, each over
/// a different configuration and a different in-place change.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an alarm retained by its UpdateReplacePolicy, re-thresholded in place",
            stack: "alarm-lifecycle-retain",
            policy: Some(deletion_policy::RETAIN),
            first: "alarm-lifecycle-retain-first",
            second: "alarm-lifecycle-retain-second",
            created: Config {
                namespace: "AWS/Lambda",
                metric: "Errors",
                statistic: "Sum",
                period: 60,
                evaluation_periods: 1,
                datapoints_to_alarm: None,
                threshold: 1.0,
                comparison: "GreaterThanThreshold",
                unit: Some("Count"),
                dimensions: &[("FunctionName", "sftp-ingest")],
                treat_missing_data: "notBreaching",
                description: Some("the errors of the ingest function"),
            },
            updated: Config {
                namespace: "AWS/Lambda",
                metric: "Errors",
                statistic: "Sum",
                period: 300,
                evaluation_periods: 3,
                datapoints_to_alarm: Some(2),
                threshold: 5.0,
                comparison: "GreaterThanOrEqualToThreshold",
                unit: Some("Count"),
                dimensions: &[("FunctionName", "sftp-ingest")],
                treat_missing_data: "breaching",
                description: Some("the errors of the ingest function, two of three periods"),
            },
            retains_the_old: true,
        },
        Row {
            case: "an alarm deleted by its UpdateReplacePolicy, re-pointed at another metric \
                   in place",
            stack: "alarm-lifecycle-delete",
            policy: Some(deletion_policy::DELETE),
            first: "alarm-lifecycle-delete-first",
            second: "alarm-lifecycle-delete-second",
            created: Config {
                namespace: "AWS/SQS",
                metric: "ApproximateAgeOfOldestMessage",
                statistic: "Maximum",
                period: 300,
                evaluation_periods: 2,
                datapoints_to_alarm: Some(2),
                threshold: 900.0,
                comparison: "GreaterThanThreshold",
                unit: Some("Seconds"),
                dimensions: &[("QueueName", "orders-intake"), ("Tier", "gold")],
                treat_missing_data: "missing",
                description: Some("the backlog age of the intake queue"),
            },
            updated: Config {
                namespace: "AWS/SQS",
                metric: "ApproximateNumberOfMessagesVisible",
                statistic: "Average",
                period: 60,
                evaluation_periods: 2,
                datapoints_to_alarm: None,
                threshold: 12.5,
                comparison: "LessThanThreshold",
                unit: None,
                dimensions: &[("QueueName", "orders-intake")],
                treat_missing_data: "ignore",
                description: None,
            },
            retains_the_old: false,
        },
        Row {
            case: "an alarm with no UpdateReplacePolicy declared and no dimension, \
                   re-described in place",
            stack: "alarm-lifecycle-default",
            policy: None,
            first: "alarm-lifecycle-default-first",
            second: "alarm-lifecycle-default-second",
            created: Config {
                namespace: "Sftp/Diagnostics",
                metric: "SftpErrors",
                statistic: "SampleCount",
                period: 60,
                evaluation_periods: 5,
                datapoints_to_alarm: Some(3),
                threshold: 0.5,
                comparison: "GreaterThanOrEqualToThreshold",
                unit: None,
                dimensions: &[],
                treat_missing_data: "notBreaching",
                description: None,
            },
            updated: Config {
                namespace: "Sftp/Diagnostics",
                metric: "SftpErrors",
                statistic: "SampleCount",
                period: 60,
                evaluation_periods: 5,
                datapoints_to_alarm: Some(3),
                threshold: 0.5,
                comparison: "GreaterThanOrEqualToThreshold",
                unit: None,
                dimensions: &[],
                treat_missing_data: "ignore",
                description: Some("the error lines the filter publishes"),
            },
            retains_the_old: false,
        },
    ]
}

/// RULE: see the module header — the declared alarm is the one alarm CloudWatch holds under
/// the published name, through the create, the in-place update, the replacement and the
/// stack's own deletion.
#[tokio::test]
async fn a_declared_alarm_is_the_one_cloudwatch_holds_through_the_stacks_whole_lifecycle() {
    for row in rows() {
        let Row {
            case,
            stack,
            policy,
            first,
            second,
            created,
            updated,
            retains_the_old,
        } = row;
        let router = router();

        // The revision that CREATES the alarm.
        let (code, body) = create_stack(&router, stack, &template(first, &created, policy), &[]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the create must be accepted, got {code} {body}"
        );
        // A DELETED stack is no longer reported under its NAME, so the read that waits out the
        // deletion below addresses it by the id the create minted.
        let stack_id = xml_text(&body, "StackId").unwrap_or_default();
        assert!(
            !stack_id.is_empty(),
            "case {case:?}: expected CreateStack to report a StackId, got {body}"
        );
        let described = settled_stack(case, &router, REGION, stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {case:?}: expected the create to complete, got {described} because {:?}",
            failure_reasons(&router, stack).await
        );

        // The identity the stack PUBLISHES is the handle every CloudWatch call about the alarm
        // takes, so it is what the reads below address...
        let published = output_value(&described, NAME_OUTPUT).unwrap_or_default();
        assert_eq!(
            published, first,
            "case {case:?}: expected the stack to publish the declared name {first:?}, got \
             {published:?}"
        );
        let published_arn = output_value(&described, ARN_OUTPUT).unwrap_or_default();

        // ...and CloudWatch holds exactly one alarm of that name, carrying the declaration.
        let alarm = the_one_alarm(&router, case, "after the create", first).await;
        assert_configuration(case, "after the create", &alarm, first, &created);
        assert_eq!(
            alarm.get("AlarmArn").and_then(Value::as_str),
            Some(published_arn.as_str()),
            "case {case:?}: expected the alarm CloudWatch holds to carry the ARN the stack \
             published ({published_arn:?}), got {alarm:?}"
        );

        // A deployment that changes properties the owning call takes updates THAT alarm: one
        // entry of the same name and ARN, carrying the configuration now declared.
        let (code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", stack),
                member("TemplateBody", &template(first, &updated, policy)),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the in-place update must be accepted, got {code} {body}"
        );
        let redescribed = settled_stack(case, &router, REGION, stack).await;
        assert_eq!(
            xml_text(&redescribed, "StackStatus").as_deref(),
            Some(status::UPDATE_COMPLETE),
            "case {case:?}: expected the in-place update to complete, got {redescribed} \
             because {:?}",
            failure_reasons(&router, stack).await
        );
        let in_place = the_one_alarm(&router, case, "after the in-place update", first).await;
        assert_configuration(case, "after the in-place update", &in_place, first, &updated);

        // A deployment that asks for another NAME — the value the alarm takes its identity
        // from — creates the alarm it now declares...
        let (code, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", stack),
                member("TemplateBody", &template(second, &updated, policy)),
            ],
        )
        .await;
        assert_eq!(
            code, 200,
            "case {case:?}: the replacing update must be accepted, got {code} {body}"
        );
        let replaced = settled_stack(case, &router, REGION, stack).await;
        assert_eq!(
            xml_text(&replaced, "StackStatus").as_deref(),
            Some(status::UPDATE_COMPLETE),
            "case {case:?}: expected the replacing update to complete, got {replaced} because \
             {:?}",
            failure_reasons(&router, stack).await
        );
        let replacement = the_one_alarm(&router, case, "after the replacement", second).await;
        assert_configuration(case, "after the replacement", &replacement, second, &updated);
        let republished = output_value(&replaced, NAME_OUTPUT).unwrap_or_default();
        assert_eq!(
            republished, second,
            "case {case:?}: expected the stack to publish the replacement's name {second:?}, \
             got {republished:?}"
        );

        // ...and leaves the one it replaced exactly where its own policy says.
        let left_behind = alarms_named(&router, case, first).await;
        if retains_the_old {
            assert_eq!(
                left_behind.len(),
                1,
                "case {case:?}: expected the replaced alarm {first:?} to be left exactly as the \
                 in-place update left it, got {left_behind:?}"
            );
            assert_configuration(
                case,
                "the replaced alarm, retained",
                &left_behind[0],
                first,
                &updated,
            );
        } else {
            assert!(
                left_behind.is_empty(),
                "case {case:?}: expected the replaced alarm {first:?} to be deleted by the \
                 cleanup, got {left_behind:?}"
            );
        }

        // The stack's own deletion removes the alarm it still manages, and that name then
        // resolves to nothing — while an alarm the stack released outlives it.
        let (code, body) = call(&router, "DeleteStack", &[member("StackName", stack)]).await;
        assert_eq!(
            code, 200,
            "case {case:?}: the deletion must be accepted, got {code} {body}"
        );
        let deleted = settled_stack(case, &router, REGION, &stack_id).await;
        assert_eq!(
            xml_text(&deleted, "StackStatus").as_deref(),
            Some(status::DELETE_COMPLETE),
            "case {case:?}: expected the deletion to complete, got {deleted} because {:?}",
            failure_reasons(&router, &stack_id).await
        );
        let after_the_deletion = alarms_named(&router, case, second).await;
        assert!(
            after_the_deletion.is_empty(),
            "case {case:?}: expected DescribeAlarms {second:?} to report no alarm once the \
             stack that managed it is gone, got {after_the_deletion:?}"
        );
        let outliving = alarms_named(&router, case, first).await;
        assert_eq!(
            outliving.len(),
            usize::from(retains_the_old),
            "case {case:?}: expected {first:?} to {} the stack's deletion, got {outliving:?}",
            if retains_the_old {
                "outlive"
            } else {
                "have been gone before"
            }
        );
    }
}
