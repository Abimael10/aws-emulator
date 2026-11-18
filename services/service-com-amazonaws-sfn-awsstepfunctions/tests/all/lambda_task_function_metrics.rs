// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the `AWS/States` series a `Task` state publishes about the work
//! it took on a LAMBDA FUNCTION, driven over each service's own wire protocol through
//! the production `MultiServiceRouter` with Step Functions, Lambda and CloudWatch
//! mounted on ONE engine — the shape `core/router/src/lib.rs` mounts them in — so the
//! function really runs and the datapoints land in the store CloudWatch's own readers
//! answer from.
//!
//! RULE: a `Task` state whose `Resource` is the ARN of a Lambda function counts each
//! invocation it takes on that function in the per-function `AWS/States` series —
//! `LambdaFunctionsScheduled` when it hands the work over, `LambdaFunctionsStarted` when
//! it begins, and one of `LambdaFunctionsSucceeded`, `LambdaFunctionsFailed` or
//! `LambdaFunctionsTimedOut` when it closes, beside the spans
//! `LambdaFunctionScheduleTime`, `LambdaFunctionRunTime` and `LambdaFunctionTime` — all
//! dimensioned on that function's ARN alone. A `Resource` that names one of this
//! service's integration tokens names no function, so it is counted in none of them, and
//! the per-function and the per-state-machine dimension never cross.
//!
//! The rows differ along what the rule generalises over: how the invocation closes (the
//! function returning, the function erroring, the state's own `TimeoutSeconds` expiring),
//! how many invocations one execution takes (one, one per `Retry` attempt, one per `Map`
//! iteration — which is also a task state nested inside another state's processor), and
//! the form of the `Resource` (a function ARN against the optimized `lambda:invoke`
//! integration on the very same function).
//!
//! The second suite below reads those same families where a client reads them — through
//! CloudWatch's own `GetMetricStatistics` — over a window in which the invocations of ONE
//! function closed DIFFERENTLY, so what is asserted is that summing a count series gives
//! the number of invocations that closed that way and summing the start series gives the
//! number taken, however the window's invocations were split between outcomes. Its rows
//! differ along how many invocations the window holds and how they are split.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;
use service_com_amazonaws_sfn_awsstepfunctions::state::observability;
use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::lambda_task_control_flow::{package, parsed};
use crate::state_machine_lifecycle::{string, PAST_THE_EXECUTION_DELAY, REGION, ROLE_ARN};

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `State` a function reports once it can be invoked.
const FUNCTION_ACTIVE: &str = "Active";

/// The runtime the handler below is written for.
const RUNTIME: &str = "python3.12";

/// The handler the package's one module exports.
const HANDLER: &str = "index.handler";

/// The execution role the function runs as. No IAM is mounted here, so nothing is known
/// about it.
const LAMBDA_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/LambdaExecutionRole";

/// How long one poll of a status the clock owes sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a row waits for a function to reach [`FUNCTION_ACTIVE`].
const PATIENCE: Duration = Duration::from_secs(20);

/// How long a row waits for an execution to leave `RUNNING`.
const TERMINAL_PATIENCE: Duration =
    PAST_THE_EXECUTION_DELAY.saturating_add(Duration::from_secs(40));

/// The one handler every row invokes: it returns what the event asks it to echo, raises
/// when the event asks it to fail, and sleeps when the event asks it to — so one
/// deployment package serves every row and what the invocation does is decided by the
/// data the definition carries.
const HANDLER_SOURCE: &str = r#"
import time


def handler(event, context):
    mode = event.get("mode", "ok")
    if mode == "boom":
        raise ValueError("the row fails on purpose")
    if mode == "nap":
        time.sleep(0.6)
    if mode == "slow":
        time.sleep(10)
    return {"echo": event.get("echo", 0)}
"#;

/// How long the handler sleeps in the `nap` mode, in milliseconds: the span an
/// invocation of it RUNS for, which is what the row that asks for it must read back out
/// of the run-time series.
const NAP_MILLIS: f64 = 600.0;

/// The `TimeoutSeconds` the row whose invocation overruns declares, in milliseconds: a
/// wait answers no sooner than the span it was given, so that is the floor on how long
/// such an invocation ran.
const OVERRUN_MILLIS: f64 = 2000.0;

/// Step Functions, Lambda and CloudWatch over ONE engine.
struct Metered {
    router: MultiServiceRouter,
    engine: Arc<engine::InMemoryEngine>,
}

