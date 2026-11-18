// SPDX-License-Identifier: CC0-1.0

//! The LIFECYCLE of the task token a `Task` state hands out when its `Resource` asks for
//! the callback pattern over a queue send
//! (`arn:aws:states:::sqs:sendMessage.waitForTaskToken`), driven over the ASSEMBLED
//! emulator — Step Functions, Amazon SQS and IAM on one engine, reached through
//! `aws_emulator_router::build_router` — because the token only exists if the integration
//! call the execution role is judged for really put it on the queue.
//!
//! RULE: a callback task's token is handed to the work the state's `Parameters` put it in
//! and the state then PARKS — the execution reports `RUNNING` while that token is
//! outstanding — and what ends the state is one of two things and nothing else: a
//! `SendTask*` quoting the token, where a `SendTaskSuccess` carries the execution on with
//! the `output` it supplied (and a `SendTaskHeartbeat` only reports progress, leaving the
//! task outstanding), or the state's own bound elapsing, which fails the state with the
//! Amazon States Language's `States.Timeout` — "if more time than the specified seconds
//! elapses between heartbeats from the task, this state fails with a States.Timeout error
//! name" for `HeartbeatSeconds`, and the same error name for `TimeoutSeconds`. A token
//! whose task has been closed either way answers nothing afterwards: every `SendTask*`
//! quoting it is refused HTTP 400 `TaskTimedOut`. And a `taskToken` that is not one the
//! service minted names no task at all, so it is refused HTTP 400 `InvalidToken` ("the
//! provided token is not valid",
//! `smithy_query(query="operation",operation="SendTaskSuccess")`) and resolves nothing —
//! an execution parked at a token of its own is untouched by it and still redeemable.
//!
//! Nothing in this rule makes a caller WAIT: every call above is answered, which is what
//! lets an application tell a token it may redeem from one it may not.
//!
//! What the neighbours pin and this suite does not repeat:
//! `sfn_lambda_task_result_and_sqs_callback_token` pins the message the task hands its
//! token out on and the output a redemption places, and the Step Functions crate's
//! `callback_timeout_error_handling` pins which of the state's own `Retry`/`Catch`
//! handlers answers for the `States.Timeout` a bound raises. What only this suite reaches
//! is the token's own lifecycle across the three directions a caller takes it in — kept
//! live by heartbeats and then redeemed, left to the bound and refused afterwards, and
//! fabricated — on the assembled emulator.
//!
//! The rows differ along what the rule generalises over: how many heartbeats restart the
//! window before the token is redeemed (none, one, two, three), the JSON type of the
//! payload the message carries beside the token and of the `output` the redemption
//! supplies (an object, text, text outside ASCII, a number, an empty object, a list),
//! WHICH bound of the state elapses (the heartbeat window alone, that window inside a
//! generous task bound, the task bound alone), and the FORM a fabricated token takes
//! (text that is not base64, base64 of text naming no task, base64 naming another
//! service's resource, base64 of an id the service does not mint beside a real execution,
//! and a token at the modelled `@length` ceiling of 2048).

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

// The percent-encoder for an `application/x-www-form-urlencoded` component and the reader
// of one element of an awsQuery answer, which is how IAM is called: the `all` target is
// one crate root, so this suite borrows both rather than carrying a second copy.
use crate::query_wire::{form_encode, opt_element};

/// The Region every request here is signed for, and so the Region the queues, the role and
/// the state machines belong to.
const REGION: &str = "us-east-1";

/// The statuses an execution reports: the two terminal ones these rows reach, and the one
/// it sits in while its callback token is outstanding
/// (`smithy_query(query="shape",shape="ExecutionStatus")`).
const SUCCEEDED: &str = "SUCCEEDED";
const FAILED: &str = "FAILED";
const RUNNING: &str = "RUNNING";

