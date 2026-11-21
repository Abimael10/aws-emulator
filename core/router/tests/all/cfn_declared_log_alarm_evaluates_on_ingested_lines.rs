// SPDX-License-Identifier: CC0-1.0

//! The WHOLE monitoring estate one stack declares — a log group, the metric filter over its
//! lines, and the alarm over the metric that filter publishes — read and driven through the
//! three services that own it, on the emulator `core/router/src/lib.rs` assembles for the
//! binary.
//!
//! The RULE under test: **a resource a stack reports created exists, before the stack reaches
//! `CREATE_COMPLETE`, in the service that OWNS it, under the identifier the stack publishes for
//! it, and behaves exactly as that service's own create would have made it behave** — so the
//! alarm an `AWS::CloudWatch::Alarm` declares is the one entry
//! `DescribeAlarms(AlarmNames=[<the published name>])` reports, carrying the declaration's
//! namespace, metric, statistic, period, evaluation periods, threshold, comparison operator and
//! missing-data treatment and no action it did not declare; it is EVALUATED by its owning
//! service's own clock, so it leaves the `INSUFFICIENT_DATA` a create sets for the state its
//! empty series and `TreatMissingData` imply and records that one transition in
//! `DescribeAlarmHistory`; and it is moved to `ALARM` by datapoints the `AWS::Logs::MetricFilter`
//! of the same stack publishes out of lines `PutLogEvents` ingests into the stack's own log
//! group, recording that transition too.
//!
//! The reported case deployed exactly that estate, polled the stack to `CREATE_COMPLETE` — the
//! Logs half of it real, the group reporting the declared retention and a metric-filter count of
//! one — and then found `MetricAlarms` EMPTY for the name the stack published: the first read of
//! the deploy-then-diagnose flow found nothing, so nothing after it (the alarm's configuration,
//! its settled state, its history, the transition a matching log line should drive) was
//! reachable, while the deployment looked green.
//!
//! The suites this one divides the rule with, so the overlap stays visible:
//! `core/router/tests/all/cfn_declared_alarm_is_held_by_the_cloudwatch_store.rs` pins the
//! declared alarm's presence in CloudWatch's own STORE (resolved through the series, named in a
//! composite rule, its actions enabled and disabled) and stops at the state its empty series
//! implies, and `core/router/tests/all/logs_metric_filter_drives_the_alarm_notification_chain.rs`
//! pins the extraction-to-notification chain for a filter and an alarm a CLIENT put, whose
//! aggregate arithmetic this suite reuses rather than copying. What only this suite reaches is
//! the EVALUATION of an alarm no client call created: its recorded settling transition, and the
//! transition the stack's own filter drives it through.
//!
//! The rows differ along what the rule generalises over rather than restating that one template:
//! the FORM of the declared `FilterPattern` (an unstructured term, a JSON pattern over a member,
//! a space-delimited field list) and of the `MetricValue` the transformation publishes (a literal
//! and a selected field), the `Statistic` the alarm aggregates the period with, the direction of
//! its `ComparisonOperator`, the `Threshold` spelled as an integer and as a fraction, the
//! retention the group declares, and how many of the ingested lines the filter matches. Every
//! aggregate a row expects is computed from the row's own lines.
//!
//! Nothing is waited out: the stacks are asked for together and each is polled to the status it
//! settles on, and every alarm state is polled until the evaluation its owning service's clock
//! owes has been materialised.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::logs_metric_filter_drives_the_alarm_notification_chain::{aggregate, breaches};
use crate::query_wire::{elements, form_encode, opt_element, outputs_of};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-west-2";

