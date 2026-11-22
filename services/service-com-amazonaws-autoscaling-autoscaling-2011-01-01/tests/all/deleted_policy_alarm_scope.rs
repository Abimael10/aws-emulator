// SPDX-License-Identifier: CC0-1.0

//! What a scaling policy's DELETION does to the CloudWatch alarms around it, over the
//! awsQuery wire.
//!
//! The rules under test:
//!
//! 1. "Deletes the specified scaling policy": the alarms the service itself created FOR that
//!    policy — "the CloudWatch alarms created for the target tracking scaling policy"
//!    (`PolicyARNType$Alarms`) — are retired from the account's own CloudWatch store with
//!    it, and NOTHING else of the group is: every other policy of the group, and every alarm
//!    those policies hold, is exactly where it was. The table spans what decides a policy's
//!    own alarm set (its type, and whether scaling in is disabled, which withholds the
//!    scale-in alarm) for the deleted policy and for the sibling that must survive it, and
//!    the spelling the delete names its policy by.
//! 2. "Deleting either a step scaling policy or a simple scaling policy deletes the
//!    underlying alarm action, but does not delete the alarm, even if it no longer has an
//!    associated action": an alarm the CLIENT created is its own resource, so it stays — with
//!    the deleted policy no longer among the actions it acts through, and every other action
//!    it held still there. The table spans the action list the policy sits in (an Auto
//!    Scaling policy is a documented action of all three of `AlarmActions`, `OKActions` and
//!    `InsufficientDataActions`), the kind of alarm that holds it, and the type of the policy
//!    deleted.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch_service;

use crate::support::{borrowed, json_rpc, new_engine, p, query, tag_value, tag_values};

/// The launch source every group in this suite is created from — a launch TEMPLATE, because
/// no account can create a launch configuration.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-1");

/// The signing name a client scopes a CloudWatch request to.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The `x-amz-target` prefix CloudWatch's awsJson protocol names its operations under.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// The awsJson version a CloudWatch client sends.
const CLOUDWATCH_JSON_VERSION: &str = "1.0";

/// The prefix Amazon EC2 Auto Scaling names a target-tracking policy's alarms with, for the
/// group `group`: every alarm of every policy of that group, and nothing else.
fn group_alarm_prefix(group: &str) -> String {
    format!("TargetTracking-{group}-")
}

/// A router carrying Amazon EC2 Auto Scaling and CloudWatch on ONE engine: the alarms a
/// policy holds, and the alarms a client creates over them, are CloudWatch's own resources,
/// so both are read back through CloudWatch's own API.
fn router() -> MultiServiceRouter {
    let engine = new_engine();
    crate::support::seed_launch_templates(&engine, &[LAUNCH_SOURCE.1]);
    let mut router = MultiServiceRouter::new();
    router.register_service(autoscaling::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(cloudwatch_service::EmulatedService::new(Arc::clone(&engine)));
    router
}

/// Create one group and assert the create succeeded.
async fn create_group(router: &MultiServiceRouter, name: &str) {
    let (status, body) = query(
        router,
        "CreateAutoScalingGroup",
        &[
            ("AutoScalingGroupName", name),
            LAUNCH_SOURCE,
            ("MinSize", "0"),
            ("MaxSize", "4"),
            ("DesiredCapacity", "0"),
            ("AvailabilityZones.member.1", "us-west-2a"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the group {name:?} must be created, got {status} with {body}"
    );
}

/// Put one scaling policy and answer the `(PolicyARN, alarm names)` the service answered.
async fn put_policy(
    router: &MultiServiceRouter,
    group: &str,
    policy: &str,
    members: &[(String, String)],
) -> (String, Vec<String>) {
    let mut params = vec![p("AutoScalingGroupName", group), p("PolicyName", policy)];
    params.extend(members.iter().cloned());
    let (status, body) = query(router, "PutScalingPolicy", &borrowed(&params)).await;
    assert_eq!(
        status, 200,
        "the policy {policy:?} must be put, got {status} with {body}"
    );
    let arn = tag_value(&body, "PolicyARN");
    assert!(
        !arn.is_empty(),
        "PutScalingPolicy must report the policy's ARN, got {body}"
    );
    let mut alarms = tag_values(&body, "AlarmName");
    alarms.sort();
    (arn, alarms)
}

/// The policy names Amazon EC2 Auto Scaling reports for `group` right now.
async fn reported_policies(router: &MultiServiceRouter, group: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribePolicies",
        &[("AutoScalingGroupName", group)],
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribePolicies for {group:?} expected 200, got {status}: {body}"
    );
    tag_values(&body, "PolicyName")
}

/// One CloudWatch call that must succeed, answered as the JSON a client parses.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: Value,
) -> Value {
    let (status, answer) = json_rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        CLOUDWATCH_JSON_VERSION,
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CloudWatch {operation} expected 200, got {status} with {answer}"
    );
    answer
}

