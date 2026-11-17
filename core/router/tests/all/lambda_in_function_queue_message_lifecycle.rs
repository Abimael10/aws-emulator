// SPDX-License-Identifier: CC0-1.0

//! The queue message LIFECYCLE a running Lambda handler drives through the AWS SDK its
//! managed runtime supplies, and the rule it reads through the same SDK.
//!
//! RULE: a managed `python3.x` runtime carries the AWS SDK for Python on the handler's import
//! path, so `import boto3` at module scope succeeds and every client method the handler calls
//! is dispatched to the service that owns the operation, as a session of the function's
//! execution role: the owning service decides it against the policies in force on that role,
//! serves what they allow — the message the handler sent is the message its own receive hands
//! back, a message's lock becomes the one its visibility call names counted from that call, and
//! the message it deletes is gone from the queue — and refuses what they do not with its OWN
//! access-denied refusal, naming the session, the action and the resource, leaving the queue
//! exactly as it was. The handler catches the refusal, so the invocation is HTTP 200 with no
//! `X-Amz-Function-Error` header either way, carrying the handler's own report.
//!
//! Five crates have to agree for that to hold — CloudFormation deploys the queues, the roles
//! and the consumer functions, IAM holds each role and its document, the token service mints
//! the session the handler's calls are signed with, Lambda runs the handler and supplies it the
//! SDK, and Amazon SQS and Amazon EventBridge take the decisions and do the work — so the
//! suite lives here. What the neighbours pin and this one does not repeat:
//! `lambda_in_function_sqs_authorization` drives the decision for a send, a receive and a name
//! resolution, and the EventBridge crate's `handler_execution_role_publication` drives a
//! handler's own `PutEvents`. What only this suite reaches is the rest of the message
//! lifecycle a consumer function performs — a batch send, the receipt handles its own receive
//! minted, the visibility lock it hands back, shortens and extends while it works, and the
//! delete that ends a message — together with a rule read on the bus, each decided against the
//! role the function runs as.
//!
//! The rows differ along what the rule generalises over: what the execution role's document
//! allows (each lifecycle action named one by one, every action of each service through a
//! wildcard, or only the send and the receive) and which resource those statements name (every
//! resource, or the queue and the rule the handler addresses) — and so along which of the
//! handler's calls are served and which are refused. Each row asserts both halves: what the
//! handler reports of every call it made, and what the QUEUE holds afterwards, read through
//! Amazon SQS's own API, so a call that answered without doing the work, and one that did the
//! work after being refused, both fail here.
//!
//! Every lock a message here ends up under is one a CALL set, never one the handler's own
//! runtime raced: the receive takes a lock far longer than any run, so what the queue holds at
//! the end follows from the visibility calls and the delete alone — no assertion turns on how
//! fast the host ran the interpreter. The one short span is the lock the handler leaves two
//! messages under, which is the span this suite tests; it is waited out by looking for the
//! CONDITION (those messages becoming visible) on a generous deadline.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode};

/// The Region every request here is signed for, and so the Region the queues, the functions
/// and the rule belong to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack the queues, the roles and the consumer functions are deployed by.
const STACK: &str = "xsvc-fn-lifecycle";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The rule the handler reads through the SDK, and the pattern it carries. Nothing is
/// published onto it here: what is under test is the READ a handler makes of it.
const RULE: &str = "xsvc-fn-lifecycle-rule";
const RULE_SOURCE: &str = "com.demo.xsvc-fn-lifecycle";
const RULE_STATE: &str = "ENABLED";

/// The `VisibilityTimeout` every declared queue carries, in seconds: the longest a queue may
/// be configured for ("an integer from 0 to 43,200 (12 hours)",
/// `SetQueueAttributesRequest$Attributes`). No assertion here rests on it — every receive this
/// suite and its handler make names its own `VisibilityTimeout` — so the queue's own default
/// can never end a lock while the suite runs.
const QUEUE_VISIBILITY_TIMEOUT: u64 = 43_200;

