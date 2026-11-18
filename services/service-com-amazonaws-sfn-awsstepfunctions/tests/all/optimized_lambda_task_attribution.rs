// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH dimension family the series of a Lambda `Task` state's work
//! land in, driven over each service's own wire protocol through the production
//! `MultiServiceRouter` with Step Functions, IAM, Lambda and CloudWatch mounted on ONE
//! engine — the shape `core/router/src/lib.rs` mounts them in — so the role documents the
//! attempt is judged against are the ones IAM's handlers wrote, an allowed attempt really
//! runs the function, and every series is read back through CloudWatch's own
//! `GetMetricStatistics`.
//!
//! RULE: the per-function `AWS/States` series under the `LambdaFunctionArn` dimension
//! account for the invocations a `Task` state took on a function its own `Resource`
//! NAMED, so a `Task` whose `Resource` is an integration token names no function and is
//! counted in none of them however many times its call runs — while the per-execution
//! series under the `StateMachineArn` dimension count every execution of every state
//! machine, whatever form its states' `Resource`s take, and the `AWS/Lambda` series
//! account for the invocations LAMBDA admitted, whichever form asked for them. The three
//! families are therefore independent readings of the same work, and the magnitude each
//! reports over a window is the number of executions that ran in it.
//!
//! The rows differ along what the rule generalises over: the form the `Task` names its
//! work in (the function's own ARN, and the optimized integration token on the very same
//! kind of function), whether the state machine's role may make the call at all, and how
//! many executions of the row ran in the window — which is what each series' magnitude is
//! read against.
//!
//! Which attempts a per-function series accounts for is therefore a fact about the
//! `Resource` that named the function rather than about the function: ONE function reached
//! in both forms is counted there once per attempt of the naming form and never for the
//! integration token's, and a function reached ONLY through the token is named by no
//! `Resource` at all, so every series of that family — the counts and the spans — holds
//! nothing for it, whichever outcome its executions reached. Each magnitude is settled once
//! the execution it counts has left `RUNNING`: a client POLLING it is answered the same
//! figures however many times the service has since reported on the same histories, which
//! is what the rows below that read every series repeatedly are about.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::EmulatedService;

use crate::execution_data_encryption::{form_encode, xml_element};
use crate::lambda_task_control_flow::{package, parsed};
use crate::state_machine_lifecycle::{string, REGION};

/// The namespace Step Functions publishes its own series to, and the names the AWS Step
/// Functions metric reference gives the per-function counts, the per-execution counts, the
/// run span and the two dimensions they are reported under. Written as literals here
/// rather than read from the crate under test, so a series published under a name no AWS
/// reader would ask for fails here.
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
const RUN_TIME_SERIES: &str = "LambdaFunctionRunTime";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_SUCCEEDED_SERIES: &str = "ExecutionsSucceeded";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_FAILED_SERIES: &str = "ExecutionsFailed";

/// The namespace Lambda publishes its own metrics to, the name the AWS Lambda metric
/// reference gives the invocation count and the dimension it is reported per.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
/// See [`LAMBDA_NAMESPACE`].
const INVOCATIONS_SERIES: &str = "Invocations";
/// See [`LAMBDA_NAMESPACE`].
const FUNCTION_NAME_DIMENSION: &str = "FunctionName";

/// The `Resource` of the OPTIMIZED Lambda integration: one of this service's own
/// integration tokens, which names no function at all — the request its `Parameters`
/// build does.
const OPTIMIZED_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// The `Period`, in seconds, every query reads at: the one-minute granularity these
/// series are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of now a query's window reaches, in seconds — wide enough that the
/// minute a datapoint was published in is inside it however a row straddles a minute
/// boundary.
const WINDOW_SECS: f64 = 600.0;

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The `State` a function reports once it can be invoked.
const FUNCTION_ACTIVE: &str = "Active";

/// The runtime the handler below is written for.
const RUNTIME: &str = "python3.12";

/// The handler the package's one module exports.
const HANDLER: &str = "index.handler";

/// The execution role the FUNCTION runs as, which is not the state machine's.
const LAMBDA_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/LambdaExecutionRole";

