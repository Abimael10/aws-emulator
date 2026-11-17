// SPDX-License-Identifier: CC0-1.0

//! The two Lambda-backed `Task` shapes a CloudFormation stack deploys, driven end to end over
//! the assembled emulator: CloudFormation, IAM, the token service, AWS Lambda, Amazon S3 and
//! AWS Step Functions on ONE engine, so every call a handler makes is served by the store
//! these suites read.
//!
//! RULE 1 (the callback pattern): a `Task` whose `Resource` names the Lambda invocation with
//! the `.waitForTaskToken` pattern hands the invocation the token the context object carries
//! and leaves the task outstanding until that token is redeemed; the AWS SDK a managed
//! `python3.x` runtime puts on the handler's import path can build a client for ANY service
//! the emulator serves — Step Functions among them — so the handler itself redeems it, and
//! the execution's terminal outcome is exactly what the redemption supplied: `SUCCEEDED`
//! carrying the `output` a `SendTaskSuccess` named, `FAILED` carrying the `error` and `cause`
//! a `SendTaskFailure` named. A client the runtime's SDK could not construct would be an
//! exception raised before anything is signed and would surface as the execution's own
//! error, which no row here tolerates.
//!
//! RULE 2 (the plain function task): a `Task` whose `Resource` is a function ARN invokes that
//! function under the state machine's execution role, so the function's own side effects
//! become visible through the owning service's API, the execution leaves `RUNNING` for a
//! terminal status, and the history carries the `LambdaFunction*` family — `Scheduled` with
//! the resolved resource and the document the task was scheduled with, `Succeeded` with the
//! function's own output. The role is assumed before the invocation is made, so an execution
//! whose role does not admit this service's principal ends `FAILED` with the service's own
//! refusal of that assume as its cause — reported as the attempt's own failure in that same
//! family — with nothing invoked and so no side effect of the function in place.
//!
//! Six crates have to agree for either rule to hold, so the suite lives here rather than in
//! any one of them. What the neighbours pin and this one does not repeat: the Step Functions
//! crate's `lambda_callback_task_token` pins WHEN a token may be answered and its
//! `execution_role_assumption` how an unassumable role is WORDED,
//! `sfn_lambda_task_handler_writes_objects` pins what a task's handler can write, and
//! `cloudformation_sfn_lambda_published_handles` pins the HANDLE a stack publishes for a
//! machine. What only this suite reaches is the IaC composition: one stack declaring the
//! function, the roles and the state machine that names them, deployed and then RUN, which is
//! how a client builds either pattern.
//!
//! The rows differ along what the rules generalise over. Rule 1: the value the execution's
//! input carries (so the document the callback computes differs per row — a positive number,
//! zero, a negative one, a large one), which token the handler builds its Step Functions
//! client with (the SDK's own name for the service, and the service's SDK id), and whether
//! the redemption is a success or a failure. Rule 2: whether the execution role's trust
//! policy admits this service, and the shape of the key the handler writes under (a flat
//! prefix, a nested one carrying a dot and an underscore).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_sfn_awsstepfunctions::state::integration::TASK_FAILED_ERROR;
use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EVENT_LAMBDA_FUNCTION_FAILED, EVENT_LAMBDA_FUNCTION_SCHEDULED, EVENT_LAMBDA_FUNCTION_SUCCEEDED,
};

// The `StackStatus` read and the awsQuery form encoder this test crate writes once, borrowed
// rather than copied: `tests/all/main.rs` is this binary's crate root.
use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{form_encode, outputs_of};

/// The Region every request here is signed for — the Region the shared stack read polls in —
/// and so the Region every resource these stacks declare belongs to.
const REGION: &str = "us-east-1";

/// The `StackStatus` a deployment that created everything it declared reports.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The capability a template declaring NAMED IAM resources has to be created with: "If you
/// have IAM resources with custom names, you must specify CAPABILITY_NAMED_IAM"
/// (`CreateStack`).
const NAMED_IAM: &str = "CAPABILITY_NAMED_IAM";

