// SPDX-License-Identifier: CC0-1.0

//! What a CloudFormation-deployed EventBridge -> Lambda -> Amazon SQS relay delivers, and what
//! an under-permissioned one is TOLD instead, end to end over the assembled router with
//! CloudFormation, IAM, the token service, EventBridge, Lambda and Amazon SQS on one engine.
//!
//! RULE: **a managed `python3.x` runtime makes the AWS SDK for Python importable to a
//! deployment package that bundles none of it, and signs every client built inside the handler
//! as a session of the function's own execution role
//! (`arn:aws:sts::<account>:assumed-role/<role>/<function>`), so each call the handler makes is
//! decided by the service that SERVES it against the identity-based documents in force on THAT
//! role: a call some statement in force allows is served and its effect is in the serving
//! service afterwards, and a call none allows is refused with that service's own authorization
//! refusal naming the session, the action case-folded as it words one, and the resource
//! addressed. A refusal the handler does not catch is the FUNCTION's failure and not the
//! `Invoke` call's — the invocation is still HTTP 200, carrying `X-Amz-Function-Error:
//! Unhandled` and the run's error document, whose `errorType` is the SDK exception class raised
//! and whose `errorMessage` is the sentence the SDK renders for a service's refusal, `An error
//! occurred (<code>) when calling the <Operation> operation: <the service's own message>`.**
//!
//! "The execution role grants the function permission to use Amazon Web Services services"
//! (`FunctionConfiguration$Role`); a request is allowed when some identity-based policy allows
//! it and otherwise falls to IAM's default deny (IAM User Guide, *Policy evaluation logic*);
//! and an unhandled function failure is none of `Invoke`'s modelled errors — "if present,
//! indicates that an error occurred during function execution"
//! (`InvocationResponse$FunctionError`), which is a header of a 200.
//!
//! That the SDK is the RUNTIME's and not the package's is what every leg below rests on: each
//! relay's archive carries one handler module, `import boto3` is at its module scope and the
//! client is built there, so a runtime that made no SDK importable would fail every one of
//! these invocations at initialization — with its loader's own class — instead of reaching the
//! authorization decision this suite reads. The first call each handler makes is a write every
//! role IS granted, to a witness queue, so a relay that was handed its event is observable even
//! where its own target write is refused: an empty target queue then means refused, never
//! "never ran".
//!
//! Six crates have to agree for the rule to hold — CloudFormation deploys the queues, the
//! roles, the functions, the rules and the invoke statements together, IAM holds each role and
//! its inline document, the token service vends each function's session, EventBridge matches a
//! publication and hands it over, Lambda runs the handler and supplies it the SDK, and Amazon
//! SQS takes the decision and owns what a delivery leaves behind — so the suite lives here.
//! Every observation is one a client makes: `DescribeStacks`, `DescribeStackResources`,
//! `GetFunctionConfiguration`, `Invoke`, `PutEvents` and `ReceiveMessage`.
//!
//! What the neighbours pin and this one does not repeat:
//! [`crate::lambda_in_function_sqs_authorization`] drives the same decision over the forms a
//! role's own identity-based document can take, and
//! [`crate::lambda_in_function_sqs_queue_policy_send`] over the forms the addressed QUEUE's own
//! policy can take — both for a relay a client invokes itself, and neither for a relay an
//! EventBridge rule hands an event to; and
//! [`crate::events_rule_lambda_target_direct_invoke_and_relay`] drives the handover to a target
//! whose role IS granted the write, and so never reads what an under-permissioned target is
//! told. What only this suite reaches is the two drive modes CROSSED with the grant: the same
//! relay pair answered the same way whether the event came from a client or from a rule, so a
//! handover that quietly delivered nothing, and one that delivered under a role that does not
//! allow the write, are both visible here.
//!
//! The rows differ along what the rule generalises over: HOW the relay is driven (a client's
//! own `RequestResponse` `Invoke`, and a rule handover of a published event) and HOW the
//! execution role withholds the grant (no statement about the service at all, and a statement
//! allowing the same action on another queue of the account) beside a permitted relay in the
//! SAME stack — two withholding forms rather than the neighbour's full set, since what is new
//! here is the crossing and not the forms. Both halves of every leg are asserted — what the
//! invocation answered, and what each queue holds afterwards — so a refusal that still
//! delivered, and a grant that answered without delivering, both fail here.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{elements, form_encode, physical_ids_of};

