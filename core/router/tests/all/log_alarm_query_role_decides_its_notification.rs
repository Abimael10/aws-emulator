// SPDX-License-Identifier: CC0-1.0

//! WHETHER A LOG ALARM NOTIFIES, decided by the role its scheduled query is executed under —
//! over the whole mounted service set, so the trust document is the one IAM persisted, the log
//! events are the ones CloudWatch Logs holds, and the notification is read where a client reads
//! one: off the queue subscribed to the alarm's topic.
//!
//! RULE under test: **an execution of the scheduled query backing a log alarm is made under "the
//! IAM role that CloudWatch assumes when executing the scheduled query against the configured log
//! groups" (`ScheduledQueryConfiguration$ScheduledQueryRoleARN`), so a role of this account whose
//! trust policy admits neither service principal that assume is made by executes nothing: that
//! alarm reports `EvaluationState` `EVALUATION_ERROR` ("configuration errors in the alarm setup
//! that require review and correction", `LogAlarm$EvaluationState`), keeps `INSUFFICIENT_DATA`
//! because nothing was computed for it, carries every query result of its window as `FAILED` with
//! no value, transitions nowhere and therefore publishes none of its `AlarmActions` — while an
//! alarm whose query DID execute transitions on the aggregate it computed, reports no
//! `EvaluationState`, and publishes the notification its action names. Repairing only the trust
//! policy lets the same alarm execute, clears the evaluation state and delivers the notification
//! that was withheld.**
//!
//! Five services have to agree for that to hold — CloudWatch owns the alarm and makes the assume,
//! IAM answers for the role, CloudWatch Logs runs the query over its log events, SNS fans the
//! notification out and SQS holds what arrived — and the publish CloudWatch makes is an in-process
//! call addressed the way a client's own is, so a suite mounting only the services it needs cannot
//! see the withholding where a client sees it. The crate-local suites of
//! `service-com-amazonaws-cloudwatch-graniteserviceversion20100801` pin the trust decision itself
//! over a table of documents and spellings; what only this suite reaches is the SNS/SQS end of it.
//!
//! The rows differ along what the rule generalises over: which service's principal the role's
//! trust document names, which effect the statement states, and whether IAM holds the role at all.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The harness that assembles the whole emulator, speaks to CloudWatch and SQS, wires a topic to a
// subscribed queue and reads the alarm notifications off it is the sibling suite's: a suite that
// writes its own drifts from it.
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{
    authorization, cw, drain, given_topic_with_subscriber, router, Grant,
};
use crate::query_wire::form_encode;

/// The Region the whole wiring of this suite lives in.
const REGION: &str = "us-east-1";

/// CloudWatch Logs' `X-Amz-Target` prefix and the `awsJson1_1` media type its model declares.
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_CONTENT_TYPE: &str = "application/x-amz-json-1.1";

/// IAM's own API version, which its `awsQuery` wire carries.
const IAM_VERSION: &str = "2010-05-08";

/// The query every alarm here runs: the matching lines only.
const MATCHING_LINES_QUERY: &str = "fields @timestamp, @message | filter @message like /BREACH/";

/// The aggregation every alarm here folds its rows by.
const AGGREGATION: &str = "count(*)";

/// The lines the log group holds, one log event each: seven that match the query's filter and
/// three that do not, so a completed execution's aggregate is seven.
const LINES: [&str; 10] = [
    "BREACH 1", "calm 1", "BREACH 2", "BREACH 3", "calm 2", "BREACH 4", "BREACH 5", "calm 3",
    "BREACH 6", "BREACH 7",
];

/// The threshold every alarm here compares its aggregate against — below the aggregate
/// [`LINES`] implies, so an execution that ran crosses it.
const THRESHOLD: f64 = 3.0;

/// The lookback every alarm here queries over, in seconds
/// (`ScheduleConfiguration$StartTimeOffset`): wide enough that every line this suite published is
/// inside the window of each execution it waits for.
const START_TIME_OFFSET: i64 = 300;

/// The alarm states and evaluation state this suite reads, spelled out here rather than imported,
/// so a wrong constant in the implementation cannot make the suite pass.
const ALARM: &str = "ALARM";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";
const EVALUATION_ERROR: &str = "EVALUATION_ERROR";