/// The execution statuses the rules are stated in
/// (`smithy_query(query="shape",shape="ExecutionStatus")`).
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const RUNNING: &str = "RUNNING";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The service principals the stacks' trust policies are written for: the one Step Functions
/// assumes an execution role as, and the one Lambda assumes a function's execution role as.
const STATES_PRINCIPAL: &str = "states.amazonaws.com";
const LAMBDA_PRINCIPAL: &str = "lambda.amazonaws.com";

/// How long one invocation is given, as the function's own `Timeout`, and how long the
/// callback task is allowed to stay outstanding. Both are well past what any row needs on any
/// host: neither bound is what is under test, so neither may decide the outcome.
const FUNCTION_TIMEOUT: u64 = 120;
/// See [`FUNCTION_TIMEOUT`].
const CALLBACK_TIMEOUT: u64 = 300;

/// How often to look while waiting for work the clock owes — a stack settling, a function
/// becoming invocable, an execution leaving [`RUNNING`] — and how many looks to take. The
/// spans belong to those services, so no fixed sleep names the instant one ends, and the
/// deadline is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
/// See [`LOOK`].
const LOOKS: usize = 1_800;

/// The services these stacks run through, over ONE shared engine, as `core/router/src/lib.rs`
/// mounts them: a handler's own SDK call is dispatched back into this same router, so the task
/// it answers and the bucket it writes to are the ones this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One response of a REST surface: its status and its body's own bytes.
struct Answer {
    status: u16,
    body: Vec<u8>,
}

impl Answer {
    /// The body as text, for a refusal or a JSON document.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body as the JSON document a `restJson1` answer carries.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or_else(|_| Value::String(self.text()))
        }
    }
}

/// One request against Amazon S3's REST surface, path-style.
async fn s3(router: &MultiServiceRouter, method: &str, path: &str) -> Answer {
    let host = format!("s3.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("authorization", authorization("s3"))
        .body(Bytes::new())
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: response.body().to_vec(),
    }
}

/// One Lambda `restJson1` GET.
async fn lambda_get(router: &MultiServiceRouter, uri: &str) -> Answer {
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
    Answer {
        status: response.status().as_u16(),
        body: response.body().to_vec(),
    }
}

/// One Step Functions call (`awsJson1_0`, `AWSStepFunctions.<Operation>`) that must answer
/// 200.
async fn sfn(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let host = format!("states.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AWSStepFunctions.{operation}"))
        .header("authorization", authorization("states"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Step Functions {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One awsQuery call to CloudFormation, answered with its status and its XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("cloudformation.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("cloudformation"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).to_string(),
    )
}

/// A trust policy admitting `principal` for `sts:AssumeRole`, as a template states one.
fn trust(principal: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
}

/// An inline policy list with one `Allow` statement about `actions`, as `AWS::IAM::Role`'s
/// `Policies` property carries one.
fn policies(name: &str, actions: Value) -> Value {
    json!([{
        "PolicyName": name,
        "PolicyDocument": {
            "Version": "2012-10-17",
            "Statement": [{ "Effect": "Allow", "Action": actions, "Resource": "*" }],
        },
    }])
}

/// Deploy `template` as `stack` and answer the `DescribeStacks` body it reached
/// [`CREATE_COMPLETE`] with. The completion is owed from the instant the owning services
/// report their resources ready, which no fixed sleep names: this looks until they do.
async fn deployed(router: &MultiServiceRouter, case: &str, stack: &str, template: &str) -> String {
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", stack),
            ("TemplateBody", template),
            ("Capabilities.member.1", NAMED_IAM),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected CreateStack({stack:?}) to be accepted, got {status} with \
         {answered}"
    );
    let mut reported = stack_status(router, stack).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            return reported.1;
        }
        // A deployment that has already stopped somewhere else is reported now, rather than
        // polled out: the status is terminal, so looking again can only say the same thing.
        assert!(
            reported.0.is_empty() || reported.0.ends_with("_IN_PROGRESS"),
            "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, it stopped at {} \
             with {}",
            reported.0,
            reported.1
        );
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, stack).await;
    }
    panic!(
        "case {case:?}: expected the stack to reach {CREATE_COMPLETE}, last saw {} with {}",
        reported.0, reported.1
    );
}