/// The Region every request here is signed for, and so the Region every queue, role, function
/// and rule below belongs to.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every queue ARN and every session ARN carries.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// The stack the whole topology is deployed by.
const STACK: &str = "xsvc-relay-grant-delivery";

/// The `StackStatus` that stack must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The three queues the stack declares: the one the permitted relay addresses, the one every
/// under-permissioned relay addresses — which must therefore stay empty — and the witness every
/// relay IS granted a write to, whose arrival is what shows a relay ran at all.
const GOOD_QUEUE: &str = "xsvc-relay-grant-good";
const DENIED_QUEUE: &str = "xsvc-relay-grant-denied";
const WITNESS_QUEUE: &str = "xsvc-relay-grant-witness";

/// Their logical ids in the template, which the handler's `QUEUE_URL` is `Ref`erenced from.
const GOOD_LOGICAL: &str = "GoodQueue";
const DENIED_LOGICAL: &str = "DeniedQueue";
const WITNESS_LOGICAL: &str = "WitnessQueue";

/// The IAM action the write each handler makes performs, which is what a statement grants and
/// what a refusal names — Amazon SQS publishes one `sqs:<OperationName>` action per operation.
const SEND_ACTION: &str = "sqs:SendMessage";

/// The operation name the SDK's own sentence names the refused call by, which is the modelled
/// operation it called (`smithy_query(query="operation", operation="SendMessage")`).
const SEND_OPERATION: &str = "SendMessage";

/// The code Amazon SQS refuses a call the policies in force do not allow with: its model
/// declares no authorization error shape, so the refusal is the service-common one.
const ACCESS_DENIED: &str = "AccessDenied";

/// The signal an invocation whose handler raised carries, and the SDK exception class raised
/// for a service's refusal — which is therefore what the run's error document names.
const UNHANDLED: &str = "Unhandled";
const CLIENT_ERROR: &str = "ClientError";

/// The `errorType` a Python runtime reports when a module the handler asked for could not be
/// imported at all. No leg here may answer it: every relay's package bundles no SDK, so this is
/// what a runtime that provided none would report in place of an authorization decision.
const IMPORT_MODULE_ERROR: &str = "Runtime.ImportModuleError";

/// The runtime and handler every declared function carries.
const RUNTIME: &str = "python3.12";
const HANDLER: &str = "index.handler";

/// How long one invocation is given, as the function's own `Timeout`: well past what any run
/// needs on any host, because what is under test is the authorization decision and never how
/// fast an interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 120;

/// How often to look again for work the clock owes — the stack settling, a function reaching
/// `Active`, a queue being handed a message — and how many looks to take. The spans are the
/// owning services' own, so no fixed sleep names the instant one ends, and the budget is
/// generous because every case asserts the CONDITION and never the time it took.
const LOOK: Duration = Duration::from_millis(100);
const LOOKS: usize = 1_800;

/// How many looks an observation that must find NOTHING takes before concluding nothing came.
/// Nothing rests on this window alone: the delivery asserted before it is what shows the
/// dispatcher was live while it was open.
const QUIET_LOOKS: usize = 30;

/// The envelope members the rules match on, and the values every publication carries for them:
/// a control-plane event of a warehouse cluster, as a trail delivers one.
const SOURCE: &str = "com.example.warehouse.control";
const DETAIL_TYPE: &str = "AWS API Call via CloudTrail";
const EVENT_SOURCE_MEMBER: &str = "eventSource";
const EVENT_SOURCE: &str = "redshift.amazonaws.com";
const EVENT_NAME_MEMBER: &str = "eventName";
const EVENT_NAME: &str = "ResumeCluster";
const PARAMETERS_MEMBER: &str = "requestParameters";
const CLUSTER_MEMBER: &str = "clusterIdentifier";

