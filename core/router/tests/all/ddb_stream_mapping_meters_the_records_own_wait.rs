// SPDX-License-Identifier: CC0-1.0

//! What the `AWS/Lambda` `IteratorAge` of a mapping on a DynamoDB table's stream is worth to
//! the client reading it, across the four services that own the halves of it: DynamoDB holds
//! the table and mints its stream, DynamoDB Streams holds the stream's records, Lambda polls
//! the shard and meters each batch it takes off it, and CloudWatch answers the series.
//!
//! The RULE pinned here: **the age Lambda publishes for a batch is the span between the WRITE
//! of that batch's newest record and the read that took the batch off the shard** — "Age is the
//! difference between the time Lambda received the batch and the time the last record in the
//! batch was written to the stream" (the AWS Lambda metric reference for `IteratorAge`) — so a
//! datapoint is never larger than the time that has really elapsed since that write, whatever
//! sub-second phase the write fell on, and never smaller than the wait the record already had
//! when the mapping was created.
//!
//! What makes this a rule and not a restatement of the implementation: the stamp a CONSUMER of
//! the stream reads for a record, `StreamRecord.ApproximateCreationDateTime`, is documented as
//! "in UNIX epoch time format and rounded down to the closest second". It therefore names a
//! moment up to a second BEFORE the write, and an age measured from it is up to a second longer
//! than the record ever waited — a lag a client reads as a backlog that is not there. The
//! service that holds the record holds the write instant too, and that is what the age is
//! measured from.
//!
//! Four crates have to agree for that to hold, and the hand-off is made in-process (Lambda's
//! poller reads the shard through DynamoDB Streams' own data plane and publishes through
//! CloudWatch's), so only the assembled emulator can ask it. What the neighbours pin and this
//! one does not repeat: `all/ddb_stream_lambda_esm_provisioning` pins that a stream-enabled
//! table's stream is mappable at all, and the Lambda crate's own
//! `stream_mapping_iterator_age_series` pins WHICH series the age is published as.
//!
//! The rows differ along what the rule generalises over: the sub-second phase the write fell on
//! — which is exactly what rounding a stamp down to the second discards — and how many records
//! the batch holds. One case takes the live wall clock's own phase, which nobody chooses, by
//! writing the item through DynamoDB's own `PutItem`.
//!
//! This suite asserts nothing about the INVOCATION the batch is then handed to: the age is a
//! property of the read, published before anything is handed over, and the deployment package
//! these functions carry is opaque.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810 as streams_crate;
use smithy_server_runtime::MultiServiceRouter;
use streams_crate::ops::types::AttributeValue;
use streams_crate::state::{
    floor_to_second, AttributeMap,
    ComAmazonawsDynamodbstreamsDynamodbstreams20120810State as StreamsState, OperationType,
    RecordAppend,
};

/// The Region every request below is signed for, and so the Region every resource it makes
/// belongs to.
const REGION: &str = "us-west-2";

/// The table the stream belongs to, the function the mapping invokes and the execution role
/// that function reports.
const TABLE: &str = "metered-orders";
const FUNCTION: &str = "metered-consumer";
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/metered-consumer";

/// An opaque deployment package, base64-encoded as `FunctionCode$ZipFile` is on the wire. Its
/// contents are never interpreted here: what is under test is the metering of the READ.
const PACKAGE_ZIP_BASE64: &str = "cGFja2FnZQ==";

/// Where Lambda binds the two collections used below.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";
const MAPPINGS_URI: &str = "/2015-03-31/event-source-mappings";

/// CloudWatch's `X-Amz-Target` prefix and its signing name (`sigv4Name: monitoring`).
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The series the age is read back through, as the AWS Lambda metric reference names it: the
/// namespace, the metric, the unit and the one dimension it is reported per.
const NAMESPACE: &str = "AWS/Lambda";
const ITERATOR_AGE: &str = "IteratorAge";
const MILLISECONDS: &str = "Milliseconds";
const FUNCTION_DIMENSION: &str = "FunctionName";

/// The `Period` every read below is answered at, in seconds, and how far either side of now its
/// window reaches — wide enough that the minute a batch was read in is inside it.
const PERIOD_SECS: i64 = 60;
/// See [`PERIOD_SECS`].
const WINDOW_SECS: f64 = 900.0;

/// The `StreamViewType` the table is created with, and where the mapping starts reading: the
/// oldest record of the shard, so a write made before the mapping's poller first looked is
/// still delivered.
const STREAM_VIEW_TYPE: &str = "NEW_AND_OLD_IMAGES";
const STARTING_POSITION: &str = "TRIM_HORIZON";