/// The IAM action an invocation is subject to: the one Lambda PUBLISHES for `Invoke`
/// rather than the operation's own name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The error a `Task` state reports when the role may not make the call: Lambda's base SDK
/// exception, which is what a refusal `Invoke` does not model arrives under.
const REFUSED_ERROR: &str = "Lambda.AWSLambdaException";

/// The fixed text of the sentence that refusal carries.
const REFUSED_TEXT: &str = "not authorized to perform: lambda:InvokeFunction";

/// How long one poll of a status the clock owes sleeps between reads.
const POLL_INTERVAL: Duration = Duration::from_millis(50);

/// How long a row waits for a function to reach [`FUNCTION_ACTIVE`], and for an execution
/// to leave `RUNNING`.
const PATIENCE: Duration = Duration::from_secs(40);

/// How long the handler SLEEPS before it answers, in seconds — the span an invocation of
/// it really runs for, so the run span the per-function series reports can be read against
/// work whose duration the test itself chose.
const SLEEP_SECS: f64 = 0.5;

/// The same span in milliseconds, which is the unit the run series is published in.
const SLEEP_MILLIS: f64 = SLEEP_SECS * 1000.0;

/// The handler every row's function exports: it sleeps for [`SLEEP_SECS`] and echoes what
/// the event carries, so an allowed attempt's result is the run's own answer and the span
/// it ran for is at least that sleep.
const HANDLER_SOURCE: &str = r#"
import time


def handler(event, context):
    time.sleep(0.5)
    return {"echo": event["n"]}
"#;

/// Step Functions, IAM, Lambda and CloudWatch over ONE engine.
struct Chain {
    router: MultiServiceRouter,
}