/// The result statuses one `StateReasonData` entry carries.
const RESULT_COMPLETE: &str = "COMPLETE";
const RESULT_FAILED: &str = "FAILED";

/// How long a wait keeps looking for the CONDITION it is about before giving up.
///
/// Every execution of an alarm's managed query is owed a span of its own, the repair leg needs a
/// second execution after the first one failed, and a run a loaded host cut short is simply looked
/// at again — so the budget is deliberately generous. Nothing here is asserted about the wall
/// clock: a wait ends as soon as what it waits for holds.
const SETTLE_DEADLINE: Duration = Duration::from_secs(180);

/// How long one look waits before the next, beside the long poll the queue read makes.
const LOOK_INTERVAL: Duration = Duration::from_millis(200);

/// How long the withholding is watched once every expected notification has arrived: a bound on an
/// ABSENCE, so it is the one span a slow host may spend without changing the verdict — the
/// notifying rows beside it prove the same wiring delivers in a fraction of it.
const WITHHELD_WINDOW: Duration = Duration::from_secs(5);

/// One CloudWatch Logs call over the `awsJson1_1` wire that service serves, which must answer 200.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("logs", REGION))
        .header("content-type", LOGS_CONTENT_TYPE)
        .header("x-amz-target", format!("{LOGS_TARGET_PREFIX}.{operation}"))
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
        "case {case:?}: expected HTTP 200 from logs {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One IAM call over the `awsQuery` form wire that service serves, which must answer 200.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    params: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version={IAM_VERSION}");
    for (name, value) in params {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("authorization", authorization("iam", REGION))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from IAM {action}, got {status} with {text}"
    );
    text
}

/// A trust policy with one statement naming `service` as the principal, under `effect`, for the
/// action an assume is made with.
fn trust(effect: &str, service: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": effect,
            "Principal": {"Service": service},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The ARN of this account's role `name`.
fn role_arn(name: &str) -> String {
    format!(
        "arn:aws:iam::{}:role/{name}",
        engine::EMULATOR_ACCOUNT_ID
    )
}

/// Wall-clock now as epoch seconds, the clock a query's window is measured against.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |since| since.as_secs_f64())
}

/// A log group holding [`LINES`] as one log event each, stamped at the instant this runs.
async fn given_log_group(router: &MultiServiceRouter, case: &str, name: &str) {
    logs(router, case, "CreateLogGroup", &json!({"logGroupName": name})).await;
    logs(
        router,
        case,
        "CreateLogStream",
        &json!({"logGroupName": name, "logStreamName": "s"}),
    )
    .await;
    #[allow(clippy::cast_possible_truncation)]
    let at = (now_secs() * 1_000.0) as i64;
    let events: Vec<Value> = LINES
        .iter()
        .enumerate()
        .map(|(index, message)| {
            json!({"timestamp": at - (LINES.len() - index) as i64, "message": message})
        })
        .collect();
    let accepted = logs(
        router,
        case,
        "PutLogEvents",
        &json!({"logGroupName": name, "logStreamName": "s", "logEvents": events}),
    )
    .await;
    assert!(
        accepted.get("rejectedLogEventsInfo").is_none(),
        "case {case:?}: the lines of {name} must all be accepted, got {accepted}"
    );
}

/// The `PutLogAlarm` request one row sends: every member the operation requires, the role this row
/// runs its query under, and the topic its ALARM transition notifies.
fn put_log_alarm_request(name: &str, log_group: &str, role: &str, topic: &str) -> Value {
    json!({
        "AlarmName": name,
        "QueryResultsToEvaluate": 2,
        "QueryResultsToAlarm": 1,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanOrEqualToThreshold",
        "AlarmActions": [topic],
        "ScheduledQueryConfiguration": {
            "QueryString": MATCHING_LINES_QUERY,
            "LogGroupIdentifiers": [log_group],
            "ScheduledQueryRoleARN": role,
            "AggregationExpression": AGGREGATION,
            "ScheduleConfiguration": {
                "ScheduleExpression": "rate(1 minute)",
                "StartTimeOffset": START_TIME_OFFSET,
                "EndTimeOffset": 0,
            },
        },
    })
}

