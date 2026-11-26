// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the three ways a `Choice` comparator's `*Path` OPERAND can fail
//! to be an operand, driven over this service's own wire protocol (`awsJson1_0`, `POST /`
//! with `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: a comparator written in its `*Path` form READS its operand out of
//! the execution's data, so a path that selects no operand that comparator can read leaves
//! the rule undecidable and fails the execution with `States.Runtime` — and the cause says
//! WHICH of the three authoring mistakes was made, because each is a different thing to
//! correct:
//!
//! * the path selected NOTHING: the condition references a value that is not there;
//! * the path selected an explicit NULL: a value is there and nothing can be compared
//!   with it;
//! * the path selected a value of a type the comparator cannot read as an operand at all
//!   — the `Timestamp*` families compare the instants their operands denote, and an
//!   instant is written as a string: the VALUE at the path is invalid.
//!
//! The three sentences are never byte-identical, and each is wrapped in the one preamble
//! every diagnosis raised while executing a state carries: the state's name and the id of
//! the event it was entered at.
//!
//! A value the comparator CAN read but of a type its family merely does not compare is no
//! failure at all: ASL's typed comparators never coerce, so the rule does not match and
//! the state's `Default` is taken. Those rows are the control.
//!
//! The rows differ along what the rule generalises over: the comparator family (`String*`,
//! `Numeric*`, `Boolean*`, `Timestamp*`), the comparison inside it (equality and each
//! ordering), what the operand path resolved to (absent, null, a number, a boolean, an
//! array, an object, a string), and where the rule sits (a state's own `Choices` and a
//! condition nested in `And`/`Not`).
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`) is the one `state_machine_lifecycle` builds; `tests/all/main.rs` picks this
//! file up, so each is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::RUNTIME_ERROR;

use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator,
};

/// How long a row waits for an execution to reach a terminal status: generous, because
/// what it waits for is the emulator's own settle delay and not the host's speed.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// The state every definition here routes through, which the cause names.
const STATE: &str = "Route";

/// The id of the event that state is entered at: `ExecutionStarted` is the first event of
/// every history and the `Choice` is the first state, so its entered event is the second.
const ENTERED_AT: i64 = 2;

/// Poll `execution` until it reports a status other than `RUNNING`, and answer that
/// record. Panics naming `case` when [`TERMINAL_DEADLINE`] passes first.
async fn settled(emulator: &Emulator, case: &str, execution: &str) -> Value {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let described = describe_execution(emulator, execution).await;
        if string(&described, "status") != "RUNNING" {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to reach a terminal status, got {described:?}"
        );
        ::tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// A definition whose `Choice` state tests `rules` and sends every other outcome to a
/// `Pass` that puts out the text `"default"`.
fn choosing(rules: Value) -> String {
    json!({
        "StartAt": STATE,
        "States": {
            STATE: { "Type": "Choice", "Choices": rules, "Default": "Else" },
            "Then": { "Type": "Pass", "Result": "rule", "End": true },
            "Else": { "Type": "Pass", "Result": "default", "End": true },
        },
    })
    .to_string()
}

/// The preamble every diagnosis raised while executing a state carries, with `diagnosis`
/// after it.
fn executing_state(diagnosis: &str) -> String {
    format!(
        "An error occurred while executing the state '{STATE}' (entered at the event id \
         #{ENTERED_AT}). {diagnosis}"
    )
}

/// One row of the failure table: the rule, the input it is evaluated over, and the whole
/// cause the execution must carry.
struct FailingRow {
    case: &'static str,
    rules: Value,
    input: Value,
    cause: String,
}

/// An operand path that selects no operand the comparator can read fails the execution,
/// and the cause states which of the three classes it was.
#[tokio::test]
async fn an_unreadable_comparator_operand_names_its_own_failure_class() {
    // The three sentences, each written once: the row that uses one names the path it is
    // reported for.
    let unresolved = |path: &str| {
        format!("Invalid path '{path}': The choice state's condition path references an invalid value.")
    };
    let null_value =
        |path: &str| format!("Null value provided at path '{path}': Unable to compare with a null input.");
    let invalid_value = |path: &str| format!("Invalid value provided at path '{path}'");

    let rows: Vec<FailingRow> = vec![
        // A path that selects nothing, per family.
        FailingRow {
            case: "a StringEqualsPath operand the input carries no member for",
            rules: json!([{ "Variable": "$.a", "StringEqualsPath": "$.b", "Next": "Then" }]),
            input: json!({ "a": "x" }),
            cause: executing_state(&unresolved("$.b")),
        },
        FailingRow {
            case: "a NumericLessThanPath operand the input stops short of",
            rules: json!([{ "Variable": "$.a", "NumericLessThanPath": "$.limits.max", "Next": "Then" }]),
            input: json!({ "a": 1, "limits": {} }),
            cause: executing_state(&unresolved("$.limits.max")),
        },
        FailingRow {
            case: "a TimestampGreaterThanEqualsPath operand nothing resolves",
            rules: json!([{ "Variable": "$.when", "TimestampGreaterThanEqualsPath": "$.since", "Next": "Then" }]),
            input: json!({ "when": "2026-01-01T00:00:00Z" }),
            cause: executing_state(&unresolved("$.since")),
        },
        // An explicit null, which every family refuses as an operand.
        FailingRow {
            case: "a StringEqualsPath operand resolving to null",
            rules: json!([{ "Variable": "$.a", "StringEqualsPath": "$.b", "Next": "Then" }]),
            input: json!({ "a": "x", "b": null }),
            cause: executing_state(&null_value("$.b")),
        },
        FailingRow {
            case: "a NumericGreaterThanPath operand resolving to null",
            rules: json!([{ "Variable": "$.a", "NumericGreaterThanPath": "$.b", "Next": "Then" }]),
            input: json!({ "a": 7, "b": null }),
            cause: executing_state(&null_value("$.b")),
        },
        FailingRow {
            case: "a BooleanEqualsPath operand resolving to null",
            rules: json!([{ "Variable": "$.a", "BooleanEqualsPath": "$.b", "Next": "Then" }]),
            input: json!({ "a": true, "b": null }),
            cause: executing_state(&null_value("$.b")),
        },
        FailingRow {
            case: "a TimestampLessThanPath operand resolving to null",
            rules: json!([{ "Variable": "$.when", "TimestampLessThanPath": "$.b", "Next": "Then" }]),
            input: json!({ "when": "2026-01-01T00:00:00Z", "b": null }),
            cause: executing_state(&null_value("$.b")),
        },
        FailingRow {
            case: "a null operand of a condition nested in an And",
            rules: json!([{
                "And": [
                    { "Variable": "$.a", "StringEquals": "x" },
                    { "Variable": "$.a", "StringEqualsPath": "$.b" },
                ],
                "Next": "Then",
            }]),
            input: json!({ "a": "x", "b": null }),
            cause: executing_state(&null_value("$.b")),
        },
        FailingRow {
            case: "a null operand of a condition nested in a Not",
            rules: json!([{
                "Not": { "Variable": "$.a", "StringEqualsPath": "$.b" },
                "Next": "Then",
            }]),
            input: json!({ "a": "x", "b": null }),
            cause: executing_state(&null_value("$.b")),
        },
        // A value of a type the comparator cannot read as an operand: an instant is
        // written as a string, so the timestamp families read no operand out of a value
        // of another type.
        FailingRow {
            case: "a TimestampGreaterThanPath operand resolving to a number",
            rules: json!([{ "Variable": "$$.State.EnteredTime", "TimestampGreaterThanPath": "$.threshold", "Next": "Then" }]),
            input: json!({ "threshold": 42 }),
            cause: executing_state(&invalid_value("$.threshold")),
        },
        FailingRow {
            case: "a TimestampEqualsPath operand resolving to a boolean",
            rules: json!([{ "Variable": "$.when", "TimestampEqualsPath": "$.flag", "Next": "Then" }]),
            input: json!({ "when": "2026-01-01T00:00:00Z", "flag": true }),
            cause: executing_state(&invalid_value("$.flag")),
        },
        FailingRow {
            case: "a TimestampLessThanEqualsPath operand resolving to an array",
            rules: json!([{ "Variable": "$.when", "TimestampLessThanEqualsPath": "$.list", "Next": "Then" }]),
            input: json!({ "when": "2026-01-01T00:00:00Z", "list": [1, 2] }),
            cause: executing_state(&invalid_value("$.list")),
        },
        FailingRow {
            case: "a TimestampGreaterThanPath operand resolving to an object",
            rules: json!([{ "Variable": "$.when", "TimestampGreaterThanPath": "$.object", "Next": "Then" }]),
            input: json!({ "when": "2026-01-01T00:00:00Z", "object": { "at": 1 } }),
            cause: executing_state(&invalid_value("$.object")),
        },
        FailingRow {
            case: "a wrongly typed operand of a condition nested in a Not",
            rules: json!([{
                "Not": { "Variable": "$.when", "TimestampGreaterThanPath": "$.threshold" },
                "Next": "Then",
            }]),
            input: json!({ "when": "2026-01-01T00:00:00Z", "threshold": 42 }),
            cause: executing_state(&invalid_value("$.threshold")),
        },
    ];

    let emulator = Emulator::new();
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let machine = create_machine(
            &emulator,
            &format!("operand-class-{index}"),
            &choosing(row.rules.clone()),
        )
        .await;
        started.push(start_execution(&emulator, &machine, "only", &row.input.to_string()).await);
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
        assert_eq!(
            cause, row.cause,
            "case {case:?}: expected the cause {:?}, got {cause:?}",
            row.cause
        );
    }
}

/// The three classes are told apart from each other: the same rule over an absent member,
/// over a null one and over a wrongly typed one carries three different causes.
#[tokio::test]
async fn the_null_the_absent_and_the_wrongly_typed_operand_differ() {
    let emulator = Emulator::new();
    // One rule, three inputs — so nothing but what the path resolved to differs.
    let rules = json!([{
        "Variable": "$.when",
        "TimestampGreaterThanPath": "$.threshold",
        "Next": "Then",
    }]);
    // (case, the input)
    let inputs: [(&str, Value); 3] = [
        ("the member is absent", json!({ "when": "2026-01-01T00:00:00Z" })),
        (
            "the member is null",
            json!({ "when": "2026-01-01T00:00:00Z", "threshold": null }),
        ),
        (
            "the member is a number",
            json!({ "when": "2026-01-01T00:00:00Z", "threshold": 42 }),
        ),
    ];

    let mut started = Vec::with_capacity(inputs.len());
    for (index, (_, input)) in inputs.iter().enumerate() {
        let machine = create_machine(
            &emulator,
            &format!("operand-distinct-{index}"),
            &choosing(rules.clone()),
        )
        .await;
        started.push(start_execution(&emulator, &machine, "only", &input.to_string()).await);
    }
    let mut causes: Vec<(&str, String)> = Vec::with_capacity(inputs.len());
    for ((case, _), execution) in inputs.iter().zip(started) {
        let described = settled(&emulator, case, &execution).await;
        causes.push((case, string(&described, "cause")));
    }
    for (left, right) in [(0, 1), (0, 2), (1, 2)] {
        let (first, second) = (&causes[left], &causes[right]);
        assert_ne!(
            first.1, second.1,
            "case {:?} against {:?}: the two classes must not be worded the same way, both \
             answered {:?}",
            first.0, second.0, first.1
        );
    }
}

/// The control: a value the comparator CAN read but of a type its family does not compare
/// is not a failure — the rule does not match, the `Default` is taken, and the execution
/// succeeds.
#[tokio::test]
async fn an_operand_of_a_type_the_family_does_not_compare_falls_through() {
    // (case, the rule, the input)
    let rows: Vec<(&str, Value, Value)> = vec![
        (
            "a StringEqualsPath operand resolving to a number",
            json!([{ "Variable": "$.a", "StringEqualsPath": "$.b", "Next": "Then" }]),
            json!({ "a": "1", "b": 1 }),
        ),
        (
            "a NumericEqualsPath operand resolving to a string",
            json!([{ "Variable": "$.a", "NumericEqualsPath": "$.b", "Next": "Then" }]),
            json!({ "a": 1, "b": "1" }),
        ),
        (
            "a BooleanEqualsPath operand resolving to a string",
            json!([{ "Variable": "$.a", "BooleanEqualsPath": "$.b", "Next": "Then" }]),
            json!({ "a": true, "b": "true" }),
        ),
        (
            "a TimestampEqualsPath operand resolving to a string that denotes no instant",
            json!([{ "Variable": "$.when", "TimestampEqualsPath": "$.b", "Next": "Then" }]),
            json!({ "when": "2026-01-01T00:00:00Z", "b": "yesterday" }),
        ),
        (
            "an operand of the right type, which the rule decides on",
            json!([{ "Variable": "$.a", "StringEqualsPath": "$.b", "Next": "Then" }]),
            json!({ "a": "x", "b": "y" }),
        ),
    ];

    let emulator = Emulator::new();
    let mut started = Vec::with_capacity(rows.len());
    for (index, (_, rules, input)) in rows.iter().enumerate() {
        let machine = create_machine(
            &emulator,
            &format!("operand-fallthrough-{index}"),
            &choosing(rules.clone()),
        )
        .await;
        started.push(start_execution(&emulator, &machine, "only", &input.to_string()).await);
    }
    for ((case, _, _), execution) in rows.iter().zip(started) {
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("output").and_then(Value::as_str),
            ),
            (Some("SUCCEEDED"), Some("\"default\"")),
            "case {case:?}: expected the Default to be taken, got {described:?}"
        );
    }
}
