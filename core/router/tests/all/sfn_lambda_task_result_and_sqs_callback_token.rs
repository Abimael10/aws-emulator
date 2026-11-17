// SPDX-License-Identifier: CC0-1.0

//! What a Step Functions `Task` state's work PRODUCES, over the two forms of work whose
//! result comes from outside Step Functions itself: a Lambda function the state names, and
//! a callback the state hands out a task token for.
//!
//! RULE 1: a `Task` state whose `Resource` is a Lambda function ARN invokes that function
//! with the state's effective input as the event payload and takes what the function
//! RETURNED as the state's result, so an execution of such a definition reaches `SUCCEEDED`
//! with `output` equal to the function's returned value and reports neither `error` nor
//! `cause` (`DescribeExecution` carries `error`/`cause` only for a failure).
//!
//! RULE 2: a `Task` state whose `Resource` asks for the callback pattern over the queue
//! send integration (`arn:aws:states:::sqs:sendMessage.waitForTaskToken`) delivers exactly
//! one message to the queue its `QueueUrl` names, whose body is the rendered `MessageBody`
//! — the reserved context path `$$.Task.Token` resolved to that task's own token, each
//! `<member>.$` resolved from the state input, and every literal member as written — and
//! then HOLDS the execution `RUNNING` until that token is redeemed: a `SendTaskSuccess`
//! ends it `SUCCEEDED` with `output` equal to the output supplied, and a `SendTaskFailure`
//! ends it `FAILED` carrying the `error` and the `cause` supplied (both optional on that
//! call, so a failure naming no cause reports none).
//!
//! Five crates have to agree for either rule to hold — Step Functions interprets the
//! definition and makes the integration call, IAM holds the documents that call is judged
//! against, CloudFormation archives the inline handler into the function's deployment
//! package, Lambda runs it, and Amazon SQS takes the message and hands it back — so the
//! suite lives here: a crate-local suite can only ever see one side of it. What the
//! neighbours already pin and this one does not repeat: `sfn_lambda_task_invocation` pins
//! that the invocation HAPPENS and is metered, `sfn_lambda_task_handler_writes_objects`
//! pins what a handler can do to other services, and the Step Functions crate's
//! `callback_timeout_error_handling` pins what the callback task's own BOUND does. What
//! only this suite reaches is the RESULT each kind of work hands back: the function's
//! returned value as the execution's output, and the token-bearing message a worker
//! redeems.
//!
//! The rows differ along what the rules generalise over. For rule 1: the type and shape of
//! the value the function returns (an object carrying the event it was handed, text outside
//! ASCII, a list, a number, an empty object) and so the input each is computed from. For
//! rule 2: the JSON type of the payload the `MessageBody` resolves out of the input (text,
//! an object, a number, text outside ASCII), and how the token is redeemed (a success
//! carrying an object output, a success carrying a scalar output, a failure carrying an
//! error and a cause, a failure carrying an error alone).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The reader of one element of an awsQuery answer and the percent-encoder for an
// `application/x-www-form-urlencoded` component, plus the poll that waits for a stack to
// settle: the `all` target is one crate root, so this suite borrows all three rather than
// carrying a second copy.
use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the queue, the function
/// and the state machines belong to. The stack poll this suite borrows reads that Region,
/// so both halves speak for one deployment.
const REGION: &str = "us-east-1";

/// The terminal statuses an execution reaches, and the one it sits in while its work is
/// outstanding (`smithy_query(query="shape",shape="ExecutionStatus")`).
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const RUNNING: &str = "RUNNING";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The `StackStatus` the function's stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stack the function and its execution role are deployed by, and the function's name.
const FUNCTION_STACK: &str = "xsvc-sfn-result-stack";
const FUNCTION_NAME: &str = "xsvc-sfn-result-transformer";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the value the handler returned and not
/// how fast an interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// The bound every callback row's task declares, in seconds. Generous: no row waits it out,
/// and each redeems its token long before it runs out however loaded the host is.
const CALLBACK_TIMEOUT: u64 = 300;