impl Chain {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(
                Arc::clone(&engine),
            ),
        );
        router.register_service(
            service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
        );
        router.register_service(
            service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
                engine,
            ),
        );
        Self { router }
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

    /// One `awsJson` call to the service `signing_name` routes to, requiring a 200.
    async fn rpc(
        &self,
        case: &str,
        signing_name: &str,
        target_prefix: &str,
        operation: &str,
        body: &Value,
    ) -> Value {
        let request = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.0")
            .header("x-amz-target", format!("{target_prefix}.{operation}"))
            .header("authorization", Self::authorization(signing_name))
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

    async fn sfn(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(case, "states", "AWSStepFunctions", operation, body)
            .await
    }

    async fn cloudwatch(&self, case: &str, operation: &str, body: &Value) -> Value {
        self.rpc(
            case,
            "monitoring",
            "GraniteServiceVersion20100801",
            operation,
            body,
        )
        .await
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

    /// One `awsQuery` call to IAM, requiring a 200.
    async fn iam(&self, case: &str, action: &str, params: &[(&str, String)]) -> String {
        let mut body = format!("Action={action}&Version=2010-05-08");
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
            .header("authorization", Self::authorization("iam"))
            .body(Bytes::from(body))
            .expect("a well-formed awsQuery request");
        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let text = String::from_utf8_lossy(&response.into_body()).to_string();
        assert_eq!(
            status, 200,
            "case {case:?}: {action} must answer 200 from IAM, got {status} with {text}"
        );
        text
    }

    /// A role this service's own principal may assume, carrying `statements` as its inline
    /// policy; answers the ARN IAM's own `CreateRole` reported for it.
    async fn role(&self, case: &str, name: &str, statements: &Value) -> String {
        let trust = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Effect": "Allow",
                "Principal": { "Service": "states.amazonaws.com" },
                "Action": "sts:AssumeRole",
            }],
        });
        let created = self
            .iam(
                case,
                "CreateRole",
                &[
                    ("RoleName", name.to_string()),
                    ("AssumeRolePolicyDocument", trust.to_string()),
                ],
            )
            .await;
        self.iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", name.to_string()),
                ("PolicyName", "TaskWork".to_string()),
                (
                    "PolicyDocument",
                    json!({ "Version": "2012-10-17", "Statement": statements }).to_string(),
                ),
            ],
        )
        .await;
        xml_element(&created, "Arn").unwrap_or_else(|| {
            panic!("case {case:?}: CreateRole answers the role Arn, got {created}")
        })
    }

    /// The row's function, waited out to [`FUNCTION_ACTIVE`] so an invocation of it is one
    /// Lambda admits; answers its ARN.
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
                    // Longer than the handler sleeps, so nothing the function's own bound
                    // does ends an invocation of it.
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

    /// Create `definition` as a state machine under `role_arn`; answers its ARN.
    async fn machine(&self, case: &str, name: &str, definition: &Value, role_arn: &str) -> String {
        let created = self
            .sfn(
                case,
                "CreateStateMachine",
                &json!({
                    "name": name,
                    "definition": definition.to_string(),
                    "roleArn": role_arn,
                }),
            )
            .await;
        string(&created, "stateMachineArn")
    }

    /// Start one execution of `machine` named `name` with `input`, and report it once it
    /// has left `RUNNING`.
    async fn ran(&self, case: &str, machine: &str, name: &str, input: &Value) -> Value {
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
                began.elapsed() < PATIENCE,
                "case {case:?}: expected a terminal status within {PATIENCE:?}, last saw \
                 {described}"
            );
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// The datapoints the series `metric` of `namespace` reports over the window for the
    /// dimension set `dimensions`, each carrying `statistics`. A series with no datapoint
    /// answers an empty list, which is what "the series stayed empty" reads as.
    async fn datapoints(
        &self,
        case: &str,
        namespace: &str,
        metric: &str,
        dimensions: &Value,
        statistics: &[&str],
    ) -> Vec<Value> {
        let now = now_secs();
        let stats = self
            .cloudwatch(
                case,
                "GetMetricStatistics",
                &json!({
                    "Namespace": namespace,
                    "MetricName": metric,
                    "Dimensions": dimensions,
                    "StartTime": now - WINDOW_SECS,
                    "EndTime": now + WINDOW_SECS,
                    "Period": PERIOD_SECS,
                    "Statistics": statistics,
                }),
            )
            .await;
        stats["Datapoints"].as_array().cloned().unwrap_or_default()
    }

    /// The SUM the series reports over the window — the magnitude a client charts or
    /// alarms on. A series with no datapoint sums to zero.
    async fn summed(&self, case: &str, namespace: &str, metric: &str, dimensions: &Value) -> f64 {
        self.datapoints(case, namespace, metric, dimensions, &["Sum"])
            .await
            .iter()
            .filter_map(|point| point["Sum"].as_f64())
            .sum()
    }
}

/// Now, in whole epoch seconds, which is how an `awsJson` request carries an instant.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("now is after the epoch")
        .as_secs_f64()
        .trunc()
}

/// The dimension set of a series reported per invoked FUNCTION, as `AWS/States` names it.
fn function_arn_dimensions(function_arn: &str) -> Value {
    json!([{ "Name": LAMBDA_FUNCTION_ARN_DIMENSION, "Value": function_arn }])
}

/// The dimension set of a series reported per STATE MACHINE.
fn machine_dimensions(machine_arn: &str) -> Value {
    json!([{ "Name": STATE_MACHINE_ARN_DIMENSION, "Value": machine_arn }])
}

/// The dimension set `AWS/Lambda` reports an invocation count per.
fn function_name_dimensions(name: &str) -> Value {
    json!([{ "Name": FUNCTION_NAME_DIMENSION, "Value": name }])
}

/// A statement allowing `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// How a `Task` state names the work it takes on a Lambda function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// The `Resource` is the function's own ARN, so the state names the function.
    FunctionArn,
    /// The `Resource` is the optimized integration token, so the state names no function
    /// and the request its `Parameters` build carries the one to call.
    Optimized,
}

impl Form {
    /// The one-state definition this form writes over `function_arn`.
    fn definition(self, function_arn: &str) -> Value {
        let task = match self {
            Self::FunctionArn => json!({
                "Type": "Task",
                "Resource": function_arn,
                "End": true,
            }),
            Self::Optimized => json!({
                "Type": "Task",
                "Resource": OPTIMIZED_RESOURCE,
                "Parameters": { "FunctionName": function_arn, "Payload.$": "$" },
                "End": true,
            }),
        };
        json!({ "StartAt": "Invoke", "States": { "Invoke": task } })
    }

