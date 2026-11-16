// SPDX-License-Identifier: CC0-1.0

//! An alarm transition reaches the SQS target of a matching EventBridge rule — in the Region
//! the alarm lives in, whichever dimension of the envelope the rule filters on, and however
//! many times the client has asked for that transition.
//!
//! Two rules are asserted here, both about what a CHANGE of an alarm's `StateValue` owes:
//!
//! * **WHERE IT GOES.** The change is published as one event onto the DEFAULT event bus of
//!   that alarm's own Region, with `source` `aws.cloudwatch`, the alarm's ARN in `resources`
//!   and the state it moved to inside `detail` — so a rule of that Region whose `EventPattern`
//!   matches any dimension of that envelope delivers it to the rule's targets, and until such
//!   a rule exists the same transitions are delivered nowhere.
//! * **WHAT COUNTS AS ONE.** A caller's `SetAlarmState` into the state the alarm is ALREADY in
//!   moves it nowhere, so it publishes nothing — and, because it is not a transition, it does
//!   not postpone the evaluation that takes a forced state back either ("Temporarily sets the
//!   state of an alarm for testing purposes. […] Metric alarms returns to their actual state
//!   quickly, often within seconds", `SetAlarmState`). A client that asks for one transition
//!   again and again — the loop every client retrying a transition runs — therefore gets it:
//!   the alarm returns to its actual state and the next ask moves it out of that state again.
//!   Without that, the first ask would hold the alarm in the asked-for state for as long as the
//!   client kept asking, and the transition it is waiting for would never happen.
//!
//! The rows differ along what the rules generalise over: the REGION the alarm and the rule live
//! in (so nothing may be addressed to one fixed Region), WHICH dimension of the envelope the
//! matching rule filters on (the source and detail type alone, the alarm's name inside
//! `detail`, the alarm's ARN in `resources`, the state value nested inside `detail`), WHICH
//! state the client re-asks for, and what the alarm's own series holds (nothing, or a datapoint
//! past its threshold) and so which state it returns to.
//!
//! The wire is the one this crate's harness defines; what is built here is the emulator with
//! EventBridge and Amazon SQS mounted beside CloudWatch, because the event is asserted where a
//! client sees it: on the queue the rule targets.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{rpc_in_region, Emulator, TARGET_PREFIX};

/// The `source` and `detail-type` of an alarm state change, spelled out here rather than
/// imported, so a wrong constant in the implementation cannot make this suite pass.
const SOURCE: &str = "aws.cloudwatch";
const DETAIL_TYPE: &str = "CloudWatch Alarm State Change";

/// The service principal EventBridge delivers to an SQS target as, which the queue's own policy
/// has to allow the send for.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The namespace and metric every row's alarm watches: a load balancer's target health, which
/// is a series a client of this service only ever READS — `PutMetricData` refuses an `AWS/`
/// namespace, so a row that needs a datapoint of its own watches [`OWN_NAMESPACE`] instead.
const NAMESPACE: &str = "AWS/ApplicationELB";
const METRIC: &str = "UnHealthyHostCount";

/// A namespace of the CALLER's own, for the row that publishes the datapoint its alarm is
/// graded on: the same metric and dimension pair, in a namespace a client may write.
const OWN_NAMESPACE: &str = "Custom/TargetHealth";

/// The period those alarms aggregate over, as the modelled seconds member carries it.
const PERIOD: i64 = 60;

/// How long a row waits for work the clock owes — a publication, its delivery, or the
/// evaluation that takes a forced state back — before giving up. Generous: what is asserted is
/// that it happens at all, never how fast the host runs.
const WORK_WAIT: Duration = Duration::from_secs(120);

/// How often it looks meanwhile.
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The state a new alarm reports, and the two a row forces.
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";
const ALARM: &str = "ALARM";
const OK: &str = "OK";

