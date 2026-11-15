// SPDX-License-Identifier: CC0-1.0

//! What a mapping on an Amazon DynamoDB table's STREAM does with a batch its function
//! ERRORED on when `BisectBatchOnFunctionError` asks for that batch to be split, end to end
//! over the wire with DynamoDB, DynamoDB Streams and IAM mounted beside Lambda on ONE engine
//! — so the items are written where a client writes them, the shard is read as the function's
//! own execution role, and every effect this suite asserts is read back where a client reads
//! it.
//!
//! The RULE asserted over the table below: "BisectBatchOnFunctionError – If the function
//! returns an error, split the batch in two before retrying" — so a mapping that asks for it
//! attempts HALF of the batch its function errored on next, and halves again while the half
//! it attempts is errored on too, down to the single record. The record the function cannot
//! process therefore ends up handed over ALONE, and every other record of the batch is
//! processed; a mapping that does NOT ask for the split gives up on the whole batch at once,
//! so the records that shared it with the refused one are never processed and the function is
//! never handed a batch of one.
//!
//! Every verdict is a consequence of the handler having been invoked with the batch: the
//! function's package RUNS, it records the keys of the batch it was handed before it decides
//! anything, refuses the batch holding the refused record, and otherwise writes one item per
//! record through the AWS SDK its runtime supplies. So the batches table is the account of
//! what Lambda handed over, and the results table the account of what was processed.
//!
//! The rows differ along what the rule generalises over: whether the mapping asks for the
//! split, where in the batch the refused record sits (its head, its middle), and how many
//! records the batch holds (two, four).
//!
//! Nothing here waits a span of its own: every wait looks again until what it waits for is
//! there or a generous deadline passes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role, logs_only, DYNAMODB_READS};
use crate::synchronous_invoke_run::package;

/// The Region every call below signs for, and the Region every ARN carries.
const REGION: &str = "us-east-1";

/// Where `CreateFunction` is bound, and where `CreateEventSourceMapping` is.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
/// See [`FUNCTIONS_URI`].
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// The `X-Amz-Target` prefix of DynamoDB (its own `OPERATIONS.md`, "Wire protocol").
const DYNAMODB_TARGET_PREFIX: &str = "DynamoDB_20120810";

/// The runtime every function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation of this handler is given, as the function's own `Timeout`: well
/// past what an interpreted run needs, because what is under test is the delivery.
const FUNCTION_TIMEOUT: u64 = 30;

/// The `StartingPosition` that reads the records already sitting in the stream: "Required for
/// Amazon Kinesis and Amazon DynamoDB Stream event sources"
/// (`CreateEventSourceMappingInput$StartingPosition`).
const TRIM_HORIZON: &str = "TRIM_HORIZON";

/// What the stream of every table below captures for each change
/// (`smithy_query(query="shape", shape="StreamViewType")`, DynamoDB), which is the view a
/// handler reading the new image of a change needs.
const NEW_AND_OLD_IMAGES: &str = "NEW_AND_OLD_IMAGES";

/// The attribute every table below is keyed on, and the attribute the handler reads a
/// record's verdict out of. Neither is read by this emulator: both are the handler's own.
const KEY: &str = "pk";
/// See [`KEY`].
const VERDICT: &str = "fail";

/// How many retries every mapping below allows: none, so one attempt is all a batch gets and
/// the record the handler refuses is given up on as soon as it is handed over alone
/// ("Discard records after the specified number of retries", `MaximumRetryAttempts`).
const RETRIES: i64 = 0;

/// How long a row waits for work the clock owes — a table settling, a mapping settling, the
/// batches being read and split, and the records being processed.
///
/// A ceiling on PATIENCE, never a span anything counts on: every wait looks again until what
/// it waits for is there or this deadline passes. It is generous because a row drives several
/// interpreted runs of a real package — one per batch the split hands over — and how fast
/// those run is the host's business, not this suite's.
const BUDGET: Duration = Duration::from_secs(600);

/// How long between two looks while waiting.
const LOOK_AGAIN: Duration = Duration::from_millis(50);

/// One row: whether the mapping asks for a batch its function errored on to be split, and
/// the records written to the table it reads — the key each is written under, and whether the
/// handler refuses the record of that write.
struct Row {
    case: &'static str,
    /// A name no other row uses, short enough for `FunctionName` and `RoleName`.
    name: &'static str,
    /// The `BisectBatchOnFunctionError` the mapping states.
    bisects: bool,
    /// The items written to the source table, in order.
    records: &'static [(&'static str, bool)],
}