/// Wait until Lambda reports `name` [`FUNCTION_ACTIVE`] — "during this time, you can't invoke
/// or modify the function" (`CreateFunction`) — and answer the ARN it reports for it.
async fn function_active(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let mut configuration = Value::Null;
    for _ in 0..LOOKS {
        configuration = lambda_get(
            router,
            &format!("/2015-03-31/functions/{name}/configuration"),
        )
        .await
        .json();
        if configuration["State"].as_str() == Some(FUNCTION_ACTIVE) {
            let arn = configuration["FunctionArn"].as_str().unwrap_or_else(|| {
                panic!("case {case:?}: expected the function to report its ARN, got {configuration}")
            });
            return arn.to_string();
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!(
        "case {case:?}: expected Lambda to report {name:?} {FUNCTION_ACTIVE}, last saw \
         {configuration}"
    );
}

/// Start one execution of `machine_arn` with `input` and answer its ARN beside the
/// `DescribeExecution` answer taken once it had left [`RUNNING`].
async fn ran(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    name: &str,
    input: &Value,
) -> (String, Value) {
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
    let execution_arn = started["executionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected StartExecution to answer an ARN, got {started}")
        })
        .to_string();
    let mut described = Value::Null;
    for _ in 0..LOOKS {
        described = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if described["status"].as_str() != Some(RUNNING) {
            return (execution_arn, described);
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: expected the execution to leave {RUNNING}, last saw {described}");
}

/// The document a member of an answer carries as JSON TEXT, parsed.
fn document(case: &str, holder: &Value, member: &str) -> Value {
    let text = holder[member].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected a {member} on {holder}")
    });
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("case {case:?}: expected {member} to be JSON, got {text:?} ({error})")
    })
}

/// Every history event of `execution_arn`.
async fn history(router: &MultiServiceRouter, case: &str, execution_arn: &str) -> Vec<Value> {
    let answered = sfn(
        router,
        case,
        "GetExecutionHistory",
        &json!({ "executionArn": execution_arn }),
    )
    .await;
    answered["events"].as_array().cloned().unwrap_or_default()
}

/// The one event of `events` whose `type` is `wanted`, or `None` where there is none.
fn event_of(events: &[Value], wanted: &str) -> Option<Value> {
    let matching: Vec<&Value> = events
        .iter()
        .filter(|event| event["type"].as_str() == Some(wanted))
        .collect();
    assert!(
        matching.len() <= 1,
        "expected at most one {wanted} event, got {} of them",
        matching.len()
    );
    matching.first().map(|event| (*event).clone())
}

// ---------------------------------------------------------------------------
// Rule 1: the callback task a stack deploys, answered by the function's own SDK
// ---------------------------------------------------------------------------

/// The stack the callback topology is deployed as, and the names it gives its resources.
const CALLBACK_STACK: &str = "xsvc-callback-token-stack";
/// See [`CALLBACK_STACK`].
const CALLBACK_FUNCTION: &str = "xsvc-callback-token-function";
/// See [`CALLBACK_STACK`].
const CALLBACK_MACHINE: &str = "xsvc-callback-token-machine";

/// The `Resource` of the optimized Lambda invocation with the callback pattern: the Task "hands
/// the function the token and waits" until it is redeemed.
const CALLBACK_RESOURCE: &str = "arn:aws:states:::lambda:invoke.waitForTaskToken";

/// The placeholder the deployed definition names the function by, and the key
/// `DefinitionSubstitutions` states its value under.
const FUNCTION_PLACEHOLDER: &str = "${CallbackFunctionArn}";
/// See [`FUNCTION_PLACEHOLDER`].
const FUNCTION_SUBSTITUTION: &str = "CallbackFunctionArn";

/// The handler the callback stack declares inline: it is handed the task's token and the
/// execution's own input, builds a Step Functions client with the SDK its runtime supplies, and
/// redeems the token the way the input asked for.
///
/// The client's construction is the step the whole pattern rests on: a runtime whose SDK has no
/// registry entry for the service raises there, before anything is signed, and that exception —
/// never a task result — becomes the execution's error.
const CALLBACK_HANDLER: &str = r#"
import json

