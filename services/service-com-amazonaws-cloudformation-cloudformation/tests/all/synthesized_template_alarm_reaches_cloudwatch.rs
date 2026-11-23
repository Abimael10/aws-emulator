// SPDX-License-Identifier: CC0-1.0

//! The alarm a SYNTHESIZED template declares, from the deploy that creates it to the stack
//! deletion that takes it away — read through Amazon CloudWatch's own wire at every step.
//!
//! RULE under test: **a resource a stack reports created is created in the service that OWNS
//! it, under the identifier the stack publishes for it, and the SHAPE the template arrived in
//! changes nothing about that** — the sections synthesis tooling wraps a declaration in (a
//! bootstrap-version parameter of an AWS-specific type with a `Rules` assertion over it, a
//! per-resource `Metadata` block, a Region condition and the version-report resource it guards,
//! and `Ref`s to template parameters in place of the declaration's own literals) are template
//! scaffolding, not part of the resource, so the `AWS::CloudWatch::Alarm` such a template
//! declares is one `DescribeAlarms(AlarmNames=[<the published name>])` reports exactly once,
//! carrying the declared namespace, metric, statistic, period, evaluation periods, threshold
//! and comparison; CloudWatch then EVALUATES it — `INSUFFICIENT_DATA` while no datapoint of its
//! series exists, and the state one published datapoint implies once that datapoint is there,
//! recorded as a `StateUpdate` history item whose `HistoryData` names the state it came from
//! and the state it reached; and the stack's own deletion removes it, after which that name
//! resolves to nothing.
//!
//! The reported case deployed an order-service application from a synthesized, asset-free
//! template through plain `CreateStack`, polled the stack to `CREATE_COMPLETE` and then read
//! CloudWatch for the alarm name it had passed in: `MetricAlarms` came back EMPTY, so the
//! deploy looked healthy while the application's only observability gate did not exist —
//! nothing evaluated the threshold, no transition was ever recorded, and a client polling the
//! alarm's `StateValue` read "no rows" forever.
//!
//! The rows differ along what the rule generalises over rather than restating that one
//! template:
//!
//! * HOW MUCH of the synthesis scaffolding the template carries — all of it with the Region
//!   condition holding, all of it with the condition naming other Regions only (so the
//!   version-report resource is skipped), the bootstrap parameter and its rule alone, and a
//!   bare declaration with none of it;
//! * where the declaration's own values come from — `Ref` of a template parameter for the
//!   alarm's name and for its namespace, literals in the declaration, and no `AlarmName` at
//!   all, which CloudFormation names itself;
//! * the configuration: three statistics, four comparison operators, a period of half a
//!   minute, of a minute and of two, one and two evaluation periods, a threshold spelled as an
//!   integer and as a fraction, and no dimension, one and two;
//! * which state the published datapoint drives the alarm to — `ALARM` where it breaches the
//!   row's comparison and `OK` where it does not.
//!
//! Nothing is waited out: the stack is polled to the status it settles on, the alarm's state is
//! polled until the evaluation the clock owes has been materialised, and the datapoints are
//! stamped into period windows that have already CLOSED rather than being waited for.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudformation_cloudformation::state::status;

use crate::cloudwatch_alarm_and_ecs_service_resources::{
    alarm_history_items, cloudwatch, settled_alarm_history_items,
};
use crate::declared_alarm_is_readable_in_cloudwatch::{alarms_named, settled_alarm};
use crate::dynamic_references::put_parameter;
use crate::dynamodb_table_resources::failure_reasons;
use crate::harness::{call, create_stack, member, router, settled_stack, xml_text, REGION};
use crate::stack_lifecycle::{output_value, ACCOUNT_ID};

/// The type under test, and the logical id it is declared under in every row.
const ALARM_TYPE: &str = "AWS::CloudWatch::Alarm";
const ALARM_ID: &str = "Alarm";

/// The output keys every row publishes the alarm's identity under: `Ref` of the resource ("Ref
/// returns the alarm name") and `Fn::GetAtt … Arn`.
const NAME_OUTPUT: &str = "AlarmName";
const ARN_OUTPUT: &str = "AlarmArn";

/// The metric every row's alarm watches, which is also the metric its datapoints are published
/// under: the application's own counter, so the series is a custom one in the row's namespace.
const METRIC_NAME: &str = "OrdersFulfilled";

