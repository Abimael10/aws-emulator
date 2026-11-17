// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING `python3.x` function's own AWS SDK calls DO, for three destination services
//! whose wires differ: Amazon SNS, Amazon SQS and EventBridge, reached from ONE invocation of
//! ONE function whose deployment package holds nothing but its handler module.
//!
//! RULE: a managed `python3.x` runtime carries the AWS SDK on the handler's import path, so
//! `import boto3` resolves for a package holding only the handler module, and a client the
//! handler builds for ANY service the runtime reaches binds the operation to that service's
//! OWN protocol, signs it as a session of the function's execution role and is answered by
//! that service inside this emulator — so the effect of each call is observable at that
//! service's own front door, and the policies in force on the execution role are what decide
//! it: a call they grant is served and leaves its effect, one they do not is refused with that
//! service's own authorization code and leaves nothing. Either way the INVOCATION is HTTP 200
//! with no `X-Amz-Function-Error`, because what decides that is whether the handler returned —
//! "`Unhandled`" means "the function had an error" (`InvocationResponse$FunctionError`), and a
//! refusal the handler catches is not an error of the run.
//!
//! Five services have to agree for that to hold — CloudFormation archives the inline handler
//! into the deployment package and deploys the roles and the functions, Lambda runs the handler
//! and supplies it the SDK, the token service and IAM vend and resolve the session its calls
//! are signed as, and Amazon SNS, Amazon SQS and EventBridge each answer one of those calls on
//! its own wire — so the suite lives here: a crate-local suite can only ever see one side of
//! it.
//!
//! What the neighbours pin and this one does not repeat: `lambda_in_function_sqs_authorization`
//! drives the TABLE of policy shapes one queue call is decided by, the Lambda crate's
//! `handler_notification_fan_out` the granularity of one publication's decision and the text it
//! carries, and the EventBridge crate's `handler_execution_role_publication` the publication's
//! own refusal wording. What only this suite reaches is the one handler calling all THREE from
//! a package that ships no SDK of its own, and the effect of each arriving where a client reads
//! it.
//!
//! The rows differ along what the rule generalises over: WHICH service the handler calls and so
//! which protocol its request is bound to (`awsQuery` for Amazon SNS, `awsJson1_0` for Amazon
//! SQS, `awsJson1_1` for EventBridge), what the destination does with what it took (fan a
//! notification out to a subscribed queue, hold a message for a receive, match a rule and
//! deliver to its target), and whether the role the function runs as grants the call. Each row
//! asserts both halves: what the SDK gave the handler, and what the DESTINATION holds
//! afterwards — so a call that answered an id and delivered nothing, and a refusal that
//! delivered anyway, both fail here.
//!
//! Nothing here sleeps for a delivery: it polls each queue until the message it is waiting for
//! is there, and the "nothing arrived" reads are made at an instant by which a message planted
//! through the same path AFTER the refused invocation has already arrived, rather than after a
//! span of the clock.

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::query_wire::{element, elements, form_encode};

/// The Region every request below is signed for, and so the Region the stack, the topic, the
/// queues and the bus belong to. The CloudFormation wait this suite shares reads the stack in
/// this Region.
const REGION: &str = "us-east-1";

/// The account this emulator serves, which every ARN below names.
const ACCOUNT: &str = ::engine::EMULATOR_ACCOUNT_ID;

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The stack that deploys the two execution roles and the two functions.
const STACK: &str = "xsvc-handler-fan-out";

/// The role each function runs as, and the function running as it: the role that grants all
/// three calls, and the role that grants none of them beyond the log writes every execution
/// role carries.
const GRANTED_ROLE: &str = "xsvc-fan-out-granted-role";
const GRANTED_FUNCTION: &str = "xsvc-fan-out-caller";
const BARE_ROLE: &str = "xsvc-fan-out-bare-role";
const BARE_FUNCTION: &str = "xsvc-fan-out-bare-caller";

/// The module and attribute the deployment package carries its handler as. CloudFormation
/// archives inline code under the handler's own file stem, so the module is `index`.
const HANDLER: &str = "index.lambda_handler";