import boto3


def lambda_handler(event, context):
    asked = event["input"]
    states = boto3.client(asked["client"])
    if asked["mode"] == "failure":
        states.send_task_failure(
            taskToken=event["token"],
            error=asked["error"],
            cause=asked["cause"],
        )
        return {"answered": "failure"}
    states.send_task_success(
        taskToken=event["token"],
        output=json.dumps({"doubled": asked["value"] * 2}),
    )
    return {"answered": "success"}
"#;

/// How one row redeems the token it was handed.
enum Redemption {
    /// A `SendTaskSuccess` whose output is computed from the input's own value.
    Success(i64),
    /// A `SendTaskFailure` naming this error and cause.
    Failure(&'static str, &'static str),
}

/// One row of rule 1: what the execution's input asks the handler to do.
struct CallbackRow {
    case: &'static str,
    /// The token the handler builds its Step Functions client with.
    client: &'static str,
    redemption: Redemption,
}

fn callback_rows() -> Vec<CallbackRow> {
    vec![
        CallbackRow {
            // The AWS SDK for Python's own name for this service, which is what a handler
            // written for the callback pattern builds its client with.
            case: "a positive value, redeemed through the SDK's own name for the service",
            client: "stepfunctions",
            redemption: Redemption::Success(21),
        },
        CallbackRow {
            // The same service under the token its SDK id gives it, the other name a client
            // is built with.
            case: "zero, redeemed through the service's SDK id",
            client: "sfn",
            redemption: Redemption::Success(0),
        },
        CallbackRow {
            case: "a negative value",
            client: "stepfunctions",
            redemption: Redemption::Success(-7),
        },
        CallbackRow {
            case: "a value far outside the range a single digit covers",
            client: "sfn",
            redemption: Redemption::Success(1_234_567),
        },
        CallbackRow {
            case: "a failure the work itself reports",
            client: "stepfunctions",
            redemption: Redemption::Failure(
                "ApprovalRejected",
                "the reviewer declined the request",
            ),
        },
    ]
}

/// The input one row starts its execution with, which the Task hands the function whole.
fn callback_input(row: &CallbackRow) -> Value {
    match row.redemption {
        Redemption::Success(value) => json!({
            "client": row.client,
            "mode": "success",
            "value": value,
        }),
        Redemption::Failure(error, cause) => json!({
            "client": row.client,
            "mode": "failure",
            "error": error,
            "cause": cause,
        }),
    }
}

/// The whole callback topology in ONE stack: the function's execution role, the function, the
/// state machine's execution role, and the machine whose single `Task` names the function by
/// the ARN the stack resolves for it.
fn callback_template() -> String {
    let definition = json!({
        "StartAt": "AwaitCallback",
        "States": {
            "AwaitCallback": {
                "Type": "Task",
                "Resource": CALLBACK_RESOURCE,
                "Parameters": {
                    "FunctionName": FUNCTION_PLACEHOLDER,
                    // How the Amazon States Language hands the token to the work it
                    // dispatches, beside the execution's own input.
                    "Payload": { "token.$": "$$.Task.Token", "input.$": "$" },
                },
                "TimeoutSeconds": CALLBACK_TIMEOUT,
                "End": true,
            },
        },
    })
    .to_string();
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "CallbackRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{CALLBACK_FUNCTION}-exec"),
                    "AssumeRolePolicyDocument": trust(LAMBDA_PRINCIPAL),
                    "Policies": policies(
                        "answer-the-token",
                        json!(["states:SendTaskSuccess", "states:SendTaskFailure"]),
                    ),
                },
            },
            "Callback": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": CALLBACK_FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": { "Fn::GetAtt": ["CallbackRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": CALLBACK_HANDLER },
                },
            },
            "MachineRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{CALLBACK_MACHINE}-exec"),
                    "AssumeRolePolicyDocument": trust(STATES_PRINCIPAL),
                    "Policies": policies("invoke-the-work", json!("lambda:InvokeFunction")),
                },
            },
            "Machine": {
                "Type": "AWS::StepFunctions::StateMachine",
                "Properties": {
                    "StateMachineName": CALLBACK_MACHINE,
                    "RoleArn": { "Fn::GetAtt": ["MachineRole", "Arn"] },
                    "DefinitionString": definition,
                    "DefinitionSubstitutions": {
                        FUNCTION_SUBSTITUTION: { "Fn::GetAtt": ["Callback", "Arn"] },
                    },
                },
            },
        },
        "Outputs": {
            "MachineArn": { "Value": { "Fn::GetAtt": ["Machine", "Arn"] } },
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Callback", "Arn"] } },
        },
    })
    .to_string()
}