/// The status a stack whose resources were all created reports, and the one it reports while it
/// is still working through them.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";
const CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`), and
/// CloudWatch Logs' own.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";
const LOGS_TARGET_PREFIX: &str = "Logs_20140328";
const LOGS_SIGNING_NAME: &str = "logs";

/// The three values of `StateValue` this suite names
/// (`smithy_query(query="shape",shape="StateValue")`): the one a create sets
/// ("When this operation creates an alarm, the alarm state is immediately set to
/// INSUFFICIENT_DATA", `PutMetricAlarm`), the one an empty series under `notBreaching` settles
/// in, and the one a breaching series moves the alarm to.
const STATE_INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";
const STATE_OK: &str = "OK";
const STATE_ALARM: &str = "ALARM";

/// The `HistoryItemType` a state transition is recorded under
/// (`smithy_query(query="shape",shape="HistoryItemType")`), and the order a history read asks
/// for so the items come back oldest first (`DescribeAlarmHistoryInput$ScanBy`).
const STATE_UPDATE: &str = "StateUpdate";
const ASCENDING: &str = "TimestampAscending";

/// The alarm type every alarm here is (`smithy_query(query="shape",shape="AlarmType")`), asked
/// for explicitly on a history read because that read reports composite-alarm history unless it
/// is told otherwise (`DescribeAlarmHistoryInput$AlarmTypes`).
const METRIC_ALARM: &str = "MetricAlarm";

/// The missing-data treatment every row declares: "notBreaching — Missing data points are
/// treated as 'good' and within the threshold"
/// (`smithy_query(query="doc",shape="PutMetricAlarmInput",member="TreatMissingData")`), so an
/// alarm over a series nothing has published into settles in `OK`.
const NOT_BREACHING: &str = "notBreaching";

/// How many of the alarm's periods one evaluation grades. One, so the window a row's own lines
/// land in is the window its alarm is graded on.
const EVALUATION_PERIODS: i64 = 1;

/// The period every row's alarm aggregates, in seconds: one minute, the shortest regular
/// resolution a metric filter's series is published at.
const PERIOD_SECS: i64 = 60;

/// The logical ids the template declares its three resources and its one stream under, and the
/// output keys it publishes the alarm's and the group's identity under.
const GROUP_ID: &str = "Group";
const STREAM_ID: &str = "Stream";
const FILTER_ID: &str = "Filter";
const ALARM_ID: &str = "Alarm";
const ALARM_NAME_OUTPUT: &str = "AlarmName";
const GROUP_NAME_OUTPUT: &str = "LogGroupName";

/// How long a row keeps looking for something the clock owes before calling it undelivered, and
/// how often it looks again.
///
/// Generous on purpose, and never measured: what a row asserts is WHAT it read, never how long
/// the reading took, so a loaded host takes longer and still decides the same way.
const DEADLINE: Duration = Duration::from_secs(90);
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The whole emulator over one engine, exactly as the binary assembles it.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service` in [`REGION`], which is what
/// routes the request. The signature is never checked: the router above runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One CloudFormation `awsQuery` form call, which must answer 200.
async fn cfn(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudFormation {action}, got {status} with {text}"
    );
    text
}

/// One `X-Amz-Target` RPC call, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Amazon CloudWatch call (`awsJson1_0`), which must answer 200.
async fn cw(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon CloudWatch Logs call (`awsJson1_1`), which must answer 200.
async fn logs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(
        router,
        LOGS_SIGNING_NAME,
        LOGS_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from CloudWatch Logs {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// Milliseconds since the epoch, which is the unit a log event is stamped in
/// (`InputLogEvent$timestamp`).
fn now_ms() -> i64 {
    i64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("the clock is after the epoch")
            .as_millis(),
    )
    .expect("the epoch milliseconds fit an i64")
}

/// One row: the estate one template declares, the lines ingested into the group it declares, and
/// how its alarm grades the window those lines land in.
struct Row {
    /// What this row varies, named in every assertion it makes.
    case: &'static str,
    /// The stack, the log group, the log stream, the filter and the alarm this row deploys.
    stack: &'static str,
    group: &'static str,
    stream: &'static str,
    filter: &'static str,
    alarm: &'static str,
    /// The series the filter publishes into and the alarm watches: one per row, so a row reads
    /// its own metric and no other.
    namespace: &'static str,
    metric: &'static str,
    /// The retention the group declares. "Possible values are: 1, 3, 5, 7, 14, 30, 60, 90, 120,
    /// 150, 180, 365, 400, 545, 731, 1096, 1827, 2192, 2557, 2922, 3288, and 3653"
    /// (`AWS::Logs::LogGroup`'s `RetentionInDays`, which is `PutRetentionPolicy`'s own
    /// `retentionInDays`).
    retention: i64,
    /// The `FilterPattern` the declaration carries and the `MetricValue` its one transformation
    /// publishes.
    pattern: &'static str,
    metric_value: &'static str,
    /// The `Statistic` the alarm aggregates the period with
    /// (`smithy_query(query="shape",shape="Statistic")`).
    statistic: &'static str,
    /// The `ComparisonOperator` it grades that aggregate with
    /// (`smithy_query(query="shape",shape="ComparisonOperator")`).
    comparison: &'static str,
    /// The `Threshold` exactly as the template spells it — an integer or a fraction, both of
    /// which CloudWatch holds as one `Double` (`PutMetricAlarmInput$Threshold`).
    threshold: Value,
    /// The lines ingested as one batch, each paired with the value the declared filter publishes
    /// for it — `None` for a line the pattern does not match, which publishes nothing because the
    /// transformation declares no `DefaultValue`.
    lines: &'static [(&'static str, Option<f64>)],
}

