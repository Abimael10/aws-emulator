// SPDX-License-Identifier: CC0-1.0

//! A COMPOSITE alarm whose children watch the series TWO OTHER services publish about their own
//! work — AWS Lambda's `Errors` for a function's invocations and Amazon SQS's visible-backlog
//! gauge for a queue — end to end with CloudWatch, AWS Lambda, CloudWatch Logs, SNS and SQS on
//! one engine, so what a client sees is what is asserted: the states `DescribeAlarms` reports and
//! the notification waiting on the queue subscribed to the parent's topic.
//!
//! The RULE asserted here: a composite alarm's state is "the rule that this alarm uses to
//! evaluate its alarm state" (`CompositeAlarm$AlarmRule`) applied to the CURRENT states of the
//! alarms that rule references, WHOEVER published the series those children are graded on — a
//! client's own datapoints or a series another service published about its own work — so the
//! parent moves exactly when the rule's value over its children changes, and a move executes the
//! action list of the state it moved into, which publishes the notification to the subscribers of
//! the topic that action names. The notification reports the alarm, the state moved to and out
//! of, the rule that was evaluated and every child the rule read a state from
//! (`AlarmRule`/`TriggeringChildren`), and carries no metric `Trigger`, a composite alarm having
//! no metric of its own.
//!
//! The rows differ along what that rule generalises over: which operator combines the two
//! children's state functions, which of the two services' work breaches (the function's failing
//! invocations, the queue's backlog, both, neither), and how much of it there is — so the same
//! wiring fires for one row and stays quiet for another. Each row's expected parent state is the
//! rule's own value over the children its work puts in `ALARM`, computed from the row rather than
//! taken from a recording.
//!
//! Every wait here is for a CONDITION — the state an evaluation owes, the notification a
//! transition owes — looked at again until it holds or a generous ceiling passes, so how fast the
//! host runs decides nothing asserted here. The rows are driven at the same time, each against
//! its own emulator, because each of them waits for the close of the period window its children
//! are graded over.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use service_com_amazonaws_sqs_amazonsqs::state::queue_depth_metrics::DEPTH_PUBLISH_INTERVAL;
use service_com_amazonaws_sqs_amazonsqs::state::ComAmazonawsSqsAmazonsqsState;

use crate::harness::{
    given_queue, given_topic_with_subscriber, received_messages, rpc, Emulator, LOOK_INTERVAL,
    REGION,
};
use crate::lambda_errors_alarm_notification::{
    emulator_with_lambda_and_notifications, given_function, invoke, ERRORS_METRIC,
    FUNCTION_DIMENSION, NAMESPACE as LAMBDA_NAMESPACE,
};
// The CloudWatch caller and the alarm reader the composite suites of this crate share: a parent
// and its children are read through ONE definition of that `DescribeAlarms` walk rather than one
// per suite.
use crate::composite_rule_over_published_children::{alarm_of, cw};

/// The series Amazon SQS publishes a queue's ready backlog on and the dimension it reports that
/// series per — the names of the Amazon SQS metric reference, written as literals so a series
/// published under a name no AWS reader asks for fails here instead of agreeing with itself.
const SQS_NAMESPACE: &str = "AWS/SQS";
const VISIBLE_METRIC: &str = "ApproximateNumberOfMessagesVisible";
const QUEUE_DIMENSION: &str = "QueueName";

/// The states an alarm reports (`StateValue`).
const ALARM: &str = "ALARM";
const OK: &str = "OK";

/// The period both children are graded over and how many of them each grades: the shortest
/// standard-resolution period `PutMetricAlarm` takes, with the one-period look-back a client
/// writes for "anything wrong now".
const PERIOD: i64 = 60;
const EVALUATION_PERIODS: i64 = 1;

/// The threshold both children compare their window against: any failure, and any ready message,
/// breaches.
const THRESHOLD: f64 = 0.0;

/// How long a row waits for work the clock owes — the evaluation that grades a period once it has
/// closed, the transition the graded states make due, the delivery that follows it — before
/// calling it undone.
///
/// A one-minute period is graded once it has CLOSED, so the state a row's work implies is owed
/// somewhere inside the minute after that work: longer than the spans the suites that publish
/// their own datapoints into an already-closed window wait, and so this suite's own. Generous on
/// purpose: it bounds a state or a message that never arrives, and nothing asserted here is about
/// how fast the host runs.
const PERIOD_GRADING_WAIT: Duration = Duration::from_secs(180);

