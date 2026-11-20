// SPDX-License-Identifier: CC0-1.0

//! What an EventBridge Scheduler schedule's OWN dead-letter queue holds, and what it does
//! not: end to end over the real `MultiServiceRouter` with EventBridge Scheduler, Lambda,
//! Amazon SQS, IAM and CloudFormation mounted on ONE engine.
//!
//! RULE: `Target$DeadLetterConfig` names the queue EventBridge Scheduler "delivers failed
//! events that could not be successfully delivered to a target" to, so what reaches that
//! queue is an invocation the schedule could not HAND OVER — the target's own API refused
//! the call, or the schedule's execution role withheld it — and never one that API ACCEPTED.
//! A function target is handed its event asynchronously ("Event — invoke the function
//! asynchronously", `Invoke$InvocationType`), so an `Invoke` Lambda accepted is a delivery
//! the schedule MADE, whatever the run then comes to: what a handler that fails inside
//! itself earns is Lambda's own asynchronous retry and the function's
//! `DestinationConfig$OnFailure` record (`PutFunctionEventInvokeConfig`), not an event on
//! the queue the schedule names.
//!
//! Only a cross-service suite can pin that distinction: the schedule, the occurrence and the
//! dead-letter event are EventBridge Scheduler's, the invocation, the run and the failure
//! record are Lambda's, the execution role is IAM's, all three queues are Amazon SQS's, and
//! the handlers that really run are archived into their deployment packages by
//! CloudFormation. Each half alone proves nothing: a queue that stays empty because nothing
//! was ever invoked would read exactly like one that stays empty because the invocation
//! succeeded, so every row that is NOT dead-lettered is also read where its invocation left
//! its own mark.
//!
//! The rows differ along what the rule generalises over — what became of the invocation —
//! and span both sides of it: a handler that raises and one that returns (both DELIVERED), a
//! function this account never held, one deleted before the occurrence, and one the
//! execution role may not invoke (none delivered). They differ along the form of
//! `Target$Input` as well (a flat object, a nested one, a top-level array, text that is not
//! JSON), because the event the queue is handed is the request the attempt made whatever
//! that input was.
//!
//! Nothing here waits for a fixed span: every schedule names an instant seconds ahead and
//! each wait watches for the CONDITION — the messages a queue holds — until a generous
//! deadline.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::query_wire::{element, form_encode};

/// The Region every request below is signed for, and the one every ARN carries.
const REGION: &str = "eu-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long each wait below watches for the condition it expects, and how often it looks.
/// Every schedule names an instant [`OCCURRENCE_IN`] ahead, so the deadline is room for the
/// clock's tick, the invocation and the run on a loaded host rather than a latency
/// expectation of its own: it is reached only where nothing happened at all.
const PATIENCE: Duration = Duration::from_secs(180);
/// See [`PATIENCE`].
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long each queue keeps being watched after everything expected has arrived, so a
/// message that should never come has had its chance to. Nothing is asserted about how long
/// anything took: the window is only how long the absence is watched for.
const QUIET_WINDOW: Duration = Duration::from_secs(10);

/// How far ahead of its creation every schedule below places its one occurrence: far enough
/// that the whole table is written before the first one falls due, and close enough for one
/// poll loop to watch it.
const OCCURRENCE_IN: Duration = Duration::from_secs(8);

