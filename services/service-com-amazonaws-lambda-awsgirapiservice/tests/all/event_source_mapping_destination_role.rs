// SPDX-License-Identifier: CC0-1.0

//! WHO writes the record of a batch an event source mapping gave up on, and what therefore
//! happens when that writer may not reach the destination — Lambda, Amazon DynamoDB, DynamoDB
//! Streams, Amazon Kinesis, Amazon SQS, Amazon SNS and IAM on ONE engine, with a real IAM role
//! behind the function.
//!
//! RULE: Lambda reads a mapping's event source and invokes its function ACTING AS that
//! function's execution role — "the Amazon Resource Name (ARN) of the function's execution
//! role" (`FunctionConfiguration$Role`) — and the record it writes to the mapping's
//! `DestinationConfig$OnFailure` ("the destination configuration for failed invocations") is
//! one more call made under that same identity. So the policies in force on that role decide
//! the destination, in both the places they decide the event SOURCE:
//!
//! 1. the call that CONFIGURES the pair refuses a destination whose write those policies
//!    withhold, with the modelled `InvalidParameterValueException` (HTTP 400) and no mapping
//!    stored, rather than wiring up a dead-letter path no record could ever reach;
//! 2. the POLLER that would write the record asks again when it writes, because the policies
//!    in force on a role are not frozen when the pair is wired: a grant withdrawn after the
//!    mapping was created is a destination that receives nothing from then on.
//!
//! The discard itself is not conditional on the write — the records are gone from the
//! function's reach either way — so the mapping reads on past the batch and keeps polling,
//! which is how the second test proves the absence rather than waiting a span out: the record
//! BEHIND the discarded one is processed, and that is the barrier the empty destination is
//! asserted against.
//!
//! The rows differ along what the rule generalises over: the KIND of destination the write is
//! judged for (an Amazon SQS queue, whose write is `sqs:SendMessage`, and an Amazon SNS topic,
//! whose write is `sns:Publish`), what the role's policies say about that write (granted, no
//! statement at all, a grant of the OTHER kind's write only, a grant together with an explicit
//! deny) and which shard source the records sit in — because none of those is what the rule
//! turns on.
//!
//! Each function's package RUNS, and whether it refuses a batch is decided inside the handler
//! from the record's own data, so the second test's discard is a consequence of the handler
//! having been invoked with the batch.
//!
//! The sibling suite `event_source_mapping_failure_destination.rs` pins WHAT that record says,
//! with no IAM mounted, so nothing is known about its role and nothing is withheld; this one
//! pins WHETHER it may be written at all.
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{given_table, KEY, NEW_AND_OLD_IMAGES};
use crate::event_source_mapping_failure_destination::given_topic_fanning_out;
use crate::kinesis_event_source_delivery::given_stream;
use crate::synchronous_invoke_run::package;