    /// What the handler's own answer looks like in the execution's `output` for this form:
    /// the optimized integration answers the whole `Invoke` response, which carries the
    /// answer under `Payload`, while a `Resource` that named the function answers it
    /// directly.
    fn answered(self, output: &Value) -> Value {
        match self {
            Self::FunctionArn => output.clone(),
            Self::Optimized => output["Payload"].clone(),
        }
    }
}

/// One row: the form the `Task` names its work in, whether the role may make the call, and
/// how many executions of it ran in the window.
struct Row {
    case: &'static str,
    form: Form,
    /// Whether the state machine's role is granted the invocation.
    granted: bool,
    /// How many executions of this row's state machine the client ran.
    executions: usize,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a Task whose Resource is the function's own ARN, run twice",
            form: Form::FunctionArn,
            granted: true,
            executions: 2,
        },
        Row {
            case: "the optimized integration on the same kind of function, run three times",
            form: Form::Optimized,
            granted: true,
            executions: 3,
        },
        Row {
            case: "the optimized integration under a role without the grant, run twice",
            form: Form::Optimized,
            granted: false,
            executions: 2,
        },
        Row {
            case: "a Task whose Resource is the function's own ARN, refused once",
            form: Form::FunctionArn,
            granted: false,
            executions: 1,
        },
    ]
}

