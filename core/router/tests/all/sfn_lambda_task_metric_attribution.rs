// SPDX-License-Identifier: CC0-1.0

//! WHERE the work a Step Functions `Task` state takes on a LAMBDA FUNCTION is accounted,
//! read exactly as a client reads it: `GetMetricStatistics` over CloudWatch's own wire.
//!
//! RULE: a `Task` state whose `Resource` is the ARN of a Lambda function accounts each
//! invocation it takes on that function in the `AWS/States` series dimensioned on that
//! function's ARN ALONE — `LambdaFunctionsScheduled` and `LambdaFunctionsStarted` once per
//! invocation, `LambdaFunctionsSucceeded` or `LambdaFunctionsFailed` once per invocation
//! according to how it closed, and one `LambdaFunctionRunTime` sample per invocation —
//! while the EXECUTIONS that took them are accounted in the `AWS/States` series dimensioned
//! on the state machine's ARN alone (`ExecutionsStarted` / `ExecutionsSucceeded` /
//! `ExecutionsFailed`). The two dimension families never cross: neither family's series
//! appears under the other's dimension. The same work is accounted a third time by Lambda,
//! in `AWS/Lambda` per function name: one `Invocations` observation per invocation it
//! admitted and one `Errors` observation per invocation whose handler failed. Every one of
//! those series is readable through `GetMetricStatistics`, and a series nothing was
//! accounted in answers with no datapoint rather than refusing or never settling.
//!
//! Four crates have to agree for that to hold — Step Functions decides and makes the
//! invocation and accounts both families, IAM holds the document the invocation is judged
//! against, Lambda runs the handler and accounts its own series, and CloudWatch answers for
//! every series — so the suite lives here rather than in any one of them. The function is
//! declared as a CloudFormation stack's inline source, which is how a handler written out
//! in a suite comes to be a deployment package at all.
//!
//! The rows differ along what the rule generalises over: how many invocations the state
//! machine takes (one, two, three) and how each of them closes (the handler returning, the
//! handler raising, and both mixed into one pair of series), which is what every counter
//! above is a function of.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// `tests/all/` is ONE crate root, so the percent-encoder of an
// `application/x-www-form-urlencoded` component, the reader of one element of an awsQuery
// answer, and the wait for a deployed stack to settle are borrowed rather than copied.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request below is signed for, and so the Region the function, the role
/// and the state machine belong to.
const REGION: &str = "us-east-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// The namespace Step Functions publishes its own series to, the two dimensions those
/// series are reported per, and the names the AWS Step Functions metric reference gives the
/// Lambda-task counters, the run span and the execution counters. Written as literals here
/// rather than read from the Step Functions crate, so a series published under a name no AWS
/// reader would ask for fails here.
const STATES_NAMESPACE: &str = "AWS/States";
/// See [`STATES_NAMESPACE`].
const LAMBDA_FUNCTION_ARN_DIMENSION: &str = "LambdaFunctionArn";
/// See [`STATES_NAMESPACE`].
const STATE_MACHINE_ARN_DIMENSION: &str = "StateMachineArn";
/// See [`STATES_NAMESPACE`].
const LAMBDA_FUNCTIONS_SCHEDULED: &str = "LambdaFunctionsScheduled";
/// See [`STATES_NAMESPACE`].
const LAMBDA_FUNCTIONS_STARTED: &str = "LambdaFunctionsStarted";
/// See [`STATES_NAMESPACE`].
const LAMBDA_FUNCTIONS_SUCCEEDED: &str = "LambdaFunctionsSucceeded";
/// See [`STATES_NAMESPACE`].
const LAMBDA_FUNCTIONS_FAILED: &str = "LambdaFunctionsFailed";
/// See [`STATES_NAMESPACE`].
const LAMBDA_FUNCTION_RUN_TIME: &str = "LambdaFunctionRunTime";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_STARTED: &str = "ExecutionsStarted";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_SUCCEEDED: &str = "ExecutionsSucceeded";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_FAILED: &str = "ExecutionsFailed";

/// The namespace Lambda publishes its own series to, the dimension they are reported per,
/// and the two the AWS Lambda metric reference names for an invocation and for a run that
/// came to a function error. Literals, for the same reason as above.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
/// See [`LAMBDA_NAMESPACE`].
const FUNCTION_NAME_DIMENSION: &str = "FunctionName";
/// See [`LAMBDA_NAMESPACE`].
const INVOCATIONS: &str = "Invocations";
/// See [`LAMBDA_NAMESPACE`].
const ERRORS: &str = "Errors";

