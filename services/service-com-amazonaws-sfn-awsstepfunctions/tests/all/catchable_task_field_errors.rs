// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHO answers a `Task` state's own FIELD that could not be read,
//! driven over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: a JSONata expression written in a field of a `Task` state that
//! misevaluates raises `States.QueryEvaluationError` as an ORDINARY error of that state
//! — the state's own `Catch` answers for it, the catcher's `Next` runs and the
//! execution goes on down that branch — and only an error no catcher of the state names
//! ends the execution, with the documented cause and the `EvaluationFailed` event that
//! locates the field.
//!
//! Rows vary the dimensions the rule generalises over: WHICH field of the state carries
//! the expression (`TimeoutSeconds`, `HeartbeatSeconds`), WHAT the expression produced
//! (a string, a boolean, nothing at all), WHICH work the state's `Resource` names (a
//! service-integration call and an activity a worker polls, the two kinds of task), and
//! WHAT the state declares about errors (a catcher naming the wildcard, one naming the
//! error itself, one naming another error, and no catcher at all). A state whose fields
//! evaluate is the last test, so the suite also pins that nothing here refuses a field
//! that READ.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`) is the one `state_machine_lifecycle` builds, the history reader is
//! `execution_history`'s and the activity helpers are `activity_tasks`';
//! `tests/all/main.rs` picks this file up, so each is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EVENT_EVALUATION_FAILED, STATE_ENTERED_SUFFIX,
};

use crate::activity_tasks::{claimed, create_activity};
use crate::execution_history::history;
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator,
};

/// The ASL error name an expression that could not be evaluated fails under
/// ("States.QueryEvaluationError — the JSONata expression evaluation failed").
const QUERY_EVALUATION_ERROR: &str = "States.QueryEvaluationError";

/// The wildcard a catcher names to answer for any error of its state.
const ALL_ERRORS: &str = "States.ALL";

/// The name of the `Task` state every row runs, and of the `Pass` state a catcher of it
/// routes to.
const TASK_STATE: &str = "T";
/// See [`TASK_STATE`].
const HANDLER_STATE: &str = "Handled";

/// What both timing fields of a `Task` state must carry, as the service words it: the
/// `must_be` clause of the diagnosis of an expression that produced something else.
const POSITIVE_SECONDS: &str = "a positive whole number of seconds";

/// A `Resource` naming a service-integration call, which is the kind of task whose work
/// this emulator performs itself.
const INTEGRATION_RESOURCE: &str = "arn:aws:states:::aws-sdk:sts:getCallerIdentity";

/// How often an execution is looked at again, and how long it is waited for at all.
///
/// The wait is for the CONDITION — the execution leaving `RUNNING` — so the deadline is
/// generous and a host that settles one quickly spends none of it: nothing here measures
/// how fast the host runs.
const POLL: Duration = Duration::from_millis(25);
/// See [`POLL`].
const DEADLINE: Duration = Duration::from_secs(120);

/// The record of `execution_arn` once it has left `RUNNING`, looked for again until it
/// has or [`DEADLINE`] passes.
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

/// The diagnosis `diagnosis` as the state `state`, entered at the event `entered`,
/// reports it: the ONE wrapper every failure the interpreter raises while executing a
/// state is reported under.
fn executing_state(state: &str, entered: i64, diagnosis: &str) -> String {
    format!(
        "An error occurred while executing the state '{state}' (entered at the event id \
         #{entered}). {diagnosis}"
    )
}

/// What the diagnosis of the expression `expression`, written in the field `field`,
/// says when it produced a value of a type that field cannot take: the expression, the
/// field, what the field must carry, and the type and value produced.
fn unexpected_type(expression: &str, field: &str, produced: &str, value: &str) -> String {
    format!(
        "The JSONata expression '{expression}' specified for the field '{field}' returned an \
         unexpected result type. Expected '{POSITIVE_SECONDS}', but was '{produced}' for value: \
         {value}"
    )
}