/// The Region every call below is made in, and the Region every ARN carries.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound, and where `CreateEventSourceMapping` is.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
/// See [`FUNCTIONS_URI`].
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The `X-Amz-Target` prefixes of the services this suite drives (each service's own
/// `OPERATIONS.md`, "Wire protocol").
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";
/// See [`DYNAMODB_TARGET_PREFIX`].
const KINESIS_TARGET_PREFIX: &str = "Kinesis_20131202";
/// See [`DYNAMODB_TARGET_PREFIX`].
const SQS_TARGET_PREFIX: &str = "AmazonSQS";

/// The runtime every function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `StartingPosition` both shard sources admit, which reads the records already sitting in
/// the stream.
const TRIM_HORIZON: &str = "TRIM_HORIZON";

/// What `LastProcessingResult` reports for an attempt that processed its batch
/// (`EventSourceMappingConfiguration$LastProcessingResult`).
const RESULT_OK: &str = "OK";

/// The modelled error a refused create answers, and the status it carries
/// (`smithy_query(query="operation",operation="CreateEventSourceMapping")`:
/// `InvalidParameterValueException`, `httpError` 400).
const INVALID_PARAMETER: &str = "InvalidParameterValueException";
/// See [`INVALID_PARAMETER`].
const INVALID_PARAMETER_STATUS: u16 = 400;

/// The sentence AWS Lambda refuses a mapping whose declared failure destination the function's
/// execution role may not write with, written out here rather than imported from the emulator
/// so a wrong value there fails a row.
///
/// Fixed text apart from the CALL and the service it would have been made to; the
/// destination's own ARN is not part of it.
fn refusal_sentence(action: &str, service: &str) -> String {
    format!("The function execution role does not have permissions to call {action} on {service}")
}

/// The marker a record carries to be one the handler refuses. Nothing in the emulator reads
/// it: it is read by the handler, out of the record's own data.
const POISON: &str = "poison";

/// The data of the record the handler REFUSES, which is the batch the mapping gives up on, and
/// of the record BEHIND it, which the mapping processes once it has read on past the discard.
const REFUSED_RECORD: &str = "poison-record";
/// See [`REFUSED_RECORD`].
const HEALTHY_RECORD: &str = "healthy-record";

/// How many records one batch holds here: ONE, so the refused record is a batch of its own and
/// the record behind it is the next batch — which is what makes the mapping reading on past the
/// discard observable.
const BATCH_SIZE: i64 = 1;

/// How many shards a Kinesis row's stream holds: one, so the records it is given are one
/// shard's and read in the order they were put.
const ONE_SHARD: i64 = 1;

/// The `MaximumRetryAttempts` every mapping below states: none, so a refused batch is
/// discarded after its one attempt.
const NO_RETRIES: i64 = 0;

/// The write each kind of destination takes, as a statement names it, and the call and service
/// a refusal names it by.
const QUEUE_WRITE: &str = "sqs:SendMessage";
/// See [`QUEUE_WRITE`].
const QUEUE_CALL: (&str, &str) = ("SendMessage", "SQS");
/// See [`QUEUE_WRITE`].
const TOPIC_WRITE: &str = "sns:Publish";
/// See [`QUEUE_WRITE`].
const TOPIC_CALL: (&str, &str) = ("Publish", "SNS");

/// The reads of a DynamoDB table's stream the poller is subject to, which every role below is
/// granted on the stream its mapping polls — so what a row is about is the DESTINATION and
/// never the source.
///
/// Source: the calls a reader of such a stream makes on it, as the grant AWS documents for a
/// table-stream mapping's execution role names them.
const TABLE_STREAM_READS: &[&str] = &[
    "dynamodb:GetRecords",
    "dynamodb:GetShardIterator",
    "dynamodb:DescribeStream",
    "dynamodb:ListStreams",
];

/// The same for an Amazon Kinesis data stream.
const KINESIS_STREAM_READS: &[&str] = &[
    "kinesis:GetRecords",
    "kinesis:GetShardIterator",
    "kinesis:DescribeStream",
    "kinesis:ListStreams",
];

/// How long a row waits for work the CLOCK owes — a table or stream becoming active, a
/// function reaching `Active`, the mapping settling, the batch being read and discarded, and
/// the record reaching its destination.
///
/// A ceiling on PATIENCE, never a span anything counts on: every wait looks again until what
/// it waits for is there or this deadline passes.
const BUDGET: Duration = Duration::from_secs(120);
/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The handler every function below runs: it reads each record of the batch it was handed out
/// of that record's OWN data — whichever shard source it came from — and RAISES for a record
/// carrying [`POISON`], so a batch it refuses is a batch it was really handed.
fn handler_source() -> String {
    format!(
        r#"
import base64
import json

MARKER = "{POISON}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    read = []
    for record in records:
        source = record["eventSource"]
        if source == "aws:kinesis":
            text = base64.b64decode(record["kinesis"]["data"]).decode()
        elif source == "aws:dynamodb":
            text = json.dumps(record["dynamodb"]["Keys"])
        else:
            raise ValueError("eventSource is " + repr(source))
        read.append(text)
        if MARKER in text:
            raise RuntimeError("the handler cannot process " + text)
    return {{"processed": read}}
"#
    )
}

/// Which shard source a row's records sit in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Source {
    /// An Amazon DynamoDB table's stream.
    TableStream,
    /// An Amazon Kinesis data stream.
    Kinesis,
}