/// The unit a counter carries, and the unit a span of time carries.
const COUNT_UNIT: &str = "Count";
/// See [`COUNT_UNIT`].
const MILLISECONDS_UNIT: &str = "Milliseconds";

/// The `Period`, in seconds, every read below asks for: the one-minute granularity these
/// series are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of now a read's window reaches, in seconds — wide enough that the
/// minute an observation was published in is inside it however a row straddles a minute
/// boundary.
const WINDOW_SECS: f64 = 1800.0;

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The `StackStatus` a deployed stack whose resources are all up reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The terminal statuses an execution reaches, and the one it sits in while its task runs.
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`SUCCEEDED`].
const FAILED: &str = "FAILED";
/// See [`SUCCEEDED`].
const RUNNING: &str = "RUNNING";

/// The runtime the handler below is written for, and the module the archived inline source
/// lands in, which the handler name resolves against.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "index.handler";

/// The one handler every row invokes: it echoes back what the event asks it to echo, and
/// raises when the event asks it to — so one deployment package serves every row and what an
/// invocation does is decided by the data the execution's own input carries.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    if event.get("mode") == "boom":
        raise ValueError("the row fails on purpose")
    return {"echo": event.get("echo", 0)}
"#;

/// The `Timeout`, in seconds, the function declares: well past what a run needs on any host,
/// so nothing but the handler ends one of its runs.
const AMPLE_TIMEOUT_SECS: u64 = 300;

/// The class Python's own `ValueError` reports, which is the `errorType` of the document a
/// raising run answers and so the name the failed task and the failed execution carry.
const RAISED_CLASS: &str = "ValueError";

/// The IAM action an invocation is subject to, which is the one Lambda PUBLISHES for
/// `Invoke` rather than the operation's own name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// An action a function's own role carries, so the row's documents say something about both
/// roles rather than leaving either without one.
const LOG_ACTION: &str = "logs:PutLogEvents";

/// The service a state machine's execution role is assumed by, and the one a function's own
/// execution role is.
const STATES_PRINCIPAL: &str = "states.amazonaws.com";
/// See [`STATES_PRINCIPAL`].
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// How often to look while waiting for work the clock owes — a deployed function reaching
/// [`FUNCTION_ACTIVE`], an execution leaving [`RUNNING`] — and how many looks to take. The
/// spans are those services' own, so no fixed sleep names the instant one ends, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const LOOKS: usize = 2_400;

/// The emulator as it is ASSEMBLED, on one engine, so the invocation a task makes, the
/// documents it is judged against and every series published for it are all served by the
/// same mounted services a client reaches.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what
/// routes the request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One call to an AWS JSON service, as its status and parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("{signing_name}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
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

/// One Step Functions call, requiring a 200.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "states", "AWSStepFunctions", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One CloudWatch call, requiring a 200.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        "monitoring",
        "GraniteServiceVersion20100801",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One Lambda `restJson1` request, as its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(payload))
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

/// One awsQuery call, as its status and its XML body.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
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
        .uri(format!("https://{host}/"))
        .header("host", host.to_string())
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One awsQuery call to IAM, requiring a 200, answered with its XML body.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> String {
    let (status, text) = query(
        router,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "{action} must answer 200 from IAM, got {status} with {text}"
    );
    text
}

/// One awsQuery call to CloudFormation, requiring a 200, answered with its XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let (status, text) = query(
        router,
        "cloudformation",
        &host,
        "2010-05-15",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "{action} must answer 200 from CloudFormation, got {status} with {text}"
    );
    text
}

/// The text a member of a response carries.
fn string(body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("expected a {name} in {body}"))
        .to_string()
}

/// Now, in epoch seconds, which is how CloudWatch's `awsJson` wire form carries an instant.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// A role `service` may assume, carrying one statement granting `action` on `resource` as its
/// inline policy; answers its ARN.
///
/// A state machine's execution role is trusted by Step Functions and a function's own
/// execution role by Lambda — which `CreateFunction` checks — so the trusted service is the
/// caller's.
async fn given_role(
    router: &MultiServiceRouter,
    name: &str,
    service: &str,
    action: &str,
    resource: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": service },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("CreateRole({name:?}) must answer an ARN, got {created}"));
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": resource }],
    })
    .to_string();
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "inline"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    arn
}