/// CloudWatch, EventBridge and Amazon SQS over ONE engine, as `core/router/src/lib.rs` mounts
/// them: the publication CloudWatch makes through the in-process callback reaches EventBridge's
/// own handler, which matches it against this account's rules and delivers it to the queue the
/// row reads.
fn emulator() -> Emulator {
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

/// One CloudWatch call signed for `region`, over this crate's own wire.
async fn monitoring(emulator: &Emulator, region: &str, operation: &str, body: &Value) -> Value {
    rpc_in_region(emulator, region, "monitoring", TARGET_PREFIX, operation, body).await
}

/// One Amazon SQS call signed for `region`: that service serves the same `awsJson` version this
/// one does, so the harness's wire serves it too.
async fn sqs(emulator: &Emulator, region: &str, operation: &str, body: &Value) -> Value {
    rpc_in_region(emulator, region, "sqs", "AmazonSQS", operation, body).await
}

/// One EventBridge call signed for `region`.
///
/// EventBridge serves `awsJson1_1` where this service serves `awsJson1_0`, so the request is
/// built here rather than through the shared wire: the content type is part of it.
async fn eventbridge(emulator: &Emulator, case: &str, region: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{region}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("AWSEvents.{operation}"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{region}/events/\
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

/// A queue a rule can target: its url and ARN, with its own policy allowing EventBridge the
/// send — the grant AWS documents for a queue a rule delivers to.
async fn given_target_queue(
    emulator: &Emulator,
    case: &str,
    region: &str,
    name: &str,
) -> (String, String) {
    let created = sqs(emulator, region, "CreateQueue", &json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        emulator,
        region,
        "GetQueueAttributes",
        &json!({"QueueUrl": &url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
        .to_string();
    let policy = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "AllowEventBridgeDelivery",
            "Effect": "Allow",
            "Principal": {"Service": EVENTS_PRINCIPAL},
            "Action": "sqs:SendMessage",
            "Resource": &arn,
        }],
    })
    .to_string();
    sqs(
        emulator,
        region,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// An alarm over a load balancer's target health, the way a client puts one: a metric of a
/// service's own namespace, dimensioned by the target group and the load balancer, with no
/// `TreatMissingData` of its own. Answers the ARN CloudWatch reports for it.
async fn given_alarm(
    emulator: &Emulator,
    case: &str,
    region: &str,
    name: &str,
    namespace: &str,
) -> String {
    monitoring(
        emulator,
        region,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": namespace,
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
    let described = monitoring(
        emulator,
        region,
        "DescribeAlarms",
        &json!({"AlarmNames": [name]}),
    )
    .await;
    described["MetricAlarms"][0]["AlarmArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected the alarm's ARN in {described}"))
        .to_string()
}

/// Publish one datapoint past the alarm `name`'s threshold into the series it watches, so the
/// state its own evaluation computes is `ALARM` rather than insufficient data.
async fn given_breaching_datapoint(
    emulator: &Emulator,
    region: &str,
    name: &str,
    namespace: &str,
) {
    monitoring(
        emulator,
        region,
        "PutMetricData",
        &json!({
            "Namespace": namespace,
            "MetricData": [{
                "MetricName": METRIC,
                "Dimensions": [
                    {"Name": "TargetGroup", "Value": format!("targetgroup/{name}/0123456789abcdef")},
                    {"Name": "LoadBalancer", "Value": format!("app/{name}/0123456789abcdef")},
                ],
                "Value": 2.0,
                "Timestamp": crate::harness::graded_window_stamp(PERIOD as f64),
            }],
        }),
    )
    .await;
}

/// Wire an ENABLED rule carrying `pattern` to the queue `queue_arn`.
async fn given_rule(
    emulator: &Emulator,
    case: &str,
    region: &str,
    name: &str,
    pattern: &Value,
    queue_arn: &str,
) {
    eventbridge(
        emulator,
        case,
        region,
        "PutRule",
        &json!({"Name": name, "EventPattern": pattern.to_string(), "State": "ENABLED"}),
    )
    .await;
    let wired = eventbridge(
        emulator,
        case,
        region,
        "PutTargets",
        &json!({"Rule": name, "Targets": [{"Id": "q", "Arn": queue_arn}]}),
    )
    .await;
    assert_eq!(
        wired["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected the queue target to be accepted, got {wired}"
    );
}

/// Ask for the alarm `name` to be in `state`.
async fn force_state(emulator: &Emulator, region: &str, name: &str, state: &str) {
    monitoring(
        emulator,
        region,
        "SetAlarmState",
        &json!({
            "AlarmName": name,
            "StateValue": state,
            "StateReason": "forced by this suite",
        }),
    )
    .await;
}

/// The `StateValue` `DescribeAlarms` reports for the alarm `name`.
async fn reported_state(emulator: &Emulator, region: &str, name: &str) -> String {
    let described = monitoring(
        emulator,
        region,
        "DescribeAlarms",
        &json!({"AlarmNames": [name]}),
    )
    .await;
    described["MetricAlarms"][0]["StateValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Everything waiting on the queue at `url`, as the parsed envelopes their bodies carry.
async fn drain(emulator: &Emulator, region: &str, url: &str) -> Vec<Value> {
    let received = sqs(
        emulator,
        region,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|message| {
            serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default()).ok()
        })
        .collect()
}

/// Whether `held` carries an envelope reporting the state `state`.
fn holds_state(held: &[Value], state: &str) -> bool {
    held.iter()
        .any(|event| event.pointer("/detail/state/value") == Some(&json!(state)))
}

/// Look again until an envelope reporting `state` has arrived on the queue at `url` or
/// [`WORK_WAIT`] has passed, answering everything that arrived meanwhile. The wait is for the
/// CONDITION against a wall-clock deadline, so a slow host delays a row instead of breaking it.
async fn until_state(emulator: &Emulator, region: &str, url: &str, state: &str) -> Vec<Value> {
    let started = Instant::now();
    let mut held: Vec<Value> = Vec::new();
    while started.elapsed() < WORK_WAIT {
        held.extend(drain(emulator, region, url).await);
        if holds_state(&held, state) {
            return held;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    held
}

/// One row of the delivery table: the Region everything lives in, and the pattern the matching
/// rule carries.
struct Row {
    case: &'static str,
    region: &'static str,
    /// The rule's pattern, over the alarm's name and its ARN.
    pattern: fn(&str, &str) -> Value,
}

/// RULE: every alarm transition reaches the target of a rule of the alarm's own Region whose
/// pattern matches the envelope, whichever dimension of it the pattern filters on — and reaches
/// nothing while no such rule exists.
#[tokio::test]
async fn an_alarm_transition_reaches_the_target_of_every_matching_rule() {
    let rows = [
        Row {
            case: "the source and detail type alone, us-west-2",
            region: "us-west-2",
            pattern: |_, _| json!({"source": [SOURCE], "detail-type": [DETAIL_TYPE]}),
        },
        Row {
            case: "the alarm's name inside the detail, us-west-2",
            region: "us-west-2",
            pattern: |name, _| json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
                "detail": {"alarmName": [name]},
            }),
        },
        Row {
            case: "the alarm's ARN in the event's resources, us-west-2",
            region: "us-west-2",
            pattern: |_, arn| json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
                "resources": [arn],
            }),
        },
        Row {
            case: "the state nested inside the detail, eu-west-1",
            region: "eu-west-1",
            pattern: |name, _| json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
                "detail": {"alarmName": [name], "state": {"value": [OK]}},
            }),
        },
        Row {
            case: "the source and detail type alone, us-east-1",
            region: "us-east-1",
            pattern: |_, _| json!({"source": [SOURCE], "detail-type": [DETAIL_TYPE]}),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let region = row.region;
        let emulator = emulator();
        let alarm_name = format!("target-health-{index}");
        let (url, queue_arn) =
            given_target_queue(&emulator, case, region, &format!("q-{index}")).await;
        let alarm_arn = given_alarm(&emulator, case, region, &alarm_name, NAMESPACE).await;

        // The negative half first: with no rule on the bus, the transitions this asks for are
        // delivered nowhere.
        force_state(&emulator, region, &alarm_name, ALARM).await;
        force_state(&emulator, region, &alarm_name, OK).await;
        let withheld = drain(&emulator, region, &url).await;
        assert!(
            withheld.is_empty(),
            "case {case:?}: with no rule on the bus the queue must receive nothing, got \
             {withheld:?}"
        );

        // Then the rule, and the same pair of transitions again.
        given_rule(
            &emulator,
            case,
            region,
            &format!("r-{index}"),
            &(row.pattern)(&alarm_name, &alarm_arn),
            &queue_arn,
        )
        .await;
        force_state(&emulator, region, &alarm_name, ALARM).await;
        force_state(&emulator, region, &alarm_name, OK).await;

        let arrived = until_state(&emulator, region, &url, OK).await;
        let delivered: Vec<&Value> = arrived
            .iter()
            .filter(|event| event.pointer("/detail/state/value") == Some(&json!(OK)))
            .collect();
        assert!(
            !delivered.is_empty(),
            "case {case:?}: expected an envelope reporting the OK transition of \
             {alarm_name:?} on {url}, got {arrived:?}"
        );
        for envelope in delivered {
            for (pointer, expected) in [
                ("/source", json!(SOURCE)),
                ("/detail-type", json!(DETAIL_TYPE)),
                ("/region", json!(region)),
                ("/resources", json!([&alarm_arn])),
                ("/detail/alarmName", json!(&alarm_name)),
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

/// One row of the retry table: which state the client keeps asking for, and whether the alarm's
/// own series holds a datapoint past its threshold — which decides the state the alarm returns
/// to between the asks.
struct RetryRow {
    case: &'static str,
    /// The state the client asks for, again and again.
    asked: &'static str,
    /// The namespace the alarm's series lives in.
    namespace: &'static str,
    /// Whether a breaching datapoint is published into the alarm's series.
    breaching: bool,
    /// The state the alarm's own evaluation takes it back to.
    returns_to: &'static str,
}

/// RULE: a client that asks for one transition again and again gets it — a `SetAlarmState` into
/// the state the alarm is already in moves it nowhere and so cannot postpone the evaluation that
/// takes a forced state back, so the alarm returns to its actual state and the next ask is a
/// transition that publishes its event.
///
/// This is the sequence a client runs when it wires a rule to an alarm it has already driven:
/// the transition is asked for once with nothing watching, and then asked for again, as often as
/// it takes, with the rule in place. Nothing here asks for the OTHER state in between.
#[tokio::test]
async fn a_client_re_asking_for_one_transition_still_sees_it_delivered() {
    let rows = [
        RetryRow {
            case: "OK re-asked on an alarm whose series holds nothing",
            asked: OK,
            namespace: NAMESPACE,
            breaching: false,
            returns_to: INSUFFICIENT_DATA,
        },
        RetryRow {
            case: "ALARM re-asked on an alarm whose series holds nothing",
            asked: ALARM,
            namespace: NAMESPACE,
            breaching: false,
            returns_to: INSUFFICIENT_DATA,
        },
        RetryRow {
            case: "OK re-asked on an alarm whose series holds a breaching datapoint",
            asked: OK,
            namespace: OWN_NAMESPACE,
            breaching: true,
            returns_to: ALARM,
        },
    ];

    let region = "us-west-2";
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let alarm_name = format!("re-asked-{index}");
        let (url, queue_arn) =
            given_target_queue(&emulator, case, region, &format!("q-re-asked-{index}")).await;
        let alarm_arn = given_alarm(&emulator, case, region, &alarm_name, row.namespace).await;
        if row.breaching {
            given_breaching_datapoint(&emulator, region, &alarm_name, row.namespace).await;
        }

        // The ask that happens before anything watches the bus, which is what leaves the alarm
        // in the asked-for state.
        force_state(&emulator, region, &alarm_name, row.asked).await;
        let reported = reported_state(&emulator, region, &alarm_name).await;
        assert_eq!(
            reported, row.asked,
            "case {case:?}: expected the state the client asked for to be reported, got \
             {reported:?}"
        );
        let withheld = drain(&emulator, region, &url).await;
        assert!(
            withheld.is_empty(),
            "case {case:?}: with no rule on the bus the queue must receive nothing, got \
             {withheld:?}"
        );

        given_rule(
            &emulator,
            case,
            region,
            &format!("r-re-asked-{index}"),
            &json!({
                "source": [SOURCE],
                "detail-type": [DETAIL_TYPE],
                "detail": {"alarmName": [&alarm_name]},
            }),
            &queue_arn,
        )
        .await;

        // The client's own loop: ask for the SAME state again, look at the queue, repeat until
        // the envelope is there or the deadline passes. Each ask is answered; only the one made
        // after the alarm has returned to its actual state is a transition, and that is the one
        // that publishes.
        let started = Instant::now();
        let mut held: Vec<Value> = Vec::new();
        while started.elapsed() < WORK_WAIT && !holds_state(&held, row.asked) {
            force_state(&emulator, region, &alarm_name, row.asked).await;
            held.extend(drain(&emulator, region, &url).await);
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
        let delivered: Vec<&Value> = held
            .iter()
            .filter(|event| event.pointer("/detail/state/value") == Some(&json!(row.asked)))
            .collect();
        assert!(
            !delivered.is_empty(),
            "case {case:?}: expected an envelope reporting the {:?} transition of \
             {alarm_name:?} on {url} while the client kept asking for it, got {held:?}",
            row.asked
        );
        // The transition it reports is a real one: the alarm moved out of the state its own
        // evaluation had taken it back to, rather than out of the state the client asked for.
        for envelope in delivered {
            for (pointer, expected) in [
                ("/resources", json!([&alarm_arn])),
                ("/detail/alarmName", json!(&alarm_name)),
                ("/detail/previousState/value", json!(row.returns_to)),
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