impl Source {
    /// The reads of this kind of source the poller is subject to.
    fn reads(self) -> &'static [&'static str] {
        match self {
            Self::TableStream => TABLE_STREAM_READS,
            Self::Kinesis => KINESIS_STREAM_READS,
        }
    }
}

/// Which kind of resource a row's failure destination is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Target {
    /// An Amazon SQS queue, named by its own ARN.
    Queue,
    /// An Amazon SNS topic.
    Topic,
}

impl Target {
    /// The write a record to this kind of destination takes, as a statement names it.
    fn write(self) -> &'static str {
        match self {
            Self::Queue => QUEUE_WRITE,
            Self::Topic => TOPIC_WRITE,
        }
    }

    /// The call and the service a refusal of that write names.
    fn call(self) -> (&'static str, &'static str) {
        match self {
            Self::Queue => QUEUE_CALL,
            Self::Topic => TOPIC_CALL,
        }
    }
}

/// Lambda, DynamoDB, DynamoDB Streams, Kinesis, Amazon SQS, Amazon SNS and IAM on ONE engine:
/// the mapping's source, the destination it records a discarded batch at and the ROLE the write
/// is judged against are all resources of this same emulator.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_sns_amazonsimplenotificationservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming `signing_name` in [`REGION`]. Nothing in it is verified: the
/// harness turns request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// What one Lambda call answered: its status, its body as JSON and as text.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        if self.body.is_empty() {
            return Value::Null;
        }
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The wire error code a refusal carries (`__type`).
    fn error_code(&self) -> String {
        self.json()
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The `Message` a refusal carries, under either spelling a restJson1 answer uses.
    fn message(&self) -> String {
        let body = self.json();
        body.get("Message")
            .or_else(|| body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// One Lambda call, over its own restJson1 routes.
async fn lambda(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: response.into_body(),
    }
}

/// One AWS JSON call against the service whose signing name is `signing_name`.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    content_type: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", content_type)
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(bytes::Bytes::from(body.to_string()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One DynamoDB call.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "dynamodb",
        DYNAMODB_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await
}

/// One Kinesis call.
async fn kinesis(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "kinesis",
        KINESIS_TARGET_PREFIX,
        "application/x-amz-json-1.1",
        operation,
        body,
    )
    .await
}

/// One Amazon SQS call.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(
        router,
        "sqs",
        SQS_TARGET_PREFIX,
        "application/x-amz-json-1.0",
        operation,
        body,
    )
    .await
}

/// One awsQuery call to IAM that must be served.
async fn iam(router: &MultiServiceRouter, case: &str, action: &str, parameters: &[(&str, &str)]) {
    let mut form = form_urlencoded::Serializer::new(String::new());
    form.append_pair("Action", action);
    form.append_pair("Version", "2010-05-08");
    for (name, value) in parameters {
        form.append_pair(name, value);
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .header("authorization", authorization("iam"))
        .body(bytes::Bytes::from(form.finish()))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let answered = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: IAM {action} must succeed, got {status} {answered}"
    );
}

/// A queue of this account: its url and its ARN, both as the queue itself reports them.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, created) = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue {name:?} answered {status} {created}"
    );
    let url = created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueUrl in {created}"))
        .to_string();
    let (status, attributes) = sqs(
        router,
        "GetQueueAttributes",
        &json!({ "QueueUrl": url, "AttributeNames": ["QueueArn"] }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: GetQueueAttributes answered {status} {attributes}"
    );
    let arn = attributes["Attributes"]["QueueArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no QueueArn in {attributes}"))
        .to_string();
    (url, arn)
}

