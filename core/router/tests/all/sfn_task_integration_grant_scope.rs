// SPDX-License-Identifier: CC0-1.0

//! WHICH calls a Step Functions `Task` state's execution role may make: the SCOPE of the
//! grant its documents carry, over the two integrations whose work is visible outside Step
//! Functions itself — a Lambda invocation, whose result the execution reports, and a queue
//! send, whose message the queue hands back.
//!
//! RULE: Step Functions makes a `Task` state's integration call ACTING AS the state
//! machine's `roleArn`, and the call happens only where a document in force on that role
//! allows THAT action on THAT resource: a grant of the published action over the ARN of the
//! resource the request addresses (or over a resource wildcard covering it) lets the call be
//! made, while a grant of the same action over another resource, a grant of another action
//! over this very resource, and a document saying nothing about the call each leave the
//! implicit deny standing. A call left denied is NOT made: the execution settles in the
//! terminal status `FAILED` under the invoked service's base SDK exception, the refusal is
//! attributed to the denied action — the cause `DescribeExecution` carries, and the cause of
//! the failed-task event `GetExecutionHistory` carries, both name the invoked service's own
//! access-denied code and that action over the addressed ARN — and the resource is untouched:
//! no message reaches the queue, and the execution reports no output a function returned. A
//! call allowed is made and its real work is what the execution reports: the value the
//! handler computed for the event it was handed, and the message body on the queue.
//!
//! The role every row runs under is trusted by this service, so a refusal here can only be
//! the missing grant: a role this service may not act as fails every call of the walk with a
//! sentence about the ASSUME instead, and no row's cause carries one.
//!
//! Five crates have to agree for that to hold — Step Functions decides and makes the call,
//! IAM holds the documents it is judged against, CloudFormation archives the inline handler
//! into the function's deployment package, Lambda runs it, and Amazon SQS takes the message
//! and hands it back — so the suite lives here, driven over each service's own wire protocol
//! through the emulator as `core/router/src/lib.rs` assembles it.
//!
//! What the neighbours already pin and this one does not repeat:
//! `all/sfn_task_target_authorization.rs` pins WHICH resource the access request is over and
//! `all/sfn_execution_role_documents_in_force.rs` pins WHERE the documents in force come
//! from. What only this suite reaches is the grant's SCOPE deciding work that really runs:
//! the rows differ along what the rule generalises over — what the document says about the
//! call (nothing, the action on another resource of the same service, another action on this
//! resource, the action on this very ARN, the action on every resource) — and each allowed
//! row's answer is computed from that row's own input, so no row could pass on a fixed value.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// `tests/all/` is ONE crate root, so the percent-encoder of an
// `application/x-www-form-urlencoded` component, the reader of one element of an awsQuery
// answer and the reader of a deployed stack's status are borrowed rather than copied.
use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{form_encode, opt_element};

/// The Region every request below is signed for, and so the Region the function, the queues,
/// the roles and the state machines belong to. It is the Region the borrowed stack reader
/// speaks to, so both halves speak for one deployment.
const REGION: &str = "us-east-1";

/// The statuses an execution reaches (`smithy_query(query="shape",
/// shape="ExecutionStatus")`).
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const RUNNING: &str = "RUNNING";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// The `StackStatus` the function's stack must reach before the function exists.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The stack that deploys the function this suite's tasks invoke, and the function's name.
const FUNCTION_STACK: &str = "xsvc-sfn-grant-scope-stack";
const FUNCTION_NAME: &str = "xsvc-sfn-grant-scope-doubler";

/// The names of the other resources of each service a row's grant may name instead of the
/// one its request addresses, which is no grant on the call at all.
const OTHER_FUNCTION: &str = "xsvc-sfn-grant-scope-bystander";
/// See [`OTHER_FUNCTION`].
const OTHER_QUEUE: &str = "xsvc-sfn-grant-scope-other-queue";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is whether the invocation happened at all.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a stack reaching
/// [`CREATE_COMPLETE`], a function reaching [`FUNCTION_ACTIVE`], an execution leaving
/// [`RUNNING`], the message an allowed send owes its queue — and how many looks to take.
/// The spans are those services' own, so no fixed sleep names the instant one ends; the
/// deadline is generous because the host may be working through the rest of the workspace at
/// the same time, and a run whose conditions hold promptly spends none of it.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 6_000;