/// The per-function series account for the invocations a `Task` state's own `Resource`
/// named, so the optimized integration — whose `Resource` names no function — is counted
/// in none of them, while the per-execution series count every execution of either form
/// and `AWS/Lambda` counts every invocation Lambda admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn only_a_resource_that_names_the_function_is_counted_per_function() {
    let chain = Chain::new();

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            form,
            granted,
            executions,
        } = row;
        // One function, one role and one state machine per row, so each row's series are
        // its own.
        let name = format!("attributed-{index}");
        let function = chain.given_function(&name).await;
        let statements = if granted {
            json!([
                allow(INVOKE_ACTION, &function),
                allow(INVOKE_ACTION, &format!("{function}:*")),
            ])
        } else {
            json!([allow("logs:PutLogEvents", "*")])
        };
        let role = chain
            .role(case, &format!("AttributedRole{index}"), &statements)
            .await;
        let machine = chain
            .machine(case, &name, &form.definition(&function), &role)
            .await;

        for run in 0..executions {
            let described = chain
                .ran(
                    case,
                    &machine,
                    &format!("run-{run}"),
                    &json!({ "n": run + 1 }),
                )
                .await;
            if granted {
                let output: Value = serde_json::from_str(&string(&described, "output"))
                    .unwrap_or_else(|error| {
                        panic!("case {case:?}: run {run} must answer JSON output, got {error}")
                    });
                assert_eq!(
                    (
                        described.get("status").and_then(Value::as_str),
                        form.answered(&output),
                    ),
                    (Some("SUCCEEDED"), json!({ "echo": run + 1 })),
                    "case {case:?}: expected run {run} to answer the handler's own result, got \
                     {described}"
                );
            } else {
                assert_eq!(
                    (
                        described.get("status").and_then(Value::as_str),
                        described.get("error").and_then(Value::as_str),
                    ),
                    (Some("FAILED"), Some(REFUSED_ERROR)),
                    "case {case:?}: expected run {run} to end on the refusal, got {described}"
                );
                let cause = string(&described, "cause");
                // The principal is the SESSION of the role the call was made as, under
                // the name the assume gave it.
                let principal =
                    crate::task_refusal_session_principal::session_principal(&role, &cause);
                for fragment in [REFUSED_TEXT, function.as_str(), principal.as_str()] {
                    assert!(
                        cause.contains(fragment),
                        "case {case:?}: expected run {run}'s cause to carry {fragment:?}, got \
                         {cause:?}"
                    );
                }
            }
        }

        #[allow(clippy::cast_precision_loss)]
        let ran = executions as f64;
        // Only a `Resource` that NAMED the function is counted per function: for the
        // optimized form every series of that family stays empty, so a reader sums zero.
        let attributed = if form == Form::FunctionArn { ran } else { 0.0 };
        for (metric, expected) in [
            (SCHEDULED_SERIES, attributed),
            (STARTED_SERIES, attributed),
            (
                SUCCEEDED_SERIES,
                if granted { attributed } else { 0.0 },
            ),
            (
                FAILED_SERIES,
                if granted { 0.0 } else { attributed },
            ),
        ] {
            let counted = chain
                .summed(
                    case,
                    STATES_NAMESPACE,
                    metric,
                    &function_arn_dimensions(&function),
                )
                .await;
            assert!(
                (counted - expected).abs() < f64::EPSILON,
                "case {case:?}: expected {metric} to count {expected} for {function}, got \
                 {counted}"
            );
        }
        // The run span is sampled once per invocation the state took on the function it
        // named, and each sample is at least as long as the work really took — so a form
        // counted per function reports the handler's own sleep, and the optimized form
        // samples nothing at all.
        let spans = chain
            .datapoints(
                case,
                STATES_NAMESPACE,
                RUN_TIME_SERIES,
                &function_arn_dimensions(&function),
                &["SampleCount", "Minimum"],
            )
            .await;
        let samples: f64 = spans
            .iter()
            .filter_map(|point| point["SampleCount"].as_f64())
            .sum();
        assert!(
            (samples - attributed).abs() < f64::EPSILON,
            "case {case:?}: expected {attributed} {RUN_TIME_SERIES} sample(s) of {function}, got \
             {samples} from {spans:?}"
        );
        // A refused attempt made no call, so the only floor its span has is zero; an
        // attempt that ran the handler waited out its sleep.
        let floor = if granted { SLEEP_MILLIS } else { 0.0 };
        for point in &spans {
            let minimum = point["Minimum"].as_f64().unwrap_or_else(|| {
                panic!("case {case:?}: a {RUN_TIME_SERIES} datapoint carries Minimum, got {point}")
            });
            assert!(
                minimum >= floor,
                "case {case:?}: expected every {RUN_TIME_SERIES} sample to be at least \
                 {floor}ms, got {minimum} from {point}"
            );
        }
        // Every execution is counted under its own state machine whatever form its Task
        // took, in the one series its outcome names.
        for (metric, expected) in [
            (
                EXECUTIONS_SUCCEEDED_SERIES,
                if granted { ran } else { 0.0 },
            ),
            (EXECUTIONS_FAILED_SERIES, if granted { 0.0 } else { ran }),
        ] {
            let counted = chain
                .summed(case, STATES_NAMESPACE, metric, &machine_dimensions(&machine))
                .await;
            assert!(
                (counted - expected).abs() < f64::EPSILON,
                "case {case:?}: expected {metric} to count {expected} for {machine}, got {counted}"
            );
        }
        // And Lambda meters the invocations it ADMITTED, whichever form asked for them —
        // so the optimized form, counted in no per-function `AWS/States` series, is still
        // visible there.
        let invoked = chain
            .summed(
                case,
                LAMBDA_NAMESPACE,
                INVOCATIONS_SERIES,
                &function_name_dimensions(&name),
            )
            .await;
        let expected = if granted { ran } else { 0.0 };
        assert!(
            (invoked - expected).abs() < f64::EPSILON,
            "case {case:?}: expected {expected} invocation(s) of {name} in \
             {LAMBDA_NAMESPACE}/{INVOCATIONS_SERIES}, got {invoked}"
        );
    }
}

// What follows reads the SAME families over ONE function reached in BOTH forms, and reads
// them REPEATEDLY: which attempts a per-function series accounts for is a fact about the
// `Resource` that named the function rather than about the function, and the magnitude a
// client polls for is settled once the execution it counts has left `RUNNING`.

/// See [`STATES_NAMESPACE`].
const TIMED_OUT_SERIES: &str = "LambdaFunctionsTimedOut";
/// See [`STATES_NAMESPACE`].
const SCHEDULE_TIME_SERIES: &str = "LambdaFunctionScheduleTime";
/// See [`STATES_NAMESPACE`].
const TIME_SERIES: &str = "LambdaFunctionTime";

