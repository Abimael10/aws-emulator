// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the EVENT ID a failure diagnosis names when the attempt that
//! failed CONTINUES an execution rather than beginning one, driven over this service's
//! own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: every diagnosis the interpreter raises about a state's own
//! expression is reported under "An error occurred while executing the state '<name>'
//! (entered at the event id #<id>). <diagnosis>", and `<id>` is the event the
//! execution's history HOLDS that state's entry at. A call that continues an execution —
//! a redrive of the unsuccessful step, a worker's reply to the activity task the
//! execution was suspended at — begins at a state the attempt it continues already
//! entered, and "Step Functions preserves the results and execution history of the
//! successful steps": that entry is recorded ONCE, so the continuation's own diagnoses
//! name the id it carries there. A state the continuation itself enters records an entry
//! of its own, and names THAT event.
//!
//! The rows differ along what the rule generalises over: which MEMBER of the state could
//! not be resolved (a payload template's field, an `OutputPath`, a `Choice` rule's
//! `Variable`, a `Wait` state's `SecondsPath`) and so which diagnosis follows the
//! prefix, the failing state's TYPE, its POSITION in the definition — which is what makes
//! the preserved event id differ from row to row — how many times the execution has been
//! continued (a first and a second redrive), which CONTINUATION re-enters the state (a
//! redrive, which re-runs it, and a worker's reply, which places its result there), and
//! whether the failing state is the one the continuation begins at or one it reaches
//! itself (a state after the reply, and a state inside a re-entered `Map`).
//!
//! No event id is written into a row: each one is read out of `GetExecutionHistory` and
//! the cause is required to name THAT id, so the two readings of one execution cannot
//! drift.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`) is the one `state_machine_lifecycle` builds, the history reader is
//! `execution_history`'s, the activity helpers are `activity_tasks`' and the diagnosis
//! wording is `unresolved_path_cause`'s; `tests/all.rs` is this binary's crate root, so
//! each is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::RUNTIME_ERROR;
use service_com_amazonaws_sfn_awsstepfunctions::state::STATE_ENTERED_SUFFIX;

use crate::activity_tasks::{claimed, create_activity};
use crate::execution_history::history;
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator,
};
use crate::unresolved_path_cause::{expected_cause, Diagnosis};

/// The name every row's failing top-level state carries.
const FAILING: &str = "JobComplete";

/// Generous room for an outcome the clock owes: far longer than the delay itself, so a
/// loaded host still settles inside it. The loop below waits for the CONDITION, not for
/// this.
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

/// How long the poll below pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(20);

/// Describe `execution_arn` until it is no longer `RUNNING`, and report what it then
/// says.
async fn settled(emulator: &Emulator, case: &str, execution_arn: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = describe_execution(emulator, execution_arn).await;
        if string(&described, "status") != "RUNNING" {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected {execution_arn} to settle, got {described}"
        );
        ::tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The ids of every event this history holds the ENTRY of the state `name` at, in
/// history order: one per entry, whatever type of state it is.
fn entries_of(events: &[Value], name: &str) -> Vec<i64> {
    events
        .iter()
        .filter(|event| {
            event
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind.ends_with(STATE_ENTERED_SUFFIX))
                && event
                    .get("stateEnteredEventDetails")
                    .and_then(|details| details.get("name"))
                    .and_then(Value::as_str)
                    == Some(name)
        })
        .map(|event| {
            event
                .get("id")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| panic!("expected an id on {event:?}"))
        })
        .collect()
}

