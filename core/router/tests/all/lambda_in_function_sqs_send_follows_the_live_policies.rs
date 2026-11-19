// SPDX-License-Identifier: CC0-1.0

//! WHEN the policies that decide a running function's own `SendMessage` are read: at the
//! instant of each call.
//!
//! RULE: a service decides every request it serves against the policy documents in force AT
//! THAT INSTANT — the calling principal's identity-based documents and the addressed
//! resource's own resource-based policy — so a document written, revised or removed between
//! two otherwise identical calls decides the NEXT one and nothing is carried over from the
//! call before it: a grant taken off the principal refuses the next call, the same grant
//! written again serves the one after it, an explicit `Deny` about the caller in the
//! resource's own policy refuses it whatever the principal is granted, a statement about some
//! other principal decides nothing about this one, and an `Allow` in the resource's policy
//! naming the caller serves a call no identity-based document allows. A refused call is
//! `AccessDenied` (HTTP 403) naming the caller, the action and the resource, with the reason
//! the document that withheld the grant states, and it leaves the resource exactly as it was.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), so the identity a handler's own SDK call speaks for is the
//! session `arn:aws:sts::<account>:assumed-role/<role>/<function>`; a request is allowed when
//! an identity-based policy allows it or the resource's own policy names the caller and allows
//! it, and an explicit `Deny` in either refuses it (IAM User Guide, *Policy evaluation
//! logic*). Amazon SQS declares no authorization error for `SendMessage`, so the refusal is
//! the AWS-common one, worded as that service words it. A queue's `Policy` is that
//! resource-based document (`SetQueueAttributesRequest$Attributes`), and `iam:PutRolePolicy` /
//! `iam:DeleteRolePolicy` are how an inline identity-based document comes and goes.
//!
//! Five crates have to agree for that to hold — IAM holds the role and the inline document
//! this suite takes away and puts back, CloudFormation deploys the sender function,
//! STS mints the session its calls are signed with, Lambda runs the handler and supplies it
//! the SDK, and Amazon SQS takes the decision — so the suite lives here, where a crate-local
//! one could only see one side of it. What the neighbours pin and this one does not repeat:
//! `lambda_in_function_sqs_authorization` drives the decision over roles whose identity-based
//! documents DIFFER, and `lambda_in_function_sqs_queue_policy_send` over queues whose own
//! documents differ, both with every document in place before anything runs;
//! `lambda_in_function_sqs_receive_grant_revocation` drives one role's identity-based document
//! being REWRITTEN between two of the same function's READS. What only this suite reaches is
//! the queue's OWN document being installed, narrowed to somebody else and cleared under one
//! running function — the resource-based half of the same instant-of-the-call rule, including
//! the send that is served by that document alone while the role's own grants nothing — which
//! is what a client installing a queue policy to close a blast radius, or revoking a grant to
//! prove one, is relying on.
//!
//! The rows differ along what the rule generalises over: WHICH document changes (the
//! principal's own inline policy, the queue's resource policy, neither), in WHICH direction (a
//! grant taken off and put back, a document installed and cleared), with WHICH effect (`Allow`
//! or `Deny`) and naming WHICH principal (the role the function runs as, or a principal of
//! another account). Every row asserts three things: what the queue reports its stored
//! `Policy` to be, what the handler reports of its call, and what the QUEUE holds afterwards —
//! so a document stored and then ignored, a refusal that still enqueued and a grant that
//! answered without enqueuing all fail here.

use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode};

/// The Region every request here is signed for, and so the Region the queue and the function
/// belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which the queue ARN and the session ARN both carry.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The one queue every invocation addresses.
const QUEUE: &str = "xsvc-live-policy-sqs-sink";

/// The one execution role the function runs as, the inline document that grants the send and
/// the one that grants the logs a function writes. The send grant is the document this suite
/// takes away and puts back; the log grant stays in force throughout, so a revocation is a
/// revocation of the send alone.
const ROLE: &str = "xsvc-live-policy-sqs-role";
const SEND_POLICY: &str = "send-to-the-sink";
const LOGS_POLICY: &str = "write-its-logs";

/// The one function whose handler makes the call.
const FUNCTION: &str = "xsvc-live-policy-sqs-sender";

/// The stack that function is deployed by.
const STACK: &str = "xsvc-live-policy-sqs-stack";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// A principal of ANOTHER account, which no session of this one is: the spelling the rows that
/// state something about somebody else name.
const ANOTHER_ACCOUNT_PRINCIPAL: &str = "arn:aws:iam::210987654321:root";

