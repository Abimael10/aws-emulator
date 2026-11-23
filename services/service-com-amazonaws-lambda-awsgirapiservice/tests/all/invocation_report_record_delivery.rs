// SPDX-License-Identifier: CC0-1.0

//! WHICH level the platform records that close an invocation are stated at, and so which of
//! them a function's `LoggingConfig$SystemLogLevel` sends to its log group.
//!
//! RULE: `SystemLogLevel` is "set this property to filter the system logs for your function
//! that Lambda sends to CloudWatch. Lambda only sends system logs at the selected level of
//! detail and lower", and each platform record is judged at the level of what IT reports: the
//! records that close a run whose invocation did NOT succeed — the runtime reporting its work
//! on the invocation done, and the report of the run's duration, what it is billed for, the
//! memory it held and the outcome the platform reports for it — are a WARNING about that run,
//! so a function filtering at the least detailed level still receives them, while the records
//! of an ordinary run (the frames that open a phase, and the same two records of a run that
//! answered) are stated at the level of their kind and withheld below it.
//!
//! The rows differ along what the rule generalises over: the level the function selects (every
//! value the model's `SystemLogLevel` enum declares, at and around the level the records of an
//! ordinary run are stated at) and what the run CAME TO (a handler that answered, one that
//! raised, and one the sandbox stopped at the function's own `Timeout`) — which is what decides
//! whether a record of that run is a warning about it.
//!
//! Every row drives real HTTP through the production `MultiServiceRouter` with Lambda,
//! CloudWatch Logs and IAM mounted on ONE engine — the harness
//! [`crate::invocation_log_delivery`] pins the delivery itself with, reused rather than copied
//! — and reads the delivery back through CloudWatch Logs' own group-wide search
//! (`FilterLogEvents`), so nothing that did not reach that store can pass.
//!
//! This test RUNS handlers, so it takes the target's one invocation stream
//! ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]).

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mapping_role_access::{allow, given_role};
use crate::event_source_mappings::{await_function_active, call};
use crate::invocation_log_delivery::{
    create_function_logging, emulator, invocation_request_carrying, searched, Created, LOGS,
    LOG_WRITES, PYTHON_RUNTIME,
};

/// Where `UpdateFunctionConfiguration` is bound, under the collection `CreateFunction` is.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// Every level of detail a function may filter its system records at, from the most detailed to
/// the least ("`DEBUG` is the highest level and `WARN` is the lowest"). Source: the values the
/// model's `SystemLogLevel` enum declares (`DEBUG | INFO | WARN`).
const SYSTEM_LOG_LEVELS: [&str; 3] = ["DEBUG", "INFO", "WARN"];

/// The level each KIND of platform record of an ordinary run is stated at: the two frames that
/// state an invocation happened and what it spent are the envelope every invocation is reported
/// by, while the records of one phase of the execution environment and the record that closes
/// the runtime's work on the invocation are the DETAIL of how the run was served, which only the
/// most detailed level admits.
///
/// Source: the levels AWS states for the system records of a run
/// (<https://docs.aws.amazon.com/lambda/latest/dg/monitoring-cloudwatchlogs-log-level.html>),
/// against the records one run of this function leaves.
const RECORD_LEVELS: [(&str, &str); 6] = [
    ("platform.initStart", "DEBUG"),
    ("platform.initRuntimeDone", "DEBUG"),
    ("platform.initReport", "DEBUG"),
    ("platform.start", "INFO"),
    ("platform.runtimeDone", "DEBUG"),
    ("platform.report", "INFO"),
];

/// The level a record of a run that did NOT succeed is stated at: such a record is a warning
/// about that run, which even the least detailed level admits.
const FAILED_RUN_LEVEL: &str = "WARN";

/// The level the record `kind` of a run that came to `succeeded` is stated at.
fn stated_level(kind: &str, succeeded: bool) -> &'static str {
    if !succeeded {
        return FAILED_RUN_LEVEL;
    }
    RECORD_LEVELS
        .iter()
        .find(|(named, _)| *named == kind)
        .map(|(_, level)| *level)
        .unwrap_or_else(|| panic!("the level of {kind:?} is one of the table's"))
}

/// Whether a function filtering its system records at `filter` is sent a record stated at
/// `stated`: "Lambda only sends system logs at the selected level of detail and lower", so a
/// filter at least as detailed as the record admits it.
fn admits(filter: &str, stated: &str) -> bool {
    let placed = |named: &str| SYSTEM_LOG_LEVELS.iter().position(|value| *value == named);
    placed(filter) <= placed(stated)
}

