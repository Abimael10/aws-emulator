// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH logging configuration decides what an execution delivers
//! to CloudWatch Logs: the one its state machine HOLDS when the run happens, which every
//! accepted `UpdateStateMachine` replaces.
//!
//! The rule under test: a run's history events are delivered to the log group the
//! configuration the state machine holds names, with the event types that configuration's
//! `level` selects and the execution payload only where its `includeExecutionData` asks
//! for it — so an accepted write of a new `loggingConfiguration` governs every run after
//! it, a level that selects none of a run's events delivers nothing for that run and
//! leaves it no stream, and a level that selects some of them resumes delivery into the
//! same group.
//!
//! Each phase below writes a configuration, runs ONE execution under it and reads the
//! group back through CloudWatch Logs' own readers. The phases differ along what the rule
//! generalises over: which call installed the configuration (`CreateStateMachine` for the
//! first, `UpdateStateMachine` for the rest), the `level` (`ALL`, `ERROR`, `FATAL` and
//! `OFF`, the whole of `LogLevel`), whether the configuration names a destination at all,
//! `includeExecutionData` true and false, and the outcome the run reaches (failed and
//! succeeded) — so the same group is watched while the configuration over it narrows from
//! every event to none and widens back.
//!
//! A phase that must deliver NOTHING is graded after a later phase's delivery has landed
//! in the same group: a delivery that was owed would have been made by then, so the
//! absence is observed rather than waited for.
//!
//! The harness (`Observed`, `create`, `logging`) is the one `execution_observability`
//! builds for Step Functions and CloudWatch Logs over one engine, and the group reader is
//! `vended_log_event_members`'; `tests/all/main.rs` is this binary's crate root, so both
//! are reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    observability, EXECUTION_SETTLE_DELAY, LOG_LEVEL_OFF,
};

use crate::execution_observability::{create, logging, Observed};
use crate::state_machine_lifecycle::{string, HELLO_DEFINITION};
use crate::vended_log_event_members::{delivered, holds_group, PAYLOAD_MEMBERS};

/// A definition whose run produces one state's entry and exit and then fails, so one
/// execution carries an execution-level event, a pair of ordinary progress events and
/// both failure events — which is what makes the three logging levels differ over it.
const PASS_THEN_FAIL: &str = r#"{"StartAt":"Note","States":{
    "Note":{"Type":"Pass","Result":{"noted":true},"Next":"Nope"},
    "Nope":{"Type":"Fail","Error":"Boom","Cause":"the phase fails on purpose"}}}"#;

/// How long a wait below gives an outcome, or a delivery, the clock owes.
///
/// The outcome is due one [`EXECUTION_SETTLE_DELAY`] after the start and the clock that
/// materialises it wakes on its own schedule, so the wait is that span with ample room
/// around it: what each wait below looks for is the CONDITION, and the bound only makes a
/// failing phase report rather than hang.
const DEADLINE: Duration = EXECUTION_SETTLE_DELAY.saturating_add(Duration::from_secs(30));

/// How long a wait below leaves between two looks at the store.
const POLL: Duration = Duration::from_millis(25);

/// The status a run of [`PASS_THEN_FAIL`] reaches.
const FAILED: &str = "FAILED";

/// The status a run of `HELLO_DEFINITION` reaches.
const SUCCEEDED: &str = "SUCCEEDED";

// The event type names below are members of `HistoryEventType`
// (`smithy_query(query="shape", shape="HistoryEventType")`), and which level delivers
// which of them is `LogLevel`'s own documentation: ALL logs all events, ERROR logs the
// events of the ERROR and FATAL classes, FATAL logs the FATAL class — the end of the
// execution itself — and OFF logs none.

/// Every event a run of [`PASS_THEN_FAIL`] produces, in its history order: what `ALL`
/// delivers.
const EVERY_FAILING_EVENT: &[&str] = &[
    "ExecutionStarted",
    "PassStateEntered",
    "PassStateExited",
    "FailStateEntered",
    "ExecutionFailed",
];

/// The error-class events of that run: entering the `Fail` state, which is the definition
/// declaring the run failed, and the execution's own failure.
const FAILING_ERROR_EVENTS: &[&str] = &["FailStateEntered", "ExecutionFailed"];

/// The fatal-class event of that run: the end of the execution itself.
const FAILING_FATAL_EVENTS: &[&str] = &["ExecutionFailed"];

/// Every event a run of `HELLO_DEFINITION` produces, in its history order.
const EVERY_SUCCEEDING_EVENT: &[&str] = &[
    "ExecutionStarted",
    "PassStateEntered",
    "PassStateExited",
    "ExecutionSucceeded",
];

