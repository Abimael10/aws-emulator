// SPDX-License-Identifier: CC0-1.0

//! What `DescribeExecution` REPORTS about a run whose `Task` state failed, driven over
//! both services' own wire protocols through the production `MultiServiceRouter` with
//! Step Functions and Lambda mounted on ONE engine, so the function really runs and the
//! outcome the report projects is the run's own.
//!
//! The rules under test:
//!
//! 1. the report is a projection of the TERMINAL state the definition reached, not of the
//!    first failure inside it: a task error the definition's `Catch` takes over is not the
//!    execution's outcome, so the run ends `SUCCEEDED` with `output`/`outputDetails` and
//!    no `error`/`cause`, and a task error nothing catches ends it `FAILED` carrying the
//!    name the WORK answered with — the function's own unhandled-error class — and that
//!    work's own error document as the cause. `redriveStatus`/`redriveStatusReason` follow
//!    from the same outcome: a `SUCCEEDED` run is `NOT_REDRIVABLE` for the documented
//!    reason, a `FAILED` one is `REDRIVABLE` with no reason;
//! 2. `includedData = METADATA_ONLY` reports that same metadata and NONE of the
//!    execution's data — `input`, `output`, `error`, `cause` and the detail structures
//!    about them — whatever the outcome was;
//! 3. a payload-template member whose JSONPath selects nothing fails the state with
//!    `States.Runtime`, and the cause is ONE sentence that names the state being executed
//!    and the id of the history event that state was entered at BEFORE the diagnosis, with
//!    the document quoted in the bytes the caller supplied it in.
//!
//! The rows differ along what each rule generalises over: for rule 1 and 2, which error
//! class the handler raised and what the definition says about it (nothing, a catcher
//! naming that very class, a catcher naming the other class the handler can raise, the
//! task-failure wildcard, the catch-all, and a catcher naming a class the run never
//! raises); for rule 3, the field, the form of the path, the bytes of the input and
//! WHERE in the definition the failing state sits, so the event id the cause names is
//! not one definition's.
//!
//! The Lambda harness (`Chain`) is `lambda_task_control_flow`'s and the single-service
//! one (`Emulator`, `create_machine`, `start_execution`) is `state_machine_lifecycle`'s;
//! `tests/all/main.rs` is this binary's crate root, so each is reached through
//! `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::RUNTIME_ERROR;

use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::{
    create_machine, discriminated, start_execution, string, Emulator,
};

/// The handler every row of rules 1 and 2 invokes: it raises, and WHICH class it raises
/// is decided by the event, so one deployment package serves every row and the error name
/// a row expects is the one the caller's own input asked for.
const HANDLER_SOURCE: &str = r#"
class OrderRejected(Exception):
    pass


def handler(event, context):
    order = event.get("orderId")
    if event.get("kind") == "rejected":
        raise OrderRejected("order %s was rejected" % order)
    raise RuntimeError("order %s cannot be processed" % order)
"#;

/// The class the handler raises for an event asking for neither kind: a class of the
/// language's own, which is the `errorType` of the document the run produces.
const RUNTIME_CLASS: &str = "RuntimeError";

/// The class it raises for the other kind: one the handler's own module declares.
const REJECTED_CLASS: &str = "OrderRejected";

/// The ASL wildcard that answers for any failed task ("States.TaskFailed — a Task state
/// failed because of an error in the state's work"), which a catcher may name instead of
/// the work's own error class.
const TASK_FAILED: &str = "States.TaskFailed";

/// The ASL wildcard that answers for every error name.
const ALL_ERRORS: &str = "States.ALL";

/// The reason `redriveStatusReason` carries for a run that finished successfully, from
/// the list the member's own documentation gives (`smithy_query(query="doc",
/// shape="DescribeExecutionOutput", member="redriveStatusReason")`).
const SUCCEEDED_NOT_REDRIVABLE: &str = "Execution is SUCCEEDED and cannot be redriven";

/// The shape `inputDetails`/`outputDetails` report, whose discriminator a nested
/// structure of this API carries.
const DATA_DETAILS_SHAPE: &str = "CloudWatchEventsExecutionDataDetails";

