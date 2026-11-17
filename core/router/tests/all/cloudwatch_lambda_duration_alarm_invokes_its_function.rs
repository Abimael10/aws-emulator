// SPDX-License-Identifier: CC0-1.0

//! An invoked function's own `AWS/Lambda` latency series drives a CloudWatch alarm whose
//! action is a LAMBDA FUNCTION — through the WHOLE mounted service set, the one
//! `core/router/src/lib.rs` assembles for the binary.
//!
//! The RULE under test, in one sentence: **every invocation a function serves is metered on
//! that function's own `AWS/Lambda` series under its `FunctionName` dimension — one
//! `Duration` observation, in `Milliseconds`, carrying exactly the time that run's own
//! `REPORT` line states, and one `Invocations` count — so a metric alarm over that series
//! transitions when the statistic it grades crosses its threshold, and the transition
//! executes each action it fires: an action naming a function invokes THAT function with the
//! state change, and the target's own resource-based policy decides the invocation, so a
//! function granting this service `lambda:InvokeFunction` runs and a function granting it
//! nothing is not invoked at all.**
//!
//! Five services have to agree for that to hold — CloudFormation archives each inline handler
//! into a deployment package this emulator can run, IAM holds the execution role, Lambda
//! serves the invocations and publishes the series, CloudWatch answers for the series and
//! owes the evaluation, and CloudWatch Logs holds what a run printed — and the invocation
//! CloudWatch makes for an alarm action is an in-process call addressed the way a client's own
//! `lambda`-scoped call is, so only the production service set shows the whole chain. What the
//! crate-local suites pin and this one does not repeat:
//! `services/service-com-amazonaws-lambda-awsgirapiservice/tests/all/invocation_duration_report_agreement.rs`
//! pins the two planes' agreement over a table of run lengths and concurrencies, and
//! `services/service-com-amazonaws-cloudwatch-graniteserviceversion20100801/tests/all/lambda_alarm_actions.rs`
//! pins the policy decision over a table of grant forms against a transition a client FORCES
//! with `SetAlarmState`. What only this suite reaches is the chain joined end to end: the
//! alarm is moved by the datapoints an INVOCATION published, not by a forced state.
//!
//! The rows differ along what the rule generalises over: how long each metered run takes, the
//! threshold the graded statistic has to cross, how the invocation ADDRESSES the worker (its
//! bare name, its ARN — the series is dimensioned by the function's name either way), and
//! whether the target function's policy admits the invocation the transition fires. Beside
//! each row's watched series runs a control alarm with the same configuration over the series
//! of a function nothing invoked, which has no datapoint to breach on and must stay `OK`.
//!
//! Every namespace, metric, dimension and unit name here is written as a literal taken from
//! the AWS Lambda metric reference rather than read from a service crate's constants, so a
//! series published under a name no AWS reader would ask for fails here.

use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode};

/// The Region the whole wiring lives in. It is the one the shared stack-settling helper signs
/// its own reads for, so every call of this suite is made there too.
const REGION: &str = "us-east-1";

/// The `StackStatus` the deployment must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The awsQuery API version AWS CloudFormation declares.
const CLOUDFORMATION_API_VERSION: &str = "2010-05-15";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET: &str = "GraniteServiceVersion20100801";
/// See [`CLOUDWATCH_TARGET`].
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// CloudWatch Logs' `X-Amz-Target` prefix and its signing name.
const LOGS_TARGET: &str = "Logs_20140328";
/// See [`LOGS_TARGET`].
const LOGS_SIGNING_NAME: &str = "logs";

/// The namespace Lambda's own metrics live in, the two series this suite reads, the units the
/// AWS Lambda metric reference reports them in, and the dimension a function's own series is
/// reported per.
const NAMESPACE: &str = "AWS/Lambda";
/// See [`NAMESPACE`].
const DURATION_SERIES: &str = "Duration";
/// See [`NAMESPACE`].
const INVOCATIONS_SERIES: &str = "Invocations";
/// See [`NAMESPACE`].
const MILLISECONDS_UNIT: &str = "Milliseconds";
/// See [`NAMESPACE`].
const COUNT_UNIT: &str = "Count";
/// See [`NAMESPACE`].
const FUNCTION_DIMENSION: &str = "FunctionName";

