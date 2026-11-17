// SPDX-License-Identifier: CC0-1.0

//! Which records of an Amazon DynamoDB table's STREAM a mapping carrying `FilterCriteria`
//! hands its function, on the ASSEMBLED emulator: DynamoDB holds the table and mints its
//! stream, DynamoDB Streams serves that stream's shards and records, CloudFormation archives
//! the inline handler into the function's deployment package, IAM holds the execution role the
//! poller reads the stream as and the handler's own call is judged against, STS mints the
//! session that call is signed with, Lambda polls the stream and RUNS the handler, and Amazon
//! SQS takes the one message the handler sends per record it was handed.
//!
//! RULE: `FilterCriteria` is "an object that defines the filter criteria that determine
//! whether Lambda should process an event" and `FilterCriteria$Filters` is "a list of
//! filters", so of the records one poll takes off the source exactly those matching AT LEAST
//! ONE of the mapping's patterns are handed to the function; a record no pattern matches is
//! discarded by the poller without an invocation while the mapping's position still advances
//! past it, so the records behind it are delivered, and a mapping carrying no criteria
//! determines nothing and admits every record.
//!
//! Six crates have to agree for that to hold, and the handler's own SDK call is dispatched
//! back into this same router, so only the assembled emulator shows it end to end: the
//! owning crate's `dynamodb_stream_event_source_filter_criteria` pins the decision over a
//! router holding Lambda, DynamoDB, DynamoDB Streams, Amazon SQS and IAM alone, and
//! `all/ddb_stream_lambda_custom_metric_alarm` pins what an UNFILTERED mapping's function
//! publishes about its own work. What is added here is the filtered mapping on the whole
//! emulator, with the records written BEFORE the mapping exists and read from the oldest
//! retained one, which is the shape a client wires an insert-only projection in.
//!
//! Every row's verdict is read off what the HANDLER forwarded: a record that reached it
//! appears as one message on that row's queue, so a record withheld from it cannot appear.
//! The last record each row expects delivered is written AFTER every record it expects
//! withheld, and a shard is read in order — so once that record's message is on the queue the
//! withheld ones have been read past, and their absence is a verdict rather than a race.
//!
//! The rows differ along what the rule generalises over: which member of the record the
//! pattern names (the change's own name, a value nested inside the changed item's keys), how
//! many values one clause lists, how many patterns the criteria carry (none, one, two that
//! are ORed), and how much of the batch they admit (all of it, some of it, none of it).

use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::cloudformation_firehose_stream_stabilisation::wait_for_stack_status;
use crate::cloudwatch_alarm_action_reaches_its_subscribed_queue::{authorization, router as emulator};
use crate::query_wire::{elements, form_encode};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-east-1";

/// DynamoDB's `X-Amz-Target` prefix.
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// Where Lambda binds the two collections used below.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The stack the execution role and the forwarding functions are deployed by, and the
/// `StackStatus` it has to reach.
const STACK: &str = "xsvc-filtered-stream-consumers";
const CREATE_COMPLETE: &str = "CREATE_COMPLETE";

/// The attribute every item below is keyed by, and the attribute a write changes.
const KEY: &str = "pk";
const VALUE: &str = "v";

/// The `StreamViewType` every source table is created with: both images, so every member a
/// pattern could name is in the record.
const STREAM_VIEW_TYPE: &str = "NEW_AND_OLD_IMAGES";

/// Where every mapping starts reading: the oldest record in the shard, so a write made before
/// the mapping's poller first looked is still delivered.
const STARTING_POSITION: &str = "TRIM_HORIZON";

/// How many records one mapping takes per batch, and the batching window it gathers one in —
/// the shape the recorded wiring uses.
const BATCH_SIZE: u64 = 10;
const BATCHING_WINDOW_SECONDS: u64 = 1;

/// How long one invocation is given, as the function's own `Timeout`: well past what a run
/// needs on any host, because what is under test is which records reach the handler and not
/// how fast an interpreter gets through it.
const FUNCTION_TIMEOUT: u64 = 120;

/// The `OperationType` a stream record reports for each kind of change
/// (`smithy_query(query="shape", shape="OperationType")`, DynamoDB Streams).
const INSERT: &str = "INSERT";
/// See [`INSERT`].
const MODIFY: &str = "MODIFY";
/// See [`INSERT`].
const REMOVE: &str = "REMOVE";