impl Metered {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(
                &engine,
            )),
        );
        router.register_service(
            service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        Self { router, engine }
    }

    /// A SigV4 `Authorization` header whose credential scope names `service`, which is
    /// what routes the request. The signature is never checked: this router runs with
    /// request authentication off.
    fn authorization(service: &str) -> String {
        format!(
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
             SignedHeaders=host, Signature=0123456789abcdef"
        )
    }

    /// One Step Functions call, requiring a 200.
    async fn sfn(&self, case: &str, operation: &str, body: &Value) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("states.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("AWSStepFunctions.{operation}"))
            .header("authorization", Self::authorization("states"))
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = parsed(response.into_body());
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must answer 200, got {status} with {answered}"
        );
        answered
    }

    /// One `restJson1` call to Lambda, as its status and parsed body.
    async fn lambda(&self, method: &str, uri: &str, body: &Value) -> (u16, Value) {
        let payload = if body.is_null() {
            Vec::new()
        } else {
            body.to_string().into_bytes()
        };
        let request = http::Request::builder()
            .method(method)
            .uri(format!("http://lambda.{REGION}.amazonaws.com{uri}"))
            .header("host", format!("lambda.{REGION}.amazonaws.com"))
            .header("content-type", "application/json")
            .header("authorization", Self::authorization("lambda"))
            .body(Bytes::from(payload))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        (status, parsed(response.into_body()))
    }

    /// The row's function, waited out to [`FUNCTION_ACTIVE`] so an invocation of it is
    /// one Lambda admits; answers its ARN.
    async fn given_function(&self, name: &str) -> String {
        let (status, created) = self
            .lambda(
                "POST",
                FUNCTIONS_URI,
                &json!({
                    "FunctionName": name,
                    "Role": LAMBDA_ROLE_ARN,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    // Longer than the one row whose handler sleeps, so what ends that
                    // invocation is the STATE's bound rather than the function's own.
                    "Timeout": 60,
                    "Code": { "ZipFile": package(HANDLER_SOURCE) },
                }),
            )
            .await;
        assert_eq!(
            status, 201,
            "CreateFunction must answer 201, got {status} with {created}"
        );
        let arn = string(&created, "FunctionArn");
        let began = Instant::now();
        loop {
            let (status, configuration) = self
                .lambda(
                    "GET",
                    &format!("{FUNCTIONS_URI}/{name}/configuration"),
                    &Value::Null,
                )
                .await;
            assert_eq!(
                status, 200,
                "GetFunctionConfiguration must resolve the created function, got {status} with \
                 {configuration}"
            );
            if configuration.get("State").and_then(Value::as_str) == Some(FUNCTION_ACTIVE) {
                return arn;
            }
            assert!(
                began.elapsed() < PATIENCE,
                "the function must reach {FUNCTION_ACTIVE} within {PATIENCE:?}, last saw \
                 {configuration}"
            );
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Create `definition` as a state machine, run one execution of it over `input`, and
    /// answer `(the state machine's ARN, the execution once it has left RUNNING)`.
    ///
    /// The create and the run are the two helpers a row driving SEVERAL executions of one
    /// state machine uses on their own ([`Self::machine_with`], [`Self::ran_named`]), so
    /// the terminal-status wait exists once.
    async fn ran(
        &self,
        case: &str,
        name: &str,
        definition: &Value,
        input: &Value,
    ) -> (String, Value) {
        let machine = self.machine_with(case, name, definition).await;
        let described = self.ran_named(case, &machine, "only", input).await;
        (machine, described)
    }

    /// The datapoints the series `metric` holds for the dimension set `dimensions`, read
    /// out of the CloudWatch store through the shared engine — the store CloudWatch's own
    /// readers answer from.
    fn series(
        &self,
        metric: &str,
        dimensions: std::collections::BTreeMap<String, String>,
    ) -> Vec<cloudwatch::StoredDatum> {
        let store = cloudwatch::service_state(&*self.engine).expect("the CloudWatch store");
        let store = store.lock();
        store
            .metrics
            .get(&cloudwatch::MetricKey {
                namespace: observability::METRIC_NAMESPACE.to_string(),
                metric_name: metric.to_string(),
                dimensions,
            })
            .cloned()
            .unwrap_or_default()
    }

    /// The datapoints the per-function series `metric` holds for `function_arn`.
    fn function_series(&self, metric: &str, function_arn: &str) -> Vec<cloudwatch::StoredDatum> {
        self.series(metric, observability::lambda_function_dimensions(function_arn))
    }
}

/// The sum of a series' datapoints, which is what a reader summing a window gets.
fn sum(datapoints: &[cloudwatch::StoredDatum]) -> f64 {
    datapoints.iter().map(|datum| datum.sum).sum()
}

/// The count series of the family, so a row can pin the ones it must NOT be counted in
/// as well as the one it must.
const COUNT_SERIES: &[&str] = &[
    observability::LAMBDA_FUNCTIONS_SUCCEEDED_METRIC,
    observability::LAMBDA_FUNCTIONS_FAILED_METRIC,
    observability::LAMBDA_FUNCTIONS_TIMED_OUT_METRIC,
];

/// One row: the definition a `Task` state is written in, and what the invocations it
/// takes are counted as.
struct Row {
    case: &'static str,
    /// The definition, over the ARN of the function it names.
    definition: fn(&str) -> Value,
    /// The input the execution is started with.
    input: fn() -> Value,
    /// The terminal status the execution reaches.
    status: &'static str,
    /// How many invocations are scheduled and started.
    invocations: f64,
    /// The least each invocation RAN for, in milliseconds: the span the handler this
    /// row's input selects really takes.
    ran_for_millis: f64,
    /// The series each of those invocations CLOSES in, and `None` for a row whose work
    /// is never counted per function at all.
    closed_in: Option<&'static str>,
}

/// A `Task` state whose `Resource` is the function's ARN.
fn direct(function_arn: &str) -> Value {
    json!({
        "StartAt": "Invoke",
        "States": { "Invoke": { "Type": "Task", "Resource": function_arn, "End": true } },
    })
}

/// The same, with a `Retry` on the task-failure wildcard, so a failing invocation is
/// taken once per attempt the retrier allows.
fn retried(function_arn: &str) -> Value {
    json!({
        "StartAt": "Invoke",
        "States": {
            "Invoke": {
                "Type": "Task",
                "Resource": function_arn,
                "Retry": [{
                    "ErrorEquals": ["States.ALL"],
                    "IntervalSeconds": 1,
                    "MaxAttempts": 2,
                    "BackoffRate": 1.0,
                }],
                "End": true,
            },
        },
    })
}

/// The same task, with the state's own `TimeoutSeconds` shorter than the invocation
/// takes, so the invocation closes on that bound.
fn timed_out(function_arn: &str) -> Value {
    json!({
        "StartAt": "Invoke",
        "States": {
            "Invoke": {
                "Type": "Task",
                "Resource": function_arn,
                "TimeoutSeconds": 2,
                "End": true,
            },
        },
    })
}

/// The same task NESTED inside a `Map` state's processor, so one execution takes one
/// invocation per item.
fn mapped(function_arn: &str) -> Value {
    json!({
        "StartAt": "Fan",
        "States": {
            "Fan": {
                "Type": "Map",
                "ItemsPath": "$.items",
                "ItemProcessor": {
                    "StartAt": "InvokeItem",
                    "States": {
                        "InvokeItem": {
                            "Type": "Task",
                            "Resource": function_arn,
                            "End": true,
                        },
                    },
                },
                "End": true,
            },
        },
    })
}

/// The OPTIMIZED Lambda integration on the very same function: the `Resource` names one
/// of this service's integration tokens, and the function is named by the request the
/// state's `Parameters` build.
fn optimized(function_arn: &str) -> Value {
    json!({
        "StartAt": "Invoke",
        "States": {
            "Invoke": {
                "Type": "Task",
                "Resource": "arn:aws:states:::lambda:invoke",
                "Parameters": {
                    "FunctionName": function_arn,
                    "Payload": { "mode": "ok", "echo": 7 },
                },
                "End": true,
            },
        },
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a function ARN whose invocation returns",
            definition: direct,
            input: || json!({ "mode": "ok", "echo": 1 }),
            status: "SUCCEEDED",
            invocations: 1.0,
            ran_for_millis: 0.0,
            closed_in: Some(observability::LAMBDA_FUNCTIONS_SUCCEEDED_METRIC),
        },
        Row {
            case: "a function ARN whose invocation sleeps before it returns",
            definition: direct,
            input: || json!({ "mode": "nap", "echo": 2 }),
            status: "SUCCEEDED",
            invocations: 1.0,
            ran_for_millis: NAP_MILLIS,
            closed_in: Some(observability::LAMBDA_FUNCTIONS_SUCCEEDED_METRIC),
        },
        Row {
            case: "a function ARN whose invocation errors",
            definition: direct,
            input: || json!({ "mode": "boom" }),
            status: "FAILED",
            invocations: 1.0,
            ran_for_millis: 0.0,
            closed_in: Some(observability::LAMBDA_FUNCTIONS_FAILED_METRIC),
        },
        Row {
            case: "a function ARN retried twice, so three invocations",
            definition: retried,
            input: || json!({ "mode": "boom" }),
            status: "FAILED",
            invocations: 3.0,
            ran_for_millis: 0.0,
            closed_in: Some(observability::LAMBDA_FUNCTIONS_FAILED_METRIC),
        },
        Row {
            case: "a function ARN whose invocation outlives the state's TimeoutSeconds",
            definition: timed_out,
            input: || json!({ "mode": "slow" }),
            status: "FAILED",
            invocations: 1.0,
            ran_for_millis: OVERRUN_MILLIS,
            closed_in: Some(observability::LAMBDA_FUNCTIONS_TIMED_OUT_METRIC),
        },
        Row {
            case: "a function ARN inside a Map processor, so one invocation per item",
            definition: mapped,
            input: || {
                json!({ "items": [
                    { "mode": "ok", "echo": 1 },
                    { "mode": "ok", "echo": 2 },
                    { "mode": "ok", "echo": 3 },
                ] })
            },
            status: "SUCCEEDED",
            invocations: 3.0,
            ran_for_millis: 0.0,
            closed_in: Some(observability::LAMBDA_FUNCTIONS_SUCCEEDED_METRIC),
        },
        Row {
            case: "the optimized integration on the same function, which names no function",
            definition: optimized,
            input: || json!({}),
            status: "SUCCEEDED",
            invocations: 0.0,
            ran_for_millis: 0.0,
            closed_in: None,
        },
    ]
}

/// Every invocation a `Task` state takes on a Lambda function the `Resource` named is
/// counted in the per-function series, dimensioned on that function's ARN; a `Resource`
/// naming an integration is counted in none of them.
#[tokio::test]
async fn a_function_arn_task_is_counted_per_function() {
    let metered = Metered::new();

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            definition,
            input,
            status,
            invocations,
            ran_for_millis,
            closed_in,
        } = row;
        // One function and one state machine per row, so each row's series are its own.
        let function = metered.given_function(&format!("counted-{index}")).await;
        let (machine, execution) = metered
            .ran(
                case,
                &format!("counted-{index}"),
                &definition(&function),
                &input(),
            )
            .await;
        assert_eq!(
            execution.get("status").and_then(Value::as_str),
            Some(status),
            "case {case:?}: expected the execution to end {status}, got {execution}"
        );

        for (metric, expected) in [
            (
                observability::LAMBDA_FUNCTIONS_SCHEDULED_METRIC,
                invocations,
            ),
            (observability::LAMBDA_FUNCTIONS_STARTED_METRIC, invocations),
        ] {
            let held = metered.function_series(metric, &function);
            let counted = sum(&held);
            assert_eq!(
                counted, expected,
                "case {case:?}: expected {metric} to count {expected}, got {counted} from \
                 {held:?}"
            );
        }
        for metric in COUNT_SERIES {
            let held = metered.function_series(metric, &function);
            let counted = sum(&held);
            let expected = if closed_in == Some(metric) {
                invocations
            } else {
                0.0
            };
            assert_eq!(
                counted, expected,
                "case {case:?}: expected {metric} to count {expected}, got {counted} from \
                 {held:?}"
            );
        }
        // The spans: one datapoint per invocation that started, one per invocation that
        // closed, each a span of milliseconds — and the RUN span is the span the work
        // really took, so an invocation that slept is reported as having run at least
        // that long.
        for (metric, samples) in [
            (
                observability::LAMBDA_FUNCTION_SCHEDULE_TIME_METRIC,
                invocations,
            ),
            (observability::LAMBDA_FUNCTION_RUN_TIME_METRIC, invocations),
            (observability::LAMBDA_FUNCTION_TIME_METRIC, invocations),
        ] {
            let held = metered.function_series(metric, &function);
            #[allow(clippy::cast_precision_loss)]
            let counted = held.len() as f64;
            assert_eq!(
                counted, samples,
                "case {case:?}: expected {samples} {metric} samples, got {held:?}"
            );
            for datum in &held {
                assert_eq!(
                    datum.unit.as_deref(),
                    Some("Milliseconds"),
                    "case {case:?}: {metric} is a span of milliseconds, got {datum:?}"
                );
            }
        }
        for metric in [
            observability::LAMBDA_FUNCTION_RUN_TIME_METRIC,
            observability::LAMBDA_FUNCTION_TIME_METRIC,
        ] {
            for datum in &metered.function_series(metric, &function) {
                assert!(
                    datum.minimum >= ran_for_millis,
                    "case {case:?}: expected {metric} to be at least {ran_for_millis}ms, got \
                     {datum:?}"
                );
            }
        }
        // The two families never cross: no per-function series is written under the
        // state machine's dimension, and no execution series under the function's.
        for metric in COUNT_SERIES {
            let held = metered.series(metric, observability::state_machine_dimensions(&machine));
            assert!(
                held.is_empty(),
                "case {case:?}: {metric} belongs to the function's dimension alone, got {held:?}"
            );
        }
        for metric in [
            observability::EXECUTIONS_STARTED_METRIC,
            observability::EXECUTIONS_SUCCEEDED_METRIC,
            observability::EXECUTIONS_FAILED_METRIC,
        ] {
            let held = metered.function_series(metric, &function);
            assert!(
                held.is_empty(),
                "case {case:?}: {metric} belongs to the state machine's dimension alone, got \
                 {held:?}"
            );
        }
        // And the execution itself is still counted under its state machine, so the
        // absences above are about the DIMENSION rather than about publishing at all.
        let started = metered.series(
            observability::EXECUTIONS_STARTED_METRIC,
            observability::state_machine_dimensions(&machine),
        );
        assert_eq!(
            sum(&started),
            1.0,
            "case {case:?}: expected the one execution to be counted, got {started:?}"
        );
    }
}

