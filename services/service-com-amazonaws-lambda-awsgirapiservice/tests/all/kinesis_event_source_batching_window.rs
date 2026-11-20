// SPDX-License-Identifier: CC0-1.0

//! The BATCHING WINDOW of an event source mapping on an Amazon Kinesis data stream, end to
//! end over the wire with no client action after the records are put: Lambda reads the
//! stream's shards itself, gathers what it reads, and invokes the mapped function once per
//! batch.
//!
//! The RULE asserted over the tables below: a mapping hands its function a batch for exactly
//! two reasons — the batch holds `BatchSize`, "the maximum number of records in each batch
//! that Lambda pulls from your stream or queue", or it has been gathering for the whole of
//! `MaximumBatchingWindowInSeconds`, "the maximum amount of time, in seconds, that Lambda
//! spends gathering records before invoking the function". Both are MAXIMA, so a batch that
//! fills goes without waiting the rest of the window out, a window that has run hands over
//! whatever was gathered however small it is, a mapping carrying the stream default of 0
//! seconds hands over what one read found, and a mapping whose window has NOT run holds its
//! partial batch — which is what makes it invoke its function with the records that
//! accumulated over the window rather than with each record as it arrives.
//!
//! Nothing here invokes the function: every record read off a sink stream was put there by a
//! handler the poller invoked, which writes ONE record per INVOCATION carrying how many
//! records that invocation was handed — so the sink spells the batches the mapping handed
//! over, and a mapping that gathered nothing, or handed a partial batch over early, is a
//! sink holding other batches than the row expects.
//!
//! Nothing here waits a fixed span either. What paces the first test is another MAPPING's
//! delivery: a mapping on its own stream carrying no window hands its records over as it
//! reads them, so its sink filling is the proof that the pollers have made a pass — and the
//! records the row then puts are ones that arrived AFTER the gathering mapping had already
//! read, and held, the records before them.
//!
//! The rows of the second table differ along what the rule generalises over: the window
//! configured (absent, the published minimum of `@range(min: 0, max: 300)` stated, a span
//! between the bounds, and the maximum), how many records are waiting, and whether
//! `BatchSize` fills before the window runs. The first table's rows differ along how the
//! records are split between the two arrivals and the `BatchSize` the second arrival fills.
//!
//! The Lambda, Kinesis and mapping helpers are the ones `kinesis_event_source_delivery.rs`
//! publishes, the emulator shape and the sink reader are
//! `kinesis_event_source_handler_writes_kinesis.rs`'s and the role fixture is
//! `event_source_mapping_role_access.rs`'s, so every suite over this surface drives the same
//! wire.

use std::time::{Duration, Instant};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::STARTING_POSITION_TRIM_HORIZON;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role, logs_only, KINESIS_READS};
use crate::kinesis_event_source_delivery::{
    get_mapping, given_stream, lambda, put_records, LOOK_AGAIN, MAPPINGS_URI,
};
use crate::kinesis_event_source_handler_writes_kinesis::{emulator, SinkReader};
use crate::synchronous_invoke_run::package;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The runtime every function below is created with, and the module and attribute its
/// package exposes its handler as.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation of this handler is given, as the function's own `Timeout`: well
/// past what a run needs, because what is under test is the batching and not how fast an
/// interpreter gets through a handler.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long a wait for work nothing asked for — a stream or a function settling, a batch
/// gathered and handed over, the handler's own write landing — is given before the row is a
/// failure. Well above the poller's interval, the state-settle span, the longest window any
/// row below gathers over and one interpreted run of the handler, so a row fails only when
/// the work did not happen at all.
const BUDGET: Duration = Duration::from_secs(120);

/// The one `State` a mapping that is reading rests in.
const STATE_ENABLED: &str = "Enabled";

/// The published bounds of `MaximumBatchingWindowInSeconds` (`@range(min: 0, max: 300)`), and
/// a span between them short enough for a row to wait out.
const WINDOW_MIN: i64 = 0;
/// See [`WINDOW_MIN`].
const WINDOW_MAX: i64 = 300;
/// See [`WINDOW_MIN`].
const WINDOW_MID: i64 = 2;

/// The handler every function below runs: it puts ONE record onto the sink stream per
/// INVOCATION, carrying how many records that invocation was handed and the `eventID` of
/// each of them.
///
/// It computes nothing and asserts nothing: a record read off the sink is the shape of ONE
/// batch the poller handed over, so every count this suite checks is the emulator's own.
const HANDLER_SOURCE: &str = r#"
import json
import os

import boto3

SINK = os.environ["SINK_STREAM"]
CLIENT = boto3.client("kinesis")


def lambda_handler(event, context):
    records = event["Records"]
    batch = {"size": len(records), "ids": [record["eventID"] for record in records]}
    CLIENT.put_record(
        StreamName=SINK,
        Data=json.dumps(batch).encode("utf-8"),
        PartitionKey="batch",
    )
    return batch
"#;

