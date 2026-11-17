// SPDX-License-Identifier: CC0-1.0

//! WHAT a Lambda alarm action's function is handed, read off the RUNNING function rather than
//! off this service's own record of it — and the `Action` history item the same execution
//! writes.
//!
//! The RULE asserted here: when an alarm's `StateValue` changes, "the actions to execute when
//! this alarm transitions to the ALARM state from any other state. Each action is specified as
//! an Amazon Resource Name (ARN)" (`PutMetricAlarmInput$AlarmActions`, and the same for
//! `OKActions` and `InsufficientDataActions`) are executed for the state it moved TO, and an
//! action naming a function invokes that function with ONE document describing that transition:
//! the publishing service as its `source`, the firing alarm's own ARN, and an `alarmData`
//! carrying the alarm's name, the state it moved to with the explanation the transition was
//! given, and the state it moved OUT of — and the same execution appends an `Action` item to
//! that alarm's history whose data reports it succeeded and whose summary names the ARN
//! executed, carrying the very document the function was handed.
//!
//! Nothing here reads the payload out of this crate: the deployed handler prints the event it
//! was invoked with into its own log group, and every member is asserted off THAT line — so a
//! document this service renders but never delivers, or delivers without recording, fails a
//! row. `tests/all/lambda_alarm_actions.rs` pins WHETHER the invocation is made (the target
//! function's resource-based policy deciding it) over a table of grant forms; this suite pins
//! WHAT the admitted invocation delivers.
//!
//! The rows differ along what the rule generalises over: the state the transition moves the
//! alarm INTO (so a different one of the three action lists names the function each time), the
//! state it moves OUT of (which is the state the row before it moved the alarm into, so one
//! alarm's deliveries are a chain), the wording and length of the explanation the transition
//! carries, and WHICH alarm fires — two alarms, each naming its own function, so an identity
//! reported for the wrong alarm fails.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{rpc, Emulator, REGION};

/// The IAM action an invocation performs, which is what the grant on the target function names
/// ("Actions defined by AWS Lambda", IAM Service Authorization Reference).
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The principal AWS has a client grant a Lambda alarm action to.
const ALARM_ACTION_PRINCIPAL: &str = "lambda.alarms.cloudwatch.amazonaws.com";

/// The `source` every event this service publishes carries: EventBridge names a publishing
/// service as `aws.<service>`. Written as a literal rather than read from a crate constant, so
/// a source no AWS reader would match on fails here.
const EVENT_SOURCE: &str = "aws.cloudwatch";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The role the created functions name. No IAM store is mounted here, so it is a value the
/// create carries rather than a record this suite relies on.
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-exec";

/// What the handler prefixes the event it was handed with, so the delivered document is told
/// from every other line the runtime writes into the log group.
const MARKER: &str = "DELIVERED:";

/// How long this suite waits for work another service's clock owes — a created function becoming
/// `Active`, the asynchronous invocation, and the handler's own log delivery — before giving up
/// on it. Generous on purpose: nothing here is asserted about how fast the host runs.
const WORK_WAIT: Duration = Duration::from_secs(60);

/// How often it looks while waiting.
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// The handler the rows deploy: it prints the WHOLE event it was invoked with, as JSON, on one
/// line — so what is asserted is the document the function itself received.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    print("DELIVERED:" + json.dumps(event))
    return {"ok": True}
"#;

/// CloudWatch, AWS Lambda and CloudWatch Logs over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the invocation CloudWatch makes for an alarm action
/// reaches Lambda's own handler, which runs the function, and the lines it printed are
/// delivered into the log group this suite reads.
fn emulator_with_lambda() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    Emulator { router, engine }
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One AWS Lambda call over its `restJson1` wire, answering its status and parsed body.
async fn lambda(emulator: &Emulator, method: &str, path: &str, body: Option<Value>) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(bytes::Bytes::from(payload))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = response.into_body();
    let parsed: Value = serde_json::from_slice(&answered).unwrap_or(Value::Null);
    (status, parsed)
}