/// How long an observation of work the CLOCK owes — a table reaching `ACTIVE`, a mapping
/// settling, a batch being read and metered — waits before calling it a hang, and how long
/// between two looks. Generous: a case fails only when the work did not happen at all, so a
/// loaded host cannot decide the outcome.
const DEADLINE: Duration = Duration::from_secs(120);
/// See [`DEADLINE`].
const LOOK_AGAIN: Duration = Duration::from_millis(25);

/// How long before now the records of a case were written. Several seconds, so every bound
/// below is a span the clock really ran rather than one a fast host could collapse.
const WRITTEN_AGO: Duration = Duration::from_secs(3);

/// The four services this hand-off spans, over ONE engine as `core/router/src/lib.rs` mounts
/// them, with the engine handed back: the records are written at a CHOSEN instant through the
/// seam DynamoDB's own item writes call, which is the only way a case can fix the sub-second
/// phase of a write.
fn emulator() -> (Arc<engine::InMemoryEngine>, MultiServiceRouter) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(streams_crate::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    (engine, router)
}

/// A SigV4 credential scope naming `signing_name` in [`REGION`], which is what routes the
/// request. The signature is never checked: this router runs with request authentication off.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=0000"
    )
}

/// One `awsJson1_0` RPC call, as its status and parsed body.
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
        .header("authorization", authorization(signing_name))
        .header("content-type", "application/x-amz-json-1.0")
        .header("x-amz-target", format!("{target_prefix}.{operation}"))
        .body(Bytes::from(body.to_string()))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A DynamoDB call that must answer 200.
async fn ddb(router: &MultiServiceRouter, case: &str, operation: &str, body: &Value) -> Value {
    let (status, parsed) = rpc(router, "dynamodb", "DynamoDB_20120810", operation, body).await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// A DynamoDB Streams call that must answer 200.
async fn stream_call(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        "dynamodb",
        "DynamoDBStreams_20120810",
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// A CloudWatch call that must answer 200.
async fn cloudwatch(
    router: &MultiServiceRouter,
    case: &str,
    operation: &str,
    body: &Value,
) -> Value {
    let (status, parsed) = rpc(
        router,
        CLOUDWATCH_SIGNING_NAME,
        CLOUDWATCH_TARGET_PREFIX,
        operation,
        body,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {operation} must answer HTTP 200, got {status} with {parsed}"
    );
    parsed
}

/// One `restJson1` call to Lambda, as its status and parsed body.
async fn rest(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        body.to_string().into_bytes()
    };
    let request = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.{REGION}.amazonaws.com{uri}"))
        .header("host", format!("lambda.{REGION}.amazonaws.com"))
        .header("authorization", authorization("lambda"))
        .header("content-type", "application/json")
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let parsed = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// Create the stream-enabled table and answer the stream ARN DynamoDB minted for it, once that
/// table reports `ACTIVE` — the condition the suite waits for, never a span.
async fn given_stream_table(router: &MultiServiceRouter, case: &str) -> String {
    let created = ddb(
        router,
        case,
        "CreateTable",
        &json!({
            "TableName": TABLE,
            "KeySchema": [{"AttributeName": "pk", "KeyType": "HASH"}],
            "AttributeDefinitions": [{"AttributeName": "pk", "AttributeType": "S"}],
            "BillingMode": "PAY_PER_REQUEST",
            "StreamSpecification": {"StreamEnabled": true, "StreamViewType": STREAM_VIEW_TYPE},
        }),
    )
    .await;
    let stream_arn = created["TableDescription"]["LatestStreamArn"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the create must mint a stream, got {created}"))
        .to_string();
    let started = Instant::now();
    let mut last = Value::Null;
    while started.elapsed() < DEADLINE {
        let described = ddb(router, case, "DescribeTable", &json!({"TableName": TABLE})).await;
        if described["Table"]["TableStatus"].as_str() == Some("ACTIVE") {
            return stream_arn;
        }
        last = described;
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!("case {case:?}: the table never reached ACTIVE within {DEADLINE:?}, last saw {last}")
}

/// Create the function the mapping invokes.
async fn given_function(router: &MultiServiceRouter, case: &str) {
    let (status, function) = rest(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Code": {"ZipFile": PACKAGE_ZIP_BASE64},
        }),
    )
    .await;
    assert_eq!(
        status, 201,
        "case {case:?}: CreateFunction must answer HTTP 201, got {status} with {function}"
    );
}

/// Publish one `INSERT` of the item `id` onto `stream_arn` at `at` — the seam DynamoDB's own
/// item writes call, which is where a record's creation instant is stamped — and answer the
/// sequence number the stream gave it.
fn append_at(
    engine: &engine::InMemoryEngine,
    case: &str,
    stream_arn: &str,
    id: &str,
    at: SystemTime,
) -> String {
    let keys: AttributeMap = [("pk".to_string(), AttributeValue::S(id.to_string()))]
        .into_iter()
        .collect();
    engine
        .state::<StreamsState>()
        .lock()
        .append_record(
            stream_arn,
            &RecordAppend {
                event_name: OperationType::Insert,
                keys: keys.clone(),
                new_image: Some(keys),
                old_image: None,
            },
            at,
        )
        .unwrap_or_else(|rejected| {
            panic!("case {case:?}: the stream must take the record, got {rejected}")
        })
}

/// Milliseconds from `from` to `to`, and 0 for a `to` that is not after `from`.
fn millis_between(from: SystemTime, to: SystemTime) -> f64 {
    to.duration_since(from).unwrap_or_default().as_secs_f64() * 1_000.0
}

/// Epoch seconds, as a stored datapoint is stamped and as a query's window is stated.
fn epoch_secs(at: SystemTime) -> f64 {
    at.duration_since(UNIX_EPOCH).unwrap_or_default().as_secs_f64()
}

/// The `IteratorAge` datapoints CloudWatch holds for [`FUNCTION`].
async fn iterator_age_datapoints(router: &MultiServiceRouter, case: &str) -> Vec<Value> {
    let now = epoch_secs(SystemTime::now());
    let answered = cloudwatch(
        router,
        case,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": ITERATOR_AGE,
            "Dimensions": [{ "Name": FUNCTION_DIMENSION, "Value": FUNCTION }],
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Maximum", "Minimum"],
            "Unit": MILLISECONDS,
        }),
    )
    .await;
    answered["Datapoints"].as_array().cloned().unwrap_or_default()
}

