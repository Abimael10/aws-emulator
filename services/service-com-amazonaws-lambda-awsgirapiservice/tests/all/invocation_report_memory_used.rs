// SPDX-License-Identifier: CC0-1.0

//! What the runtime's closing REPORT line states about ONE run, and where the handler's own
//! output sits in the log around it.
//!
//! RULE: every invocation a managed runtime runs closes its log with a `REPORT` line that
//! states what that run spent — tab separated after the invocation's request id: the
//! `Duration`, the `Billed Duration`, the `Memory Size` the function is configured for and
//! the `Max Memory Used` the run was measured using, a whole number of megabytes that is at
//! least one and never more than the configured size — and everything the handler itself
//! wrote is carried between the `START` line that opens the log and the `END` line that
//! closes it.
//!
//! So a client reading a run's log learns both what its own code said and what that run cost.
//! The figure is the host's own reading of what the run took, in the unit the field is stated
//! in, so what is asserted here is the rule's shape and range over every row rather than a
//! number a host and its allocator decide; the arithmetic that turns a reading into the field
//! is pinned by the owning module's own table
//! (`state::execution::tests::the_report_states_the_memory_a_run_was_measured_using`).
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda and
//! CloudWatch Logs mounted on ONE engine, and reads what the run produced: the log the
//! invocation itself reports (`X-Amz-Log-Result`, "the last 4 KB of the execution log") and,
//! for the delivery row, the events CloudWatch Logs holds for the function's group — which is
//! where a client reads them.
//!
//! The rows differ along what the rule generalises over: which RUNTIME ran the handler (python
//! and node, whose engines measure their own memory), the `MemorySize` the function is
//! configured for (the smallest the model allows and two larger ones), which channel the
//! handler wrote on (the standard output, the Python logging facility, and the node console's
//! error channel) and how much memory the run holds (nothing of its own, and a buffer of tens
//! of megabytes).
//!
//! Every test here RUNS handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use base64::Engine as _;
use serde_json::json;
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::{await_function_active, call};
use crate::invocation_log_delivery::{
    delivered, emulator, invocations_log, invoke, streams, NODE_RUNTIME, PYTHON_RUNTIME,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A well-formed execution role ARN. No IAM is mounted here, so nothing is known about the
/// role and no log write is withheld.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The fixed text of the line under test, and the fields it states what the run spent in, in
/// the order the runtime writes them. Source: the `REPORT RequestId: <id> Duration: <d> ms
/// Billed Duration: <b> ms Memory Size: <n> MB Max Memory Used: <m> MB` line Lambda writes at
/// the end of every invocation.
const REPORT_PREFIX: &str = "REPORT RequestId: ";
/// See [`REPORT_PREFIX`].
const REPORT_FIELDS: &[&str] = &[
    "Duration: ",
    "Billed Duration: ",
    "Memory Size: ",
    "Max Memory Used: ",
];
/// See [`REPORT_PREFIX`]: the field this suite is about.
const USED_FIELD: &str = "Max Memory Used: ";
/// The unit every memory field of that line is stated in.
const MEGABYTES: &str = " MB";

/// The lines that frame one run's log.
const START_PREFIX: &str = "START RequestId: ";
/// See [`START_PREFIX`].
const END_PREFIX: &str = "END RequestId: ";

/// How long each row's function may run for, in seconds.
const TIMEOUT_SECONDS: i64 = 10;

/// One row of the table: the handler, how its function is configured, what it must have
/// written, and whether it holds memory of its own.
struct Row {
    case: &'static str,
    /// The body of the handler.
    source: &'static str,
    /// The runtime that runs it, which decides the package's one entry and the handler name.
    runtime: &'static str,
    /// The `MemorySize` the function is created with, which the report must state.
    memory_size: i64,
    /// The lines the HANDLER itself must have contributed, each between the framing.
    written: &'static [&'static str],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a python handler that writes one line and holds nothing of its own",
            source: "def lambda_handler(event, context):\n    \
                         print('PLAIN wrote one line')\n    \
                         return {'ok': True}\n",
            runtime: PYTHON_RUNTIME,
            memory_size: 128,
            written: &["PLAIN wrote one line"],
        },
        Row {
            case: "a python handler that logs through the logging facility",
            source: "import logging\n\
                     \n\
                     LOGGER = logging.getLogger()\n\
                     LOGGER.setLevel(logging.INFO)\n\
                     \n\
                     \n\
                     def lambda_handler(event, context):\n    \
                         LOGGER.info('RECORD wrote one record')\n    \
                         return {'ok': True}\n",
            runtime: PYTHON_RUNTIME,
            memory_size: 256,
            written: &["RECORD wrote one record"],
        },
        Row {
            case: "a python handler that holds tens of megabytes",
            // 32 MB, written rather than merely asked for, so the pages really are the
            // run's: a run that holds memory of its own is a case of its own, whatever the
            // host's page accounting attributes to it.
            source: "def lambda_handler(event, context):\n    \
                         held = b'x' * (32 * 1024 * 1024)\n    \
                         print('HELD bytes=%d' % len(held))\n    \
                         return {'ok': True}\n",
            runtime: PYTHON_RUNTIME,
            memory_size: 512,
            written: &["HELD bytes=33554432"],
        },
        Row {
            case: "a node handler that writes on the console's error channel",
            source: "exports.handler = async () => {\n    \
                         console.error('CONSOLE wrote one line');\n    \
                         return { ok: true };\n\
                     };",
            runtime: NODE_RUNTIME,
            memory_size: 128,
            written: &["CONSOLE wrote one line"],
        },
        Row {
            case: "a node handler that holds tens of megabytes",
            // 32 MB, for the reason the python row above holds its own.
            source: "exports.handler = async () => {\n    \
                         const held = new Uint8Array(32 * 1024 * 1024);\n    \
                         held.fill(7);\n    \
                         console.log('HELD bytes=' + held.length);\n    \
                         return { ok: true };\n\
                     };",
            runtime: NODE_RUNTIME,
            memory_size: 512,
            written: &["HELD bytes=33554432"],
        },
    ]
}