/// The status a stack reports once it has deployed what it declares.
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The `State` a function reports once it can be invoked (`FunctionConfiguration$State`:
/// "Active" once "the function is ready to be invoked").
const FUNCTION_ACTIVE: &str = "Active";

/// Where Lambda's function operations are bound (`/2015-03-31/functions`), and where its
/// asynchronous-invocation configuration is (`/2019-09-25/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
/// See [`FUNCTIONS_URI`].
const EVENT_INVOKE_CONFIG_URI: &str = "/2019-09-25/functions";

/// The queues this suite reads: where the returning handler forwards its own marker, where
/// the functions' on-failure records land, and the one every schedule names for its failed
/// events.
const SINK_QUEUE: &str = "scheduler-function-error-sink";
/// See [`SINK_QUEUE`].
const DESTINATION_QUEUE: &str = "scheduler-function-error-destination";
/// See [`SINK_QUEUE`].
const DEAD_LETTER_QUEUE: &str = "scheduler-function-error-dlq";

/// The role the functions run as, the role the schedules are invoked under, and the role of
/// the row whose invocation is withheld.
const FUNCTION_ROLE: &str = "scheduler-function-error-exec";
/// See [`FUNCTION_ROLE`].
const INVOKING_ROLE: &str = "scheduler-function-error-invoking";
/// See [`FUNCTION_ROLE`].
const WITHHOLDING_ROLE: &str = "scheduler-function-error-withholding";

/// The functions this suite deploys, the one it creates and deletes again, and the name no
/// `CreateFunction` was ever called with.
const RAISING_FUNCTION: &str = "scheduler-function-error-raising";
/// See [`RAISING_FUNCTION`].
const RETURNING_FUNCTION: &str = "scheduler-function-error-returning";
/// See [`RAISING_FUNCTION`].
const DELETED_FUNCTION: &str = "scheduler-function-error-deleted";
/// See [`RAISING_FUNCTION`].
const ABSENT_FUNCTION: &str = "scheduler-function-error-absent";

/// The stack that archives both handlers into their deployment packages.
const FUNCTION_STACK: &str = "scheduler-function-error-stack";

/// A deployment package as `Code.ZipFile` carries it: an empty archive. It belongs to the
/// function that is DELETED before its occurrence, so nothing ever runs it; what it has to
/// be is a package `CreateFunction` accepts.
const EMPTY_PACKAGE: &str = "UEsDBAoAAAAAAAAAAAA=";

/// The IAM action an invocation of a function performs.
const INVOKE_FUNCTION: &str = "lambda:InvokeFunction";

/// The `InvocationType` a schedule's function target is invoked with, which the request a
/// failed event carries therefore names.
const ASYNCHRONOUS: &str = "Event";

/// The refusal Lambda answers an invoke of a function it does not hold with
/// (`ResourceNotFoundException`, HTTP 404).
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// The refusal reported for a call the schedule's own execution role withheld, in the modern
/// form Lambda's protocol spells AWS's authorization refusal in.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The `responseContext.functionError` Lambda reports for a run whose handler raised ("If
/// present, indicates that an error occurred during function execution", `Invoke`).
const UNHANDLED: &str = "Unhandled";

/// The `requestContext.condition` an on-failure record carries for an event whose delivery
/// attempts are spent.
const RETRIES_EXHAUSTED: &str = "RetriesExhausted";

/// The handler of the function that FAILS inside itself: it raises for every event, naming
/// the event it was handed, so the record the destination queue holds is its own account of
/// that event.
const RAISING_HANDLER: &str = r#"
import json


def handler(event, context):
    raise RuntimeError("the handler refused " + json.dumps(event, separators=(",", ":")))
"#;

/// The handler of the function that RETURNS: it forwards the marker of the event it was
/// handed to the sink queue, which is where this suite reads that the invocation happened at
/// all. The queue is addressed by name, resolved through Amazon SQS's own `GetQueueUrl`, so
/// the source carries nothing the deployment had to compute.
const RETURNING_HANDLER: &str = r#"
import boto3


def handler(event, context):
    client = boto3.client("sqs")
    url = client.get_queue_url(QueueName="SINK_QUEUE_NAME")["QueueUrl"]
    client.send_message(QueueUrl=url, MessageBody=event["marker"])
    return {"forwarded": event["marker"]}
"#;

/// The five services this flow spans, on ONE engine exactly as `core/router/src/lib.rs`
/// mounts them. Request authentication is off, as in every other suite here; the calls this
/// emulator makes to itself sign themselves either way.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_cloudformation_cloudformation::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_scheduler_awschronosservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`],
/// which is what routes the request. The signature is never checked: this router runs with
/// request authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// One awsQuery call (CloudFormation, IAM) over its own wire.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
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
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// One CloudFormation call.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    query(
        router,
        "cloudformation",
        &format!("cloudformation.{REGION}.amazonaws.com"),
        "2010-05-15",
        action,
        members,
    )
    .await
}