/// Wait until the series holds a datapoint, answering what it held and the instant the read
/// that found it was made — the reference the upper bound is taken against, so the bound is
/// what the clock had really run by then.
async fn datapoints_once_published(
    router: &MultiServiceRouter,
    case: &str,
) -> (Vec<Value>, SystemTime) {
    let started = Instant::now();
    while started.elapsed() < DEADLINE {
        let points = iterator_age_datapoints(router, case).await;
        let read_at = SystemTime::now();
        if !points.is_empty() {
            return (points, read_at);
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
    panic!(
        "case {case:?}: {NAMESPACE}/{ITERATOR_AGE} dimensioned on \
         {FUNCTION_DIMENSION}={FUNCTION} held no datapoint within {DEADLINE:?}"
    )
}

/// Create the mapping over `stream_arn`, answering the instant just before the create: every
/// read the poller then makes is after it, so the age of a record written before it is at
/// least the wait that record already had.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    stream_arn: &str,
    batch_size: usize,
) -> SystemTime {
    let before = SystemTime::now();
    let (status, mapping) = rest(
        router,
        "POST",
        MAPPINGS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "EventSourceArn": stream_arn,
            "StartingPosition": STARTING_POSITION,
            "BatchSize": batch_size,
        }),
    )
    .await;
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping must answer HTTP 202, got {status} with \
         {mapping}"
    );
    before
}

/// The sub-second phases a write can fall on, and so the overstatement rounding its stamp down
/// to the second would produce: the second's boundary (none at all), a millisecond in, the
/// middle, and the last whole millisecond before the next second.
const PHASES: &[(&str, Duration)] = &[
    ("on-the-second", Duration::ZERO),
    ("one-millisecond-in", Duration::from_millis(1)),
    ("mid-second", Duration::from_millis(500)),
    ("the-last-millisecond", Duration::from_millis(999)),
];