/// What a phase's `loggingConfiguration` asks for.
#[derive(Clone, Copy)]
enum Configured {
    /// The one group this suite watches, at this `level`, with `includeExecutionData` as
    /// given.
    Group(&'static str, bool),
    /// `OFF` with no destination at all, which the shape admits: `destinations` is
    /// "Required, if your log level is not set to OFF"
    /// (`smithy_query(query="shape", shape="LoggingConfiguration")`).
    Off,
}

impl Configured {
    /// The `loggingConfiguration` member a write carries for it.
    fn value(self, group: &str) -> Value {
        match self {
            Self::Group(level, include_execution_data) => {
                logging(level, group, include_execution_data)
            }
            Self::Off => json!({ "level": LOG_LEVEL_OFF }),
        }
    }

    /// The `level` it names.
    fn level(self) -> &'static str {
        match self {
            Self::Group(level, _) => level,
            Self::Off => LOG_LEVEL_OFF,
        }
    }

    /// Whether it asks for the execution data.
    fn includes_execution_data(self) -> bool {
        matches!(self, Self::Group(_, true))
    }
}

/// One phase of the table: the configuration in force, the definition the run under it
/// interprets, and what the group must then hold for that run.
struct Phase {
    /// What the phase is about, named in every assertion it makes.
    case: &'static str,
    /// The configuration the phase writes before its run.
    configured: Configured,
    /// The definition the run interprets.
    definition: &'static str,
    /// The terminal status that run reaches.
    status: &'static str,
    /// The event types the group must hold for that run, in the run's history order —
    /// empty for a configuration that delivers nothing of it.
    expected: &'static [&'static str],
    /// Whether the delivered events must carry the document the run was started with,
    /// which only a delivered `ExecutionStarted` carries and only where the configuration
    /// asks for the execution data.
    carries_input: bool,
}