/// The lock the handler's own receive takes on every message it is handed, in seconds: far
/// longer than any run of the handler, so nothing it does afterwards races a lapse and what the
/// queue holds at the end is decided by the calls the handler made and nothing else.
const RECEIVE_HOLD: u64 = 3_600;

/// The brief lock the handler leaves two messages under, in seconds — the one span this suite
/// tests, and so the only tight value here. It is set by a call of its own AFTER the receive,
/// so no part of the handler's own runtime falls inside it.
const BRIEF_HOLD: u64 = 2;

/// What the handler extends one briefly-locked message to, in seconds: far past the span its
/// sibling under the same brief lock lapses in, so that message staying in flight is the
/// extension and nothing else.
const EXTENDED_HOLD: u64 = 3_600;

/// The bodies the handler's batch send puts on the queue, and what it does with each: it hands
/// the first back at once, deletes the second once it has processed it, shortens the third's
/// lock to the brief span and leaves it to lapse, and puts the fourth under that same brief
/// span before extending it.
const BODIES: [&str; 4] = ["alpha", "beta", "gamma", "delta"];
const RELEASED_BODY: &str = BODIES[0];
const DELETED_BODY: &str = BODIES[1];
const SHORTENED_BODY: &str = BODIES[2];
const EXTENDED_BODY: &str = BODIES[3];

/// How many receives the handler may take to collect a handle for every message it sent. A
/// bound on the loop and not a wait: nothing in the guest sleeps, so a generous bound costs
/// the run nothing, and how many messages each look handed back is reported so a look that
/// came back short names itself.
const HANDLER_LOOKS: u64 = 60;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is the work the handler's calls do and not
/// how fast an interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look while waiting for work the clock owes — a function reaching `Active`, a
/// message whose brief lock has lapsed — and how long to keep looking. The deadline is
/// generous, so a loaded host cannot decide the outcome.
const LOOK: Duration = Duration::from_millis(100);
const DEADLINE: Duration = Duration::from_secs(120);

/// The code and status Amazon SQS refuses a call the policies in force do not permit with.
const SQS_ACCESS_DENIED: &str = "AccessDenied";
const SQS_ACCESS_DENIED_STATUS: u64 = 403;

/// The code Amazon EventBridge refuses such a call with.
const EVENTS_ACCESS_DENIED: &str = "AccessDeniedException";

/// The IAM action each call the handler makes performs, which is the action a statement grants
/// and the one a refusal names.
const CHANGE_VISIBILITY_ACTION: &str = "sqs:ChangeMessageVisibility";
const DELETE_MESSAGE_ACTION: &str = "sqs:DeleteMessage";
const DESCRIBE_RULE_ACTION: &str = "events:DescribeRule";

/// The handler every row runs. It drives the whole lifecycle with the SDK the runtime
/// supplies — a batch send, receives until it holds a handle for each message, then one
/// visibility call per message and a delete — and reads the rule on the bus, reporting what
/// each call answered and catching the refusals, so the invocation carries no function error
/// whichever way the calls went.
const CONSUMER_HANDLER: &str = r##"
import boto3
from botocore.exceptions import ClientError


def attempt(call):
    try:
        answered = call()
    except ClientError as refused:
        return {
            "ok": False,
            "code": refused.response["Error"]["Code"],
            "message": refused.response["Error"]["Message"],
            "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
        }
    return {
        "ok": True,
        "status": answered["ResponseMetadata"]["HTTPStatusCode"],
        "answered": {
            key: value for key, value in answered.items() if key != "ResponseMetadata"
        },
    }