/// The optimized integrations the rows name: the invocation of the function a `FunctionName`
/// parameter addresses, and the send to the queue a `QueueUrl` parameter addresses.
const INVOKE_RESOURCE: &str = "arn:aws:states:::lambda:invoke";
const SEND_RESOURCE: &str = "arn:aws:states:::sqs:sendMessage";

/// The action a policy GRANTS each call under: the name the invoked service PUBLISHES for
/// it, which for an invocation is not the operation's own name.
const INVOKE_GRANT: &str = "lambda:InvokeFunction";
const SEND_GRANT: &str = "sqs:SendMessage";

/// ANOTHER action each service publishes, which a row grants over the very resource the
/// request addresses: a grant of it says nothing about the call the `Task` state makes
/// (`smithy_query(query="list_operations")` names both operations on their services).
const INVOKE_OTHER_ACTION: &str = "lambda:GetFunction";
const SEND_OTHER_ACTION: &str = "sqs:ReceiveMessage";

/// The action a refusal NAMES, as the invoked service's own authorization layer words it —
/// Amazon SQS reports it in lower case. IAM compares action names without regard to case, so
/// each pair names one action.
const INVOKE_REFUSED: &str = "lambda:InvokeFunction";
const SEND_REFUSED: &str = "sqs:sendmessage";

/// The code the invoked service's answer names an authorization failure by: the AWS-common
/// one for Lambda, and Amazon SQS's own, whose front door words it differently.
const INVOKE_DENIED_CODE: &str = "AccessDeniedException";
const SEND_DENIED_CODE: &str = "AccessDenied";

/// The error name a `Task` state reports for a refusal the invoked operation does not model,
/// which is what an access denial arrives as: the invoked service's base SDK exception.
const INVOKE_REFUSAL_ERROR: &str = "Lambda.AWSLambdaException";
const SEND_REFUSAL_ERROR: &str = "SQS.AmazonSQSException";

/// What this service's OWN failure to act as a role says, which is the other reason a walk's
/// calls could all fail. No row's cause may carry it: every role below is trusted by this
/// service, so what a refusal here reports is the grant and nothing else.
const TRUST_FAILURE: &str = "not authorized to assume";

/// The body every send carries, which is what an allowed call leaves on the queue.
const BODY: &str = "the-work";

/// The handler the invocation rows run: it computes its answer from the event it was handed
/// and RETURNS it, so a value the execution reports can only have come from a run of this
/// source over that event.
const DOUBLING_HANDLER: &str = r#"
def handler(event, context):
    return {"doubled": event["n"] * 2}
"#;

/// The emulator as it is ASSEMBLED, on one engine: every call below is served by the same
/// store, so the documents IAM wrote are the documents Step Functions decides against.
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
/// integration calls are made as is a session of this role and the statements below are what
/// decide them.
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

/// The statement every execution role below carries whatever else it does: the log writes a
/// state machine makes, which say nothing about any integration call.
fn logs_only() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogDelivery", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// A statement allowing `action` on `resource`.
fn allow(action: &str, resource: &str) -> Value {
    json!({ "Effect": "Allow", "Action": action, "Resource": resource })
}

/// Deploy the function the invocation rows invoke and answer its ARN, once Lambda reports it
/// [`FUNCTION_ACTIVE`].
///
/// The handler is declared as a stack's inline source, which CloudFormation archives into the
/// function's deployment package — the way a handler written out in a suite comes to be a
/// package at all.
async fn given_function(router: &MultiServiceRouter, case: &str, role_arn: &str) -> String {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Doubler": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION_NAME,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": { "ZipFile": DOUBLING_HANDLER },
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

/// The ARN of another resource of the same service, formed by renaming the last field of
/// `arn`: a grant naming it is a grant on something other than what the request addresses.
fn sibling_arn(arn: &str, name: &str) -> String {
    let (prefix, _) = arn
        .rsplit_once(':')
        .unwrap_or_else(|| panic!("expected an ARN whose last field is a name, got {arn:?}"));
    format!("{prefix}:{name}")
}

/// A queue called `name`; answers its url and the ARN a policy names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue must answer a url, got {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: GetQueueAttributes must answer the QueueArn, got {attributes}")
        })
        .to_string();
    (url, arn)
}

