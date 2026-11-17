// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING Python function's own `SendMessage` is decided by: the policies in force on
//! the queue it addresses, the queue's OWN resource-based policy included.
//!
//! RULE: a managed `python3.x` runtime puts the AWS SDK on the handler's import path, so a
//! package carrying only the handler module may `import boto3` and call another service; the
//! call is made as a session of the function's execution role, and the service serving it
//! decides it with the SAME policy evaluation it applies to any other request — so the
//! addressed queue's own `Policy` decides it too: a statement in that document naming the
//! calling session, or the IAM role behind it, grants the send even where no identity-based
//! policy does; an explicit `Deny` in it refuses the send whatever the role is granted; and a
//! document naming some other principal grants this session nothing. A served send answers the
//! handler the message's own id and is in the queue afterwards; a refused one is
//! `AccessDenied` (HTTP 403) naming the assumed-role session, the action and the queue's ARN,
//! and leaves the queue exactly as it was. A refusal the handler does not catch is the
//! FUNCTION's failure, not the `Invoke` call's: the invocation is still answered HTTP 200,
//! carrying `X-Amz-Function-Error: Unhandled` and the run's error document, whose `errorType`
//! is the SDK exception class raised — the class the service's model declares for that error
//! shape where it declares one, and the SDK's own base class where it declares none, as the
//! AWS-common refusals no model carries are raised — and whose `errorMessage` carries the
//! sentence the SDK renders for that refusal.
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`), and the identity such a call speaks for is the session
//! `arn:aws:sts::<account>:assumed-role/<role>/<function>`. A request is allowed when an
//! identity-based policy allows it OR the resource's own policy names the caller and allows
//! it, and an explicit `Deny` in either refuses it (IAM User Guide, *Policy evaluation
//! logic*). Amazon SQS declares no authorization error for these operations, so the refusal is
//! the AWS-common one, worded as that service words it.
//!
//! Five crates have to agree for that to hold — CloudFormation deploys the queues, the roles
//! and the senders together, IAM holds each role and its inline document, STS mints the
//! session the handler's calls are signed with, Lambda runs the handler and supplies it the
//! SDK, and Amazon SQS takes the decision — so the suite lives here, where a crate-local one
//! could only see one side of it. What the neighbours pin and this one does not repeat:
//! `lambda_in_function_sqs_authorization` drives the decision taken on the execution role's
//! own IDENTITY-based documents, and the Amazon SQS crate's `queue_policy_authorization`
//! drives a queue's resource-based policy for a credential a client assumed directly. What
//! only this suite reaches is a queue's own policy deciding a send a FUNCTION made, which is
//! how a queue owned by one estate takes writes from another's relay.
//!
//! The rows differ along what the rule generalises over: WHAT the queue's own document states
//! about the caller (nothing at all, an allow naming the session, an allow naming the role
//! behind it, an allow naming another account's principal, a deny naming the session), WHAT
//! the execution role's own document grants beside it (nothing of the service, the send on the
//! queue, every action of the service), WHETHER the queue the send addresses is there at all —
//! which is the refusal the service's model DOES declare an error shape for — and whether the
//! handler catches the refusal. Each probe asserts both halves — what the handler reports of
//! the call, and what the QUEUE holds afterwards, down to the id the send answered — so a
//! refusal that still enqueued, and a grant that answered without enqueuing, both fail here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode, physical_ids_of};

/// The Region every request here is signed for, and so the Region the queues and the functions
/// belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every queue ARN and every session ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// A principal of ANOTHER account, which no session of this one is: the spelling one row's
/// queue policy grants instead of the caller.
const OTHER_ACCOUNT_PRINCIPAL: &str = "arn:aws:iam::210987654321:root";

/// The stack the queues, the roles and the sender functions are deployed by.
const STACK: &str = "xsvc-fn-queue-policy-send";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The IAM action the SDK call every probe makes performs, which is what a statement grants and
/// what a refusal names.
const SEND_ACTION: &str = "sqs:SendMessage";

/// The code and status Amazon SQS refuses a call the policies in force do not permit with.
const ACCESS_DENIED: &str = "AccessDenied";
const ACCESS_DENIED_STATUS: u64 = 403;

/// The reason an IMPLICIT deny states: the caller's own documents allow the action on nothing,
/// so what the refusal names is the action no policy of theirs allows.
const NO_IDENTITY_GRANT: &str = "because no identity-based policy allows the sqs:sendmessage \
                                 action";

/// The reason an explicit `Deny` in the QUEUE's own document states.
const RESOURCE_DENY: &str = "with an explicit deny in a resource-based policy";

/// The signal an invocation whose handler raised carries, and the class the SDK raises for a
/// refusal — which is therefore what the run's error document names as the type raised.
const UNHANDLED: &str = "Unhandled";
const CLIENT_ERROR: &str = "ClientError";

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the authorization decision and not how fast
/// an interpreter gets through the handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a created function reaching
/// `Active`, a message the queue is owed — and how many looks to take. The spans are those
/// services' own, so no fixed sleep names the instant one ends, and the deadline is generous
/// enough that a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// The handler every row runs: it imports the SDK the runtime supplies at MODULE scope and
/// builds its client there — so a row whose runtime made no SDK importable could not run at all
/// — and sends one message to the queue the event names, either reporting the refusal Amazon SQS
/// answered or leaving it to propagate, which is what makes the invocation report a function
/// error.
const SENDER_HANDLER: &str = r##"
import boto3
from botocore.exceptions import ClientError

CLIENT = boto3.client("sqs")


def send(event):
    answered = CLIENT.send_message(QueueUrl=event["url"], MessageBody=event["body"])
    return {"ok": True, "id": answered["MessageId"]}


def handler(event, context):
    if not event["catch"]:
        return send(event)
    try:
        return send(event)
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
/// the queue's owner, whose calls no row's policy refuses.
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

/// What the wire answers a send to a queue that is NOT THERE: the error SHAPE it named, the code
/// a client of this service reports it under, the sentence it carried and its status.
///
/// Read off the wire rather than written out here, and it says the same thing to every caller:
/// such a refusal names no principal and no policy decides it, so the SAME request made by the
/// queue's owner answers what a function's own SDK call is answered. The shape is the class an
/// SDK has for the refusal, and the code the one its model keys that class by — which for a
/// query-compatible service is the LEGACY code the answer carries beside the shape, so this
/// request opts into that vocabulary the way every SDK client of such a service does
/// (`x-amzn-query-mode`).
async fn absent_queue_refusal(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
) -> (String, String, String, u64) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("authorization", authorization("sqs"))
        .header("x-amz-target", "AmazonSQS.SendMessage")
        .header("x-amzn-query-mode", "true")
        .body(Bytes::from(
            json!({"QueueUrl": url, "MessageBody": "nowhere"}).to_string(),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let status = parts.status.as_u16();
    let answered: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let header = |name: &str| {
        parts
            .headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    // The shape, as the answer names it: in the header where it carries one, and otherwise in
    // the `__type` member an AWS JSON refusal carries it in.
    let named = match header("x-amzn-errortype") {
        empty if empty.is_empty() => answered["__type"].as_str().unwrap_or_default().to_string(),
        carried => carried,
    };
    let shape = named
        .rsplit('#')
        .next()
        .unwrap_or_default()
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string();
    // `<code>;<fault>`: the code is the part a client reports, and the fault is the one the
    // status already says.
    let legacy = header("x-amzn-query-error");
    let code = legacy
        .split(';')
        .next()
        .map(str::trim)
        .filter(|code| !code.is_empty())
        .unwrap_or(&shape)
        .to_string();
    let message = answered["message"]
        .as_str()
        .or_else(|| answered["Message"].as_str())
        .unwrap_or_default()
        .to_string();
    assert!(
        !shape.is_empty() && !message.is_empty(),
        "setup for {case:?}: a send to {url:?} must be refused with a named shape and a \
         sentence, got {status} {answered} with {:?}",
        parts.headers
    );
    (shape, code, message, u64::from(status))
}

/// WHICH principal a row's queue policy names in its one statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Named {
    /// The assumed-role session the function's own calls are signed as.
    Session,
    /// The IAM role that session is a session of.
    Role,
    /// A principal of another account, which this session is not.
    OtherAccount,
}

/// One probe against a row's deployed queue, role and sender.
struct Probe {
    case: &'static str,
    /// Whether the handler catches the refusal. A probe that does not is what pins the function
    /// error an uncaught SDK exception makes of the run.
    catches: bool,
    /// The reason the refusal states, or `None` where the send must be served.
    reason: Option<&'static str>,
    /// Whether the send addresses a queue that is NOT THERE, which is the refusal whose code
    /// the service's model DOES declare an error shape for — so the class it is raised as is
    /// that shape's, where an authorization refusal has none of its own.
    absent: bool,
}

/// One row: what the execution role's own document grants, what the queue's own document states,
/// and the probes taken against the pair.
struct Row {
    /// The action the role's one inline statement allows on the row's queue, beside the log
    /// actions every execution role carries; `None` for a role granted nothing of this service.
    role_grant: Option<&'static str>,
    /// The principal the row's queue names in its own `Policy`, and the effect that statement
    /// states about it; `None` for a queue carrying no policy at all.
    policy: Option<(Named, &'static str)>,
    probes: &'static [Probe],
}

const ROWS: &[Row] = &[
    // The queue's own document is the only grant there is: a role granted nothing of this
    // service still sends, because a resource-based policy naming the caller allows the call on
    // its own.
    Row {
        role_grant: None,
        policy: Some((Named::Session, "Allow")),
        probes: &[Probe {
            case: "the queue's policy names the session the function's calls are signed as",
            catches: true,
            reason: None,
            absent: false,
        }],
    },
    // And the role BEHIND the session is the other spelling that names the same caller.
    Row {
        role_grant: None,
        policy: Some((Named::Role, "Allow")),
        probes: &[Probe {
            case: "the queue's policy names the role the session belongs to",
            catches: true,
            reason: None,
            absent: false,
        }],
    },
    // A document that grants somebody else grants this caller nothing, so the refusal is IAM's
    // implicit deny — and the same refusal, left uncaught, is the run's own failure.
    Row {
        role_grant: None,
        policy: Some((Named::OtherAccount, "Allow")),
        probes: &[
            Probe {
                case: "the queue's policy grants another account's principal, not this session",
                catches: true,
                reason: Some(NO_IDENTITY_GRANT),
                absent: false,
            },
            Probe {
                case: "that refusal, left uncaught, is the function's own error",
                catches: false,
                reason: Some(NO_IDENTITY_GRANT),
                absent: false,
            },
        ],
    },
    // An explicit deny in the queue's document refuses a role granted every action of the
    // service: no grant overturns one.
    Row {
        role_grant: Some("sqs:*"),
        policy: Some((Named::Session, "Deny")),
        probes: &[Probe {
            case: "the queue's policy denies the session a role granted every action",
            catches: true,
            reason: Some(RESOURCE_DENY),
            absent: false,
        }],
    },
    // The control the rule is stated against, and the pair a least-privilege relay is written
    // as: the role's own grant serves the send where the queue states no policy at all, whether
    // or not the handler is ready to catch a refusal.
    Row {
        role_grant: Some(SEND_ACTION),
        policy: None,
        probes: &[
            Probe {
                case: "the role's own grant sends where the queue states no policy",
                catches: true,
                reason: None,
                absent: false,
            },
            Probe {
                case: "a served send leaves the invocation carrying no function error",
                catches: false,
                reason: None,
                absent: false,
            },
            // The other side of the class an uncaught refusal is reported under: a refusal the
            // service's model DOES declare an error shape for is raised as that shape's own
            // class, where an authorization refusal — which no service's model declares — is
            // raised as the SDK's base class.
            Probe {
                case: "a send to a queue that is not there, caught, is the service's own \
                       modelled refusal",
                catches: true,
                reason: None,
                absent: true,
            },
            Probe {
                case: "that modelled refusal, left uncaught, names its own class",
                catches: false,
                reason: None,
                absent: true,
            },
        ],
    },
    // And the other half of that pair: the same least-privilege role, on a queue it was granted
    // nothing on, sends nothing.
    Row {
        role_grant: None,
        policy: None,
        probes: &[Probe {
            case: "a role granted only the log actions sends nothing",
            catches: true,
            reason: Some(NO_IDENTITY_GRANT),
            absent: false,
        }],
    },
];

/// The queue, the role and the sender one row is deployed with. The refusal's own sentence names
/// the last two: the identity a function's call is signed as is the session
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn queue_name(index: usize) -> String {
    format!("xsvc-fn-queue-policy-{index}")
}
fn role_name(index: usize) -> String {
    format!("xsvc-fn-queue-policy-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-fn-queue-policy-sender-{index}")
}

/// The sentence Amazon SQS refuses a send the policies in force do not permit with. Written out
/// here rather than built from the service's own helper, so the wording has something to be
/// wrong about.
///
/// The action is named case-folded, which is how this service words a refusal: the case a policy
/// or the model spells a name in is not part of the sentence.
fn refusal_message(index: usize, queue_arn: &str, reason: &str) -> String {
    format!(
        "User: arn:aws:sts::{ACCOUNT}:assumed-role/{}/{} is not authorized to perform: {} on \
         resource: {queue_arn} {reason}",
        role_name(index),
        function_name(index),
        SEND_ACTION.to_lowercase(),
    )
}

/// The sentence the SDK renders for a refusal it raises, which is what an uncaught one leaves in
/// the run's error document.
fn sdk_sentence(code: &str, message: &str) -> String {
    format!("An error occurred ({code}) when calling the SendMessage operation: {message}")
}

/// The trust policy a Lambda execution role needs: Lambda has to be able to assume it for the
/// function to be handed it at all.
fn trust_policy() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": {"Service": "lambda.amazonaws.com"},
            "Action": "sts:AssumeRole",
        }],
    })
}

/// The inline document one row's role carries: the log actions every execution role needs, and
/// the row's own statement where it has one, scoped to that row's queue.
fn role_document(row: &Row, index: usize) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    if let Some(action) = row.role_grant {
        statements.push(json!({
            "Effect": "Allow",
            "Action": [action],
            "Resource": format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{}", queue_name(index)),
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements})
}

/// The template the suite deploys: one queue, one execution role carrying that row's document and
/// one sender function running as it, per row. The inline source is archived into the deployment
/// package by CloudFormation, which is how a function whose handler is written here comes to
/// exist at all.
fn stack_template() -> String {
    let mut resources = serde_json::Map::new();
    for (index, row) in ROWS.iter().enumerate() {
        resources.insert(
            format!("Queue{index}"),
            json!({
                "Type": "AWS::SQS::Queue",
                "Properties": {"QueueName": queue_name(index)},
            }),
        );
        let role_logical = format!("Role{index}");
        resources.insert(
            role_logical.clone(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": role_name(index),
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "sender-grants",
                        "PolicyDocument": role_document(row, index),
                    }],
                },
            }),
        );
        resources.insert(
            format!("Sender{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [role_logical, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": SENDER_HANDLER},
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
/// resource.
async fn deploy(router: &MultiServiceRouter) -> BTreeMap<String, String> {
    let template = stack_template();
    let (status, answered) = query(
        router,
        "cloudformation",
        "2010-05-15",
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares named IAM roles, which a create acknowledges before
            // CloudFormation provisions them.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    // The stack's completion is owed from the instant the owning services report its resources
    // ready, which no fixed sleep can name: this looks until it reports it, on the shared wait
    // whose deadline is generous enough that a loaded host does not decide the outcome.
    let reported = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
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

/// The url and the ARN Amazon SQS reports for the queue `name`, read out of that service rather
/// than spelled here: the ARN is what a statement names and what a refusal reports.
async fn resolved_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let (status, answered) = sqs(router, "GetQueueUrl", &json!({"QueueName": name})).await;
    assert_eq!(
        status, 200,
        "setup: GetQueueUrl({name:?}) must succeed, got {status} with {answered}"
    );
    let url = answered["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: GetQueueUrl({name:?}) must answer a url, got {answered}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetQueueAttributes({name:?}) must succeed, got {status} with {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: the queue {name:?} must report its ARN, got {attributes}"))
        .to_string();
    (url, arn)
}

/// The ARN IAM holds for the role `name`, which is the spelling a statement naming the role
/// behind a session has to carry.
async fn role_arn(router: &MultiServiceRouter, name: &str) -> String {
    let (status, answered) = query(
        router,
        "iam",
        "2010-05-08",
        "GetRole",
        &[("RoleName", name)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetRole({name:?}) must succeed, got {status} with {answered}"
    );
    element(&answered, "Arn")
}

/// The document one row puts on its queue: one statement about the principal the row names.
fn queue_policy(named: Named, effect: &str, principal: &str, queue_arn: &str) -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Sid": format!("InFunctionSend{named:?}"),
            "Effect": effect,
            "Principal": {"AWS": principal},
            "Action": SEND_ACTION,
            "Resource": queue_arn,
        }],
    })
}

/// Put `document` on the queue `url` as the queue's OWNER, which is the caller a queue's policy
/// is written by.
async fn set_queue_policy(router: &MultiServiceRouter, case: &str, url: &str, document: &Value) {
    let (status, answered) = sqs(
        router,
        "SetQueueAttributes",
        &json!({"QueueUrl": url, "Attributes": {"Policy": document.to_string()}}),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup for {case:?}: SetQueueAttributes must store the queue's policy, got {status} with \
         {answered}"
    );
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
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

/// Take everything the queue still hands over, as the account's own credential, deleting each
/// message so the next probe starts from an empty queue — and answer the `(body, id)` of each in
/// the order they came over.
///
/// `wanted` is how many the caller expects: the loop looks again until it has them or the
/// generous deadline passes, so a host that is slow to make a message available cannot decide the
/// outcome, and a caller expecting none takes one look and reports what it found.
async fn drained(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> Vec<(String, String)> {
    let mut held = Vec::new();
    for look in 0..LOOKS {
        let (status, answered) = sqs(
            router,
            "ReceiveMessage",
            &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: draining the queue must succeed, got {status} with {answered}"
        );
        let handed = answered["Messages"].as_array().cloned().unwrap_or_default();
        for message in &handed {
            let body = message["Body"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a message with no body {message}"))
                .to_string();
            let id = message["MessageId"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a message with no id {message}"))
                .to_string();
            held.push((body, id));
            let (status, deleted) = sqs(
                router,
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: deleting a drained message must succeed, got {status} with \
                 {deleted}"
            );
        }
        if held.len() >= wanted || look + 1 == LOOKS {
            break;
        }
        tokio::time::sleep(LOOK).await;
    }
    held
}

/// The `X-Amz-Function-Error` an invocation carries, which is set exactly when the run failed.
fn function_error(headers: &http::HeaderMap) -> Option<String> {
    headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
}

/// Every probe: a send a running function makes with the SDK its runtime supplies is decided by
/// the policies in force on the queue — the queue's OWN document included — and what the queue
/// holds afterwards follows that decision.
#[tokio::test]
async fn an_in_function_send_is_decided_by_the_queues_own_policy_too() {
    let router = build_router();
    let physical = deploy(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let sender_logical = format!("Sender{index}");
        let sender = physical.get(&sender_logical).cloned().unwrap_or_else(|| {
            panic!("setup: the stack must report a physical id for {sender_logical}")
        });
        assert_eq!(
            sender,
            function_name(index),
            "setup: the stack must name the function {:?}, got {sender:?}",
            function_name(index)
        );
        let (url, arn) = resolved_queue(&router, &queue_name(index)).await;
        assert_eq!(
            physical.get(&format!("Queue{index}")).map(String::as_str),
            Some(url.as_str()),
            "setup: the stack must report Queue{index} as the queue Amazon SQS holds, got {:?}",
            physical.get(&format!("Queue{index}"))
        );

        if let Some((named, effect)) = row.policy {
            let principal = match named {
                // The session a function's own calls are signed as, in the form STS reports it.
                Named::Session => format!(
                    "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
                    role_name(index),
                    function_name(index)
                ),
                Named::Role => role_arn(&router, &role_name(index)).await,
                Named::OtherAccount => OTHER_ACCOUNT_PRINCIPAL.to_string(),
            };
            set_queue_policy(
                &router,
                row.probes[0].case,
                &url,
                &queue_policy(named, effect, &principal, &arn),
            )
            .await;
        }

        wait_until_active(&router, &sender).await;

        for (probe_index, probe) in row.probes.iter().enumerate() {
            let case = probe.case;
            let body = format!("sent-{index}-{probe_index}");
            // What the probe's send ADDRESSES: the row's own queue, or a url naming a queue that
            // is not there.
            let absent_url = format!("{url}-absent");
            let addressed = if probe.absent { &absent_url } else { &url };
            let (status, headers, payload) = lambda(
                &router,
                "POST",
                &format!("/2015-03-31/functions/{sender}/invocations"),
                json!({"url": addressed, "body": body, "catch": probe.catches})
                    .to_string()
                    .as_bytes(),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the invocation must be served, got {status} with {payload}"
            );

            let mut sent_id = None;
            if probe.absent {
                // The refusal of a queue that is not there, held to what the SAME request
                // answers on the wire: the shape the service named, the code a client of it
                // reports and the sentence it carried, so this probe states none of its own.
                let (shape, code, message, refused_status) =
                    absent_queue_refusal(&router, case, addressed).await;
                if probe.catches {
                    assert_eq!(
                        function_error(&headers),
                        None,
                        "case {case:?}: a handler that catches the refusal runs to completion, \
                         got {:?} with {payload}",
                        function_error(&headers)
                    );
                    let expected = json!({
                        "ok": false,
                        "code": code,
                        "message": message,
                        "status": refused_status,
                    });
                    assert_eq!(
                        payload, expected,
                        "case {case:?}: expected the handler to report {expected}, got {payload}"
                    );
                } else {
                    assert_eq!(
                        function_error(&headers).as_deref(),
                        Some(UNHANDLED),
                        "case {case:?}: an uncaught refusal must surface as the function's own \
                         error, got {:?} with {payload}",
                        function_error(&headers)
                    );
                    assert_eq!(
                        payload["errorType"],
                        json!(shape),
                        "case {case:?}: the error document must name the class the service's \
                         model declares for this refusal ({shape:?}), got {payload}"
                    );
                    let wanted = sdk_sentence(&code, &message);
                    assert_eq!(
                        payload["errorMessage"],
                        json!(wanted),
                        "case {case:?}: the error document must carry the sentence the SDK \
                         renders ({wanted:?}), got {payload}"
                    );
                }
            } else {
                match (probe.reason, probe.catches) {
                    // A served send: the handler ran to completion and answered the id Amazon
                    // SQS minted for the message, whether or not it was ready to catch a
                    // refusal.
                    (None, _) => {
                        assert_eq!(
                            function_error(&headers),
                            None,
                            "case {case:?}: a served send fails no run, got {:?} with {payload}",
                            function_error(&headers)
                        );
                        assert_eq!(
                            payload["ok"],
                            json!(true),
                            "case {case:?}: expected the handler's own report of a served send, \
                             got {payload}"
                        );
                        let id = payload["id"].as_str().unwrap_or_default().to_string();
                        assert!(
                            !id.is_empty(),
                            "case {case:?}: a served send answers the message's own id, got \
                             {payload}"
                        );
                        sent_id = Some(id);
                    }
                    // A refusal the handler caught: what it reports is what Amazon SQS answered.
                    (Some(reason), true) => {
                        assert_eq!(
                            function_error(&headers),
                            None,
                            "case {case:?}: a handler that catches the refusal runs to \
                             completion, got {:?} with {payload}",
                            function_error(&headers)
                        );
                        let expected = json!({
                            "ok": false,
                            "code": ACCESS_DENIED,
                            "message": refusal_message(index, &arn, reason),
                            "status": ACCESS_DENIED_STATUS,
                        });
                        assert_eq!(
                            payload, expected,
                            "case {case:?}: expected the handler to report {expected}, got \
                             {payload}"
                        );
                    }
                    // A refusal left to propagate: the RUN failed, so the invocation carries the
                    // function error and the run's own error document names the class the SDK
                    // raised — for an authorization refusal, which no service's model declares
                    // an error shape for, the SDK's own base class.
                    (Some(reason), false) => {
                        assert_eq!(
                            function_error(&headers).as_deref(),
                            Some(UNHANDLED),
                            "case {case:?}: an uncaught refusal must surface as the function's \
                             own error, got {:?} with {payload}",
                            function_error(&headers)
                        );
                        assert_eq!(
                            payload["errorType"],
                            json!(CLIENT_ERROR),
                            "case {case:?}: the error document must name the class the SDK \
                             raised, got {payload}"
                        );
                        let wanted =
                            sdk_sentence(ACCESS_DENIED, &refusal_message(index, &arn, reason));
                        assert_eq!(
                            payload["errorMessage"],
                            json!(wanted),
                            "case {case:?}: the error document must carry the sentence the SDK \
                             renders ({wanted:?}), got {payload}"
                        );
                    }
                }
            }

            // And what the QUEUE holds: a served send is in it under the id it answered, and a
            // refused one enqueued nothing at all.
            let expected = match &sent_id {
                Some(id) => vec![(body.clone(), id.clone())],
                None => Vec::new(),
            };
            let held = drained(&router, case, &url, expected.len()).await;
            assert_eq!(
                held, expected,
                "case {case:?}: expected the queue to hold {expected:?}, got {held:?}"
            );
        }
    }
}
