// SPDX-License-Identifier: CC0-1.0

//! The CLASS a run the platform ended is reported under, end to end over the wire: one
//! `Invoke` per row, with `LogType=Tail` so the run's own `REPORT` record comes back beside
//! its answer.
//!
//! The rule pinned here: a run the PLATFORM ended rather than the handler is classified once,
//! and that one class is reported in both places a client reads it — the `errorType` of the
//! error envelope the invocation answers with, and the class the run's `REPORT` record states
//! beside its status (the plain-text `Error Type` field, and the structured record's
//! `errorType` member under `LoggingConfig$LogFormat=JSON`). The class names the bound that
//! ended the run: the response-size class for an answer past the quota an invocation may
//! return, and the out-of-memory kill for a run that held more memory than the invoked
//! qualifier's own `MemorySize`. A run the HANDLER completed — one that returned, and one that
//! raised — answered for itself and its report classifies nothing, and a run stopped at the
//! function's own `Timeout` is reported as the timeout it is by the status alone.
//!
//! Because the classification is per QUALIFIER, one row invokes a published version whose
//! `MemorySize` was raised on `$LATEST` afterwards: the version keeps the size it was
//! published with, so an allocation that fits the raised size is still killed against the
//! version, and each qualifier's report states that qualifier's own size.
//!
//! Each test here RUNS a handler, so it takes the one invocation stream the suites that run
//! handlers share ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::io::Write as _;
use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};

use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, make_update_due, router_on, send, zip_file,
    Answer, ROLE,
};
use crate::invoke_request_id::one_invocation_stream_at_a_time;

/// The module and attribute the package below carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The smallest `MemorySize` a function may have
/// (`smithy_query(query="shape", shape="MemorySize")`: `@range(min: 128, max: 32768)`), which
/// is the size the run about the memory ceiling is killed against.
const SMALLEST_MEMORY_SIZE_MB: i64 = 128;

/// The `MemorySize` `$LATEST` is RAISED to after a version is published, so the version's own
/// size is the only thing that can still end the run against that version. A size no row
/// about another bound comes near, so the memory ceiling ends none of them.
const RAISED_MEMORY_SIZE_MB: i64 = 256;

/// A `Timeout` far above the span a row about another bound needs, so what ends such a run is
/// never the clock. Well inside the range the model allows the member
/// (`smithy_query(query="shape", shape="Timeout")`: `@range(min: 1, max: 5400)`).
const GENEROUS_TIMEOUT_SECS: i64 = 120;

/// The `Timeout` the row about the DEADLINE carries: the smallest the model allows, which is
/// the bound that row is about.
const SHORTEST_TIMEOUT_SECS: i64 = 1;

/// How many bytes the row about the RESPONSE quota answers with: past the 6 MB AWS documents
/// as the quota for a synchronous invocation's response, whatever the exact byte figure is.
const OVERSIZE_RESPONSE_BYTES: usize = 7 * 1024 * 1024;

/// The handler every row runs: it ends the way its event asks — holding memory a megabyte at
/// a time until the platform kills it, answering with more bytes than an invocation may
/// return, outliving its `Timeout`, raising, or returning a value.
const HANDLER_SOURCE: &str = r#"
import time


def lambda_handler(event, context):
    mode = event.get("mode")
    if mode == "fill":
        kept = []
        while True:
            kept.append(bytearray(b"x" * (1024 * 1024)))
    if mode == "oversize":
        return {"payload": "x" * int(event["bytes"])}
    if mode == "sleep":
        time.sleep(600)
    if mode == "raise":
        raise ValueError("boom")
    return {"ok": True}
"#;

/// A .zip deployment package holding [`HANDLER_SOURCE`] as `lambda_function.py`.
fn package() -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(HANDLER_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// One row of the table: the case, how the run ends, the function it needs, and what the
/// report has to state.
struct Row {
    case: &'static str,
    /// What the event asks the handler to do.
    mode: &'static str,
    memory_size: i64,
    timeout: i64,
    /// Whether the function's records are the structured ones.
    json_records: bool,
    /// The class the ANSWER's `errorType` carries.
    error_type: &'static str,
    /// The status the report states, or `None` for a run whose report states none.
    status: Option<&'static str>,
    /// The class the report states beside that status, or `None` for a run whose report
    /// classifies nothing.
    reported_class: Option<&'static str>,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a handler whose answer passes the quota an invocation may return",
            mode: "oversize",
            memory_size: RAISED_MEMORY_SIZE_MB,
            timeout: GENEROUS_TIMEOUT_SECS,
            json_records: false,
            error_type: "Function.ResponseSizeTooLarge",
            status: Some("error"),
            reported_class: Some("Function.ResponseSizeTooLarge"),
        },
        Row {
            case: "the same run under the structured log format",
            mode: "oversize",
            memory_size: RAISED_MEMORY_SIZE_MB,
            timeout: GENEROUS_TIMEOUT_SECS,
            json_records: true,
            error_type: "Function.ResponseSizeTooLarge",
            status: Some("error"),
            reported_class: Some("Function.ResponseSizeTooLarge"),
        },
        Row {
            case: "a handler that outlives its Timeout",
            mode: "sleep",
            memory_size: RAISED_MEMORY_SIZE_MB,
            timeout: SHORTEST_TIMEOUT_SECS,
            json_records: false,
            error_type: "Sandbox.Timedout",
            status: Some("timeout"),
            reported_class: None,
        },
        Row {
            case: "a handler that raised an exception of its own",
            mode: "raise",
            memory_size: RAISED_MEMORY_SIZE_MB,
            timeout: GENEROUS_TIMEOUT_SECS,
            json_records: false,
            error_type: "ValueError",
            status: None,
            reported_class: None,
        },
        Row {
            case: "a handler that answered",
            mode: "return",
            memory_size: RAISED_MEMORY_SIZE_MB,
            timeout: GENEROUS_TIMEOUT_SECS,
            json_records: false,
            error_type: "",
            status: None,
            reported_class: None,
        },
    ]
}