impl Row {
    /// The key of the record the handler refuses.
    fn refused(&self) -> &'static str {
        self.records
            .iter()
            .find(|(_, refused)| *refused)
            .map(|(key, _)| *key)
            .unwrap_or_else(|| panic!("case {:?}: a row states one refused record", self.case))
    }

    /// The keys the handler processes without refusing anything.
    fn processed(&self) -> Vec<&'static str> {
        self.records
            .iter()
            .filter(|(_, refused)| !*refused)
            .map(|(key, _)| *key)
            .collect()
    }

    /// How many records one batch of this mapping holds: every record this row writes, so the
    /// first batch read holds the refused record beside the others and the split is what
    /// tells them apart.
    fn batch_size(&self) -> usize {
        self.records.len()
    }
}

/// The rows: the split asked for with the refused record in the middle of four — two
/// halvings, four to two and two to one — and at the head of two, and, as the control the
/// rule needs, the same stream read by a mapping that does not ask for the split.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the split asked for, the refused record in the middle of four",
            name: "mid",
            bisects: true,
            records: &[
                ("mid-a", false),
                ("mid-b", false),
                ("mid-c", true),
                ("mid-d", false),
            ],
        },
        Row {
            case: "the split asked for, the refused record at the head of two",
            name: "head",
            bisects: true,
            records: &[("head-a", true), ("head-b", false)],
        },
        Row {
            case: "the split not asked for, the refused record in the middle of three",
            name: "whole",
            bisects: false,
            records: &[("whole-a", false), ("whole-b", true), ("whole-c", false)],
        },
    ]
}

/// The handler every function below runs: it records the keys of the batch it was handed
/// BEFORE it decides anything about it, RAISES when the batch holds a record whose verdict
/// attribute says to, and otherwise writes one item per record of the batch into the results
/// table through the SDK the runtime supplies.
///
/// So an item in the batches table is evidence of a batch Lambda handed over, whatever became
/// of it, and an item in the results table is evidence that the record was processed.
fn handler_source() -> String {
    format!(
        r#"
import os

import boto3

BATCHES = os.environ["BATCHES_TABLE"]
RESULTS = os.environ["RESULTS_TABLE"]
CLIENT = boto3.client("dynamodb")


def lambda_handler(event, context):
    keys = []
    refused = []
    for record in event["Records"]:
        if record["eventSource"] != "aws:dynamodb":
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        image = record["dynamodb"]["NewImage"]
        key = image["{KEY}"]["S"]
        keys.append(key)
        if image.get("{VERDICT}", {{}}).get("S") == "true":
            refused.append(key)
    CLIENT.put_item(
        TableName=BATCHES,
        Item={{
            "{KEY}": {{"S": ",".join(keys)}},
            "held": {{"N": str(len(keys))}},
        }},
    )
    if refused:
        raise ValueError("the handler refuses " + ",".join(refused))
    for key in keys:
        CLIENT.put_item(TableName=RESULTS, Item={{"{KEY}": {{"S": key}}}})
    return {{"processed": keys}}
"#
    )
}

/// Lambda, DynamoDB, DynamoDB Streams and IAM on ONE engine: the mapping's event source is a
/// table's stream of this emulator, the role it reads that stream as is the one IAM holds, and
/// the handler's own SDK calls are dispatched back into this same router — the shape
/// `core/router/src/lib.rs` mounts them in.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// A SigV4 credential scope naming `signing_name` in [`REGION`]. Nothing in it is verified:
/// the harness turns request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=emulator-test-key/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host, Signature=not-verified"
    )
}

/// One Lambda call, over its own restJson1 routes.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
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
    let status = response.status().as_u16();
    let body = response.into_body();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One DynamoDB call, over its AWS JSON surface.
