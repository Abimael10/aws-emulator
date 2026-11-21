// SPDX-License-Identifier: CC0-1.0

//! The FORM an invocation's delivered log records take: what `LoggingConfig$LogFormat`
//! selects.
//!
//! RULE: a function's `LoggingConfig$LogFormat` — "the format in which Lambda sends your
//! function's application and system logs to CloudWatch. Select between plain text and
//! structured JSON", defaulting to plain text for a function that names none — decides the
//! form of every record its invocations leave in its log group and report in their log tail.
//! Under the structured format each record is ONE JSON object: an application record the
//! runtime's logging interface produced carries the emitting logger's own LEVEL NAME, the
//! formatted message, the invocation's request id and the instant it was written, a failed
//! invocation's record carries the error's class and frames beside them, and each platform
//! frame Lambda writes itself names the `platform.*` record it is and carries that frame's
//! own fields. Under the plain-text format the same log is the runtime's tab-separated
//! framing and the `START`/`END`/`REPORT` lines.
//!
//! Output a handler writes straight to its standard streams is not a record of that
//! interface — it carries no level and no logger — so it is delivered as the text it was
//! written as under either format, which is what AWS documents for the Python runtime
//! ("Lambda doesn't convert print output to JSON"). It is still an application log, so the
//! second rule here judges it at the default level of detail: `ApplicationLogLevel` is the
//! filter on "the application logs for your function that Lambda sends to CloudWatch", so
//! Lambda sends an application record — one the logging interface produced, and one written
//! straight to a standard stream alike — only at the selected level of detail and lower,
//! while the invocation's own log tail still reports the whole log the run wrote.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! CloudWatch Logs and IAM mounted on ONE engine — the same harness
//! [`crate::invocation_log_delivery`] pins the delivery itself with, reused rather than
//! copied — so what is asserted is what a client reads back with `GetLogEvents` and off the
//! invocation's own `X-Amz-Log-Result`.
//!
//! The rows differ along what the rule generalises over: which format the function names
//! (the structured one, the plain-text one, and none at all, which defaults), which RUNTIME
//! wrote the records (the two engines, whose logging interfaces name their levels
//! differently), which LEVEL the record was emitted at, whether the invocation answered or
//! failed, and which log group the records are delivered to (the default one and one the
//! configuration names).
//!
//! Every test here RUNS handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::event_source_mappings::call;
use crate::invocation_log_delivery::{
    create_function_logging, delivered, emulator, groups, invocations_log, invoke,
    states_initialization, streams, structured_record as record, Created, LOGS, LOG_WRITES,
    NODE_RUNTIME, PYTHON_RUNTIME,
};

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// A python handler that logs ONE record at warning level through the logging interface and
/// writes one line straight to its standard output.
const PYTHON_LOGGING: &str = "import logging\n\nlogger = \
                              logging.getLogger()\n\n\ndef lambda_handler(event, \
                              context):\n    logger.warning('record-marker says %s', \
                              'hello')\n    print('printed-marker')\n    return {'ok': \
                              True}\n";

/// A python handler that fails, so the runtime writes its own error report.
const PYTHON_RAISES: &str = "def lambda_handler(event, context):\n    raise \
                             ValueError('record-marker says no')\n";

/// A node handler that logs ONE record at warning level through its console.
const NODE_LOGGING: &str = "exports.handler = async () => {\n    console.warn('record-marker \
                            says hello');\n    return { ok: true };\n};";

/// One row: the `LoggingConfig` the function is created with, the handler that writes, and the
/// form the delivered records must take.
struct Row {
    case: &'static str,
    /// The `LoggingConfig` the create names, or `None` for a create that names none.
    logging_config: Option<Value>,
    /// The `LogFormat` every read of the function must report.
    reported_format: &'static str,
    /// Whether the delivered records are the structured ones.
    structured: bool,
    /// The handler's source.
    source: &'static str,
    /// The runtime that runs it.
    runtime: &'static str,
    /// The level the record carrying the marker must be stated at, as the runtime that wrote
    /// it names its levels.
    level: &'static str,
    /// Whether the invocation FAILED, which is the record the runtime writes itself.
    fails: bool,
    /// Whether the line the handler wrote straight to its standard output is WITHHELD from
    /// CloudWatch Logs — it states no level of its own, so the application log level judges it
    /// at the default level of detail.
    withholds_printed: bool,
    /// The group the configuration sends the log to, or `None` for the default one.
    log_group: Option<&'static str>,
}

