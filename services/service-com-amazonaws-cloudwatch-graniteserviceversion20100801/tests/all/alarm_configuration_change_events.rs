// SPDX-License-Identifier: CC0-1.0

//! Every write that CHANGES an alarm's configuration publishes a `CloudWatch Alarm
//! Configuration Change` event onto the account's default event bus, end to end over the wire
//! through the real `MultiServiceRouter` with CloudWatch, EventBridge and Amazon SQS on one
//! engine — so what a client sees is what is asserted: the envelope on the queue a matching
//! rule targets.
//!
//! The RULE asserted here: **a write that creates an alarm, one that replaces the configuration
//! of an alarm that already exists with a different one, and a call that deletes an alarm each
//! publish one event with `source` `aws.cloudwatch`, `detail-type` `CloudWatch Alarm
//! Configuration Change`, the alarm's own ARN in `resources` and a `detail` naming the alarm,
//! the `operation` the write performed (`create`, `update`, `delete`), the state the alarm is in
//! and the configuration the write left — beside the configuration it REPLACED, for an update;
//! while a call that changes no configuration publishes none.** The calls that change none are
//! what the negative half of this rule is about: a put that re-states the configuration the
//! alarm already carries overwrites it with itself ("the update completely overwrites the
//! previous configuration of the alarm", `PutMetricAlarm`), and a tagging call changes something
//! that is not the alarm's configuration at all ("any tags you specify in this parameter are
//! ignored. To change the tags of an existing alarm, use TagResource or UntagResource",
//! `PutMetricAlarmInput$Tags`).
//!
//! The rows differ along what the rule generalises over: the KIND of alarm written (a metric
//! alarm, a composite alarm), WHICH operation the call performed, WHICH member of the
//! configuration an update changed (a threshold, an action list, a rule), and the two shapes of
//! call that change no configuration.
//!
//! The negative halves are graded WITHOUT waiting out a span: every call of a row is made in
//! order and the whole SEQUENCE of events delivered is compared against the sequence the rule
//! owes, so an event a no-op call published would land between two the rule names and fail the
//! comparison.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{given_queue, received_messages, rpc, Emulator, REGION};

/// The `source` every event below is matched on.
const SOURCE: &str = "aws.cloudwatch";

/// The `detail-type` of an alarm configuration change, spelled out here rather than imported
/// from the service crate, so a wrong constant in the implementation cannot make this suite
/// pass.
const DETAIL_TYPE: &str = "CloudWatch Alarm Configuration Change";

/// The three `detail.operation` values a write reports.
const CREATE: &str = "create";
const UPDATE: &str = "update";
const DELETE: &str = "delete";

/// The service principal EventBridge delivers to an SQS target as, which the queue's own policy
/// has to allow the send for.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The namespace and metric the metric alarms below watch.
const NAMESPACE: &str = "Custom/ConfigurationChange";
const METRIC: &str = "Backlog";

/// The period those alarms aggregate over, as the modelled seconds member carries it.
const PERIOD: i64 = 60;

/// How long this suite waits for work the clock owes — the asynchronous publication of an event
/// and its delivery to a target — before giving up on it. Generous on purpose: nothing here is
/// asserted about how fast the host runs, only about what eventually arrives.
const WORK_WAIT: Duration = Duration::from_secs(60);

/// How often it looks while waiting.
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// CloudWatch, EventBridge and Amazon SQS over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: the event CloudWatch publishes reaches EventBridge's own handler, which matches
/// it against this account's rules and delivers it to the queue this suite reads.
fn emulator_with_a_bus() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_eventbridge_awsevents::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    Emulator { router, engine }
}

/// One EventBridge call (`awsJson1_1`, `AWSEvents.<Operation>`) that must answer 200.
///
/// EventBridge serves `awsJson1_1` where this service serves `awsJson1_0`, so the call is made
/// here rather than through the shared RPC helper: the content type is part of the wire.
async fn eventbridge(emulator: &Emulator, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/events/\
                 aws4_request, SignedHeaders=host, Signature=0000"
            ),
        )
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed: Value = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from EventBridge {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One CloudWatch call that must answer 200.
async fn monitoring(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(
        emulator,
        "monitoring",
        crate::harness::TARGET_PREFIX,
        operation,
        body,
    )
    .await
}

/// A queue a rule can target: its url, with its own policy allowing EventBridge the send — the
/// grant AWS documents for a queue a rule delivers to, and what a queue needs to receive
/// anything at all.
async fn given_target_queue(emulator: &Emulator, name: &str) -> String {
    let (url, arn) = given_queue(emulator, name).await;
    rpc(
        emulator,
        "sqs",
        "AmazonSQS",
        "SetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "Attributes": {"Policy": json!({
                "Version": "2012-10-17",
                "Statement": [{
                    "Sid": "AllowEventBridgeDelivery",
                    "Effect": "Allow",
                    "Principal": {"Service": EVENTS_PRINCIPAL},
                    "Action": "sqs:SendMessage",
                    "Resource": arn,
                }],
            })
            .to_string()},
        }),
    )
    .await;
    url
}