/// RULE, the half the stream service owns: a record's reported creation stamp is the second its
/// write fell in and nothing finer, while the instant of the WRITE is kept — so a span measured
/// from the stamp is longer than the record's real wait by exactly the phase the write fell on,
/// and a span measured from the kept instant is that wait.
///
/// One table takes one record per phase, so this asserts the whole table over the one stream a
/// client would write them all to.
#[tokio::test]
async fn a_table_stream_record_keeps_the_write_behind_the_second_it_reports() {
    let case = "the record a write of each phase produced";
    let (engine, router) = emulator();
    let stream_arn = given_stream_table(&router, case).await;
    let anchor = floor_to_second(SystemTime::now() - WRITTEN_AGO);

    // One record per phase, each at its own whole second so the stream holds them in a known
    // order, with the instant each was written and the sequence number it got.
    let mut written_records: Vec<(&str, SystemTime, String)> = Vec::new();
    for (index, (phase_case, phase)) in PHASES.iter().enumerate() {
        let written = anchor - Duration::from_secs(PHASES.len() as u64 - index as u64) + *phase;
        let sequence_number =
            append_at(&engine, phase_case, &stream_arn, phase_case, written);
        written_records.push((phase_case, written, sequence_number));
    }

    // What a CONSUMER reads: the whole second each write fell in.
    let described = stream_call(
        &router,
        case,
        "DescribeStream",
        &json!({ "StreamArn": stream_arn }),
    )
    .await;
    let shard_id = described["StreamDescription"]["Shards"][0]["ShardId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no shard in {described}"))
        .to_string();
    let minted = stream_call(
        &router,
        case,
        "GetShardIterator",
        &json!({
            "StreamArn": stream_arn,
            "ShardId": shard_id,
            "ShardIteratorType": "TRIM_HORIZON",
        }),
    )
    .await;
    let page = stream_call(
        &router,
        case,
        "GetRecords",
        &json!({ "ShardIterator": minted["ShardIterator"] }),
    )
    .await;
    let read = page["Records"].as_array().cloned().unwrap_or_default();

    for ((phase_case, written, sequence_number), phase) in written_records
        .iter()
        .zip(PHASES.iter().map(|(_, phase)| *phase))
    {
        let record = read
            .iter()
            .find(|record| {
                record["dynamodb"]["SequenceNumber"].as_str() == Some(sequence_number.as_str())
            })
            .unwrap_or_else(|| panic!("case {phase_case:?}: no record {sequence_number} in {page}"));
        let reported = record["dynamodb"]["ApproximateCreationDateTime"]
            .as_f64()
            .unwrap_or_else(|| panic!("case {phase_case:?}: no creation stamp in {record}"));
        let expected = epoch_secs(floor_to_second(*written));
        assert!(
            (reported - expected).abs() < f64::EPSILON && reported.fract() == 0.0,
            "case {phase_case:?}: expected the reported stamp to be the whole second \
             {expected}, got {reported}"
        );

        // What the service KEPT: the write itself.
        let kept = engine
            .state::<StreamsState>()
            .lock()
            .record_written_at(&stream_arn, sequence_number);
        assert_eq!(
            kept,
            Some(*written),
            "case {phase_case:?}: expected the write instant {written:?} to be kept for the \
             record, got {kept:?}"
        );

        // So the two measure differently, by exactly the phase the write fell on: a reference
        // read two seconds after the write waited two seconds, and the stamp would say longer.
        let read_at = *written + Duration::from_secs(2);
        let from_the_write = millis_between(*written, read_at);
        let from_the_stamp = millis_between(floor_to_second(*written), read_at);
        let phase_millis = phase.as_secs_f64() * 1_000.0;
        assert!(
            (from_the_write - 2_000.0).abs() < 1.0,
            "case {phase_case:?}: expected the wait measured from the write to be 2000 ms, got \
             {from_the_write}"
        );
        assert!(
            (from_the_stamp - from_the_write - phase_millis).abs() < 1.0,
            "case {phase_case:?}: expected a wait measured from the reported stamp to overstate \
             the real one by {phase_millis} ms, got {} ms",
            from_the_stamp - from_the_write
        );
    }

    // And nothing is kept for a record or a stream this service does not hold.
    let held = written_records
        .first()
        .map(|(_, _, sequence_number)| sequence_number.clone())
        .unwrap_or_default();
    let absent = engine
        .state::<StreamsState>()
        .lock()
        .record_written_at(&stream_arn, "00000000000000000000000000000000000000");
    assert_eq!(
        absent, None,
        "case {case:?}: expected no write instant for a sequence number the stream does not \
         hold, got {absent:?}"
    );
    let unknown = engine
        .state::<StreamsState>()
        .lock()
        .record_written_at("arn:aws:dynamodb:us-west-2:000000000000:table/none/stream/x", &held);
    assert_eq!(
        unknown, None,
        "case {case:?}: expected no write instant for a stream this service does not hold, got \
         {unknown:?}"
    );
}

