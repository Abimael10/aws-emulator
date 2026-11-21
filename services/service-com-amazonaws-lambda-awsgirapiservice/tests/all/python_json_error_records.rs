// SPDX-License-Identifier: CC0-1.0

//! What a PYTHON run's structured records state about a FAILURE, and which log variables its
//! execution environment carries.
//!
//! Three rules, each read off the invocation's own log tail and its answer rather than off any
//! handler's Rust:
//!
//! 1. Under `LoggingConfig$LogFormat` `JSON` a record carrying an exception states that
//!    exception's stack as a LIST OF FRAMES — one entry per frame of the traceback, each the
//!    location of a frame and the source line at it — beside the class and the message of the
//!    exception and a `location` naming the file, the function and the line the record was
//!    emitted at. The header line a rendered traceback opens with and the exception line it
//!    closes on are no frames of the stack: what they say is on the record's own members.
//! 2. The record the runtime writes itself for an UNHANDLED exception states its level under
//!    the member the Python runtime documents for that record, `log_level`, and every other
//!    member of it is the one the invocation's own error document carries under the same name —
//!    so a client correlating the answer with the log reads one failure rather than two — and
//!    no plain-text traceback is written beside it.
//! 3. `AWS_LAMBDA_LOG_FORMAT` and `AWS_LAMBDA_LOG_LEVEL` are the two variables the STRUCTURED
//!    format brings with it: the execution environment of a function whose `LoggingConfig` did
//!    not select that format carries neither, so a logging library that branches on the mere
//!    presence of one takes the plain-text branch exactly where the function is on the
//!    plain-text format.
//!
//! The rows differ along what each rule generalises over: how DEEP the stack of the logged
//! exception is (one frame, two, three), WHICH logging call carried it (`logging.exception`
//! and a call passing `exc_info` itself), which CLASS was raised (a built-in one and one the
//! handler declared), and — for the variables, which are the HOST's to set — which format and
//! level the configuration names and which RUNTIME reads them (both engines).
//!
//! Every test here RUNS handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::invocation_log_delivery::{
    create_function_logging, emulator, invoke, structured_record as record, Created, NODE_RUNTIME,
    PYTHON_RUNTIME,
};
use crate::node_handler_error_wording::ROLE;

/// The `LoggingConfig` a function on the structured format carries here: the format alone, so
/// what the records state is the format's doing and no level's.
fn json_format() -> Value {
    json!({ "LogFormat": "JSON" })
}

/// Create `name` on `runtime` with `source` and `logging_config`, and wait until it is
/// `Active`.
async fn created(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    runtime: &str,
    source: &str,
    logging_config: Option<&Value>,
) {
    create_function_logging(
        router,
        &Created {
            case,
            name,
            source,
            runtime,
            role: ROLE,
            log_group: None,
        },
        logging_config,
    )
    .await;
}

/// Every structured record of `tail` that carries the member `member`.
fn carrying(tail: &str, member: &str) -> Vec<serde_json::Map<String, Value>> {
    tail.lines()
        .filter_map(record)
        .filter(|members| members.contains_key(member))
        .collect()
}

