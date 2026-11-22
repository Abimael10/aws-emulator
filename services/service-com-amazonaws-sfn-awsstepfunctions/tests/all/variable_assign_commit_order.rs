// SPDX-License-Identifier: CC0-1.0

//! When what a state ASSIGNS becomes readable, driven end to end over this service's own
//! wire protocol (`awsJson1_0`, `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`)
//! through the production `MultiServiceRouter`.
//!
//! The rule under test: a node's `Assign` COMMITS for the nodes that come after it, so the
//! node's own fields are evaluated against the variables the run held when the node was
//! reached and the next state is the first to read what that `Assign` wrote. A node that
//! both reads and writes one variable therefore puts out the value it was reached with,
//! and a node whose own output reads a variable only its own `Assign` writes reads nothing
//! at all.
//!
//! The rows span what the rule generalises over: the query language the node is written in
//! (both of the two `QueryLanguage` selects between, which spell a variable read
//! differently and evaluate it through different machinery) and the NODE that assigns (a
//! state's own `Assign` and a `Catch` catcher's own) — and, for the one language whose
//! output is an expression, the scope that holds nothing for the name at all.
//!
//! A `Choice` is the one node this does NOT hold for, and it is covered here too: a
//! `Choice` assigns as part of the TRANSITION its selected rule decides, so what that rule
//! assigned is in scope for the output of that transition — the reading
//! `interpreter_data_fidelity` pins for the rule's own `Output`, asserted here for both
//! query languages so the two cannot drift.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`) is
//! the one `state_machine_lifecycle` builds; `tests/all/main.rs` picks this file up, so
//! each is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, Emulator,
};

/// The ASL error name an expression that could not be evaluated fails under.
const QUERY_EVALUATION_ERROR: &str = "States.QueryEvaluationError";

/// How often an execution is looked at again, and how long it is waited for at all.
///
/// The wait is for the CONDITION — the execution leaving `RUNNING` — so the deadline is
/// generous and a host that settles an execution quickly spends none of it: nothing here
/// measures how fast the host runs.
const SETTLE_POLL: Duration = Duration::from_millis(25);
/// See [`SETTLE_POLL`].
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// The record of `execution_arn` once it has left `RUNNING`, looked for again until it has
/// or [`SETTLE_DEADLINE`] passes.
async fn settled(emulator: &Emulator, case: &str, execution_arn: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
    loop {
        let described = describe_execution(emulator, execution_arn).await;
        if described.get("status").and_then(Value::as_str) != Some("RUNNING") {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to settle, got {described:?}"
        );
        tokio::time::sleep(SETTLE_POLL).await;
    }
}

/// What one row must end as.
enum Expected {
    /// The run completed, carrying this output: `seen` is what the ASSIGNING node put
    /// out, and `nextSeen` what the state after it read.
    Output(Value),
    /// The assigning node's own output read a name nothing in scope held, which fails the
    /// execution: the expression the failure must name.
    Nothing(&'static str),
}

/// One row: a definition, the input it runs over, and what it must end as.
struct Row {
    case: &'static str,
    definition: Value,
    input: &'static str,
    expected: Expected,
}

/// A JSONata definition that starts at `Seed`, which assigns `x` the value `1`, and then
/// hands over to the node `states` declares.
fn jsonata(states: Value) -> Value {
    let mut declared = json!({
        "Seed": { "Type": "Pass", "Assign": { "x": 1 }, "Next": "Mutate" },
        "Read": {
            "Type": "Pass",
            "Output": {
                "seen": "{% $states.input.seen %}",
                "nextSeen": "{% $x %}",
            },
            "End": true,
        },
    });
    for (name, state) in states.as_object().expect("the states are an object") {
        declared[name] = state.clone();
    }
    json!({ "QueryLanguage": "JSONata", "StartAt": "Seed", "States": declared })
}

/// A `Map` state whose one item fails, for a row about a `Catch` catcher's own `Assign`.
fn failing_map(catcher: Value) -> Value {
    json!({
        "Type": "Map",
        "Items": [1],
        "ItemProcessor": {
            "StartAt": "F",
            "States": { "F": { "Type": "Fail", "Error": "ItemBroke", "Cause": "boom" } },
        },
        "Catch": [catcher],
        "End": true,
    })
}

/// A node's `Assign` commits for the nodes after it: its own output reads the value the
/// run was already holding, and the next state reads the value it assigned.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_node_assigns_is_read_by_the_nodes_after_it() {
    let rows: Vec<Row> = vec![
        Row {
            // The recorded shape: one state both reads `$x` in its `Output` and sets it
            // in its own `Assign`.
            case: "a JSONata state's own Assign and its own Output",
            definition: jsonata(json!({
                "Mutate": {
                    "Type": "Pass",
                    "Assign": { "x": 2 },
                    "Output": { "seen": "{% $x %}" },
                    "Next": "Read",
                },
            })),
            input: "{}",
            expected: Expected::Output(json!({ "seen": 1, "nextSeen": 2 })),
        },
        Row {
            case: "a Catch catcher's own Assign and Output",
            definition: jsonata(json!({
                "Mutate": failing_map(json!({
                    "ErrorEquals": ["States.ALL"],
                    "Assign": { "x": 2 },
                    "Output": { "seen": "{% $x %}" },
                    "Next": "Read",
                })),
            })),
            input: "{}",
            expected: Expected::Output(json!({ "seen": 1, "nextSeen": 2 })),
        },
        Row {
            // The same rule in the other query language, where a variable is read as the
            // first term of a path: `OutputPath` is the field of a JSONPath state that is
            // evaluated after its assignments, and the state after it reads the path
            // `$x` in a payload template.
            case: "a JSONPath state's own Assign and its own OutputPath",
            definition: json!({
                "StartAt": "Seed",
                "States": {
                    "Seed": { "Type": "Pass", "Assign": { "x": 1 }, "Next": "Mutate" },
                    "Mutate": {
                        "Type": "Pass",
                        "Assign": { "x": 2 },
                        "OutputPath": "$x",
                        "Next": "Read",
                    },
                    "Read": {
                        "Type": "Pass",
                        "Parameters": { "seen.$": "$", "nextSeen.$": "$x" },
                        "End": true,
                    },
                },
            }),
            input: "{}",
            expected: Expected::Output(json!({ "seen": 1, "nextSeen": 2 })),
        },
        Row {
            // The consequence of the same rule where NOTHING assigned the name before
            // the node that reads it: the node's own `Assign` is not in scope for its own
            // `Output`, so the expression produces nothing and the state fails.
            case: "a JSONata state whose Output reads a name only its own Assign writes",
            definition: json!({
                "QueryLanguage": "JSONata",
                "StartAt": "Mutate",
                "States": {
                    "Mutate": {
                        "Type": "Pass",
                        "Assign": { "y": 2 },
                        "Output": { "seen": "{% $y %}" },
                        "End": true,
                    },
                },
            }),
            input: "{}",
            expected: Expected::Nothing("$y"),
        },
    ];