/// The `cause` of the event the history ENDS on, which is the one the attempt just
/// finished wrote.
fn last_cause(events: &[Value], case: &str) -> String {
    let last = events
        .last()
        .unwrap_or_else(|| panic!("case {case:?}: expected a history, got none"));
    last.get("executionFailedEventDetails")
        .and_then(|details| details.get("cause"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a failed execution, got {last}"))
        .to_string()
}

/// The ids a history holds, which number its events from one in order: an attempt
/// appends its events to the history, so a continuation neither re-uses an id nor leaves
/// a gap.
fn assert_numbered(events: &[Value], case: &str) {
    let ids: Vec<i64> = events
        .iter()
        .map(|event| {
            event
                .get("id")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| panic!("case {case:?}: expected an id on {event:?}"))
        })
        .collect();
    let expected: Vec<i64> = (1..=i64::try_from(events.len()).expect("the count fits")).collect();
    assert_eq!(
        ids, expected,
        "case {case:?}: expected the history to be numbered from one in order, got {ids:?}"
    );
}

/// Redrive `execution_arn`, which must succeed, and wait out the outcome the re-run owes.
async fn redriven(emulator: &Emulator, case: &str, execution_arn: &str) -> Value {
    emulator
        .ok("RedriveExecution", &json!({ "executionArn": execution_arn }))
        .await;
    settled(emulator, case, execution_arn).await
}

// ---------------------------------------------------------------------------
// A redrive: the step it continues from was entered by the attempt it continues
// ---------------------------------------------------------------------------

/// One row: the failing state, where it sits in the definition, and the diagnosis its
/// unresolvable member is reported with.
struct Row {
    case: &'static str,
    /// The failing state itself, which every row reaches as its last state.
    failing: Value,
    /// How many `Pass` states the execution walks before the failing one, so the event
    /// id the cause names differs from row to row.
    before: usize,
    /// The execution input, which carries none of the paths below.
    input: &'static str,
    /// The path the cause must name.
    path: &'static str,
    /// Whether the failing state names a `Done` state the definition then has to
    /// declare: a state no state reaches is not a definition this service accepts.
    transitions_to_done: bool,
    /// The diagnosis the service words for this row's member.
    diagnosis: Diagnosis,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a payload template's field, at the first state",
            failing: json!({
                "Type": "Pass",
                "Parameters": { "bucket.$": "$.targetBucket" },
                "End": true,
            }),
            before: 0,
            // Written with a space after the key separator, so the document the cause
            // echoes is the caller's own bytes rather than a re-encoding of them.
            input: r#"{"orderId": "o-1"}"#,
            path: "$.targetBucket",
            transitions_to_done: false,
            diagnosis: Diagnosis::Field {
                field: "bucket.$",
                searched: r#"{"orderId": "o-1"}"#,
            },
        },
        Row {
            case: "an OutputPath, two states into the definition",
            failing: json!({ "Type": "Pass", "OutputPath": "$['chosen']", "End": true }),
            before: 2,
            input: r#"{"present":true}"#,
            path: "$['chosen']",
            transitions_to_done: false,
            diagnosis: Diagnosis::Path,
        },
        Row {
            case: "a Choice rule's Variable, one state in",
            failing: json!({
                "Type": "Choice",
                "Choices": [{ "Variable": "$.status", "StringEquals": "done", "Next": "Done" }],
            }),
            before: 1,
            input: r#"{"other":1}"#,
            path: "$.status",
            transitions_to_done: true,
            diagnosis: Diagnosis::ChoiceCondition,
        },
        Row {
            case: "a Wait state's SecondsPath, at the first state",
            failing: json!({ "Type": "Wait", "SecondsPath": "$.delay", "End": true }),
            before: 0,
            input: r#"{"no-delay-here":true}"#,
            path: "$.delay",
            transitions_to_done: false,
            diagnosis: Diagnosis::Member("SecondsPath"),
        },
    ]
}

/// The definition one row runs: `before` `Pass` states, then the failing one, and the
/// `Done` state the failing one transitions to where it names one.
fn definition(row: &Row) -> String {
    let mut states = serde_json::Map::new();
    let mut start_at = FAILING.to_string();
    for index in 0..row.before {
        let name = format!("Step{index}");
        let next = if index + 1 == row.before {
            FAILING.to_string()
        } else {
            format!("Step{}", index + 1)
        };
        if index == 0 {
            start_at = name.clone();
        }
        states.insert(name, json!({ "Type": "Pass", "Next": next }));
    }
    states.insert(FAILING.to_string(), row.failing.clone());
    if row.transitions_to_done {
        states.insert("Done".to_string(), json!({ "Type": "Pass", "End": true }));
    }
    json!({ "StartAt": start_at, "States": Value::Object(states) }).to_string()
}