/// The environment variable the handler reads the queue it forwards to out of.
const QUEUE_VARIABLE: &str = "QUEUE_URL";

/// How long an observation of work the CLOCK owes — a table reaching `ACTIVE`, a function
/// going `Active`, a mapping settling, a record reaching the handler — waits before calling it
/// a hang, and how long between two looks. Generous: a row fails only when the work did not
/// happen at all, so a loaded host cannot decide the outcome.
const DEADLINE: Duration = Duration::from_secs(120);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(100);

/// The handler every row runs: for each record it is handed it forwards ONE message to the
/// queue its environment names, carrying the change's own name and the changed item's key,
/// both taken out of the record itself — so a message can only appear for a record that
/// reached it.
const FORWARDING_HANDLER: &str = r##"
import json
import os

import boto3

QUEUE_URL = os.environ["QUEUE_URL"]


def handler(event, context):
    client = boto3.client("sqs")
    forwarded = 0
    for record in event["Records"]:
        client.send_message(
            QueueUrl=QUEUE_URL,
            MessageBody=json.dumps(
                {
                    "eventName": record["eventName"],
                    "pk": record["dynamodb"]["Keys"]["pk"]["S"],
                }
            ),
        )
        forwarded = forwarded + 1
    return {"forwarded": forwarded}
"##;

/// What one write does to the item it names.
#[derive(Debug, Clone, Copy)]
enum Write {
    /// A key the table does not hold yet, which the stream reports as an `INSERT`.
    Insert,
    /// A key it holds, written with a different value, which it reports as a `MODIFY`.
    Overwrite,
    /// A key it holds, deleted, which it reports as a `REMOVE`.
    Delete,
}

/// One row: the patterns the mapping filters with, the writes made to its table, and which of
/// the records they produce the handler must and must not be handed.
struct Row {
    case: &'static str,
    /// The `FilterCriteria.Filters[].Pattern` values, in order; empty for a mapping carrying
    /// no criteria at all.
    patterns: &'static [&'static str],
    /// The key and the change of each write, in the order they are made.
    writes: &'static [(&'static str, Write)],
    /// The records the handler must be handed, as `(eventName, key)`; the LAST of them is
    /// produced by a write made after every withheld one.
    delivered: &'static [(&'static str, &'static str)],
    /// The records no invocation may be handed, as `(eventName, key)`.
    withheld: &'static [(&'static str, &'static str)],
}