/// Create a state machine whose ONE `Task` state makes `resource`'s call with `parameters`,
/// under `role_arn`, and answer its ARN.
///
/// `output_path` is how the state's own answer is narrowed to the work's result — the
/// optimized invocation reports the whole `Invoke` response, of which `Payload` is what the
/// function returned — or `None` for a state whose answer is reported as it came.
async fn given_machine(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    resource: &str,
    parameters: &Value,
    output_path: Option<&str>,
) -> String {
    let mut state = json!({
        "Type": "Task",
        "Resource": resource,
        "Parameters": parameters,
        "End": true,
    });
    if let Some(path) = output_path {
        state["OutputPath"] = Value::String(path.to_string());
    }
    let definition = json!({ "StartAt": "Call", "States": { "Call": state } });
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

/// Start one execution carrying `input` and report it once it has left [`RUNNING`], with its
/// ARN.
async fn ran(
    router: &MultiServiceRouter,
    case: &str,
    machine_arn: &str,
    input: &Value,
) -> (String, Value) {
    let started = sfn(
        router,
        case,
        "StartExecution",
        &json!({
            "stateMachineArn": machine_arn,
            "name": "only",
            "input": input.to_string(),
        }),
    )
    .await;
    let execution_arn = started["executionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartExecution must answer an ARN, got {started}")
        })
        .to_string();
    let mut reported = Value::Null;
    for _ in 0..LOOKS {
        reported = sfn(
            router,
            case,
            "DescribeExecution",
            &json!({ "executionArn": execution_arn }),
        )
        .await;
        if reported["status"].as_str() != Some(RUNNING) {
            return (execution_arn, reported);
        }
        tokio::time::sleep(LOOK).await;
    }
    panic!("case {case:?}: the execution must leave {RUNNING}, last saw {reported}");
}

/// Every cause the events of `execution_arn`'s history carry, whichever event details carry
/// it: the failure of a `Task` state is reported under the details member of the event type
/// that state's work names, so what is read here is the cause itself rather than one member's
/// spelling.
async fn history_causes(
    router: &MultiServiceRouter,
    case: &str,
    execution_arn: &str,
) -> Vec<String> {
    let answered = sfn(
        router,
        case,
        "GetExecutionHistory",
        &json!({ "executionArn": execution_arn, "maxResults": 100 }),
    )
    .await;
    let events = answered["events"].as_array().cloned().unwrap_or_default();
    let mut causes = Vec::new();
    for event in &events {
        let Some(members) = event.as_object() else {
            continue;
        };
        for member in members.values() {
            if let Some(cause) = member.get("cause").and_then(Value::as_str) {
                causes.push(cause.to_string());
            }
        }
    }
    causes
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

/// The JSON document a member of a response carries as text.
fn document(carrier: &Value, member: &str, case: &str) -> Value {
    let text = carrier[member].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected a {member} carrying a JSON document, got {carrier}")
    });
    serde_json::from_str(text).unwrap_or_else(|why| {
        panic!("case {case:?}: the {member} must be a JSON document, got {text:?} ({why})")
    })
}

/// What one row's document says about the call it is judged on, stated once for both
/// integrations: each integration resolves it to its own published action, the ARN its
/// request addresses, and the ARN of another resource of the same service
/// ([`resolved`]).
enum Says {
    /// Nothing about the call at all.
    Nothing,
    /// The published action, over another resource of the same service.
    GrantOnAnother,
    /// Another action of the same service, over the resource the request addresses.
    AnotherActionHere,
    /// The published action, over the very ARN the request addresses.
    GrantHere,
    /// The published action, over every resource.
    GrantEverywhere,
}

/// One row: what the execution role's document says about the call, and whether that leaves
/// the call allowed.
struct Row {
    case: &'static str,
    says: Says,
    allowed: bool,
}

