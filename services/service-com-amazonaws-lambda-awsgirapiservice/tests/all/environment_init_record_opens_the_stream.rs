// SPDX-License-Identifier: CC0-1.0

//! The record that OPENS the log stream of a new execution ENVIRONMENT under the structured log
//! format, and WHICH system levels send it.
//!
//! RULE: under `LoggingConfig$LogFormat` `JSON` — "the format in which Lambda sends your
//! function's application and system logs to CloudWatch. Select between plain text and structured
//! JSON" (`smithy_query(query="doc",shape="LoggingConfig",member="LogFormat")`) — the log stream of
//! an execution environment is opened by exactly ONE platform record stating that environment's
//! INITIALIZATION, written ahead of the first invocation that environment serves and before that
//! invocation's own opening frame, and every field of it is read from the function's configuration
//! and from the environment itself: the function's name and the version the environment holds, the
//! kind of initialization it was, the phase it reports, the instance the environment IS (the stream
//! it writes to) and the memory that instance may hold (the function's own `MemorySize`, in bytes).
//! An invocation an already-standing environment serves pays no initialization, so one environment
//! leaves one such record however many invocations it serves.
//!
//! That record is the ENVELOPE a client is reported a unit of work by, like the frame that opens an
//! invocation and the one that states what it spent, so `SystemLogLevel` sends it at the middle
//! level of detail and lower — the detail of HOW a phase was served is what only the most detailed
//! level admits ([`crate::invocation_system_record_levels`] owns the level of every kind) — and the
//! least detailed level withholds it.
//!
//! The rows differ along what the rule generalises over: the level the function filters its system
//! records at (every value the model's `SystemLogLevel` enum declares, plus the function that names
//! none and is filtered at the documented default), the `MemorySize` the function is configured for
//! (the one the harness creates functions with, and the largest the model allows), and the RUNTIME
//! the environment stands on (the two managed engines — the record is the HOST's, so neither
//! runtime writes it). Every row drives TWO invocations, so each is judged on a group holding more
//! runs than environments where the engine keeps one standing, and one environment per run where
//! it does not.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda, CloudWatch
//! Logs and IAM mounted on ONE engine — the harness [`crate::invocation_log_delivery`] pins the
//! delivery itself with, reused rather than copied — so what is asserted is what a client reads
//! back with `GetLogEvents`.
//!
//! This test RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::event_source_mappings::{await_function_active, call};
use crate::invocation_log_delivery::{
    create_function_logging, delivered, emulator, invoke, searched, streams,
    structured_record as record, Created, LOGS, LOG_WRITES, NODE_RUNTIME, PYTHON_RUNTIME,
};

/// Where `UpdateFunctionConfiguration` is bound, under the collection `CreateFunction` is.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The kind of the record that opens an execution environment's log, and the kind of the one that
/// opens an invocation's.
const INIT_START: &str = "platform.initStart";
const START: &str = "platform.start";

/// The version an environment of a function created without one holds: the unpublished version
/// every `CreateFunction` leaves behind.
const UNPUBLISHED_VERSION: &str = "$LATEST";

/// The kind of initialization an environment built to serve a synchronous invocation reports, which
/// is the value that invocation's own `AWS_LAMBDA_INITIALIZATION_TYPE` states.
const ON_DEMAND: &str = "on-demand";

/// The phase such a record reports.
const INIT_PHASE: &str = "init";

/// The unit `MemorySize` is stated in — "the amount of memory available to the function at
/// runtime", in MB (`smithy_query(query="shape",shape="MemorySize")`) — in the BYTES the record
/// states the instance's own ceiling in.
const BYTES_PER_MB: i64 = 1024 * 1024;

/// The largest `MemorySize` the model allows a function (`state::MAX_MEMORY_SIZE_MB`), which one
/// row configures so the record is judged against a configuration other than the harness's own.
const LARGEST_MEMORY_SIZE_MB: i32 = 10_240;

/// The level the rows filter the handler's own records at, with the record every handler below
/// writes: a record that is delivered whatever the system level withholds, which is how a row
/// waits for the delivery of a run whose platform records are all withheld.
const APPLICATION_LOG_LEVEL: &str = "WARN";
const MARKER: &str = "environment-record-marker";

/// The python handler every python row runs.
const PYTHON_HANDLER: &str = concat!(
    "import logging\n",
    "\n",
    "logger = logging.getLogger()\n",
    "\n",
    "\n",
    "def lambda_handler(event, context):\n",
    "    logger.warning('environment-record-marker')\n",
    "    return {'ok': True}\n",
);

/// The node handler the node row runs: the same one record, through the console method that
/// runtime maps to the level above.
const NODE_HANDLER: &str = concat!(
    "exports.handler = async () => {\n",
    "    console.warn('environment-record-marker');\n",
    "    return { ok: true };\n",
    "};\n",
);

