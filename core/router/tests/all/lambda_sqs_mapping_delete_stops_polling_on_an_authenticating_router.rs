// SPDX-License-Identifier: CC0-1.0

//! What DELETING an Amazon SQS event source mapping does, on the emulator as it SERVES
//! traffic: the whole router over one engine with request authentication ON, so every call
//! below — and every call the running handler makes for itself — is a signed call that is
//! verified, which is the configuration a client of the emulator drives.
//!
//! `DeleteEventSourceMapping` "deletes an event source mapping… When you delete an event
//! source mapping, it enters a `Deleting` state and might not be completely deleted for
//! several seconds."
//!
//! RULE: a mapping reads its event source only while this account HOLDS it, so from the
//! instant a delete is accepted no pass of that mapping polls again — a message sent to the
//! source afterwards is neither handed to the function nor taken out of the queue, and stays
//! there for an ordinary consumer — and the mapping is no longer one a further call may act
//! on: a second delete and an update of it answer the modelled `ResourceNotFoundException`
//! (HTTP 404), while a READ still reports the `Deleting` state the teardown is in until the
//! removal the clock owes falls due.
//!
//! Four crates have to agree for that to hold, which is why the suite lives here: Amazon SQS
//! holds both queues and decides who may receive from them, IAM answers for the execution
//! role the poller reads as and the handler sends as, CloudFormation archives the inline
//! handler into the deployment package Lambda runs, and Lambda holds the function, the
//! mapping and the poller. Every observation is one a client makes over the wire.
//!
//! The negative half means nothing without the positive one, so each row asserts both: first
//! that the mapping really was consuming (the message reaches the handler, which relays the
//! envelope it was handed to a second queue with the SDK its managed runtime supplies), and
//! then that nothing is consumed after the delete.
//!
//! The rows differ along what the rule generalises over: the `BatchSize` the mapping reads in
//! (one message per batch, and a ceiling above what is waiting) and HOW MANY messages are
//! sent once the mapping is gone (one, and several).
//!
//! Assembling the whole router costs stack (see `boot_serve.rs`), so it is built — and the
//! runtime driving the rows is created — on an explicitly sized thread.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use aws_credential_types::Credentials;
use aws_emulator_router::{build_router, InMemoryEngine};
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{element, form_encode};

/// Keep this at or above `boot_serve.rs`'s stack: the same assembly runs here.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The Region every request below is signed for, and the one every resource lives in.
const REGION: &str = "us-west-2";

/// The runtime the relay is created with, and the module and attribute CloudFormation's
/// archive of the inline source exposes its handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "index.lambda_handler";

/// The `eventSource` every record of a queue-sourced event carries.
const EVENT_SOURCE: &str = "aws:sqs";

/// The `State` a mapping reports while it is polling its event source, and the one a delete
/// moves it into — both from `EventSourceMappingConfiguration$State`.
const STATE_ENABLED: &str = "Enabled";
/// See [`STATE_ENABLED`].
const STATE_DELETING: &str = "Deleting";

/// The modelled error a mapping this account no longer holds for acting on answers with.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";

/// How long one invocation of the relay is given, as the function's own `Timeout`: well past
/// what a run needs, because what is under test is the mapping's lifecycle and not how fast
/// an interpreter gets through a handler.
const FUNCTION_TIMEOUT: i64 = 30;

/// How long the source queue hides a message it hands over, in seconds, and the mapped
/// function's own `Timeout`: Lambda accepts a queue mapping only while the queue's
/// `VisibilityTimeout` is at least the function's `Timeout`. The span is also what makes the
/// negative half sharp: a message a pass had taken would stay hidden for this long, so a
/// source that still reports its messages VISIBLE is one nothing polled.
const VISIBILITY_TIMEOUT: i64 = 30;

/// The environment variable the relay reads the sink queue's url from.
const SINK_VARIABLE: &str = "SINK_QUEUE_URL";

/// How long an observation of work the CLOCK owes — the function going `Active`, the mapping
/// settling, a pass handing a batch to a running handler and the attempt it makes of it, the
/// removal a delete owes — waits before the row is a failure. GENEROUS, and above the span a
/// batch an attempt did not get through stays hidden for ([`VISIBILITY_TIMEOUT`]), so a loaded
/// host that cost one attempt its run delays the row instead of failing it: a row fails only
/// when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(150);