/// Create the row's function with the `MemorySize` it names and wait until it reports
/// `Active`.
async fn create_function(router: &MultiServiceRouter, row: &Row, name: &str) {
    let (handler, package) = if row.runtime.starts_with("python") {
        (
            "lambda_function.lambda_handler",
            crate::synchronous_invoke_run::package(row.source),
        )
    } else {
        (
            "index.handler",
            crate::node_invoke_run::package(&[("index.js", row.source)]),
        )
    };
    let created = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": row.runtime,
            "Handler": handler,
            "Timeout": TIMEOUT_SECONDS,
            "MemorySize": row.memory_size,
            "Code": { "ZipFile": base64::engine::general_purpose::STANDARD.encode(package) },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {:?}: expected CreateFunction {name:?} to answer 201, got {} {}",
        row.case,
        created.status,
        created.text()
    );
    await_function_active(router, row.case, name).await;
}

/// The `REPORT` line of `log`, which has to be its last.
fn report_of(case: &str, log: &str) -> String {
    let lines: Vec<&str> = log.lines().collect();
    let last = lines
        .last()
        .unwrap_or_else(|| panic!("case {case:?}: expected a log at all, got {log:?}"));
    assert!(
        last.starts_with(REPORT_PREFIX),
        "case {case:?}: expected the log to close on the report line, got {lines:?}"
    );
    (*last).to_string()
}

/// The fields `report` states what the run spent in, in the order it states them.
fn spent_fields(case: &str, report: &str) -> Vec<String> {
    let fields: Vec<String> = report.split('\t').map(str::to_string).collect();
    assert!(
        fields.len() > REPORT_FIELDS.len(),
        "case {case:?}: expected the request id and then {} fields on the report, got {fields:?}",
        REPORT_FIELDS.len()
    );
    for (index, expected) in REPORT_FIELDS.iter().enumerate() {
        let actual = &fields[index + 1];
        assert!(
            actual.starts_with(expected),
            "case {case:?}: expected field {index} of the report to state {expected:?}, got \
             {actual:?}"
        );
    }
    fields
}

/// The megabytes `report` states the run USED.
fn used_megabytes(case: &str, report: &str) -> i64 {
    let fields = spent_fields(case, report);
    let field = fields
        .iter()
        .find(|field| field.starts_with(USED_FIELD))
        .unwrap_or_else(|| panic!("case {case:?}: expected a used-memory field, got {fields:?}"));
    let stated = field
        .strip_prefix(USED_FIELD)
        .and_then(|rest| rest.strip_suffix(MEGABYTES))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the memory used in whole megabytes, got {field:?}")
        });
    stated.parse().unwrap_or_else(|error| {
        panic!("case {case:?}: expected the memory used as an integer, got {stated:?}: {error}")
    })
}