/// The members that carry the execution's own DATA, which a `METADATA_ONLY` report sends
/// none of.
const DATA_MEMBERS: &[&str] = &[
    "input",
    "inputDetails",
    "output",
    "outputDetails",
    "error",
    "cause",
];

/// Long enough for a run whose one task invokes a function to reach a terminal status,
/// whatever else the host is running. Nothing here measures how long anything took: the
/// deadline only bounds the wait.
const PATIENCE: Duration = Duration::from_secs(90);

/// How long one look at a status the clock owes waits before the next.
const POLL_INTERVAL: Duration = Duration::from_millis(25);

/// What a row's definition says about the task's failure.
enum Handled {
    /// Nothing takes the error over, so it is the execution's own outcome.
    Nothing,
    /// A catcher naming these error names takes it over, if one of them matches.
    Catcher(&'static [&'static str]),
}

/// The state the recovery of a caught task routes to, whose output is the error output
/// the catcher wrote — so a `SUCCEEDED` row's `output` shows WHICH error was handled.
const RECOVERED: &str = "Recovered";

/// The one `Task` state every row of rules 1 and 2 runs, with the row's own handling of
/// its failure.
fn definition(function_arn: &str, handled: &Handled) -> Value {
    let mut task = json!({
        "Type": "Task",
        "Resource": function_arn,
        "End": true,
    });
    let mut states = json!({});
    if let Handled::Catcher(errors) = handled {
        task["Catch"] = json!([{ "ErrorEquals": errors, "Next": RECOVERED }]);
        states[RECOVERED] = json!({ "Type": "Pass", "End": true });
    }
    states["Invoke"] = task;
    json!({ "StartAt": "Invoke", "States": states })
}