/// How long one invocation is given, as the function's own `Timeout`: generously past what a
/// run needs on any host, because nothing here is about how fast an interpreter gets through
/// three SDK calls.
const FUNCTION_TIMEOUT: u64 = 120;

/// The topic the handler publishes to, and the queue subscribed to it, which is where a
/// publication Amazon SNS accepted is read back.
const TOPIC: &str = "xsvc-fan-out-topic";
const TOPIC_SINK: &str = "xsvc-fan-out-topic-sink";

/// The queue the handler sends to directly, where its own message is read back.
const DIRECT_QUEUE: &str = "xsvc-fan-out-direct";

/// The rule that watches the default bus for the handler's own event, and the queue it targets.
const RULE: &str = "xsvc-fan-out-rule";
const BUS_SINK: &str = "xsvc-fan-out-bus-sink";

/// The bus an entry naming none is published onto, and the resource a publication onto it is
/// authorized on.
const DEFAULT_BUS: &str = "default";

/// The `source` and `detail-type` the handler publishes under, which is all the rule matches
/// on — so a message on the rule's queue is the handler's event and nothing else.
const SOURCE: &str = "com.demo.xsvc-handler-fan-out";
const DETAIL_TYPE: &str = "Handler Fan Out";

/// The `Type` member every Amazon SNS notification envelope declares.
const NOTIFICATION_TYPE: &str = "Notification";

/// The service principal each sink queue's policy admits, because a delivery to a queue is made
/// by the delivering service itself and "EventBridge relies on resource-based policies"
/// (`PutTargets`), as does Amazon SNS for an `sqs`-protocol subscription.
const SNS_PRINCIPAL: &str = "sns.amazonaws.com";
const EVENTS_PRINCIPAL: &str = "events.amazonaws.com";

/// The `StackStatus` the deployment must reach.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// How long work the CLOCK owes — a function becoming `Active`, a notification reaching a
/// subscribed queue, an event reaching a rule's target — is waited for before giving up, and
/// how long between two looks. A GENEROUS ceiling rather than an expectation: every wait here
/// is on the CONDITION, and nothing asserts how long anything took.
const DELIVERY_DEADLINE: Duration = Duration::from_secs(60);
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The whole emulator over one engine, exactly as `core/router/src/lib.rs` assembles it for the
/// binary: the handler's own SDK calls are dispatched back into this same router, so the topic,
/// the queue and the bus it reaches are the ones this suite created.
fn router() -> MultiServiceRouter {
    let mut router = aws_emulator_router::build_router(aws_emulator_router::new_engine());
    // The suite signs nothing: the credential scope is what routes a request. The HANDLER's own
    // calls are signed by the runtime, with the credential the invocation was given.
    router.set_request_authentication(false);
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. Its credential is the account's own published key, so every call this suite makes
/// on its own behalf is the account's and not any role's.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `awsQuery` call of `service`: the form body its parameters travel in, and the XML it
/// answered.
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

/// One Amazon SNS call that must be served, as the XML it carried.
async fn sns(
    router: &MultiServiceRouter,
    case: &str,
    action: &str,
    members: &[(&str, &str)],
) -> String {
    let (status, body) = query(router, "sns", "2010-03-31", action, members).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from SNS {action}, got {status} with {body}"
    );
    body
}

/// One `X-Amz-Target` RPC call that must be served, against the service whose signing name is
/// `signing_name`.
async fn rpc(
    router: &MultiServiceRouter,
    case: &str,
    signing_name: &str,
    target_prefix: &str,
    json_version: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("authorization", authorization(signing_name))
        .header(
            "content-type",
            format!("application/x-amz-json-{json_version}"),
        )
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
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from {signing_name} {operation}, got {status} with \
         {parsed}"
    );
    parsed
}

/// One Amazon SQS call that must be served (`awsJson1_0`, `AmazonSQS.<Operation>`).
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "sqs", "AmazonSQS", "1.0", operation, body).await
}

/// One EventBridge call that must be served (`awsJson1_1`, `AWSEvents.<Operation>`).
async fn events(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    rpc(router, case, "events", "AWSEvents", "1.1", operation, body).await
}