/// The records that CLOSE an invocation, in the order the platform writes them: the runtime
/// reporting its work on the invocation done, and the report of what the run spent. Each states
/// the invocation it belongs to and the status the platform reports for it.
const CLOSING_RECORDS: [&str; 2] = ["platform.runtimeDone", "platform.report"];

/// The platform records that OPEN a phase, and the records of the INITIALIZATION of the
/// execution environment: records about an ordinary run whatever that run came to.
const OPENING_FRAMES: [&str; 4] = [
    "platform.initStart",
    "platform.initRuntimeDone",
    "platform.initReport",
    "platform.start",
];

/// The `Timeout` the row whose run is stopped at it gives the function, in the seconds the
/// member is stated in: the smallest the model allows, which is the one bound that row tests —
/// every other row runs under the generous bound the harness creates functions with.
const TIGHT_TIMEOUT_SECONDS: i64 = 1;

/// How long the handler waits when the event asks it to, in the seconds its own library states
/// a wait in: far past the bound above, so the run is stopped at that bound rather than at the
/// end of a wait.
const WAIT_SECONDS: i64 = 30;

/// The handler every row runs: it dispatches on the event, so one function answers each outcome
/// a run can come to.
const PYTHON_HANDLER: &str = concat!(
    "import time\n",
    "\n",
    "\n",
    "def lambda_handler(event, context):\n",
    "    action = event.get('action')\n",
    "    if action == 'raise':\n",
    "        raise RuntimeError('the handler could not answer')\n",
    "    if action == 'wait':\n",
    "        time.sleep(event['seconds'])\n",
    "    return {'ok': True}\n",
);

/// One row of the outcome table: the event the invocation carries, the `Timeout` the function is
/// given for it, and the status the platform reports for the run.
struct Outcome {
    case: &'static str,
    event: fn() -> Value,
    /// Whether the function's `Timeout` is cut to [`TIGHT_TIMEOUT_SECONDS`] for this row.
    tight_timeout: bool,
    /// The status the records that close the run state.
    status: &'static str,
    /// Whether that status is the one a run that ANSWERED is reported under — the one outcome
    /// whose closing records are records of an ordinary run rather than warnings about it.
    succeeded: bool,
}

/// The rows, in the order they are driven: the one that cuts the function's `Timeout` comes
/// last, so no other row is judged against a bound it does not test.
fn outcome_rows() -> Vec<Outcome> {
    vec![
        Outcome {
            case: "a handler that answered",
            event: || json!({ "action": "return" }),
            tight_timeout: false,
            status: "success",
            succeeded: true,
        },
        Outcome {
            case: "a handler that raised",
            event: || json!({ "action": "raise" }),
            tight_timeout: false,
            status: "failure",
            succeeded: false,
        },
        Outcome {
            case: "a run the sandbox stopped at the function's own Timeout",
            event: || json!({ "action": "wait", "seconds": WAIT_SECONDS }),
            tight_timeout: true,
            status: "timeout",
            succeeded: false,
        },
    ]
}

/// `message` as a JSON object, or `None` for a record that is not one. A delivered record
/// carries the break that closed the write it was made in, which the parse reads past.
fn record(message: &str) -> Option<Value> {
    match serde_json::from_str::<Value>(message) {
        Ok(value @ Value::Object(_)) => Some(value),
        _ => None,
    }
}

/// The kind a platform record names itself as, or `None` for a record that names none.
fn kind(value: &Value) -> Option<&str> {
    value.get("type").and_then(Value::as_str)
}

/// The records of `records` of the kind `named`.
fn of_kind<'a>(records: &'a [Value], named: &str) -> Vec<&'a Value> {
    records
        .iter()
        .filter(|value| kind(value) == Some(named))
        .collect()
}

/// `Invoke` the function `name` with `event` as its payload, asking for the log tail, and answer
/// the records that tail reports — the whole log the run wrote, which is the independent
/// statement of what the delivery had to judge.
async fn invoked(
    router: &MultiServiceRouter,
    case: &str,
    name: &str,
    event: &Value,
) -> (u16, Vec<Value>) {
    let request = invocation_request_carrying(
        name,
        &[("x-amz-log-type", "Tail")],
        event.to_string().as_bytes(),
    );
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    let status = parts.status.as_u16();
    let encoded = parts
        .headers
        .get("x-amz-log-result")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a log tail on the answer, got {} {}",
                status,
                String::from_utf8_lossy(&body)
            )
        })
        .to_string();
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(&encoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the log tail is base64: {error}"));
    let tail = String::from_utf8(decoded)
        .unwrap_or_else(|error| panic!("case {case:?}: the log tail is text: {error}"));
    (status, tail.lines().filter_map(record).collect())
}

