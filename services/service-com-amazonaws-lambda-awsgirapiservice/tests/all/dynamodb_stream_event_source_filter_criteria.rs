// SPDX-License-Identifier: CC0-1.0

//! Which records of an Amazon DynamoDB table's STREAM an event source mapping hands its
//! function, end to end over the wire with DynamoDB, DynamoDB Streams, Amazon SQS and IAM
//! mounted beside Lambda on ONE engine — so the items are written where a client writes
//! them, the execution role is the one IAM holds, and what reached the handler is read off
//! the queue the handler forwarded it to.
//!
//! Two rules decide that, and this suite pins them. `FilterCriteria` is "an object that
//! defines the filter criteria that determine whether Lambda should process an event", and
//! `Filters` is "a list of filters", so of the records one poll takes off the source exactly
//! those matching AT LEAST ONE of the mapping's patterns are handed to the function; a record
//! no pattern matches is discarded without an invocation, and the mapping's position still
//! advances past it, so the records behind it are delivered. A mapping carrying no criteria
//! determines nothing and so admits every record. Beside them, `MaximumRecordAgeInSeconds` is
//! "Discard records older than the specified age. The default value is infinite (-1)", so a
//! mapping stating a finite age hands no record older than that age to its function at all,
//! and one carrying the infinite default hands over every record whatever its age.
//!
//! Every row's verdict is read off what the HANDLER forwarded: a record that reached it
//! appears as one message on the output queue, so a record withheld from it cannot appear.
//! The last record each row expects delivered is written AFTER every record it expects
//! withheld, and a shard is read in order — so once that record's message is on the queue
//! the withheld ones have been read past, and their absence is a verdict rather than a race.
//!
//! The criteria rows differ along what that rule generalises over: which member of the record
//! the pattern names (the change's own name, a value nested inside the item's keys), how many
//! values one clause lists, how many patterns the criteria carry (none, one, two that are
//! ORed), and how much of the batch they admit (all of it, some of it, none of it). The
//! record-age rows differ along what the age rule generalises over: the age configured (the
//! member absent, the infinite default, the smallest age AWS publishes, the largest the
//! member's own range admits, and a finite age the records have already passed) and whether
//! the records are older than it when the mapping starts reading.
//!
//! The age is decided per RECORD, so the last suite is the shape that mixes the two: a
//! backlog that has already passed the age with ONE record written after the mapping existed
//! behind it. Each record of such a read is decided by its own age, and a record the age
//! discarded is accounted for by the poller itself — so the position advances past it and the
//! younger record behind it is still handed over. Its rows differ along the age stated (two
//! finite spans and the infinite default) and how many aged records sit ahead of the younger
//! one.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{
    dynamodb, given_table, results_until, write_item, Write, BUDGET, KEY, LOOK_AGAIN,
    NEW_AND_OLD_IMAGES, RESULT_OK, TRIM_HORIZON,
};
use crate::event_source_mapping_role_access::{allow, given_role, DYNAMODB_READS};
use crate::event_source_mappings::{call, call_sqs, create_mapping, create_queue, get_mapping};
// The one writer of a deployment archive in this target, shared with every suite whose
// handler RUNS.
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound (`POST /2015-03-31/functions`).
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below runs, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `State` a mapping reports once it polls its event source.
const STATE_ENABLED: &str = "Enabled";

/// The `OperationType` a stream record reports for each kind of change
/// (`smithy_query(query="shape", shape="OperationType")`, DynamoDB Streams).
const INSERT: &str = "INSERT";
/// See [`INSERT`].
const MODIFY: &str = "MODIFY";
/// See [`INSERT`].
const REMOVE: &str = "REMOVE";

/// The one Amazon SQS action the handler's own call takes, which is all its execution role is
/// granted of that service.
const SEND_MESSAGE: [&str; 1] = ["SendMessage"];

