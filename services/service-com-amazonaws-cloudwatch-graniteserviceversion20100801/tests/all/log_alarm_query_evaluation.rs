// SPDX-License-Identifier: CC0-1.0

//! What DRIVES a log alarm: the executions of the CloudWatch Logs scheduled query this service
//! manages for it, the aggregate each execution computes over the lines really published, and the
//! M-out-of-N fold that turns those aggregates into the alarm's state, its history and its
//! notifications.
//!
//! The rule under test is the one `PutLogAlarm` documents: "Each scheduled query execution
//! returns one or more aggregated values determined by the AggregationExpression, and each
//! aggregated value is compared against the alarm Threshold to determine the alarm state. The
//! alarm uses M-out-of-N evaluation: if QueryResultsToAlarm out of the most recent
//! QueryResultsToEvaluate query results breach the threshold, the alarm transitions to ALARM."
//!
//! The first table's rows differ along what that rule generalises over — the aggregation
//! expression, the comparison operator, the lines published and so the value the aggregate takes
//! — rather than repeating one recorded request; the end-to-end suite then follows one alarm's
//! whole observable chain, from the managed scheduled query CloudWatch Logs resolves to the
//! notification a queue subscribed to the alarm's topic receives.
//!
//! CloudWatch Logs is mounted beside CloudWatch, SNS and SQS on ONE engine, as
//! `core/router/src/lib.rs` mounts them: the query runs where the log events are, and the
//! notification is asserted where a client sees it.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{element, grant_sns_delivery, now_secs, rpc, sns, Emulator, QUERY_ROLE, REGION};

/// The CloudWatch signing name and target prefix, for `rpc`.
const CLOUDWATCH: (&str, &str) = ("monitoring", crate::harness::TARGET_PREFIX);

/// The SQS signing name and target prefix.
const SQS: (&str, &str) = ("sqs", "AmazonSQS");

/// The CloudWatch Logs signing name and target prefix.
const LOGS: (&str, &str) = ("logs", "Logs_20140328");

/// How many times a wait looks again before it gives up.
///
/// Each execution of an alarm's managed query is owed a span of its own
/// (`state::log_alarm_evaluation`'s `QUERY_EXECUTION_SPAN_SECS`) and an alarm that rests in `OK`
/// needs as many of them as it evaluates, so a chain of them needs several spans. The budget is
/// deliberately generous: what every assertion below waits for is a CONDITION, never a span of
/// wall clock, and a wait ends as soon as it holds.
const LOOKS: usize = 900;

/// How long one look waits before the next.
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The lookback every alarm in this suite queries over, in seconds: wide enough that every line
/// the case published is inside the window of the first execution
/// (`ScheduleConfiguration$StartTimeOffset`).
const START_TIME_OFFSET: i64 = 300;

/// CloudWatch, CloudWatch Logs, SNS and SQS over ONE shared engine.
fn emulator_with_logs() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    Emulator { router, engine }
}

/// One CloudWatch call that must answer 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, CLOUDWATCH.0, CLOUDWATCH.1, operation, body).await
}

/// One SQS call that must answer 200.
async fn sqs(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, SQS.0, SQS.1, operation, body).await
}

/// One CloudWatch Logs call that must answer 200.
async fn logs(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, LOGS.0, LOGS.1, operation, body).await
}

/// A log group holding `messages` as one log event each, stamped at the instant this runs.
async fn given_log_group(emulator: &Emulator, name: &str, messages: &[&str]) {
    logs(emulator, "CreateLogGroup", &json!({"logGroupName": name})).await;
    logs(
        emulator,
        "CreateLogStream",
        &json!({"logGroupName": name, "logStreamName": "s"}),
    )
    .await;
    #[allow(clippy::cast_possible_truncation)]
    let at = (now_secs() * 1_000.0) as i64;
    let events: Vec<Value> = messages
        .iter()
        .enumerate()
        .map(|(index, message)| {
            json!({"timestamp": at - (messages.len() - index) as i64, "message": message})
        })
        .collect();
    let accepted = logs(
        emulator,
        "PutLogEvents",
        &json!({"logGroupName": name, "logStreamName": "s", "logEvents": events}),
    )
    .await;
    assert!(
        accepted.get("rejectedLogEventsInfo").is_none(),
        "the lines of {name} must all be accepted, got {accepted}"
    );
}