/// One CloudFormation call over its `awsQuery` wire.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(router, "cloudformation", "2010-05-15", action, members).await
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
    let parsed: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&body).to_string()))
    };
    (parts.status.as_u16(), parts.headers, parsed)
}

/// The ARN of a queue of this account in this Region, which is what a statement names and what
/// a refusal reports.
fn queue_arn(name: &str) -> String {
    format!("arn:aws:sqs:{REGION}:{ACCOUNT}:{name}")
}

/// The ARN of the topic of this account in this Region.
fn topic_arn() -> String {
    format!("arn:aws:sns:{REGION}:{ACCOUNT}:{TOPIC}")
}

/// The ARN of the default event bus of this account in this Region.
fn bus_arn() -> String {
    format!("arn:aws:events:{REGION}:{ACCOUNT}:event-bus/{DEFAULT_BUS}")
}

/// The assumed-role session ARN a call made by an invocation of `function` running as `role`
/// speaks for: the session a function's calls are made under is named for the FUNCTION, which
/// is how a refusal says which function was refused.
fn session_arn(role: &str, function: &str) -> String {
    format!("arn:aws:sts::{ACCOUNT}:assumed-role/{role}/{function}")
}

/// WHICH service the handler calls, and so which protocol its request is bound to and what the
/// destination does with what it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Destination {
    /// Amazon SNS `Publish` onto the topic, fanned out to the subscribed queue.
    Topic,
    /// Amazon SQS `SendMessage` onto the queue, held there for a receive.
    Queue,
    /// EventBridge `PutEvents` onto the default bus, matched by the rule and delivered to its
    /// target queue.
    Bus,
}

/// Every destination one invocation reaches, in the order the handler is asked to reach them.
const DESTINATIONS: [Destination; 3] = [Destination::Topic, Destination::Queue, Destination::Bus];