/// The report of a run is the projection of the terminal state its definition reached,
/// and a `METADATA_ONLY` report of the same run carries that metadata with none of its
/// data.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_report_projects_the_terminal_outcome_the_definition_reached() {
    let chain = Chain::new();
    let function_arn = chain
        .given_function_of("outcome-projection", HANDLER_SOURCE, None)
        .await;

    // (case, the kind the event asks for, the class that raises, how the definition
    // handles it, the terminal status the run reaches)
    let rows: Vec<(&str, &str, &str, Handled, &str)> = vec![
        (
            "nothing handles the class the handler raised",
            "runtime",
            RUNTIME_CLASS,
            Handled::Nothing,
            "FAILED",
        ),
        (
            "a catcher naming the function's own error class",
            "runtime",
            RUNTIME_CLASS,
            Handled::Catcher(&[RUNTIME_CLASS]),
            "SUCCEEDED",
        ),
        (
            "a catcher naming the other class the handler can raise",
            "rejected",
            REJECTED_CLASS,
            Handled::Catcher(&[REJECTED_CLASS]),
            "SUCCEEDED",
        ),
        (
            "a catcher naming the task-failure wildcard",
            "rejected",
            REJECTED_CLASS,
            Handled::Catcher(&[TASK_FAILED]),
            "SUCCEEDED",
        ),
        (
            "a catcher naming every error",
            "runtime",
            RUNTIME_CLASS,
            Handled::Catcher(&[ALL_ERRORS]),
            "SUCCEEDED",
        ),
        (
            "a catcher naming a class this run never raises",
            "rejected",
            REJECTED_CLASS,
            Handled::Catcher(&["OtherTeam.Rejected"]),
            "FAILED",
        ),
    ];

    for (index, (case, kind, raised, handled, status)) in rows.into_iter().enumerate() {
        let order = format!("o-{index}");
        let input = json!({ "kind": kind, "orderId": order });
        let described = chain
            .ran(
                case,
                &format!("outcome-{index}"),
                &definition(&function_arn, &handled),
                &input,
            )
            .await;
        let read = |name: &str| {
            described
                .get(name)
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        assert_eq!(
            read("status").as_deref(),
            Some(status),
            "case {case:?}: expected the run to end {status}, got {described}"
        );

        // The execution's own input is reported whatever the outcome was.
        assert_eq!(
            read("input")
                .map(|text| serde_json::from_str::<Value>(&text).expect("the input is JSON")),
            Some(input.clone()),
            "case {case:?}: expected the started input, got {described}"
        );

        if status == "FAILED" {
            // The name is the one the WORK answered with — the function's unhandled-error
            // class — and the cause is that work's own error document, carrying the
            // message the handler built from the caller's input.
            assert_eq!(
                read("error").as_deref(),
                Some(raised),
                "case {case:?}: expected the class the function raised, got {described}"
            );
            let cause = read("cause")
                .unwrap_or_else(|| panic!("case {case:?}: expected a cause in {described}"));
            let document: Value = serde_json::from_str(&cause).unwrap_or_else(|error| {
                panic!("case {case:?}: expected the error document, got {cause:?}: {error}")
            });
            let message = match raised {
                REJECTED_CLASS => format!("order {order} was rejected"),
                _ => format!("order {order} cannot be processed"),
            };
            assert_eq!(
                (
                    document.get("errorType").and_then(Value::as_str),
                    document.get("errorMessage").and_then(Value::as_str),
                ),
                (Some(raised), Some(message.as_str())),
                "case {case:?}: expected the raised class and message, got {document}"
            );
            // "This field is set only if the execution succeeds. If the execution fails,
            // this field is null" — so neither the output nor a detail structure about it.
            assert_eq!(
                (described.get("output"), described.get("outputDetails")),
                (None, None),
                "case {case:?}: expected no output on a failed run, got {described}"
            );
            // A failed run is one `RedriveExecution` would continue.
            assert_eq!(
                (read("redriveStatus").as_deref(), described.get("redriveStatusReason")),
                (Some("REDRIVABLE"), None),
                "case {case:?}: expected a redrivable run with no reason, got {described}"
            );
        } else {
            // The catcher took the error over, so the error is the handled state's
            // OUTPUT and not the execution's outcome.
            assert_eq!(
                (described.get("error"), described.get("cause")),
                (None, None),
                "case {case:?}: expected no failure members on a run that recovered, got \
                 {described}"
            );
            let output = read("output")
                .unwrap_or_else(|| panic!("case {case:?}: expected an output in {described}"));
            let handled_error: Value = serde_json::from_str(&output).unwrap_or_else(|error| {
                panic!("case {case:?}: expected a JSON output, got {output:?}: {error}")
            });
            assert_eq!(
                handled_error.get("Error").and_then(Value::as_str),
                Some(raised),
                "case {case:?}: expected the caught error in the output, got {handled_error}"
            );
            assert_eq!(
                described.get("outputDetails"),
                Some(&discriminated(DATA_DETAILS_SHAPE, json!({ "included": true }))),
                "case {case:?}: expected the output's detail structure, got {described}"
            );
            // "redriveStatus is NOT_REDRIVABLE if calling the RedriveExecution API action
            // would return the ExecutionNotRedrivable error", and a finished run cannot be
            // redriven at all.
            assert_eq!(
                (
                    read("redriveStatus").as_deref(),
                    read("redriveStatusReason").as_deref(),
                ),
                (Some("NOT_REDRIVABLE"), Some(SUCCEEDED_NOT_REDRIVABLE)),
                "case {case:?}: expected the documented reason, got {described}"
            );
        }

        // Rule 2: the same run, read with the data left out. The metadata is the same and
        // none of the data members is sent, whichever outcome the run reached.
        let execution_arn = string(&described, "executionArn");
        let metadata = chain
            .sfn(
                case,
                "DescribeExecution",
                &json!({ "executionArn": execution_arn, "includedData": "METADATA_ONLY" }),
            )
            .await;
        assert_eq!(
            (
                metadata.get("status"),
                metadata.get("redriveStatus"),
                metadata.get("redriveStatusReason"),
            ),
            (
                described.get("status"),
                described.get("redriveStatus"),
                described.get("redriveStatusReason"),
            ),
            "case {case:?}: expected the same metadata, got {metadata} beside {described}"
        );
        let present: Vec<&&str> = DATA_MEMBERS
            .iter()
            .filter(|member| metadata.get(**member).is_some())
            .collect();
        assert!(
            present.is_empty(),
            "case {case:?}: expected no execution data in a METADATA_ONLY report, got \
             {present:?} in {metadata}"
        );
    }
}