def handler(event, context):
    queues = boto3.client("sqs")
    url = event["url"]
    bodies = event["bodies"]
    report = {}
    report["sent"] = attempt(
        lambda: queues.send_message_batch(
            QueueUrl=url,
            Entries=[
                {"Id": "e%d" % position, "MessageBody": body}
                for position, body in enumerate(bodies)
            ],
        )
    )

    handles = {}
    ids = {}
    looks = []
    taken = {"ok": False, "looked": False}
    for _ in range(event["looks"]):
        taken = attempt(
            lambda: queues.receive_message(
                QueueUrl=url,
                MaxNumberOfMessages=10,
                VisibilityTimeout=event["receive_hold"],
            )
        )
        if not taken["ok"]:
            break
        handed = taken["answered"].get("Messages", [])
        looks.append(len(handed))
        for message in handed:
            handles[message["Body"]] = message["ReceiptHandle"]
            ids[message["Body"]] = message["MessageId"]
        if len(handles) >= len(bodies):
            break
    report["received"] = taken
    report["looks"] = looks
    report["ids"] = ids
    report["handled"] = sorted(handles)

    def on_handle(body, call):
        handle = handles.get(body)
        if handle is None:
            return {"ok": False, "no_handle": body}
        return attempt(lambda: call(handle))

    def visibility(body, timeout):
        return on_handle(
            body,
            lambda handle: queues.change_message_visibility(
                QueueUrl=url, ReceiptHandle=handle, VisibilityTimeout=timeout
            ),
        )

    # The message handed straight back: a timeout of zero ends the lock at once.
    report["released"] = visibility(event["release"], 0)
    # The message this run is done with.
    report["deleted"] = on_handle(
        event["delete"],
        lambda handle: queues.delete_message(QueueUrl=url, ReceiptHandle=handle),
    )
    # The message whose lock is cut to the brief span, and left to lapse.
    report["shortened"] = visibility(event["shorten"], event["brief"])
    # The message put under that same brief span and then extended past it, which is what a
    # consumer still working on a message does.
    report["briefed"] = visibility(event["extend"], event["brief"])
    report["extended"] = visibility(event["extend"], event["extension"])

    report["described"] = attempt(
        lambda: boto3.client("events").describe_rule(Name=event["rule"])
    )
    return report
"##;

/// WHICH resource a row's statement names.
#[derive(Debug, Clone, Copy)]
enum Resource {
    /// The queue the handler drives.
    Queue,
    /// The rule the handler reads.
    Rule,
    /// Every resource, which the bare `*` wildcard stands for.
    Every,
}

impl Resource {
    fn named(self, index: usize) -> String {
        match self {
            Self::Queue => queue_arn(&queue_name(index)),
            Self::Rule => rule_arn(),
            Self::Every => "*".to_string(),
        }
    }
}

/// One statement a row's execution role carries: the actions it allows, on the resource it
/// names.
struct Grant {
    actions: &'static [&'static str],
    resource: Resource,
}

/// One row: the document the execution role carries, and what the policies in force therefore
/// let the handler's calls do.
struct Row {
    case: &'static str,
    grants: &'static [Grant],
    /// Whether the visibility calls and the delete are allowed.
    lifecycle_allowed: bool,
    /// Whether the read of the rule is allowed.
    describe_allowed: bool,
}

const ROWS: &[Row] = &[
    // Each action the lifecycle performs, named one by one on every resource: the whole
    // lifecycle is served and the rule reads back.
    Row {
        case: "a role granted each action of the lifecycle by name, on every resource",
        grants: &[
            Grant {
                actions: &[
                    "sqs:SendMessage",
                    "sqs:ReceiveMessage",
                    "sqs:ChangeMessageVisibility",
                    "sqs:DeleteMessage",
                ],
                resource: Resource::Every,
            },
            Grant {
                actions: &["events:DescribeRule"],
                resource: Resource::Every,
            },
        ],
        lifecycle_allowed: true,
        describe_allowed: true,
    },
    // The same grants written as each service's whole action set, scoped to the two resources
    // the handler addresses: the same calls are served, so neither the spelling of the action
    // nor the narrower resource changes the decision.
    Row {
        case: "a role granted every action of each service, scoped to the queue and the rule",
        grants: &[
            Grant {
                actions: &["sqs:*"],
                resource: Resource::Queue,
            },
            Grant {
                actions: &["events:*"],
                resource: Resource::Rule,
            },
        ],
        lifecycle_allowed: true,
        describe_allowed: true,
    },
    // A role granted only the two actions that put messages on the queue and take them off
    // it: the handler sends and receives, and every call past that is refused — so no lock is
    // changed and no message is deleted.
    Row {
        case: "a role granted only the send and the receive",
        grants: &[Grant {
            actions: &["sqs:SendMessage", "sqs:ReceiveMessage"],
            resource: Resource::Every,
        }],
        lifecycle_allowed: false,
        describe_allowed: false,
    },
];

