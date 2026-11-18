// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING function's own Amazon SQS READ is authorized against: the identity-based
//! policies in force on the function's execution role AT THE INSTANT OF THE CALL.
//!
//! RULE: a call an AWS SDK makes from inside a running function is made as a session of that
//! function's execution role, and the service serving it decides EACH call afresh against the
//! documents then in force on that role — so a `ReceiveMessage` those documents allow hands the
//! queue's messages over and counts that delivery against each one, the identical call made
//! while nothing in force allows it is refused `AccessDenied` (HTTP 403) naming the assumed-role
//! session, the action and the queue's ARN, and such a refusal takes NOTHING out of the queue:
//! the message is still there and no delivery is counted for it, so the next read the policies
//! do allow is handed that message as its next delivery. Nothing about the function, its
//! environment or the session its calls are signed with changes between the calls: the only
//! thing that changes is the document IAM holds.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`; an explicit `Deny` refuses whatever
//! else allows, and with neither an identity-based nor a resource-based allow a request is
//! denied (IAM User Guide, *Policy evaluation logic*). Amazon SQS's model declares no
//! authorization error for this operation (`smithy_query(query="operation",
//! operation="ReceiveMessage")` names `OverLimit`, `QueueDoesNotExist`, the `Kms*` family,
//! `RequestThrottled` and `UnsupportedOperation`), so the refusal is the AWS-common one every
//! service answers a request its policies do not permit with. What a delivery counts is the
//! message's own `ApproximateReceiveCount` — "the number of times a message has been received
//! across all queues but not deleted" (`ReceiveMessageRequest$MessageSystemAttributeNames`) —
//! which is what a read that was refused must leave alone.
//!
//! Five crates have to agree for that to hold — IAM holds the role and the inline document a
//! `PutRolePolicy` rewrites between invocations, the token service mints the session the
//! handler's calls are signed with, CloudFormation archives the inline handler into a
//! deployment package this emulator can run, Lambda runs that handler and supplies it the SDK,
//! and Amazon SQS holds the queue and takes the decision — so the suite lives here: a
//! crate-local suite can only ever see one side of it. What the neighbours pin and this one
//! does not repeat: `lambda_in_function_sqs_authorization` drives the table of policy shapes an
//! in-function queue call is decided by, with each row's document fixed for the life of its
//! stack; the Amazon SQS crate's own `queue_policy_authorization` drives the decision for a
//! credential a client signs with directly; `lambda_in_function_secretsmanager_grant_revocation`
//! drives a rewritten grant against another service's read. What only this suite reaches is the
//! same function's queue READ being served, refused and served again as the role's document is
//! rewritten under it, with the message's own delivery count as the place each decision is read
//! back from.
//!
//! The rows differ along what the rule generalises over: HOW the grant is written (the action
//! alone, the whole service's actions by wildcard, the action inside a list beside another of
//! this service's), WHICH resource that statement names (the queue the handler reads, or every
//! resource), the FORM of the message body a served read must answer with (plain text, a JSON
//! document, text carrying newlines and non-ASCII) and WHY a refusal refuses (nothing in force
//! allows the read; an explicit `Deny` beside an allow of every action). Every row is probed on
//! its own queue AND on a second queue of the account, so a statement scoped to one queue is
//! shown to admit that queue and no other.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::stack_status;
use crate::query_wire::{elements, form_encode, opt_element, physical_ids_of};

/// The Region every request here is signed for, and so the Region the queues and the functions
/// belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every queue ARN and every session ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack that declares one function per row.
const STACK: &str = "xsvc-in-fn-sqs-recv-probes";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The IAM action the handler's read performs, which a grant names and a refusal blames.
const RECEIVE_ACTION: &str = "sqs:ReceiveMessage";

/// Another action of this service, which one row's grant names beside the read's own: it grants
/// nothing of the read.
const URL_ACTION: &str = "sqs:GetQueueUrl";

/// The code and status Amazon SQS refuses a call the policies in force do not permit with.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u64 = 403;

/// The name of the one inline policy each row's role carries, which every rewrite of that row's
/// grant replaces.
const POLICY_NAME: &str = "inline";

/// The second queue of the account, which no row's queue-scoped statement names, and the one
/// message it holds.
const OTHER_QUEUE_NAME: &str = "xsvc-in-fn-sqs-recv-elsewhere";
const OTHER_BODY: &str = "the-other-queues-own-message";

