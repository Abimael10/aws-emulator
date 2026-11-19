// SPDX-License-Identifier: CC0-1.0

//! The alarm a stack declares beside the CloudWatch Logs resources whose metric it watches,
//! read back through the two services that own them.
//!
//! RULE under test: **every resource a stack reports created is created in the service that
//! OWNS it before the stack reaches `CREATE_COMPLETE`, under the identifier the stack
//! publishes for it** — so an `AWS::CloudWatch::Alarm` whose name a completed stack publishes
//! with `Ref` is one Amazon CloudWatch holds: `DescribeAlarms(AlarmNames=[<that name>])`,
//! paged to the end, reports EXACTLY ONE `MetricAlarms` entry for it, carrying the
//! declaration's namespace, metric, statistic, period, evaluation periods, threshold,
//! comparison operator and missing-data treatment, and a `StateValue` its own evaluation
//! reached.
//!
//! The reported case deployed a diagnosis estate — a log group, a metric filter over its
//! `ERROR` lines and an alarm over the metric that filter publishes — whose stack reached
//! `CREATE_COMPLETE` and published the alarm's name, while CloudWatch held no alarm of that
//! name at all: the Logs half of the same template was real and the alarm was not, so the
//! first read of the deploy-then-observe flow found nothing and could not tell an alarm that
//! does not exist from one that has no data.
//!
//! The rows differ along what the rule generalises over rather than restating that template:
//!
//! * where the alarm's NAME comes from — a template parameter resolved by `Ref`, a literal in
//!   the declaration, and no `AlarmName` at all, which CloudFormation names itself;
//! * WHICH state the alarm's own evaluation reaches with no datapoint published, over every
//!   value of `StateValue` ([`settled_state`]);
//! * the configuration itself — four of the five modelled statistics, four comparison
//!   operators, one and several evaluation periods with and without an "M out of N" count, a
//!   period of a minute and of five, and a threshold spelled as an integer and as a fraction;
//! * how much else of the SAME template belongs to another service — a log group alone, and a
//!   log group with the metric filter that publishes the metric the alarm watches.
//!
//! Nothing is waited out: the stack is polled to its terminal status and the alarm's state is
//! polled until the evaluation the clock owes has been materialised.
//!
//! The second suite reads the same rule for the OTHER alarm type a template declares: an
//! `AWS::CloudWatch::CompositeAlarm`, whose owning call is `PutCompositeAlarm` ("Creates or
//! updates a composite alarm") and whose state is DERIVED from the alarms its `AlarmRule`
//! names — so it is the one resource that cannot be read back at all unless the children the
//! same stack declared are really there. Its rows differ along the form of the rule, how many
//! children the stack declares, which state each child settles in, where the roll-up's name
//! comes from, and which of its optional members the declaration states; each row then
//! re-deploys the stack with another rule and another child threshold, so the update half of
//! the rule is read for both types.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_and_ecs_service_resources::cloudwatch;
use crate::dynamodb_table_resources::failure_reasons;
use crate::elbv2_resource_provisioning::poll_to_terminal;
use crate::logs_and_iot_resources::described_group;
use crate::stack_lifecycle::{
    acknowledging_iam, call, create_stack, member, output_value, router, xml_text, ACCOUNT_ID,
    REGION,
};

/// The logical id of the template parameter a row may take the alarm's name from, which is
/// also the key `CreateStack` passes its value under.
const ALARM_NAME_PARAMETER: &str = "AlarmName";

/// The metric the filter publishes and the alarm watches, and the namespace it is published
/// in: one metric for every row, so a row's own configuration is the only thing that differs.
const METRIC_NAME: &str = "SftpErrors";
const METRIC_NAMESPACE: &str = "Sftp/Diagnostics";

/// The values `StateValue` publishes
/// (`smithy_query(query="shape",shape="StateValue")`) — the three states a readable alarm can
/// report, each one reached by a row below.
const STATE_VALUE_VALUES: [&str; 3] = ["OK", "ALARM", "INSUFFICIENT_DATA"];

/// How many pages a read of ONE alarm may take before the paging is judged not to terminate.
const PAGE_LIMIT: usize = 8;

/// How many times, and how far apart, the alarm's state is re-read while waiting for the
/// evaluation the clock owes it: a generous budget for a transition that is due promptly, so
/// the outcome does not depend on how fast the host runs.
const STATE_POLL_ATTEMPTS: usize = 120;
const STATE_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);