/// The output of a settled execution, as the document it is.
fn output_of(described: &Value) -> Value {
    described
        .get("output")
        .and_then(Value::as_str)
        .map(|text| serde_json::from_str(text).expect("the output is JSON"))
        .unwrap_or(Value::Null)
}

/// The id of the one event `events` holds the ENTRY of the state `name` at.
fn entered_at(events: &[Value], case: &str, name: &str) -> i64 {
    let entries: Vec<i64> = events
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
        .filter_map(|event| event.get("id").and_then(Value::as_i64))
        .collect();
    assert_eq!(
        entries.len(),
        1,
        "case {case:?}: expected {name:?} to be entered once, got {entries:?} in {events:?}"
    );
    entries[0]
}

/// Whether `events` holds the entry of the state `name`.
fn entered(events: &[Value], name: &str) -> bool {
    events.iter().any(|event| {
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
}

/// The one `EvaluationFailed` event of `events`, which reports the expression that could
/// not be evaluated.
fn evaluation_failed(events: &[Value], case: &str) -> Value {
    events
        .iter()
        .find(|event| event.get("type").and_then(Value::as_str) == Some(EVENT_EVALUATION_FAILED))
        .unwrap_or_else(|| {
            panic!("case {case:?}: expected an {EVENT_EVALUATION_FAILED} event in {events:?}")
        })
        .clone()
}

/// One catcher routing to [`HANDLER_STATE`] with the error name it took over as its
/// output: `$states.errorOutput` is in scope for a catcher's own `Output`, which is how
/// a definition reads what it caught.
fn catchers(errors: &[&str]) -> Value {
    json!([{
        "ErrorEquals": errors,
        "Output": { "caught": "{% $states.errorOutput.Error %}" },
        "Next": HANDLER_STATE,
    }])
}

/// The JSONata state machine a row runs: one `Task` named [`TASK_STATE`] whose
/// `Resource` names `resource` and which carries `members` — the field under test, and
/// the `Catch` where the row declares one — beside the `Pass` state its catchers route
/// to.
///
/// A row declaring no `Catch` declares that `Pass` state either: nothing would reach it,
/// which is a definition the service refuses.
fn machine(resource: &str, members: Value, catch: Option<Value>) -> String {
    let mut task = json!({
        "Type": "Task",
        "Resource": resource,
        "Arguments": {},
        "End": true,
    });
    let object = task.as_object_mut().expect("the state is an object");
    for (member, value) in members.as_object().expect("the members are an object") {
        object.insert(member.clone(), value.clone());
    }
    let catching = catch.is_some();
    if let Some(catch) = catch {
        object.insert("Catch".to_string(), catch);
    }
    let mut states = serde_json::Map::new();
    states.insert(TASK_STATE.to_string(), task);
    if catching {
        states.insert(
            HANDLER_STATE.to_string(),
            json!({ "Type": "Pass", "End": true }),
        );
    }
    json!({
        "QueryLanguage": "JSONata",
        "StartAt": TASK_STATE,
        "States": Value::Object(states),
    })
    .to_string()
}

/// What one row must end as.
enum Expect {
    /// A catcher of the state took the error over: the execution SUCCEEDED and its
    /// output is what that catcher's own `Output` built.
    Caught,
    /// No catcher of the state names the error: the execution FAILED under
    /// [`QUERY_EVALUATION_ERROR`], with this diagnosis and the field it locates.
    Failed {
        diagnosis: String,
        location: &'static str,
    },
}

/// One row: the `Task` state's own members, the input the execution runs over, what the
/// state declares about errors, and what the execution must end as.
struct Row {
    case: &'static str,
    /// Whether the state's `Resource` names an ACTIVITY — a task a worker polls for —
    /// rather than a service-integration call.
    activity: bool,
    members: Value,
    input: &'static str,
    /// The `ErrorEquals` of the state's one catcher, or `None` for a state declaring no
    /// `Catch` at all.
    catches: Option<&'static [&'static str]>,
    expect: Expect,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            // The recorded shape: a timing field whose expression produced a string.
            case: "TimeoutSeconds producing a string, with a wildcard catcher",
            activity: false,
            members: json!({ "TimeoutSeconds": "{% $states.input.t %}" }),
            input: r#"{"t":"10"}"#,
            catches: Some(&[ALL_ERRORS]),
            expect: Expect::Caught,
        },
        Row {
            case: "the same field on an activity task, with a wildcard catcher",
            activity: true,
            members: json!({ "TimeoutSeconds": "{% $states.input.t %}" }),
            input: r#"{"t":"10"}"#,
            catches: Some(&[ALL_ERRORS]),
            expect: Expect::Caught,
        },
        Row {
            case: "HeartbeatSeconds producing a boolean, caught by the error's own name",
            activity: false,
            members: json!({
                "TimeoutSeconds": 120,
                "HeartbeatSeconds": "{% $states.input.beat %}",
            }),
            input: r#"{"beat":true}"#,
            catches: Some(&[QUERY_EVALUATION_ERROR]),
            expect: Expect::Caught,
        },
        Row {
            // Varying what the expression produced: a reference to a member the input
            // does not carry produces NOTHING, which is no count of seconds either.
            case: "TimeoutSeconds producing nothing, with a wildcard catcher",
            activity: false,
            members: json!({ "TimeoutSeconds": "{% $states.input.missing %}" }),
            input: r#"{"t":"10"}"#,
            catches: Some(&[ALL_ERRORS]),
            expect: Expect::Caught,
        },
        Row {
            // A catcher that names ANOTHER error answers for nothing here, so the
            // execution ends on the failure itself.
            case: "TimeoutSeconds producing a string, with a catcher naming another error",
            activity: false,
            members: json!({ "TimeoutSeconds": "{% $states.input.t %}" }),
            input: r#"{"t":"10"}"#,
            catches: Some(&["SomethingElse"]),
            expect: Expect::Failed {
                diagnosis: unexpected_type(
                    "$states.input.t",
                    "TimeoutSeconds",
                    "string",
                    "\"10\"",
                ),
                location: "TimeoutSeconds",
            },
        },
        Row {
            case: "HeartbeatSeconds producing a string on an activity task, with no catcher",
            activity: true,
            members: json!({
                "TimeoutSeconds": 120,
                "HeartbeatSeconds": "{% $states.input.beat %}",
            }),
            input: r#"{"beat":"30"}"#,
            catches: None,
            expect: Expect::Failed {
                diagnosis: unexpected_type(
                    "$states.input.beat",
                    "HeartbeatSeconds",
                    "string",
                    "\"30\"",
                ),
                location: "HeartbeatSeconds",
            },
        },
    ]
}

