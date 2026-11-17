// SPDX-License-Identifier: CC0-1.0

//! What a client SEES of a batch an event source mapping on a DynamoDB table's STREAM
//! delivered: the handler's own lines in the function's CloudWatch Logs group, with no client
//! action between the item write and the log line.
//!
//! `CreateEventSourceMapping` "creates a mapping between an event source and an Lambda Event
//! Function. Lambda reads items from the event source and invokes the function", and `Enabled`
//! is "when true, the event source mapping is active. When false, Lambda pauses polling and
//! invocation" — so the RULE asserted here, in one sentence: while a mapping whose
//! `EventSourceArn` names a table's stream reports `Enabled`, every change that stream captures
//! is handed to the mapping's function by the mapping's own poller, and that hand-over is a
//! FULL invocation of the function, so the log it produces — the lines the handler itself wrote
//! about the records it was handed included — is delivered to the function's log group (the one
//! its `LoggingConfig$LogGroup` names, else the default `/aws/lambda/<function name>`) and the
//! mapping reports the attempt it made of the batch as its `LastProcessingResult`.
//!
//! That is the only pair a client can watch a stream consumer by: a mapping reporting `Enabled`
//! says nothing about a batch having been delivered, so a consumer that silently processes
//! nothing shows up here as a log group holding no line of the handler while the mapping calls
//! itself healthy. `crate::sqs_event_source_log_delivery` is the same rule over an Amazon SQS
//! source; this suite is the stream half of it, where the records are made by WRITING to a
//! table rather than by sending a message.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! DynamoDB, DynamoDB Streams and CloudWatch Logs on ONE engine, as `core/router` assembles
//! them: the table and its stream are created and written through DynamoDB's own surface, the
//! function carries a REAL `python3.x` deployment package whose handler prints one line per
//! record, and the log is read back with CloudWatch Logs' `DescribeLogGroups`,
//! `DescribeLogStreams` and `GetLogEvents` — so a line no handler wrote fails here.
//!
//! The rows differ along what the rule generalises over: which mutation the write is (an
//! insert, a change, a removal), which `StreamViewType` the stream captures (each of the four
//! the model declares), which `StartingPosition` the mapping reads from, how many items are
//! written and how many of them one batch may hold, and which group the function logs to (the
//! default, and one its `LoggingConfig` names). Each row is its OWN test, over an engine of its
//! own — a test of this target runs in its own process, so the rows run beside each other and no
//! test carries more than one row's work. The last two tests vary the remaining dimensions —
//! WHEN the table's stream was turned on: a table created without one, whose stream
//! `UpdateTable` enables afterwards, is consumed exactly as a table created with one is; and
//! WHICH attempt the mapping reports once a batch has been logged, over a mapping that gave up
//! on the batch before it.
//!
//! No step waits a span of its own choosing and no assertion reads the clock: each WRITE opens
//! one deadline ([`ROW_BUDGET`]) for the chain it starts and every look-again after it shares
//! that one — the last of them, the mapping's own report, holding a share back
//! ([`REPORT_RESERVE`]) so the wait before it cannot spend the whole patience — so the row is as
//! patient as the emulator's own run limits allow and still terminates. The deadline is opened
//! by the write rather than before the fixture the write needs, so none of it is spent standing
//! a table, a function and a mapping up, and a chain that follows an attempt the mapping gave up
//! on opens its own.
//!
//! Nothing here waits a span of its own choosing: every state a row needs is waited for by
//! looking again until THE STATE IT ASSERTS holds, the mapping's own report of its last
//! attempt included — a member every pass the poller makes overwrites — so no assertion depends
//! on how fast the host runs or on which pass happened to report first. The two bounds that
//! could still turn the host's load into a FAILED attempt — the wall clock one run may spend
//! and the memory it may hold — are the function's own, and the fixture that creates these
//! functions gives both all the room the emulator has to give
//! ([`crate::invocation_log_delivery::function_timeout_seconds`],
//! [`crate::invocation_log_delivery::function_memory_size_mb`]), because no row here is about
//! either.
//!
//! Every test here RUNS handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves at
//! most `max_concurrent_runs` invocations at once for the whole process, and a suite that keeps
//! several of those slots busy starves every other suite's runs.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::json;
use service_com_amazonaws_lambda_awsgirapiservice::state::execution::{
    HOST_WAIT_CEILING, PERMIT_WAIT,
};
use service_com_amazonaws_lambda_awsgirapiservice::state::{
    LAST_PROCESSING_RESULT_FUNCTION_CALL_FAILED, LAST_PROCESSING_RESULT_OK,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{
    dynamodb, given_table, results_until, KEY, LATEST, NEW_AND_OLD_IMAGES, TRIM_HORIZON,
};
use crate::event_source_mappings::{
    get_mapping, given_enabled_mapping, router_on, CLOCK_BUDGET, LOOK_AGAIN,
};
use crate::invocation_log_delivery::{
    create_function, events, groups, streams, Created, PYTHON_RUNTIME,
};

/// How long the chain ONE write starts is given for every state the clock owes it together —
/// the mapping's poller taking its batches, the invocations running, their logs reaching the
/// group and the mapping reporting the attempt that consumed the batch.
///
/// PATIENCE, not a weakened expectation: every assertion below is unchanged, and the steps SHARE
/// this one deadline rather than each taking a span of its own, so the wait is as patient as it
/// can be and still terminates well inside the time a test may take. The bound is taken from
/// what the EMULATOR itself allows an invocation rather than picked here — the wait for a share
/// of the run capacity ([`PERMIT_WAIT`]) and the wait for the run's own answer
/// ([`HOST_WAIT_CEILING`]), for each of the two batches a row can be delivered in — with room on
/// top, because a whole-workspace run shares its cores with every other suite. It is opened by
/// the write, so none of it is spent on the fixture that write needs.
const ROW_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_mul(2)
    .saturating_add(Duration::from_secs(60));

/// The share of [`ROW_BUDGET`] kept back for the LAST state a row waits for — the mapping's own
/// report of the attempt that consumed the batch — so the wait for the log before it cannot
/// leave that wait with no patience at all.
///
/// Taken from the same place the budget itself is: what the emulator allows ONE invocation, the
/// wait for a share of the run capacity ([`PERMIT_WAIT`]) beside the wait for the run's own
/// answer ([`HOST_WAIT_CEILING`]). That is the span the attempt a row is waiting for can still
/// take once a line of an earlier pass is already in the group: such a pass is retried, and the
/// retry is an invocation like any other. The row's whole patience is unchanged — this only
/// decides how the row SPENDS it, and both halves stay far above what either state takes.
const REPORT_RESERVE: Duration = PERMIT_WAIT.saturating_add(HOST_WAIT_CEILING);

/// The instant the wait for the log of a row that shares `deadline` stops at: the row's own
/// deadline less the share kept back for the report that follows it ([`REPORT_RESERVE`]).
fn log_deadline(deadline: Instant) -> Instant {
    deadline.checked_sub(REPORT_RESERVE).unwrap_or(deadline)
}

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern. No IAM is
/// mounted beside the services below, so nothing is known about the role and no read or write
/// of the chain is withheld from it.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The default group a function's invocations are logged to: "By default, Lambda functions send
/// logs to a default log group named `/aws/lambda/<function name>`"
/// (`LoggingConfig$LogGroup`).
const DEFAULT_GROUP_PREFIX: &str = "/aws/lambda/";

/// What a table stream's ARN opens with (`arn:…:dynamodb:…:table/<name>/stream/<label>`), which
/// is what a stream a client can map has to be reported as.
const STREAM_ARN_PREFIX: &str = "arn:aws:dynamodb:";

/// The `StreamViewType` values the model declares beside [`NEW_AND_OLD_IMAGES`]
/// (`smithy_query(query="shape", shape="StreamViewType")`, DynamoDB): what the stream captures
/// for each change.
const KEYS_ONLY: &str = "KEYS_ONLY";
/// See [`KEYS_ONLY`].
const NEW_IMAGE: &str = "NEW_IMAGE";
/// See [`KEYS_ONLY`].
const OLD_IMAGE: &str = "OLD_IMAGE";

/// The `OperationType` a stream record reports for each kind of change
/// (`smithy_query(query="shape", shape="OperationType")`, DynamoDB Streams), which the handler
/// reads off the record and writes into its line.
const INSERT: &str = "INSERT";
/// See [`INSERT`].
const MODIFY: &str = "MODIFY";
/// See [`INSERT`].
const REMOVE: &str = "REMOVE";

/// The `eventSource` a record of a table's stream names itself with, which the handler refuses
/// to process anything else under.
const EVENT_SOURCE: &str = "aws:dynamodb";

/// What the handler prints before each record's own values. ONE spelling of it, bound into the
/// handler's source and into what the assertions look for.
const MARKER: &str = "stream record: ";

/// The key whose record the handler REFUSES to process, so the batch holding it is one the
/// mapping attempts and gives up on. Nothing in the emulator reads it: the handler reads it off
/// the record's own keys, and no other row writes an item keyed with it.
const REFUSED: &str = "refused";

/// How many attempts a mapping gives one batch when it allows no retry: "Discard records after
/// the specified number of retries. The default value is infinite (-1)"
/// (`MaximumRetryAttempts`), so a mapping stating zero retries attempts a batch once and then
/// reads on from past it.
const NO_RETRIES: i64 = 0;

/// How many records one batch of the mapping below holds, so each record is a batch and an
/// attempt of its own.
const ONE_RECORD_PER_BATCH: i64 = 1;

/// The handler every function below runs: one printed line per record of the batch it was
/// handed, carrying that record's own mutation and key, so a line in the log group is a
/// consequence of the record having reached the handler.
///
/// It also checks the envelope Lambda hands a stream batch over as, so a batch delivered in
/// some other shape fails the run instead of being logged, and it raises for the one key the
/// rows below refuse a record under ([`REFUSED`]) — before printing anything about it, so such
/// a record has no line of its own at all.
fn logging_handler() -> String {
    format!(
        r#"
MARKER = "{MARKER}"
EVENT_SOURCE = "{EVENT_SOURCE}"
KEY = "{KEY}"
REFUSED = "{REFUSED}"


def lambda_handler(event, context):
    records = event["Records"]
    if not records:
        raise ValueError("a batch was handed over holding no record")
    for record in records:
        if record["eventSource"] != EVENT_SOURCE:
            raise ValueError("eventSource is " + repr(record["eventSource"]))
        if not record["eventSourceARN"]:
            raise ValueError("a record carries no eventSourceARN")
        change = record["dynamodb"]
        key = change["Keys"][KEY]["S"]
        if key.startswith(REFUSED):
            raise RuntimeError("the handler cannot process " + key)
        print(MARKER + record["eventName"] + " " + key)
    return len(records)
"#
    )
}

/// The line the handler writes for the record of `mutation` on the item keyed `key`.
fn expected_line(mutation: &str, key: &str) -> String {
    format!("{MARKER}{mutation} {key}")
}

/// Lambda, DynamoDB, DynamoDB Streams and CloudWatch Logs over ONE shared engine — the four
/// services this chain spans, mounted the way the production router mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    router.register_service(
        service_com_amazonaws_dynamodb_dynamodb_20120810::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_dynamodbstreams_dynamodbstreams_20120810::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router
}

