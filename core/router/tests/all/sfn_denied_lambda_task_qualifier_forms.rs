// SPDX-License-Identifier: CC0-1.0

//! WHAT a client reads about a Step Functions `Task` state's Lambda invocation the state
//! machine's EXECUTION ROLE may not make: the terminal status `DescribeExecution` serves, the
//! refusal it carries, and the `AWS/States`, `AWS/Lambda` series the attempt is accounted in.
//!
//! RULE: Step Functions decides each `Task` state's Lambda invocation against the documents in
//! force on the state machine's `roleArn` over the ARN the state's `Resource` NAMES — the
//! qualifier it carries included, so a grant written for the unqualified function does not reach
//! a request addressed to one of its aliases. An invocation no document allows is not made: the
//! task fails under Lambda's base SDK exception `Lambda.AWSLambdaException` with that service's
//! own authorization sentence, which blames the assumed-role SESSION of that role and names the
//! action and the very ARN the request addressed; the execution settles in the terminal status
//! `FAILED`, which is what a `DescribeExecution` poller reads. However it closed, the attempt is
//! accounted once as scheduled and once as started in the `AWS/States` series under the
//! `LambdaFunctionArn` dimension of the ARN the `Resource` named and once in the series its
//! outcome names, and the execution once in `ExecutionsFailed` or `ExecutionsSucceeded` under
//! `StateMachineArn`, while `AWS/Lambda` `Invocations` under `FunctionName` counts only the
//! invocations Lambda ADMITTED. The dimension of a series is the ARN the `Resource` named, so no
//! other spelling of the same function carries the attempt. And the documents are read per call:
//! a grant that reaches the addressed ARN afterwards lets the SAME state machine's next
//! execution succeed with the handler's own payload.
//!
//! Four crates have to agree for that to hold — Step Functions decides and makes the invocation
//! and accounts both of its own families, IAM holds the documents it is judged against, Lambda
//! holds the alias, runs the handler and accounts its own series, and CloudWatch answers every
//! read — so the suite lives here, driven over each service's own wire protocol through the
//! emulator as `core/router/src/lib.rs` assembles it. The functions are declared as one
//! CloudFormation stack's inline source, which is how a handler written out in a suite comes to
//! be a deployment package at all.
//!
//! The rows differ along what the rule generalises over: the form the `Task` names its work in
//! (the function's own ARN, and that function AT one of its aliases) and what the role's
//! documents say about the invocation (nothing about it, a grant on the unqualified ARN, and a
//! grant under the ARN's qualifier wildcard) — the two allowed rows being the positive controls
//! that the same series count an invocation that really ran.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// `tests/all/` is ONE crate root, so the percent-encoder of an
// `application/x-www-form-urlencoded` component, the reader of one element of an awsQuery
// answer, the wait for a deployed stack to settle and the reader of the session a refusal blames
// are borrowed rather than copied.
use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{form_encode, opt_element};
use crate::sfn_task_target_authorization::session_principal;

/// The Region every request below is signed for, and so the Region the functions, the roles and
/// the state machines belong to.
const REGION: &str = "us-east-1";

/// The namespace Step Functions publishes its own series to, the two dimensions those series are
/// reported per, and the names the AWS Step Functions metric reference gives the Lambda-task
/// counts and the execution counts. Written as literals here rather than read from the Step
/// Functions crate, so a series published under a name no AWS reader would ask for fails here.
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
const EXECUTIONS_SUCCEEDED: &str = "ExecutionsSucceeded";
/// See [`STATES_NAMESPACE`].
const EXECUTIONS_FAILED: &str = "ExecutionsFailed";

/// The namespace Lambda publishes its own series to, the dimension they are reported per, and
/// the name the AWS Lambda metric reference gives the invocation count. Literals, for the same
/// reason as above.
const LAMBDA_NAMESPACE: &str = "AWS/Lambda";
/// See [`LAMBDA_NAMESPACE`].
const FUNCTION_NAME_DIMENSION: &str = "FunctionName";
/// See [`LAMBDA_NAMESPACE`].
const INVOCATIONS: &str = "Invocations";

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

/// The terminal statuses an execution reaches here, and the one it sits in while its task runs.
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