/// The IAM action the handler's call performs, which is what a statement grants or denies.
const SEND_ACTION: &str = "sqs:SendMessage";

/// The same action as a refusal of this service NAMES it: case-folded, which is how the
/// RECORDED refusals of Amazon SQS word one (`sqs:sendmessage`), whatever case the policy and
/// the model spell the name in. The case is the service's wording and not part of the decision,
/// since IAM compares action names case-insensitively.
const REPORTED_ACTION: &str = "sqs:sendmessage";

/// The code and status Amazon SQS refuses a call the policies in force do not permit with.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u64 = 403;

/// The reason an IMPLICIT deny states: within one account the permission the caller is missing
/// is its own, so the refusal names the action no policy of its allows.
const NO_IDENTITY_GRANT: &str =
    "because no identity-based policy allows the sqs:sendmessage action";

/// The reason an explicit `Deny` in the QUEUE's own document states.
const RESOURCE_DENY: &str = "with an explicit deny in a resource-based policy";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and not how
/// fast an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// The `VisibilityTimeout` the queue carries, in seconds: the longest a queue may be
/// configured for ("an integer from 0 to 43,200 (12 hours)",
/// `SetQueueAttributesRequest$Attributes`). Nothing here tests the timeout itself; it is this
/// long so no assertion about what the queue holds can turn on how fast the host runs.
const VISIBILITY_TIMEOUT: u64 = 43_200;

/// How often to look while waiting for work the clock owes — the function reaching `Active`, a
/// message the queue is owed — and how many looks to take. The spans are those services' own,
/// so no fixed sleep names the instant one ends, and the deadline is generous enough that a
/// loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// How many looks a drain takes before it reports what it found, so an enqueue that landed a
/// moment after the answer is caught rather than raced past: a row expecting nothing cannot
/// wait for something to arrive, and a row expecting one message must not stop at the first
/// one it sees and miss a second. More than one, and far fewer than [`LOOKS`], because what
/// these looks confirm is that nothing MORE is owed.
const CONFIRMING_LOOKS: usize = 3;

/// The handler every row runs: it sends one message to the queue the event names with the SDK
/// the runtime supplies, and reports either the message id Amazon SQS answered or the refusal
/// it answered — its code, its sentence and the HTTP status it came back on.
const SENDER_HANDLER: &str = r##"
import boto3
from botocore.exceptions import ClientError


def handler(event, context):
    client = boto3.client("sqs")
    try:
        answered = client.send_message(
            QueueUrl=event["url"], MessageBody=event["body"]
        )
        return {"ok": True, "id": bool(answered.get("MessageId"))}
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
"##;

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the queue it addresses is the queue this suite reads.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = aws_emulator_router::build_router(engine);
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service`, which is what routes
/// the request. Its credential is the long-term key every emulator publishes, so these calls
/// are the account's own and not the role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call of `service` at `host`, answered with the HTTP status and the XML body.
async fn query(
    router: &MultiServiceRouter,
    service: &str,
    host: &str,
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
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization(service))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call that must be served.
async fn cloudformation(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {action} must be served, got {status} with {body}"
    );
    body
}

/// One IAM call that must be served.
async fn iam(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(
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
        "case {case:?}: {action} must be served, got {status} with {body}"
    );
    body
}

/// One Amazon SQS operation over its `awsJson1_0` wire, made as the account's own credential —
/// the queue's owner, whose calls no row's grant decides.
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

/// One Amazon SQS operation that must be served.
async fn sqs_ok(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = sqs(router, operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must be served, got {status} with {answered}"
    );
    answered
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

/// The ARN the queue of this account in this Region carries, which is what a statement names
/// and what a refusal reports.
fn queue_arn() -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{QUEUE}")
}

/// The ARN of the role the function runs as, which is the spelling a resource policy names the
/// role itself by.
fn role_arn() -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{ROLE}")
}

/// The sentence Amazon SQS refuses a call with, for the document that withheld the grant.
/// Written out here rather than built from the service's own helper, so the wording has
/// something to be wrong about: the identity is the assumed-role SESSION a function's calls are
/// signed as, named for the function the session was opened for.
fn refusal_message(reason: &str) -> String {
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{ROLE}/{FUNCTION} is not authorized to \
         perform: {REPORTED_ACTION} on resource: {} {reason}",
        queue_arn()
    )
}

/// What one row does to the identity-based document the role carries, through IAM's own API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Identity {
    /// Leave in force whatever the row before it left.
    Unchanged,
    /// Write the inline document that allows the send on the queue (`iam:PutRolePolicy`).
    Granted,
    /// Take that document off the role again (`iam:DeleteRolePolicy`).
    Revoked,
}

/// What one row does to the queue's own resource-based document, through
/// `sqs:SetQueueAttributes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum QueuePolicy {
    /// Leave in force whatever the row before it left.
    Unchanged,
    /// Remove the attribute: "you can remove the … attribute by setting it to an empty
    /// string" (`SetQueueAttributesRequest$Attributes`).
    Cleared,
    /// Install a document stating `Effect` about `Principal` for the send on this queue.
    Stated(Effect, Principal),
}