/// The fixed text of the runtime's closing report and the field one run's own measured time is
/// stated in. Source: the `REPORT RequestId: <id> Duration: <d> ms Billed Duration: <b> ms`
/// line Lambda writes at the end of every invocation.
const REPORT_PREFIX: &str = "REPORT RequestId: ";
/// See [`REPORT_PREFIX`].
const REPORT_DURATION_FIELD: &str = "Duration: ";

/// How far a published `Duration` may differ from the figure its run's own `REPORT` line
/// states, in milliseconds: the report prints the same measurement to two decimal places, so
/// it rounds by at most half of the last place it prints.
const REPORT_ROUNDING_MS: f64 = 0.005;

/// Where `Invoke` carries `LogType`, the value that asks for the log tail the `REPORT` line is
/// read out of, and the header the tail comes back in.
const LOG_TYPE_HEADER: &str = "x-amz-log-type";
/// See [`LOG_TYPE_HEADER`].
const LOG_TYPE_TAIL: &str = "Tail";
/// See [`LOG_TYPE_HEADER`].
const LOG_RESULT_HEADER: &str = "x-amz-log-result";

/// The principal AWS has a client grant a Lambda alarm action to, and the IAM action an
/// invocation performs — which is what the grant has to name.
const ALARM_ACTION_PRINCIPAL: &str = "lambda.alarms.cloudwatch.amazonaws.com";
/// See [`ALARM_ACTION_PRINCIPAL`].
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The `Period`, in seconds, both the alarms and the metric reads below use: the one-minute
/// granularity Lambda's own series are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of the clock a metric read's window reaches, in seconds — wide enough
/// that the minute a datapoint was published in is inside it however a row straddles a minute
/// boundary.
const WINDOW_SECS: f64 = 1_800.0;

/// Each function's own `Timeout`, in seconds: well past what any run below needs on any host,
/// because what is under test is the metering and the handover rather than how fast an
/// interpreter gets through a handler.
const FUNCTION_TIMEOUT_SECS: u64 = 120;

/// How long this suite waits for work another service's clock owes — a stack settling, a
/// function reaching `Active`, an evaluation, an asynchronous invocation, a log delivery — and
/// how often it looks while waiting. Generous on purpose: nothing here is asserted about how
/// fast the host runs, and every wait is for a CONDITION rather than for a span.
const DEADLINE: Duration = Duration::from_secs(180);
/// See [`DEADLINE`].
const LOOK: Duration = Duration::from_millis(100);

/// What the target function prints, which is the line a client reading its log group looks for.
const PRINTED_PREFIX: &str = "ALARMED ";

/// The worker handler, whose run is what gets metered: it sleeps for as long as the event asks
/// and answers how long that was, so the time its `REPORT` line states is the row's to choose.
const WORKER_HANDLER: &str = r#"
import time


def handler(event, context):
    sleep_ms = event.get("sleep_ms") or 0
    if sleep_ms:
        time.sleep(sleep_ms / 1000.0)
    return {"slept_ms": sleep_ms}
"#;

/// The alarm action's target: it prints one line naming the alarm and the states it moved out
/// of and into, read out of the document the invocation handed it — so what the function was
/// handed is readable in its own log group and every claim about it is checked by this suite
/// rather than by the handler.
const RESPONDER_HANDLER: &str = r#"
def handler(event, context):
    data = event.get("alarmData") or {}
    state = (data.get("state") or {}).get("value") or "none"
    previous = (data.get("previousState") or {}).get("value") or "none"
    print("ALARMED " + str(data.get("alarmName")) + " " + str(previous) + "->" + str(state))
    return {"seen": data.get("alarmName")}
"#;

