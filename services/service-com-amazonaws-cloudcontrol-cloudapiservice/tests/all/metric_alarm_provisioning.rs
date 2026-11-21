// SPDX-License-Identifier: CC0-1.0

//! Cloud Control API over the metric alarms Amazon CloudWatch holds.
//!
//! The rule every case here holds the service to: **a resource of a type another service of
//! this emulator owns is provisioned, resolved, patched and removed in THAT service's own
//! state, so a `CreateResource` that reaches `SUCCESS` has created the resource that
//! service's own readers report — and every property of the model travels to it as the
//! caller wrote it.**
//!
//! The rows differ along what the rule generalises over: the magnitude and precision of the
//! `Threshold` a desired state carries (a fraction no `f32` holds, a very small value, a
//! large one with a fractional part, a negative one, a whole number), which API the alarm
//! came into being through, and which verdict the owning service returns for work it will
//! not do — including a patch of the one property of the type this handler does not apply,
//! which is refused rather than accepted and dropped.

use serde_json::{json, Value};

use super::emulator::{progress, text, Emulator, STATUS_FAILED};

/// The type whose resources live in the Amazon CloudWatch service's own store.
const ALARM: &str = "AWS::CloudWatch::Alarm";

/// One desired state for an alarm over a metric, with `threshold` as its `Threshold`.
///
/// Everything beside the threshold is the configuration a metric alarm needs:
/// `ComparisonOperator` and `EvaluationPeriods` are required of one, and the metric it
/// watches is named by `Namespace`, `MetricName`, `Statistic` and `Period`.
fn alarm(name: &str, threshold: Value) -> Value {
    json!({
        "AlarmName": name,
        "Namespace": "AWS/Billing",
        "MetricName": "EstimatedCharges",
        "Statistic": "Maximum",
        "Period": 300,
        "EvaluationPeriods": 1,
        "ComparisonOperator": "GreaterThanThreshold",
        "Threshold": threshold,
    })
}

