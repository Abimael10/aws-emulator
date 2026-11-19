// SPDX-License-Identifier: CC0-1.0

//! What a RUNNING EventBridge Pipe whose SOURCE is an Amazon Kinesis data stream MOVES when its
//! `SourceParameters` carry `FilterCriteria` over the record's decoded payload and its
//! `TargetParameters` carry an `InputTemplate`.
//!
//! Three crates have to agree for the rule to hold — EventBridge Pipes runs the pipe, Kinesis Data
//! Streams holds the source the pipe reads with no client action, Amazon SQS holds the target the
//! renders arrive on — so the suite lives here rather than in any one of them. Every call goes
//! through `MultiServiceRouter::dispatch`, the one path a socket-served request takes: the records
//! are put with `PutRecords` and the renders are read off the target queue with `ReceiveMessage`,
//! with nothing in between asked to do anything.
//!
//! RULE: a RUNNING pipe whose `Source` names a Kinesis data stream reads that stream itself and
//! delivers to its target exactly one payload per polled record its `FilterCriteria` admits —
//! where a pattern's clause and an `InputTemplate` placeholder both read the record's `data` as
//! the payload its producer put, base64-decoded and as the JSON document that text spells when it
//! spells one — rendered through that template; a record no pattern admits, including one whose
//! payload spells no document where the pattern reads a field of one, reaches no later stage at
//! all.
//!
//! The rows differ along what the rule generalises over: how many records of one batch are
//! admitted and how many are not, the `BatchSize` the records are polled under (one poll holding
//! the whole batch, or one record per poll), what the pattern's clause reads (a string field, a
//! numeric comparison, two patterns ORed), what the template renders (an object with several
//! placeholders, a bare field, the whole decoded document), why a record is not admitted (its
//! field disagrees, its payload spells no document, its payload spells a bare scalar, the field
//! the clause reads is absent), and whether any record of the batch is admitted at all.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every call below is signed for, which is what each handler reads its Region from.
const REGION: &str = "us-east-1";

/// The role every pipe below names. Nothing here turns on its policies: the engine carries no IAM
/// store, which says nothing about what the role may do.
const ROLE: &str = "arn:aws:iam::000000000000:role/pipes-kinesis-filter";

/// The `CurrentState` a pipe that is reading its source rests in.
const RUNNING: &str = "RUNNING";

/// How long work the pipe has to do on its own is given before a case fails. Far above the data
/// plane's own poll interval and the span a pipe takes to reach RUNNING, so a case fails only when
/// the pipe did not do the work at all.
const BUDGET: Duration = Duration::from_secs(60);

/// How long an assertion that a record is NEVER delivered watches for, after everything the
/// patterns admit has arrived. A delivery that was going to be made would have: it is many times
/// the poller's own interval.
const QUIET: Duration = Duration::from_secs(3);

/// How often a queue, a stream or a pipe is looked at again while waiting for a condition.
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// Pipes, Kinesis Data Streams and Amazon SQS on ONE engine, exactly as `core/router/src/lib.rs`
/// mounts them. Request authentication is off, as in every other suite here.
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
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `service` in [`REGION`], which is
/// what routes the request. The signature is never checked: this router runs with request
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
        let arn = summary["StreamARN"].as_str().unwrap_or_default().to_string();
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