/// The rows: one per shape of criteria.
const ROWS: &[Row] = &[
    // The recorded shape: one clause on the change's own name, admitting the insert and
    // withholding the change made to the same item afterwards.
    Row {
        case: "one-event-name",
        patterns: &[r#"{"eventName":["INSERT"]}"#],
        writes: &[
            ("row-1", Write::Insert),
            ("row-1", Write::Overwrite),
            ("row-2", Write::Insert),
        ],
        delivered: &[(INSERT, "row-1"), (INSERT, "row-2")],
        withheld: &[(MODIFY, "row-1")],
    },
    // Two values in one clause, and a value NESTED inside the record: the changed item's own
    // key, on two patterns that are ORed.
    Row {
        case: "two-patterns-one-of-them-nested",
        patterns: &[
            r#"{"eventName":["MODIFY","REMOVE"]}"#,
            r#"{"dynamodb":{"Keys":{"pk":{"S":["keep"]}}}}"#,
        ],
        writes: &[
            ("drop", Write::Insert),
            ("keep", Write::Insert),
            ("drop", Write::Delete),
        ],
        delivered: &[(INSERT, "keep"), (REMOVE, "drop")],
        withheld: &[(INSERT, "drop")],
    },
    // No criteria at all: every record of the stream is handed over.
    Row {
        case: "no-criteria-admits-every-record",
        patterns: &[],
        writes: &[("row-x", Write::Insert), ("row-x", Write::Overwrite)],
        delivered: &[(INSERT, "row-x"), (MODIFY, "row-x")],
        withheld: &[],
    },
    // A whole batch no pattern matches: nothing is handed over for it, and the position still
    // advances — which the record behind it, delivered afterwards, is the proof of.
    Row {
        case: "a-batch-the-criteria-admit-nothing-of",
        patterns: &[r#"{"eventName":["REMOVE"]}"#],
        writes: &[
            ("row-y", Write::Insert),
            ("row-y", Write::Overwrite),
            ("row-y", Write::Delete),
        ],
        delivered: &[(REMOVE, "row-y")],
        withheld: &[(INSERT, "row-y"), (MODIFY, "row-y")],
    },
];

/// The resources of one row, each named off its position so no row can read or write another's.
fn function_name(index: usize) -> String {
    format!("xsvc-filtered-stream-consumer-{index}")
}
fn table_name(index: usize) -> String {
    format!("xsvc-filtered-stream-source-{index}")
}
fn queue_name(index: usize) -> String {
    format!("xsvc-filtered-stream-sink-{index}")
}

/// The one execution role every function below runs as.
const ROLE: &str = "xsvc-filtered-stream-role";

/// One DynamoDB call (`awsJson1_0`, `DynamoDB_20120810.<Operation>`) that must answer 200.
async fn ddb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("authorization", authorization("dynamodb", REGION))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{DYNAMODB_TARGET_PREFIX}.{operation}"),
        )
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from DynamoDB {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Amazon SQS call (`awsJson1_0`, `AmazonSQS.<Operation>`) that must answer 200.
async fn sqs(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("sqs.{REGION}.amazonaws.com"))
        .header("authorization", authorization("sqs", REGION))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("AmazonSQS.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from Amazon SQS {operation}, got {status} with {parsed}"
    );
    parsed
}

/// One Lambda `restJson1` call, as its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<&Value>,
) -> (u16, Value) {
    let payload = body.map(Value::to_string).unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda", REGION))
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One CloudFormation `awsQuery` call, as its status and XML body.
async fn cloudformation(
    router: &MultiServiceRouter,
    action: &str,
    members: &[(&str, &str)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-15");
    for (name, value) in members {
        body.push('&');
        body.push_str(&form_encode(name));
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("cloudformation.{REGION}.amazonaws.com"))
        .header("authorization", authorization("cloudformation", REGION))
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Bytes::from(body))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    (
        status,
        String::from_utf8_lossy(response.body()).to_string(),
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

/// The inline document the execution role carries: the log actions every execution role needs,
/// the stream reads a DynamoDB-stream mapping's poller makes, and the one send the handler
/// makes.
///
/// The actions a DynamoDB-stream poller is decided on are the stream's own three
/// (`DescribeStream`, `GetShardIterator`, `GetRecords`), with the account-wide `ListStreams`
/// beside them as AWS's own managed execution-role policy for that source
/// (`AWSLambdaDynamoDBExecutionRole`) writes it.
fn role_document() -> Value {
    json!({
        "Version": "2012-10-17",
        "Statement": [
            {
                "Effect": "Allow",
                "Action": [
                    "logs:CreateLogGroup",
                    "logs:CreateLogStream",
                    "logs:PutLogEvents",
                ],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": [
                    "dynamodb:DescribeStream",
                    "dynamodb:GetShardIterator",
                    "dynamodb:GetRecords",
                    "dynamodb:ListStreams",
                ],
                "Resource": "*",
            },
            {
                "Effect": "Allow",
                "Action": ["sqs:SendMessage"],
                "Resource": "*",
            },
        ],
    })
}

/// The template the suite deploys: the one execution role, and one forwarding function per row
/// told the queue that row reads its verdict off. The inline source is archived into the
/// deployment package by CloudFormation, which is how a function whose handler is written here
/// comes to exist at all.
fn stack_template(queue_urls: &[String]) -> String {
    let mut resources = serde_json::Map::new();
    resources.insert(
        "Role".to_string(),
        json!({
            "Type": "AWS::IAM::Role",
            "Properties": {
                "RoleName": ROLE,
                "AssumeRolePolicyDocument": trust_policy(),
                "Policies": [{
                    "PolicyName": "consume-and-forward",
                    "PolicyDocument": role_document(),
                }],
            },
        }),
    );
    for (index, url) in queue_urls.iter().enumerate() {
        resources.insert(
            format!("Consumer{index}"),
            json!({
                "Type": "AWS::Lambda::Function",
                "Properties": {
                    "FunctionName": function_name(index),
                    "Runtime": "python3.12",
                    "Handler": "index.handler",
                    "Role": {"Fn::GetAtt": ["Role", "Arn"]},
                    "Timeout": FUNCTION_TIMEOUT,
                    "Environment": {"Variables": {QUEUE_VARIABLE: url}},
                    "Code": {"ZipFile": FORWARDING_HANDLER},
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

/// Deploy the stack and wait for `CREATE_COMPLETE`, reporting what failed when it does not
/// reach it.
async fn deploy(router: &MultiServiceRouter, queue_urls: &[String]) {
    let template = stack_template(queue_urls);
    let (status, answered) = cloudformation(
        router,
        "CreateStack",
        &[
            ("StackName", STACK),
            ("TemplateBody", &template),
            // The template declares a named IAM role, which a create acknowledges before
            // CloudFormation provisions it.
            ("Capabilities.member.1", "CAPABILITY_NAMED_IAM"),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "setup: CreateStack({STACK:?}) must be accepted, got {status} with {answered}"
    );
    let (reported, described) = wait_for_stack_status(router, STACK, CREATE_COMPLETE).await;
    let failures = if reported == CREATE_COMPLETE {
        Vec::new()
    } else {
        let (_, events) =
            cloudformation(router, "DescribeStackEvents", &[("StackName", STACK)]).await;
        elements(&events, "ResourceStatusReason")
    };
    assert_eq!(
        reported, CREATE_COMPLETE,
        "setup: the stack must reach {CREATE_COMPLETE}, got {described} with resource failures \
         {failures:?}"
    );
}

/// Wait until Lambda reports `name` `Active`, which is the state an invocation needs.
async fn wait_until_active(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (_, configuration) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            None,
        )
        .await;
        if configuration["State"].as_str() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: Lambda never reported {name:?} Active, last read {configuration}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The queue one row reads its verdict off, answered as its url.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let created = sqs(router, case, "CreateQueue", &json!({"QueueName": name})).await;
    created["QueueUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the created queue reports no url, got {created}"))
        .to_string()
}

/// One row's stream-enabled source table, waited out to `ACTIVE`, answered as the ARN of the
/// stream it minted.
async fn given_table(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    ddb(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": name,
            "AttributeDefinitions": [{"AttributeName": KEY, "AttributeType": "S"}],
            "KeySchema": [{"AttributeName": KEY, "KeyType": "HASH"}],
            "BillingMode": "PAY_PER_REQUEST",
            "StreamSpecification": {"StreamEnabled": true, "StreamViewType": STREAM_VIEW_TYPE},
        }),
    )
    .await;
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = ddb(router, case, "DescribeTable", &json!({"TableName": name})).await;
        let active = described["Table"]["TableStatus"] == json!("ACTIVE");
        if let (true, Some(arn)) = (active, described["Table"]["LatestStreamArn"].as_str()) {
            return arn.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table {name:?} never reached ACTIVE with a stream, last read \
             {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Make one write of a row against `table`.
async fn write_item(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    key: &str,
    write: Write,
) {
    match write {
        Write::Insert => {
            ddb(
                router,
                case,
                "PutItem",
                &json!({
                    "TableName": table,
                    "Item": {KEY: {"S": key}, VALUE: {"S": "first"}},
                }),
            )
            .await;
        }
        Write::Overwrite => {
            ddb(
                router,
                case,
                "PutItem",
                &json!({
                    "TableName": table,
                    "Item": {KEY: {"S": key}, VALUE: {"S": "second"}},
                }),
            )
            .await;
        }
        Write::Delete => {
            ddb(
                router,
                case,
                "DeleteItem",
                &json!({"TableName": table, "Key": {KEY: {"S": key}}}),
            )
            .await;
        }
    }
}

/// One row's mapping over `stream_arn`, carrying that row's patterns when it has any, polled
/// until Lambda reports it `Enabled`.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    patterns: &[&str],
) {
    let mut request = json!({
        "FunctionName": function,
        "EventSourceArn": stream_arn,
        "StartingPosition": STARTING_POSITION,
        "BatchSize": BATCH_SIZE,
        "MaximumBatchingWindowInSeconds": BATCHING_WINDOW_SECONDS,
        "Enabled": true,
    });
    if !patterns.is_empty() {
        request["FilterCriteria"] = json!({
            "Filters": patterns
                .iter()
                .map(|pattern| json!({"Pattern": pattern}))
                .collect::<Vec<Value>>(),
        });
    }
    let (status, created) = lambda(router, "POST", MAPPINGS_URI, Some(&request)).await;
    assert_eq!(
        status, 202,
        "case {case:?}: expected HTTP 202 from CreateEventSourceMapping, got {status} with \
         {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the created mapping reports no UUID, got {created}")
        })
        .to_string();
    let deadline = Instant::now() + DEADLINE;
    loop {
        let (status, read) = lambda(router, "GET", &format!("{MAPPINGS_URI}/{uuid}"), None).await;
        if status == 200 && read["State"].as_str() == Some("Enabled") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping {uuid} never reported Enabled, last read {status} {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// How this suite names one delivered record: the change it reported and the key it changed.
fn marker(event_name: &str, key: &str) -> String {
    format!("{event_name}#{key}")
}

/// Everything waiting on `url` right now, as the markers the handler forwarded, each message
/// deleted so a later look finds only later ones.
async fn take_markers(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let received = sqs(
        router,
        case,
        "ReceiveMessage",
        &json!({"QueueUrl": url, "MaxNumberOfMessages": 10}),
    )
    .await;
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut markers = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default().to_string();
        sqs(
            router,
            case,
            "DeleteMessage",
            &json!({
                "QueueUrl": url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        )
        .await;
        let forwarded: Value = serde_json::from_str(&body).unwrap_or_else(|error| {
            panic!("case {case:?}: expected a forwarded record as JSON, got {body:?} ({error})")
        });
        markers.push(marker(
            forwarded["eventName"].as_str().unwrap_or_default(),
            forwarded["pk"].as_str().unwrap_or_default(),
        ));
    }
    markers
}

/// Look at `url` again until every marker of `expected` has arrived, answering every marker
/// seen in the order they arrived.
async fn awaited_markers(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    expected: &[String],
) -> Vec<String> {
    let deadline = Instant::now() + DEADLINE;
    let mut seen: Vec<String> = Vec::new();
    loop {
        seen.extend(take_markers(router, case, url).await);
        if expected.iter().all(|marker| seen.contains(marker)) {
            return seen;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the handler to be handed {expected:?}, it forwarded {seen:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: of the records a DynamoDB-stream mapping polls on the assembled emulator, exactly
/// those at least one of its `FilterCriteria` patterns matches are handed to its function; the
/// rest are discarded without an invocation while the position advances past them.
#[tokio::test]
async fn filter_criteria_decide_which_stream_records_reach_the_function() {
    let router = emulator();

    // Every row's queue exists before the stack, because each function is told the url of the
    // queue its row reads.
    let mut queue_urls: Vec<String> = Vec::with_capacity(ROWS.len());
    for (index, row) in ROWS.iter().enumerate() {
        queue_urls.push(given_queue(&router, row.case, &queue_name(index)).await);
    }
    deploy(&router, &queue_urls).await;

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let function = function_name(index);
        let table = table_name(index);
        wait_until_active(&router, case, &function).await;
        let stream_arn = given_table(&router, case, &table).await;

        // Every write is made BEFORE the mapping exists, which is the recorded shape: a
        // mapping reading from the oldest retained record reads the backlog, so the filter
        // decides records that are already in the shard.
        for (key, write) in row.writes {
            write_item(&router, case, &table, key, *write).await;
        }
        given_mapping(&router, case, &function, &stream_arn, row.patterns).await;

        let expected: Vec<String> = row
            .delivered
            .iter()
            .map(|(event_name, key)| marker(event_name, key))
            .collect();
        let seen = awaited_markers(&router, case, &queue_urls[index], &expected).await;
        for (event_name, key) in row.withheld {
            let withheld = marker(event_name, key);
            assert!(
                !seen.contains(&withheld),
                "case {case:?}: no pattern of {:?} matches the {event_name} record of {key:?}, \
                 so no invocation may be handed it, but the handler forwarded {seen:?}",
                row.patterns
            );
        }
    }
}