/// One mutation a row makes on the table, as the write that makes it and the record it is
/// captured as.
#[derive(Debug, Clone, Copy)]
enum Change {
    /// An item that was not there is written: `PutItem`.
    Insert,
    /// An item that was there is written again with another value: a second `PutItem`.
    Modify,
    /// An item that was there is taken away: `DeleteItem`.
    Remove,
}

/// One row: what the stream captures, where the mapping reads from, how many items are written
/// and in what batches, which mutation each write is, and the group the function names.
struct Row {
    case: &'static str,
    view_type: &'static str,
    starting_position: &'static str,
    /// How many items the row writes, each keyed `item-<n>`.
    items: usize,
    /// What is done to each of them.
    change: Change,
    batch_size: i64,
    /// The group the function's `LoggingConfig` names, or `None` to take the default.
    log_group: Option<&'static str>,
}

/// Every record the row's writes produce, in the order the stream captures them: a change of an
/// item that was not there yet is an insert FIRST and the change second, because the item has
/// to be put before it can be changed or taken away.
fn expected_lines(row: &Row) -> Vec<String> {
    let mut wanted = Vec::new();
    for index in 0..row.items {
        let key = item_key(index);
        wanted.push(expected_line(INSERT, &key));
        match row.change {
            Change::Insert => {}
            Change::Modify => wanted.push(expected_line(MODIFY, &key)),
            Change::Remove => wanted.push(expected_line(REMOVE, &key)),
        }
    }
    wanted
}

