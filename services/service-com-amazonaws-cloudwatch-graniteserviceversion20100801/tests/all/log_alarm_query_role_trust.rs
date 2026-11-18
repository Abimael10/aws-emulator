// SPDX-License-Identifier: CC0-1.0

//! WHICH ROLE a log alarm's scheduled query can be executed under, over the TRUST DOCUMENTS the
//! role carries — end to end, with CloudWatch Logs and the IAM emulator mounted beside this
//! service on one engine, so the document read is the one IAM itself stored and the query runs
//! where the log events are.
//!
//! RULE: an execution of the scheduled query backing a log alarm is made under "the IAM role that
//! CloudWatch assumes when executing the scheduled query against the configured log groups"
//! (`ScheduledQueryConfiguration$ScheduledQueryRoleARN`), so no execution happens when IAM
//! reports a role of this account whose trust policy admits NEITHER service whose principal that
//! assume is made by — the CloudWatch Logs scheduled query that runs it and the CloudWatch alarm
//! that owns it, each named in a trust policy by the global `<service>.amazonaws.com` or the
//! Regional `<service>.<region>.amazonaws.com` spelling. An alarm whose query could not be
//! executed reports `EvaluationState` `EVALUATION_ERROR` ("configuration errors in the alarm setup
//! that require review and correction", `LogAlarm$EvaluationState`) with a `StateReason` saying
//! why, keeps `INSUFFICIENT_DATA` because nothing was computed for it, carries its query results
//! as `FAILED` entries with no value, and transitions nowhere — so none of its actions is taken.
//! The write itself is served either way: `PutLogAlarm` models no error for a role, and the
//! failure is one its executions surface.
//!
//! The rows differ along what the rule generalises over: which service's principal the document
//! names, which spelling of it, whether it names one or several, whether the action is written
//! with a wildcard, which effect the statement states, which Region's spelling it names, and
//! whether IAM holds the role at all. Beside the table: repairing ONLY the trust policy of a role
//! an alarm already failed on lets that same alarm evaluate.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{now_secs, rpc, Emulator, REGION};

/// The CloudWatch signing name and target prefix, for `rpc`.
const CLOUDWATCH: (&str, &str) = ("monitoring", crate::harness::TARGET_PREFIX);

/// The CloudWatch Logs signing name and target prefix.
const LOGS: (&str, &str) = ("logs", "Logs_20140328");

/// IAM's own API version, which its `awsQuery` wire names.
const IAM_VERSION: &str = "2010-05-08";

/// How many times a wait looks again before it gives up.
///
/// Each execution of an alarm's managed query is owed a span of its own
/// (`state::log_alarm_evaluation`'s `QUERY_EXECUTION_SPAN_SECS`), the repair case needs a second
/// execution after the first one failed, and a run this emulator's own pass ceiling cuts short on
/// a loaded host is simply looked at again — so the budget is deliberately generous: what every
/// assertion below waits for is a CONDITION, never a span of wall clock, and a wait ends as soon
/// as it holds.
const LOOKS: usize = 1_200;

/// How long one look waits before the next.
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The lookback every alarm in this suite queries over, in seconds: wide enough that every line it
/// published is inside the window of the first execution
/// (`ScheduleConfiguration$StartTimeOffset`).
const START_TIME_OFFSET: i64 = 300;

/// The query every alarm here runs: the matching lines only.
const MATCHING_LINES_QUERY: &str = "fields @timestamp, @message | filter @message like /BREACH/";

/// The lines every log group here holds: seven that match the query's filter and three that do
/// not, so a completed execution's `count(*)` is seven.
const LINES: [&str; 10] = [
    "BREACH 1", "calm 1", "BREACH 2", "BREACH 3", "calm 2", "BREACH 4", "BREACH 5", "calm 3",
    "BREACH 6", "BREACH 7",
];

/// The aggregate a completed execution over [`LINES`] computes.
const MATCHED: f64 = 7.0;

/// The threshold every alarm here compares that aggregate against.
const THRESHOLD: f64 = 3.0;

/// CloudWatch, CloudWatch Logs and IAM over ONE shared engine: the query reads the log events
/// CloudWatch Logs holds, and the trust document is the one IAM stored.
fn emulator_with_logs_and_iam() -> Emulator {
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
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    Emulator { router, engine }
}