/// The Amazon States Language's own error name for a bound of a `Task` state that elapsed:
/// "if the task runs longer than the specified seconds, this state fails with a
/// States.Timeout error name".
const TIMEOUT_ERROR: &str = "States.Timeout";

/// The modelled refusal for a token whose task is no longer outstanding: "the task token
/// has either expired or the task associated with the token has already been closed".
const TASK_TIMED_OUT: &str = "TaskTimedOut";

/// The modelled refusal for a value that is not a token this service issued: "the provided
/// token is not valid".
const INVALID_TOKEN: &str = "InvalidToken";

/// Every refusal of a `SendTask*` is a client error, which this model declares at HTTP 400
/// (`smithy_query(query="operation",operation="SendTaskSuccess")`).
const REFUSED: u16 = 400;

/// The `@length` ceiling the model puts on `taskToken` (1..2048), which one fabricated row
/// sits exactly on.
const MAX_TOKEN_CHARS: usize = 2048;

/// The bound a row that is NOT about a bound declares, in seconds: generous, because no
/// such row waits it out — each redeems its token long before it runs out however loaded
/// the host is.
const LIVE_WINDOW: u64 = 300;

/// The bound a row that IS about a bound declares, in seconds: the smallest a definition
/// may state, so such a row waits for the condition it is about and no longer.
const LAPSING_BOUND: u64 = 1;

/// How often to look again while waiting for something the clock owes — the message a
/// parked task hands its token out on, an execution leaving [`RUNNING`] — and how many
/// looks to take. The wait is for the CONDITION: a run where each holds promptly takes
/// seconds, and only a run where one never holds spends the whole budget.
const LOOK: Duration = Duration::from_millis(50);
const LOOKS: usize = 2_400;

/// The emulator as it is ASSEMBLED, on one engine: the integration call a callback task
/// makes is served by the same store this suite reads the token back out of.
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

/// One Step Functions call that is expected to be REFUSED, as its status and error code.
async fn sfn_refusal(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
) -> (u16, String, Value) {
    let (status, parsed) = rpc(router, "states", "AWSStepFunctions", operation, body).await;
    (status, error_code(&parsed), parsed)
}

/// The error code an awsJson refusal carries in `__type`, without the namespace it may be
/// qualified by.
fn error_code(body: &Value) -> String {
    let raw = body.get("__type").and_then(Value::as_str).unwrap_or_default();
    raw.split('#').next_back().unwrap_or(raw).to_string()
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

/// One IAM call, requiring a 200, as its XML body.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let host = format!("iam.{REGION}.amazonaws.com");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(Bytes::from(body))
        .expect("a well-formed awsQuery request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must answer 200 from IAM, got {status} with {answered}"
    );
    answered
}

/// A queue named `name`, as the pair (its url, its ARN).
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: CreateQueue({name:?}) must answer a url"))
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
        .unwrap_or_else(|| panic!("case {case:?}: GetQueueAttributes must answer the QueueArn"))
        .to_string();
    (url, arn)
}

/// A role Step Functions may assume, granting `sqs:SendMessage` on each queue in
/// `queue_arns`; answers its ARN.
///
/// A state machine's execution role is trusted by Step Functions, so the identity the
/// integration call that hands the token out is made as is a session of this role, and
/// these statements are what decide it.
async fn given_machine_role(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    queue_arns: &[String],
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "states.amazonaws.com" },
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
    let document = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": "sqs:SendMessage",
            "Resource": queue_arns,
        }],
    })
    .to_string();
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