/// The state an alarm no datapoint of its series has been published for reports: "When this
/// operation creates an alarm, the alarm state is immediately set to INSUFFICIENT_DATA"
/// (`PutMetricAlarm`), and the default missing-data treatment substitutes no datapoint for an
/// empty window ("If `TreatMissingData` is omitted, the default behavior of `missing` is used",
/// `smithy_query(query="doc",shape="PutMetricAlarmInput",member="TreatMissingData")`), so the
/// alarm's own evaluation leaves it there until a datapoint arrives.
const UN_EVALUATED: &str = "INSUFFICIENT_DATA";

/// The template parameters a row may take the alarm's name and namespace from, which are also
/// the keys `CreateStack` passes their values under.
const NAME_PARAMETER: &str = "AlarmName";
const NAMESPACE_PARAMETER: &str = "MetricNamespace";

/// The version-report resource synthesis tooling adds to a stack, the logical id it gives it,
/// the condition it guards it with and the form it encodes the report in.
const REPORT_TYPE: &str = "AWS::CDK::Metadata";
const REPORT_ID: &str = "CDKMetadata";
const REPORT_CONDITION: &str = "CDKMetadataAvailable";
const REPORT_ANALYTICS: &str = "v2:deflate64:H4sIAAAAAAAA";

/// The Parameter Store parameter such a template reads the bootstrap version from, and a
/// version outside the range the rule over it rejects.
const BOOTSTRAP_PARAMETER_NAME: &str = "BootstrapVersion";
const BOOTSTRAP_VERSION_PARAMETER: &str = "/cdk-bootstrap/hnb659fds/version";
const BOOTSTRAP_VERSION: &str = "21";

/// The history item type a transition is recorded as: one of `HistoryItemType`'s values
/// (`smithy_query(query="shape",shape="HistoryItemType")`).
const STATE_UPDATE: &str = "StateUpdate";

/// WHERE in the past a row's datapoints are stamped, as multiples of the alarm's own period.
///
/// A metric is aggregated into periods anchored on multiples of the period and only the periods
/// that have CLOSED are graded, so a datapoint a row wants graded is stamped at least one whole
/// period back; the half-period offset puts it inside a window whichever phase of a period the
/// suite happens to run in. Three consecutive windows are filled so the two most recent real
/// datapoints the evaluation finds are the row's own however the clock crosses a boundary while
/// the publish and the read run.
const WINDOW_OFFSETS: [f64; 3] = [1.5, 2.5, 3.5];

/// The `StorageResolution` a datapoint is published at when the alarm's period is finer than a
/// minute: "1 for high resolution, 60 for regular resolution" (`MetricDatum$StorageResolution`)
/// — a series stored at regular resolution holds one datapoint per minute however often a
/// client publishes into it, so a sub-minute window can only hold a datapoint of its own at the
/// finer resolution.
const HIGH_RESOLUTION: i64 = 1;

/// How much of the synthesis scaffolding one row's template carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scaffolding {
    /// All of it: the bootstrap-version parameter and the rule over it, the per-resource
    /// `Metadata`, and the version-report resource under a Region condition that HOLDS, so that
    /// resource is part of the deployment too.
    Whole,
    /// The same, with the condition naming other Regions only — so the version-report resource
    /// is skipped and the alarm is the one resource the stack creates.
    ReportSkipped,
    /// The bootstrap-version parameter and its rule, with no version-report resource at all.
    BootstrapOnly,
    /// None of it: the bare declaration a hand-written template carries.
    Bare,
}

impl Scaffolding {
    /// Whether the template declares the bootstrap-version parameter and the rule over it.
    fn carries_bootstrap(self) -> bool {
        self != Self::Bare
    }

    /// Whether the template declares the version-report resource and the condition over it.
    fn declares_the_report(self) -> bool {
        matches!(self, Self::Whole | Self::ReportSkipped)
    }

    /// Whether that condition holds in this suite's Region, which is what decides whether the
    /// version-report resource is deployed at all.
    fn the_report_is_deployed(self) -> bool {
        self == Self::Whole
    }
}

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

