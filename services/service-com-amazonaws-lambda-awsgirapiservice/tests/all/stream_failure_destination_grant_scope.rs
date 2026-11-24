// SPDX-License-Identifier: CC0-1.0

//! WHICH grant authorizes the record of a batch an event source mapping on a table's stream
//! gave up on — Lambda, Amazon DynamoDB, DynamoDB Streams, Amazon SQS and IAM on ONE engine,
//! with a real IAM role behind the function.
//!
//! RULE: Lambda reads a mapping's event source and invokes its function ACTING AS that
//! function's execution role — "the Amazon Resource Name (ARN) of the function's execution
//! role" (`FunctionConfiguration$Role`) — and the record it writes to the mapping's
//! `DestinationConfig$OnFailure` ("the destination configuration for failed invocations") is
//! one more call made under that identity, so it stands only while the policies in force on
//! that role allow THAT destination's own write ON THAT DESTINATION's resource. Two
//! consequences, each asserted here:
//!
//! 1. `CreateEventSourceMapping` pre-flights the pair and refuses a destination the role may
//!    not write with the modelled `InvalidParameterValueException` (HTTP 400), storing no
//!    mapping, rather than wiring up a dead-letter path no record could reach;
//! 2. the POLLER asks again when it writes, so a role whose grant no longer covers the
//!    destination delivers nothing there — while the batch is discarded and the mapping reads
//!    on either way.
//!
//! The sibling suite `event_source_mapping_destination_role.rs` varies the destination KIND and
//! what the role's policies SAY about its write, always scoped to the destination itself. This
//! one varies the one thing that suite holds fixed: the RESOURCE the grant is scoped to — the
//! destination's own ARN, the wildcard standing for every resource, a prefix wildcard covering
//! the destination, and another queue of the same account — plus the ACTION's own breadth (the
//! write itself, the service's whole action set, a read of the destination that is not its
//! write, and no statement about it at all). None of those is what the rule turns on: the
//! question is whether the documents in force allow the destination's write on the destination.
//!
//! The function's package RUNS, and whether it refuses a batch is decided inside the handler
//! from the record's own keys, so a discard here is a consequence of the handler having been
//! invoked with the batch.
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{
    given_table, write_item, Write, NEW_AND_OLD_IMAGES,
};
use crate::event_source_mapping_failure_destination::{given_queue, sqs};
use crate::handler_bedrock_agent_sdk::{execution_role, role_policy};
use crate::synchronous_invoke_run::package;

/// The Region every call below is made in, and the Region every ARN carries.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound, and where `CreateEventSourceMapping` is.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
/// See [`FUNCTIONS_URI`].
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The runtime every function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `StartingPosition` a table-stream mapping reads the records already in the stream from
/// (`CreateEventSourceMappingRequest$StartingPosition`).
const TRIM_HORIZON: &str = "TRIM_HORIZON";

/// The `MaximumRetryAttempts` every mapping below states: none, so a refused batch is given up
/// on after its one attempt.
const NO_RETRIES: i64 = 0;

/// How many records one batch holds here: ONE, so the refused record is a batch of its own and
/// the record behind it is the next batch.
const BATCH_SIZE: i64 = 1;

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
/// execution role may not write with, written out here rather than imported from the emulator so
/// a wrong value there fails a row. Entirely fixed text for a queue destination.
const REFUSAL_SENTENCE: &str =
    "The function execution role does not have permissions to call SendMessage on SQS";

/// The write a record to a queue takes, as an identity policy names it.
const QUEUE_WRITE: &str = "sqs:SendMessage";

/// The whole action set of the queue service, which COVERS that write.
const QUEUE_ACTIONS: &str = "sqs:*";

/// Two calls a reader of a queue makes, neither of which is the write a record takes.
const QUEUE_READS: &[&str] = &["sqs:ReceiveMessage", "sqs:GetQueueAttributes"];

/// The reads of a DynamoDB table's stream the poller is subject to, which every role below is
/// granted on the stream its mapping polls — so what a row is about is the DESTINATION and never
/// the source.
///
/// Source: the calls a reader of such a stream makes on it, as the grant AWS documents for a
/// table-stream mapping's execution role names them.
const TABLE_STREAM_READS: &[&str] = &[
    "dynamodb:GetRecords",
    "dynamodb:GetShardIterator",
    "dynamodb:DescribeStream",
    "dynamodb:ListStreams",
];

/// The logging grant an execution role carries, which says nothing about any destination.
const LOGS_GRANTS: &[&str] = &[
    "logs:CreateLogGroup",
    "logs:CreateLogStream",
    "logs:PutLogEvents",
];

