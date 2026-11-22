// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the EXECUTION VARIABLES of the JSONPath half of the Amazon
//! States Language — the `Assign` block that writes them and the `$<name>` reference
//! that reads one — driven over this service's own wire protocol (`awsJson1_0`,
//! `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`, beside the three other run-time rules this suite's issue is
//! about.
//!
//! The rules under test, each over a table whose rows differ along what the rule
//! generalises over rather than along the one input the issue cited:
//!
//! 1. a state written in JSONPath may carry `Assign`, which is a PAYLOAD TEMPLATE: a
//!    member named `"<name>.$"` assigns the value its path or intrinsic function
//!    selected and every other member assigns the literal the definition wrote. The
//!    variables outlive the state, and a path whose first term is `$<name>` reads one
//!    wherever the language reads a path — a payload template, a `Choice` comparator, a
//!    `*Path` member. `Assign` is read from whichever node decided the transition: the
//!    state itself, the `Choice` rule that matched, or the `Catch` catcher that took a
//!    failure over;
//! 2. the variables an execution holds TOGETHER are bounded, and the space is freed by
//!    assigning a variable again and by leaving the state machine that assigned it, so
//!    an `Assign` past the bound fails with `States.DataLimitExceeded` while the same
//!    bytes assigned over and over do not;
//! 3. a `Map` state's concurrency is held to the range the member carries where the
//!    state RESOLVES it as it runs, so a path resolving past that range fails the
//!    execution with `States.Runtime`, while a bound the definition STATES past it runs
//!    under the largest figure the member carries;
//! 4. a retrier's wait is the whole seconds of the figure its own members compute, so a
//!    computed 7.5 seconds is waited as 7 — asserted through `TestState`, which reports
//!    the interval a retriable state owes without anything waiting for it;
//! 5. a `Task` state whose `Resource` names an activity this emulator does not hold
//!    schedules nothing: the task would be queued under that activity and handed out by
//!    a `GetActivityTask` on it, so the execution fails with `States.Runtime` instead of
//!    waiting for a worker that can never be handed it.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`) is the one `state_machine_lifecycle` builds and the activity helpers are
//! `activity_tasks`'; `tests/all/main.rs` declares every suite of this target, so both
//! are reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::{
    DATA_LIMIT_ERROR, MAX_MAP_CONCURRENCY, MAX_VARIABLE_BYTES, QUERY_EVALUATION_ERROR,
    RUNTIME_ERROR,
};

use crate::activity_tasks::{activity_arn, activity_definition, claimed, create_activity};
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator, ROLE_ARN,
};

/// How long a row waits for an execution to reach a terminal status: generous, because
/// what it is waiting for is the emulator's own settle delay and not the host's speed.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// How long a row waits between two looks at an execution's status.
const POLL_INTERVAL: Duration = Duration::from_millis(250);

