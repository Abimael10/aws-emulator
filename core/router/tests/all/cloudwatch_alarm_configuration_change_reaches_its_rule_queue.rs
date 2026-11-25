// SPDX-License-Identifier: CC0-1.0

//! What a client governing alarms reads off the bus when an alarm's CONFIGURATION changes —
//! over the WHOLE assembled emulator, the service set the binary serves.
//!
//! Two rules, both read by the same cross-service pipeline (CloudWatch publishes, EventBridge
//! matches, Amazon SQS holds what was delivered):
//!
//! 1. **An alarm event's `configuration` states WHAT the alarm evaluates as `metrics`, the list
//!    of metric-data queries the alarm holds — the ONE query its metric IS for an alarm
//!    configured with a single metric — each entry naming its metric by `namespace`, `name` and
//!    its dimensions beside the `period` it is aggregated over and the `stat` it is aggregated
//!    with, and the configuration an update REPLACED states its selection the same way.** "One
//!    item in the Metrics array is the expression that the alarm watches"
//!    (`PutMetricAlarmInput$Metrics`) is the selection a query-array alarm states for itself; an
//!    alarm configured with `Namespace`, `MetricName`, `Statistic`/`ExtendedStatistic` and
//!    `Period` states the same selection in the single-metric form, so the event reports it as
//!    the one query that metric is and a consumer reads one shape either way. The id of a query
//!    the alarm itself did not name is CloudWatch's own, so it is asserted as an identifier that
//!    is there, never as a literal.
//! 2. **Every call that leaves an alarm's configuration different from the one the account held
//!    publishes exactly one `CloudWatch Alarm Configuration Change`, promptly and for every such
//!    call including repeated writes to the same alarm, while a call that leaves the
//!    configuration as it was publishes none** — a forced state is the alarm's state rather than
//!    its configuration and publishes the state change alone ("when the updated state differs
//!    from the previous value, the action configured for the appropriate state is invoked",
//!    `SetAlarmState`), a tagging call changes something that is not the configuration at all
//!    ("any tags you specify in this parameter are ignored. To change the tags of an existing
//!    alarm, use TagResource or UntagResource", `PutMetricAlarmInput$Tags`), and a write that
//!    re-states the stored configuration overwrites it with itself ("the update completely
//!    overwrites the previous configuration of the alarm", `PutMetricAlarm`).
//!
//! What this suite adds to the crate-local suites that pin the same two rules
//! (`services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/tests/all/alarm_configuration_change_metric_selection.rs`
//! and `alarm_write_emission_scope.rs`, which mount CloudWatch, EventBridge and Amazon SQS
//! alone) is the axis neither can reach: the publication CloudWatch makes through the emulator's
//! own callback has to be claimed, routed and delivered by whichever mounted model a client's
//! `events`- and `sqs`-scoped calls reach when EVERY service is mounted — and it drives the
//! shape a consumer governing alarms actually drives: a RUN of consecutive writes to one alarm,
//! each owing its own event.
//!
//! The rows differ along what the rules generalise over: how the alarm's single metric is
//! selected (no dimensions under a statistic, one dimension under an extended statistic over a
//! longer period), how many consecutive writes the run makes, which operation made the call
//! (`PutMetricAlarm`, `SetAlarmState`, `TagResource`, `DisableAlarmActions`), and whether the
//! call changed anything at all.
//!
//! Nothing here is asserted about how fast the host runs: an event is waited for by looking again
//! until it has arrived, and the absences are graded against the count of events the rule owes.
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! requests driven — on an explicitly sized thread. The assembly and the `Authorization` that
//! routes a call are the ones [`crate::cloudwatch_alarm_action_reaches_its_subscribed_queue`]
//! already holds for this service pair.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router};

/// Keep this at or above the size `boot_serve.rs` uses: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every call below is addressed in, and so the `region` an envelope must report.
const REGION: &str = "us-west-2";

/// The `source` and the two `detail-type`s an alarm's events are published under, spelled out
/// here rather than imported, so a wrong constant in the implementation cannot make this suite
/// pass.
const SOURCE: &str = "aws.cloudwatch";
const CONFIGURATION_CHANGE: &str = "CloudWatch Alarm Configuration Change";
const STATE_CHANGE: &str = "CloudWatch Alarm State Change";