/// The one handler every row invokes: it echoes what the event carries, so an admitted
/// invocation's answer is the run's own and one deployment package serves every row.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    return {"echo": event["n"]}
"#;

/// The `Timeout`, in seconds, the functions declare: well past what a run needs on any host, so
/// nothing but the handler ends one of its runs.
const AMPLE_TIMEOUT_SECS: u64 = 300;

/// The alias every row's function carries, and the version it points at: the unpublished version
/// the function is addressed by with no qualifier at all, so one package serves both spellings.
const ALIAS_NAME: &str = "warm";
/// See [`ALIAS_NAME`].
const ALIAS_VERSION: &str = "$LATEST";

/// The IAM action an invocation is subject to, which is the one Lambda PUBLISHES for `Invoke`
/// rather than the operation's own name.
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// An action that has nothing to do with an invocation, so every row's role carries a document
/// that states something.
const LOG_ACTION: &str = "logs:PutLogEvents";

/// The error a `Task` state reports when the documents may not make the call: Lambda's base SDK
/// exception, which is what a refusal `Invoke` models no error of its own arrives under.
const REFUSED_ERROR: &str = "Lambda.AWSLambdaException";

/// The service a state machine's execution role is assumed by, and the one a function's own
/// execution role is — which `CreateFunction` checks.
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
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
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

/// The body of a response, parsed as JSON where it carries any.
fn parsed(body: &[u8]) -> Value {
    if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body).unwrap_or(Value::Null)
    }
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
    let answered = parsed(response.body());
    (status, answered)
}

/// One Step Functions call, requiring a 200.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = rpc(router, "states", "AWSStepFunctions", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {answered}"
    );
    answered
}

/// One CloudWatch call, requiring a 200.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, answered) = rpc(
        router,
        "monitoring",
        "GraniteServiceVersion20100801",
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
    let answered = parsed(response.body());
    (status, answered)
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

/// A statement allowing `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// A role `service` may assume, carrying `statements` as its inline policy; answers its ARN.
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
    put_role_policy(router, name, statements).await;
    arn
}

/// Write `statements` as the role's inline policy, which is also how a row GRANTS something to a
/// role that already exists.
async fn put_role_policy(router: &MultiServiceRouter, name: &str, statements: &Value) {
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
}

/// Declare every row's function as ONE stack's inline source — which is what archives the
/// handler into a deployment package — wait until Lambda reports each [`FUNCTION_ACTIVE`], give
/// each the alias [`ALIAS_NAME`], and answer their ARNs in the order their names were given.
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
        let arn = active_function(router, name).await;
        given_alias(router, name, &arn).await;
        arns.push(arn);
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

/// Give `name` the alias [`ALIAS_NAME`], which is the second ARN the same function is addressed
/// by — and the one a `Task` state's `Resource` names in the qualified rows.
async fn given_alias(router: &MultiServiceRouter, name: &str, function_arn: &str) {
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
    assert_eq!(
        created.get("AliasArn").and_then(Value::as_str),
        Some(format!("{function_arn}:{ALIAS_NAME}").as_str()),
        "the alias must be named under the function's own ARN, got {created}"
    );
}

