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
//! test carries more than one row's work. The last test varies the remaining dimension — WHEN
//! the table's stream was turned on: a table created without one, whose stream `UpdateTable`
//! enables afterwards, is consumed exactly as a table created with one is.
//!
//! No step waits a span of its own choosing and no assertion reads the clock: each row opens one
//! deadline ([`ROW_BUDGET`]) and every look-again below shares it, so the row is as patient as
//! the emulator's own run limits allow and still terminates.
//!
//! Nothing here waits a span of its own choosing: every state a row needs is waited for by
//! looking again until it holds, so no assertion depends on how fast the host runs.
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
use service_com_amazonaws_lambda_awsgirapiservice::state::LAST_PROCESSING_RESULT_OK;
use smithy_server_runtime::MultiServiceRouter;

use crate::dynamodb_stream_event_source_delivery::{
    dynamodb, given_table, KEY, LATEST, NEW_AND_OLD_IMAGES, TRIM_HORIZON,
};
use crate::event_source_mappings::{
    given_enabled_mapping, processing_result_within_budget, router_on, LOOK_AGAIN,
};
use crate::invocation_log_delivery::{
    create_function, events, groups, streams, Created, PYTHON_RUNTIME,
};

/// How long ONE row is given for every state the clock owes it together — the mapping's poller
/// taking its batches, the invocations running and their logs reaching the group.
///
/// PATIENCE, not a weakened expectation: every assertion below is unchanged, and the steps SHARE
/// this one deadline rather than each taking a span of its own, so the row is as patient as it
/// can be and still terminates well inside the time a test may take. The bound is taken from
/// what the EMULATOR itself allows an invocation rather than picked here — the wait for a share
/// of the run capacity ([`PERMIT_WAIT`]) and the wait for the run's own answer
/// ([`HOST_WAIT_CEILING`]), for each of the two batches a row can be delivered in — with room on
/// top, because a whole-workspace run shares its cores with every other suite.
const ROW_BUDGET: Duration = PERMIT_WAIT
    .saturating_add(HOST_WAIT_CEILING)
    .saturating_mul(2)
    .saturating_add(Duration::from_secs(60));

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

/// The handler every function below runs: one printed line per record of the batch it was
/// handed, carrying that record's own mutation and key, so a line in the log group is a
/// consequence of the record having reached the handler.
///
/// It also checks the envelope Lambda hands a stream batch over as, so a batch delivered in
/// some other shape fails the run instead of being logged.
fn logging_handler() -> String {
    format!(
        r#"
MARKER = "{MARKER}"
EVENT_SOURCE = "{EVENT_SOURCE}"
KEY = "{KEY}"


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
/// Every state the clock owes the row is waited for against ONE deadline opened by its caller,
/// so the steps share the row's patience instead of each taking a span of its own.
async fn assert_delivered_and_logged(
    router: &MultiServiceRouter,
    row: &Row,
    function: &str,
    table: &str,
    stream_arn: &str,
    deadline: Instant,
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

    // The ONE client action: the items are written where a client writes them.
    write_items(router, case, table, row).await;

    // Where the log has to be: the group the function's configuration names, else the default
    // its own documentation gives.
    let group = row
        .log_group
        .map_or_else(|| format!("{DEFAULT_GROUP_PREFIX}{function}"), str::to_string);
    let wanted = expected_lines(row);
    let lines = logged(router, case, &group, &wanted, deadline).await;
    for line in &wanted {
        assert!(
            lines.iter().any(|written| written.contains(line)),
            "case {case:?}: the handler's line {line:?} must be delivered to {group:?}, got \
             {lines:?}"
        );
    }
    // The group the lines are in is the one the rule names, reported by CloudWatch Logs itself
    // rather than inferred from the read above.
    let named = groups(router, case, &group).await;
    assert!(
        named.iter().any(|found| found == &group),
        "case {case:?}: the delivery must have created {group:?}, got {named:?}"
    );
    // And the mapping says what its attempt at the batch came to, which is the other half a
    // client watches a consumer by. Read through the suite harness's ONE waiter for that
    // member, because the mapping's record is written by the poller that took the batch: a row
    // that asked once would be reading an instant of its own choosing in the middle of the
    // chain the log read above only proves the start of.
    let reported = processing_result_within_budget(router, case, &uuid).await;
    assert_eq!(
        reported.as_deref(),
        Some(LAST_PROCESSING_RESULT_OK),
        "case {case:?}: the mapping must report the attempt that consumed its batch as \
         {LAST_PROCESSING_RESULT_OK}, got {reported:?}"
    );
}

/// Stand one row up on a table created WITH its stream, and assert the rule over it.
///
/// `label` keeps one test's table and function names apart from another's; every row runs on an
/// engine of its own.
async fn assert_row_on_a_stream_enabled_table(label: &str, row: &Row) {
    let deadline = Instant::now() + ROW_BUDGET;
    let router = emulator();
    let name = format!("ddb-stream-log-{label}");
    let stream_arn = given_table(&router, row.case, &name, Some(row.view_type)).await;
    assert!(
        !stream_arn.is_empty(),
        "case {:?}: the table created with a stream must report its LatestStreamArn",
        row.case
    );
    assert_delivered_and_logged(&router, row, &name, &name, &stream_arn, deadline).await;
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
    let deadline = Instant::now() + ROW_BUDGET;
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
    let stream_arn = await_stream_arn(&router, row.case, table, deadline).await;
    assert!(
        stream_arn.starts_with(STREAM_ARN_PREFIX),
        "case {:?}: the enabled stream must be reported as an ARN, got {stream_arn:?}",
        row.case
    );

    assert_delivered_and_logged(&router, &row, function, table, &stream_arn, deadline).await;
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