/// The `detail.operation` a write that created an alarm and one that replaced its configuration
/// report.
const CREATE: &str = "create";
const UPDATE: &str = "update";

/// The service principal EventBridge delivers to an SQS target as, which the queue's own policy
/// has to allow the send for.
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// CloudWatch's `X-Amz-Target` prefix and signing name.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The series the alarms below watch: a namespace of the client's own, which nothing publishes
/// into, so no evaluation ever moves one of them and every event on a queue is one a call of this
/// suite made.
const NAMESPACE: &str = "Custom/AlarmGovernance";
const METRIC: &str = "ConfigurationProbe";

/// The thresholds the run of consecutive writes stores, in order: the first write creates the
/// alarm and each later one replaces the threshold alone, which is how each write's own event is
/// told apart from its neighbours'.
const THRESHOLDS: [f64; 5] = [5.0, 7.5, 10.0, 12.5, 15.0];

/// The state the emit-scope half forces the alarm into.
const FORCED_STATE: &str = "ALARM";

/// How long a wait for a publication and its delivery keeps looking before reporting that it
/// never arrived, and how often it looks meanwhile. Generous against a slow host — the suite's
/// own deliveries land in milliseconds — and bounded so a real regression, where nothing is
/// published at all, is reported promptly rather than once per minute.
const DELIVERY_WAIT: Duration = Duration::from_secs(30);
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long the emit-scope half keeps looking after the witness event has arrived, to catch an
/// event a call that changed nothing published late. A bound on an absence, so it is the one span
/// a slow host may spend without changing the verdict.
const WITHHELD_WINDOW: Duration = Duration::from_secs(5);

/// One `X-Amz-Target` RPC call that has to be taken, answering the parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name, REGION))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One CloudWatch call (`awsJson1_0`).
async fn monitoring(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    rpc(
        router,
        case,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        "1.0",
        operation,
        body,
    )
    .await
}

/// One EventBridge call (`awsJson1_1`).
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One Amazon SQS call (`awsJson1_0`).
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// A queue a rule can target: its url and the ARN a rule names it by, with its own policy
/// allowing EventBridge the send.
async fn given_target_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({"QueueName": name})).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
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
        router,
        case,
        "SetQueueAttributes",
        &json!({"QueueUrl": &url, "Attributes": {"Policy": policy}}),
    )
    .await;
    (url, arn)
}

/// Wire a rule matching BOTH event families of the alarm `alarm` to the queue `queue_arn`: one
/// queue holds everything the account published about that alarm, which is how a call is asserted
/// to have published the one family it owes and nothing of the other.
async fn given_rule_for_both_families(
    router: &MultiServiceRouter,
    case: &str,
    rule: &str,
    alarm: &str,
    queue_arn: &str,
) {
    let pattern = json!({
        "source": [SOURCE],
        "detail-type": [CONFIGURATION_CHANGE, STATE_CHANGE],
        "detail": {"alarmName": [alarm]},
    })
    .to_string();
    events(
        router,
        case,
        "PutRule",
        &json!({"Name": rule, "EventPattern": pattern, "State": "ENABLED"}),
    )
    .await;
    let wired = events(
        router,
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

/// Receive everything waiting on the queue at `url`, delete it and add the envelopes its message
/// bodies carried to `held`.
async fn collect(router: &MultiServiceRouter, case: &str, url: &str, held: &mut Vec<Value>) {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                router,
                case,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
        if let Ok(parsed) =
            serde_json::from_str::<Value>(message["Body"].as_str().unwrap_or_default())
        {
            held.push(parsed);
        }
    }
}

/// The events among `held` carrying `detail_type`, in the order they were delivered.
fn of_family<'held>(held: &'held [Value], detail_type: &str) -> Vec<&'held Value> {
    held.iter()
        .filter(|event| event["detail-type"] == json!(detail_type))
        .collect()
}