/// Lambda, DynamoDB, DynamoDB Streams, Amazon SQS and IAM on ONE engine — the shape
/// `core/router/src/lib.rs` mounts them in: the mapping's source is a table's stream, the
/// handler forwards what it was handed to a queue, and the execution role the poller and the
/// handler act as is the one IAM holds.
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
    router.register_service(service_com_amazonaws_sqs_amazonsqs::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// The handler every row runs: it forwards ONE message per record it was handed to the queue
/// its environment names, carrying the change's name and the changed item's key — both taken
/// out of the record itself, so a message can only appear for a record that reached it.
fn forwarding_handler() -> String {
    format!(
        r#"
import json
import os

import boto3

QUEUE_URL = os.environ["QUEUE_URL"]
KEY = "{KEY}"


def lambda_handler(event, context):
    client = boto3.client("sqs")
    forwarded = 0
    for record in event["Records"]:
        client.send_message(
            QueueUrl=QUEUE_URL,
            MessageBody=json.dumps(
                {{
                    "eventName": record["eventName"],
                    "key": record["dynamodb"]["Keys"][KEY]["S"],
                }}
            ),
        )
        forwarded += 1
    return {{"forwarded": forwarded}}
"#
    )
}

/// The row's function: it RUNS [`forwarding_handler`], as `role_arn`, forwarding to the queue
/// `queue_url` addresses; waited out to `Active`, because a mapping polls nothing for a
/// function that cannot be invoked.
async fn given_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    queue_url: &str,
) {
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": role_arn,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 30,
            "Environment": { "Variables": { "QUEUE_URL": queue_url } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&forwarding_handler())),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        created.status,
        created.text()
    );
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = call(
            router,
            "GET",
            &format!("{FUNCTIONS_URI}/{name}/configuration"),
            &Value::Null,
        )
        .await;
        if read.string("State").as_deref() == Some("Active") {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the function {name:?} never became Active: {}",
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// The row's mapping on `stream_arn`, carrying the row's patterns as its `FilterCriteria`
/// when it has any; waited out to `Enabled`, and answered as its UUID.
async fn given_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    patterns: &[&str],
) -> String {
    let mut request = json!({
        "FunctionName": function,
        "EventSourceArn": stream_arn,
        "StartingPosition": TRIM_HORIZON,
        "BatchSize": 10,
        "MaximumBatchingWindowInSeconds": 1,
        "Enabled": true,
    });
    if !patterns.is_empty() {
        request["FilterCriteria"] = json!({
            "Filters": patterns
                .iter()
                .map(|pattern| json!({ "Pattern": pattern }))
                .collect::<Vec<Value>>(),
        });
    }
    enabled_mapping(router, case, request).await
}