/// The `VisibilityTimeout` every queue here is created with, in seconds: the shortest a queue
/// may be configured for ("Valid values: An integer from 0 to 43,200 (12 hours)",
/// `SetQueueAttributesRequest$Attributes`). A message a probe's read was handed is therefore
/// available again to the next probe at once, so no assertion below waits for a span to pass or
/// turns on how fast the host runs. Nothing here tests the timeout itself.
const NO_HOLD: &str = "0";

/// How many messages one read asks for: the most a receive may ask for ("Valid values: 1 to
/// 10", `ReceiveMessageRequest$MaxNumberOfMessages`), so a read that was served reports
/// everything the queue would hand over and not a prefix of it.
const MAX_MESSAGES: u64 = 10;

/// How long one invocation is given, as the function's own `Timeout`: the most a function may be
/// configured for ("the maximum allowed value is 900 seconds",
/// `FunctionConfiguration$Timeout`), because what is under test is the authorization decision and
/// not how fast a loaded host gets an interpreter through one SDK call.
const FUNCTION_TIMEOUT: u64 = 900;

/// How often to look while waiting for work the clock owes — the stack reaching
/// `CREATE_COMPLETE`, a created function reaching `Active` — and how many looks to take. The
/// spans are those services' own, so no fixed sleep names the instant one ends, and the deadline
/// is generous so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_200;

/// The handler every row runs: it builds an Amazon SQS client with the SDK the runtime supplies,
/// reads the queue the EVENT names asking for each message's own delivery count, and reports
/// either what that read was handed or the refusal Amazon SQS answered it with — its code, its
/// sentence and the HTTP status it came back on.
const PROBE_HANDLER: &str = r##"
import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    client = boto3.client("sqs")
    try:
        answered = client.receive_message(
            QueueUrl=event["url"],
            MaxNumberOfMessages=event["max"],
            MessageSystemAttributeNames=["ApproximateReceiveCount"],
        )
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
            "reported": str(refused),
        }
    handed = answered.get("Messages", [])
    return {
        "ok": True,
        "bodies": [message["Body"] for message in handed],
        "counts": [
            message["Attributes"]["ApproximateReceiveCount"] for message in handed
        ],
    }
"##;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the queue it reads is the queue this suite created.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the credential
    // scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. Its credential is the long-term key every emulator publishes, so these calls are
/// the account's own and not any row's role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One Lambda `restJson1` call, as its status, its headers and its parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &[u8],
) -> (u16, http::HeaderMap, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(body.to_vec()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// One Amazon SQS operation over its `awsJson1_0` wire, made as the account's own credential —
/// the queues' owner, whose calls no row's grant decides.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization("sqs"))
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
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

/// WHICH resource a row's statement names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    /// The row's own queue, by the ARN Amazon SQS reports for it.
    OwnQueue,
    /// Every resource, which the bare `*` wildcard stands for.
    Every,
}

/// One row: how the grant is written, which resource it names, and the body of the one message
/// the row's own queue holds.
struct Row {
    case: &'static str,
    /// The actions the grant allows, beside the log actions every execution role carries.
    actions: &'static [&'static str],
    scope: Scope,
    /// The `MessageBody` the row's queue is planted with, which a served read must answer
    /// verbatim. `SendMessageRequest$MessageBody` constrains only the characters a message may
    /// carry, so its text is otherwise free.
    body: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the action alone, on the queue's own ARN, over plain text",
        actions: &[RECEIVE_ACTION],
        scope: Scope::OwnQueue,
        body: "a-plain-relay-message",
    },
    Row {
        case: "every action of the service, on every resource, over a JSON document",
        actions: &["sqs:*"],
        scope: Scope::Every,
        body: r#"{"order":"4711","total":19.5,"items":["a","b"]}"#,
    },
    Row {
        case: "the action inside a list beside another of this service's, on the queue's own \
               ARN, over text with newlines and non-ASCII",
        actions: &[URL_ACTION, RECEIVE_ACTION],
        scope: Scope::OwnQueue,
        body: "line one\nline two with £ and a space\n",
    },
];

/// The role one row runs as, the function it runs in, and the queue it reads — the first two are
/// named here because the refusal's own sentence names them: the identity a function's call is
/// signed as is the session `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-in-fn-sqs-recv-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-in-fn-sqs-recv-probe-{index}")
}
fn queue_name(index: usize) -> String {
    format!("xsvc-in-fn-sqs-recv-{index}")
}

