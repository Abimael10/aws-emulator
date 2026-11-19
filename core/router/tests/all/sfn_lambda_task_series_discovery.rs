// SPDX-License-Identifier: CC0-1.0

//! WHICH `AWS/States` series a Step Functions Lambda `Task` publishes, read the two ways a
//! client comes to chart one: CloudWatch's own discovery call (`ListMetrics`) and the read
//! call it then polls (`GetMetricStatistics`).
//!
//! RULE: the per-function `AWS/States` family — the counts an attempt is scheduled, started
//! and closed in, and the three spans measured either side of its work — EXISTS under the
//! `LambdaFunctionArn` dimension for exactly the invocations a `Task` state's own `Resource`
//! NAMED, so a state whose `Resource` is one of this service's integration tokens names no
//! function and publishes no series of that family at all, however many times its call runs;
//! the per-execution family exists under `StateMachineArn` for every execution of either
//! form, in the series its own outcome names; and the invoked function's `AWS/Lambda`
//! `Invocations` series exists for exactly the invocations LAMBDA admitted. Discovery and
//! reading are two views of the same store and cannot disagree: a series `ListMetrics` lists
//! answers `GetMetricStatistics` with datapoints whose magnitude is the work that was
//! accounted, and a series it does not list answers no datapoint rather than refusing.
//!
//! Four crates have to agree for that to hold — Step Functions decides and makes the
//! invocation and accounts both of its own families, IAM holds the document the invocation is
//! judged against, Lambda runs the handler and accounts its own series, and CloudWatch answers
//! both of its reads — so the suite lives here rather than in any one of them. The functions
//! are declared as one CloudFormation stack's inline source, which is how a handler written out
//! in a suite comes to be a deployment package at all.
//!
//! The rows differ along what the rule generalises over: the form the `Task` names its work in
//! (the function's own ARN, and the optimized integration token on the very same kind of
//! function), whether the state machine's role may make the call at all, and how many
//! executions of the row ran in the window — which is the magnitude every series listed for
//! that row is read against.

use std::collections::{BTreeMap, BTreeSet};
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

/// The Region every request below is signed for, and so the Region the functions, the roles
/// and the state machines belong to.
const REGION: &str = "us-east-1";

/// The namespace Step Functions publishes its own series to, the two dimensions those series
/// are reported per, and the names the AWS Step Functions metric reference gives the
/// Lambda-task counts, the three spans and the execution counts. Written as literals here
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
const EXECUTIONS_ABORTED: &str = "ExecutionsAborted";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_TIMED_OUT: &str = "ExecutionsTimedOut";
/// See [`STATES_NAMESPACE`].
const EXECUTION_TIME: &str = "ExecutionTime";

/// Every series of the PER-FUNCTION family, which is the set each row's reading is taken over:
/// the five counts an attempt is scheduled, started and closed in, and the three spans
/// measured either side of its work.
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

/// Every series of the PER-EXECUTION family: the counts an execution is started and closed in,
/// and the span it ran for.
const PER_EXECUTION_FAMILY: &[&str] = &[
    EXECUTIONS_STARTED,
    EXECUTIONS_SUCCEEDED,
    EXECUTIONS_FAILED,
    EXECUTIONS_ABORTED,
    EXECUTIONS_TIMED_OUT,
    EXECUTION_TIME,
];

/// The namespace Lambda publishes its own series to, the dimension they are reported per, and
/// the name the AWS Lambda metric reference gives the invocation count. Literals, for the same
/// reason as above.
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
/// an observation was published in is inside it however a row straddles a minute boundary.
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

/// The `HistoryEventType` values a `Task` state whose `Resource` NAMED a Lambda function records
/// its attempt under — all four modelled
/// (`smithy_query(query="shape",shape="HistoryEventType")` on the Step Functions model) — which
/// is the record the per-function series are accounted from.
const EVENT_SCHEDULED: &str = "LambdaFunctionScheduled";
/// See [`EVENT_SCHEDULED`].
const EVENT_STARTED: &str = "LambdaFunctionStarted";
/// See [`EVENT_SCHEDULED`].
const EVENT_SUCCEEDED: &str = "LambdaFunctionSucceeded";
/// See [`EVENT_SCHEDULED`].
const EVENT_FAILED: &str = "LambdaFunctionFailed";

/// The runtime the handler below is written for, and the module the archived inline source lands
/// in, which the handler name resolves against.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "index.handler";

/// The one handler every row invokes: it echoes what the event asks it to echo, so an allowed
/// attempt's answer is the run's own and one deployment package serves every row.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    return {"echo": event.get("echo", 0)}
"#;

/// The `Timeout`, in seconds, the functions declare: well past what a run needs on any host, so
/// nothing but the handler ends one of its runs.
const AMPLE_TIMEOUT_SECS: u64 = 300;

