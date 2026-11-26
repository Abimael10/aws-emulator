// SPDX-License-Identifier: CC0-1.0

//! WHAT an EventBridge Scheduler dead-letter message says about an occurrence that could not
//! be handed over, read where a client reads it: on the Amazon SQS queue
//! `Target.DeadLetterConfig.Arn` names.
//!
//! RULE: **`Target$DeadLetterConfig` names the queue EventBridge Scheduler "delivers failed
//! events that could not be successfully delivered to a target" to, and what it delivers is
//! the CALL the attempt made — a templated target names one API operation of the target's own
//! service (`SqsParameters` is "the templated target type for the Amazon SQS `SendMessage` API
//! operation", and the function, topic and state-machine parameters are the same for their
//! services' operations), so the message BODY is that operation's own request document: the
//! members that operation publishes, with `Target$Input` ("the text, or well-formed JSON,
//! passed to the target") verbatim in the member that request carries the payload under, and
//! the members the target's KIND decides rather than the schedule beside them. The schedule's
//! `Input` is a MEMBER of that document, never the document itself. The failure that ended the
//! invocation is reported beside it, in `ERROR_CODE` and `ERROR_MESSAGE`, as the refusing
//! service's own code and sentence for that very call — so an invocation of a function this
//! account does not hold is reported with the sentence Lambda answers an `Event` invocation of
//! it with, whose variable part is the UNQUALIFIED function ARN the request addressed, because
//! an asynchronous invocation is handed on rather than run and so resolves no version to
//! name.**
//!
//! Only a cross-service suite can pin it: the schedule, the occurrence and the dead-letter
//! message are EventBridge Scheduler's, the refusal belongs to the target's own service (AWS
//! Lambda, Amazon SNS, AWS Step Functions), the execution role is IAM's, and the dead-letter
//! queue is Amazon SQS's.
//!
//! Nothing in the expected code or sentence is written out here: every row REPLAYS the document
//! the queue handed it as a real request against the very API its members name, and the refusal
//! that replay earns is what the row's `ERROR_CODE` and `ERROR_MESSAGE` are judged against —
//! which is also what makes the body a document a client can replay at all. The one sentence
//! stated literally is the function refusal's fixed text, where the whole point is which ARN
//! spelling fills it.
//!
//! The rows differ along what the rule generalises over: the KIND of target naming the call
//! (a function, a topic, a state machine — three operations, three member sets, three payload
//! member names), WHY the target is not there (never held by this account, or deleted before
//! the occurrence), and the FORM of `Target$Input` (a flat object, a nested object, a
//! top-level array, text that is not JSON), because the body is the request the attempt made
//! whatever that input was.
//!
//! Nothing here waits for a fixed span: every schedule names an instant seconds ahead and each
//! wait watches for the CONDITION — the messages a queue holds — until a generous deadline.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// The Region every request below is signed for, and the one every ARN carries.
const REGION: &str = "eu-west-2";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// How long each wait below watches for the condition it expects, and how often it looks.
/// Every schedule names an instant [`OCCURRENCE_IN`] ahead, so the deadline is room for the
/// clock's tick and the attempts on a loaded host rather than a latency expectation of its
/// own: it is reached only where nothing happened at all.
const PATIENCE: Duration = Duration::from_secs(180);
/// See [`PATIENCE`].
const LOOK_INTERVAL: Duration = Duration::from_millis(100);

/// How long the dead-letter queue keeps being watched after every expected message has
/// arrived, so a message that should never come has had its chance to. Nothing is asserted
/// about how long anything took: the window is only how long the surplus is watched for.
const QUIET_WINDOW: Duration = Duration::from_secs(5);

/// How far ahead of its creation every schedule below places its one occurrence: far enough
/// that the whole table is written before the first one falls due, and close enough for one
/// poll loop to watch it.
const OCCURRENCE_IN: Duration = Duration::from_secs(5);

/// Where Lambda's function operations are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The queue every schedule below names for its failed events.
const DEAD_LETTER_QUEUE: &str = "dlq-envelope-queue";

/// The role every schedule below is invoked under.
const EXECUTION_ROLE: &str = "dlq-envelope-execution";

/// The function this account never holds, and the one created and deleted again before its
/// occurrence: two readings of the same absence.
const ABSENT_FUNCTION: &str = "dlq-envelope-absent";
/// See [`ABSENT_FUNCTION`].
const DELETED_FUNCTION: &str = "dlq-envelope-deleted";

