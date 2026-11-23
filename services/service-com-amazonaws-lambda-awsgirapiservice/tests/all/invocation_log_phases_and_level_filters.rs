// SPDX-License-Identifier: CC0-1.0

//! The PHASES an invocation's log states and the two LEVELS that filter it.
//!
//! RULE: an invocation's log is the records its runtime and the platform wrote, in the phases
//! they were written in and in the function's own `LoggingConfig$LogFormat` — the
//! initialization of the execution environment opens that environment's log, before the first
//! invocation's own opening frame, and the invocation's report states what the initialization
//! took; and each level member is the filter on the records of its own KIND:
//! `ApplicationLogLevel` is "set this property to filter the application logs for your
//! function that Lambda sends to CloudWatch. Lambda only sends application logs at the
//! selected level of detail and lower", which the managed runtimes read as
//! `AWS_LAMBDA_LOG_LEVEL` and configure their own logging interface from, while
//! `SystemLogLevel` filters the platform records independently of it.
//!
//! Every test drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! CloudWatch Logs and IAM mounted on ONE engine — the harness
//! [`crate::invocation_log_delivery`] pins the delivery itself with, reused rather than copied
//! — so what is asserted is what a client reads back with `GetLogEvents` and off the
//! invocation's own `X-Amz-Log-Result`.
//!
//! The rows differ along what the rule generalises over: which FORMAT the function selected
//! (the plain-text one and the structured one), which RUNTIME wrote the records (the two
//! engines, whose logging interfaces name and render their records differently), which LEVEL
//! each filter names (every value the model's two level enums declare, at and around the level
//! the records were emitted at), and whether the invocation BUILT the environment it ran in or
//! was served by one already standing.
//!
//! Every test here RUNS handlers, so each takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::invocation_log_delivery::{
    create_function_logging, delivered, emulator, function_memory_size_mb, groups, invoke,
    streams, structured_record as record, Created, LOGS, LOG_WRITES, NODE_RUNTIME, PYTHON_RUNTIME,
};

/// The line the handlers below write while their module is being imported, which is the
/// INIT phase: it belongs to the initialization of the environment and not to the invocation
/// that happened to pay for it.
const INIT_MARKER: &str = "INIT-module";

/// The line they write from inside the handler, which belongs to the invocation.
const HANDLER_MARKER: &str = "HANDLER-line";

/// The record that opens the plain-text log of a new execution environment.
const INIT_START_PREFIX: &str = "INIT_START Runtime Version: ";

/// The field the report of a run that paid an initialization states that span in.
const INIT_DURATION_FIELD: &str = "\tInit Duration: ";

/// A python handler that writes during its own import, logs one record at each of four levels
/// and reports what the runtime told it about the level filter.
const PYTHON_LEVELS: &str = r#"import logging
import os

print("INIT-module")
logger = logging.getLogger()


def lambda_handler(event, context):
    logger.debug("level-marker DEBUG")
    logger.info("level-marker INFO")
    logger.warning("level-marker WARNING")
    logger.error("level-marker ERROR")
    print("HANDLER-line")
    return {
        "level": os.environ.get("AWS_LAMBDA_LOG_LEVEL"),
        "effective": logging.getLogger().getEffectiveLevel(),
    }
"#;

/// A node handler that writes during its own load and logs through every console method the
/// runtime maps to a level.
const NODE_LEVELS: &str = "process.stdout.write('INIT-module\\n');\nexports.handler = async () \
                           => {\n    console.trace('level-marker TRACE');\n    \
                           console.debug('level-marker DEBUG');\n    console.info('level-marker \
                           INFO');\n    console.log('level-marker INFO-log');\n    \
                           console.warn('level-marker WARN');\n    console.error('level-marker \
                           ERROR');\n    process.stdout.write('HANDLER-line\\n');\n    return { \
                           level: process.env.AWS_LAMBDA_LOG_LEVEL === undefined ? null : \
                           process.env.AWS_LAMBDA_LOG_LEVEL };\n};\n";