/// The key of the `index`th item a row writes.
fn item_key(index: usize) -> String {
    format!("item-{index}")
}

/// Make the row's writes on `table` through DynamoDB's own surface, which is the only action a
/// client takes: everything after this is the mapping's own work.
async fn write_items(router: &MultiServiceRouter, case: &str, table: &str, row: &Row) {
    for index in 0..row.items {
        let key = item_key(index);
        put_item(router, case, table, &key, "written").await;
        match row.change {
            Change::Insert => {}
            Change::Modify => put_item(router, case, table, &key, "changed").await,
            Change::Remove => delete_item(router, case, table, &key).await,
        }
    }
}

/// `PutItem` one item keyed `key`, carrying `payload`.
async fn put_item(router: &MultiServiceRouter, case: &str, table: &str, key: &str, payload: &str) {
    let (status, answered) = dynamodb(
        router,
        "PutItem",
        &json!({
            "TableName": table,
            "Item": { KEY: { "S": key }, "payload": { "S": payload } },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: PutItem({key:?}) answered {status} {answered}"
    );
}

/// `DeleteItem` the item keyed `key`.
async fn delete_item(router: &MultiServiceRouter, case: &str, table: &str, key: &str) {
    let (status, answered) = dynamodb(
        router,
        "DeleteItem",
        &json!({ "TableName": table, "Key": { KEY: { "S": key } } }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: DeleteItem({key:?}) answered {status} {answered}"
    );
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

/// The catalog, one row per test so that no test carries more than one row's work: each mutation
/// a stream captures, every view type the model declares, both starting positions a stream
/// source admits, one item and several, a batch ceiling above what is waiting and one record per
/// batch, and both the default log group and one the function's `LoggingConfig` names.
fn inserted_row() -> Row {
    Row {
        case: "one insert, captured as both images, read from the horizon, default group",
        view_type: NEW_AND_OLD_IMAGES,
        starting_position: TRIM_HORIZON,
        items: 1,
        change: Change::Insert,
        batch_size: 10,
        log_group: None,
    }
}

/// See [`inserted_row`]: two items inserted and read from the LATEST position, in one batch.
fn inserted_pair_row() -> Row {
    Row {
        case: "two inserts in one batch, keys only, read from the latest position",
        view_type: KEYS_ONLY,
        starting_position: LATEST,
        items: 2,
        change: Change::Insert,
        batch_size: 10,
        log_group: None,
    }
}

/// See [`inserted_row`]: an item CHANGED, whose two records are read one per batch, and whose
/// function logs to a group its own `LoggingConfig` names.
fn changed_row() -> Row {
    Row {
        case: "a change of an item, new image, one record per batch, a group the function names",
        view_type: NEW_IMAGE,
        starting_position: TRIM_HORIZON,
        items: 1,
        change: Change::Modify,
        batch_size: 1,
        log_group: Some("/aws/lambda/ddb-stream-mapping-named-group"),
    }
}

/// See [`inserted_row`]: an item REMOVED.
fn removed_row() -> Row {
    Row {
        case: "a removal of an item, old image, read from the horizon",
        view_type: OLD_IMAGE,
        starting_position: TRIM_HORIZON,
        items: 1,
        change: Change::Remove,
        batch_size: 10,
        log_group: None,
    }
}

/// Stand one row up on a table whose stream `stream_arn` names, write its items, and assert the
/// handler's own line for every record the writes produced is in the function's group — and
/// that the mapping reports the attempt that produced them.
///
/// Every state the clock owes the row AFTER the write is waited for against ONE deadline
/// ([`ROW_BUDGET`]), so those steps share the row's patience instead of each taking a span of
/// its own. The deadline is opened by the WRITE, the one client action the chain follows from,
/// rather than before the fixture the chain needs: standing a table, a function and a mapping
/// up is work the clock owes too, each piece of it already waited for by looking again
/// (`crate::event_source_mappings::CLOCK_BUDGET`), and patience spent there is patience the
/// delivery never had.
async fn assert_delivered_and_logged(
    router: &MultiServiceRouter,
    row: &Row,
    function: &str,
    table: &str,
    stream_arn: &str,
) {
    let case = row.case;
    let source = logging_handler();
    create_function(
        router,
        &Created {
            case,
            name: function,
            source: &source,
            runtime: PYTHON_RUNTIME,
            role: ROLE,
            log_group: row.log_group,
        },
    )
    .await;
    let uuid = given_enabled_mapping(
        router,
        case,
        &json!({
            "FunctionName": function,
            "EventSourceArn": stream_arn,
            "StartingPosition": row.starting_position,
            "BatchSize": row.batch_size,
            "Enabled": true,
        }),
    )
    .await;

    // The ONE client action: the items are written where a client writes them. Everything
    // after it is the mapping's own work, and the row's patience is for THAT.
    write_items(router, case, table, row).await;
    let deadline = Instant::now() + ROW_BUDGET;

    // Where the log has to be: the group the function's configuration names, else the default
    // its own documentation gives.
    let group = row
        .log_group
        .map_or_else(|| format!("{DEFAULT_GROUP_PREFIX}{function}"), str::to_string);
    let wanted = expected_lines(row);
    let lines = logged(router, case, &group, &wanted, log_deadline(deadline)).await;
    let missing: Vec<&String> = wanted
        .iter()
        .filter(|line| !lines.iter().any(|written| written.contains(line.as_str())))
        .collect();
    // What the mapping says about its own last attempt is read only to SAY why a delivery that
    // did not arrive did not arrive — a run the host cut off is reported there — so the row
    // that fails names the cause instead of only the lines it did not find.
    assert!(
        missing.is_empty(),
        "case {case:?}: the handler's lines {missing:?} must be delivered to {group:?}, got \
         {lines:?}; the mapping reports {}",
        get_mapping(router, &uuid).await.text()
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
    // attempt, because every pass the mapping makes overwrites the member: a row that took the
    // first result it found could be reading a pass that was retried — one the host was too
    // busy to answer in time, say — rather than the one that consumed the batch the log above
    // only proves the delivery of. The wait shares the row's own deadline, like every other
    // look-again here.
    let reported = results_until(router, case, &uuid, LAST_PROCESSING_RESULT_OK, deadline).await;
    assert_eq!(
        reported.last().map(String::as_str),
        Some(LAST_PROCESSING_RESULT_OK),
        "case {case:?}: the mapping must report the attempt that consumed its batch as \
         {LAST_PROCESSING_RESULT_OK}, saw {reported:?}"
    );
}

/// Stand one row up on a table created WITH its stream, and assert the rule over it.
///
/// `label` keeps one test's table and function names apart from another's; every row runs on an
/// engine of its own.
async fn assert_row_on_a_stream_enabled_table(label: &str, row: &Row) {
    let router = emulator();
    let name = format!("ddb-stream-log-{label}");
    let stream_arn = given_table(&router, row.case, &name, Some(row.view_type)).await;
    assert!(
        !stream_arn.is_empty(),
        "case {:?}: the table created with a stream must report its LatestStreamArn",
        row.case
    );
    assert_delivered_and_logged(&router, row, &name, &name, &stream_arn).await;
}

/// RULE: a change a table's stream captures is handed to the function of the mapping that polls
/// that stream with no client action, and that invocation's log — the handler's own lines
/// included — is delivered to the function's log group.
#[tokio::test]
async fn an_inserted_item_is_delivered_and_logged_to_the_functions_group() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    assert_row_on_a_stream_enabled_table("insert", &inserted_row()).await;
}

/// The same RULE over several records of one batch, read from the position a mapping created
/// `LATEST` starts at.
#[tokio::test]
async fn two_inserted_items_are_delivered_and_logged_to_the_functions_group() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    assert_row_on_a_stream_enabled_table("pair", &inserted_pair_row()).await;
}

/// The same RULE over a CHANGE of an item, read one record per batch and logged to the group the
/// function's own `LoggingConfig` names.
#[tokio::test]
async fn a_changed_item_is_delivered_and_logged_to_the_group_the_function_names() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    assert_row_on_a_stream_enabled_table("change", &changed_row()).await;
}

/// The same RULE over a REMOVAL, the third mutation a stream captures.
#[tokio::test]
async fn a_removed_item_is_delivered_and_logged_to_the_functions_group() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    assert_row_on_a_stream_enabled_table("remove", &removed_row()).await;
}

/// RULE, over the remaining dimension: a stream `UpdateTable` turned on AFTER the table was
/// created is polled by its mapping exactly as one the create enabled — "`StreamSpecification`
/// … represents the DynamoDB Streams configuration for the table" — so the change written
/// afterwards reaches the function and is logged by it.
#[tokio::test]
async fn a_stream_enabled_after_the_create_is_consumed_by_its_mapping() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let row = Row {
        case: "a stream UpdateTable enabled on a table created without one",
        view_type: NEW_AND_OLD_IMAGES,
        starting_position: TRIM_HORIZON,
        items: 1,
        change: Change::Insert,
        batch_size: 10,
        log_group: None,
    };
    let router = emulator();
    let table = "ddb-stream-enabled-later";
    let function = "ddb-stream-enabled-later";

    // A table with NO stream, exactly as a client creates one before deciding to consume it.
    let none = given_table(&router, row.case, table, None).await;
    assert!(
        none.is_empty(),
        "case {:?}: a table created without a stream reports no LatestStreamArn, got {none:?}",
        row.case
    );

    // Then the stream is turned on, and the ARN it is minted under is the one the table reports.
    let (status, updated) = dynamodb(
        &router,
        "UpdateTable",
        &json!({
            "TableName": table,
            "StreamSpecification": { "StreamEnabled": true, "StreamViewType": row.view_type },
        }),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {:?}: UpdateTable answered {status} {updated}",
        row.case
    );
    let stream_arn = await_stream_arn(&router, row.case, table, Instant::now() + CLOCK_BUDGET).await;
    assert!(
        stream_arn.starts_with(STREAM_ARN_PREFIX),
        "case {:?}: the enabled stream must be reported as an ARN, got {stream_arn:?}",
        row.case
    );

    assert_delivered_and_logged(&router, &row, function, table, &stream_arn).await;
}

