// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH field of a state a failure names, and in WHAT WORDS, driven
//! over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rules under test:
//!
//! 1. the fields of the node that decides a transition are evaluated in a FIXED order with
//!    `Assign` LAST — a `Choice` rule's `Condition`, then the `Output` that decides what the
//!    state puts out, then the `Assign` that applies — so the FIRST of them to fail is the
//!    one the cause and the `EvaluationFailed` event locate, and what the `Assign` writes is
//!    first read by the state AFTER the transition rather than by the output of it;
//! 2. a `*Path` member that holds a count of seconds and whose path selects a value that is
//!    not a whole number could not be READ as that count: the failure names the member, the
//!    reference path as the definition wrote it, and the value the path selected, rendered as
//!    JSON;
//! 3. a failure about a document the EXECUTION's own data carries withholds that document:
//!    the source of every location clause is the redaction marker, never the text the
//!    expression was reading;
//! 4. a member whose value IS a string and merely denotes no instant could not be READ,
//!    which is reported as the parse failure it is rather than as a value of the wrong JSON
//!    type — the two are different outcomes and a client classifies them apart;
//! 5. the context object (`$$`) a cause quotes carries its members in the order the service
//!    serializes them, and the state's own members are the attempt being run — its name, the
//!    instant it was entered and the attempt it is on — because the cause is ONE string a
//!    client compares whole;
//! 6. a continuation that does not re-enter the state it resumes names the entry the
//!    PRESERVED history holds, so a deterministic failure reproduced by a redrive reports
//!    the same cause byte for byte.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`) is the one `state_machine_lifecycle` builds, the history reader is
//! `execution_history`'s and the activity helpers are `activity_tasks`'; `tests/all/main.rs`
//! picks this file up, so each is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EVENT_EVALUATION_FAILED, STATE_ENTERED_SUFFIX,
};

use crate::activity_tasks::{activity_definition, create_activity};
use crate::execution_history::history;
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator,
};

/// The ASL error name an expression that could not be evaluated fails under.
const QUERY_EVALUATION_ERROR: &str = "States.QueryEvaluationError";

/// The ASL error name the interpreter raises its own failures under.
const RUNTIME_ERROR: &str = "States.Runtime";

/// The source a location clause names where the document it was reading is WITHHELD
/// (`service_common::json_parse_fault::REDACTED_SOURCE`, this workspace's one statement of
/// it).
const REDACTED_SOURCE: &str = "REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` \
                               disabled)";

/// How often an execution is looked at again, and how long it is waited for at all.
///
/// The wait is for the CONDITION — the execution leaving `RUNNING` — so the deadline is
/// generous and a host that settles one quickly spends none of it: nothing here measures how
/// fast the host runs.
const POLL: Duration = Duration::from_millis(25);
/// See [`POLL`].
const DEADLINE: Duration = Duration::from_secs(120);

/// The record of `execution_arn` once it has left `RUNNING`, looked for again until it has or
/// [`DEADLINE`] passes.
async fn settled(emulator: &Emulator, case: &str, execution_arn: &str) -> Value {
    let deadline = Instant::now() + DEADLINE;
    loop {
        let described = describe_execution(emulator, execution_arn).await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to settle, got {described:?}"
        );
        tokio::time::sleep(POLL).await;
    }
}

/// The diagnosis `diagnosis` as the state `state`, entered at the event `entered`, reports
/// it: the ONE wrapper every failure the interpreter raises while executing a state is
/// reported under.
fn executing_state(state: &str, entered: i64, diagnosis: &str) -> String {
    format!(
        "An error occurred while executing the state '{state}' (entered at the event id \
         #{entered}). {diagnosis}"
    )
}

/// The ids of every event `events` holds the ENTRY of the state `name` at, in history order.
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

/// The one event `events` holds the entry of `name` at.
fn entered_once(events: &[Value], case: &str, name: &str) -> i64 {
    let entries = entries_of(events, name);
    assert_eq!(
        entries.len(),
        1,
        "case {case:?}: expected {name} to be entered once, got {entries:?} in {events:?}"
    );
    entries[0]
}

/// The `location` the one `EvaluationFailed` event of `events` names.
fn failed_location(events: &[Value], case: &str) -> String {
    let event = events
        .iter()
        .find(|event| event.get("type").and_then(Value::as_str) == Some(EVENT_EVALUATION_FAILED))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an {EVENT_EVALUATION_FAILED} event in {events:?}")
        });
    event
        .get("evaluationFailedEventDetails")
        .and_then(|details| details.get("location"))
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected a location on {event:?}"))
        .to_string()
}