/// One CloudWatch Logs call over its `awsJson1_1` wire, answering its status and parsed body.
async fn logs(emulator: &Emulator, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("logs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("Logs_20140328.{operation}"))
        .header("authorization", authorization("logs"))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = emulator.router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = response.into_body();
    let parsed: Value = serde_json::from_slice(&answered).unwrap_or(Value::Null);
    (status, parsed)
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

/// A metric alarm named `name` naming `function_arn` in EVERY one of its three action lists, so
/// whichever state a row forces the alarm into fires that function, and answer the ARN
/// CloudWatch assigned the alarm.
///
/// The watched series is dimensioned by the alarm's own name and never receives data, and
/// `TreatMissingData` is "ignore — the current alarm state is maintained"
/// (`PutMetricAlarmInput$TreatMissingData`), so the only transitions of this alarm are the ones
/// a row forces.
async fn given_alarm(emulator: &Emulator, name: &str, function_arn: &str) -> String {
    monitoring(
        emulator,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": "Custom/AlarmPayload",
            "MetricName": "Backlog",
            "Dimensions": [{"Name": "QueueName", "Value": name}],
            "Statistic": "Average",
            "Period": 60,
            "EvaluationPeriods": 1,
            "Threshold": 1.0,
            "ComparisonOperator": "GreaterThanThreshold",
            "TreatMissingData": "ignore",
            "AlarmActions": [function_arn],
            "OKActions": [function_arn],
            "InsufficientDataActions": [function_arn],
        }),
    )
    .await;
    let described = monitoring(
        emulator,
        "DescribeAlarms",
        &json!({"AlarmNames": [name], "AlarmTypes": ["MetricAlarm"]}),
    )
    .await;
    described["MetricAlarms"]
        .as_array()
        .and_then(|alarms| alarms.first())
        .and_then(|alarm| alarm["AlarmArn"].as_str())
        .unwrap_or_else(|| panic!("alarm {name:?}: expected an AlarmArn, got {described}"))
        .to_string()
}