/// One queue a probe reads, as the suite created it and as a served read must report it.
struct Queue {
    /// The url the handler's request carries.
    url: String,
    /// The ARN a statement names it by and a refusal reports.
    arn: String,
    /// The body of the one message it holds, which is never deleted: every probe reads the same
    /// message, so what a refused read did to it is visible to the next served one.
    body: &'static str,
    /// How many times a read has been handed that message so far, which is the
    /// `ApproximateReceiveCount` the next delivery of it reports. Raised ONLY where a read is
    /// expected to be served, so a refusal that counted a delivery fails here.
    handed: u32,
}

impl Queue {
    /// What the handler reports for a read the policies in force ALLOW: the queue's one message,
    /// and the delivery count that hand-over makes.
    fn served(&self) -> Value {
        json!({
            "ok": true,
            "bodies": [self.body],
            "counts": [self.handed.to_string()],
        })
    }
}

/// The clause a refusal closes with where NOTHING in force allows the read: IAM's implicit deny
/// names the action no identity-based policy allows.
fn implicit_clause() -> String {
    format!(
        "because no identity-based policy allows the {} action",
        reported_action()
    )
}

/// The clause it closes with where a document in force states an explicit `Deny` about the read.
const EXPLICIT_CLAUSE: &str = "with an explicit deny in an identity-based policy";

/// The action as a refusal of this service NAMES it: case-folded, whatever case the policy and
/// the model spell it in. IAM compares action names case-insensitively, so the case is the
/// service's own wording rather than part of the decision — and the refusals recorded for this
/// service carry it case-folded, which is what the crate's own refusal helper states
/// (`services/service-com-amazonaws-sqs-amazonsqs/src/state/authorization.rs`) and what the
/// neighbouring `lambda_in_function_sqs_authorization` suite asserts.
fn reported_action() -> String {
    RECEIVE_ACTION.to_lowercase()
}

/// The sentence Amazon SQS refuses a read with. Written out here rather than built from the
/// service's own helper, so the wording has something to be wrong about: the principal is the
/// session the function's call is signed as, the action is the one the read performs, the
/// resource is the queue the request addressed, and the clause says which document refused it.
fn refusal_message(index: usize, queue: &Queue, clause: &str) -> String {
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{}/{} is not authorized to perform: {} on \
         resource: {} {clause}",
        role_name(index),
        function_name(index),
        reported_action(),
        queue.arn,
    )
}

/// What the handler reports for a read nothing in force allows, or one a `Deny` refuses: the
/// code, the sentence and the status the service answered, and the one line a handler that logs
/// the exception writes — the SDK's own rendering of a refusal, "An error occurred (<Code>) when
/// calling the <Operation> operation: <Message>".
fn refused(index: usize, queue: &Queue, clause: &str) -> Value {
    let message = refusal_message(index, queue, clause);
    json!({
        "ok": false,
        "code": ACCESS_DENIED,
        "message": message,
        "status": ACCESS_DENIED_STATUS,
        "reported": format!(
            "An error occurred ({ACCESS_DENIED}) when calling the ReceiveMessage operation: \
             {message}"
        ),
    })
}

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for the
/// function to be handed it at all.
fn trust_policy() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// The statement every execution role carries whatever else it is granted: the log actions a
/// function writes its own log stream with, which grant nothing of Amazon SQS.
fn log_statement() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// The document a row's role carries while its grant is IN FORCE: the log actions, and the row's
/// own statement on the resource it names.
fn granting_document(row: &Row, own_queue_arn: &str) -> String {
    let resource = match row.scope {
        Scope::OwnQueue => own_queue_arn.to_string(),
        Scope::Every => "*".to_string(),
    };
    json!({
        "Version": "2012-10-17",
        "Statement": [
            log_statement(),
            {"Effect": "Allow", "Action": row.actions, "Resource": resource},
        ],
    })
    .to_string()
}

/// The document that replaces it when the grant is WITHDRAWN: the same least-privilege baseline
/// every execution role needs, and nothing of Amazon SQS at all.
fn logs_only_document() -> String {
    json!({"Version": "2012-10-17", "Statement": [log_statement()]}).to_string()
}

/// The document that allows every action of the service on every resource and states an explicit
/// `Deny` of the read on one queue beside it: the read of THAT queue is refused and the read of
/// any other is served.
fn allow_all_denying_document(denied_queue_arn: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            log_statement(),
            {"Effect": "Allow", "Action": "sqs:*", "Resource": "*"},
            {"Effect": "Deny", "Action": RECEIVE_ACTION, "Resource": denied_queue_arn},
        ],
    })
    .to_string()
}