/// The service principal EventBridge hands a function target over as, and the action the
/// handover performs: "for Lambda functions … EventBridge relies on resource-based policies"
/// (`PutTargets`).
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";
const INVOKE_ACTION: &str = "lambda:InvokeFunction";

/// The handler every relay runs, archived into the deployment package by CloudFormation.
///
/// `import boto3` is at MODULE scope and the client is built there, so a runtime supplying no
/// SDK could not initialize this package at all. It writes the witness queue first — a write
/// every role is granted — and only then the queue the row's grant decides, leaving a refusal of
/// that second write to propagate, which is what makes the invocation report a function error.
const RELAY_HANDLER: &str = r#"
import json
import os

import boto3

CLIENT = boto3.client("sqs")
WITNESS_URL = os.environ["WITNESS_URL"]
QUEUE_URL = os.environ["QUEUE_URL"]
RELAY = os.environ["RELAY"]


def handler(event, context):
    CLIENT.send_message(
        QueueUrl=WITNESS_URL,
        MessageBody=json.dumps({"relay": RELAY, "event": event}, sort_keys=True),
    )
    answered = CLIENT.send_message(
        QueueUrl=QUEUE_URL, MessageBody=json.dumps(event, sort_keys=True)
    )
    return {"relayed": True, "id": bool(answered.get("MessageId"))}
"#;

/// What the permitted relay's handler returns once its own write was served.
fn relayed_report() -> Value {
    json!({"relayed": true, "id": true})
}

/// HOW a row's execution role stands towards the write its handler makes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Grant {
    /// A statement allowing the send on the queue the handler addresses.
    SendOnItsTarget,
    /// No statement about the service at all, which is IAM's default deny.
    NothingOfTheService,
    /// A statement allowing the SAME action on another queue of the account.
    SendOnAnotherQueue,
}

/// One relay of the stack: its role, its function, the cluster its rule matches, and the grant
/// its role carries.
#[derive(Debug, Clone, Copy)]
struct Relay {
    case: &'static str,
    /// The suffix of the logical ids this relay's resources are declared under.
    logical: &'static str,
    role: &'static str,
    function: &'static str,
    cluster: &'static str,
    grant: Grant,
}

impl Relay {
    /// Whether the policies in force on this relay's role allow the write it makes.
    fn delivers(&self) -> bool {
        self.grant == Grant::SendOnItsTarget
    }

    /// The queue this relay's handler addresses: the permitted one its own, every refused one
    /// the queue that must stay empty.
    fn target(&self) -> &'static str {
        if self.delivers() {
            GOOD_QUEUE
        } else {
            DENIED_QUEUE
        }
    }

    /// Its logical id, which the function's `QUEUE_URL` is `Ref`erenced from.
    fn target_logical(&self) -> &'static str {
        if self.delivers() {
            GOOD_LOGICAL
        } else {
            DENIED_LOGICAL
        }
    }
}

/// The permitted relay FIRST, so every later row's empty target queue is read in a run that has
/// already shown this topology delivers.
const RELAYS: &[Relay] = &[
    Relay {
        case: "a relay whose role allows the send on the queue it addresses",
        logical: "Permitted",
        role: "xsvc-relay-grant-permitted",
        function: "xsvc-relay-permitted",
        cluster: "warehouse-permitted",
        grant: Grant::SendOnItsTarget,
    },
    Relay {
        case: "a relay whose role states nothing about the service",
        logical: "NoStatement",
        role: "xsvc-relay-grant-no-statement",
        function: "xsvc-relay-no-statement",
        cluster: "warehouse-no-statement",
        grant: Grant::NothingOfTheService,
    },
    Relay {
        case: "a relay whose role allows the send on another queue of the account",
        logical: "OtherQueue",
        role: "xsvc-relay-grant-other-queue",
        function: "xsvc-relay-other-queue",
        cluster: "warehouse-other-queue",
        grant: Grant::SendOnAnotherQueue,
    },
];

/// The emulator as it is ASSEMBLED, on one engine: a handler's own SDK call is dispatched back
/// into this same router, so the queue it writes is the queue this suite reads.
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
/// are the account's own and no row's role's.
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