/// The record `execution` reports once it has left `RUNNING`, looked for again until it
/// has or [`DEADLINE`] passes.
async fn settled(observed: &Observed, case: &str, execution: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = observed
            .ok("DescribeExecution", &json!({ "executionArn": execution }))
            .await;
        if string(&described, "status") != "RUNNING" {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {execution} to reach a terminal status within \
             {DEADLINE:?}, got {described:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The events the group `group` holds for `execution`, looked for again until it holds at
/// least `wanted` of them or [`DEADLINE`] passes.
///
/// The count is a FLOOR rather than the grade: a group holding more than the level selects
/// is reported by the comparison the caller makes, not hidden by this wait.
///
/// A group the FIRST delivery into it has not created yet holds none of them, and CloudWatch
/// Logs refuses a read of a group it does not hold, so the group's existence is what this
/// looks for first: the wait is on the CONDITION either way, and the delivery owed by the
/// clock creates the group as it lands.
async fn delivered_for(
    observed: &Observed,
    case: &str,
    group: &str,
    execution: &str,
    wanted: usize,
) -> Vec<Value> {
    let deadline = Instant::now() + DEADLINE;
    loop {
        // The group is CREATED by the first delivery into it, so a look taken before that
        // delivery has landed finds no group at all — which a read of a log group answers
        // as a missing resource rather than as an empty group. What this wait is for is the
        // DELIVERY, so it starts at the group coming into existence.
        let messages: Vec<Value> = if holds_group(observed, group).await {
            delivered(observed, case, group)
                .await
                .into_iter()
                .filter(|message| {
                    message.get("execution_arn").and_then(Value::as_str) == Some(execution)
                })
                .collect()
        } else {
            Vec::new()
        };
        if messages.len() >= wanted {
            return messages;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {wanted} delivered event(s) of {execution} in {group} \
             within {DEADLINE:?}, got {messages:?}"
        );
        ::tokio::time::sleep(POLL).await;
    }
}

/// The names of the streams the group `group` holds.
async fn streams_of(observed: &Observed, group: &str) -> Vec<String> {
    let described = observed
        .logs("DescribeLogStreams", &json!({ "logGroupName": group }))
        .await;
    described
        .get("logStreams")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .filter_map(|stream| stream.get("logStreamName").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// The ids the history of `execution` gives the events of the types `selected`, as the
/// text a delivered event carries its id as.
async fn history_ids(observed: &Observed, execution: &str, selected: &[&str]) -> (Vec<String>, usize) {
    let history = observed
        .ok("GetExecutionHistory", &json!({ "executionArn": execution }))
        .await;
    let events = history
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let ids = events
        .iter()
        .filter(|event| {
            let event_type = event.get("type").and_then(Value::as_str).unwrap_or_default();
            selected.contains(&event_type)
        })
        .filter_map(|event| event.get("id").and_then(Value::as_i64))
        .map(|id| id.to_string())
        .collect();
    (ids, events.len())
}

/// The configuration a state machine HOLDS decides what its runs deliver: each accepted
/// write replaces it, and the run after that write is delivered — or not delivered — by
/// the configuration that write left.
#[tokio::test]
async fn the_configuration_in_force_decides_what_a_run_delivers() {
    let observed = Observed::new();
    let group = "/aws/vendedlogs/states/in-force";
    let machine_name = "in-force";

    let phases: &[Phase] = &[
        Phase {
            case: "the level ALL the create installed logs every event of the run",
            configured: Configured::Group(observability::LOG_LEVEL_ALL, true),
            definition: PASS_THEN_FAIL,
            status: FAILED,
            expected: EVERY_FAILING_EVENT,
            carries_input: true,
        },
        Phase {
            case: "an update to OFF, which names no destination, blocks the delivery",
            configured: Configured::Off,
            definition: PASS_THEN_FAIL,
            status: FAILED,
            expected: &[],
            carries_input: false,
        },
        Phase {
            case: "an update back to ERROR delivers the error class into the same group",
            configured: Configured::Group(observability::LOG_LEVEL_ERROR, true),
            definition: PASS_THEN_FAIL,
            status: FAILED,
            expected: FAILING_ERROR_EVENTS,
            carries_input: false,
        },
        Phase {
            case: "an update to FATAL narrows it to the end of the execution itself",
            configured: Configured::Group(observability::LOG_LEVEL_FATAL, true),
            definition: PASS_THEN_FAIL,
            status: FAILED,
            expected: FAILING_FATAL_EVENTS,
            carries_input: false,
        },
        Phase {
            case: "a run under FATAL that succeeds has no event of that class to deliver",
            configured: Configured::Group(observability::LOG_LEVEL_FATAL, true),
            definition: HELLO_DEFINITION,
            status: SUCCEEDED,
            expected: &[],
            carries_input: false,
        },
        Phase {
            case: "an update back to ALL resumes every event, with the payload left out",
            configured: Configured::Group(observability::LOG_LEVEL_ALL, false),
            definition: HELLO_DEFINITION,
            status: SUCCEEDED,
            expected: EVERY_SUCCEEDING_EVENT,
            carries_input: false,
        },
    ];

    let mut machine = String::new();
    // The runs whose configuration delivers nothing, graded once a later phase's delivery
    // has landed in the same group.
    let mut silent: Vec<(&str, String, String)> = Vec::new();

    for (index, phase) in phases.iter().enumerate() {
        let case = phase.case;
        let configuration = phase.configured.value(group);
        if index == 0 {
            machine = create(
                &observed,
                machine_name,
                phase.definition,
                "STANDARD",
                Some(configuration.clone()),
            )
            .await;
        } else {
            observed
                .ok(
                    "UpdateStateMachine",
                    &json!({
                        "stateMachineArn": machine,
                        "definition": phase.definition,
                        "loggingConfiguration": configuration,
                    }),
                )
                .await;
        }

        // The configuration the write installed is the one the record now holds, so the
        // run below is logged by a configuration this service really stored.
        let described = observed
            .ok(
                "DescribeStateMachine",
                &json!({ "stateMachineArn": machine }),
            )
            .await;
        let reported = described
            .get("loggingConfiguration")
            .cloned()
            .unwrap_or_default();
        assert_eq!(
            reported.get("level").and_then(Value::as_str),
            Some(phase.configured.level()),
            "case {case:?}: expected the record to hold the level the write installed, got \
             {reported:?}"
        );

        let marker = format!("marker-{index}");
        let run = format!("run-{index}");
        let started = observed
            .ok(
                "StartExecution",
                &json!({
                    "stateMachineArn": machine,
                    "name": run.clone(),
                    "input": format!("{{\"marker\":\"{marker}\"}}"),
                }),
            )
            .await;
        let execution = string(&started, "executionArn");
        let settled = settled(&observed, case, &execution).await;
        assert_eq!(
            string(&settled, "status"),
            phase.status,
            "case {case:?}: expected the run to reach {}, got {settled:?}",
            phase.status
        );

        let stream = format!("states/{machine_name}/{run}");
        if phase.expected.is_empty() {
            silent.push((case, execution, stream));
            continue;
        }

        let messages =
            delivered_for(&observed, case, group, &execution, phase.expected.len()).await;
        let types: Vec<&str> = messages
            .iter()
            .filter_map(|message| message.get("type").and_then(Value::as_str))
            .collect();
        assert_eq!(
            types, phase.expected,
            "case {case:?}: expected the group to hold exactly {:?} for this run, got \
             {messages:?}",
            phase.expected
        );

        // Each delivered event is the history event of the same id, so a consumer can join
        // the group to the history event for event.
        let (wanted_ids, history_length) =
            history_ids(&observed, &execution, phase.expected).await;
        let ids: Vec<String> = messages
            .iter()
            .filter_map(|message| message.get("id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        assert_eq!(
            ids, wanted_ids,
            "case {case:?}: expected the ids of the history events of those types, got \
             {messages:?}"
        );
        // A level that selects every event delivers the whole contiguous sequence of the
        // run's history, with no gap and no duplicate.
        if phase.expected.len() == history_length {
            let whole: Vec<String> = (1..=history_length).map(|id| id.to_string()).collect();
            assert_eq!(
                ids, whole,
                "case {case:?}: expected the whole history 1..{history_length} to be \
                 delivered, got {ids:?}"
            );
        }

        let carries_input = messages
            .iter()
            .any(|message| message.to_string().contains(&marker));
        assert_eq!(
            carries_input, phase.carries_input,
            "case {case:?}: expected the run's own input to be delivered={}, got {messages:?}",
            phase.carries_input
        );
        if !phase.configured.includes_execution_data() {
            for message in &messages {
                let details = message.get("details").cloned().unwrap_or_default();
                for member in PAYLOAD_MEMBERS {
                    assert!(
                        details.get(*member).is_none(),
                        "case {case:?}: a configuration that excludes the execution data \
                         delivers no {member:?}, got {message:?}"
                    );
                }
            }
        }

        let streams = streams_of(&observed, group).await;
        assert!(
            streams.contains(&stream),
            "case {case:?}: expected the run's own stream {stream:?} in the group, got \
             {streams:?}"
        );
    }

    // The last phase delivered, so every delivery owed to an earlier phase has been made
    // by now: a run whose configuration delivered nothing has nothing in the group and no
    // stream of its own.
    let held = delivered(&observed, "the whole group", group).await;
    let streams = streams_of(&observed, group).await;
    for (case, execution, stream) in &silent {
        let mine: Vec<&Value> = held
            .iter()
            .filter(|message| {
                message.get("execution_arn").and_then(Value::as_str) == Some(execution.as_str())
            })
            .collect();
        assert!(
            mine.is_empty(),
            "case {case:?}: expected nothing of {execution} in the group, got {mine:?}"
        );
        assert!(
            !streams.contains(stream),
            "case {case:?}: expected no stream {stream:?} for a run the level logs nothing \
             of, got {streams:?}"
        );
    }
}

/// A state machine whose configuration logs nothing delivers nothing at all — the
/// destination group is not even created — until an update names a level that logs, from
/// which point the runs after it are delivered into it.
#[tokio::test]
async fn an_update_turns_a_dormant_destination_on() {
    let observed = Observed::new();
    let group = "/aws/vendedlogs/states/reenabled";
    let machine_name = "reenabled";

    // The configuration names the group but logs nothing of any run, so nothing reaches
    // it: a level is what decides delivery, not the presence of a destination.
    let machine = create(
        &observed,
        machine_name,
        PASS_THEN_FAIL,
        "STANDARD",
        Some(logging(LOG_LEVEL_OFF, group, true)),
    )
    .await;
    let started = observed
        .ok(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": "while-off" }),
        )
        .await;
    let dormant = string(&started, "executionArn");
    settled(&observed, "while logging is off", &dormant).await;

    // Re-enable logging, and run again: the barrier is this run's own delivery, so the
    // absence asserted afterwards is observed rather than waited for.
    observed
        .ok(
            "UpdateStateMachine",
            &json!({
                "stateMachineArn": machine,
                "loggingConfiguration": logging(observability::LOG_LEVEL_ERROR, group, true),
            }),
        )
        .await;
    let started = observed
        .ok(
            "StartExecution",
            &json!({ "stateMachineArn": machine, "name": "once-on" }),
        )
        .await;
    let live = string(&started, "executionArn");
    settled(&observed, "once logging is on", &live).await;
    let messages = delivered_for(
        &observed,
        "once logging is on",
        group,
        &live,
        FAILING_ERROR_EVENTS.len(),
    )
    .await;
    let types: Vec<&str> = messages
        .iter()
        .filter_map(|message| message.get("type").and_then(Value::as_str))
        .collect();
    assert_eq!(
        types, FAILING_ERROR_EVENTS,
        "expected the re-enabled level to deliver the error class, got {messages:?}"
    );

    assert!(
        holds_group(&observed, group).await,
        "expected the first delivery to have created the group it names"
    );
    let streams = streams_of(&observed, group).await;
    assert_eq!(
        streams,
        vec![format!("states/{machine_name}/once-on")],
        "expected the one stream of the one delivered run, got {streams:?}"
    );
    let held = delivered(&observed, "the whole group", group).await;
    let dormant_events: Vec<&Value> = held
        .iter()
        .filter(|message| {
            message.get("execution_arn").and_then(Value::as_str) == Some(dormant.as_str())
        })
        .collect();
    assert!(
        dormant_events.is_empty(),
        "expected the run made while logging was off to stay undelivered, got \
         {dormant_events:?}"
    );
}