/// RULE, over the remaining dimension of what the pair reports: `LastProcessingResult` is "the
/// result of the event source mapping's LAST processing attempt", and every pass the mapping
/// makes overwrites it — so once a batch has been delivered and logged, the attempt the mapping
/// reports is the one that CONSUMED that batch, whatever it came to on a batch before it.
///
/// This row FORCES the earlier attempt rather than leaving it to a host that happens to be busy
/// enough to make one: a mapping that allows no retry is handed a record its handler refuses,
/// gives up on that batch, and then consumes the record written behind it. So a reader of that
/// member which took whatever result it found first — the attempt that was given up on — would
/// be reading a pass that is not the one it is waiting for, which is what every row above waits
/// past.
#[tokio::test]
async fn the_attempt_that_consumed_the_batch_is_what_the_mapping_reports() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a batch given up on, then one consumed, read one record per batch";
    let router = emulator();
    let name = "ddb-stream-log-given-up";
    let stream_arn = given_table(&router, case, name, Some(NEW_AND_OLD_IMAGES)).await;
    let source = logging_handler();
    create_function(
        &router,
        &Created {
            case,
            name,
            source: &source,
            runtime: PYTHON_RUNTIME,
            role: ROLE,
            log_group: None,
        },
    )
    .await;
    let uuid = given_enabled_mapping(
        &router,
        case,
        &json!({
            "FunctionName": name,
            "EventSourceArn": stream_arn,
            "StartingPosition": TRIM_HORIZON,
            "BatchSize": ONE_RECORD_PER_BATCH,
            "MaximumRetryAttempts": NO_RETRIES,
            "Enabled": true,
        }),
    )
    .await;

    // The record the handler refuses: the one attempt the mapping gives that batch is spent on
    // it, so the mapping reports the attempt it gave up on and reads on from past the record.
    // Each write opens the patience for the chain it starts, so the attempt that is given up on
    // does not spend the patience of the delivery behind it.
    let refused_key = format!("{REFUSED}-0");
    put_item(&router, case, name, &refused_key, "written").await;
    let given_up = results_until(
        &router,
        case,
        &uuid,
        LAST_PROCESSING_RESULT_FUNCTION_CALL_FAILED,
        Instant::now() + ROW_BUDGET,
    )
    .await;
    assert_eq!(
        given_up.last().map(String::as_str),
        Some(LAST_PROCESSING_RESULT_FUNCTION_CALL_FAILED),
        "case {case:?}: the attempt its handler refused must be reported as \
         {LAST_PROCESSING_RESULT_FUNCTION_CALL_FAILED}, saw {given_up:?}"
    );

    // Then the record behind it, which the handler does process: its line is delivered to the
    // function's group exactly as it is for a mapping that gave up on nothing.
    let key = item_key(0);
    put_item(&router, case, name, &key, "written").await;
    let deadline = Instant::now() + ROW_BUDGET;
    let group = format!("{DEFAULT_GROUP_PREFIX}{name}");
    let wanted = expected_line(INSERT, &key);
    let lines = logged(
        &router,
        case,
        &group,
        std::slice::from_ref(&wanted),
        log_deadline(deadline),
    )
    .await;
    assert!(
        lines.iter().any(|written| written.contains(&wanted)),
        "case {case:?}: the handler's line {wanted:?} must be delivered to {group:?}, got \
         {lines:?}"
    );
    let refused_line = expected_line(INSERT, &refused_key);
    assert!(
        !lines.iter().any(|written| written.contains(&refused_line)),
        "case {case:?}: the record the handler refused must have no line of its own, got \
         {lines:?}"
    );

    // And what the mapping reports is the attempt that consumed THAT batch, not the one it gave
    // up on before it.
    let reported = results_until(&router, case, &uuid, LAST_PROCESSING_RESULT_OK, deadline).await;
    assert_eq!(
        reported.last().map(String::as_str),
        Some(LAST_PROCESSING_RESULT_OK),
        "case {case:?}: the mapping must report the attempt that consumed its batch as \
         {LAST_PROCESSING_RESULT_OK}, saw {reported:?}"
    );
}

/// Wait until `DescribeTable` reports the table `ACTIVE` with a stream, and answer that
/// stream's ARN: enabling a stream is part of a table update, which the table reports as
/// `UPDATING` until it is done.
async fn await_stream_arn(
    router: &MultiServiceRouter,
    case: &str,
    table: &str,
    deadline: Instant,
) -> String {
    loop {
        let (status, read) = dynamodb(router, "DescribeTable", &json!({ "TableName": table })).await;
        assert_eq!(
            status, 200,
            "case {case:?}: DescribeTable answered {status} {read}"
        );
        let description = &read["Table"];
        let arn = description["LatestStreamArn"].as_str().unwrap_or_default();
        if description["TableStatus"] == json!("ACTIVE") && !arn.is_empty() {
            return arn.to_string();
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the table never reported an enabled stream: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