/// The body a create of the probe function carries, with the bounds and the log format one row
/// needs.
fn create_body(name: &str, memory_size: i64, timeout: i64, json_records: bool) -> Value {
    let mut body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": "python3.12",
        "Handler": HANDLER,
        "Timeout": timeout,
        "MemorySize": memory_size,
        "Code": { "ZipFile": zip_file(&package()) },
    });
    if json_records {
        body["LoggingConfig"] = json!({ "LogFormat": "JSON" });
    }
    body
}

/// `Invoke` the function `reference` names, asking for the run's own log tail.
async fn invoke_with_log_tail(
    router: &smithy_server_runtime::MultiServiceRouter,
    reference: &str,
    mode: &str,
) -> Answer {
    send(
        router,
        "POST",
        &format!("/2015-03-31/functions/{reference}/invocations"),
        "application/json",
        serde_json::to_vec(&json!({ "mode": mode, "bytes": OVERSIZE_RESPONSE_BYTES }))
            .expect("the event serializes"),
        &[("x-amz-log-type", "Tail".to_string())],
    )
    .await
}

/// The log tail of one invocation, as text: the base64 `X-Amz-Log-Result` the answer carried.
fn log_tail(answered: &Answer, case: &str) -> String {
    let encoded = answered
        .header("x-amz-log-result")
        .unwrap_or_else(|| panic!("case {case:?}: a log tail was asked for and must come back"));
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded.as_bytes())
        .unwrap_or_else(|error| panic!("case {case:?}: the log tail must be base64: {error}"));
    String::from_utf8_lossy(&decoded).to_string()
}

/// The `REPORT` record of that log, as the fields it is laid out in: the plain-text line split
/// on the separator its fields are closed by.
fn report_fields(log: &str, case: &str) -> Vec<String> {
    log.lines()
        .find(|line| line.starts_with("REPORT RequestId: "))
        .unwrap_or_else(|| panic!("case {case:?}: the log must carry a REPORT line, got {log:?}"))
        .split('\t')
        .map(str::to_string)
        .collect()
}

/// The structured `platform.report` record of that log.
fn report_record(log: &str, case: &str) -> Value {
    log.lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .find(|record| record.get("type").and_then(Value::as_str) == Some("platform.report"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: the log must carry a platform.report record, got {log:?}")
        })
}

/// The `errorType` an invocation's answer reports, or the empty string for an answer that is no
/// error document.
fn answered_class(answered: &Answer) -> String {
    answered
        .json()
        .get("errorType")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// RULE: a run the platform ended carries ONE classification, reported both as the answer's
/// `errorType` and as the class of its own `REPORT` record; a run the handler completed is
/// classified in neither.
///
/// The rows differ along what the rule generalises over: which bound ended the run, whether
/// the handler itself ended it, and which format the function's records take.
#[tokio::test]
async fn one_class_is_reported_in_the_answer_and_in_the_report_of_a_run() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    for row in rows() {
        let case = row.case;
        let name = "report-class-probe";
        let created = create_function(
            &router,
            &create_body(name, row.memory_size, row.timeout, row.json_records),
        )
        .await;
        assert_eq!(
            created.status,
            201,
            "case {case:?}: CreateFunction must succeed, got {} {}",
            created.status,
            created.json()
        );
        make_activation_due(&engine);
        let answered = invoke_with_log_tail(&router, name, row.mode).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: a run the platform ended is still a 200, got {} {}",
            answered.status,
            answered.json()
        );
        let reported = answered_class(&answered);
        assert_eq!(
            reported, row.error_type,
            "case {case:?}: expected the answer to report the class {:?}, got {reported:?} in {}",
            row.error_type,
            answered.json()
        );
        let log = log_tail(&answered, case);
        if row.json_records {
            let record = report_record(&log, case);
            let stated = record["record"]["errorType"].as_str();
            assert_eq!(
                stated,
                row.reported_class,
                "case {case:?}: expected the structured report to state {:?}, got {record}",
                row.reported_class
            );
            let status = record["record"]["status"].as_str();
            assert_eq!(
                status, row.status,
                "case {case:?}: expected the structured report's status {:?}, got {record}",
                row.status
            );
        } else {
            let fields = report_fields(&log, case);
            let class = fields
                .iter()
                .find_map(|field| field.strip_prefix("Error Type: "));
            assert_eq!(
                class, row.reported_class,
                "case {case:?}: expected the REPORT line to carry the class {:?} as a field of \
                 its own, got {fields:?}",
                row.reported_class
            );
            let status = fields
                .iter()
                .find_map(|field| field.strip_prefix("Status: "));
            assert_eq!(
                status, row.status,
                "case {case:?}: expected the REPORT line to state the status {:?}, got {fields:?}",
                row.status
            );
            assert!(
                fields
                    .iter()
                    .any(|field| field == &format!("Memory Size: {} MB", row.memory_size)),
                "case {case:?}: expected the REPORT line to state the size the run was given, \
                 got {fields:?}"
            );
        }
        let deleted = json_call(
            &router,
            "DELETE",
            &format!("/2015-03-31/functions/{name}"),
            &Value::Null,
        )
        .await;
        assert_eq!(
            deleted.status, 204,
            "case {case:?}: the function must be removable between rows"
        );
    }
}