/// How long a row watches for a delivery that must NOT be made, and for a state the parent must
/// HOLD, once both children have been graded: several of the alarm clock's own wake-ups. A bound
/// on an absence, so it is the one span a slow host may spend here without changing a verdict.
const DELIVERY_QUIET_WATCH: Duration = Duration::from_secs(5);

/// One Amazon SQS call, requiring a 200.
async fn sqs(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, "sqs", "AmazonSQS", operation, body).await
}

/// How a row's rule combines its two children's state functions: the operators an alarm rule is
/// written with ("You can use either alarm names or ARNs to reference the other alarms that are
/// to be evaluated", `PutCompositeAlarmInput$AlarmRule`).
#[derive(Debug, Clone, Copy)]
enum Operator {
    /// The parent follows EITHER child.
    Or,
    /// The parent follows BOTH children at once.
    And,
}

impl Operator {
    /// This operator as the rule spells it.
    fn text(self) -> &'static str {
        match self {
            Self::Or => "OR",
            Self::And => "AND",
        }
    }

    /// Whether the rule is true for children in those states — the value the parent must report.
    fn holds(self, errors_breached: bool, backlog_breached: bool) -> bool {
        match self {
            Self::Or => errors_breached || backlog_breached,
            Self::And => errors_breached && backlog_breached,
        }
    }
}

/// One row: the rule's operator, the work each service is given, and so which children breach.
struct Row {
    case: &'static str,
    operator: Operator,
    /// How many invocations of the function fail, which is what its `Errors` series counts.
    failing: usize,
    /// How many return, so the function's series carries a graded zero rather than a hole.
    returning: usize,
    /// How many messages are left ready on the watched queue, which is what its backlog gauge
    /// reads.
    messages: usize,
}

/// The rows: either child alone completing a disjunction, neither of them completing one, one
/// child alone failing to complete a conjunction, and both of them completing it.
const ROWS: &[Row] = &[
    Row {
        case: "a disjunction completed by the function's failing invocation",
        operator: Operator::Or,
        failing: 1,
        returning: 0,
        messages: 0,
    },
    Row {
        case: "a disjunction completed by the queue's backlog",
        operator: Operator::Or,
        failing: 0,
        returning: 1,
        messages: 2,
    },
    Row {
        case: "a disjunction whose children both stay under their thresholds",
        operator: Operator::Or,
        failing: 0,
        returning: 1,
        messages: 0,
    },
    Row {
        case: "a conjunction with only the function's invocations failing",
        operator: Operator::And,
        failing: 1,
        returning: 0,
        messages: 0,
    },
    Row {
        case: "a conjunction with failing invocations beside a backlog",
        operator: Operator::And,
        failing: 2,
        returning: 1,
        messages: 3,
    },
];

/// The metric alarm over the `Errors` series of `function`: the failures of a period against
/// [`THRESHOLD`], with no action of its own — the actions under test are the PARENT's.
fn errors_child(name: &str, function: &str) -> Value {
    json!({
        "AlarmName": name,
        "Namespace": LAMBDA_NAMESPACE,
        "MetricName": ERRORS_METRIC,
        "Dimensions": [{"Name": FUNCTION_DIMENSION, "Value": function}],
        "Statistic": "Sum",
        "Period": PERIOD,
        "EvaluationPeriods": EVALUATION_PERIODS,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanThreshold",
    })
}

/// The metric alarm over the ready-backlog gauge of `queue`: the gauge is one reading of the
/// queue per publish interval, so the window is folded by `Maximum` — the deepest the backlog
/// stood in it, which is what a backlog alarm is written against.
fn backlog_child(name: &str, queue: &str) -> Value {
    json!({
        "AlarmName": name,
        "Namespace": SQS_NAMESPACE,
        "MetricName": VISIBLE_METRIC,
        "Dimensions": [{"Name": QUEUE_DIMENSION, "Value": queue}],
        "Statistic": "Maximum",
        "Period": PERIOD,
        "EvaluationPeriods": EVALUATION_PERIODS,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanThreshold",
    })
}