/// Create the mapping `request` asks for and wait it out to `Enabled`, answering its UUID: a
/// mapping still settling reads nothing, so a row that looked before it settled would be
/// reading the lifecycle rather than the rule.
async fn enabled_mapping(router: &MultiServiceRouter, case: &str, request: Value) -> String {
    let created = create_mapping(router, &request).await;
    assert_eq!(
        created.status,
        202,
        "case {case:?}: expected CreateEventSourceMapping to answer 202, got {} {}",
        created.status,
        created.text()
    );
    let uuid = created
        .string("UUID")
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {}", created.text()));
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = get_mapping(router, &uuid).await;
        if read.string("State").as_deref() == Some(STATE_ENABLED) {
            return uuid;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping never reported {STATE_ENABLED}: {}",
            read.text()
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Everything waiting on the output queue right now, as the markers the handler forwarded
/// (`<eventName>#<key>`), each message deleted so a later look finds only later ones.
async fn take_markers(router: &MultiServiceRouter, case: &str, url: &str) -> Vec<String> {
    let (status, received) = call_sqs(
        router,
        "ReceiveMessage",
        &json!({ "QueueUrl": url, "MaxNumberOfMessages": 10 }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected ReceiveMessage to answer 200, got {status} {received}"
    );
    let messages = received["Messages"].as_array().cloned().unwrap_or_default();
    let mut markers = Vec::with_capacity(messages.len());
    for message in &messages {
        let body = message["Body"].as_str().unwrap_or_default().to_string();
        let (status, deleted) = call_sqs(
            router,
            "DeleteMessage",
            &json!({
                "QueueUrl": url,
                "ReceiptHandle": message["ReceiptHandle"].as_str().unwrap_or_default(),
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected DeleteMessage to answer 200, got {status} {deleted}"
        );
        let forwarded: Value = serde_json::from_str(&body).unwrap_or_else(|error| {
            panic!("case {case:?}: expected a forwarded record as JSON, got {body:?} ({error})")
        });
        markers.push(marker(
            forwarded["eventName"].as_str().unwrap_or_default(),
            forwarded["key"].as_str().unwrap_or_default(),
        ));
    }
    markers
}

/// How this suite names one delivered record: the change it reported and the key it changed.
fn marker(event_name: &str, key: &str) -> String {
    format!("{event_name}#{key}")
}

/// Poll the output queue until every marker of `expected` has arrived, answering every marker
/// seen in the order they arrived.
async fn await_markers(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    expected: &[String],
) -> Vec<String> {
    markers_until(router, case, url, expected, Instant::now() + BUDGET).await
}

/// Poll the output queue until every marker of `expected` has arrived or `deadline` passes,
/// answering every marker seen in the order they arrived.
///
/// `deadline` is the CALLER's, so a suite whose row is not about how long a delivery takes
/// can give that delivery room while the one limit its row IS about stays tight.
async fn markers_until(
    router: &MultiServiceRouter,
    case: &str,
    url: &str,
    expected: &[String],
    deadline: Instant,
) -> Vec<String> {
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

/// One row: the patterns the mapping filters with, the writes made to the table, and which of
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

/// The rows: one per shape of criteria, over a table whose stream captures both images so
/// every member a pattern could name is in the record.
fn rows() -> Vec<Row> {
    vec![
        // The recorded shape: one clause on the change's own name, admitting the insert and
        // withholding the change made to the same item afterwards.
        Row {
            case: "one-event-name",
            patterns: &[r#"{"eventName": ["INSERT"]}"#],
            writes: &[
                ("row-1", Write::Insert),
                ("row-1", Write::Overwrite),
                ("row-2", Write::Insert),
            ],
            delivered: &[(INSERT, "row-1"), (INSERT, "row-2")],
            withheld: &[(MODIFY, "row-1")],
        },
        // Two values in one clause: a record matching either is admitted.
        Row {
            case: "two-event-names-in-one-clause",
            patterns: &[r#"{"eventName": ["MODIFY", "REMOVE"]}"#],
            writes: &[
                ("row-a", Write::Insert),
                ("row-a", Write::Update),
                ("row-a", Write::Delete),
            ],
            delivered: &[(MODIFY, "row-a"), (REMOVE, "row-a")],
            withheld: &[(INSERT, "row-a")],
        },
        // A value NESTED inside the record: the changed item's own key.
        Row {
            case: "a-value-nested-in-the-keys",
            patterns: &[r#"{"dynamodb": {"Keys": {"pk": {"S": ["keep"]}}}}"#],
            writes: &[("drop", Write::Insert), ("keep", Write::Insert)],
            delivered: &[(INSERT, "keep")],
            withheld: &[(INSERT, "drop")],
        },
        // Two patterns: a record matching EITHER of them is admitted.
        Row {
            case: "two-patterns-are-ored",
            patterns: &[
                r#"{"eventName": ["REMOVE"]}"#,
                r#"{"dynamodb": {"Keys": {"pk": {"S": ["keep"]}}}}"#,
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
        // A whole batch no pattern matches: nothing is handed over for it, and the position
        // still advances — which the record behind it, delivered afterwards, is the proof of.
        Row {
            case: "a-batch-the-criteria-admit-nothing-of",
            patterns: &[r#"{"eventName": ["REMOVE"]}"#],
            writes: &[
                ("row-y", Write::Insert),
                ("row-y", Write::Overwrite),
                ("row-y", Write::Delete),
            ],
            delivered: &[(REMOVE, "row-y")],
            withheld: &[(INSERT, "row-y"), (MODIFY, "row-y")],
        },
    ]
}

/// RULE: of the records a DynamoDB-stream mapping polls, exactly those at least one of its
/// `FilterCriteria` patterns matches are handed to its function; the rest are discarded
/// without an invocation while the position advances past them.
#[tokio::test]
async fn filter_criteria_decide_which_stream_records_reach_the_function() {
    for row in rows() {
        let case = row.case;
        let router = emulator();
        let table = format!("filtered-{case}");
        let function = format!("filtered-fn-{case}");
        let (queue_url, queue_arn) =
            create_queue(&router, case, &format!("filtered-out-{case}"), &json!({})).await;
        let stream_arn = given_table(&router, case, &table, Some(NEW_AND_OLD_IMAGES)).await;
        // The least-privilege role a client writes for this wiring: the reads consuming a
        // table's stream takes, on that stream, and the one write the handler makes.
        let role_arn = given_role(
            &router,
            case,
            &format!("filtered-role-{case}"),
            Some(json!([
                allow("dynamodb", &DYNAMODB_READS, &stream_arn),
                allow("sqs", &SEND_MESSAGE, &queue_arn),
            ])),
        )
        .await;
        given_function(&router, case, &function, &role_arn, &queue_url).await;

        // Every write is made BEFORE the mapping exists, which is the recorded shape: a
        // mapping reading from the oldest retained record reads the backlog, so the filter
        // decides records that are already in the shard.
        for (key, write) in row.writes {
            write_item(&router, case, &table, key, *write).await;
        }
        let uuid = given_mapping(&router, case, &function, &stream_arn, row.patterns).await;

        let expected: Vec<String> = row
            .delivered
            .iter()
            .map(|(event_name, key)| marker(event_name, key))
            .collect();
        let seen = await_markers(&router, case, &queue_url, &expected).await;
        for (event_name, key) in row.withheld {
            let withheld = marker(event_name, key);
            assert!(
                !seen.contains(&withheld),
                "case {case:?}: no pattern of {:?} matches the {event_name} record of \
                 {key:?}, so no invocation may be handed it, but the handler forwarded \
                 {seen:?}",
                row.patterns
            );
        }

        // The trigger is torn down as a client tears one down, so nothing reads the stream
        // once the row is done with it.
        stop_mapping(&router, case, &uuid).await;
    }
}

/// Tear one mapping down as a client does, so nothing reads the stream once a row is done
/// with it: "When you delete an event source mapping, it enters a `Deleting` state".
async fn stop_mapping(router: &MultiServiceRouter, case: &str, uuid: &str) {
    let deleted = call(
        router,
        "DELETE",
        &format!("/2015-03-31/event-source-mappings/{uuid}"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        deleted.status,
        202,
        "case {case:?}: expected DeleteEventSourceMapping to answer 202, got {} {}",
        deleted.status,
        deleted.text()
    );
}

/// The value `MaximumRecordAgeInSeconds` documents as no bound at all: "Discard records
/// older than the specified age. The default value is infinite (-1)."
const INFINITE_RECORD_AGE: i64 = -1;

/// The smallest age AWS publishes for the member, which is the one the defect was reported
/// against.
const SMALLEST_PUBLISHED_RECORD_AGE: i64 = 60;

/// The keys the record-age rows write, in the order they are written: the backlog whose age
/// the mapping's own `MaximumRecordAgeInSeconds` decides.
const AGED_KEYS: [&str; 3] = ["aged-1", "aged-2", "aged-3"];

/// One row of the record-age suite: the age the mapping states, when its records are
/// written against the mapping's own creation, whether they have already passed that age
/// when it starts reading, and whether its function is handed them.
struct AgeRow {
    case: &'static str,
    /// `MaximumRecordAgeInSeconds` as the create states it; `None` leaves the member out of
    /// the request, which is the infinite default.
    maximum_record_age: Option<i64>,
    /// Whether the records are written BEFORE the mapping exists — the backlog shape, where
    /// the mapping meets records that are already as old as they are — or once it is
    /// `Enabled`, where they arrive while it reads.
    writes_the_backlog_first: bool,
    /// Whether the records are waited out past that age before the mapping is created.
    waits_past_the_age: bool,
    /// Whether the handler must be handed the records.
    delivered: bool,
}

/// The record-age rows: one per shape the configured age can take.
fn age_rows() -> Vec<AgeRow> {
    vec![
        // No age at all: the infinite default, which discards nothing however old the
        // backlog it meets is.
        AgeRow {
            case: "the-member-absent-is-the-infinite-default",
            maximum_record_age: None,
            writes_the_backlog_first: true,
            waits_past_the_age: false,
            delivered: true,
        },
        // A finite age the records are younger than: the age the defect was reported
        // against, over records that arrive while the mapping reads — so nothing about how
        // long this run's own setup took can age them out.
        AgeRow {
            case: "an-age-the-records-are-younger-than",
            maximum_record_age: Some(SMALLEST_PUBLISHED_RECORD_AGE),
            writes_the_backlog_first: false,
            waits_past_the_age: false,
            delivered: true,
        },
        // The largest age the member admits, and the smallest AWS publishes, are read off
        // the member by the unit table in `state/event_source_poller/record_age.rs`; the rows
        // here are the ones that need a stream, a function and a delivery to decide, so the
        // suite stays as small as the rule allows.
        // The recorded shape: the records have already passed the age when the mapping is
        // created, so the function is handed none of them.
        AgeRow {
            case: "an-age-the-records-have-already-passed",
            maximum_record_age: Some(1),
            writes_the_backlog_first: true,
            waits_past_the_age: true,
            delivered: false,
        },
    ]
}

/// The row's mapping on `stream_arn`, carrying the age the row states; waited out to
/// `Enabled` and read back, so a row cannot pass against an age the mapping never carried.
async fn given_aged_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    maximum_record_age: Option<i64>,
) -> String {
    let mut request = json!({
        "FunctionName": function,
        "EventSourceArn": stream_arn,
        "StartingPosition": TRIM_HORIZON,
        "BatchSize": 10,
        "Enabled": true,
    });
    if let Some(age) = maximum_record_age {
        request["MaximumRecordAgeInSeconds"] = json!(age);
    }
    let uuid = enabled_mapping(router, case, request).await;
    let read = get_mapping(router, &uuid).await;
    assert_eq!(
        read.json()["MaximumRecordAgeInSeconds"],
        json!(maximum_record_age.unwrap_or(INFINITE_RECORD_AGE)),
        "case {case:?}: the mapping must carry the record age it was created with, got {}",
        read.text()
    );
    uuid
}

/// Wait until every record written by `written_at` is OLDER than `age`.
///
/// A record's `ApproximateCreationDateTime` is stamped when the change was published and
/// floored to the whole second, so it is at or before the instant the last write answered:
/// once the clock has passed that instant plus the age, every one of those records has
/// passed it. The condition is the records' own age, which is what the mapping decides them
/// by, and it is looked at again until it holds rather than waited out in one span.
async fn aged_past(age: Duration, written_at: SystemTime) {
    let passed = written_at + age + LOOK_AGAIN;
    while SystemTime::now() < passed {
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// RULE: a mapping stating a finite `MaximumRecordAgeInSeconds` hands its function no record
/// older than that age, and one carrying the infinite default hands over every record
/// whatever its age; a discarded record is handed to no invocation while the mapping still
/// reads on past it.
#[tokio::test]
async fn the_record_age_a_mapping_states_decides_which_stream_records_reach_the_function() {
    for row in age_rows() {
        let case = row.case;
        let router = emulator();
        let table = format!("aged-{case}");
        let function = format!("aged-fn-{case}");
        let control_function = format!("control-fn-{case}");
        let (queue_url, queue_arn) =
            create_queue(&router, case, &format!("aged-out-{case}"), &json!({})).await;
        let (control_url, control_arn) =
            create_queue(&router, case, &format!("control-out-{case}"), &json!({})).await;
        let stream_arn = given_table(&router, case, &table, Some(NEW_AND_OLD_IMAGES)).await;
        let role_arn = given_role(
            &router,
            case,
            &format!("aged-role-{case}"),
            Some(json!([
                allow("dynamodb", &DYNAMODB_READS, &stream_arn),
                allow("sqs", &SEND_MESSAGE, &queue_arn),
                allow("sqs", &SEND_MESSAGE, &control_arn),
            ])),
        )
        .await;
        given_function(&router, case, &function, &role_arn, &queue_url).await;

        // The backlog shape: every record is written BEFORE any mapping exists, so the
        // mapping meets records that are already as old as they are. A row that does not take
        // that shape writes them once its mapping is reading instead (see [`AgeRow`]).
        if row.writes_the_backlog_first {
            for key in AGED_KEYS {
                write_item(&router, case, &table, key, Write::Insert).await;
            }
        }
        let written_at = SystemTime::now();
        if row.waits_past_the_age {
            let age = row
                .maximum_record_age
                .and_then(|age| u64::try_from(age).ok())
                .unwrap_or_else(|| panic!("case {case:?}: a row that waits states a finite age"));
            aged_past(Duration::from_secs(age), written_at).await;
        }

        // The CONTROL, for a row whose records must be WITHHELD: the same records on the
        // same stream, read by a mapping carrying the infinite age, so the withholding is
        // read against a delivery that did happen in the same window rather than against
        // silence.
        let control = if row.delivered {
            None
        } else {
            given_function(&router, case, &control_function, &role_arn, &control_url).await;
            Some(
                given_aged_mapping(
                    &router,
                    case,
                    &control_function,
                    &stream_arn,
                    Some(INFINITE_RECORD_AGE),
                )
                .await,
            )
        };
        let uuid =
            given_aged_mapping(&router, case, &function, &stream_arn, row.maximum_record_age).await;
        if !row.writes_the_backlog_first {
            for key in AGED_KEYS {
                write_item(&router, case, &table, key, Write::Insert).await;
            }
        }

        // The deadlines below are GENEROUS: the only tight limit a row is about is the
        // record age it states, so a loaded machine must not be able to turn a delivery into
        // a verdict of its own.
        let deadline = Instant::now() + BUDGET + BUDGET;
        let expected: Vec<String> = AGED_KEYS.iter().map(|key| marker(INSERT, key)).collect();
        if row.delivered {
            let seen = markers_until(&router, case, &queue_url, &expected, deadline).await;
            assert!(
                expected.iter().all(|forwarded| seen.contains(forwarded)),
                "case {case:?}: a mapping stating {:?} must hand over every record of the \
                 backlog, the handler forwarded {seen:?}",
                row.maximum_record_age
            );
        } else {
            let control_uuid = control
                .unwrap_or_else(|| panic!("case {case:?}: a withholding row has a control"));
            // The control settles first: every record of the backlog reached ITS handler, so
            // the records are deliverable and the window in which they would have been
            // delivered has passed.
            markers_until(&router, case, &control_url, &expected, deadline).await;
            // And the aged-out mapping read them and came to no problem at all, which is
            // what discarding them is: it is not a batch it keeps failing on.
            let seen = results_until(&router, case, &uuid, RESULT_OK, deadline).await;
            assert_eq!(
                seen.last().map(String::as_str),
                Some(RESULT_OK),
                "case {case:?}: an aged-out batch is discarded rather than failed, saw {seen:?}"
            );
            let forwarded = take_markers(&router, case, &queue_url).await;
            assert!(
                forwarded.is_empty(),
                "case {case:?}: every record of the backlog is older than the {:?} second(s) \
                 the mapping allows, so its handler must be handed none of them, but it \
                 forwarded {forwarded:?}",
                row.maximum_record_age
            );
            stop_mapping(&router, case, &control_uuid).await;
        }

        stop_mapping(&router, case, &uuid).await;
    }
}

/// The shortest finite age any boundary row states, in the seconds the member states them in.
///
/// An EMULATOR choice of a value inside `@range(min: -1, max: 604800)`, not an AWS number:
/// short enough that a row waits its backlog past it in seconds, and long enough that the one
/// record written while the mapping reads is read well inside it even on a loaded machine —
/// so the only span a row is tight about is the age of the backlog it wrote, never how fast
/// the host delivers the record behind it.
const BOUNDARY_RECORD_AGE: i64 = 5;

/// A second, LONGER finite age the boundary rows state, so the suite reads the member as the
/// span it states rather than as one value it was tried with. Longer than
/// [`BOUNDARY_RECORD_AGE`], because the headroom that one leaves the delivery is the least any
/// row may leave it.
const LONGER_BOUNDARY_RECORD_AGE: i64 = 8;

/// The keys of the backlog a boundary row writes BEFORE its mapping exists, in the order they
/// are written; a row takes as many of them as it states.
const BACKLOG_KEYS: [&str; 3] = ["backlog-1", "backlog-2", "backlog-3"];

/// The key of the one record a boundary row writes once its mapping is already reading: the
/// record that is younger than the age whatever that age is.
const FRESH_KEY: &str = "fresh";

/// One row of the boundary suite: the age the mapping states, how many aged records sit ahead
/// of the record written after it existed, and whether those aged records reach the handler.
struct BoundaryRow {
    case: &'static str,
    /// `MaximumRecordAgeInSeconds` as the create states it; `None` leaves the member out of
    /// the request, which is the infinite default.
    maximum_record_age: Option<i64>,
    /// How many of [`BACKLOG_KEYS`] the row writes before its mapping exists.
    backlog: usize,
    /// Whether the handler must be handed the backlog's records. The record written after the
    /// mapping existed must reach it in every row.
    delivers_the_backlog: bool,
}

impl BoundaryRow {
    /// The span this row waits its backlog past before creating its mapping: the age the
    /// mapping states where that is finite, and otherwise the longest age this suite states —
    /// so a row carrying the infinite age meets a backlog at least as aged as the finite rows'.
    fn waits_past(&self) -> Duration {
        let seconds = self
            .maximum_record_age
            .and_then(|age| u64::try_from(age).ok())
            .unwrap_or_else(|| {
                u64::try_from(LONGER_BOUNDARY_RECORD_AGE)
                    .expect("the suite's age is a positive span")
            });
        Duration::from_secs(seconds)
    }
}

/// The boundary rows: the recorded shape and the two it generalises over.
fn boundary_rows() -> Vec<BoundaryRow> {
    vec![
        // The recorded shape: a whole backlog past the age, with one record written after the
        // mapping existed behind it.
        BoundaryRow {
            case: "a-backlog-past-the-age-then-a-younger-record",
            maximum_record_age: Some(BOUNDARY_RECORD_AGE),
            backlog: BACKLOG_KEYS.len(),
            delivers_the_backlog: false,
        },
        // Another age over a backlog of ONE: the age is read as the span the member states,
        // and the position advances past a single discarded record as past a run of them.
        BoundaryRow {
            case: "one-aged-record-ahead-of-the-younger-one",
            maximum_record_age: Some(LONGER_BOUNDARY_RECORD_AGE),
            backlog: 1,
            delivers_the_backlog: false,
        },
        // The control the withholding is read against: the same backlog, as old, read by a
        // mapping carrying the infinite default, which ages nothing out.
        BoundaryRow {
            case: "the-infinite-age-keeps-the-same-backlog",
            maximum_record_age: Some(INFINITE_RECORD_AGE),
            backlog: BACKLOG_KEYS.len(),
            delivers_the_backlog: true,
        },
    ]
}

/// Every item `keys` names is still in `table`, read with the strongest read a client has:
/// the age is a CONSUMER-side bound, so what it discarded is no change to the table the
/// records came from.
async fn items_are_still_in_the_table(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    keys: &[String],
) {
    for key in keys {
        let (status, read) = dynamodb(
            router,
            "GetItem",
            &json!({
                "TableName": table,
                "Key": { KEY: { "S": key } },
                "ConsistentRead": true,
            }),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: GetItem {key:?} answered {status} {read}"
        );
        assert_eq!(
            read["Item"][KEY],
            json!({ "S": key }),
            "case {case:?}: the item {key:?} must still be in {table:?} whatever the mapping \
             discarded, got {read}"
        );
    }
}

/// RULE: the age a stream mapping states decides each record it reads on that record's OWN
/// age, and a record the age discarded is accounted for by the poller — so a mapping stating
/// a finite age hands its function none of a backlog that has passed that age while still
/// handing over the younger record behind it, and keeps reading as an `Enabled` mapping that
/// came to no problem.
#[tokio::test]
async fn an_aged_out_backlog_is_discarded_while_the_record_behind_it_is_delivered() {
    for row in boundary_rows() {
        let case = row.case;
        let router = emulator();
        let table = format!("boundary-{case}");
        let function = format!("boundary-fn-{case}");
        let (queue_url, queue_arn) =
            create_queue(&router, case, &format!("boundary-out-{case}"), &json!({})).await;
        let stream_arn = given_table(&router, case, &table, Some(NEW_AND_OLD_IMAGES)).await;
        let role_arn = given_role(
            &router,
            case,
            &format!("boundary-role-{case}"),
            Some(json!([
                allow("dynamodb", &DYNAMODB_READS, &stream_arn),
                allow("sqs", &SEND_MESSAGE, &queue_arn),
            ])),
        )
        .await;
        given_function(&router, case, &function, &role_arn, &queue_url).await;

        // The backlog, written before any mapping exists and waited out past the age the row
        // states, so the mapping meets records that have already passed it.
        let backlog = &BACKLOG_KEYS[..row.backlog];
        for key in backlog {
            write_item(&router, case, &table, key, Write::Insert).await;
        }
        aged_past(row.waits_past(), SystemTime::now()).await;

        let uuid =
            given_aged_mapping(&router, case, &function, &stream_arn, row.maximum_record_age).await;
        // And the one record written once that mapping is already reading: younger than the
        // age, and behind every record of the backlog in the shard.
        write_item(&router, case, &table, FRESH_KEY, Write::Insert).await;

        // The deadlines are GENEROUS: the only tight span a row is about is the age it
        // states, so a loaded machine must not turn a slow delivery into a verdict.
        let deadline = Instant::now() + BUDGET + BUDGET;
        let fresh = marker(INSERT, FRESH_KEY);
        let mut seen = markers_until(
            &router,
            case,
            &queue_url,
            std::slice::from_ref(&fresh),
            deadline,
        )
        .await;
        assert!(
            seen.contains(&fresh),
            "case {case:?}: the record written while the mapping read is younger than the \
             {:?} second(s) it allows, so its handler must be handed it, it forwarded {seen:?}",
            row.maximum_record_age
        );
        // A shard is read IN ORDER, so once the younger record's message is on the queue the
        // backlog ahead of it has been read past: what the handler did not forward for it is
        // a verdict rather than a race.
        seen.extend(take_markers(&router, case, &queue_url).await);
        for key in backlog {
            let aged = marker(INSERT, key);
            assert_eq!(
                seen.contains(&aged),
                row.delivers_the_backlog,
                "case {case:?}: a mapping stating {:?} must {} the record of {key:?}, which \
                 had passed {:?} before the mapping existed; the handler forwarded {seen:?}",
                row.maximum_record_age,
                if row.delivers_the_backlog {
                    "hand over"
                } else {
                    "discard"
                },
                row.waits_past()
            );
        }

        // Discarding is not failing: the mapping comes to no problem at all with the records
        // it shed, keeps the age it was created with, and stays Enabled.
        let results = results_until(&router, case, &uuid, RESULT_OK, deadline).await;
        assert_eq!(
            results.last().map(String::as_str),
            Some(RESULT_OK),
            "case {case:?}: an aged-out record is discarded rather than failed, saw {results:?}"
        );
        let read = get_mapping(&router, &uuid).await;
        assert_eq!(
            read.string("State").as_deref(),
            Some(STATE_ENABLED),
            "case {case:?}: a mapping that shed records keeps reading, so it must still \
             report {STATE_ENABLED}, got {}",
            read.text()
        );
        assert_eq!(
            read.json()["MaximumRecordAgeInSeconds"],
            json!(row.maximum_record_age.unwrap_or(INFINITE_RECORD_AGE)),
            "case {case:?}: the mapping must still carry the age it was created with, got {}",
            read.text()
        );
        // And the table itself is untouched by what the consumer shed.
        let keys: Vec<String> = backlog
            .iter()
            .chain(std::iter::once(&FRESH_KEY))
            .map(|key| (*key).to_string())
            .collect();
        items_are_still_in_the_table(&router, case, &table, &keys).await;

        stop_mapping(&router, case, &uuid).await;
    }
}