/// The topic and the state machine this account never holds.
const ABSENT_TOPIC: &str = "dlq-envelope-absent-topic";
/// See [`ABSENT_TOPIC`].
const ABSENT_STATE_MACHINE: &str = "dlq-envelope-absent-machine";

/// A deployment package as `Code.ZipFile` carries it: an empty archive. It belongs to the
/// function that is DELETED before any occurrence, so nothing ever runs it; what it has to be
/// is a package `CreateFunction` accepts.
const EMPTY_PACKAGE: &str = "UEsDBAoAAAAAAAAAAAA=";

/// A well-formed execution role for the function that is created and deleted again.
const FUNCTION_ROLE_ARN: &str = "arn:aws:iam::000000000000:role/dlq-envelope-function-exec";

/// The `InvocationType` a schedule's function target is invoked with ("Event — invoke the
/// function asynchronously", `Invoke$InvocationType`), which the request a failed event
/// carries therefore names.
const ASYNCHRONOUS: &str = "Event";

/// The fixed text of the refusal Lambda answers an invocation of a function it does not hold
/// with: `Function not found: ` followed by the ARN the request addressed. The only part
/// written out here, because the whole question is which ARN spelling fills it.
const FUNCTION_NOT_FOUND_PREFIX: &str = "Function not found: ";

/// The qualifier label a version-resolving reference carries, which an ARN the caller named no
/// qualifier for does not.
const LATEST: &str = "$LATEST";

/// How many retries each schedule below admits: none, so every failure is its invocation's
/// first and only attempt, and `RETRY_ATTEMPTS` is this many.
const NO_RETRY: u32 = 0;

/// The services this flow spans, on ONE engine exactly as `core/router/src/lib.rs` mounts
/// them. Request authentication is off, as in every other suite here; the calls this emulator
/// makes to itself sign themselves either way.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_scheduler_awschronosservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_sfn_awsstepfunctions::EmulatedService::new(Arc::clone(&engine)),
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

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which
/// is what routes the request. The signature is never checked: this router runs with request
/// authentication off.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// What one dispatched request answered.
struct Answer {
    status: u16,
    error_type: String,
    body: Bytes,
}

impl Answer {
    /// The answer's body as the JSON document it carries, or `Null` when it is not one.
    fn document(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The answer's body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// WHERE one request goes and how it is written: the one place this suite states a service's
/// wire, so every call below differs only in its own members.
struct Wire<'a> {
    method: &'a str,
    uri: &'a str,
    /// The signing name the credential scope names, which is what routes the request.
    signing_name: &'a str,
    host: &'a str,
    content_type: &'a str,
    /// Whatever further headers the protocol needs (an `x-amz-target`, an invocation type).
    headers: &'a [(&'a str, &'a str)],
}

/// Dispatch one request through the assembled router.
async fn dispatch(router: &MultiServiceRouter, wire: &Wire<'_>, body: Bytes) -> Answer {
    let mut builder = http::Request::builder()
        .method(wire.method)
        .uri(wire.uri)
        .header("host", wire.host)
        .header("content-type", wire.content_type)
        .header("authorization", authorization(wire.signing_name));
    for (name, value) in wire.headers {
        builder = builder.header(*name, *value);
    }
    let request = builder.body(body).expect("a well-formed request");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let error_type = parts
        .headers
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Answer {
        status: parts.status.as_u16(),
        error_type,
        body,
    }
}

/// One awsQuery call (IAM, Amazon SNS) over its own wire.
async fn query(
    router: &MultiServiceRouter,
    signing_name: &str,
    host: &str,
    version: &str,
    action: &str,
    members: &[(&str, &str)],
) -> Answer {
    let mut body = format!("Action={action}&Version={version}");
    for (key, value) in members {
        body.push('&');
        body.push_str(&form_encode(key));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    dispatch(
        router,
        &Wire {
            method: "POST",
            uri: "/",
            signing_name,
            host,
            content_type: "application/x-www-form-urlencoded",
            headers: &[],
        },
        Bytes::from(body),
    )
    .await
}

/// One IAM call. IAM is global; the request is signed for this suite's Region only because
/// that is what routes it.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> Answer {
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

/// One Amazon SNS call (API version 2010-03-31) over its awsQuery wire.
async fn sns(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) -> Answer {
    query(
        router,
        "sns",
        &format!("sns.{REGION}.amazonaws.com"),
        "2010-03-31",
        action,
        members,
    )
    .await
}

/// One Amazon SQS call over its `awsJson1_0` wire.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let answer = dispatch(
        router,
        &Wire {
            method: "POST",
            uri: "/",
            signing_name: "sqs",
            host: &format!("sqs.{REGION}.amazonaws.com"),
            content_type: "application/x-amz-json-1.0",
            headers: &[("x-amz-target", &format!("AmazonSQS.{operation}"))],
        },
        Bytes::from(body.to_string()),
    )
    .await;
    (answer.status, answer.document())
}