/// A node handler that logs ONE object and ONE error through its console, which is what the
/// structured format renders as a record rather than as the runtime's inspect text.
const NODE_STRUCTURED: &str = "exports.handler = async () => {\n    console.log({ marker: \
                               'obj-marker', data: 12.3, flag: false, list: [1, 'two'] });\n    \
                               console.error('err-marker', new RangeError('range-boom'));\n    \
                               return { ok: true };\n};\n";

/// The marker every level row's record carries.
const LEVEL_MARKER: &str = "level-marker";

/// Create the function `name` with `logging_config` and wait until it is `Active`, under a role
/// granted every log write — so what reaches CloudWatch Logs is the level filter's doing and
/// never the role's.
async fn created(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    runtime: &str,
    source: &str,
    logging_config: Option<&Value>,
) {
    let role = given_role(
        router,
        case,
        name,
        Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
    )
    .await;
    create_function_logging(
        router,
        &Created {
            case,
            name,
            source,
            runtime,
            role: &role,
            log_group: None,
        },
        logging_config,
    )
    .await;
}

/// The `type` of every platform record of `tail`, in order.
fn platform_kinds(tail: &str) -> Vec<String> {
    tail.lines()
        .filter_map(record)
        .filter_map(|members| {
            members
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// One row of the phase table: the runtime that ran, the format the function selected, and
/// whether its records are the structured ones.
struct Phase {
    case: &'static str,
    runtime: &'static str,
    source: &'static str,
    /// The `LoggingConfig` the create names, or `None` for a function that names none.
    logging_config: Option<Value>,
    /// Whether the records are the structured ones.
    structured: bool,
}

fn phase_rows() -> Vec<Phase> {
    vec![
        Phase {
            case: "a python function on the plain-text format",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_LEVELS,
            logging_config: None,
            structured: false,
        },
        Phase {
            case: "a python function on the structured format",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_LEVELS,
            // The records of the INIT phase state the detail of how a run was served, so they
            // reach the group only at the most detailed system level
            // ([`crate::invocation_system_record_levels`] owns that rule): a row asserting the
            // phase a record belongs to selects the level that withholds none of them.
            logging_config: Some(json!({ "LogFormat": "JSON", "SystemLogLevel": "DEBUG" })),
            structured: true,
        },
        Phase {
            case: "a node function on the plain-text format",
            runtime: NODE_RUNTIME,
            source: NODE_LEVELS,
            logging_config: None,
            structured: false,
        },
        Phase {
            case: "a node function on the structured format",
            runtime: NODE_RUNTIME,
            source: NODE_LEVELS,
            logging_config: Some(json!({ "LogFormat": "JSON", "SystemLogLevel": "DEBUG" })),
            structured: true,
        },
    ]
}

/// RULE: the log of an execution ENVIRONMENT opens on its initialization — the record that
/// states the runtime version it stands on, ahead of the first invocation's own opening frame
/// — and the output the init phase wrote belongs to the invocation that paid for it, before
/// that frame, whose report states what the initialization took. The initialization record is
/// the environment's rather than the invocation's, so it is in the function's log group and
/// not in what the invocation reports as its own log.
#[tokio::test]
async fn the_initialization_of_an_environment_opens_its_log_before_the_first_invocation() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in phase_rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("log-phase-{index}");
        let group = format!("/aws/lambda/{name}");
        created(
            &router,
            case,
            &name,
            row.runtime,
            row.source,
            row.logging_config.as_ref(),
        )
        .await;
        let (tail, _payload) = invoke(&router, case, &name).await;
        let lines: Vec<&str> = tail.lines().collect();
        let held = streams(&router, case, &group).await;
        assert_eq!(
            held.len(),
            1,
            "case {case:?}: expected the one environment's one stream, got {held:?}"
        );
        let (delivered_records, _text) = delivered(&router, case, &group, &held[0]).await;

        if row.structured {
            let kinds = platform_kinds(&delivered_records.join("\n"));
            assert_eq!(
                kinds.first().map(String::as_str),
                Some("platform.initStart"),
                "case {case:?}: expected the initialization to open the environment's log, got \
                 {kinds:?} in {delivered_records:?}"
            );
            assert_eq!(
                kinds.iter().filter(|kind| *kind == "platform.initStart").count(),
                1,
                "case {case:?}: expected one initialization record, got {kinds:?}"
            );
            let opened = delivered_records
                .iter()
                .filter_map(|line| record(line))
                .find(|members| {
                    members.get("type").and_then(Value::as_str) == Some("platform.initStart")
                })
                .unwrap_or_default();
            let about = &opened["record"];
            // (the member, what the record must state it as)
            let stated: Vec<(&str, Value)> = vec![
                ("initializationType", json!("on-demand")),
                ("phase", json!("init")),
                ("functionName", json!(name.clone())),
                ("functionVersion", json!("$LATEST")),
                (
                    "instanceMaxMemory",
                    json!(i64::from(function_memory_size_mb()) * 1024 * 1024),
                ),
                // The instance the environment IS, as the stream it writes to.
                ("instanceId", json!(held[0].clone())),
            ];
            for (member, expected) in stated {
                assert_eq!(
                    about[member], expected,
                    "case {case:?}: expected the initialization record's {member} to be \
                     {expected}, got {about}"
                );
            }
        } else {
            assert!(
                delivered_records
                    .first()
                    .is_some_and(|line| line.starts_with(INIT_START_PREFIX)),
                "case {case:?}: expected the environment's log to open on the initialization, \
                 got {delivered_records:?}"
            );
        }
        // It is the ENVIRONMENT's record, so the invocation reports none of it as its own log.
        assert!(
            !tail.contains(INIT_START_PREFIX) && !tail.contains("platform.initStart"),
            "case {case:?}: expected the initialization record in the group and not in the \
             invocation's own log, got {tail:?}"
        );

        // Whatever the format, the output the INIT phase wrote precedes the invocation's own
        // opening frame, and the handler's own output follows it.
        let position = |needle: &str| {
            lines
                .iter()
                .position(|line| line.contains(needle))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected {needle:?} in the log, got {tail:?}")
                })
        };
        let opening = if row.structured {
            position("platform.start")
        } else {
            position("START RequestId: ")
        };
        assert!(
            position(INIT_MARKER) < opening,
            "case {case:?}: expected the init-phase output before the opening frame, got {tail:?}"
        );
        assert!(
            opening < position(HANDLER_MARKER),
            "case {case:?}: expected the handler's own output after it, got {tail:?}"
        );

        // And the report of the run that built the environment states what that cost.
        if row.structured {
            let report = lines
                .iter()
                .filter_map(|line| record(line))
                .find(|members| {
                    members.get("type").and_then(Value::as_str) == Some("platform.report")
                })
                .unwrap_or_default();
            let spent = report["record"]["metrics"]["initDurationMs"].as_f64();
            assert!(
                spent.is_some_and(|spent| spent > 0.0),
                "case {case:?}: expected the report to state what the initialization took, got \
                 {report:?}"
            );
        } else {
            let report = lines
                .iter()
                .find(|line| line.starts_with("REPORT RequestId: "))
                .copied()
                .unwrap_or_default();
            assert!(
                report.contains(INIT_DURATION_FIELD),
                "case {case:?}: expected the report to state what the initialization took, got \
                 {report:?}"
            );
        }
    }
}