/// The rows, spanning the class the rule covers.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the reported estate: an unstructured term, a literal metric value and a Sum \
                   at a threshold of one",
            stack: "sftp-alarm-term",
            group: "/aws/transfer/sftp-alarm-term",
            stream: "sftp-alarm-term-stream",
            filter: "sftp-alarm-term-filter",
            alarm: "sftp-alarm-term-alarm",
            namespace: "Sftp/Diagnostics",
            metric: "SftpErrors",
            retention: 1,
            pattern: "ERROR",
            metric_value: "1",
            statistic: "Sum",
            comparison: "GreaterThanOrEqualToThreshold",
            threshold: json!(1),
            lines: &[
                ("INFO session opened", None),
                ("ERROR upload refused", Some(1.0)),
            ],
        },
        Row {
            case: "a JSON pattern over a member, graded by SampleCount above a fractional \
                   threshold",
            stack: "sftp-alarm-json",
            group: "/aws/transfer/sftp-alarm-json",
            stream: "sftp-alarm-json-stream",
            filter: "sftp-alarm-json-filter",
            alarm: "sftp-alarm-json-alarm",
            namespace: "Sftp/Structured",
            metric: "StructuredErrors",
            retention: 7,
            pattern: r#"{ $.level = "ERROR" }"#,
            metric_value: "1",
            statistic: "SampleCount",
            comparison: "GreaterThanThreshold",
            threshold: json!(1.5),
            lines: &[
                (r#"{"level":"ERROR","session":1}"#, Some(1.0)),
                (r#"{"level":"INFO","session":2}"#, None),
                (r#"{"level":"ERROR","session":3}"#, Some(1.0)),
            ],
        },
        Row {
            case: "a space-delimited field list whose field is the metric value, graded by \
                   Maximum",
            stack: "sftp-alarm-fields",
            group: "/aws/transfer/sftp-alarm-fields",
            stream: "sftp-alarm-fields-stream",
            filter: "sftp-alarm-fields-filter",
            alarm: "sftp-alarm-fields-alarm",
            namespace: "Sftp/Transfers",
            metric: "TransferredBytes",
            retention: 30,
            pattern: "[ip, status, size]",
            metric_value: "$size",
            statistic: "Maximum",
            comparison: "GreaterThanThreshold",
            threshold: json!(1024),
            lines: &[
                ("10.0.0.1 500 2048", Some(2048.0)),
                ("10.0.0.2 500 512", Some(512.0)),
                ("unparseable", None),
            ],
        },
    ]
}

/// The template a row deploys: the log group, the stream inside it, the metric filter over its
/// lines and the alarm over the metric that filter publishes, with the alarm's name and the
/// group's name as the stack's outputs.
///
/// `Ref` of an `AWS::CloudWatch::Alarm` returns the alarm's name and `Ref` of an
/// `AWS::Logs::LogGroup` returns the group's, which is the handle every call below takes.
fn template_of(row: &Row) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Description": "a log group, a metric filter over its lines and an alarm over the \
                        metric that filter publishes",
        "Resources": {
            GROUP_ID: {
                "Type": "AWS::Logs::LogGroup",
                "Properties": {
                    "LogGroupName": row.group,
                    "RetentionInDays": row.retention,
                },
            },
            STREAM_ID: {
                "Type": "AWS::Logs::LogStream",
                "Properties": {
                    "LogGroupName": {"Ref": GROUP_ID},
                    "LogStreamName": row.stream,
                },
            },
            FILTER_ID: {
                "Type": "AWS::Logs::MetricFilter",
                "Properties": {
                    "LogGroupName": {"Ref": GROUP_ID},
                    "FilterName": row.filter,
                    "FilterPattern": row.pattern,
                    "MetricTransformations": [{
                        "MetricNamespace": row.namespace,
                        "MetricName": row.metric,
                        "MetricValue": row.metric_value,
                    }],
                },
            },
            ALARM_ID: {
                "Type": "AWS::CloudWatch::Alarm",
                "DependsOn": FILTER_ID,
                "Properties": {
                    "AlarmName": row.alarm,
                    "Namespace": row.namespace,
                    "MetricName": row.metric,
                    "Statistic": row.statistic,
                    "Period": PERIOD_SECS,
                    "EvaluationPeriods": EVALUATION_PERIODS,
                    "Threshold": row.threshold,
                    "ComparisonOperator": row.comparison,
                    "TreatMissingData": NOT_BREACHING,
                },
            },
        },
        "Outputs": {
            ALARM_NAME_OUTPUT: {"Value": {"Ref": ALARM_ID}},
            GROUP_NAME_OUTPUT: {"Value": {"Ref": GROUP_ID}},
        },
    })
    .to_string()
}