/// Put one record carrying `data` onto `name`, the way a client writes one.
async fn put_record(
    router: &MultiServiceRouter,
    case: &str,
    source: Source,
    name: &str,
    data: &str,
) {
    match source {
        Source::Kinesis => {
            let (status, written) = kinesis(
                router,
                "PutRecord",
                &json!({
                    "StreamName": name,
                    "PartitionKey": data,
                    "Data": base64::engine::general_purpose::STANDARD.encode(data.as_bytes()),
                }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: PutRecord answered {status} {written}"
            );
        }
        Source::TableStream => {
            let (status, written) = dynamodb(
                router,
                "PutItem",
                &json!({ "TableName": name, "Item": { KEY: { "S": data } } }),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: PutItem answered {status} {written}"
            );
        }
    }
}

/// The ARN of the role `name`, as IAM holds it.
fn role_arn(name: &str) -> String {
    format!("arn:aws:iam::{}:role/{name}", engine::emulator_account_id())
}

/// A role of `name` trusted by AWS Lambda, so a function may run as it.
async fn given_role(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let trust = json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": "lambda.amazonaws.com" },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string();
    iam(
        router,
        case,
        "CreateRole",
        &[("RoleName", name), ("AssumeRolePolicyDocument", &trust)],
    )
    .await;
    role_arn(name)
}

/// What ONE role's inline document says: the resources it is about, and what it allows and
/// denies on the destination.
struct Grants<'a> {
    /// The source whose reads the role is granted, and the stream those reads are scoped to.
    source: Source,
    /// See [`Grants::source`].
    stream_arn: &'a str,
    /// The destination the statements below are scoped to.
    destination: &'a str,
    /// The actions the document GRANTS on that destination.
    granted: &'a [&'a str],
    /// The action it explicitly DENIES on it, where a case states one.
    denied: Option<&'a str>,
}

/// Write the inline identity policy in force on the role `name` from now on: the reads of
/// `grants.source` on `grants.stream_arn` always, plus `grants.granted` on
/// `grants.destination` where a case grants anything, plus an explicit deny where it states
/// one.
///
/// A put REPLACES the document, which is how a case WITHDRAWS a grant the role held when its
/// mapping was created.
async fn role_policy(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    grants: &Grants<'_>,
) {
    let mut statements = vec![json!({
        "Effect": "Allow",
        "Action": grants.source.reads(),
        "Resource": grants.stream_arn,
    })];
    if !grants.granted.is_empty() {
        statements.push(json!({
            "Effect": "Allow",
            "Action": grants.granted,
            "Resource": grants.destination,
        }));
    }
    if let Some(denied) = grants.denied {
        statements.push(json!({
            "Effect": "Deny",
            "Action": denied,
            "Resource": grants.destination,
        }));
    }
    let document = json!({ "Version": "2012-10-17", "Statement": statements }).to_string();
    iam(
        router,
        case,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "poller-and-destination"),
            ("PolicyDocument", &document),
        ],
    )
    .await;
}

