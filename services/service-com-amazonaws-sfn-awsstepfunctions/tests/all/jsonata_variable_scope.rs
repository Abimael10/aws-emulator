// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a JSONata expression that reads a VARIABLE nothing in
//! scope assigned decides, driven over this service's own wire protocol (`awsJson1_0`,
//! `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: an expression of a JSONata state reads each variable the run
//! has ASSIGNED under its own name, so a name the scope reaching that expression does
//! not hold produces NOTHING — and a member or field whose expression produced nothing
//! fails the execution with `States.QueryEvaluationError`, under a cause naming the
//! expression as the definition wrote it and the field of the definition it is written
//! in. Which names the scope holds is decided by the path the run TOOK, not by what the
//! definition declares somewhere: a `Choice` that matched a rule leaves a name only its
//! state level declares unassigned, while the `Default` transition of that same machine
//! assigns it.
//!
//! Rows vary along the two dimensions the rule generalises over: WHY the name is not in
//! scope (nothing assigns it anywhere, it is misspelled, it differs from the assigned
//! name only in case, only the path the run did not take assigns it) and WHICH member
//! of a state reads it (an `Output` member, an element of an `Output` array, an
//! `Assign` member, a `Map`'s `ItemSelector` member, a `Choice` rule's `Condition`, a
//! `Wait`'s `Seconds`, a `Fail`'s `Error`, a `Map`'s `Items`). The control rows are the
//! same definitions over a scope that DOES hold the name, which run to completion over
//! the value it holds.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`)
//! is the one `state_machine_lifecycle` builds and the history reader is
//! `execution_history`'s; `tests/all/main.rs` picks this file up, so each is reached
//! through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::EVENT_EVALUATION_FAILED;

use crate::execution_history::history;
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, Emulator,
};

/// The ASL error name an expression that could not be evaluated fails under.
const QUERY_EVALUATION_ERROR: &str = "States.QueryEvaluationError";

/// How often a wait looks at an execution again, and how long it waits at all.
///
/// The wait is for the CONDITION — the execution leaving `RUNNING` — so the deadline is
/// generous and a host that settles an execution quickly spends none of it: nothing
/// here measures how fast the host runs.
const SETTLE_POLL: Duration = Duration::from_millis(25);
/// See [`SETTLE_POLL`].
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// The record of `execution_arn` once it has left `RUNNING`, looked for again until it
/// has or [`SETTLE_DEADLINE`] passes.
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

/// A JSONata definition that starts at `start` and declares `states`.
fn machine(start: &str, states: Value) -> String {
    json!({ "QueryLanguage": "JSONata", "StartAt": start, "States": states }).to_string()
}

/// A one-state JSONata definition: the state `name`, of the shape `state`.
fn one_state(name: &str, state: Value) -> String {
    machine(name, json!({ name: state }))
}

/// The two-state shape a variable travels through: `Set` assigns `assign` and hands
/// over to `Read`, which echoes `output`.
fn assign_then_read(assign: Value, output: Value) -> String {
    machine(
        "Set",
        json!({
            "Set": { "Type": "Pass", "Assign": assign, "Next": "Read" },
            "Read": { "Type": "Pass", "Output": output, "End": true },
        }),
    )
}

/// A `Choice` whose rules each declare `assign` (none, where that is `Value::Null`)
/// and whose state level declares one of its own, routing to a `Pass` per rule that
/// echoes `$tag`.
///
/// The recorded shape: two rules whose conditions both hold for a large enough `n`, so
/// the first match decides, and a `Default` the state level's own assignment reaches.
fn choice_over_tag(rule_assign: bool) -> String {
    let rule = |bound: i64, next: &str, tag: &str| {
        let mut rule = json!({
            "Condition": format!("{{% $states.input.n > {bound} %}}"),
            "Next": next,
        });
        if rule_assign {
            rule["Assign"] = json!({ "tag": tag });
        }
        rule
    };
    let echo = |branch: &str| {
        json!({
            "Type": "Pass",
            "Output": { "branch": branch, "tag": "{% $tag %}" },
            "End": true,
        })
    };
    machine(
        "Route",
        json!({
            "Route": {
                "Type": "Choice",
                "Assign": { "tag": "from-state-level" },
                "Choices": [
                    rule(10, "First", "from-first-rule"),
                    rule(0, "Second", "from-second-rule"),
                ],
                "Default": "Fallback",
            },
            "First": echo("first"),
            "Second": echo("second"),
            "Fallback": echo("fallback"),
        }),
    )
}

/// What one row must end as.
enum Expected {
    /// The reference produced nothing: the expression and the field of the definition
    /// the failure must name.
    Nothing {
        expression: &'static str,
        field: &'static str,
    },
    /// The scope held every name the run read: the output it must carry.
    Output(Value),
}