/// One row of the application-level table: the level the function names, the level the runtime
/// must be told, and the records of the handler that survive it.
struct Application {
    case: &'static str,
    /// The `LoggingConfig` the create names, or `None` for a function that names none.
    logging_config: Option<Value>,
    /// The level the runtime must read out of its own environment, or `None` for a function
    /// that filters by none.
    told: Option<&'static str>,
    /// What the root logger's effective level must be, for the runtime whose logging library
    /// states one.
    effective: i64,
    /// The levels of the handler's records the log must carry, in the order it wrote them. The
    /// handler writes one record at each of DEBUG, INFO, WARNING and ERROR.
    kept: &'static [&'static str],
}

/// The levels the python logging library states for the levels the model declares. Source: the
/// model's `ApplicationLogLevel` enum against the levels `logging` declares, which is what the
/// managed runtime sets the root logger to.
const PYTHON_LEVEL_VALUES: &[(&str, i64)] = &[
    ("TRACE", 5),
    ("DEBUG", 10),
    ("INFO", 20),
    ("WARN", 30),
    ("ERROR", 40),
    ("FATAL", 50),
];

fn application_rows() -> Vec<Application> {
    vec![
        Application {
            case: "a function on the plain-text format, which filters by no level at all",
            logging_config: None,
            told: None,
            // Nothing lowers or raises the root logger, so it stays where the library leaves
            // it: a warning.
            effective: 30,
            kept: &["WARNING", "ERROR"],
        },
        Application {
            case: "the structured format naming no level, which filters at the documented \
                   default",
            logging_config: Some(json!({ "LogFormat": "JSON" })),
            told: Some("INFO"),
            effective: 20,
            kept: &["INFO", "WARNING", "ERROR"],
        },
        Application {
            case: "the most detailed level the model declares, which withholds nothing",
            logging_config: Some(json!({ "LogFormat": "JSON", "ApplicationLogLevel": "TRACE" })),
            told: Some("TRACE"),
            effective: 5,
            kept: &["DEBUG", "INFO", "WARNING", "ERROR"],
        },
        Application {
            case: "one level lower in detail than the records at the top",
            logging_config: Some(json!({ "LogFormat": "JSON", "ApplicationLogLevel": "WARN" })),
            told: Some("WARN"),
            effective: 30,
            kept: &["WARNING", "ERROR"],
        },
        Application {
            case: "the least detailed level of all, which withholds every record the handler \
                   wrote",
            logging_config: Some(json!({ "LogFormat": "JSON", "ApplicationLogLevel": "FATAL" })),
            told: Some("FATAL"),
            effective: 50,
            kept: &[],
        },
    ]
}