/// The alarms the OWNING service reports under `name`, as its own `DescribeAlarms` answers.
async fn native_alarms(emulator: &Emulator, name: &str) -> Vec<Value> {
    let described = emulator
        .cloudwatch_ok("DescribeAlarms", json!({ "AlarmNames": [name] }))
        .await;
    described["MetricAlarms"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

#[tokio::test]
async fn a_created_alarm_is_the_alarm_amazon_cloudwatch_holds() {
    let emulator = Emulator::with_cloudwatch();

    // (case, alarm name, the threshold the desired state carries)
    let rows: [(&str, &str, Value); 5] = [
        (
            "a fraction with more digits than a 32-bit float holds",
            "precise-alarm",
            json!(0.123_456_789_012_345),
        ),
        ("a very small value", "tiny-alarm", json!(0.000_000_1)),
        (
            "a large value with a fractional part",
            "large-alarm",
            json!(123_456_789.123_456),
        ),
        ("a negative value", "negative-alarm", json!(-42.5)),
        ("a whole number", "whole-alarm", json!(7)),
    ];

    // Started together, then waited for: the creates are independent, so one span covers
    // them all.
    let mut accepted = Vec::new();
    for (_, name, threshold) in &rows {
        let event = emulator
            .create_ok(ALARM, &alarm(name, threshold.clone()), None)
            .await;
        accepted.push(text(&event, "RequestToken"));
    }
    for ((case, name, threshold), request_token) in rows.iter().zip(&accepted) {
        let settled = emulator.settled(request_token).await;
        assert_eq!(
            &text(&settled, "Identifier"),
            name,
            "case {case:?}: the create must report the alarm's own name"
        );
        // The alarm EXISTS in the owning service: exactly one alarm of that name, and the
        // threshold it holds is the number the desired state carried, digit for digit.
        let held = native_alarms(&emulator, name).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected exactly one native alarm named {name:?}, got {held:?}"
        );
        assert_eq!(
            held[0]["Threshold"].as_f64(),
            threshold.as_f64(),
            "case {case:?}: expected the native alarm's threshold to be {threshold:?}, got \
             {:?}",
            held[0]["Threshold"]
        );
        for (member, expected) in [
            ("Namespace", "AWS/Billing"),
            ("MetricName", "EstimatedCharges"),
            ("Statistic", "Maximum"),
            ("ComparisonOperator", "GreaterThanThreshold"),
        ] {
            assert_eq!(
                held[0][member].as_str(),
                Some(expected),
                "case {case:?}: expected the native alarm's {member} to be {expected:?}, \
                 got {:?}",
                held[0][member]
            );
        }
        // And the model this service reports is the alarm's FULL current model: the
        // threshold with the same precision, and the read-only ARN the owning service
        // computed.
        let properties = emulator.properties(ALARM, name).await;
        assert_eq!(
            properties["Threshold"].as_f64(),
            threshold.as_f64(),
            "case {case:?}: expected the reported model to carry the threshold, got \
             {properties:?}"
        );
        assert_eq!(
            properties["Arn"].as_str(),
            held[0]["AlarmArn"].as_str(),
            "case {case:?}: expected the model to carry the ARN the owning service holds, \
             got {properties:?}"
        );
    }

    // The listing reports them all, under the names the owning service holds them by.
    let mut listed = emulator.walk(ALARM, None).await;
    listed.sort();
    let mut expected: Vec<String> = rows.iter().map(|(_, name, _)| (*name).to_string()).collect();
    expected.sort();
    assert_eq!(
        listed, expected,
        "expected the listing to report every alarm, got {listed:?}"
    );
}

#[tokio::test]
async fn a_patch_and_a_delete_act_on_the_alarm_amazon_cloudwatch_holds() {
    let emulator = Emulator::with_cloudwatch();
    let name = "patched-alarm";
    emulator.provision(ALARM, &alarm(name, json!(10.5))).await;

    // A patch through this service is a write to THAT store.
    let patch = json!([
        { "op": "replace", "path": "/Threshold", "value": 99.0625 },
        { "op": "add", "path": "/AlarmDescription", "value": "watch the bill" },
    ]);
    emulator.update_and_settle(ALARM, name, &patch).await;
    let held = native_alarms(&emulator, name).await;
    assert_eq!(
        held.len(),
        1,
        "expected the patched alarm to still be the one alarm of its name, got {held:?}"
    );
    assert_eq!(
        held[0]["Threshold"].as_f64(),
        Some(99.0625),
        "expected the patch to reach the owning service, got {:?}",
        held[0]["Threshold"]
    );
    assert_eq!(
        held[0]["AlarmDescription"].as_str(),
        Some("watch the bill"),
        "expected the patched description to reach the owning service, got {:?}",
        held[0]["AlarmDescription"]
    );

    // And a delete removes it from that store, after which nothing resolves here either.
    emulator.delete_and_settle(ALARM, name).await;
    let held = native_alarms(&emulator, name).await;
    assert!(
        held.is_empty(),
        "Amazon CloudWatch must no longer hold an alarm Cloud Control API deleted, got \
         {held:?}"
    );
    let (status, body) = emulator.get(ALARM, name).await;
    assert_eq!(
        status, 400,
        "the deleted alarm must resolve to nothing, got {status} with body {body:?}"
    );
}

#[tokio::test]
async fn an_alarm_the_owning_api_created_is_the_same_resource() {
    let emulator = Emulator::with_cloudwatch();
    let name = "native-alarm";
    // Created through the OWNING service's own API — nothing of it is ever named to Cloud
    // Control API.
    emulator
        .cloudwatch_ok(
            "PutMetricAlarm",
            json!({
                "AlarmName": name,
                "Namespace": "AWS/EC2",
                "MetricName": "CPUUtilization",
                "Statistic": "Average",
                "Period": 60,
                "EvaluationPeriods": 2,
                "ComparisonOperator": "GreaterThanOrEqualToThreshold",
                "Threshold": 80.25,
            }),
        )
        .await;

    // Cloud Control API resolves a resource whichever API created it.
    let properties = emulator.properties(ALARM, name).await;
    assert_eq!(
        properties["Threshold"].as_f64(),
        Some(80.25),
        "an alarm the owning API created must resolve, got {properties:?}"
    );
    let listed = emulator.walk(ALARM, None).await;
    assert_eq!(
        listed,
        vec![name.to_string()],
        "an alarm the owning API created must be listed, got {listed:?}"
    );

    // And a create over it is the owning handler's already-exists verdict, with the alarm
    // left exactly as that API wrote it.
    let accepted = emulator
        .create_ok(ALARM, &alarm(name, json!(1.0)), None)
        .await;
    let failed = emulator.terminal(&text(&accepted, "RequestToken")).await;
    assert_eq!(
        text(&failed, "OperationStatus"),
        STATUS_FAILED,
        "a create over an alarm that is already there must fail, got {failed:?}"
    );
    assert_eq!(
        text(&failed, "ErrorCode"),
        "AlreadyExists",
        "unexpected handler error code in {failed:?}"
    );
    let held = native_alarms(&emulator, name).await;
    assert_eq!(
        held[0]["Threshold"].as_f64(),
        Some(80.25),
        "a refused create must leave the alarm untouched, got {held:?}"
    );
}

#[tokio::test]
async fn the_tags_a_desired_state_names_are_the_tags_the_owning_service_reports() {
    let emulator = Emulator::with_cloudwatch();
    let name = "tagged-alarm";
    let mut desired = alarm(name, json!(5.0));
    desired["Tags"] = json!([
        { "Key": "team", "Value": "billing" },
        { "Key": "env", "Value": "test" },
    ]);
    emulator.provision(ALARM, &desired).await;

    // The tags are on the NATIVE resource: the owning service's own tag read reports them,
    // under the ARN it holds the alarm by.
    let held = native_alarms(&emulator, name).await;
    let arn = text(&held[0], "AlarmArn");
    let listed = emulator
        .cloudwatch_ok("ListTagsForResource", json!({ "ResourceARN": arn }))
        .await;
    let mut reported: Vec<(String, String)> = listed["Tags"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|tag| (text(tag, "Key"), text(tag, "Value")))
        .collect();
    reported.sort();
    assert_eq!(
        reported,
        vec![
            ("env".to_string(), "test".to_string()),
            ("team".to_string(), "billing".to_string()),
        ],
        "expected the desired state's tags on the native alarm, got {reported:?}"
    );
}

#[tokio::test]
async fn a_patch_naming_a_property_this_handler_does_not_apply_is_refused() {
    let emulator = Emulator::with_cloudwatch();
    let name = "math-alarm";
    // An alarm the OWNING service created over metric MATH: its `Metrics` are queries that
    // service parses and resolves on its own write path.
    emulator
        .cloudwatch_ok(
            "PutMetricAlarm",
            json!({
                "AlarmName": name,
                "EvaluationPeriods": 1,
                "ComparisonOperator": "GreaterThanThreshold",
                "Threshold": 1.5,
                "Metrics": [{
                    "Id": "e1",
                    "Expression": "m1 * 2",
                    "ReturnData": true,
                }, {
                    "Id": "m1",
                    "ReturnData": false,
                    "MetricStat": {
                        "Metric": { "Namespace": "AWS/EC2", "MetricName": "CPUUtilization" },
                        "Period": 60,
                        "Stat": "Average",
                    },
                }],
            }),
        )
        .await;

    // A patch that names a property this handler does not apply asks for work nothing
    // performs, so the request terminates FAILED carrying the handler's verdict rather than
    // reporting a success that changed nothing.
    let patch = json!([{
        "op": "add",
        "path": "/Metrics",
        "value": [{ "Id": "m1", "Expression": "m1 * 3", "ReturnData": true }],
    }]);
    let (status, body) = emulator.update(ALARM, name, &patch, None).await;
    assert_eq!(
        status, 200,
        "UpdateResource must be accepted, got {status} with body {body:?}"
    );
    let accepted = progress(&body);
    let failed = emulator.terminal(&text(&accepted, "RequestToken")).await;
    assert_eq!(
        text(&failed, "OperationStatus"),
        STATUS_FAILED,
        "a patch of a property this handler does not apply must fail, got {failed:?}"
    );
    assert_eq!(
        text(&failed, "ErrorCode"),
        "InvalidRequest",
        "unexpected handler error code in {failed:?}"
    );
    assert_eq!(
        text(&failed, "StatusMessage"),
        "Metrics is a property of AWS::CloudWatch::Alarm this emulator's handler does not \
         apply",
        "unexpected wording"
    );
    // And the alarm the owning service holds is exactly the one it wrote: its queries and
    // its threshold are untouched.
    let held = native_alarms(&emulator, name).await;
    assert_eq!(
        held[0]["Threshold"].as_f64(),
        Some(1.5),
        "a refused patch must leave the alarm untouched, got {held:?}"
    );
    let queries: Vec<String> = held[0]["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|query| text(query, "Id"))
        .collect();
    assert_eq!(
        queries,
        vec!["e1".to_string(), "m1".to_string()],
        "a refused patch must leave the alarm's own metric queries in place, got {held:?}"
    );

    // A patch that names something else still applies, and the queries stay as they are:
    // what this path does not apply, it also does not replace or drop.
    let patch = json!([{ "op": "replace", "path": "/Threshold", "value": 2.25 }]);
    emulator.update_and_settle(ALARM, name, &patch).await;
    let held = native_alarms(&emulator, name).await;
    assert_eq!(
        held[0]["Threshold"].as_f64(),
        Some(2.25),
        "expected the patch to reach the owning service, got {held:?}"
    );
    let queries: Vec<String> = held[0]["Metrics"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|query| text(query, "Id"))
        .collect();
    assert_eq!(
        queries,
        vec!["e1".to_string(), "m1".to_string()],
        "expected the alarm's own metric queries to survive a patch of another property, \
         got {held:?}"
    );
}

#[tokio::test]
async fn work_the_owning_service_refuses_leaves_the_request_failed() {
    let emulator = Emulator::with_cloudwatch();

    // (case, desired state, the diagnostic the handler reports, the verdict it carries)
    //
    // A value outside a set only the OWNING service publishes is that service's own error,
    // while a property the type's schema declares required and a property it does not declare
    // at all are faults the handler judges against that schema.
    let rows: [(&str, Value, &str, &str); 4] = [
        (
            "a comparison operator outside the set the owning service admits",
            json!({
                "AlarmName": "misoperated-alarm",
                "EvaluationPeriods": 1,
                "ComparisonOperator": "GreaterThanish",
            }),
            "ComparisonOperator must be one of",
            "GeneralServiceException",
        ),
        (
            "a statistic outside the set it admits",
            json!({
                "AlarmName": "misstatted-alarm",
                "EvaluationPeriods": 1,
                "ComparisonOperator": "GreaterThanThreshold",
                "Statistic": "Median",
            }),
            "Statistic must be one of",
            "GeneralServiceException",
        ),
        (
            "no comparison operator at all",
            json!({ "AlarmName": "incomplete-alarm", "EvaluationPeriods": 1 }),
            "ComparisonOperator is required of AWS::CloudWatch::Alarm",
            "InvalidRequest",
        ),
        (
            "a property the type does not declare",
            json!({
                "AlarmName": "unknown-alarm",
                "EvaluationPeriods": 1,
                "ComparisonOperator": "GreaterThanThreshold",
                "Secret": "s3cret",
            }),
            "Secret is not a property of AWS::CloudWatch::Alarm",
            "InvalidRequest",
        ),
    ];
    let mut accepted = Vec::new();
    for (_, desired, _, _) in &rows {
        let event = emulator.create_ok(ALARM, desired, None).await;
        accepted.push(text(&event, "RequestToken"));
    }
    for ((case, desired, detail, expected_code), request_token) in rows.iter().zip(&accepted) {
        let failed = emulator.terminal(request_token).await;
        assert_eq!(
            text(&failed, "OperationStatus"),
            STATUS_FAILED,
            "case {case:?}: expected the request to fail, got {failed:?}"
        );
        assert_eq!(
            text(&failed, "ErrorCode"),
            *expected_code,
            "case {case:?}: unexpected handler error code"
        );
        assert!(
            text(&failed, "StatusMessage").contains(detail),
            "case {case:?}: expected the refusal to state {detail:?}, got {:?}",
            text(&failed, "StatusMessage")
        );
        // And nothing was created: the owning service holds no alarm of that name.
        let name = text(desired, "AlarmName");
        let held = native_alarms(&emulator, &name).await;
        assert!(
            held.is_empty(),
            "case {case:?}: a refused create must leave no alarm behind, got {held:?}"
        );
    }
}