/// Where the alarm's name comes from.
#[derive(Debug, Clone, Copy)]
enum Naming {
    /// The declaration's `AlarmName` is `Ref` of a template parameter carrying this value.
    FromParameter(&'static str),
    /// The declaration states this name outright.
    Literal(&'static str),
    /// The declaration states no name, so CloudFormation names the resource itself.
    Generated,
}

/// One row: the estate a template declares, and what the two owning services must then hold.
struct Row {
    case: &'static str,
    stack: &'static str,
    /// The log group the template declares, and the retention CloudWatch Logs must report.
    group: &'static str,
    retention: i64,
    /// Whether the template also declares the metric filter that publishes the metric.
    metric_filter: bool,
    naming: Naming,
    statistic: &'static str,
    comparison_operator: &'static str,
    period: i64,
    evaluation_periods: i64,
    /// The "M out of N" count the declaration states, or `None` where it states none.
    datapoints_to_alarm: Option<i64>,
    /// The threshold exactly as the template spells it — an integer or a fraction, both of
    /// which CloudWatch holds as one `Double` (`PutMetricAlarmInput$Threshold`).
    threshold: Value,
    /// The `TreatMissingData` the declaration states, or `None` where it states none.
    treat_missing_data: Option<&'static str>,
}

/// The state an alarm no datapoint has been published for settles in, decided by its
/// missing-data treatment alone.
///
/// "Sets how this alarm is to handle missing data points. If `TreatMissingData` is omitted,
/// the default behavior of `missing` is used. […] Valid Values: `breaching | notBreaching |
/// ignore | missing`"
/// (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="TreatMissingData")`):
/// `notBreaching` substitutes a non-breaching datapoint for every empty window, so the
/// evaluation reaches `OK`; `breaching` substitutes a breaching one, so it reaches `ALARM`;
/// `missing` substitutes nothing, which leaves the window short of the datapoints either
/// direction needs; and `ignore` keeps the state the alarm is in, which for an alarm nothing
/// has evaluated yet is the `INSUFFICIENT_DATA` its creation set
/// ("When this operation creates an alarm, the alarm state is immediately set to
/// INSUFFICIENT_DATA", `PutMetricAlarm`).
fn settled_state(treat_missing_data: Option<&str>) -> &'static str {
    match treat_missing_data {
        Some("notBreaching") => "OK",
        Some("breaching") => "ALARM",
        _ => "INSUFFICIENT_DATA",
    }
}

/// The rows, spanning the class the rule covers.
///
/// The statistics and comparison operators are the model's own
/// (`smithy_query(query="shape",shape="Statistic")`,
/// `smithy_query(query="shape",shape="ComparisonOperator")`), and the retentions are values
/// `AWS::Logs::LogGroup`'s `RetentionInDays` publishes.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an alarm named by a template parameter, over the metric the filter in the \
                   same template publishes",
            stack: "sftp-diagnosis-parameterised",
            group: "/sim/sftp/parameterised",
            retention: 30,
            metric_filter: true,
            naming: Naming::FromParameter("sftp-error-alarm-parameterised"),
            statistic: "Sum",
            comparison_operator: "GreaterThanOrEqualToThreshold",
            period: 60,
            evaluation_periods: 1,
            datapoints_to_alarm: None,
            threshold: json!(1),
            treat_missing_data: Some("notBreaching"),
        },
        Row {
            case: "an alarm the template names outright, over an average above a fractional \
                   threshold, whose missing data breaches",
            stack: "sftp-diagnosis-named",
            group: "/sim/sftp/named",
            retention: 7,
            metric_filter: false,
            naming: Naming::Literal("sftp-latency-alarm-named"),
            statistic: "Average",
            comparison_operator: "GreaterThanThreshold",
            period: 300,
            evaluation_periods: 2,
            datapoints_to_alarm: None,
            threshold: json!(12.5),
            treat_missing_data: Some("breaching"),
        },
        Row {
            case: "an alarm CloudFormation names itself, an M out of N window and no \
                   missing-data treatment of its own",
            stack: "sftp-diagnosis-generated",
            group: "/sim/sftp/generated",
            retention: 1,
            metric_filter: true,
            naming: Naming::Generated,
            statistic: "Minimum",
            comparison_operator: "LessThanOrEqualToThreshold",
            period: 60,
            evaluation_periods: 3,
            datapoints_to_alarm: Some(2),
            threshold: json!(0),
            treat_missing_data: None,
        },
        Row {
            case: "an alarm that ignores missing data, so it keeps the state it was created in",
            stack: "sftp-diagnosis-ignored",
            group: "/sim/sftp/ignored",
            retention: 90,
            metric_filter: false,
            naming: Naming::Literal("sftp-session-alarm-ignored"),
            statistic: "Maximum",
            comparison_operator: "LessThanThreshold",
            period: 60,
            evaluation_periods: 1,
            datapoints_to_alarm: None,
            threshold: json!(100),
            treat_missing_data: Some("ignore"),
        },
    ]
}