/// Put `document` in force as the one inline policy of the row's role.
async fn put_role_policy(router: &MultiServiceRouter, case: &str, index: usize, document: &str) {
    let name = role_name(index);
    let (status, answered) = query(
        router,
        "iam",
        "2010-05-08",
        "PutRolePolicy",
        &[
            ("RoleName", &name),
            ("PolicyName", POLICY_NAME),
            ("PolicyDocument", document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRolePolicy({name:?}) must succeed, got {status} with {answered}"
    );
}

/// Create the role one row runs as, and answer the ARN IAM minted for it. The grant itself is
/// put in force later, so one role is rewritten by `PutRolePolicy` between the probes.
async fn create_role(router: &MultiServiceRouter, index: usize) -> String {
    let name = role_name(index);
    let (status, created) = query(
        router,
        "iam",
        "2010-05-08",
        "CreateRole",
        &[
            ("RoleName", &name),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    opt_element(&created, "Arn").unwrap_or_else(|| {
        panic!("setup: CreateRole({name:?}) must answer the role's ARN, got {created}")
    })
}

/// Create one queue holding one message of `body`, and answer it as the probes address it.
async fn create_queue(router: &MultiServiceRouter, name: &str, body: &'static str) -> Queue {
    let (status, created) = sqs(
        router,
        "CreateQueue",
        &json!({"QueueName": name, "Attributes": {"VisibilityTimeout": NO_HOLD}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateQueue({name:?}) must succeed, got {status} with {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("setup: CreateQueue({name:?}) must answer the queue's url, got {created}")
        })
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn", "VisibilityTimeout"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetQueueAttributes({name:?}) must succeed, got {status} with {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("setup: the queue {name:?} must report its ARN, got {attributes}")
        })
        .to_string();
    assert_eq!(
        arn,
        format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}"),
        "setup: the ARN a statement and a refusal name is the account's own, got {arn:?}"
    );
    assert_eq!(
        attributes["Attributes"]["VisibilityTimeout"].as_str(),
        Some(NO_HOLD),
        "setup: the queue {name:?} must hold the configured timeout, got {attributes}"
    );
    let (status, sent) = sqs(
        router,
        "SendMessage",
        &json!({"QueueUrl": url, "MessageBody": body}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: planting the message on {name:?} must succeed, got {status} with {sent}"
    );
    Queue {
        url,
        arn,
        body,
        handed: 0,
    }
}

/// The template the suite deploys: one function per row, running as that row's role. The inline
/// source is archived into the deployment package by CloudFormation, which is how a function
/// whose handler is written here comes to exist at all.
fn stack_template(role_arns: &[String]) -> String {
    let mut resources = serde_json::Map::new();
    for (index, role_arn) in role_arns.iter().enumerate() {
        resources.insert(
            format!("Probe{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": PROBE_HANDLER},
                },
            }),
        );
    }
    json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": Value::Object(resources),
    })
    .to_string()
}

/// Deploy the stack, wait for `CREATE_COMPLETE` and answer the physical id of each declared
/// function.
async fn deploy(router: &MultiServiceRouter, role_arns: &[String]) -> BTreeMap<String, String> {
    let template = stack_template(role_arns);
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant Lambda reports its functions created,
    // which no fixed sleep can name: this looks until it reports it, with a deadline generous
    // enough that a loaded host does not decide the outcome.
    let mut reported = stack_status(router, STACK).await;
    for _ in 0..LOOKS {
        if reported.0 == CREATE_COMPLETE {
            break;
        }
        tokio::time::sleep(LOOK).await;
        reported = stack_status(router, STACK).await;
    }
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
        reported.1
    );
    let (status, resources) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "DescribeStackResources",
        &[("StackName", STACK)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: DescribeStackResources({STACK:?}) must succeed, got {status} with {resources}"
    );
    physical_ids_of(&resources)
}

/// Wait until Lambda reports `name` `Active` — "the function is ready to be invoked"
/// (`FunctionConfiguration$State`) — which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {name:?} Active, last reported {last}");
}