/// RULE: the function's `ApplicationLogLevel` reaches the RUNTIME as `AWS_LAMBDA_LOG_LEVEL`
/// and configures its logging interface, so the runtime writes the records at that level of
/// detail and lower and none of the rest — and a function that names no level leaves the
/// interface exactly as its library does.
#[tokio::test]
async fn the_application_log_level_reaches_the_runtime_and_filters_what_it_writes() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in application_rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("log-level-{index}");
        created(
            &router,
            case,
            &name,
            PYTHON_RUNTIME,
            PYTHON_LEVELS,
            row.logging_config.as_ref(),
        )
        .await;
        let (tail, payload) = invoke(&router, case, &name).await;

        assert_eq!(
            payload["level"],
            row.told.map_or(Value::Null, |level| json!(level)),
            "case {case:?}: expected the runtime to be told the level {:?}, got {payload}",
            row.told
        );
        assert_eq!(
            payload["effective"],
            json!(row.effective),
            "case {case:?}: expected the root logger at {}, got {payload}",
            row.effective
        );
        // Every level the ladder above places is one of the levels the model declares, which
        // is what the row's expectation is read from.
        assert!(
            row.told.is_none_or(|level| PYTHON_LEVEL_VALUES
                .iter()
                .any(|(named, placed)| *named == level && *placed == row.effective)),
            "case {case:?}: expected the level {:?} to be the one the library states, got {}",
            row.told,
            row.effective
        );

        let levels: Vec<String> = tail
            .lines()
            .filter(|line| line.contains(LEVEL_MARKER))
            .map(|line| {
                let member = record(line)
                    .and_then(|members| {
                        members.get("level").and_then(Value::as_str).map(str::to_string)
                    })
                    // The plain-text framing states the level in brackets, as its first field.
                    .unwrap_or_else(|| {
                        line.trim_start_matches('[')
                            .split(']')
                            .next()
                            .unwrap_or_default()
                            .to_string()
                    });
                member
            })
            .collect();
        assert_eq!(
            levels,
            row.kept
                .iter()
                .map(|level| (*level).to_string())
                .collect::<Vec<String>>(),
            "case {case:?}: expected the records at {:?} to be written, got {levels:?} out of \
             {tail:?}",
            row.kept
        );
    }
}

