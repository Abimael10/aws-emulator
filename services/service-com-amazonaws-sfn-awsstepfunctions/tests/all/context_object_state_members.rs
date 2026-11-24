// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SHAPE of the context object (`$$`) a state resolves, driven
//! over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: the context object a state resolves carries `Execution`, `State`
//! and `StateMachine` (and `Map` inside a `Map` state's iteration), and its `State`
//! carries the state's `Name` and `EnteredTime` plus `RetryCount` for a state of a kind
//! the Amazon States Language gives a `Retry` — the kinds something can re-run — and
//! nothing else. A state of a kind nothing can retry has no attempt to report, so the
//! member is absent from the document such a state resolves.
//!
//! Rows vary the dimension the rule generalises over: the KIND of the state reading the
//! object, over both query languages — three kinds the language gives no `Retry`
//! (`Pass`, `Wait`, `Succeed`) and two it does (`Map`, `Parallel`).
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`)
//! is the one `state_machine_lifecycle` builds; `tests/all/main.rs` picks this file up,
//! so each is reached through `crate::<suite>`.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, Emulator,
};

/// How often an execution is looked at again, and how long it is waited for at all.
///
/// The wait is for the CONDITION — the execution leaving `RUNNING` — so the deadline is
/// generous and a host that settles one quickly spends none of it: nothing here measures
/// how fast the host runs.
const POLL: Duration = Duration::from_millis(25);
/// See [`POLL`].
const DEADLINE: Duration = Duration::from_secs(120);

/// The name every row's state carries.
const STATE_NAME: &str = "Ctx";

/// The members the context object's `State` carries for a state nothing can retry: its
/// name and the instant it was entered.
const STATE_MEMBERS: [&str; 2] = ["Name", "EnteredTime"];

/// The member it carries BESIDE those for a state of a kind that can be retried: the
/// attempt that state is on.
const ATTEMPT_MEMBER: &str = "RetryCount";

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

/// The output of a settled execution, as the document it is.
fn output_of(described: &Value) -> Value {
    described
        .get("output")
        .and_then(Value::as_str)
        .map(|text| serde_json::from_str(text).expect("the output is JSON"))
        .unwrap_or(Value::Null)
}

/// The member names of `value`, which must be an object.
fn members(case: &str, what: &str, value: &Value) -> BTreeSet<String> {
    value
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected {what} to be an object, got {value}"))
        .keys()
        .cloned()
        .collect()
}

/// The member names `names` as a set, for comparing against a resolved document's own.
fn expected(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| (*name).to_string()).collect()
}

/// One row: the state that resolves the context object, the input the execution runs
/// over, and whether that state's kind can be retried.
struct Row {
    case: &'static str,
    /// The whole definition, whose one state under test is named [`STATE_NAME`] and
    /// whose output carries the resolved object under `ctx`.
    definition: String,
    input: &'static str,
    /// Whether the Amazon States Language gives this state's kind a `Retry`, which is
    /// whether its context object reports the attempt it is on.
    retriable: bool,
    /// Whether the resolved object carries `Map` beside the three members every state's
    /// does: only the states of a `Map` state's own iteration read the item being
    /// iterated.
    map_member: bool,
}

fn rows() -> Vec<Row> {
    // One state per row, named the same way, with its own language's way of projecting
    // the context object into what the state puts out: a payload template reading `$$`
    // for JSONPath, and `$states.context` for JSONata.
    let machine = |state: Value| {
        json!({ "StartAt": STATE_NAME, "States": { STATE_NAME: state } }).to_string()
    };
    vec![
        Row {
            // The recorded shape: a JSONPath `Pass` state projecting the whole object.
            case: "a Pass state, which the language gives no Retry",
            definition: machine(json!({
                "Type": "Pass",
                "Parameters": { "ctx.$": "$$" },
                "End": true,
            })),
            input: r#"{"marker":"pass"}"#,
            retriable: false,
            map_member: false,
        },
        Row {
            case: "a JSONata Succeed state, which the language gives no Retry",
            definition: machine(json!({
                "Type": "Succeed",
                "QueryLanguage": "JSONata",
                "Output": { "ctx": "{% $states.context %}" },
            })),
            input: r#"{"marker":"succeed"}"#,
            retriable: false,
            map_member: false,
        },
        Row {
            case: "a JSONata Wait state, which the language gives no Retry",
            definition: machine(json!({
                "Type": "Wait",
                "QueryLanguage": "JSONata",
                "Seconds": 1,
                "Output": { "ctx": "{% $states.context %}" },
                "End": true,
            })),
            input: r#"{"marker":"wait"}"#,
            retriable: false,
            map_member: false,
        },
        Row {
            // A `Parallel` state's kind carries `Retry`, so its context object reports
            // the attempt it is on even where the state declares no retrier of its own.
            case: "a Parallel state, which the language gives a Retry",
            definition: machine(json!({
                "Type": "Parallel",
                "Parameters": { "ctx.$": "$$" },
                "Branches": [{
                    "StartAt": "Branch",
                    "States": { "Branch": { "Type": "Pass", "End": true } },
                }],
                "OutputPath": "$[0]",
                "End": true,
            })),
            input: r#"{"marker":"parallel"}"#,
            retriable: true,
            map_member: false,
        },
        Row {
            // And a `Map` state's, whose iteration also reads the item it is on.
            case: "a Map state, which the language gives a Retry",
            definition: machine(json!({
                "Type": "Map",
                "ItemSelector": { "ctx.$": "$$" },
                "ItemProcessor": {
                    "StartAt": "Each",
                    "States": { "Each": { "Type": "Pass", "End": true } },
                },
                "OutputPath": "$[0]",
                "End": true,
            })),
            input: "[1]",
            retriable: true,
            map_member: true,
        },
    ]
}

/// The context object a state resolves carries the attempt it is on only where the
/// state's kind can be retried, beside the state's name and entry instant.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_context_object_reports_an_attempt_only_for_a_state_that_can_be_retried() {
    let rows = rows();
    let emulator = Emulator::new();

    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let machine = create_machine(&emulator, &format!("ctx-{index}"), &row.definition).await;
        started.push(start_execution(&emulator, &machine, "only", row.input).await);
    }

    for (row, execution) in rows.iter().zip(started) {
        let case = row.case;
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected reading the context object to succeed, got {described:?}"
        );
        let context = output_of(&described)
            .get("ctx")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected the resolved object"));

        // The top-level members: the run, the state and the state machine, plus the item
        // a `Map` state's iteration is on. Nothing else — `Task` belongs to a `Task`
        // state alone.
        let mut top: Vec<&str> = vec!["Execution", "State", "StateMachine"];
        if row.map_member {
            top.push("Map");
        }
        assert_eq!(
            members(case, "the context object", &context),
            expected(&top),
            "case {case:?}: expected exactly the members {top:?}, got {context}"
        );

        // And the state's own: its name, its entry instant, and the attempt it is on
        // only where something can re-run it.
        let state = context
            .get("State")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected the state's members"));
        let mut names: Vec<&str> = STATE_MEMBERS.to_vec();
        if row.retriable {
            names.push(ATTEMPT_MEMBER);
        }
        assert_eq!(
            members(case, "the state's members", &state),
            expected(&names),
            "case {case:?}: expected exactly the members {names:?}, got {state}"
        );
        assert_eq!(
            state.get("Name").and_then(Value::as_str),
            Some(STATE_NAME),
            "case {case:?}: expected the state's own name, got {state}"
        );
        assert!(
            state.get("EnteredTime").is_some_and(Value::is_string),
            "case {case:?}: expected the instant the state was entered, got {state}"
        );
        // A state no retrier has re-run is on its first attempt, which is none taken.
        assert_eq!(
            state.get(ATTEMPT_MEMBER).and_then(Value::as_u64),
            row.retriable.then_some(0),
            "case {case:?}: expected the attempt the state is on, got {state}"
        );
    }
}