/// The template one row deploys: its log group, its metric filter where it declares one, the
/// alarm, and the two outputs the stack publishes the identifiers under.
fn template_of(row: &Row) -> String {
    let mut alarm = json!({
        "Namespace": METRIC_NAMESPACE,
        "MetricName": METRIC_NAME,
        "Statistic": row.statistic,
        "ComparisonOperator": row.comparison_operator,
        "Period": row.period,
        "EvaluationPeriods": row.evaluation_periods,
        "Threshold": row.threshold,
    });
    if let Some(count) = row.datapoints_to_alarm {
        alarm["DatapointsToAlarm"] = json!(count);
    }
    if let Some(treatment) = row.treat_missing_data {
        alarm["TreatMissingData"] = json!(treatment);
    }
    match row.naming {
        Naming::FromParameter(_) => alarm["AlarmName"] = json!({"Ref": ALARM_NAME_PARAMETER}),
        Naming::Literal(name) => alarm["AlarmName"] = json!(name),
        Naming::Generated => {}
    }
    let mut resources = json!({
        "Group": {
            "Type": "AWS::Logs::LogGroup",
            "Properties": {"LogGroupName": row.group, "RetentionInDays": row.retention},
        },
        "Alarm": {"Type": "AWS::CloudWatch::Alarm", "Properties": alarm},
    });
    if row.metric_filter {
        resources["Filter"] = json!({
            "Type": "AWS::Logs::MetricFilter",
            "Properties": {
                "FilterName": format!("{}-errors", row.stack),
                "LogGroupName": {"Ref": "Group"},
                "FilterPattern": "ERROR",
                "MetricTransformations": [{
                    "MetricName": METRIC_NAME,
                    "MetricNamespace": METRIC_NAMESPACE,
                    "MetricValue": "1",
                }],
            },
        });
    }
    let mut template = json!({
        "Resources": resources,
        "Outputs": {
            "AlarmName": {"Value": {"Ref": "Alarm"}},
            "LogGroupName": {"Value": {"Ref": "Group"}},
        },
    });
    if let Naming::FromParameter(_) = row.naming {
        template["Parameters"] = json!({"AlarmName": {"Type": "String"}});
    }
    template.to_string()
}