/// RULE, end to end: the age the mapping publishes for the batch it read lies between the wait
/// its newest record already had when the mapping was created and the wait it had when the
/// datapoint was read — so it is never longer than the time really elapsed since the write,
/// whatever sub-second phase that write fell on.
///
/// One test per row of [`PHASES`], because each deploys a whole pipeline of its own: a row is
/// the phase the batch's writes fell on, crossed with how many records that batch holds.
async fn the_metered_age_is_the_records_own_wait(case: &str, phase: Duration, records: usize) {
    let (engine, router) = emulator();
    let stream_arn = given_stream_table(&router, case).await;
    given_function(&router, case).await;

    // Every record of the batch is written at the same instant, so the wait of the batch's
    // NEWEST record — which is what the age is of — is a span this case knows.
    let written = floor_to_second(SystemTime::now() - WRITTEN_AGO) + phase;
    for index in 0..records {
        append_at(
            &engine,
            case,
            &stream_arn,
            &format!("written-{index}"),
            written,
        );
    }

    let before_create = given_mapping(&router, case, &stream_arn, records.max(1)).await;
    let (points, read_at) = datapoints_once_published(&router, case).await;

    let at_most = millis_between(written, read_at);
    let at_least = millis_between(written, before_create);
    for point in &points {
        let unit = point["Unit"].as_str().unwrap_or_default();
        assert_eq!(
            unit, MILLISECONDS,
            "case {case:?}: the age must be reported in {MILLISECONDS}, got {point}"
        );
        let largest = point["Maximum"].as_f64().unwrap_or_default();
        let smallest = point["Minimum"].as_f64().unwrap_or_default();
        assert!(
            largest <= at_most,
            "case {case:?}: a batch whose newest record was written {at_most} ms before the \
             read cannot meter longer than that, got {largest} ms"
        );
        assert!(
            smallest >= 0.0,
            "case {case:?}: an age is never negative, got {smallest} ms"
        );
        assert!(
            smallest >= at_least,
            "case {case:?}: the record had already waited {at_least} ms when the mapping was \
             created, and every read is after that, got {smallest} ms"
        );
    }
}

/// [`the_metered_age_is_the_records_own_wait`] for a write on the second's own boundary, where
/// rounding the stamp down loses nothing at all.
#[tokio::test]
async fn the_metered_age_of_a_batch_written_on_the_second() {
    the_metered_age_is_the_records_own_wait(PHASES[0].0, PHASES[0].1, 1).await;
}

/// See [`the_metered_age_is_the_records_own_wait`]: a write a millisecond into its second.
#[tokio::test]
async fn the_metered_age_of_a_batch_written_a_millisecond_into_its_second() {
    the_metered_age_is_the_records_own_wait(PHASES[1].0, PHASES[1].1, 2).await;
}

/// See [`the_metered_age_is_the_records_own_wait`]: a write halfway through its second.
#[tokio::test]
async fn the_metered_age_of_a_batch_written_mid_second() {
    the_metered_age_is_the_records_own_wait(PHASES[2].0, PHASES[2].1, 3).await;
}

/// See [`the_metered_age_is_the_records_own_wait`]: a write in the last millisecond of its
/// second, where rounding the stamp down loses the most it can.
#[tokio::test]
async fn the_metered_age_of_a_batch_written_in_the_last_millisecond_of_its_second() {
    the_metered_age_is_the_records_own_wait(PHASES[3].0, PHASES[3].1, 1).await;
}

/// RULE, over the phase NOBODY chooses: an item written through DynamoDB's own `PutItem` lands
/// on whatever sub-second phase the wall clock is at, and the age metered for the batch that
/// carries it is bounded the same way — which is the path a client's own write takes.
#[tokio::test]
async fn the_age_metered_for_a_live_write_is_bounded_by_the_time_since_that_write() {
    let case = "the live wall clock's own phase";
    let (_engine, router) = emulator();
    let stream_arn = given_stream_table(&router, case).await;
    given_function(&router, case).await;

    let before_write = SystemTime::now();
    ddb(
        &router,
        case,
        "PutItem",
        &json!({
            "TableName": TABLE,
            "Item": {"pk": {"S": "live"}, "payload": {"S": "written"}},
        }),
    )
    .await;

    let _before_create = given_mapping(&router, case, &stream_arn, 1).await;
    let (points, read_at) = datapoints_once_published(&router, case).await;

    // The write happened at or after `before_write`, so the wait can be no longer than the span
    // from there to the read that found the datapoint.
    let at_most = millis_between(before_write, read_at);
    for point in &points {
        let largest = point["Maximum"].as_f64().unwrap_or_default();
        let smallest = point["Minimum"].as_f64().unwrap_or_default();
        assert!(
            largest <= at_most,
            "case {case:?}: the item was written at most {at_most} ms before the read, so no \
             batch can meter longer than that, got {largest} ms"
        );
        assert!(
            smallest >= 0.0,
            "case {case:?}: an age is never negative, got {smallest} ms"
        );
    }
}