/// One row: the template one deploy carries, and what CloudWatch must then hold and evaluate.
struct Row {
    case: &'static str,
    stack: &'static str,
    scaffolding: Scaffolding,
    naming: Naming,
    /// The namespace the alarm watches its metric in, and whether the declaration states it
    /// through `Ref` of a template parameter rather than as a literal.
    namespace: &'static str,
    namespace_from_parameter: bool,
    /// One of `Statistic`'s values (`smithy_query(query="shape",shape="Statistic")`).
    statistic: &'static str,
    /// One of `ComparisonOperator`'s values
    /// (`smithy_query(query="shape",shape="ComparisonOperator")`).
    comparison: &'static str,
    /// "Valid values are 10, 20, 30, and any multiple of 60."
    /// (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="Period")`).
    period: i32,
    evaluation_periods: i32,
    /// The threshold exactly as the template spells it — an integer or a fraction, both of which
    /// CloudWatch holds as one `Double` (`PutMetricAlarmInput$Threshold`).
    threshold: Value,
    /// The metric's dimensions, as `(Name, Value)` pairs; the empty slice is a metric with none.
    dimensions: &'static [(&'static str, &'static str)],
    /// The value every datapoint this row publishes carries, and the state the alarm's own
    /// evaluation must then reach: `ALARM` where the datapoint breaches the row's comparison
    /// against its threshold and `OK` where it does not.
    datapoint: f64,
    evaluated_state: &'static str,
}

/// The rows, spanning the class the rule covers.
fn rows() -> Vec<Row> {
    vec![
        // The shape the reported deploy carried: the whole scaffolding, with the alarm's name
        // and its namespace handed in as template parameters, and a datapoint over a threshold
        // of one.
        Row {
            case: "the whole synthesis scaffolding, the alarm's name and namespace from \
                   template parameters, a datapoint over the threshold",
            stack: "synthesized-alarm-whole",
            scaffolding: Scaffolding::Whole,
            naming: Naming::FromParameter("sim-xsvc-alarm-whole"),
            namespace: "Sim/OrderService",
            namespace_from_parameter: true,
            statistic: "Sum",
            comparison: "GreaterThanOrEqualToThreshold",
            period: 60,
            evaluation_periods: 1,
            threshold: json!(1),
            dimensions: &[],
            datapoint: 2.0,
            evaluated_state: "ALARM",
        },
        // The same scaffolding whose version-report resource its own condition skips, a name
        // the declaration states, two evaluation periods and a datapoint that leaves the alarm
        // clean.
        Row {
            case: "the scaffolding with its version report skipped by the condition, a literal \
                   name, a datapoint under a fractional threshold",
            stack: "synthesized-alarm-report-skipped",
            scaffolding: Scaffolding::ReportSkipped,
            naming: Naming::Literal("sim-xsvc-alarm-report-skipped"),
            namespace: "Sim/Fulfilment",
            namespace_from_parameter: false,
            statistic: "Average",
            comparison: "GreaterThanThreshold",
            period: 120,
            evaluation_periods: 2,
            threshold: json!(2.5),
            dimensions: &[("Service", "orders")],
            datapoint: 1.0,
            evaluated_state: "OK",
        },
        // The bootstrap parameter and its rule alone, with no `AlarmName` at all — so the name
        // CloudWatch holds the alarm under is the one CloudFormation generated and published.
        Row {
            case: "the bootstrap parameter and its rule alone, a name CloudFormation generates, \
                   a datapoint under a threshold a LessThanThreshold alarm breaches",
            stack: "synthesized-alarm-generated-name",
            scaffolding: Scaffolding::BootstrapOnly,
            naming: Naming::Generated,
            namespace: "Sim/Intake",
            namespace_from_parameter: false,
            statistic: "Maximum",
            comparison: "LessThanThreshold",
            period: 60,
            evaluation_periods: 2,
            threshold: json!(0.5),
            dimensions: &[("Service", "intake"), ("Stage", "prod")],
            datapoint: 0.25,
            evaluated_state: "ALARM",
        },
        // None of the scaffolding: the bare declaration, at a period finer than a minute.
        Row {
            case: "a bare declaration at a sub-minute period, a datapoint over a \
                   LessThanOrEqualToThreshold alarm's threshold",
            stack: "synthesized-alarm-bare",
            scaffolding: Scaffolding::Bare,
            naming: Naming::Literal("sim-xsvc-alarm-bare"),
            namespace: "Sim/Bare",
            namespace_from_parameter: false,
            statistic: "Sum",
            comparison: "LessThanOrEqualToThreshold",
            period: 30,
            evaluation_periods: 1,
            threshold: json!(10),
            dimensions: &[("Service", "bare")],
            datapoint: 20.0,
            evaluated_state: "OK",
        },
    ]
}

/// The `Dimensions` one row declares, in the `[{Name, Value}]` form both
/// `PutMetricAlarm$Dimensions` and `MetricDatum$Dimensions` take.
fn dimensions_of(row: &Row) -> Vec<Value> {
    row.dimensions
        .iter()
        .map(|(name, value)| json!({"Name": name, "Value": value}))
        .collect()
}