/// Look at the queue at `url` until `wanted` events of the family `detail_type` have been
/// delivered, adding everything delivered meanwhile to `held`.
///
/// The wait is for the CONDITION rather than for a span: it looks again until the events are
/// there, and an event that never arrives is reported as that — so a missing publication is told
/// apart from one delivered with the wrong payload.
async fn until_family_delivered(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    detail_type: &str,
    wanted: usize,
    held: &mut Vec<Value>,
) {
    let deadline = Instant::now() + DELIVERY_WAIT;
    loop {
        collect(router, case, url, held).await;
        if of_family(held, detail_type).len() >= wanted {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} {detail_type:?} event(s) on {url}, got {} — the \
             rest never arrived: {held:?}",
            of_family(held, detail_type).len()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// One row: an alarm whose single metric is selected one way, and the query the events it
/// publishes must report that selection as.
struct Row {
    /// What this row is about.
    case: &'static str,
    /// The alarm this row writes, under its own name so one row's event is never another's.
    alarm: &'static str,
    /// The `Dimensions` the alarm is written with.
    dimensions: Value,
    /// The dimension map the event reports for the metric.
    reported_dimensions: Value,
    /// The member that aggregates the metric, and its value: a statistic or an extended one.
    aggregation: (&'static str, &'static str),
    /// The `Period` the metric is aggregated over, as the modelled seconds member carries it.
    period: i64,
}

/// The shapes of single-metric selection this suite spans.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an alarm over a metric with no dimensions, under a statistic",
            alarm: "governance-no-dimensions",
            dimensions: json!([]),
            reported_dimensions: json!({}),
            aggregation: ("Statistic", "Average"),
            period: 60,
        },
        Row {
            case: "an alarm over a metric with one dimension, under an extended statistic over a \
                   longer period",
            alarm: "governance-one-dimension",
            dimensions: json!([{"Name": "Stage", "Value": "prod"}]),
            reported_dimensions: json!({"Stage": "prod"}),
            aggregation: ("ExtendedStatistic", "p90"),
            period: 300,
        },
    ]
}

/// The `PutMetricAlarm` request of `row` storing `threshold`.
fn put_metric_alarm(row: &Row, threshold: f64) -> Value {
    let mut body = json!({
        "AlarmName": row.alarm,
        "AlarmDescription": "the alarm of this row",
        "Namespace": NAMESPACE,
        "MetricName": METRIC,
        "Dimensions": row.dimensions.clone(),
        "Period": row.period,
        "EvaluationPeriods": 1,
        "Threshold": threshold,
        "ComparisonOperator": "GreaterThanThreshold",
        "TreatMissingData": "missing",
        "ActionsEnabled": true,
    });
    body[row.aggregation.0] = json!(row.aggregation.1);
    body
}

/// The `metrics` the events of `row` must report: the ONE query the alarm's single metric is.
fn expected_metrics(row: &Row) -> Value {
    json!([{
        "metricStat": {
            "metric": {
                "namespace": NAMESPACE,
                "name": METRIC,
                "dimensions": row.reported_dimensions.clone(),
            },
            "period": row.period,
            "stat": row.aggregation.1,
        },
        "returnData": true,
    }])
}

/// The selection reported at `pointer` of `event`, ready to compare against
/// [`expected_metrics`]: the id of a query the alarm itself did not name is CloudWatch's own, so
/// it is asserted to be there and then taken out of the comparison.
fn comparable_metrics(case: &str, event: &Value, pointer: &str) -> Value {
    let reported = event
        .pointer(pointer)
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a metric selection at {pointer} of {event}")
        });
    let comparable: Vec<Value> = reported
        .iter()
        .map(|entry| {
            let mut entry = entry.clone();
            let id = entry
                .get("id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            assert!(
                !id.is_empty(),
                "case {case:?}: expected the query CloudWatch reports for the alarm's own metric \
                 to carry an id, got {entry} in {event}"
            );
            if let Some(members) = entry.as_object_mut() {
                members.remove("id");
            }
            entry
        })
        .collect();
    Value::Array(comparable)
}