/// Every `MetricAlarms` entry `DescribeAlarms` reports for `name`, read to the END of the
/// paging — the read a client makes, so "no entry on any page" is what the absence of the
/// alarm looks like.
///
/// `MaxRecords` is one, so a name the store held more than one entry for would really be
/// paged rather than reported in a single answer.
///
/// `pub(crate)` so the suite over the whole lifecycle of a declared alarm
/// (`declared_alarm_lifecycle_in_cloudwatch.rs`) asks CloudWatch what it holds under a name
/// through this one reader rather than spelling the paging of a `DescribeAlarms` read again.
pub(crate) async fn alarms_named(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Vec<Value> {
    let mut found: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..PAGE_LIMIT {
        let mut request = json!({"AlarmNames": [name], "MaxRecords": 1});
        if let Some(cursor) = &token {
            request["NextToken"] = json!(cursor);
        }
        let (status, document) = cloudwatch(router, "DescribeAlarms", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeAlarms for {name:?}, got {status} \
             with {document}"
        );
        if let Some(page) = document.get("MetricAlarms").and_then(Value::as_array) {
            found.extend(page.iter().cloned());
        }
        token = document
            .get("NextToken")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if token.is_none() {
            break;
        }
    }
    assert!(
        token.is_none(),
        "case {case:?}: expected the read of {name:?} to reach the end of the paging within \
         {PAGE_LIMIT} pages, got a further NextToken"
    );
    found
}

/// The one alarm `DescribeAlarms` reports for `name`, once it reports the state `expected`.
///
/// An alarm's state is EVALUATED rather than written by the caller, and the first evaluation
/// is due a span after the write that configured it, so the state is polled for until it holds
/// — never waited out for a fixed time.
async fn settled_alarm(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    expected: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..STATE_POLL_ATTEMPTS {
        let reported = alarms_named(router, case, name).await;
        let mine: Vec<Value> = reported
            .into_iter()
            .filter(|alarm| alarm.get("AlarmName").and_then(Value::as_str) == Some(name))
            .collect();
        assert_eq!(
            mine.len(),
            1,
            "case {case:?}: expected Amazon CloudWatch to report exactly one alarm named \
             {name:?} — the one the stack reports created — got {mine:?}"
        );
        last = mine.into_iter().next().unwrap_or(Value::Null);
        if last.get("StateValue").and_then(Value::as_str) == Some(expected) {
            break;
        }
        tokio::time::sleep(STATE_POLL_INTERVAL).await;
    }
    last
}

#[tokio::test]
async fn an_alarm_a_stack_declares_is_readable_in_cloudwatch_by_the_name_it_publishes() {
    for row in rows() {
        let case = row.case;
        let router = router();

        let mut extra = acknowledging_iam(&[]);
        if let Naming::FromParameter(value) = row.naming {
            extra.push(member(
                "Parameters.member.1.ParameterKey",
                ALARM_NAME_PARAMETER,
            ));
            extra.push(member("Parameters.member.1.ParameterValue", value));
        }
        let (status, body) = create_stack(&router, row.stack, &template_of(&row), &extra).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
        );
        let described = poll_to_terminal(&router, case, row.stack).await;
        let reported = xml_text(&described, "StackStatus");
        assert_eq!(
            reported.as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {reported:?} because {:?}",
            failure_reasons(&router, row.stack).await
        );

        // The identifier the stack PUBLISHES for the alarm is the handle every later
        // CloudWatch call about it takes, so it is the name the read below addresses.
        let name = output_value(&described, "AlarmName").unwrap_or_default();
        match row.naming {
            Naming::FromParameter(value) | Naming::Literal(value) => assert_eq!(
                name, value,
                "case {case:?}: expected the stack to publish the declared alarm name \
                 {value:?}, got {name:?}"
            ),
            Naming::Generated => {
                let prefix = format!("{}-Alarm-", row.stack);
                assert!(
                    name.starts_with(&prefix),
                    "case {case:?}: expected a generated name under {prefix:?}, got {name:?}"
                );
            }
        }

        // The Logs half of the SAME template, which the reported case already had: the group
        // exists with the retention it declared, and reports the filter declared on it.
        let published_group = output_value(&described, "LogGroupName").unwrap_or_default();
        assert_eq!(
            published_group, row.group,
            "case {case:?}: expected the stack to publish the declared log group name, got \
             {published_group:?}"
        );
        let group = described_group(&router, &published_group)
            .await
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected CloudWatch Logs to hold the group \
                     {published_group:?} the stack reports created, got no group at all"
                )
            });
        assert_eq!(
            group.get("retentionInDays").and_then(Value::as_i64),
            Some(row.retention),
            "case {case:?}: expected the group to carry the declared retention {}, got {group}",
            row.retention
        );
        let expected_filters = i64::from(row.metric_filter);
        assert_eq!(
            group.get("metricFilterCount").and_then(Value::as_i64),
            Some(expected_filters),
            "case {case:?}: expected the group to report {expected_filters} metric filter(s), \
             got {group}"
        );

        // The alarm itself, by the name the stack published: one entry, the declaration's
        // configuration, and a state its own evaluation reached.
        let expected_state = settled_state(row.treat_missing_data);
        let alarm = settled_alarm(&router, case, &name, expected_state).await;

        for (member_name, expected) in [
            ("Namespace", METRIC_NAMESPACE),
            ("MetricName", METRIC_NAME),
            ("Statistic", row.statistic),
            ("ComparisonOperator", row.comparison_operator),
        ] {
            let held = alarm.get(member_name).and_then(Value::as_str);
            assert_eq!(
                held,
                Some(expected),
                "case {case:?}: expected CloudWatch to report {member_name} as {expected:?}, \
                 got {held:?}"
            );
        }
        for (member_name, expected) in [
            ("Period", Some(row.period)),
            ("EvaluationPeriods", Some(row.evaluation_periods)),
            ("DatapointsToAlarm", row.datapoints_to_alarm),
        ] {
            let held = alarm.get(member_name).and_then(Value::as_i64);
            assert_eq!(
                held, expected,
                "case {case:?}: expected CloudWatch to report {member_name} as {expected:?}, \
                 got {held:?}"
            );
        }
        let held_threshold = alarm.get("Threshold").and_then(Value::as_f64);
        assert_eq!(
            held_threshold,
            row.threshold.as_f64(),
            "case {case:?}: expected CloudWatch to report the declared threshold {}, got \
             {held_threshold:?}",
            row.threshold
        );
        let held_treatment = alarm.get("TreatMissingData").and_then(Value::as_str);
        assert_eq!(
            held_treatment,
            row.treat_missing_data,
            "case {case:?}: expected CloudWatch to report TreatMissingData as {:?}, got \
             {held_treatment:?}",
            row.treat_missing_data
        );

        let state = alarm.get("StateValue").and_then(Value::as_str);
        assert!(
            state.is_some_and(|value| STATE_VALUE_VALUES.contains(&value)),
            "case {case:?}: expected a readable StateValue, one of {STATE_VALUE_VALUES:?}, got \
             {state:?}"
        );
        assert_eq!(
            state,
            Some(expected_state),
            "case {case:?}: expected the alarm's own evaluation to reach {expected_state:?} \
             with no datapoint published, got {state:?} ({:?})",
            alarm.get("StateReason")
        );
    }
}