/// A function whose package RUNS [`HANDLER_SOURCE`], running as `role_arn` and told through
/// its environment which stream to write, waited out to `Active`.
async fn given_handler_function(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    role_arn: &str,
    sink: &str,
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
            "Environment": { "Variables": { "SINK_STREAM": sink } },
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
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

/// Create an `Enabled` mapping from `stream_arn` to `function`, reading from the oldest
/// record the stream holds, with `BatchSize` and the row's window, waited out to the one
/// `State` a reading mapping rests in.
async fn given_enabled_mapping(
    router: &MultiServiceRouter,
    case: &str,
    function: &str,
    stream_arn: &str,
    batch_size: i64,
    window: Option<i64>,
) -> String {
    let mut body = json!({
        "FunctionName": function,
        "EventSourceArn": stream_arn,
        "StartingPosition": STARTING_POSITION_TRIM_HORIZON,
        "BatchSize": batch_size,
        "Enabled": true,
    });
    if let Some(window) = window {
        body["MaximumBatchingWindowInSeconds"] = json!(window);
    }
    let (status, created) = lambda(router, "POST", MAPPINGS_URI, &body).await;
    assert_eq!(
        status, 202,
        "case {case:?}: CreateEventSourceMapping answered {status} {created}"
    );
    let uuid = created["UUID"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no UUID in {created}"))
        .to_string();
    let deadline = Instant::now() + BUDGET;
    loop {
        let read = get_mapping(router, case, &uuid).await;
        if read["State"] == json!(STATE_ENABLED) {
            return uuid;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: the mapping never reported {STATE_ENABLED}: {read}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// Everything one mapping of a row needs: the stream it reads, the sink its handler writes,
/// a reader of that sink, and the mapping itself.
struct Mapped {
    /// The source stream's name, which is what records are put to.
    stream: String,
    /// The reader of the sink the handler of this mapping writes one record per invocation
    /// to.
    sink: SinkReader,
}

/// A stream, a role, a handler function, a sink and an `Enabled` mapping wiring them
/// together, under names derived from `label` so one row's resources are its own.
async fn given_mapped_stream(
    router: &MultiServiceRouter,
    case: &str,
    label: &str,
    batch_size: i64,
    window: Option<i64>,
) -> Mapped {
    let stream = format!("{label}-source");
    let sink = format!("{label}-sink");
    let function = format!("{label}-batches");
    let source_arn = given_stream(router, case, &stream, 1).await;
    let sink_arn = given_stream(router, case, &sink, 1).await;
    // Lambda reads the source acting as the function's execution role, and the handler
    // writes the sink as itself: the role carries both grants, as a client wiring this
    // pipeline gives it.
    let role = given_role(
        router,
        case,
        &format!("{label}-role"),
        Some(json!([
            logs_only(),
            allow("kinesis", &KINESIS_READS, &source_arn),
            allow("kinesis", &["PutRecord"], &sink_arn),
        ])),
    )
    .await;
    given_handler_function(router, case, &function, &role, &sink).await;
    let reader = SinkReader::of(router, case, &sink).await;
    given_enabled_mapping(router, case, &function, &source_arn, batch_size, window).await;
    Mapped {
        stream,
        sink: reader,
    }
}

/// How many records each batch this mapping's handler has been handed held, in the order the
/// handler wrote them.
async fn batches(mapped: &mut Mapped, router: &MultiServiceRouter, case: &str) -> Vec<usize> {
    mapped
        .sink
        .drain(router, case)
        .await
        .iter()
        .map(|batch| {
            batch["size"]
                .as_u64()
                .and_then(|size| usize::try_from(size).ok())
                .unwrap_or_else(|| panic!("case {case:?}: a sink record carries no size: {batch}"))
        })
        .collect()
}

/// Wait until this mapping's handler has been handed `expected` records in total, answering
/// the batches it was handed; the deadline is this suite's own [`BUDGET`].
async fn wait_for_records(
    mapped: &mut Mapped,
    router: &MultiServiceRouter,
    case: &str,
    expected: usize,
) -> Vec<usize> {
    let mut held: Vec<usize> = Vec::new();
    let deadline = Instant::now() + BUDGET;
    loop {
        held.extend(batches(mapped, router, case).await);
        if held.iter().sum::<usize>() >= expected {
            return held;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the mapping to hand its function {expected} record(s), got \
             the batches {held:?}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}

/// One row of the gathering table: how the records are split between the two arrivals, and
/// the `BatchSize` the second arrival fills.
struct Gathering {
    case: &'static str,
    /// The records put before the pollers are known to have made a pass.
    first: usize,
    /// The records put after that pass, which the window has to gather into the same batch.
    later: usize,
    /// The `BatchSize` the two arrivals together fill, which is what sends the gathered
    /// batch.
    batch_size: i64,
}

/// RULE: a mapping whose `MaximumBatchingWindowInSeconds` has not run holds the partial batch
/// it read, and gathers the records that arrive afterwards into that SAME batch — so its
/// function is invoked once with every record of the window, and not once per arrival.
#[tokio::test]
async fn a_batching_window_gathers_the_records_that_arrive_while_it_runs() {
    let rows = [
        Gathering {
            case: "two records held, three more gathered",
            first: 2,
            later: 3,
            batch_size: 5,
        },
        Gathering {
            case: "one record held, five more gathered",
            first: 1,
            later: 5,
            batch_size: 6,
        },
    ];
    for row in rows {
        let case = row.case;
        let router = emulator();
        // The PACER: a mapping of its own carrying no window at all, which hands over what
        // one read found. Its sink filling is what says a poller has read the records that
        // were waiting — so the row never waits a fixed span to decide the gathering mapping
        // has had its chance.
        let mut pacer = given_mapped_stream(
            &router,
            case,
            &format!("pacer-{}", row.first),
            10,
            Some(WINDOW_MIN),
        )
        .await;
        // The mapping under test: the longest window the member admits, so nothing but its
        // `BatchSize` filling can send its batch, and that fills only once BOTH arrivals are
        // in it.
        let mut gathering = given_mapped_stream(
            &router,
            case,
            &format!("gathering-{}", row.first),
            row.batch_size,
            Some(WINDOW_MAX),
        )
        .await;

        put_records(&router, "pacer-first", &pacer.stream, row.first).await;
        put_records(&router, "gathering-first", &gathering.stream, row.first).await;
        // The pacer has handed its records over, so both pollers have read what was waiting.
        let paced = wait_for_records(&mut pacer, &router, case, row.first).await;
        assert_eq!(
            paced.iter().sum::<usize>(),
            row.first,
            "case {case:?}: a mapping carrying no window must hand over the {} record(s) it \
             read, got the batches {paced:?}",
            row.first
        );
        // And the gathering mapping has handed over NOTHING: its batch is short of its
        // `BatchSize` and its window has not run, so it holds what it read.
        let held = batches(&mut gathering, &router, case).await;
        assert!(
            held.is_empty(),
            "case {case:?}: a mapping gathering over {WINDOW_MAX}s must hold a batch of {} \
             record(s) that is short of its BatchSize of {}, it handed over {held:?}",
            row.first,
            row.batch_size
        );

        // The rest arrive while the window is still open, filling the batch that is being
        // gathered — so a mapping that gathers hands its function ONE batch of everything,
        // and one that does not would hand over the arrivals separately.
        put_records(&router, "gathering-later", &gathering.stream, row.later).await;
        let records = row.first + row.later;
        let gathered = wait_for_records(&mut gathering, &router, case, records).await;
        assert_eq!(
            gathered,
            vec![records],
            "case {case:?}: expected the window to gather both arrivals into one batch of \
             {records} record(s), got the batches {gathered:?}"
        );
    }
}

/// One row of the sending table: the window the mapping gathers over, how many records are
/// waiting for it, its `BatchSize`, and the batches it must hand over.
struct Sent {
    case: &'static str,
    window: Option<i64>,
    records: usize,
    batch_size: i64,
    /// The batches the function is handed, as their sizes in order.
    handed: &'static [usize],
}

/// RULE: a batch is handed over as soon as it holds `BatchSize` or its
/// `MaximumBatchingWindowInSeconds` has run, whichever comes first — so a mapping carrying no
/// window hands over what one read found, a window that has run hands a partial batch over,
/// and a full batch goes without the window being waited out.
#[tokio::test]
async fn a_batch_goes_when_it_is_full_or_its_window_has_run() {
    let rows = [
        // The stream default: a mapping stating no window at all.
        Sent {
            case: "no window stated hands over what one read found",
            window: None,
            records: 3,
            batch_size: 10,
            handed: &[3],
        },
        // The published minimum of the member's own range, stated.
        Sent {
            case: "the shortest window hands over what one read found",
            window: Some(WINDOW_MIN),
            records: 2,
            batch_size: 10,
            handed: &[2],
        },
        // A window between the bounds, which RUNS and sends a batch short of its size.
        Sent {
            case: "a window that runs hands over a partial batch",
            window: Some(WINDOW_MID),
            records: 3,
            batch_size: 10,
            handed: &[3],
        },
        // The published maximum, which nothing waits out: the batch fills instead.
        Sent {
            case: "a full batch goes without the longest window running",
            window: Some(WINDOW_MAX),
            records: 2,
            batch_size: 2,
            handed: &[2],
        },
    ];
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let mut mapped = given_mapped_stream(
            &router,
            case,
            &format!("sent-{index}"),
            row.batch_size,
            row.window,
        )
        .await;
        put_records(&router, case, &mapped.stream, row.records).await;
        let handed = wait_for_records(&mut mapped, &router, case, row.records).await;
        assert_eq!(
            handed,
            row.handed.to_vec(),
            "case {case:?}: expected a mapping of BatchSize {} gathering over {:?}s to hand its \
             function the batches {:?}, got {handed:?}",
            row.batch_size,
            row.window,
            row.handed
        );
    }
}