/// Invoke `function` on `queue`, assert its handler reported `expected`, and assert the queue
/// still holds its one message afterwards whichever way the decision went — a read that was
/// refused deletes nothing, and a read that was served takes nothing permanently out of a queue
/// whose visibility timeout is zero.
async fn probe(
    router: &MultiServiceRouter,
    case: &str,
    phase: &str,
    function: &str,
    queue: &Queue,
    expected: &Value,
) {
    let event = json!({"url": queue.url, "max": MAX_MESSAGES});
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?} ({phase}): the invocation must be served, got {status} with {payload}"
    );
    let faulted = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    assert_eq!(
        faulted, None,
        "case {case:?} ({phase}): the handler must run to completion, got {faulted:?} with \
         {payload}"
    );
    assert_eq!(
        &payload, expected,
        "case {case:?} ({phase}): expected the handler to report {expected}, got {payload}"
    );
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": queue.url,
            "AttributeNames": [
                "ApproximateNumberOfMessages",
                "ApproximateNumberOfMessagesNotVisible",
            ],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?} ({phase}): GetQueueAttributes must succeed, got {status} with {attributes}"
    );
    assert_eq!(
        attributes["Attributes"]["ApproximateNumberOfMessages"].as_str(),
        Some("1"),
        "case {case:?} ({phase}): expected the queue to still hold its one message, got \
         {attributes}"
    );
    assert_eq!(
        attributes["Attributes"]["ApproximateNumberOfMessagesNotVisible"].as_str(),
        Some("0"),
        "case {case:?} ({phase}): expected no message of the queue to be held in flight, got \
         {attributes}"
    );
}

/// Every row: the function's own queue read is decided against the document in force on its
/// execution role when the call is made — so withdrawing the grant refuses the identical call,
/// denying it explicitly refuses it with the other clause, and putting the grant back serves it
/// again on the message no refused read took or counted.
#[tokio::test]
async fn an_in_function_queue_read_follows_its_execution_roles_grant() {
    let router = build_router();

    let mut other = create_queue(&router, OTHER_QUEUE_NAME, OTHER_BODY).await;

    let mut queues = Vec::new();
    let mut role_arns = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        queues.push(create_queue(&router, &queue_name(index), row.body).await);
        role_arns.push(create_role(&router, index).await);
    }
    let physical = deploy(&router, &role_arns).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let logical = format!("Probe{index}");
        let function = physical
            .get(&logical)
            .cloned()
            .unwrap_or_else(|| panic!("setup: the stack must report a physical id for {logical}"));
        assert_eq!(
            function,
            function_name(index),
            "setup: the stack must name the function {:?}, got {function:?}",
            function_name(index)
        );
        wait_until_active(&router, &function).await;
        let granting = granting_document(row, &queues[index].arn);

        // Phase 1: the grant is in force, so the read is served and counts the one delivery it
        // was handed.
        put_role_policy(&router, case, index, &granting).await;
        let own = &mut queues[index];
        own.handed += 1;
        let expected = own.served();
        probe(&router, case, "granted", &function, own, &expected).await;

        // And a statement scoped to that one queue admits that queue and no other, while one
        // naming every resource admits both.
        let expected = if row.scope == Scope::Every {
            other.handed += 1;
            other.served()
        } else {
            refused(index, &other, &implicit_clause())
        };
        probe(&router, case, "granted", &function, &other, &expected).await;

        // Phase 2: the same role, carrying the logs-only baseline instead, is granted nothing of
        // this service — so the identical call is refused, with IAM's own implicit-deny sentence.
        put_role_policy(&router, case, index, &logs_only_document()).await;
        let own = &queues[index];
        let expected = refused(index, own, &implicit_clause());
        probe(&router, case, "withdrawn", &function, own, &expected).await;

        // Phase 3: the grant is put back and the identical call is served again — on the very
        // message the refused read was not handed, whose delivery count that refusal left where
        // it was.
        put_role_policy(&router, case, index, &granting).await;
        let own = &mut queues[index];
        own.handed += 1;
        let expected = own.served();
        probe(&router, case, "restored", &function, own, &expected).await;

        // Phase 4: a document allowing every action of the service on every resource, with an
        // explicit deny of the read on this queue beside it: no grant overturns that deny, and
        // the other queue the same document allows is still read.
        put_role_policy(&router, case, index, &allow_all_denying_document(&own.arn)).await;
        let own = &queues[index];
        let expected = refused(index, own, EXPLICIT_CLAUSE);
        probe(&router, case, "explicitly denied", &function, own, &expected).await;
        other.handed += 1;
        let expected = other.served();
        probe(&router, case, "explicitly denied", &function, &other, &expected).await;

        // Phase 5: with the deny lifted the read is served once more, and the message's delivery
        // count has risen only by the reads that were SERVED.
        put_role_policy(&router, case, index, &granting).await;
        let own = &mut queues[index];
        own.handed += 1;
        let expected = own.served();
        probe(&router, case, "deny lifted", &function, own, &expected).await;
    }
}