/// The envelope members every configuration change of this suite carries.
fn assert_envelope(case: &str, event: &Value, alarm: &str, alarm_arn: &str, operation: &str) {
    for (pointer, expected) in [
        ("/source", json!(SOURCE)),
        ("/detail-type", json!(CONFIGURATION_CHANGE)),
        ("/region", json!(REGION)),
        ("/account", json!(engine::EMULATOR_ACCOUNT_ID)),
        ("/resources", json!([alarm_arn])),
        ("/detail/alarmName", json!(alarm)),
        ("/detail/operation", json!(operation)),
    ] {
        let actual = event.pointer(pointer).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {pointer} to be {expected}, got {actual} in {event}"
        );
    }
}

/// The threshold the configuration at `pointer` of `event` reports.
fn reported_threshold(event: &Value, pointer: &str) -> Option<f64> {
    event
        .pointer(&format!("{pointer}/threshold"))
        .and_then(Value::as_f64)
}

/// RULES 1 and 2, over the assembled emulator: a RUN of consecutive writes to one alarm has each
/// write publish its own configuration change, reporting the metric selection the alarm evaluates
/// beside the configuration that write replaced — and the calls that change no configuration
/// publish none.
#[test]
fn every_alarm_write_reaches_its_rule_s_queue_reporting_the_metric_it_watches() {
    std::thread::Builder::new()
        .name("alarm-configuration-change".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(drive_configuration_changes());
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread runs to completion");
}

/// Every row, against ONE assembled router.
async fn drive_configuration_changes() {
    let router = router();
    for row in rows() {
        let case = row.case;
        let (url, queue_arn) = given_target_queue(&router, case, &format!("{}-q", row.alarm)).await;
        // The rule is wired BEFORE the alarm exists, so the event the create publishes is matched
        // by a rule that was already live.
        given_rule_for_both_families(
            &router,
            case,
            &format!("{}-rule", row.alarm),
            row.alarm,
            &queue_arn,
        )
        .await;

        let mut held: Vec<Value> = Vec::new();
        let mut alarm_arn = String::new();
        for (index, threshold) in THRESHOLDS.iter().enumerate() {
            let write_case = format!("{case}, the write storing the threshold {threshold}");
            monitoring(
                &router,
                &write_case,
                "PutMetricAlarm",
                &put_metric_alarm(&row, *threshold),
            )
            .await;
            if alarm_arn.is_empty() {
                let described = monitoring(
                    &router,
                    &write_case,
                    "DescribeAlarms",
                    &json!({"AlarmNames": [row.alarm]}),
                )
                .await;
                alarm_arn = described["MetricAlarms"][0]["AlarmArn"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!("case {write_case:?}: expected the alarm's ARN in {described}")
                    })
                    .to_string();
            }

            until_family_delivered(
                &router,
                &write_case,
                &url,
                CONFIGURATION_CHANGE,
                index + 1,
                &mut held,
            )
            .await;
            let delivered = of_family(&held, CONFIGURATION_CHANGE);
            assert_eq!(
                delivered.len(),
                index + 1,
                "case {write_case:?}: expected one configuration change per write, so {} by now, \
                 got {held:?}",
                index + 1
            );

            // The event of THIS write, told apart from its neighbours' by the threshold it
            // stored: one write, one event.
            let matching: Vec<&Value> = delivered
                .iter()
                .copied()
                .filter(|event| {
                    reported_threshold(event, "/detail/configuration") == Some(*threshold)
                })
                .collect();
            assert_eq!(
                matching.len(),
                1,
                "case {write_case:?}: expected exactly one event reporting that threshold, got \
                 {delivered:?}"
            );
            let event = matching[0];
            let operation = if index == 0 { CREATE } else { UPDATE };
            assert_envelope(&write_case, event, row.alarm, &alarm_arn, operation);

            let reported = comparable_metrics(&write_case, event, "/detail/configuration/metrics");
            let expected = expected_metrics(&row);
            assert_eq!(
                reported, expected,
                "case {write_case:?}: expected the configuration to report the metric selection \
                 {expected}, got {reported} in {event}"
            );

            // An update reports the configuration it REPLACED in the same shape; a create
            // replaced none and reports none.
            match index.checked_sub(1).map(|previous| THRESHOLDS[previous]) {
                Some(replaced) => {
                    let before = reported_threshold(event, "/detail/previousConfiguration");
                    assert_eq!(
                        before,
                        Some(replaced),
                        "case {write_case:?}: expected the replaced configuration to report the \
                         threshold {replaced}, got {before:?} in {event}"
                    );
                    let before = comparable_metrics(
                        &write_case,
                        event,
                        "/detail/previousConfiguration/metrics",
                    );
                    assert_eq!(
                        before, expected,
                        "case {write_case:?}: expected the replaced configuration to report the \
                         same metric selection {expected}, got {before} in {event}"
                    );
                }
                None => {
                    let before = event.pointer("/detail/previousConfiguration");
                    assert_eq!(
                        before, None,
                        "case {write_case:?}: a create replaced no configuration, so it reports \
                         none, got {before:?} in {event}"
                    );
                }
            }
        }

        // The emit scope, against the alarm those writes left: three calls that leave the
        // configuration as it was, and then one that really changes it as the witness their
        // silence is graded against.
        let last = THRESHOLDS[THRESHOLDS.len() - 1];
        let quiet: Vec<(&str, &str, Value)> = vec![
            (
                "a call that forces the alarm's state",
                "SetAlarmState",
                json!({
                    "AlarmName": row.alarm,
                    "StateValue": FORCED_STATE,
                    "StateReason": "forced by this suite",
                }),
            ),
            (
                "a call that tags the alarm",
                "TagResource",
                json!({
                    "ResourceARN": &alarm_arn,
                    "Tags": [{"Key": "team", "Value": "platform"}],
                }),
            ),
            (
                "a write that re-states the stored configuration",
                "PutMetricAlarm",
                put_metric_alarm(&row, last),
            ),
        ];
        for (quiet_case, operation, body) in &quiet {
            monitoring(&router, &format!("{case}, {quiet_case}"), operation, body).await;
        }
        monitoring(
            &router,
            &format!("{case}, the call that turns the alarm's actions off"),
            "DisableAlarmActions",
            &json!({"AlarmNames": [row.alarm]}),
        )
        .await;

        let owed = THRESHOLDS.len() + 1;
        until_family_delivered(&router, case, &url, CONFIGURATION_CHANGE, owed, &mut held).await;
        // The witness arrived, so every call before it has been served; from here the count may
        // only stay where it is, which is what the looks below grade.
        let watching = Instant::now();
        loop {
            let delivered = of_family(&held, CONFIGURATION_CHANGE);
            assert_eq!(
                delivered.len(),
                owed,
                "case {case:?}: expected one configuration change per call that changed the \
                 configuration — the {} writes and the actions switch — and none for the forced \
                 state, the tagging and the write that changed nothing, got {held:?}",
                THRESHOLDS.len()
            );
            if watching.elapsed() >= WITHHELD_WINDOW {
                break;
            }
            tokio::time::sleep(LOOK_INTERVAL).await;
            collect(&router, case, &url, &mut held).await;
        }

        // The switch's own event is the only one reporting the setting it stored, and the forced
        // state published the other family instead of this one.
        let switched: Vec<&Value> = of_family(&held, CONFIGURATION_CHANGE)
            .into_iter()
            .filter(|event| {
                event.pointer("/detail/configuration/actionsEnabled") == Some(&json!(false))
            })
            .collect();
        assert_eq!(
            switched.len(),
            1,
            "case {case:?}: expected the actions switch to publish the one event reporting the \
             setting it stored, got {switched:?}"
        );
        let forced: Vec<&Value> = of_family(&held, STATE_CHANGE)
            .into_iter()
            .filter(|event| event.pointer("/detail/state/value") == Some(&json!(FORCED_STATE)))
            .collect();
        assert_eq!(
            forced.len(),
            1,
            "case {case:?}: expected the forced state to publish one state change reporting \
             {FORCED_STATE:?}, got {:?}",
            of_family(&held, STATE_CHANGE)
        );
    }
}
