// SPDX-License-Identifier: CC0-1.0

//! What a client SEES of the records an event source mapping whose `EventSourceArn` names an
//! Amazon Kinesis STREAM CONSUMER delivered: the handler's own lines in the function's
//! CloudWatch Logs group, with no client action between the `PutRecords` and the log line.
//!
//! `EventSourceArn` is "Amazon Kinesis – The ARN of the data stream or a stream consumer", and
//! `RegisterStreamConsumer` "registers a consumer with a Kinesis data stream… you use this
//! operation to notify Kinesis Data Streams that you are going to register a consumer so that
//! this consumer can read data" — the enhanced fan-out form of reading a stream, whose records
//! a consumer is pushed over a `SubscribeToShard` subscription. So the RULE asserted over the
//! table below, in one sentence: a mapping on EITHER of the two resources that member admits
//! delivers every record put to the stream to its function, in the same shape and with no
//! client action, so what the mapping names decides which reader Lambda consumes the shard
//! with and nothing about WHICH records the function is handed.
//!
//! That is the only pair a client can watch such a consumer by: a mapping reporting `Enabled`
//! says nothing about a batch having been delivered, so a fan-out mapping that silently
//! processes nothing shows up here as a log group holding no line of the handler while the
//! mapping calls itself healthy, and the mapping's own `LastProcessingResult` — "the result of
//! the event source mapping's last processing attempt" — is the other half of it.
//!
//! Every test drives real HTTP through the production `MultiServiceRouter` with Lambda, Amazon
//! Kinesis and CloudWatch Logs on ONE engine, as `core/router` assembles them: the stream, its
//! consumer and the records are created and put through Kinesis's own surface, the function
//! carries a REAL `python3.x` deployment package whose handler prints one line per record it
//! was handed, and the log is read back with CloudWatch Logs' `DescribeLogGroups`,
//! `DescribeLogStreams` and `GetLogEvents` — so a line no handler wrote fails here.
//!
//! The rows differ along what the rule generalises over: which of the two resources the
//! event source names (a registered consumer, the data stream itself), which
//! `StartingPosition` the mapping reads from, how many shards the stream has, how many records
//! are put and how many of them one batch may hold — and, in the last test, how many consumers
//! of one stream are being read at once, which is what enhanced fan-out is for.
//!
//! Nothing here waits a span of its own choosing: each `PutRecords` opens one deadline
//! ([`ROW_BUDGET`]) for the chain it starts and every look-again after it shares that one, and
//! every state a row needs is waited for by looking again until THE STATE IT ASSERTS holds, so
//! no assertion depends on how fast the host runs. The function's own wall-clock and memory
//! bounds are given all the room the emulator has to give
//! ([`crate::invocation_log_delivery::function_timeout_seconds`]), because no row here is about
//! either.
//!
//! Every test here RUNS handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::LAST_PROCESSING_RESULT_OK;
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::results_until;
use crate::event_source_mappings::{
    get_mapping, given_enabled_mapping, router_on, CLOCK_BUDGET, LOOK_AGAIN,
};
use crate::invocation_log_delivery::{
    create_function, events, groups, streams, Created, PYTHON_RUNTIME,
};
use crate::kinesis_event_source_delivery::{given_stream, kinesis, put_records};

/// How long the chain ONE `PutRecords` starts is given for every state the clock owes it
/// together — the mapping's poller taking its batches, the invocations running, their logs
/// reaching the group and the mapping reporting the attempt that consumed the batch.
///
/// PATIENCE, not a weakened expectation, and taken from what the EMULATOR itself allows an
/// invocation rather than picked here: the wait for a share of the run capacity
/// ([`PERMIT_WAIT`]) and the wait for the run's own answer ([`HOST_WAIT_CEILING`]), for each of
/// the two batches a row can be delivered in, with room on top because a whole-workspace run
/// shares its cores with every other suite.
const ROW_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_mul(2)
    .saturating_add(Duration::from_secs(60));

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted beside the services below, so nothing is known about the role and no read of the
/// chain is withheld from it.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The default group a function's invocations are logged to: "By default, Lambda functions
/// send logs to a default log group named `/aws/lambda/<function name>`"
/// (`LoggingConfig$LogGroup`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// The `eventSource` a record of a Kinesis data stream names itself with, which the handler
/// refuses to process anything else under.
const EVENT_SOURCE: &str = "aws:kinesis";