/// Rule 1: one stack deploys the callback topology, and every row's execution ends on what the
/// handler's own Step Functions client supplied.
#[tokio::test]
async fn a_deployed_callback_task_ends_on_what_the_functions_own_sdk_answered() {
    let case = "the stack the callback rows run against";
    let router = build_router();

    let described = deployed(&router, case, CALLBACK_STACK, &callback_template()).await;
    let outputs = outputs_of(&described);
    let output = |name: &str| {
        outputs.get(name).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish {name}, got {described}")
        })
    };
    let machine_arn = output("MachineArn");
    let function_arn = output("FunctionArn");

    // The definition the stack deployed names the function by the ARN CloudFormation resolved
    // for it, so the Task's `FunctionName` is a handle Lambda answers to.
    let machine = sfn(
        &router,
        case,
        "DescribeStateMachine",
        &json!({ "stateMachineArn": machine_arn }),
    )
    .await;
    let definition = machine["definition"].as_str().unwrap_or_default();
    assert!(
        definition.contains(&function_arn),
        "case {case:?}: expected the deployed definition to name {function_arn:?}, got \
         {definition:?}"
    );
    assert!(
        !definition.contains(FUNCTION_PLACEHOLDER),
        "case {case:?}: expected the substitution to be filled in, got {definition:?}"
    );

    // The function has to be invocable before any row runs: a hand-over withheld inside that
    // window would be the lifecycle's refusal rather than the pattern's own answer.
    function_active(&router, case, CALLBACK_FUNCTION).await;

    for (index, row) in callback_rows().into_iter().enumerate() {
        let case = row.case;
        let input = callback_input(&row);
        let (execution_arn, described) = ran(
            &router,
            case,
            &machine_arn,
            &format!("callback-{index}"),
            &input,
        )
        .await;

        match row.redemption {
            Redemption::Success(value) => {
                assert_eq!(
                    described["status"].as_str(),
                    Some(SUCCEEDED),
                    "case {case:?}: expected the execution to end {SUCCEEDED}, got {} with \
                     error {} and cause {}",
                    described["status"],
                    described["error"],
                    described["cause"]
                );
                let answered = document(case, &described, "output");
                assert_eq!(
                    answered,
                    json!({ "doubled": value * 2 }),
                    "case {case:?}: expected the output the callback computed from the \
                     input's own value, got {answered}"
                );
            }
            Redemption::Failure(error, cause) => {
                assert_eq!(
                    (
                        described["status"].as_str(),
                        described["error"].as_str(),
                        described["cause"].as_str(),
                    ),
                    (Some(FAILED), Some(error), Some(cause)),
                    "case {case:?}: expected the execution to end {FAILED} carrying the error \
                     and cause the callback named, got {described}"
                );
                assert!(
                    described["output"].is_null(),
                    "case {case:?}: expected an execution that failed to report no output, \
                     got {described}"
                );
            }
        }

        // The whole point of the pattern: the task was answered by the token, so the history
        // carries the hand-over the state made rather than a client the handler could not
        // build. A task closed by its own token is SUBMITTED and then succeeds or fails.
        let events = history(&router, case, &execution_arn).await;
        assert!(
            events
                .iter()
                .any(|event| event["type"].as_str() == Some("TaskSubmitted")),
            "case {case:?}: expected the callback task to be submitted, got {events:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// Rule 2: the plain function task a stack deploys, and the role it runs under
// ---------------------------------------------------------------------------

/// The stack the function-ARN topology is deployed as, and the names it gives its resources.
const TASK_STACK: &str = "xsvc-lambda-task-stack";
/// See [`TASK_STACK`].
const TASK_BUCKET: &str = "xsvc-lambda-task-results";
/// See [`TASK_STACK`].
const TASK_FUNCTION: &str = "xsvc-lambda-task-writer";
/// See [`TASK_STACK`].
const TRUSTED_MACHINE: &str = "xsvc-lambda-task-trusted";
/// See [`TASK_STACK`].
const MISTRUSTED_MACHINE: &str = "xsvc-lambda-task-mistrusted";

/// The placeholder the deployed definitions name the function by.
const WRITER_PLACEHOLDER: &str = "${WriterFunctionArn}";
/// See [`WRITER_PLACEHOLDER`].
const WRITER_SUBSTITUTION: &str = "WriterFunctionArn";

/// The handler the task stack declares inline: it writes the object the task named, which is
/// the side effect a client observes through Amazon S3's own API.
const WRITER_HANDLER: &str = r#"
import json

import boto3


def lambda_handler(event, context):
    body = json.dumps({"key": event["key"], "marker": event["marker"]}, sort_keys=True)
    boto3.client("s3").put_object(
        Bucket=event["bucket"], Key=event["key"], Body=body.encode("utf-8")
    )
    return {"written": event["key"], "marker": event["marker"]}
"#;

/// One row of rule 2: which machine the execution runs on — and so what its execution role's
/// trust policy admits — and the object the task's handler is asked to write.
struct TaskRow {
    case: &'static str,
    /// Whether the machine's execution role admits this service's principal.
    assumable: bool,
    key: &'static str,
    marker: &'static str,
}

fn task_rows() -> Vec<TaskRow> {
    vec![
        TaskRow {
            case: "a role this service may assume, writing under a flat prefix",
            assumable: true,
            key: "results/alpha.json",
            marker: "alpha",
        },
        TaskRow {
            case: "a role this service may assume, writing under a nested prefix carrying a \
                   dot and an underscore",
            assumable: true,
            key: "results/nested/deeper/beta_1.part.json",
            marker: "β two ☕",
        },
        TaskRow {
            case: "a role whose trust policy names another service instead, writing under a \
                   flat prefix",
            assumable: false,
            key: "results/gamma.json",
            marker: "gamma",
        },
        TaskRow {
            case: "a role whose trust policy names another service instead, writing under a \
                   nested prefix",
            assumable: false,
            key: "results/nested/deeper/delta.json",
            marker: "delta",
        },
    ]
}

/// The definition both machines run: ONE `Task` whose `Resource` is the function's own ARN,
/// handed the bucket it writes to and the key and marker the execution's input carries.
fn writer_definition() -> String {
    json!({
        "StartAt": "Write",
        "States": {
            "Write": {
                "Type": "Task",
                "Resource": WRITER_PLACEHOLDER,
                "Parameters": {
                    "bucket": TASK_BUCKET,
                    "key.$": "$.key",
                    "marker.$": "$.marker",
                },
                "End": true,
            },
        },
    })
    .to_string()
}

/// The whole task topology in ONE stack: the bucket the handler writes to, the function's own
/// execution role and the function, and TWO machines over one definition whose execution roles
/// differ in nothing but the principal their trust policies admit.
fn task_template() -> String {
    let definition = writer_definition();
    let substitutions = json!({
        WRITER_SUBSTITUTION: { "Fn::GetAtt": ["Writer", "Arn"] },
    });
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Results": {
                "Type": "AWS::S3::Bucket",
                "Properties": { "BucketName": TASK_BUCKET },
            },
            "WriterRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{TASK_FUNCTION}-exec"),
                    "AssumeRolePolicyDocument": trust(LAMBDA_PRINCIPAL),
                    "Policies": policies("write-the-result", json!("s3:PutObject")),
                },
            },
            "Writer": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": TASK_FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.lambda_handler",
                    "Role": { "Fn::GetAtt": ["WriterRole", "Arn"] },
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": WRITER_HANDLER },
                },
            },
            "TrustingRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{TRUSTED_MACHINE}-exec"),
                    "AssumeRolePolicyDocument": trust(STATES_PRINCIPAL),
                    "Policies": policies("invoke-the-work", json!("lambda:InvokeFunction")),
                },
            },
            // The one resource the rows differ in: the SAME policies, and a trust policy
            // naming the principal Lambda assumes a function's role as instead of the one
            // this service assumes an execution role as.
            "MistrustingRole": {
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": format!("{MISTRUSTED_MACHINE}-exec"),
                    "AssumeRolePolicyDocument": trust(LAMBDA_PRINCIPAL),
                    "Policies": policies("invoke-the-work", json!("lambda:InvokeFunction")),
                },
            },
            "TrustedMachine": {
                "Type": "AWS::StepFunctions::StateMachine",
                "Properties": {
                    "StateMachineName": TRUSTED_MACHINE,
                    "RoleArn": { "Fn::GetAtt": ["TrustingRole", "Arn"] },
                    "DefinitionString": definition,
                    "DefinitionSubstitutions": substitutions,
                },
            },
            "MistrustedMachine": {
                "Type": "AWS::StepFunctions::StateMachine",
                "Properties": {
                    "StateMachineName": MISTRUSTED_MACHINE,
                    "RoleArn": { "Fn::GetAtt": ["MistrustingRole", "Arn"] },
                    "DefinitionString": definition,
                    "DefinitionSubstitutions": substitutions,
                },
            },
        },
        "Outputs": {
            "TrustedMachineArn": { "Value": { "Fn::GetAtt": ["TrustedMachine", "Arn"] } },
            "MistrustedMachineArn": { "Value": { "Fn::GetAtt": ["MistrustedMachine", "Arn"] } },
            "MistrustingRoleArn": { "Value": { "Fn::GetAtt": ["MistrustingRole", "Arn"] } },
            "FunctionArn": { "Value": { "Fn::GetAtt": ["Writer", "Arn"] } },
        },
    })
    .to_string()
}