/// One visibility or delete call the handler makes: the key it reports it under, the step name
/// an assertion blames, and the IAM action it performs.
struct Step {
    key: &'static str,
    name: &'static str,
    action: &'static str,
}

/// Every such call, in the order the handler makes them.
const STEPS: &[Step] = &[
    Step {
        key: "released",
        name: "hand-back of a message",
        action: CHANGE_VISIBILITY_ACTION,
    },
    Step {
        key: "deleted",
        name: "delete of a processed message",
        action: DELETE_MESSAGE_ACTION,
    },
    Step {
        key: "shortened",
        name: "shortening of a lock",
        action: CHANGE_VISIBILITY_ACTION,
    },
    Step {
        key: "briefed",
        name: "brief lock taken before an extension",
        action: CHANGE_VISIBILITY_ACTION,
    },
    Step {
        key: "extended",
        name: "extension of a lock",
        action: CHANGE_VISIBILITY_ACTION,
    },
];

/// The queue one row's handler drives.
fn queue_name(index: usize) -> String {
    format!("xsvc-fn-lifecycle-{index}")
}

/// The role one row runs as, and the function it runs in — named here because a refusal's own
/// sentence names both: the identity a handler's call is signed as is the session
/// `arn:aws:sts::<account>:assumed-role/<role>/<function>`.
fn role_name(index: usize) -> String {
    format!("xsvc-fn-lifecycle-role-{index}")
}
fn function_name(index: usize) -> String {
    format!("xsvc-fn-lifecycle-consumer-{index}")
}

/// The ARN a queue of this account in this Region carries, which is what a statement names and
/// what a refusal reports.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// The ARN of the rule the handler reads: a rule on the default bus carries no bus segment.
fn rule_arn() -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:rule/{RULE}")
}

/// The session a call made by an invocation of one row's function speaks for.
fn session_arn(index: usize) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
        role_name(index),
        function_name(index)
    )
}

/// The sentence Amazon SQS refuses a call nothing in force allows with. Written out here
/// rather than taken from the service's own helper, so the wording has something to be wrong
/// about.
///
/// The action is named case-folded, which is how this service words a refusal.
fn sqs_refusal(index: usize, action: &str) -> String {
    let named = action.to_lowercase();
    format!(
        "User: {} is not authorized to perform: {named} on resource: {} because no \
         identity-based policy allows the {named} action",
        session_arn(index),
        queue_arn(&queue_name(index)),
    )
}

/// The sentence Amazon EventBridge refuses such a call with: the same shape, with the action
/// named as that service's own action list spells it.
fn events_refusal(index: usize) -> String {
    format!(
        "User: {} is not authorized to perform: {DESCRIBE_RULE_ACTION} on resource: {} because \
         no identity-based policy allows the {DESCRIBE_RULE_ACTION} action",
        session_arn(index),
        rule_arn(),
    )
}

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the queue it drives is the queue this suite reads.
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
/// are the account's own and not any row's role's.
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

/// One `awsJson` RPC call, answered with the HTTP status and the parsed body.
async fn rpc(
    router: &MultiServiceRouter,
    service: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{service}.{REGION}.amazonaws.com"))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
        .header("authorization", authorization(service))
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
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

/// One Amazon SQS operation, made as the account's own credential — the queue's owner, whose
/// calls no row's grant decides.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// One Amazon EventBridge operation, made as the account's own credential, which must be
/// served.
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, answered) = rpc(router, "events", "AWSEvents", "1.1", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: EventBridge {operation} must succeed, got {status} with {answered}"
    );
    answered
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
/// the row's own statements.
fn role_document(index: usize, row: &Row) -> Value {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })];
    for grant in row.grants {
        statements.push(json!({
            "Effect": "Allow",
            "Action": grant.actions,
            "Resource": grant.resource.named(index),
        }));
    }
    json!({"Version": "2012-10-17", "Statement": statements})
}