/// The marker every row's handler writes, which is how the record it wrote is found among the
/// delivered events.
const MARKER: &str = "record-marker";

/// The line a python handler writes straight to its standard output.
const PRINTED: &str = "printed-marker";

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a python function on the structured format",
            logging_config: Some(json!({
                "LogFormat": "JSON",
                "ApplicationLogLevel": "WARN",
                "SystemLogLevel": "INFO",
            })),
            reported_format: "JSON",
            structured: true,
            source: PYTHON_LOGGING,
            runtime: PYTHON_RUNTIME,
            // The level is the EMITTING LOGGER's own level name, which is not how
            // `ApplicationLogLevel` spells the filter above.
            level: "WARNING",
            fails: false,
            // The filter is above the default level of detail, so the line the handler wrote
            // straight to its standard output does not reach the group.
            withholds_printed: true,
            log_group: None,
        },
        Row {
            case: "a python function on the structured format logging into a named group",
            logging_config: Some(json!({
                "LogFormat": "JSON",
                "LogGroup": "/aws/lambda/records-elsewhere",
            })),
            reported_format: "JSON",
            structured: true,
            source: PYTHON_LOGGING,
            runtime: PYTHON_RUNTIME,
            level: "WARNING",
            fails: false,
            withholds_printed: false,
            log_group: Some("/aws/lambda/records-elsewhere"),
        },
        Row {
            case: "a python function on the structured format whose handler failed",
            logging_config: Some(json!({ "LogFormat": "JSON" })),
            reported_format: "JSON",
            structured: true,
            source: PYTHON_RAISES,
            runtime: PYTHON_RUNTIME,
            level: "ERROR",
            fails: true,
            withholds_printed: false,
            log_group: None,
        },
        Row {
            case: "a python function on the plain-text format",
            logging_config: Some(json!({ "LogFormat": "Text" })),
            reported_format: "Text",
            structured: false,
            source: PYTHON_LOGGING,
            runtime: PYTHON_RUNTIME,
            level: "WARNING",
            fails: false,
            withholds_printed: false,
            log_group: None,
        },
        Row {
            case: "a python function naming no LoggingConfig at all, which defaults",
            logging_config: None,
            reported_format: "Text",
            structured: false,
            source: PYTHON_LOGGING,
            runtime: PYTHON_RUNTIME,
            level: "WARNING",
            fails: false,
            withholds_printed: false,
            log_group: None,
        },
        Row {
            case: "a node function on the structured format",
            logging_config: Some(json!({ "LogFormat": "JSON" })),
            reported_format: "JSON",
            structured: true,
            source: NODE_LOGGING,
            runtime: NODE_RUNTIME,
            // A node console method names its own level, which is the one the record states.
            level: "WARN",
            fails: false,
            withholds_printed: false,
            log_group: None,
        },
        Row {
            case: "a node function naming no format, which defaults to plain text",
            logging_config: None,
            reported_format: "Text",
            structured: false,
            source: NODE_LOGGING,
            runtime: NODE_RUNTIME,
            level: "WARN",
            fails: false,
            withholds_printed: false,
            log_group: None,
        },
    ]
}

/// Create the row's function with the `LoggingConfig` it names, and wait until it is `Active`.
///
/// The create path itself is the suite's shared one
/// ([`crate::invocation_log_delivery::create_function_logging`]), so there is one writer of a
/// function whose log this target reads back.
async fn create(router: &MultiServiceRouter, row: &Row, name: &str, role: &str) {
    create_function_logging(
        router,
        &Created {
            case: row.case,
            name,
            source: row.source,
            runtime: row.runtime,
            role,
            log_group: None,
        },
        row.logging_config.as_ref(),
    )
    .await;
}