/// The two effects the policy grammar has.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Effect {
    Allow,
    Deny,
}

impl Effect {
    fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "Allow",
            Self::Deny => "Deny",
        }
    }
}

/// WHO a row's queue-policy statement is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Principal {
    /// The role the function runs as, which is one of the spellings its session is named by.
    TheRole,
    /// A principal of another account, which this session is not.
    AnotherAccount,
}

impl Principal {
    fn arn(self) -> String {
        match self {
            Self::TheRole => role_arn(),
            Self::AnotherAccount => ANOTHER_ACCOUNT_PRINCIPAL.to_string(),
        }
    }
}

/// One row: what it changes before the invocation, and what the invocation then answers.
struct Row {
    case: &'static str,
    identity: Identity,
    queue_policy: QueuePolicy,
    /// Whether the send is served; `reason` is the one a refusal states, and is `None` exactly
    /// when the send is served.
    allowed: bool,
    reason: Option<&'static str>,
}

/// The rows, taken IN ORDER against the one function, the one role and the one queue: each
/// changes a document and asks the same invocation again, so what decides it is what is in
/// force at that instant and nothing the row before it was answered.
const ROWS: &[Row] = &[
    Row {
        case: "the role's own grant serves the send",
        identity: Identity::Granted,
        queue_policy: QueuePolicy::Unchanged,
        allowed: true,
        reason: None,
    },
    Row {
        case: "the grant taken off the role refuses the next send",
        identity: Identity::Revoked,
        queue_policy: QueuePolicy::Unchanged,
        allowed: false,
        reason: Some(NO_IDENTITY_GRANT),
    },
    Row {
        case: "the grant written again serves the send after it",
        identity: Identity::Granted,
        queue_policy: QueuePolicy::Unchanged,
        allowed: true,
        reason: None,
    },
    Row {
        case: "a queue policy denying the role refuses the send its own grant allows",
        identity: Identity::Unchanged,
        queue_policy: QueuePolicy::Stated(Effect::Deny, Principal::TheRole),
        allowed: false,
        reason: Some(RESOURCE_DENY),
    },
    Row {
        case: "a queue policy denying another account's principal refuses this role nothing",
        identity: Identity::Unchanged,
        queue_policy: QueuePolicy::Stated(Effect::Deny, Principal::AnotherAccount),
        allowed: true,
        reason: None,
    },
    Row {
        case: "the queue policy cleared leaves the role's own grant deciding",
        identity: Identity::Unchanged,
        queue_policy: QueuePolicy::Cleared,
        allowed: true,
        reason: None,
    },
    Row {
        case: "a queue policy allowing the role serves a send no document of its own allows",
        identity: Identity::Revoked,
        queue_policy: QueuePolicy::Stated(Effect::Allow, Principal::TheRole),
        allowed: true,
        reason: None,
    },
    Row {
        case: "a queue policy allowing another account's principal grants this role nothing",
        identity: Identity::Unchanged,
        queue_policy: QueuePolicy::Stated(Effect::Allow, Principal::AnotherAccount),
        allowed: false,
        reason: Some(NO_IDENTITY_GRANT),
    },
];

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

/// The inline document that grants the handler's send on the queue it addresses, and nothing
/// else.
fn send_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": SEND_ACTION,
            "Resource": queue_arn(),
        }],
    })
    .to_string()
}

/// The inline document granting the logs a function writes, which stays in force throughout so
/// that taking the send grant off the role takes nothing else with it.
fn logs_document() -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }],
    })
    .to_string()
}

/// The queue's own document stating `effect` about `principal` for the send on this queue.
fn queue_document(effect: Effect, principal: Principal) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": "the-row-statement",
            "Effect": effect.as_str(),
            "Principal": {"AWS": principal.arn()},
            "Action": SEND_ACTION,
            "Resource": queue_arn(),
        }],
    })
    .to_string()
}

