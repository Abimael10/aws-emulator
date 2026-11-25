// SPDX-License-Identifier: CC0-1.0

//! What a client READS off the destination a Kinesis-sourced EventBridge Pipe's
//! `DeadLetterConfig` names, once the pipe has given up on a batch.
//!
//! Four crates have to agree for the rule to hold — EventBridge Pipes runs the pipe, Kinesis
//! Data Streams holds the records it reads with no client action, Lambda answers the
//! enrichment call the pipe makes under its role, and Amazon SQS holds both the target the
//! batch never reaches and the destination the record of it arrives on — so the suite lives
//! here rather than in any one of them. Every call goes through
//! `MultiServiceRouter::dispatch`, the one path a socket-served request takes.
//!
//! RULE: when a stream-sourced pipe has spent the retries its source parameters allow for a
//! batch, it discards the batch — nothing of it reaches the target — and writes exactly ONE
//! record to the destination its `DeadLetterConfig.Arn` names: a JSON document whose
//! `context` object names the resource that gave up (`partnerResourceArn`, the pipe's own
//! ARN as its record reports it) and the condition it gave up under (`condition`), and
//! carries no other member, beside the envelope's `version` and the pointer bounding the
//! discarded records under the member of the source kind they were read from — never the
//! records' own payloads.
//!
//! The rows differ along what the rule generalises over: how many records the discarded
//! batch holds, the `BatchSize` they were polled under and how many retries the pipe allowed
//! before giving up. The `context` object is the same two members in every row, which is
//! what says neither the stage that failed nor the attempts the batch cost reaches it.
//!
//! The member names of the configuration are the pipes model's own
//! (`smithy_query(query="shape", shape="PipeSourceKinesisStreamParameters,DeadLetterConfig")`);
//! the document's own members are recorded behaviour, which the model states nothing about.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for, which is what each handler reads its Region
/// from.
const REGION: &str = "us-east-1";

/// The role every pipe below names. Nothing here turns on its policies: the engine carries no
/// IAM store, which says nothing about what the role may do.
const ROLE: &str = "arn:aws:iam::000000000000:role/pipes-kinesis-dead-letter";

/// The `CurrentState` a pipe that is reading its source rests in.
const RUNNING: &str = "RUNNING";

/// What the record of a discarded batch reports it was discarded FOR.
const RETRIES_EXHAUSTED: &str = "RetryAttemptsExhausted";

/// The member a dead letter points at a discarded KINESIS batch under, and the one a
/// DynamoDB-stream batch would travel under: a document naming the second describes no batch
/// read off a data stream.
const KINESIS_BATCH_INFO: &str = "KinesisBatchInfo";
/// See [`KINESIS_BATCH_INFO`].
const DYNAMODB_BATCH_INFO: &str = "DDBStreamBatchInfo";

/// How long work nothing asked for — the stream becoming readable, the pipe settling, the
/// attempts at a batch being spent, the record of it landing — is given before a row is a
/// failure. Far above the poll interval, the state-settle span and the backoff between
/// attempts, so a row fails only when the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(90);

/// How long a row watches a queue that must stay EMPTY — the target a discarded batch never
/// reaches, and the destination after its one record has arrived. A second record, or a
/// delivery, that was going to happen would have: it is many times the poll interval and the
/// backoff between attempts at one batch.
const QUIET: Duration = Duration::from_secs(5);

/// How often a queue or a pipe is looked at again while waiting for a condition.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// Pipes, Kinesis Data Streams, Lambda and Amazon SQS on ONE engine, exactly as
/// `core/router/src/lib.rs` mounts them. Request authentication is off, as in every other
/// suite here.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_pipes_pipes::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
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
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/aws4_request, \
         SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000"
    )
}

/// One `awsJson` RPC call against the service whose signing name is `signing_name`.
async fn rpc(
    router: &MultiServiceRouter,
    signing_name: &str,
    target_prefix: &str,
    operation: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .header("authorization", authorization(signing_name))
        .body(Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload = response.into_body();
    let parsed = if payload.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice::<Value>(&payload).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// One Kinesis Data Streams call.
async fn kinesis(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "kinesis", "Kinesis_20131202", operation, body).await
}

/// One Amazon SQS call.
async fn sqs(router: &MultiServiceRouter, operation: &str, body: &Value) -> (u16, Value) {
    rpc(router, "sqs", "AmazonSQS", operation, body).await
}

/// One EventBridge Pipes call over its `restJson1` wire, on the URI the operation's `@http`
/// binding declares.
async fn pipes(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|body| Bytes::from(body.to_string()))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", format!("pipes.{REGION}.amazonaws.com"))
        .header("content-type", "application/json")
        .header("authorization", authorization("pipes"))
        .body(payload)
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, parsed)
}