/// The template one row deploys: the alarm, wrapped in as much of the synthesis scaffolding as
/// the row carries, publishing the alarm's name and ARN as outputs.
fn template(row: &Row) -> String {
    let namespace = if row.namespace_from_parameter {
        json!({"Ref": NAMESPACE_PARAMETER})
    } else {
        json!(row.namespace)
    };
    let mut properties = json!({
        "Namespace": namespace,
        "MetricName": METRIC_NAME,
        "Statistic": row.statistic,
        "Period": row.period,
        "EvaluationPeriods": row.evaluation_periods,
        "Threshold": row.threshold,
        "ComparisonOperator": row.comparison,
        "Dimensions": dimensions_of(row),
    });
    match row.naming {
        Naming::FromParameter(_) => properties["AlarmName"] = json!({"Ref": NAME_PARAMETER}),
        Naming::Literal(name) => properties["AlarmName"] = json!(name),
        Naming::Generated => {}
    }

    let mut alarm = json!({"Type": ALARM_TYPE, "Properties": properties});
    if row.scaffolding.carries_bootstrap() {
        // The per-resource block synthesis tooling stamps the construct path into.
        alarm["Metadata"] = json!({"aws:cdk:path": format!("{}/Alarm/Resource", row.stack)});
    }
    let mut resources = serde_json::Map::new();
    resources.insert(ALARM_ID.to_string(), alarm);
    if row.scaffolding.declares_the_report() {
        resources.insert(
            REPORT_ID.to_string(),
            json!({
                "Type": REPORT_TYPE,
                "Properties": {"Analytics": REPORT_ANALYTICS},
                "Condition": REPORT_CONDITION,
                "Metadata": {"aws:cdk:path": format!("{}/CDKMetadata/Default", row.stack)},
            }),
        );
    }

    let mut declared = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "the order service's observability gate",
        "Resources": Value::Object(resources),
        "Outputs": {
            NAME_OUTPUT: {"Value": {"Ref": ALARM_ID}},
            ARN_OUTPUT: {"Value": {"Fn::GetAtt": [ALARM_ID, "Arn"]}},
        },
    });

    let mut parameters = serde_json::Map::new();
    if matches!(row.naming, Naming::FromParameter(_)) {
        parameters.insert(NAME_PARAMETER.to_string(), json!({"Type": "String"}));
    }
    if row.namespace_from_parameter {
        parameters.insert(NAMESPACE_PARAMETER.to_string(), json!({"Type": "String"}));
    }
    if row.scaffolding.carries_bootstrap() {
        parameters.insert(
            BOOTSTRAP_PARAMETER_NAME.to_string(),
            json!({
                "Type": "AWS::SSM::Parameter::Value<String>",
                "Default": BOOTSTRAP_VERSION_PARAMETER,
                "Description": "Version of the bootstrap resources in this environment",
            }),
        );
        declared["Rules"] = json!({
            "CheckBootstrapVersion": {
                "Assertions": [{
                    "Assert": {"Fn::Not": [{"Fn::Contains": [
                        ["1", "2", "3", "4", "5"], {"Ref": BOOTSTRAP_PARAMETER_NAME},
                    ]}]},
                    "AssertDescription":
                        "bootstrap stack version 6 required. Please run a recent bootstrap.",
                }],
            },
        });
    }
    if !parameters.is_empty() {
        declared["Parameters"] = Value::Object(parameters);
    }
    if row.scaffolding.declares_the_report() {
        // A disjunction of equalities over the Region, as the tooling emits its condition: the
        // row decides whether this Region is one of them.
        let regions: &[&str] = if row.scaffolding.the_report_is_deployed() {
            &[REGION, "eu-west-1"]
        } else {
            &["eu-west-1", "ap-south-1"]
        };
        let equalities: Vec<Value> = regions
            .iter()
            .map(|region| json!({"Fn::Equals": [{"Ref": "AWS::Region"}, region]}))
            .collect();
        declared["Conditions"] = json!({REPORT_CONDITION: {"Fn::Or": equalities}});
    }
    declared.to_string()
}