/// What the handler prints before each record's own payload. ONE spelling of it, bound into
/// the handler's source and into what the assertions look for.
const MARKER: &str = "kinesis record: ";

/// The `StartingPosition` values a stream source admits that these rows read from (source:
/// `smithy_query(query="shape", shape="EventSourcePosition")`, Lambda): "the oldest record in
/// the shard" and "the most recent record".
const TRIM_HORIZON: &str = "TRIM_HORIZON";
/// See [`TRIM_HORIZON`].
const LATEST: &str = "LATEST";

/// The `ConsumerStatus` a consumer that may be read reports: "a consumer can't read data while
/// in the CREATING or DELETING states" (`Consumer$ConsumerStatus`).
const CONSUMER_ACTIVE: &str = "ACTIVE";

/// The handler every function below runs: one printed line per record of the batch it was
/// handed, carrying that record's own payload, so a line in the log group is a consequence of
/// the record having reached the handler.
///
/// It also checks the envelope Lambda hands a Kinesis batch over as — the record's own
/// `eventSource`, the event source it names, and its partition key and sequence number — so a
/// batch delivered in some other shape fails the run instead of being logged.
fn logging_handler() -> String {
    format!(
        r#"
import base64

MARKER = "{MARKER}"
EVENT_SOURCE = "{EVENT_SOURCE}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    for record in records:
        if record["eventSource"] != EVENT_SOURCE:
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        if not record["eventSourceARN"]:
            raise ValueError("a record carries no eventSourceARN")
        data = record["kinesis"]
        if not data["partitionKey"] or not data["sequenceNumber"]:
            raise ValueError("a record carries no partition key or sequence number")
        print(MARKER + base64.b64decode(data["data"]).decode("utf-8"))
    return len(records)
"#
    )
}