/// What `requestContext.condition` reports for a batch whose every allowed attempt was spent.
const CONDITION_RETRY_ATTEMPTS_EXHAUSTED: &str = "RetryAttemptsExhausted";

/// The member the batch of a DynamoDB table's stream is described under.
const DDB_STREAM_BATCH_INFO: &str = "DDBStreamBatchInfo";

/// The name prefix the destination queue of each test carries, which the prefix-wildcard row's
/// grant is scoped to.
const DESTINATION_PREFIX: &str = "esmgrantscope";

/// How long a row waits for work the CLOCK owes — the table becoming active, the function
/// reaching `Active`, the mapping settling, the batch being read and discarded, and the record
/// reaching its destination.
///
/// A ceiling on PATIENCE, never a span anything counts on: every wait looks again until what it
/// waits for is there or this deadline passes.
const BUDGET: Duration = Duration::from_secs(120);
/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// The key value whose record the handler RAISES on, which is the batch the mapping gives up on,
/// and the key of the record behind it, which the mapping processes once it has read on past the
/// discard. Nothing in the emulator reads either: they are read by the handler, out of the
/// record's own keys.
const REFUSED_KEY: &str = "poison-record";
/// See [`REFUSED_KEY`].
const HEALTHY_KEY: &str = "healthy-record";

/// The handler every function below runs: it reads each record's own keys and RAISES for the
/// refused one, so a batch it gives up on is a batch it was really handed.
fn handler_source() -> String {
    format!(
        r#"
import json


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    read = []
    for record in records:
        keys = record["dynamodb"]["Keys"]
        text = json.dumps(keys)
        read.append(text)
        if "{REFUSED_KEY}" in text:
            raise RuntimeError("the handler cannot process " + text)
    return {{"processed": read}}
"#
    )
}

/// WHICH resource a row's destination grant is scoped to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// The destination queue's own ARN, the narrowest scope that covers it.
    Destination,
    /// The wildcard that stands for every resource.
    Everything,
    /// A prefix wildcard over this account's queues of this suite, which covers the
    /// destination.
    Prefix,
    /// Another queue of the same account, which is not the destination.
    Other,
}

impl Scope {
    /// The `Resource` element a statement of this scope names, for the destination `destination`
    /// and the other queue `other`.
    fn resource(self, destination: &str, other: &str) -> String {
        match self {
            Self::Destination => destination.to_string(),
            Self::Everything => "*".to_string(),
            Self::Prefix => format!(
                "arn:aws:sqs:{REGION}:{}:{DESTINATION_PREFIX}*",
                engine::emulator_account_id()
            ),
            Self::Other => other.to_string(),
        }
    }
}

/// Lambda, DynamoDB, DynamoDB Streams, Amazon SQS and IAM on ONE engine: the mapping's source,
/// the destination it records a discarded batch at and the ROLE the write is judged against are
/// all resources of this same emulator.
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
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 credential scope naming Lambda in [`REGION`]. Nothing in it is verified: the harness
/// turns request authentication off.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/lambda/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// What one Lambda call answered: its status and its body, as JSON and as text.
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

    /// The message a refusal carries, under either spelling a restJson1 answer uses.
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
        .header("authorization", authorization())
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    Answer {
        status: response.status().as_u16(),
        body: response.into_body(),
    }
}