/// The catalog, one entry per thing a document can say about one call. The two allowed rows
/// are the positive controls: they are what shows the work really happens where the scope
/// reaches it.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a document saying nothing about the call",
            says: Says::Nothing,
            allowed: false,
        },
        Row {
            case: "the action, over another resource of the same service",
            says: Says::GrantOnAnother,
            allowed: false,
        },
        Row {
            case: "another action of the same service, over this very resource",
            says: Says::AnotherActionHere,
            allowed: false,
        },
        Row {
            case: "the action, over the very ARN the request addresses",
            says: Says::GrantHere,
            allowed: true,
        },
        Row {
            case: "the action, over every resource",
            says: Says::GrantEverywhere,
            allowed: true,
        },
    ]
}

/// The statements one row's role carries: the log writes every execution role has, plus what
/// this row says about the call, resolved to this integration's own actions and ARNs.
fn resolved(
    row: &Row,
    grant: &str,
    other_action: &str,
    addressed: &str,
    other: &str,
) -> Value {
    let mut statements = vec![logs_only()];
    match row.says {
        Says::Nothing => {}
        Says::GrantOnAnother => statements.push(allow(grant, other)),
        Says::AnotherActionHere => statements.push(allow(other_action, addressed)),
        Says::GrantHere => statements.push(allow(grant, addressed)),
        Says::GrantEverywhere => statements.push(allow(grant, "*")),
    }
    Value::Array(statements)
}

/// A role name for one row, which IAM holds one of per row.
fn role_name(prefix: &str, index: usize) -> String {
    format!("xsvc-sfn-grant-scope-{prefix}-{index}")
}

/// RULE, over the Lambda invocation: the grant's scope decides whether the function runs, and
/// an invocation left denied fails the execution naming the denied action while the function's
/// own answer never appears.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_grant_scope_decides_a_lambda_task_invocation() {
    let setup = "the function the invocation rows address";
    let router = build_router();
    let function_role = given_role(
        &router,
        setup,
        "xsvc-sfn-grant-scope-handler",
        "lambda.amazonaws.com",
        json!([{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }]),
    )
    .await;
    let function_arn = given_function(&router, setup, &function_role).await;
    let other_function = sibling_arn(&function_arn, OTHER_FUNCTION);

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let role_arn = given_role(
            &router,
            case,
            &role_name("invoke", index),
            "states.amazonaws.com",
            resolved(
                &row,
                INVOKE_GRANT,
                INVOKE_OTHER_ACTION,
                &function_arn,
                &other_function,
            ),
        )
        .await;
        let machine_arn = given_machine(
            &router,
            case,
            &format!("xsvc-sfn-grant-scope-invoke-{index}"),
            &role_arn,
            INVOKE_RESOURCE,
            &json!({ "FunctionName": function_arn.clone(), "Payload.$": "$" }),
            Some("$.Payload"),
        )
        .await;
        // The number each row doubles is its own, so the value the execution reports can only
        // have been computed from the event this row's input handed the handler.
        let n = 21 + index;
        let (execution_arn, described) = ran(&router, case, &machine_arn, &json!({ "n": n })).await;
        let causes = history_causes(&router, case, &execution_arn).await;

        if row.allowed {
            assert_eq!(
                described["status"].as_str(),
                Some(SUCCEEDED),
                "case {case:?}: expected {SUCCEEDED}, got {described}"
            );
            assert_eq!(
                document(&described, "output", case),
                json!({ "doubled": n * 2 }),
                "case {case:?}: expected the value the handler computed for {n}, got {described}"
            );
            continue;
        }

        assert_eq!(
            described["status"].as_str(),
            Some(FAILED),
            "case {case:?}: expected {FAILED}, got {described}"
        );
        assert_eq!(
            described["error"].as_str(),
            Some(INVOKE_REFUSAL_ERROR),
            "case {case:?}: expected {INVOKE_REFUSAL_ERROR}, got {described}"
        );
        let cause = described["cause"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: a failed execution must carry a cause, got {described}")
        });
        for named in [INVOKE_DENIED_CODE, INVOKE_REFUSED, function_arn.as_str()] {
            assert!(
                cause.contains(named),
                "case {case:?}: expected the cause to name {named:?}, got {cause:?}"
            );
        }
        assert!(
            !cause.contains(TRUST_FAILURE),
            "case {case:?}: expected the refusal to be the grant and not the assume, got {cause:?}"
        );
        assert!(
            described.get("output").is_none(),
            "case {case:?}: expected no output from an invocation that never happened, got \
             {described}"
        );
        assert!(
            causes
                .iter()
                .any(|carried| carried.contains(INVOKE_DENIED_CODE)
                    && carried.contains(INVOKE_REFUSED)),
            "case {case:?}: expected a history event naming {INVOKE_DENIED_CODE} and \
             {INVOKE_REFUSED}, got {causes:?}"
        );
    }
}