/// The whole emulator over one engine, exactly as the binary assembles it: the invocation
/// CloudWatch makes for an alarm action is dispatched back into this same router, so the run it
/// starts is a run this suite can read.
fn router() -> MultiServiceRouter {
    let engine = aws_emulator_router::new_engine();
    let mut router = aws_emulator_router::build_router(engine);
    // Signature verification is the front door's concern, not this suite's.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature is never checked: the router above runs with authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
    headers: &[(&str, &str)],
) -> (u16, http::HeaderMap, Value) {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"));
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// One `X-Amz-Target` RPC call against the service whose signing name is `signing_name`, as its
/// status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CloudWatch call, which must answer 200.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET,
        "1.0",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {answered}"
    );
    answered
}

/// One CloudWatch Logs call, as its status and parsed body — a group no run has written to yet
/// does not exist, which is a refusal rather than a fault.
async fn logs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, LOGS_SIGNING_NAME, LOGS_TARGET, "1.1", operation, body).await
}

/// Now, in the epoch seconds a stored datapoint is stamped with and CloudWatch's `awsJson` wire
/// carries an instant as.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for a
/// function to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The inline document the execution role carries: the CloudWatch Logs calls an invocation's
/// log delivery makes, which is what the AWS-managed basic execution policy grants a function
/// and what makes a run's printed line readable in its own log group.
fn role_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": [
                "logs:CreateLogGroup",
                "logs:CreateLogStream",
                "logs:PutLogEvents",
            ],
            "Resource": "*",
        }],
    })
}

/// The template one row deploys: its execution role, the worker whose runs are metered, and the
/// target the alarm action names. The inline source is archived into each deployment package by
/// CloudFormation, which is how a function whose handler is written here comes to run at all.
fn stack_template(row_names: &RowNames) -> String {
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "ExecutionRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": row_names.role,
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "basic-execution",
                        "PolicyDocument": role_document(),
                    }],
                },
            },
            "Worker": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row_names.worker,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["ExecutionRole", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT_SECS,
                    "Code": {"ZipFile": WORKER_HANDLER},
                },
            },
            "Responder": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": row_names.responder,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["ExecutionRole", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT_SECS,
                    "Code": {"ZipFile": RESPONDER_HANDLER},
                },
            },
        },
    })
    .to_string()
}