/// The identity policy document in force on a row's role: the logging grant and the stream's
/// reads always, plus the statement a row's `destination` names where it states one.
fn policy_document(stream_arn: &str, destination: Option<(&[&str], &str)>) -> String {
    let mut statements = vec![
        json!({ "Effect": "Allow", "Action": LOGS_GRANTS, "Resource": "*" }),
        json!({ "Effect": "Allow", "Action": TABLE_STREAM_READS, "Resource": stream_arn }),
    ];
    if let Some((actions, resource)) = destination {
        statements.push(json!({ "Effect": "Allow", "Action": actions, "Resource": resource }));
    }
    json!({ "Version": "2012-10-17", "Statement": statements }).to_string()
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

/// `CreateEventSourceMapping` for `function` over `stream_arn`, recording a batch it gives up on
/// at `destination`.
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

/// Every message visible on the queue `url` addresses right now, removed from it so a later look
/// sees only what arrived after this one.
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

/// Wait until the mapping has PROCESSED a batch, which for every row below is the record BEHIND
/// the discarded one: the mapping reads on past a batch it gave up on, so this is the barrier
/// that makes the state of the destination a settled fact. Answers everything that reached the
/// queue at `url` while waiting.
///
/// A row that EXPECTS the record waits on for it beyond that barrier ([`awaited_record`]), since
/// the write and the healthy batch's completion are two independent pieces of work; a row that
/// expects none reads the barrier as the whole answer, because the discard it is about has
/// already happened.
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
        let read = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), &Value::Null).await;
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
            "case {case:?}: the mapping never read on past the discarded batch within {BUDGET:?}, \
             last reported {}",
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Wait for the record of the discarded batch to reach the queue at `url`, past the barrier
/// above: the write and the healthy batch's completion are independent pieces of work, so a row
/// that expects the record looks again until it is there or the deadline passes rather than
/// reading one look as the answer.
async fn awaited_record(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<Value> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let seen = drained(router, case, url).await;
        if !seen.is_empty() {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the record of the discarded batch never reached the destination \
             within {BUDGET:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

// ---------------------------------------------------------------------------
// The call that CONFIGURES the destination
// ---------------------------------------------------------------------------

/// One row of the create table: what the role's document allows, which resource that allowance
/// is scoped to, and whether the create is accepted.
struct CreateRow {
    case: &'static str,
    /// A name no other row uses, for the function and role the row owns.
    name: &'static str,
    /// The actions the role's inline policy grants, or none at all for a row whose document says
    /// nothing about any queue.
    granted: &'static [&'static str],
    scope: Scope,
    /// Whether the create is accepted.
    accepted: bool,
}

/// The rows differ along the RESOURCE the grant is scoped to and the BREADTH of the action it
/// names — the two halves of "the role may make this write on this destination".
fn create_rows() -> Vec<CreateRow> {
    vec![
        CreateRow {
            case: "the write, scoped to the destination's own ARN",
            name: "esmgrant-create-exact",
            granted: &[QUEUE_WRITE],
            scope: Scope::Destination,
            accepted: true,
        },
        CreateRow {
            case: "the write, scoped to every resource",
            name: "esmgrant-create-everything",
            granted: &[QUEUE_WRITE],
            scope: Scope::Everything,
            accepted: true,
        },
        CreateRow {
            case: "the write, scoped to a prefix wildcard covering the destination",
            name: "esmgrant-create-prefix",
            granted: &[QUEUE_WRITE],
            scope: Scope::Prefix,
            accepted: true,
        },
        CreateRow {
            case: "the service's whole action set on the destination",
            name: "esmgrant-create-allactions",
            granted: &[QUEUE_ACTIONS],
            scope: Scope::Destination,
            accepted: true,
        },
        CreateRow {
            case: "the write, scoped to another queue of the same account",
            name: "esmgrant-create-elsewhere",
            granted: &[QUEUE_WRITE],
            scope: Scope::Other,
            accepted: false,
        },
        CreateRow {
            case: "reads of the destination, which are not its write",
            name: "esmgrant-create-reads",
            granted: QUEUE_READS,
            scope: Scope::Destination,
            accepted: false,
        },
        CreateRow {
            case: "no statement about any queue at all",
            name: "esmgrant-create-silent",
            granted: &[],
            scope: Scope::Destination,
            accepted: false,
        },
    ]
}

/// RULE (1): the create is accepted exactly while the documents in force on the function's
/// execution role allow the destination's own write ON THE DESTINATION, and otherwise refused
/// with the modelled 400 and no mapping stored.
#[tokio::test]
async fn a_mapping_is_wired_only_where_the_role_may_write_that_destination() {
    let router = emulator();
    let setup = "the suite's own wiring";
    // The one source, the destination and the other queue every row shares: a row is about the
    // ROLE, so the resources it judges are the same ones throughout.
    let stream_arn = given_table(
        &router,
        setup,
        &format!("{DESTINATION_PREFIX}-create-table"),
        Some(NEW_AND_OLD_IMAGES),
    )
    .await;
    let (_destination_url, destination) =
        given_queue(&router, setup, &format!("{DESTINATION_PREFIX}-create-records")).await;
    let (_other_url, other) = given_queue(&router, setup, "esmgrantother-create-records").await;

    for row in create_rows() {
        let case = row.case;
        let resource = row.scope.resource(&destination, &other);
        let statement = (!row.granted.is_empty()).then_some((row.granted, resource.as_str()));
        let role = execution_role(&router, row.name, &policy_document(&stream_arn, statement)).await;
        given_function(&router, case, row.name, &role).await;

        let answered = create_mapping(&router, row.name, &stream_arn, &destination).await;
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
        assert_eq!(
            answered.message(),
            REFUSAL_SENTENCE,
            "case {case:?}: expected {REFUSAL_SENTENCE:?}, got {}",
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

/// One row of the delivery table: the scope the role's grant is RESTATED at once the mapping has
/// been created, and whether the record of the discarded batch then lands.
struct DeliveryRow {
    case: &'static str,
    /// A name no other row uses, for every resource the row creates.
    name: &'static str,
    /// The scope the destination grant is rewritten at after the create.
    restated: Scope,
    /// Whether the record of the discarded batch reaches the destination.
    recorded: bool,
}

/// Both rows rewrite the document after the create — so what a row turns on is the RESOURCE the
/// restated grant covers, and nothing about when it was written.
fn delivery_rows() -> Vec<DeliveryRow> {
    vec![
        DeliveryRow {
            case: "a grant restated over every resource",
            name: "esmgrant-write-everything",
            restated: Scope::Everything,
            recorded: true,
        },
        DeliveryRow {
            case: "a grant restated over another queue of the same account",
            name: "esmgrant-write-elsewhere",
            restated: Scope::Other,
            recorded: false,
        },
    ]
}

/// RULE (2): the poller asks the documents in force again when it writes, so the record reaches
/// the destination only while the grant then in force covers that destination — and the batch is
/// discarded and read on past either way.
#[tokio::test]
async fn the_record_reaches_the_destination_only_under_a_grant_that_covers_it() {
    for row in delivery_rows() {
        let case = row.case;
        let router = emulator();
        let table = format!("{DESTINATION_PREFIX}-{}", row.name);
        let stream_arn = given_table(&router, case, &table, Some(NEW_AND_OLD_IMAGES)).await;
        let (destination_url, destination) =
            given_queue(&router, case, &format!("{DESTINATION_PREFIX}-{}", row.name)).await;
        let (_other_url, other) =
            given_queue(&router, case, &format!("esmgrantother-{}", row.name)).await;

        // The create is accepted under a grant scoped to the destination itself, so every row
        // reaches the poller the same way.
        let granted = policy_document(
            &stream_arn,
            Some((&[QUEUE_WRITE], Scope::Destination.resource(&destination, &other).as_str())),
        );
        let role = execution_role(&router, row.name, &granted).await;
        given_function(&router, case, row.name, &role).await;

        let answered = create_mapping(&router, row.name, &stream_arn, &destination).await;
        assert_eq!(
            answered.status,
            202,
            "case {case:?}: the create must be accepted under the grant, got {} {}",
            answered.status,
            answered.text()
        );
        let uuid = answered.json()["UUID"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", answered.text()))
            .to_string();

        // The document in force is REPLACED by one granting the same write at the row's scope,
        // so only the resource the grant covers differs from here on.
        let restated = policy_document(
            &stream_arn,
            Some((
                &[QUEUE_WRITE],
                row.restated.resource(&destination, &other).as_str(),
            )),
        );
        role_policy(&router, case, row.name, &restated).await;

        // The refused record first, then the healthy one behind it: each is a batch of its own,
        // so the mapping gives up on the first and processes the second.
        write_item(&router, case, &table, REFUSED_KEY, Write::Insert).await;
        write_item(&router, case, &table, HEALTHY_KEY, Write::Insert).await;

        let mut delivered =
            awaited_read_on_past_the_discard(&router, case, &uuid, &destination_url).await;
        if row.recorded && delivered.is_empty() {
            delivered = awaited_record(&router, case, &destination_url).await;
        }
        if !row.recorded {
            assert!(
                delivered.is_empty(),
                "case {case:?}: expected the destination to receive nothing under a grant that \
                 does not cover it, got {delivered:?}"
            );
            continue;
        }
        assert_eq!(
            delivered.len(),
            1,
            "case {case:?}: expected exactly the record of the discarded batch, got {delivered:?}"
        );
        let body = delivered[0]["Body"].as_str().unwrap_or_default();
        let record: Value = serde_json::from_str(body).unwrap_or_else(|error| {
            panic!("case {case:?}: the record is no document: {error} in {body}")
        });
        let condition = record["requestContext"]["condition"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            condition, CONDITION_RETRY_ATTEMPTS_EXHAUSTED,
            "case {case:?}: expected {CONDITION_RETRY_ATTEMPTS_EXHAUSTED:?}, got \
             {condition:?} in {record}"
        );
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
        let recorded_stream = record[DDB_STREAM_BATCH_INFO]["streamArn"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            recorded_stream, stream_arn,
            "case {case:?}: expected the batch to name the stream {stream_arn:?}, got \
             {recorded_stream:?} in {record}"
        );
        // The record describes the batch rather than carrying its payloads, so the healthy key
        // — the record BEHIND the discarded batch — is what must be absent from it.
        assert!(
            !body.contains(HEALTHY_KEY),
            "case {case:?}: expected the record of the REFUSED batch alone, got {body}"
        );
        assert_eq!(
            record[DDB_STREAM_BATCH_INFO]["batchSize"],
            json!(BATCH_SIZE),
            "case {case:?}: expected a batch of {BATCH_SIZE} record, got {record}"
        );
    }
}