/// The `LoggingConfig.LogFormat` a read of the function reports.
async fn reported_format(router: &MultiServiceRouter, case: &str, name: &str) -> String {
    let read = call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status, 200,
        "case {case:?}: expected GetFunctionConfiguration to answer 200, got {} {}",
        read.status,
        read.text()
    );
    read.json()["LoggingConfig"]["LogFormat"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Every delivered record of one run, in the form the function's format selects.
#[tokio::test]
async fn the_log_format_selects_the_form_of_every_delivered_record() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let role = given_role(
            &router,
            case,
            &format!("record-format-{index}"),
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        let name = format!("record-format-{index}");
        let default_group = format!("/aws/lambda/{name}");
        let group = row.log_group.unwrap_or(&default_group).to_string();

        create(&router, row, &name, &role).await;
        let format = reported_format(&router, case, &name).await;
        assert_eq!(
            format, row.reported_format,
            "case {case:?}: expected a read to report the LogFormat {:?}, got {format:?}",
            row.reported_format
        );

        let (tail, _payload) = invoke(&router, case, &name).await;
        let found = groups(&router, case, &group).await;
        assert_eq!(
            found,
            vec![group.clone()],
            "case {case:?}: expected the invocation to deliver into {group:?}, got {found:?}"
        );
        let held = streams(&router, case, &group).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected one stream for one invocation, got {held:?}"
        );
        let (lines, _text) = delivered(&router, case, &group, &held[0]).await;
        // What the group holds beside the one record the ENVIRONMENT wrote for its own
        // initialization, which no invocation reports as its log.
        let text = invocations_log(&lines);
        if row.withholds_printed {
            // ONE log, read two ways — except for a record the function's application log
            // level withholds from CloudWatch Logs, which the invocation's own tail still
            // reports: the filter is on what Lambda SENDS to the group.
            assert!(
                tail.contains(PRINTED) && !text.contains(PRINTED),
                "case {case:?}: expected the withheld line in the tail and not in the group, \
                 got {text:?} against {tail:?}"
            );
            for line in lines.iter().filter(|line| !states_initialization(line)) {
                assert!(
                    tail.contains(line.as_str()),
                    "case {case:?}: expected every delivered record in the log the invocation \
                     reported, got {line:?} against {tail:?}"
                );
            }
        } else {
            // ONE log, read two ways: what the group holds is what the invocation reported.
            assert_eq!(
                text, tail,
                "case {case:?}: expected the group to hold the log the invocation reported, got \
                 {text:?} against {tail:?}"
            );
        }
        assert!(
            lines.len() >= 4,
            "case {case:?}: expected the three platform frames around what the handler wrote, \
             got {lines:?}"
        );

        if row.structured {
            assert_structured(case, row, &lines);
        } else {
            assert_plain_text(case, row, &lines);
        }
    }
}

/// A python handler that lowers its own logger to the most detailed level and writes ONE
/// record at each of four levels, so what reaches the group is the filter's doing and not the
/// logger's.
const PYTHON_EVERY_LEVEL: &str = "import logging\n\nlogger = \
                                  logging.getLogger()\nlogger.setLevel(logging.DEBUG)\n\n\ndef \
                                  lambda_handler(event, context):\n    \
                                  logger.debug('level-marker DEBUG')\n    \
                                  logger.info('level-marker INFO')\n    \
                                  logger.warning('level-marker WARNING')\n    \
                                  logger.error('level-marker ERROR')\n    return {'ok': True}\n";

/// The marker the handler above writes on every one of its records.
const LEVEL_MARKER: &str = "level-marker";

/// One row of the level table: the `LoggingConfig` the function carries, and the records the
/// level it names lets through to CloudWatch Logs.
struct Filtered {
    case: &'static str,
    /// The `LoggingConfig` the create names, or `None` for a create that names none.
    logging_config: Option<Value>,
    /// The levels of the records the group must hold, in the order the handler wrote them.
    /// The handler writes one record at each of DEBUG, INFO, WARNING and ERROR.
    delivered: &'static [&'static str],
}

fn filtered_rows() -> Vec<Filtered> {
    vec![
        Filtered {
            case: "the structured format naming no level, which filters at the documented \
                   default",
            logging_config: Some(json!({ "LogFormat": "JSON" })),
            delivered: &["INFO", "WARNING", "ERROR"],
        },
        Filtered {
            case: "a filter at the most detailed level, which withholds nothing",
            logging_config: Some(json!({ "LogFormat": "JSON", "ApplicationLogLevel": "TRACE" })),
            delivered: &["DEBUG", "INFO", "WARNING", "ERROR"],
        },
        Filtered {
            case: "a filter at the level the issue's function carries",
            logging_config: Some(json!({ "LogFormat": "JSON", "ApplicationLogLevel": "WARN" })),
            delivered: &["WARNING", "ERROR"],
        },
        Filtered {
            case: "a filter one level lower in detail",
            logging_config: Some(json!({ "LogFormat": "JSON", "ApplicationLogLevel": "ERROR" })),
            delivered: &["ERROR"],
        },
        Filtered {
            case: "a filter at the least detailed level, which withholds every record the \
                   handler wrote",
            logging_config: Some(json!({ "LogFormat": "JSON", "ApplicationLogLevel": "FATAL" })),
            delivered: &[],
        },
    ]
}

