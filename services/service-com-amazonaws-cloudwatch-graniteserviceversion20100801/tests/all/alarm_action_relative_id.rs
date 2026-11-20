// SPDX-License-Identifier: CC0-1.0

//! An alarm action whose vendor is one whose actions CloudWatch CARRIES OUT itself names an
//! operation out of that vendor's own fixed set, so an ARN of such a vendor whose relative id
//! names none of them names nothing that could ever run: `PutMetricAlarm` refuses it with HTTP
//! 400 under the request-validation class, naming the relative id the request sent and the set
//! the vendor publishes, and stores nothing.
//!
//! The two vendors are the ones the documented action values write the four EC2 instance
//! actions under — "EC2 actions: arn:aws:automate:region:ec2:stop …
//! arn:aws:swf:region:account-id:action/actions/AWS_EC2.InstanceId.Stop/1.0 …"
//! (`smithy_query(query="doc", shape="PutMetricAlarmInput", member="AlarmActions")`) — and the
//! rows span what the rule generalises over rather than repeating one request: which vendor,
//! how the relative id misses the set (an unknown verb, a verb of another namespace, a
//! truncated or re-versioned Simple Workflow resource, an empty relative id), which of the
//! three action-list members carries the action, the Region the alarm lives in, whether the
//! alarm's own metric is dimensioned by `InstanceId` at all, and whether valid actions sit
//! beside the bad one.
//!
//! The accepted table is the other half: every one of the four actions, in both forms and in
//! every action-list member, is stored and read back verbatim through `DescribeAlarms`, as is
//! an action of a vendor this rule does not judge whose resource merely READS like one of
//! these.

use crate::harness::{build_router, call, call_in_region, error_code};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The account the Simple Workflow form of an action names.
const ACCOUNT: &str = "000000000000";

/// The Region the rows are signed for unless they say otherwise.
const REGION: &str = "us-east-1";

/// The three action-list members every alarm writer carries; each one's documentation lists the
/// same action values.
const ACTION_MEMBERS: [&str; 3] = ["AlarmActions", "OKActions", "InsufficientDataActions"];

/// The vendor of the `automate` form, as the documented action values write it.
const AUTOMATE: &str = "automate";

/// The vendor of the Simple Workflow form.
const SWF: &str = "swf";

/// The relative ids a refusal lists for the `automate` form, in the order it lists them: the
/// four EC2 instance actions as the documented action values write them, with EC2's namespace
/// ahead of the verb ("arn:aws:automate:region:ec2:stop … :terminate … :reboot … :recover",
/// `PutMetricAlarmInput$AlarmActions`).
const AUTOMATE_RELATIVE_IDS: [&str; 4] =
    ["ec2:stop", "ec2:terminate", "ec2:recover", "ec2:reboot"];

/// The same four under the Simple Workflow form:
/// "arn:aws:swf:region:account-id:action/actions/AWS_EC2.InstanceId.Stop/1.0 … .Terminate/1.0
/// … .Reboot/1.0 … .Recover/1.0" (`PutMetricAlarmInput$AlarmActions`).
const SWF_RELATIVE_IDS: [&str; 4] = [
    "action/actions/AWS_EC2.InstanceId.Stop/1.0",
    "action/actions/AWS_EC2.InstanceId.Terminate/1.0",
    "action/actions/AWS_EC2.InstanceId.Recover/1.0",
    "action/actions/AWS_EC2.InstanceId.Reboot/1.0",
];

/// The refusal CloudWatch answers for a relative id outside `vendor`'s set, built from the
/// request: the vendor the ARN named, the relative id it carried, and the set that vendor
/// publishes.
fn refusal(vendor: &str, relative_id: &str) -> String {
    let (verb, allowed) = match vendor {
        AUTOMATE => ("cannot", AUTOMATE_RELATIVE_IDS.join(", ")),
        SWF => ("can not", SWF_RELATIVE_IDS.join(", ")),
        other => panic!("{other:?} is not a vendor whose actions CloudWatch carries out"),
    };
    format!(
        "The vendor \"{vendor}\" {verb} be used with relative id {relative_id}. Allowed \
         relative IDs are {allowed}"
    )
}

/// One action ARN the rule refuses: how it misses the set, the ARN itself, and the vendor and
/// relative id the refusal names.
struct BadAction {
    /// What is wrong with it, for the assertions to name.
    case: &'static str,
    /// The action ARN, with its Region segment filled in.
    arn: String,
    /// The vendor the refusal names.
    vendor: &'static str,
    /// The relative id the refusal names.
    relative_id: String,
}