/// Poll an execution until it reports a status other than `RUNNING`, and answer that
/// record. Fails with `case` when [`TERMINAL_DEADLINE`] passes first.
async fn poll_until_terminal(emulator: &Emulator, case: &str, execution: &str) -> Value {
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
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// The `output` of a terminal execution, parsed.
fn output_of(case: &str, described: &Value) -> Value {
    let text = described
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected an output in {described:?}"));
    serde_json::from_str(text)
        .unwrap_or_else(|_| panic!("case {case:?}: expected JSON output, got {text:?}"))
}

/// Run `definition` over `input` and answer the terminal record of that execution.
async fn ran(emulator: &Emulator, case: &str, name: &str, definition: &Value, input: &str) -> Value {
    let machine = create_machine(emulator, name, &definition.to_string()).await;
    let execution = start_execution(emulator, &machine, "only", input).await;
    poll_until_terminal(emulator, case, &execution).await
}

// ---------------------------------------------------------------------------
// 1. `Assign` writes the variables a later state's paths read
// ---------------------------------------------------------------------------

/// A definition whose first state ASSIGNS `assign` and whose second state reads the
/// variables back through a payload template: the output is what the reads produced.
fn assign_then_read(assign: Value, read: Value) -> Value {
    json!({
        "StartAt": "Seed",
        "States": {
            "Seed": { "Type": "Pass", "Assign": assign, "Next": "Read" },
            "Read": { "Type": "Pass", "Parameters": read, "End": true },
        },
    })
}

/// A JSONPath `Assign` binds its members as execution variables — the literal ones as
/// written, the `"<name>.$"` ones as what their path or intrinsic selected — and a path
/// whose first term is `$<name>` reads one back.
///
/// The rows differ along what the rule generalises over: the JSON type of the value
/// (number, string, boolean, null, array, object), the FORM of the assignment (a
/// literal, a path into the state's input, a path into the context object, an intrinsic
/// over a variable already assigned, a nested template member) and the FORM of the read
/// (the whole variable, a member of it, an index into it, an intrinsic over it).
#[tokio::test]
async fn a_jsonpath_assign_binds_variables_a_later_state_reads() {
    // (case, the `Assign` block, the reading template, the output it must produce)
    let rows: Vec<(&str, Value, Value, Value)> = vec![
        (
            "a literal number, read whole",
            json!({ "v": 10 }),
            json!({ "seen.$": "$v" }),
            json!({ "seen": 10 }),
        ),
        (
            "a literal string, read whole",
            json!({ "v": "ten" }),
            json!({ "seen.$": "$v" }),
            json!({ "seen": "ten" }),
        ),
        (
            "a literal boolean and null, read whole",
            json!({ "flag": true, "nothing": null }),
            json!({ "flag.$": "$flag", "nothing.$": "$nothing" }),
            json!({ "flag": true, "nothing": null }),
        ),
        (
            "an array, read whole and by index",
            json!({ "items": [1, 2, 3] }),
            json!({ "all.$": "$items", "second.$": "$items[1]" }),
            json!({ "all": [1, 2, 3], "second": 2 }),
        ),
        (
            "an object, read whole and by member",
            json!({ "who": { "name": "ada", "age": 36 } }),
            json!({ "who.$": "$who", "name.$": "$who.name" }),
            json!({ "who": { "name": "ada", "age": 36 }, "name": "ada" }),
        ),
        (
            "a member of the state's own input",
            json!({ "v.$": "$.val" }),
            json!({ "seen.$": "$v" }),
            json!({ "seen": 42 }),
        ),
        (
            "a member of the context object",
            json!({ "machine.$": "$$.Execution.Name" }),
            json!({ "seen.$": "$machine" }),
            json!({ "seen": "only" }),
        ),
        (
            "an intrinsic over the input",
            json!({ "v.$": "States.MathAdd($.val, 8)" }),
            json!({ "seen.$": "$v" }),
            json!({ "seen": 50 }),
        ),
        (
            // The members are evaluated in order, so an assignment reads the ones
            // written before it — which is also how an intrinsic over a variable is
            // written.
            "an intrinsic over a variable assigned before it",
            json!({ "v.$": "$.val", "doubled.$": "States.MathAdd($v, $v)" }),
            json!({ "seen.$": "$doubled" }),
            json!({ "seen": 84 }),
        ),
        (
            "a nested template member, read through an intrinsic",
            json!({ "nested": { "inner.$": "$.val" } }),
            json!({ "seen.$": "States.Format('{}', $nested.inner)" }),
            json!({ "seen": "42" }),
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, assign, read, expected)) in rows.into_iter().enumerate() {
        let definition = assign_then_read(assign, read);
        let described = ran(
            &emulator,
            case,
            &format!("assign-read-{index}"),
            &definition,
            r#"{"val":42}"#,
        )
        .await;
        assert_eq!(
            string(&described, "status"),
            "SUCCEEDED",
            "case {case:?}: a machine that uses variables runs, got {described:?}"
        );
        let output = output_of(case, &described);
        assert_eq!(
            output, expected,
            "case {case:?}: expected the variables to be read as {expected}, got {output}"
        );
    }
}

/// One row of the `Choice` table: the case, the variables `Seed` assigns, the rules the
/// state declares, the execution input, the branch that must be taken and the variable
/// that branch must report.
struct ChoiceRow {
    case: &'static str,
    seed: Value,
    choices: Value,
    input: &'static str,
    branch: &'static str,
    chose: Value,
}

/// A `Choice` rule compares a VARIABLE through the comparator's `*Path` form, and the
/// rule that matched is the node whose own `Assign` applies — the state's own applying
/// only where no rule matched.
///
/// The rows differ along the comparator family and the type of the value compared, and
/// along which node assigned the variable the following state reads.
#[tokio::test]
async fn a_choice_rule_reads_and_assigns_variables() {
    let rows: Vec<ChoiceRow> = vec![
        ChoiceRow {
            case: "NumericEqualsPath against a variable",
            seed: json!({ "v": 10 }),
            choices: json!([{ "Variable": "$.val", "NumericEqualsPath": "$v", "Next": "Matched",
                     "Assign": { "chose": "numeric" } }]),
            input: r#"{"val":10}"#,
            branch: "Matched",
            chose: json!("numeric"),
        },
        ChoiceRow {
            case: "StringEqualsPath against a variable",
            seed: json!({ "v": "ten" }),
            choices: json!([{ "Variable": "$.val", "StringEqualsPath": "$v", "Next": "Matched",
                     "Assign": { "chose": "string" } }]),
            input: r#"{"val":"ten"}"#,
            branch: "Matched",
            chose: json!("string"),
        },
        ChoiceRow {
            case: "BooleanEqualsPath against a variable",
            seed: json!({ "v": true }),
            choices: json!([{ "Variable": "$.val", "BooleanEqualsPath": "$v", "Next": "Matched",
                     "Assign": { "chose": "boolean" } }]),
            input: r#"{"val":true}"#,
            branch: "Matched",
            chose: json!("boolean"),
        },
        ChoiceRow {
            // A typed comparator never coerces across JSON types, so a variable of
            // another type simply does not match and the `Default` is taken — where
            // the STATE's own `Assign` is the one that applies.
            case: "a variable of another type, which matches nothing",
            seed: json!({ "v": 10 }),
            choices: json!([{ "Variable": "$.val", "NumericEqualsPath": "$v", "Next": "Matched",
                     "Assign": { "chose": "numeric" } }]),
            input: r#"{"val":"10"}"#,
            branch: "Fell",
            chose: json!("default"),
        },
        ChoiceRow {
            case: "NumericGreaterThanPath against a variable",
            seed: json!({ "v": 10 }),
            choices: json!([{ "Variable": "$.val", "NumericGreaterThanPath": "$v", "Next": "Matched",
                     "Assign": { "chose": "greater" } }]),
            input: r#"{"val":11}"#,
            branch: "Matched",
            chose: json!("greater"),
        },
    ];

    let emulator = Emulator::new();
    for (
        index,
        ChoiceRow {
            case,
            seed,
            choices,
            input,
            branch,
            chose,
        },
    ) in rows.into_iter().enumerate()
    {
        let definition = json!({
            "StartAt": "Seed",
            "States": {
                "Seed": { "Type": "Pass", "Assign": seed, "Next": "Decide" },
                "Decide": {
                    "Type": "Choice",
                    "Choices": choices,
                    "Default": "Fell",
                    "Assign": { "chose": "default" },
                },
                "Matched": {
                    "Type": "Pass",
                    "Parameters": { "branch": "Matched", "chose.$": "$chose" },
                    "End": true,
                },
                "Fell": {
                    "Type": "Pass",
                    "Parameters": { "branch": "Fell", "chose.$": "$chose" },
                    "End": true,
                },
            },
        });
        let described = ran(
            &emulator,
            case,
            &format!("choice-variable-{index}"),
            &definition,
            input,
        )
        .await;
        assert_eq!(
            string(&described, "status"),
            "SUCCEEDED",
            "case {case:?}: a Choice over a variable runs, got {described:?}"
        );
        let output = output_of(case, &described);
        assert_eq!(
            output,
            json!({ "branch": branch, "chose": chose }),
            "case {case:?}: expected branch {branch:?} with {chose}, got {output}"
        );
    }
}

/// A `Catch` catcher's own `Assign` is evaluated over the ERROR OUTPUT it was handed,
/// and the state the catcher sends the execution to reads what it assigned.
#[tokio::test]
async fn a_catcher_assigns_from_the_error_output_it_was_handed() {
    let definition = json!({
        "StartAt": "Failing",
        "States": {
            // A `Map` whose one iteration fails is a failure the state's own `Catch`
            // takes over, with no service integration involved.
            "Failing": {
                "Type": "Map",
                "ItemsPath": "$.items",
                "ItemProcessor": {
                    "StartAt": "Boom",
                    "States": {
                        "Boom": { "Type": "Fail", "Error": "ItemBroke", "Cause": "the item said so" },
                    },
                },
                "Catch": [{
                    "ErrorEquals": ["ItemBroke"],
                    "Next": "Recovered",
                    "Assign": { "why.$": "$.Error", "detail.$": "$.Cause" },
                }],
                "End": true,
            },
            "Recovered": {
                "Type": "Pass",
                "Parameters": { "why.$": "$why", "detail.$": "$detail" },
                "End": true,
            },
        },
    });

    let emulator = Emulator::new();
    let case = "a catcher that assigns from the error output";
    let described = ran(
        &emulator,
        case,
        "catcher-assign",
        &definition,
        r#"{"items":[1]}"#,
    )
    .await;
    assert_eq!(
        string(&described, "status"),
        "SUCCEEDED",
        "case {case:?}: the catcher absorbs the failure, got {described:?}"
    );
    let output = output_of(case, &described);
    assert_eq!(
        output,
        json!({ "why": "ItemBroke", "detail": "the item said so" }),
        "case {case:?}: expected the catcher's assignments, got {output}"
    );
}

// ---------------------------------------------------------------------------
// 2. What the variables an execution holds together may carry
// ---------------------------------------------------------------------------

/// The variables an execution holds together are bounded: an `Assign` that would leave
/// more than [`MAX_VARIABLE_BYTES`] in scope fails with `States.DataLimitExceeded`,
/// while the same bytes assigned to ONE name over and over — or assigned inside a `Map`
/// iteration, whose scope the walk then leaves — stay inside it.
#[tokio::test]
async fn the_variables_an_execution_holds_together_are_bounded() {
    // Two of these fill the space; one leaves room to spare. The input carries the
    // bytes, so nothing of the definition has to build them.
    let half = "x".repeat(MAX_VARIABLE_BYTES / 2);
    let input = json!({ "big": half, "items": [1, 2] }).to_string();

    // (case, the definition, the status it must reach, the error it must carry)
    let rows: Vec<(&str, Value, &str, Option<&str>)> = vec![
        (
            "one variable inside the space",
            json!({
                "StartAt": "One",
                "States": {
                    "One": { "Type": "Pass", "Assign": { "a.$": "$.big" }, "End": true },
                },
            }),
            "SUCCEEDED",
            None,
        ),
        (
            "the same name assigned twice, which frees what it held",
            json!({
                "StartAt": "First",
                "States": {
                    "First": { "Type": "Pass", "Assign": { "a.$": "$.big" }, "Next": "Again" },
                    "Again": { "Type": "Pass", "Assign": { "a.$": "$.big" }, "End": true },
                },
            }),
            "SUCCEEDED",
            None,
        ),
        (
            "a second NAME, which is held beside the first",
            json!({
                "StartAt": "First",
                "States": {
                    "First": { "Type": "Pass", "Assign": { "a.$": "$.big" }, "Next": "Second" },
                    "Second": { "Type": "Pass", "Assign": { "b.$": "$.big" }, "End": true },
                },
            }),
            "FAILED",
            Some(DATA_LIMIT_ERROR),
        ),
        (
            // A nested state machine's assignments do not outlive it, so each
            // iteration assigns in a scope of its own and the space is free again
            // once the `Map` has been left.
            "a name assigned inside every Map iteration, whose scope is left each time",
            json!({
                "StartAt": "Fan",
                "States": {
                    "Fan": {
                        "Type": "Map",
                        "ItemsPath": "$.items",
                        "ItemSelector": { "big.$": "$.big", "tag": "done" },
                        "ItemProcessor": {
                            "StartAt": "Inner",
                            "States": {
                                "Inner": {
                                    "Type": "Pass",
                                    "Assign": { "inner.$": "$.big" },
                                    "OutputPath": "$.tag",
                                    "End": true,
                                },
                            },
                        },
                        "End": true,
                    },
                },
            }),
            "SUCCEEDED",
            None,
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, definition, status, error)) in rows.into_iter().enumerate() {
        let described = ran(
            &emulator,
            case,
            &format!("variable-space-{index}"),
            &definition,
            &input,
        )
        .await;
        assert_eq!(
            string(&described, "status"),
            status,
            "case {case:?}: expected {status}, got {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            error,
            "case {case:?}: expected the error {error:?}, got {described:?}"
        );
    }
}

/// One row of the padding table: the case, the width `$pad` is given, the status the
/// run must reach, the error it must carry, and the length the variable must hold.
struct PaddedRow {
    case: &'static str,
    width: usize,
    status: &'static str,
    error: Option<&'static str>,
    built: Option<usize>,
}

/// A JSONata expression that PADS a string is evaluated when the width it names is
/// inside what one state's document may carry, and refused when it is past it: the one
/// step whose cost is a width the expression names is bounded ahead of the step rather
/// than left out of the language.
#[tokio::test]
async fn a_padded_string_is_built_up_to_what_a_state_may_carry() {
    let rows: Vec<PaddedRow> = vec![
        PaddedRow {
            case: "a width well inside the document ceiling",
            width: 100,
            status: "SUCCEEDED",
            error: None,
            built: Some(100),
        },
        PaddedRow {
            case: "a width at the bytes one document may carry, less its quotes",
            width: MAX_VARIABLE_BYTES - 2,
            status: "SUCCEEDED",
            error: None,
            built: Some(MAX_VARIABLE_BYTES - 2),
        },
        PaddedRow {
            case: "a width past it",
            width: MAX_VARIABLE_BYTES * 2,
            status: "FAILED",
            error: Some(QUERY_EVALUATION_ERROR),
            built: None,
        },
    ];

    let emulator = Emulator::new();
    for (
        index,
        PaddedRow {
            case,
            width,
            status,
            error,
            built,
        },
    ) in rows.into_iter().enumerate()
    {
        let definition = json!({
            "QueryLanguage": "JSONata",
            "StartAt": "Pad",
            "States": {
                // The `Assign` and the `Output` of ONE state share one allowance, so a
                // variable that fills it is reported by the state after it.
                "Pad": {
                    "Type": "Pass",
                    "Assign": { "padded": format!("{{% $pad('', {width}, 'x') %}}") },
                    "Next": "Report",
                },
                "Report": {
                    "Type": "Pass",
                    "Output": { "length": "{% $length($padded) %}" },
                    "End": true,
                },
            },
        });
        let described = ran(
            &emulator,
            case,
            &format!("padded-{index}"),
            &definition,
            "{}",
        )
        .await;
        assert_eq!(
            string(&described, "status"),
            status,
            "case {case:?}: expected {status}, got {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            error,
            "case {case:?}: expected the error {error:?}, got {described:?}"
        );
        if let Some(built) = built {
            let output = output_of(case, &described);
            assert_eq!(
                output,
                json!({ "length": built }),
                "case {case:?}: expected a string of {built} characters, got {output}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// 3. The range a `Map` state's `MaxConcurrency` carries
// ---------------------------------------------------------------------------

/// One row of the concurrency table: the case, what the `Map` state declares, the
/// execution input, and the status and error the run must reach.
struct ConcurrencyRow {
    case: &'static str,
    declared: Value,
    input: String,
    status: &'static str,
    error: Option<&'static str>,
}

/// A `Map` state's concurrency is held to the range the member carries where the state
/// RESOLVES it as it runs: the published bounds run, a path resolving past the largest
/// one fails the execution with `States.Runtime`, and a bound the definition STATES past
/// it runs under that largest figure.
#[tokio::test]
async fn a_map_states_concurrency_is_held_to_the_range_the_member_carries() {
    let rows: Vec<ConcurrencyRow> = vec![
        ConcurrencyRow {
            case: "the member's own default, which is no limit",
            declared: json!({ "MaxConcurrency": 0 }),
            input: r#"{"items":[1,2]}"#.to_string(),
            status: "SUCCEEDED",
            error: None,
        },
        ConcurrencyRow {
            case: "a limit of one",
            declared: json!({ "MaxConcurrency": 1 }),
            input: r#"{"items":[1,2]}"#.to_string(),
            status: "SUCCEEDED",
            error: None,
        },
        ConcurrencyRow {
            case: "the largest figure the member carries",
            declared: json!({ "MaxConcurrency": MAX_MAP_CONCURRENCY }),
            input: r#"{"items":[1,2]}"#.to_string(),
            status: "SUCCEEDED",
            error: None,
        },
        ConcurrencyRow {
            case: "a declared figure one past the largest the member carries",
            declared: json!({ "MaxConcurrency": MAX_MAP_CONCURRENCY + 1 }),
            input: r#"{"items":[1,2]}"#.to_string(),
            status: "SUCCEEDED",
            error: None,
        },
        ConcurrencyRow {
            case: "a declared figure far past it",
            declared: json!({ "MaxConcurrency": 1_000_000_000_000_i64 }),
            input: r#"{"items":[1,2]}"#.to_string(),
            status: "SUCCEEDED",
            error: None,
        },
        ConcurrencyRow {
            case: "a path resolving to the largest figure the member carries",
            declared: json!({ "MaxConcurrencyPath": "$.limit" }),
            input: json!({ "items": [1, 2], "limit": MAX_MAP_CONCURRENCY }).to_string(),
            status: "SUCCEEDED",
            error: None,
        },
        ConcurrencyRow {
            case: "a path resolving past it",
            declared: json!({ "MaxConcurrencyPath": "$.limit" }),
            input: json!({ "items": [1, 2], "limit": MAX_MAP_CONCURRENCY + 1 }).to_string(),
            status: "FAILED",
            error: Some(RUNTIME_ERROR),
        },
        ConcurrencyRow {
            case: "a path resolving to a negative figure",
            declared: json!({ "MaxConcurrencyPath": "$.limit" }),
            input: json!({ "items": [1, 2], "limit": -1 }).to_string(),
            status: "FAILED",
            error: Some(RUNTIME_ERROR),
        },
    ];

    let emulator = Emulator::new();
    for (
        index,
        ConcurrencyRow {
            case,
            declared,
            input,
            status,
            error,
        },
    ) in rows.into_iter().enumerate()
    {
        let mut state = json!({
            "Type": "Map",
            "ItemsPath": "$.items",
            "ItemProcessor": {
                "StartAt": "Item",
                "States": { "Item": { "Type": "Pass", "Result": "seen", "End": true } },
            },
            "End": true,
        });
        let members = state.as_object_mut().expect("the state is an object");
        for (member, value) in declared.as_object().expect("the declaration is an object") {
            members.insert(member.clone(), value.clone());
        }
        let definition = json!({ "StartAt": "Fan", "States": { "Fan": state } });
        let described = ran(
            &emulator,
            case,
            &format!("map-concurrency-{index}"),
            &definition,
            &input,
        )
        .await;
        assert_eq!(
            string(&described, "status"),
            status,
            "case {case:?}: expected {status}, got {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            error,
            "case {case:?}: expected the error {error:?}, got {described:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// 5. The activity a `Task` state's `Resource` names
// ---------------------------------------------------------------------------

/// A `Task` state's `Resource` naming an activity this emulator HOLDS schedules a task
/// a worker is handed, and one naming an activity that does not exist fails the
/// execution with `States.Runtime` — the cause naming both the state and the activity.
#[tokio::test]
async fn a_task_on_an_activity_that_does_not_exist_fails_the_execution() {
    let emulator = Emulator::new();

    // The control: an activity this emulator holds, whose task a worker really claims.
    let held = create_activity(&emulator, "held-activity").await;
    let machine = create_machine(
        &emulator,
        "activity-held",
        &activity_definition(&held, json!({})),
    )
    .await;
    let running = start_execution(&emulator, &machine, "only", r#"{"work":1}"#).await;
    let (token, input) = claimed(&emulator, "an activity this emulator holds", &held, None).await;
    assert!(
        !token.is_empty(),
        "case \"an activity this emulator holds\": expected a task to be handed out"
    );
    assert_eq!(
        input,
        json!({ "work": 1 }),
        "case \"an activity this emulator holds\": expected the state's input on the task"
    );
    assert_eq!(
        string(&describe_execution(&emulator, &running).await, "status"),
        "RUNNING",
        "case \"an activity this emulator holds\": the execution waits for the worker"
    );

    // And the rule: an activity ARN naming nothing this emulator holds.
    let missing = activity_arn("no-such-activity");
    let machine = create_machine(
        &emulator,
        "activity-missing",
        &activity_definition(&missing, json!({})),
    )
    .await;
    let execution = start_execution(&emulator, &machine, "only", r#"{"work":1}"#).await;
    let case = "an activity that does not exist";
    let described = poll_until_terminal(&emulator, case, &execution).await;
    assert_eq!(
        (
            string(&described, "status"),
            described.get("error").and_then(Value::as_str),
        ),
        ("FAILED".to_string(), Some(RUNTIME_ERROR)),
        "case {case:?}: expected FAILED with {RUNTIME_ERROR}, got {described:?}"
    );
    let cause = string(&described, "cause");
    assert!(
        cause.contains("Work") && cause.ends_with(&format!("The activity {missing} does not exist.")),
        "case {case:?}: expected a cause naming the state and the activity, got {cause:?}"
    );
}

// ---------------------------------------------------------------------------
// 4. The whole seconds a retrier waits
// ---------------------------------------------------------------------------

/// A retrier's wait is the WHOLE seconds of `IntervalSeconds` scaled by `BackoffRate`
/// once per attempt already taken: the fraction is dropped rather than rounded up.
///
/// Asserted through `TestState`, which reports the interval a `RETRIABLE` state owes —
/// so the rule is read off the arithmetic itself and nothing waits for it. The rows
/// differ along the rate (whole and fractional), the attempts already taken and the cap
/// the product has reached.
#[tokio::test]
async fn a_retriers_wait_is_the_whole_seconds_of_what_its_members_compute() {
    // (case, the retrier, the attempts already taken, the seconds it owes)
    let rows: Vec<(&str, Value, i64, i64)> = vec![
        (
            "a whole-second product",
            json!({ "ErrorEquals": ["Broke"], "IntervalSeconds": 5, "BackoffRate": 2.0 }),
            1,
            10,
        ),
        (
            "a fractional product, truncated down",
            json!({ "ErrorEquals": ["Broke"], "IntervalSeconds": 5, "BackoffRate": 1.5 }),
            1,
            7,
        ),
        (
            "the same retrier one attempt further on",
            json!({ "ErrorEquals": ["Broke"], "IntervalSeconds": 5, "BackoffRate": 1.5 }),
            2,
            11,
        ),
        (
            "a rate that leaves a fraction on the first attempt",
            json!({ "ErrorEquals": ["Broke"], "IntervalSeconds": 3, "BackoffRate": 1.1 }),
            1,
            3,
        ),
        (
            "a product the cap holds down, which is already whole",
            json!({
                "ErrorEquals": ["Broke"],
                "IntervalSeconds": 5,
                "BackoffRate": 1.5,
                "MaxDelaySeconds": 8,
            }),
            2,
            8,
        ),
        (
            "the published defaults",
            json!({ "ErrorEquals": ["Broke"] }),
            0,
            1,
        ),
    ];

    let emulator = Emulator::new();
    for (case, retrier, taken, owed) in rows {
        let state = json!({
            "Type": "Task",
            "Resource": "arn:aws:states:::lambda:invoke",
            "Parameters": { "FunctionName": "summer" },
            "Retry": [retrier],
            "End": true,
        });
        let answered = emulator
            .ok(
                "TestState",
                &json!({
                    "definition": state.to_string(),
                    "input": "{}",
                    "roleArn": ROLE_ARN,
                    "inspectionLevel": "DEBUG",
                    "stateConfiguration": { "retrierRetryCount": taken },
                    "mock": { "errorOutput": { "error": "Broke", "cause": "the mock said so" } },
                }),
            )
            .await;
        assert_eq!(
            answered.get("status").and_then(Value::as_str),
            Some("RETRIABLE"),
            "case {case:?}: expected the retrier to owe an attempt, got {answered:?}"
        );
        let details = answered
            .get("inspectionData")
            .and_then(|data| data.get("errorDetails"))
            .unwrap_or_else(|| panic!("case {case:?}: expected errorDetails in {answered:?}"));
        assert_eq!(
            details
                .get("retryBackoffIntervalSeconds")
                .and_then(Value::as_i64),
            Some(owed),
            "case {case:?}: expected a wait of {owed} whole seconds, got {details:?}"
        );
    }
}