/// The log alarm one case configures over `log_group`.
#[allow(clippy::too_many_arguments)]
fn log_alarm_request(
    name: &str,
    log_group: &str,
    query_string: &str,
    aggregation: &str,
    threshold: f64,
    comparison: &str,
    results_to_evaluate: i32,
    results_to_alarm: i32,
    actions: &[&str],
) -> Value {
    json!({
        "AlarmName": name,
        "QueryResultsToEvaluate": results_to_evaluate,
        "QueryResultsToAlarm": results_to_alarm,
        "Threshold": threshold,
        "ComparisonOperator": comparison,
        "AlarmActions": actions,
        "OKActions": actions,
        "ScheduledQueryConfiguration": {
            "QueryString": query_string,
            "LogGroupIdentifiers": [log_group],
            "ScheduledQueryRoleARN": QUERY_ROLE,
            "AggregationExpression": aggregation,
            "ScheduleConfiguration": {
                "ScheduleExpression": "rate(1 minute)",
                "StartTimeOffset": START_TIME_OFFSET,
                "EndTimeOffset": 0,
            },
        },
    })
}

/// The log alarm `DescribeAlarms` reports under `name`.
async fn described(emulator: &Emulator, name: &str) -> Value {
    let reported = cw(
        emulator,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": ["LogAlarm"]}),
    )
    .await;
    reported["LogAlarms"][0].clone()
}

/// Read the alarm until it reports `state`, and answer it; panic with what it last reported.
///
/// An execution of the alarm's managed query is owed a span after the write, and the fold over
/// what it produced happens when it has really run — so this waits for the CONDITION and awaits
/// while it does, which is what lets the service's own alarm clock run.
async fn until_state(emulator: &Emulator, case: &str, name: &str, state: &str) -> Value {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        last = described(emulator, name).await;
        if last.get("StateValue").and_then(Value::as_str) == Some(state) {
            return last;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: {name} must reach {state}; it last reported {last}")
}

/// The `StateReasonData` of a described alarm, parsed.
fn reason_data(case: &str, alarm: &Value) -> Value {
    let text = alarm
        .get("StateReasonData")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a StateReasonData, got {alarm}"));
    serde_json::from_str(text)
        .unwrap_or_else(|_| panic!("case {case:?}: StateReasonData must be JSON, got {text}"))
}

/// The results a `StateReasonData` document reports.
fn results(case: &str, alarm: &Value) -> Vec<Value> {
    reason_data(case, alarm)["results"]
        .as_array()
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected results in {alarm}"))
}

/// One row of the aggregation table: what the case is, the lines it publishes, the query, the
/// aggregation, the threshold and comparison, and the state and aggregate it must reach.
struct Row<'a> {
    /// What this row is about.
    case: &'a str,
    /// The lines the row publishes into its own log group.
    messages: &'a [&'a str],
    /// "The CloudWatch Logs query to execute on each scheduled run."
    query_string: &'a str,
    /// "The expression that defines how to aggregate query results into one or more scalar
    /// values for alarm evaluation." (`ScheduledQueryConfiguration$AggregationExpression`)
    aggregation: &'a str,
    /// "The value to compare with the aggregated query result." (`LogAlarm$Threshold`)
    threshold: f64,
    /// The arithmetic the aggregate is compared with.
    comparison: &'a str,
    /// The state the aggregate puts the alarm in.
    state: &'a str,
    /// The value every one of the alarm's query results must carry.
    value: f64,
}

/// The lines the counting rows publish: seven that match the filter and three that do not.
const BREACHING_LINES: [&str; 10] = [
    "BREACH 1", "calm 1", "BREACH 2", "BREACH 3", "calm 2", "BREACH 4", "BREACH 5", "calm 3",
    "BREACH 6", "BREACH 7",
];