/// Every series of the per-function family: the five counts an attempt is scheduled,
/// started and closed in, and the three spans measured either side of its work.
const PER_FUNCTION_SERIES: &[&str] = &[
    SCHEDULED_SERIES,
    STARTED_SERIES,
    SUCCEEDED_SERIES,
    FAILED_SERIES,
    TIMED_OUT_SERIES,
    SCHEDULE_TIME_SERIES,
    RUN_TIME_SERIES,
    TIME_SERIES,
];

/// How many times a row's client asks for the series it watches. More than once, because
/// the rule is about what a POLLING reader converges on: every reading after the first is
/// taken once the service has reported on the same histories again.
const POLLS: usize = 3;

/// One leg of the window: the form its `Task` state names its work in, whether its state
/// machine's role may make the call, and how many executions of it the client ran.
struct Leg {
    case: &'static str,
    form: Form,
    granted: bool,
    executions: usize,
}

/// The legs that share ONE function, so each series below is a reading of the same
/// function's work taken in both forms, allowed and refused.
fn legs() -> Vec<Leg> {
    vec![
        Leg {
            case: "the function's own ARN under a role granted the invocation, run twice",
            form: Form::FunctionArn,
            granted: true,
            executions: 2,
        },
        Leg {
            case: "the optimized integration on the same function under that role, run twice",
            form: Form::Optimized,
            granted: true,
            executions: 2,
        },
        Leg {
            case: "the function's own ARN under a role without the grant, run once",
            form: Form::FunctionArn,
            granted: false,
            executions: 1,
        },
        Leg {
            case: "the optimized integration on the same function under that role, run once",
            form: Form::Optimized,
            granted: false,
            executions: 1,
        },
    ]
}

/// How many attempts the legs of `legs` make in `form`, counting only the legs whose role
/// `granted` where that is asked for.
fn attempts(legs: &[Leg], form: Form, granted: Option<bool>) -> f64 {
    let counted: usize = legs
        .iter()
        .filter(|leg| leg.form == form && granted.is_none_or(|want| leg.granted == want))
        .map(|leg| leg.executions)
        .sum();
    #[allow(clippy::cast_precision_loss)]
    {
        counted as f64
    }
}

/// The statements the role of a leg that may, or may not, invoke `function` carries: the
/// invocation of that function and of its qualified forms, or an action that has nothing
/// to do with one.
fn statements(granted: bool, function: &str) -> Value {
    if granted {
        json!([
            allow(INVOKE_ACTION, function),
            allow(INVOKE_ACTION, &format!("{function}:*")),
        ])
    } else {
        json!([allow("logs:PutLogEvents", "*")])
    }
}

/// One execution of a leg ended as that leg's grant decides: an allowed call answers the
/// run's own result, and a refused one fails under Lambda's base SDK exception.
fn ended(described: &Value, case: &str, run: usize, granted: bool) {
    let expected = if granted { "SUCCEEDED" } else { "FAILED" };
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some(expected),
        "case {case:?}: expected run {run} to end {expected}, got {described}"
    );
    if !granted {
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(REFUSED_ERROR),
            "case {case:?}: expected run {run} to end on the refusal, got {described}"
        );
    }
}

/// Whether two magnitudes of a series are the same figure.
fn same(counted: f64, expected: f64) -> bool {
    (counted - expected).abs() < f64::EPSILON
}