/// One IAM call. IAM is global; the request is signed for this suite's Region only because
/// that is what routes it.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> (u16, String) {
    query(
        router,
        "iam",
        "iam.amazonaws.com",
        "2010-05-08",
        action,
        members,
    )
    .await
}

/// One Amazon SQS call over its `awsJson1_0` wire.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .header("authorization", authorization("sqs"))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One AWS Lambda call over its `restJson1` wire.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(ToString::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("lambda"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// One EventBridge Scheduler call over its `restJson1` wire.
async fn scheduler(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|body| body.to_string()).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("scheduler.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("scheduler"))
        .body(Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// This account's ARN for the function `name` in the Region this suite drives.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// The ARN a role of `name` carries. IAM is global, so the ARN carries no Region.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// The ARN the schedule `name` of the default group carries.
fn schedule_arn(name: &str) -> String {
    format!("arn:aws:scheduler:{REGION}:{ACCOUNT}:schedule/default/{name}")
}

/// Where one function's asynchronous-invocation configuration is addressed.
fn config_uri(function: &str) -> String {
    format!("{EVENT_INVOKE_CONFIG_URI}/{function}/event-invoke-config")
}

/// Create one queue and answer its URL and its ARN, read through Amazon SQS's own API.
async fn given_queue(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let (status, created) = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "setup: CreateQueue({name:?}) must succeed, got {status} with {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no QueueUrl in {created}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: GetQueueAttributes({name:?}) must succeed, got {status} with {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("setup: no QueueArn in {attributes}"))
        .to_string();
    (url, arn)
}

/// A role `principal` may assume, granted each `(action, resource)` pair it needs, and
/// answer its ARN.
async fn given_role(
    router: &MultiServiceRouter,
    name: &str,
    principal: &str,
    grants: &[(&str, &str)],
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let (status, created) = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateRole({name:?}) must succeed, got {status} with {created}"
    );
    let statements: Vec<Value> = grants
        .iter()
        .map(|(action, resource)| {
            json!({ "Effect": "Allow", "Action": action, "Resource": resource })
        })
        .collect();
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    let (status, written) = iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "what-this-role-may-do"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutRolePolicy({name:?}) must succeed, got {status} with {written}"
    );
    role_arn(name)
}