/// The alarms CloudWatch's own store reports under `body`, from the array `field`.
async fn described(
    router: &MultiServiceRouter,
    case: &str,
    body: Value,
    field: &str,
) -> Vec<Value> {
    cloudwatch(router, case, "DescribeAlarms", body)
        .await
        .pointer(&format!("/{field}"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// The names CloudWatch resolves for the metric alarms `names`, sorted.
async fn resolved_metric_alarms(
    router: &MultiServiceRouter,
    case: &str,
    names: &[String],
) -> Vec<String> {
    if names.is_empty() {
        // An empty `AlarmNames` asks for EVERY alarm, which is not what a reader of one
        // policy's alarms means: a policy holding none resolves to none.
        return Vec::new();
    }
    let mut resolved: Vec<String> = described(
        router,
        case,
        json!({"AlarmNames": names, "AlarmTypes": ["MetricAlarm"]}),
        "MetricAlarms",
    )
    .await
    .iter()
    .map(|alarm| string_member(alarm, "AlarmName"))
    .collect();
    resolved.sort();
    resolved
}

/// Every alarm name CloudWatch holds for the policies of `group`, sorted.
async fn group_alarm_names(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let mut names: Vec<String> = described(
        router,
        case,
        json!({"AlarmNamePrefix": group_alarm_prefix(group), "AlarmTypes": ["MetricAlarm"]}),
        "MetricAlarms",
    )
    .await
    .iter()
    .map(|alarm| string_member(alarm, "AlarmName"))
    .collect();
    names.sort();
    names
}

/// The string member `name` of one reported alarm.
fn string_member(alarm: &Value, name: &str) -> String {
    alarm
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// The string list member `name` of one reported alarm.
fn list_member(alarm: &Value, name: &str) -> Vec<String> {
    alarm
        .get(name)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// The members of a `SimpleScaling` put, which is a policy type that holds no alarm.
fn simple_policy() -> Vec<(String, String)> {
    vec![
        p("PolicyType", "SimpleScaling"),
        p("AdjustmentType", "ChangeInCapacity"),
        p("ScalingAdjustment", "1"),
    ]
}

/// The members of a `StepScaling` put, the other type that holds none.
fn step_policy() -> Vec<(String, String)> {
    vec![
        p("PolicyType", "StepScaling"),
        p("AdjustmentType", "ChangeInCapacity"),
        p("StepAdjustments.member.1.MetricIntervalLowerBound", "0"),
        p("StepAdjustments.member.1.ScalingAdjustment", "1"),
    ]
}

/// The members of a `TargetTrackingScaling` put over the group's own CPU utilization,
/// tracking `target_value`, with scaling in disabled when `disable_scale_in` — which
/// withholds the scale-in alarm, so the policy holds one alarm rather than two.
fn target_tracking(target_value: &str, disable_scale_in: bool) -> Vec<(String, String)> {
    let mut members = vec![
        p("PolicyType", "TargetTrackingScaling"),
        p("TargetTrackingConfiguration.TargetValue", target_value),
        p(
            "TargetTrackingConfiguration.PredefinedMetricSpecification.PredefinedMetricType",
            "ASGAverageCPUUtilization",
        ),
    ];
    if disable_scale_in {
        members.push(p("TargetTrackingConfiguration.DisableScaleIn", "true"));
    }
    members
}

/// A target-tracking policy that scales in, so it holds its full alarm pair.
fn scaling_in_policy() -> Vec<(String, String)> {
    target_tracking("43.5", false)
}

/// The same policy with scaling in disabled, so it holds only the scale-out alarm.
fn scale_in_disabled_policy() -> Vec<(String, String)> {
    target_tracking("43.5", true)
}

/// How the delete names the policy it removes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// By its name, inside the group the request names.
    Name,
    /// By the ARN the service minted for it.
    Arn,
}

/// One row of rule 1: the policy deleted, the sibling that must survive it, how many alarms
/// each holds, and the spelling the delete names the doomed one by.
struct ScopeCase {
    case: &'static str,
    doomed: fn() -> Vec<(String, String)>,
    doomed_alarms: usize,
    twin: fn() -> Vec<(String, String)>,
    twin_alarms: usize,
    named: Named,
}

/// RULE 1: a policy's deletion retires exactly the alarms that policy holds, and the sibling
/// policy of the same group keeps its own.
#[tokio::test]
async fn a_deleted_policy_retires_only_the_alarms_it_holds() {
    // The rows differ along what decides each policy's alarm set and along the spelling the
    // delete names its policy by. The two target-tracking rows are TWINS that differ only in
    // whether scaling in is disabled, so neither one's alarms can be mistaken for the
    // other's by the alarm count alone.
    let cases = [
        ScopeCase {
            case: "a policy that scales in, beside its twin that does not",
            doomed: scaling_in_policy,
            doomed_alarms: 2,
            twin: scale_in_disabled_policy,
            twin_alarms: 1,
            named: Named::Name,
        },
        ScopeCase {
            case: "a policy with scaling in disabled, named by its ARN, beside its twin",
            doomed: scale_in_disabled_policy,
            doomed_alarms: 1,
            twin: scaling_in_policy,
            twin_alarms: 2,
            named: Named::Arn,
        },
        ScopeCase {
            case: "a step policy, which holds no alarm of its own",
            doomed: step_policy,
            doomed_alarms: 0,
            twin: scaling_in_policy,
            twin_alarms: 2,
            named: Named::Name,
        },
        ScopeCase {
            case: "a simple policy, beside a twin with scaling in disabled",
            doomed: simple_policy,
            doomed_alarms: 0,
            twin: scale_in_disabled_policy,
            twin_alarms: 1,
            named: Named::Arn,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let label = case.case;
        let router = router();
        let group = format!("scoped-{index}");
        create_group(&router, &group).await;

        let (doomed_arn, doomed_alarms) =
            put_policy(&router, &group, "doomed", &(case.doomed)()).await;
        assert_eq!(
            doomed_alarms.len(),
            case.doomed_alarms,
            "case {label:?}: expected the deleted policy to hold {} alarm(s), got \
             {doomed_alarms:?}",
            case.doomed_alarms
        );
        let (_, twin_alarms) = put_policy(&router, &group, "twin", &(case.twin)()).await;
        assert_eq!(
            twin_alarms.len(),
            case.twin_alarms,
            "case {label:?}: expected the surviving policy to hold {} alarm(s), got \
             {twin_alarms:?}",
            case.twin_alarms
        );
        let mut expected_before: Vec<String> = doomed_alarms
            .iter()
            .chain(twin_alarms.iter())
            .cloned()
            .collect();
        expected_before.sort();
        let held = group_alarm_names(&router, label, &group).await;
        assert_eq!(
            held, expected_before,
            "case {label:?}: CloudWatch must hold both policies' alarms before the delete, got \
             {held:?}"
        );

        let params = match case.named {
            Named::Name => vec![
                p("AutoScalingGroupName", group.clone()),
                p("PolicyName", "doomed"),
            ],
            Named::Arn => vec![
                p("AutoScalingGroupName", group.clone()),
                p("PolicyName", doomed_arn.clone()),
            ],
        };
        let (status, body) = query(&router, "DeletePolicy", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {label:?}: DeletePolicy expected 200, got {status}: {body}"
        );

        let remaining = reported_policies(&router, &group).await;
        assert_eq!(
            remaining,
            vec!["twin".to_string()],
            "case {label:?}: expected the deleted policy gone and the sibling kept, got \
             {remaining:?}"
        );
        let retired = resolved_metric_alarms(&router, label, &doomed_alarms).await;
        assert!(
            retired.is_empty(),
            "case {label:?}: expected the deleted policy's alarms {doomed_alarms:?} retired with \
             it, got {retired:?}"
        );
        let kept = resolved_metric_alarms(&router, label, &twin_alarms).await;
        assert_eq!(
            kept, twin_alarms,
            "case {label:?}: the sibling policy's alarms must be untouched, got {kept:?}"
        );
        let held = group_alarm_names(&router, label, &group).await;
        assert_eq!(
            held, twin_alarms,
            "case {label:?}: the group must hold exactly the sibling's alarms afterwards, got \
             {held:?}"
        );
    }
}

/// Which kind of alarm a client created over a policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AlarmKind {
    /// A metric alarm, written by `PutMetricAlarm`.
    Metric,
    /// An alarm over other alarms' states, written by `PutCompositeAlarm`.
    Composite,
}

impl AlarmKind {
    /// The operation that writes an alarm of this kind, and the field a read reports it in.
    fn writer(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::Metric => ("PutMetricAlarm", "MetricAlarm", "MetricAlarms"),
            Self::Composite => ("PutCompositeAlarm", "CompositeAlarm", "CompositeAlarms"),
        }
    }
}

/// One row of rule 2: the alarm a client created, which of its action lists names the
/// policies, and the type of the policy deleted.
struct ActionCase {
    case: &'static str,
    kind: AlarmKind,
    /// The action list, as `PutMetricAlarm` and `PutCompositeAlarm` both spell it.
    list: &'static str,
    doomed: fn() -> Vec<(String, String)>,
}

/// Every action list an alarm carries, as the alarm writers and the alarm readers spell them.
const ACTION_LISTS: [&str; 3] = ["AlarmActions", "OKActions", "InsufficientDataActions"];

/// RULE 2: the alarm a client created outlives the policy it acted through, without the
/// deleted policy among its actions and with every other action it held still there.
#[tokio::test]
async fn an_alarm_a_client_created_outlives_the_policy_it_acted_through() {
    let cases = [
        ActionCase {
            case: "a step policy fired by a metric alarm's ALARM actions",
            kind: AlarmKind::Metric,
            list: "AlarmActions",
            doomed: step_policy,
        },
        ActionCase {
            case: "a simple policy fired by a metric alarm's OK actions",
            kind: AlarmKind::Metric,
            list: "OKActions",
            doomed: simple_policy,
        },
        ActionCase {
            case: "a target-tracking policy fired by a metric alarm's insufficient-data actions",
            kind: AlarmKind::Metric,
            list: "InsufficientDataActions",
            doomed: scaling_in_policy,
        },
        ActionCase {
            case: "a step policy fired by a composite alarm's ALARM actions",
            kind: AlarmKind::Composite,
            list: "AlarmActions",
            doomed: step_policy,
        },
    ];

    for (index, case) in cases.iter().enumerate() {
        let label = case.case;
        let router = router();
        let group = format!("acting-{index}");
        create_group(&router, &group).await;

        let (doomed_arn, doomed_alarms) =
            put_policy(&router, &group, "doomed", &(case.doomed)()).await;
        // The policy the SAME action list also names, which the delete must leave alone.
        let (kept_arn, _) = put_policy(&router, &group, "kept", &simple_policy()).await;

        let (writer, kind, field) = case.kind.writer();
        let alarm_name = format!("client-watch-{index}");
        let mut request = match case.kind {
            AlarmKind::Metric => json!({
                "AlarmName": alarm_name,
                "Namespace": "Acme/Fleet",
                "MetricName": "Backlog",
                "Statistic": "Average",
                "Period": 60,
                "EvaluationPeriods": 1,
                "Threshold": 10.0,
                "ComparisonOperator": "GreaterThanThreshold",
            }),
            // "TRUE" is a rule that needs no child alarm to be evaluated
            // (`PutCompositeAlarmInput$AlarmRule`).
            AlarmKind::Composite => json!({
                "AlarmName": alarm_name,
                "AlarmRule": "TRUE",
            }),
        };
        request[case.list] = json!([doomed_arn, kept_arn]);
        cloudwatch(&router, label, writer, request).await;

        let read = json!({"AlarmNames": [alarm_name.clone()], "AlarmTypes": [kind]});
        let before = described(&router, label, read.clone(), field).await;
        assert_eq!(
            before.len(),
            1,
            "case {label:?}: CloudWatch must hold the alarm the client created, got {before:?}"
        );
        assert_eq!(
            list_member(&before[0], case.list),
            vec![doomed_arn.clone(), kept_arn.clone()],
            "case {label:?}: the alarm must act through both policies before the delete, got \
             {before:?}"
        );

        let params = vec![
            p("AutoScalingGroupName", group.clone()),
            p("PolicyName", "doomed"),
        ];
        let (status, body) = query(&router, "DeletePolicy", &borrowed(&params)).await;
        assert_eq!(
            status, 200,
            "case {label:?}: DeletePolicy expected 200, got {status}: {body}"
        );

        let after = described(&router, label, read, field).await;
        assert_eq!(
            after.len(),
            1,
            "case {label:?}: the alarm the client created must still exist, got {after:?}"
        );
        assert_eq!(
            string_member(&after[0], "AlarmName"),
            alarm_name,
            "case {label:?}: the alarm must be the same one, got {after:?}"
        );
        for list in ACTION_LISTS {
            let expected = if list == case.list {
                vec![kept_arn.clone()]
            } else {
                Vec::new()
            };
            assert_eq!(
                list_member(&after[0], list),
                expected,
                "case {label:?}: expected {list} to be {expected:?} after the delete, got {:?}",
                list_member(&after[0], list)
            );
        }
        // The alarms the deleted policy itself HELD are a different thing from an action: they
        // go with it.
        let retired = resolved_metric_alarms(&router, label, &doomed_alarms).await;
        assert!(
            retired.is_empty(),
            "case {label:?}: expected the deleted policy's own alarms {doomed_alarms:?} retired, \
             got {retired:?}"
        );
        // And the policy the alarm still names is still there to be acted through.
        let remaining = reported_policies(&router, &group).await;
        assert_eq!(
            remaining,
            vec!["kept".to_string()],
            "case {label:?}: expected only the surviving policy, got {remaining:?}"
        );
    }
}