/// Look at the alarm `name` of the `DescribeAlarms` list `list` until it reports `wanted`, and
/// answer it as it then stood — `MetricAlarms` for a child, `CompositeAlarms` for the parent.
///
/// The shared reader of that list is [`alarm_of`]; the wait around it is this suite's own, because
/// a child here is graded when its one-minute period CLOSES rather than when a datapoint this
/// suite placed in an already-closed window is published ([`PERIOD_GRADING_WAIT`]).
async fn until_alarm(
    emulator: &Emulator,
    case: &str,
    list: &str,
    name: &str,
    wanted: &str,
) -> Value {
    let deadline = Instant::now() + PERIOD_GRADING_WAIT;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        last = alarm_of(emulator, list, name).await;
        if last.get("StateValue").and_then(Value::as_str) == Some(wanted) {
            return last;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: {name:?} must reach {wanted}; it last reported {last}")
}

/// Take every notification waiting on the queue at `url`, deleting what it hands over so a later
/// look reads only what arrived since.
async fn take(emulator: &Emulator, case: &str, url: &str) -> Vec<Value> {
    let mut taken = Vec::new();
    for message in received_messages(emulator, url).await {
        let body = message["Body"].as_str().unwrap_or_default();
        let envelope: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: not the SNS envelope ({error}): {body}")
        });
        let inner = envelope["Message"].as_str().unwrap_or_default();
        taken.push(serde_json::from_str(inner).unwrap_or_else(|error| {
            panic!("case {case:?}: not an alarm notification ({error}): {inner}")
        }));
        if let Some(handle) = message["ReceiptHandle"].as_str() {
            sqs(
                emulator,
                "DeleteMessage",
                &json!({"QueueUrl": url, "ReceiptHandle": handle}),
            )
            .await;
        }
    }
    taken
}

