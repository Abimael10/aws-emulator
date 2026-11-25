// SPDX-License-Identifier: CC0-1.0

//! WHICH of the platform's own records one function's Amazon CloudWatch Logs group holds after a
//! SEQUENCE of runs that came to different outcomes, at each level the function filters its
//! system logs at.
//!
//! RULE: `LoggingConfig$SystemLogLevel` is "set this property to filter the system logs for your
//! function that Lambda sends to CloudWatch. Lambda only sends system logs at the selected level
//! of detail and lower, where `DEBUG` is the highest level and `WARN` is the lowest", and every
//! platform record is judged at the level IT states: a record whose status reports that the run
//! it closes did not succeed is a warning about that run, and every other record — the records
//! of the INITIALIZATION of the execution environment among them — is stated at the level of its
//! own kind. So the group of a function holds, run by run, exactly the records the selected level
//! admits and in the order the platform wrote them: at the most detailed level the whole opening
//! sequence of a cold run (the environment's initialization ahead of the invocation's own
//! frames), and at the least detailed one nothing but the records that close a run which did not
//! succeed — the frame that OPENS an invocation states no outcome, so it follows its own kind's
//! level whatever the run came to and is withheld below it however many runs wrote one.
//!
//! The rows differ along what the rule generalises over: the level the function selects (every
//! value the model's `SystemLogLevel` enum declares, plus the function that names none and is
//! filtered at the documented default) and what each run of the sequence came to (a handler that
//! answered, then a run the sandbox stopped at the function's own `Timeout`) — so each kind of
//! record is judged at its own level, above it and below it, and for a run of each outcome.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda, CloudWatch
//! Logs and IAM mounted on ONE engine — the harness [`crate::invocation_log_delivery`] pins the
//! delivery itself with, reused rather than copied — and reads the delivery back through
//! CloudWatch Logs' own group-wide search (`FilterLogEvents`), so nothing that did not reach that
//! store can pass.
//!
//! This test RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::event_source_mappings::{await_function_active, call};
use crate::invocation_log_delivery::{
    create_function_logging, emulator, invocation_request_carrying, searched,
    structured_record as record, Created, LOGS, LOG_WRITES, PYTHON_RUNTIME,
};

/// Where `CreateFunction` is bound, and under it the configuration of one function.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Every level of detail a function may filter its system records at, from the most detailed to
/// the least. Source: the values the model's `SystemLogLevel` enum declares (`DEBUG | INFO |
/// WARN`).
const SYSTEM_LOG_LEVELS: [&str; 3] = ["DEBUG", "INFO", "WARN"];

/// The level a function on the structured format filters its system records at until it names
/// one, which AWS documents for the member ("Default: INFO").
const DEFAULT_SYSTEM_LOG_LEVEL: &str = "INFO";

/// The level the rows filter the HANDLER's own records at: one that admits the record the handler
/// below writes whatever the system level is, so the two filters are seen to be independent.
const APPLICATION_LOG_LEVEL: &str = "INFO";

/// The level a record reporting a run that did NOT succeed is stated at: such a record is a
/// warning about that run, which even the least detailed level admits.
const FAILED_RUN_LEVEL: &str = "WARN";

/// The records of the INITIALIZATION of an execution environment, in the order the platform
/// writes them, with the level each is stated at: the frame that states an environment was built
/// is the envelope of that unit of work, while the records that CLOSE the phase inside it are the
/// detail of how it was served.
///
/// They report the ENVIRONMENT rather than an invocation, so they name no invocation and are
/// stated at their own kind's level whatever the runs served in that environment came to.
///
/// Source: the levels AWS states for the system records of a run
/// (<https://docs.aws.amazon.com/lambda/latest/dg/monitoring-cloudwatchlogs-log-level.html>).
const ENVIRONMENT_RECORDS: [(&str, &str); 3] = [
    ("platform.initStart", "INFO"),
    ("platform.initRuntimeDone", "DEBUG"),
    ("platform.initReport", "DEBUG"),
];

/// The records of ONE invocation, in the order the platform writes them: the kind, the level it
/// is stated at for a run that answered, and whether it states the OUTCOME of that run — which is
/// what makes it a warning about a run that did not succeed.
///
/// Source: the same page as [`ENVIRONMENT_RECORDS`], against the records one run leaves.
const INVOCATION_RECORDS: [(&str, &str, bool); 3] = [
    ("platform.start", "INFO", false),
    ("platform.runtimeDone", "DEBUG", true),
    ("platform.report", "INFO", true),
];