/// A one-state definition whose `Task` asks for the callback pattern over a send to
/// `queue_url`, carrying `bounds` (the `TimeoutSeconds`/`HeartbeatSeconds` it declares).
///
/// `$$.Task.Token` is how the language hands the token to the work the state calls, so it
/// is a member of the document the payload template builds — and the payload beside it is
/// read from the state's input, so the message proves both travelled.
fn definition(queue_url: &str, bounds: &Value) -> Value {
    let mut state = json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::sqs:sendMessage.waitForTaskToken",
        "Parameters": {
            "QueueUrl": queue_url,
            "MessageBody": { "token.$": "$$.Task.Token", "job.$": "$.job" },
        },
        "End": true,
    });
    let members = state.as_object_mut().expect("the state is an object");
    for (member, value) in bounds.as_object().expect("the bounds are an object") {
        members.insert(member.clone(), value.clone());
    }
    json!({ "StartAt": "HandOff", "States": { "HandOff": state } })
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
        &json!({ "name": name, "definition": definition.to_string(), "roleArn": role_arn }),
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
    let answered = sfn(
        router,
        case,
        "StartExecution",
        &json!({ "stateMachineArn": machine_arn, "name": name, "input": input.to_string() }),
    )
    .await;
    answered["executionArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: StartExecution must answer an ARN, got {answered}")
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

/// The JSON document a member of an answer carries as text — an execution's `output`, a
/// message's `Body`.
fn document(carrier: &Value, member: &str, case: &str) -> Value {
    let text = carrier[member].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected a {member} carrying a JSON document, got {carrier}")
    });
    serde_json::from_str(text).unwrap_or_else(|why| {
        panic!("case {case:?}: the {member} must be a JSON document, got {text:?} ({why})")
    })
}

/// The one message the parked task owes the queue at `queue_url`, taken off the queue for
/// good so nothing comes back into view later in the row.
async fn handed_out(router: &MultiServiceRouter, case: &str, queue_url: &str) -> Value {
    for _ in 0..LOOKS {
        let answered = sqs(
            router,
            case,
            "ReceiveMessage",
            &json!({ "QueueUrl": queue_url, "MaxNumberOfMessages": 10 }),
        )
        .await;
        let messages = answered["Messages"].as_array().cloned().unwrap_or_default();
        if messages.is_empty() {
            tokio::time::sleep(LOOK).await;
            continue;
        }
        assert_eq!(
            messages.len(),
            1,
            "case {case:?}: expected the one message the task hands its token out on, got \
             {messages:?}"
        );
        let handle = messages[0]["ReceiptHandle"].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: expected a ReceiptHandle, got {}", messages[0])
        });
        sqs(
            router,
            case,
            "DeleteMessage",
            &json!({ "QueueUrl": queue_url, "ReceiptHandle": handle }),
        )
        .await;
        return document(&messages[0], "Body", case);
    }
    panic!("case {case:?}: the callback task must hand its token to {queue_url:?}");
}

/// The token the parked task handed out, read off the message body beside the payload the
/// state's input supplied.
fn token_of(body: &Value, job: &Value, case: &str) -> String {
    assert_eq!(
        &body["job"], job,
        "case {case:?}: expected the payload {job} the state input carried, got {body}"
    );
    let token = body["token"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: expected a task token on the message, got {body}")
    });
    assert!(
        !token.is_empty() && token.len() <= MAX_TOKEN_CHARS,
        "case {case:?}: expected a token inside the modelled 1..{MAX_TOKEN_CHARS}, got {token:?}"
    );
    token.to_string()
}

/// Assert that every `SendTask*` quoting `token` is refused `REFUSED` with `code`.
///
/// All three are asserted because the rule is about the TOKEN and not about one of the
/// three calls that quote it; each models the same two refusals
/// (`smithy_query(query="operation",operation="SendTaskSuccess,SendTaskFailure")`, and the
/// same two on `SendTaskHeartbeat`).
async fn every_report_is_refused(router: &MultiServiceRouter, case: &str, token: &str, code: &str) {
    for operation in ["SendTaskSuccess", "SendTaskFailure", "SendTaskHeartbeat"] {
        let request = match operation {
            "SendTaskSuccess" => json!({ "taskToken": token, "output": "{}" }),
            "SendTaskFailure" => json!({ "taskToken": token, "error": "WorkerRejected" }),
            _ => json!({ "taskToken": token }),
        };
        let (status, answered, body) = sfn_refusal(router, operation, &request).await;
        assert_eq!(
            (status, answered.as_str()),
            (REFUSED, code),
            "case {case:?}: expected {operation} on this token to be refused {REFUSED} {code}, \
             got {status} {answered:?} with {body}"
        );
    }
}