/// One CloudWatch call that must answer 200.
async fn cw(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, CLOUDWATCH.0, CLOUDWATCH.1, operation, body).await
}

/// One CloudWatch Logs call that must answer 200.
async fn logs(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, LOGS.0, LOGS.1, operation, body).await
}

/// One `awsQuery` call to IAM, which must succeed; answers the raw XML.
async fn iam(emulator: &Emulator, case: &str, action: &str, params: &[(&str, String)]) -> String {
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
        .header("content-type", "application/x-www-form-urlencoded")
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/iam/aws4_request, \
                 SignedHeaders=host, Signature=not-verified"
            ),
        )
        .body(bytes::Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: the {action} setup call must succeed, got {status} with {text}"
    );
    text
}

/// Percent-encode one `application/x-www-form-urlencoded` component through the maintained RFC
/// 3986 coder, over the whole non-alphanumeric set — so a trust document's braces, quotes and
/// colons survive the round trip.
fn form_encode(value: &str) -> String {
    ::percent_encoding::utf8_percent_encode(value, ::percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// A trust policy with one statement naming `services` (a JSON string or array), `action` and
/// `effect`, as a client would send it.
fn trust(effect: &str, action: &str, services: &str) -> String {
    format!(
        "{{\"Version\":\"2012-10-17\",\"Statement\":[{{\"Effect\":\"{effect}\",\
         \"Principal\":{{\"Service\":{services}}},\"Action\":\"{action}\"}}]}}"
    )
}

/// The role ARN of this account's role `name`.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// A log group holding [`LINES`] as one log event each, stamped at the instant this runs.
async fn given_log_group(emulator: &Emulator, name: &str) {
    logs(emulator, "CreateLogGroup", &json!({"logGroupName": name})).await;
    logs(
        emulator,
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

/// The log alarm one case configures: identical in every member but the role it runs under.
fn log_alarm_request(name: &str, log_group: &str, role: &str) -> Value {
    json!({
        "AlarmName": name,
        "QueryResultsToEvaluate": 2,
        "QueryResultsToAlarm": 1,
        "Threshold": THRESHOLD,
        "ComparisonOperator": "GreaterThanOrEqualToThreshold",
        "ScheduledQueryConfiguration": {
            "QueryString": MATCHING_LINES_QUERY,
            "LogGroupIdentifiers": [log_group],
            "ScheduledQueryRoleARN": role,
            "AggregationExpression": "count(*)",
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

/// The results a described alarm's `StateReasonData` document reports.
fn results(alarm: &Value) -> Vec<Value> {
    alarm
        .get("StateReasonData")
        .and_then(Value::as_str)
        .and_then(|text| serde_json::from_str::<Value>(text).ok())
        .and_then(|data| data["results"].as_array().cloned())
        .unwrap_or_default()
}

/// Read the alarm until `ready` holds of what it reports, and answer that reading; panic with
/// `want` and the last reading when it never does.
///
/// An execution of the alarm's managed query is owed a span after the write, so this waits for the
/// CONDITION and awaits while it does, which is what lets the service's own alarm clock run.
async fn until(
    emulator: &Emulator,
    case: &str,
    name: &str,
    want: &str,
    ready: impl Fn(&Value) -> bool,
) -> Value {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        last = described(emulator, name).await;
        if ready(&last) {
            return last;
        }
        ::tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: {name} must {want}; it last reported {last}")
}

/// Whether this reading is of an alarm whose query RAN: it reached the graded state the lines it
/// queried imply, and the evaluation that put it there reports no evaluation state of its own.
///
/// Both halves are asked of ONE reading, so a run that produced nothing — a query this emulator's
/// own ceiling cut short on a loaded host, which is no part of the rule under test — leaves the
/// wait looking again rather than deciding the case.
fn executed(alarm: &Value) -> bool {
    alarm.get("StateValue").and_then(Value::as_str) == Some("ALARM")
        && alarm.get("EvaluationState").is_none()
}

/// Whether this reading is of an alarm whose query could not be executed under its role at all:
/// the configuration-error evaluation state (`LogAlarm$EvaluationState`).
fn refused(alarm: &Value) -> bool {
    alarm.get("EvaluationState").and_then(Value::as_str) == Some("EVALUATION_ERROR")
}

/// One row: the trust document IAM holds for this row's role (or none at all), and whether the
/// alarm's scheduled query can be executed under it.
struct Row {
    /// What the row varies, named in every assertion it makes.
    case: &'static str,
    /// The trust document IAM holds for the row's role, or `None` for a role IAM holds nothing
    /// under.
    trust: Option<String>,
    /// Whether an execution of the alarm's query is made under that role.
    executed: bool,
}

/// The table.
fn rows() -> Vec<Row> {
    let regional_logs = format!("logs.{REGION}.amazonaws.com");
    let regional_cloudwatch = format!("cloudwatch.{REGION}.amazonaws.com");
    vec![
        Row {
            case: "the global spelling of the executing service's principal",
            trust: Some(trust("Allow", "sts:AssumeRole", "\"logs.amazonaws.com\"")),
            executed: true,
        },
        Row {
            case: "the Regional spelling of the executing service's principal",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                &format!("\"{regional_logs}\""),
            )),
            executed: true,
        },
        Row {
            case: "the global spelling of the alarm's own service principal",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                "\"cloudwatch.amazonaws.com\"",
            )),
            executed: true,
        },
        Row {
            case: "the Regional spelling of the alarm's own service principal",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                &format!("\"{regional_cloudwatch}\""),
            )),
            executed: true,
        },
        Row {
            case: "both principals in one list",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                "[\"cloudwatch.amazonaws.com\",\"logs.amazonaws.com\"]",
            )),
            executed: true,
        },
        Row {
            case: "the executing principal, the action written with a wildcard",
            trust: Some(trust("Allow", "sts:*", "\"logs.amazonaws.com\"")),
            executed: true,
        },
        Row {
            case: "another service's principal only",
            trust: Some(trust("Allow", "sts:AssumeRole", "\"ec2.amazonaws.com\"")),
            executed: false,
        },
        Row {
            case: "another Region's spelling of the executing service's principal",
            trust: Some(trust(
                "Allow",
                "sts:AssumeRole",
                "\"logs.ap-south-1.amazonaws.com\"",
            )),
            executed: false,
        },
        Row {
            case: "the executing principal, explicitly denied",
            trust: Some(trust("Deny", "sts:AssumeRole", "\"logs.amazonaws.com\"")),
            executed: false,
        },
        Row {
            case: "a statement about another action entirely",
            trust: Some(trust("Allow", "s3:GetObject", "\"logs.amazonaws.com\"")),
            executed: false,
        },
        Row {
            case: "a role this account's IAM holds nothing under",
            trust: None,
            executed: true,
        },
    ]
}