/// One row: a definition, the input it runs over, and what it must end as.
struct Row {
    case: &'static str,
    definition: String,
    input: &'static str,
    expected: Expected,
}

/// The processor a `Map` row iterates with, which does nothing of its own.
fn passing_processor() -> Value {
    json!({ "StartAt": "P", "States": { "P": { "Type": "Pass", "End": true } } })
}

/// A JSONata expression reading a variable the scope that reaches it does not hold
/// fails the execution `States.QueryEvaluationError`, naming the expression and the
/// field it is written in, wherever in a state that reference is written.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_variable_nothing_in_scope_assigned_fails_the_state_that_reads_it() {
    let rows: Vec<Row> = vec![
        Row {
            case: "a name nothing in the definition assigns, read in an Output member",
            definition: one_state(
                "Echo",
                json!({
                    "Type": "Pass",
                    "Output": { "branch": "first", "tag": "{% $tag %}" },
                    "End": true,
                }),
            ),
            input: r#"{"n":50}"#,
            expected: Expected::Nothing {
                expression: "$tag",
                field: "Output/tag",
            },
        },
        Row {
            case: "a misspelled name, where the spelling the definition assigns is in scope",
            definition: assign_then_read(
                json!({ "total": "{% $states.input.n %}" }),
                json!({ "seen": "{% $totl %}" }),
            ),
            input: r#"{"n":7}"#,
            expected: Expected::Nothing {
                expression: "$totl",
                field: "Output/seen",
            },
        },
        Row {
            // A variable's name is read exactly as it is written, so a reference
            // differing from the assignment only in case holds nothing.
            case: "a name differing from the assigned one only in case",
            definition: assign_then_read(
                json!({ "total": "{% $states.input.n %}" }),
                json!({ "seen": "{% $Total %}" }),
            ),
            input: r#"{"n":7}"#,
            expected: Expected::Nothing {
                expression: "$Total",
                field: "Output/seen",
            },
        },
        Row {
            // The control for the two above: the scope does hold the assigned name.
            case: "the assigned name itself, read by a later state",
            definition: assign_then_read(
                json!({ "total": "{% $states.input.n %}" }),
                json!({ "seen": "{% $total %}" }),
            ),
            input: r#"{"n":7}"#,
            expected: Expected::Output(json!({ "seen": 7 })),
        },
        Row {
            // The recorded shape: `n` satisfies the first rule, whose own `Assign`
            // declares nothing, so the state level's `tag` never reaches the target.
            case: "a name only the state level of a MATCHED Choice declares",
            definition: choice_over_tag(false),
            input: r#"{"n":50}"#,
            expected: Expected::Nothing {
                expression: "$tag",
                field: "Output/tag",
            },
        },
        Row {
            case: "the same machine where the SECOND rule is the one that matched",
            definition: choice_over_tag(false),
            input: r#"{"n":5}"#,
            expected: Expected::Nothing {
                expression: "$tag",
                field: "Output/tag",
            },
        },
        Row {
            // The complement: no rule matches, so the `Default` transition is taken
            // and the state level's own assignment is what reaches the target.
            case: "the same machine over an input no rule matches",
            definition: choice_over_tag(false),
            input: r#"{"n":-1}"#,
            expected: Expected::Output(json!({
                "branch": "fallback",
                "tag": "from-state-level",
            })),
        },
        Row {
            case: "the same machine whose matched rule declares the name itself",
            definition: choice_over_tag(true),
            input: r#"{"n":50}"#,
            expected: Expected::Output(json!({
                "branch": "first",
                "tag": "from-first-rule",
            })),
        },
        Row {
            case: "a name read in an element of an Output array",
            definition: one_state(
                "Echo",
                json!({
                    "Type": "Pass",
                    "Output": { "all": ["kept", "{% $tag %}"] },
                    "End": true,
                }),
            ),
            input: r#"{"n":1}"#,
            expected: Expected::Nothing {
                expression: "$tag",
                field: "Output/all[1]",
            },
        },
        Row {
            case: "a name read in an Assign member",
            definition: one_state(
                "Copy",
                json!({
                    "Type": "Pass",
                    "Assign": { "copy": "{% $tag %}" },
                    "End": true,
                }),
            ),
            input: r#"{"n":1}"#,
            expected: Expected::Nothing {
                expression: "$tag",
                field: "Assign/copy",
            },
        },
        Row {
            case: "a name read in a Choice rule's Condition",
            definition: machine(
                "Route",
                json!({
                    "Route": {
                        "Type": "Choice",
                        "Choices": [{ "Condition": "{% $tag %}", "Next": "Done" }],
                        "Default": "Done",
                    },
                    "Done": { "Type": "Pass", "End": true },
                }),
            ),
            input: r#"{"n":1}"#,
            expected: Expected::Nothing {
                expression: "$tag",
                field: "Choices[0]/Condition",
            },
        },
        Row {
            case: "a name read in a Wait state's Seconds",
            definition: one_state(
                "Hold",
                json!({ "Type": "Wait", "Seconds": "{% $delay %}", "End": true }),
            ),
            input: r#"{"n":1}"#,
            expected: Expected::Nothing {
                expression: "$delay",
                field: "Seconds",
            },
        },
        Row {
            case: "a name read in a Fail state's Error",
            definition: one_state(
                "Boom",
                json!({ "Type": "Fail", "Error": "{% $code %}", "Cause": "boom" }),
            ),
            input: r#"{"n":1}"#,
            expected: Expected::Nothing {
                expression: "$code",
                field: "Error",
            },
        },
        Row {
            case: "a name read in a Map state's Items",
            definition: one_state(
                "Each",
                json!({
                    "Type": "Map",
                    "Items": "{% $batch %}",
                    "ItemProcessor": passing_processor(),
                    "End": true,
                }),
            ),
            input: r#"{"n":1}"#,
            expected: Expected::Nothing {
                expression: "$batch",
                field: "Items",
            },
        },
        Row {
            case: "a name read in a Map state's ItemSelector member",
            definition: one_state(
                "Each",
                json!({
                    "Type": "Map",
                    "Items": [1],
                    "ItemSelector": { "v": "{% $tag %}" },
                    "ItemProcessor": passing_processor(),
                    "End": true,
                }),
            ),
            input: r#"{"n":1}"#,
            expected: Expected::Nothing {
                expression: "$tag",
                field: "ItemSelector/v",
            },
        },
    ];

    let emulator = Emulator::new();
    // Every row's execution is started before any is read: each settles on the clock
    // the service owes it, so the rows settle together rather than one after another.
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let machine = create_machine(&emulator, &format!("scope-{index}"), &row.definition).await;
        started.push(start_execution(&emulator, &machine, "only", row.input).await);
    }

    for (row, execution) in rows.iter().zip(started) {
        let case = row.case;
        let described = settled(&emulator, case, &execution).await;
        let status = described.get("status").and_then(Value::as_str);
        let events = history(&emulator, &execution, json!({})).await;
        let failures: Vec<&Value> = events
            .iter()
            .filter(|event| {
                event.get("type").and_then(Value::as_str) == Some(EVENT_EVALUATION_FAILED)
            })
            .collect();

        let (expression, field) = match &row.expected {
            Expected::Nothing { expression, field } => (*expression, *field),
            Expected::Output(wanted) => {
                assert_eq!(
                    status,
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the run whose scope holds the name to succeed, got \
                     {described:?}"
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
                assert!(
                    failures.is_empty(),
                    "case {case:?}: expected no evaluation failure, got {failures:?}"
                );
                continue;
            }
        };

        assert_eq!(
            status,
            Some("FAILED"),
            "case {case:?}: expected the unassigned reference to fail the execution, got \
             {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(QUERY_EVALUATION_ERROR),
            "case {case:?}: expected {QUERY_EVALUATION_ERROR}, got {described:?}"
        );
        let cause = described
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let diagnosed = format!(
            "The JSONata expression '{expression}' specified for the field '{field}' returned \
             nothing (undefined)."
        );
        assert!(
            cause.contains(&diagnosed),
            "case {case:?}: expected a cause carrying {diagnosed:?}, got {cause:?}"
        );
        assert!(
            described.get("output").is_none_or(Value::is_null),
            "case {case:?}: a failed execution carries no output, got {described:?}"
        );
        // The history says the same thing, once, about the field the reference is
        // written in.
        assert_eq!(
            failures.len(),
            1,
            "case {case:?}: expected exactly one {EVENT_EVALUATION_FAILED}, got {failures:?}"
        );
        let details = failures[0]
            .get("evaluationFailedEventDetails")
            .unwrap_or_else(|| panic!("case {case:?}: expected the event's own detail"));
        assert_eq!(
            details.get("location").and_then(Value::as_str),
            Some(field),
            "case {case:?}: expected the field the reference is written in, got {details:?}"
        );
        assert_eq!(
            details.get("error").and_then(Value::as_str),
            Some(QUERY_EVALUATION_ERROR),
            "case {case:?}: expected the event to carry the same error, got {details:?}"
        );
        assert_eq!(
            details.get("cause").and_then(Value::as_str),
            Some(cause.as_str()),
            "case {case:?}: expected the event and the execution to carry one cause, got \
             {details:?}"
        );
    }
}
