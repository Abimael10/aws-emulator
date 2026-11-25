// SPDX-License-Identifier: CC0-1.0

//! What a `TargetTrackingScaling` put has to state about the metric it tracks and the value it
//! holds it at, and what a put that states it wrongly leaves behind.
//!
//! The rules under test:
//!
//! 1. A target-tracking configuration names EXACTLY ONE metric to track: "A predefined metric.
//!    You must specify either a predefined metric or a customized metric"
//!    (`TargetTrackingConfiguration$PredefinedMetricSpecification`, and the same sentence on
//!    `$CustomizedMetricSpecification`) — both members are optional in the model, which cannot
//!    express the alternative, so a configuration carrying BOTH (two metrics to track) and one
//!    carrying NEITHER (none) are refused with the one sentence the service answers for that
//!    class.
//! 2. A customized metric that NAMES one metric is graded by a statistic of it
//!    (`CustomizedMetricSpecification$Statistic`, "the statistic of the metric"), while the
//!    alternative form, "the metrics to include in the target tracking scaling policy, as a
//!    metric data query" (`$Metrics`), carries its statistic inside the query
//!    (`TargetTrackingMetricStat$Stat`). The model marks `Statistic` optional for exactly that
//!    reason, so the service's own constraint sentence is what refuses the named form without
//!    one.
//! 3. The value tracked sits inside the window the service holds: `$TargetValue` is required
//!    and targets `MetricScale`, a bare double with NO modelled range
//!    (`smithy_query(query="shape",shape="TargetTrackingConfiguration")`), so the window is the
//!    service's own and both of its bounds are in the sentence it answers outside them.
//! 4. Across all three: a refused put WRITES NOTHING — the group holds no such policy and
//!    CloudWatch holds no alarm for it — while an accepted one mints the policy's alarm pair in
//!    the account's own CloudWatch store, which is where the alarms of a target-tracking policy
//!    live ("The CloudWatch alarms created for the target tracking scaling policy",
//!    `PolicyARNType$Alarms`).
//!
//! The rows differ along what the rules generalise over: which of the three rules the put
//! breaks, the FORM of the break (both specifications or neither; a named metric with
//! dimensions or without; a target value at zero, negative, a hair under the published lower
//! bound, or a hair over the published upper one), and, for the accepted rows, each published
//! bound of the window itself and which specification form names the metric.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_autoscaling_autoscaling_2011_01_01 as autoscaling;
use service_com_amazonaws_cloudwatch_graniteserviceversion20100801 as cloudwatch_service;

use crate::support::{
    borrowed, json_rpc, new_engine, p, query, seed_launch_templates, tag_value, tag_values,
    xml_unescape,
};

/// The launch template every group here is created from — a launch TEMPLATE, because no
/// account can create a launch configuration.
const LAUNCH_SOURCE: (&str, &str) = ("LaunchTemplate.LaunchTemplateName", "lt-target-rules");

/// The signing name, awsJson target prefix and protocol version a CloudWatch client sends.
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_JSON_VERSION: &str = "1.0";

/// The wire prefixes the two metric specification forms are carried under.
const PREDEFINED: &str = "TargetTrackingConfiguration.PredefinedMetricSpecification";
const CUSTOMIZED: &str = "TargetTrackingConfiguration.CustomizedMetricSpecification";

/// The series the customized rows name: a namespace and metric of the caller's own.
const NAMESPACE: &str = "Acme/Queue";
const METRIC: &str = "Backlog";

/// Both bounds of the window the service tracks a value in, spelled as a client sends them and
/// as the refusal sentence states them.
const MIN_TARGET_VALUE: &str = "8.51592E-12";
const MAX_TARGET_VALUE: &str = "1.174271E17";

/// The sentence a configuration that does not name exactly one metric is refused with: fixed
/// text, the same whether the configuration carries both specifications or neither.
const ONE_METRIC_SPECIFICATION: &str =
    "You should provide either a CustomizedMetricSpecification or a PredefinedMetricSpecification";