/// RULE: under the structured format a node console record is ONE JSON object naming the level
/// of the method that wrote it, a single object argument stays a JSON object message, and an
/// error among several arguments names its own class, message and stack on the record.
#[tokio::test]
async fn a_node_console_record_is_rendered_as_the_record_the_format_selects() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "a node function on the structured format";
    let router = emulator();
    let name = "log-node-structured";
    created(
        &router,
        case,
        name,
        NODE_RUNTIME,
        NODE_STRUCTURED,
        Some(&json!({ "LogFormat": "JSON" })),
    )
    .await;
    let (tail, _payload) = invoke(&router, case, name).await;
    let records: Vec<serde_json::Map<String, Value>> =
        tail.lines().filter_map(record).collect();

    // The object argument is a JSON object message, which is what a client that selected the
    // structured format parses rather than the runtime's inspect text of it.
    let logged = records
        .iter()
        .find(|members| members.get("message").is_some_and(Value::is_object))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a record whose message is an object, got {tail:?}")
        });
    assert_eq!(
        logged["message"],
        json!({ "marker": "obj-marker", "data": 12.3, "flag": false, "list": [1, "two"] }),
        "case {case:?}: expected the object argument to stay structured, got {logged:?}"
    );
    assert_eq!(
        logged["level"], json!("INFO"),
        "case {case:?}: expected console.log to name its own level, got {logged:?}"
    );

    // The error among several arguments names its class, its message and its stack beside the
    // formatted line.
    let failed = records
        .iter()
        .find(|members| members.contains_key("errorType"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected a record carrying the error's fields, got {tail:?}")
        });
    // (the member, what the record must state)
    for (member, expected) in [
        ("level", json!("ERROR")),
        ("errorType", json!("RangeError")),
        ("errorMessage", json!("range-boom")),
    ] {
        assert_eq!(
            failed[member], expected,
            "case {case:?}: expected the record's {member} to be {expected}, got {failed:?}"
        );
    }
    let frames = failed["stackTrace"].as_array().cloned().unwrap_or_default();
    assert!(
        frames
            .first()
            .and_then(Value::as_str)
            .is_some_and(|head| head == "RangeError: range-boom"),
        "case {case:?}: expected the error's own stack on the record, got {failed:?}"
    );
    assert!(
        failed["message"]
            .as_str()
            .is_some_and(|line| line.starts_with("err-marker ")),
        "case {case:?}: expected the formatted line as the message, got {failed:?}"
    );
}

/// One row of the system-level table: the level the function names, and whether the platform
/// records reach CloudWatch Logs under it.
struct System {
    case: &'static str,
    /// The level `SystemLogLevel` names.
    level: &'static str,
    /// Whether the platform records of an ordinary run are delivered.
    delivers_platform: bool,
}

fn system_rows() -> Vec<System> {
    vec![
        System {
            case: "the most detailed system level the model declares",
            level: "DEBUG",
            delivers_platform: true,
        },
        System {
            case: "the level the frames of a run are stated at",
            level: "INFO",
            delivers_platform: true,
        },
        System {
            case: "one level lower in detail than those frames",
            level: "WARN",
            delivers_platform: false,
        },
    ]
}