/// Create a one-`Task` state machine whose `Resource` is `resource`, under `role_arn`; answers
/// its ARN.
async fn given_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    resource: &str,
    role_arn: &str,
) -> String {
    let definition = json!({
        "StartAt": "Invoke",
        "States": {
            "Invoke": { "Type": "Task", "Resource": resource, "End": true },
        },
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

/// The `Sum` one series reports over a window the whole run falls inside: the magnitude a client
/// charts or alarms on. A series nothing was accounted in answers no datapoint, which sums to
/// zero — what "that series holds nothing for this resource" reads as.
async fn summed(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    dimensions: &Value,
) -> f64 {
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
            "Statistics": ["Sum"],
        }),
    )
    .await;
    answer["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|point| point["Sum"].as_f64())
        .sum()
}

/// Assert one series' `Sum` over the window is `expected`.
async fn assert_summed(
    router: &MultiServiceRouter,
    case: &str,
    namespace: &str,
    metric: &str,
    dimensions: &Value,
    expected: f64,
) {
    let counted = summed(router, case, namespace, metric, dimensions).await;
    assert!(
        (counted - expected).abs() < f64::EPSILON,
        "case {case:?}: expected {namespace}/{metric} under {dimensions} to sum to {expected}, \
         got {counted}"
    );
}

/// The dimension set the per-function `AWS/States` series are reported under: the ARN the
/// `Resource` named, alone.
fn function_arn_dimensions(named_arn: &str) -> Value {
    json!([{ "Name": LAMBDA_FUNCTION_ARN_DIMENSION, "Value": named_arn }])
}

/// The dimension set the per-execution `AWS/States` series are reported under.
fn state_machine_dimensions(machine_arn: &str) -> Value {
    json!([{ "Name": STATE_MACHINE_ARN_DIMENSION, "Value": machine_arn }])
}

/// The dimension set Lambda's own series are reported under.
fn function_name_dimensions(name: &str) -> Value {
    json!([{ "Name": FUNCTION_NAME_DIMENSION, "Value": name }])
}

/// How a `Task` state's `Resource` names the function it invokes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Form {
    /// The function's own ARN, with no qualifier at all.
    FunctionArn,
    /// The same function AT one of its aliases, which is a second ARN of the same function and a
    /// resource of its own as far as a policy document is concerned.
    AliasArn,
}

impl Form {
    /// The ARN this form addresses `function_arn` by, which is the resource the invocation is
    /// judged over and the dimension its series are reported under.
    fn named(self, function_arn: &str) -> String {
        match self {
            Self::FunctionArn => function_arn.to_string(),
            Self::AliasArn => format!("{function_arn}:{ALIAS_NAME}"),
        }
    }

    /// The OTHER spelling of the same function, which named no `Resource` and so must carry none
    /// of the attempt.
    fn other(self, function_arn: &str) -> String {
        match self {
            Self::FunctionArn => Self::AliasArn.named(function_arn),
            Self::AliasArn => Self::FunctionArn.named(function_arn),
        }
    }
}

/// What the row's execution role is granted the invocation ON.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Grant {
    /// Nothing: the documents in force state nothing about the invocation at all.
    Nothing,
    /// The function's unqualified ARN alone, which reaches a request addressed to no qualifier.
    UnqualifiedArn,
    /// Every qualified spelling of the function (`<function ARN>:*`), which reaches a request
    /// addressed to one of its aliases.
    QualifierWildcard,
}

impl Grant {
    /// The statements the role's inline policy carries over `function_arn`.
    fn statements(self, function_arn: &str) -> Value {
        let mut statements = vec![allow(LOG_ACTION, "*")];
        match self {
            Self::Nothing => {}
            Self::UnqualifiedArn => statements.push(allow(INVOKE_ACTION, function_arn)),
            Self::QualifierWildcard => {
                statements.push(allow(INVOKE_ACTION, &format!("{function_arn}:*")));
            }
        }
        Value::Array(statements)
    }
}

/// One row: the form its `Task` names its work in, what its role is granted, and whether the
/// documents in force let the invocation be made.
struct Row {
    case: &'static str,
    form: Form,
    grant: Grant,
    allowed: bool,
}

const ROWS: &[Row] = &[
    Row {
        case: "the function's own ARN, under a role whose documents say nothing about the \
               invocation",
        form: Form::FunctionArn,
        grant: Grant::Nothing,
        allowed: false,
    },
    Row {
        case: "the function's own ARN, under a role granted the invocation on that ARN",
        form: Form::FunctionArn,
        grant: Grant::UnqualifiedArn,
        allowed: true,
    },
    Row {
        case: "the function at one of its aliases, under a role granted the invocation on the \
               unqualified ARN alone",
        form: Form::AliasArn,
        grant: Grant::UnqualifiedArn,
        allowed: false,
    },
    Row {
        case: "the function at one of its aliases, under a role granted the invocation under the \
               ARN's qualifier wildcard",
        form: Form::AliasArn,
        grant: Grant::QualifierWildcard,
        allowed: true,
    },
];

/// The input every row's execution carries, and the echo its handler answers for it.
const ECHOED: i64 = 7;