/// RULE: the memory a run is allowed is the INVOKED QUALIFIER's own `MemorySize`, so a
/// published version keeps the size it was published with: an allocation that fits the size
/// `$LATEST` was raised to is still killed against that version, and each qualifier's report
/// states that qualifier's own size.
#[tokio::test]
async fn a_published_version_is_killed_against_the_memory_size_it_was_published_with() {
    let _stream = one_invocation_stream_at_a_time().await;
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let name = "report-class-version-probe";
    let created = create_function(
        &router,
        &create_body(name, SMALLEST_MEMORY_SIZE_MB, GENEROUS_TIMEOUT_SECS, false),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction must succeed, got {} {}",
        created.status,
        created.json()
    );
    make_activation_due(&engine);
    let published = json_call(
        &router,
        "POST",
        &format!("/2015-03-31/functions/{name}/versions"),
        &json!({}),
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "PublishVersion must succeed, got {} {}",
        published.status,
        published.json()
    );
    let version = published
        .string("Version")
        .expect("the published version names itself");
    let raised = json_call(
        &router,
        "PUT",
        &format!("/2015-03-31/functions/{name}/configuration"),
        &json!({ "MemorySize": RAISED_MEMORY_SIZE_MB }),
    )
    .await;
    assert_eq!(
        raised.status,
        200,
        "UpdateFunctionConfiguration must succeed, got {} {}",
        raised.status,
        raised.json()
    );
    make_update_due(&engine);
    // Both qualifiers are invoked, and the REPORT of each states the size THAT qualifier was
    // given: the version keeps the 128 MB it was published with, so the same allocation is
    // still killed against it, while $LATEST runs with the size it was raised to.
    let killed = invoke_with_log_tail(&router, &format!("{name}:{version}"), "fill").await;
    let case = "the published version, which keeps the size it was published with";
    assert_eq!(
        killed.status,
        200,
        "case {case:?}: a run the platform ended is still a 200, got {} {}",
        killed.status,
        killed.json()
    );
    assert_eq!(
        killed.header("x-amz-function-error").as_deref(),
        Some("Unhandled"),
        "case {case:?}: expected the Unhandled signal, got {}",
        killed.json()
    );
    let reported = answered_class(&killed);
    assert_eq!(
        reported, "Runtime.OutOfMemory",
        "case {case:?}: expected the out-of-memory class, got {reported:?} in {}",
        killed.json()
    );
    let fields = report_fields(&log_tail(&killed, case), case);
    assert!(
        fields
            .iter()
            .any(|field| field == "Error Type: Runtime.OutOfMemory"),
        "case {case:?}: expected the REPORT line to record the out-of-memory kill, got {fields:?}"
    );
    assert!(
        fields
            .iter()
            .any(|field| field == &format!("Memory Size: {SMALLEST_MEMORY_SIZE_MB} MB")),
        "case {case:?}: expected the REPORT line to state the size the version was published \
         with, got {fields:?}"
    );
    // The same allocation FITS the size $LATEST was raised to, so nothing kills a run of it —
    // and its report states the raised size, which is what the version's own report must not.
    let case = "$LATEST, whose size was raised past the allocation";
    let answered = invoke_with_log_tail(&router, name, "return").await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected the handler's own answer, got {} {}",
        answered.status,
        answered.json()
    );
    assert_eq!(
        answered.header("x-amz-function-error"),
        None,
        "case {case:?}: the handler must run to completion, got {}",
        answered.json()
    );
    let fields = report_fields(&log_tail(&answered, case), case);
    assert!(
        !fields.iter().any(|field| field.starts_with("Error Type: ")),
        "case {case:?}: a run the handler completed classifies nothing, got {fields:?}"
    );
    assert!(
        fields
            .iter()
            .any(|field| field == &format!("Memory Size: {RAISED_MEMORY_SIZE_MB} MB")),
        "case {case:?}: expected the REPORT line to state the raised size, got {fields:?}"
    );
}