/// The entries of the record member `member` as a list of strings, and the empty list for a
/// member that is no list of them.
fn strings(members: &serde_json::Map<String, Value>, member: &str) -> Vec<String> {
    members
        .get(member)
        .and_then(Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .map(|entry| entry.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The FILE one rendered frame names: what its location line quotes.
fn quoted_file(frame: &str) -> String {
    frame
        .split('"')
        .nth(1)
        .unwrap_or_default()
        .to_string()
}

/// The line a rendered traceback OPENS with, which is no frame of the stack.
const TRACEBACK_HEADER: &str = "Traceback (most recent call last):";

/// The framing the Python runtime writes an error report in under the PLAIN-TEXT format, which
/// a function on the structured format leaves none of.
const TEXT_ERROR_PREFIX: &str = "[ERROR]";

/// One row of the caught-exception table: the handler that logs one, and what the record it
/// leaves must state.
struct Caught {
    case: &'static str,
    /// The handler's source.
    source: &'static str,
    /// How many FRAMES the logged exception's stack has.
    frames: usize,
    /// The class the record names.
    error_type: &'static str,
    /// The message it names.
    error_message: &'static str,
    /// The function the deepest frame of the stack is in.
    raised_in: &'static str,
    /// The function the record was emitted from, which its `location` names.
    emitted_in: &'static str,
    /// The message the logging call itself carried.
    logged: &'static str,
}

/// A handler that catches an exception raised in its own frame and logs it.
const PYTHON_FLAT: &str = r#"import logging

logger = logging.getLogger()
logger.setLevel(logging.INFO)


def lambda_handler(event, context):
    try:
        raise ValueError("flat-boom")
    except ValueError:
        logger.exception("caught-flat")
    return {"ok": True}
"#;

/// A handler that catches one raised two frames down.
const PYTHON_NESTED: &str = r#"import logging

logger = logging.getLogger()
logger.setLevel(logging.INFO)


def inner():
    raise KeyError("nested-boom")


def lambda_handler(event, context):
    try:
        inner()
    except KeyError:
        logger.exception("caught-nested")
    return {"ok": True}
"#;

/// A handler that catches an exception of its OWN class, raised three frames down, and logs it
/// through a call that carries `exc_info` itself rather than through `logging.exception`.
const PYTHON_DEEP: &str = r#"import logging

logger = logging.getLogger()
logger.setLevel(logging.INFO)


class OrderRefused(Exception):
    pass


def innermost():
    raise OrderRefused("deep-boom")


def middle():
    innermost()


def lambda_handler(event, context):
    try:
        middle()
    except OrderRefused:
        logger.error("caught-deep", exc_info=True)
    return {"ok": True}
"#;

fn caught_rows() -> Vec<Caught> {
    vec![
        Caught {
            case: "one frame, logged through logging.exception",
            source: PYTHON_FLAT,
            frames: 1,
            error_type: "ValueError",
            error_message: "flat-boom",
            raised_in: "lambda_handler",
            emitted_in: "lambda_handler",
            logged: "caught-flat",
        },
        Caught {
            case: "two frames, logged through logging.exception",
            source: PYTHON_NESTED,
            frames: 2,
            error_type: "KeyError",
            // `str()` of a `KeyError` is the repr of the key it names, which is what the
            // record states as the message the exception carries.
            error_message: "'nested-boom'",
            raised_in: "inner",
            emitted_in: "lambda_handler",
            logged: "caught-nested",
        },
        Caught {
            case: "three frames of a class the handler declared, logged with exc_info",
            source: PYTHON_DEEP,
            frames: 3,
            error_type: "OrderRefused",
            error_message: "deep-boom",
            raised_in: "innermost",
            emitted_in: "lambda_handler",
            logged: "caught-deep",
        },
    ]
}

/// RULE 1: a structured record carrying an exception states the stack as a list of FRAMES —
/// one entry per frame, no traceback header and no exception line among them — beside the
/// class and the message of the exception and the `location` the record was emitted at.
#[tokio::test]
async fn a_caught_exceptions_record_states_its_stack_as_frames() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in caught_rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("python-caught-{index}");
        created(
            &router,
            case,
            &name,
            PYTHON_RUNTIME,
            row.source,
            Some(&json_format()),
        )
        .await;
        let (tail, _payload) = invoke(&router, case, &name).await;

        let records: Vec<serde_json::Map<String, Value>> = carrying(&tail, "errorType")
            .into_iter()
            .filter(|members| {
                members.get("message").and_then(Value::as_str) == Some(row.logged)
            })
            .collect();
        assert_eq!(
            records.len(),
            1,
            "case {case:?}: expected one record carrying {logged:?} and the exception, got \
             {records:?} in {tail:?}",
            logged = row.logged
        );
        let found = &records[0];
        for (member, expected) in [
            ("errorType", row.error_type),
            ("errorMessage", row.error_message),
            ("level", "ERROR"),
        ] {
            let actual = found.get(member).and_then(Value::as_str).unwrap_or_default();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the record's {member} to be {expected:?}, got \
                 {actual:?} in {found:?}"
            );
        }

        // The stack is the FRAMES of the traceback and nothing else.
        let frames = strings(found, "stackTrace");
        assert_eq!(
            frames.len(),
            row.frames,
            "case {case:?}: expected {expected} frame(s) in the stack, got {frames:?}",
            expected = row.frames
        );
        for frame in &frames {
            assert!(
                frame.contains("  File \"") && frame.contains(", in "),
                "case {case:?}: expected every entry to be one rendered frame, got {frame:?}"
            );
            assert!(
                !frame.contains(TRACEBACK_HEADER)
                    && !frame.trim_start().starts_with(row.error_type),
                "case {case:?}: expected no traceback header and no exception line among the \
                 frames, got {frame:?}"
            );
        }
        let deepest = frames.last().cloned().unwrap_or_default();
        assert!(
            deepest.contains(&format!(", in {}", row.raised_in)),
            "case {case:?}: expected the deepest frame to be in {raised:?}, got {deepest:?}",
            raised = row.raised_in
        );
        let first = frames.first().cloned().unwrap_or_default();
        assert!(
            first.contains(&format!(", in {}", row.emitted_in)),
            "case {case:?}: expected the stack to open in {emitted:?}, got {first:?}",
            emitted = row.emitted_in
        );

        // WHERE the record was emitted: the file of that frame, the function in it and the
        // line in that function.
        let location = found.get("location").and_then(Value::as_str).unwrap_or_default();
        let parts: Vec<&str> = location.split(':').collect();
        assert_eq!(
            parts.len(),
            3,
            "case {case:?}: expected the location to name a file, a function and a line, got \
             {location:?}"
        );
        assert_eq!(
            parts[0],
            quoted_file(&first),
            "case {case:?}: expected the location to name the file the frames name, got \
             {location:?} against {first:?}"
        );
        assert_eq!(
            parts[1], row.emitted_in,
            "case {case:?}: expected the location to name {emitted:?}, got {location:?}",
            emitted = row.emitted_in
        );
        assert!(
            parts[2].parse::<u32>().is_ok(),
            "case {case:?}: expected the location to end on the line in it, got {location:?}"
        );

        // A record of the structured format carries the rendering of a traceback nowhere: the
        // frames are the record's own member.
        assert!(
            !tail.contains(TRACEBACK_HEADER) && !tail.contains(TEXT_ERROR_PREFIX),
            "case {case:?}: expected no plain-text traceback in the log, got {tail:?}"
        );
    }
}