/// The execution `execution_arn` describes as once it has left `RUNNING`, looked at again
/// until it has or [`PATIENCE`] has passed.
async fn settled(emulator: &Emulator, case: &str, execution_arn: &str) -> Value {
    let began = Instant::now();
    loop {
        let described = emulator
            .ok(
                "DescribeExecution",
                &json!({ "executionArn": execution_arn }),
            )
            .await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            began.elapsed() < PATIENCE,
            "case {case:?}: expected a terminal status within {PATIENCE:?}, last saw {described}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// The state whose entering event the cause names, in a definition where another state
/// runs before it.
const AFTER: &str = "FirstLook";

/// The cause of a template member whose path selected nothing names the state being
/// executed and the id of the event that state was entered at, then the diagnosis, and
/// quotes the document in the bytes the caller supplied.
#[tokio::test]
async fn an_unresolvable_template_path_names_the_state_the_event_id_and_the_given_bytes() {
    let emulator = Emulator::new();

    // (case, the failing state's name, whether a state runs before it, the field, the
    // path, the input AS THE CALLER SENDS IT)
    let rows: Vec<(&str, &str, bool, &str, &str, &str)> = vec![
        (
            "the first state's Parameters, a member the input does not carry",
            "InvokeProcessingLambda",
            false,
            "bucket.$",
            "$.targetBucket",
            r#"{"orderId": "o-1"}"#,
        ),
        (
            "a nested path the input stops short of",
            "ResolveTarget",
            false,
            "city.$",
            "$.address.city",
            r#"{"address": {"street": "one"}}"#,
        ),
        (
            "a bracketed member, with an input written compactly",
            "PickValue",
            false,
            "picked.$",
            "$['missing']",
            r#"{"present":true}"#,
        ),
        (
            "a state another state ran before, so another entering event",
            AFTER,
            true,
            "who.$",
            "$.name",
            r#"{"orderId": "o-2", "items": [1, 2]}"#,
        ),
    ];

    for (index, (case, state, preceded, field, path, input)) in rows.into_iter().enumerate() {
        let failing = json!({
            "Type": "Pass",
            "Parameters": { field: path },
            "End": true,
        });
        let definition = if preceded {
            json!({
                "StartAt": "CarryThrough",
                "States": {
                    "CarryThrough": { "Type": "Pass", "Next": state },
                    state: failing,
                },
            })
        } else {
            json!({ "StartAt": state, "States": { state: failing } })
        };
        let machine = create_machine(
            &emulator,
            &format!("entered-at-{index}"),
            &definition.to_string(),
        )
        .await;
        let execution = start_execution(&emulator, &machine, "only", input).await;
        let described = settled(&emulator, case, &execution).await;

        // The id the cause names is the id of the event that ENTERED the failing state,
        // which the execution's own history carries: a run that passed through another
        // state first entered this one at a later event.
        let history = emulator
            .ok(
                "GetExecutionHistory",
                &json!({ "executionArn": execution }),
            )
            .await;
        let events = history
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected events in {history}"));
        let entered = events
            .iter()
            .find(|event| {
                event
                    .get("stateEnteredEventDetails")
                    .and_then(|details| details.get("name"))
                    .and_then(Value::as_str)
                    == Some(state)
            })
            .and_then(|event| event.get("id").and_then(Value::as_i64))
            .unwrap_or_else(|| panic!("case {case:?}: expected {state} to be entered in {events:?}"));
        // `ExecutionStarted` is event one, and a `Pass` that ran before the failing state
        // wrote its own entered and exited events before it.
        let expected_entered = if preceded { 4 } else { 2 };
        assert_eq!(
            entered, expected_entered,
            "case {case:?}: expected {state} to be entered at event {expected_entered}, got \
             {entered} in {events:?}"
        );

        let expected = format!(
            "An error occurred while executing the state '{state}' (entered at the event id \
             #{entered}). The JSONPath '{path}' specified for the field '{field}' could not be \
             found in the input '{input}'"
        );
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
                described.get("cause").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RUNTIME_ERROR), Some(expected.as_str())),
            "case {case:?}: expected {RUNTIME_ERROR} with {expected:?}, got {described}"
        );
    }
}