/// One function reached in BOTH forms is counted in the per-function series once per
/// attempt of the form whose `Resource` NAMED it and never for the integration token's,
/// and the figures a client polls for do not move as it polls them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_function_counts_only_the_attempts_its_own_resource_named() {
    let chain = Chain::new();
    let case = "one function, reached in both forms";
    let name = "converged-shared";
    let function = chain.given_function(name).await;

    let legs = legs();
    let mut machines: Vec<String> = Vec::new();
    let mut executions: Vec<String> = Vec::new();
    for (index, leg) in legs.iter().enumerate() {
        let role = chain
            .role(
                leg.case,
                &format!("ConvergedRole{index}"),
                &statements(leg.granted, &function),
            )
            .await;
        let machine = chain
            .machine(
                leg.case,
                &format!("converged-{index}"),
                &leg.form.definition(&function),
                &role,
            )
            .await;
        for run in 0..leg.executions {
            let described = chain
                .ran(
                    leg.case,
                    &machine,
                    &format!("run-{run}"),
                    &json!({ "n": run + 1 }),
                )
                .await;
            ended(&described, leg.case, run, leg.granted);
            executions.push(string(&described, "executionArn"));
        }
        machines.push(machine);
    }

    // Only the attempts of the form whose `Resource` named the function are counted per
    // function: each of them was scheduled and then started whether or not the call was
    // allowed, and closed in the one series its outcome names. No attempt ran out of the
    // time a state allowed it, so the timed-out series of the family stays empty.
    let named = attempts(&legs, Form::FunctionArn, None);
    let counts = [
        (SCHEDULED_SERIES, named),
        (STARTED_SERIES, named),
        (
            SUCCEEDED_SERIES,
            attempts(&legs, Form::FunctionArn, Some(true)),
        ),
        (
            FAILED_SERIES,
            attempts(&legs, Form::FunctionArn, Some(false)),
        ),
        (TIMED_OUT_SERIES, 0.0),
    ];
    // Lambda meters the invocations it ADMITTED, whichever form asked for them, so the
    // optimized legs' work is visible there though no per-function series counts it.
    let admitted = attempts(&legs, Form::FunctionArn, Some(true))
        + attempts(&legs, Form::Optimized, Some(true));

    for poll in 0..POLLS {
        for (metric, expected) in counts {
            let counted = chain
                .summed(
                    case,
                    STATES_NAMESPACE,
                    metric,
                    &function_arn_dimensions(&function),
                )
                .await;
            assert!(
                same(counted, expected),
                "case {case:?}: poll {poll}: expected {metric} to count {expected} for \
                 {function}, got {counted}"
            );
        }
        // The spans are sampled once per attempt counted per function — one either side of
        // the work and one over the whole of it — so the family's counts and its samples
        // are two readings of the same attempts.
        for metric in [SCHEDULE_TIME_SERIES, RUN_TIME_SERIES, TIME_SERIES] {
            let held = chain
                .datapoints(
                    case,
                    STATES_NAMESPACE,
                    metric,
                    &function_arn_dimensions(&function),
                    &["SampleCount"],
                )
                .await;
            let samples: f64 = held
                .iter()
                .filter_map(|point| point["SampleCount"].as_f64())
                .sum();
            assert!(
                same(samples, named),
                "case {case:?}: poll {poll}: expected {named} {metric} sample(s) of {function}, \
                 got {samples} from {held:?}"
            );
        }
        let invoked = chain
            .summed(
                case,
                LAMBDA_NAMESPACE,
                INVOCATIONS_SERIES,
                &function_name_dimensions(name),
            )
            .await;
        assert!(
            same(invoked, admitted),
            "case {case:?}: poll {poll}: expected {admitted} invocation(s) of {name} in \
             {LAMBDA_NAMESPACE}/{INVOCATIONS_SERIES}, got {invoked}"
        );
        // Every execution is counted under its own state machine whatever form its `Task`
        // took, in the one series its outcome names.
        for (leg, machine) in legs.iter().zip(&machines) {
            #[allow(clippy::cast_precision_loss)]
            let ran = leg.executions as f64;
            for (metric, expected) in [
                (
                    EXECUTIONS_SUCCEEDED_SERIES,
                    if leg.granted { ran } else { 0.0 },
                ),
                (
                    EXECUTIONS_FAILED_SERIES,
                    if leg.granted { 0.0 } else { ran },
                ),
            ] {
                let counted = chain
                    .summed(case, STATES_NAMESPACE, metric, &machine_dimensions(machine))
                    .await;
                assert!(
                    same(counted, expected),
                    "case {:?}: poll {poll}: expected {metric} to count {expected} for {machine}, \
                     got {counted}",
                    leg.case
                );
            }
        }
        // Read every execution back before the next poll: a client watching a workflow
        // reads its runs as it reads their series, and the service reports on those same
        // histories again as it serves the reads — which is what the next poll is taken
        // after, and what a count made twice would show up in.
        for execution in &executions {
            let described = chain
                .sfn(
                    case,
                    "DescribeExecution",
                    &json!({ "executionArn": execution }),
                )
                .await;
            assert_ne!(
                described.get("status").and_then(Value::as_str),
                Some("RUNNING"),
                "case {case:?}: poll {poll}: an execution that ended stays ended, got {described}"
            );
        }
    }
}