// What follows reads the SAME families over the wire, through CloudWatch's own
// `GetMetricStatistics`, over a window holding invocations of one function that closed
// differently.

/// The namespace these series belong to, the names the AWS Step Functions metric
/// reference gives the per-function counts, the per-execution counts and the run span,
/// the two dimensions they are reported under, and the units they are published in.
/// Written as literals here rather than read from the crate under test, so a series
/// published under a name no AWS reader would ask for fails here.
const STATES_NAMESPACE: &str = "AWS/States";
/// See [`STATES_NAMESPACE`].
const LAMBDA_FUNCTION_ARN_DIMENSION: &str = "LambdaFunctionArn";
/// See [`STATES_NAMESPACE`].
const STATE_MACHINE_ARN_DIMENSION: &str = "StateMachineArn";
/// See [`STATES_NAMESPACE`].
const SCHEDULED_SERIES: &str = "LambdaFunctionsScheduled";
/// See [`STATES_NAMESPACE`].
const STARTED_SERIES: &str = "LambdaFunctionsStarted";
/// See [`STATES_NAMESPACE`].
const SUCCEEDED_SERIES: &str = "LambdaFunctionsSucceeded";
/// See [`STATES_NAMESPACE`].
const FAILED_SERIES: &str = "LambdaFunctionsFailed";
/// See [`STATES_NAMESPACE`].
const TIMED_OUT_SERIES: &str = "LambdaFunctionsTimedOut";
/// See [`STATES_NAMESPACE`].
const RUN_TIME_SERIES: &str = "LambdaFunctionRunTime";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_STARTED_SERIES: &str = "ExecutionsStarted";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_SUCCEEDED_SERIES: &str = "ExecutionsSucceeded";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_FAILED_SERIES: &str = "ExecutionsFailed";
/// See [`STATES_NAMESPACE`].
const COUNT_UNIT: &str = "Count";
/// See [`STATES_NAMESPACE`].
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The `X-Amz-Target` prefix of CloudWatch's `awsJson1_0` wire protocol, and the signing
/// name that routes a request to it.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
/// See [`CLOUDWATCH_TARGET_PREFIX`].
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The `Period`, in seconds, every read below uses: the one-minute granularity these
/// series are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of now a read's window reaches, in seconds — wide enough that
/// every minute a row published in is inside it however the row straddles a minute
/// boundary.
const WINDOW_SECS: f64 = 600.0;