/// One row of the redeemed direction: the payload the message carries beside the token,
/// how many heartbeats restart the window first, and the `output` the redemption supplies.
struct Redeemed {
    case: &'static str,
    queue: &'static str,
    job: Value,
    beats: usize,
    output: Value,
}

fn redeemed_rows() -> Vec<Redeemed> {
    vec![
        Redeemed {
            case: "no heartbeat at all, redeemed with an object output",
            queue: "xsvc-cbk-live-plain",
            job: json!({ "id": 7, "parts": ["a", "b"] }),
            beats: 0,
            output: json!({ "approved": true, "by": "the worker outside" }),
        },
        Redeemed {
            case: "one heartbeat, redeemed with a text output",
            queue: "xsvc-cbk-live-one-beat",
            job: json!("j-4711"),
            beats: 1,
            output: json!("approved"),
        },
        Redeemed {
            case: "two heartbeats, a payload outside ASCII and a list output",
            queue: "xsvc-cbk-live-unicode",
            job: json!("ждём ☕"),
            beats: 2,
            output: json!(["café ☕", 1, null]),
        },
        Redeemed {
            case: "three heartbeats, a number payload and an empty object output",
            queue: "xsvc-cbk-live-number",
            job: json!(12),
            beats: 3,
            output: json!({}),
        },
    ]
}