/// The sentence a target value outside the window is refused with: fixed text naming both
/// bounds of that window.
const TARGET_VALUE_RANGE: &str = "Target value must be between 8.51592E-12 and 1.174271E17";

/// The service's constraint sentence for a member the request left null, naming the member by
/// the wire path the service reports it under.
fn member_must_not_be_null(member_path: &str) -> String {
    format!(
        "1 validation error detected: Value null at '{member_path}' failed to satisfy constraint: \
         Member must not be null"
    )
}

/// The prefix Amazon EC2 Auto Scaling names the alarms of `group`'s policies with.
fn group_alarm_prefix(group: &str) -> String {
    format!("TargetTracking-{group}-")
}

/// A router carrying Amazon EC2 Auto Scaling and CloudWatch on ONE engine: the alarms a
/// target-tracking policy mints are CloudWatch's own resources, so what a put left behind is
/// read back through CloudWatch's own API.
fn router() -> MultiServiceRouter {
    let engine = new_engine();
    seed_launch_templates(&engine, &[LAUNCH_SOURCE.1]);
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
        "the group {name:?} must be created, got {status} with {body:?}"
    );
}

/// POST one `PutScalingPolicy` of type `TargetTrackingScaling` and answer its status and body.
async fn put_policy(
    router: &MultiServiceRouter,
    group: &str,
    policy: &str,
    extra: &[(String, String)],
) -> (u16, String) {
    let mut params = vec![
        p("AutoScalingGroupName", group),
        p("PolicyName", policy),
        p("PolicyType", "TargetTrackingScaling"),
    ];
    params.extend_from_slice(extra);
    query(router, "PutScalingPolicy", &borrowed(&params)).await
}

