// SPDX-License-Identifier: CC0-1.0

//! WHICH of the two accountings of a Step Functions Lambda call moves for which INTEGRATION
//! STYLE, read on ONE function that both styles address, exactly as a client reads it:
//! `GetMetricStatistics` and `ListMetrics` over CloudWatch's own wire.
//!
//! RULE: the per-function `AWS/States` family — `LambdaFunctionsScheduled`,
//! `LambdaFunctionsStarted`, the count of the outcome an attempt reached, and the three spans
//! measured either side of its work — is published under the `LambdaFunctionArn` dimension for
//! exactly the `Task` attempts whose OWN `Resource` named a Lambda function ARN, whatever the
//! call then came to; a `Task` whose `Resource` is one of this service's optimized integration
//! tokens names no function at all, so it publishes none of that family for the function its
//! `Parameters` address, allowed and refused alike. Lambda's own `AWS/Lambda` `Invocations`
//! series under `FunctionName` counts a disjoint thing: exactly the invocations LAMBDA ADMITTED,
//! whoever made them — a task of either style whose role allows the call, and a client's own
//! `Invoke` — and never one no document allowed. So the two accountings answer different
//! questions about the very same function, and neither leaks into the other.
//!
//! What this suite pins that no sibling does: ONE function is addressed by both integration
//! styles, under both a granting and a bare role, and by a direct `Invoke` through one of its
//! ALIASES — so the `LambdaFunctionArn` series a client charts carries the direct-ARN tasks'
//! attempts and nothing else, even though six other invocation attempts named that very
//! function, and `AWS/Lambda` `Invocations` for it carries every admitted call of either style
//! plus the client's own. Reading both families on one function is what makes the two
//! impossible to confuse: a per-style function, which is how
//! `sfn_lambda_task_series_discovery` and `sfn_lambda_task_metric_attribution` are laid out,
//! cannot show that the optimized traffic does not contaminate a series the direct-ARN traffic
//! DOES populate.
//!
//! Four crates have to agree for that to hold — Step Functions decides and makes the invocation
//! and accounts its own families, IAM holds the document the invocation is judged against,
//! Lambda runs the handler and accounts its own series, and CloudWatch answers both reads — so
//! the suite lives here rather than in any one of them. The function is declared as a
//! CloudFormation stack's inline source, which is how a handler written out in a suite comes to
//! be a deployment package at all.
//!
//! The rows differ along what the rule generalises over: the form the `Task` names its work in
//! (the function's own ARN, and the optimized integration token), whether the state machine's
//! role may make the call at all, and how many executions of the row ran — and beside them
//! stands an invocation that came from no task at all.

use std::collections::BTreeSet;
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

/// The Region every request below is signed for, and so the Region the function, the roles and
/// the state machines belong to.
const REGION: &str = "us-east-1";

/// The namespace Step Functions publishes its own series to, the two dimensions those series are
/// reported per, and the names the AWS Step Functions metric reference gives the Lambda-task
/// counts, the three spans and the execution counts. Written as literals here rather than read
/// from the Step Functions crate, so a series published under a name no AWS reader would ask for
/// fails here.
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
const LAMBDA_FUNCTIONS_TIMED_OUT: &str = "LambdaFunctionsTimedOut";
/// See [`STATES_NAMESPACE`].
const LAMBDA_FUNCTION_SCHEDULE_TIME: &str = "LambdaFunctionScheduleTime";
/// See [`STATES_NAMESPACE`].
const LAMBDA_FUNCTION_RUN_TIME: &str = "LambdaFunctionRunTime";
/// See [`STATES_NAMESPACE`].
const LAMBDA_FUNCTION_TIME: &str = "LambdaFunctionTime";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_STARTED: &str = "ExecutionsStarted";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_SUCCEEDED: &str = "ExecutionsSucceeded";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_FAILED: &str = "ExecutionsFailed";
/// See [`STATES_NAMESPACE`].
const EXECUTION_TIME: &str = "ExecutionTime";