/// RULE: the invocation a `Task` state's `Resource` addresses is decided over the ARN that
/// `Resource` names, and a refused one settles the execution `FAILED` under
/// `Lambda.AWSLambdaException` with Lambda's authorization sentence while the attempt is still
/// accounted — as scheduled, started and failed — under that ARN's own dimension.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_tasks_invocation_is_decided_and_accounted_over_the_arn_its_resource_names() {
    let router = build_router();
    let function_role = given_role(
        &router,
        "QualifierFormsFunctionRole",
        LAMBDA_PRINCIPAL,
        &json!([allow(LOG_ACTION, "*")]),
    )
    .await;
    // One function per row, so each row's series are its own.
    let names: Vec<String> = (0..ROWS.len())
        .map(|index| format!("qualifier-forms-{index}"))
        .collect();
    let functions = given_functions(
        &router,
        "qualifier-forms-functions",
        &names,
        &function_role,
    )
    .await;

    for (index, row) in ROWS.iter().enumerate() {
        let Row {
            case,
            form,
            grant,
            allowed,
        } = *row;
        let name = &names[index];
        let function_arn = &functions[index];
        let named = form.named(function_arn);
        let role_name = format!("QualifierFormsRole{index}");
        let role_arn = given_role(
            &router,
            &role_name,
            STATES_PRINCIPAL,
            &grant.statements(function_arn),
        )
        .await;
        let machine = given_machine(&router, case, name, &named, &role_arn).await;

        let described = ran(&router, case, &machine, "only", &json!({ "n": ECHOED })).await;
        if allowed {
            assert_eq!(
                (
                    described.get("status").and_then(Value::as_str),
                    described.get("output").and_then(Value::as_str),
                ),
                (
                    Some(SUCCEEDED),
                    Some(json!({ "echo": ECHOED }).to_string().as_str()),
                ),
                "case {case:?}: expected the admitted invocation's own answer, got {described}"
            );
        } else {
            assert_eq!(
                (
                    described.get("status").and_then(Value::as_str),
                    described.get("error").and_then(Value::as_str),
                ),
                (Some(FAILED), Some(REFUSED_ERROR)),
                "case {case:?}: expected the refusal to settle the execution, got {described}"
            );
            // The sentence Lambda words for an unauthorized call: the session of the role the
            // call was made as, the action it publishes for an invocation, and the very ARN the
            // request addressed.
            let cause = string(&described, "cause");
            let principal = session_principal(&role_arn, &cause);
            let opening = format!(
                "User: {principal} is not authorized to perform: {INVOKE_ACTION} on resource: \
                 {named} "
            );
            assert!(
                cause.starts_with(&opening),
                "case {case:?}: expected the cause to open {opening:?}, got {cause:?}"
            );
        }

        // Every attempt is scheduled and started, whatever the call then turned out to do.
        for metric in [LAMBDA_FUNCTIONS_SCHEDULED, LAMBDA_FUNCTIONS_STARTED] {
            assert_summed(
                &router,
                case,
                STATES_NAMESPACE,
                metric,
                &function_arn_dimensions(&named),
                1.0,
            )
            .await;
        }
        // And it closes in the one series its outcome names.
        for (metric, expected) in [
            (LAMBDA_FUNCTIONS_SUCCEEDED, f64::from(u8::from(allowed))),
            (LAMBDA_FUNCTIONS_FAILED, f64::from(u8::from(!allowed))),
        ] {
            assert_summed(
                &router,
                case,
                STATES_NAMESPACE,
                metric,
                &function_arn_dimensions(&named),
                expected,
            )
            .await;
        }
        // The dimension is the ARN the `Resource` named, so the other spelling of the same
        // function carries none of it.
        let other = form.other(function_arn);
        for metric in [
            LAMBDA_FUNCTIONS_SCHEDULED,
            LAMBDA_FUNCTIONS_STARTED,
            LAMBDA_FUNCTIONS_SUCCEEDED,
            LAMBDA_FUNCTIONS_FAILED,
        ] {
            assert_summed(
                &router,
                case,
                STATES_NAMESPACE,
                metric,
                &function_arn_dimensions(&other),
                0.0,
            )
            .await;
        }
        // The execution itself is counted under its state machine, in the one series its own
        // outcome names.
        for (metric, expected) in [
            (EXECUTIONS_SUCCEEDED, f64::from(u8::from(allowed))),
            (EXECUTIONS_FAILED, f64::from(u8::from(!allowed))),
        ] {
            assert_summed(
                &router,
                case,
                STATES_NAMESPACE,
                metric,
                &state_machine_dimensions(&machine),
                expected,
            )
            .await;
        }
        // Lambda meters the invocations it ADMITTED, so a refused attempt leaves the function's
        // own series empty.
        assert_summed(
            &router,
            case,
            LAMBDA_NAMESPACE,
            INVOCATIONS,
            &function_name_dimensions(name),
            f64::from(u8::from(allowed)),
        )
        .await;
    }
}