/// A `Task` state's JSONata field that misevaluated is the state's own error: a catcher
/// of the state takes it over and the execution goes on down that branch, and only an
/// error no catcher names ends the execution.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_field_that_misevaluates_goes_through_the_states_catch() {
    let rows = rows();
    let emulator = Emulator::new();

    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let resource = match row.activity {
            true => create_activity(&emulator, &format!("field-{index}")).await,
            false => INTEGRATION_RESOURCE.to_string(),
        };
        let definition = machine(
            &resource,
            row.members.clone(),
            row.catches.map(catchers),
        );
        let machine = create_machine(&emulator, &format!("field-{index}"), &definition).await;
        started.push(start_execution(&emulator, &machine, "only", row.input).await);
    }

    for (row, execution) in rows.iter().zip(started) {
        let case = row.case;
        let described = settled(&emulator, case, &execution).await;
        let events = history(&emulator, &execution, json!({})).await;
        match &row.expect {
            Expect::Caught => {
                assert_eq!(
                    (
                        described.get("status").and_then(Value::as_str),
                        described.get("error").and_then(Value::as_str),
                    ),
                    (Some("SUCCEEDED"), None),
                    "case {case:?}: expected the catcher to carry the execution on, got \
                     {described:?}"
                );
                let produced = output_of(&described);
                assert_eq!(
                    produced,
                    json!({ "caught": QUERY_EVALUATION_ERROR }),
                    "case {case:?}: expected the error output the catcher built, got {produced}"
                );
                assert!(
                    entered(&events, HANDLER_STATE),
                    "case {case:?}: expected the handler the catcher names to have run, got \
                     {events:?}"
                );
            }
            Expect::Failed {
                diagnosis,
                location,
            } => {
                assert_eq!(
                    (
                        described.get("status").and_then(Value::as_str),
                        described.get("error").and_then(Value::as_str),
                    ),
                    (Some("FAILED"), Some(QUERY_EVALUATION_ERROR)),
                    "case {case:?}: expected FAILED with {QUERY_EVALUATION_ERROR}, got \
                     {described:?}"
                );
                let expected = executing_state(
                    TASK_STATE,
                    entered_at(&events, case, TASK_STATE),
                    diagnosis,
                );
                let cause = string(&described, "cause");
                assert_eq!(
                    cause, expected,
                    "case {case:?}: expected the cause {expected:?}, got {cause:?}"
                );
                // The history says the same thing, and locates the field the expression
                // is written in.
                let event = evaluation_failed(&events, case);
                let details = event
                    .get("evaluationFailedEventDetails")
                    .unwrap_or_else(|| panic!("case {case:?}: expected details on {event:?}"));
                assert_eq!(
                    (
                        details.get("error").and_then(Value::as_str),
                        details.get("cause").and_then(Value::as_str),
                        details.get("location").and_then(Value::as_str),
                    ),
                    (
                        Some(QUERY_EVALUATION_ERROR),
                        Some(expected.as_str()),
                        Some(*location),
                    ),
                    "case {case:?}: expected the event to report the same failure at \
                     {location:?}, got {event:?}"
                );
                assert!(
                    !entered(&events, HANDLER_STATE),
                    "case {case:?}: expected no catcher to have run, got {events:?}"
                );
            }
        }
    }
}