/// How many alarms CloudWatch's own store holds for every policy of `group`.
async fn alarm_count_of_group(router: &MultiServiceRouter, group: &str) -> usize {
    let (status, answer) = json_rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        CLOUDWATCH_JSON_VERSION,
        "DescribeAlarms",
        json!({"AlarmNamePrefix": group_alarm_prefix(group), "AlarmTypes": ["MetricAlarm"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "DescribeAlarms must succeed, got {status} with {answer}"
    );
    answer
        .pointer("/MetricAlarms")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or_default()
}

/// The names `DescribePolicies` reports for the one policy `policy` of `group` — empty when the
/// group holds no policy of that name.
async fn policy_names(router: &MultiServiceRouter, group: &str, policy: &str) -> Vec<String> {
    let (status, body) = query(
        router,
        "DescribePolicies",
        &[
            ("AutoScalingGroupName", group),
            ("PolicyNames.member.1", policy),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "the read-back of {policy:?} must succeed, got {status} with {body:?}"
    );
    tag_values(&body, "PolicyName")
}

/// What one row expects of its put.
enum Expectation {
    /// The put is refused with this whole sentence, and writes nothing.
    Refused(String),
    /// The put is accepted, and mints the policy's pair of alarms.
    Accepted,
}

/// One row: the configuration a put carries, and what the rules make of it.
struct Case {
    case: &'static str,
    params: Vec<(String, String)>,
    expectation: Expectation,
}

/// The members of a customized specification that NAMES one metric, with `statistic` where the
/// row sends one and the dimensions where it sends them.
fn named_metric(statistic: Option<&str>, dimensions: bool) -> Vec<(String, String)> {
    let mut params = vec![
        p(&format!("{CUSTOMIZED}.Namespace"), NAMESPACE),
        p(&format!("{CUSTOMIZED}.MetricName"), METRIC),
    ];
    if let Some(statistic) = statistic {
        params.push(p(&format!("{CUSTOMIZED}.Statistic"), statistic));
    }
    if dimensions {
        params.push(p(
            &format!("{CUSTOMIZED}.Dimensions.member.1.Name"),
            "QueueName",
        ));
        params.push(p(
            &format!("{CUSTOMIZED}.Dimensions.member.1.Value"),
            "orders",
        ));
    }
    params
}

/// The members of a predefined specification measured over the group itself, which needs no
/// resource label.
fn predefined_metric() -> Vec<(String, String)> {
    vec![p(
        &format!("{PREDEFINED}.PredefinedMetricType"),
        "ASGAverageCPUUtilization",
    )]
}

/// One row's configuration: the metric specification `spec` tracked to `target_value`.
fn tracked(spec: Vec<(String, String)>, target_value: &str) -> Vec<(String, String)> {
    let mut params = spec;
    params.push(p("TargetTrackingConfiguration.TargetValue", target_value));
    params
}

/// Every row of the table: each rule broken in each form it can be broken in, and each rule
/// satisfied — including at both published bounds of the target-value window.
fn cases() -> Vec<Case> {
    let one_metric = || Expectation::Refused(ONE_METRIC_SPECIFICATION.to_string());
    let out_of_range = || Expectation::Refused(TARGET_VALUE_RANGE.to_string());
    vec![
        Case {
            case: "a configuration carrying BOTH metric specifications",
            params: tracked(
                {
                    let mut both = predefined_metric();
                    both.extend(named_metric(Some("Maximum"), false));
                    both
                },
                "50",
            ),
            expectation: one_metric(),
        },
        Case {
            case: "a configuration carrying NEITHER metric specification",
            params: tracked(Vec::new(), "50"),
            expectation: one_metric(),
        },
        Case {
            case: "a named customized metric with dimensions and no statistic",
            params: tracked(named_metric(None, true), "50"),
            expectation: Expectation::Refused(member_must_not_be_null(
                "targetTrackingConfiguration.customizedMetricSpecification.statistic",
            )),
        },
        Case {
            case: "a named customized metric with no dimensions and no statistic",
            params: tracked(named_metric(None, false), "50"),
            expectation: Expectation::Refused(member_must_not_be_null(
                "targetTrackingConfiguration.customizedMetricSpecification.statistic",
            )),
        },
        Case {
            case: "a target value of zero",
            params: tracked(predefined_metric(), "0"),
            expectation: out_of_range(),
        },
        Case {
            case: "a negative target value",
            params: tracked(predefined_metric(), "-5"),
            expectation: out_of_range(),
        },
        Case {
            case: "a target value a hair under the window's lower bound",
            params: tracked(predefined_metric(), "8.51591E-12"),
            expectation: out_of_range(),
        },
        Case {
            case: "a target value a hair over the window's upper bound",
            params: tracked(predefined_metric(), "1.174272E17"),
            expectation: out_of_range(),
        },
        Case {
            case: "a predefined metric tracked to a value inside the window",
            params: tracked(predefined_metric(), "40"),
            expectation: Expectation::Accepted,
        },
        Case {
            case: "a named customized metric graded by a statistic",
            params: tracked(named_metric(Some("Maximum"), true), "50"),
            expectation: Expectation::Accepted,
        },
        Case {
            case: "a target value AT the window's lower bound",
            params: tracked(predefined_metric(), MIN_TARGET_VALUE),
            expectation: Expectation::Accepted,
        },
        Case {
            case: "a target value AT the window's upper bound",
            params: tracked(predefined_metric(), MAX_TARGET_VALUE),
            expectation: Expectation::Accepted,
        },
    ]
}

/// RULES 1-4: each row's put is refused with its class's whole sentence and writes nothing, or
/// accepted and mints its alarm pair in CloudWatch's own store.
#[tokio::test]
async fn a_target_tracking_put_states_one_metric_and_a_value_inside_the_window() {
    let router = router();
    let group = "asg-target-rules";
    create_group(&router, group).await;

    for (index, row) in cases().into_iter().enumerate() {
        let case = row.case;
        let policy_name = format!("tracked-{index}");
        let before = alarm_count_of_group(&router, group).await;
        let (status, body) = put_policy(&router, group, &policy_name, &row.params).await;
        let after = alarm_count_of_group(&router, group).await;
        let reported = policy_names(&router, group, &policy_name).await;

        match row.expectation {
            Expectation::Refused(expected) => {
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected HTTP 400, got {status} with {body:?}"
                );
                let code = tag_value(&body, "Code");
                assert_eq!(
                    code, "ValidationError",
                    "case {case:?}: expected the ValidationError code, got {code:?} with {body:?}"
                );
                // The WHOLE sentence, not a substring: a message that merely contains the
                // right words would satisfy a refusal for another reason.
                let message = xml_unescape(&tag_value(&body, "Message"));
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected the message {expected:?}, got {message:?}"
                );
                assert_eq!(
                    after, before,
                    "case {case:?}: a refused put must leave no alarm behind, the group's alarm \
                     count went from {before} to {after}"
                );
                assert!(
                    reported.is_empty(),
                    "case {case:?}: a refused put must store no policy, got {reported:?}"
                );
            }
            Expectation::Accepted => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} with {body:?}"
                );
                let arn = tag_value(&body, "PolicyARN");
                assert!(
                    arn.contains(&policy_name),
                    "case {case:?}: the answered PolicyARN must name the policy, got {arn:?}"
                );
                let answered = tag_values(&body, "AlarmName");
                assert_eq!(
                    answered.len(),
                    2,
                    "case {case:?}: expected the scale-out and the scale-in alarm, got \
                     {answered:?}"
                );
                assert_eq!(
                    after,
                    before + 2,
                    "case {case:?}: an accepted put must mint its alarm pair, the group's alarm \
                     count went from {before} to {after}"
                );
                assert_eq!(
                    reported,
                    vec![policy_name.clone()],
                    "case {case:?}: an accepted put must store its policy, got {reported:?}"
                );
            }
        }
    }
}