/// RULE, continued: the documents are read PER CALL over the ARN the `Resource` names, so a
/// grant that reaches that qualified ARN afterwards lets the SAME state machine's next execution
/// succeed with the handler's payload — and the series count each execution's own attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn granting_the_addressed_qualifier_lets_the_same_machine_succeed_afterwards() {
    let case = "the grant reaching the alias arrives after the first execution";
    let router = build_router();
    let function_role = given_role(
        &router,
        "RegrantedAliasFunctionRole",
        LAMBDA_PRINCIPAL,
        &json!([allow(LOG_ACTION, "*")]),
    )
    .await;
    let name = "regranted-alias".to_string();
    let functions = given_functions(
        &router,
        "regranted-alias-functions",
        std::slice::from_ref(&name),
        &function_role,
    )
    .await;
    let function_arn = &functions[0];
    let alias_arn = Form::AliasArn.named(function_arn);

    // The role is granted the invocation on the unqualified ARN alone, which does not reach the
    // alias the `Task` addresses.
    let role_name = "RegrantedAliasRole";
    let role_arn = given_role(
        &router,
        role_name,
        STATES_PRINCIPAL,
        &Grant::UnqualifiedArn.statements(function_arn),
    )
    .await;
    let machine = given_machine(&router, case, &name, &alias_arn, &role_arn).await;

    let refused = ran(&router, case, &machine, "denied", &json!({ "n": ECHOED })).await;
    assert_eq!(
        (
            refused.get("status").and_then(Value::as_str),
            refused.get("error").and_then(Value::as_str),
        ),
        (Some(FAILED), Some(REFUSED_ERROR)),
        "case {case:?}: expected the first execution to be refused, got {refused}"
    );

    put_role_policy(
        &router,
        role_name,
        &Grant::QualifierWildcard.statements(function_arn),
    )
    .await;

    let granted = ran(&router, case, &machine, "granted", &json!({ "n": 9 })).await;
    assert_eq!(
        (
            granted.get("status").and_then(Value::as_str),
            granted.get("output").and_then(Value::as_str),
        ),
        (
            Some(SUCCEEDED),
            Some(json!({ "echo": 9 }).to_string().as_str()),
        ),
        "case {case:?}: expected the granted execution to answer the handler's payload, got \
         {granted}"
    );

    // Two executions, two attempts: each scheduled and started under the ARN the `Resource`
    // named, one closed as a failure and one as a success, and the one invocation Lambda
    // admitted metered once.
    for (metric, expected) in [
        (LAMBDA_FUNCTIONS_SCHEDULED, 2.0),
        (LAMBDA_FUNCTIONS_STARTED, 2.0),
        (LAMBDA_FUNCTIONS_FAILED, 1.0),
        (LAMBDA_FUNCTIONS_SUCCEEDED, 1.0),
    ] {
        assert_summed(
            &router,
            case,
            STATES_NAMESPACE,
            metric,
            &function_arn_dimensions(&alias_arn),
            expected,
        )
        .await;
    }
    for (metric, expected) in [(EXECUTIONS_FAILED, 1.0), (EXECUTIONS_SUCCEEDED, 1.0)] {
        assert_summed(
            &router,
            case,
            STATES_NAMESPACE,
            metric,
            &state_machine_dimensions(&machine),
            expected,
        )
        .await;
    }
    assert_summed(
        &router,
        case,
        LAMBDA_NAMESPACE,
        INVOCATIONS,
        &function_name_dimensions(&name),
        1.0,
    )
    .await;
}