/// The attempt a redrive begins re-runs the unsuccessful step without entering it a
/// second time, so its diagnosis names the event the preserved history holds that
/// state's entry at — the same id the first attempt's own diagnosis named.
#[tokio::test]
async fn a_redriven_step_names_the_entry_the_preserved_history_holds() {
    let emulator = Emulator::new();
    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let machine = create_machine(&emulator, &format!("redrive-cause-{index}"), &definition(&row))
            .await;
        let execution = start_execution(&emulator, &machine, "only", row.input).await;
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR)),
            "case {case:?}: expected FAILED with {RUNTIME_ERROR}, got {described}"
        );

        // The entry the FIRST attempt recorded, and the cause it reported under it.
        let events = history(&emulator, &execution, json!({})).await;
        let entries = entries_of(&events, FAILING);
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: expected the failing state to be entered once, got {entries:?} in \
             {events:?}"
        );
        let entered = entries[0];
        let expected = expected_cause(FAILING, entered, row.path, &row.diagnosis);
        assert_eq!(
            last_cause(&events, case),
            expected,
            "case {case:?}: expected the first attempt's cause to name event #{entered}, got \
             {events:?}"
        );

        // The redriven attempt: the step is re-run, its entry is not recorded again, and
        // the cause it reports names that one entry.
        let redriven = redriven(&emulator, case, &execution).await;
        assert_eq!(
            (
                redriven.get("status").and_then(Value::as_str),
                redriven.get("error").and_then(Value::as_str),
                string(&redriven, "cause"),
            ),
            (
                Some("FAILED"),
                Some(RUNTIME_ERROR),
                expected.clone()
            ),
            "case {case:?}: expected the re-run to fail with the same cause, got {redriven}"
        );
        let after = history(&emulator, &execution, json!({})).await;
        assert_eq!(
            entries_of(&after, FAILING),
            vec![entered],
            "case {case:?}: expected the preserved entry to be the only one, got {after:?}"
        );
        assert_eq!(
            last_cause(&after, case),
            expected,
            "case {case:?}: expected the re-run's cause to name event #{entered}, got {after:?}"
        );
        assert_numbered(&after, case);
        // And on the OTHER read path: the paged, reversed read of the same history
        // answers the same stored event.
        let reversed = history(
            &emulator,
            &execution,
            json!({ "reverseOrder": true, "maxResults": 2 }),
        )
        .await;
        let newest = reversed
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected a page, got {reversed:?}"));
        assert_eq!(
            newest
                .get("executionFailedEventDetails")
                .and_then(|details| details.get("cause"))
                .and_then(Value::as_str),
            Some(expected.as_str()),
            "case {case:?}: expected the reversed page to carry the same cause, got {newest}"
        );
    }
}

/// A SECOND redrive names that same one entry: the entry is the history's, so continuing
/// the execution again does not move it.
#[tokio::test]
async fn a_second_redrive_names_the_same_preserved_entry() {
    let emulator = Emulator::new();
    let case = "an execution redriven twice";
    let row = rows().remove(0);
    let machine = create_machine(&emulator, "redrive-twice", &definition(&row)).await;
    let execution = start_execution(&emulator, &machine, "only", row.input).await;
    settled(&emulator, case, &execution).await;
    let entered = entries_of(&history(&emulator, &execution, json!({})).await, FAILING)
        .first()
        .copied()
        .unwrap_or_else(|| panic!("case {case:?}: expected the failing state to be entered"));
    let expected = expected_cause(FAILING, entered, row.path, &row.diagnosis);

    for attempt in 1..=2 {
        let redriven = redriven(&emulator, case, &execution).await;
        assert_eq!(
            (
                redriven.get("status").and_then(Value::as_str),
                string(&redriven, "cause"),
                redriven.get("redriveCount").and_then(Value::as_i64),
            ),
            (Some("FAILED"), expected.clone(), Some(attempt)),
            "case {case:?}: expected redrive {attempt} to fail with the same cause, got {redriven}"
        );
        let events = history(&emulator, &execution, json!({})).await;
        assert_eq!(
            entries_of(&events, FAILING),
            vec![entered],
            "case {case:?}: expected redrive {attempt} to record no second entry, got {events:?}"
        );
        assert_eq!(
            last_cause(&events, case),
            expected,
            "case {case:?}: expected redrive {attempt} to name event #{entered}, got {events:?}"
        );
        assert_numbered(&events, case);
    }
}