/// RULE: while its window is live the token answers its task — heartbeats only report
/// progress, and the `SendTaskSuccess` carries the execution on with the output it
/// supplied — and the task is then closed, so a second report on that token is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_parked_callback_token_is_redeemed_while_its_window_is_live() {
    let case = "a one-task machine whose work is a callback send";
    let router = build_router();
    let rows = redeemed_rows();

    // One queue per row, so a message one row's task sent is never counted in another's,
    // and the role is granted the send on each of them by ARN.
    let mut urls: Vec<String> = Vec::new();
    let mut arns: Vec<String> = Vec::new();
    for row in &rows {
        let (url, arn) = given_queue(&router, row.case, row.queue).await;
        urls.push(url);
        arns.push(arn);
    }
    let role_arn = given_machine_role(&router, case, "xsvc-cbk-live-role", &arns).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let queue_url = &urls[index];
        // Both bounds are declared and both are generous: what ends this row's task is the
        // report quoting its token, never the clock.
        let bounds = json!({ "TimeoutSeconds": LIVE_WINDOW, "HeartbeatSeconds": LIVE_WINDOW });
        let machine_arn = given_machine(
            &router,
            case,
            &format!("xsvc-cbk-live-{index}"),
            &definition(queue_url, &bounds),
            &role_arn,
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

        let token = token_of(&handed_out(&router, case, queue_url).await, &row.job, case);

        // The token is outstanding and nothing has reported on it, so the state is parked.
        let holding = described(&router, case, &execution_arn).await;
        assert_eq!(
            holding["status"].as_str(),
            Some(RUNNING),
            "case {case:?}: expected {RUNNING} while the token is outstanding, got {holding}"
        );

        // "This action resets the Heartbeat clock": a beat reports progress and nothing
        // more, so the task is still outstanding after each one and the execution is still
        // held.
        for beat in 0..row.beats {
            let answered = sfn(
                &router,
                case,
                "SendTaskHeartbeat",
                &json!({ "taskToken": token }),
            )
            .await;
            assert!(
                answered.as_object().is_none_or(|members| members.is_empty()),
                "case {case:?}: expected beat {beat} to be answered with no members, got \
                 {answered}"
            );
            let still = described(&router, case, &execution_arn).await;
            assert_eq!(
                still["status"].as_str(),
                Some(RUNNING),
                "case {case:?}: expected {RUNNING} after beat {beat}, got {still}"
            );
        }

        sfn(
            &router,
            case,
            "SendTaskSuccess",
            &json!({ "taskToken": token, "output": row.output.to_string() }),
        )
        .await;
        let reported = settled(&router, case, &execution_arn).await;
        assert_eq!(
            reported["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected {SUCCEEDED} once the token was redeemed, got {reported}"
        );
        assert_eq!(
            &document(&reported, "output", case),
            &row.output,
            "case {case:?}: expected the output the redeeming call supplied, got {reported}"
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

        // One token answers one task: the task is closed, so every later report on it is
        // refused and the execution it already carried on is left alone.
        every_report_is_refused(&router, case, &token, TASK_TIMED_OUT).await;
        let afterwards = described(&router, case, &execution_arn).await;
        assert_eq!(
            afterwards["status"].as_str(),
            Some(SUCCEEDED),
            "case {case:?}: expected the refused reports to change nothing, got {afterwards}"
        );
    }
}

/// One row of the lapsing direction: which bound the state declares, which is the one that
/// elapses.
struct Lapsed {
    case: &'static str,
    queue: &'static str,
    bounds: Value,
}

fn lapsed_rows() -> Vec<Lapsed> {
    vec![
        Lapsed {
            case: "the heartbeat window alone, with no beat to restart it",
            queue: "xsvc-cbk-lapse-heartbeat",
            bounds: json!({ "HeartbeatSeconds": LAPSING_BOUND }),
        },
        Lapsed {
            case: "the heartbeat window inside a generous whole-task bound",
            queue: "xsvc-cbk-lapse-inner",
            bounds: json!({ "HeartbeatSeconds": LAPSING_BOUND, "TimeoutSeconds": LIVE_WINDOW }),
        },
        Lapsed {
            case: "the whole task's bound alone",
            queue: "xsvc-cbk-lapse-task",
            bounds: json!({ "TimeoutSeconds": LAPSING_BOUND }),
        },
    ]
}

/// RULE: a bound of the state elapsing fails it with `States.Timeout` and closes the task,
/// after which every report quoting that token is refused `TaskTimedOut`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_callback_token_nobody_reports_on_is_closed_by_the_states_bound() {
    let case = "a one-task machine whose callback bound elapses";
    let router = build_router();
    let rows = lapsed_rows();

    let mut urls: Vec<String> = Vec::new();
    let mut arns: Vec<String> = Vec::new();
    for row in &rows {
        let (url, arn) = given_queue(&router, row.case, row.queue).await;
        urls.push(url);
        arns.push(arn);
    }
    let role_arn = given_machine_role(&router, case, "xsvc-cbk-lapse-role", &arns).await;

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let queue_url = &urls[index];
        let job = json!({ "attempt": index });
        let machine_arn = given_machine(
            &router,
            case,
            &format!("xsvc-cbk-lapse-{index}"),
            &definition(queue_url, &row.bounds),
            &role_arn,
        )
        .await;
        let execution_arn =
            started(&router, case, &machine_arn, "only", &json!({ "job": job })).await;

        // The token really travelled, so what the bound closes below is a task this state
        // handed out.
        let token = token_of(&handed_out(&router, case, queue_url).await, &job, case);

        let reported = settled(&router, case, &execution_arn).await;
        assert_eq!(
            (reported["status"].as_str(), reported["error"].as_str()),
            (Some(FAILED), Some(TIMEOUT_ERROR)),
            "case {case:?}: expected {FAILED} with {TIMEOUT_ERROR}, got {reported}"
        );
        // The bound that elapsed is the whole report: nobody answered the token, so there
        // is nothing to explain beside the error name.
        assert!(
            reported.get("cause").is_none_or(Value::is_null),
            "case {case:?}: expected no cause beside the error name, got {reported}"
        );

        every_report_is_refused(&router, case, &token, TASK_TIMED_OUT).await;
        let afterwards = described(&router, case, &execution_arn).await;
        assert_eq!(
            afterwards["status"].as_str(),
            Some(FAILED),
            "case {case:?}: expected the refused reports to change nothing, got {afterwards}"
        );
    }
}