/// RULE: Lambda sends the application records of a function to CloudWatch Logs only at the
/// `ApplicationLogLevel` it selected and lower, and a function carrying no such filter — one
/// on the plain-text format, which can hold no level — has every record delivered.
#[tokio::test]
async fn the_application_log_level_decides_which_records_reach_cloudwatch_logs() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in filtered_rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let role = given_role(
            &router,
            case,
            &format!("record-level-{index}"),
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        let name = format!("record-level-{index}");
        let group = format!("/aws/lambda/{name}");
        let function = Row {
            case,
            logging_config: row.logging_config.clone(),
            reported_format: "JSON",
            structured: true,
            source: PYTHON_EVERY_LEVEL,
            runtime: PYTHON_RUNTIME,
            level: "WARNING",
            fails: false,
            withholds_printed: false,
            log_group: None,
        };
        create(&router, &function, &name, &role).await;
        invoke(&router, case, &name).await;

        let held = streams(&router, case, &group).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected one stream for one invocation, got {held:?}"
        );
        let (lines, _text) = delivered(&router, case, &group, &held[0]).await;
        let levels: Vec<String> = lines
            .iter()
            .filter_map(|line| record(line))
            .filter(|members| {
                members
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains(LEVEL_MARKER))
            })
            .map(|members| {
                members
                    .get("level")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string()
            })
            .collect();
        assert_eq!(
            levels,
            row.delivered
                .iter()
                .map(|level| (*level).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the records at {:?} to reach the group, got {levels:?} out \
             of {lines:?}",
            row.delivered
        );
        // Whatever the filter withholds, the frames of the run itself are delivered: the group
        // and its stream exist for every invocation.
        let frames: Vec<&String> = lines
            .iter()
            .filter(|line| {
                record(line).is_some_and(|members| members.contains_key("type"))
            })
            .collect();
        assert_eq!(
            frames.len(),
            6,
            "case {case:?}: expected the run's six platform frames — the three of the \
             environment's initialization and the three of the invocation — got {frames:?}"
        );
    }
}

/// RULE: a function on the plain-text format carries no level filter at all
/// ([`crate::log_level_requires_json_format`] pins that such a configuration is refused), so
/// every record its handler wrote is delivered.
#[tokio::test]
async fn a_plain_text_function_carries_no_level_filter() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a plain-text function whose handler logs at four levels";
    let router = emulator();
    let role = given_role(
        &router,
        case,
        "record-level-text",
        Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
    )
    .await;
    let name = "record-level-text";
    let group = format!("/aws/lambda/{name}");
    let function = Row {
        case,
        logging_config: Some(json!({ "LogFormat": "Text" })),
        reported_format: "Text",
        structured: false,
        source: PYTHON_EVERY_LEVEL,
        runtime: PYTHON_RUNTIME,
        level: "WARNING",
        fails: false,
        withholds_printed: false,
        log_group: None,
    };
    create(&router, &function, name, &role).await;
    invoke(&router, case, name).await;

    let held = streams(&router, case, &group).await;
    let (lines, _text) = delivered(&router, case, &group, &held[0]).await;
    for level in ["DEBUG", "INFO", "WARNING", "ERROR"] {
        let framing = format!("[{level}]\t");
        assert!(
            lines
                .iter()
                .any(|line| line.starts_with(&framing) && line.contains(LEVEL_MARKER)),
            "case {case:?}: expected the {level} line to be delivered, got {lines:?}"
        );
    }
}