/// Deploy the row's function as a stack's inline source — which is what archives the handler
/// into a deployment package — wait until Lambda reports it [`FUNCTION_ACTIVE`], and answer
/// its ARN.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    stack: &str,
    name: &str,
    role_arn: &str,
) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Echoing": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": name,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": AMPLE_TIMEOUT_SECS,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            },
        },
    })
    .to_string();
    cloudformation(
        router,
        "CreateStack",
        &[("StackName", stack), ("TemplateBody", &template)],
    )
    .await;
    let (reported, answer) = wait_for_stack_status(router, stack, CREATE_COMPLETE).await;
    assert_eq!(
        reported, CREATE_COMPLETE,
        "case {case:?}: the function's stack must reach {CREATE_COMPLETE}, last saw {answer}"
    );

    let mut configuration = Value::Null;
    for _ in 0..LOOKS {
        let (status, answered) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetFunctionConfiguration must resolve the deployed function, got \
             {status} with {answered}"
        );
        configuration = answered;
        if configuration.get("State").and_then(Value::as_str) == Some(FUNCTION_ACTIVE) {
            return string(&configuration, "FunctionArn");
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the function must reach {FUNCTION_ACTIVE}, last saw {configuration}");
}

/// A state machine whose one `Task` state's `Resource` is `function_arn` — the BARE function
/// ARN, which names the function itself — under `role_arn`; answers its ARN.
async fn given_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    function_arn: &str,
    role_arn: &str,
) -> String {
    let definition = json!({
        "StartAt": "Invoke",
        "States": { "Invoke": { "Type": "Task", "Resource": function_arn, "End": true } },
    });
    let created = sfn(
        router,
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

/// Start one execution named `name` with `input` and report it once it has left [`RUNNING`].
async fn ran(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    name: &str,
    input: &Value,
) -> Value {
    let started = sfn(
        router,
        case,
        "StartExecution",
        &json!({
            "stateMachineArn": machine_arn,
            "name": name,
            "input": input.to_string(),
        }),
    )
    .await;
    let execution_arn = string(&started, "executionArn");
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        described = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if described.get("status").and_then(Value::as_str) != Some(RUNNING) {
            return described;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the execution must leave {RUNNING}, last saw {described}");
}

/// Every datapoint `GetMetricStatistics` answers with for one series, over a window the whole
/// run falls inside. A series nothing was accounted in answers with none, which is what "that
/// family is absent under this dimension" reads as.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    unit: &str,
    dimensions: &Value,
) -> Vec<Value> {
    let now = now_secs();
    let stats = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": namespace,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum", "SampleCount"],
            "Unit": unit,
        }),
    )
    .await;
    stats["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The `Sum` of a counter's series over the window: the magnitude a client charts or alarms
/// on. A series with no datapoint sums to zero.
async fn counted(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    dimensions: &Value,
) -> f64 {
    datapoints(router, case, namespace, metric, COUNT_UNIT, dimensions)
        .await
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}

/// How many SAMPLES a span's series holds over the window: one per observation published,
/// which for a run span is one per invocation that closed.
async fn sampled(router: &MultiServiceRouter, case: &str, metric: &str, dimensions: &Value) -> f64 {
    datapoints(
        router,
        case,
        STATES_NAMESPACE,
        metric,
        MILLISECONDS_UNIT,
        dimensions,
    )
    .await
    .iter()
    .filter_map(|point| point["SampleCount"].as_f64())
    .sum()
}

/// The dimension set the per-function `AWS/States` series are reported under: the function's
/// ARN alone.
fn function_arn_dimensions(function_arn: &str) -> Value {
    json!([{ "Name": LAMBDA_FUNCTION_ARN_DIMENSION, "Value": function_arn }])
}

/// The dimension set the per-execution `AWS/States` series are reported under: the state
/// machine's ARN alone.
fn state_machine_dimensions(machine_arn: &str) -> Value {
    json!([{ "Name": STATE_MACHINE_ARN_DIMENSION, "Value": machine_arn }])
}

/// The dimension set Lambda's own series are reported under.
fn function_name_dimensions(name: &str) -> Value {
    json!([{ "Name": FUNCTION_NAME_DIMENSION, "Value": name }])
}

/// The mode that makes the handler return, and the one that makes it raise.
const OK: &str = "ok";
/// See [`OK`].
const BOOM: &str = "boom";

/// One row: how many invocations one state machine takes, and how each of them closes. Every
/// counter the rule names is a FUNCTION of this list, so no row states one twice.
struct Row {
    case: &'static str,
    /// Each execution the row starts, as `(the mode the handler is asked to take, the value
    /// it is asked to echo)`.
    runs: &'static [(&'static str, i64)],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one invocation, which returns",
            runs: &[(OK, 1)],
        },
        Row {
            case: "one invocation, which raises",
            runs: &[(BOOM, 0)],
        },
        // The shape the behaviour was recorded in: one return and one raise, so each closing
        // counter carries a magnitude different from the scheduled and started ones.
        Row {
            case: "two invocations, one returning and one raising",
            runs: &[(OK, 2), (BOOM, 0)],
        },
        Row {
            case: "three invocations, two returning and one raising",
            runs: &[(OK, 3), (OK, 4), (BOOM, 0)],
        },
    ]
}