/// The template the suite deploys: one queue, one execution role carrying that row's document
/// and one consumer function running as that role, per row. The inline source is archived into
/// the deployment package by CloudFormation, which is how a function whose handler is written
/// here comes to exist at all.
fn stack_template() -> String {
    let mut resources = serde_json::Map::new();
    for (index, row) in ROWS.iter().enumerate() {
        resources.insert(
            format!("Queue{index}"),
            json!({
                "Type": "AWS::SQS::Queue",
                "Properties": {
                    "QueueName": queue_name(index),
                    "VisibilityTimeout": QUEUE_VISIBILITY_TIMEOUT,
                },
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
                        "PolicyName": "consumer-grants",
                        "PolicyDocument": role_document(index, row),
                    }],
                },
            }),
        );
        resources.insert(
            format!("Consumer{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": [role_logical, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": CONSUMER_HANDLER},
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

/// Deploy the stack and wait for `CREATE_COMPLETE`.
async fn deploy(router: &MultiServiceRouter) {
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
    // The stack's completion is owed from the instant the owning services report its
    // resources ready, which no fixed sleep can name: this looks until it reports it, on the
    // shared wait whose deadline is generous enough that a loaded host does not decide the
    // outcome.
    let reported = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, stack_events) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&stack_events, "ResourceStatusReason")
    };
    assert_eq!(
        reported.0, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {} with resource failures \
         {failures:?}",
        reported.1
    );
}

/// The rule the handler reads, put on the default bus as the account's own credential.
async fn put_rule(router: &MultiServiceRouter) {
    let pattern = json!({"source": [RULE_SOURCE]}).to_string();
    let answered = events(
        router,
        "setup",
        "PutRule",
        &json!({"Name": RULE, "EventPattern": pattern, "State": RULE_STATE}),
    )
    .await;
    assert_eq!(
        answered["RuleArn"].as_str(),
        Some(rule_arn().as_str()),
        "setup: the rule's ARN is what a statement and a refusal name, got {answered}"
    );
}