impl Destination {
    /// The case this destination names in a report.
    fn case(self) -> &'static str {
        match self {
            Self::Topic => "Amazon SNS Publish, over its awsQuery wire",
            Self::Queue => "Amazon SQS SendMessage, over its awsJson1_0 wire",
            Self::Bus => "EventBridge PutEvents, over its awsJson1_1 wire",
        }
    }

    /// The token the handler builds its client with, which is the service's own SDK id lower
    /// cased — what `boto3.client(...)` is given.
    fn sdk_token(self) -> &'static str {
        match self {
            Self::Topic => "sns",
            Self::Queue => "sqs",
            Self::Bus => "events",
        }
    }

    /// The client method the handler calls, which is the operation name as every AWS SDK for
    /// Python spells it.
    fn method(self) -> &'static str {
        match self {
            Self::Topic => "publish",
            Self::Queue => "send_message",
            Self::Bus => "put_events",
        }
    }

    /// The keyword arguments that call carries, which ARE the operation's input document.
    fn parameters(self, estate: &Estate, marker: &str) -> Value {
        match self {
            Self::Topic => json!({ "TopicArn": topic_arn(), "Message": marker }),
            Self::Queue => json!({ "QueueUrl": estate.direct_url, "MessageBody": marker }),
            Self::Bus => json!({
                "Entries": [{
                    "Source": SOURCE,
                    "DetailType": DETAIL_TYPE,
                    "Detail": json!({ "marker": marker }).to_string(),
                }],
            }),
        }
    }

    /// The IAM action that call performs, as a statement grants it: one `<namespace>:<ApiName>`
    /// per operation.
    fn action(self) -> &'static str {
        match self {
            Self::Topic => "sns:Publish",
            Self::Queue => "sqs:SendMessage",
            Self::Bus => "events:PutEvents",
        }
    }

    /// The resource that call addresses, which is what a statement names and what a refusal
    /// reports.
    fn resource(self) -> String {
        match self {
            Self::Topic => topic_arn(),
            Self::Queue => queue_arn(DIRECT_QUEUE),
            Self::Bus => bus_arn(),
        }
    }

    /// The error code the destination service refuses an unauthorized call with: the code its
    /// OWN model declares for that condition — Amazon SNS's `AuthorizationErrorException`
    /// travels as the legacy query code `AuthorizationError` its clients match on, Amazon SQS
    /// answers the AWS-common `AccessDenied` for operations whose model declares no
    /// authorization error, and EventBridge the AWS-common `AccessDeniedException`.
    fn refusal_code(self) -> &'static str {
        match self {
            Self::Topic => "AuthorizationError",
            Self::Queue => "AccessDenied",
            Self::Bus => "AccessDeniedException",
        }
    }

    /// The HTTP status that refusal comes back on, which is the destination service's own and
    /// not one status for all three.
    ///
    /// Amazon SNS's is its model's: `AuthorizationErrorException` carries `httpError` 403 and an
    /// `awsQueryError` whose `httpResponseCode` is 403
    /// (`smithy_query(query="shape", shape="AuthorizationErrorException")`). Amazon SQS's
    /// `SendMessage` and EventBridge's `PutEvents` declare no authorization error at all
    /// (`smithy_query(query="operation", operation="SendMessage"|"PutEvents")`), so each
    /// answers the AWS-common refusal of its own front door: 403 for Amazon SQS, which the
    /// sibling `lambda_in_function_sqs_authorization` pins, and the AWS JSON default for a
    /// client error, 400, for EventBridge.
    fn refusal_status(self) -> u64 {
        match self {
            Self::Topic | Self::Queue => 403,
            Self::Bus => 400,
        }
    }

    /// The id the destination service minted for what it took, read out of what the SDK gave
    /// the handler: a message id for the two message services, and the id of the one accepted
    /// entry for a publication onto a bus.
    fn minted(self, answered: &Value) -> Option<&str> {
        match self {
            Self::Topic | Self::Queue => answered["MessageId"].as_str(),
            Self::Bus => answered["Entries"][0]["EventId"].as_str(),
        }
    }

    /// The queue this destination's effect is read off.
    fn sink(self, estate: &Estate) -> &str {
        match self {
            Self::Topic => &estate.topic_sink_url,
            Self::Queue => &estate.direct_url,
            Self::Bus => &estate.bus_sink_url,
        }
    }

    /// The marker a message that arrived on that queue carries, read out of the body the queue
    /// holds: the notification envelope's own `Message` for a fan-out, the message body itself
    /// for a direct send, and the event envelope's `detail` for a bus delivery.
    fn marker_of(self, body: &str) -> Option<String> {
        match self {
            Self::Queue => Some(body.to_string()),
            Self::Topic => {
                let envelope: Value = serde_json::from_str(body).ok()?;
                assert_eq!(
                    envelope["Type"], json!(NOTIFICATION_TYPE),
                    "case {:?}: expected a notification envelope on the subscribed queue, got \
                     {envelope}",
                    self.case()
                );
                assert_eq!(
                    envelope["TopicArn"], json!(topic_arn()),
                    "case {:?}: expected the envelope to name the published topic, got {envelope}",
                    self.case()
                );
                envelope["Message"].as_str().map(str::to_string)
            }
            Self::Bus => {
                let envelope: Value = serde_json::from_str(body).ok()?;
                assert_eq!(
                    envelope["source"], json!(SOURCE),
                    "case {:?}: expected the handler's own source on the rule's queue, got \
                     {envelope}",
                    self.case()
                );
                envelope["detail"]["marker"].as_str().map(str::to_string)
            }
        }
    }
}

/// What the suite created before the functions were deployed: the queues each destination's
/// effect is read off, by the url a client addresses them with.
struct Estate {
    topic_sink_url: String,
    direct_url: String,
    bus_sink_url: String,
}

/// The handler both functions run: it makes the calls the event names, in order, with the SDK
/// the runtime supplies, and reports what each one answered — so the invocation's payload IS
/// what the SDK gave the guest, and a refusal it caught is in the payload rather than in the
/// invocation.
///
/// `import boto3` and every client are at MODULE scope, which is where a deployment package
/// holding only this module has to be able to resolve them, and the credentials the runtime
/// supplies have to be in force before the handler itself runs.
const FAN_OUT_HANDLER: &str = r#"
import boto3
import botocore.exceptions