/// Every invocation a function-ARN `Task` takes is accounted per FUNCTION and every execution
/// that took them per STATE MACHINE, both readable through `GetMetricStatistics`, with neither
/// family under the other's dimension.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_function_arn_task_is_accounted_per_function_and_per_state_machine() {
    let router = build_router();

    for (index, row) in rows().into_iter().enumerate() {
        let Row { case, runs } = row;
        // One function and one state machine per row, so each row reads its OWN series.
        let function_name = format!("xsvc-accounted-{index}");
        let function_role = given_role(
            &router,
            &format!("xsvc-accounted-function-role-{index}"),
            LAMBDA_PRINCIPAL,
            LOG_ACTION,
            "*",
        )
        .await;
        let function_arn = given_function(
            &router,
            case,
            &format!("xsvc-accounted-stack-{index}"),
            &function_name,
            &function_role,
        )
        .await;
        assert_eq!(
            function_arn,
            format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{function_name}"),
            "case {case:?}: the ARN the task names must be the function's own, got \
             {function_arn:?}"
        );
        // The execution role carries the grant the invocation is judged against, on the
        // function's own ARN.
        let machine_role = given_role(
            &router,
            &format!("xsvc-accounted-states-role-{index}"),
            STATES_PRINCIPAL,
            INVOKE_ACTION,
            &function_arn,
        )
        .await;
        let machine = given_machine(
            &router,
            case,
            &format!("xsvc-accounted-{index}"),
            &function_arn,
            &machine_role,
        )
        .await;

        // Run the row's executions, and check each outcome really came from the run: a
        // returning one echoes what its input asked for, a raising one carries the class the
        // handler raised.
        let mut expected_succeeded = 0.0;
        let mut expected_failed = 0.0;
        for (run, (mode, echo)) in runs.iter().enumerate() {
            let input = json!({ "mode": mode, "echo": echo });
            let execution = ran(&router, case, &machine, &format!("run-{run}"), &input).await;
            let status = execution.get("status").and_then(Value::as_str);
            if *mode == OK {
                expected_succeeded += 1.0;
                assert_eq!(
                    status,
                    Some(SUCCEEDED),
                    "case {case:?} run {run}: expected {SUCCEEDED}, got {execution}"
                );
                let reported = string(&execution, "output");
                let output: Value = serde_json::from_str(&reported).unwrap_or_else(|error| {
                    panic!("case {case:?} run {run}: expected a JSON output, got {reported:?} ({error})")
                });
                assert_eq!(
                    output,
                    json!({ "echo": echo }),
                    "case {case:?} run {run}: expected the handler's own return value, got \
                     {output}"
                );
            } else {
                expected_failed += 1.0;
                assert_eq!(
                    status,
                    Some(FAILED),
                    "case {case:?} run {run}: expected {FAILED}, got {execution}"
                );
                assert_eq!(
                    execution.get("error").and_then(Value::as_str),
                    Some(RAISED_CLASS),
                    "case {case:?} run {run}: expected the raised class as the error, got \
                     {execution}"
                );
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let invocations = runs.len() as f64;

        // The per-FUNCTION family, under the function's ARN alone.
        let per_function = function_arn_dimensions(&function_arn);
        for (metric, expected) in [
            (LAMBDA_FUNCTIONS_SCHEDULED, invocations),
            (LAMBDA_FUNCTIONS_STARTED, invocations),
            (LAMBDA_FUNCTIONS_SUCCEEDED, expected_succeeded),
            (LAMBDA_FUNCTIONS_FAILED, expected_failed),
        ] {
            let actual = counted(&router, case, STATES_NAMESPACE, metric, &per_function).await;
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {STATES_NAMESPACE} {metric} under \
                 {LAMBDA_FUNCTION_ARN_DIMENSION} to sum to {expected}, got {actual}"
            );
        }
        let run_samples = sampled(&router, case, LAMBDA_FUNCTION_RUN_TIME, &per_function).await;
        assert_eq!(
            run_samples, invocations,
            "case {case:?}: expected {invocations} {LAMBDA_FUNCTION_RUN_TIME} samples under \
             {LAMBDA_FUNCTION_ARN_DIMENSION}, got {run_samples}"
        );

        // The per-EXECUTION family, under the state machine's ARN alone.
        let per_machine = state_machine_dimensions(&machine);
        for (metric, expected) in [
            (EXECUTIONS_STARTED, invocations),
            (EXECUTIONS_SUCCEEDED, expected_succeeded),
            (EXECUTIONS_FAILED, expected_failed),
        ] {
            let actual = counted(&router, case, STATES_NAMESPACE, metric, &per_machine).await;
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {STATES_NAMESPACE} {metric} under \
                 {STATE_MACHINE_ARN_DIMENSION} to sum to {expected}, got {actual}"
            );
        }

        // The two families never cross. Each read is asserted on the DATAPOINTS rather than
        // on their sum, so a zero-valued datapoint under the wrong dimension fails too.
        for metric in [
            LAMBDA_FUNCTIONS_SCHEDULED,
            LAMBDA_FUNCTIONS_STARTED,
            LAMBDA_FUNCTIONS_SUCCEEDED,
            LAMBDA_FUNCTIONS_FAILED,
        ] {
            let held = datapoints(
                &router,
                case,
                STATES_NAMESPACE,
                metric,
                COUNT_UNIT,
                &per_machine,
            )
            .await;
            assert!(
                held.is_empty(),
                "case {case:?}: {metric} belongs to {LAMBDA_FUNCTION_ARN_DIMENSION} alone, yet \
                 {STATE_MACHINE_ARN_DIMENSION} answered {held:?}"
            );
        }
        let crossed = datapoints(
            &router,
            case,
            STATES_NAMESPACE,
            LAMBDA_FUNCTION_RUN_TIME,
            MILLISECONDS_UNIT,
            &per_machine,
        )
        .await;
        assert!(
            crossed.is_empty(),
            "case {case:?}: {LAMBDA_FUNCTION_RUN_TIME} belongs to \
             {LAMBDA_FUNCTION_ARN_DIMENSION} alone, yet {STATE_MACHINE_ARN_DIMENSION} answered \
             {crossed:?}"
        );
        for metric in [EXECUTIONS_STARTED, EXECUTIONS_SUCCEEDED, EXECUTIONS_FAILED] {
            let held = datapoints(
                &router,
                case,
                STATES_NAMESPACE,
                metric,
                COUNT_UNIT,
                &per_function,
            )
            .await;
            assert!(
                held.is_empty(),
                "case {case:?}: {metric} belongs to {STATE_MACHINE_ARN_DIMENSION} alone, yet \
                 {LAMBDA_FUNCTION_ARN_DIMENSION} answered {held:?}"
            );
        }

        // And Lambda accounted the same work in its own namespace: every invocation the task
        // made was one Lambda admitted, and the raising ones came to a function error.
        let per_name = function_name_dimensions(&function_name);
        let admitted = counted(&router, case, LAMBDA_NAMESPACE, INVOCATIONS, &per_name).await;
        assert!(
            admitted >= invocations,
            "case {case:?}: expected at least {invocations} {LAMBDA_NAMESPACE} {INVOCATIONS}, got \
             {admitted}"
        );
        let errored = counted(&router, case, LAMBDA_NAMESPACE, ERRORS, &per_name).await;
        assert_eq!(
            errored, expected_failed,
            "case {case:?}: expected {LAMBDA_NAMESPACE} {ERRORS} to sum to {expected_failed}, got \
             {errored}"
        );
    }
}