/// Poll a row's stack until it first reports a status other than `CREATE_IN_PROGRESS`, answering
/// the `DescribeStacks` answer it was read from. The wait is on that CONDITION, looked at again
/// until it holds or [`DEADLINE`] passes, so nothing is waited out past it.
async fn settled_stack(router: &MultiServiceRouter, row: &Row) -> String {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = cfn(
            router,
            row.case,
            "DescribeStacks",
            &[("StackName", row.stack)],
        )
        .await;
        let status = opt_element(&described, "StackStatus").unwrap_or_default();
        if status != CREATE_IN_PROGRESS {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {:?}: the stack never left {CREATE_IN_PROGRESS}, last read {described}",
            row.case
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The one `MetricAlarms` entry `DescribeAlarms` reports for `name`, looked at again until its
/// `StateValue` is `wanted` — the state the evaluation the clock owes the alarm reaches.
///
/// `CompositeAlarms` must stay empty throughout: a metric alarm is reported under the metric
/// half of the answer and nowhere else ("if you omit this parameter or specify MetricAlarms, the
/// operation returns only a list of metric alarms",
/// `smithy_query(query="doc",shape="DescribeAlarmsInput",member="AlarmTypes")`).
async fn settled_alarm(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    wanted: &str,
) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = cw(router, case, "DescribeAlarms", &json!({"AlarmNames": [name]})).await;
        let reported = described["MetricAlarms"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            reported.len(),
            1,
            "case {case:?}: expected DescribeAlarms {name:?} to report exactly one MetricAlarms \
             entry, got {described}"
        );
        assert!(
            described["CompositeAlarms"]
                .as_array()
                .is_none_or(|alarms| alarms.is_empty()),
            "case {case:?}: expected no CompositeAlarms entry for the metric alarm {name:?}, got \
             {described}"
        );
        let last = reported[0].clone();
        if last["StateValue"].as_str() == Some(wanted) {
            return last;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the alarm {name:?} never reported {wanted:?}, last read {last}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The `HistorySummary` of every `StateUpdate` item `DescribeAlarmHistory` holds for `name`,
/// oldest first.
async fn state_history(router: &MultiServiceRouter, case: &str, name: &str) -> Vec<String> {
    let described = cw(
        router,
        case,
        "DescribeAlarmHistory",
        &json!({
            "AlarmName": name,
            "AlarmTypes": [METRIC_ALARM],
            "HistoryItemType": STATE_UPDATE,
            "ScanBy": ASCENDING,
        }),
    )
    .await;
    described["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|item| {
            item["HistorySummary"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The summary a transition between two states is recorded under.
fn transition(from: &str, to: &str) -> String {
    format!("Alarm updated from {from} to {to}")
}

/// Every member of the declaration as CloudWatch reports it for the alarm the stack published.
fn assert_configuration(case: &str, alarm: &Value, row: &Row) {
    for (member, expected, actual) in [
        (
            "AlarmName",
            Some(row.alarm),
            alarm["AlarmName"].as_str(),
        ),
        (
            "Namespace",
            Some(row.namespace),
            alarm["Namespace"].as_str(),
        ),
        ("MetricName", Some(row.metric), alarm["MetricName"].as_str()),
        (
            "Statistic",
            Some(row.statistic),
            alarm["Statistic"].as_str(),
        ),
        (
            "ComparisonOperator",
            Some(row.comparison),
            alarm["ComparisonOperator"].as_str(),
        ),
        (
            "TreatMissingData",
            Some(NOT_BREACHING),
            alarm["TreatMissingData"].as_str(),
        ),
    ] {
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected:?} on the deployed alarm, got {actual:?} \
             in {alarm}"
        );
    }
    for (member, expected, actual) in [
        ("Period", PERIOD_SECS, alarm["Period"].as_i64().unwrap_or(-1)),
        (
            "EvaluationPeriods",
            EVALUATION_PERIODS,
            alarm["EvaluationPeriods"].as_i64().unwrap_or(-1),
        ),
    ] {
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {member} {expected} on the deployed alarm, got {actual} in \
             {alarm}"
        );
    }
    let declared_threshold = row
        .threshold
        .as_f64()
        .expect("every row spells its threshold as a number");
    assert_eq!(
        alarm["Threshold"].as_f64(),
        Some(declared_threshold),
        "case {case:?}: expected Threshold {declared_threshold} on the deployed alarm, got {} in \
         {alarm}",
        alarm["Threshold"]
    );
    // The declaration states no action of any kind, so the alarm carries none: the deployment
    // configures what the template declares and nothing else.
    for member in ["AlarmActions", "OKActions", "InsufficientDataActions"] {
        let actions = alarm[member].as_array().cloned().unwrap_or_default();
        assert!(
            actions.is_empty(),
            "case {case:?}: a declaration stating no {member} must leave the alarm with none, \
             got {actions:?}"
        );
    }
    // "The ARN format of an alarm is arn:aws:cloudwatch:Region:account-id:alarm:alarm-name" —
    // the account is the emulator's own, so only the Region and the name are asserted here.
    let arn = alarm["AlarmArn"].as_str().unwrap_or_default();
    let tail = format!(":alarm:{}", row.alarm);
    assert!(
        arn.starts_with(&format!("arn:aws:cloudwatch:{REGION}:")) && arn.ends_with(&tail),
        "case {case:?}: expected the alarm's ARN to put it at {tail:?} in {REGION}, got {arn:?}"
    );
}

/// RULE: see the module header — the log group, the metric filter and the alarm a stack reports
/// created are all real in the services that own them, and the alarm is evaluated and driven by
/// the lines the stack's own filter extracts from.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_stacks_declared_alarm_is_evaluated_over_its_declared_metric_filters_lines() {
    let router = router();
    let rows = rows();

    // Every row's stack is asked for first, so the settling of one overlaps the settling of the
    // others rather than being waited out in turn.
    for row in &rows {
        cfn(
            &router,
            row.case,
            "CreateStack",
            &[
                ("StackName", row.stack),
                ("TemplateBody", &template_of(row)),
            ],
        )
        .await;
    }

    // 1. Each stack completes and publishes the identity of the alarm and of the group.
    for row in &rows {
        let case = row.case;
        let described = settled_stack(&router, row).await;
        let status = opt_element(&described, "StackStatus").unwrap_or_default();
        if status != CREATE_COMPLETE {
            let events = cfn(
                &router,
                case,
                "DescribeStackEvents",
                &[("StackName", row.stack)],
            )
            .await;
            let reasons = elements(&events, "ResourceStatusReason");
            panic!(
                "case {case:?}: the stack must reach {CREATE_COMPLETE}, got {status:?} with \
                 resource failures {reasons:?}"
            );
        }
        let published = outputs_of(&described);
        assert_eq!(
            published.get(ALARM_NAME_OUTPUT).map(String::as_str),
            Some(row.alarm),
            "case {case:?}: expected the stack to publish the declared alarm name, got \
             {published:?}"
        );
        assert_eq!(
            published.get(GROUP_NAME_OUTPUT).map(String::as_str),
            Some(row.group),
            "case {case:?}: expected the stack to publish the declared log group name, got \
             {published:?}"
        );

        // 2. The Logs half of the same template: the group carries the retention it declared and
        //    counts the one filter the stack put on it, and that filter is the declared one.
        let described_groups = logs(
            &router,
            case,
            "DescribeLogGroups",
            &json!({"logGroupNamePrefix": row.group}),
        )
        .await;
        let groups = described_groups["logGroups"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let group = groups
            .iter()
            .find(|group| group["logGroupName"].as_str() == Some(row.group))
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected DescribeLogGroups to report the declared group \
                     {:?}, got {described_groups}",
                    row.group
                )
            });
        assert_eq!(
            group["retentionInDays"].as_i64(),
            Some(row.retention),
            "case {case:?}: expected the declared retention {} on {:?}, got {group}",
            row.retention,
            row.group
        );
        assert_eq!(
            group["metricFilterCount"].as_i64(),
            Some(1),
            "case {case:?}: expected the group to count the one declared metric filter, got \
             {group}"
        );
        let described_filters = logs(
            &router,
            case,
            "DescribeMetricFilters",
            &json!({"logGroupName": row.group}),
        )
        .await;
        let filters = described_filters["metricFilters"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            filters.len(),
            1,
            "case {case:?}: expected exactly the declared filter on {:?}, got \
             {described_filters}",
            row.group
        );
        assert_eq!(
            filters[0]["filterName"].as_str(),
            Some(row.filter),
            "case {case:?}: expected the declared filter name, got {}",
            filters[0]
        );
        assert_eq!(
            filters[0]["filterPattern"].as_str(),
            Some(row.pattern),
            "case {case:?}: expected the declared filter pattern, got {}",
            filters[0]
        );

        // 3. The alarm the stack published is the ONE CloudWatch holds under that name, carrying
        //    the declaration, and its own evaluation settles it out of the state its creation
        //    set: nothing has been published into the series it watches and its missing windows
        //    are not breaching, so it reaches OK with no call forcing it.
        let settled = settled_alarm(&router, case, row.alarm, STATE_OK).await;
        assert_configuration(case, &settled, row);
        let history = state_history(&router, case, row.alarm).await;
        assert_eq!(
            history,
            vec![transition(STATE_INSUFFICIENT_DATA, STATE_OK)],
            "case {case:?}: expected the settling of the deployed alarm to be its one recorded \
             transition, got {history:?}"
        );
    }

    // 4. The lines, ingested into the stream the same stack declared inside the same group. They
    //    are stamped one period back, so they land in a window the clock has already closed and
    //    the next evaluation grades them rather than the one after it.
    for row in &rows {
        let timestamp = now_ms() - PERIOD_SECS * 1_000;
        let events: Vec<Value> = row
            .lines
            .iter()
            .map(|(message, _)| json!({"timestamp": timestamp, "message": message}))
            .collect();
        let accepted = logs(
            &router,
            row.case,
            "PutLogEvents",
            &json!({
                "logGroupName": row.group,
                "logStreamName": row.stream,
                "logEvents": events,
            }),
        )
        .await;
        assert!(
            accepted.get("rejectedLogEventsInfo").is_none(),
            "case {:?}: the batch must be accepted whole, got {accepted}",
            row.case
        );
    }

    // 5. What the declared filter made of those lines moves the declared alarm: the aggregate of
    //    the values its matching lines published crosses the declared threshold, so the alarm
    //    reaches ALARM and records that transition after the settling one.
    for row in &rows {
        let case = row.case;
        let published: Vec<f64> = row.lines.iter().filter_map(|(_, value)| *value).collect();
        let expected = aggregate(case, row.statistic, &published);
        let threshold = row
            .threshold
            .as_f64()
            .expect("every row spells its threshold as a number");
        assert!(
            breaches(case, row.comparison, expected, threshold),
            "case {case:?}: the row must state a threshold its own lines breach: {expected} {} \
             {threshold}",
            row.comparison
        );
        let fired = settled_alarm(&router, case, row.alarm, STATE_ALARM).await;
        assert_configuration(case, &fired, row);
        let history = state_history(&router, case, row.alarm).await;
        let wanted = [
            transition(STATE_INSUFFICIENT_DATA, STATE_OK),
            transition(STATE_OK, STATE_ALARM),
        ];
        assert!(
            history.len() >= wanted.len() && history[..wanted.len()] == wanted,
            "case {case:?}: expected the deployed alarm's history to open with {wanted:?}, got \
             {history:?}"
        );
    }
}