/// The `Parameters.member.N.*` request members one row's parameter values are sent as. The
/// bootstrap-version parameter carries a `Default`, so a client passes no value for it — which
/// is what makes Parameter Store's own record of the version the thing the rule is read over.
fn stack_parameters(row: &Row) -> Vec<(String, String)> {
    let mut declared: Vec<(&str, &str)> = Vec::new();
    if let Naming::FromParameter(value) = row.naming {
        declared.push((NAME_PARAMETER, value));
    }
    if row.namespace_from_parameter {
        declared.push((NAMESPACE_PARAMETER, row.namespace));
    }
    let mut members = Vec::new();
    for (position, (key, value)) in declared.into_iter().enumerate() {
        let index = position + 1;
        members.push(member(&format!("Parameters.member.{index}.ParameterKey"), key));
        members.push(member(
            &format!("Parameters.member.{index}.ParameterValue"),
            value,
        ));
    }
    members
}

/// Wall-clock now as epoch seconds — the clock an alarm's period windows are measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// Publish one row's datapoint into each of the period windows [`WINDOW_OFFSETS`] names, as the
/// application whose metric the alarm watches publishes it.
async fn publish_the_series(router: &MultiServiceRouter, case: &str, row: &Row) {
    let period = f64::from(row.period);
    let now = now_secs();
    let data: Vec<Value> = WINDOW_OFFSETS
        .iter()
        .map(|offset| {
            let mut datum = json!({
                "MetricName": METRIC_NAME,
                "Dimensions": dimensions_of(row),
                "Value": row.datapoint,
                "Timestamp": now - offset * period,
            });
            if period < 60.0 {
                datum["StorageResolution"] = json!(HIGH_RESOLUTION);
            }
            datum
        })
        .collect();
    let (code, answered) = cloudwatch(
        router,
        "PutMetricData",
        &json!({"Namespace": row.namespace, "MetricData": data}),
    )
    .await;
    assert_eq!(
        code, 200,
        "case {case:?}: expected HTTP 200 from PutMetricData, got {code} with {answered}"
    );
}

/// The state value one transition document names on one side of the move.
fn transition_state(document: &Value, side: &str) -> Option<String> {
    document
        .pointer(&format!("/{side}/stateValue"))
        .and_then(Value::as_str)
        .map(ToString::to_string)
}