/// Lambda, Amazon Kinesis and CloudWatch Logs over ONE shared engine — the three services
/// this chain spans, mounted the way the production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_kinesis_kinesis_20131202::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// A consumer registered on the stream `stream_arn` names under `name`, waited out to the
/// status in which it may be read ([`CONSUMER_ACTIVE`]); answers its `ConsumerARN`.
///
/// Registered and read back through Kinesis's own surface, so the ARN a mapping below names is
/// the one that service minted and reports.
///
/// `pub(crate)` because a consumer a mapping may be created on has ONE fixture in this target:
/// the sibling suite that watches the same delivery through the handler's own AWS call
/// ([`crate::kinesis_fan_out_consumer_handler_writes_dynamodb`]) stands its consumer up here
/// too, so the two tables cannot drift apart on what a readable consumer is.
pub(crate) async fn given_active_consumer(
    router: &MultiServiceRouter,
    case: &str,
    stream_arn: &str,
    name: &str,
) -> String {
    let (status, registered) = kinesis(
        router,
        "RegisterStreamConsumer",
        &json!({ "StreamARN": stream_arn, "ConsumerName": name }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: RegisterStreamConsumer answered {status} {registered}"
    );
    let arn = registered["Consumer"]["ConsumerARN"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no ConsumerARN in {registered}"))
        .to_string();
    let deadline = Instant::now() + CLOCK_BUDGET;
    loop {
        let (status, read) = kinesis(
            router,
            "DescribeStreamConsumer",
            &json!({ "ConsumerARN": arn }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeStreamConsumer answered {status} {read}"
        );
        if read["ConsumerDescription"]["ConsumerStatus"] == json!(CONSUMER_ACTIVE) {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the consumer never reported {CONSUMER_ACTIVE}: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row: which resource the mapping's event source names, where it reads from, how many
/// shards the stream has, how many records are put and how many of them one batch may hold.
struct Row {
    case: &'static str,
    /// Whether the mapping's `EventSourceArn` names a registered CONSUMER of the stream
    /// rather than the data stream itself.
    through_consumer: bool,
    starting_position: &'static str,
    shards: i64,
    records: usize,
    batch_size: i64,
}

/// The line the handler writes for the `index`th record `put_records` puts for `case`, whose
/// payload is that pair ([`put_records`]).
fn expected_line(case: &str, index: usize) -> String {
    format!("{MARKER}{case}-{index}")
}

/// Every line the row's records must produce, one per record.
fn expected_lines(row: &Row) -> Vec<String> {
    (0..row.records)
        .map(|index| expected_line(row.case, index))
        .collect()
}

/// Every log line the group `group` holds, across every stream of it, as a client reads them:
/// `DescribeLogStreams` and then `GetLogEvents` on each stream.
///
/// A group no delivery has created yet holds none, which is what this answers while the chain
/// is still in flight.
async fn logged_lines(router: &MultiServiceRouter, case: &str, group: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for stream in streams(router, case, group).await {
        for (_, message) in events(router, case, group, &stream).await {
            lines.push(message);
        }
    }
    lines
}

/// Watch the function's log group until every line of `wanted` has been delivered, answering
/// what was last read.
///
/// The whole chain is waited for TOGETHER — the poller taking the batch, the invocation running
/// and its log reaching the group — because a reader that stopped at the first of them would be
/// reading an instant of its own choosing in the middle of it.
async fn logged(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    wanted: &[String],
    deadline: Instant,
) -> Vec<String> {
    loop {
        let lines = logged_lines(router, case, group).await;
        let settled = wanted
            .iter()
            .all(|line| lines.iter().any(|written| written.contains(line)));
        if settled || Instant::now() >= deadline {
            return lines;
        }
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Create the function `function` with the handler every row runs.
async fn given_logging_function(router: &MultiServiceRouter, case: &str, function: &str) {
    let source = logging_handler();
    create_function(
        router,
        &Created {
            case,
            name: function,
            source: &source,
            runtime: PYTHON_RUNTIME,
            role: ROLE,
            log_group: None,
        },
    )
    .await;
}

/// Assert that every line of `wanted` reached the group of `function`, and that the mapping
/// `uuid` names reports the attempt that consumed its batch.
async fn assert_lines_and_report(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    uuid: &str,
    wanted: &[String],
    deadline: Instant,
) {
    let group = format!("{DEFAULT_GROUP_PREFIX}{function}");
    let lines = logged(router, case, &group, wanted, deadline).await;
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|line| !lines.iter().any(|written| written.contains(line.as_str())))
        .collect();
    // What the mapping says about its own last attempt is read only to SAY why a delivery that
    // did not arrive did not arrive, so the row that fails names the cause instead of only the
    // lines it did not find.
    assert!(
        missing.is_empty(),
        "case {case:?}: the handler's lines {missing:?} must be delivered to {group:?}, got \
         {lines:?}; the mapping reports {}",
        get_mapping(router, uuid).await.text()
    );
    // The group the lines are in is the one the rule names, reported by CloudWatch Logs itself
    // rather than inferred from the read above.
    let named = groups(router, case, &group).await;
    assert!(
        named.iter().any(|found| found == &group),
        "case {case:?}: the delivery must have created {group:?}, got {named:?}"
    );
    // And the mapping says what its attempt at the batch came to, which is the other half a
    // client watches a consumer by. Waited for by looking again until the mapping reports THAT
    // attempt, because every pass the mapping makes overwrites the member.
    let reported = results_until(router, case, uuid, LAST_PROCESSING_RESULT_OK, deadline).await;
    assert_eq!(
        reported.last().map(String::as_str),
        Some(LAST_PROCESSING_RESULT_OK),
        "case {case:?}: the mapping must report the attempt that consumed its batch as \
         {LAST_PROCESSING_RESULT_OK}, saw {reported:?}"
    );
}

/// Stand one row up — a stream, the resource its mapping names, a function and the mapping —
/// put the row's records, and assert the handler's own line for every one of them is in the
/// function's group.
///
/// `label` keeps one test's stream and function names apart from another's; every row runs on
/// an engine of its own.
async fn assert_every_record_reaches_the_function(label: &str, row: &Row) {
    let case = row.case;
    let router = emulator();
    let stream = format!("kinesis-fanout-{label}");
    let function = format!("kinesis-fanout-{label}");
    let stream_arn = given_stream(&router, case, &stream, row.shards).await;
    given_logging_function(&router, case, &function).await;

    // What the mapping names: a registered consumer of the stream, or the data stream itself.
    let event_source = if row.through_consumer {
        let consumer = given_active_consumer(&router, case, &stream_arn, label).await;
        assert!(
            consumer.starts_with(&stream_arn) && consumer.contains("/consumer/"),
            "case {case:?}: a consumer's ARN extends its stream's, got {consumer:?}"
        );
        consumer
    } else {
        stream_arn.clone()
    };
    let uuid = given_enabled_mapping(
        &router,
        case,
        &json!({
            "FunctionName": function,
            "EventSourceArn": event_source,
            "StartingPosition": row.starting_position,
            "BatchSize": row.batch_size,
            "Enabled": true,
        }),
    )
    .await;
    // The mapping reports the event source it was created on, whichever of the two it is.
    let read = get_mapping(&router, &uuid).await;
    assert_eq!(
        read.string("EventSourceArn").as_deref(),
        Some(event_source.as_str()),
        "case {case:?}: the mapping must report the event source it polls, got {}",
        read.text()
    );

    // The ONE client action: the records are put where a client puts them. Everything after it
    // is the mapping's own work, and the row's patience is for THAT.
    put_records(&router, case, &stream, row.records).await;
    let deadline = Instant::now() + ROW_BUDGET;
    assert_lines_and_report(
        &router,
        case,
        &function,
        &uuid,
        &expected_lines(row),
        deadline,
    )
    .await;
}

/// RULE: a mapping whose `EventSourceArn` names a registered stream CONSUMER is delivered
/// every record put to that consumer's stream — through the consumer's own enhanced fan-out
/// subscription — and the invocation it is handed to logs the handler's line for each of them.
#[tokio::test]
async fn a_consumer_sourced_mapping_delivers_every_record_to_its_function() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    assert_every_record_reaches_the_function(
        "consumer-horizon",
        &Row {
            case: "a consumer read from the horizon, three records in one batch",
            through_consumer: true,
            starting_position: TRIM_HORIZON,
            shards: 1,
            records: 3,
            batch_size: 10,
        },
    )
    .await;
}

/// The same RULE over the dimensions a consumer-sourced mapping varies along: records spread
/// over several shards, read from the position a mapping created `LATEST` starts at, and one
/// record per batch — so each record is a batch, and an invocation, of its own.
#[tokio::test]
async fn a_consumer_sourced_mapping_delivers_a_record_per_batch_over_several_shards() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    assert_every_record_reaches_the_function(
        "consumer-latest",
        &Row {
            case: "a consumer read from the latest position, two records one per batch",
            through_consumer: true,
            starting_position: LATEST,
            shards: 2,
            records: 2,
            batch_size: 1,
        },
    )
    .await;
}

/// The OTHER resource `EventSourceArn` admits, as the row the rule generalises from: a mapping
/// on the data stream itself is delivered the same records in the same shape, which is what a
/// consumer-sourced mapping is asserted to match.
#[tokio::test]
async fn a_stream_sourced_mapping_delivers_every_record_to_its_function() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    assert_every_record_reaches_the_function(
        "stream-horizon",
        &Row {
            case: "the data stream itself, read from the horizon, three records in one batch",
            through_consumer: false,
            starting_position: TRIM_HORIZON,
            shards: 1,
            records: 3,
            batch_size: 10,
        },
    )
    .await;
}

/// RULE, over the remaining dimension and the reason enhanced fan-out exists: each registered
/// consumer of one stream is read INDEPENDENTLY, so two mappings — one per consumer, each
/// invoking its own function — are both delivered every record put to that stream, and neither
/// consumes the other's.
#[tokio::test]
async fn two_consumers_of_one_stream_each_receive_every_record() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "two consumers of one stream, one mapping and one function each";
    let router = emulator();
    let stream = "kinesis-fanout-two-consumers";
    let stream_arn = given_stream(&router, case, stream, 1).await;

    let mut mappings: Vec<(String, String)> = Vec::new();
    for name in ["reader-one", "reader-two"] {
        let function = format!("kinesis-fanout-{name}");
        given_logging_function(&router, case, &function).await;
        let consumer = given_active_consumer(&router, case, &stream_arn, name).await;
        let uuid = given_enabled_mapping(
            &router,
            case,
            &json!({
                "FunctionName": function,
                "EventSourceArn": consumer,
                "StartingPosition": TRIM_HORIZON,
                "BatchSize": 10,
                "Enabled": true,
            }),
        )
        .await;
        mappings.push((function, uuid));
    }

    let records = 2;
    put_records(&router, case, stream, records).await;
    let deadline = Instant::now() + ROW_BUDGET;
    let wanted: Vec<String> = (0..records)
        .map(|index| expected_line(case, index))
        .collect();
    for (function, uuid) in &mappings {
        assert_lines_and_report(&router, case, function, uuid, &wanted, deadline).await;
    }
}