/// The url Amazon SQS reports for the queue `name`, asserting the ARN it carries is the one
/// every statement and every refusal names.
async fn resolved_queue(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, answered) = sqs(router, "GetQueueUrl", &json!({"QueueName": name})).await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueUrl({name:?}) must succeed, got {status} with {answered}"
    );
    let url = answered["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: GetQueueUrl({name:?}) must answer a url, got {answered}")
        })
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({"QueueUrl": url, "AttributeNames": ["QueueArn"]}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes({name:?}) must succeed, got {status} with \
         {attributes}"
    );
    assert_eq!(
        attributes["Attributes"]["QueueArn"].as_str(),
        Some(queue_arn(name).as_str()),
        "case {case:?}: the queue's ARN is what a statement and a refusal name, got \
         {attributes}"
    );
    url
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, name: &str) {
    let giving_up_at = Instant::now() + DEADLINE;
    loop {
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
        assert!(
            Instant::now() < giving_up_at,
            "setup: Lambda must report {name:?} Active, last reported {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// `Invoke` `name` synchronously with `event`, answering the `FunctionError` signal the
/// invocation carried, if any, and what the handler returned.
async fn invoke(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    event: &Value,
) -> (Option<String>, Value) {
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/invocations"),
        event.to_string().as_bytes(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} with {payload}"
    );
    let signal = headers
        .get("x-amz-function-error")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (signal, payload)
}

/// The bodies the queue hands over while they are VISIBLE, looking again until it has handed
/// over `wanted` of them or the generous deadline passes.
///
/// Every look takes a `VisibilityTimeout` of zero, so a look leaves every message visible: the
/// read cannot change what the next look, or the depth read after it, sees.
async fn visible_bodies(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    wanted: usize,
) -> BTreeSet<String> {
    let giving_up_at = Instant::now() + DEADLINE;
    let mut seen: BTreeSet<String> = BTreeSet::new();
    loop {
        let (status, answered) = sqs(
            router,
            "ReceiveMessage",
            &json!({
                "QueueUrl": url,
                "MaxNumberOfMessages": 10,
                "VisibilityTimeout": 0,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: looking at the queue must succeed, got {status} with {answered}"
        );
        for message in answered["Messages"].as_array().cloned().unwrap_or_default() {
            if let Some(body) = message["Body"].as_str() {
                seen.insert(body.to_string());
            }
        }
        if seen.len() >= wanted || Instant::now() >= giving_up_at {
            return seen;
        }
        tokio::time::sleep(LOOK).await;
    }
}

/// How many messages the queue reports as visible, and how many as in flight.
async fn depth(router: &MultiServiceRouter, case: &str, url: &str) -> (String, String) {
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({
            "QueueUrl": url,
            "AttributeNames": [
                "ApproximateNumberOfMessages",
                "ApproximateNumberOfMessagesNotVisible",
            ],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: reading the queue's depth must succeed, got {status} with {attributes}"
    );
    let reported = &attributes["Attributes"];
    (
        reported["ApproximateNumberOfMessages"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        reported["ApproximateNumberOfMessagesNotVisible"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    )
}

/// The `MessageId` the batch send minted for each entry, keyed by the entry's own `Id`.
fn minted_ids(case: &str, sent: &Value) -> BTreeMap<String, String> {
    let successful = sent["answered"]["Successful"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(
        successful.len(),
        BODIES.len(),
        "case {case:?}: expected {} successful entries from the batch send, got {sent}",
        BODIES.len()
    );
    successful
        .iter()
        .map(|entry| {
            (
                entry["Id"].as_str().unwrap_or_default().to_string(),
                entry["MessageId"].as_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// Assert that `answered` is the refusal `code`/`message` the service in question states, read
/// by the handler rather than faulting it.
fn assert_refused(case: &str, step: &str, answered: &Value, code: &str, message: &str) {
    assert_eq!(
        answered["ok"],
        json!(false),
        "case {case:?}: the {step} must be refused, got {answered}"
    );
    assert_eq!(
        answered["code"].as_str(),
        Some(code),
        "case {case:?}: expected the {step} to be refused {code}, got {answered}"
    );
    assert_eq!(
        answered["message"].as_str(),
        Some(message),
        "case {case:?}: expected the {step} refusal {message:?}, got {answered}"
    );
}

/// Assert that `answered` is a served call carrying the operation's own output.
fn assert_served(case: &str, step: &str, answered: &Value) {
    assert_eq!(
        answered["ok"],
        json!(true),
        "case {case:?}: the {step} must be served, got {answered}"
    );
    assert_eq!(
        answered["status"],
        json!(200),
        "case {case:?}: expected the {step} to answer HTTP 200, got {answered}"
    );
}

/// Every row: what a running handler's own lifecycle calls do to the queue it drives, and what
/// its read of the rule answers, is what the policies in force on its execution role allow —
/// and the invocation itself carries no function error either way.
#[tokio::test]
async fn a_running_handler_drives_the_queue_message_lifecycle_with_the_runtimes_sdk() {
    let router = build_router();
    deploy(&router).await;
    put_rule(&router).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let url = resolved_queue(&router, case, &queue_name(index)).await;
        wait_until_active(&router, &function_name(index)).await;
        let (signal, report) = invoke(
            &router,
            case,
            &function_name(index),
            &json!({
                "url": url,
                "bodies": BODIES,
                "looks": HANDLER_LOOKS,
                "receive_hold": RECEIVE_HOLD,
                "brief": BRIEF_HOLD,
                "extension": EXTENDED_HOLD,
                "release": RELEASED_BODY,
                "delete": DELETED_BODY,
                "shorten": SHORTENED_BODY,
                "extend": EXTENDED_BODY,
                "rule": RULE,
            }),
        )
        .await;
        assert_eq!(
            signal, None,
            "case {case:?}: the handler catches every refusal, so the invocation carries no \
             function error, got {signal:?} with {report}"
        );

        // The batch send, and the receives that took the messages it put on the queue: what
        // the handler was handed is the message it sent, under the id the send minted for it.
        assert_served(case, "batch send", &report["sent"]);
        let minted = minted_ids(case, &report["sent"]);
        assert_served(case, "receive", &report["received"]);
        let handled: Vec<String> = report["handled"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|body| body.as_str().map(str::to_string))
            .collect();
        let mut expected_handled: Vec<String> =
            BODIES.iter().map(|body| (*body).to_string()).collect();
        expected_handled.sort();
        assert_eq!(
            handled, expected_handled,
            "case {case:?}: the handler must hold a receipt handle for every message it sent; \
             its receives handed back {} message(s) per look, in {report}",
            report["looks"]
        );
        for (position, body) in BODIES.iter().enumerate() {
            let expected = minted.get(&format!("e{position}")).cloned();
            assert_eq!(
                report["ids"][body].as_str().map(str::to_string),
                expected,
                "case {case:?}: the id the receive reported for {body:?} must be the one the \
                 send minted, got {report}"
            );
        }

        // Every visibility call and the delete, each decided against the role the function
        // runs as.
        for step in STEPS {
            let answered = &report[step.key];
            if row.lifecycle_allowed {
                assert_served(case, step.name, answered);
            } else {
                assert_refused(
                    case,
                    step.name,
                    answered,
                    SQS_ACCESS_DENIED,
                    &sqs_refusal(index, step.action),
                );
                assert_eq!(
                    answered["status"],
                    json!(SQS_ACCESS_DENIED_STATUS),
                    "case {case:?}: expected the {} refusal on HTTP \
                     {SQS_ACCESS_DENIED_STATUS}, got {answered}",
                    step.name
                );
            }
        }

        // The read of the rule, taken on the same credential through the same SDK.
        let described = &report["described"];
        if row.describe_allowed {
            assert_served(case, "rule read", described);
            assert_eq!(
                described["answered"]["Arn"].as_str(),
                Some(rule_arn().as_str()),
                "case {case:?}: expected the rule's own ARN, got {described}"
            );
            assert_eq!(
                described["answered"]["Name"].as_str(),
                Some(RULE),
                "case {case:?}: expected the rule's own name, got {described}"
            );
            assert_eq!(
                described["answered"]["State"].as_str(),
                Some(RULE_STATE),
                "case {case:?}: expected the rule's own state, got {described}"
            );
            assert_eq!(
                described["answered"]["EventPattern"].as_str(),
                Some(json!({"source": [RULE_SOURCE]}).to_string().as_str()),
                "case {case:?}: expected the pattern the rule was put with, got {described}"
            );
        } else {
            assert_refused(
                case,
                "rule read",
                described,
                EVENTS_ACCESS_DENIED,
                &events_refusal(index),
            );
        }

        // What the QUEUE holds afterwards, which is what the handler's calls made of it. The
        // receive locked every message for far longer than any run, so a message that is
        // VISIBLE here is one a call handed back or cut the lock of, and the one still in
        // flight once its sibling under the same brief lock has lapsed is the one the
        // extension holds.
        let expected_visible: BTreeSet<String> = if row.lifecycle_allowed {
            [RELEASED_BODY.to_string(), SHORTENED_BODY.to_string()]
                .into_iter()
                .collect()
        } else {
            BTreeSet::new()
        };
        let seen = visible_bodies(&router, case, &url, expected_visible.len()).await;
        assert_eq!(
            seen, expected_visible,
            "case {case:?}: expected exactly {expected_visible:?} to be visible on the queue, \
             got {seen:?}"
        );
        let expected_depth = if row.lifecycle_allowed {
            // The handed-back message and the one whose lock was cut are visible, the
            // extended one is still in flight, and the deleted one is in neither count.
            ("2".to_string(), "1".to_string())
        } else {
            // Nothing was handed back, nothing was deleted and no lock was touched, so every
            // message is still held by the handler's own receive.
            ("0".to_string(), BODIES.len().to_string())
        };
        let reported = depth(&router, case, &url).await;
        assert_eq!(
            reported, expected_depth,
            "case {case:?}: expected the queue to report (visible, in flight) \
             {expected_depth:?}, got {reported:?}"
        );
    }
}
