// SPDX-License-Identifier: CC0-1.0

//! What the invocation's own log tail carries of what the handler logged, and what ONE
//! structured record of it states.
//!
//! RULE: `ApplicationLogLevel` is the level Lambda filters the application logs it SENDS to
//! CloudWatch Logs at — it is the level the execution environment's logger STARTS at and not a
//! ceiling on what the runtime emits — so every record the application's own logger admits is
//! emitted into the invocation's log, which is what `X-Amz-Log-Result` reports, and the level
//! is applied again only on the delivery to the function's log group. Under
//! `LogFormat` `JSON` each such record is one JSON object carrying the standard keys and the
//! keys the logging call itself passed, merged in at the TOP level with their own JSON types.
//!
//! The rows differ along what the rule generalises over: which level the function is
//! configured at (around and at the level the handler's records are emitted at, and the
//! configured default), which level the application's own logger is set to (more and less
//! detailed than the function's), and whether the platform records are filtered beside the
//! application ones.
//!
//! Every test here RUNS a handler, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::invocation_log_delivery::{
    create_function_logging, delivered, streams, structured_record, Created, LOGS, LOG_WRITES,
    PYTHON_RUNTIME,
};
use crate::invocation_log_delivery::{emulator, invoke};

/// The marker every record the handler logs carries, so the rows read the handler's own
/// records out of the log and nothing else.
const MARKER: &str = "app-marker";

/// The value the logging call passes under the key `a`, which a record must carry as the
/// string it is.
const EXTRA_TEXT: &str = "mark-7f4f";

/// A handler whose own logger admits records at INFO and lower in detail, which it states at
/// MODULE scope — where a function states what its logger admits, before any invocation is
/// bound.
const PYTHON_AT_INFO: &str = r#"import logging

logging.getLogger().setLevel(logging.INFO)


def lambda_handler(event, context):
    logger = logging.getLogger()
    logger.debug("app-marker DEBUG")
    logger.info(
        "app-marker INFO",
        extra={
            "a": "mark-7f4f",
            "b": [3, "x"],
            "nested": {"k": True},
            "count": 7,
            "ratio": 1.5,
        },
    )
    logger.warning("app-marker WARNING")
    return {"effective": logging.getLogger().getEffectiveLevel()}
"#;

/// The same handler whose logger admits everything, which is MORE detail than any level the
/// rows configure.
const PYTHON_AT_DEBUG: &str = r#"import logging

logging.getLogger().setLevel(logging.DEBUG)


def lambda_handler(event, context):
    logger = logging.getLogger()
    logger.debug("app-marker DEBUG")
    logger.info(
        "app-marker INFO",
        extra={
            "a": "mark-7f4f",
            "b": [3, "x"],
            "nested": {"k": True},
            "count": 7,
            "ratio": 1.5,
        },
    )
    logger.warning("app-marker WARNING")
    return {"effective": logging.getLogger().getEffectiveLevel()}
"#;

/// The same handler setting no level of its own, which leaves the environment's logger at the
/// level the function is configured at.
const PYTHON_AT_NOTHING: &str = r#"import logging


def lambda_handler(event, context):
    logger = logging.getLogger()
    logger.debug("app-marker DEBUG")
    logger.info(
        "app-marker INFO",
        extra={
            "a": "mark-7f4f",
            "b": [3, "x"],
            "nested": {"k": True},
            "count": 7,
            "ratio": 1.5,
        },
    )
    logger.warning("app-marker WARNING")
    return {"effective": logging.getLogger().getEffectiveLevel()}
"#;

/// One row: the `LoggingConfig` the function carries, the handler that runs on it, the levels
/// the invocation's own tail must report, and the levels the log group must hold.
struct Row {
    case: &'static str,
    /// The whole `LoggingConfig` the create names.
    logging_config: Value,
    /// The handler's source, which states what its own logger admits.
    source: &'static str,
    /// The levels of the handler's records the TAIL must carry, in the order it wrote them.
    emitted: &'static [&'static str],
    /// The levels of them the log GROUP must hold, which is what the level filters.
    delivered: &'static [&'static str],
    /// Whether the platform records reach the group beside them.
    delivers_platform: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a logger more detailed than the level the function filters at",
            logging_config: json!({ "LogFormat": "JSON", "ApplicationLogLevel": "WARN" }),
            source: PYTHON_AT_INFO,
            emitted: &["INFO", "WARNING"],
            delivered: &["WARNING"],
            delivers_platform: true,
        },
        Row {
            case: "the least detailed level of all, which delivers none of the records",
            logging_config: json!({ "LogFormat": "JSON", "ApplicationLogLevel": "FATAL" }),
            source: PYTHON_AT_DEBUG,
            emitted: &["DEBUG", "INFO", "WARNING"],
            delivered: &[],
            delivers_platform: true,
        },
        Row {
            case: "the platform records filtered beside the application ones",
            logging_config: json!({
                "LogFormat": "JSON",
                "ApplicationLogLevel": "WARN",
                "SystemLogLevel": "WARN",
            }),
            source: PYTHON_AT_DEBUG,
            emitted: &["DEBUG", "INFO", "WARNING"],
            delivered: &["WARNING"],
            delivers_platform: false,
        },
        Row {
            case: "a logger the function's own level starts, the handler setting none",
            logging_config: json!({ "LogFormat": "JSON", "ApplicationLogLevel": "INFO" }),
            source: PYTHON_AT_NOTHING,
            emitted: &["INFO", "WARNING"],
            delivered: &["INFO", "WARNING"],
            delivers_platform: true,
        },
    ]
}