/// A single-shard stream called `name`, waited out until it reports ACTIVE; answers its ARN.
async fn given_stream(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let (status, created) = kinesis(
        router,
        "CreateStream",
        &json!({ "StreamName": name, "ShardCount": 1 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateStream answered {status} {created}"
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, described) = kinesis(
            router,
            "DescribeStreamSummary",
            &json!({ "StreamName": name }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStreamSummary answered {status} {described}"
        );
        let summary = &described["StreamDescriptionSummary"];
        let arn = summary["StreamARN"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        if summary["StreamStatus"].as_str() == Some("ACTIVE") && !arn.is_empty() {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the stream to reach ACTIVE with an ARN, got {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// A queue called `name`; answers its url and the ARN a pipe names it by.
async fn given_queue(router: &MultiServiceRouter, case: &str, name: &str) -> (String, String) {
    let (status, created) = sqs(router, "CreateQueue", &json!({ "QueueName": name })).await;
    assert_eq!(
        status, 200,
        "case {case:?}: CreateQueue answered {status} {created}"
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

/// Put one record carrying `payload` on `stream`; answers the sequence number the stream
/// named it by, which is what a dead letter's pointer has to bound.
async fn put_record(
    router: &MultiServiceRouter,
    stream: &str,
    case: &str,
    payload: &str,
) -> String {
    let (status, answered) = kinesis(
        router,
        "PutRecord",
        &json!({
            "StreamARN": stream,
            // A record's `Data` is bytes, which travel base64-encoded on the wire.
            "Data": aws_smithy_types::base64::encode(payload.as_bytes()),
            "PartitionKey": "orders",
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRecord answered {status} {answered}"
    );
    answered["SequenceNumber"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no SequenceNumber in {answered}"))
        .to_string()
}

/// Wait until the pipe `name` reports it RUNS, which is the state it reads its source in;
/// answers the ARN the pipe's own record reports for it.
async fn running_pipe(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, described) = pipes(router, "GET", &format!("/v1/pipes/{name}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe answered {status} {described}"
        );
        if described["CurrentState"].as_str() == Some(RUNNING) {
            let arn = described["Arn"].as_str().unwrap_or_default().to_string();
            assert!(
                !arn.is_empty(),
                "case {case:?}: a running pipe must report its own ARN, got {described}"
            );
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pipe to reach {RUNNING}, got {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Receive everything waiting on the queue at `url` and delete each message, so a later call
/// hands over what is behind it.
async fn drain(router: &MultiServiceRouter, url: &str, case: &str) -> Vec<String> {
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
    let mut taken: Vec<String> = Vec::new();
    for message in received["Messages"].as_array().cloned().unwrap_or_default() {
        taken.push(message["Body"].as_str().unwrap_or_default().to_string());
        if let Some(handle) = message["ReceiptHandle"].as_str() {
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
    }
    taken
}

/// Everything that arrives on the queue at `url` until there are `want` messages or the
/// budget lapses.
async fn collect_at_least(
    router: &MultiServiceRouter,
    url: &str,
    case: &str,
    want: usize,
) -> Vec<String> {
    let deadline = Instant::now() + BUDGET;
    let mut collected: Vec<String> = Vec::new();
    loop {
        collected.extend(drain(router, url, case).await);
        if collected.len() >= want || Instant::now() >= deadline {
            return collected;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Everything that arrives on the queue at `url` during the whole of [`QUIET`] — what makes
/// the absence of a message an answer rather than a race.
async fn nothing_more(router: &MultiServiceRouter, url: &str, case: &str) -> Vec<String> {
    let deadline = Instant::now() + QUIET;
    let mut collected: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        collected.extend(drain(router, url, case).await);
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    collected
}

/// One row of the dead-letter-document table.
struct Row {
    case: &'static str,
    /// The payloads the row puts on its stream, in put order: the records the pipe then gives
    /// up on as ONE batch.
    payloads: &'static [&'static str],
    /// `BatchSize`: how many records one poll takes. Every row polls its whole run of records
    /// in one batch, so one batch is given up on and one record of it is written.
    batch_size: usize,
    /// `MaximumRetryAttempts`: the attempts the pipe allows BEYOND the first.
    retries: i64,
}

/// RULE: a Kinesis-sourced pipe that has spent the retries its source parameters allow for a
/// batch discards it — nothing of it reaches the target — and writes exactly one record to
/// the destination its `DeadLetterConfig.Arn` names, whose `context` names the pipe that gave
/// up and the condition it gave up under and nothing else, and whose pointer bounds the
/// discarded records under the member of the source kind they were read from.
///
/// The rows differ along the records the discarded batch holds, the `BatchSize` they were
/// polled under and the retries the pipe allowed, which is what the rule generalises over.
#[tokio::test]
async fn a_discarded_kinesis_batch_is_documented_once_on_the_configured_destination() {
    let rows: [Row; 3] = [
        Row {
            // The shape the recorded client drives: one record, given up on at the first
            // failure.
            case: "one-record-discarded-at-once",
            payloads: &[r#"{"order": 1}"#],
            batch_size: 1,
            retries: 0,
        },
        Row {
            // A run of records polled together: the pointer bounds the whole run rather than
            // its first record.
            case: "three-records-in-one-batch-after-two-retries",
            payloads: &[r#"{"order": 1}"#, r#"{"order": 2}"#, r#"{"order": 3}"#],
            batch_size: 10,
            retries: 2,
        },
        Row {
            // A batch size that is exactly the run put, with one retry allowed.
            case: "two-records-under-a-batch-size-of-two",
            payloads: &[r#"{"order": 1}"#, r#"{"order": 2}"#],
            batch_size: 2,
            retries: 1,
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();
        let stream = given_stream(&router, case, case).await;
        let (dlq_url, dlq_arn) = given_queue(&router, case, &format!("{case}-dlq")).await;
        let (target_url, target_arn) = given_queue(&router, case, &format!("{case}-target")).await;

        // The records are put BEFORE the pipe is created, so the pipe reads its whole run
        // from the oldest record of the stream as one batch.
        let mut sequence_numbers: Vec<String> = Vec::new();
        for payload in row.payloads {
            sequence_numbers.push(put_record(&router, &stream, case, payload).await);
        }

        // An enrichment the account holds no function at: every attempt at the batch fails at
        // the enrichment stage, so the retries the row allows are the attempts it gets.
        let enrichment = format!(
            "arn:aws:lambda:{REGION}:{}:function:{case}-never-created",
            ::engine::emulator_account_id()
        );
        let (status, created) = pipes(
            &router,
            "POST",
            &format!("/v1/pipes/{case}"),
            Some(json!({
                "Source": stream,
                "Target": target_arn,
                "Enrichment": enrichment,
                "RoleArn": ROLE,
                "DesiredState": RUNNING,
                "SourceParameters": {
                    "KinesisStreamParameters": {
                        "StartingPosition": "TRIM_HORIZON",
                        "BatchSize": row.batch_size,
                        "MaximumRetryAttempts": row.retries,
                        "DeadLetterConfig": { "Arn": dlq_arn },
                    },
                },
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreatePipe answered {status} {created}"
        );
        let pipe_arn = running_pipe(&router, case, case).await;

        let bodies = collect_at_least(&router, &dlq_url, case, 1).await;
        assert_eq!(
            bodies.len(),
            1,
            "case {case:?}: expected the discarded batch to be recorded once, got {bodies:?}"
        );
        let letter: Value = serde_json::from_str(&bodies[0]).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: a dead letter is a JSON document ({error}): {:?}",
                bodies[0]
            )
        });

        // The WHOLE of the context object: the resource that gave up, as the pipe's own
        // record reports its ARN, and the condition it gave up under — and nothing beside
        // them, however many retries the row allowed and whatever stage failed.
        assert_eq!(
            letter["context"],
            json!({ "partnerResourceArn": pipe_arn, "condition": RETRIES_EXHAUSTED }),
            "case {case:?}: expected the pipe that gave up and why, got {letter}"
        );
        assert_eq!(
            letter["version"],
            json!("1.0"),
            "case {case:?}: expected the envelope's version, got {letter}"
        );
        assert!(
            letter.get(DYNAMODB_BATCH_INFO).is_none(),
            "case {case:?}: a Kinesis batch is not described as a DynamoDB-stream one, got \
             {letter}"
        );
        let pointer = &letter[KINESIS_BATCH_INFO];
        assert_eq!(
            pointer["streamArn"],
            json!(stream),
            "case {case:?}: expected the stream the batch was read off, got {letter}"
        );
        assert_eq!(
            pointer["batchSize"],
            json!(row.payloads.len()),
            "case {case:?}: expected the discarded batch's size, got {letter}"
        );
        assert_eq!(
            pointer["startSequenceNumber"],
            json!(sequence_numbers.first()),
            "case {case:?}: expected the first discarded record's sequence number, got {letter}"
        );
        assert_eq!(
            pointer["endSequenceNumber"],
            json!(sequence_numbers.last()),
            "case {case:?}: expected the last discarded record's sequence number, got {letter}"
        );
        assert!(
            pointer["shardId"]
                .as_str()
                .is_some_and(|shard| shard.starts_with("shardId-")),
            "case {case:?}: expected the shard the batch was read from, got {letter}"
        );
        for payload in row.payloads {
            assert!(
                !letter.to_string().contains(payload),
                "case {case:?}: a dead letter points at the batch and carries no record \
                 payload, got {letter}"
            );
        }

        // The batch reached neither the target nor the destination a second time: the pipe
        // gave up on it once and moved its source past it.
        let delivered = nothing_more(&router, &target_url, case).await;
        assert!(
            delivered.is_empty(),
            "case {case:?}: a discarded batch is delivered nowhere, got {delivered:?}"
        );
        let again = nothing_more(&router, &dlq_url, case).await;
        assert!(
            again.is_empty(),
            "case {case:?}: a discarded batch is recorded once, got {again:?}"
        );
    }
}