/// A `Task` state whose `Resource` is the optimized integration token names no function,
/// so the per-function family of the function its request reaches holds nothing at all —
/// for the allowed call and for the refused one alike, however often the client asks —
/// while the work both legs did is read off the series that do account for it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_optimized_task_leaves_the_per_function_family_empty() {
    let chain = Chain::new();
    let case = "the optimized integration, allowed and refused";
    let name = "converged-optimized";
    let function = chain.given_function(name).await;

    // The two legs of the window: the same state, under a role that may make the call and
    // under one that may not.
    let mut ran: Vec<(bool, String, String)> = Vec::new();
    for granted in [true, false] {
        let leg = if granted { "allowed" } else { "refused" };
        let role = chain
            .role(
                case,
                &format!("ConvergedOptimized-{leg}"),
                &statements(granted, &function),
            )
            .await;
        let machine = chain
            .machine(
                case,
                &format!("converged-optimized-{leg}"),
                &Form::Optimized.definition(&function),
                &role,
            )
            .await;
        let described = chain.ran(case, &machine, "run-0", &json!({ "n": 1 })).await;
        ended(&described, case, 0, granted);
        if granted {
            let output: Value =
                serde_json::from_str(&string(&described, "output")).unwrap_or_else(|error| {
                    panic!("case {case:?}: the allowed leg must answer JSON output, got {error}")
                });
            assert_eq!(
                Form::Optimized.answered(&output),
                json!({ "echo": 1 }),
                "case {case:?}: expected the allowed leg to answer the handler's own result, got \
                 {described}"
            );
        }
        ran.push((granted, machine, string(&described, "executionArn")));
    }

    for poll in 0..POLLS {
        // Not one series of the per-function family carries a datapoint: the dimension
        // names a function no `Resource` here named.
        for metric in PER_FUNCTION_SERIES {
            let held = chain
                .datapoints(
                    case,
                    STATES_NAMESPACE,
                    metric,
                    &function_arn_dimensions(&function),
                    &["Sum", "SampleCount"],
                )
                .await;
            assert!(
                held.is_empty(),
                "case {case:?}: poll {poll}: {metric} accounts for a `Resource` that named the \
                 function, got {held:?}"
            );
        }
        // And the work both legs did is read off the series that do account for it: Lambda
        // admitted the allowed leg's invocation and never saw the refused one, and each
        // execution is counted under its own state machine.
        let invoked = chain
            .summed(
                case,
                LAMBDA_NAMESPACE,
                INVOCATIONS_SERIES,
                &function_name_dimensions(name),
            )
            .await;
        assert!(
            same(invoked, 1.0),
            "case {case:?}: poll {poll}: expected the one admitted invocation of {name} in \
             {LAMBDA_NAMESPACE}/{INVOCATIONS_SERIES}, got {invoked}"
        );
        for (granted, machine, _) in &ran {
            for (metric, expected) in [
                (
                    EXECUTIONS_SUCCEEDED_SERIES,
                    if *granted { 1.0 } else { 0.0 },
                ),
                (EXECUTIONS_FAILED_SERIES, if *granted { 0.0 } else { 1.0 }),
            ] {
                let counted = chain
                    .summed(case, STATES_NAMESPACE, metric, &machine_dimensions(machine))
                    .await;
                assert!(
                    same(counted, expected),
                    "case {case:?}: poll {poll}: expected {metric} to count {expected} for \
                     {machine}, got {counted}"
                );
            }
        }
        for (_, _, execution) in &ran {
            chain
                .sfn(
                    case,
                    "DescribeExecution",
                    &json!({ "executionArn": execution }),
                )
                .await;
        }
    }
}