/// How long between two looks while waiting for one of those.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// How many times a row LOOKS at a source nothing may poll any more, and how long between
/// those looks: several of the poller's own intervals altogether, so a mapping that was going
/// to read the queue has had many passes in which to do it.
const QUIET_LOOKS: usize = 20;
/// See [`QUIET_LOOKS`].
const QUIET_LOOK_AGAIN: Duration = Duration::from_millis(150);

/// The handler every row runs: relay the whole envelope it was handed to the queue its
/// environment names, with the SDK its managed runtime supplies. The client is built at
/// module scope, which is where a managed runtime's handler builds it.
const RELAY_SOURCE: &str = r#"
import json
import os

import boto3

QUEUES = boto3.client("sqs")
SINK_URL = os.environ["SINK_QUEUE_URL"]


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    QUEUES.send_message(QueueUrl=SINK_URL, MessageBody=json.dumps(event))
    return {"relayed": [record["messageId"] for record in records]}
"#;

/// What one call answered.
struct Answer {
    status: u16,
    body: String,
}

impl Answer {
    /// The answer as a document, or `Null` when it is not one.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    /// One top-level member of the answered document, as text.
    fn string(&self, member: &str) -> Option<String> {
        self.json()
            .get(member)
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The wire error code this answer carries, however the protocol spells the member.
    fn error_code(&self) -> String {
        let parsed = self.json();
        for member in ["__type", "code", "Code"] {
            if let Some(code) = parsed.get(member).and_then(Value::as_str) {
                return code.to_string();
            }
        }
        String::new()
    }
}

/// The emulator this suite drives, and the credential every call it makes is signed with:
/// the pair this process publishes once request authentication is on.
struct Emulator {
    router: MultiServiceRouter,
    key_id: String,
    secret: String,
}

impl Emulator {
    /// The whole emulator over one engine, serving as it serves traffic.
    fn start() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = build_router(engine);
        router.set_request_authentication(true);
        let published = ::emulator_server::SigV4Authenticator::minted_credentials()
            .expect("turning authentication on publishes this process's keys");
        Self {
            router,
            key_id: published.access_key_id.clone(),
            secret: published.secret_access_key.clone(),
        }
    }