/// The namespace the children of the composite rows below are declared over: one namespace
/// for every row, so a row's own rule is the only thing that differs.
const ROLL_UP_NAMESPACE: &str = "Sim/RollUp";

/// The logical ids the composite rows declare: the roll-up itself and the two children its
/// rule may name.
const ROLL_UP_ID: &str = "RollUp";
const FIRST_CHILD_ID: &str = "Child";
const SECOND_CHILD_ID: &str = "Second";

/// One composite row: the children the template declares, the rule over them, and what the
/// two owning calls must then hold.
struct CompositeRow {
    case: &'static str,
    stack: &'static str,
    /// The name the composite's declaration states, or `None` where CloudFormation names the
    /// resource itself.
    declared_name: Option<&'static str>,
    /// The `TreatMissingData` of each child the template declares, first [`FIRST_CHILD_ID`]
    /// then [`SECOND_CHILD_ID`]: with no datapoint published it is what decides the child's
    /// own state ([`settled_state`]), and so what the rule is evaluated over.
    children: &'static [&'static str],
    /// The rule as the `Fn::Sub` source the declaration carries, naming each child by its
    /// logical id — which is what makes the child a DEPENDENCY of the roll-up, so the stack
    /// creates the alarms the rule reads before the alarm that reads them.
    rule: &'static str,
    /// The state the rule derives from those children: a composite alarm is in `ALARM` when
    /// its rule is true and in `OK` when it is false.
    state: &'static str,
    description: Option<&'static str>,
    actions_enabled: Option<bool>,
    /// The rule a later deployment declares, and the state it then derives.
    updated_rule: &'static str,
    updated_state: &'static str,
}

/// The composite rows, spanning what the rule generalises over: the form of the alarm rule
/// (one state function, a negation, a disjunction, a conjunction, a parenthesised term, the
/// `TRUE`/`FALSE` literals, and a reference both quoted and bare), how many children the
/// stack declares, which state each child settles in, whether the composite's declaration
/// names itself, and whether it carries a description and an `ActionsEnabled` of its own.
///
/// The rule grammar is CloudWatch's own
/// (`smithy_query(query="doc",shape="PutCompositeAlarmInput",member="AlarmRule")`: "ALARM",
/// "OK", "INSUFFICIENT_DATA", "AND", "OR", "NOT", "TRUE", "FALSE", and a name that "can be
/// surrounded with double-quotes (\"), but do not have to be").
fn composite_rows() -> Vec<CompositeRow> {
    vec![
        CompositeRow {
            case: "a named roll-up over one breaching child, negated by the update",
            stack: "rollup-named",
            declared_name: Some("rollup-named-signal"),
            children: &["breaching"],
            rule: "ALARM(\"${Child}\")",
            state: "ALARM",
            description: Some("the roll-up of one child"),
            actions_enabled: None,
            updated_rule: "NOT ALARM(\"${Child}\")",
            updated_state: "OK",
        },
        CompositeRow {
            case: "a roll-up CloudFormation names itself, over one child that is OK, with its \
                   actions disabled",
            stack: "rollup-generated",
            declared_name: None,
            children: &["notBreaching"],
            rule: "ALARM(${Child})",
            state: "OK",
            description: None,
            actions_enabled: Some(false),
            updated_rule: "OK(${Child})",
            updated_state: "ALARM",
        },
        CompositeRow {
            case: "a disjunction over two children, one of each state, narrowed to a \
                   conjunction by the update",
            stack: "rollup-two-children",
            declared_name: Some("rollup-two-children-signal"),
            children: &["notBreaching", "breaching"],
            rule: "ALARM(\"${Child}\") OR ALARM(\"${Second}\")",
            state: "ALARM",
            description: Some("either child"),
            actions_enabled: Some(true),
            updated_rule: "ALARM(\"${Child}\") AND ALARM(\"${Second}\")",
            updated_state: "OK",
        },
        CompositeRow {
            case: "a parenthesised term over one child, replaced by a literal rule",
            stack: "rollup-parenthesised",
            declared_name: Some("rollup-parenthesised-signal"),
            children: &["notBreaching"],
            rule: "(OK(\"${Child}\"))",
            state: "ALARM",
            description: None,
            actions_enabled: None,
            updated_rule: "FALSE",
            updated_state: "OK",
        },
    ]
}