/// One row: the system level the function names, the runtime its environment stands on, the
/// `MemorySize` it is configured for, and whether the level in force sends the record that opens
/// the environment's log.
struct Row {
    case: &'static str,
    runtime: &'static str,
    source: &'static str,
    /// The `SystemLogLevel` the function's `LoggingConfig` names, or `None` for a function that
    /// names none and is filtered at the documented default (`INFO`).
    selected: Option<&'static str>,
    /// The `MemorySize` to configure the function for, in the MB the member is stated in, or
    /// `None` to leave the one the harness creates it with. Only values at or above it, which the
    /// emulator's per-run heap ceiling clamps to the same memory, so a row varies what the record
    /// must REPORT and not what the run is given.
    memory_size_mb: Option<i32>,
    /// Whether the record that opens the environment's log reaches the function's log group.
    sent: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the most detailed level the model declares, which withholds nothing",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_HANDLER,
            selected: Some("DEBUG"),
            memory_size_mb: None,
            sent: true,
        },
        Row {
            case: "the middle level, at which the envelope of a unit of work is stated, on a \
                   function configured for the largest memory the model allows",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_HANDLER,
            selected: Some("INFO"),
            memory_size_mb: Some(LARGEST_MEMORY_SIZE_MB),
            sent: true,
        },
        Row {
            case: "a node function naming no system level, which is filtered at the documented \
                   default",
            runtime: NODE_RUNTIME,
            source: NODE_HANDLER,
            selected: None,
            memory_size_mb: None,
            sent: true,
        },
        Row {
            case: "the least detailed level, below the envelope every unit of work is reported by",
            runtime: PYTHON_RUNTIME,
            source: PYTHON_HANDLER,
            selected: Some("WARN"),
            memory_size_mb: None,
            sent: false,
        },
    ]
}

/// How long to wait between looks at the group, and how many looks to take. The delivery of a
/// run's log is not part of the invocation's answer, so every read of the group waits for the
/// CONDITION; the budget is far longer than any span this emulator chooses, so a loaded host is
/// slower without the rule being any different and nothing here asserts on elapsed time.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 600;

/// The `MemorySize` a read of the function reports for it, in the MB the member is stated in.
async fn configured_memory_size_mb(router: &MultiServiceRouter, case: &str, name: &str) -> i64 {
    let read = call(
        router,
        "GET",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &Value::Null,
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: expected a read of {name:?} to answer 200, got {} {}",
        read.status,
        read.text()
    );
    read.json()["MemorySize"].as_i64().unwrap_or_else(|| {
        panic!("case {case:?}: expected the read to report the function's MemorySize, got {}", read.text())
    })
}

/// Configure the function `name` for `megabytes` of memory and wait until the update has settled,
/// so the environment the rows invoke is built under that configuration.
async fn given_memory_size(router: &MultiServiceRouter, case: &str, name: &str, megabytes: i32) {
    let updated = call(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &json!({ "MemorySize": megabytes }),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "case {case:?}: expected the MemorySize of {name:?} to be updatable, got {} {}",
        updated.status,
        updated.text()
    );
    await_function_active(router, case, name).await;
}