CLIENTS = {
    "sns": boto3.client("sns"),
    "sqs": boto3.client("sqs"),
    "events": boto3.client("events"),
}


def lambda_handler(event, context):
    results = []
    for asked in event["calls"]:
        client = CLIENTS[asked["service"]]
        try:
            answered = getattr(client, asked["method"])(**asked["parameters"])
        except botocore.exceptions.ClientError as refused:
            results.append(
                {
                    "ok": False,
                    "code": refused.response["Error"]["Code"],
                    "message": refused.response["Error"]["Message"],
                    "status": refused.response["ResponseMetadata"]["HTTPStatusCode"],
                }
            )
            continue
        results.append(
            {
                "ok": True,
                "status": answered["ResponseMetadata"]["HTTPStatusCode"],
                "answered": {
                    name: value
                    for name, value in answered.items()
                    if name != "ResponseMetadata"
                },
            }
        )
    return {"results": results}
"#;

/// The trust policy a Lambda execution role carries: this service has to be able to assume the
/// role for a function to be handed it at all.
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

/// The statement every execution role carries whatever else it does: the log writes a function
/// makes, which grant nothing of any destination service.
fn logs_statement() -> Value {
    json!({
        "Effect": "Allow",
        "Action": ["logs:CreateLogGroup", "logs:CreateLogStream", "logs:PutLogEvents"],
        "Resource": "*",
    })
}

/// The inline document one role carries: the log writes, plus — for the granted role — one
/// statement per destination naming that destination's own action on its own resource, so a
/// refusal is about the action and the resource together.
fn role_document(granted: bool) -> Value {
    let mut statements = vec![logs_statement()];
    if granted {
        for destination in DESTINATIONS {
            statements.push(json!({
                "Effect": "Allow",
                "Action": destination.action(),
                "Resource": destination.resource(),
            }));
        }
    }
    json!({ "Version": "2012-10-17", "Statement": statements })
}

