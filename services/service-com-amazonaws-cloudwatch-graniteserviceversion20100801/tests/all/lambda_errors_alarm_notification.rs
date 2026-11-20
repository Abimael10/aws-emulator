// SPDX-License-Identifier: CC0-1.0

//! An alarm over the `AWS/Lambda` `Errors` series a FUNCTION's own invocations publish, end to
//! end: CloudWatch, AWS Lambda, CloudWatch Logs, SNS and SQS on one engine, so what a client sees
//! is what is asserted — the state `DescribeAlarms` reports and the notification waiting on the
//! queue subscribed to the alarm's topic.
//!
//! The RULE asserted here: a metric alarm grades a series ANOTHER service published about its own
//! work exactly as it grades one a client published, and Lambda reports the outcome of every
//! invocation it performs on the `Errors` series — 1 for a run that came to a function error and 0
//! for one that returned. So, with the DOCUMENTED default missing-data treatment ("If
//! `TreatMissingData` is omitted, the default behavior of `missing` is used",
//! `PutMetricAlarmInput$TreatMissingData`):
//!
//! * a function whose invocations cross the alarm's threshold takes it into `ALARM`, which
//!   executes its `AlarmActions` and delivers the notification to the topic's subscribers;
//! * a function whose invocations stay under the threshold — a healthy one included, whose window
//!   reads an explicit zero — takes it into `OK` and delivers nothing;
//! * a function nobody invoked publishes no datapoint at all, so its alarm has nothing to grade
//!   and stays `INSUFFICIENT_DATA` — which is what tells "this function failed nothing" from
//!   "this function never ran".
//!
//! The rows differ along what the rule generalises over: how many invocations of the function
//! error, how many return, and the threshold their count is compared against — so the same wiring
//! breaches for one row and stays quiet for another, and every row's expected state is computed
//! from the row rather than taken from a recording.
//!
//! Every wait here is for a CONDITION — the state an evaluation owes, the notification a
//! transition owes — looked at again until it holds or a generous ceiling passes, so how fast the
//! host runs decides nothing asserted here.

use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::harness::{
    given_topic_with_subscriber, notifications, rpc, states_over_the_watch, until_notified,
    until_state, Emulator, LOOK_INTERVAL, REGION, TARGET_PREFIX, WORK_WAIT,
};

/// The namespace Lambda publishes its own series into, the series it reports an invocation's
/// outcome on and the dimension it reports that series per — the names of the AWS Lambda metric
/// reference, written as literals so a series published under a name no AWS reader asks for fails
/// here instead of agreeing with itself.
pub const NAMESPACE: &str = "AWS/Lambda";
pub const ERRORS_METRIC: &str = "Errors";
pub const FUNCTION_DIMENSION: &str = "FunctionName";

/// The three states an alarm reports (`StateValue`).
const ALARM: &str = "ALARM";
const OK: &str = "OK";
const INSUFFICIENT_DATA: &str = "INSUFFICIENT_DATA";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The role the created functions name. No IAM store is mounted here, so it is a value the create
/// carries rather than a record this suite relies on.
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-exec";

/// The handler every function here runs: it fails the way the event asks, or returns a value.
const SOURCE: &str = r#"
def lambda_handler(event, context):
    if event.get("raises"):
        raise RuntimeError("this invocation failed on purpose")
    return {"ok": True}
"#;

/// The alarm's period, and how many of them it grades: the shortest standard-resolution period
/// CloudWatch takes, with the one-period look-back a client writes for "any failure now".
const PERIOD: i64 = 60;
const EVALUATION_PERIODS: i64 = 1;