/// Deploy the stack that archives both handlers into their deployment packages, and answer
/// once CloudFormation reports it complete.
async fn given_deployed_functions(router: &MultiServiceRouter, role: &str) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Raising": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": RAISING_FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role,
                    "Timeout": 30,
                    "Code": { "ZipFile": RAISING_HANDLER },
                },
            },
            "Returning": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": RETURNING_FUNCTION,
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": role,
                    "Timeout": 30,
                    "Code": { "ZipFile": RETURNING_HANDLER.replace("SINK_QUEUE_NAME", SINK_QUEUE) },
                },
            },
        },
    })
    .to_string();
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[("StackName", FUNCTION_STACK), ("TemplateBody", &template)],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({FUNCTION_STACK:?}) must be accepted, got {status} with {answered}"
    );
    let began = Instant::now();
    loop {
        let (status, described) =
            cloudformation(router, "DescribeStacks", &[("StackName", FUNCTION_STACK)]).await;
        assert_eq!(
            status, 200,
            "setup: DescribeStacks must answer, got {status} with {described}"
        );
        let reported = element(&described, "StackStatus");
        if reported == CREATE_COMPLETE {
            return;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "setup: the stack must reach {CREATE_COMPLETE}, last reported {reported:?} in \
             {described}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Answer once Lambda reports `function` ready to be invoked.
async fn given_active(router: &MultiServiceRouter, function: &str) {
    let began = Instant::now();
    loop {
        let (status, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{function}/configuration"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "setup: GetFunctionConfiguration({function:?}) must answer, got {status} with \
             {configuration}"
        );
        let state = configuration["State"].as_str().unwrap_or_default();
        if state == FUNCTION_ACTIVE {
            return;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "setup: the function {function:?} must reach {FUNCTION_ACTIVE}, last reported \
             {state:?}"
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }
}

/// Create the function one row names and DELETE it again, so an invocation of it is refused
/// by Lambda the way it refuses one of a function nothing holds.
async fn given_deleted_function(router: &MultiServiceRouter, role: &str) {
    let (status, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        Some(&json!({
            "FunctionName": DELETED_FUNCTION,
            "Role": role,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": { "ZipFile": EMPTY_PACKAGE },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup: CreateFunction({DELETED_FUNCTION:?}) must succeed, got {status} with {created}"
    );
    let (status, deleted) = lambda(
        router,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{DELETED_FUNCTION}"),
        None,
    )
    .await;
    assert_eq!(
        status, 204,
        "setup: DeleteFunction({DELETED_FUNCTION:?}) must succeed, got {status} with {deleted}"
    );
}

/// Configure `function` to write the record of an event it could not process to
/// `destination`, with no retry of its own — so the event's one run decides the record, and
/// the record is read back through the operation that reports it rather than from the put's
/// own answer.
async fn given_failure_destination(
    router: &MultiServiceRouter,
    function: &str,
    destination: &str,
) {
    let (status, configured) = lambda(
        router,
        "PUT",
        &config_uri(function),
        Some(&json!({
            "DestinationConfig": { "OnFailure": { "Destination": destination } },
            "MaximumRetryAttempts": 0,
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: PutFunctionEventInvokeConfig({function:?}) must succeed, got {status} with \
         {configured}"
    );
    let (status, held) = lambda(router, "GET", &config_uri(function), None).await;
    assert_eq!(
        (status, held["DestinationConfig"]["OnFailure"]["Destination"].as_str()),
        (200, Some(destination)),
        "setup: Lambda must hold {function:?}'s on-failure destination, got {status} with {held}"
    );
}

/// `at` as an `at(...)` expression's body states it: a local date and time with no offset.
fn at_expression_body(at: SystemTime) -> String {
    let rendered = ::aws_smithy_types::DateTime::from(at)
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant renders as ISO-8601");
    rendered.trim_end_matches('Z').to_string()
}

/// A whole number of seconds ahead of now, which is the resolution an `at(...)` expression
/// states an instant at.
fn whole_seconds_ahead(ahead: Duration) -> SystemTime {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    SystemTime::UNIX_EPOCH + Duration::from_secs(now) + ahead
}

/// What one schedule's target points at: the function it invokes, the role it is invoked
/// under, the queue its failed events go to, and the input it hands over.
struct Pointing<'a> {
    arn: &'a str,
    role: &'a str,
    queue: &'a str,
    input: &'a str,
}

/// A one-time schedule pointing where `pointing` says at `occurrence`, with no retry of its
/// own — so every failure below is the invocation's first and only attempt.
async fn given_schedule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    occurrence: SystemTime,
    pointing: &Pointing<'_>,
) {
    let (status, created) = scheduler(
        router,
        "POST",
        &format!("/schedules/{name}"),
        Some(json!({
            "ScheduleExpression": format!("at({})", at_expression_body(occurrence)),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": {
                "Arn": pointing.arn,
                "RoleArn": pointing.role,
                "Input": pointing.input,
                "RetryPolicy": { "MaximumRetryAttempts": 0 },
                "DeadLetterConfig": { "Arn": pointing.queue },
            },
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateSchedule must succeed, got {status} with {created}"
    );
}

/// Add every message the queue `url` holds by this look to `seen`, keyed by the `MessageId`
/// Amazon SQS minted for it.
///
/// A received message becomes visible again once its visibility timeout lapses, so keying by
/// that id is what makes one message count once however many looks return it — and makes the
/// count a later look reads an accumulation rather than a snapshot.
async fn gather(router: &MultiServiceRouter, url: &str, seen: &mut BTreeMap<String, Value>) {
    let (status, received) = sqs(
        router,
        "ReceiveMessage",
        &json!({
            "QueueUrl": url,
            "MaxNumberOfMessages": 10,
            "MessageAttributeNames": ["All"],
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "ReceiveMessage({url}) must answer, got {status} with {received}"
    );
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        let id = message["MessageId"].as_str().unwrap_or_default().to_string();
        seen.insert(id, message);
    }
}

/// One message's body as the document it is.
fn body_document(message: &Value) -> Value {
    let body = message["Body"].as_str().unwrap_or_default();
    serde_json::from_str(body).unwrap_or(Value::Null)
}

/// The value a message carries under the attribute `name`.
fn attribute(message: &Value, name: &str) -> String {
    message["MessageAttributes"][name]["StringValue"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// What became of one row's invocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Became {
    /// Lambda ACCEPTED the invocation and the handler RAISED: a delivery the schedule made,
    /// whose failure is the function's own — so the record lands at the function's
    /// configured on-failure destination.
    AcceptedAndRaised,
    /// Lambda accepted the invocation and the handler RETURNED, forwarding its own marker.
    AcceptedAndReturned,
    /// The target names a function this account never held, so Lambda refused the call.
    NeverHeld,
    /// The function was DELETED before the occurrence, which is the same absence.
    Deleted,
    /// The schedule's execution role may not invoke the function, so the call was withheld.
    Withheld,
}

impl Became {
    /// The function the row's target names.
    fn target(self) -> String {
        function_arn(match self {
            Self::AcceptedAndRaised => RAISING_FUNCTION,
            // The withheld row names the function that WOULD have run, so what stops it is
            // the role alone.
            Self::AcceptedAndReturned | Self::Withheld => RETURNING_FUNCTION,
            Self::NeverHeld => ABSENT_FUNCTION,
            Self::Deleted => DELETED_FUNCTION,
        })
    }

    /// The error code the schedule's dead-letter event reports, or `None` for an invocation
    /// the target accepted — which is dead-lettered nowhere.
    fn dead_letter_code(self) -> Option<&'static str> {
        match self {
            Self::AcceptedAndRaised | Self::AcceptedAndReturned => None,
            Self::NeverHeld | Self::Deleted => Some(RESOURCE_NOT_FOUND),
            Self::Withheld => Some(ACCESS_DENIED),
        }
    }

    /// Whether the row's schedule is invoked under the role that may NOT invoke the
    /// function.
    fn withheld(self) -> bool {
        self == Self::Withheld
    }
}

/// One row: what the schedule hands its target, and what became of the invocation.
struct Row {
    case: &'static str,
    /// `Target.Input`, in the form this row carries it.
    input: &'static str,
    became: Became,
}

/// The marker the accepted-and-raised row's event carries, and the one the
/// accepted-and-returned row's handler forwards: each row's event is identified by its own.
const RAISED_MARKER: &str = "scheduler-function-error-raised";
/// See [`RAISED_MARKER`].
const RETURNED_MARKER: &str = "scheduler-function-error-returned";

const ROWS: &[Row] = &[
    Row {
        case: "a flat object whose handler raises",
        input: r#"{"marker":"scheduler-function-error-raised"}"#,
        became: Became::AcceptedAndRaised,
    },
    Row {
        case: "a flat object whose handler returns",
        input: r#"{"marker":"scheduler-function-error-returned"}"#,
        became: Became::AcceptedAndReturned,
    },
    Row {
        case: "a top-level array for a function this account never held",
        input: r#"[1,"two",{"three":3}]"#,
        became: Became::NeverHeld,
    },
    Row {
        case: "text that is not JSON for a function deleted before the occurrence",
        input: "a line of plain text, not JSON",
        became: Became::Deleted,
    },
    Row {
        case: "a nested object whose invocation the execution role withholds",
        input: r#"{"marker":"withheld","detail":{"depth":2}}"#,
        became: Became::Withheld,
    },
];

/// What one row's schedule asked for, kept so the assertions read the request the invocation
/// was given rather than rebuilding it.
struct Prepared<'a> {
    row: &'a Row,
    /// The `Invoke` request the attempt makes of Lambda, which is the body a failed event
    /// carries.
    request: Value,
    schedule_arn: String,
}

/// Every row: the occurrence hands the event to Lambda's own `Invoke`, so an invocation
/// Lambda ACCEPTED leaves its mark where the run leaves it — the function's on-failure record
/// for the handler that raised, the sink queue for the handler that returned — and reaches
/// the schedule's dead-letter queue nowhere; while an invocation that could not be handed
/// over at all is the one that queue holds, under the code the refusal answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn only_an_undelivered_invocation_reaches_the_schedules_dead_letter_queue() {
    let router = build_router();
    let (sink_url, sink_arn) = given_queue(&router, SINK_QUEUE).await;
    let (destination_url, destination_arn) = given_queue(&router, DESTINATION_QUEUE).await;
    let (dead_letter_url, dead_letter_arn) = given_queue(&router, DEAD_LETTER_QUEUE).await;

    // The functions run as one role: it may resolve and write the sink queue, which is the
    // returning handler's own call, and write the on-failure record.
    let function_role = given_role(
        &router,
        FUNCTION_ROLE,
        "lambda.amazonaws.com",
        &[
            ("sqs:GetQueueUrl", sink_arn.as_str()),
            ("sqs:SendMessage", sink_arn.as_str()),
            ("sqs:SendMessage", destination_arn.as_str()),
        ],
    )
    .await;
    given_deployed_functions(&router, &function_role).await;
    given_active(&router, RAISING_FUNCTION).await;
    given_active(&router, RETURNING_FUNCTION).await;
    given_deleted_function(&router, &function_role).await;
    for function in [RAISING_FUNCTION, RETURNING_FUNCTION] {
        given_failure_destination(&router, function, &destination_arn).await;
    }

    // A failed event is dead-lettered under the execution role too, so both roles may write
    // that queue; what the withholding one lacks is the invocation itself.
    let invoking_role = given_role(
        &router,
        INVOKING_ROLE,
        "scheduler.amazonaws.com",
        &[
            (INVOKE_FUNCTION, "*"),
            ("sqs:SendMessage", dead_letter_arn.as_str()),
        ],
    )
    .await;
    let withholding_role = given_role(
        &router,
        WITHHOLDING_ROLE,
        "scheduler.amazonaws.com",
        &[("sqs:SendMessage", dead_letter_arn.as_str())],
    )
    .await;

    let occurrence = whole_seconds_ahead(OCCURRENCE_IN);
    let mut prepared: Vec<Prepared<'_>> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let target = row.became.target();
        let role = match row.became.withheld() {
            true => &withholding_role,
            false => &invoking_role,
        };
        let name = format!("function-error-schedule-{index}");
        given_schedule(
            &router,
            row.case,
            &name,
            occurrence,
            &Pointing {
                arn: &target,
                role,
                queue: &dead_letter_arn,
                input: row.input,
            },
        )
        .await;
        prepared.push(Prepared {
            row,
            request: json!({
                "FunctionName": target,
                "InvocationType": ASYNCHRONOUS,
                "Payload": row.input,
            }),
            schedule_arn: schedule_arn(&name),
        });
    }

    let undelivered = ROWS
        .iter()
        .filter(|row| row.became.dead_letter_code().is_some())
        .count();
    let mut dead_letters: BTreeMap<String, Value> = BTreeMap::new();
    let mut records: BTreeMap<String, Value> = BTreeMap::new();
    let mut forwarded: BTreeMap<String, Value> = BTreeMap::new();
    let began = Instant::now();
    loop {
        gather(&router, &dead_letter_url, &mut dead_letters).await;
        gather(&router, &destination_url, &mut records).await;
        gather(&router, &sink_url, &mut forwarded).await;
        if dead_letters.len() >= undelivered && !records.is_empty() && !forwarded.is_empty() {
            break;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "expected {undelivered} failed event(s), one on-failure record and one forwarded \
             marker, got {:?}, {:?} and {:?}",
            dead_letters.values().collect::<Vec<&Value>>(),
            records.values().collect::<Vec<&Value>>(),
            forwarded.values().collect::<Vec<&Value>>()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    // Each queue holds those messages and no others: a delivery the schedule MADE is not
    // dead-lettered later either, and the run of each accepted invocation left ONE mark.
    let until = Instant::now() + QUIET_WINDOW;
    while Instant::now() < until {
        gather(&router, &dead_letter_url, &mut dead_letters).await;
        gather(&router, &destination_url, &mut records).await;
        gather(&router, &sink_url, &mut forwarded).await;
        assert_eq!(
            dead_letters.len(),
            undelivered,
            "expected exactly {undelivered} failed event(s) on the schedules' dead-letter \
             queue, got {:?}",
            dead_letters.values().collect::<Vec<&Value>>()
        );
        assert_eq!(
            records.len(),
            1,
            "expected exactly one on-failure record, got {:?}",
            records.values().collect::<Vec<&Value>>()
        );
        assert_eq!(
            forwarded.len(),
            1,
            "expected exactly one forwarded marker, got {:?}",
            forwarded.values().collect::<Vec<&Value>>()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    for held in &prepared {
        let case = held.row.case;
        let matching: Vec<&Value> = dead_letters
            .values()
            .filter(|message| body_document(message) == held.request)
            .collect();
        let Some(expected_code) = held.row.became.dead_letter_code() else {
            // The invocation the target ACCEPTED: the schedule delivered it, so its event
            // is on no dead-letter queue however the run then went.
            assert!(
                matching.is_empty(),
                "case {case:?}: expected the accepted invocation not to be dead-lettered, got \
                 {matching:?}"
            );
            continue;
        };
        assert_eq!(
            matching.len(),
            1,
            "case {case:?}: expected exactly one failed event carrying the request {}, got {:?}",
            held.request,
            dead_letters.values().collect::<Vec<&Value>>()
        );
        let message = matching[0];
        assert_eq!(
            (
                attribute(message, "ERROR_CODE"),
                attribute(message, "TARGET_ARN"),
                attribute(message, "SCHEDULE_ARN"),
                attribute(message, "RETRY_ATTEMPTS"),
            ),
            (
                expected_code.to_string(),
                held.row.became.target(),
                held.schedule_arn.clone(),
                // The schedule admitted no retry, so this is the invocation's first and only
                // attempt.
                "0".to_string(),
            ),
            "case {case:?}: expected the failed event to report the refusal, the target, the \
             schedule and no retry, got {message}"
        );
        assert!(
            !attribute(message, "ERROR_MESSAGE").is_empty(),
            "case {case:?}: expected the failed event to carry the refusal's own sentence, got \
             {message}"
        );
    }

    // The accepted invocation whose handler raised is Lambda's to report: its record is at
    // the destination the FUNCTION names, and it is the record of that event.
    let record = records
        .values()
        .next()
        .map(body_document)
        .unwrap_or(Value::Null);
    let raised: Value = serde_json::from_str(
        ROWS.iter()
            .find(|row| row.became == Became::AcceptedAndRaised)
            .map(|row| row.input)
            .unwrap_or_default(),
    )
    .unwrap_or(Value::Null);
    assert_eq!(
        (
            record.pointer("/requestPayload").cloned(),
            record
                .pointer("/responseContext/functionError")
                .and_then(Value::as_str),
            record
                .pointer("/requestContext/condition")
                .and_then(Value::as_str),
        ),
        (
            Some(raised),
            Some(UNHANDLED),
            Some(RETRIES_EXHAUSTED)
        ),
        "expected the on-failure record of the raised event, got {record}"
    );
    let reported = record
        .pointer("/responsePayload/errorMessage")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        reported.contains(RAISED_MARKER),
        "expected the handler's own message to name the event it was handed, got {record}"
    );
    assert!(
        record
            .pointer("/requestContext/functionArn")
            .and_then(Value::as_str)
            .is_some_and(|arn| arn.starts_with(&function_arn(RAISING_FUNCTION))),
        "expected the record to name the function that raised, got {record}"
    );

    // The accepted invocation whose handler returned left its own mark too: the marker its
    // run forwarded, which is what says the delivery was made rather than merely not
    // dead-lettered.
    let markers: Vec<&str> = forwarded
        .values()
        .map(|message| message["Body"].as_str().unwrap_or_default())
        .collect();
    assert_eq!(
        markers,
        vec![RETURNED_MARKER],
        "expected the returning handler's own marker and nothing else, got {markers:?}"
    );
}