/// The template the suite deploys: one execution role per grant state, and one function running
/// as each. The inline source is archived into the deployment package by CloudFormation, which
/// is what makes the package hold exactly ONE module — the handler's — and no SDK of its own.
fn stack_template() -> String {
    let mut resources = serde_json::Map::new();
    for (granted, role, function, role_logical, function_logical) in [
        (true, GRANTED_ROLE, GRANTED_FUNCTION, "GrantedRole", "GrantedCaller"),
        (false, BARE_ROLE, BARE_FUNCTION, "BareRole", "BareCaller"),
    ] {
        resources.insert(
            role_logical.to_string(),
            json!({
                "Type": "AWS::IAM::Role",
                "Properties": {
                    "RoleName": role,
                    "AssumeRolePolicyDocument": trust_policy(),
                    "Policies": [{
                        "PolicyName": "fan-out-grants",
                        "PolicyDocument": role_document(granted),
                    }],
                },
            }),
        );
        resources.insert(
            function_logical.to_string(),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": "python3.12",
                    "Handler": HANDLER,
                    "Role": {"Fn::GetAtt": [role_logical, "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Code": {"ZipFile": FAN_OUT_HANDLER},
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

/// A queue created where a client creates one, carrying the policy that admits `principal` to
/// send to it when the queue is a delivery SINK and no policy at all when it is not; answers its
/// url and the ARN it reports.
async fn created_queue(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    principal: Option<&str>,
) -> (String, String) {
    let created = sqs(router, case, "CreateQueue", &json!({ "QueueName": name })).await;
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueUrl in {created}"))
        .to_string();
    let attributes = sqs(
        router,
        case,
        "GetQueueAttributes",
        &json!({ "QueueUrl": &url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a QueueArn in {attributes}"))
        .to_string();
    assert_eq!(
        arn,
        queue_arn(name),
        "case {case:?}: expected the queue ARN of this account and Region, got {arn}"
    );
    if let Some(principal) = principal {
        let policy = json!({
            "Version": "2012-10-17",
            "Statement": [{
                "Sid": "AllowDelivery",
                "Effect": "Allow",
                "Principal": {"Service": principal},
                "Action": "sqs:SendMessage",
                "Resource": &arn,
            }],
        })
        .to_string();
        sqs(
            router,
            case,
            "SetQueueAttributes",
            &json!({ "QueueUrl": &url, "Attributes": { "Policy": policy } }),
        )
        .await;
    }
    (url, arn)
}

/// The topic, the three queues, the subscription and the rule — each created through the owning
/// service's own API, so every destination the handler reaches is one a client could have made.
async fn given_estate(router: &MultiServiceRouter) -> Estate {
    let case = "setup";
    let created = sns(router, case, "CreateTopic", &[("Name", TOPIC)]).await;
    let arn = element(&created, "TopicArn");
    assert_eq!(
        arn,
        topic_arn(),
        "case {case:?}: expected the topic ARN of this account and Region, got {arn}"
    );

    let (topic_sink_url, topic_sink_arn) =
        created_queue(router, case, TOPIC_SINK, Some(SNS_PRINCIPAL)).await;
    let subscribed = sns(
        router,
        case,
        "Subscribe",
        &[
            ("TopicArn", &topic_arn()),
            ("Protocol", "sqs"),
            ("Endpoint", &topic_sink_arn),
            ("ReturnSubscriptionArn", "true"),
        ],
    )
    .await;
    let subscription = element(&subscribed, "SubscriptionArn");
    assert!(
        subscription.starts_with(&topic_arn()),
        "case {case:?}: expected the subscription ARN to extend the topic's, got {subscription}"
    );

    let (direct_url, _) = created_queue(router, case, DIRECT_QUEUE, None).await;
    let (bus_sink_url, bus_sink_arn) =
        created_queue(router, case, BUS_SINK, Some(EVENTS_PRINCIPAL)).await;

    let pattern = json!({ "source": [SOURCE], "detail-type": [DETAIL_TYPE] }).to_string();
    events(
        router,
        case,
        "PutRule",
        &json!({
            "Name": RULE,
            "EventBusName": DEFAULT_BUS,
            "EventPattern": pattern,
            "State": "ENABLED",
        }),
    )
    .await;
    let targets = events(
        router,
        case,
        "PutTargets",
        &json!({
            "Rule": RULE,
            "EventBusName": DEFAULT_BUS,
            "Targets": [{ "Id": "sink", "Arn": bus_sink_arn }],
        }),
    )
    .await;
    assert_eq!(
        targets["FailedEntryCount"],
        json!(0),
        "case {case:?}: expected PutTargets to accept the queue target, got {targets}"
    );

    Estate {
        topic_sink_url,
        direct_url,
        bus_sink_url,
    }
}

/// Deploy the stack and wait for `CREATE_COMPLETE`, then wait until each function reports
/// `Active` — both are owed by the CLOCK, so each is waited for by looking again.
async fn given_functions(router: &MultiServiceRouter) {
    let template = stack_template();
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares NAMED IAM roles, which a create acknowledges before
            // CloudFormation provisions them.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: expected CreateStack({STACK:?}) to be accepted, got {status} with {answered}"
    );
    let (reported, body) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let reasons = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events_body) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
        elements(&events_body, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: expected the stack to reach {CREATE_COMPLETE}, got {reported} ({body}) with \
         resource failures {reasons:?}"
    );
    for function in [GRANTED_FUNCTION, BARE_FUNCTION] {
        await_active(router, function).await;
    }
}

/// Wait until Lambda reports `function` `Active`, which is the state an invocation needs.
async fn await_active(router: &MultiServiceRouter, function: &str) {
    let deadline = Instant::now() + DELIVERY_DEADLINE;
    loop {
        let (_, _, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            &[],
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "setup: expected Lambda to report {function:?} Active, last reported {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `Invoke` `function` synchronously, asking the handler to make one call per destination
/// carrying `marker`, and answer one result per call.
///
/// The invocation itself must be HTTP 200 with no `FunctionError` whichever way the calls went:
/// the handler catches a refusal and reports it, so a function error here is the crash the
/// runtime must not produce.
async fn invoked(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    estate: &Estate,
    marker: &str,
) -> Vec<Value> {
    let calls: Vec<Value> = DESTINATIONS
        .iter()
        .map(|destination| {
            json!({
                "service": destination.sdk_token(),
                "method": destination.method(),
                "parameters": destination.parameters(estate, marker),
            })
        })
        .collect();
    let event = json!({ "calls": calls });
    let (status, headers, payload) = lambda(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{function}/invocations"),
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
    assert_eq!(
        signal, None,
        "case {case:?}: expected no FunctionError, got {signal:?} with {payload}"
    );
    let results = payload["results"].as_array().cloned().unwrap_or_default();
    assert_eq!(
        results.len(),
        DESTINATIONS.len(),
        "case {case:?}: expected one result per call the handler was asked to make, got {payload}"
    );
    results
}

/// Every marker that has arrived on one destination's queue, taken off it as it arrives and
/// accumulated across the whole suite, so a "nothing arrived" read is made over everything the
/// queue ever held rather than over one receive.
struct Watch {
    destination: Destination,
    seen: Vec<String>,
}

impl Watch {
    fn new(destination: Destination) -> Self {
        Self {
            destination,
            seen: Vec::new(),
        }
    }

    /// Look again at the queue until `wanted` has arrived on it, or the generous deadline
    /// passes; everything received on the way is kept.
    async fn until(
        &mut self,
        router: &MultiServiceRouter,
        case: &str,
        estate: &Estate,
        wanted: &str,
    ) {
        let url = self.destination.sink(estate);
        let deadline = Instant::now() + DELIVERY_DEADLINE;
        loop {
            let received = sqs(
                router,
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
            )
            .await;
            for message in received["Messages"].as_array().cloned().unwrap_or_default() {
                let body = message["Body"].as_str().unwrap_or_default();
                if let Some(marker) = self.destination.marker_of(body) {
                    self.seen.push(marker);
                }
            }
            if self.seen.iter().any(|marker| marker == wanted) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "case {case:?}: expected {wanted:?} to arrive on the queue of {:?}, got {:?}",
                self.destination.case(),
                self.seen
            );
            tokio::time::sleep(LOOK_AGAIN).await;
        }
    }

    /// Whether `marker` has ever arrived on that queue.
    fn holds(&self, marker: &str) -> bool {
        self.seen.iter().any(|seen| seen == marker)
    }
}

/// Make one destination's call AS THE ACCOUNT, carrying `marker`: the positive control that the
/// delivery path still works, so a marker that is absent after it arrived is absent because the
/// handler's own call was refused and not because nothing had been delivered yet.
async fn planted(
    router: &MultiServiceRouter,
    case: &str,
    destination: Destination,
    estate: &Estate,
    marker: &str,
) {
    match destination {
        Destination::Topic => {
            let answered = sns(
                router,
                case,
                "Publish",
                &[("TopicArn", &topic_arn()), ("Message", marker)],
            )
            .await;
            assert!(
                !element(&answered, "MessageId").is_empty(),
                "case {case:?}: expected the account's own publication to be served, got \
                 {answered}"
            );
        }
        Destination::Queue => {
            sqs(
                router,
                case,
                "SendMessage",
                &json!({ "QueueUrl": &estate.direct_url, "MessageBody": marker }),
            )
            .await;
        }
        Destination::Bus => {
            let answered = events(
                router,
                case,
                "PutEvents",
                &json!({
                    "Entries": [{
                        "Source": SOURCE,
                        "DetailType": DETAIL_TYPE,
                        "Detail": json!({ "marker": marker }).to_string(),
                    }],
                }),
            )
            .await;
            assert_eq!(
                answered["FailedEntryCount"],
                json!(0),
                "case {case:?}: expected the account's own publication to be accepted, got \
                 {answered}"
            );
        }
    }
}

/// RULE: a `python3.x` handler whose package holds only its own module reaches every service the
/// runtime's SDK covers — each over that service's own protocol, signed as a session of the
/// function's execution role — so a call the role's policies grant is served and its effect is
/// where a client reads it, a call they do not grant is refused with that service's own
/// authorization code and leaves nothing, and the invocation is 200 with no `FunctionError`
/// either way.
#[tokio::test]
async fn a_running_handler_reaches_sns_sqs_and_eventbridge_as_its_execution_role() {
    let router = router();
    let estate = given_estate(&router).await;
    given_functions(&router).await;

    let mut watches: Vec<Watch> = DESTINATIONS.into_iter().map(Watch::new).collect();

    // The role grants all three calls: each is served, each answers the id its own service
    // minted, and each effect is on the queue that destination is read off.
    let granted = "the execution role grants every call the handler makes";
    let granted_marker = "granted-marker";
    let results = invoked(&router, granted, GRANTED_FUNCTION, &estate, granted_marker).await;
    for (destination, result) in DESTINATIONS.into_iter().zip(&results) {
        let case = destination.case();
        assert_eq!(
            result["ok"],
            json!(true),
            "case {case:?}: expected the call to be served, got {result}"
        );
        assert_eq!(
            result["status"],
            json!(200),
            "case {case:?}: expected the SDK to report HTTP 200, got {result}"
        );
        let minted = destination.minted(&result["answered"]).unwrap_or_default();
        assert!(
            !minted.is_empty(),
            "case {case:?}: expected the service's own minted id beside the answer, got {result}"
        );
        if destination == Destination::Bus {
            assert_eq!(
                result["answered"]["FailedEntryCount"],
                json!(0),
                "case {case:?}: expected no failed entry, got {result}"
            );
        }
    }
    for watch in &mut watches {
        let case = watch.destination.case();
        watch.until(&router, case, &estate, granted_marker).await;
    }

    // The same handler, running as a role granting only the log writes every execution role
    // carries: every call is refused by the service it was made to, with that service's own
    // code, its status and the sentence naming the session the call really spoke for.
    let denied = "the execution role grants none of them";
    let denied_marker = "denied-marker";
    let results = invoked(&router, denied, BARE_FUNCTION, &estate, denied_marker).await;
    let session = session_arn(BARE_ROLE, BARE_FUNCTION);
    for (destination, result) in DESTINATIONS.into_iter().zip(&results) {
        let case = destination.case();
        assert_eq!(
            result["ok"],
            json!(false),
            "case {case:?}: expected the call to be refused, got {result}"
        );
        assert_eq!(
            result["code"],
            json!(destination.refusal_code()),
            "case {case:?}: expected the service's own authorization code, got {result}"
        );
        let status = destination.refusal_status();
        assert_eq!(
            result["status"],
            json!(status),
            "case {case:?}: expected the refusal on HTTP {status}, got {result}"
        );
        // The sentence names the principal the call spoke for, the action it asked for and the
        // resource it addressed, and then why the grant was withheld. Compared case-blind on the
        // action alone: WHICH case a service spells its own action namespace in is that
        // service's own, pinned by that service's suites, and not what this one is about.
        let head = format!(
            "User: {session} is not authorized to perform: {} on resource: {}",
            destination.action(),
            destination.resource()
        );
        let message = result["message"].as_str().unwrap_or_default();
        assert!(
            message.to_lowercase().starts_with(&head.to_lowercase()),
            "case {case:?}: expected a refusal beginning {head:?}, got {message:?}"
        );
        assert!(
            message.len() > head.len(),
            "case {case:?}: expected a reason clause after the resource, got {message:?}"
        );
    }

    // And nothing of that invocation reached any destination. The read is taken at an instant by
    // which a message planted through the SAME path as the account has already arrived, so the
    // absence is the refusal's and not the clock's.
    let control = "a message planted by the account after the refused invocation";
    let sentinel = "sentinel-marker";
    for destination in DESTINATIONS {
        planted(&router, control, destination, &estate, sentinel).await;
    }
    for watch in &mut watches {
        let case = watch.destination.case();
        watch.until(&router, case, &estate, sentinel).await;
        assert!(
            !watch.holds(denied_marker),
            "case {case:?}: expected nothing of the refused invocation on the queue, got {:?}",
            watch.seen
        );
    }
}