/// One row of the unhandled-exception table: the handler that raises, and the class and
/// message the failure carries.
struct Unhandled {
    case: &'static str,
    source: &'static str,
    error_type: &'static str,
    error_message: &'static str,
}

/// A handler that raises in its own frame.
const PYTHON_RAISES_FLAT: &str = r#"def lambda_handler(event, context):
    raise ValueError("unhandled-flat")
"#;

/// One that raises two frames down.
const PYTHON_RAISES_NESTED: &str = r#"def inner():
    raise KeyError("unhandled-nested")


def lambda_handler(event, context):
    inner()
"#;

/// One that raises an exception of its own class.
const PYTHON_RAISES_DECLARED: &str = r#"class PaymentDeclined(Exception):
    pass


def lambda_handler(event, context):
    raise PaymentDeclined("unhandled-declared")
"#;

fn unhandled_rows() -> Vec<Unhandled> {
    vec![
        Unhandled {
            case: "a built-in class raised in the handler's own frame",
            source: PYTHON_RAISES_FLAT,
            error_type: "ValueError",
            error_message: "unhandled-flat",
        },
        Unhandled {
            case: "one raised two frames down",
            source: PYTHON_RAISES_NESTED,
            error_type: "KeyError",
            error_message: "'unhandled-nested'",
        },
        Unhandled {
            case: "a class the handler declared",
            source: PYTHON_RAISES_DECLARED,
            error_type: "PaymentDeclined",
            error_message: "unhandled-declared",
        },
    ]
}

/// RULE 2: the record the runtime writes for an unhandled exception states its level under
/// `log_level`, agrees member for member with the invocation's own error document, and is
/// written instead of a plain-text traceback — not beside one.
#[tokio::test]
async fn an_unhandled_exceptions_record_agrees_with_the_error_document() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in unhandled_rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("python-unhandled-{index}");
        created(
            &router,
            case,
            &name,
            PYTHON_RUNTIME,
            row.source,
            Some(&json_format()),
        )
        .await;
        let (tail, payload) = invoke(&router, case, &name).await;

        let levelled = carrying(&tail, "log_level");
        assert_eq!(
            levelled.len(),
            1,
            "case {case:?}: expected one record stating its level under log_level, got \
             {levelled:?} in {tail:?}"
        );
        let found = &levelled[0];
        let level = found.get("log_level").and_then(Value::as_str).unwrap_or_default();
        assert_eq!(
            level, "ERROR",
            "case {case:?}: expected the failure's record at ERROR, got {level:?} in {found:?}"
        );
        for (member, expected) in [
            ("errorType", row.error_type),
            ("errorMessage", row.error_message),
        ] {
            let actual = found.get(member).and_then(Value::as_str).unwrap_or_default();
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the record's {member} to be {expected:?}, got \
                 {actual:?} in {found:?}"
            );
        }
        // Every member the error document carries, as the document carries it: the class, the
        // sentence, the invocation and the frames.
        for member in ["errorType", "errorMessage", "requestId", "stackTrace"] {
            assert_eq!(
                found.get(member),
                payload.get(member),
                "case {case:?}: expected the record's {member} to be the error document's \
                 {expected:?}, got {actual:?}",
                expected = payload.get(member),
                actual = found.get(member)
            );
        }
        assert!(
            !strings(found, "stackTrace").is_empty(),
            "case {case:?}: expected the failure's frames on the record, got {found:?}"
        );
        assert!(
            !tail.contains(TRACEBACK_HEADER) && !tail.contains(TEXT_ERROR_PREFIX),
            "case {case:?}: expected no plain-text traceback beside the record, got {tail:?}"
        );
    }
}

