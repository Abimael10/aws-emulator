// SPDX-License-Identifier: CC0-1.0

//! EXECUTING a target-tracking policy whose scalable target is a `custom-resource` endpoint.
//!
//! RULE: a target-tracking policy is executed by the alarm it minted on the metric it tracks
//! WHATEVER namespace its scalable target is in — when that alarm is in `ALARM` the policy
//! moves the capacity to the least capacity at which the metric would sit at the target value
//! ("Application Auto Scaling […] scales the capacity to keep the metric at, or close to, the
//! specified target value"), clamped into the `MinCapacity`/`MaxCapacity` the target carries,
//! and records that movement as one scaling activity. For a `custom-resource` target the
//! capacity is not held by any AWS service: it is the capacity the caller's own endpoint
//! serves, so the movement is the `PATCH` of `{"desiredCapacity": <N>}` that contract names,
//! and `DisableScaleIn` — "scale in is disabled and the target tracking scaling policy won't
//! remove capacity from the scalable target" — takes the scale-in alarm away while leaving the
//! scale-out one to drive that `PATCH`.
//!
//! The rows differ along what the rule generalises over: whether scale-in is disabled, which of
//! the policy's alarms the published series breaches, the target value, whether the capacity
//! the control loop asks for lands inside the registered window or past its ceiling, and — the
//! negative row — a scale-in-disabled policy whose metric falls BELOW the target, which has no
//! alarm to act on it and so moves nothing at all.
//!
//! The endpoint is the one [`super::custom_resource_endpoint_targets`] stands up: an API
//! Gateway REST API whose `GET` and `PATCH` on the dimension's own path read and write the
//! item one DynamoDB table holds for it, so what this suite observes is the caller's own
//! resource moving and the body it really received.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::custom_resource_endpoint_targets::{
    application_auto_scaling, dynamodb, given_capacity_table, given_endpoint, given_item, router,
    CAPACITY_TABLE, DEPLOYED_STAGE, DIMENSION, DIMENSION_PATH,
};
use super::emulator::REGION;

/// The metric namespace the rows publish their load into. A customer namespace, so every
/// datapoint goes over the wire exactly as a client's does.
const METRIC_NAMESPACE: &str = "Emulator/CustomResourceTracking";

/// The statistic the policies' specifications name, and so the one their alarms aggregate with.
const STATISTIC: &str = "Average";

/// The period, in seconds, the alarms of a CUSTOMISED series watch it on: a customised series
/// is published at the resolution the caller publishes it at, so the alarms watch it on
/// CloudWatch's own standard period.
const SERIES_PERIOD: f64 = 60.0;

/// How many closed period windows a row publishes its load into.
///
/// The scale-in alarm of a tracked metric is evaluated over fifteen periods and the scale-out
/// alarm over three, so publishing this many covers either of them.
const PUBLISHED_WINDOWS: i64 = 18;

/// How long a row keeps looking for what the clocks owe it, and how often it looks.
///
/// Nothing in the emulator sleeps for an execution: each poll is the TEST waiting for
/// CloudWatch's alarm evaluation and the scaling pass that follows it, so a row whose capacity
/// never moves fails on the value it last read.
const POLL_FOR: Duration = Duration::from_secs(30);
/// See [`POLL_FOR`].
const POLL_EVERY: Duration = Duration::from_millis(200);

/// How long the row that must move NOTHING is watched for a movement that may not happen: long
/// enough to span many of the scaling clock's passes and several alarm evaluations.
const QUIET_WATCH: Duration = Duration::from_secs(3);

/// The status a capacity movement whose change took effect settles in.
const SUCCESSFUL: &str = "Successful";

/// Which of a target-tracking policy's own alarms a row's published series breaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Breaches {
    /// `AlarmHigh`: the metric is above the target value.
    High,
    /// `AlarmLow`: the metric is below the buffered target value.
    Low,
    /// Neither: the policy has no alarm the published series breaches.
    Neither,
}

/// One execution to pin.
struct Row {
    case: &'static str,
    /// The capacity the endpoint's own item starts at.
    starting: i64,
    min: i64,
    max: i64,
    /// The policy's `TargetValue`.
    target_value: f64,
    /// The value the published series takes in every graded window.
    measured: f64,
    /// Whether the policy disables scale-in.
    disable_scale_in: bool,
    /// The alarm kinds the policy mints, in the order `PutScalingPolicy` reports them.
    minted: &'static [&'static str],
    breaches: Breaches,
    /// The capacity the endpoint must land on, or `None` for a row that moves nothing.
    moved_to: Option<i64>,
}