    /// Sign `request` for `signing_name` in [`REGION`], in the `Authorization` header an SDK
    /// request carries.
    fn signed(
        &self,
        mut request: http::Request<Vec<u8>>,
        signing_name: &str,
    ) -> http::Request<Vec<u8>> {
        let identity = Credentials::new(&self.key_id, &self.secret, None, None, "test").into();
        let params = SigningParams::builder()
            .identity(&identity)
            .region(REGION)
            .name(signing_name)
            .time(SystemTime::now())
            .settings(SigningSettings::default())
            .build()
            .expect("the signing parameters are complete")
            .into();
        let (method, uri) = (request.method().to_string(), request.uri().to_string());
        let headers: Vec<(String, String)> = request
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().expect("a text header").to_string(),
                )
            })
            .collect();
        let signable = SignableRequest::new(
            &method,
            &uri,
            headers
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str())),
            SignableBody::Bytes(request.body()),
        )
        .expect("the request is signable");
        let (instructions, _) = sign(signable, &params)
            .expect("the request signs")
            .into_parts();
        instructions.apply_to_request_http1x(&mut request);
        request
    }

    /// Dispatch one signed request as it arrives on the wire.
    async fn send(
        &self,
        method: &str,
        host: &str,
        signing: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> Answer {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(format!("https://{host}{path}"))
            .header("host", host);
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        let built = builder
            .body(body.as_bytes().to_vec())
            .expect("a well-formed request");
        let (parts, payload) = self.signed(built, signing).into_parts();
        let response = self
            .router
            .dispatch(http::Request::from_parts(
                parts,
                bytes::Bytes::from(payload),
            ))
            .await;
        Answer {
            status: response.status().as_u16(),
            body: String::from_utf8_lossy(response.body()).into_owned(),
        }
    }

    /// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`).
    async fn sqs(&self, operation: &str, body: &Value) -> Answer {
        self.send(
            "POST",
            &format!("sqs.{REGION}.amazonaws.com"),
            "sqs",
            "/",
            &[
                ("content-type", "application/x-amz-json-1.0"),
                ("x-amz-target", &format!("AmazonSQS.{operation}")),
            ],
            &body.to_string(),
        )
        .await
    }

    /// One Amazon SQS call that must answer 200.
    async fn sqs_ok(&self, case: &str, operation: &str, body: &Value) -> Value {
        let answered = self.sqs(operation, body).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected Amazon SQS {operation} to answer 200, got {} {}",
            answered.status, answered.body
        );
        answered.json()
    }

    /// One AWS Lambda call over its `restJson1` wire.
    async fn lambda(&self, method: &str, path: &str, body: Option<&Value>) -> Answer {
        let payload = body.map(Value::to_string).unwrap_or_default();
        self.send(
            method,
            &format!("lambda.{REGION}.amazonaws.com"),
            "lambda",
            path,
            &[("content-type", "application/json")],
            &payload,
        )
        .await
    }

    /// One awsQuery call of `signing` at `host`.
    async fn query(
        &self,
        signing: &str,
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
        self.send(
            "POST",
            host,
            signing,
            "/",
            &[("content-type", "application/x-www-form-urlencoded")],
            &body,
        )
        .await
    }

    /// One IAM call that must answer 200.
    async fn iam(&self, case: &str, action: &str, members: &[(&str, &str)]) -> String {
        let answered = self
            .query("iam", "iam.amazonaws.com", "2010-05-08", action, members)
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected IAM {action} to answer 200, got {} {}",
            answered.status, answered.body
        );
        answered.body
    }

    /// One CloudFormation call that must answer 200.
    async fn cloudformation(&self, case: &str, action: &str, members: &[(&str, &str)]) -> String {
        let answered = self
            .query(
                "cloudformation",
                &format!("cloudformation.{REGION}.amazonaws.com"),
                "2010-05-15",
                action,
                members,
            )
            .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected CloudFormation {action} to answer 200, got {} {}",
            answered.status, answered.body
        );
        answered.body
    }

    /// A queue of `name` carrying `attributes`; answers its url and the ARN Amazon SQS itself
    /// reports for it.
    async fn given_queue(&self, case: &str, name: &str, attributes: &Value) -> (String, String) {
        let created = self
            .sqs_ok(
                case,
                "CreateQueue",
                &json!({ "QueueName": name, "Attributes": attributes }),
            )
            .await;
        let url = created["QueueUrl"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: CreateQueue {name:?} reports no QueueUrl"))
            .to_string();
        let read = self
            .sqs_ok(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
            )
            .await;
        let arn = read["Attributes"]["QueueArn"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the queue {name:?} reports no QueueArn"))
            .to_string();
        (url, arn)
    }

    /// What a queue holds right now, as `(visible, in flight, delayed)`.
    async fn counts(&self, case: &str, url: &str) -> (u64, u64, u64) {
        let read = self
            .sqs_ok(
                case,
                "GetQueueAttributes",
                &json!({ "QueueUrl": url, "AttributeNames": ["All"] }),
            )
            .await;
        let count = |name: &str| -> u64 {
            read["Attributes"][name]
                .as_str()
                .and_then(|text| text.parse().ok())
                .unwrap_or_default()
        };
        (
            count("ApproximateNumberOfMessages"),
            count("ApproximateNumberOfMessagesNotVisible"),
            count("ApproximateNumberOfMessagesDelayed"),
        )
    }

    /// Take everything the queue `url` addresses hands over right now, DELETING each message
    /// so a later look finds only what arrived after it: the body of each.
    async fn take_everything(&self, case: &str, url: &str) -> Vec<String> {
        let answered = self
            .sqs_ok(
                case,
                "ReceiveMessage",
                &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
            )
            .await;
        let handed = answered["Messages"].as_array().cloned().unwrap_or_default();
        let mut bodies = Vec::with_capacity(handed.len());
        for message in &handed {
            self.sqs_ok(
                case,
                "DeleteMessage",
                &json!({
                    "QueueUrl": url,
                    "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
                }),
            )
            .await;
            bodies.push(
                message["Body"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case:?}: a message with no Body: {message}"))
                    .to_string(),
            );
        }
        bodies
    }

    /// `GetEventSourceMapping` on the mapping `uuid` names.
    async fn get_mapping(&self, uuid: &str) -> Answer {
        self.lambda("GET", &mapping_path(uuid), None).await
    }
}

/// Where a mapping's own resource is bound.
fn mapping_path(uuid: &str) -> String {
    format!("/2015-03-31/event-source-mappings/{uuid}")
}

/// One row: what the mapping reads in, and how many messages are sent once it is gone.
struct Row {
    case: &'static str,
    /// The stem every resource of the row is named from.
    slug: &'static str,
    batch_size: i64,
    /// How many messages the row sends after the delete, each of which must stay put.
    after: usize,
}

/// The rows: one message per batch against a ceiling above what is waiting, and one message
/// sent after the delete against several.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "one message per batch, one message sent after the delete",
            slug: "esm-stop-one",
            batch_size: 1,
            after: 1,
        },
        Row {
            case: "a batch ceiling above what is waiting, three messages sent after the delete",
            slug: "esm-stop-many",
            batch_size: 10,
            after: 3,
        },
    ]
}

