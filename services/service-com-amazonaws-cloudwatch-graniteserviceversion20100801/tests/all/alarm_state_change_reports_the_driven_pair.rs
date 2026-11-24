// SPDX-License-Identifier: CC0-1.0

//! The ORDERED PAIR of states a client drives an alarm through is the pair the delivered
//! `CloudWatch Alarm State Change` envelope reports.
//!
//! The RULE asserted here: **a change of a metric alarm's `StateValue` is published onto the
//! account's default event bus as `source` `aws.cloudwatch` / `detail-type` `CloudWatch Alarm
//! State Change`, whose `detail.state.value` is the state the alarm moved TO and whose
//! `detail.previousState.value` is the state it moved OUT OF — for EVERY ordered pair of the
//! three values `StateValue` admits — and a rule whose `EventPattern` matches that envelope
//! delivers it to the rule's targets.** Sources: `SetAlarmState` ("when the updated state
//! differs from the previous value, the action configured for the appropriate state is
//! invoked"), the `StateValue` enum for the catalog of states
//! (`smithy_query(query="shape",shape="StateValue",model_path="services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/smithy-model/com-amazonaws-cloudwatch-graniteserviceversion20100801.json")`
//! reports exactly `OK`, `ALARM`, `INSUFFICIENT_DATA`), and `PutTargets` for the grant a queue
//! target needs ("for Lambda functions, Amazon SNS topics and Amazon SQS queues, EventBridge
//! relies on resource-based policies").
//!
//! The rows differ along what the rule generalises over: WHICH ordered pair of states the client
//! drives — every one of the six pairs of distinct values, so neither half of the envelope's
//! `detail` can be right for one direction and wrong for another, and a consumer filtering on
//! `detail.previousState.value` sees the state the alarm really left. A client that watches an
//! alarm for a particular move reads BOTH halves: the suites beside this one pin where the
//! envelope goes and that one transition's blocks chain onto the next, while the pair a single
//! drive announces is what this one pins.
//!
//! The alarm each row drives watches a load balancer's target health — `AWS/ApplicationELB`
//! `UnHealthyHostCount` under the `TargetGroup` + `LoadBalancer` dimension pair — which is a
//! series a client of this service only READS, so no row's drive is racing a datapoint of its
//! own: every transition below is one the client asked for.
//!
//! What is built is the emulator with EventBridge and Amazon SQS mounted beside CloudWatch,
//! because the envelope is asserted where a client sees it: on the queue the rule targets.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    given_queue, received_messages, rpc, set_alarm_state, Emulator, LOOK_INTERVAL, REGION,
    TARGET_PREFIX, WORK_WAIT,
};

/// The `source` and `detail-type` of an alarm state change, spelled out here rather than
/// imported, so a wrong constant in the implementation cannot make this suite pass.
const SOURCE: &str = "aws.cloudwatch";
const DETAIL_TYPE: &str = "CloudWatch Alarm State Change";

/// The service principal EventBridge delivers to an SQS target as, which the queue's own policy
/// has to allow the send for.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The series every row's alarm watches: a load balancer's target health, which a client of this
/// service only ever reads.
const NAMESPACE: &str = "AWS/ApplicationELB";
const METRIC: &str = "UnHealthyHostCount";

/// The period those alarms aggregate over, as the modelled seconds member carries it.
const PERIOD: i64 = 60;

/// The three values `StateValue` admits, as the model lists them (see the module docs).
const OK: &str = "OK";
const ALARM: &str = "ALARM";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// CloudWatch, EventBridge and Amazon SQS over ONE engine, as `core/router/src/lib.rs` mounts
/// them: the publication CloudWatch makes through the in-process callback reaches EventBridge's
/// own handler, which matches it against this account's rules and delivers it to the queue the
/// row reads. (`crate::harness::emulator` mounts SNS and EC2 instead, for the alarm-ACTION
/// suites; what an event needs is the bus and the queue.)
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

/// One EventBridge call.
///
/// EventBridge serves `awsJson1_1` where this service serves `awsJson1_0`, so the request is
/// built here rather than through the shared wire: the content type is part of it.
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

/// Allow EventBridge the send on the queue at `url` — the resource-based policy `PutTargets`
/// documents for a queue a rule delivers to.
async fn grant_event_delivery(emulator: &Emulator, url: &str, queue_arn: &str) {
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridgeDelivery",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": queue_arn,
        }],
    })
    .to_string();
    rpc(
        emulator,
        "sqs",
        "AmazonSQS",
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": {"Policy": policy}}),
    )
    .await;
}

/// An alarm over a load balancer's target health, the way a client puts one, with no
/// `TreatMissingData` of its own. Answers the ARN CloudWatch reports for it.
async fn given_alarm(emulator: &Emulator, case: &str, name: &str) -> String {
    rpc(
        emulator,
        "monitoring",
        TARGET_PREFIX,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": METRIC,
            "Dimensions": [
                {"Name": "TargetGroup", "Value": format!("targetgroup/{name}/0123456789abcdef")},
                {"Name": "LoadBalancer", "Value": format!("app/{name}/0123456789abcdef")},
            ],
            "Statistic": "Maximum",
            "Period": PERIOD,
            "EvaluationPeriods": 1,
            "Threshold": 0.0,
            "ComparisonOperator": "GreaterThanThreshold",
        }),
    )
    .await;
    let described = rpc(
        emulator,
        "monitoring",
        TARGET_PREFIX,
        "DescribeAlarms",
        &json!({"AlarmNames": [name]}),
    )
    .await;
    described["MetricAlarms"][0]["AlarmArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the alarm's ARN in {described}"))
        .to_string()
}