/// The IAM action an invocation is subject to, which is the one Lambda PUBLISHES for `Invoke`
/// rather than the operation's own name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// An action that has nothing to do with an invocation, so a row without the grant still has a
/// document saying something.
const LOG_ACTION: &str = "logs:PutLogEvents";

/// The error a `Task` state reports when the role may not make the call: Lambda's base SDK
/// exception, which is what a refusal `Invoke` does not model arrives under.
const REFUSED_ERROR: &str = "Lambda.AWSLambdaException";

/// The service a state machine's execution role is assumed by, and the one a function's own
/// execution role is.
const STATES_PRINCIPAL: &str = "states.amazonaws.com";
/// See [`STATES_PRINCIPAL`].
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

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
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request.
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

/// Deploy every row's function as ONE stack's inline source — which is what archives the handler
/// into a deployment package — wait until Lambda reports each [`FUNCTION_ACTIVE`], and answer
/// their ARNs in the order their names were given.
///
/// One stack for all of them because the functions are independent of each other: declared
/// together they come up together, rather than one span of provisioning per row.
async fn given_functions(
    router: &MultiServiceRouter,
    stack: &str,
    names: &[String],
    role_arn: &str,
) -> Vec<String> {
    let mut resources = serde_json::Map::new();
    for (index, name) in names.iter().enumerate() {
        resources.insert(
            format!("Echoing{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": name,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": AMPLE_TIMEOUT_SECS,
                    "Code": { "ZipFile": HANDLER_SOURCE },
                },
            }),
        );
    }
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
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
        "the functions' stack must reach {CREATE_COMPLETE}, last saw {answer}"
    );

    let mut arns = Vec::with_capacity(names.len());
    for name in names {
        arns.push(active_function(router, name).await);
    }
    arns
}

/// The ARN Lambda reports for `name` once it has reached [`FUNCTION_ACTIVE`], so an invocation of
/// it is one Lambda admits.
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