/// The template one composite row deploys: a metric alarm per child, the roll-up over them,
/// and the outputs the stack publishes the identifiers under.
///
/// `threshold` is the child alarms' own, so the update can declare another one and the
/// configuration CloudWatch holds for the child can be read against it.
fn composite_template(row: &CompositeRow, rule: &str, threshold: f64) -> String {
    let mut resources = serde_json::Map::new();
    for (index, treatment) in row.children.iter().enumerate() {
        let logical_id = if index == 0 {
            FIRST_CHILD_ID
        } else {
            SECOND_CHILD_ID
        };
        resources.insert(
            logical_id.to_string(),
            json!({
                "Type": "AWS::CloudWatch::Alarm",
                "Properties": {
                    "Namespace": ROLL_UP_NAMESPACE,
                    "MetricName": format!("{}-{logical_id}", row.stack),
                    "Statistic": "Sum",
                    "Period": 60,
                    "EvaluationPeriods": 1,
                    "Threshold": threshold,
                    "ComparisonOperator": "GreaterThanThreshold",
                    "TreatMissingData": treatment,
                },
            }),
        );
    }
    let mut composite = json!({"AlarmRule": {"Fn::Sub": rule}});
    if let Some(name) = row.declared_name {
        composite["AlarmName"] = json!(name);
    }
    if let Some(description) = row.description {
        composite["AlarmDescription"] = json!(description);
    }
    if let Some(enabled) = row.actions_enabled {
        composite["ActionsEnabled"] = json!(enabled);
    }
    resources.insert(
        ROLL_UP_ID.to_string(),
        json!({"Type": "AWS::CloudWatch::CompositeAlarm", "Properties": composite}),
    );
    let mut outputs = json!({
        "RollUpName": {"Value": {"Ref": ROLL_UP_ID}},
        "RollUpArn": {"Value": {"Fn::GetAtt": [ROLL_UP_ID, "Arn"]}},
        "FirstChildName": {"Value": {"Ref": FIRST_CHILD_ID}},
    });
    if row.children.len() > 1 {
        outputs["SecondChildName"] = json!({"Value": {"Ref": SECOND_CHILD_ID}});
    }
    json!({"Resources": Value::Object(resources), "Outputs": outputs}).to_string()
}

/// Every `CompositeAlarms` entry `DescribeAlarms` reports for `name`, read to the END of the
/// paging — the read a client makes, so "no entry on any page" is what the absence of the
/// alarm looks like.
///
/// "If you omit this parameter, only metric alarms are returned, even if composite alarms or
/// log alarms exist in the account." (`DescribeAlarmsInput$AlarmTypes`) — so the read names
/// the type it is about.
async fn composite_alarms_named(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> Vec<Value> {
    let mut found: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..PAGE_LIMIT {
        let mut request = json!({
            "AlarmNames": [name],
            "AlarmTypes": ["CompositeAlarm"],
            "MaxRecords": 1,
        });
        if let Some(cursor) = &token {
            request["NextToken"] = json!(cursor);
        }
        let (status, document) = cloudwatch(router, "DescribeAlarms", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from DescribeAlarms for {name:?}, got {status} \
             with {document}"
        );
        if let Some(page) = document.get("CompositeAlarms").and_then(Value::as_array) {
            found.extend(
                page.iter()
                    .filter(|alarm| {
                        alarm.get("AlarmName").and_then(Value::as_str) == Some(name)
                    })
                    .cloned(),
            );
        }
        token = document
            .get("NextToken")
            .and_then(Value::as_str)
            .map(ToString::to_string);
        if token.is_none() {
            break;
        }
    }
    assert!(
        token.is_none(),
        "case {case:?}: expected the read of {name:?} to reach the end of the paging within \
         {PAGE_LIMIT} pages, got a further NextToken"
    );
    found
}

/// The one composite alarm `DescribeAlarms` reports for `name`, once it reports the state
/// `expected`.
///
/// A composite alarm's state is DERIVED from the alarms its rule names rather than written by
/// the caller, and the derivation is due a span after the write that configured it, so the
/// state is polled for until it holds — never waited out for a fixed time.
async fn settled_composite(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    expected: &str,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..STATE_POLL_ATTEMPTS {
        let reported = composite_alarms_named(router, case, name).await;
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected Amazon CloudWatch to report exactly one composite alarm \
             named {name:?} — the one the stack reports created — got {reported:?}"
        );
        last = reported.into_iter().next().unwrap_or(Value::Null);
        if last.get("StateValue").and_then(Value::as_str) == Some(expected) {
            break;
        }
        tokio::time::sleep(STATE_POLL_INTERVAL).await;
    }
    last
}