/// A state the redriven attempt ITSELF enters records an entry of its own, and its
/// diagnosis names that event: the entry the attempt did not record for the step it
/// continues from is not one of the events it appends, so nothing after it is numbered
/// as though it had been.
#[tokio::test]
async fn a_state_the_redriven_attempt_enters_names_the_event_it_records() {
    let emulator = Emulator::new();
    let case = "a state inside the Map state the redrive continues from";
    /// The nested state that fails, inside the re-entered `Map` state's processor.
    const NESTED: &str = "Check";
    let definition = json!({
        "StartAt": FAILING,
        "States": {
            FAILING: {
                "Type": "Map",
                "ItemsPath": "$.items",
                "ItemProcessor": {
                    "StartAt": NESTED,
                    "States": {
                        NESTED: { "Type": "Pass", "OutputPath": "$['chosen']", "End": true },
                    },
                },
                "End": true,
            },
        },
    })
    .to_string();
    let machine = create_machine(&emulator, "redrive-nested", &definition).await;
    let execution = start_execution(&emulator, &machine, "only", r#"{"items":[{"n":1}]}"#).await;
    settled(&emulator, case, &execution).await;

    let redriven = redriven(&emulator, case, &execution).await;
    let events = history(&emulator, &execution, json!({})).await;
    assert_numbered(&events, case);
    // The enclosing state was entered once, by the first attempt; the nested state was
    // entered by each attempt, because each one ran the iteration.
    assert_eq!(
        entries_of(&events, FAILING).len(),
        1,
        "case {case:?}: expected the re-entered state's one preserved entry, got {events:?}"
    );
    let nested = entries_of(&events, NESTED);
    assert_eq!(
        nested.len(),
        2,
        "case {case:?}: expected one entry of the nested state per attempt, got {nested:?} in \
         {events:?}"
    );
    let entered = nested[1];
    let expected = expected_cause(NESTED, entered, "$['chosen']", &Diagnosis::Path);
    assert_eq!(
        (
            redriven.get("status").and_then(Value::as_str),
            string(&redriven, "cause"),
        ),
        (Some("FAILED"), expected.clone()),
        "case {case:?}: expected the re-run to name the entry it recorded (#{entered}), got \
         {redriven}"
    );
    assert_eq!(
        last_cause(&events, case),
        expected,
        "case {case:?}: expected the stored event to name #{entered}, got {events:?}"
    );
}

// ---------------------------------------------------------------------------
// A worker's reply: the state it is placed at was entered by the call that
// scheduled the task
// ---------------------------------------------------------------------------

/// The definition an activity row runs: the `Task` state that schedules the activity,
/// with `extra` as its own members, and the `Pick` state it transitions to where it
/// names one.
fn activity_definition_of(activity_arn: &str, extra: Value, next: Option<Value>) -> String {
    let mut task = json!({ "Type": "Task", "Resource": activity_arn });
    let members = task.as_object_mut().expect("the state is an object");
    for (member, value) in extra.as_object().expect("the extras are an object") {
        members.insert(member.clone(), value.clone());
    }
    let mut states = json!({});
    let declared = states.as_object_mut().expect("the states are an object");
    match next {
        Some(next) => {
            members.insert("Next".to_string(), json!("Pick"));
            declared.insert("Pick".to_string(), next);
        }
        None => {
            members.insert("End".to_string(), json!(true));
        }
    }
    declared.insert(FAILING.to_string(), task);
    json!({ "StartAt": FAILING, "States": states }).to_string()
}

/// One row of the reply table: the members the scheduling state carries, the state it
/// transitions to where it names one, and the state whose member could not be resolved.
struct Answered {
    case: &'static str,
    /// The members the `Task` state that schedules the activity carries beside its
    /// `Resource`.
    extra: Value,
    /// The state the task transitions to, for a row whose FAILING state is one the
    /// continuation enters itself.
    next: Option<Value>,
    /// The state the failure is reported for.
    failing: &'static str,
    /// The path the cause must name.
    path: &'static str,
    /// The diagnosis the service words for this row's member.
    diagnosis: Diagnosis,
}

/// The continuation a worker's reply begins is placed AT the state that scheduled the
/// task, whose entry the call that scheduled it recorded: a failure of that state's own
/// result-side members names that preserved entry, and a failure of a state the
/// continuation goes on to enter names the entry the continuation recorded for it.
#[tokio::test]
async fn an_answered_task_names_the_entry_its_scheduling_recorded() {
    let emulator = Emulator::new();
    let rows: Vec<Answered> = vec![
        Answered {
            case: "a ResultSelector field of the state the reply is placed at",
            extra: json!({ "ResultSelector": { "picked.$": "$.absent" } }),
            next: None,
            failing: FAILING,
            path: "$.absent",
            diagnosis: Diagnosis::Field {
                field: "picked.$",
                searched: r#"{"other":1}"#,
            },
        },
        Answered {
            case: "an OutputPath of the same state, over what the reply produced",
            extra: json!({ "OutputPath": "$['chosen']" }),
            next: None,
            failing: FAILING,
            path: "$['chosen']",
            diagnosis: Diagnosis::Path,
        },
        Answered {
            case: "an OutputPath of the state the continuation goes on to enter",
            extra: json!({}),
            next: Some(json!({ "Type": "Pass", "OutputPath": "$['chosen']", "End": true })),
            failing: "Pick",
            path: "$['chosen']",
            diagnosis: Diagnosis::Path,
        },
    ];

    for (index, row) in rows.into_iter().enumerate() {
        let Answered {
            case,
            extra,
            next,
            failing,
            path,
            diagnosis,
        } = row;
        let activity = create_activity(&emulator, &format!("answered-{index}")).await;
        let machine = create_machine(
            &emulator,
            &format!("answered-{index}"),
            &activity_definition_of(&activity, extra, next),
        )
        .await;
        let execution = start_execution(&emulator, &machine, "only", "{}").await;
        let (token, _) = claimed(&emulator, case, &activity, Some("worker")).await;

        // The entry the call that SCHEDULED the task recorded for that state.
        let scheduled = history(&emulator, &execution, json!({})).await;
        let entered_task = entries_of(&scheduled, FAILING);
        assert_eq!(
            entered_task.len(),
            1,
            "case {case:?}: expected the scheduling state's one entry, got {scheduled:?}"
        );

        emulator
            .ok(
                "SendTaskSuccess",
                &json!({ "taskToken": token, "output": r#"{"other":1}"# }),
            )
            .await;
        let described = settled(&emulator, case, &execution).await;
        let events = history(&emulator, &execution, json!({})).await;
        assert_numbered(&events, case);
        let entries = entries_of(&events, failing);
        assert_eq!(
            entries.len(),
            1,
            "case {case:?}: expected one entry of the failing state, got {entries:?} in \
             {events:?}"
        );
        let entered = entries[0];
        let expected = expected_cause(failing, entered, path, &diagnosis);
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                string(&described, "cause"),
            ),
            (Some("FAILED"), expected.clone()),
            "case {case:?}: expected the continuation to name event #{entered}, got {described}"
        );
        assert_eq!(
            last_cause(&events, case),
            expected,
            "case {case:?}: expected the stored event to name #{entered}, got {events:?}"
        );
    }
}
