// SPDX-License-Identifier: CC0-1.0

//! WHICH of the platform's own records of a run reach the function's Amazon CloudWatch Logs
//! group, kind by kind, at the level the function filters its SYSTEM logs at.
//!
//! RULE: `LoggingConfig$SystemLogLevel` is "set this property to filter the system logs for your
//! function that Lambda sends to CloudWatch. Lambda only sends system logs at the selected level
//! of detail and lower, where `DEBUG` is the highest level and `WARN` is the lowest", and each
//! platform record is judged at the level ITS OWN KIND is stated at: the two frames that state
//! that an invocation happened and what it spent are the envelope every invocation is reported
//! by, stated at the middle level, while the records of one PHASE of the execution environment
//! and the record that closes the runtime's work on the invocation state the DETAIL of how the
//! run was served and are stated at the most detailed level. So a record more detailed than the
//! selected level is withheld even though the run wrote it: the filter is on what Lambda SENDS,
//! which the invocation's own log tail — the whole log the run wrote — is the independent
//! statement of. It is applied independently of `ApplicationLogLevel`, which governs the
//! handler's own records alone.
//!
//! The rows differ along what the rule generalises over: the level the function selects (every
//! value the model's `SystemLogLevel` enum declares, plus the function that names none and is
//! filtered at the documented default), each driven against EVERY kind of platform record one
//! cold run leaves — so each kind is judged at its own level, above it and below it.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! CloudWatch Logs and IAM mounted on ONE engine — the harness
//! [`crate::invocation_log_delivery`] pins the delivery itself with, reused rather than copied —
//! so what is asserted is what a client reads back with `GetLogEvents`.
//!
//! This test RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::time::Duration;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::invocation_log_delivery::{
    create_function_logging, emulator, invoke, searched, structured_record as record, Created,
    LOGS, LOG_WRITES, PYTHON_RUNTIME,
};

/// Every level of detail a function may filter its system records at, from the most detailed to
/// the least. Source: the values the model's `SystemLogLevel` enum declares (`DEBUG | INFO |
/// WARN`).
const SYSTEM_LOG_LEVELS: [&str; 3] = ["DEBUG", "INFO", "WARN"];

/// The level a function on the structured format is filtered at until it names one, which AWS
/// documents for the member ("Default: INFO").
const DEFAULT_SYSTEM_LOG_LEVEL: &str = "INFO";

/// The level each KIND of platform record a successful run leaves is stated at, in the order the
/// platform writes them.
///
/// Source: the levels AWS states for the system records of a run
/// (<https://docs.aws.amazon.com/lambda/latest/dg/monitoring-cloudwatchlogs-log-level.html>),
/// against the records one COLD run of a function leaves.
const RECORD_LEVELS: [(&str, &str); 6] = [
    ("platform.initStart", "DEBUG"),
    ("platform.initRuntimeDone", "DEBUG"),
    ("platform.initReport", "DEBUG"),
    ("platform.start", "INFO"),
    ("platform.runtimeDone", "DEBUG"),
    ("platform.report", "INFO"),
];

/// The kind that opens the log of an execution ENVIRONMENT rather than of an invocation: the
/// record of its initialization, which is in the function's log group and not in what any
/// invocation reports as its own log.
const ENVIRONMENT_RECORD: &str = "platform.initStart";

/// Whether a function filtering its system records at `filter` is sent a record stated at
/// `stated`: a filter at least as detailed as the record admits it, and every less detailed one
/// withholds it.
fn admits(filter: &str, stated: &str) -> bool {
    let placed = |named: &str| {
        SYSTEM_LOG_LEVELS
            .iter()
            .position(|value| *value == named)
            .unwrap_or_else(|| panic!("{named:?} is one of the levels the model declares"))
    };
    placed(filter) <= placed(stated)
}

/// The level in force on a function whose `LoggingConfig` names `selected`.
fn in_force(selected: Option<&str>) -> &str {
    selected.unwrap_or(DEFAULT_SYSTEM_LOG_LEVEL)
}

/// The kinds of platform record the group of a function filtering at `filter` must hold after one
/// cold, successful run, in the order the platform writes them.
fn admitted_kinds(filter: &str) -> Vec<String> {
    RECORD_LEVELS
        .iter()
        .filter(|(_, stated)| admits(filter, stated))
        .map(|(kind, _)| (*kind).to_string())
        .collect()
}

/// The handler every row runs: it logs ONE record through the runtime's own logging interface, at
/// the level the application filter below admits, so each row shows that the system level
/// withholds no application record.
const PYTHON_HANDLER: &str = concat!(
    "import logging\n",
    "\n",
    "logger = logging.getLogger()\n",
    "\n",
    "\n",
    "def lambda_handler(event, context):\n",
    "    logger.warning('system-level-marker')\n",
    "    return {'ok': True}\n",
);