/// The ARN of the queue at `url`, as that service reports it.
async fn queue_arn_of(emulator: &Emulator, url: &str) -> String {
    let attributes = rpc(
        emulator,
        "sqs",
        "AmazonSQS",
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("GetQueueAttributes {url}: no QueueArn in {attributes}"))
        .to_string()
}

/// Wire a rule matching the configuration changes of the alarm `alarm_name` to the queue at
/// `url`.
async fn given_rule_for_alarm(
    emulator: &Emulator,
    case: &str,
    rule_name: &str,
    alarm_name: &str,
    queue_url: &str,
) {
    let pattern = json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": {"alarmName": [alarm_name]},
    })
    .to_string();
    eventbridge(
        emulator,
        case,
        "PutRule",
        &json!({"Name": rule_name, "EventPattern": pattern, "State": "ENABLED"}),
    )
    .await;
    let queue_arn = queue_arn_of(emulator, queue_url).await;
    let wired = eventbridge(
        emulator,
        case,
        "PutTargets",
        &json!({"Rule": rule_name, "Targets": [{"Id": "q", "Arn": queue_arn}]}),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the target to be accepted, got {wired}"
    );
}

/// Take whatever has been delivered to the queue at `url` and add it to `held`, parsed out of
/// the message bodies — accumulated across looks, because a received message is invisible to
/// the next look.
async fn collect(emulator: &Emulator, url: &str, held: &mut Vec<Value>) {
    for message in received_messages(emulator, url).await {
        let body = message["Body"].as_str().unwrap_or_default();
        if let Ok(parsed) = serde_json::from_str::<Value>(body) {
            held.push(parsed);
        }
    }
}

/// Wait until `held` holds `wanted` events, adding everything delivered meanwhile to it.
///
/// The wait is for the CONDITION rather than for a span: it looks again until the events are
/// there or [`WORK_WAIT`] has passed.
async fn until_delivered(emulator: &Emulator, url: &str, wanted: usize, held: &mut Vec<Value>) {
    let looks = WORK_WAIT.as_millis() / LOOK_INTERVAL.as_millis().max(1);
    for _ in 0..looks {
        collect(emulator, url, held).await;
        if held.len() >= wanted {
            return;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    collect(emulator, url, held).await;
}

/// The metric alarm request of this suite, with `patch` merged over it: one metric, dimensioned
/// by the alarm's own name so one row's datapoints are never another row's.
fn metric_alarm(name: &str, patch: &Value) -> Value {
    crate::harness::merged(
        json!({
            "AlarmName": name,
            "AlarmDescription": "the alarm of this row",
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [{"Name": "QueueName", "Value": name}],
            "Statistic": "Average",
            "Period": PERIOD,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": "ignore",
        }),
        patch,
    )
}

/// The ARN the alarm `name` of kind `kind` reports.
async fn alarm_arn(emulator: &Emulator, case: &str, kind: &str, name: &str) -> String {
    let described = monitoring(
        emulator,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": [kind]}),
    )
    .await;
    let member = if kind == "CompositeAlarm" {
        "CompositeAlarms"
    } else {
        "MetricAlarms"
    };
    described[member][0]["AlarmArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the alarm {name:?} reports no AlarmArn"))
        .to_string()
}

/// Assert the envelope of one delivered event: what it is, which alarm it is about, which
/// operation it reports, and that it names the alarm's own ARN.
fn assert_envelope(case: &str, event: &Value, alarm: &str, arn: &str, operation: &str) {
    for (member, pointer, expected) in [
        ("the event's source", "/source", json!(SOURCE)),
        ("the event's detail-type", "/detail-type", json!(DETAIL_TYPE)),
        ("the alarm's name", "/detail/alarmName", json!(alarm)),
        ("the operation", "/detail/operation", json!(operation)),
    ] {
        let found = event.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            found, expected,
            "case {case:?}: expected {member} to be {expected} at {pointer}, got {found} in \
             {event}"
        );
    }
    // "AWS resources, identified by Amazon Resource Name (ARN), which the event primarily
    // concerns": the alarm that was written.
    let resources = event["resources"].as_array().cloned().unwrap_or_default();
    assert!(
        resources.contains(&json!(arn)),
        "case {case:?}: expected the alarm's ARN {arn:?} in the event's resources, got \
         {resources:?} in {event}"
    );
    // The state a configuration write leaves the alarm in is part of every one of these
    // events: "When you update an existing alarm, its state is left unchanged".
    assert!(
        event
            .pointer("/detail/state/value")
            .and_then(Value::as_str)
            .is_some(),
        "case {case:?}: expected the alarm's state in the event's detail, got {event}"
    );
}