/// The execution role of the row's relay: trusted by Lambda, granting the reads the poller
/// makes on the source and the one send the handler makes to the sink — the least privilege
/// such a pair needs. Answers the ARN IAM reports for it.
async fn given_role(
    emulator: &Emulator,
    case: &str,
    name: &str,
    source_arn: &str,
    sink_arn: &str,
) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    let created = emulator
        .iam(
            case,
            "CreateRole",
            &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
        )
        .await;
    let arn = element(&created, "Arn");
    // The actions AWS's own managed execution-role policy for a queue event source
    // (`AWSLambdaSQSQueueExecutionRole`) grants, on this row's source alone, beside the one
    // send the handler makes.
    let permissions = json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": ["sqs:ReceiveMessage", "sqs:DeleteMessage", "sqs:GetQueueAttributes"],
                "Resource": source_arn,
            },
            { "Effect": "Allow", "Action": "sqs:SendMessage", "Resource": sink_arn },
        ],
    })
    .to_string();
    emulator
        .iam(
            case,
            "PutRolePolicy",
            &[
                ("RoleName", name),
                ("PolicyName", "consume-and-relay"),
                ("PolicyDocument", &permissions),
            ],
        )
        .await;
    arn
}

/// Deploy the row's relay — a stack holding the one function, whose inline code
/// CloudFormation archives into the deployment package Lambda runs — and wait until Lambda
/// reports it `Active`.
async fn given_relay(
    emulator: &Emulator,
    case: &str,
    stack: &str,
    function: &str,
    role_arn: &str,
    sink_url: &str,
) {
    let template = json!({
        "AWSTemplateFormatVersion": "2010-09-09",
        "Resources": {
            "Relay": {
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function,
                    "Runtime": RUNTIME,
                    "Handler": HANDLER,
                    "Role": role_arn,
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": { "Variables": { SINK_VARIABLE: sink_url } },
                    "Code": { "ZipFile": RELAY_SOURCE },
                },
            },
        },
    })
    .to_string();
    emulator
        .cloudformation(
            case,
            "CreateStack",
            &[("StackName", stack), ("TemplateBody", &template)],
        )
        .await;
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = emulator
            .lambda(
                "GET",
                &format!("/2015-03-31/functions/{function}/configuration"),
                None,
            )
            .await;
        if read.status == 200 && read.string("State").as_deref() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {function} Active, last read {} {}",
            read.status,
            read.body
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create the mapping `request` states, wait until it reports [`STATE_ENABLED`] — the state in
/// which it polls — and answer its UUID.
async fn given_enabled_mapping(emulator: &Emulator, case: &str, request: &Value) -> String {
    let created = emulator
        .lambda("POST", "/2015-03-31/event-source-mappings", Some(request))
        .await;
    assert_eq!(
        created.status, 202,
        "case {case:?}: expected CreateEventSourceMapping to answer 202, got {} {}",
        created.status, created.body
    );
    let uuid = created
        .string("UUID")
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.body));
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = emulator.get_mapping(&uuid).await;
        if read.string("State").as_deref() == Some(STATE_ENABLED) {
            return uuid;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping never reported {STATE_ENABLED}, last read {} {}",
            read.status,
            read.body
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait until the sink queue holds a relayed envelope, and answer it as the document the
/// handler sent.
///
/// The wait is for the ENVELOPE, not for a span: a host slow to run a handler delays the loop
/// instead of weakening what it asserts.
async fn relayed_envelope(emulator: &Emulator, case: &str, sink_url: &str) -> Value {
    let deadline = Instant::now() + BUDGET;
    loop {
        if let Some(body) = emulator
            .take_everything(case, sink_url)
            .await
            .into_iter()
            .next()
        {
            return serde_json::from_str(&body).unwrap_or_else(|error| {
                panic!("case {case:?}: the relayed envelope is not a document: {body} ({error})")
            });
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping must hand its function the message it polled, and the \
             handler relayed nothing to the sink queue"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Assert `answered` is the modelled not-found of a mapping this account no longer holds for
/// acting on.
fn assert_not_found(case: &str, what: &str, answered: &Answer) {
    assert_eq!(
        (
            answered.status,
            answered.error_code().contains(RESOURCE_NOT_FOUND)
        ),
        (404, true),
        "case {case:?}: {what} on a mapping whose delete was accepted must be 404 \
         {RESOURCE_NOT_FOUND}, got {} {}",
        answered.status,
        answered.body
    );
}

/// RULE: a mapping polls only while this account holds it — once a `DeleteEventSourceMapping`
/// is accepted, no pass reads the event source again, so a message sent afterwards is neither
/// handed to the function nor taken out of the queue — and the mapping is no longer one a
/// further call may act on, while a read reports the teardown until the removal is due.
#[test]
fn a_deleted_mapping_stops_polling_on_an_authenticating_router() {
    std::thread::Builder::new()
        .name("esm-delete-stops-polling".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let emulator = Emulator::start();
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .worker_threads(4)
                .build()
                .expect("a multi-thread runtime");

            runtime.block_on(async {
                for row in rows() {
                    let case = row.case;
                    let (sink_url, sink_arn) = emulator
                        .given_queue(case, &format!("{}-sink", row.slug), &json!({}))
                        .await;
                    let (source_url, source_arn) = emulator
                        .given_queue(
                            case,
                            &format!("{}-source", row.slug),
                            &json!({ "VisibilityTimeout": VISIBILITY_TIMEOUT.to_string() }),
                        )
                        .await;
                    let role_arn = given_role(
                        &emulator,
                        case,
                        &format!("{}-role", row.slug),
                        &source_arn,
                        &sink_arn,
                    )
                    .await;
                    let function = format!("{}-relay", row.slug);
                    given_relay(
                        &emulator,
                        case,
                        &format!("{}-stack", row.slug),
                        &function,
                        &role_arn,
                        &sink_url,
                    )
                    .await;
                    let uuid = given_enabled_mapping(
                        &emulator,
                        case,
                        &json!({
                            "FunctionName": function,
                            "EventSourceArn": source_arn,
                            "BatchSize": row.batch_size,
                            "Enabled": true,
                        }),
                    )
                    .await;

                    // The positive control: while the mapping is held it consumes, so the
                    // message reaches the handler and the envelope it was handed is relayed.
                    let before = format!("before-{}", row.slug);
                    emulator
                        .sqs_ok(
                            case,
                            "SendMessage",
                            &json!({ "QueueUrl": source_url, "MessageBody": before }),
                        )
                        .await;
                    let envelope = relayed_envelope(&emulator, case, &sink_url).await;
                    let records = envelope["Records"].as_array().cloned().unwrap_or_default();
                    assert_eq!(
                        records.len(),
                        1,
                        "case {case:?}: the mapping must hand over one record per message it \
                         took, got {envelope}"
                    );
                    assert_eq!(
                        (
                            records[0]["body"].as_str(),
                            records[0]["eventSource"].as_str(),
                            records[0]["eventSourceARN"].as_str(),
                        ),
                        (
                            Some(before.as_str()),
                            Some(EVENT_SOURCE),
                            Some(source_arn.as_str()),
                        ),
                        "case {case:?}: the record must carry the message the queue held, got \
                         {envelope}"
                    );
                    // A message the invocation PROCESSED leaves the queue, so the positive
                    // control ends with the source drained — waited for, because an attempt a
                    // loaded host cost its run leaves the message to be handed over again when
                    // the hold on it lapses, and the mapping is still held, so it polls again.
                    let deadline = Instant::now() + BUDGET;
                    loop {
                        let counts = emulator.counts(case, &source_url).await;
                        if counts == (0, 0, 0) {
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "case {case:?}: a message the invocation processed is deleted, so the \
                             source must be drained, and it reports {counts:?} as (visible, in \
                             flight, delayed)"
                        );
                        tokio::time::sleep(LOOK_AGAIN).await;
                    }
                    // Anything else the mapping relayed while it was held is taken now, so what
                    // the sink reports below is about the invocations made AFTER the delete.
                    emulator.take_everything(case, &sink_url).await;

                    // The delete: the mapping enters `Deleting` and answers the record in that
                    // state, which is what a client watching the teardown reads back.
                    let deleted = emulator.lambda("DELETE", &mapping_path(&uuid), None).await;
                    assert_eq!(
                        (deleted.status, deleted.string("State").as_deref()),
                        (202, Some(STATE_DELETING)),
                        "case {case:?}: the delete must answer the mapping as being deleted, got \
                         {} {}",
                        deleted.status,
                        deleted.body
                    );
                    let being_deleted = emulator.get_mapping(&uuid).await;
                    assert_eq!(
                        (
                            being_deleted.status,
                            being_deleted.string("State").as_deref()
                        ),
                        (200, Some(STATE_DELETING)),
                        "case {case:?}: a read inside the teardown must report it, got {} {}",
                        being_deleted.status,
                        being_deleted.body
                    );
                    // And from that instant it is not a mapping a further call may act on.
                    let again = emulator.lambda("DELETE", &mapping_path(&uuid), None).await;
                    assert_not_found(case, "a second DeleteEventSourceMapping", &again);
                    let updated = emulator
                        .lambda("PUT", &mapping_path(&uuid), Some(&json!({ "BatchSize": 2 })))
                        .await;
                    assert_not_found(case, "UpdateEventSourceMapping", &updated);

                    // The negative half: every message sent now stays where it was sent. It is
                    // asserted over many LOOKS rather than after one, so a mapping that was
                    // still polling — on any pass of the poller's own cadence — fails here.
                    let after: Vec<String> = (0..row.after)
                        .map(|index| format!("after-{}-{index}", row.slug))
                        .collect();
                    for body in &after {
                        emulator
                            .sqs_ok(
                                case,
                                "SendMessage",
                                &json!({ "QueueUrl": source_url, "MessageBody": body }),
                            )
                            .await;
                    }
                    let waiting = u64::try_from(after.len()).unwrap_or(u64::MAX);
                    for look in 0..QUIET_LOOKS {
                        let counts = emulator.counts(case, &source_url).await;
                        assert_eq!(
                            counts,
                            (waiting, 0, 0),
                            "case {case:?}: nothing polls a mapping whose delete was accepted, \
                             so look {look} must find every message visible on the source, got \
                             {counts:?} as (visible, in flight, delayed)"
                        );
                        let sink = emulator.counts(case, &sink_url).await;
                        assert_eq!(
                            sink,
                            (0, 0, 0),
                            "case {case:?}: the function must not be invoked once the mapping \
                             is gone, and look {look} found the sink queue reporting {sink:?} as \
                             (visible, in flight, delayed)"
                        );
                        tokio::time::sleep(QUIET_LOOK_AGAIN).await;
                    }

                    // The removal the clock owed falls due, and then the mapping is held no
                    // more — by a read as by every call that already answered not-found.
                    let deadline = Instant::now() + BUDGET;
                    loop {
                        let read = emulator.get_mapping(&uuid).await;
                        if read.status == 404 {
                            assert_not_found(case, "GetEventSourceMapping", &read);
                            break;
                        }
                        assert!(
                            Instant::now() < deadline,
                            "case {case:?}: the mapping {uuid} was never removed, last read {} {}",
                            read.status,
                            read.body
                        );
                        tokio::time::sleep(LOOK_AGAIN).await;
                    }

                    // And what stayed is receivable by an ordinary consumer, body for body.
                    let mut held = emulator.take_everything(case, &source_url).await;
                    held.sort();
                    assert_eq!(
                        held, after,
                        "case {case:?}: every message sent after the delete must still be \
                         receivable, got {held:?}"
                    );
                }
            });
        })
        .expect("spawn the suite's thread")
        .join()
        .expect("the suite runs");
}