/// The mode an invocation that must RETURN is asked for, and the one an invocation that
/// must ERROR is: the two the one [`HANDLER_SOURCE`] branches on.
const RETURNS: &str = "ok";
/// See [`RETURNS`].
const ERRORS: &str = "boom";

/// How long a row looks for the window to hold every invocation it took. Generous on
/// purpose: the wait is for that CONDITION rather than for a span, and nothing here is
/// asserted about how fast the host runs.
const READ_PATIENCE: Duration = Duration::from_secs(60);

impl Metered {
    /// One CloudWatch call, requiring a 200.
    async fn cloudwatch(&self, case: &str, operation: &str, body: &Value) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header(
                "host",
                format!("{CLOUDWATCH_SIGNING_NAME}.{REGION}.amazonaws.com"),
            )
            .header("content-type", "application/x-amz-json-1.0")
            .header(
                "x-amz-target",
                format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}"),
            )
            .header(
                "authorization",
                Self::authorization(CLOUDWATCH_SIGNING_NAME),
            )
            .body(Bytes::from(body.to_string()))
            .expect("a well-formed request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let answered = parsed(response.into_body());
        assert_eq!(
            status, 200,
            "case {case:?}: {operation} must answer 200, got {status} with {answered}"
        );
        answered
    }

    /// The datapoints the series `metric` of [`STATES_NAMESPACE`] reports over the window
    /// for the dimension set `dimensions`, each carrying `statistics`. A series with no
    /// datapoint at all answers an empty list, which is what "the series stayed empty"
    /// reads as.
    async fn read(
        &self,
        case: &str,
        metric: &str,
        dimensions: &Value,
        statistics: &[&str],
    ) -> Vec<Value> {
        let now = now_secs();
        let answered = self
            .cloudwatch(
                case,
                "GetMetricStatistics",
                &json!({
                    "Namespace": STATES_NAMESPACE,
                    "MetricName": metric,
                    "Dimensions": dimensions,
                    "StartTime": now - WINDOW_SECS,
                    "EndTime": now + WINDOW_SECS,
                    "Period": PERIOD_SECS,
                    "Statistics": statistics,
                }),
            )
            .await;
        answered["Datapoints"]
            .as_array()
            .cloned()
            .unwrap_or_default()
    }

    /// The SUM the series reports over the window — the magnitude a client charting or
    /// alarming on it reads. A series with no datapoint sums to zero.
    async fn summed(&self, case: &str, metric: &str, dimensions: &Value) -> f64 {
        self.read(case, metric, dimensions, &["Sum"])
            .await
            .iter()
            .filter_map(|point| point["Sum"].as_f64())
            .sum()
    }

    /// How many observations the series holds over the window, summed across its
    /// datapoints: one per invocation for a span series.
    async fn sampled(&self, case: &str, metric: &str, dimensions: &Value) -> f64 {
        self.read(case, metric, dimensions, &["SampleCount"])
            .await
            .iter()
            .filter_map(|point| point["SampleCount"].as_f64())
            .sum()
    }

    /// Every reading the split below is asserted against, as `(what it is, the magnitude
    /// read)`: each one holds the number of invocations the row took once every
    /// observation of its executions has been handed over, whatever each invocation's
    /// outcome was — so a row waits for all of them rather than for the earliest.
    async fn settled(
        &self,
        case: &str,
        function_arn: &str,
        machine_arn: &str,
    ) -> Vec<(&'static str, f64)> {
        let function = function_arn_dimensions(function_arn);
        let machine = machine_arn_dimensions(machine_arn);
        let mut closed = 0.0;
        for metric in [SUCCEEDED_SERIES, FAILED_SERIES, TIMED_OUT_SERIES] {
            closed += self.summed(case, metric, &function).await;
        }
        let mut executions_closed = 0.0;
        for metric in [EXECUTIONS_SUCCEEDED_SERIES, EXECUTIONS_FAILED_SERIES] {
            executions_closed += self.summed(case, metric, &machine).await;
        }
        vec![
            (
                SCHEDULED_SERIES,
                self.summed(case, SCHEDULED_SERIES, &function).await,
            ),
            (
                STARTED_SERIES,
                self.summed(case, STARTED_SERIES, &function).await,
            ),
            ("the invocation outcome series", closed),
            (
                RUN_TIME_SERIES,
                self.sampled(case, RUN_TIME_SERIES, &function).await,
            ),
            (
                EXECUTIONS_STARTED_SERIES,
                self.summed(case, EXECUTIONS_STARTED_SERIES, &machine).await,
            ),
            ("the execution outcome series", executions_closed),
        ]
    }

    /// Create `definition` as a state machine; answers its ARN.
    async fn machine_with(&self, case: &str, name: &str, definition: &Value) -> String {
        let created = self
            .sfn(
                case,
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": definition.to_string(),
                    "roleArn": ROLE_ARN,
                }),
            )
            .await;
        string(&created, "stateMachineArn")
    }

    /// Start one execution of `machine` named `name` with `input`, and report it once it
    /// has left `RUNNING`.
    async fn ran_named(&self, case: &str, machine: &str, name: &str, input: &Value) -> Value {
        let started = self
            .sfn(
                case,
                "StartExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": name,
                    "input": input.to_string(),
                }),
            )
            .await;
        let execution = string(&started, "executionArn");
        let began = Instant::now();
        loop {
            let described = self
                .sfn(
                    case,
                    "DescribeExecution",
                    &json!({ "executionArn": execution }),
                )
                .await;
            if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
                return described;
            }
            assert!(
                began.elapsed() < TERMINAL_PATIENCE,
                "case {case:?}: expected a terminal status within {TERMINAL_PATIENCE:?}, last \
                 saw {described}"
            );
            std::thread::sleep(POLL_INTERVAL);
        }
    }
}