/// Wait for one notification, then keep watching: a count asserted after that watch is a count of
/// what the transition really delivered rather than of what arrived first.
async fn the_one_notification(emulator: &Emulator, case: &str, url: &str) -> Value {
    let deadline = Instant::now() + PERIOD_GRADING_WAIT;
    let mut arrived: Vec<Value> = Vec::new();
    while arrived.is_empty() && Instant::now() < deadline {
        arrived.extend(take(emulator, case, url).await);
        if arrived.is_empty() {
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
    }
    let watch = Instant::now();
    while watch.elapsed() < DELIVERY_QUIET_WATCH {
        arrived.extend(take(emulator, case, url).await);
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert_eq!(
        arrived.len(),
        1,
        "case {case:?}: the transition must execute the parent's one action exactly once, got \
         {arrived:?}"
    );
    arrived.into_iter().next().expect("the one notification")
}

/// Take the next reading of every queue this store holds, so the backlog the sends left reaches
/// the `AWS/SQS` series without the row waiting out the publisher's own interval.
///
/// A queue's backlog is sampled once per publish interval, so the reading that follows a send made
/// in the same interval is the next one. It is made due through the engine seam — the recorded
/// reading moves one interval into the past, the way that service's own suite drives this clock —
/// and then one SQS call takes it, as every request of that service does on its way out. Nothing
/// about WHEN a reading happens is changed: the row still waits for the evaluation that grades it.
async fn next_queue_reading(emulator: &Emulator) {
    let store = emulator.engine.state::<ComAmazonawsSqsAmazonsqsState>();
    {
        let mut store = store.lock();
        for sampled in store.last_sampled_at.values_mut() {
            *sampled = sampled
                .checked_sub(DEPTH_PUBLISH_INTERVAL)
                .unwrap_or(*sampled);
        }
    }
    sqs(emulator, "ListQueues", &json!({})).await;
}

/// The ARNs of the children the parent's rule references, sorted — the identities its explanation
/// and its notification have to name, whatever state each child is in.
async fn child_arns(emulator: &Emulator, children: &[&str]) -> Vec<String> {
    let mut arns = Vec::with_capacity(children.len());
    for child in children {
        let alarm = alarm_of(emulator, "MetricAlarms", child).await;
        arns.push(alarm["AlarmArn"].as_str().unwrap_or_default().to_string());
    }
    arns.sort();
    arns
}

/// The `triggeringAlarms` of a composite alarm's `StateReasonData` ("An explanation for the alarm
/// state, in JSON format"), as (arn, state value) pairs, sorted: the snapshot of the children the
/// transition was decided over, as the alarm itself reports it. It is written only where the
/// state value changed, so it belongs to the same transition the notification reports.
fn triggering_alarms(case: &str, alarm: &Value) -> Vec<(String, String)> {
    let data = alarm["StateReasonData"].as_str().unwrap_or_default();
    let document: Value = serde_json::from_str(data).unwrap_or_else(|error| {
        panic!("case {case:?}: StateReasonData is not JSON ({error}): {data}")
    });
    let mut named: Vec<(String, String)> = document["triggeringAlarms"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                entry["arn"].as_str().unwrap_or_default().to_string(),
                entry["state"]["value"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    named.sort();
    named
}

/// The `TriggeringChildren` of a notification, as (arn, state value) pairs, sorted.
fn triggering_children(notification: &Value) -> Vec<(String, String)> {
    let mut named: Vec<(String, String)> = notification["TriggeringChildren"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|entry| {
            (
                entry["Arn"].as_str().unwrap_or_default().to_string(),
                entry["State"]["Value"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect();
    named.sort();
    named
}

/// RULE: a composite alarm's rule is read over the current states of the alarms it references
/// whoever published the series those children watch, so a parent over a Lambda `Errors` child
/// and an Amazon SQS backlog child moves exactly when its rule's value over them changes — and
/// the move executes the action list of the state it moved into, delivering the notification to
/// the subscribers of that action's topic.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_composite_rule_over_two_services_own_series_follows_their_work() {
    let mut rows = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        rows.push(tokio::spawn(drive(index, row)));
    }
    for row in rows {
        if let Err(failed) = row.await {
            std::panic::resume_unwind(failed.into_panic());
        }
    }
}

/// One row, against its own emulator.
async fn drive(index: usize, row: &'static Row) {
    let case = row.case;
    let emulator = emulator_with_lambda_and_notifications();

    // Where the parent's action delivers, and the queue whose backlog one of its children
    // watches: two queues, so the notification a transition delivers is no part of the backlog
    // the same transition was decided over.
    let (topic, notified) =
        given_topic_with_subscriber(&emulator, &format!("composite-notified-{index}")).await;
    let watched = format!("composite-backlog-{index}");
    let (backlog_url, _) = given_queue(&emulator, &watched).await;

    let function = format!("composite-errors-{index}");
    given_function(&emulator, case, &function).await;

    let errors = format!("errors-child-{index}");
    let backlog = format!("backlog-child-{index}");
    cw(&emulator, "PutMetricAlarm", &errors_child(&errors, &function)).await;
    cw(
        &emulator,
        "PutMetricAlarm",
        &backlog_child(&backlog, &watched),
    )
    .await;

    let parent = format!("composite-{index}");
    let rule = format!(
        "ALARM(\"{errors}\") {} ALARM(\"{backlog}\")",
        row.operator.text()
    );
    cw(
        &emulator,
        "PutCompositeAlarm",
        &json!({
            "AlarmName": &parent,
            "AlarmRule": &rule,
            "AlarmActions": [&topic],
        }),
    )
    .await;

    // "When this operation creates an alarm, the alarm state is immediately set to
    // INSUFFICIENT_DATA. The alarm is then evaluated and its state is set appropriately."
    // (`PutCompositeAlarm`) — with no child in ALARM the rule is false, so the parent settles in
    // OK, which is the state every row's transition is then measured from. It carries no OK
    // action, so that first evaluation delivers nothing.
    until_alarm(&emulator, case, "CompositeAlarms", &parent, OK).await;

    // The work the two children grade, each done through its own service's API.
    for _ in 0..row.failing {
        invoke(&emulator, case, &function, true).await;
    }
    for _ in 0..row.returning {
        invoke(&emulator, case, &function, false).await;
    }
    for ordinal in 0..row.messages {
        sqs(
            &emulator,
            "SendMessage",
            &json!({
                "QueueUrl": &backlog_url,
                "MessageBody": format!("backlog {ordinal}"),
            }),
        )
        .await;
    }
    next_queue_reading(&emulator).await;

    // Both children, graded on the work their own service published.
    let errors_breached = row.failing > 0;
    let backlog_breached = row.messages > 0;
    let errors_state = if errors_breached { ALARM } else { OK };
    let backlog_state = if backlog_breached { ALARM } else { OK };
    until_alarm(&emulator, case, "MetricAlarms", &errors, errors_state).await;
    until_alarm(&emulator, case, "MetricAlarms", &backlog, backlog_state).await;

    if !row.operator.holds(errors_breached, backlog_breached) {
        // The rule stays false, so the parent holds the state its first evaluation computed and
        // its action is executed for nothing.
        let watch = Instant::now();
        let mut arrived: Vec<Value> = Vec::new();
        while watch.elapsed() < DELIVERY_QUIET_WATCH {
            arrived.extend(take(&emulator, case, &notified).await);
            let held = alarm_of(&emulator, "CompositeAlarms", &parent).await;
            assert_eq!(
                held.get("StateValue").and_then(Value::as_str),
                Some(OK),
                "case {case:?}: the rule {rule:?} is false over its children, so the parent must \
                 hold {OK}, it reports {held}"
            );
            tokio::time::sleep(LOOK_INTERVAL).await;
        }
        assert!(
            arrived.is_empty(),
            "case {case:?}: a parent that never moved must deliver nothing, got {arrived:?}"
        );
        return;
    }

    let moved = until_alarm(&emulator, case, "CompositeAlarms", &parent, ALARM).await;
    let notification = the_one_notification(&emulator, case, &notified).await;
    for (member, expected) in [
        ("AlarmName", json!(&parent)),
        ("AlarmArn", moved["AlarmArn"].clone()),
        ("NewStateValue", json!(ALARM)),
        ("OldStateValue", json!(OK)),
        ("NewStateReason", moved["StateReason"].clone()),
        ("AlarmRule", json!(&rule)),
        ("Region", json!(REGION)),
    ] {
        let actual = notification.get(member).cloned().unwrap_or(Value::Null);
        assert_eq!(
            actual, expected,
            "case {case:?}: the notification's {member} must report the transition as \
             {expected}, got {actual} in {notification}"
        );
    }

    // Every child the rule READ is named: the rule references both, so both are reported — each
    // with the state it was in at the instant the parent moved, which for a child this row left
    // ungraded until later is the state it still held then.
    let reported = triggering_children(&notification);
    let read: Vec<String> = reported.iter().map(|(arn, _)| arn.clone()).collect();
    let referenced = child_arns(&emulator, &[errors.as_str(), backlog.as_str()]).await;
    assert_eq!(
        read, referenced,
        "case {case:?}: TriggeringChildren must name every child the rule read, got \
         {notification}"
    );
    // Those states are the snapshot the parent's own explanation data carries: one reading of the
    // transition, rendered twice.
    assert_eq!(
        reported,
        triggering_alarms(case, &moved),
        "case {case:?}: TriggeringChildren must carry the states the parent's StateReasonData \
         names, got {notification} beside {moved}"
    );
    // "An explanation for the alarm state, in text format" (`CompositeAlarm$StateReason`) opens
    // with the transition that made the rule be read again — the latest of the children's own,
    // named with the state that child moved to — so it is asserted over the children the same
    // envelope reports, rather than over a guess at which of them moved last.
    let reason = moved["StateReason"].as_str().unwrap_or_default().to_string();
    let openings: Vec<String> = reported
        .iter()
        .map(|(arn, state)| format!("{arn} transitioned to {state}"))
        .collect();
    assert!(
        openings.iter().any(|opening| reason.starts_with(opening)),
        "case {case:?}: the explanation must open with the transition of a child the rule read \
         (one of {openings:?}), got {reason:?}"
    );
    assert!(
        notification.get("Trigger").is_none(),
        "case {case:?}: a composite alarm has no metric, so its notification carries no \
         Trigger, got {notification}"
    );
}