/// Wire an ENABLED rule matching the alarm `name`'s state changes to the queue `queue_arn`.
async fn given_rule(
    emulator: &Emulator,
    case: &str,
    rule: &str,
    alarm_name: &str,
    queue_arn: &str,
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
        &json!({"Name": rule, "EventPattern": pattern, "State": "ENABLED"}),
    )
    .await;
    let wired = eventbridge(
        emulator,
        case,
        "PutTargets",
        &json!({"Rule": rule, "Targets": [{"Id": "q", "Arn": queue_arn}]}),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the queue target to be accepted, got {wired}"
    );
}

/// Everything waiting on the queue at `url`, as the parsed envelopes their bodies carry.
async fn drain(emulator: &Emulator, url: &str) -> Vec<Value> {
    received_messages(emulator, url)
        .await
        .iter()
        .filter_map(|message| {
            serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default()).ok()
        })
        .collect()
}

/// The envelopes among `held` that report the move `from` -> `to`.
fn reporting_the_pair<'a>(held: &'a [Value], from: &str, to: &str) -> Vec<&'a Value> {
    held.iter()
        .filter(|event| {
            event.pointer("/detail/state/value") == Some(&json!(to))
                && event.pointer("/detail/previousState/value") == Some(&json!(from))
        })
        .collect()
}

/// One row: the ordered pair of states the client drives the alarm through.
struct Row {
    case: &'static str,
    /// The state the client asks for first — the state the move is OUT OF.
    from: &'static str,
    /// The state the client asks for next — the state the move is INTO.
    to: &'static str,
}

/// RULE: for every ordered pair of the states `StateValue` admits, the envelope a driven move
/// publishes reports that pair — the state moved INTO as `detail.state.value` and the state moved
/// OUT OF as `detail.previousState.value` — and a matching rule delivers it to its SQS target.
#[tokio::test]
async fn a_driven_move_is_reported_as_the_pair_of_states_it_was() {
    // Every ordered pair of distinct values of the `StateValue` enum the model declares.
    let rows = [
        Row {
            case: "ALARM into OK",
            from: ALARM,
            to: OK,
        },
        Row {
            case: "OK into ALARM",
            from: OK,
            to: ALARM,
        },
        Row {
            case: "ALARM into INSUFFICIENT_DATA",
            from: ALARM,
            to: INSUFFICIENT_DATA,
        },
        Row {
            case: "INSUFFICIENT_DATA into ALARM",
            from: INSUFFICIENT_DATA,
            to: ALARM,
        },
        Row {
            case: "OK into INSUFFICIENT_DATA",
            from: OK,
            to: INSUFFICIENT_DATA,
        },
        Row {
            case: "INSUFFICIENT_DATA into OK",
            from: INSUFFICIENT_DATA,
            to: OK,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = emulator_with_a_bus();
        let alarm_name = format!("target-health-pair-{index}");
        let (url, queue_arn) = given_queue(&emulator, &format!("pair-q-{index}")).await;
        grant_event_delivery(&emulator, &url, &queue_arn).await;
        let alarm_arn = given_alarm(&emulator, case, &alarm_name).await;
        given_rule(
            &emulator,
            case,
            &format!("pair-r-{index}"),
            &alarm_name,
            &queue_arn,
        )
        .await;

        // The client's own drive: ask for the state the move is out of, then ask for the state it
        // is into. Only an ask that finds the alarm in another state moves it, so the first ask is
        // what puts the alarm in `from` whenever it is not there already, and the second is the
        // move this row is about. Look again until that envelope is on the queue or the deadline
        // passes — the wait is for the CONDITION, so a slow host delays a row instead of breaking
        // it.
        let started = Instant::now();
        let mut held: Vec<Value> = Vec::new();
        while started.elapsed() < WORK_WAIT
            && reporting_the_pair(&held, row.from, row.to).is_empty()
        {
            set_alarm_state(&emulator, &alarm_name, row.from).await;
            set_alarm_state(&emulator, &alarm_name, row.to).await;
            held.extend(drain(&emulator, &url).await);
            if reporting_the_pair(&held, row.from, row.to).is_empty() {
                tokio::time::sleep(LOOK_INTERVAL).await;
            }
        }

        let delivered = reporting_the_pair(&held, row.from, row.to);
        assert!(
            !delivered.is_empty(),
            "case {case:?}: expected an envelope reporting the move of {alarm_name:?} out of \
             {:?} into {:?} on {url}, got {held:?}",
            row.from,
            row.to
        );
        for envelope in delivered {
            for (pointer, expected) in [
                ("/source", json!(SOURCE)),
                ("/detail-type", json!(DETAIL_TYPE)),
                ("/region", json!(REGION)),
                ("/account", json!(engine::EMULATOR_ACCOUNT_ID)),
                ("/resources", json!([&alarm_arn])),
                ("/detail/alarmName", json!(&alarm_name)),
                ("/detail/state/value", json!(row.to)),
                ("/detail/previousState/value", json!(row.from)),
            ] {
                let actual = envelope.pointer(pointer).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    actual, expected,
                    "case {case:?}: expected {pointer} to be {expected}, got {actual} in \
                     {envelope}"
                );
            }
        }
    }
}