/// Deploy one row's stack and wait for it to settle.
async fn deploy(router: &MultiServiceRouter, case: &str, names: &RowNames) {
    let template = stack_template(names);
    let (status, answered) = query(
        router,
        "cloudformation",
        CLOUDFORMATION_API_VERSION,
        "CreateStack",
        &[
            ("StackName", names.stack.as_str()),
            ("TemplateBody", &template),
            // The template declares a named IAM role, which a create acknowledges before
            // CloudFormation provisions it.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant the owning services report its resources
    // ready, which no fixed sleep can name: this looks until it reports it.
    let reported = wait_for_stack_status(router, &names.stack, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            CLOUDFORMATION_API_VERSION,
            "DescribeStackEvents",
            &[("StackName", names.stack.as_str())],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "case {case:?}: the stack must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
        reported.1
    );
}

/// The ARN Lambda assigned `function`, once it reports `Active` — the state an invocation needs
/// ("During this time, you can't invoke or modify the function", `CreateFunction`). Waited out
/// by LOOKING at the function's own `State` rather than by sleeping a span.
async fn active_function_arn(router: &MultiServiceRouter, case: &str, function: &str) -> String {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let (status, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            b"",
            &[],
        )
        .await;
        if status == 200 && configuration["State"].as_str() == Some("Active") {
            return configuration["FunctionArn"]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: an active function reports a FunctionArn, got {configuration}")
                })
                .to_string();
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: {function:?} must report Active within {DEADLINE:?}, last read \
             {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// One synchronous invocation of the worker, asking for the log tail the `REPORT` line is read
/// out of; answers the milliseconds that run's own report states.
async fn metered_run(
    router: &MultiServiceRouter,
    case: &str,
    addressed_as: &str,
    sleep_ms: u64,
) -> f64 {
    let (status, headers, answered) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{addressed_as}/invocations"),
        json!({"sleep_ms": sleep_ms}).to_string().as_bytes(),
        &[(LOG_TYPE_HEADER, LOG_TYPE_TAIL)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the invocation must be served, got {status} with {answered}"
    );
    assert_eq!(
        answered["slept_ms"].as_u64(),
        Some(sleep_ms),
        "case {case:?}: the run must answer what it slept, got {answered}"
    );
    let reported = reported_duration_ms(case, &headers);
    assert!(
        reported + REPORT_ROUNDING_MS >= sleep_ms as f64,
        "case {case:?}: the run slept {sleep_ms} ms, so its report must state at least that, \
         got {reported}"
    );
    reported
}

/// The `Duration`, in milliseconds, the `REPORT` line of one invocation's own log tail states.
fn reported_duration_ms(case: &str, headers: &http::HeaderMap) -> f64 {
    let encoded = headers
        .get(LOG_RESULT_HEADER)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: {LOG_TYPE_TAIL} must answer a tail in {LOG_RESULT_HEADER}")
        });
    let decoded = ::aws_smithy_types::base64::decode(encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the log tail must be base64 ({error})"));
    let tail = String::from_utf8(decoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the log tail must be text ({error})"));
    let report = tail
        .lines()
        .find(|line| line.starts_with(REPORT_PREFIX))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the tail must carry a report line, got {tail:?}")
        });
    // The plain `Duration` field precedes `Billed Duration` in the line, so the first match is
    // the measured time rather than the billed one.
    let (_, after) = report.split_once(REPORT_DURATION_FIELD).unwrap_or_else(|| {
        panic!("case {case:?}: the report must state {REPORT_DURATION_FIELD:?}, got {report:?}")
    });
    let stated = after.split_whitespace().next().unwrap_or_else(|| {
        panic!("case {case:?}: {REPORT_DURATION_FIELD:?} must carry a number, got {report:?}")
    });
    stated
        .parse::<f64>()
        .unwrap_or_else(|error| panic!("case {case:?}: {stated:?} is no number ({error})"))
}

/// The `Datapoints` `GetMetricStatistics` answers for the series `metric` names on the function
/// `function`, selected by the unit the AWS Lambda metric reference gives it.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    metric: &str,
    unit: &str,
) -> Vec<Value> {
    let now = now_secs();
    let answered = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": [{"Name": FUNCTION_DIMENSION, "Value": function}],
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum", "SampleCount", "Minimum", "Maximum"],
            "Unit": unit,
        }),
    )
    .await;
    answered["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The same read, looked at again until the series reports at least `wanted` observations or
/// the deadline passes — so a host that is slow to make a published datapoint readable cannot
/// decide the outcome, while a caller expecting none takes one look and reports what it found.
async fn awaited_datapoints(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    metric: &str,
    unit: &str,
    wanted: f64,
) -> Vec<Value> {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let points = datapoints(router, case, function, metric, unit).await;
        if total(&points, "SampleCount") >= wanted || Instant::now() >= giving_up_at {
            return points;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// The total of one statistic over every datapoint of a series.
fn total(points: &[Value], statistic: &str) -> f64 {
    points
        .iter()
        .filter_map(|point| point[statistic].as_f64())
        .sum()
}

/// The greatest `Maximum` any datapoint of a series reports, and the least `Minimum` — `None`
/// for a series holding no datapoint at all.
fn greatest(points: &[Value]) -> Option<f64> {
    points
        .iter()
        .filter_map(|point| point["Maximum"].as_f64())
        .max_by(f64::total_cmp)
}

/// See [`greatest`].
fn least(points: &[Value]) -> Option<f64> {
    points
        .iter()
        .filter_map(|point| point["Minimum"].as_f64())
        .min_by(f64::total_cmp)
}

/// Put one metric alarm over the `Duration` series of `watched`, grading the slowest run of each
/// period against `threshold_ms`, with `actions` as its `AlarmActions`.
async fn put_alarm(
    router: &MultiServiceRouter,
    case: &str,
    alarm: &str,
    watched: &str,
    threshold_ms: f64,
    actions: &[&str],
) {
    cloudwatch(
        router,
        case,
        "PutMetricAlarm",
        &json!({
            "AlarmName": alarm,
            "Namespace": NAMESPACE,
            "MetricName": DURATION_SERIES,
            "Dimensions": [{"Name": FUNCTION_DIMENSION, "Value": watched}],
            // The slowest run of a period is what a latency alarm grades, and the unit selects
            // the data it is graded over: "If you specify a unit, the operation returns only
            // data that was collected with that unit specified".
            "Statistic": "Maximum",
            "Unit": MILLISECONDS_UNIT,
            "Period": PERIOD_SECS,
            "EvaluationPeriods": 1,
            "Threshold": threshold_ms,
            "ComparisonOperator": "GreaterThanThreshold",
            // A function nobody invoked publishes no datapoint at all, and a period with no
            // data is not a breach of a latency threshold.
            "TreatMissingData": "notBreaching",
            "AlarmActions": actions,
        }),
    )
    .await;
}

/// What `DescribeAlarms` reports for the alarm: the member `member` names.
async fn described_alarm(
    router: &MultiServiceRouter,
    case: &str,
    alarm: &str,
    member: &str,
) -> String {
    let described = cloudwatch(
        router,
        case,
        "DescribeAlarms",
        &json!({"AlarmNames": [alarm], "AlarmTypes": ["MetricAlarm"]}),
    )
    .await;
    described["MetricAlarms"][0][member]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// What `DescribeAlarms` reports as the alarm's state.
async fn alarm_state(router: &MultiServiceRouter, case: &str, alarm: &str) -> String {
    described_alarm(router, case, alarm, "StateValue").await
}

/// Look at the alarm until it reports `wanted`, which is the evaluation the clock owes having
/// happened — no call forces it.
async fn awaited_alarm_state(
    router: &MultiServiceRouter,
    case: &str,
    alarm: &str,
    wanted: &str,
) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
        let last = alarm_state(router, case, alarm).await;
        if last == wanted {
            return;
        }
        assert!(
            Instant::now() < giving_up_at,
            "case {case:?}: {alarm:?} must report {wanted} within {DEADLINE:?}, last saw {last:?}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// The `Action` history items the alarm holds, as `DescribeAlarmHistory` reports them: one per
/// action execution the alarm accounted for, either way.
async fn action_items(router: &MultiServiceRouter, case: &str, alarm: &str) -> Vec<Value> {
    let described = cloudwatch(
        router,
        case,
        "DescribeAlarmHistory",
        &json!({
            "AlarmName": alarm,
            "AlarmTypes": ["MetricAlarm"],
            "HistoryItemType": "Action",
        }),
    )
    .await;
    described["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// The lines `FilterLogEvents` reports for a function's own log group — "By default, Lambda
/// functions send logs to a default log group named `/aws/lambda/<function name>`"
/// (`LoggingConfig$LogGroup`). A group no run has written to yet holds nothing.
async fn logged(router: &MultiServiceRouter, function: &str) -> Vec<String> {
    let (status, body) = logs(
        router,
        "FilterLogEvents",
        &json!({"logGroupName": format!("/aws/lambda/{function}")}),
    )
    .await;
    if status != 200 {
        return Vec::new();
    }
    body["events"]
        .as_array()
        .map(|events| {
            events
                .iter()
                .filter_map(|event| event["message"].as_str().map(ToString::to_string))
                .collect()
        })
        .unwrap_or_default()
}

/// The names one row's resources carry, so no two rows share one.
struct RowNames {
    stack: String,
    role: String,
    worker: String,
    responder: String,
    alarm: String,
    control_alarm: String,
    /// The function the control alarm watches, which nothing ever invokes.
    untouched: String,
}

impl RowNames {
    fn of(index: usize) -> Self {
        Self {
            stack: format!("cw-lambda-duration-alarm-{index}"),
            role: format!("cw-lambda-duration-alarm-role-{index}"),
            worker: format!("duration-worker-{index}"),
            responder: format!("alarm-responder-{index}"),
            alarm: format!("duration-high-{index}"),
            control_alarm: format!("duration-high-control-{index}"),
            untouched: format!("duration-untouched-{index}"),
        }
    }
}

/// One row: how long each of the worker's two metered runs sleeps, the threshold the slowest of
/// them has to cross, whether the invocation addresses the worker by its ARN rather than by its
/// bare name, and whether the target function's own policy admits the invocation the transition
/// fires.
struct Row {
    case: &'static str,
    sleeps_ms: [u64; 2],
    threshold_ms: f64,
    by_arn: bool,
    granted: bool,
}

/// The rows. Each one's threshold sits below the slower of its two runs, so the period holding
/// that run breaches it whichever minute the clock put the runs in.
const ROWS: &[Row] = &[
    Row {
        case: "a target granting this service the invocation, the worker addressed by name",
        sleeps_ms: [120, 700],
        threshold_ms: 60.0,
        by_arn: false,
        granted: true,
    },
    Row {
        case: "a target granting this service nothing, the worker addressed by its ARN",
        sleeps_ms: [900, 300],
        threshold_ms: 100.0,
        by_arn: true,
        granted: false,
    },
];

/// RULE: an invocation is metered on its function's own `AWS/Lambda` series with the time its
/// `REPORT` line states, an alarm over that series moves on the datapoints the invocations
/// published, and the transition invokes the function its action names exactly where that
/// function's own resource-based policy admits it.
#[tokio::test]
async fn an_invocations_duration_series_drives_the_alarm_that_invokes_its_target() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        // One emulator per row, so the only invocations in the window are this row's.
        let router = router();
        let names = RowNames::of(index);
        deploy(&router, case, &names).await;
        let worker_arn = active_function_arn(&router, case, &names.worker).await;
        let responder_arn = active_function_arn(&router, case, &names.responder).await;

        // The grant a client writes for an alarm action: the target function's own
        // resource-based policy, naming the principal CloudWatch invokes it as.
        if row.granted {
            let (status, _, answered) = lambda(
                &router,
                "POST",
                &format!("/2015-03-31/functions/{}/policy", names.responder),
                json!({
                    "StatementId": "cloudwatch-alarm-action",
                    "Action": INVOKE_ACTION,
                    "Principal": ALARM_ACTION_PRINCIPAL,
                })
                .to_string()
                .as_bytes(),
                &[],
            )
            .await;
            assert_eq!(
                status, 201,
                "case {case:?}: AddPermission must be accepted, got {status} with {answered}"
            );
        }

        // Both alarms are configured the same way; they differ only in the function whose
        // series they watch, and nothing ever invokes the control's.
        put_alarm(
            &router,
            case,
            &names.alarm,
            &names.worker,
            row.threshold_ms,
            &[responder_arn.as_str()],
        )
        .await;
        put_alarm(
            &router,
            case,
            &names.control_alarm,
            &names.untouched,
            row.threshold_ms,
            &[],
        )
        .await;
        // Both settle at OK before any run is metered, so the transition the runs cause is the
        // OK -> ALARM one and the control has somewhere to stay.
        awaited_alarm_state(&router, case, &names.alarm, "OK").await;
        awaited_alarm_state(&router, case, &names.control_alarm, "OK").await;

        // The metered runs, and what each one's own report stated for it.
        let addressed_as = if row.by_arn {
            worker_arn.clone()
        } else {
            names.worker.clone()
        };
        let mut reported = Vec::new();
        for sleep_ms in row.sleeps_ms {
            reported.push(metered_run(&router, case, &addressed_as, sleep_ms).await);
        }
        let runs = reported.len() as f64;
        let reported_max = reported.iter().copied().fold(f64::MIN, f64::max);
        assert!(
            reported_max > row.threshold_ms,
            "case {case:?}: the slowest run must pass the alarm's {} ms threshold, got \
             {reported_max}",
            row.threshold_ms
        );

        // The latency plane: one `Duration` observation per run, carrying the figure that run's
        // own report stated — dimensioned by the function's NAME however the call addressed it.
        let durations = awaited_datapoints(
            &router,
            case,
            &names.worker,
            DURATION_SERIES,
            MILLISECONDS_UNIT,
            runs,
        )
        .await;
        let samples = total(&durations, "SampleCount");
        assert_eq!(
            samples, runs,
            "case {case:?}: expected {runs} {DURATION_SERIES} measurement(s) of {:?}, got \
             {samples} in {durations:?}",
            names.worker
        );
        let tolerance = REPORT_ROUNDING_MS * runs;
        let published_sum = total(&durations, "Sum");
        let reported_sum: f64 = reported.iter().sum();
        assert!(
            (published_sum - reported_sum).abs() <= tolerance,
            "case {case:?}: expected {DURATION_SERIES} Sum {reported_sum} (the reports' own \
             total), got {published_sum}"
        );
        let published_max = greatest(&durations)
            .unwrap_or_else(|| panic!("case {case:?}: {DURATION_SERIES} must hold a datapoint"));
        assert!(
            (published_max - reported_max).abs() <= REPORT_ROUNDING_MS,
            "case {case:?}: expected {DURATION_SERIES} Maximum {reported_max}, got {published_max}"
        );
        let reported_min = reported.iter().copied().fold(f64::MAX, f64::min);
        let published_min = least(&durations)
            .unwrap_or_else(|| panic!("case {case:?}: {DURATION_SERIES} must hold a datapoint"));
        assert!(
            (published_min - reported_min).abs() <= REPORT_ROUNDING_MS,
            "case {case:?}: expected {DURATION_SERIES} Minimum {reported_min}, got {published_min}"
        );

        // The throughput plane: every run was one invocation Lambda admitted.
        let invocations = awaited_datapoints(
            &router,
            case,
            &names.worker,
            INVOCATIONS_SERIES,
            COUNT_UNIT,
            runs,
        )
        .await;
        let counted = total(&invocations, "Sum");
        assert_eq!(
            counted, runs,
            "case {case:?}: expected {INVOCATIONS_SERIES} Sum {runs}, got {counted} in \
             {invocations:?}"
        );

        // The chain: the evaluation the clock owes moves the alarm past its threshold and the
        // transition accounts for its one action. The control alarm is looked at every time
        // round, so its silence is checked for as long as the subject's chain is in flight.
        let giving_up_at = Instant::now() + DEADLINE;
        let items = loop {
            let items = action_items(&router, case, &names.alarm).await;
            let standing = alarm_state(&router, case, &names.control_alarm).await;
            assert_eq!(
                standing, "OK",
                "case {case:?}: the alarm watching a function nothing invoked must stay OK, got \
                 {standing:?}"
            );
            if !items.is_empty() {
                break items;
            }
            assert!(
                Instant::now() < giving_up_at,
                "case {case:?}: the breaching alarm must account for its action within \
                 {DEADLINE:?}"
            );
            tokio::time::sleep(LOOK).await;
        };
        assert_eq!(
            items.len(),
            1,
            "case {case:?}: expected exactly one accounted action execution, got {items:?}"
        );
        let item = &items[0];
        let summary = item["HistorySummary"].as_str().unwrap_or_default();
        let data: Value = serde_json::from_str(item["HistoryData"].as_str().unwrap_or("null"))
            .unwrap_or(Value::Null);

        if row.granted {
            assert_eq!(
                summary,
                format!("Successfully executed action {responder_arn}"),
                "case {case:?}: expected the action recorded as executed, got {summary:?} with \
                 {data}"
            );
            // What the target was handed is the transition itself: the alarm's identity beside
            // the states it moved out of and into.
            let published: Value =
                serde_json::from_str(data["publishedMessage"].as_str().unwrap_or("null"))
                    .unwrap_or(Value::Null);
            let alarm_arn = described_alarm(&router, case, &names.alarm, "AlarmArn").await;
            assert!(
                alarm_arn.ends_with(&format!(":alarm:{}", names.alarm)),
                "case {case:?}: the alarm must report an ARN naming itself, got {alarm_arn:?}"
            );
            for (member, pointer, wanted) in [
                ("the alarm's ARN", "/alarmArn", json!(alarm_arn)),
                ("the alarm's name", "/alarmData/alarmName", json!(names.alarm)),
                ("the state it moved to", "/alarmData/state/value", json!("ALARM")),
                (
                    "the state it moved out of",
                    "/alarmData/previousState/value",
                    json!("OK"),
                ),
            ] {
                let found = published.pointer(pointer).cloned().unwrap_or(Value::Null);
                assert_eq!(
                    found, wanted,
                    "case {case:?}: expected {member} to be {wanted} at {pointer} of the payload \
                     the function was handed, got {found} in {published}"
                );
            }
            // And the target RAN: the line its handler printed is in its own log group, naming
            // the alarm and the transition it was handed.
            let expected = format!("{PRINTED_PREFIX}{} OK->ALARM", names.alarm);
            let giving_up_at = Instant::now() + DEADLINE;
            loop {
                let lines = logged(&router, &names.responder).await;
                if lines.iter().any(|line| line.contains(&expected)) {
                    break;
                }
                assert!(
                    Instant::now() < giving_up_at,
                    "case {case:?}: expected the target to print {expected:?} into its own log \
                     group within {DEADLINE:?}, last saw {lines:?}"
                );
                tokio::time::sleep(LOOK).await;
            }
            // The run is an invocation of the TARGET, so Lambda meters it on that function's
            // own series too — the two planes agree about the same handover.
            let target_invocations = awaited_datapoints(
                &router,
                case,
                &names.responder,
                INVOCATIONS_SERIES,
                COUNT_UNIT,
                1.0,
            )
            .await;
            let target_counted = total(&target_invocations, "Sum");
            assert_eq!(
                target_counted, 1.0,
                "case {case:?}: expected the target's own {INVOCATIONS_SERIES} series to count \
                 the one invocation the action made, got {target_counted} in \
                 {target_invocations:?}"
            );
            continue;
        }

        // The refused arm: nothing was invoked, and the alarm's history says why.
        assert_eq!(
            summary,
            format!("Failed to execute action {responder_arn}"),
            "case {case:?}: expected the action recorded as failed, got {summary:?} with {data}"
        );
        let reported_error = data["error"].as_str().unwrap_or_default();
        let expected = format!(
            "User: {ALARM_ACTION_PRINCIPAL} is not authorized to perform: {INVOKE_ACTION} on \
             resource: {responder_arn} because no resource-based policy allows the \
             {INVOKE_ACTION} action"
        );
        assert_eq!(
            reported_error, expected,
            "case {case:?}: expected the recorded error to be {expected:?}, got {reported_error:?}"
        );
        assert!(
            data["publishedMessage"].is_null(),
            "case {case:?}: a refused action hands nothing over, so it records no published \
             message, got {data}"
        );
        // Nothing ran: the target's log group holds no line of its handler, and its own
        // invocation series holds nothing either. The record above is written as the handover is
        // decided, so a run owed here would already have been started.
        let lines = logged(&router, &names.responder).await;
        assert!(
            !lines.iter().any(|line| line.contains(PRINTED_PREFIX)),
            "case {case:?}: expected no run of the refused target, got {lines:?}"
        );
        let target_invocations = datapoints(
            &router,
            case,
            &names.responder,
            INVOCATIONS_SERIES,
            COUNT_UNIT,
        )
        .await;
        let target_counted = total(&target_invocations, "Sum");
        assert!(
            target_counted.abs() < f64::EPSILON,
            "case {case:?}: expected no invocation of the refused target in its own \
             {INVOCATIONS_SERIES} series, got {target_counted} in {target_invocations:?}"
        );
    }
}