const ROWS: &[Row] = &[
    // The recorded case: a scale-in-disabled policy at ten times its target value, whose
    // control loop asks for 1 * 100 / 10 = 10 and is clamped to the registered ceiling.
    Row {
        case: "scale-in disabled, a breach past the registered ceiling",
        starting: 1,
        min: 1,
        max: 4,
        target_value: 10.0,
        measured: 100.0,
        disable_scale_in: true,
        minted: &["AlarmHigh"],
        breaches: Breaches::High,
        moved_to: Some(4),
    },
    // The same rule inside the window: 2 * 100 / 50 = 4.
    Row {
        case: "scale-in disabled, a breach inside the window",
        starting: 2,
        min: 1,
        max: 10,
        target_value: 50.0,
        measured: 100.0,
        disable_scale_in: true,
        minted: &["AlarmHigh"],
        breaches: Breaches::High,
        moved_to: Some(4),
    },
    // Scale-in left enabled mints both alarms, and the high one drives the same scale-out:
    // 2 * 150 / 50 = 6.
    Row {
        case: "scale-in enabled, the high alarm scales out",
        starting: 2,
        min: 1,
        max: 10,
        target_value: 50.0,
        measured: 150.0,
        disable_scale_in: false,
        minted: &["AlarmHigh", "AlarmLow"],
        breaches: Breaches::High,
        moved_to: Some(6),
    },
    // The mirror direction, which only a policy that did not disable scale-in has an alarm
    // for: the metric is below the buffered target (a tenth under 50), and 6 * 20 / 50 = 2.4
    // units, taken up.
    Row {
        case: "scale-in enabled, the low alarm scales in",
        starting: 6,
        min: 1,
        max: 10,
        target_value: 50.0,
        measured: 20.0,
        disable_scale_in: false,
        minted: &["AlarmHigh", "AlarmLow"],
        breaches: Breaches::Low,
        moved_to: Some(3),
    },
    // The negative row: the same metric under the same target with scale-in DISABLED. There is
    // no scale-in alarm to breach, so the endpoint is never asked to give capacity back.
    Row {
        case: "scale-in disabled, a metric below the target removes nothing",
        starting: 6,
        min: 1,
        max: 10,
        target_value: 50.0,
        measured: 20.0,
        disable_scale_in: true,
        minted: &["AlarmHigh"],
        breaches: Breaches::Neither,
        moved_to: None,
    },
];

/// The dimension id a row's item is keyed by, which is also the last segment of its resource id.
fn dimension_id(index: usize) -> String {
    format!("tracked-{index}")
}

/// The metric each row publishes into, one per row so no row grades another's series.
fn metric_name(index: usize) -> String {
    format!("Load{index}")
}

/// The policy name a row puts.
fn policy_name(index: usize) -> String {
    format!("tracked-policy-{index}")
}

/// The three identity members of the `custom-resource` scalable target on `resource_id`.
fn target(resource_id: &str) -> Value {
    json!({
        "ServiceNamespace": "custom-resource",
        "ResourceId": resource_id,
        "ScalableDimension": DIMENSION,
    })
}

/// Call one CloudWatch operation.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("monitoring.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("GraniteServiceVersion20100801.{operation}"),
        )
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEMULATOR/20260101/{REGION}/monitoring\
                 /aws4_request, SignedHeaders=host;x-amz-target, Signature=0"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body().to_vec();
    (status, serde_json::from_slice(&payload).unwrap_or(Value::Null))
}

/// The middle of the `index`-th most recent period window that has already CLOSED.
///
/// A metric is aggregated into periods anchored at multiples of the period and an alarm grades
/// the windows that are complete, so a datapoint meant to decide an alarm now is stamped in a
/// closed window rather than in the one the clock is still in.
fn graded_window_stamp(index: i64) -> f64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_secs_f64();
    ((now / SERIES_PERIOD).floor() - index as f64) * SERIES_PERIOD + SERIES_PERIOD / 2.0
}