/// Create the execution role where IAM holds it, carrying the log grant alone: the send grant
/// is the first row's own doing.
async fn given_role(router: &MultiServiceRouter) {
    iam(
        router,
        "setup",
        "CreateRole",
        &[
            ("RoleName", ROLE),
            ("AssumeRolePolicyDocument", &trust_policy()),
        ],
    )
    .await;
    iam(
        router,
        "setup",
        "PutRolePolicy",
        &[
            ("RoleName", ROLE),
            ("PolicyName", LOGS_POLICY),
            ("PolicyDocument", &logs_document()),
        ],
    )
    .await;
}

/// Create the queue through the service's own API and answer its url, asserting the ARN it
/// carries is the one every statement and every refusal names.
async fn given_queue(router: &MultiServiceRouter) -> String {
    let created = sqs_ok(
        router,
        "setup",
        "CreateQueue",
        &json!({
            "QueueName": QUEUE,
            "Attributes": {"VisibilityTimeout": VISIBILITY_TIMEOUT.to_string()},
        }),
    )
    .await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: CreateQueue must answer a url, got {created}"))
        .to_string();
    let attributes = sqs_ok(
        router,
        "setup",
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    assert_eq!(
        attributes["Attributes"]["QueueArn"].as_str(),
        Some(queue_arn().as_str()),
        "setup: the queue's ARN is what a statement and a refusal name, got {attributes}"
    );
    url
}

/// Deploy the one function the rows invoke. The inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all.
async fn given_function(router: &MultiServiceRouter) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Sender": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role_arn(),
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": SENDER_HANDLER},
                },
            },
        },
    })
    .to_string();
    cloudformation(
        router,
        "setup",
        "CreateStack",
        &[("StackName", STACK), ("TemplateBody", &template)],
    )
    .await;
    // The stack's completion is owed from the instant the owning service reports its resource
    // ready, which no fixed sleep can name: this looks until it reports it, on the shared wait
    // whose deadline is generous enough that a loaded host does not decide the outcome.
    let reported = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let events = cloudformation(
            router,
            "setup",
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
}

/// Wait until Lambda reports the function `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter) {
    let mut last = Value::Null;
    for _ in 0..LOOKS {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup: Lambda must report {FUNCTION:?} Active, last reported {last}");
}

/// Apply one row's change to the identity-based document in force on the role.
async fn apply_identity(router: &MultiServiceRouter, case: &str, change: Identity) {
    match change {
        Identity::Unchanged => {}
        Identity::Granted => {
            iam(
                router,
                case,
                "PutRolePolicy",
                &[
                    ("RoleName", ROLE),
                    ("PolicyName", SEND_POLICY),
                    ("PolicyDocument", &send_document()),
                ],
            )
            .await;
        }
        Identity::Revoked => {
            iam(
                router,
                case,
                "DeleteRolePolicy",
                &[("RoleName", ROLE), ("PolicyName", SEND_POLICY)],
            )
            .await;
        }
    }
}

/// Apply one row's change to the queue's own document, and answer what the queue reports as its
/// stored `Policy` afterwards — so a row whose document was never stored fails before its
/// invocation is judged.
async fn apply_queue_policy(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    change: QueuePolicy,
) -> Option<Value> {
    match change {
        QueuePolicy::Unchanged => {}
        QueuePolicy::Cleared => {
            sqs_ok(
                router,
                case,
                "SetQueueAttributes",
                &json!({"QueueUrl": url, "Attributes": {"Policy": ""}}),
            )
            .await;
        }
        QueuePolicy::Stated(effect, principal) => {
            sqs_ok(
                router,
                case,
                "SetQueueAttributes",
                &json!({
                    "QueueUrl": url,
                    "Attributes": {"Policy": queue_document(effect, principal)},
                }),
            )
            .await;
        }
    }
    let attributes = sqs_ok(
        router,
        case,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["Policy"]}),
    )
    .await;
    attributes["Attributes"]["Policy"]
        .as_str()
        .map(|document| {
            serde_json::from_str(document).unwrap_or_else(|_| {
                panic!("case {case:?}: the stored Policy must be a document, got {document}")
            })
        })
}