/// CloudWatch, AWS Lambda, CloudWatch Logs, SNS and SQS over ONE shared engine, as
/// `core/router/src/lib.rs` mounts them: the series Lambda publishes about its invocations is the
/// one CloudWatch grades, and the notification an alarm action publishes reaches the queue
/// subscribed to its topic.
pub fn emulator_with_lambda_and_notifications() -> Emulator {
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
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    Emulator { router, engine }
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes the
/// request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTEST/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One AWS Lambda call over its `restJson1` wire, answering its status, its headers and its
/// parsed body.
async fn lambda(
    emulator: &Emulator,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, ::http::HeaderMap, Value) {
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
    let (parts, answered) = response.into_parts();
    let parsed: Value = serde_json::from_slice(&answered).unwrap_or(Value::Null);
    (status, parts.headers, parsed)
}

/// One CloudWatch call that must answer 200.
async fn monitoring(emulator: &Emulator, operation: &str, body: &Value) -> Value {
    rpc(emulator, "monitoring", TARGET_PREFIX, operation, body).await
}

/// A .zip deployment package holding [`SOURCE`] as `lambda_function.py`, written in memory the
/// way a caller uploads one and base64-encoded as `FunctionCode$ZipFile` carries it.
fn package() -> String {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    base64::engine::general_purpose::STANDARD.encode(buffer.into_inner())
}

/// Create the function `name`, and wait until Lambda reports it `Active` — the state an
/// invocation needs. The wait is for that service's own report rather than for a span.
pub async fn given_function(emulator: &Emulator, case: &str, name: &str) {
    let (status, _, created) = lambda(
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
        "case {case:?}: expected HTTP 201 from Lambda CreateFunction for {name:?}, got {status} \
         ({created})"
    );
    let deadline = Instant::now() + WORK_WAIT;
    let mut last = Value::Null;
    while Instant::now() < deadline {
        let (_, _, configuration) = lambda(
            emulator,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if configuration["State"] == json!("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
    panic!("case {case:?}: Lambda never reported {name:?} Active, last read {last}");
}

/// Invoke `name` once, synchronously, with a handler that raises when `raises`.
pub async fn invoke(emulator: &Emulator, case: &str, name: &str, raises: bool) {
    let (status, headers, answered) = lambda(
        emulator,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        Some(json!({ "raises": raises })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Lambda Invoke of {name:?}, got {status} \
         ({answered})"
    );
    let reported = headers.get("x-amz-function-error").is_some();
    assert_eq!(
        reported, raises,
        "case {case:?}: the invocation of {name:?} must report a function error only when its \
         handler raised, got {answered}"
    );
}

/// A metric alarm over the `Errors` series of the function `function`, comparing the failures of
/// a period against `threshold`, with `topic` as its one alarm action and the DOCUMENTED default
/// missing-data treatment — the member is omitted, so what a hole in the series means is the
/// service's own default rather than this suite's choice.
async fn given_alarm(
    emulator: &Emulator,
    name: &str,
    function: &str,
    threshold: f64,
    topic: &str,
) {
    monitoring(
        emulator,
        "PutMetricAlarm",
        &json!({
            "AlarmName": name,
            "Namespace": NAMESPACE,
            "MetricName": ERRORS_METRIC,
            "Dimensions": [{"Name": FUNCTION_DIMENSION, "Value": function}],
            "Statistic": "Sum",
            "Period": PERIOD,
            "EvaluationPeriods": EVALUATION_PERIODS,
            "Threshold": threshold,
            "ComparisonOperator": "GreaterThanThreshold",
            "AlarmActions": [topic],
        }),
    )
    .await;
}

/// One row: how many invocations of the function fail, how many return, the threshold their
/// failures are compared against, and therefore whether the alarm breaches.
struct Row {
    case: &'static str,
    failing: usize,
    returning: usize,
    threshold: f64,
    breaches: bool,
}

/// The rows: a single failure over a threshold of none, several failures over a higher threshold,
/// a function whose every invocation returned (the healthy twin, whose window reads an explicit
/// zero), and failures that stay under their threshold.
const ROWS: &[Row] = &[
    Row {
        case: "one invocation that fails, over a threshold of none",
        failing: 1,
        returning: 0,
        threshold: 0.0,
        breaches: true,
    },
    Row {
        case: "three invocations that fail beside one that returns, over a threshold of two",
        failing: 3,
        returning: 1,
        threshold: 2.0,
        breaches: true,
    },
    Row {
        case: "a function whose every invocation returns, over a threshold of none",
        failing: 0,
        returning: 2,
        threshold: 0.0,
        breaches: false,
    },
    Row {
        case: "two invocations that fail, under a threshold of five",
        failing: 2,
        returning: 0,
        threshold: 5.0,
        breaches: false,
    },
];

/// RULE: an alarm over the series Lambda publishes about a function's invocations transitions on
/// what those invocations came to — into ALARM when their failures cross its threshold, which
/// delivers its notification to the subscribers of the topic its action names, and into OK when
/// they do not, which delivers nothing.
#[tokio::test]
async fn an_alarm_over_a_functions_error_series_transitions_on_its_invocations() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let emulator = emulator_with_lambda_and_notifications();
        let name = format!("lambda-errors-{index}");
        let (topic, queue_url) = given_topic_with_subscriber(&emulator, &name).await;

        let function = format!("errors-fn-{index}");
        given_function(&emulator, case, &function).await;
        let alarm = format!("errors-alarm-{index}");
        given_alarm(&emulator, &alarm, &function, row.threshold, &topic).await;

        for _ in 0..row.failing {
            invoke(&emulator, case, &function, true).await;
        }
        for _ in 0..row.returning {
            invoke(&emulator, case, &function, false).await;
        }

        if row.breaches {
            let state = until_state(&emulator, &alarm, ALARM).await;
            assert_eq!(
                state, ALARM,
                "case {case:?}: {} failed invocation(s) over a threshold of {} must take the \
                 alarm to {ALARM}, it reports {state:?}",
                row.failing, row.threshold
            );
            let delivered = until_notified(&emulator, &queue_url).await;
            assert_eq!(
                delivered.len(),
                1,
                "case {case:?}: the transition must deliver exactly one notification to the \
                 subscribed queue, got {delivered:?}"
            );
            assert_eq!(
                delivered[0]["AlarmName"].as_str(),
                Some(alarm.as_str()),
                "case {case:?}: the notification must name the alarm, got {}",
                delivered[0]
            );
            assert_eq!(
                delivered[0]["NewStateValue"].as_str(),
                Some(ALARM),
                "case {case:?}: the notification must carry the state moved to, got {}",
                delivered[0]
            );
            assert_eq!(
                delivered[0]["Trigger"]["MetricName"].as_str(),
                Some(ERRORS_METRIC),
                "case {case:?}: the notification must carry the series it fired on, got {}",
                delivered[0]
            );
            continue;
        }

        // The quiet rows: the invocations WERE graded — so the alarm leaves INSUFFICIENT_DATA for
        // OK, which a function with no datapoint at all could never do — and nothing is
        // delivered, the alarm carrying no action for the state it moved into.
        let state = until_state(&emulator, &alarm, OK).await;
        assert_eq!(
            state, OK,
            "case {case:?}: {} failed invocation(s) under a threshold of {} must take the alarm \
             to {OK}, it reports {state:?}",
            row.failing, row.threshold
        );
        let held = states_over_the_watch(&emulator, &alarm).await;
        assert!(
            held.iter().all(|state| state == OK),
            "case {case:?}: the alarm must HOLD {OK} over the watch, it reported {held:?}"
        );
        let delivered = notifications(&emulator, &queue_url).await;
        assert!(
            delivered.is_empty(),
            "case {case:?}: an alarm that never breached must deliver nothing, got {delivered:?}"
        );
    }
}

/// RULE (the contrast that gives the healthy row its meaning): a function nobody invoked
/// publishes nothing on the series, so its alarm has nothing to grade and holds
/// INSUFFICIENT_DATA under the default missing-data treatment — and one invocation that returns
/// is what takes the same alarm to OK.
#[tokio::test]
async fn an_alarm_over_a_function_that_was_never_invoked_has_nothing_to_grade() {
    let case = "a function created and never invoked";
    let emulator = emulator_with_lambda_and_notifications();
    let (topic, queue_url) = given_topic_with_subscriber(&emulator, "lambda-errors-idle").await;

    let function = "errors-fn-idle";
    given_function(&emulator, case, function).await;
    let alarm = "errors-alarm-idle";
    given_alarm(&emulator, alarm, function, 0.0, &topic).await;

    let held = states_over_the_watch(&emulator, alarm).await;
    assert!(
        held.iter().all(|state| state == INSUFFICIENT_DATA),
        "case {case:?}: an alarm over a function nobody invoked must hold {INSUFFICIENT_DATA}, \
         it reported {held:?}"
    );

    invoke(&emulator, case, function, false).await;
    let state = until_state(&emulator, alarm, OK).await;
    assert_eq!(
        state, OK,
        "case {case:?}: the one invocation that returned must take the alarm to {OK}, it reports \
         {state:?}"
    );
    let delivered = notifications(&emulator, &queue_url).await;
    assert!(
        delivered.is_empty(),
        "case {case:?}: an alarm that never breached must deliver nothing, got {delivered:?}"
    );
}