/// Now, in whole epoch seconds, which is how an `awsJson` request carries an instant.
fn now_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("now is after the epoch")
        .as_secs_f64()
        .trunc()
}

/// The dimension set of a series reported per invoked FUNCTION, as a request carries it.
fn function_arn_dimensions(function_arn: &str) -> Value {
    json!([{ "Name": LAMBDA_FUNCTION_ARN_DIMENSION, "Value": function_arn }])
}

/// The dimension set of a series reported per STATE MACHINE.
fn machine_arn_dimensions(machine_arn: &str) -> Value {
    json!([{ "Name": STATE_MACHINE_ARN_DIMENSION, "Value": machine_arn }])
}

/// Whether two magnitudes read off a series are the same count.
fn same(read: f64, expected: f64) -> bool {
    (read - expected).abs() < f64::EPSILON
}

/// One row of the read below: the modes the row's executions are started with, in order —
/// one invocation each, closing as the mode asks.
struct Split {
    case: &'static str,
    modes: &'static [&'static str],
}

fn splits() -> Vec<Split> {
    vec![
        Split {
            case: "one invocation returns and one errors",
            modes: &[RETURNS, ERRORS],
        },
        Split {
            case: "one invocation errors and one returns, the other way round",
            modes: &[ERRORS, RETURNS],
        },
        Split {
            case: "two of three invocations return and one errors",
            modes: &[RETURNS, ERRORS, RETURNS],
        },
        Split {
            case: "both invocations error",
            modes: &[ERRORS, ERRORS],
        },
        Split {
            case: "the one invocation returns",
            modes: &[RETURNS],
        },
    ]
}