/// RULE: `SystemLogLevel` filters the PLATFORM records Lambda sends to CloudWatch Logs
/// independently of the application ones — so a client can suppress the platform noise and
/// keep its function's own records — while the invocation's own log tail still reports the
/// whole log the run wrote.
#[tokio::test]
async fn the_system_log_level_filters_the_platform_records_independently() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in system_rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("log-system-{index}");
        let group = format!("/aws/lambda/{name}");
        created(
            &router,
            case,
            &name,
            PYTHON_RUNTIME,
            PYTHON_LEVELS,
            Some(&json!({
                "LogFormat": "JSON",
                "ApplicationLogLevel": "INFO",
                "SystemLogLevel": row.level,
            })),
        )
        .await;
        let (tail, _payload) = invoke(&router, case, &name).await;

        // Whatever the filter withholds from the group, the tail reports the whole log the run
        // wrote: the filter is on what Lambda SENDS.
        let written = platform_kinds(&tail);
        for kind in ["platform.start", "platform.runtimeDone", "platform.report"] {
            assert!(
                written.iter().any(|named| named == kind),
                "case {case:?}: expected the run's own log to carry {kind:?}, got {written:?}"
            );
        }

        let found = groups(&router, case, &group).await;
        assert_eq!(
            found,
            vec![group.clone()],
            "case {case:?}: expected the invocation to create {group:?}, got {found:?}"
        );
        let held = streams(&router, case, &group).await;
        let (lines, _text) = delivered(&router, case, &group, &held[0]).await;
        let delivered_kinds: Vec<String> = lines
            .iter()
            .filter_map(|line| record(line))
            .filter_map(|members| {
                members
                    .get("type")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect();
        assert_eq!(
            !delivered_kinds.is_empty(),
            row.delivers_platform,
            "case {case:?}: expected the platform records to be delivered: {}, got \
             {delivered_kinds:?}",
            row.delivers_platform
        );

        // The APPLICATION records are judged by the other level alone, so they are delivered
        // whatever this one withholds.
        let application: Vec<&String> = lines
            .iter()
            .filter(|line| line.contains(LEVEL_MARKER))
            .collect();
        assert_eq!(
            application.len(),
            3,
            "case {case:?}: expected the records at INFO and lower in detail to be delivered, \
             got {application:?}"
        );
        // Each of them is a record the RUNTIME wrote, rendered by that runtime's own JSON
        // serializer — so a client reading the text of a python function's record finds the
        // members laid out as that library lays them out.
        assert!(
            application
                .iter()
                .any(|line| line.contains("\"message\": \"level-marker INFO\"")),
            "case {case:?}: expected the record as the runtime's own serializer wrote it, got \
             {application:?}"
        );
    }
}

/// RULE: an invocation an already-initialized environment serves pays no initialization — it
/// writes to the stream that environment opened, states no initialization of its own and
/// reports no span for one.
#[tokio::test]
async fn a_warm_invocation_shares_the_environments_stream_and_states_no_initialization() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let case = "two invocations of one python function";
    let router = emulator();
    let name = "log-warm-reuse";
    let group = format!("/aws/lambda/{name}");
    created(&router, case, name, PYTHON_RUNTIME, PYTHON_LEVELS, None).await;

    let (cold, _) = invoke(&router, case, name).await;
    let (warm, _) = invoke(&router, case, name).await;
    // The output the init phase wrote belongs to the invocation that paid for it, and to no
    // invocation served after it.
    assert!(
        cold.contains(INIT_MARKER),
        "case {case:?}: expected the first run to report the init-phase output it paid for, got \
         {cold:?}"
    );
    assert!(
        !warm.contains(INIT_MARKER),
        "case {case:?}: expected the second run to report no initialization, got {warm:?}"
    );
    for (which, log, expected) in [("the cold run", &cold, true), ("the warm run", &warm, false)] {
        let report = log
            .lines()
            .find(|line| line.starts_with("REPORT RequestId: "))
            .unwrap_or_default();
        assert_eq!(
            report.contains(INIT_DURATION_FIELD),
            expected,
            "case {case:?}: expected {which} to state an initialization span: {expected}, got \
             {report:?}"
        );
    }

    // ONE stream for the one environment that served both: a stream is a property of the
    // environment, not of the invocation — and the environment was initialized ONCE, which is
    // the one record of its own it holds, ahead of both runs.
    let held = streams(&router, case, &group).await;
    assert_eq!(
        held.len(),
        1,
        "case {case:?}: expected the environment's one stream to hold both runs, got {held:?}"
    );
    let (records, text) = delivered(&router, case, &group, &held[0]).await;
    let initialized: Vec<&String> = records
        .iter()
        .filter(|line| line.starts_with(INIT_START_PREFIX))
        .collect();
    assert_eq!(
        initialized.len(),
        1,
        "case {case:?}: expected the environment to be initialized once, got {records:?}"
    );
    assert_eq!(
        text,
        format!("{}\n{cold}{warm}", initialized[0]),
        "case {case:?}: expected the stream to hold the initialization and both runs' logs, got \
         {text:?}"
    );
}