async fn dynamodb(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("dynamodb.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header(
            "x-amz-target",
            format!("{DYNAMODB_TARGET_PREFIX}.{operation}"),
        )
        .header("authorization", authorization("dynamodb"))
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

/// A table keyed on [`KEY`], with a stream capturing every change when `streamed`, waited out
/// to `ACTIVE` — "you can perform read and write operations only on an ACTIVE table". Answers
/// the table's ARN and, for a streamed table, its `LatestStreamArn`.
async fn given_table(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    streamed: bool,
) -> (String, String) {
    let mut request = json!({
        "TableName": name,
        "AttributeDefinitions": [{ "AttributeName": KEY, "AttributeType": "S" }],
        "KeySchema": [{ "AttributeName": KEY, "KeyType": "HASH" }],
        "BillingMode": "PAY_PER_REQUEST",
    });
    if streamed {
        request["StreamSpecification"] = json!({
            "StreamEnabled": true,
            "StreamViewType": NEW_AND_OLD_IMAGES,
        });
    }
    let (status, created) = dynamodb(router, "CreateTable", &request).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateTable {name:?} answered {status} {created}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, read) = dynamodb(router, "DescribeTable", &json!({ "TableName": name })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTable {name:?} answered {status} {read}"
        );
        let description = &read["Table"];
        if description["TableStatus"] == json!("ACTIVE") {
            let arn = description["TableArn"]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: no TableArn in {read}"))
                .to_string();
            let stream_arn = description["LatestStreamArn"]
                .as_str()
                .unwrap_or_default()
                .to_string();
            assert_eq!(
                stream_arn.is_empty(),
                !streamed,
                "case {case:?}: a streamed table reports its stream and a plain one does not, \
                 got {read}"
            );
            return (arn, stream_arn);
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table {name:?} never became ACTIVE: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// A function whose package RUNS [`handler_source`], running as `role_arn` and told through
/// its environment which tables to write, waited out to `Active`.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    batches_table: &str,
    results_table: &str,
) {
    let (status, created) = lambda(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Environment": {
                "Variables": {
                    "BATCHES_TABLE": batches_table,
                    "RESULTS_TABLE": results_table,
                },
            },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&handler_source())),
            },
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFunction answered {status} {created}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, read) = lambda(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetFunctionConfiguration answered {status} {read}"
        );
        if read["State"] == json!("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function never became Active: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The mapping between `function` and `stream_arn`, reading the row's whole stream as one
/// batch, allowing [`RETRIES`] retries per batch, and asking for a batch its function errored
/// on to be split when the row does.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    batch_size: usize,
    bisects: bool,
) {
    let (status, created) = lambda(
        router,
        "POST",
        MAPPINGS_URI,
        &json!({
            "FunctionName": function,
            "EventSourceArn": stream_arn,
            "StartingPosition": TRIM_HORIZON,
            "BatchSize": batch_size,
            "Enabled": true,
            "MaximumRetryAttempts": RETRIES,
            "BisectBatchOnFunctionError": bisects,
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping answered {status} {created}"
    );
    assert_eq!(
        created["BisectBatchOnFunctionError"],
        json!(bisects),
        "case {case:?}: the mapping must report the split it was created with, got {created}"
    );
}

/// Write one item keyed by `key` into `table`, marked as a record the handler refuses when
/// `refused`.
async fn put_item(router: &MultiServiceRouter, case: &str, table: &str, key: &str, refused: bool) {
    let (status, written) = dynamodb(
        router,
        "PutItem",
        &json!({
            "TableName": table,
            "Item": {
                KEY: { "S": key },
                VERDICT: { "S": if refused { "true" } else { "false" } },
            },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutItem {key:?} answered {status} {written}"
    );
}

/// Every key `table` holds an item under, read through DynamoDB's own `Scan` the way a client
/// that polls for the effect reads it.
async fn keys_held(router: &MultiServiceRouter, case: &str, table: &str) -> Vec<String> {
    let (status, scanned) = dynamodb(
        router,
        "Scan",
        &json!({ "TableName": table, "ConsistentRead": true }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: Scan {table:?} answered {status} {scanned}"
    );
    let mut keys: Vec<String> = scanned["Items"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|item| item[KEY]["S"].as_str().map(str::to_string))
        .collect();
    keys.sort();
    keys
}

/// Wait until `table` holds an item under every key of `wanted`, answering everything it holds
/// — more than `wanted` when more is there, which is what makes "exactly these" assertable.
async fn awaited_keys(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    wanted: &[&str],
    waiting_for: &str,
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    loop {
        let held = keys_held(router, case, table).await;
        if wanted.iter().all(|key| held.iter().any(|got| got == key)) {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: {waiting_for}, so {table:?} must hold {wanted:?}, got {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// What one row stood up: the tables everything below is asserted against, and what the
/// mapping is created over once the row's writes are in the stream.
struct Wired {
    source_table: String,
    batches_table: String,
    results_table: String,
    stream_arn: String,
    function: String,
}

/// Stand `row` up: its streamed source table, the table its handler records the batches it was
/// handed in, the table it records the records it processed in, the execution role carrying
/// exactly the grants this pipeline needs, and the function.
///
/// The MAPPING is not created here: a row writes its records first and maps the stream
/// afterwards, so the batch the mapping reads first holds every record the row wrote rather
/// than whichever of them had been written by the instant its poller first looked.
async fn wire(router: &MultiServiceRouter, row: &Row) -> Wired {
    let case = row.case;
    let source_table = format!("src-{}", row.name);
    let batches_table = format!("bat-{}", row.name);
    let results_table = format!("res-{}", row.name);
    let function = format!("fn-{}", row.name);
    let (_, stream_arn) = given_table(router, case, &source_table, true).await;
    let (batches_arn, _) = given_table(router, case, &batches_table, false).await;
    let (results_arn, _) = given_table(router, case, &results_table, false).await;
    // Lambda reads the stream acting as the function's execution role, and the handler writes
    // its two tables as itself: the role carries each grant, as a client that wires this
    // pipeline together gives it.
    let role = given_role(
        router,
        case,
        &format!("role-{}", row.name),
        Some(json!([
            logs_only(),
            allow("dynamodb", &DYNAMODB_READS, &stream_arn),
            allow("dynamodb", &["PutItem"], &batches_arn),
            allow("dynamodb", &["PutItem"], &results_arn),
        ])),
    )
    .await;
    given_function(
        router,
        case,
        &function,
        &role,
        &batches_table,
        &results_table,
    )
    .await;
    Wired {
        source_table,
        batches_table,
        results_table,
        stream_arn,
        function,
    }
}

/// RULE: a mapping whose `BisectBatchOnFunctionError` asks for it SPLITS a batch its function
/// errored on in two before attempting it again, halving on while the half it attempts is
/// errored on too, down to the single record — so the record the function cannot process is
/// handed over alone and given up on alone, and every other record of the batch is processed;
/// a mapping that does not ask for the split gives up on the whole batch, so the records that
/// shared it with the refused one are never processed and no batch of one is ever handed over.
#[tokio::test]
async fn a_batch_its_function_errored_on_is_split_where_the_mapping_asks() {
    for row in rows() {
        let case = row.case;
        let router = emulator();
        let wired = wire(&router, &row).await;

        // The only data-plane calls this row makes: the writes a client makes to the table.
        // Nothing below invokes the function.
        for (key, refused) in row.records {
            put_item(&router, case, &wired.source_table, key, *refused).await;
        }

        // The mapping is created over a stream that already holds every record the row wrote,
        // so the batch it reads first is the whole batch its `BatchSize` admits.
        given_mapping(
            &router,
            case,
            &wired.function,
            &wired.stream_arn,
            row.batch_size(),
            row.bisects,
        )
        .await;

        let refused = row.refused();
        let processed = row.processed();
        if !row.bisects {
            // The whole batch is given up on at once, so the batch the function was handed
            // held every record of the stream and no record of it was ever processed.
            let whole: String = row
                .records
                .iter()
                .map(|(key, _)| *key)
                .collect::<Vec<&str>>()
                .join(",");
            let handed = awaited_keys(
                &router,
                case,
                &wired.batches_table,
                &[whole.as_str()],
                "a mapping that does not ask for the split hands the whole batch over",
            )
            .await;
            assert_eq!(
                handed,
                vec![whole.clone()],
                "case {case:?}: a mapping that does not ask for the split must hand over the \
                 one batch {whole:?} and never a part of it, got {handed:?}"
            );
            let held = keys_held(&router, case, &wired.results_table).await;
            assert!(
                held.is_empty(),
                "case {case:?}: the batch holding {refused:?} was given up on whole, so none of \
                 {:?} was processed, got {held:?}",
                row.processed()
            );
            continue;
        }

        // Every record the function CAN process is processed: the halves it was handed them in
        // are the split working.
        let held = awaited_keys(
            &router,
            case,
            &wired.results_table,
            &processed,
            "splitting the batch leaves every record the function can process processed",
        )
        .await;
        let mut expected: Vec<String> = processed.iter().map(|key| (*key).to_string()).collect();
        expected.sort();
        assert_eq!(
            held, expected,
            "case {case:?}: expected the results table to hold exactly {expected:?} — the \
             refused record {refused:?} must leave no effect — got {held:?}"
        );

        // And the refused record was ISOLATED: the function was handed a batch holding that
        // record and nothing else, which is what halving down to the single record comes to.
        let handed = keys_held(&router, case, &wired.batches_table).await;
        assert!(
            handed.iter().any(|batch| batch == refused),
            "case {case:?}: the refused record {refused:?} must be handed over alone, but the \
             batches handed over were {handed:?}"
        );
        // Every batch handed over was a run of the records the stream holds, and the first of
        // them was the whole batch the mapping's `BatchSize` reads: the split follows the
        // error rather than the mapping reading smaller batches from the start.
        let whole: String = row
            .records
            .iter()
            .map(|(key, _)| *key)
            .collect::<Vec<&str>>()
            .join(",");
        assert!(
            handed.contains(&whole),
            "case {case:?}: the mapping must first hand over the whole batch {whole:?} its \
             BatchSize reads, got {handed:?}"
        );
        // Each half is half of what it was split from, so no batch handed over held more
        // records than the mapping's own `BatchSize`.
        for batch in &handed {
            let records = batch.split(',').count();
            assert!(
                records <= row.batch_size(),
                "case {case:?}: no batch holds more than the mapping's BatchSize of {}, got \
                 {batch:?} with {records}",
                row.batch_size()
            );
        }
    }
}