/// The lines the sub-threshold counting row publishes: two that match, one that does not.
const CLEAR_LINES: [&str; 3] = ["BREACH 1", "calm 1", "BREACH 2"];

/// The lines the field rows publish: JSON objects whose discovered fields the aggregation folds.
const MEASURED_LINES: [&str; 3] = [
    r#"{"host": "one", "latency": 10}"#,
    r#"{"host": "two", "latency": 20}"#,
    r#"{"host": "two", "latency": 30}"#,
];

/// The query every counting row runs: the matching lines only, so the aggregate counts them and
/// not the lines the filter excludes.
const MATCHING_LINES_QUERY: &str = "fields @timestamp, @message | filter @message like /BREACH/";

#[tokio::test]
async fn the_aggregate_each_execution_computes_over_the_published_lines_decides_the_alarm() {
    let emulator = emulator_with_logs();
    // (case, lines, query, aggregation, threshold, comparison, state, value)
    let rows = [
        Row {
            case: "a count of the matching lines, at the threshold",
            messages: &BREACHING_LINES,
            query_string: MATCHING_LINES_QUERY,
            aggregation: "count(*)",
            threshold: 3.0,
            comparison: "GreaterThanOrEqualToThreshold",
            state: "ALARM",
            value: 7.0,
        },
        Row {
            case: "a count of the matching lines, below the threshold",
            messages: &CLEAR_LINES,
            query_string: MATCHING_LINES_QUERY,
            aggregation: "count(*)",
            threshold: 3.0,
            comparison: "GreaterThanOrEqualToThreshold",
            state: "OK",
            value: 2.0,
        },
        Row {
            case: "the sum of a discovered field",
            messages: &MEASURED_LINES,
            query_string: "fields latency",
            aggregation: "sum(latency)",
            threshold: 50.0,
            comparison: "GreaterThanThreshold",
            state: "ALARM",
            value: 60.0,
        },
        Row {
            case: "the average of a discovered field",
            messages: &MEASURED_LINES,
            query_string: "fields latency",
            aggregation: "avg(latency)",
            threshold: 100.0,
            comparison: "GreaterThanThreshold",
            state: "OK",
            value: 20.0,
        },
        Row {
            case: "the largest value of a discovered field",
            messages: &MEASURED_LINES,
            query_string: "fields latency",
            aggregation: "max(latency)",
            threshold: 25.0,
            comparison: "GreaterThanThreshold",
            state: "ALARM",
            value: 30.0,
        },
        Row {
            case: "the smallest value of a discovered field, under a less-than comparison",
            messages: &MEASURED_LINES,
            query_string: "fields latency",
            aggregation: "min(latency)",
            threshold: 15.0,
            comparison: "LessThanThreshold",
            state: "ALARM",
            value: 10.0,
        },
        Row {
            case: "a distinct count of a discovered field",
            messages: &MEASURED_LINES,
            query_string: "fields host",
            aggregation: "count_distinct(host)",
            threshold: 2.0,
            comparison: "LessThanOrEqualToThreshold",
            state: "ALARM",
            value: 2.0,
        },
    ];

    // Every row's group and alarm is written first, so the executions of all of them are owed
    // from the same instant and the suite waits once rather than once per row.
    for (index, row) in rows.iter().enumerate() {
        let name = format!("aggregate-{index}");
        given_log_group(&emulator, &name, row.messages).await;
        cw(
            &emulator,
            "PutLogAlarm",
            &log_alarm_request(
                &name,
                &name,
                row.query_string,
                row.aggregation,
                row.threshold,
                row.comparison,
                2,
                1,
                &[],
            ),
        )
        .await;
    }

    for (index, row) in rows.iter().enumerate() {
        let name = format!("aggregate-{index}");
        let alarm = until_state(&emulator, row.case, &name, row.state).await;
        let reported = results(row.case, &alarm);
        assert!(
            !reported.is_empty(),
            "case {:?}: expected the results the state rests on, got {alarm}",
            row.case
        );
        assert!(
            reported.len() <= 2,
            "case {:?}: expected at most the two results the alarm evaluates, got {reported:?}",
            row.case
        );
        for result in &reported {
            assert_eq!(
                result.get("status").and_then(Value::as_str),
                Some("COMPLETE"),
                "case {:?}: expected a completed result, got {result}",
                row.case
            );
            let value = result.get("value").and_then(Value::as_f64);
            assert_eq!(
                value,
                Some(row.value),
                "case {:?}: expected the aggregate {}, got {value:?}",
                row.case,
                row.value
            );
            assert!(
                result
                    .get("queryResultId")
                    .and_then(Value::as_str)
                    .is_some_and(|id| !id.is_empty()),
                "case {:?}: expected a queryResultId on {result}",
                row.case
            );
        }
        let data = reason_data(row.case, &alarm);
        assert_eq!(
            data.get("threshold").and_then(Value::as_f64),
            Some(row.threshold),
            "case {:?}: expected the threshold it was compared against, got {data}",
            row.case
        );
        assert_eq!(
            data.get("queryResultsToEvaluate").and_then(Value::as_u64),
            Some(2),
            "case {:?}: expected the N of the M-of-N evaluation, got {data}",
            row.case
        );
        assert_eq!(
            data.get("queryResultsToAlarm").and_then(Value::as_u64),
            Some(1),
            "case {:?}: expected the M of the M-of-N evaluation, got {data}",
            row.case
        );
        // "If the value of this field is PARTIAL_DATA […]" — an evaluation that ran over
        // everything it asked for reports no evaluation state at all.
        assert_eq!(
            alarm.get("EvaluationState"),
            None,
            "case {:?}: expected no EvaluationState, got {alarm}",
            row.case
        );
    }
}