#[tokio::test]
async fn a_synthesized_templates_alarm_is_created_evaluated_and_removed_in_cloudwatch() {
    for row in rows() {
        let case = row.case;
        let router = router();

        // The bootstrap version the template's own rule is read over, where the row carries
        // that parameter at all.
        if row.scaffolding.carries_bootstrap() {
            put_parameter(
                &router,
                case,
                BOOTSTRAP_VERSION_PARAMETER,
                "String",
                BOOTSTRAP_VERSION,
            )
            .await;
        }

        let (code, created) =
            create_stack(&router, row.stack, &template(&row), &stack_parameters(&row)).await;
        assert_eq!(
            code, 200,
            "case {case:?}: expected HTTP 200 from CreateStack, got {code} with {created}"
        );
        let stack_id = xml_text(&created, "StackId").unwrap_or_else(|| row.stack.to_string());

        let described = settled_stack(case, &router, REGION, row.stack).await;
        assert_eq!(
            xml_text(&described, "StackStatus").as_deref(),
            Some(status::CREATE_COMPLETE),
            "case {case:?}: expected the create to complete, got {described} because {:?}",
            failure_reasons(&router, row.stack).await
        );

        // The identifier the stack PUBLISHES for the alarm is the handle every later CloudWatch
        // call about it takes, so it is the name every read below addresses.
        let name = output_value(&described, NAME_OUTPUT).unwrap_or_default();
        match row.naming {
            Naming::FromParameter(declared) | Naming::Literal(declared) => assert_eq!(
                name, declared,
                "case {case:?}: expected the stack to publish the declared alarm name \
                 {declared:?}, got {name:?}"
            ),
            Naming::Generated => {
                let prefix = format!("{}-{ALARM_ID}-", row.stack);
                assert!(
                    name.starts_with(&prefix),
                    "case {case:?}: expected a generated name under {prefix:?}, got {name:?}"
                );
            }
        }

        // THE ISSUE'S OWN OBSERVABLE: the alarm EXISTS, with the configuration the declaration
        // carried, and reads un-evaluated while no datapoint of its series has been published.
        let alarm = settled_alarm(&router, case, &name, UN_EVALUATED).await;
        let created_state = alarm.get("StateValue").and_then(Value::as_str);
        assert_eq!(
            created_state,
            Some(UN_EVALUATED),
            "case {case:?}: expected the alarm to read {UN_EVALUATED:?} while no datapoint of \
             its series exists, got {created_state:?} ({:?})",
            alarm.get("StateReason")
        );
        for (reported, expected) in [
            ("Namespace", row.namespace),
            ("MetricName", METRIC_NAME),
            ("Statistic", row.statistic),
            ("ComparisonOperator", row.comparison),
        ] {
            let held = alarm.get(reported).and_then(Value::as_str);
            assert_eq!(
                held,
                Some(expected),
                "case {case:?}: expected CloudWatch to report {reported} as {expected:?}, got \
                 {held:?}"
            );
        }
        for (reported, expected) in [
            ("Period", i64::from(row.period)),
            ("EvaluationPeriods", i64::from(row.evaluation_periods)),
        ] {
            let held = alarm.get(reported).and_then(Value::as_i64);
            assert_eq!(
                held,
                Some(expected),
                "case {case:?}: expected CloudWatch to report {reported} as {expected}, got \
                 {held:?}"
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
        let held_dimensions = alarm.get("Dimensions").cloned().unwrap_or(json!([]));
        assert_eq!(
            held_dimensions,
            Value::Array(dimensions_of(&row)),
            "case {case:?}: expected CloudWatch to report the declared dimensions, got \
             {held_dimensions}"
        );

        // The ARN the stack publishes for the alarm is the one CloudWatch holds it at: "The ARN
        // format of an alarm is arn:aws:cloudwatch:Region:account-id:alarm:alarm-name".
        let published_arn = output_value(&described, ARN_OUTPUT).unwrap_or_default();
        let expected_arn = format!("arn:aws:cloudwatch:{REGION}:{ACCOUNT_ID}:alarm:{name}");
        assert_eq!(
            published_arn, expected_arn,
            "case {case:?}: expected the stack to publish {expected_arn:?} for the alarm, got \
             {published_arn:?}"
        );
        assert_eq!(
            alarm.get("AlarmArn").and_then(Value::as_str),
            Some(published_arn.as_str()),
            "case {case:?}: expected CloudWatch to hold the alarm at the ARN the stack \
             published, got {alarm:?}"
        );

        // Nothing has recorded a transition yet: the alarm was created un-evaluated and no
        // datapoint has moved it.
        let before = alarm_history_items(&router, case, &name, STATE_UPDATE).await;
        assert!(
            before.is_empty(),
            "case {case:?}: expected no {STATE_UPDATE} item before a datapoint is published, \
             got {before:?}"
        );

        // The datapoint the application publishes is what the alarm is evaluated over, so the
        // state it implies is the state CloudWatch then reports...
        publish_the_series(&router, case, &row).await;
        let evaluated = settled_alarm(&router, case, &name, row.evaluated_state).await;
        let graded_state = evaluated.get("StateValue").and_then(Value::as_str);
        assert_eq!(
            graded_state,
            Some(row.evaluated_state),
            "case {case:?}: expected the datapoint to drive the alarm to {:?}, got \
             {graded_state:?} ({:?})",
            row.evaluated_state,
            evaluated.get("StateReason")
        );
        let reason = evaluated
            .get("StateReason")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert!(
            !reason.is_empty(),
            "case {case:?}: expected the evaluated state to carry its own explanation, got \
             {evaluated:?}"
        );

        // ...and the move out of the un-evaluated state is recorded, naming both states.
        let items = settled_alarm_history_items(&router, case, &name, STATE_UPDATE).await;
        assert_eq!(
            items.len(),
            1,
            "case {case:?}: expected exactly one {STATE_UPDATE} item for the one transition the \
             datapoint drove, got {items:?}"
        );
        let item = items.into_iter().next().expect("one item");
        assert_eq!(
            transition_state(&item, "oldState").as_deref(),
            Some(UN_EVALUATED),
            "case {case:?}: expected the transition to name {UN_EVALUATED:?} as the state it \
             came from, got {item}"
        );
        assert_eq!(
            transition_state(&item, "newState").as_deref(),
            Some(row.evaluated_state),
            "case {case:?}: expected the transition to name {:?} as the state it reached, got \
             {item}",
            row.evaluated_state
        );

        // The stack's own deletion removes the alarm it manages, and that name then resolves to
        // nothing — the last step of the deploy-then-observe flow the reported case never
        // reached.
        let (code, body) = call(&router, "DeleteStack", &[member("StackName", row.stack)]).await;
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
        let after = alarms_named(&router, case, &name).await;
        assert!(
            after.is_empty(),
            "case {case:?}: expected DescribeAlarms {name:?} to report no alarm once the stack \
             that managed it is gone, got {after:?}"
        );
    }
}