/// Under the structured format every delivered line is one JSON object: the platform frames
/// of the run, and the application record the handler left.
fn assert_structured(case: &str, row: &Row, lines: &[String]) {
    let mut parsed = Vec::new();
    for line in lines {
        // The one line a handler wrote straight to its standard output is not a record of the
        // logging interface, so it is delivered as the text it was written as.
        if line.trim() == PRINTED {
            continue;
        }
        let Some(members) = record(line) else {
            panic!(
                "case {case:?}: the function's LogFormat is JSON, but the group holds an \
                 unstructured line: {line:?}"
            );
        };
        parsed.push(members);
    }
    // The frames Lambda writes itself: the environment's initialization starting, the runtime
    // reporting that phase done, the report of what it spent, then the run opened, the
    // handler's run ended, and what it spent — each naming the platform record it is.
    let kinds: Vec<&str> = parsed
        .iter()
        .filter_map(|members| members.get("type").and_then(Value::as_str))
        .collect();
    assert_eq!(
        kinds,
        vec![
            "platform.initStart",
            "platform.initRuntimeDone",
            "platform.initReport",
            "platform.start",
            "platform.runtimeDone",
            "platform.report"
        ],
        "case {case:?}: expected the run's platform records, got {kinds:?}"
    );
    let frame = parsed
        .iter()
        .find(|members| members.get("type").and_then(Value::as_str) == Some("platform.start"))
        .cloned()
        .unwrap_or_default();
    let request_id = frame["record"]["requestId"].as_str().unwrap_or_default().to_string();
    assert!(
        crate::invoke_request_id::is_request_id(&request_id),
        "case {case:?}: expected the opening frame to name the invocation, got {request_id:?}"
    );

    // The handler's own record: exactly one, carrying the level, the message, the invocation
    // and the instant. For a run the handler FAILED the record is the one the runtime writes
    // itself, which the Python runtime states the level of under `log_level` and carries the
    // failure's own sentence on `errorMessage` rather than on a message member.
    let (level_member, message_member) = if row.fails {
        ("log_level", "errorMessage")
    } else {
        ("level", "message")
    };
    let application: Vec<&serde_json::Map<String, Value>> = parsed
        .iter()
        .filter(|members| {
            members.get(level_member).is_some()
                && members
                    .get(message_member)
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains(MARKER))
        })
        .collect();
    assert_eq!(
        application.len(),
        1,
        "case {case:?}: expected exactly one structured application record carrying {MARKER:?}, \
         got {application:?}"
    );
    let found = application[0];
    let level = found.get(level_member).and_then(Value::as_str).unwrap_or_default();
    assert_eq!(
        level, row.level,
        "case {case:?}: expected the record's level under {level_member:?} to be {:?}, got \
         {level:?}",
        row.level
    );
    let attributed = found.get("requestId").and_then(Value::as_str).unwrap_or_default();
    assert_eq!(
        attributed, request_id,
        "case {case:?}: expected the record to name the invocation {request_id:?}, got \
         {attributed:?}"
    );
    let at = found.get("timestamp").and_then(Value::as_str).unwrap_or_default();
    assert!(
        at.len() >= 20 && at.ends_with('Z'),
        "case {case:?}: expected the record to carry the instant it was written, got {at:?}"
    );
    if row.fails {
        // A failed invocation's record states the class of the failure and its frames, which
        // is what a client reads a failure off.
        let error_type = found.get("errorType").and_then(Value::as_str).unwrap_or_default();
        assert_eq!(
            error_type, "ValueError",
            "case {case:?}: expected the record to name the error's class, got {error_type:?}"
        );
        let frames = found.get("stackTrace").and_then(Value::as_array);
        assert!(
            frames.is_some_and(|frames| !frames.is_empty()),
            "case {case:?}: expected the record to carry the handler's frames, got {frames:?}"
        );
    }
}

/// Under the plain-text format the same log is the runtime's own framing.
fn assert_plain_text(case: &str, row: &Row, lines: &[String]) {
    assert!(
        lines[0].starts_with("INIT_START Runtime Version: "),
        "case {case:?}: expected the log to open on the environment's initialization, got \
         {lines:?}"
    );
    assert!(
        lines[1].starts_with("START RequestId: "),
        "case {case:?}: expected the invocation's START line after it, got {lines:?}"
    );
    let closing: Vec<&String> = lines.iter().rev().take(2).collect();
    assert!(
        closing[0].starts_with("REPORT RequestId: ") && closing[1].starts_with("END RequestId: "),
        "case {case:?}: expected the END and REPORT lines to close the log, got {lines:?}"
    );
    let carrying: Vec<&String> = lines.iter().filter(|line| line.contains(MARKER)).collect();
    assert_eq!(
        carrying.len(),
        1,
        "case {case:?}: expected one line carrying {MARKER:?}, got {carrying:?}"
    );
    let line = carrying[0];
    assert!(
        record(line).is_none(),
        "case {case:?}: the function's LogFormat is Text, so its records are not JSON objects, \
         got {line:?}"
    );
    assert!(
        line.contains(row.level) && line.contains('\t'),
        "case {case:?}: expected the runtime's framing to state the level {:?}, got {line:?}",
        row.level
    );
}