/// One row of the log-variable table: what the function's `LoggingConfig` names, and the value
/// each of the two variables has in the execution environment of its runs.
struct Variables {
    case: &'static str,
    runtime: &'static str,
    source: &'static str,
    /// The `LoggingConfig` the create names, or `None` for a create that names none.
    logging_config: Option<Value>,
    /// What `AWS_LAMBDA_LOG_FORMAT` reads as, and `None` for a variable the environment does
    /// not carry.
    log_format: Option<&'static str>,
    /// What `AWS_LAMBDA_LOG_LEVEL` reads as, read the same way.
    log_level: Option<&'static str>,
}

/// A python handler that reports the two variables as its own environment holds them.
const PYTHON_REPORTS_VARIABLES: &str = r#"import os


def lambda_handler(event, context):
    return {
        "format": os.environ.get("AWS_LAMBDA_LOG_FORMAT"),
        "level": os.environ.get("AWS_LAMBDA_LOG_LEVEL"),
    }
"#;

/// A node handler that reports the same two.
const NODE_REPORTS_VARIABLES: &str = "exports.handler = async () => {\n    var named = \
                                      function (name) {\n        return \
                                      process.env[name] === undefined ? null : \
                                      process.env[name];\n    };\n    return { format: \
                                      named('AWS_LAMBDA_LOG_FORMAT'), level: \
                                      named('AWS_LAMBDA_LOG_LEVEL') };\n};\n";

fn variable_rows() -> Vec<Variables> {
    vec![
        Variables {
            case: "a python function naming no LoggingConfig at all",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_REPORTS_VARIABLES,
            logging_config: None,
            log_format: None,
            log_level: None,
        },
        Variables {
            case: "a python function naming the plain-text format",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_REPORTS_VARIABLES,
            logging_config: Some(json!({ "LogFormat": "Text" })),
            log_format: None,
            log_level: None,
        },
        Variables {
            case: "a python function naming the structured format and no level",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_REPORTS_VARIABLES,
            logging_config: Some(json!({ "LogFormat": "JSON" })),
            log_format: Some("JSON"),
            // The level a function on that format is filtered at until it names one
            // (`crate::invocation_logs` owns that default).
            log_level: Some("INFO"),
        },
        Variables {
            case: "a python function naming the structured format and an application level",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_REPORTS_VARIABLES,
            logging_config: Some(json!({
                "LogFormat": "JSON",
                "ApplicationLogLevel": "DEBUG",
                "SystemLogLevel": "WARN",
            })),
            log_format: Some("JSON"),
            log_level: Some("DEBUG"),
        },
        Variables {
            case: "a node function naming no LoggingConfig at all",
            runtime: NODE_RUNTIME,
            source: NODE_REPORTS_VARIABLES,
            logging_config: None,
            log_format: None,
            log_level: None,
        },
        Variables {
            case: "a node function naming the structured format and an application level",
            runtime: NODE_RUNTIME,
            source: NODE_REPORTS_VARIABLES,
            logging_config: Some(json!({
                "LogFormat": "JSON",
                "ApplicationLogLevel": "WARN",
            })),
            log_format: Some("JSON"),
            log_level: Some("WARN"),
        },
    ]
}

/// RULE 3: the execution environment carries `AWS_LAMBDA_LOG_FORMAT` and
/// `AWS_LAMBDA_LOG_LEVEL` only where the function's `LoggingConfig` selected the structured
/// format — whichever runtime reads them.
#[tokio::test]
async fn the_log_variables_are_set_only_under_the_structured_format() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in variable_rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("log-variables-{index}");
        created(
            &router,
            case,
            &name,
            row.runtime,
            row.source,
            row.logging_config.as_ref(),
        )
        .await;
        let (_tail, payload) = invoke(&router, case, &name).await;

        let expected = json!({
            "format": row.log_format,
            "level": row.log_level,
        });
        assert_eq!(
            payload, expected,
            "case {case:?}: expected the run to read {expected}, got {payload}"
        );
    }
}