/// The messages the group `group` holds once the logs of `runs` invocations have reached it, which
/// is once each run's own application record — the one record every row's levels admit — is there.
async fn delivered_runs(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    runs: usize,
) -> Vec<String> {
    let carries_marker = |held: &[String]| {
        held.iter()
            .filter(|message| message.contains(MARKER))
            .count()
    };
    let mut held: Vec<String> = Vec::new();
    for _ in 0..POLLS {
        held = searched(router, case, group).await;
        if carries_marker(&held) >= runs {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert!(
        carries_marker(&held) >= runs,
        "case {case:?}: expected the group {group:?} to hold the record each of the {runs} runs \
         wrote, got {held:?}"
    );
    held
}

/// The `type` of every platform record of `lines`, in the order they were delivered.
fn platform_kinds(lines: &[String]) -> Vec<String> {
    lines
        .iter()
        .filter_map(|line| record(line))
        .filter_map(|members| {
            members
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// RULE: under the structured format exactly ONE platform record states the initialization of an
/// execution environment, at the head of that environment's own log stream and before the first
/// invocation's opening frame, with every field read from the function's configuration and from the
/// environment; and `SystemLogLevel` sends it at the level the envelope of a unit of work is stated
/// at and lower.
#[tokio::test]
async fn one_initialization_record_opens_a_new_environments_log_stream() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("environment-record-{index}");
        let group = format!("/aws/lambda/{name}");
        // A role granted every log write, so what reaches CloudWatch Logs is the level's doing
        // and never the role's.
        let role = given_role(
            &router,
            case,
            &name,
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        let mut logging = json!({
            "LogFormat": "JSON",
            "ApplicationLogLevel": APPLICATION_LOG_LEVEL,
        });
        if let Some(level) = row.selected {
            logging["SystemLogLevel"] = json!(level);
        }
        create_function_logging(
            &router,
            &Created {
                case,
                name: &name,
                source: row.source,
                runtime: row.runtime,
                role: &role,
                log_group: None,
            },
            Some(&logging),
        )
        .await;
        if let Some(megabytes) = row.memory_size_mb {
            given_memory_size(&router, case, &name, megabytes).await;
            let reported = configured_memory_size_mb(&router, case, &name).await;
            assert_eq!(
                reported,
                i64::from(megabytes),
                "case {case:?}: expected the function to be configured for {megabytes} MB, got \
                 {reported}"
            );
        }
        let memory_size_mb = configured_memory_size_mb(&router, case, &name).await;

        // TWO invocations, so the record is judged on an environment that served more than the
        // run that built it: the first invocation builds one, and the second is served by it
        // wherever the engine it stands on keeps it standing.
        let (first, _payload) = invoke(&router, case, &name).await;
        let (second, _payload) = invoke(&router, case, &name).await;
        for (which, log) in [("the first run", &first), ("the second run", &second)] {
            assert!(
                !log.contains(INIT_START),
                "case {case:?}: the initialization is the ENVIRONMENT's record, so {which} \
                 reports none of it as its own log, got {log:?}"
            );
        }

        // One stream per execution environment: whichever environments served the two runs, each
        // is judged on the stream it writes to.
        let _ = delivered_runs(&router, case, &group, 2).await;
        let held = streams(&router, case, &group).await;
        assert!(
            !held.is_empty(),
            "case {case:?}: expected the environments that served the runs to have streams, got \
             {held:?}"
        );
        let mut initializations = 0_usize;
        let mut invocations = 0_usize;
        for stream in &held {
            let (lines, _text) = delivered(&router, case, &group, stream).await;
            let kinds = platform_kinds(&lines);
            initializations += kinds.iter().filter(|kind| *kind == INIT_START).count();
            invocations += kinds.iter().filter(|kind| *kind == START).count();
            if !row.sent {
                // The least detailed level withholds the whole envelope, so the stream holds
                // the handler's own records alone.
                assert!(
                    kinds.is_empty(),
                    "case {case:?}: expected no platform record of the stream {stream:?} at this \
                     level, got {kinds:?}"
                );
                continue;
            }

            // ONE initialization record, and it OPENS the stream: the first record of it, ahead
            // of the opening frame of every invocation that environment served.
            assert_eq!(
                kinds.iter().filter(|kind| *kind == INIT_START).count(),
                1,
                "case {case:?}: expected one initialization record in the stream {stream:?}, got \
                 {kinds:?} out of {lines:?}"
            );
            assert_eq!(
                kinds.first().map(String::as_str),
                Some(INIT_START),
                "case {case:?}: expected the initialization to open the stream {stream:?}, got \
                 {kinds:?} out of {lines:?}"
            );

            // Every field of it is read from the function's configuration and from the
            // environment.
            let stated = lines
                .iter()
                .filter_map(|line| record(line))
                .find(|members| members.get("type").and_then(Value::as_str) == Some(INIT_START))
                .unwrap_or_else(|| {
                    panic!("case {case:?}: expected the initialization record, got {lines:?}")
                });
            let about = stated.get("record").cloned().unwrap_or_default();
            // (the member, what the record must state it as)
            let expected: Vec<(&str, Value)> = vec![
                ("functionName", json!(name.clone())),
                ("functionVersion", json!(UNPUBLISHED_VERSION)),
                ("initializationType", json!(ON_DEMAND)),
                ("phase", json!(INIT_PHASE)),
                // The instance the environment IS, as the stream it writes to names it.
                ("instanceId", json!(stream.clone())),
                // What that instance may hold: the function's own MemorySize, in bytes.
                ("instanceMaxMemory", json!(memory_size_mb * BYTES_PER_MB)),
            ];
            for (member, value) in expected {
                assert_eq!(
                    about[member], value,
                    "case {case:?}: expected the initialization record's {member} to be {value}, \
                     got {about}"
                );
            }
            let at = stated
                .get("time")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert!(
                at.len() >= 20 && at.ends_with('Z'),
                "case {case:?}: expected the record to carry the instant it was written, got \
                 {at:?} in {stated:?}"
            );
        }
        if !row.sent {
            continue;
        }
        // The record is the ENVIRONMENT's and not the invocation's: one per environment however
        // many invocations that environment served, against the two opening frames of the two
        // runs.
        assert_eq!(
            initializations,
            held.len(),
            "case {case:?}: expected one initialization record per environment, got \
             {initializations} across the streams {held:?}"
        );
        assert_eq!(
            invocations, 2,
            "case {case:?}: expected the two runs' own opening frames in the group, got \
             {invocations} across the streams {held:?}"
        );
    }
}