/// Take everything the queue still hands over, as the account's own credential, deleting each
/// message so the next row starts from an empty queue — and answer the bodies in the order they
/// came over.
///
/// `wanted` is how many the caller expects: the loop looks again until it has them or the
/// generous deadline passes, so a host that is slow to make a message available cannot decide
/// the outcome — and then takes [`CONFIRMING_LOOKS`] looks in all, so a send that enqueued
/// nothing, or enqueued more than the one message it answered for, is caught rather than raced
/// past by a single look.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<String> {
    let mut bodies = Vec::new();
    for look in 0..LOOKS {
        let answered = sqs_ok(
            router,
            case,
            "ReceiveMessage",
            &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
        )
        .await;
        let handed = answered["Messages"].as_array().cloned().unwrap_or_default();
        for message in &handed {
            bodies.push(
                message["Body"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a message with no body {message}"))
                    .to_string(),
            );
            sqs_ok(
                router,
                case,
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
        }
        if (bodies.len() >= wanted && look + 1 >= CONFIRMING_LOOKS) || look + 1 == LOOKS {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    bodies
}

/// The `X-Amz-Function-Error` an invocation carries, which is set exactly when the run failed.
fn function_error(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Every row: the same invocation of the same function is decided by the documents in force at
/// the instant it is made, and what the queue holds afterwards follows that decision.
#[tokio::test]
async fn an_in_function_send_follows_the_documents_in_force_at_each_call() {
    let router = build_router();
    given_role(&router).await;
    let url = given_queue(&router).await;
    given_function(&router).await;
    wait_until_active(&router).await;

    // Nothing is in force on the queue to begin with: the rows that state a document state it
    // against a queue that carried none.
    let before = apply_queue_policy(&router, "setup", &url, QueuePolicy::Unchanged).await;
    assert_eq!(
        before, None,
        "setup: a queue nobody wrote a policy to carries none, got {before:?}"
    );

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        apply_identity(&router, case, row.identity).await;
        let stored = apply_queue_policy(&router, case, &url, row.queue_policy).await;

        // The document the queue holds is the one this row installed — so a row whose decision
        // is wrong cannot be excused by a write that never landed.
        let wanted_statement = match row.queue_policy {
            QueuePolicy::Cleared => None,
            QueuePolicy::Stated(effect, principal) => Some((effect, principal)),
            // Whatever the row before it left in force, which is what the rows that change
            // nothing are about.
            QueuePolicy::Unchanged => ROWS[..index]
                .iter()
                .rev()
                .find_map(|earlier| match earlier.queue_policy {
                    QueuePolicy::Stated(effect, principal) => Some(Some((effect, principal))),
                    QueuePolicy::Cleared => Some(None),
                    QueuePolicy::Unchanged => None,
                })
                .flatten(),
        };
        match wanted_statement {
            None => assert_eq!(
                stored, None,
                "case {case:?}: expected the queue to carry no policy, got {stored:?}"
            ),
            Some((effect, principal)) => {
                let document = stored.clone().unwrap_or(Value::Null);
                assert_eq!(
                    document["Statement"][0]["Effect"].as_str(),
                    Some(effect.as_str()),
                    "case {case:?}: expected the stored policy to state {}, got {document}",
                    effect.as_str()
                );
                assert_eq!(
                    document["Statement"][0]["Principal"]["AWS"].as_str(),
                    Some(principal.arn().as_str()),
                    "case {case:?}: expected the stored policy to name {}, got {document}",
                    principal.arn()
                );
            }
        }

        let body = format!("live-policy-{index}");
        let event = json!({"url": url, "body": body});
        let (status, headers, payload) = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{FUNCTION}/invocations"),
            event.to_string().as_bytes(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the invocation must be served, got {status} with {payload}"
        );
        assert_eq!(
            function_error(&headers),
            None,
            "case {case:?}: a handler that catches the refusal runs to completion, got {:?} with \
             {payload}",
            function_error(&headers)
        );

        // What the handler reports is what Amazon SQS answered its call with.
        let expected = match row.reason {
            None => {
                assert!(
                    row.allowed,
                    "case {case:?}: a row that states no refusal reason is a served one"
                );
                json!({"ok": true, "id": true})
            }
            Some(reason) => {
                assert!(
                    !row.allowed,
                    "case {case:?}: a row that states a refusal reason is a refused one"
                );
                json!({
                    "ok": false,
                    "code": ACCESS_DENIED,
                    "message": refusal_message(reason),
                    "status": ACCESS_DENIED_STATUS,
                })
            }
        };
        assert_eq!(
            payload, expected,
            "case {case:?}: expected the handler to report {expected}, got {payload}"
        );

        // And what the QUEUE holds: a served send is in it, a refused one enqueued nothing.
        let expected_bodies: Vec<String> = if row.allowed {
            vec![body.clone()]
        } else {
            Vec::new()
        };
        let held = drained(&router, case, &url, expected_bodies.len()).await;
        assert_eq!(
            held, expected_bodies,
            "case {case:?}: expected the queue to hold {expected_bodies:?}, got {held:?}"
        );
    }
}