/// A .zip deployment package holding the handler above as `lambda_function.py`, written in
/// memory the way a caller uploads one and base64-encoded as `FunctionCode$ZipFile` carries it.
fn package() -> String {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(HANDLER_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
}

/// Create the function an alarm action names, grant this service the invocation on it the way a
/// client does, and answer the ARN Lambda assigned it.
async fn given_function(emulator: &Emulator, name: &str) -> String {
    let (status, created) = lambda(
        emulator,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": 20,
            "Code": {"ZipFile": package()},
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "function {name:?}: expected HTTP 201 from Lambda CreateFunction, got {status} ({created})"
    );
    let arn = created["FunctionArn"]
        .as_str()
        .unwrap_or_else(|| panic!("function {name:?}: expected a FunctionArn, got {created}"))
        .to_string();
    let (status, answered) = lambda(
        emulator,
        "POST",
        &format!("/2015-03-31/functions/{name}/policy"),
        Some(json!({
            "StatementId": "cloudwatch-alarm-action",
            "Action": INVOKE_ACTION,
            "Principal": ALARM_ACTION_PRINCIPAL,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "function {name:?}: expected HTTP 201 from Lambda AddPermission, got {status} ({answered})"
    );
    arn
}

/// Past the span a created function spends in `Pending` before it is `Active`: waited out by
/// LOOKING at the function's own `State` until that service reports it ready, so nothing here
/// depends on how long the host took to get there.
///
/// "During this time, you can't invoke or modify the function" (`CreateFunction`).
async fn until_active(emulator: &Emulator, function: &str) {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let (_, configuration) = lambda(
            emulator,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        if configuration["State"] == json!("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "function {function:?}: never became Active, last read {configuration}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Every document the function `function` was invoked with, as the handler printed it into the
/// function's own log group — "By default, Lambda functions send logs to a default log group
/// named `/aws/lambda/<function name>`" (`LoggingConfig$LogGroup`).
async fn delivered(emulator: &Emulator, function: &str) -> Vec<Value> {
    let (status, body) = logs(
        emulator,
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
                .filter_map(|event| event["message"].as_str())
                .filter_map(|line| line.split_once(MARKER))
                .filter_map(|(_, document)| serde_json::from_str::<Value>(document.trim()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Look until the function has been handed a document whose transition carries `reason`, and
/// answer it: a wait for the CONDITION rather than for a span, so a row asserts on a delivery
/// and never on the clock.
async fn until_delivered(emulator: &Emulator, case: &str, function: &str, reason: &str) -> Value {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let documents = delivered(emulator, function).await;
        let found = documents.iter().find(|document| {
            document.pointer("/alarmData/state/reason") == Some(&Value::from(reason))
        });
        if let Some(found) = found {
            return found.clone();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function {function:?} was never handed a document explaining the \
             transition with {reason:?}, got {documents:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// The `Action` history items the alarm `name` holds, as `DescribeAlarmHistory` reports them.
async fn action_items(emulator: &Emulator, name: &str) -> Vec<Value> {
    let body = monitoring(
        emulator,
        "DescribeAlarmHistory",
        &json!({
            "AlarmName": name,
            "AlarmTypes": ["MetricAlarm"],
            "HistoryItemType": "Action",
        }),
    )
    .await;
    body["AlarmHistoryItems"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Look until the alarm `name` holds an `Action` item whose recorded document carries `reason`,
/// and answer that item's `HistorySummary` beside its parsed `HistoryData`.
async fn until_recorded(
    emulator: &Emulator,
    case: &str,
    name: &str,
    reason: &str,
) -> (String, Value) {
    let deadline = Instant::now() + WORK_WAIT;
    loop {
        let items = action_items(emulator, name).await;
        let found = items.iter().find_map(|item| {
            let data: Value = serde_json::from_str(item["HistoryData"].as_str().unwrap_or("null"))
                .unwrap_or(Value::Null);
            let published: Value =
                serde_json::from_str(data["publishedMessage"].as_str().unwrap_or("null"))
                    .unwrap_or(Value::Null);
            (published.pointer("/alarmData/state/reason") == Some(&Value::from(reason)))
                .then(|| (item["HistorySummary"].as_str().unwrap_or_default().to_string(), data))
        });
        if let Some(found) = found {
            return found;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the alarm {name:?} never recorded an Action item for the transition \
             explaining itself with {reason:?}, got {items:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// One alarm of the pair, with the function its action lists name.
struct Wiring {
    /// The alarm's name.
    alarm: String,
    /// The ARN CloudWatch assigned it, which the delivered document has to name.
    alarm_arn: String,
    /// The function's name, whose log group holds what it was handed.
    function: String,
    /// That function's ARN, which is the action the alarm's history has to name.
    function_arn: String,
}

/// One row: the transition forced on one of the two alarms, and the state it is expected to
/// have moved out of.
struct Row {
    case: &'static str,
    /// Which alarm this row moves: its index into the pair created below.
    alarm: usize,
    /// The `StateValue` the row forces, whose action list names that alarm's function.
    state: &'static str,
    /// The state the row expects the alarm to have moved OUT of — what the row before it on the
    /// same alarm moved it into, and `INSUFFICIENT_DATA` for the first row of each alarm, which
    /// is the state a newly written alarm reports.
    previous: &'static str,
    /// The `StateReason` the force carries, which is the explanation the delivered transition
    /// has to echo. Unique per row, so one row's delivery is never read for another's.
    reason: &'static str,
}

/// The rows, in the order they are driven: each alarm's rows form a chain, so the state one row
/// moves out of is the state the row before it moved it into.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the first alarm into ALARM, out of the state a new alarm reports",
            alarm: 0,
            state: "ALARM",
            previous: "INSUFFICIENT_DATA",
            reason: "forced into ALARM by the first row",
        },
        Row {
            case: "the same alarm back into OK, out of ALARM",
            alarm: 0,
            state: "OK",
            previous: "ALARM",
            reason: "the backlog drained to 0 units",
        },
        Row {
            case: "the same alarm into INSUFFICIENT_DATA, out of OK",
            alarm: 0,
            state: "INSUFFICIENT_DATA",
            previous: "OK",
            reason: "the series stopped reporting",
        },
        Row {
            case: "the second alarm into ALARM, with a long punctuated explanation",
            alarm: 1,
            state: "ALARM",
            previous: "INSUFFICIENT_DATA",
            reason: "Threshold Crossed: 1 out of the last 1 datapoints [7.5] was greater than \
                     the threshold (1.0) (minimum 1 datapoint for OK -> ALARM transition).",
        },
        Row {
            case: "the second alarm back into OK, out of ALARM",
            alarm: 1,
            state: "OK",
            previous: "ALARM",
            reason: "the second alarm's series came back under its threshold",
        },
    ]
}

/// RULE: a transition executes the action list of the state the alarm moved to, and a Lambda
/// function named there is handed one document reporting that transition — the publishing
/// service, the firing alarm's ARN, and the alarm's name with the state it moved to, its
/// explanation and the state it moved out of — while the same execution appends the `Action`
/// item the alarm's history reports it by, carrying that very document.
#[tokio::test]
async fn a_fired_lambda_action_hands_the_function_the_transition_and_records_it() {
    let emulator = emulator_with_lambda();
    // Two alarms, each naming its OWN function: an identity reported for the wrong alarm, or a
    // document delivered to the wrong function, fails a row. The alarm's ARN is the one
    // CloudWatch itself assigned, read back with the alarm.
    let mut wired: Vec<Wiring> = Vec::new();
    for index in 0..2 {
        let function = format!("alarm-payload-fn-{index}");
        let alarm = format!("alarm-payload-{index}");
        let function_arn = given_function(&emulator, &function).await;
        let alarm_arn = given_alarm(&emulator, &alarm, &function_arn).await;
        until_active(&emulator, &function).await;
        wired.push(Wiring {
            alarm,
            alarm_arn,
            function,
            function_arn,
        });
    }

    for row in rows() {
        let case = row.case;
        let Wiring {
            alarm,
            alarm_arn,
            function,
            function_arn,
        } = &wired[row.alarm];

        monitoring(
            &emulator,
            "SetAlarmState",
            &json!({
                "AlarmName": alarm,
                "StateValue": row.state,
                "StateReason": row.reason,
            }),
        )
        .await;

        // What the RUNNING function was handed.
        let payload = until_delivered(&emulator, case, function, row.reason).await;
        for (member, pointer, wanted) in [
            ("the publishing service", "/source", json!(EVENT_SOURCE)),
            ("the firing alarm's ARN", "/alarmArn", json!(alarm_arn)),
            ("the alarm's name", "/alarmData/alarmName", json!(alarm)),
            (
                "the state it moved to",
                "/alarmData/state/value",
                json!(row.state),
            ),
            (
                "the explanation the transition carries",
                "/alarmData/state/reason",
                json!(row.reason),
            ),
            (
                "the state it moved out of",
                "/alarmData/previousState/value",
                json!(row.previous),
            ),
        ] {
            let found = payload.pointer(pointer).cloned().unwrap_or(Value::Null);
            assert_eq!(
                found, wanted,
                "case {case:?}: expected {member} to be {wanted} at {pointer} of the document \
                 the function was handed, got {found} in {payload}"
            );
        }

        // And the alarm's own account of the same execution.
        let (summary, data) = until_recorded(&emulator, case, alarm, row.reason).await;
        assert_eq!(
            data["actionState"],
            json!("Succeeded"),
            "case {case:?}: expected the recorded action to have succeeded, got {data}"
        );
        assert_eq!(
            summary,
            format!("Successfully executed action {function_arn}"),
            "case {case:?}: expected the summary to name the action executed, got {summary:?} \
             with {data}"
        );
        let recorded: Value =
            serde_json::from_str(data["publishedMessage"].as_str().unwrap_or("null"))
                .unwrap_or(Value::Null);
        assert_eq!(
            recorded, payload,
            "case {case:?}: expected the history to record the very document the function was \
             handed, got {recorded} against {payload}"
        );
    }

    // Every transition driven above is accounted for exactly once per alarm, so nothing was
    // executed twice and no alarm's history holds another alarm's item.
    for (index, wiring) in wired.iter().enumerate() {
        let (alarm, function_arn) = (&wiring.alarm, &wiring.function_arn);
        let expected = rows().iter().filter(|row| row.alarm == index).count();
        let items = action_items(&emulator, alarm).await;
        assert_eq!(
            items.len(),
            expected,
            "alarm {alarm:?}: expected one Action item per transition forced on it ({expected}), \
             got {items:?}"
        );
        for item in &items {
            let summary = item["HistorySummary"].as_str().unwrap_or_default();
            assert_eq!(
                summary,
                format!("Successfully executed action {function_arn}"),
                "alarm {alarm:?}: expected every Action item to name its own function, got \
                 {summary:?}"
            );
        }
    }
}