/// One AWS Step Functions call over its `awsJson1_0` wire.
async fn sfn(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answer {
    dispatch(
        router,
        &Wire {
            method: "POST",
            uri: "/",
            signing_name: "states",
            host: &format!("states.{REGION}.amazonaws.com"),
            content_type: "application/x-amz-json-1.0",
            headers: &[("x-amz-target", &format!("AWSStepFunctions.{operation}"))],
        },
        Bytes::from(body.to_string()),
    )
    .await
}

/// One AWS Lambda call over its `restJson1` wire, with the headers `headers` names.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    dispatch(
        router,
        &Wire {
            method,
            uri,
            signing_name: "lambda",
            host: &format!("lambda.{REGION}.amazonaws.com"),
            content_type: "application/json",
            headers,
        },
        body,
    )
    .await
}

/// One EventBridge Scheduler call over its `restJson1` wire.
async fn scheduler(router: &MultiServiceRouter, method: &str, path: &str, body: Value) -> Answer {
    dispatch(
        router,
        &Wire {
            method,
            uri: path,
            signing_name: "scheduler",
            host: &format!("scheduler.{REGION}.amazonaws.com"),
            content_type: "application/json",
            headers: &[],
        },
        Bytes::from(body.to_string()),
    )
    .await
}

/// This account's ARN for the function `name` in the Region this suite drives.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:{ACCOUNT}:function:{name}")
}

/// This account's ARN for the topic `name`.
fn topic_arn(name: &str) -> String {
    format!("arn:aws:sns:{REGION}:{ACCOUNT}:{name}")
}

/// This account's ARN for the state machine `name`.
fn state_machine_arn(name: &str) -> String {
    format!("arn:aws:states:{REGION}:{ACCOUNT}:stateMachine:{name}")
}

/// The ARN the schedule `name` of the default group carries.
fn schedule_arn(name: &str) -> String {
    format!("arn:aws:scheduler:{REGION}:{ACCOUNT}:schedule/default/{name}")
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

/// A role EventBridge Scheduler may assume, granted each `(action, resource)` pair the
/// occurrences below need, and answer its ARN.
async fn given_execution_role(
    router: &MultiServiceRouter,
    name: &str,
    grants: &[(&str, &str)],
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "scheduler.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = iam(
        router,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "setup: CreateRole({name:?}) must succeed, got {} with {}",
        created.status,
        created.text()
    );
    let statements: Vec<Value> = grants
        .iter()
        .map(|(action, resource)| {
            json!({ "Effect": "Allow", "Action": action, "Resource": resource })
        })
        .collect();
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    let written = iam(
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
        written.status,
        200,
        "setup: PutRolePolicy({name:?}) must succeed, got {} with {}",
        written.status,
        written.text()
    );
    format!("arn:aws:iam::{ACCOUNT}:role/{name}")
}

/// Create the function that is DELETED again before any occurrence, and delete it, so the row
/// that names it names a function this account HELD and no longer does.
async fn given_deleted_function(router: &MultiServiceRouter) {
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &[],
        Bytes::from(
            json!({
                "FunctionName": DELETED_FUNCTION,
                "Role": FUNCTION_ROLE_ARN,
                "Runtime": "python3.12",
                "Handler": "index.handler",
                "Code": { "ZipFile": EMPTY_PACKAGE },
            })
            .to_string(),
        ),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "setup: CreateFunction({DELETED_FUNCTION:?}) must succeed, got {} with {}",
        created.status,
        created.text()
    );
    let deleted = lambda(
        router,
        "DELETE",
        &format!("{FUNCTIONS_URI}/{DELETED_FUNCTION}"),
        &[],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        deleted.status,
        204,
        "setup: DeleteFunction({DELETED_FUNCTION:?}) must succeed, got {} with {}",
        deleted.status,
        deleted.text()
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

/// A one-time schedule naming `target`, invoked under `role`, handing `input` over, reporting
/// a failed event to `queue`, and admitting no retry of its own.
/// What one schedule's target points at: the resource it names, the role it is invoked under,
/// the queue its failed events go to, and the input it hands over.
struct Pointing<'a> {
    target: &'a str,
    role: &'a str,
    queue: &'a str,
    input: &'a str,
}