/// How often to look while waiting for work the clock owes — a stack reaching
/// [`CREATE_COMPLETE`], a function reaching [`FUNCTION_ACTIVE`], an execution leaving
/// [`RUNNING`], the message a callback task owes its queue — and how many looks to take.
/// The spans are those services' own, so no fixed sleep names the instant one ends.
///
/// The deadline is ten minutes per condition because the host this suite runs on may be
/// working through the rest of the workspace's tests at the same time, where one function
/// invocation has been seen to take minutes; nothing here waits it out, so a run whose
/// conditions hold promptly takes a few seconds and only a run where one never holds
/// spends it.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 6_000;

/// The handler every row of rule 1 runs: it computes its answer from the event it was
/// handed and RETURNS it, so the value the execution reports can only have come from a run
/// of this source over that event.
const TRANSFORMING_HANDLER: &str = r#"
def handler(event, context):
    shape = event["shape"]

    if shape == "object":
        return {"doubled": event["n"] * 2, "seen": event}

    if shape == "text":
        return event["label"] + "!"

    if shape == "list":
        return sorted(event["items"])

    if shape == "number":
        return len(event["items"])

    return {}
"#;

/// The emulator as it is ASSEMBLED, on one engine: every call below is served by the same
/// store, so the message a task sent is the message this suite reads back.
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

/// One Amazon SQS call, requiring a 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "sqs", "AmazonSQS", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer 200, got {status} with {parsed}"
    );
    parsed
}

/// One awsQuery call of `service`, answered with its status and its XML body.
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
    let host = format!("{service}.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// One IAM call, requiring a 200.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "iam", "2010-05-08", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from IAM, got {status} with {body}"
    );
    body
}

/// One Lambda `restJson1` read, as its parsed body.
async fn lambda_get(router: &MultiServiceRouter, uri: &str) -> Value {
    let host = format!("lambda.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("https://{host}{uri}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let body = response.body();
    if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(body).to_string()))
    }
}

/// A role `service` may assume, carrying `statements` as its inline policy; answers its ARN.
///
/// A state machine's execution role is trusted by Step Functions, so the identity its
/// integration calls are made as is a session of this role and the statements below are
/// what decide them.
async fn given_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    service: &str,
    statements: Value,
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
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    let arn = opt_element(&created, "Arn")
        .unwrap_or_else(|| panic!("case {case:?}: CreateRole({name:?}) must answer an ARN"));
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    iam(
        router,
        case,
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

/// Deploy the function rule 1's task invokes and answer its ARN, once Lambda reports it
/// [`FUNCTION_ACTIVE`].
///
/// The handler is declared as a stack's inline source, which CloudFormation archives into
/// the deployment package — the way a function whose handler is written out in a suite comes
/// to exist at all.
async fn given_function(router: &MultiServiceRouter, case: &str, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Transformer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION_NAME,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": TRANSFORMING_HANDLER },
                },
            },
        },
    })
    .to_string();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", FUNCTION_STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStack({FUNCTION_STACK:?}) must be accepted, got {status} with \
         {answered}"
    );
    let mut reported = stack_status(router, FUNCTION_STACK).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, FUNCTION_STACK).await;
    }
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "case {case:?}: the function's stack must reach {CREATE_COMPLETE}, last saw {}",
        reported.1
    );

    let mut configuration = Value::Null;
    for _ in 0..LOOKS {
        let uri = format!("/2015-03-31/functions/{FUNCTION_NAME}/configuration");
        configuration = lambda_get(router, &uri).await;
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            let arn = configuration["FunctionArn"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: the function must report its ARN, got {configuration}")
            });
            return arn.to_string();
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the function must reach {FUNCTION_ACTIVE}, last saw {configuration}");
}

/// Create a state machine over `definition` and answer its ARN.
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
    created["stateMachineArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateStateMachine must answer an ARN"))
        .to_string()
}

/// Start one execution carrying `input` and answer its ARN.
async fn started(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    name: &str,
    input: &Value,
) -> String {
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
    started["executionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartExecution must answer an ARN, got {started}")
        })
        .to_string()
}