/// Where `line` sits among `lines`, which must hold it exactly once.
fn position_of(case: &str, lines: &[String], line: &str) -> usize {
    let found: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, held)| held.contains(line))
        .map(|(index, _)| index)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one line carrying {line:?}, got {lines:?}"
    );
    found[0]
}

/// Where the framing of the log sits: the index of the `START` line and of the `END` line.
fn framing(case: &str, lines: &[String]) -> (usize, usize) {
    let index_of = |prefix: &str| {
        lines
            .iter()
            .position(|line| line.starts_with(prefix))
            .unwrap_or_else(|| panic!("case {case:?}: expected a {prefix:?} line, got {lines:?}"))
    };
    (index_of(START_PREFIX), index_of(END_PREFIX))
}

/// Every row: the report states what the run used, within the memory the function is
/// configured for, and the handler's own output is framed by the runtime's own lines.
#[tokio::test]
async fn every_invocations_report_states_the_memory_its_run_used() {
    let _stream = one_invocation_stream_at_a_time().await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("report-memory-{index}");
        create_function(&router, row, &name).await;

        let (log, payload) = invoke(&router, case, &name).await;
        assert_eq!(
            payload,
            json!({ "ok": true }),
            "case {case:?}: the handler must have returned, got {payload} with the log {log:?}"
        );

        let lines: Vec<String> = log.lines().map(str::to_string).collect();
        let (opened, closed) = framing(case, &lines);
        for written in row.written {
            let at = position_of(case, &lines, written);
            assert!(
                at > opened && at < closed,
                "case {case:?}: expected {written:?} between the framing at {opened} and \
                 {closed}, got it at {at} in {lines:?}"
            );
        }

        let report = report_of(case, &log);
        let size_field = format!("Memory Size: {} MB", row.memory_size);
        assert!(
            report.contains(&size_field),
            "case {case:?}: expected the report to state {size_field:?}, got {report:?}"
        );
        let used = used_megabytes(case, &report);
        assert!(
            (1..=row.memory_size).contains(&used),
            "case {case:?}: expected the memory used between 1 and the configured {} MB, got \
             {used} MB in {report:?}",
            row.memory_size
        );
    }
}

/// The report line reaches the function's CloudWatch Logs stream as its closing event, beside
/// the handler's own line — which is where a client reads both.
#[tokio::test]
async fn the_delivered_stream_closes_on_the_report_the_invocation_stated() {
    let _stream = one_invocation_stream_at_a_time().await;
    let case = "the stream a run's report and its handler's line land in";
    let router = emulator();
    let name = "report-memory-delivered";
    let row = Row {
        case,
        source: "import logging\n\
                 \n\
                 LOGGER = logging.getLogger()\n\
                 LOGGER.setLevel(logging.INFO)\n\
                 \n\
                 \n\
                 def lambda_handler(event, context):\n    \
                     LOGGER.info('DELIVERED one record')\n    \
                     return {'ok': True}\n",
        runtime: PYTHON_RUNTIME,
        memory_size: 256,
        written: &["DELIVERED one record"],
    };
    create_function(&router, &row, name).await;

    let (reported, _) = invoke(&router, case, name).await;
    let group = format!("/aws/lambda/{name}");
    let held = streams(&router, case, &group).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected the invocation to leave one stream in {group:?}, got {held:?}"
    );
    let (lines, _delivered_text) = delivered(&router, case, &group, &held[0]).await;
    // What the group holds beside the one record the ENVIRONMENT wrote for its own
    // initialization, which no invocation reports as its log.
    let text = invocations_log(&lines);
    assert_eq!(
        text, reported,
        "case {case:?}: expected the delivered events to be the log the invocation reported"
    );

    let (opened, closed) = framing(case, &lines);
    let at = position_of(case, &lines, row.written[0]);
    assert!(
        at > opened && at < closed,
        "case {case:?}: expected the handler's own record between the framing at {opened} and \
         {closed}, got it at {at} in {lines:?}"
    );
    let report = report_of(case, &text);
    let used = used_megabytes(case, &report);
    assert!(
        (1..=row.memory_size).contains(&used),
        "case {case:?}: expected the delivered report to state the memory used between 1 and \
         the configured {} MB, got {used} MB in {report:?}",
        row.memory_size
    );
}