#[tokio::test]
async fn an_m_of_n_log_alarm_notifies_its_topic_through_its_managed_scheduled_query() {
    let emulator = emulator_with_logs();
    let (topic_arn, queue_url) = topic_with_subscribed_queue(&emulator, "log-alarm-notices").await;

    given_log_group(&emulator, "breach-group", &BREACHING_LINES).await;
    given_log_group(&emulator, "control-group", &CLEAR_LINES).await;
    for (alarm_name, group) in [("breach-alarm", "breach-group"), ("control-alarm", "control-group")]
    {
        cw(
            &emulator,
            "PutLogAlarm",
            &log_alarm_request(
                alarm_name,
                group,
                MATCHING_LINES_QUERY,
                "count(*)",
                3.0,
                "GreaterThanOrEqualToThreshold",
                2,
                1,
                &[&topic_arn],
            ),
        )
        .await;
    }

    // "This field is populated in DescribeAlarms responses."
    // (`ScheduledQueryConfiguration$QueryARN`) — one managed query per alarm, never shared.
    let breach_query = query_arn(&described(&emulator, "breach-alarm").await);
    let control_query = query_arn(&described(&emulator, "control-alarm").await);
    assert_ne!(
        breach_query, control_query,
        "each alarm's managed scheduled query is its own, got {breach_query} twice"
    );
    for (alarm_name, arn, group) in [
        ("breach-alarm", &breach_query, "breach-group"),
        ("control-alarm", &control_query, "control-group"),
    ] {
        assert!(
            arn.starts_with(&format!("arn:aws:logs:{REGION}:"))
                && arn.contains(":scheduled-query:"),
            "{alarm_name}: expected a scheduled-query ARN, got {arn}"
        );
        // The managed query resolves in CloudWatch Logs, as the query CloudWatch runs for the
        // alarm rather than one a caller created.
        let query = logs(&emulator, "GetScheduledQuery", &json!({"identifier": arn})).await;
        assert_eq!(
            query.get("scheduleType").and_then(Value::as_str),
            Some("AWS_MANAGED"),
            "{alarm_name}: expected the service-managed schedule type, got {query}"
        );
        assert_eq!(
            query.get("queryString").and_then(Value::as_str),
            Some(MATCHING_LINES_QUERY),
            "{alarm_name}: expected the alarm's own query string, got {query}"
        );
        assert_eq!(
            query["logGroupIdentifiers"],
            json!([group]),
            "{alarm_name}: expected the alarm's own log groups, got {query}"
        );
        assert_eq!(
            query.get("scheduleExpression").and_then(Value::as_str),
            Some("rate(1 minute)"),
            "{alarm_name}: expected the alarm's own schedule, got {query}"
        );
        assert_eq!(
            query.get("executionRoleArn").and_then(Value::as_str),
            Some(QUERY_ROLE),
            "{alarm_name}: expected the alarm's own role, got {query}"
        );
    }

    // The breach alarm: one breaching result is enough for the M of its M-of-N.
    let breaching = until_state(&emulator, "the breach alarm", "breach-alarm", "ALARM").await;
    let breaching_results = results("the breach alarm", &breaching);
    assert!(
        breaching_results
            .iter()
            .any(|result| result.get("value").and_then(Value::as_f64) == Some(7.0)),
        "the breach alarm must rest on the count of its matching lines, got {breaching_results:?}"
    );
    // The control alarm rests in OK once the whole window is non-breaching, which takes as many
    // results as it evaluates.
    let control = until_results(&emulator, "the control alarm", "control-alarm", "OK", 2.0, 2).await;
    let control_results = results("the control alarm", &control);
    assert_eq!(
        control_results.len(),
        2,
        "the control alarm evaluates two results, got {control_results:?}"
    );
    let control_history = history(&emulator, "control-alarm", "StateUpdate").await;
    assert!(
        control_history.iter().all(|item| !item
            .get("HistorySummary")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .ends_with("to ALARM")),
        "the control alarm never breaches, so it never moved to ALARM; got {control_history:?}"
    );

    // "Data about the alarm, in JSON format." — the transition is recorded in the alarm's own
    // history, under the state-update class.
    let items = until_history(&emulator, "breach-alarm", "StateUpdate", 1).await;
    let recorded: Value = serde_json::from_str(
        items[0]
            .get("HistoryData")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    )
    .expect("the history item's data is JSON");
    assert_eq!(
        recorded["newState"]["stateValue"].as_str(),
        Some("ALARM"),
        "the state update must record the state the alarm moved to, got {recorded}"
    );
    assert_eq!(
        recorded["oldState"]["stateValue"].as_str(),
        Some("INSUFFICIENT_DATA"),
        "the state update must record the state it moved out of, got {recorded}"
    );

    // The notification the transition published, where a client reads it.
    let delivered = until_notifications(&emulator, &queue_url, 1).await;
    let notification = delivered
        .iter()
        .find(|notification| notification["AlarmName"] == json!("breach-alarm"))
        .unwrap_or_else(|| panic!("the breach alarm's notification must arrive, got {delivered:?}"));
    for (member, expected) in [
        ("AlarmName", json!("breach-alarm")),
        ("NewStateValue", json!("ALARM")),
        ("OldStateValue", json!("INSUFFICIENT_DATA")),
        ("AlarmArn", breaching["AlarmArn"].clone()),
        ("LogGroups", json!(["breach-group"])),
        ("QueryString", json!(MATCHING_LINES_QUERY)),
        ("AggregationExpression", json!("count(*)")),
    ] {
        assert_eq!(
            notification[member], expected,
            "the notification's {member} must be {expected}, got {notification}"
        );
    }
    // The execution the transition rests on is a run CloudWatch Logs reports in the managed
    // query's own trigger history.
    let execution_id = notification["QueryExecutionId"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a QueryExecutionId, got {notification}"))
        .to_string();
    let runs = trigger_history(&emulator, &breach_query).await;
    let run = runs
        .iter()
        .find(|run| run["queryId"] == json!(execution_id))
        .unwrap_or_else(|| {
            panic!("the execution {execution_id} must be in the query's history, got {runs:?}")
        });
    assert_eq!(
        run.get("executionStatus").and_then(Value::as_str),
        Some("Complete"),
        "the execution the alarm rests on completed, got {run}"
    );
    assert!(
        run.get("triggeredTimestamp").and_then(Value::as_i64).is_some(),
        "an execution that happened names the instant it was triggered, got {run}"
    );
}

#[tokio::test]
async fn an_aggregation_expression_nothing_can_compute_is_reported_as_a_configuration_error() {
    let emulator = emulator_with_logs();
    given_log_group(&emulator, "unreadable-group", &CLEAR_LINES).await;
    cw(
        &emulator,
        "PutLogAlarm",
        &log_alarm_request(
            "unreadable-alarm",
            "unreadable-group",
            MATCHING_LINES_QUERY,
            "nothing an aggregation names",
            1.0,
            "GreaterThanOrEqualToThreshold",
            2,
            1,
            &[],
        ),
    )
    .await;
    // "If the value of this field is EVALUATION_ERROR, it indicates configuration errors in the
    // alarm setup that require review and correction. Refer to the StateReason field of the alarm
    // for more details." (`LogAlarm$EvaluationState`)
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        last = described(&emulator, "unreadable-alarm").await;
        if last.get("EvaluationState").and_then(Value::as_str) == Some("EVALUATION_ERROR") {
            break;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    assert_eq!(
        last.get("EvaluationState").and_then(Value::as_str),
        Some("EVALUATION_ERROR"),
        "an aggregation nothing can compute is a configuration error, got {last}"
    );
    assert!(
        last.get("StateReason")
            .and_then(Value::as_str)
            .is_some_and(|reason| !reason.is_empty()),
        "the reason must say why no result was produced, got {last}"
    );
    assert_eq!(
        last.get("StateValue").and_then(Value::as_str),
        Some("INSUFFICIENT_DATA"),
        "an alarm no aggregate was computed for has no graded state, got {last}"
    );
    // The run happened and produced nothing, so the managed query's history records it as the
    // invalid query it was rather than as a run that completed.
    let runs = trigger_history(&emulator, &query_arn(&last)).await;
    let invalid = runs
        .iter()
        .find(|run| run.get("executionStatus").and_then(Value::as_str) == Some("InvalidQuery"))
        .unwrap_or_else(|| panic!("expected a run recorded as an invalid query, got {runs:?}"));
    assert!(
        invalid
            .get("errorMessage")
            .and_then(Value::as_str)
            .is_some_and(|message| !message.is_empty()),
        "a run that produced nothing records why, got {invalid}"
    );
}

/// The `QueryARN` of a described log alarm's scheduled query configuration.
fn query_arn(alarm: &Value) -> String {
    alarm["ScheduledQueryConfiguration"]["QueryARN"]
        .as_str()
        .unwrap_or_else(|| panic!("expected a populated QueryARN, got {alarm}"))
        .to_string()
}

/// Every run CloudWatch Logs reports for the managed scheduled query `arn` names, over a window
/// wide enough to hold every instant one of this suite's runs can fall at.
///
/// The window's ends are the epoch MILLISECONDS CloudWatch Logs states an epoch instant in.
async fn trigger_history(emulator: &Emulator, arn: &str) -> Vec<Value> {
    #[allow(clippy::cast_possible_truncation)]
    let end_time = (now_secs() * 1_000.0) as i64 + 60_000;
    let reported = logs(
        emulator,
        "GetScheduledQueryHistory",
        &json!({"identifier": arn, "startTime": 0, "endTime": end_time}),
    )
    .await;
    reported["triggerHistory"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// A queue subscribed to a fresh topic: the path a log alarm's notification travels. Answers the
/// topic ARN and the queue URL.
async fn topic_with_subscribed_queue(emulator: &Emulator, name: &str) -> (String, String) {
    let queue = sqs(emulator, "CreateQueue", &json!({"QueueName": name})).await;
    let queue_url = queue["QueueUrl"]
        .as_str()
        .expect("CreateQueue answers a QueueUrl")
        .to_string();
    let attributes = sqs(
        emulator,
        "GetQueueAttributes",
        &json!({"QueueUrl": queue_url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    let queue_arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .expect("GetQueueAttributes answers the QueueArn")
        .to_string();
    sqs(
        emulator,
        "SetQueueAttributes",
        &json!({
            "QueueUrl": queue_url,
            "Attributes": {"Policy": grant_sns_delivery(&queue_arn)},
        }),
    )
    .await;
    let topic_arn = element(&sns(emulator, "CreateTopic", &[("Name", name)]).await, "TopicArn");
    sns(
        emulator,
        "Subscribe",
        &[
            ("TopicArn", &topic_arn),
            ("Protocol", "sqs"),
            ("Endpoint", &queue_arn),
        ],
    )
    .await;
    (topic_arn, queue_url)
}

/// Read the alarm until it reports `state` AND every result it rests on carries `value`, with at
/// least `count` of them; answer it.
///
/// The window fills one execution at a time, so the first reading in a state need not be the one
/// the whole window decides: this waits for the window the rule describes.
async fn until_results(
    emulator: &Emulator,
    case: &str,
    name: &str,
    state: &str,
    value: f64,
    count: usize,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        last = described(emulator, name).await;
        let reported = last
            .get("StateReasonData")
            .and_then(Value::as_str)
            .and_then(|text| serde_json::from_str::<Value>(text).ok())
            .and_then(|data| data["results"].as_array().cloned())
            .unwrap_or_default();
        let settled = last.get("StateValue").and_then(Value::as_str) == Some(state)
            && reported.len() >= count
            && reported
                .iter()
                .all(|result| result.get("value").and_then(Value::as_f64) == Some(value));
        if settled {
            return last;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!(
        "case {case:?}: {name} must rest in {state} on {count} results of {value}; it last \
         reported {last}"
    )
}

/// The log alarm's history items of `item_type`.
async fn history(emulator: &Emulator, name: &str, item_type: &str) -> Vec<Value> {
    let reported = cw(
        emulator,
        "DescribeAlarmHistory",
        &json!({
            "AlarmName": name,
            "AlarmTypes": ["LogAlarm"],
            "HistoryItemType": item_type,
        }),
    )
    .await;
    reported["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Read the alarm's history until it holds at least `want` items of `item_type`, and answer them.
async fn until_history(
    emulator: &Emulator,
    name: &str,
    item_type: &str,
    want: usize,
) -> Vec<Value> {
    let mut items = Vec::new();
    for _ in 0..LOOKS {
        items = history(emulator, name, item_type).await;
        if items.len() >= want {
            return items;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("{name} must record {want} {item_type} items; it last held {items:?}")
}

/// Every notification the queue holds, as the documents they carry.
async fn notifications(emulator: &Emulator, queue_url: &str) -> Vec<Value> {
    let received = sqs(
        emulator,
        "ReceiveMessage",
        &json!({"QueueUrl": queue_url, "MaxNumberOfMessages": 10}),
    )
    .await;
    received["Messages"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|message| {
            let envelope: Value =
                serde_json::from_str(message.get("Body").and_then(Value::as_str)?).ok()?;
            serde_json::from_str(envelope["Message"].as_str()?).ok()
        })
        .collect()
}

/// Read the queue until it has delivered at least `want` notifications, and answer every one it
/// delivered: delivery is asynchronous, so a test waits for it rather than assuming the read that
/// saw the new state also saw the publish.
async fn until_notifications(emulator: &Emulator, queue_url: &str, want: usize) -> Vec<Value> {
    let mut delivered: Vec<Value> = Vec::new();
    for _ in 0..LOOKS {
        delivered.extend(notifications(emulator, queue_url).await);
        if delivered.len() >= want {
            return delivered;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("the queue must receive {want} notifications; it received {delivered:?}")
}