async fn given_schedule(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    occurrence: SystemTime,
    pointing: &Pointing<'_>,
) {
    let created = scheduler(
        router,
        "POST",
        &format!("/schedules/{name}"),
        json!({
            "ScheduleExpression": format!("at({})", at_expression_body(occurrence)),
            "ScheduleExpressionTimezone": "UTC",
            "FlexibleTimeWindow": { "Mode": "OFF" },
            "State": "ENABLED",
            "Target": {
                "Arn": pointing.target,
                "RoleArn": pointing.role,
                "Input": pointing.input,
                "RetryPolicy": { "MaximumRetryAttempts": NO_RETRY },
                "DeadLetterConfig": { "Arn": pointing.queue },
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        200,
        "case {case:?}: CreateSchedule must succeed, got {} with {}",
        created.status,
        created.text()
    );
}

/// Add every message the queue `url` holds by this look to `seen`, keyed by the `MessageId`
/// Amazon SQS minted for it.
///
/// A received message becomes visible again once its visibility timeout lapses, so keying by
/// that id is what makes one message count once however many looks return it.
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

/// One error class as its wire name states it, stripped of the two decorations either place
/// the name is carried in may add: the namespace a shape id qualifies it by, and the URL an
/// `x-amzn-errortype` header may append after a colon.
fn class_of(named: &str) -> String {
    let after_namespace = named.rsplit('#').next().unwrap_or(named);
    after_namespace
        .split(':')
        .next()
        .unwrap_or(after_namespace)
        .trim()
        .to_string()
}

/// The `message` an answer carries, under either spelling a protocol admits.
fn json_message(document: &Value) -> String {
    ["message", "Message"]
        .iter()
        .find_map(|member| document.get(*member).and_then(Value::as_str))
        .unwrap_or_default()
        .to_string()
}

/// WHICH call a row's target names, which decides the member set its request document carries
/// and whose service the refusal belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Names {
    /// A function this account never held: the call is AWS Lambda's `Invoke`.
    AbsentFunction,
    /// A function this account HELD and deleted before the occurrence: the same call and the
    /// same absence, reached the other way.
    DeletedFunction,
    /// A topic this account never held: the call is Amazon SNS's `Publish`.
    AbsentTopic,
    /// A state machine this account never held: the call is Step Functions'
    /// `StartExecution`.
    AbsentStateMachine,
}

impl Names {
    /// The ARN the row's `Target.Arn` carries.
    fn target(self) -> String {
        match self {
            Self::AbsentFunction => function_arn(ABSENT_FUNCTION),
            Self::DeletedFunction => function_arn(DELETED_FUNCTION),
            Self::AbsentTopic => topic_arn(ABSENT_TOPIC),
            Self::AbsentStateMachine => state_machine_arn(ABSENT_STATE_MACHINE),
        }
    }

    /// The request the attempt makes of the target's own API, which is the document the
    /// dead-letter message's body must be: the members that operation publishes, with the
    /// schedule's `Input` in the one carrying the payload.
    fn attempted_call(self, input: &str) -> Value {
        let target = self.target();
        match self {
            // `Invoke` takes `FunctionName` and `Payload`, and the KIND of invocation beside
            // them — which the target's kind decides rather than the schedule, so it is part
            // of the request that failed.
            Self::AbsentFunction | Self::DeletedFunction => json!({
                "FunctionName": target,
                "InvocationType": ASYNCHRONOUS,
                "Payload": input,
            }),
            // `Publish` takes `TopicArn` and `Message`.
            Self::AbsentTopic => json!({ "TopicArn": target, "Message": input }),
            // `StartExecution` takes `StateMachineArn` and `Input`.
            Self::AbsentStateMachine => json!({ "StateMachineArn": target, "Input": input }),
        }
    }

    /// Whether the row's call is an invocation of a function, whose refusal sentence this
    /// suite also judges letter by letter.
    fn invokes_a_function(self) -> bool {
        matches!(self, Self::AbsentFunction | Self::DeletedFunction)
    }
}

/// What one replayed call was refused with.
#[derive(Debug)]
struct Refusal {
    status: u16,
    class: String,
    message: String,
}

/// REPLAY the document `call` as a real request against the API its members name, and answer
/// the refusal it earns.
///
/// This is what makes the body a request rather than a shape: a client reading its dead-letter
/// queue to replay a failed invocation submits exactly this document to exactly this
/// operation, and what it gets back is what the dead-letter message said it would.
async fn replay(router: &MultiServiceRouter, names: Names, call: &Value) -> Refusal {
    let member = |name: &str| call[name].as_str().unwrap_or_default().to_string();
    match names {
        Names::AbsentFunction | Names::DeletedFunction => {
            let answer = lambda(
                router,
                "POST",
                &format!("{FUNCTIONS_URI}/{}/invocations", member("FunctionName")),
                &[("x-amz-invocation-type", &member("InvocationType"))],
                Bytes::from(member("Payload")),
            )
            .await;
            Refusal {
                status: answer.status,
                class: class_of(&answer.error_type),
                message: json_message(&answer.document()),
            }
        }
        Names::AbsentTopic => {
            let answer = sns(
                router,
                "Publish",
                &[
                    ("TopicArn", &member("TopicArn")),
                    ("Message", &member("Message")),
                ],
            )
            .await;
            let body = answer.text();
            Refusal {
                status: answer.status,
                class: class_of(&element(&body, "Code")),
                message: element(&body, "Message"),
            }
        }
        Names::AbsentStateMachine => {
            let answer = sfn(
                router,
                "StartExecution",
                &json!({
                    "stateMachineArn": member("StateMachineArn"),
                    "input": member("Input"),
                }),
            )
            .await;
            let document = answer.document();
            let named = match answer.error_type.is_empty() {
                true => document
                    .get("__type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                false => answer.error_type.clone(),
            };
            Refusal {
                status: answer.status,
                class: class_of(&named),
                message: json_message(&document),
            }
        }
    }
}

/// One row: the call its schedule names, and the form of the input it hands over.
struct Row {
    case: &'static str,
    names: Names,
    /// `Target.Input`, in the form this row carries it.
    input: &'static str,
}

/// The rows differ along the target KIND naming the call, along WHY the target is not there,
/// and along the form of `Target.Input`.
const ROWS: &[Row] = &[
    Row {
        case: "a function this account never held, handed a flat object",
        names: Names::AbsentFunction,
        input: r#"{"marker":"dlq-envelope-flat"}"#,
    },
    Row {
        case: "a function this account never held, handed a top-level array",
        names: Names::AbsentFunction,
        input: r#"[1,"two",{"three":3}]"#,
    },
    Row {
        case: "a function this account never held, handed text that is not JSON",
        names: Names::AbsentFunction,
        input: "a line of plain text, not JSON",
    },
    Row {
        case: "a function deleted before its occurrence, handed a nested object",
        names: Names::DeletedFunction,
        input: r#"{"marker":"dlq-envelope-nested","detail":{"depth":2}}"#,
    },
    Row {
        case: "a topic this account never held, handed text that is not JSON",
        names: Names::AbsentTopic,
        input: "a published line of plain text",
    },
    Row {
        case: "a state machine this account never held, handed a flat object",
        names: Names::AbsentStateMachine,
        input: r#"{"marker":"dlq-envelope-machine"}"#,
    },
];

/// What one row's schedule asked for, kept so the assertions read the request the invocation
/// was given rather than rebuilding it.
struct Prepared<'a> {
    row: &'a Row,
    /// The request the attempt makes of the target's own API.
    call: Value,
    schedule_arn: String,
}

/// Every row: the occurrence's call cannot be handed over, so the schedule reports it on the
/// queue it names — and what that report carries is the request the attempt made, with the
/// refusing service's own code and sentence for that very request beside it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failed_events_body_is_the_request_the_attempt_made() {
    let router = build_router();
    let (dead_letter_url, dead_letter_arn) = given_queue(&router, DEAD_LETTER_QUEUE).await;
    given_deleted_function(&router).await;

    // The role withholds nothing: every row below fails because the target is not there, not
    // because the call was refused the caller — so the role grants each operation the rows
    // name, and the dead-letter write beside them.
    let role = given_execution_role(
        &router,
        EXECUTION_ROLE,
        &[
            ("lambda:InvokeFunction", "*"),
            ("sns:Publish", "*"),
            ("states:StartExecution", "*"),
            ("sqs:SendMessage", dead_letter_arn.as_str()),
        ],
    )
    .await;

    let occurrence = whole_seconds_ahead(OCCURRENCE_IN);
    let mut prepared: Vec<Prepared<'_>> = Vec::new();
    for (index, row) in ROWS.iter().enumerate() {
        let name = format!("dlq-envelope-schedule-{index}");
        given_schedule(
            &router,
            row.case,
            &name,
            occurrence,
            &Pointing {
                target: &row.names.target(),
                role: &role,
                queue: &dead_letter_arn,
                input: row.input,
            },
        )
        .await;
        prepared.push(Prepared {
            row,
            call: row.names.attempted_call(row.input),
            schedule_arn: schedule_arn(&name),
        });
    }

    let mut dead_letters: BTreeMap<String, Value> = BTreeMap::new();
    let began = Instant::now();
    loop {
        gather(&router, &dead_letter_url, &mut dead_letters).await;
        if dead_letters.len() >= ROWS.len() {
            break;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "expected {} failed event(s) on the dead-letter queue, got {:?}",
            ROWS.len(),
            dead_letters.values().collect::<Vec<&Value>>()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    // One failed event per undelivered occurrence and no others: a schedule that admits no
    // retry reports its one attempt once.
    let until = Instant::now() + QUIET_WINDOW;
    while Instant::now() < until {
        gather(&router, &dead_letter_url, &mut dead_letters).await;
        assert_eq!(
            dead_letters.len(),
            ROWS.len(),
            "expected exactly {} failed event(s) on the dead-letter queue, got {:?}",
            ROWS.len(),
            dead_letters.values().collect::<Vec<&Value>>()
        );
        tokio::time::sleep(LOOK_INTERVAL).await;
    }

    for held in &prepared {
        let case = held.row.case;
        let matching: Vec<&Value> = dead_letters
            .values()
            .filter(|message| body_document(message) == held.call)
            .collect();
        assert_eq!(
            matching.len(),
            1,
            "case {case:?}: expected exactly one failed event whose body is the request the \
             attempt made, {}, got {:?}",
            held.call,
            dead_letters
                .values()
                .map(body_document)
                .collect::<Vec<Value>>()
        );
        let message = matching[0];

        // The schedule's own `Input` is a MEMBER of that document, verbatim, and never the
        // document itself.
        let body = body_document(message);
        assert_ne!(
            body,
            serde_json::from_str::<Value>(held.row.input).unwrap_or(Value::Null),
            "case {case:?}: the failed event's body must be the attempted call, not the \
             schedule's own Input, got {body}"
        );

        // What the report says the call came to, judged against what that very call comes to
        // when it is REPLAYED: the refusing service's own class and sentence.
        let refusal = replay(&router, held.row.names, &held.call).await;
        assert!(
            (400..500).contains(&refusal.status),
            "case {case:?}: replaying the failed event's body must be refused as the attempt \
             was, got {refusal:?}"
        );
        assert_eq!(
            (
                attribute(message, "ERROR_CODE"),
                attribute(message, "ERROR_MESSAGE"),
                attribute(message, "TARGET_ARN"),
                attribute(message, "SCHEDULE_ARN"),
                attribute(message, "RETRY_ATTEMPTS"),
            ),
            (
                refusal.class.clone(),
                refusal.message.clone(),
                held.row.names.target(),
                held.schedule_arn.clone(),
                NO_RETRY.to_string(),
            ),
            "case {case:?}: expected the failed event to report the refusal {refusal:?}, the \
             target, the schedule and no retry, got {message}"
        );

        if !held.row.names.invokes_a_function() {
            continue;
        }
        // An invocation of a function this account does not hold: the fixed text followed by
        // the ARN the request ADDRESSED. The target named no qualifier, and an asynchronous
        // invocation resolves none on the caller's behalf, so the ARN carries none.
        let target = held.row.names.target();
        assert_eq!(
            attribute(message, "ERROR_MESSAGE"),
            format!("{FUNCTION_NOT_FOUND_PREFIX}{target}"),
            "case {case:?}: expected the failed event to name the unqualified function ARN, \
             got {}",
            attribute(message, "ERROR_MESSAGE")
        );
        assert!(
            !attribute(message, "ERROR_MESSAGE").ends_with(LATEST),
            "case {case:?}: the reported ARN must carry no qualifier the target never named, \
             got {}",
            attribute(message, "ERROR_MESSAGE")
        );
    }
}