/// One Lambda `restJson1` call, as its status, its headers and its parsed body — the headers
/// because `X-Amz-Function-Error` is where a caller reads whether the run faulted.
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

/// One Amazon SQS operation over its `awsJson1_0` wire, made as the ACCOUNT's own credential —
/// the queues' owner, whose reads no row's grant decides.
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

/// One EventBridge `awsJson1_1` call (`AWSEvents.<Operation>`), as its status and parsed body.
async fn events(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("events.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("authorization", authorization("events"))
        .header("x-amz-target", format!("AWSEvents.{operation}"))
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

/// The ARN a queue of this account in this Region carries, which is what a statement names and
/// what a refusal reports. Asserted against the ARN the service itself reports in
/// [`resolved_queue`], so no expected value here is only a literal.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// The identity a relay's calls are signed as: "the execution role grants the function
/// permission to use Amazon Web Services services" (`FunctionConfiguration$Role`), and the
/// session such a call speaks for is the role assumed under the function's own name.
fn session_arn(relay: &Relay) -> String {
    format!(
        "arn:aws:sts::{ACCOUNT}:assumed-role/{}/{}",
        relay.role, relay.function
    )
}

/// The whole sentence the error document of a refused relay carries: the SDK's own rendering of
/// a service refusal, around the sentence Amazon SQS refuses a call nothing in force allows
/// with. Built here from the role, the function and the queue the stack deployed, so no
/// expected value is a literal of one run.
///
/// The outer half is botocore's fixed text for a service refusal; the inner half is IAM's
/// sentence for its DEFAULT deny — the decision each row's role leaves the write to — with the
/// reason clause that names the action no document allowed. The action is named case-folded,
/// which is how Amazon SQS words a refusal: the case a policy or the model spells an action in
/// is not part of the sentence. Pinned the same way by
/// [`crate::lambda_in_function_sqs_authorization`], whose rows reach the same sentence through a
/// client's own invocation.
fn refusal_sentence(relay: &Relay) -> String {
    let named = SEND_ACTION.to_lowercase();
    format!(
        "An error occurred ({ACCESS_DENIED}) when calling the {SEND_OPERATION} operation: \
         User: {} is not authorized to perform: {named} on resource: {} because no \
         identity-based policy allows the {named} action",
        session_arn(relay),
        queue_arn(relay.target()),
    )
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

/// The inline document one relay's role carries: the log writes every execution role makes, the
/// witness write every relay IS granted, and the row's own statement where it has one.
fn role_document(relay: &Relay) -> Value {
    let mut statements = vec![
        json!({
            "Effect": "Allow",
            "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
            "Resource": "*",
        }),
        json!({
            "Effect": "Allow",
            "Action": SEND_ACTION,
            "Resource": queue_arn(WITNESS_QUEUE),
        }),
    ];
    match relay.grant {
        Grant::SendOnItsTarget | Grant::SendOnAnotherQueue => statements.push(json!({
            "Effect": "Allow",
            "Action": SEND_ACTION,
            "Resource": queue_arn(GOOD_QUEUE),
        })),
        Grant::NothingOfTheService => {}
    }
    json!({"Version": "2012-10-17", "Statement": statements})
}

/// The pattern one relay's rule carries: the envelope members every publication names, and the
/// cluster identifier that tells this relay's event from its siblings'.
fn pattern(relay: &Relay) -> Value {
    json!({
        "source": [SOURCE],
        "detail-type": [DETAIL_TYPE],
        "detail": {
            EVENT_SOURCE_MEMBER: [EVENT_SOURCE],
            EVENT_NAME_MEMBER: [EVENT_NAME],
            PARAMETERS_MEMBER: {CLUSTER_MEMBER: [relay.cluster]},
        },
    })
}

/// The `detail` one relay's entry publishes.
fn detail(relay: &Relay) -> Value {
    json!({
        EVENT_SOURCE_MEMBER: EVENT_SOURCE,
        EVENT_NAME_MEMBER: EVENT_NAME,
        PARAMETERS_MEMBER: {CLUSTER_MEMBER: relay.cluster},
    })
}

/// The template the suite deploys: the three queues, and per relay an execution role carrying
/// that row's document, a function running as it, a rule matching that relay's cluster and the
/// statement admitting EventBridge's handover of it.
fn stack_template() -> String {
    let mut resources = serde_json::Map::new();
    for (logical, name) in [
        (GOOD_LOGICAL, GOOD_QUEUE),
        (DENIED_LOGICAL, DENIED_QUEUE),
        (WITNESS_LOGICAL, WITNESS_QUEUE),
    ] {
        resources.insert(
            logical.to_string(),
            json!({
                "Type": "AWS::SQS::Queue",
                "Properties": {"QueueName": name},
            }),
        );
    }
    for relay in RELAYS {
        let role_logical = format!("Role{}", relay.logical);
        let function_logical = format!("Relay{}", relay.logical);
        let rule_logical = format!("Rule{}", relay.logical);
        resources.insert(
            role_logical.clone(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": relay.role,
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "relay-grants",
                        "PolicyDocument": role_document(relay),
                    }],
                },
            }),
        );
        resources.insert(
            function_logical.clone(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": relay.function,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [role_logical, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": RELAY_HANDLER},
                    "Environment": {"Variables": {
                        "QUEUE_URL": {"Ref": relay.target_logical()},
                        "WITNESS_URL": {"Ref": WITNESS_LOGICAL},
                        "RELAY": relay.function,
                    }},
                },
            }),
        );
        resources.insert(
            rule_logical.clone(),
            json!({
                "Type": "AWS::Events::Rule",
                "Properties": {
                    "Name": format!("{STACK}-{}", relay.logical),
                    "EventPattern": pattern(relay),
                    "State": "ENABLED",
                    "Targets": [{
                        "Id": "relay",
                        "Arn": {"Fn::GetAtt": [function_logical.clone(), "Arn"]},
                    }],
                },
            }),
        );
        resources.insert(
            format!("Invoke{}", relay.logical),
            json!({
                "Type": "AWS::Lambda::Permission",
                "Properties": {
                    "FunctionName": {"Fn::GetAtt": [function_logical, "Arn"]},
                    "Action": INVOKE_ACTION,
                    "Principal": EVENTS_PRINCIPAL,
                    "SourceArn": {"Fn::GetAtt": [rule_logical, "Arn"]},
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
    // ready, which no fixed sleep can name: this looks until it reports it.
    let reported = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported.0 == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, happened) = query(
            router,
            "cloudformation",
            "2010-05-15",
            "DescribeStackEvents",
            &[("StackName", STACK)],
        )
        .await;
        elements(&happened, "ResourceStatusReason")
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

/// The url Amazon SQS reports for the queue `name`, asserting the ARN it carries is the one
/// every statement and every refusal names.
async fn resolved_queue(router: &MultiServiceRouter, name: &str) -> String {
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
    assert_eq!(
        attributes["Attributes"]["QueueArn"].as_str(),
        Some(queue_arn(name).as_str()),
        "setup: the queue's ARN is what a statement and a refusal name, got {attributes}"
    );
    url
}

/// Wait until Lambda first reports `name` `Active` — "during this time, you can't invoke or
/// modify the function" (`CreateFunction`) — which is the state an invocation needs.
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

/// Take everything the queue at `url` hands over, as the account's own credential, deleting each
/// message as it is read so a later look answers what arrived after this one — which is what
/// lets a delivery be counted ONCE — and answer the bodies parsed.
///
/// `wanted` is how many the caller expects: the loop looks again until it has them, so a host
/// slow to make a message available cannot decide the outcome. A caller expecting none keeps
/// looking for the short quiet window instead, so a delivery that did happen is seen.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str, wanted: usize) -> Vec<Value> {
    let mut held: Vec<Value> = Vec::new();
    let looks = if wanted == 0 { QUIET_LOOKS } else { LOOKS };
    for look in 0..looks {
        let (status, answered) = sqs(
            router,
            "ReceiveMessage",
            &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: draining {url} must succeed, got {status} with {answered}"
        );
        let handed = answered["Messages"].as_array().cloned().unwrap_or_default();
        for message in &handed {
            let body = message["Body"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a message with no body {message}"));
            let parsed = serde_json::from_str(body)
                .unwrap_or_else(|_| Value::String(body.to_string()));
            held.push(parsed);
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
        // A caller expecting none stops at the first arrival too: what it found is what it
        // reports, and there is nothing further to wait for.
        if held.len() >= wanted.max(1) || look + 1 == looks {
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

/// Assert the answer to an invocation of `relay`, whichever way it was driven: the permitted
/// relay's own report with no function error, and a refused relay's run error document carrying
/// the SDK's sentence for what Amazon SQS answered its write with.
fn assert_invocation(
    relay: &Relay,
    leg: &str,
    status: u16,
    headers: &http::HeaderMap,
    body: &Value,
) {
    let case = relay.case;
    assert_eq!(
        status, 200,
        "case {case:?}/{leg}: an unhandled run failure is still an answered Invoke, got {status} \
         with {body}"
    );
    if relay.delivers() {
        assert_eq!(
            function_error(headers),
            None,
            "case {case:?}/{leg}: a served write fails no run, got {:?} with {body}",
            function_error(headers)
        );
        assert_eq!(
            body,
            &relayed_report(),
            "case {case:?}/{leg}: expected the handler's own report {}, got {body}",
            relayed_report()
        );
        return;
    }
    assert_eq!(
        function_error(headers).as_deref(),
        Some(UNHANDLED),
        "case {case:?}/{leg}: an uncaught refusal is the function's own error, got {:?} with \
         {body}",
        function_error(headers)
    );
    assert_ne!(
        body["errorType"].as_str(),
        Some(IMPORT_MODULE_ERROR),
        "case {case:?}/{leg}: the runtime supplies the SDK, so no run may fail at import, got \
         {body}"
    );
    assert_eq!(
        body["errorType"].as_str(),
        Some(CLIENT_ERROR),
        "case {case:?}/{leg}: expected the SDK exception class raised for a service refusal, \
         got {body}"
    );
    assert_eq!(
        body["errorMessage"].as_str(),
        Some(refusal_sentence(relay).as_str()),
        "case {case:?}/{leg}: expected the error document to carry {:?}, got {body}",
        refusal_sentence(relay)
    );
}

/// Every leg: a deployed relay's own SDK write is decided against the policies in force on the
/// role THAT relay runs as, however the relay was driven, and what each queue holds afterwards
/// follows that decision.
#[tokio::test]
async fn a_deployed_relays_sdk_write_follows_its_own_execution_role() {
    let router = build_router();
    let physical = deploy(&router).await;

    let good_url = resolved_queue(&router, GOOD_QUEUE).await;
    let denied_url = resolved_queue(&router, DENIED_QUEUE).await;
    let witness_url = resolved_queue(&router, WITNESS_QUEUE).await;
    for (logical, expected) in [
        (GOOD_LOGICAL, &good_url),
        (DENIED_LOGICAL, &denied_url),
        (WITNESS_LOGICAL, &witness_url),
    ] {
        let reported = physical.get(logical).cloned().unwrap_or_default();
        assert_eq!(
            &reported, expected,
            "setup: the stack must report {logical} as the queue the service holds, got \
             {reported:?}"
        );
    }

    for relay in RELAYS {
        let logical = format!("Relay{}", relay.logical);
        let reported = physical
            .get(&logical)
            .cloned()
            .unwrap_or_else(|| panic!("setup: the stack must report a physical id for {logical}"));
        assert_eq!(
            reported, relay.function,
            "setup: the stack must name the function {:?}, got {reported:?}",
            relay.function
        );
        wait_until_active(&router, relay.function).await;
    }

    // Leg one: the invocation a client makes itself, which runs the handler under the relay's
    // own role.
    for relay in RELAYS {
        let case = relay.case;
        let payload = json!({"probe": relay.logical, CLUSTER_MEMBER: relay.cluster});
        let (status, headers, answered) = lambda(
            &router,
            "POST",
            &format!("/2015-03-31/functions/{}/invocations", relay.function),
            payload.to_string().as_bytes(),
        )
        .await;
        assert_invocation(relay, "invoked", status, &headers, &answered);

        // The witness write every role is granted took effect, so the handler RAN and its
        // runtime did supply the SDK it imports at module scope.
        let witnessed = drained(&router, case, &witness_url, 1).await;
        assert_eq!(
            witnessed,
            vec![json!({"relay": relay.function, "event": payload})],
            "case {case:?}/invoked: expected the witness queue to hold the one write every role \
             is granted, got {witnessed:?}"
        );

        // And the write the row's grant decides: in the queue where it was allowed, nowhere
        // where it was refused.
        let delivered = drained(
            &router,
            case,
            if relay.delivers() {
                &good_url
            } else {
                &denied_url
            },
            usize::from(relay.delivers()),
        )
        .await;
        let expected: Vec<Value> = if relay.delivers() {
            vec![payload.clone()]
        } else {
            Vec::new()
        };
        assert_eq!(
            delivered, expected,
            "case {case:?}/invoked: expected the addressed queue to hold {expected:?}, got \
             {delivered:?}"
        );
    }

    // Leg two: one publication carrying an entry per relay, each matched by that relay's own
    // rule and handed over to that relay's function.
    let entries: Vec<Value> = RELAYS
        .iter()
        .map(|relay| {
            json!({
                "Source": SOURCE,
                "DetailType": DETAIL_TYPE,
                "Detail": detail(relay).to_string(),
            })
        })
        .collect();
    let (status, published) = events(&router, "PutEvents", &json!({"Entries": entries})).await;
    assert_eq!(
        status, 200,
        "publication: PutEvents must be served, got {status} with {published}"
    );
    assert_eq!(
        published["FailedEntryCount"],
        json!(0),
        "publication: an accepted publication reports no failed entry, got {published}"
    );

    // Every relay was handed its own entry: the witness write each of them IS granted arrives,
    // one per relay, carrying the envelope that relay's rule matched.
    let witnessed = drained(&router, "handed over", &witness_url, RELAYS.len()).await;
    assert_eq!(
        witnessed.len(),
        RELAYS.len(),
        "handed over: expected each of the {} relays to have been handed its entry, got \
         {witnessed:?}",
        RELAYS.len()
    );
    for relay in RELAYS {
        let case = relay.case;
        let seen: Vec<&Value> = witnessed
            .iter()
            .filter(|written| written["relay"] == json!(relay.function))
            .collect();
        assert_eq!(
            seen.len(),
            1,
            "case {case:?}/handed over: expected this relay to be handed its entry exactly once, \
             got {seen:?}"
        );
        let envelope = &seen[0]["event"];
        assert_eq!(
            envelope["source"],
            json!(SOURCE),
            "case {case:?}/handed over: expected the published source, got {envelope}"
        );
        assert_eq!(
            envelope["detail-type"],
            json!(DETAIL_TYPE),
            "case {case:?}/handed over: expected the published detail-type, got {envelope}"
        );
        assert_eq!(
            envelope["detail"],
            detail(relay),
            "case {case:?}/handed over: expected the published detail unchanged, got {envelope}"
        );
    }

    // The permitted relay's handover DELIVERED, exactly once, carrying what was published…
    let delivered = drained(&router, "handed over", &good_url, 1).await;
    assert_eq!(
        delivered.len(),
        1,
        "handed over: expected the permitted relay to deliver exactly one message, got \
         {delivered:?}"
    );
    let permitted = RELAYS
        .iter()
        .find(|relay| relay.delivers())
        .expect("one relay is the permitted one");
    assert_eq!(
        delivered[0]["detail"],
        detail(permitted),
        "handed over: expected the delivered message to carry the published detail, got {:?}",
        delivered[0]
    );

    // …and every under-permissioned relay's did not, which is read AFTER that delivery, so the
    // quiet window was open while this topology was demonstrably delivering.
    let refused = drained(&router, "handed over", &denied_url, 0).await;
    assert!(
        refused.is_empty(),
        "handed over: expected the queue the under-permissioned relays address to stay empty, \
         got {refused:?}"
    );
}