/// The levels of the handler's own records in `log`, in the order they were written.
fn marked_levels(log: &str) -> Vec<String> {
    log.lines()
        .filter(|line| line.contains(MARKER))
        .filter_map(structured_record)
        .filter_map(|members| {
            members
                .get("level")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// RULE: the tail reports every record the application's own logger admitted, and the
/// function's `ApplicationLogLevel` decides only which of them Lambda delivers to the log
/// group.
#[tokio::test]
async fn the_log_tail_reports_what_the_application_logger_admitted() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("app-records-{index}");
        let role = given_role(
            &router,
            case,
            &name,
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        create_function_logging(
            &router,
            &Created {
                case,
                name: &name,
                source: row.source,
                runtime: PYTHON_RUNTIME,
                role: &role,
                log_group: None,
            },
            Some(&row.logging_config),
        )
        .await;

        let (tail, _payload) = invoke(&router, case, &name).await;
        let written = marked_levels(&tail);
        assert_eq!(
            written,
            row.emitted
                .iter()
                .map(|level| (*level).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the tail to report {:?}, got {written:?} out of {tail:?}",
            row.emitted
        );

        // The delivery is the leg the level is applied on, so the group holds what it admits
        // and the tail above holds what the handler emitted.
        let group = format!("/aws/lambda/{name}");
        let held = delivered_records(&router, case, &group, row.delivered).await;
        let sent = marked_levels(&held.join("\n"));
        assert_eq!(
            sent,
            row.delivered
                .iter()
                .map(|level| (*level).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the group to hold {:?}, got {sent:?} out of {held:?}",
            row.delivered
        );
        let platform = held
            .iter()
            .filter_map(|line| structured_record(line))
            .any(|members| {
                members
                    .get("type")
                    .and_then(Value::as_str)
                    .is_some_and(|kind| kind.starts_with("platform."))
            });
        assert_eq!(
            platform, row.delivers_platform,
            "case {case:?}: expected the platform records delivered: {}, got {held:?}",
            row.delivers_platform
        );
    }
}

/// RULE: under the structured format one application record is one JSON object carrying the
/// standard keys and the keys the logging call passed, each at the top level and with its own
/// JSON type.
#[tokio::test]
async fn a_structured_record_carries_the_keys_the_logging_call_passed() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a record whose logging call carried fields of several JSON types";
    let router = emulator();
    let name = "app-record-fields";
    create_function_logging(
        &router,
        &Created {
            case,
            name,
            source: PYTHON_AT_INFO,
            runtime: PYTHON_RUNTIME,
            role: "arn:aws:iam::000000000000:role/service-role/lambda-exec",
            log_group: None,
        },
        Some(&json!({ "LogFormat": "JSON" })),
    )
    .await;
    let (tail, _payload) = invoke(&router, case, name).await;
    let record = tail
        .lines()
        .filter(|line| line.contains(MARKER))
        .filter_map(structured_record)
        .find(|members| members.get("level").and_then(Value::as_str) == Some("INFO"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the INFO record in the tail, got {tail:?}")
        });

    // The standard keys the structured format states for every record, and the value each
    // must have where the row knows it.
    for member in ["timestamp", "level", "message", "logger", "requestId"] {
        assert!(
            record.get(member).is_some_and(|held| !held.is_null()),
            "case {case:?}: expected the standard key {member:?} on the record, got {record:?}"
        );
    }
    assert_eq!(
        record["message"],
        json!(format!("{MARKER} INFO")),
        "case {case:?}: expected the message the call made, got {record:?}"
    );
    // The keys the CALL carried, each keeping the JSON type the value has: a string stays a
    // string, a list a list, a mapping an object, a whole number a number, a fraction a
    // fraction.
    for (key, expected) in [
        ("a", json!(EXTRA_TEXT)),
        ("b", json!([3, "x"])),
        ("nested", json!({ "k": true })),
        ("count", json!(7)),
        ("ratio", json!(1.5)),
    ] {
        assert_eq!(
            record.get(key),
            Some(&expected),
            "case {case:?}: expected the key {key:?} to be {expected} at the top level, got \
             {record:?}"
        );
    }
    // And NOTHING else: the record is the standard keys and the call's own fields, so the
    // framing the logging library puts on every record is not leaked into it.
    let mut held: Vec<&str> = record.keys().map(String::as_str).collect();
    held.sort_unstable();
    assert_eq!(
        held,
        vec![
            "a", "b", "count", "level", "logger", "message", "nested", "ratio", "requestId",
            "timestamp",
        ],
        "case {case:?}: expected the standard keys and the call's own, got {record:?}"
    );
}

/// The records the group `group` holds once the delivery of the run's log has put the ones
/// the row expects there: the delivery happens off the invocation's own answer, so this looks
/// again until the group holds them or a generous deadline passes — and answers whatever it
/// last read, which is what the assertion then reports.
async fn delivered_records(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    expected: &[&str],
) -> Vec<String> {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut held: Vec<String> = Vec::new();
    loop {
        if let Some(stream) = streams(router, case, group).await.first() {
            let (records, _text) = delivered(router, case, group, stream).await;
            held = records;
            // A delivery writes one run's log in one go, so a group holding any record of it
            // holds every record the levels admitted.
            let settled = !held.is_empty()
                && marked_levels(&held.join("\n"))
                    == expected
                        .iter()
                        .map(|level| (*level).to_string())
                        .collect::<Vec<String>>();
            if settled {
                return held;
            }
        }
        if std::time::Instant::now() >= deadline {
            return held;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