/// A function whose package RUNS [`handler_source`] under `role_arn`, waited out to `Active`.
async fn given_function(router: &MultiServiceRouter, case: &str, name: &str, role_arn: &str) {
    let created = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&handler_source())),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction answered {} {}",
        created.status,
        created.text()
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
        )
        .await;
        if read.json()["State"] == json!("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function never became Active: {}",
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// `CreateEventSourceMapping` for `function` over `stream_arn`, recording a batch it gives up
/// on at `destination`.
async fn create_mapping(
    router: &MultiServiceRouter,
    function: &str,
    stream_arn: &str,
    destination: &str,
) -> Answer {
    lambda(
        router,
        "POST",
        MAPPINGS_URI,
        &json!({
            "FunctionName": function,
            "EventSourceArn": stream_arn,
            "StartingPosition": TRIM_HORIZON,
            "BatchSize": BATCH_SIZE,
            "Enabled": true,
            "MaximumRetryAttempts": NO_RETRIES,
            "DestinationConfig": { "OnFailure": { "Destination": destination } },
        }),
    )
    .await
}

/// How many mappings this store holds for `function`.
async fn mappings_held(router: &MultiServiceRouter, case: &str, function: &str) -> usize {
    let listed = lambda(
        router,
        "GET",
        &format!("{MAPPINGS_URI}?FunctionName={function}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "case {case:?}: ListEventSourceMappings answered {} {}",
        listed.status,
        listed.text()
    );
    listed.json()["EventSourceMappings"]
        .as_array()
        .map_or(0, Vec::len)
}

/// Every message visible on the queue `url` addresses right now, removed from it so a later
/// look sees only what arrived after this one.
async fn drained(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let (status, received) = sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: ReceiveMessage answered {status} {received}"
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    for message in &messages {
        let handle = message["ReceiptHandle"].as_str().unwrap_or_default();
        let (status, deleted) = sqs(
            router,
            "DeleteMessage",
            &json!({ "QueueUrl": url, "ReceiptHandle": handle }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DeleteMessage answered {status} {deleted}"
        );
    }
    messages
}

/// Wait until the mapping has PROCESSED a batch, which for every case below is the record
/// BEHIND the discarded one: the mapping reads on past a batch it gave up on, so this is the
/// barrier that makes the state of the destination a settled fact. Answers everything that
/// reached the queue at `url` while waiting.
async fn awaited_read_on_past_the_discard(
    router: &MultiServiceRouter,
    case: &str,
    uuid: &str,
    url: &str,
) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    let mut seen: Vec<Value> = Vec::new();
    loop {
        seen.extend(drained(router, case, url).await);
        let read = lambda(
            router,
            "GET",
            &format!("{MAPPINGS_URI}/{uuid}"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: GetEventSourceMapping answered {} {}",
            read.status,
            read.text()
        );
        if read.json()["LastProcessingResult"] == json!(RESULT_OK) {
            // One more look, so a record written just before the healthy batch landed is
            // counted rather than raced past.
            seen.extend(drained(router, case, url).await);
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping never read on past the discarded batch within \
             {BUDGET:?}, last reported {}",
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

// ---------------------------------------------------------------------------
// The call that CONFIGURES the destination
// ---------------------------------------------------------------------------

/// One row of the create table: the destination kind the mapping names, what the role's
/// policies say about its write, which source the mapping polls, and whether the create is
/// accepted.
struct CreateRow {
    case: &'static str,
    /// A name no other row uses, for the function and role the row owns.
    name: &'static str,
    source: Source,
    target: Target,
    /// The actions the role's inline policy GRANTS on the destination.
    granted: &'static [&'static str],
    /// The action it explicitly DENIES on the destination, where a row states one.
    denied: Option<&'static str>,
    /// Whether the create is accepted.
    accepted: bool,
}

/// The rows differ along the kind of destination the write is judged for, what the role's
/// policies say about that write, and which shard source the mapping polls.
fn create_rows() -> Vec<CreateRow> {
    vec![
        CreateRow {
            case: "a queue destination whose write the role is granted",
            name: "esmdest-create-queue-granted",
            source: Source::TableStream,
            target: Target::Queue,
            granted: &[QUEUE_WRITE],
            denied: None,
            accepted: true,
        },
        CreateRow {
            case: "a queue destination the role is granted nothing about",
            name: "esmdest-create-queue-none",
            source: Source::TableStream,
            target: Target::Queue,
            granted: &[],
            denied: None,
            accepted: false,
        },
        CreateRow {
            case: "a queue destination and a role granted only a topic's write",
            name: "esmdest-create-queue-other",
            source: Source::TableStream,
            target: Target::Queue,
            granted: &[TOPIC_WRITE],
            denied: None,
            accepted: false,
        },
        CreateRow {
            case: "a queue destination whose write is granted and explicitly denied",
            name: "esmdest-create-queue-denied",
            source: Source::TableStream,
            target: Target::Queue,
            granted: &[QUEUE_WRITE],
            denied: Some(QUEUE_WRITE),
            accepted: false,
        },
        CreateRow {
            case: "a topic destination whose publish the role is granted",
            name: "esmdest-create-topic-granted",
            source: Source::TableStream,
            target: Target::Topic,
            granted: &[TOPIC_WRITE],
            denied: None,
            accepted: true,
        },
        CreateRow {
            case: "a topic destination the role is granted nothing about",
            name: "esmdest-create-topic-none",
            source: Source::TableStream,
            target: Target::Topic,
            granted: &[],
            denied: None,
            accepted: false,
        },
        CreateRow {
            case: "a kinesis source whose queue destination the role is granted nothing about",
            name: "esmdest-create-kinesis-none",
            source: Source::Kinesis,
            target: Target::Queue,
            granted: &[],
            denied: None,
            accepted: false,
        },
        CreateRow {
            case: "a kinesis source whose queue destination the role may write",
            name: "esmdest-create-kinesis-granted",
            source: Source::Kinesis,
            target: Target::Queue,
            granted: &[QUEUE_WRITE],
            denied: None,
            accepted: true,
        },
    ]
}

/// RULE (1): the call that configures a mapping refuses a failure destination whose write the
/// policies in force on the invoked function's execution role withhold, with the modelled 400
/// and no mapping stored.
#[tokio::test]
async fn a_failure_destination_the_execution_role_may_not_write_is_refused_at_the_create() {
    let router = emulator();
    let setup = "the suite's own wiring";
    // The two sources and the two destinations every row shares: a row is about the ROLE, so
    // the resources it judges are the same ones throughout.
    let table_stream = given_table(
        &router,
        setup,
        "esmdest-create-table",
        Some(NEW_AND_OLD_IMAGES),
    )
    .await;
    let data_stream = given_stream(&router, setup, "esmdest-create-stream", ONE_SHARD).await;
    let (queue_url, queue) = given_queue(&router, setup, "esmdest-create-records").await;
    let topic = given_topic_fanning_out(
        &router,
        setup,
        "esmdest-create-records",
        &queue_url,
        &queue,
    )
    .await;

    for row in create_rows() {
        let case = row.case;
        let stream_arn = match row.source {
            Source::TableStream => &table_stream,
            Source::Kinesis => &data_stream,
        };
        let destination = match row.target {
            Target::Queue => &queue,
            Target::Topic => &topic,
        };
        let role = given_role(&router, case, row.name).await;
        role_policy(
            &router,
            case,
            row.name,
            &Grants {
                source: row.source,
                stream_arn,
                destination,
                granted: row.granted,
                denied: row.denied,
            },
        )
        .await;
        given_function(&router, case, row.name, &role).await;

        let answered = create_mapping(&router, row.name, stream_arn, destination).await;
        let held = mappings_held(&router, case, row.name).await;
        if row.accepted {
            assert_eq!(
                answered.status,
                202,
                "case {case:?}: a destination the role may write must be accepted, got {} {}",
                answered.status,
                answered.text()
            );
            assert_eq!(
                held, 1,
                "case {case:?}: an accepted create must store one mapping, got {held}"
            );
            continue;
        }
        assert_eq!(
            answered.status,
            INVALID_PARAMETER_STATUS,
            "case {case:?}: expected HTTP {INVALID_PARAMETER_STATUS}, got {} {}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.error_code(),
            INVALID_PARAMETER,
            "case {case:?}: expected {INVALID_PARAMETER}, got {}",
            answered.text()
        );
        let (action, service) = row.target.call();
        assert_eq!(
            answered.message(),
            refusal_sentence(action, service),
            "case {case:?}: expected {:?}, got {}",
            refusal_sentence(action, service),
            answered.text()
        );
        assert_eq!(
            held, 0,
            "case {case:?}: a refused create must store no mapping, got {held}"
        );
    }
}

// ---------------------------------------------------------------------------
// The POLLER that writes the record
// ---------------------------------------------------------------------------

/// One row of the delivery table: whether the grant the create was accepted under is
/// WITHDRAWN before the batch is discarded, and whether the record then lands.
struct DeliveryRow {
    case: &'static str,
    /// A name no other row uses, for every resource the row creates.
    name: &'static str,
    source: Source,
    target: Target,
    /// Whether the role's document is rewritten WITHOUT the destination's write once the
    /// mapping has been created.
    withdrawn: bool,
    /// Whether the record of the discarded batch reaches the destination.
    recorded: bool,
}

/// The rows differ along whether the grant is still in force when the record is written, and
/// along the kind of destination and the shard source — because the rule turns on the grant
/// alone.
fn delivery_rows() -> Vec<DeliveryRow> {
    vec![
        DeliveryRow {
            case: "a queue destination whose write the role still has",
            name: "esmdest-write-queue-held",
            source: Source::TableStream,
            target: Target::Queue,
            withdrawn: false,
            recorded: true,
        },
        DeliveryRow {
            case: "a queue destination whose write was withdrawn after the create",
            name: "esmdest-write-queue-gone",
            source: Source::TableStream,
            target: Target::Queue,
            withdrawn: true,
            recorded: false,
        },
        DeliveryRow {
            case: "a topic destination whose publish was withdrawn after the create",
            name: "esmdest-write-topic-gone",
            source: Source::TableStream,
            target: Target::Topic,
            withdrawn: true,
            recorded: false,
        },
        DeliveryRow {
            case: "a kinesis source whose queue destination was withdrawn after the create",
            name: "esmdest-write-kinesis-gone",
            source: Source::Kinesis,
            target: Target::Queue,
            withdrawn: true,
            recorded: false,
        },
    ]
}

/// RULE (2): the poller asks again when it writes, so a destination whose write the role no
/// longer has receives nothing — while the batch is discarded and the mapping reads on either
/// way.
#[tokio::test]
async fn the_record_of_a_discarded_batch_is_written_only_while_the_role_may_write_it() {
    for row in delivery_rows() {
        let case = row.case;
        let router = emulator();
        let stream_arn = match row.source {
            Source::TableStream => {
                given_table(&router, case, row.name, Some(NEW_AND_OLD_IMAGES)).await
            }
            Source::Kinesis => given_stream(&router, case, row.name, ONE_SHARD).await,
        };
        // The queue is the row's observation point whichever kind the destination is: a topic
        // destination relays to it.
        let (queue_url, queue) =
            given_queue(&router, case, &format!("{}-records", row.name)).await;
        let destination = match row.target {
            Target::Queue => queue.clone(),
            Target::Topic => {
                given_topic_fanning_out(
                    &router,
                    case,
                    &format!("{}-records", row.name),
                    &queue_url,
                    &queue,
                )
                .await
            }
        };
        let role = given_role(&router, case, row.name).await;
        role_policy(
            &router,
            case,
            row.name,
            &Grants {
                source: row.source,
                stream_arn: &stream_arn,
                destination: &destination,
                granted: &[row.target.write()],
                denied: None,
            },
        )
        .await;
        given_function(&router, case, row.name, &role).await;

        let answered = create_mapping(&router, row.name, &stream_arn, &destination).await;
        assert_eq!(
            answered.status,
            202,
            "case {case:?}: the create must be accepted under the grant, got {} {}",
            answered.status,
            answered.text()
        );
        let uuid = answered
            .json()["UUID"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", answered.text()))
            .to_string();

        if row.withdrawn {
            // The document in force is replaced by one carrying the source's reads alone, so
            // the poller still reads and invokes and only the destination's write is gone.
            role_policy(
                &router,
                case,
                row.name,
                &Grants {
                    source: row.source,
                    stream_arn: &stream_arn,
                    destination: &destination,
                    granted: &[],
                    denied: None,
                },
            )
            .await;
        }

        // The refused record first, then the healthy one behind it: each is a batch of its
        // own, so the mapping gives up on the first and processes the second.
        put_record(&router, case, row.source, row.name, REFUSED_RECORD).await;
        put_record(&router, case, row.source, row.name, HEALTHY_RECORD).await;

        let delivered =
            awaited_read_on_past_the_discard(&router, case, &uuid, &queue_url).await;
        if !row.recorded {
            assert!(
                delivered.is_empty(),
                "case {case:?}: expected the destination to receive nothing once the role may \
                 not write it, got {delivered:?}"
            );
            continue;
        }
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: expected exactly the record of the discarded batch, got \
             {delivered:?}"
        );
        let body = delivered[0]["Body"].as_str().unwrap_or_default();
        let record: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the record is no document: {error} in {body}")
        });
        let named = record["requestContext"]["functionArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            named.ends_with(&format!(":function:{}", row.name)),
            "case {case:?}: expected the record to name the function {:?}, got {named:?} in \
             {record}",
            row.name
        );
        assert!(
            !body.contains(HEALTHY_RECORD),
            "case {case:?}: expected the record of the REFUSED batch alone, got {body}"
        );
    }
}