/// Publish a row's load into every window its alarms grade, the way a client publishes it.
async fn publish_load(router: &MultiServiceRouter, row: &Row, metric: &str) {
    let data: Vec<Value> = (1..=PUBLISHED_WINDOWS)
        .map(|index| {
            json!({
                "MetricName": metric,
                "Timestamp": graded_window_stamp(index),
                "Value": row.measured,
            })
        })
        .collect();
    let (status, body) = cloudwatch(
        router,
        "PutMetricData",
        json!({"Namespace": METRIC_NAMESPACE, "MetricData": data}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: expected PutMetricData to answer 200, got {status} {body:?}",
        row.case
    );
}

/// The item the endpoint serves for one dimension: the capacity it holds, how many times it has
/// been `PATCH`ed and the body of the last `PATCH` it received.
async fn item_state(router: &MultiServiceRouter, dimension: &str) -> (Option<f64>, i64, Option<String>) {
    let (status, read) = dynamodb(
        router,
        "GetItem",
        json!({
            "TableName": CAPACITY_TABLE,
            "Key": {"id": {"S": dimension}},
            "ConsistentRead": true,
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected GetItem to answer 200, got {status} {read:?}"
    );
    let item = &read["Item"];
    (
        item["desired"]["N"]
            .as_str()
            .and_then(|value| value.parse::<f64>().ok()),
        item["patches"]["N"]
            .as_str()
            .and_then(|value| value.parse::<i64>().ok())
            .unwrap_or_default(),
        item["lastBody"]["S"].as_str().map(str::to_string),
    )
}

/// The scaling activities recorded for one target, newest first.
async fn activities(router: &MultiServiceRouter, resource_id: &str) -> Vec<Value> {
    let (status, body) =
        application_auto_scaling(router, "DescribeScalingActivities", target(resource_id)).await;
    assert_eq!(
        status, 200,
        "expected DescribeScalingActivities to answer 200, got {status} {body:?}"
    );
    body["ScalingActivities"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The state CloudWatch reports for one alarm.
async fn alarm_state(router: &MultiServiceRouter, alarm_name: &str) -> String {
    let (status, body) = cloudwatch(
        router,
        "DescribeAlarms",
        json!({"AlarmNames": [alarm_name]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "expected DescribeAlarms to answer 200, got {status} {body:?}"
    );
    body["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Wait for one alarm to be evaluated into `wanted`, answering what it last reported.
///
/// A `DescribeAlarms` is also what leaves CloudWatch's own alarm clock running, so the poll
/// both waits for the evaluation and keeps the thing it waits for alive.
async fn awaited_alarm_state(router: &MultiServiceRouter, alarm_name: &str, wanted: &str) -> String {
    let deadline = SystemTime::now() + POLL_FOR;
    let mut last = alarm_state(router, alarm_name).await;
    while last != wanted && SystemTime::now() < deadline {
        tokio::time::sleep(POLL_EVERY).await;
        last = alarm_state(router, alarm_name).await;
    }
    last
}

/// The kind segment of one of a policy's alarm names: `TargetTracking-<resource>-<kind>-<id>`.
fn alarm_kind(alarm_name: &str) -> String {
    let mut segments = alarm_name.rsplit('-');
    // The trailing id is five hyphen-separated groups, and the kind is the segment before it.
    for _ in 0..5 {
        segments.next();
    }
    segments.next().unwrap_or_default().to_string()
}

/// Put a row's target-tracking policy, answering the names of the alarms it minted, by kind.
async fn put_policy(
    router: &MultiServiceRouter,
    row: &Row,
    resource_id: &str,
    name: &str,
    metric: &str,
) -> Vec<(String, String)> {
    let mut configuration = json!({
        "TargetValue": row.target_value,
        // Nothing here breaches twice, so the wait a previous activity is given does not come
        // into it; it is named rather than defaulted so each row states it.
        "ScaleOutCooldown": 0,
        "ScaleInCooldown": 0,
        "CustomizedMetricSpecification": {
            "Namespace": METRIC_NAMESPACE,
            "MetricName": metric,
            "Statistic": STATISTIC,
        },
    });
    if row.disable_scale_in {
        configuration["DisableScaleIn"] = json!(true);
    }
    let mut request = target(resource_id);
    request["PolicyName"] = json!(name);
    request["PolicyType"] = json!("TargetTrackingScaling");
    request["TargetTrackingScalingPolicyConfiguration"] = configuration;
    let (status, body) = application_auto_scaling(router, "PutScalingPolicy", request).await;
    assert_eq!(
        status, 200,
        "case {:?}: expected PutScalingPolicy to answer 200, got {status} {body:?}",
        row.case
    );
    body["Alarms"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|alarm| {
            let name = alarm["AlarmName"].as_str().unwrap_or_default().to_string();
            (alarm_kind(&name), name)
        })
        .collect()
}

#[tokio::test]
async fn a_target_tracking_policy_drives_a_custom_resource_endpoint() {
    let router = router();
    let api = given_endpoint(&router).await;
    given_capacity_table(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let dimension = dimension_id(index);
        let metric = metric_name(index);
        let policy = policy_name(index);
        given_item(&router, &dimension, row.starting).await;
        let resource_id = format!(
            "https://127.0.0.1:9297/_aws/execute-api/{api}/{DEPLOYED_STAGE}/{DIMENSION_PATH}\
             /{dimension}"
        );

        let mut registration = target(&resource_id);
        registration["MinCapacity"] = json!(row.min);
        registration["MaxCapacity"] = json!(row.max);
        let (status, body) =
            application_auto_scaling(&router, "RegisterScalableTarget", registration).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected RegisterScalableTarget to answer 200, got {status} {body:?}"
        );

        // The load first, so the windows the policy's alarms grade are already full when they
        // are minted: the rule is about the alarm's verdict, not about which pass first saw it.
        publish_load(&router, row, &metric).await;
        let minted = put_policy(&router, row, &resource_id, &policy, &metric).await;
        let kinds: Vec<&str> = minted.iter().map(|(kind, _)| kind.as_str()).collect();
        assert_eq!(
            kinds, row.minted,
            "case {case:?}: expected the policy to mint {:?}, got {minted:?}",
            row.minted
        );

        let Some(moved_to) = row.moved_to else {
            // Nothing is owed: the direction the metric asks for has no alarm at all, so the
            // endpoint is never asked to move and the log stays empty. Watched rather than
            // waited out, so a movement appearing at any point inside the watch fails here.
            let watch_until = SystemTime::now() + QUIET_WATCH;
            loop {
                let (held, patches, body) = item_state(&router, &dimension).await;
                assert_eq!(
                    (held, patches, body.clone()),
                    (Some(row.starting as f64), 0, None),
                    "case {case:?}: expected the endpoint to be left at {} with no PATCH at \
                     all, got {held:?} after {patches} PATCH(es) with {body:?}",
                    row.starting
                );
                let listed = activities(&router, &resource_id).await;
                assert!(
                    listed.is_empty(),
                    "case {case:?}: expected no scaling activity at all, got {listed:?}"
                );
                if SystemTime::now() >= watch_until {
                    break;
                }
                tokio::time::sleep(POLL_EVERY).await;
            }
            continue;
        };

        // The alarm of the direction the metric asks for reaches ALARM on the series it graded.
        let wanted_kind = match row.breaches {
            Breaches::High => "AlarmHigh",
            Breaches::Low => "AlarmLow",
            Breaches::Neither => unreachable!("a row that moves capacity breaches an alarm"),
        };
        let (_, alarm_name) = minted
            .iter()
            .find(|(kind, _)| kind == wanted_kind)
            .unwrap_or_else(|| panic!("case {case:?}: expected a {wanted_kind} alarm, got {minted:?}"));
        let state = awaited_alarm_state(&router, alarm_name, "ALARM").await;
        assert_eq!(
            state, "ALARM",
            "case {case:?}: expected {alarm_name} to grade the published series into ALARM, got \
             {state:?}"
        );

        // The movement reaches the ENDPOINT: the caller's own resource holds the capacity the
        // control loop asked for, clamped into the registered window, and the body it received
        // is the one member the custom-resource contract names.
        let expected_body = format!("{{\"desiredCapacity\": {:?}}}", moved_to as f64);
        let deadline = SystemTime::now() + POLL_FOR;
        let mut observed = item_state(&router, &dimension).await;
        while observed.0 != Some(moved_to as f64) && SystemTime::now() < deadline {
            tokio::time::sleep(POLL_EVERY).await;
            observed = item_state(&router, &dimension).await;
        }
        let (held, patches, last_body) = observed;
        assert_eq!(
            held,
            Some(moved_to as f64),
            "case {case:?}: expected the endpoint's own capacity to reach {moved_to}, got \
             {held:?} after {patches} PATCH(es) with {last_body:?}"
        );
        assert_eq!(
            (patches, last_body.clone()),
            (1, Some(expected_body.clone())),
            "case {case:?}: expected exactly one PATCH carrying {expected_body:?}, got {patches} \
             with {last_body:?}"
        );

        // And the decision is recorded as ONE activity, naming the alarm and the policy that
        // caused it and the capacity it set.
        let deadline = SystemTime::now() + POLL_FOR;
        let mut listed = activities(&router, &resource_id).await;
        while !listed
            .iter()
            .all(|activity| activity["StatusCode"] == json!(SUCCESSFUL))
            && SystemTime::now() < deadline
        {
            tokio::time::sleep(POLL_EVERY).await;
            listed = activities(&router, &resource_id).await;
        }
        assert_eq!(
            listed.len(),
            1,
            "case {case:?}: expected exactly one scaling activity, got {listed:?}"
        );
        let expected = json!({
            "Cause": format!("monitor alarm {alarm_name} in state ALARM triggered policy {policy}"),
            "Description": format!("Setting desired capacity to {moved_to}."),
            "StatusCode": SUCCESSFUL,
        });
        for (name, want) in expected.as_object().expect("an object of members") {
            assert_eq!(
                &listed[0][name], want,
                "case {case:?}: expected {name} {want:?}, got {:?}",
                listed[0]
            );
        }
    }
}