#[tokio::test]
async fn only_a_role_the_execution_can_be_made_under_evaluates_the_log_alarm() {
    let emulator = emulator_with_logs_and_iam();
    let rows = rows();

    // Every row's log group, role and alarm is written first, so the executions of all of them are
    // owed from the same instant and the suite waits once rather than once per row.
    for (index, row) in rows.iter().enumerate() {
        let name = format!("trust-{index}");
        given_log_group(&emulator, &name).await;
        if let Some(document) = &row.trust {
            iam(
                &emulator,
                row.case,
                "CreateRole",
                &[
                    ("RoleName", name.clone()),
                    ("AssumeRolePolicyDocument", document.clone()),
                ],
            )
            .await;
        }
        // "PutLogAlarm" models no error for the role, so the write is served whatever the role
        // turns out to be: the failure belongs to the executions.
        cw(
            &emulator,
            "PutLogAlarm",
            &log_alarm_request(&name, &name, &role_arn(&name)),
        )
        .await;
        let written = described(&emulator, &name).await;
        assert_eq!(
            written["ScheduledQueryConfiguration"]["ScheduledQueryRoleARN"],
            json!(role_arn(&name)),
            "case {:?}: the alarm must report the role it was configured with, got {written}",
            row.case
        );
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let name = format!("trust-{index}");
        if row.executed {
            let alarm = until(
                &emulator,
                case,
                &name,
                "evaluate its query and cross the threshold",
                executed,
            )
            .await;
            let reported = results(&alarm);
            // "If the value of this field is PARTIAL_DATA […]" — an evaluation that ran over
            // everything it asked for reports no evaluation state at all.
            assert_eq!(
                alarm.get("EvaluationState"),
                None,
                "case {case:?}: expected no EvaluationState, got {alarm}"
            );
            assert!(
                reported.iter().any(|result| {
                    result.get("status").and_then(Value::as_str) == Some("COMPLETE")
                        && result.get("value").and_then(Value::as_f64) == Some(MATCHED)
                }),
                "case {case:?}: expected a completed result of {MATCHED}, got {reported:?}"
            );
            continue;
        }
        let alarm = until(
            &emulator,
            case,
            &name,
            "report the configuration error of a role it cannot run under",
            refused,
        )
        .await;
        let reported = results(&alarm);
        assert_eq!(
            alarm.get("EvaluationState").and_then(Value::as_str),
            Some("EVALUATION_ERROR"),
            "case {case:?}: a role the execution cannot be made under is a configuration error, \
             got {alarm}"
        );
        assert_eq!(
            alarm.get("StateValue").and_then(Value::as_str),
            Some("INSUFFICIENT_DATA"),
            "case {case:?}: nothing was computed, so the alarm has no graded state, got {alarm}"
        );
        assert!(
            alarm
                .get("StateReason")
                .and_then(Value::as_str)
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
                result.get("status").and_then(Value::as_str),
                Some("FAILED"),
                "case {case:?}: a result nothing computed is FAILED, got {result}"
            );
            assert_eq!(
                result.get("value"),
                None,
                "case {case:?}: a result nothing computed carries no value, got {result}"
            );
        }
        // Nothing moved, so none of the alarm's actions was taken: the transitions an alarm's
        // actions follow are exactly the ones its own history records.
        let history = cw(
            &emulator,
            "DescribeAlarmHistory",
            &json!({
                "AlarmName": name,
                "AlarmTypes": ["LogAlarm"],
                "HistoryItemType": "StateUpdate",
            }),
        )
        .await;
        let updates = history["AlarmHistoryItems"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert!(
            updates.is_empty(),
            "case {case:?}: an alarm that evaluated nothing transitions nowhere, got {updates:?}"
        );
    }
}