/// RULE, over the queue send: the grant's scope decides whether the message is sent, and a
/// send left denied fails the execution naming the denied action while the queue stays empty.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_grant_scope_decides_a_queue_send_task() {
    let setup = "the queue a grant naming another resource names";
    let router = build_router();
    let (_, other_queue_arn) = given_queue(&router, setup, OTHER_QUEUE).await;

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        // A queue of its own per row, so what it holds afterwards is what THIS row's task did
        // with it.
        let (queue_url, queue_arn) = given_queue(
            &router,
            case,
            &format!("xsvc-sfn-grant-scope-target-{index}"),
        )
        .await;
        let role_arn = given_role(
            &router,
            case,
            &role_name("send", index),
            "states.amazonaws.com",
            resolved(
                &row,
                SEND_GRANT,
                SEND_OTHER_ACTION,
                &queue_arn,
                &other_queue_arn,
            ),
        )
        .await;
        // The body each row sends is its own, so a message found on the queue belongs to the
        // row that was allowed to send it.
        let body = format!("{BODY}-{index}");
        let machine_arn = given_machine(
            &router,
            case,
            &format!("xsvc-sfn-grant-scope-send-{index}"),
            &role_arn,
            SEND_RESOURCE,
            &json!({ "QueueUrl": queue_url.clone(), "MessageBody": body.clone() }),
            None,
        )
        .await;
        let (execution_arn, described) =
            ran(&router, case, &machine_arn, &json!({ "sent": body })).await;
        let causes = history_causes(&router, case, &execution_arn).await;

        if row.allowed {
            assert_eq!(
                described["status"].as_str(),
                Some(SUCCEEDED),
                "case {case:?}: expected {SUCCEEDED}, got {described}"
            );
            let mut messages = received(&router, case, &queue_url).await;
            for _ in 0..LOOKS {
                if !messages.is_empty() {
                    break;
                }
                tokio::time::sleep(LOOK).await;
                messages = received(&router, case, &queue_url).await;
            }
            let bodies: Vec<&str> = messages
                .iter()
                .map(|message| message["Body"].as_str().unwrap_or_default())
                .collect();
            assert_eq!(
                bodies,
                vec![body.as_str()],
                "case {case:?}: expected the one message the task sent, got {messages:?}"
            );
            continue;
        }

        assert_eq!(
            described["status"].as_str(),
            Some(FAILED),
            "case {case:?}: expected {FAILED}, got {described}"
        );
        assert_eq!(
            described["error"].as_str(),
            Some(SEND_REFUSAL_ERROR),
            "case {case:?}: expected {SEND_REFUSAL_ERROR}, got {described}"
        );
        let cause = described["cause"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: a failed execution must carry a cause, got {described}")
        });
        for named in [SEND_DENIED_CODE, SEND_REFUSED, queue_arn.as_str()] {
            assert!(
                cause.contains(named),
                "case {case:?}: expected the cause to name {named:?}, got {cause:?}"
            );
        }
        assert!(
            !cause.contains(TRUST_FAILURE),
            "case {case:?}: expected the refusal to be the grant and not the assume, got {cause:?}"
        );
        assert!(
            causes
                .iter()
                .any(|carried| carried.contains(SEND_DENIED_CODE)
                    && carried.contains(SEND_REFUSED)),
            "case {case:?}: expected a history event naming {SEND_DENIED_CODE} and \
             {SEND_REFUSED}, got {causes:?}"
        );
        assert!(
            received(&router, case, &queue_url).await.is_empty(),
            "case {case:?}: expected the queue the role may not send to to be untouched"
        );
    }
}