/// The control: a `Task` state whose timing fields READ is not refused — the task is
/// scheduled with the bounds those expressions produced and the execution waits for its
/// worker.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_task_whose_timing_fields_read_schedules_its_work() {
    let case = "TimeoutSeconds and HeartbeatSeconds producing whole seconds";
    let emulator = Emulator::new();
    let activity = create_activity(&emulator, "reads-its-fields").await;
    let definition = machine(
        &activity,
        json!({
            "TimeoutSeconds": "{% $states.input.timeout %}",
            "HeartbeatSeconds": "{% $states.input.beat %}",
        }),
        Some(catchers(&[ALL_ERRORS])),
    );
    let machine = create_machine(&emulator, "reads-its-fields", &definition).await;
    let execution = start_execution(
        &emulator,
        &machine,
        "only",
        r#"{"timeout":300,"beat":60}"#,
    )
    .await;

    // The task really was scheduled: a worker polling the activity is handed it.
    let (token, _input) = claimed(&emulator, case, &activity, Some("w-1")).await;
    assert!(
        !token.is_empty(),
        "case {case:?}: expected the task to have been handed out, got {token:?}"
    );

    // And the execution waits at the state rather than being carried down a catch
    // branch: nothing failed. The task is already out with the worker, so the walk has
    // suspended at it and nothing but that worker's answer can move it — there is no
    // later instant to wait for.
    let described = describe_execution(&emulator, &execution).await;
    assert_eq!(
        (
            described.get("status").and_then(Value::as_str),
            described.get("error").and_then(Value::as_str),
        ),
        (Some("RUNNING"), None),
        "case {case:?}: expected the execution to wait at its task, got {described:?}"
    );
}