/// Every series of the per-function family, which is the set each reading below is taken over:
/// the five counts an attempt is scheduled, started and closed in, and the three spans measured
/// either side of its work.
const PER_FUNCTION_FAMILY: &[&str] = &[
    LAMBDA_FUNCTIONS_SCHEDULED,
    LAMBDA_FUNCTIONS_STARTED,
    LAMBDA_FUNCTIONS_SUCCEEDED,
    LAMBDA_FUNCTIONS_FAILED,
    LAMBDA_FUNCTIONS_TIMED_OUT,
    LAMBDA_FUNCTION_SCHEDULE_TIME,
    LAMBDA_FUNCTION_RUN_TIME,
    LAMBDA_FUNCTION_TIME,
];

/// The namespace Lambda publishes its own series to, the dimension they are reported per, and the
/// name the AWS Lambda metric reference gives the invocation count. Literals, for the same reason
/// as above.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
/// See [`LAMBDA_NAMESPACE`].
const FUNCTION_NAME_DIMENSION: &str = "FunctionName";
/// See [`LAMBDA_NAMESPACE`].
const INVOCATIONS: &str = "Invocations";

/// The `Resource` of the OPTIMIZED Lambda integration: one of this service's own integration
/// tokens, which names no function at all — the request its `Parameters` build does.
const OPTIMIZED_RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// The `Period`, in seconds, every read below asks for: the one-minute granularity these series
/// are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of now a read's window reaches, in seconds — wide enough that the minute
/// an observation was published in is inside it however the run straddles a minute boundary.
const WINDOW_SECS: f64 = 1800.0;

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`: "Active"
/// once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The `StackStatus` a deployed stack whose resources are all up reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The terminal statuses an execution reaches, and the one it sits in while its task runs.
const SUCCEEDED: &str = "SUCCEEDED";
/// See [`SUCCEEDED`].
const FAILED: &str = "FAILED";
/// See [`SUCCEEDED`].
const RUNNING: &str = "RUNNING";

/// The runtime the handler below is written for, and the module the archived inline source lands
/// in, which the handler name resolves against.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "index.handler";

/// The one handler every row and the client's own invocation reach: it echoes what the event asks
/// it to echo, so an admitted call's answer is that call's own.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    return {"echo": event.get("echo", 0)}
"#;

/// The `Timeout`, in seconds, the function declares: well past what a run needs on any host, so
/// nothing but the handler ends one of its runs.
const AMPLE_TIMEOUT_SECS: u64 = 300;

/// The IAM action an invocation is subject to, which is the one Lambda PUBLISHES for `Invoke`
/// rather than the operation's own name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// An action that has nothing to do with an invocation, so the bare role still has a document
/// saying something.
const LOG_ACTION: &str = "logs:PutLogEvents";

/// The error a `Task` state reports when the role may not make the call: Lambda's base SDK
/// exception, which is what a refusal `Invoke` does not model arrives under.
const REFUSED_ERROR: &str = "Lambda.AWSLambdaException";

/// The service a state machine's execution role is assumed by, and the one a function's own
/// execution role is.
const STATES_PRINCIPAL: &str = "states.amazonaws.com";
/// See [`STATES_PRINCIPAL`].
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// The alias the client's own warm-up invocation addresses, and the version it points at: a
/// second ARN of the same function, so the warm-up is an invocation of the function made through
/// no task at all.
const ALIAS_NAME: &str = "warm";
/// See [`ALIAS_NAME`].
const ALIAS_VERSION: &str = "$LATEST";

/// The `echo` the client's own warm-up invocation asks the handler for, so its answer is
/// distinguishable from any task's.
const WARM_UP_ECHO: u64 = 97;

/// How often to look while waiting for work the clock owes — a deployed function reaching
/// [`FUNCTION_ACTIVE`], an execution leaving [`RUNNING`] — and how many looks to take. The spans
/// are those services' own, so no fixed sleep names the instant one ends, and the deadline is
/// generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(50);
/// See [`LOOK`].
const LOOKS: usize = 2_400;

/// The emulator as it is ASSEMBLED, on one engine, so the invocation a task makes, the documents
/// it is judged against and every series published for it are all served by the same mounted
/// services a client reaches.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes the
/// request.
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

/// A role `service` may assume, carrying `statements` as its inline policy; answers its ARN.
///
/// A state machine's execution role is trusted by Step Functions and a function's own execution
/// role by Lambda — which `CreateFunction` checks — so the trusted service is the caller's.
async fn given_role(
    router: &MultiServiceRouter,
    name: &str,
    service: &str,
    statements: &Value,
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
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
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

/// Deploy the one function every row addresses as a stack's inline source — which is what archives
/// the handler into a deployment package — and answer the ARN Lambda reports for it once it is
/// [`FUNCTION_ACTIVE`], so an invocation of it is one Lambda admits.
async fn given_function(
    router: &MultiServiceRouter,
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
        "the function's stack must reach {CREATE_COMPLETE}, last saw {answer}"
    );
    active_function(router, name).await
}

/// The ARN Lambda reports for `name` once it has reached [`FUNCTION_ACTIVE`].
async fn active_function(router: &MultiServiceRouter, name: &str) -> String {
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
            "GetFunctionConfiguration must resolve the deployed function {name}, got {status} \
             with {answered}"
        );
        configuration = answered;
        if configuration.get("State").and_then(Value::as_str) == Some(FUNCTION_ACTIVE) {
            return string(&configuration, "FunctionArn");
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("the function {name} must reach {FUNCTION_ACTIVE}, last saw {configuration}");
}

/// Give `name` the alias [`ALIAS_NAME`]; answers the alias's own ARN, which is the second ARN the
/// same function is addressed by.
async fn given_alias(router: &MultiServiceRouter, name: &str, function_arn: &str) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/aliases"),
        &json!({ "Name": ALIAS_NAME, "FunctionVersion": ALIAS_VERSION }),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreateAlias on {name} must answer 201, got {status} with {created}"
    );
    let alias_arn = string(&created, "AliasArn");
    assert_eq!(
        alias_arn,
        format!("{function_arn}:{ALIAS_NAME}"),
        "the alias must be named under the function's own ARN, got {created}"
    );
    alias_arn
}

/// Create `definition` as a state machine under `role_arn`; answers its ARN.
async fn given_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    definition: &Value,
    role_arn: &str,
) -> String {
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

/// Every series name of `namespace` a client DISCOVERS under the dimension set `dimensions`, paged
/// through to the end because the answer a client acts on is the whole enumeration ("use the
/// returned token with subsequent calls", `ListMetrics`).
async fn discovered(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    dimensions: &Value,
) -> BTreeSet<String> {
    let mut found = BTreeSet::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({ "Namespace": namespace, "Dimensions": dimensions });
        if let Some(next) = token.as_deref() {
            request["NextToken"] = json!(next);
        }
        let answer = cloudwatch(router, case, "ListMetrics", &request).await;
        for metric in answer["Metrics"].as_array().into_iter().flatten() {
            assert_eq!(
                metric.get("Dimensions"),
                Some(dimensions),
                "case {case:?}: expected {namespace} to list a series under {dimensions} alone, \
                 got {metric}"
            );
            found.insert(string(metric, "MetricName"));
        }
        token = answer
            .get("NextToken")
            .and_then(Value::as_str)
            .map(str::to_string);
        if token.is_none() {
            return found;
        }
    }
}

/// Every datapoint `GetMetricStatistics` answers for one series over a window the whole run falls
/// inside. A series nothing was accounted in answers none, which is what "that series does not
/// exist for this resource" reads as.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    dimensions: &Value,
) -> Vec<Value> {
    let now = now_secs();
    let answer = cloudwatch(
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
        }),
    )
    .await;
    answer["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// The `Sum` of a series over the window: the magnitude a client charts or alarms on.
async fn summed(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    dimensions: &Value,
) -> f64 {
    datapoints(router, case, namespace, metric, dimensions)
        .await
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}

/// How many SAMPLES a series holds over the window: one per observation published, which for a
/// span is one per attempt that closed.
async fn sampled(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    dimensions: &Value,
) -> f64 {
    datapoints(router, case, namespace, metric, dimensions)
        .await
        .iter()
        .filter_map(|point| point["SampleCount"].as_f64())
        .sum()
}

/// The dimension set the per-function `AWS/States` series are reported under: the function's ARN
/// alone.
fn function_arn_dimensions(function_arn: &str) -> Value {
    json!([{ "Name": LAMBDA_FUNCTION_ARN_DIMENSION, "Value": function_arn }])
}

/// The dimension set the per-execution `AWS/States` series are reported under: the state machine's
/// ARN alone.
fn state_machine_dimensions(machine_arn: &str) -> Value {
    json!([{ "Name": STATE_MACHINE_ARN_DIMENSION, "Value": machine_arn }])
}

/// The dimension set Lambda's own series are reported under.
fn function_name_dimensions(name: &str) -> Value {
    json!([{ "Name": FUNCTION_NAME_DIMENSION, "Value": name }])
}

/// How a `Task` state names the work it takes on a Lambda function.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// The function's own ARN as the state's `Resource`, which NAMES the function.
    FunctionArn,
    /// The optimized integration token as the state's `Resource`, which names no function — the
    /// request its `Parameters` build does.
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
}

/// One row: the form its `Task` state names its work in, whether its state machine's role may make
/// the call, and how many executions of it the client ran.
struct Row {
    case: &'static str,
    form: Form,
    granted: bool,
    executions: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "the optimized integration under a role granted the invocation, run three times",
        form: Form::Optimized,
        granted: true,
        executions: 3,
    },
    Row {
        case: "the optimized integration under a role without the grant, run three times",
        form: Form::Optimized,
        granted: false,
        executions: 3,
    },
    Row {
        case: "the function's own ARN under a role granted the invocation, run three times",
        form: Form::FunctionArn,
        granted: true,
        executions: 3,
    },
    Row {
        case: "the function's own ARN under a role without the grant, run twice",
        form: Form::FunctionArn,
        granted: false,
        executions: 2,
    },
];

/// How many executions of the rows satisfying `keep` ran, as the magnitude a series is read
/// against.
#[allow(clippy::cast_precision_loss)]
fn executions_where(keep: impl Fn(&Row) -> bool) -> f64 {
    ROWS.iter()
        .filter(|row| keep(row))
        .map(|row| row.executions)
        .sum::<usize>() as f64
}

/// The per-execution family a row's own state machine publishes: every execution is started, ran
/// for a span, and closed in the one series its outcome names.
fn expected_per_execution(granted: bool) -> BTreeSet<String> {
    [
        EXECUTIONS_STARTED,
        EXECUTION_TIME,
        if granted {
            EXECUTIONS_SUCCEEDED
        } else {
            EXECUTIONS_FAILED
        },
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// A client reading ONE function's two accountings can tell the integration styles apart: the
/// `AWS/States` series under that function's ARN carries the direct-ARN tasks' attempts and
/// nothing the optimized tasks did, while Lambda's own invocation count for the same function
/// carries every call it admitted of either style — and a refused call is in neither.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn one_functions_two_accountings_tell_the_integration_styles_apart() {
    let router = build_router();
    let setup = "the shared function and its alias";

    let function_role = given_role(
        &router,
        "xsvc-style-lambda-role",
        LAMBDA_PRINCIPAL,
        &json!([{ "Effect": "Allow", "Action": LOG_ACTION, "Resource": "*" }]),
    )
    .await;
    let name = "xsvc-style-shared";
    let function = given_function(&router, "xsvc-style-stack", name, &function_role).await;
    let alias_arn = given_alias(&router, name, &function).await;

    let per_function = function_arn_dimensions(&function);
    let per_name = function_name_dimensions(name);

    // An invocation that came from NO task at all: a client's own `Invoke` through the alias. It is
    // an invocation Lambda admitted, so Lambda counts it — and it is no `Task` attempt, so the
    // `AWS/States` per-function family stays empty for it.
    let (status, answered) = lambda(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{alias_arn}/invocations"),
        &json!({ "echo": WARM_UP_ECHO }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {setup:?}: the warm-up invocation of {alias_arn} must be served, got {status} with \
         {answered}"
    );
    assert_eq!(
        answered.get("echo").and_then(Value::as_u64),
        Some(WARM_UP_ECHO),
        "case {setup:?}: the warm-up must answer the handler's own echo, got {answered}"
    );
    for metric in PER_FUNCTION_FAMILY {
        let held = datapoints(&router, setup, STATES_NAMESPACE, metric, &per_function).await;
        assert!(
            held.is_empty(),
            "case {setup:?}: an invocation made through no task publishes no \
             {STATES_NAMESPACE}/{metric} for {function}, got {held:?}"
        );
    }
    let invoked = summed(&router, setup, LAMBDA_NAMESPACE, INVOCATIONS, &per_name).await;
    assert_eq!(
        invoked, 1.0,
        "case {setup:?}: expected the warm-up alone in {LAMBDA_NAMESPACE}/{INVOCATIONS} for \
         {name}, got {invoked}"
    );

    // Now every row's executions, against that same function.
    for (index, row) in ROWS.iter().enumerate() {
        let Row {
            case,
            form,
            granted,
            executions,
        } = *row;
        // The execution role carries the grant the invocation is judged against, over the
        // function's own ARN and its qualified forms; a row without the grant states an action
        // that has nothing to do with an invocation.
        let statements = if granted {
            json!([
                { "Effect": "Allow", "Action": INVOKE_ACTION, "Resource": function },
                { "Effect": "Allow", "Action": INVOKE_ACTION, "Resource": format!("{function}:*") },
            ])
        } else {
            json!([{ "Effect": "Allow", "Action": LOG_ACTION, "Resource": "*" }])
        };
        let machine_role = given_role(
            &router,
            &format!("xsvc-style-states-role-{index}"),
            STATES_PRINCIPAL,
            &statements,
        )
        .await;
        let machine = given_machine(
            &router,
            case,
            &format!("xsvc-style-{index}"),
            &form.definition(&function),
            &machine_role,
        )
        .await;

        for run in 0..executions {
            let described = ran(
                &router,
                case,
                &machine,
                &format!("run-{run}"),
                &json!({ "echo": run + 1 }),
            )
            .await;
            let status = described.get("status").and_then(Value::as_str);
            if granted {
                assert_eq!(
                    status,
                    Some(SUCCEEDED),
                    "case {case:?} run {run}: expected {SUCCEEDED}, got {described}"
                );
                let reported = string(&described, "output");
                assert!(
                    reported.contains(&format!("\"echo\":{}", run + 1)),
                    "case {case:?} run {run}: expected the handler's own echo in the output, got \
                     {reported:?}"
                );
            } else {
                assert_eq!(
                    (status, described.get("error").and_then(Value::as_str)),
                    (Some(FAILED), Some(REFUSED_ERROR)),
                    "case {case:?} run {run}: expected the refusal, got {described}"
                );
                let cause = string(&described, "cause");
                assert!(
                    cause.contains(INVOKE_ACTION),
                    "case {case:?} run {run}: expected the cause to name {INVOKE_ACTION}, got \
                     {cause:?}"
                );
            }
        }

        // Each row's own state machine is accounted for its executions whatever form its state's
        // `Resource` took — which is what makes the per-function family's silence a statement
        // about the RESOURCE rather than about the run having happened.
        #[allow(clippy::cast_precision_loss)]
        let ran_count = executions as f64;
        let per_machine = state_machine_dimensions(&machine);
        let expected = expected_per_execution(granted);
        let found = discovered(&router, case, STATES_NAMESPACE, &per_machine).await;
        assert_eq!(
            found, expected,
            "case {case:?}: expected {STATES_NAMESPACE} to list {expected:?} for {machine}, got \
             {found:?}"
        );
        for metric in &expected {
            let samples = sampled(&router, case, STATES_NAMESPACE, metric, &per_machine).await;
            assert_eq!(
                samples, ran_count,
                "case {case:?}: expected {ran_count} sample(s) of {metric} for {machine}, got \
                 {samples}"
            );
        }
    }

    // THE READING THE WHOLE SUITE IS FOR. The per-function family under the function's ARN counts
    // the attempts of the rows whose `Resource` NAMED it, and only those: scheduled and started
    // once per such attempt whatever it came to, closed in the series of the outcome it reached,
    // and one sample of each span per attempt. Every optimized attempt against the same function
    // — allowed and refused alike — is absent from all of it.
    let reading = "the function's own AWS/States series";
    let named = executions_where(|row| row.form == Form::FunctionArn);
    let named_and_admitted = executions_where(|row| row.form == Form::FunctionArn && row.granted);
    let named_and_refused = executions_where(|row| row.form == Form::FunctionArn && !row.granted);
    for (metric, expected) in [
        (LAMBDA_FUNCTIONS_SCHEDULED, named),
        (LAMBDA_FUNCTIONS_STARTED, named),
        (LAMBDA_FUNCTIONS_SUCCEEDED, named_and_admitted),
        (LAMBDA_FUNCTIONS_FAILED, named_and_refused),
    ] {
        let counted = summed(&router, reading, STATES_NAMESPACE, metric, &per_function).await;
        assert_eq!(
            counted, expected,
            "case {reading:?}: expected {metric} to count {expected} for {function}, got {counted}"
        );
    }
    for metric in [
        LAMBDA_FUNCTION_SCHEDULE_TIME,
        LAMBDA_FUNCTION_RUN_TIME,
        LAMBDA_FUNCTION_TIME,
    ] {
        let samples = sampled(&router, reading, STATES_NAMESPACE, metric, &per_function).await;
        assert_eq!(
            samples, named,
            "case {reading:?}: expected {named} sample(s) of {metric} for {function}, got {samples}"
        );
    }
    // Nothing timed out, so that series of the family does not exist for this function at all —
    // which is how a client tells "no attempt timed out" from "the family is not published".
    let timed_out = datapoints(
        &router,
        reading,
        STATES_NAMESPACE,
        LAMBDA_FUNCTIONS_TIMED_OUT,
        &per_function,
    )
    .await;
    assert!(
        timed_out.is_empty(),
        "case {reading:?}: no attempt timed out, so {LAMBDA_FUNCTIONS_TIMED_OUT} must answer no \
         datapoint for {function}, got {timed_out:?}"
    );
    // And exactly that set is what a client DISCOVERS under the function's ARN: the family the
    // direct-ARN rows published, under that dimension set alone.
    let expected: BTreeSet<String> = [
        LAMBDA_FUNCTIONS_SCHEDULED,
        LAMBDA_FUNCTIONS_STARTED,
        LAMBDA_FUNCTIONS_SUCCEEDED,
        LAMBDA_FUNCTIONS_FAILED,
        LAMBDA_FUNCTION_SCHEDULE_TIME,
        LAMBDA_FUNCTION_RUN_TIME,
        LAMBDA_FUNCTION_TIME,
    ]
    .into_iter()
    .map(str::to_string)
    .collect();
    let found = discovered(&router, reading, STATES_NAMESPACE, &per_function).await;
    assert_eq!(
        found, expected,
        "case {reading:?}: expected {STATES_NAMESPACE} to list {expected:?} for {function}, got \
         {found:?}"
    );

    // Lambda's own count is the OTHER question: every invocation it admitted, of either style,
    // plus the client's own — and no refused one, whichever style asked for it.
    let reading = "the function's own AWS/Lambda series";
    let admitted = 1.0 + executions_where(|row| row.granted);
    let invoked = summed(&router, reading, LAMBDA_NAMESPACE, INVOCATIONS, &per_name).await;
    assert_eq!(
        invoked, admitted,
        "case {reading:?}: expected {admitted} invocation(s) of {name} in \
         {LAMBDA_NAMESPACE}/{INVOCATIONS}, got {invoked}"
    );
    let samples = sampled(&router, reading, LAMBDA_NAMESPACE, INVOCATIONS, &per_name).await;
    assert_eq!(
        samples, admitted,
        "case {reading:?}: expected {admitted} {INVOCATIONS} sample(s) of {name}, got {samples}"
    );
}