/// The ways a relative id of these two vendors misses the set, each in `region`.
///
/// Rows differ in the vendor, in WHICH part of the relative id is wrong (the verb, the
/// namespace the `automate` form carries, the Simple Workflow prefix, its version suffix) and
/// in how many segments it carries.
fn bad_actions(region: &str) -> Vec<BadAction> {
    // The `automate` form carries EC2's namespace where an ARN usually carries an account
    // ("arn:aws:automate:region:ec2:stop"), so the ARN is written from the namespace and the
    // verb while the relative id a refusal names is the resource field alone — everything the
    // ARN names after its account.
    let automate = |case: &'static str, namespace: &str, relative_id: &str| BadAction {
        case,
        arn: format!("arn:aws:{AUTOMATE}:{region}:{namespace}:{relative_id}"),
        vendor: AUTOMATE,
        relative_id: relative_id.to_string(),
    };
    let swf = |case: &'static str, relative_id: &str| BadAction {
        case,
        arn: format!("arn:aws:{SWF}:{region}:{ACCOUNT}:{relative_id}"),
        vendor: SWF,
        relative_id: relative_id.to_string(),
    };
    vec![
        automate("an automate verb that is no EC2 instance action", "ec2", "pause"),
        automate("an automate verb that is an EC2 operation of its own", "ec2", "start"),
        automate("an automate namespace that is not EC2", "lambda", "invoke"),
        automate("an automate verb with a suffix of its own", "ec2", "stopinstance"),
        automate("an automate relative id with a trailing segment", "ec2", "stop:now"),
        swf(
            "a Simple Workflow verb that is no EC2 instance action",
            "action/actions/AWS_EC2.InstanceId.Pause/1.0",
        ),
        swf(
            "a Simple Workflow action of another service",
            "action/actions/AWS_SNS.Publish/1.0",
        ),
        swf(
            "a Simple Workflow resource at another version",
            "action/actions/AWS_EC2.InstanceId.Stop/2.0",
        ),
        swf(
            "a Simple Workflow resource with no version",
            "action/actions/AWS_EC2.InstanceId.Stop",
        ),
        swf("a Simple Workflow resource outside the action tree", "domain/payments"),
    ]
}

/// Every valid EC2 instance action, in both documented forms, in `region`.
fn good_actions(region: &str) -> Vec<String> {
    AUTOMATE_RELATIVE_IDS
        .iter()
        .map(|namespace_and_verb| format!("arn:aws:{AUTOMATE}:{region}:{namespace_and_verb}"))
        .chain(
            SWF_RELATIVE_IDS
                .iter()
                .map(|relative_id| format!("arn:aws:{SWF}:{region}:{ACCOUNT}:{relative_id}")),
        )
        .collect()
}

/// A `PutMetricAlarm` request for `name` over an EC2 metric, with `members` merged over it.
fn put_request(name: &str, members: Value) -> Value {
    merged(
        json!({
            "AlarmName": name,
            "Namespace": "AWS/EC2",
            "MetricName": "CPUUtilization",
            "Statistic": "Average",
            "Period": 300,
            "EvaluationPeriods": 1,
            "Threshold": 10.0,
            "ComparisonOperator": "LessThanThreshold",
        }),
        members,
    )
}

/// The same request for an alarm configured with a `Metrics` QUERY ARRAY instead of one named
/// metric ("Use the Metrics parameter instead of Namespace, MetricName, Dimensions, Period,
/// Unit, Statistic, or ExtendedStatistic." — `PutMetricAlarmInput$Metrics`), over a
/// per-instance series.
fn query_array_request(name: &str, members: Value) -> Value {
    merged(
        json!({
            "AlarmName": name,
            "EvaluationPeriods": 1,
            "Threshold": 10.0,
            "ComparisonOperator": "LessThanThreshold",
            "Metrics": [{
                "Id": "m1",
                "ReturnData": true,
                "MetricStat": {
                    "Metric": {
                        "Namespace": "AWS/EC2",
                        "MetricName": "CPUUtilization",
                        "Dimensions": instance_dimensions(),
                    },
                    "Period": 300,
                    "Stat": "Average",
                },
            }],
        }),
        members,
    )
}

/// The members of `extra` merged over the JSON object `request`.
fn merged(mut request: Value, extra: Value) -> Value {
    let (Value::Object(target), Value::Object(source)) = (&mut request, extra) else {
        panic!("both the request and the overrides are JSON objects");
    };
    for (member, value) in source {
        target.insert(member, value);
    }
    request
}

/// The `InstanceId` dimension an EC2 instance action's own precondition asks for.
fn instance_dimensions() -> Value {
    json!([{"Name": "InstanceId", "Value": "i-feedfacefeedfacef"}])
}