/// The marker that handler's record carries, which is how the application record is found among
/// the delivered ones.
const MARKER: &str = "system-level-marker";

/// The level the rows filter the handler's own records at: one at which the record above is sent
/// whatever the system level is, so the two filters are seen to be independent.
const APPLICATION_LOG_LEVEL: &str = "WARN";

/// One row: the `SystemLogLevel` the function's `LoggingConfig` names, or `None` for a function
/// that names none.
struct Row {
    case: &'static str,
    selected: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "the most detailed level the model declares, which withholds nothing",
        selected: Some("DEBUG"),
    },
    Row {
        case: "the middle level, at which the envelope of an invocation is stated",
        selected: Some("INFO"),
    },
    Row {
        case: "the least detailed level, below every record a successful run leaves",
        selected: Some("WARN"),
    },
    Row {
        case: "a function naming no system level, which is filtered at the documented default",
        selected: None,
    },
];

/// How long to wait between looks at the group, and how many looks to take. The delivery of a
/// run's log is not part of the invocation's answer, so every read of the group waits for the
/// CONDITION; the budget is far longer than any span this emulator chooses, so a loaded host is
/// slower without the rule being any different and nothing here asserts on elapsed time.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 600;

/// The records the group `group` holds once the delivery of the run's log has reached it, which
/// is once the handler's own record — the one record every row's levels admit — is there. One
/// delivery writes a run's log in one go, so a group holding any record of it holds every record
/// the levels admitted.
async fn delivered_records(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<serde_json::Map<String, Value>> {
    let mut held: Vec<String> = Vec::new();
    for _ in 0..POLLS {
        held = searched(router, case, group).await;
        if held.iter().any(|message| message.contains(MARKER)) {
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert!(
        held.iter().any(|message| message.contains(MARKER)),
        "case {case:?}: expected the group {group:?} to hold the record the handler wrote, got \
         {held:?}"
    );
    held.iter()
        .filter_map(|message| record(message.trim_end()))
        .collect()
}

/// The kinds of platform record `records` holds, in the order they were delivered.
fn platform_kinds(records: &[serde_json::Map<String, Value>]) -> Vec<String> {
    records
        .iter()
        .filter_map(|members| {
            members
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// RULE: Lambda sends a platform record of a run to CloudWatch Logs only where the function's
/// `SystemLogLevel` is at least as detailed as the level that record's own kind is stated at,
/// while the invocation's log tail still reports every record the run wrote and the handler's own
/// records travel by the other level alone.
#[tokio::test]
async fn each_platform_record_is_sent_only_at_its_own_level_of_detail_and_lower() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = emulator();
        let name = format!("system-level-{index}");
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
                source: PYTHON_HANDLER,
                runtime: PYTHON_RUNTIME,
                role: &role,
                log_group: None,
            },
            Some(&logging),
        )
        .await;

        // ONE cold invocation, which writes every kind of platform record: the environment it
        // built and the run it served in it.
        let (tail, _payload) = invoke(&router, case, &name).await;

        // The run WROTE every one of them, whatever the level withholds from the group: the
        // filter is on what Lambda sends. The record that opens the ENVIRONMENT's log is the
        // environment's own and no part of what an invocation reports as its log
        // ([`crate::invocation_log_phases_and_level_filters`] owns that), so the tail states
        // every other kind.
        let written: Vec<String> = platform_kinds(
            &tail
                .lines()
                .filter_map(record)
                .collect::<Vec<serde_json::Map<String, Value>>>(),
        );
        for (kind, _) in RECORD_LEVELS {
            if kind == ENVIRONMENT_RECORD {
                continue;
            }
            assert!(
                written.iter().any(|named| named == kind),
                "case {case:?}: expected the run's own log to carry {kind:?}, got {written:?} \
                 out of {tail:?}"
            );
        }

        // And the group holds exactly the kinds the level in force admits, in the order the
        // platform writes them.
        let held = delivered_records(&router, case, &group).await;
        let kinds = platform_kinds(&held);
        let expected = admitted_kinds(in_force(row.selected));
        assert_eq!(
            kinds, expected,
            "case {case:?}: expected the platform records {expected:?} at the level {:?}, got \
             {kinds:?} out of {held:?}",
            in_force(row.selected)
        );

        // The handler's own record is an APPLICATION log, which this level never withholds: the
        // two filters are independent.
        let application: Vec<&serde_json::Map<String, Value>> = held
            .iter()
            .filter(|members| {
                members
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains(MARKER))
            })
            .collect();
        assert_eq!(
            application.len(),
            1,
            "case {case:?}: expected the one record the handler wrote to be delivered whatever \
             the system level withholds, got {application:?} out of {held:?}"
        );
    }
}