/// How much detail the level `named` admits, as its place on the ladder the levels above are
/// declared in.
fn placed(named: &str) -> usize {
    SYSTEM_LOG_LEVELS
        .iter()
        .position(|value| *value == named)
        .unwrap_or_else(|| panic!("{named:?} is one of the levels the model declares"))
}

/// Whether a function filtering its system records at `filter` is sent a record stated at
/// `stated`: a filter at least as detailed as the record admits it, and every less detailed one
/// withholds it.
fn admits(filter: &str, stated: &str) -> bool {
    placed(filter) <= placed(stated)
}

/// The level a record of the kind whose ordinary level is `level` is stated at for a run that came
/// to `succeeded`: a record that states the run's outcome is a warning about a run that did not
/// succeed, and every other record keeps its own kind's level.
fn stated_level(level: &'static str, states_outcome: bool, succeeded: bool) -> &'static str {
    if states_outcome && !succeeded {
        FAILED_RUN_LEVEL
    } else {
        level
    }
}

/// The records of ONE invocation that came to `succeeded` which a function filtering at `filter`
/// is sent, in the order the platform writes them.
fn invocation_kinds(filter: &str, succeeded: bool) -> Vec<String> {
    INVOCATION_RECORDS
        .iter()
        .filter(|(_, level, states_outcome)| {
            admits(filter, stated_level(level, *states_outcome, succeeded))
        })
        .map(|(kind, _, _)| (*kind).to_string())
        .collect()
}

/// The records of the initialization of an execution environment which a function filtering at
/// `filter` is sent, in the order the platform writes them.
fn environment_kinds(filter: &str) -> Vec<String> {
    ENVIRONMENT_RECORDS
        .iter()
        .filter(|(_, level)| admits(filter, level))
        .map(|(kind, _)| (*kind).to_string())
        .collect()
}

/// The platform records the group of a function filtering at `filter` opens on: those of the
/// environment the first run built, then those of that run, which answered.
fn cold_run_sequence(filter: &str) -> Vec<String> {
    let mut opening = environment_kinds(filter);
    opening.extend(invocation_kinds(filter, true));
    opening
}

/// The handler every row runs: it writes one record of its own through the runtime's logging
/// interface and then dispatches on the event, so one function answers each outcome a run can come
/// to.
const PYTHON_HANDLER: &str = concat!(
    "import logging\n",
    "import time\n",
    "\n",
    "logger = logging.getLogger()\n",
    "\n",
    "\n",
    "def lambda_handler(event, context):\n",
    "    logger.info('run-sequence-marker')\n",
    "    if event.get('action') == 'wait':\n",
    "        time.sleep(event['seconds'])\n",
    "    return {'ok': True}\n",
);

/// The marker that handler's own record carries, which is how an APPLICATION record is told from
/// the platform's own among the delivered ones.
const MARKER: &str = "run-sequence-marker";

/// The `Timeout` the row whose run is stopped at it gives the function, in the seconds the member
/// is stated in: the smallest the model allows, which is the one bound that row tests — the run
/// before it is served under the generous bound the harness creates functions with.
const TIGHT_TIMEOUT_SECONDS: i64 = 1;

/// How long the handler waits when the event asks it to, in the seconds its own library states a
/// wait in: far past the bound above, so the run is stopped at that bound rather than at the end
/// of a wait.
const WAIT_SECONDS: i64 = 30;

/// How long to wait between looks at the group, and how many looks to take. The delivery of a
/// run's log is not part of the invocation's answer, so every read of the group waits for the
/// CONDITION; the budget is far longer than any span this emulator chooses, so a loaded host is
/// slower without the rule being any different and nothing here asserts on elapsed time.
const POLL: Duration = Duration::from_millis(50);
const POLLS: usize = 600;

/// One run of the sequence: the event it carries, the `Timeout` the function is given for it, and
/// what the platform reports the run came to.
struct Run {
    case: &'static str,
    event: fn() -> Value,
    /// Whether the function's `Timeout` is cut to [`TIGHT_TIMEOUT_SECONDS`] for this run.
    tight_timeout: bool,
    /// The status the records that close the run state.
    status: &'static str,
    /// Whether that status is the one a run that ANSWERED is reported under.
    succeeded: bool,
}

/// The sequence every level is driven with, in order: the run that builds the environment answers
/// under the generous bound, and the run stopped at the function's own `Timeout` comes last, so no
/// earlier run is judged against a bound it does not test.
fn run_sequence() -> Vec<Run> {
    vec![
        Run {
            case: "a cold run whose handler answered",
            event: || json!({ "action": "return" }),
            tight_timeout: false,
            status: "success",
            succeeded: true,
        },
        Run {
            case: "a run the sandbox stopped at the function's own Timeout",
            event: || json!({ "action": "wait", "seconds": WAIT_SECONDS }),
            tight_timeout: true,
            status: "timeout",
            succeeded: false,
        },
    ]
}

/// `Invoke` the function `name` with `event`, asking for the log tail, and answer the records that
/// tail reports — the whole log the run wrote, which is the independent statement of what the
/// delivery had to judge.
async fn invoked(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    event: &Value,
) -> Vec<serde_json::Map<String, Value>> {
    let request = invocation_request_carrying(
        name,
        &[("x-amz-log-type", "Tail")],
        event.to_string().as_bytes(),
    );
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let status = parts.status.as_u16();
    assert_eq!(
        status,
        200,
        "case {case:?}: expected HTTP 200 from Invoke, got {status} {}",
        String::from_utf8_lossy(&body)
    );
    let encoded = parts
        .headers
        .get("x-amz-log-result")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| panic!("case {case:?}: expected a log tail on the answer"))
        .to_string();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the log tail is base64: {error}"));
    let tail = String::from_utf8(decoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the log tail is text: {error}"));
    tail.lines().filter_map(record).collect()
}

/// Cut the `Timeout` of the function `name` to `seconds` and wait until the update has settled,
/// which is how the run that tests that bound gets one of its own.
async fn given_timeout(router: &MultiServiceRouter, case: &str, name: &str, seconds: i64) {
    let updated = call(
        router,
        "PUT",
        &format!("{FUNCTIONS_URI}/{name}/configuration"),
        &json!({ "Timeout": seconds }),
    )
    .await;
    assert_eq!(
        updated.status,
        200,
        "case {case:?}: expected the Timeout of {name:?} to be updatable, got {} {}",
        updated.status,
        updated.text()
    );
    await_function_active(router, case, name).await;
}

/// Every message the group `group` holds that is a structured record, in the order they were
/// delivered.
async fn delivered(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
) -> Vec<serde_json::Map<String, Value>> {
    searched(router, case, group)
        .await
        .iter()
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

/// The kinds of platform record `records` holds that name the invocation `request_id`, in the
/// order they were delivered.
fn kinds_of_run(records: &[serde_json::Map<String, Value>], request_id: &str) -> Vec<String> {
    records
        .iter()
        .filter(|members| {
            members
                .get("record")
                .and_then(|about| about.get("requestId"))
                .and_then(Value::as_str)
                == Some(request_id)
        })
        .filter_map(|members| {
            members
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

/// The group's records once the delivery of the run `request_id` has put every record `expected`
/// of it there — the CONDITION each read waits for, rather than a span.
async fn awaiting_run(
    router: &MultiServiceRouter,
    case: &str,
    group: &str,
    request_id: &str,
    expected: &[String],
) -> Vec<serde_json::Map<String, Value>> {
    let mut held = delivered(router, case, group).await;
    for _ in 0..POLLS {
        if kinds_of_run(&held, request_id).len() >= expected.len() {
            break;
        }
        tokio::time::sleep(POLL).await;
        held = delivered(router, case, group).await;
    }
    held
}

/// The invocation a record of the run's own log names, read off the frame that OPENS the run.
fn request_id_of(case: &str, written: &[serde_json::Map<String, Value>]) -> String {
    let opened = written
        .iter()
        .find(|members| members.get("type").and_then(Value::as_str) == Some("platform.start"))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected the run's own log to open on platform.start, got {written:?}")
        });
    let request_id = opened
        .get("record")
        .and_then(|about| about.get("requestId"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    assert!(
        crate::invoke_request_id::is_request_id(&request_id),
        "case {case:?}: expected the opening frame to name the invocation, got {opened:?}"
    );
    request_id
}

/// `kinds` with each kind named once, in order — the SET of record kinds one group holds.
fn distinct(kinds: &[String]) -> Vec<String> {
    let mut seen: Vec<String> = Vec::new();
    for kind in kinds {
        if !seen.contains(kind) {
            seen.push(kind.clone());
        }
    }
    seen.sort();
    seen
}

/// Every system level a row drives: each value the model declares, and the function that names
/// none.
const LEVEL_ROWS: [Option<&str>; 4] = [Some("DEBUG"), Some("INFO"), Some("WARN"), None];

/// RULE: a function's log group holds, run by run, exactly the platform records its
/// `SystemLogLevel` admits — each record at the level IT states, which for a record reporting a
/// run that did not succeed is the warning level and for every other record is its own kind's —
/// in the order the platform wrote them, while every run's own log tail still reports the whole
/// log it wrote and the handler's own records travel by the other level alone.
#[tokio::test]
async fn a_groups_platform_records_are_those_the_system_level_admits_run_by_run() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, selected) in LEVEL_ROWS.iter().enumerate() {
        let level = selected.unwrap_or(DEFAULT_SYSTEM_LOG_LEVEL);
        let router = emulator();
        let name = format!("run-sequence-{index}");
        let group = format!("/aws/lambda/{name}");
        let configured = match selected {
            Some(named) => format!("a function filtering its system records at {named}"),
            None => "a function naming no system level, filtered at the documented default"
                .to_string(),
        };
        // A role granted every log write, so what reaches CloudWatch Logs is the level's doing
        // and never the role's.
        let role = given_role(
            &router,
            &configured,
            &name,
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
        let mut logging = json!({
            "LogFormat": "JSON",
            "ApplicationLogLevel": APPLICATION_LOG_LEVEL,
        });
        if let Some(named) = selected {
            logging["SystemLogLevel"] = json!(named);
        }
        create_function_logging(
            &router,
            &Created {
                case: &configured,
                name: &name,
                source: PYTHON_HANDLER,
                runtime: PYTHON_RUNTIME,
                role: &role,
                log_group: None,
            },
            Some(&logging),
        )
        .await;

        let mut held: Vec<serde_json::Map<String, Value>> = Vec::new();
        // Set once the records the FIRST run left have been judged, which is the only run whose
        // records are the whole of what the group holds.
        let mut opened = false;
        let mut expected_group = environment_kinds(level);
        for run in run_sequence() {
            let case = format!("{configured}: {}", run.case);
            if run.tight_timeout {
                given_timeout(&router, &case, &name, TIGHT_TIMEOUT_SECONDS).await;
            }
            let written = invoked(&router, &case, &name, &(run.event)()).await;
            let request_id = request_id_of(&case, &written);
            // Whatever the level withholds from the group, the run's own log tail reports the
            // whole log it wrote: the filter is on what Lambda SENDS. Each record that states
            // the run's outcome states the one the platform reports for it, which is what
            // decides the level it is stated at.
            let wrote = kinds_of_run(&written, &request_id);
            let every_kind: Vec<String> = INVOCATION_RECORDS
                .iter()
                .map(|(kind, _, _)| (*kind).to_string())
                .collect();
            assert_eq!(
                wrote, every_kind,
                "case {case:?}: expected the run's own log to carry {every_kind:?}, got {wrote:?} \
                 out of {written:?}"
            );
            for (kind, _, states_outcome) in INVOCATION_RECORDS {
                if !states_outcome {
                    continue;
                }
                let reported = written
                    .iter()
                    .find(|members| members.get("type").and_then(Value::as_str) == Some(kind))
                    .cloned()
                    .unwrap_or_default();
                let stated = reported
                    .get("record")
                    .and_then(|about| about.get("status"))
                    .and_then(Value::as_str);
                assert_eq!(
                    stated,
                    Some(run.status),
                    "case {case:?}: expected {kind} to state the status {:?}, got {reported:?}",
                    run.status
                );
            }

            // And the group holds exactly the records of THIS run the level admits, in the order
            // the platform wrote them.
            let sent = invocation_kinds(level, run.succeeded);
            held = awaiting_run(&router, &case, &group, &request_id, &sent).await;
            let delivered_kinds = kinds_of_run(&held, &request_id);
            assert_eq!(
                delivered_kinds, sent,
                "case {case:?}: expected the group {group:?} to hold {sent:?} for the run \
                 {request_id:?}, got {delivered_kinds:?} out of {held:?}"
            );
            expected_group.extend(sent);
            // The records the FIRST run left open the group: the environment it built and then
            // its own frames, in that order.
            if !opened {
                let kinds = platform_kinds(&held);
                let expected = cold_run_sequence(level);
                assert_eq!(
                    kinds, expected,
                    "case {case:?}: expected the group to open on {expected:?}, got {kinds:?} out \
                     of {held:?}"
                );
                opened = true;
            }
        }

        // Across the whole sequence the group holds exactly the KINDS the level admits and no
        // other: a record stated above the selected level is absent however many runs wrote one.
        let kinds = distinct(&platform_kinds(&held));
        let expected = distinct(&expected_group);
        assert_eq!(
            kinds, expected,
            "case {configured:?}: expected the group {group:?} to hold the platform records \
             {expected:?} after the sequence, got {kinds:?} out of {held:?}"
        );

        // The handler's own record is an APPLICATION log, which the system level never withholds:
        // the two filters are independent.
        let application = held
            .iter()
            .filter(|members| {
                members
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some_and(|message| message.contains(MARKER))
            })
            .count();
        assert!(
            application >= 1,
            "case {configured:?}: expected the record the handler wrote to be delivered whatever \
             the system level withholds, got {held:?}"
        );
    }
}