/// The metric alarms `DescribeAlarms` reports for exactly `name`, as seen from `region`.
async fn described(router: &MultiServiceRouter, region: &str, name: &str) -> Vec<Value> {
    let (status, body) =
        call_in_region(router, region, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
    assert_eq!(
        status, 200,
        "DescribeAlarms {name}: expected HTTP 200, got {status} with body {body}"
    );
    body.get("MetricAlarms")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

/// Drive `PutMetricAlarm` in `region` with `request` and require the vendor refusal for
/// `vendor` and `relative_id`, with no alarm of `name` left behind.
async fn assert_refused(
    router: &MultiServiceRouter,
    region: &str,
    case: &str,
    name: &str,
    request: &Value,
    vendor: &str,
    relative_id: &str,
) {
    let (status, body) = call_in_region(router, region, "PutMetricAlarm", request).await;
    assert_eq!(
        status, 400,
        "case {case:?}: expected HTTP 400, got {status} with body {body}"
    );
    assert_eq!(
        error_code(&body),
        "ValidationException",
        "case {case:?}: expected the request-validation class, got {body}"
    );
    assert_eq!(
        body.get("message").and_then(Value::as_str),
        Some(refusal(vendor, relative_id).as_str()),
        "case {case:?}: expected the vendor refusal, got {body}"
    );
    let alarms = described(router, region, name).await;
    assert!(
        alarms.is_empty(),
        "case {case:?}: a refused request must store no alarm, got {alarms:?}"
    );
}

#[tokio::test]
async fn an_action_relative_id_outside_its_vendors_set_is_refused_and_stores_nothing() {
    let router = build_router();

    let mut index = 0usize;
    for member in ACTION_MEMBERS {
        for bad in bad_actions(REGION) {
            let name = format!("refused-{index}");
            index += 1;
            let case = format!("{} in {member}", bad.case);
            let request = put_request(
                &name,
                json!({"Dimensions": instance_dimensions(), member: [bad.arn]}),
            );
            assert_refused(
                &router,
                REGION,
                &case,
                &name,
                &request,
                bad.vendor,
                &bad.relative_id,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn the_relative_id_is_judged_whatever_the_alarm_watches() {
    let router = build_router();

    // The relative id is read off the ARN alone, so what the alarm watches does not decide it:
    // the rows run the same bad action on a metric that names an instance, on one that does
    // not, on one dimensioned by something else, and on a `Metrics` query array. The second
    // and third shapes could not carry an EC2 instance action at all, so the relative id is
    // what is reported for them too.
    type Shape = fn(&str, Value) -> Value;
    let watched: [(&str, Shape, Value); 4] = [
        (
            "a metric dimensioned by InstanceId",
            put_request,
            json!({"Dimensions": instance_dimensions()}),
        ),
        ("a metric with no dimensions at all", put_request, json!({})),
        (
            "a metric dimensioned by something else",
            put_request,
            json!({"Dimensions": [{"Name": "AutoScalingGroupName", "Value": "web"}]}),
        ),
        ("a Metrics query array", query_array_request, json!({})),
    ];

    let mut index = 0usize;
    for (shape, build, members) in watched {
        for bad in bad_actions(REGION) {
            let name = format!("watched-{index}");
            index += 1;
            let case = format!("{} on {shape}", bad.case);
            let request = build(
                &name,
                merged(members.clone(), json!({"AlarmActions": [bad.arn]})),
            );
            assert_refused(
                &router,
                REGION,
                &case,
                &name,
                &request,
                bad.vendor,
                &bad.relative_id,
            )
            .await;
        }
    }
}

#[tokio::test]
async fn one_bad_relative_id_refuses_the_whole_request_wherever_it_sits() {
    let router = build_router();

    // Rows differ in where the bad action sits among valid ones and in which Region the alarm
    // lives in: one action outside its vendor's set refuses the request whatever surrounds it.
    for (index, region) in ["us-east-1", "eu-central-1"].iter().enumerate() {
        let valid = good_actions(region);
        for (row, bad) in bad_actions(region).into_iter().enumerate() {
            let arrangements = [
                ("the bad action first", vec![bad.arn.clone(), valid[0].clone()]),
                ("the bad action last", vec![valid[4].clone(), bad.arn.clone()]),
                (
                    "the bad action between two valid ones",
                    vec![valid[0].clone(), bad.arn.clone(), valid[7].clone()],
                ),
            ];
            for (slot, (arrangement, actions)) in arrangements.into_iter().enumerate() {
                let name = format!("beside-{index}-{row}-{slot}");
                let case = format!("{} with {arrangement} in {region}", bad.case);
                let request = put_request(
                    &name,
                    json!({"Dimensions": instance_dimensions(), "AlarmActions": actions}),
                );
                assert_refused(
                    &router,
                    region,
                    &case,
                    &name,
                    &request,
                    bad.vendor,
                    &bad.relative_id,
                )
                .await;
            }
        }
    }
}

#[tokio::test]
async fn a_refused_update_leaves_the_stored_actions() {
    let router = build_router();

    let valid = good_actions(REGION);
    for (index, bad) in bad_actions(REGION).into_iter().enumerate() {
        let name = format!("configured-{index}");
        let created = put_request(
            &name,
            json!({"Dimensions": instance_dimensions(), "AlarmActions": [valid[0].clone()]}),
        );
        let (status, body) = call(&router, "PutMetricAlarm", &created).await;
        assert_eq!(
            status, 200,
            "the configured alarm {name}: expected HTTP 200, got {status} with body {body}"
        );

        let update = put_request(
            &name,
            json!({"Dimensions": instance_dimensions(), "AlarmActions": [bad.arn.clone()]}),
        );
        let case = format!("an update to {} carrying {}", name, bad.case);
        let (update_status, update_body) = call(&router, "PutMetricAlarm", &update).await;
        assert_eq!(
            update_status, 400,
            "case {case:?}: expected HTTP 400, got {update_status} with body {update_body}"
        );
        assert_eq!(
            update_body.get("message").and_then(Value::as_str),
            Some(refusal(bad.vendor, &bad.relative_id).as_str()),
            "case {case:?}: expected the vendor refusal, got {update_body}"
        );
        let alarms = described(&router, REGION, &name).await;
        assert_eq!(
            alarms.len(),
            1,
            "case {case:?}: the configured alarm must still be there, got {alarms:?}"
        );
        assert_eq!(
            alarms[0].get("AlarmActions"),
            Some(&json!([valid[0].clone()])),
            "case {case:?}: the refused update must leave the stored actions, got {}",
            alarms[0]
        );
    }
}

#[tokio::test]
async fn an_action_of_the_published_set_is_stored_and_read_back() {
    let router = build_router();

    let mut index = 0usize;
    for region in ["us-east-1", "eu-central-1"] {
        for member in ACTION_MEMBERS {
            for arn in good_actions(region) {
                let name = format!("accepted-{index}");
                index += 1;
                let case = format!("{arn} in {member} on an alarm in {region}");
                let request = put_request(
                    &name,
                    json!({"Dimensions": instance_dimensions(), member: [arn.clone()]}),
                );
                let (status, body) =
                    call_in_region(&router, region, "PutMetricAlarm", &request).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} with body {body}"
                );
                let alarms = described(&router, region, &name).await;
                assert_eq!(
                    alarms.len(),
                    1,
                    "case {case:?}: expected exactly the one alarm, got {alarms:?}"
                );
                assert_eq!(
                    alarms[0].get(member),
                    Some(&json!([arn])),
                    "case {case:?}: expected the action to round-trip verbatim, got {}",
                    alarms[0]
                );
            }
        }
    }
}

#[tokio::test]
async fn an_action_of_another_vendor_is_not_judged_by_this_rule() {
    let router = build_router();

    // A vendor whose action is a message to a target names a RESOURCE the caller created, so
    // its name is the caller's: a resource that reads like one of the fixed-set relative ids
    // is still just a resource name.
    let actions = [
        ("an SNS topic", format!("arn:aws:sns:{REGION}:{ACCOUNT}:ec2-stop")),
        (
            "a Lambda function named after an action",
            format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:action-actions-AWS_EC2.InstanceId.Pause"),
        ),
        (
            "an Auto Scaling policy",
            format!(
                "arn:aws:autoscaling:{REGION}:{ACCOUNT}:scalingPolicy:\
                 7bb0e3a5-0000-4000-8000-000000000000:autoScalingGroupName/web:policyName/out"
            ),
        ),
        (
            "an SSM OpsItem",
            format!("arn:aws:ssm:{REGION}:{ACCOUNT}:opsitem:3#CATEGORY=Availability"),
        ),
        (
            "an incident response plan that names no Region",
            format!("arn:aws:ssm-incidents::{ACCOUNT}:responseplan/web-outage"),
        ),
    ];

    for (index, (form, arn)) in actions.iter().enumerate() {
        let name = format!("other-vendor-{index}");
        let case = format!("{form} on an alarm in {REGION}");
        let request = put_request(
            &name,
            json!({"Dimensions": instance_dimensions(), "AlarmActions": [arn]}),
        );
        let (status, body) = call(&router, "PutMetricAlarm", &request).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} with body {body}"
        );
        let alarms = described(&router, REGION, &name).await;
        assert_eq!(
            alarms.len(),
            1,
            "case {case:?}: expected exactly the one alarm, got {alarms:?}"
        );
        assert_eq!(
            alarms[0].get("AlarmActions"),
            Some(&json!([arn])),
            "case {case:?}: expected the action to round-trip verbatim, got {}",
            alarms[0]
        );
    }
}