/// Read through CloudWatch's own `GetMetricStatistics`, each count series of the
/// per-function family reports the invocations of that function that closed ITS way and
/// the handover and start series report every invocation taken, however a window's
/// invocations were split between outcomes — while the execution counters report the same
/// split under the state machine's own dimension, and neither family exists under the
/// other's.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_windows_invocations_are_counted_in_the_series_of_each_outcome() {
    let metered = Metered::new();

    for (index, split) in splits().into_iter().enumerate() {
        let Split { case, modes } = split;
        // One function and one state machine per row, so each row's series are its own.
        let name = format!("split-{index}");
        let function = metered.given_function(&name).await;
        let machine = metered
            .machine_with(case, &name, &direct(&function))
            .await;

        for (run, mode) in modes.iter().enumerate() {
            let described = metered
                .ran_named(
                    case,
                    &machine,
                    &format!("run-{run}"),
                    &json!({ "mode": mode, "echo": run + 1 }),
                )
                .await;
            let expected = if *mode == ERRORS {
                "FAILED"
            } else {
                "SUCCEEDED"
            };
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some(expected),
                "case {case:?}: expected run {run} in mode {mode:?} to end {expected}, got \
                 {described}"
            );
        }

        #[allow(clippy::cast_precision_loss)]
        let taken = modes.len() as f64;
        #[allow(clippy::cast_precision_loss)]
        let returned = modes.iter().filter(|mode| **mode == RETURNS).count() as f64;
        let errored = taken - returned;

        // An observation is readable once the work that closed it has been handed over, so
        // the row waits for every reading the assertions below depend on to hold the
        // invocations it took — the handover, the start, the outcome and the span series of
        // the function, and the execution counters of the state machine — before reading
        // the split. The wait is for that CONDITION, not for a span.
        let began = Instant::now();
        loop {
            let settled = metered.settled(case, &function, &machine).await;
            if settled.iter().all(|(_, read)| same(*read, taken)) {
                break;
            }
            assert!(
                began.elapsed() < READ_PATIENCE,
                "case {case:?}: expected every series of {function} and {machine} to hold the \
                 {taken} invocation(s) within {READ_PATIENCE:?}, last read {settled:?}"
            );
            std::thread::sleep(POLL_INTERVAL);
        }

        // The split itself: every invocation in the handover and start series, and each
        // one in the series of the outcome it reached alone.
        for (metric, expected) in [
            (SCHEDULED_SERIES, taken),
            (STARTED_SERIES, taken),
            (SUCCEEDED_SERIES, returned),
            (FAILED_SERIES, errored),
            (TIMED_OUT_SERIES, 0.0),
        ] {
            let read = metered
                .summed(case, metric, &function_arn_dimensions(&function))
                .await;
            assert!(
                same(read, expected),
                "case {case:?}: expected {metric} of {function} to count {expected}, got {read}"
            );
            for point in &metered
                .read(case, metric, &function_arn_dimensions(&function), &["Sum"])
                .await
            {
                assert_eq!(
                    point["Unit"].as_str(),
                    Some(COUNT_UNIT),
                    "case {case:?}: {metric} is a count, got {point}"
                );
            }
        }

        // The run span is sampled once per invocation that closed, whichever series it
        // closed in, and is published as a span of milliseconds.
        let samples = metered
            .sampled(case, RUN_TIME_SERIES, &function_arn_dimensions(&function))
            .await;
        assert!(
            same(samples, taken),
            "case {case:?}: expected {taken} {RUN_TIME_SERIES} sample(s) of {function}, got \
             {samples}"
        );
        for point in &metered
            .read(
                case,
                RUN_TIME_SERIES,
                &function_arn_dimensions(&function),
                &["SampleCount"],
            )
            .await
        {
            assert_eq!(
                point["Unit"].as_str(),
                Some(MILLISECONDS_UNIT),
                "case {case:?}: {RUN_TIME_SERIES} is a span of milliseconds, got {point}"
            );
        }

        // The execution counters carry the same split under the state machine's own
        // dimension: every run started, and each run in the series of the outcome IT
        // reached.
        for (metric, expected) in [
            (EXECUTIONS_STARTED_SERIES, taken),
            (EXECUTIONS_SUCCEEDED_SERIES, returned),
            (EXECUTIONS_FAILED_SERIES, errored),
        ] {
            let read = metered
                .summed(case, metric, &machine_arn_dimensions(&machine))
                .await;
            assert!(
                same(read, expected),
                "case {case:?}: expected {metric} of {machine} to count {expected}, got {read}"
            );
        }

        // And the two families never cross: no per-function series exists under the state
        // machine's dimension, and no execution counter under the function's.
        for metric in [
            SCHEDULED_SERIES,
            STARTED_SERIES,
            SUCCEEDED_SERIES,
            FAILED_SERIES,
            RUN_TIME_SERIES,
        ] {
            let read = metered
                .read(case, metric, &machine_arn_dimensions(&machine), &["Sum"])
                .await;
            assert!(
                read.is_empty(),
                "case {case:?}: {metric} belongs to the function's dimension alone, got {read:?}"
            );
        }
        for metric in [
            EXECUTIONS_STARTED_SERIES,
            EXECUTIONS_SUCCEEDED_SERIES,
            EXECUTIONS_FAILED_SERIES,
        ] {
            let read = metered
                .read(case, metric, &function_arn_dimensions(&function), &["Sum"])
                .await;
            assert!(
                read.is_empty(),
                "case {case:?}: {metric} belongs to the state machine's dimension alone, got \
                 {read:?}"
            );
        }
    }
}