#[tokio::test]
async fn repairing_only_the_trust_policy_lets_the_same_alarm_evaluate() {
    let emulator = emulator_with_logs_and_iam();
    let case = "a role repaired after the alarm failed on it";
    let name = "repaired";
    given_log_group(&emulator, name).await;
    iam(
        &emulator,
        case,
        "CreateRole",
        &[
            ("RoleName", name.to_string()),
            (
                "AssumeRolePolicyDocument",
                trust("Allow", "sts:AssumeRole", "\"ec2.amazonaws.com\""),
            ),
        ],
    )
    .await;
    cw(
        &emulator,
        "PutLogAlarm",
        &log_alarm_request(name, name, &role_arn(name)),
    )
    .await;
    let failed = until(
        &emulator,
        case,
        name,
        "report the evaluation error",
        refused,
    )
    .await;
    assert_eq!(
        failed.get("StateValue").and_then(Value::as_str),
        Some("INSUFFICIENT_DATA"),
        "case {case:?}: the alarm has no graded state before the repair, got {failed}"
    );

    // ONLY the trust policy changes — the alarm is not rewritten.
    iam(
        &emulator,
        case,
        "UpdateAssumeRolePolicy",
        &[
            ("RoleName", name.to_string()),
            (
                "PolicyDocument",
                trust("Allow", "sts:AssumeRole", "\"logs.amazonaws.com\""),
            ),
        ],
    )
    .await;
    let repaired = until(
        &emulator,
        case,
        name,
        "reach ALARM after the repair",
        executed,
    )
    .await;
    assert_eq!(
        repaired.get("EvaluationState"),
        None,
        "case {case:?}: an execution that ran clears the evaluation state, got {repaired}"
    );
    let reported = results(&repaired);
    assert!(
        reported.iter().any(|result| {
            result.get("status").and_then(Value::as_str) == Some("COMPLETE")
                && result
                    .get("value")
                    .and_then(Value::as_f64)
                    .is_some_and(|value| value >= THRESHOLD)
        }),
        "case {case:?}: the repaired alarm must rest on a computed result, got {reported:?}"
    );
}