/// The log alarm `DescribeAlarms` reports under `name`.
async fn described(router: &MultiServiceRouter, case: &str, name: &str) -> Value {
    let body = cw(
        router,
        case,
        REGION,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": ["LogAlarm"]}),
    )
    .await;
    let alarms = body["LogAlarms"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        alarms.len(),
        1,
        "case {case:?}: expected exactly the log alarm {name}, got {body}"
    );
    alarms[0].clone()
}

/// The query results a described alarm's `StateReasonData` document reports.
fn results(alarm: &Value) -> Vec<Value> {
    alarm
        .get("StateReasonData")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|data| data["results"].as_array().cloned())
        .unwrap_or_default()
}

/// Every notification delivered so far that names the alarm `name`.
fn notifications_naming<'a>(delivered: &'a [Value], name: &str) -> Vec<&'a Value> {
    delivered
        .iter()
        .filter(|notification| notification["AlarmName"].as_str() == Some(name))
        .collect()
}

/// The `EvaluationState` a reading reports, or `None` when it reports none at all.
fn evaluation_state(alarm: &Value) -> Option<&str> {
    alarm.get("EvaluationState").and_then(Value::as_str)
}

/// Read every alarm of `names` and take whatever the queue holds, again and again, until `ready`
/// holds of the readings and everything delivered so far — or the deadline passes.
///
/// Answers the last readings, in the order `names` names them; everything the queue handed over
/// meanwhile is appended to `delivered`, so a notification is never lost to a later look. The wait
/// is on the CONDITION, which is what lets the service's own alarm clock run.
async fn look_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    names: &[String],
    delivered: &mut Vec<Value>,
    ready: &dyn Fn(&[Value], &[Value]) -> bool,
) -> Vec<Value> {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let mut readings: Vec<Value> = Vec::with_capacity(names.len());
        for name in names {
            readings.push(described(router, case, name).await);
        }
        delivered.extend(drain(router, case, REGION, url).await);
        if ready(&readings, delivered) || Instant::now() >= deadline {
            return readings;
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// One row: the trust document IAM holds for this row's role (or none at all), and whether the
/// alarm that runs its query under that role notifies its topic.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The trust document IAM holds the row's role under, as `(effect, service principal)`, or
    /// `None` for a role this account's IAM holds nothing under.
    trust: Option<(&'static str, &'static str)>,
    /// Whether an execution of the alarm's query is made under that role — and so whether the
    /// alarm transitions and publishes its action.
    notifies: bool,
}

/// The table: which principal the document names, which effect it states, and whether IAM holds
/// the role at all.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the principal of the service that runs the query",
            trust: Some(("Allow", "logs.amazonaws.com")),
            notifies: true,
        },
        Row {
            case: "the principal of the service that owns the alarm",
            trust: Some(("Allow", "cloudwatch.amazonaws.com")),
            notifies: true,
        },
        Row {
            // A role is routinely provisioned out of band, so IAM holding nothing under the ARN
            // says nothing about it: only a role IAM ANSWERED about stops a run.
            case: "a role this account's IAM holds nothing under",
            trust: None,
            notifies: true,
        },
        Row {
            case: "another service's principal only",
            trust: Some(("Allow", "ec2.amazonaws.com")),
            notifies: false,
        },
        Row {
            case: "the principal that runs the query, explicitly denied",
            trust: Some(("Deny", "logs.amazonaws.com")),
            notifies: false,
        },
    ]
}