/// Cut the `Timeout` of the function `name` to `seconds` and wait until the update has settled,
/// which is how the row that tests that bound gets one of its own.
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

/// RULE: the records that close an invocation whose run did not succeed are warnings about that
/// run, so they reach the function's log group at every level of detail `SystemLogLevel`
/// declares, while the records of an ordinary run — the frames that open a phase, and the
/// closing records of a run that answered — are sent only at the level they are stated at and
/// lower.
#[tokio::test]
async fn the_records_that_close_a_failed_run_are_sent_at_every_system_log_level() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    for (index, level) in SYSTEM_LOG_LEVELS.iter().enumerate() {
        let router = emulator();
        let name = format!("report-record-{index}");
        let group = format!("/aws/lambda/{name}");
        let configured = format!("a function filtering its system records at {level}");
        // A role granted every log write, so what reaches CloudWatch Logs is the level's doing
        // and never the role's.
        let role = given_role(
            &router,
            &configured,
            &name,
            Some(json!([allow(LOGS, &LOG_WRITES, "*")])),
        )
        .await;
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
            Some(&json!({ "LogFormat": "JSON", "SystemLogLevel": level })),
        )
        .await;
        for row in outcome_rows() {
            let case = format!("{configured}: {}", row.case);
            if row.tight_timeout {
                given_timeout(&router, &case, &name, TIGHT_TIMEOUT_SECONDS).await;
            }
            let (status, written) = invoked(&router, &case, &name, &(row.event)()).await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from Invoke, got {status} with {written:?}"
            );
            // Whatever the level withholds from the group, the run's own log tail reports the
            // whole log it wrote: the filter is on what Lambda SENDS.
            let held = searched(&router, &case, &group).await;
            let delivered: Vec<Value> = held.iter().filter_map(|line| record(line)).collect();
            for closing in CLOSING_RECORDS {
                // The closing records of a run that answered are stated at the level of their
                // own kind; those of a run that did not succeed are warnings about it, which
                // every level admits.
                let sent = admits(level, stated_level(closing, row.succeeded));
                let wrote = of_kind(&written, closing);
                assert_eq!(
                    wrote.len(),
                    1,
                    "case {case:?}: expected the run's own log to carry one {closing}, got \
                     {written:?}"
                );
                let reported = wrote[0].clone();
                assert_eq!(
                    reported["record"]["status"].as_str(),
                    Some(row.status),
                    "case {case:?}: expected {closing} to state the status {:?}, got {reported}",
                    row.status
                );
                let request_id = reported["record"]["requestId"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string();
                assert!(
                    crate::invoke_request_id::is_request_id(&request_id),
                    "case {case:?}: expected {closing} to name the invocation, got {reported}"
                );
                let found: Vec<&Value> = of_kind(&delivered, closing)
                    .into_iter()
                    .filter(|value| value["record"]["requestId"].as_str() == Some(&request_id))
                    .collect();
                assert_eq!(
                    found.len(),
                    usize::from(sent),
                    "case {case:?}: expected the group {group:?} to hold {} {closing} for \
                     {request_id:?}, got {delivered:?}",
                    usize::from(sent)
                );
                // What is delivered is the record the run wrote, not a second rendering of it.
                if let Some(delivered_record) = found.first() {
                    assert_eq!(
                        *delivered_record, &reported,
                        "case {case:?}: expected the delivered {closing} to be the record the \
                         run wrote ({reported}), got {delivered_record}"
                    );
                }
            }

            // The frames that OPEN a phase report an ordinary run whatever that run came to, so
            // each follows the level IT is stated at alone: the environment the rows run in was
            // built by the first of them, so every frame this level admits is in the group.
            let frames: Vec<&str> = OPENING_FRAMES
                .iter()
                .filter(|named| !of_kind(&delivered, named).is_empty())
                .copied()
                .collect();
            let expected: Vec<&str> = OPENING_FRAMES
                .iter()
                .filter(|named| admits(level, stated_level(named, true)))
                .copied()
                .collect();
            assert_eq!(
                frames, expected,
                "case {case:?}: expected the frames {expected:?} to be delivered, got \
                 {frames:?} of {delivered:?}"
            );
        }
    }
}