/// RULE: a write that changes a METRIC alarm's configuration publishes one configuration-change
/// event reporting what it did, and a call that changes no configuration publishes none.
#[tokio::test]
async fn metric_alarm_configuration_writes_publish_what_they_did() {
    // Each row is one call this suite makes, and the operation the event it owes must report —
    // `None` for a call that owes none. The rows differ along what the call did to the stored
    // configuration.
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "a write of an alarm the account held none of",
            json!({}),
            Some(CREATE),
        ),
        (
            "a write that re-states the same configuration",
            json!({}),
            None,
        ),
        (
            "a write that changes the threshold",
            json!({"Threshold": 90.0}),
            Some(UPDATE),
        ),
        (
            "a write that changes the alarm's action list",
            json!({
                "Threshold": 90.0,
                "AlarmActions": [format!("arn:aws:sns:{REGION}:000000000000:config-change")],
            }),
            Some(UPDATE),
        ),
        (
            "a write that re-states the changed configuration",
            json!({
                "Threshold": 90.0,
                "AlarmActions": [format!("arn:aws:sns:{REGION}:000000000000:config-change")],
            }),
            None,
        ),
    ];

    let case = "a metric alarm's configuration writes";
    let emulator = emulator_with_a_bus();
    let alarm = "configuration-change-metric";
    let queue = given_target_queue(&emulator, "configuration-change-metric-q").await;
    // The rule is wired BEFORE the alarm exists, so the event the CREATE publishes is matched
    // by a rule that was already live.
    given_rule_for_alarm(
        &emulator,
        case,
        "configuration-change-metric-rule",
        alarm,
        &queue,
    )
    .await;

    let mut held: Vec<Value> = Vec::new();
    let mut expected: Vec<&str> = Vec::new();
    for (row, patch, owed) in &rows {
        monitoring(&emulator, "PutMetricAlarm", &metric_alarm(alarm, patch)).await;
        if let Some(operation) = owed {
            expected.push(operation);
            // Waiting for the event this call owes is what makes the next row's no-op
            // assertion sound: an event is counted as missing only after the one the rule
            // really owes has arrived.
            until_delivered(&emulator, &queue, expected.len(), &mut held).await;
        } else {
            collect(&emulator, &queue, &mut held).await;
        }
        assert_eq!(
            held.len(),
            expected.len(),
            "case {case:?}, row {row:?}: expected {} event(s) on the rule's queue, got {held:?}",
            expected.len()
        );
    }

    // And the deletion of the alarm publishes the last event of the sequence.
    let arn = alarm_arn(&emulator, case, "MetricAlarm", alarm).await;
    monitoring(&emulator, "DeleteAlarms", &json!({"AlarmNames": [alarm]})).await;
    expected.push(DELETE);
    until_delivered(&emulator, &queue, expected.len(), &mut held).await;

    let delivered: Vec<String> = held
        .iter()
        .map(|event| {
            event
                .pointer("/detail/operation")
                .and_then(Value::as_str)
                .unwrap_or("<none>")
                .to_string()
        })
        .collect();
    assert_eq!(
        delivered, expected,
        "case {case:?}: expected the operations {expected:?} in order on the rule's queue, got \
         {delivered:?} from {held:?}"
    );
    for (event, operation) in held.iter().zip(&expected) {
        assert_envelope(case, event, alarm, &arn, operation);
    }
    // "previousConfiguration" is the configuration an UPDATE replaced, so it is reported for an
    // update and for nothing else — a create replaced none, and a deletion replaced none.
    for (event, operation) in held.iter().zip(&expected) {
        let previous = event.pointer("/detail/previousConfiguration");
        if *operation == UPDATE {
            assert!(
                previous.is_some_and(|previous| previous != &Value::Null),
                "case {case:?}: expected an update to report the configuration it replaced, got \
                 {event}"
            );
            assert_ne!(
                previous,
                event.pointer("/detail/configuration"),
                "case {case:?}: an update that changed the configuration must report a different \
                 previous one, got {event}"
            );
        } else {
            assert_eq!(
                previous, None,
                "case {case:?}: expected the {operation:?} event to report no previous \
                 configuration, got {event}"
            );
        }
    }
    // The metric the alarm watched is what its configuration document reports.
    let name = held[0]
        .pointer("/detail/configuration/metricName")
        .cloned()
        .unwrap_or(Value::Null);
    assert_eq!(
        name,
        json!(METRIC),
        "case {case:?}: expected the configured metric {METRIC:?} in the event's configuration, \
         got {:?}",
        held[0]
    );
}