/// Rule 2: one stack deploys the Lambda-task topology, and each row's execution either runs the
/// function — leaving its side effect and the `LambdaFunction*` history behind — or fails on the
/// execution role before any of that work is handed over.
#[tokio::test]
async fn a_deployed_lambda_task_runs_its_function_only_under_a_role_this_service_may_assume() {
    let case = "the stack the Lambda-task rows run against";
    let router = build_router();

    let described = deployed(&router, case, TASK_STACK, &task_template()).await;
    let outputs = outputs_of(&described);
    let output = |name: &str| {
        outputs.get(name).cloned().unwrap_or_else(|| {
            panic!("case {case:?}: expected the stack to publish {name}, got {described}")
        })
    };
    let trusted_arn = output("TrustedMachineArn");
    let mistrusted_arn = output("MistrustedMachineArn");
    let mistrusting_role_arn = output("MistrustingRoleArn");
    let function_arn = output("FunctionArn");

    function_active(&router, case, TASK_FUNCTION).await;

    for (index, row) in task_rows().into_iter().enumerate() {
        let case = row.case;
        let machine_arn = if row.assumable {
            &trusted_arn
        } else {
            &mistrusted_arn
        };
        let scheduled = json!({
            "bucket": TASK_BUCKET,
            "key": row.key,
            "marker": row.marker,
        });
        let (execution_arn, described) = ran(
            &router,
            case,
            machine_arn,
            &format!("write-{index}"),
            &json!({ "key": row.key, "marker": row.marker }),
        )
        .await;
        let events = history(&router, case, &execution_arn).await;
        let stored = s3(&router, "HEAD", &format!("/{TASK_BUCKET}/{}", row.key)).await;

        if !row.assumable {
            assert_eq!(
                (
                    described["status"].as_str(),
                    described["error"].as_str(),
                    described["cause"].as_str(),
                ),
                (
                    Some(FAILED),
                    Some(TASK_FAILED_ERROR),
                    Some(
                        format!(
                            "The principal {STATES_PRINCIPAL} is not authorized to assume the \
                             provided role. (role: {mistrusting_role_arn})"
                        )
                        .as_str()
                    ),
                ),
                "case {case:?}: expected the execution to end {FAILED} on the refusal of the \
                 assume, got {described}"
            );
            // The attempt is reported in the same family a function-ARN `Resource` writes,
            // and it FAILED on the assume: nothing ever answered for the invocation.
            let failed = event_of(&events, EVENT_LAMBDA_FUNCTION_FAILED).unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a {EVENT_LAMBDA_FUNCTION_FAILED} event, got \
                     {events:?}"
                )
            });
            let details = &failed["lambdaFunctionFailedEventDetails"];
            assert_eq!(
                (
                    details["error"].as_str(),
                    details["cause"].as_str() == described["cause"].as_str(),
                ),
                (Some(TASK_FAILED_ERROR), true),
                "case {case:?}: expected the attempt to report the execution's own failure, \
                 got {details}"
            );
            assert!(
                event_of(&events, EVENT_LAMBDA_FUNCTION_SUCCEEDED).is_none(),
                "case {case:?}: expected no invocation to succeed, got {events:?}"
            );
            assert_eq!(
                stored.status,
                404,
                "case {case:?}: expected HeadObject({:?}) to answer 404, got {} with {}",
                row.key,
                stored.status,
                stored.text()
            );
            continue;
        }

        assert_eq!(
            described["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected the execution to end {SUCCEEDED}, got {} with error {} \
             and cause {}",
            described["status"],
            described["error"],
            described["cause"]
        );
        let reported = document(case, &described, "output");
        assert_eq!(
            reported,
            json!({ "written": row.key, "marker": row.marker }),
            "case {case:?}: expected the function's own result as the execution's output, got \
             {reported}"
        );

        // The hand-over the task made, as the history reports it: the resolved function and
        // the document the task was scheduled with, then the function's own output.
        let scheduled_event = event_of(&events, EVENT_LAMBDA_FUNCTION_SCHEDULED)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a {EVENT_LAMBDA_FUNCTION_SCHEDULED} event, got \
                     {events:?}"
                )
            });
        let details = &scheduled_event["lambdaFunctionScheduledEventDetails"];
        assert_eq!(
            details["resource"].as_str(),
            Some(function_arn.as_str()),
            "case {case:?}: expected the hand-over to name the deployed function, got {details}"
        );
        let handed = document(case, details, "input");
        assert_eq!(
            handed, scheduled,
            "case {case:?}: expected the hand-over to carry the document the task was \
             scheduled with, got {handed}"
        );
        let succeeded_event = event_of(&events, EVENT_LAMBDA_FUNCTION_SUCCEEDED)
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a {EVENT_LAMBDA_FUNCTION_SUCCEEDED} event, got \
                     {events:?}"
                )
            });
        let answered = document(
            case,
            &succeeded_event["lambdaFunctionSucceededEventDetails"],
            "output",
        );
        assert_eq!(
            answered,
            json!({ "written": row.key, "marker": row.marker }),
            "case {case:?}: expected the hand-over's outcome to carry the function's own \
             output, got {answered}"
        );

        // And the side effect itself, read through the owning service's own API: an
        // invocation that answered without doing the work fails here.
        assert_eq!(
            stored.status,
            200,
            "case {case:?}: expected HeadObject({:?}) to answer 200, got {} with {}",
            row.key,
            stored.status,
            stored.text()
        );
        let fetched = s3(&router, "GET", &format!("/{TASK_BUCKET}/{}", row.key)).await;
        assert_eq!(
            fetched.status,
            200,
            "case {case:?}: expected GetObject({:?}) to answer 200, got {} with {}",
            row.key,
            fetched.status,
            fetched.text()
        );
        let held: Value = serde_json::from_slice(&fetched.body).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: expected the object to hold the JSON the handler wrote, got \
                 {:?} ({error})",
                fetched.text()
            )
        });
        assert_eq!(
            held,
            json!({ "key": row.key, "marker": row.marker }),
            "case {case:?}: expected the object to hold the document the handler built, got \
             {held}"
        );
    }
}