/// The output of a settled execution, as the document it is.
fn output_of(described: &Value) -> Value {
    described
        .get("output")
        .and_then(Value::as_str)
        .map(|text| serde_json::from_str(text).expect("the output is JSON"))
        .unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// 1. A node's fields are evaluated with `Assign` last
// ---------------------------------------------------------------------------

/// What one row of the field-order table must end as.
enum Expect {
    /// The field at `location`, carrying `expression`, THREW: the cause names that field and
    /// quotes that expression, and the engine's own account of the failure follows.
    Threw {
        location: &'static str,
        expression: &'static str,
    },
    /// The field at `location`, carrying `expression`, produced NOTHING, which is what a
    /// reference to a variable nothing in scope assigned does.
    Nothing {
        location: &'static str,
        expression: &'static str,
    },
    /// The run succeeded, carrying this output.
    Output(Value),
}

/// One row: the `Choice` state under test, the input it runs over, and what it must end as.
struct FieldRow {
    case: &'static str,
    state: Value,
    input: &'static str,
    expect: Expect,
}

/// An expression that THROWS when `flag` holds in the input, and produces `otherwise`
/// otherwise: the left side of the addition is the input's own string, so the engine refuses
/// the operator rather than this suite inventing a diagnostic.
fn throws_when(flag: &str, otherwise: &str) -> String {
    format!("{{% $states.input.{flag} ? ($states.input.s + 1) : '{otherwise}' %}}")
}

/// The machine one row runs: its `Choice` state, and the two states a transition can reach —
/// each of which puts out the variable the transition assigned, so what the `Assign` wrote is
/// read by the state AFTER it.
fn choice_machine(state: &Value) -> String {
    let echo = json!({ "Type": "Pass", "Output": { "z": "{% $z %}" }, "End": true });
    json!({
        "QueryLanguage": "JSONata",
        "StartAt": "C",
        "States": { "C": state, "Done": echo, "Fallback": echo },
    })
    .to_string()
}

/// A `Choice` state carrying `rules` and whatever state-level members `members` declares.
fn choice_state(rules: Value, members: Value) -> Value {
    let mut state = json!({ "Type": "Choice", "Choices": rules, "Default": "Fallback" });
    let object = state.as_object_mut().expect("the state is an object");
    for (member, value) in members.as_object().expect("the members are an object") {
        object.insert(member.clone(), value.clone());
    }
    state
}

fn field_order_rows() -> Vec<FieldRow> {
    // Every row's rule matches on a `Condition` that holds unless its own flag is set, so
    // which field fails is decided by the input alone.
    let matching = |members: Value| {
        choice_state(
            json!([{
                "Condition": throws_when("condBad", "unused"),
                "Output": throws_when("outBad", "rule-output"),
                "Assign": { "z": throws_when("assignBad", "rule-assign") },
                "Next": "Done",
            }]),
            members,
        )
    };
    // The rule's `Condition` is written as a ternary whose false arm is a string, so a
    // matching condition is one whose arm is the boolean the field requires.
    let rule = |condition: &str, members: Value| {
        choice_state(
            json!([{
                "Condition": condition,
                "Output": throws_when("outBad", "rule-output"),
                "Assign": { "z": throws_when("assignBad", "rule-assign") },
                "Next": "Done",
            }]),
            members,
        )
    };
    vec![
        FieldRow {
            // The recorded shape: the condition holds and BOTH of the rule's other fields
            // throw, so the one the order reaches first is the one reported.
            case: "a matched rule whose Output and Assign both throw",
            state: rule("{% $states.input.ok %}", json!({})),
            input: r#"{"s":"x","ok":true,"outBad":true,"assignBad":true}"#,
            expect: Expect::Threw {
                location: "Choices[0]/Output",
                expression: "$states.input.outBad ? ($states.input.s + 1) : 'rule-output'",
            },
        },
        FieldRow {
            case: "a matched rule whose Output evaluates and whose Assign throws",
            state: rule("{% $states.input.ok %}", json!({})),
            input: r#"{"s":"x","ok":true,"outBad":false,"assignBad":true}"#,
            expect: Expect::Threw {
                location: "Choices[0]/Assign/z",
                expression: "$states.input.assignBad ? ($states.input.s + 1) : 'rule-assign'",
            },
        },
        FieldRow {
            case: "a rule whose Condition throws, before either of its other fields",
            state: matching(json!({})),
            input: r#"{"s":"x","condBad":true,"outBad":true,"assignBad":true}"#,
            expect: Expect::Threw {
                location: "Choices[0]/Condition",
                expression: "$states.input.condBad ? ($states.input.s + 1) : 'unused'",
            },
        },
        FieldRow {
            // The rule the state SELECTED is the one whose fields are evaluated, so the
            // location names that rule's own place in the array.
            case: "the second rule of two, whose Output and Assign both throw",
            state: choice_state(
                json!([
                    { "Condition": "{% $states.input.first %}", "Next": "Done" },
                    {
                        "Condition": "{% $states.input.ok %}",
                        "Output": throws_when("outBad", "rule-output"),
                        "Assign": { "z": throws_when("assignBad", "rule-assign") },
                        "Next": "Done",
                    },
                ]),
                json!({}),
            ),
            input: r#"{"s":"x","first":false,"ok":true,"outBad":true,"assignBad":true}"#,
            expect: Expect::Threw {
                location: "Choices[1]/Output",
                expression: "$states.input.outBad ? ($states.input.s + 1) : 'rule-output'",
            },
        },
        FieldRow {
            // No rule matched, so the state's OWN fields decide the transition — in the
            // same order, which locates the failure at the state's own members.
            case: "the Default path, where the state's own Output and Assign both throw",
            state: choice_state(
                json!([{ "Condition": "{% $states.input.ok %}", "Next": "Done" }]),
                json!({
                    "Output": throws_when("outBad", "state-output"),
                    "Assign": { "z": throws_when("assignBad", "state-assign") },
                }),
            ),
            input: r#"{"s":"x","ok":false,"outBad":true,"assignBad":true}"#,
            expect: Expect::Threw {
                location: "Output",
                expression: "$states.input.outBad ? ($states.input.s + 1) : 'state-output'",
            },
        },
        FieldRow {
            case: "the Default path, where only the state's own Assign throws",
            state: choice_state(
                json!([{ "Condition": "{% $states.input.ok %}", "Next": "Done" }]),
                json!({
                    "Output": throws_when("outBad", "state-output"),
                    "Assign": { "z": throws_when("assignBad", "state-assign") },
                }),
            ),
            input: r#"{"s":"x","ok":false,"outBad":false,"assignBad":true}"#,
            expect: Expect::Threw {
                location: "Assign/z",
                expression: "$states.input.assignBad ? ($states.input.s + 1) : 'state-assign'",
            },
        },
        FieldRow {
            // The consequence of the order: the `Assign` has not run when the `Output` is
            // built, so an output reading the name only that `Assign` writes reads nothing.
            case: "a matched rule whose Output reads the name its own Assign writes",
            state: choice_state(
                json!([{
                    "Condition": "{% $states.input.ok %}",
                    "Output": "{% $z %}",
                    "Assign": { "z": "{% $states.input.s %}" },
                    "Next": "Done",
                }]),
                json!({}),
            ),
            input: r#"{"s":"x","ok":true}"#,
            expect: Expect::Nothing {
                location: "Choices[0]/Output",
                expression: "$z",
            },
        },
        FieldRow {
            // The control for the matched path: every field evaluates, and what the rule
            // assigned is what the state AFTER the transition reads.
            case: "a matched rule whose every field evaluates",
            state: rule("{% $states.input.ok %}", json!({})),
            input: r#"{"s":"x","ok":true,"outBad":false,"assignBad":false}"#,
            expect: Expect::Output(json!({ "z": "rule-assign" })),
        },
        FieldRow {
            // And for the Default path, where the state's own members are the ones that
            // decide — and whose assignment the state after it reads.
            case: "the Default path, where every field of the state evaluates",
            state: choice_state(
                json!([{ "Condition": "{% $states.input.ok %}", "Next": "Done" }]),
                json!({
                    "Output": throws_when("outBad", "state-output"),
                    "Assign": { "z": throws_when("assignBad", "state-assign") },
                }),
            ),
            input: r#"{"s":"x","ok":false,"outBad":false,"assignBad":false}"#,
            expect: Expect::Output(json!({ "z": "state-assign" })),
        },
    ]
}

/// A `Choice` state's fields are evaluated with `Assign` last, so the field a failure names
/// is the first one the order reaches.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_choice_states_fields_are_evaluated_with_assign_last() {
    let rows = field_order_rows();
    let emulator = Emulator::new();
    // Every row's execution is started before any is read: each settles on the clock the
    // service owes it, so the rows settle together rather than one after another.
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let machine = create_machine(
            &emulator,
            &format!("field-order-{index}"),
            &choice_machine(&row.state),
        )
        .await;
        started.push(start_execution(&emulator, &machine, "only", row.input).await);
    }

    for (row, execution) in rows.iter().zip(started) {
        let case = row.case;
        let described = settled(&emulator, case, &execution).await;
        let events = history(&emulator, &execution, json!({})).await;
        let (location, expression, diagnosis) = match &row.expect {
            Expect::Threw {
                location,
                expression,
            } => (*location, *expression, "threw an error during evaluation."),
            Expect::Nothing {
                location,
                expression,
            } => (*location, *expression, "returned nothing (undefined)."),
            Expect::Output(wanted) => {
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the run whose every field evaluates to succeed, got \
                     {described:?}"
                );
                let actual = output_of(&described);
                assert_eq!(
                    &actual, wanted,
                    "case {case:?}: expected the output {wanted}, got {actual}"
                );
                continue;
            }
        };
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(QUERY_EVALUATION_ERROR)),
            "case {case:?}: expected FAILED with {QUERY_EVALUATION_ERROR}, got {described:?}"
        );
        let entered = entered_once(&events, case, "C");
        let cause = string(&described, "cause");
        let expected = executing_state(
            "C",
            entered,
            &format!(
                "The JSONata expression '{expression}' specified for the field '{location}' \
                 {diagnosis}"
            ),
        );
        match &row.expect {
            // An expression that threw carries the engine's own account of the failure
            // after the sentence, which is the engine's text rather than this service's.
            Expect::Threw { .. } => assert!(
                cause.starts_with(&expected) && cause.len() > expected.len(),
                "case {case:?}: expected a cause opening {expected:?} and carrying the engine's \
                 own account, got {cause:?}"
            ),
            _ => assert_eq!(
                cause, expected,
                "case {case:?}: expected the cause {expected:?}, got {cause:?}"
            ),
        }
        assert_eq!(
            failed_location(&events, case),
            location,
            "case {case:?}: expected the event to locate the failure at {location:?}, got \
             {events:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 2. A timing `*Path` whose value is not a whole number
// ---------------------------------------------------------------------------

/// One row of the timing-path table: the member the definition writes, the reference path in
/// it, the input the path is resolved against, and the diagnosis the cause must carry.
struct TimingRow {
    case: &'static str,
    /// The `Task` state's own members, which carry exactly one of each pair.
    members: Value,
    input: &'static str,
    diagnosis: String,
}

fn timing_rows() -> Vec<TimingRow> {
    // The member catalog is the Amazon States Language's own for a `Task` state: the two
    // members that bound a task's clock, each of which has a `*Path` form. The rows cross
    // them with the kinds of value a path can select that are not a whole number — a string,
    // a fraction, a boolean, an object — plus the whole number the member does not allow,
    // which WAS read and is reported as the value it is.
    let not_a_long = |member: &str, path: &str, value: &str| {
        format!("The {member} parameter cannot be parsed as a long value: {path} == {value}")
    };
    vec![
        TimingRow {
            case: "HeartbeatSecondsPath resolving to a string",
            members: json!({ "HeartbeatSecondsPath": "$.hb", "TimeoutSeconds": 120 }),
            input: r#"{"hb":"not-a-number"}"#,
            diagnosis: not_a_long("HeartbeatSecondsPath", "$.hb", "\"not-a-number\""),
        },
        TimingRow {
            case: "HeartbeatSecondsPath resolving to a fraction",
            members: json!({ "HeartbeatSecondsPath": "$.hb", "TimeoutSeconds": 120 }),
            input: r#"{"hb":1.5}"#,
            diagnosis: not_a_long("HeartbeatSecondsPath", "$.hb", "1.5"),
        },
        TimingRow {
            case: "HeartbeatSecondsPath written in bracket notation, resolving to a boolean",
            members: json!({ "HeartbeatSecondsPath": "$['beat']['s']", "TimeoutSeconds": 120 }),
            input: r#"{"beat":{"s":true}}"#,
            diagnosis: not_a_long("HeartbeatSecondsPath", "$['beat']['s']", "true"),
        },
        TimingRow {
            case: "TimeoutSecondsPath resolving to a string",
            members: json!({ "TimeoutSecondsPath": "$.to" }),
            input: r#"{"to":"nope"}"#,
            diagnosis: not_a_long("TimeoutSecondsPath", "$.to", "\"nope\""),
        },
        TimingRow {
            case: "TimeoutSecondsPath resolving to an object",
            members: json!({ "TimeoutSecondsPath": "$.to" }),
            input: r#"{"to":{"seconds":5}}"#,
            diagnosis: not_a_long("TimeoutSecondsPath", "$.to", r#"{"seconds":5}"#),
        },
        TimingRow {
            // The boundary: a WHOLE number the member does not allow was read as the
            // integer the member holds, so what is reported is the value, not the text.
            case: "HeartbeatSecondsPath resolving to zero",
            members: json!({ "HeartbeatSecondsPath": "$.hb", "TimeoutSeconds": 120 }),
            input: r#"{"hb":0}"#,
            diagnosis: "The value referenced by HeartbeatSecondsPath must be a positive whole \
                        number of seconds, but the value was: 0"
                .to_string(),
        },
    ]
}

/// A `Task` state's timing `*Path` whose path selects a value that is not a whole number
/// fails the execution before the task is dispatched, naming the member, the path and the
/// value.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_timing_path_that_is_not_a_whole_number_names_the_path_and_the_value() {
    let rows = timing_rows();
    let emulator = Emulator::new();
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let activity = create_activity(&emulator, &format!("timing-{index}")).await;
        let machine = create_machine(
            &emulator,
            &format!("timing-{index}"),
            &activity_definition(&activity, row.members.clone()),
        )
        .await;
        started.push(start_execution(&emulator, &machine, "only", row.input).await);
    }

    for (row, execution) in rows.iter().zip(started) {
        let case = row.case;
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR)),
            "case {case:?}: expected FAILED with {RUNTIME_ERROR}, got {described:?}"
        );
        let events = history(&emulator, &execution, json!({})).await;
        // The id the cause names is the event the state was ENTERED at, which the suite
        // below pins; what this one pins is the diagnosis the wrapper carries.
        let opening = "An error occurred while executing the state 'Work' (entered at the event \
                       id #";
        let closing = format!("). {}", row.diagnosis);
        let cause = string(&described, "cause");
        assert!(
            cause.starts_with(opening) && cause.ends_with(&closing),
            "case {case:?}: expected a cause opening {opening:?} and ending {closing:?}, got \
             {cause:?}"
        );
        // And nothing was dispatched for a task whose own bounds could not be read: the
        // activity holds no task to claim.
        assert!(
            !events
                .iter()
                .any(|event| event.get("type").and_then(Value::as_str) == Some("ActivityScheduled")),
            "case {case:?}: expected no task to have been scheduled, got {events:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 3. A failure about the execution's own data withholds that data
// ---------------------------------------------------------------------------

/// One row of the redaction table: the text the intrinsic is handed, the parser's diagnosis
/// of it, and the position the parse ended at where the recording pins one.
struct RedactionRow {
    case: &'static str,
    text: &'static str,
    diagnosis: &'static str,
    /// The line and column the clause must state, for the row whose position is recorded;
    /// `None` leaves the position to the parse that ran and pins the clause's SHAPE alone.
    position: Option<(usize, usize)>,
}

fn redaction_rows() -> Vec<RedactionRow> {
    vec![
        RedactionRow {
            // The recorded shape: a document whose first member name is not quoted.
            case: "an unquoted member name",
            text: "{not valid json",
            diagnosis: "Unexpected character ('n' (code 110)): was expecting double-quote to \
                        start field name",
            position: Some((1, 2)),
        },
        RedactionRow {
            case: "a bare word where a value belongs",
            text: "notjson",
            diagnosis: "Unrecognized token 'notjson': was expecting (JSON String, Number, Array, \
                        Object or token 'null', 'true' or 'false')",
            position: None,
        },
        RedactionRow {
            case: "a document that carries more than one value",
            text: r#"{"a": 1}}"#,
            diagnosis: "Unexpected character ('}' (code 125)): was expecting end of the document",
            position: None,
        },
        RedactionRow {
            // A document that ran out of input inside a collection names the mark it left
            // unmatched as well, which is a second clause whose source is withheld too.
            case: "an array left open",
            text: "[1,2",
            diagnosis: "Unexpected end-of-input: expected close marker for Array (start marker \
                        at [Source: REDACTED (`StreamReadFeature.INCLUDE_SOURCE_IN_LOCATION` \
                        disabled); line: 1, column: 1])",
            position: None,
        },
    ]
}

/// `States.StringToJson` over text that is not JSON reports WHERE the parse stopped and
/// withholds the text it was reading: every location clause names the redaction marker as its
/// source.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_malformed_intrinsic_argument_is_reported_with_its_source_withheld() {
    let rows = redaction_rows();
    let definition = json!({
        "StartAt": "Stj",
        "States": {
            "Stj": {
                "Type": "Pass",
                "Parameters": { "o.$": "States.StringToJson($.bad)" },
                "End": true,
            },
        },
    })
    .to_string();

    let emulator = Emulator::new();
    let machine = create_machine(&emulator, "string-to-json", &definition).await;
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let input = json!({ "bad": row.text }).to_string();
        started.push(start_execution(&emulator, &machine, &format!("row-{index}"), &input).await);
    }

    for (row, execution) in rows.iter().zip(started) {
        let case = row.case;
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR)),
            "case {case:?}: expected FAILED with {RUNTIME_ERROR}, got {described:?}"
        );
        let events = history(&emulator, &execution, json!({})).await;
        let entered = entered_once(&events, case, "Stj");
        let opening = executing_state(
            "Stj",
            entered,
            &format!(
                "There was an error while evaluating the intrinsic function: \
                 States.StringToJson($.bad). Cannot convert String to Json in \
                 States.StringToJson, caused by: {}\n at [Source: {REDACTED_SOURCE}; line: ",
                row.diagnosis
            ),
        );
        let cause = string(&described, "cause");
        match row.position {
            // The recorded case states the position as well, so the whole cause is fixed.
            Some((line, column)) => {
                let expected = format!("{opening}{line}, column: {column}]");
                assert_eq!(
                    cause, expected,
                    "case {case:?}: expected the cause {expected:?}, got {cause:?}"
                );
            }
            None => assert!(
                cause.starts_with(&opening) && cause.ends_with(']'),
                "case {case:?}: expected a cause opening {opening:?} and ending in the position \
                 the parse stopped at, got {cause:?}"
            ),
        }
        // Whatever the fault class, no location clause reports the document it was reading
        // as its source.
        assert!(
            !cause.contains("(String)\""),
            "case {case:?}: expected no clause to quote the document it was reading, got \
             {cause:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 4. A string that denotes no instant could not be READ
// ---------------------------------------------------------------------------

/// One row of the instant table: the `Wait` state under test, the input it runs over, the
/// error name the failure carries and the diagnosis its cause states.
struct InstantRow {
    case: &'static str,
    state: Value,
    input: &'static str,
    /// Whether the machine is written in the OTHER query language, which is the one whose
    /// `*Path` members read the value out of the state's data.
    jsonpath: bool,
    error: &'static str,
    diagnosis: String,
}

fn instant_rows() -> Vec<InstantRow> {
    // The diagnosis of a member whose string could not be read as an instant, and the one of
    // a member whose value is of another JSON type: the two outcomes the rule holds apart.
    let not_an_instant = |member: &str, text: &str| {
        format!(
            "The {member} field cannot be parsed as an ISO-8601 extended offset date-time \
             format string: {text}"
        )
    };
    let wrong_type = |expression: &str, field: &str, expected: &str, was: &str, value: &str| {
        format!(
            "The JSONata expression '{expression}' specified for the field '{field}' returned an \
             unexpected result type. Expected '{expected}', but was '{was}' for value: {value}"
        )
    };
    vec![
        InstantRow {
            // The recorded shape: a date with no time and no offset, which denotes no
            // instant.
            case: "a JSONata Timestamp expression resolving to a date alone",
            state: json!({ "Type": "Wait", "Timestamp": "{% $states.input.t %}", "End": true }),
            input: r#"{"t":"2024-11-22"}"#,
            jsonpath: false,
            error: QUERY_EVALUATION_ERROR,
            diagnosis: not_an_instant("Timestamp", "2024-11-22"),
        },
        InstantRow {
            case: "a JSONata Timestamp expression resolving to a date-time with no offset",
            state: json!({ "Type": "Wait", "Timestamp": "{% $states.input.t %}", "End": true }),
            input: r#"{"t":"2024-11-22T10:15:00"}"#,
            jsonpath: false,
            error: QUERY_EVALUATION_ERROR,
            diagnosis: not_an_instant("Timestamp", "2024-11-22T10:15:00"),
        },
        InstantRow {
            case: "a JSONata Timestamp expression resolving to text that is no date at all",
            state: json!({ "Type": "Wait", "Timestamp": "{% $states.input.t %}", "End": true }),
            input: r#"{"t":"yesterday"}"#,
            jsonpath: false,
            error: QUERY_EVALUATION_ERROR,
            diagnosis: not_an_instant("Timestamp", "yesterday"),
        },
        InstantRow {
            // The same reading in the other query language, where the text is selected by a
            // reference path: the diagnosis is the service's own either way, and the error
            // name is the one that language's failures carry.
            case: "a TimestampPath resolving to a date alone",
            state: json!({ "Type": "Wait", "TimestampPath": "$.t", "End": true }),
            input: r#"{"t":"2024-11-22"}"#,
            jsonpath: true,
            error: RUNTIME_ERROR,
            diagnosis: not_an_instant("TimestampPath", "2024-11-22"),
        },
        InstantRow {
            // And for a member the definition wrote as a LITERAL, which no expression
            // produced.
            case: "a Timestamp literal that denotes no instant",
            state: json!({ "Type": "Wait", "Timestamp": "2024-11-22", "End": true }),
            input: "{}",
            jsonpath: false,
            error: RUNTIME_ERROR,
            diagnosis: not_an_instant("Timestamp", "2024-11-22"),
        },
        InstantRow {
            // The complement: a value of another JSON type never denoted an instant, and is
            // reported as the result type it is.
            case: "a JSONata Timestamp expression resolving to a number",
            state: json!({ "Type": "Wait", "Timestamp": "{% $states.input.t %}", "End": true }),
            input: r#"{"t":5}"#,
            jsonpath: false,
            error: QUERY_EVALUATION_ERROR,
            diagnosis: wrong_type("$states.input.t", "Timestamp", "string", "number", "5"),
        },
        InstantRow {
            case: "a JSONata Seconds expression resolving to a string",
            state: json!({ "Type": "Wait", "Seconds": "{% $states.input.t %}", "End": true }),
            input: r#"{"t":"5"}"#,
            jsonpath: false,
            error: QUERY_EVALUATION_ERROR,
            diagnosis: wrong_type("$states.input.t", "Seconds", "number", "string", "\"5\""),
        },
    ]
}

/// A `Wait` state's `Timestamp` that is a string denoting no instant is reported as the parse
/// failure it is, and a value of another type as the result type it is.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_timestamp_that_denotes_no_instant_is_reported_as_a_parse_failure() {
    let rows = instant_rows();
    let emulator = Emulator::new();
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let mut definition = json!({ "StartAt": "W", "States": { "W": row.state.clone() } });
        if !row.jsonpath {
            definition["QueryLanguage"] = json!("JSONata");
        }
        let machine =
            create_machine(&emulator, &format!("instant-{index}"), &definition.to_string()).await;
        started.push(start_execution(&emulator, &machine, "only", row.input).await);
    }

    for (row, execution) in rows.iter().zip(started) {
        let case = row.case;
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(row.error)),
            "case {case:?}: expected FAILED with {}, got {described:?}",
            row.error
        );
        let events = history(&emulator, &execution, json!({})).await;
        let entered = entered_once(&events, case, "W");
        let expected = executing_state("W", entered, &row.diagnosis);
        let cause = string(&described, "cause");
        assert_eq!(
            cause, expected,
            "case {case:?}: expected the cause {expected:?}, got {cause:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. The context object a cause quotes keeps its member order
// ---------------------------------------------------------------------------

/// The members of the context object's `Execution`, in the order the service serializes
/// them: the cause is ONE string, so the order is part of what a client compares.
const EXECUTION_MEMBERS: [&str; 6] = [
    "Id",
    "Input",
    "StartTime",
    "Name",
    "RoleArn",
    "RedriveCount",
];

/// One row: the state whose member reads a context path that resolves to nothing, and the
/// input the execution runs over.
struct ContextRow {
    case: &'static str,
    state: Value,
    input: &'static str,
}

/// The name every row's failing state carries.
const FAILING_STATE: &str = "BadMap";

fn context_rows() -> Vec<ContextRow> {
    vec![
        ContextRow {
            // The recorded shape: a `Map` state's `ItemSelector` reading a member of
            // `$$.Map.Item` that is not there.
            case: "a Map state's ItemSelector",
            state: json!({
                "Type": "Map",
                "ItemsPath": "$.items",
                "ItemSelector": { "bad.$": "$$.Map.Item.Invalid" },
                "ItemProcessor": {
                    "StartAt": "P",
                    "States": { "P": { "Type": "Pass", "End": true } },
                },
                "End": true,
            }),
            input: r#"{"mode":"bad","items":[10,20,30]}"#,
        },
        ContextRow {
            case: "a payload template's field reading a member of $$.Execution",
            state: json!({
                "Type": "Pass",
                "Parameters": { "bad.$": "$$.Execution.Nope" },
                "End": true,
            }),
            input: r#"{"mode":"bad"}"#,
        },
        ContextRow {
            // The other place a cause quotes the whole object: an intrinsic function's own
            // `*Path` argument.
            case: "an intrinsic function's path argument",
            state: json!({
                "Type": "Pass",
                "Parameters": { "bad.$": "States.Format('{}', $$.Execution.Nope)" },
                "End": true,
            }),
            input: r#"{"mode":"bad"}"#,
        },
    ]
}

/// The context object embedded in the cause of a path that could not be resolved against it
/// carries `Execution`'s members in the order the service serializes them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_context_object_a_cause_quotes_keeps_its_member_order() {
    let rows = context_rows();
    let emulator = Emulator::new();
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let mut states = serde_json::Map::new();
        states.insert(FAILING_STATE.to_string(), row.state.clone());
        let definition = json!({ "StartAt": FAILING_STATE, "States": states }).to_string();
        let machine = create_machine(&emulator, &format!("context-{index}"), &definition).await;
        started.push(start_execution(&emulator, &machine, "only", row.input).await);
    }

    for (row, execution) in rows.iter().zip(started) {
        let case = row.case;
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR)),
            "case {case:?}: expected FAILED with {RUNTIME_ERROR}, got {described:?}"
        );
        let cause = string(&described, "cause");
        assert!(
            cause.contains("\"Execution\":{"),
            "case {case:?}: expected the cause to quote the context object, got {cause:?}"
        );
        // Each member of `Execution` is named once before the object that follows it names
        // one of the same names, so the FIRST occurrence of each is that member's.
        let mut previous = 0usize;
        for member in EXECUTION_MEMBERS {
            let quoted = format!("\"{member}\":");
            let at = cause.find(&quoted).unwrap_or_else(|| {
                panic!("case {case:?}: expected the cause to carry {quoted:?}, got {cause:?}")
            });
            assert!(
                at > previous,
                "case {case:?}: expected Execution's members in the order \
                 {EXECUTION_MEMBERS:?}, but {member:?} is written at {at} after {previous} in \
                 {cause:?}"
            );
            previous = at;
        }
        // And the state's own members, which are the attempt being run: its name, the
        // instant it was entered, and the attempt it is on — zero for a state no `Retry`
        // has re-run, PRESENT either way.
        let state_at = cause.find("\"State\":{").unwrap_or_else(|| {
            panic!("case {case:?}: expected the cause to carry the state's members, got {cause:?}")
        });
        let state = &cause[state_at..];
        let opening = format!("\"State\":{{\"Name\":\"{FAILING_STATE}\",\"EnteredTime\":\"");
        assert!(
            state.starts_with(&opening),
            "case {case:?}: expected the state's members to open {opening:?}, got {state:?}"
        );
        assert!(
            state.contains("\"RetryCount\":0"),
            "case {case:?}: expected the state to report the attempt it is on, got {state:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 6. A redrive that does not re-enter the state names the preserved entry
// ---------------------------------------------------------------------------

/// One row: the definition whose first state fails deterministically, and the input it runs
/// over.
struct RedriveRow {
    case: &'static str,
    definition: String,
    input: &'static str,
}

fn redrive_rows() -> Vec<RedriveRow> {
    // The failing state is named `Fan` in every row, whatever its type, so the suite reads
    // its entry out of the history the same way.
    let machine = |states: Value| {
        json!({ "QueryLanguage": "JSONata", "StartAt": "Fan", "States": states }).to_string()
    };
    vec![
        RedriveRow {
            // The recorded shape: a `Parallel` state whose `Arguments` throws, so no branch
            // is started.
            case: "a Parallel state's Arguments",
            definition: machine(json!({
                "Fan": {
                    "Type": "Parallel",
                    "Arguments": "{% $number('not-a-number') %}",
                    "Branches": [{
                        "StartAt": "B",
                        "States": { "B": { "Type": "Pass", "End": true } },
                    }],
                    "End": true,
                },
            })),
            input: "{}",
        },
        RedriveRow {
            case: "a Pass state's Output",
            definition: machine(json!({
                "Fan": {
                    "Type": "Pass",
                    "Output": { "n": "{% $states.input.s + 1 %}" },
                    "End": true,
                },
            })),
            input: r#"{"s":"x"}"#,
        },
        RedriveRow {
            case: "a Choice rule's Condition",
            definition: machine(json!({
                "Fan": {
                    "Type": "Choice",
                    "Choices": [{ "Condition": "{% $states.input.s + 1 %}", "Next": "Done" }],
                    "Default": "Done",
                },
                "Done": { "Type": "Pass", "End": true },
            })),
            input: r#"{"s":"x"}"#,
        },
        RedriveRow {
            // A state that is not the definition's first, so the entry the cause names is
            // not the second event of the history.
            case: "a state two transitions into the definition",
            definition: json!({
                "QueryLanguage": "JSONata",
                "StartAt": "First",
                "States": {
                    "First": { "Type": "Pass", "Next": "Fan" },
                    "Fan": {
                        "Type": "Pass",
                        "Output": { "n": "{% $states.input.s + 1 %}" },
                        "End": true,
                    },
                },
            })
            .to_string(),
            input: r#"{"s":"x"}"#,
        },
    ]
}

/// A redrive of a deterministic evaluation failure reproduces the whole cause, the entered
/// event id included: the step is re-run without being entered again, so the id the cause
/// names is the one the preserved history holds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_redriven_evaluation_failure_names_the_preserved_entry() {
    let rows = redrive_rows();
    let emulator = Emulator::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let machine =
            create_machine(&emulator, &format!("redrive-cause-{index}"), &row.definition).await;
        let execution = start_execution(&emulator, &machine, "only", row.input).await;
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(QUERY_EVALUATION_ERROR)),
            "case {case:?}: expected FAILED with {QUERY_EVALUATION_ERROR}, got {described:?}"
        );
        let events = history(&emulator, &execution, json!({})).await;
        let entered = entered_once(&events, case, "Fan");
        let cause = string(&described, "cause");
        assert!(
            cause.contains(&format!("(entered at the event id #{entered})")),
            "case {case:?}: expected the cause to name the entry #{entered} the history holds, \
             got {cause:?}"
        );

        emulator
            .ok("RedriveExecution", &json!({ "executionArn": execution }))
            .await;
        let redriven = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                redriven.get("status").and_then(Value::as_str),
                redriven.get("error").and_then(Value::as_str),
                string(&redriven, "cause"),
                redriven.get("redriveCount").and_then(Value::as_i64),
                redriven.get("redriveStatus").and_then(Value::as_str),
            ),
            (
                Some("FAILED"),
                Some(QUERY_EVALUATION_ERROR),
                cause.clone(),
                Some(1),
                Some("REDRIVABLE"),
            ),
            "case {case:?}: expected the re-run to fail with the same cause, got {redriven:?}"
        );
        let after = history(&emulator, &execution, json!({})).await;
        assert_eq!(
            entries_of(&after, "Fan"),
            vec![entered],
            "case {case:?}: expected the preserved entry to be the only one, got {after:?}"
        );
    }
}