/// RULE: a stack creates the composite alarm its template declares in CloudWatch itself —
/// under the name the stack publishes, with the rule, description and action switch the
/// declaration states — and that alarm derives its state from the children the same stack
/// created; a later deployment that changes the rule or a child's threshold is applied
/// through the same service.
#[tokio::test]
async fn a_composite_alarm_a_stack_declares_derives_its_state_in_cloudwatch() {
    /// The threshold the children are created with, and the one the update declares: a
    /// different value, so what CloudWatch holds for the child after the update cannot be
    /// mistaken for what it held before.
    const CREATED_THRESHOLD: f64 = 1.0;
    const UPDATED_THRESHOLD: f64 = 42.5;

    for row in composite_rows() {
        let case = row.case;
        let router = router();

        let created = composite_template(&row, row.rule, CREATED_THRESHOLD);
        let (status, body) = create_stack(&router, row.stack, &created, &acknowledging_iam(&[])).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {status} with {body}"
        );
        let described = poll_to_terminal(&router, case, row.stack).await;
        let reported = xml_text(&described, "StackStatus");
        assert_eq!(
            reported.as_deref(),
            Some("CREATE_COMPLETE"),
            "case {case:?}: expected the create to complete, got {reported:?} because {:?}",
            failure_reasons(&router, row.stack).await
        );

        // The identifier the stack PUBLISHES for the roll-up is the handle every later
        // CloudWatch call about it takes, so it is the name the reads below address.
        let name = output_value(&described, "RollUpName").unwrap_or_default();
        match row.declared_name {
            Some(declared) => assert_eq!(
                name, declared,
                "case {case:?}: expected the stack to publish the declared alarm name \
                 {declared:?}, got {name:?}"
            ),
            None => {
                let prefix = format!("{}-{ROLL_UP_ID}-", row.stack);
                assert!(
                    name.starts_with(&prefix),
                    "case {case:?}: expected a generated name under {prefix:?}, got {name:?}"
                );
            }
        }
        // "The ARN format of an alarm is arn:aws:cloudwatch:Region:account-id:alarm:alarm-name"
        // — whichever kind of alarm it is, so the published ARN is that of the name above.
        let arn = output_value(&described, "RollUpArn").unwrap_or_default();
        let expected_arn = format!("arn:aws:cloudwatch:{REGION}:{ACCOUNT_ID}:alarm:{name}");
        assert_eq!(
            arn, expected_arn,
            "case {case:?}: expected the published ARN {expected_arn:?}, got {arn:?}"
        );

        // Every child the template declared is a metric alarm CloudWatch holds, in the state
        // its own missing-data treatment settles it in — the states the rule is read over.
        let mut child_names: Vec<String> = Vec::new();
        for (index, treatment) in row.children.iter().enumerate() {
            let key = if index == 0 {
                "FirstChildName"
            } else {
                "SecondChildName"
            };
            let child = output_value(&described, key).unwrap_or_default();
            assert!(
                !child.is_empty(),
                "case {case:?}: expected the stack to publish the child name under {key:?}, \
                 got {described}"
            );
            let settled = settled_alarm(&router, case, &child, settled_state(Some(treatment))).await;
            assert_eq!(
                settled.get("StateValue").and_then(Value::as_str),
                Some(settled_state(Some(treatment))),
                "case {case:?}: expected the child {child:?} to settle in {:?}, got {settled:?}",
                settled_state(Some(treatment))
            );
            assert_eq!(
                settled.get("Threshold").and_then(Value::as_f64),
                Some(CREATED_THRESHOLD),
                "case {case:?}: expected the child {child:?} to carry the declared threshold \
                 {CREATED_THRESHOLD}, got {settled:?}"
            );
            child_names.push(child);
        }

        // The roll-up itself: one entry, the declaration's configuration, and the state its
        // rule derives from those children.
        let alarm = settled_composite(&router, case, &name, row.state).await;
        let expected_rule = resolved_rule(row.rule, &child_names);
        assert_eq!(
            alarm.get("AlarmRule").and_then(Value::as_str),
            Some(expected_rule.as_str()),
            "case {case:?}: expected CloudWatch to hold the declared rule {expected_rule:?}, \
             got {alarm:?}"
        );
        assert_eq!(
            alarm.get("AlarmDescription").and_then(Value::as_str),
            row.description,
            "case {case:?}: expected AlarmDescription {:?}, got {:?}",
            row.description,
            alarm.get("AlarmDescription")
        );
        // "Indicates whether actions should be executed during any changes to the alarm
        // state." (`CompositeAlarm$ActionsEnabled`) — "The default is TRUE"
        // (`PutCompositeAlarmInput$ActionsEnabled`), so a declaration that states none is
        // held with actions enabled.
        assert_eq!(
            alarm.get("ActionsEnabled").and_then(Value::as_bool),
            Some(row.actions_enabled.unwrap_or(true)),
            "case {case:?}: expected ActionsEnabled {:?}, got {:?}",
            row.actions_enabled.unwrap_or(true),
            alarm.get("ActionsEnabled")
        );
        assert_eq!(
            alarm.get("StateValue").and_then(Value::as_str),
            Some(row.state),
            "case {case:?}: expected the rule {expected_rule:?} over those children to derive \
             {:?}, got {:?} ({:?})",
            row.state,
            alarm.get("StateValue"),
            alarm.get("StateReason")
        );

        // A later deployment that changes the rule and a child's threshold is applied through
        // the same two calls: the roll-up derives the new rule's state, and the child carries
        // the threshold the template now declares.
        let updated = composite_template(&row, row.updated_rule, UPDATED_THRESHOLD);
        let (status, body) = call(
            &router,
            "UpdateStack",
            &[
                member("StackName", row.stack),
                member("TemplateBody", &updated),
            ]
            .into_iter()
            .chain(acknowledging_iam(&[]))
            .collect::<Vec<_>>(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200 from UpdateStack, got {status} with {body}"
        );
        let redescribed = poll_to_terminal(&router, case, row.stack).await;
        let reported = xml_text(&redescribed, "StackStatus");
        assert_eq!(
            reported.as_deref(),
            Some("UPDATE_COMPLETE"),
            "case {case:?}: expected the update to complete, got {reported:?} because {:?}",
            failure_reasons(&router, row.stack).await
        );

        let updated_alarm = settled_composite(&router, case, &name, row.updated_state).await;
        let expected_updated_rule = resolved_rule(row.updated_rule, &child_names);
        assert_eq!(
            updated_alarm.get("AlarmRule").and_then(Value::as_str),
            Some(expected_updated_rule.as_str()),
            "case {case:?}: expected the update to put the rule {expected_updated_rule:?}, got \
             {updated_alarm:?}"
        );
        assert_eq!(
            updated_alarm.get("StateValue").and_then(Value::as_str),
            Some(row.updated_state),
            "case {case:?}: expected the updated rule to derive {:?}, got {:?} ({:?})",
            row.updated_state,
            updated_alarm.get("StateValue"),
            updated_alarm.get("StateReason")
        );
        for child in &child_names {
            let reread = alarms_named(&router, case, child).await;
            assert_eq!(
                reread.len(),
                1,
                "case {case:?}: expected the child {child:?} to still be the one alarm of that \
                 name, got {reread:?}"
            );
            assert_eq!(
                reread[0].get("Threshold").and_then(Value::as_f64),
                Some(UPDATED_THRESHOLD),
                "case {case:?}: expected the update to put the threshold {UPDATED_THRESHOLD} on \
                 {child:?}, got {:?}",
                reread[0]
            );
        }
    }
}

/// The rule the stack really put: the declaration's `Fn::Sub` source with each child's
/// logical id replaced by the name that child was created under, which is what `Ref` resolves
/// to.
///
/// Computed from the row and the published child names rather than read out of the answer
/// being checked.
fn resolved_rule(source: &str, child_names: &[String]) -> String {
    let mut resolved = source.to_string();
    for (index, logical_id) in [FIRST_CHILD_ID, SECOND_CHILD_ID].into_iter().enumerate() {
        if let Some(name) = child_names.get(index) {
            resolved = resolved.replace(&format!("${{{logical_id}}}"), name);
        }
    }
    resolved
}