/// RULE 2, the other side of it: the statistic is owed by the form that NAMES one metric and by
/// no other, so the metric-data-query form is accepted with none — its statistic is the `Stat`
/// inside the query.
#[tokio::test]
async fn a_metric_data_query_specification_needs_no_statistic_of_its_own() {
    let router = router();
    let group = "asg-query-form";
    create_group(&router, group).await;

    let query_form = "TargetTrackingConfiguration.CustomizedMetricSpecification.Metrics.member.1";
    let params = vec![
        p(&format!("{query_form}.Id"), "m1"),
        p(
            &format!("{query_form}.MetricStat.Metric.Namespace"),
            NAMESPACE,
        ),
        p(
            &format!("{query_form}.MetricStat.Metric.MetricName"),
            METRIC,
        ),
        p(&format!("{query_form}.MetricStat.Stat"), "Average"),
        p(&format!("{query_form}.ReturnData"), "true"),
        p("TargetTrackingConfiguration.TargetValue", "50"),
    ];

    let case = "a customized specification given as a metric data query";
    let (status, body) = put_policy(&router, group, "query-form", &params).await;
    assert_eq!(
        status, 200,
        "case {case:?}: the query form owes no Statistic of its own, got {status} with {body:?}"
    );
    let reported = policy_names(&router, group, "query-form").await;
    assert_eq!(
        reported,
        vec!["query-form".to_string()],
        "case {case:?}: the accepted put must store its policy, got {reported:?}"
    );

    // The control: the SAME specification given as a NAMED metric, with no statistic, is the
    // one the rule refuses.
    let case = "the same metric named rather than queried";
    let named = tracked(named_metric(None, false), "50");
    let (status, body) = put_policy(&router, group, "named-form", &named).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with {body:?}"
    );
    let message = xml_unescape(&tag_value(&body, "Message"));
    let expected = member_must_not_be_null(
        "targetTrackingConfiguration.customizedMetricSpecification.statistic",
    );
    assert_eq!(
        message, expected,
        "case {case:?}: expected the message {expected:?}, got {message:?}"
    );
    let reported = policy_names(&router, group, "named-form").await;
    assert!(
        reported.is_empty(),
        "case {case:?}: a refused put must store no policy, got {reported:?}"
    );
}