/// Put `records` — (partition key, the bytes the producer puts) pairs — as ONE `PutRecords` batch,
/// so the order they were put in is the order the shard holds them in.
async fn put_records(
    router: &MultiServiceRouter,
    stream: &str,
    case: &str,
    records: &[(&str, &str)],
) {
    let entries: Vec<Value> = records
        .iter()
        .map(|(key, payload)| {
            json!({
                // A record's `Data` is bytes, which travel base64-encoded on the wire.
                "Data": aws_smithy_types::base64::encode(payload.as_bytes()),
                "PartitionKey": key,
            })
        })
        .collect();
    let (status, answered) = kinesis(
        router,
        "PutRecords",
        &json!({ "StreamARN": stream, "Records": entries }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutRecords answered {status} {answered}"
    );
    assert_eq!(
        answered["FailedRecordCount"].as_i64(),
        Some(0),
        "case {case:?}: every record must be stored, got {answered}"
    );
}

/// Wait until the pipe `name` reports it RUNS, which is the state it reads its source in.
async fn wait_until_running(router: &MultiServiceRouter, case: &str, name: &str) {
    let deadline = Instant::now() + BUDGET;
    loop {
        let (status, described) = pipes(router, "GET", &format!("/v1/pipes/{name}"), None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribePipe answered {status} {described}"
        );
        if described["CurrentState"].as_str() == Some(RUNNING) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the pipe to reach {RUNNING}, got {described}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Receive up to ten messages from the queue at `url` and DELETE each one, so a later call hands
/// over what is behind them.
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

/// Every payload delivered to `url` until there are `want` of them or the budget lapses. A case
/// expecting none waits out [`nothing_more`] instead.
async fn delivered(
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

/// Every payload that arrives at `url` during the whole of [`QUIET`] — what makes the absence of a
/// delivery an answer rather than a race.
async fn nothing_more(router: &MultiServiceRouter, url: &str, case: &str) -> Vec<String> {
    let deadline = Instant::now() + QUIET;
    let mut collected: Vec<String> = Vec::new();
    while Instant::now() < deadline {
        collected.extend(drain(router, url, case).await);
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    collected
}

/// One payload in a form two of them can be compared by: the JSON it spells with every object's
/// members in name order, or the text it is when it spells no JSON. A render is asserted on the
/// VALUE it carries and not on the byte order a serializer chose for it.
fn canonical(payload: &str) -> String {
    match serde_json::from_str::<Value>(payload) {
        Ok(value) => ordered(&value).to_string(),
        Err(_) => payload.to_string(),
    }
}

/// `value` with the members of every object it holds in name order.
fn ordered(value: &Value) -> Value {
    match value {
        Value::Object(members) => {
            let mut names: Vec<&String> = members.keys().collect();
            names.sort();
            let mut rebuilt = serde_json::Map::new();
            for name in names {
                rebuilt.insert(name.clone(), ordered(members.get(name).unwrap_or(&Value::Null)));
            }
            Value::Object(rebuilt)
        }
        Value::Array(items) => Value::Array(items.iter().map(ordered).collect()),
        other => other.clone(),
    }
}

/// One record a row puts on the stream, and what the target must receive for it.
struct Record {
    /// The `PartitionKey` the record is put under.
    key: &'static str,
    /// The bytes the producer puts, which travel base64-encoded.
    payload: &'static str,
    /// What the target must receive for it, or `None` for a record the filter does not admit.
    rendered: Option<&'static str>,
}

/// One row of the filtered-delivery table.
struct Row {
    case: &'static str,
    /// The patterns `FilterCriteria.Filters` carries; a record matching ANY of them is admitted.
    patterns: &'static [&'static str],
    /// `TargetParameters.InputTemplate`: what the target is handed instead of the record.
    template: &'static str,
    /// `BatchSize`: how many records one poll takes.
    batch_size: usize,
    /// The records the row puts, in put order.
    records: &'static [Record],
}

/// RULE: a RUNNING pipe whose `Source` names a Kinesis data stream reads that stream itself and
/// delivers to its target exactly one payload per polled record its `FilterCriteria` admits —
/// where a pattern's clause and an `InputTemplate` placeholder both read the record's `data` as
/// the payload its producer put, base64-decoded and as the JSON document that text spells when it
/// spells one — rendered through that template; a record no pattern admits reaches no later stage.
///
/// The rows differ along what the rule generalises over: how many records of one batch are
/// admitted and how many are not, the `BatchSize` they are polled under, what the pattern reads, what
/// the template renders, why a record is not admitted, and whether any record is admitted at all.
#[tokio::test]
async fn a_kinesis_sourced_pipe_delivers_exactly_the_records_its_filter_admits_rendered() {
    let rows = vec![
        Row {
            // The shape the recorded client drives: one batch holding two records the pattern
            // admits and two it does not — one whose field disagrees and one whose payload spells
            // no document at all.
            case: "two-admitted-and-two-dropped-in-one-batch",
            patterns: &[r#"{"data": {"kind": ["keep"]}}"#],
            template: r#"{"t": "<$.data.t>", "k": "<$.partitionKey>"}"#,
            batch_size: 10,
            records: &[
                Record {
                    key: "events",
                    payload: r#"{"kind": "keep", "t": "first"}"#,
                    rendered: Some(r#"{"t": "first", "k": "events"}"#),
                },
                Record {
                    key: "events",
                    payload: r#"{"kind": "drop", "t": "second"}"#,
                    rendered: None,
                },
                Record {
                    key: "events",
                    payload: "not-json-at-all",
                    rendered: None,
                },
                Record {
                    key: "events",
                    payload: r#"{"kind": "keep", "t": "third"}"#,
                    rendered: Some(r#"{"t": "third", "k": "events"}"#),
                },
            ],
        },
        Row {
            // The same records, one record per poll: the decision is each record's own, not the
            // batch's.
            case: "one-record-per-poll",
            patterns: &[r#"{"data": {"kind": ["keep"]}}"#],
            template: "<$.data.t>",
            batch_size: 1,
            records: &[
                Record {
                    key: "events",
                    payload: r#"{"kind": "keep", "t": "alpha"}"#,
                    rendered: Some("alpha"),
                },
                Record {
                    key: "events",
                    payload: r#"{"kind": "drop", "t": "beta"}"#,
                    rendered: None,
                },
                Record {
                    key: "events",
                    payload: "beta-but-not-json",
                    rendered: None,
                },
                Record {
                    key: "events",
                    payload: r#"{"kind": "keep", "t": "gamma"}"#,
                    rendered: Some("gamma"),
                },
            ],
        },
        Row {
            // A numeric comparison over the decoded document, against a payload on the bound, one
            // below it, a payload spelling a bare scalar (which names nothing to descend into) and
            // a document without the field the clause reads.
            case: "a-numeric-clause-over-the-decoded-data",
            patterns: &[r#"{"data": {"amount": [{"numeric": [">=", 10]}]}}"#],
            template: r#"{"amount": <$.data.amount>}"#,
            batch_size: 10,
            records: &[
                Record {
                    key: "payments",
                    payload: r#"{"amount": 5}"#,
                    rendered: None,
                },
                Record {
                    key: "payments",
                    payload: r#"{"amount": 10}"#,
                    rendered: Some(r#"{"amount": 10}"#),
                },
                Record {
                    key: "payments",
                    payload: "12",
                    rendered: None,
                },
                Record {
                    key: "payments",
                    payload: r#"{"other": 99}"#,
                    rendered: None,
                },
                Record {
                    key: "payments",
                    payload: r#"{"amount": 11}"#,
                    rendered: Some(r#"{"amount": 11}"#),
                },
            ],
        },
        Row {
            // Two patterns ORed, and a template that renders the WHOLE decoded document.
            case: "two-patterns-ored-with-the-whole-document-rendered",
            patterns: &[
                r#"{"data": {"kind": ["keep"]}}"#,
                r#"{"data": {"priority": [{"numeric": [">", 5]}]}}"#,
            ],
            template: "<$.data>",
            batch_size: 3,
            records: &[
                Record {
                    key: "mixed",
                    payload: r#"{"kind": "keep"}"#,
                    rendered: Some(r#"{"kind": "keep"}"#),
                },
                Record {
                    key: "mixed",
                    payload: r#"{"priority": 9}"#,
                    rendered: Some(r#"{"priority": 9}"#),
                },
                Record {
                    key: "mixed",
                    payload: r#"{"kind": "drop", "priority": 1}"#,
                    rendered: None,
                },
            ],
        },
        Row {
            // No record of the batch is admitted: the pipe runs, reads and delivers nothing, which
            // is what tells a filter that drops everything from a pipe that never ran.
            case: "no-record-is-admitted",
            patterns: &[r#"{"data": {"kind": ["keep"]}}"#],
            template: "<$.data.kind>",
            batch_size: 10,
            records: &[
                Record {
                    key: "none",
                    payload: r#"{"kind": "drop"}"#,
                    rendered: None,
                },
                Record {
                    key: "none",
                    payload: "plain text",
                    rendered: None,
                },
            ],
        },
    ];

    for row in rows {
        let case = row.case;
        let router = emulator();
        let stream = given_stream(&router, case, case).await;
        let (url, target) = given_queue(&router, case, case).await;

        let filters: Vec<Value> = row
            .patterns
            .iter()
            .map(|pattern| json!({ "Pattern": pattern }))
            .collect();
        let (status, created) = pipes(
            &router,
            "POST",
            &format!("/v1/pipes/{case}"),
            Some(json!({
                "Source": stream,
                "Target": target,
                "RoleArn": ROLE,
                "DesiredState": RUNNING,
                "SourceParameters": {
                    "KinesisStreamParameters": {
                        "StartingPosition": "TRIM_HORIZON",
                        "BatchSize": row.batch_size,
                    },
                    "FilterCriteria": { "Filters": filters },
                },
                "TargetParameters": { "InputTemplate": row.template },
            })),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: CreatePipe must succeed, got {status} {created}"
        );
        wait_until_running(&router, case, case).await;

        let put: Vec<(&str, &str)> = row
            .records
            .iter()
            .map(|record| (record.key, record.payload))
            .collect();
        put_records(&router, &stream, case, &put).await;

        let mut wanted: Vec<String> = row
            .records
            .iter()
            .filter_map(|record| record.rendered)
            .map(canonical)
            .collect();
        wanted.sort();

        let mut bodies = delivered(&router, &url, case, wanted.len()).await;
        // Everything the pipe must deliver has arrived; a record it must DROP would arrive late,
        // so the quiet window is what makes its absence an answer rather than a race.
        bodies.extend(nothing_more(&router, &url, case).await);
        let mut seen: Vec<String> = bodies.iter().map(|body| canonical(body)).collect();
        seen.sort();
        assert_eq!(
            seen, wanted,
            "case {case:?}: expected the target to receive exactly {wanted:?}, got {bodies:?}"
        );
    }
}