/// RULE, over the other KIND of alarm: the same three operations, published for a composite
/// alarm, whose configuration document reports the rule it applies rather than a metric — and a
/// tagging call, which changes no configuration, publishes nothing.
#[tokio::test]
async fn composite_alarm_configuration_writes_publish_what_they_did() {
    let case = "a composite alarm's configuration writes";
    let emulator = emulator_with_a_bus();
    let alarm = "configuration-change-composite";
    let child = "configuration-change-child";
    let other_child = "configuration-change-other-child";
    let queue = given_target_queue(&emulator, "configuration-change-composite-q").await;
    given_rule_for_alarm(
        &emulator,
        case,
        "configuration-change-composite-rule",
        alarm,
        &queue,
    )
    .await;

    // The children the composite rule reads: what makes each rule below well formed.
    for name in [child, other_child] {
        monitoring(&emulator, "PutMetricAlarm", &metric_alarm(name, &json!({}))).await;
    }

    let mut held: Vec<Value> = Vec::new();
    let composite = |rule: String| {
        json!({
            "AlarmName": alarm,
            "AlarmRule": rule,
            "AlarmDescription": "the composite alarm of this suite",
        })
    };

    // 1. The create.
    monitoring(
        &emulator,
        "PutCompositeAlarm",
        &composite(format!("ALARM(\"{child}\")")),
    )
    .await;
    until_delivered(&emulator, &queue, 1, &mut held).await;
    // 2. A write re-stating the same rule changes nothing and publishes nothing.
    monitoring(
        &emulator,
        "PutCompositeAlarm",
        &composite(format!("ALARM(\"{child}\")")),
    )
    .await;
    // 3. A tagging call changes no configuration either, so it publishes nothing.
    let arn = alarm_arn(&emulator, case, "CompositeAlarm", alarm).await;
    monitoring(
        &emulator,
        "TagResource",
        &json!({"ResourceARN": arn, "Tags": [{"Key": "team", "Value": "platform"}]}),
    )
    .await;
    collect(&emulator, &queue, &mut held).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected a re-stated rule and a tagging call to publish nothing, got \
         {held:?}"
    );
    // 4. A write changing the rule is an update.
    monitoring(
        &emulator,
        "PutCompositeAlarm",
        &composite(format!("ALARM(\"{other_child}\")")),
    )
    .await;
    until_delivered(&emulator, &queue, 2, &mut held).await;
    // 5. And the deletion is a delete.
    monitoring(&emulator, "DeleteAlarms", &json!({"AlarmNames": [alarm]})).await;
    until_delivered(&emulator, &queue, 3, &mut held).await;

    let expected = [CREATE, UPDATE, DELETE];
    let delivered: Vec<String> = held
        .iter()
        .map(|event| {
            event
                .pointer("/detail/operation")
                .and_then(Value::as_str)
                .unwrap_or("<none>")
                .to_string()
        })
        .collect();
    assert_eq!(
        delivered, expected,
        "case {case:?}: expected the operations {expected:?} in order on the rule's queue, got \
         {delivered:?} from {held:?}"
    );
    for (event, operation) in held.iter().zip(expected) {
        assert_envelope(case, event, alarm, &arn, operation);
        let rule = event.pointer("/detail/configuration/alarmRule");
        assert!(
            rule.and_then(Value::as_str).is_some(),
            "case {case:?}: expected the composite alarm's rule in the {operation:?} event's \
             configuration, got {event}"
        );
    }
}