    check("commit", rows).await;
}

/// The one node the rule above does not hold for: a `Choice` assigns as part of the
/// TRANSITION its selected rule decides, so what that rule assigned is in scope for the
/// output of that transition — and the state after it reads the same value.
///
/// The rows are the two query languages, which build that output through different
/// members: the rule's own `Output` expression, and the state's `OutputPath`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_choice_assigns_for_the_transition_its_rule_decides() {
    let rows: Vec<Row> = vec![
        Row {
            case: "a JSONata Choice rule's own Assign and Output",
            definition: jsonata(json!({
                "Mutate": {
                    "Type": "Choice",
                    "Choices": [{
                        "Condition": "{% $states.input.go %}",
                        "Assign": { "x": 2 },
                        "Output": { "seen": "{% $x %}" },
                        "Next": "Read",
                    }],
                    "Default": "Read",
                },
            })),
            input: r#"{"go":true}"#,
            expected: Expected::Output(json!({ "seen": 2, "nextSeen": 2 })),
        },
        Row {
            case: "a JSONPath Choice rule's own Assign and the state's OutputPath",
            definition: json!({
                "StartAt": "Seed",
                "States": {
                    "Seed": { "Type": "Pass", "Assign": { "x": 1 }, "Next": "Mutate" },
                    "Mutate": {
                        "Type": "Choice",
                        "Choices": [{
                            "Variable": "$.go",
                            "BooleanEquals": true,
                            "Assign": { "x": 2 },
                            "Next": "Read",
                        }],
                        "Default": "Read",
                        "OutputPath": "$x",
                    },
                    "Read": {
                        "Type": "Pass",
                        "Parameters": { "seen.$": "$", "nextSeen.$": "$x" },
                        "End": true,
                    },
                },
            }),
            input: r#"{"go":true}"#,
            expected: Expected::Output(json!({ "seen": 2, "nextSeen": 2 })),
        },
    ];

    check("choice", rows).await;
}

/// Run every row of `rows` and hold each to what it must end as.
async fn check(named: &str, rows: Vec<Row>) {
    let emulator = Emulator::new();
    // Every row's execution is started before any is read: each settles on the clock the
    // service owes it, so the rows settle together rather than one after another.
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let machine = create_machine(
            &emulator,
            &format!("{named}-{index}"),
            &row.definition.to_string(),
        )
        .await;
        started.push(start_execution(&emulator, &machine, "only", row.input).await);
    }

    for (row, execution) in rows.iter().zip(started) {
        let case = row.case;
        let described = settled(&emulator, case, &execution).await;
        match &row.expected {
            Expected::Output(wanted) => {
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some("SUCCEEDED"),
                    "case {case:?}: expected SUCCEEDED, got {described:?}"
                );
                let output: Value = described
                    .get("output")
                    .and_then(Value::as_str)
                    .map(|text| serde_json::from_str(text).expect("the output is JSON"))
                    .unwrap_or(Value::Null);
                assert_eq!(
                    &output, wanted,
                    "case {case:?}: expected output {wanted}, got {output}"
                );
            }
            Expected::Nothing(expression) => {
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some("FAILED"),
                    "case {case:?}: expected FAILED, got {described:?}"
                );
                assert_eq!(
                    described.get("error").and_then(Value::as_str),
                    Some(QUERY_EVALUATION_ERROR),
                    "case {case:?}: expected {QUERY_EVALUATION_ERROR}, got {described:?}"
                );
                let cause = described
                    .get("cause")
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                assert!(
                    cause.contains(expression),
                    "case {case:?}: expected a cause naming {expression:?}, got {cause:?}"
                );
            }
        }
    }
}