/// Every series of `namespace` a client DISCOVERS under the dimension set `dimensions`, as
/// `metric name -> the dimension sets it is listed under`.
///
/// Paged through to the end, because the answer a client acts on is the whole enumeration
/// ("use the returned token with subsequent calls", `ListMetrics`).
async fn listed_series(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    dimensions: &Value,
) -> BTreeMap<String, Vec<Value>> {
    let mut found: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut token: Option<String> = None;
    loop {
        let mut request = json!({ "Namespace": namespace, "Dimensions": dimensions });
        if let Some(next) = token.as_deref() {
            request["NextToken"] = json!(next);
        }
        let answer = cloudwatch(router, case, "ListMetrics", &request).await;
        for metric in answer["Metrics"].as_array().into_iter().flatten() {
            assert_eq!(
                metric.get("Namespace").and_then(Value::as_str),
                Some(namespace),
                "case {case:?}: a listing of {namespace} must report that namespace, got {metric}"
            );
            found
                .entry(string(metric, "MetricName"))
                .or_default()
                .push(metric.get("Dimensions").cloned().unwrap_or(Value::Null));
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

/// The names of the series discovered under `dimensions`, each asserted to be listed under THAT
/// dimension set alone — which is what "this family is reported per that one resource" means to
/// a client reading the listing.
async fn discovered(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    dimensions: &Value,
) -> BTreeSet<String> {
    let listed = listed_series(router, case, namespace, dimensions).await;
    for (metric, sets) in &listed {
        assert_eq!(
            sets.as_slice(),
            std::slice::from_ref(dimensions),
            "case {case:?}: expected {namespace}/{metric} to be listed under {dimensions} alone, \
             got {sets:?}"
        );
    }
    listed.into_keys().collect()
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
/// span is one per attempt or execution that closed.
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

/// The dimension set the per-execution `AWS/States` series are reported under: the state
/// machine's ARN alone.
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
    /// The `Resource` is the function's own ARN, so the state NAMES the function.
    FunctionArn,
    /// The `Resource` is the optimized integration token, so the state names no function and
    /// the request its `Parameters` build carries the one to call.
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

/// One row: the form its `Task` state names its work in, whether its state machine's role may
/// make the call, and how many executions of it the client ran.
struct Row {
    case: &'static str,
    form: Form,
    granted: bool,
    executions: usize,
}

const ROWS: &[Row] = &[
    Row {
        case: "the function's own ARN under a role granted the invocation, run twice",
        form: Form::FunctionArn,
        granted: true,
        executions: 2,
    },
    Row {
        case: "the function's own ARN under a role without the grant, run once",
        form: Form::FunctionArn,
        granted: false,
        executions: 1,
    },
    Row {
        case: "the optimized integration under a role granted the invocation, run twice",
        form: Form::Optimized,
        granted: true,
        executions: 2,
    },
    Row {
        case: "the optimized integration under a role without the grant, run once",
        form: Form::Optimized,
        granted: false,
        executions: 1,
    },
];

/// The series of the per-function family that EXIST for a row's function: a `Resource` that named
/// the function publishes the two counts every attempt reports, the count of the outcome its
/// attempts reached and the three spans, while the optimized token names no function and
/// publishes none of them.
///
/// The two counts every attempt reports are SCHEDULED and STARTED, whether or not the call was
/// admitted, because an attempt is recorded as both before anything about the call's outcome is
/// known: the history of a refused attempt carries `LambdaFunctionScheduled`,
/// `LambdaFunctionStarted` and then `LambdaFunctionFailed` — three of the modelled
/// `HistoryEventType` values
/// (`smithy_query(query="shape",shape="HistoryEventType")` on the Step Functions model) — so a
/// refused attempt is a started attempt that failed, and the spans measured between those events
/// are sampled once for it too. That is the same rule
/// `services/service-com-amazonaws-sfn-awsstepfunctions/tests/all/refused_invocation_series.rs`
/// reads the magnitudes of; here it decides which series a client can DISCOVER.
fn expected_per_function(form: Form, granted: bool) -> BTreeSet<String> {
    if form == Form::Optimized {
        return BTreeSet::new();
    }
    [
        LAMBDA_FUNCTIONS_SCHEDULED,
        LAMBDA_FUNCTIONS_STARTED,
        if granted {
            LAMBDA_FUNCTIONS_SUCCEEDED
        } else {
            LAMBDA_FUNCTIONS_FAILED
        },
        LAMBDA_FUNCTION_SCHEDULE_TIME,
        LAMBDA_FUNCTION_RUN_TIME,
        LAMBDA_FUNCTION_TIME,
    ]
    .into_iter()
    .map(str::to_string)
    .collect()
}

/// The series of the per-execution family that EXIST for a row's state machine: every execution
/// is started, ran for a span, and closed in the one series its outcome names.
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

/// A client DISCOVERS exactly the series the work it ran published, and READS the magnitude of
/// that work from each of them: the per-function family only where a `Resource` named the
/// function, the per-execution family for every state machine, and Lambda's own invocation count
/// only for the calls it admitted.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_client_discovers_exactly_the_series_the_work_published() {
    let router = build_router();

    // One function per row, so each row reads its OWN series, all declared in one stack.
    let names: Vec<String> = (0..ROWS.len())
        .map(|index| format!("xsvc-discovered-{index}"))
        .collect();
    let function_role = given_role(
        &router,
        "xsvc-discovered-function-role",
        LAMBDA_PRINCIPAL,
        &json!([{ "Effect": "Allow", "Action": LOG_ACTION, "Resource": "*" }]),
    )
    .await;
    let functions =
        given_functions(&router, "xsvc-discovered-stack", &names, &function_role).await;

    for (index, row) in ROWS.iter().enumerate() {
        let Row {
            case,
            form,
            granted,
            executions,
        } = *row;
        let name = &names[index];
        let function = &functions[index];
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
            &format!("xsvc-discovered-states-role-{index}"),
            STATES_PRINCIPAL,
            &statements,
        )
        .await;
        let machine = given_machine(
            &router,
            case,
            &format!("xsvc-discovered-{index}"),
            &form.definition(function),
            &machine_role,
        )
        .await;

        // Run the row's executions, and check each outcome really came from the call: an allowed
        // one answers the handler's own echo, a refused one fails under Lambda's base SDK
        // exception with the permission it lacked named in the cause.
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
            // The record the per-function family is accounted FROM: a `Task` state whose
            // `Resource` named the function reports its attempt under the `LambdaFunction*`
            // history events, and reports it as scheduled and then started whatever the call
            // came to — which is why a refused attempt is counted in the scheduled, started and
            // failed series alike, and sampled once in each of the spans measured between those
            // events.
            if form == Form::FunctionArn {
                let history = sfn(
                    &router,
                    case,
                    "GetExecutionHistory",
                    &json!({ "executionArn": string(&described, "executionArn") }),
                )
                .await;
                let recorded: BTreeSet<String> = history["events"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|event| event.get("type").and_then(Value::as_str))
                    .map(str::to_string)
                    .collect();
                for event in [
                    EVENT_SCHEDULED,
                    EVENT_STARTED,
                    if granted { EVENT_SUCCEEDED } else { EVENT_FAILED },
                ] {
                    assert!(
                        recorded.contains(event),
                        "case {case:?} run {run}: expected the history to carry {event}, got \
                         {recorded:?}"
                    );
                }
            }
        }
        #[allow(clippy::cast_precision_loss)]
        let ran_count = executions as f64;

        // The PER-FUNCTION family: discovered only where the `Resource` named the function, and
        // under the function's ARN alone.
        let per_function = function_arn_dimensions(function);
        let expected = expected_per_function(form, granted);
        let found = discovered(&router, case, STATES_NAMESPACE, &per_function).await;
        assert_eq!(
            found, expected,
            "case {case:?}: expected {STATES_NAMESPACE} to list {expected:?} for {function}, got \
             {found:?}"
        );
        // Discovery and reading agree: each listed count sums to the attempts it accounts for,
        // each listed span holds one sample per attempt, and every series not listed answers no
        // datapoint at all.
        for metric in PER_FUNCTION_FAMILY {
            let held = datapoints(&router, case, STATES_NAMESPACE, metric, &per_function).await;
            if expected.contains(*metric) {
                let samples: f64 = held
                    .iter()
                    .filter_map(|point| point["SampleCount"].as_f64())
                    .sum();
                assert_eq!(
                    samples, ran_count,
                    "case {case:?}: expected {ran_count} sample(s) of {metric} for {function}, \
                     got {samples} from {held:?}"
                );
            } else {
                assert!(
                    held.is_empty(),
                    "case {case:?}: {metric} is not listed for {function}, so it must answer no \
                     datapoint, got {held:?}"
                );
            }
        }
        for metric in [
            LAMBDA_FUNCTIONS_SCHEDULED,
            LAMBDA_FUNCTIONS_STARTED,
            LAMBDA_FUNCTIONS_SUCCEEDED,
            LAMBDA_FUNCTIONS_FAILED,
        ] {
            let counted = summed(&router, case, STATES_NAMESPACE, metric, &per_function).await;
            let expected_count = if expected.contains(metric) {
                ran_count
            } else {
                0.0
            };
            assert_eq!(
                counted, expected_count,
                "case {case:?}: expected {metric} to count {expected_count} for {function}, got \
                 {counted}"
            );
        }

        // The PER-EXECUTION family: discovered for every state machine, whatever form its state's
        // `Resource` took, in the one series its executions' outcome names.
        let per_machine = state_machine_dimensions(&machine);
        let expected = expected_per_execution(granted);
        let found = discovered(&router, case, STATES_NAMESPACE, &per_machine).await;
        assert_eq!(
            found, expected,
            "case {case:?}: expected {STATES_NAMESPACE} to list {expected:?} for {machine}, got \
             {found:?}"
        );
        for metric in PER_EXECUTION_FAMILY {
            let held = datapoints(&router, case, STATES_NAMESPACE, metric, &per_machine).await;
            if expected.contains(*metric) {
                let samples: f64 = held
                    .iter()
                    .filter_map(|point| point["SampleCount"].as_f64())
                    .sum();
                assert_eq!(
                    samples, ran_count,
                    "case {case:?}: expected {ran_count} sample(s) of {metric} for {machine}, got \
                     {samples} from {held:?}"
                );
            } else {
                assert!(
                    held.is_empty(),
                    "case {case:?}: {metric} is not listed for {machine}, so it must answer no \
                     datapoint, got {held:?}"
                );
            }
        }

        // And LAMBDA's own invocation count exists for exactly the calls it ADMITTED — so the
        // optimized form, listed in no per-function `AWS/States` series, is still visible there,
        // and a refused call is visible in neither.
        let per_name = function_name_dimensions(name);
        let lambda_series = listed_series(&router, case, LAMBDA_NAMESPACE, &per_name).await;
        assert_eq!(
            lambda_series.contains_key(INVOCATIONS),
            granted,
            "case {case:?}: expected {LAMBDA_NAMESPACE}/{INVOCATIONS} to be listed for {name} = \
             {granted}, got {:?}",
            lambda_series.keys().collect::<Vec<_>>()
        );
        // Lambda reports an invocation on both of the dimension sets it belongs to — the
        // function's own and the invoked resource's — so a reader asking for the FUNCTION finds
        // the function's own set among them.
        if let Some(sets) = lambda_series.get(INVOCATIONS) {
            assert!(
                sets.contains(&per_name),
                "case {case:?}: expected {LAMBDA_NAMESPACE}/{INVOCATIONS} to be listed under \
                 {per_name} itself, got {sets:?}"
            );
        }
        let invoked = summed(&router, case, LAMBDA_NAMESPACE, INVOCATIONS, &per_name).await;
        let admitted = if granted { ran_count } else { 0.0 };
        assert_eq!(
            invoked, admitted,
            "case {case:?}: expected {admitted} invocation(s) of {name} in \
             {LAMBDA_NAMESPACE}/{INVOCATIONS}, got {invoked}"
        );
        let samples = sampled(&router, case, LAMBDA_NAMESPACE, INVOCATIONS, &per_name).await;
        assert_eq!(
            samples, admitted,
            "case {case:?}: expected {admitted} {INVOCATIONS} sample(s) of {name}, got {samples}"
        );
    }
}