/// RULE: only a log alarm whose scheduled query role can be assumed executes its query, so only
/// that alarm transitions and reaches the queue subscribed to its topic; an alarm whose role
/// admits neither executing principal reports the configuration error, keeps `INSUFFICIENT_DATA`,
/// carries `FAILED` results with no value and notifies nothing.
#[tokio::test]
async fn only_an_assumable_query_role_lets_a_log_alarm_notify_its_topic() {
    let router = router();
    let suite = "log alarm query roles";
    let (topic, url) = given_topic_with_subscriber(
        &router,
        suite,
        REGION,
        "log-alarm-query-roles",
        Grant::ScopedToTopic,
    )
    .await;
    given_log_group(&router, suite, "/suite/log-alarm-query-roles").await;

    let rows = rows();
    let mut names: Vec<String> = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let name = format!("query-role-{index}");
        if let Some((effect, service)) = row.trust {
            iam(
                &router,
                row.case,
                "CreateRole",
                &[
                    ("RoleName", &name),
                    ("AssumeRolePolicyDocument", &trust(effect, service)),
                ],
            )
            .await;
        }
        // The write is served whatever the role turns out to be: `PutLogAlarm` models no error for
        // it, so the failure is one the alarm's executions surface.
        cw(
            &router,
            row.case,
            REGION,
            "PutLogAlarm",
            &put_log_alarm_request(
                &name,
                "/suite/log-alarm-query-roles",
                &role_arn(&name),
                &topic,
            ),
        )
        .await;
        names.push(name);
    }

    // Every row's executions are owed from about the same instant, so the suite waits once: until
    // each notifying row has transitioned AND its notification has arrived, and each withheld row
    // reports the configuration error of a role nothing can execute its query under.
    let rows_ref = &rows;
    let mut delivered: Vec<Value> = Vec::new();
    let readings = look_until(
        &router,
        suite,
        &url,
        &names,
        &mut delivered,
        &|readings: &[Value], delivered: &[Value]| {
            readings.iter().zip(rows_ref).all(|(alarm, row)| {
                let name = alarm["AlarmName"].as_str().unwrap_or_default();
                if row.notifies {
                    alarm["StateValue"].as_str() == Some(ALARM)
                        && evaluation_state(alarm).is_none()
                        && !notifications_naming(delivered, name).is_empty()
                } else {
                    evaluation_state(alarm) == Some(EVALUATION_ERROR)
                }
            })
        },
    )
    .await;

    for (alarm, row) in readings.iter().zip(&rows) {
        let case = row.case;
        let name = alarm["AlarmName"].as_str().unwrap_or_default().to_string();
        let reported = results(alarm);
        let notifications = notifications_naming(&delivered, &name);
        if row.notifies {
            assert_eq!(
                alarm["StateValue"].as_str(),
                Some(ALARM),
                "case {case:?}: an executed query crosses the threshold, got {alarm}"
            );
            assert_eq!(
                evaluation_state(alarm),
                None,
                "case {case:?}: an execution that ran reports no evaluation state, got {alarm}"
            );
            assert!(
                reported.iter().any(|result| {
                    result["status"].as_str() == Some(RESULT_COMPLETE)
                        && result.get("value").and_then(Value::as_f64).is_some_and(
                            |value| value >= THRESHOLD,
                        )
                }),
                "case {case:?}: the alarm must rest on a computed result at or above \
                 {THRESHOLD}, got {reported:?}"
            );
            assert_eq!(
                notifications.len(),
                1,
                "case {case:?}: the transition must reach the subscribed queue once, got \
                 {notifications:?}"
            );
            assert_eq!(
                notifications[0]["NewStateValue"].as_str(),
                Some(ALARM),
                "case {case:?}: the notification must carry the state moved to, got {}",
                notifications[0]
            );
            continue;
        }
        assert_eq!(
            evaluation_state(alarm),
            Some(EVALUATION_ERROR),
            "case {case:?}: a role the execution cannot be made under is a configuration error, \
             got {alarm}"
        );
        assert_eq!(
            alarm["StateValue"].as_str(),
            Some(INSUFFICIENT_DATA),
            "case {case:?}: nothing was computed, so the alarm has no graded state, got {alarm}"
        );
        assert!(
            alarm["StateReason"]
                .as_str()
                .is_some_and(|reason| reason.contains(&role_arn(&name))),
            "case {case:?}: the reason must name the role the query could not run under, got \
             {alarm}"
        );
        assert!(
            !reported.is_empty(),
            "case {case:?}: the window must hold the failed result of the execution, got {alarm}"
        );
        for result in &reported {
            assert_eq!(
                result["status"].as_str(),
                Some(RESULT_FAILED),
                "case {case:?}: a result nothing computed is {RESULT_FAILED}, got {result}"
            );
            assert_eq!(
                result.get("value"),
                None,
                "case {case:?}: a result nothing computed carries no value, got {result}"
            );
        }
        assert!(
            notifications.is_empty(),
            "case {case:?}: an alarm that evaluated nothing publishes no action, got \
             {notifications:?}"
        );
    }

    // The withholding watched on: the notifying rows have already been delivered through this
    // same wiring, so anything the withheld rows owed would be here by now.
    let watching = Instant::now();
    while watching.elapsed() < WITHHELD_WINDOW {
        delivered.extend(drain(&router, suite, REGION, &url).await);
        for (alarm, row) in readings.iter().zip(&rows) {
            if row.notifies {
                continue;
            }
            let name = alarm["AlarmName"].as_str().unwrap_or_default();
            let late = notifications_naming(&delivered, name);
            assert!(
                late.is_empty(),
                "case {:?}: an alarm whose query never ran must stay silent, got {late:?}",
                row.case
            );
        }
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// RULE: the role is read at EVERY execution, so repairing only the trust policy of a role an
/// alarm already failed on lets that same alarm execute — the evaluation state is cleared, the
/// window holds a computed result again, and the notification withheld before is delivered.
#[tokio::test]
async fn repairing_only_the_trust_policy_delivers_the_withheld_notification() {
    let router = router();
    let case = "a role repaired after the alarm failed on it";
    let name = "repaired-query-role";
    let (topic, url) =
        given_topic_with_subscriber(&router, case, REGION, "log-alarm-repair", Grant::Unscoped)
            .await;
    given_log_group(&router, case, "/suite/log-alarm-repair").await;
    iam(
        &router,
        case,
        "CreateRole",
        &[
            ("RoleName", name),
            (
                "AssumeRolePolicyDocument",
                &trust("Allow", "ec2.amazonaws.com"),
            ),
        ],
    )
    .await;
    cw(
        &router,
        case,
        REGION,
        "PutLogAlarm",
        &put_log_alarm_request(name, "/suite/log-alarm-repair", &role_arn(name), &topic),
    )
    .await;

    let names = vec![name.to_string()];
    let mut delivered: Vec<Value> = Vec::new();
    let failed = look_until(
        &router,
        case,
        &url,
        &names,
        &mut delivered,
        &|readings: &[Value], _: &[Value]| {
            readings
                .first()
                .is_some_and(|alarm| evaluation_state(alarm) == Some(EVALUATION_ERROR))
        },
    )
    .await;
    let failed = failed[0].clone();
    assert_eq!(
        evaluation_state(&failed),
        Some(EVALUATION_ERROR),
        "case {case:?}: a role the execution cannot be made under is a configuration error, got \
         {failed}"
    );
    assert_eq!(
        failed["StateValue"].as_str(),
        Some(INSUFFICIENT_DATA),
        "case {case:?}: the alarm has no graded state before the repair, got {failed}"
    );
    assert!(
        notifications_naming(&delivered, name).is_empty(),
        "case {case:?}: nothing may be published before the repair, got {delivered:?}"
    );

    // ONLY the trust policy changes — the alarm is not rewritten.
    iam(
        &router,
        case,
        "UpdateAssumeRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyDocument", &trust("Allow", "logs.amazonaws.com")),
        ],
    )
    .await;
    let repaired = look_until(
        &router,
        case,
        &url,
        &names,
        &mut delivered,
        &|readings: &[Value], delivered: &[Value]| {
            readings.first().is_some_and(|alarm| {
                alarm["StateValue"].as_str() == Some(ALARM)
                    && evaluation_state(alarm).is_none()
                    && !notifications_naming(delivered, name).is_empty()
            })
        },
    )
    .await;
    let repaired = repaired[0].clone();
    assert_eq!(
        repaired["StateValue"].as_str(),
        Some(ALARM),
        "case {case:?}: the repaired alarm must reach {ALARM}, got {repaired}"
    );
    assert_eq!(
        evaluation_state(&repaired),
        None,
        "case {case:?}: an execution that ran clears the evaluation state, got {repaired}"
    );
    let reported = results(&repaired);
    assert!(
        reported.iter().any(|result| {
            result["status"].as_str() == Some(RESULT_COMPLETE)
                && result
                    .get("value")
                    .and_then(Value::as_f64)
                    .is_some_and(|value| value >= THRESHOLD)
        }),
        "case {case:?}: the repaired alarm must rest on a computed result, got {reported:?}"
    );
    let notifications = notifications_naming(&delivered, name);
    assert_eq!(
        notifications.len(),
        1,
        "case {case:?}: the transition the repair made possible must be delivered once, got \
         {notifications:?}"
    );
    assert_eq!(
        notifications[0]["NewStateValue"].as_str(),
        Some(ALARM),
        "case {case:?}: the delivered notification must carry the state moved to, got {}",
        notifications[0]
    );
}