/// `value` as base64, which is how this service encodes the task tokens it mints — so a
/// fabricated token is written in the same alphabet rather than being refused for its
/// characters.
fn base64_of(value: &str) -> String {
    ::aws_smithy_types::base64::encode(value.as_bytes())
}

/// The forms a fabricated token takes: none of them is a value this service minted, and
/// `execution_arn` is the parked execution the last of them names, to show that naming a
/// real execution is not what makes a token one of the service's own.
fn fabricated_tokens(execution_arn: &str) -> Vec<(String, String)> {
    vec![
        (
            "text that is not base64 at all".to_string(),
            "not-a-task-token".to_string(),
        ),
        (
            "base64 of text naming no task".to_string(),
            base64_of("fabricated by the caller"),
        ),
        (
            "base64 naming another service's resource".to_string(),
            base64_of("11111111-1111-1111-1111-111111111111|arn:aws:sqs:us-east-1:000000000000:q"),
        ),
        (
            "base64 of an id the service does not mint, beside the parked execution".to_string(),
            base64_of(&format!("not-a-minted-id|{execution_arn}")),
        ),
        (
            format!("a token of {MAX_TOKEN_CHARS} characters, the modelled ceiling"),
            "A".repeat(MAX_TOKEN_CHARS),
        ),
    ]
}

/// RULE: a `taskToken` the service never minted is refused `InvalidToken` and resolves
/// nothing — the execution parked at a token of its own is untouched, and that real token
/// still redeems it afterwards.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_token_the_service_never_minted_is_refused_and_resolves_nothing() {
    let case = "a parked callback task beside tokens nothing issued";
    let router = build_router();

    let (queue_url, queue_arn) = given_queue(&router, case, "xsvc-cbk-forged").await;
    let role_arn = given_machine_role(
        &router,
        case,
        "xsvc-cbk-forged-role",
        std::slice::from_ref(&queue_arn),
    )
    .await;
    // Generous bounds: this row is about the tokens, so the parked task outlives every
    // refusal below however loaded the host is.
    let bounds = json!({ "TimeoutSeconds": LIVE_WINDOW, "HeartbeatSeconds": LIVE_WINDOW });
    let machine_arn = given_machine(
        &router,
        case,
        "xsvc-cbk-forged",
        &definition(&queue_url, &bounds),
        &role_arn,
    )
    .await;
    let job = json!({ "needs": "approval" });
    let execution_arn = started(&router, case, &machine_arn, "only", &json!({ "job": job })).await;
    let token = token_of(&handed_out(&router, case, &queue_url).await, &job, case);

    for (case, forged) in fabricated_tokens(&execution_arn) {
        let case = case.as_str();
        assert_ne!(
            forged, token,
            "case {case:?}: a fabricated token must not be the one the task handed out"
        );
        every_report_is_refused(&router, case, &forged, INVALID_TOKEN).await;
        // The refusal resolved nothing: the execution is still parked at its own token.
        let holding = described(&router, case, &execution_arn).await;
        assert_eq!(
            holding["status"].as_str(),
            Some(RUNNING),
            "case {case:?}: expected the parked execution to be untouched, got {holding}"
        );
    }

    // And the token that WAS issued still answers its task, after every fabricated one was
    // refused.
    let output = json!({ "approved": true });
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
        "case {case:?}: expected {SUCCEEDED} once the real token was redeemed, got {reported}"
    );
    assert_eq!(
        document(&reported, "output", case),
        output,
        "case {case:?}: expected the output the redeeming call supplied, got {reported}"
    );
}