/// What `DescribeExecution` reports for `execution_arn` right now.
async fn described(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Value {
    sfn(
        router,
        case,
        "DescribeExecution",
        &json!({ "executionArn": execution_arn }),
    )
    .await
}

/// The execution `execution_arn` once it has left [`RUNNING`].
async fn settled(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Value {
    let mut reported = Value::Null;
    for _ in 0..LOOKS {
        reported = described(router, case, execution_arn).await;
        if reported["status"].as_str() != Some(RUNNING) {
            return reported;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the execution must leave {RUNNING}, last saw {reported}");
}

/// The messages waiting on the queue at `queue_url` right now.
async fn received(router: &MultiServiceRouter, case: &str, queue_url: &str) -> Vec<Value> {
    let answered = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({ "QueueUrl": queue_url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    answered["Messages"].as_array().cloned().unwrap_or_default()
}

/// The messages the callback task owed the queue at `queue_url`, looked for again until at
/// least one has arrived. How many arrived is the caller's own assertion.
async fn delivered(router: &MultiServiceRouter, case: &str, queue_url: &str) -> Vec<Value> {
    for _ in 0..LOOKS {
        let messages = received(router, case, queue_url).await;
        if !messages.is_empty() {
            return messages;
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the callback task must deliver a message to {queue_url:?}");
}

/// Take `message` off the queue at `queue_url` for good, so what the queue holds afterwards
/// is what else was sent to it rather than this message coming back into view.
async fn deleted(router: &MultiServiceRouter, case: &str, queue_url: &str, message: &Value) {
    let handle = message["ReceiptHandle"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected a ReceiptHandle on the message, got {message}")
    });
    sqs(
        router,
        case,
        "DeleteMessage",
        &json!({ "QueueUrl": queue_url, "ReceiptHandle": handle }),
    )
    .await;
}

/// The JSON document a member of a response carries as text — an execution's `input` or
/// `output`, a message's `Body`.
fn document(carrier: &Value, member: &str, case: &str) -> Value {
    let text = carrier[member].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected a {member} carrying a JSON document, got {carrier}")
    });
    serde_json::from_str(text).unwrap_or_else(|why| {
        panic!("case {case:?}: the {member} must be a JSON document, got {text:?} ({why})")
    })
}

/// One row of rule 1: the input the execution carries, and the value the handler returns
/// for it — which is what the execution's `output` must be.
struct Returned {
    case: &'static str,
    input: Value,
    output: Value,
}

fn returning_rows() -> Vec<Returned> {
    vec![
        Returned {
            case: "an object, carrying the whole event the function was handed",
            input: json!({ "shape": "object", "n": 21 }),
            output: json!({ "doubled": 42, "seen": { "shape": "object", "n": 21 } }),
        },
        Returned {
            case: "text outside ASCII, whose UTF-8 is longer than its characters",
            input: json!({ "shape": "text", "label": "café ☕" }),
            output: json!("café ☕!"),
        },
        Returned {
            case: "a list, in an order the input was not in",
            input: json!({ "shape": "list", "items": ["b", "a", "c"] }),
            output: json!(["a", "b", "c"]),
        },
        Returned {
            case: "a number",
            input: json!({ "shape": "number", "items": [1, 2, 3, 4] }),
            output: json!(4),
        },
        Returned {
            case: "an empty object",
            input: json!({ "shape": "empty" }),
            output: json!({}),
        },
    ]
}

/// RULE 1: the execution's `output` is what the function returned for the event the state's
/// input handed it, and a success reports no `error` or `cause`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lambda_task_reports_what_the_function_returned() {
    let case = "a one-task machine whose work is a Lambda function";
    let router = build_router();

    let function_role = given_role(
        &router,
        case,
        "xsvc-sfn-result-handler-role",
        "lambda.amazonaws.com",
        json!([{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }]),
    )
    .await;
    let function_arn = given_function(&router, case, &function_role).await;
    let machine_role = given_role(
        &router,
        case,
        "xsvc-sfn-result-machine-role",
        "states.amazonaws.com",
        json!([{
            "Effect": "Allow",
            "Action": "lambda:InvokeFunction",
            "Resource": function_arn.clone(),
        }]),
    )
    .await;

    // One state, naming the function itself: with no `Parameters` the state's effective
    // input IS the event the function is handed.
    let definition = json!({
        "StartAt": "Transform",
        "States": {
            "Transform": { "Type": "Task", "Resource": function_arn, "End": true },
        },
    });
    let machine_arn = given_machine(
        &router,
        case,
        "xsvc-sfn-result-transforming",
        &definition,
        &machine_role,
    )
    .await;

    for (index, row) in returning_rows().into_iter().enumerate() {
        let case = row.case;
        let execution_arn = started(
            &router,
            case,
            &machine_arn,
            &format!("returned-{index}"),
            &row.input,
        )
        .await;
        let reported = settled(&router, case, &execution_arn).await;
        assert_eq!(
            reported["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected {SUCCEEDED}, got {reported}"
        );
        assert_eq!(
            document(&reported, "output", case),
            row.output,
            "case {case:?}: expected the output {}, got {reported}",
            row.output
        );
        // The event the function ran on is the execution's own input, which the first row
        // reads back out of the value it returned.
        assert_eq!(
            document(&reported, "input", case),
            row.input,
            "case {case:?}: expected the input {}, got {reported}",
            row.input
        );
        assert_eq!(
            reported.get("error"),
            None,
            "case {case:?}: a succeeded execution reports no error, got {reported}"
        );
        assert_eq!(
            reported.get("cause"),
            None,
            "case {case:?}: a succeeded execution reports no cause, got {reported}"
        );
    }
}

/// How one callback row's token is redeemed.
enum Redeemed {
    /// A `SendTaskSuccess` carrying this output, which the execution must end on.
    Success(Value),
    /// A `SendTaskFailure` carrying this error and, where the row supplies one, this cause
    /// — both optional members of that call
    /// (`smithy_query(query="operation",operation="SendTaskFailure")`).
    Failure {
        error: &'static str,
        cause: Option<&'static str>,
    },
}

/// One row of rule 2: the queue the task hands its token to, the payload the rendered
/// `MessageBody` resolves out of the input, the literal member it also carries, and how the
/// token is redeemed.
struct Callback {
    case: &'static str,
    queue: &'static str,
    job: Value,
    kind: &'static str,
    redeemed: Redeemed,
}

fn callback_rows() -> Vec<Callback> {
    vec![
        Callback {
            case: "a text payload, redeemed with an object output",
            queue: "xsvc-sfn-callback-text",
            job: json!("j-token"),
            kind: "provisioning",
            redeemed: Redeemed::Success(json!({ "result": "done", "attempts": 2 })),
        },
        Callback {
            case: "an object payload, redeemed with a scalar output",
            queue: "xsvc-sfn-callback-object",
            job: json!({ "id": 7, "parts": ["a", "b"] }),
            kind: "batching",
            redeemed: Redeemed::Success(json!("approved")),
        },
        Callback {
            case: "text outside ASCII, redeemed with a failure naming an error and a cause",
            queue: "xsvc-sfn-callback-unicode",
            job: json!("ждём ☕"),
            kind: "review",
            redeemed: Redeemed::Failure {
                error: "WorkerRejected",
                cause: Some("the worker declined the job"),
            },
        },
        Callback {
            case: "a number payload, redeemed with a failure naming an error alone",
            queue: "xsvc-sfn-callback-number",
            job: json!(12),
            kind: "metering",
            redeemed: Redeemed::Failure {
                error: "WorkerUnavailable",
                cause: None,
            },
        },
    ]
}

/// RULE 2: the callback task hands its token out on exactly one message of the queue it
/// names, holds the execution `RUNNING` while the token is outstanding, and ends it on what
/// the redeeming call supplied.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_sqs_callback_task_hands_out_a_token_and_waits_for_it() {
    let case = "a one-task machine whose work is a callback send";
    let router = build_router();
    let rows = callback_rows();

    // One queue per row, so a message one row's task sent is never counted in another's,
    // and the role is granted the send on each of those queues by ARN.
    let mut queue_urls: Vec<String> = Vec::new();
    let mut grants: Vec<Value> = Vec::new();
    for row in &rows {
        let created = sqs(
            &router,
            row.case,
            "CreateQueue",
            &json!({ "QueueName": row.queue }),
        )
        .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {:?}: CreateQueue must answer a url", row.case))
            .to_string();
        let attributes = sqs(
            &router,
            row.case,
            "GetQueueAttributes",
            &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
        )
        .await;
        let arn = attributes["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: GetQueueAttributes must answer the QueueArn",
                    row.case
                )
            })
            .to_string();
        grants.push(json!({ "Effect": "Allow", "Action": "sqs:SendMessage", "Resource": arn }));
        queue_urls.push(url);
    }
    let machine_role = given_role(
        &router,
        case,
        "xsvc-sfn-callback-machine-role",
        "states.amazonaws.com",
        Value::Array(grants),
    )
    .await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let queue_url = &queue_urls[index];
        let definition = json!({
            "StartAt": "HandOff",
            "States": {
                "HandOff": {
                    "Type": "Task",
                    "Resource": "arn:aws:states:::sqs:sendMessage.waitForTaskToken",
                    "Parameters": {
                        "QueueUrl": queue_url,
                        "MessageBody": {
                            "token.$": "$$.Task.Token",
                            "job.$": "$.job",
                            "kind": row.kind,
                        },
                    },
                    "TimeoutSeconds": CALLBACK_TIMEOUT,
                    "End": true,
                },
            },
        });
        let machine_arn = given_machine(
            &router,
            case,
            &format!("xsvc-sfn-callback-{index}"),
            &definition,
            &machine_role,
        )
        .await;
        let execution_arn = started(
            &router,
            case,
            &machine_arn,
            "only",
            &json!({ "job": row.job }),
        )
        .await;

        let messages = delivered(&router, case, queue_url).await;
        assert_eq!(
            messages.len(),
            1,
            "case {case:?}: expected the one message the task hands its token out on, got \
             {messages:?}"
        );
        let body = document(&messages[0], "Body", case);
        let members = body
            .as_object()
            .unwrap_or_else(|| panic!("case {case:?}: expected a rendered object, got {body}"));
        assert_eq!(
            members.len(),
            3,
            "case {case:?}: expected the three members the MessageBody declares, got {body}"
        );
        assert_eq!(
            body["job"], row.job,
            "case {case:?}: expected the payload {} the input carried, got {body}",
            row.job
        );
        assert_eq!(
            body["kind"].as_str(),
            Some(row.kind),
            "case {case:?}: expected the literal member as written, got {body}"
        );
        let token = body["token"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a task token, got {body}"))
            .to_string();
        assert!(
            !token.is_empty(),
            "case {case:?}: expected a non-empty task token, got {body}"
        );
        // Off the queue for good, so what the queue holds at the end of the row is what else
        // was sent to it rather than this message coming back into view.
        deleted(&router, case, queue_url, &messages[0]).await;

        // The token is outstanding and nothing has redeemed it, so the execution is held.
        let holding = described(&router, case, &execution_arn).await;
        assert_eq!(
            holding["status"].as_str(),
            Some(RUNNING),
            "case {case:?}: expected {RUNNING} while the token is outstanding, got {holding}"
        );

        match &row.redeemed {
            Redeemed::Success(output) => {
                sfn(
                    &router,
                    case,
                    "SendTaskSuccess",
                    &json!({ "taskToken": token, "output": output.to_string() }),
                )
                .await;
                let reported = settled(&router, case, &execution_arn).await;
                assert_eq!(
                    reported["status"].as_str(),
                    Some(SUCCEEDED),
                    "case {case:?}: expected {SUCCEEDED} once the token was redeemed, got \
                     {reported}"
                );
                assert_eq!(
                    &document(&reported, "output", case),
                    output,
                    "case {case:?}: expected the output the redeeming call supplied, got \
                     {reported}"
                );
            }
            Redeemed::Failure { error, cause } => {
                let mut request = json!({ "taskToken": token, "error": error });
                if let Some(cause) = cause {
                    request["cause"] = Value::String((*cause).to_string());
                }
                sfn(&router, case, "SendTaskFailure", &request).await;
                let reported = settled(&router, case, &execution_arn).await;
                assert_eq!(
                    reported["status"].as_str(),
                    Some(FAILED),
                    "case {case:?}: expected {FAILED} once the token was failed, got {reported}"
                );
                assert_eq!(
                    reported["error"].as_str(),
                    Some(*error),
                    "case {case:?}: expected the error the redeeming call named, got {reported}"
                );
                assert_eq!(
                    reported.get("cause").and_then(Value::as_str),
                    *cause,
                    "case {case:?}: expected the cause the redeeming call supplied, got \
                     {reported}"
                );
            }
        }

        // The task handed its token over ONCE: no second message reached the queue.
        let afterwards = received(&router, case, queue_url).await;
        assert!(
            afterwards.is_empty(),
            "case {case:?}: expected no further message on the queue, got {afterwards:?}"
        );
    }
}
