// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a state's QUERIES decide when they cannot produce a
//! value, driven over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rules under test:
//!
//! 1. the context object's `$$.State` carries `RetryCount` only for an attempt a
//!    `Retry` re-ran: a state on its first attempt has been retried no times, so the
//!    member is absent from the document a query over `$$` reads;
//! 2. a `Fail` state's `Error`/`Cause` written as JSON null declares no value for it —
//!    null is the absence of a value, so the execution surfaces the same absent error
//!    an omitted member does, and a `Catch` naming an error does not match it;
//! 3. a `Choice` comparator's `*Path` operand must resolve to a value that comparator
//!    can READ as its operand — no comparator reads one from null, and the timestamp
//!    families read their instant out of a string — so such a path fails the execution
//!    `States.Runtime` naming the path, while a value of a type the family merely does
//!    not compare is a non-match that falls through to the `Default`;
//! 4. a JSONata expression that evaluates to NOTHING (the language's `undefined`)
//!    fails the execution `States.QueryEvaluationError` wherever it is written — a
//!    whole field, a member of a template, an element of one — and the
//!    `EvaluationFailed` event locates it, reporting the BASE field of the state when
//!    the path to it does not fit the member the location leaves through;
//! 5. a JSONata `Arguments` that evaluates to anything but a JSON object names no
//!    request for the call to make, which is the state's own `States.Runtime` failure.
//!
//! The harness (`Emulator`, `create_machine`, `describe_execution`, `start_execution`)
//! is the one `state_machine_lifecycle` builds and the history reader is
//! `execution_history`'s; `tests/all/main.rs` picks this file up, so each is reached
//! through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::MAX_LOCATION_CHARS;
use service_com_amazonaws_sfn_awsstepfunctions::state::EVENT_EVALUATION_FAILED;

use crate::execution_history::history;
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, Emulator,
};

/// The ASL error name the interpreter raises its own failures under.
const RUNTIME_ERROR: &str = "States.Runtime";

/// The ASL error name an expression that could not be evaluated fails under.
const QUERY_EVALUATION_ERROR: &str = "States.QueryEvaluationError";

/// How often a wait looks at the execution again, and how long it waits at all.
///
/// The wait is for the CONDITION — the execution leaving `RUNNING` — so the deadline is
/// generous and a host that settles an execution quickly spends none of it: nothing
/// here measures how fast the host runs.
const SETTLE_POLL: Duration = Duration::from_millis(25);
/// See [`SETTLE_POLL`].
const SETTLE_DEADLINE: Duration = Duration::from_secs(60);

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

/// Run `definition` over `input` once and report what the execution ended as.
async fn ran(emulator: &Emulator, name: &str, definition: &str, input: &str) -> Value {
    let machine = create_machine(emulator, name, definition).await;
    let execution = start_execution(emulator, &machine, "only", input).await;
    settled(emulator, name, &execution).await
}

/// The output of a settled execution, as the document it carries.
fn output(described: &Value) -> Value {
    described
        .get("output")
        .and_then(Value::as_str)
        .map(|text| serde_json::from_str(text).expect("the output is JSON"))
        .unwrap_or(Value::Null)
}

// ---------------------------------------------------------------------------
// 1. `$$.State.RetryCount` is there only once a Retry has re-run the state
// ---------------------------------------------------------------------------

/// `$$.State` omits `RetryCount` on an attempt no `Retry` re-ran and carries the
/// attempt once one has, and it carries the state's name and entry instant either way.
///
/// The rows vary the dimension the rule generalises over: how many attempts the state
/// has taken by the time the context object is read — none (the first attempt of a
/// state with no `Retry` at all, and the first attempt of one that declares one) and
/// one (the attempt a retrier re-ran).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_context_object_carries_a_retry_count_only_once_a_retry_re_ran_the_state() {
    // (case, the state that reads `$$.State`, the attempt the member must report)
    let rows: Vec<(&str, Value, Option<u64>)> = vec![
        (
            "a state with no Retry at all",
            json!({
                "Type": "Pass",
                "Parameters": { "state.$": "$$.State" },
                "End": true,
            }),
            None,
        ),
        (
            "the first attempt of a state that declares a Retry",
            json!({
                "Type": "Map",
                "Retry": [{ "ErrorEquals": ["States.ALL"], "MaxAttempts": 2 }],
                "ItemSelector": { "state.$": "$$.State" },
                "ItemProcessor": {
                    "StartAt": "Each",
                    "States": { "Each": { "Type": "Pass", "End": true } }
                },
                "OutputPath": "$[0]",
                "End": true,
            }),
            None,
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, state, expected)) in rows.iter().enumerate() {
        let definition = json!({ "StartAt": "Ctx", "States": { "Ctx": state } }).to_string();
        // The `Map` row iterates ONE item so that its single iteration's projection is
        // the document read back; every other row reads the context object directly.
        let input = match state.get("Type").and_then(Value::as_str) {
            Some("Map") => "[1]",
            _ => "{}",
        };
        let described = ran(
            &emulator,
            &format!("retry-count-{index}"),
            &definition,
            input,
        )
        .await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: reading the context object succeeds, got {described:?}"
        );
        let state = output(&described)
            .get("state")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected the state document"));
        assert_eq!(
            state.get("Name").and_then(Value::as_str),
            Some("Ctx"),
            "case {case:?}: expected the state's own name, got {state}"
        );
        assert!(
            state.get("EnteredTime").is_some_and(Value::is_string),
            "case {case:?}: expected the instant the state was entered, got {state}"
        );
        let carried = state.get("RetryCount").and_then(Value::as_u64);
        assert_eq!(
            carried, *expected,
            "case {case:?}: expected RetryCount {expected:?}, got {carried:?}"
        );
    }

    // And the attempt a retrier re-ran reads the attempt it is on: the first attempt
    // fails because the member is absent, the retry supplies it, and the output says
    // which attempt produced it.
    let definition = json!({
        "StartAt": "Try",
        "States": {
            "Try": {
                "Type": "Map",
                "Retry": [{ "ErrorEquals": ["NeedAnotherTry"], "MaxAttempts": 2,
                            "IntervalSeconds": 0 }],
                "ItemSelector": { "state.$": "$$.State" },
                "ItemProcessor": {
                    "StartAt": "Decide",
                    "States": {
                        "Decide": {
                            "Type": "Choice",
                            "Choices": [{ "Variable": "$.state.RetryCount",
                                          "IsPresent": false, "Next": "Again" }],
                            "Default": "Report",
                        },
                        "Report": {
                            "Type": "Pass",
                            "Parameters": { "attempt.$": "$.state.RetryCount" },
                            "End": true,
                        },
                        "Again": { "Type": "Fail", "Error": "NeedAnotherTry" },
                    }
                },
                "End": true,
            }
        }
    })
    .to_string();
    let described = ran(&emulator, "retry-count-retried", &definition, "[1]").await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case \"the attempt a retrier re-ran\": expected the retry to succeed, got {described:?}"
    );
    let produced = output(&described);
    assert_eq!(
        produced,
        json!([{ "attempt": 1 }]),
        "case \"the attempt a retrier re-ran\": expected the attempt the state is on, got \
         {produced}"
    );
}

// ---------------------------------------------------------------------------
// 2. A `Fail` state's null `Error`/`Cause` declares none
// ---------------------------------------------------------------------------

/// A `Fail` state whose `Error` or `Cause` is JSON null reports that member as absent,
/// and an absent error is not matched by a `Catch` that names one.
///
/// The rows vary which of the two members is null and whether the other carries a
/// value — the dimensions the rule generalises over — with a state declaring neither
/// as the control.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fail_state_member_written_as_null_declares_no_value() {
    // (case, the Fail state, the error it must report, the cause it must report)
    let rows: Vec<(&str, Value, Option<&str>, Option<&str>)> = vec![
        (
            "Error null and no Cause",
            json!({ "Type": "Fail", "Error": null }),
            None,
            None,
        ),
        (
            "Error null beside a Cause",
            json!({ "Type": "Fail", "Error": null, "Cause": "told" }),
            None,
            Some("told"),
        ),
        (
            "Cause null beside an Error",
            json!({ "Type": "Fail", "Error": "Named", "Cause": null }),
            Some("Named"),
            None,
        ),
        (
            "both members null",
            json!({ "Type": "Fail", "Error": null, "Cause": null }),
            None,
            None,
        ),
        (
            "neither member declared",
            json!({ "Type": "Fail" }),
            None,
            None,
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, state, error, cause)) in rows.iter().enumerate() {
        // The `Fail` state sits inside a `Parallel` whose `Catch` names a specific
        // error: an absent error matches no such catcher, so the execution fails.
        let definition = json!({
            "StartAt": "Fan",
            "States": {
                "Fan": {
                    "Type": "Parallel",
                    "Branches": [{ "StartAt": "Stop", "States": { "Stop": state } }],
                    "Catch": [{ "ErrorEquals": ["Named.Other"], "Next": "Caught" }],
                    "End": true,
                },
                "Caught": { "Type": "Pass", "Result": { "caught": true }, "End": true },
            }
        })
        .to_string();
        let described = ran(&emulator, &format!("null-fail-{index}"), &definition, "{}").await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: a catcher naming another error does not catch it, got {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            *error,
            "case {case:?}: expected error {error:?}, got {:?}",
            described.get("error")
        );
        assert_eq!(
            described.get("cause").and_then(Value::as_str),
            *cause,
            "case {case:?}: expected cause {cause:?}, got {:?}",
            described.get("cause")
        );
    }
}

// ---------------------------------------------------------------------------
// 3. A `Choice` comparator's `*Path` operand
// ---------------------------------------------------------------------------

/// A `*Path` operand that resolves to a value the comparator cannot read as its
/// operand fails the execution and names the path; one of a type the family merely
/// does not compare falls through to the `Default`.
///
/// The rows span the comparator families the language defines (`smithy_query` carries
/// no ASL; the families are the Amazon States Language's `StringEquals*`,
/// `NumericEquals*`, `BooleanEquals*` and `Timestamp*Path` comparators) and vary what
/// the operand path resolves to: null, a value of another type, and a value of the
/// comparator's own type.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_choice_operand_path_must_resolve_to_a_value_the_comparator_can_read() {
    /// What a row must end as.
    #[derive(Debug, PartialEq, Eq)]
    enum Decided {
        /// The rule matched, so its own branch ran.
        Rule,
        /// No rule matched, so the `Default` ran.
        Default,
        /// The rule could not be decided, so the execution failed.
        Failed,
    }

    // (case, the rule, the input, what the run must decide)
    let rows: Vec<(&str, Value, &str, Decided)> = vec![
        (
            "StringEqualsPath whose operand is null",
            json!({ "Variable": "$.s", "StringEqualsPath": "$.other", "Next": "Then" }),
            r#"{"s":"a","other":null}"#,
            Decided::Failed,
        ),
        (
            "NumericEqualsPath whose operand is null",
            json!({ "Variable": "$.n", "NumericEqualsPath": "$.other", "Next": "Then" }),
            r#"{"n":1,"other":null}"#,
            Decided::Failed,
        ),
        (
            "BooleanEqualsPath whose operand is null",
            json!({ "Variable": "$.b", "BooleanEqualsPath": "$.other", "Next": "Then" }),
            r#"{"b":true,"other":null}"#,
            Decided::Failed,
        ),
        (
            "TimestampGreaterThanPath whose operand is null",
            json!({ "Variable": "$.t", "TimestampGreaterThanPath": "$.other", "Next": "Then" }),
            r#"{"t":"2026-01-01T00:00:00Z","other":null}"#,
            Decided::Failed,
        ),
        (
            "TimestampGreaterThanPath whose operand is a number",
            json!({ "Variable": "$.t", "TimestampGreaterThanPath": "$.other", "Next": "Then" }),
            r#"{"t":"2026-01-01T00:00:00Z","other":1767225600}"#,
            Decided::Failed,
        ),
        (
            "TimestampLessThanPath whose operand is an object",
            json!({ "Variable": "$.t", "TimestampLessThanPath": "$.other", "Next": "Then" }),
            r#"{"t":"2026-01-01T00:00:00Z","other":{"at":"2026-01-02T00:00:00Z"}}"#,
            Decided::Failed,
        ),
        (
            // The operand is of a type the numeric family does not compare, which is a
            // judgement about the DATA: the rule does not match and the run goes on.
            "NumericEqualsPath whose operand is a string",
            json!({ "Variable": "$.n", "NumericEqualsPath": "$.other", "Next": "Then" }),
            r#"{"n":1,"other":"1"}"#,
            Decided::Default,
        ),
        (
            "StringEqualsPath whose operand is a number",
            json!({ "Variable": "$.s", "StringEqualsPath": "$.other", "Next": "Then" }),
            r#"{"s":"1","other":1}"#,
            Decided::Default,
        ),
        (
            "TimestampGreaterThanPath whose operand is an earlier instant",
            json!({ "Variable": "$.t", "TimestampGreaterThanPath": "$.other", "Next": "Then" }),
            r#"{"t":"2026-01-02T00:00:00Z","other":"2026-01-01T00:00:00Z"}"#,
            Decided::Rule,
        ),
        (
            "NumericEqualsPath whose operand is the same number",
            json!({ "Variable": "$.n", "NumericEqualsPath": "$.other", "Next": "Then" }),
            r#"{"n":1,"other":1}"#,
            Decided::Rule,
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, rule, input, expected)) in rows.iter().enumerate() {
        let definition = json!({
            "StartAt": "Route",
            "States": {
                "Route": { "Type": "Choice", "Choices": [rule], "Default": "Fell" },
                "Then": { "Type": "Pass", "Result": { "took": "rule" }, "End": true },
                "Fell": { "Type": "Pass", "Result": { "took": "default" }, "End": true },
            }
        })
        .to_string();
        let described = ran(&emulator, &format!("operand-{index}"), &definition, input).await;
        let status = described.get("status").and_then(Value::as_str);
        match expected {
            Decided::Failed => {
                assert_eq!(
                    status,
                    Some("FAILED"),
                    "case {case:?}: an operand the comparator cannot read fails the execution, \
                     got {described:?}"
                );
                assert_eq!(
                    described.get("error").and_then(Value::as_str),
                    Some(RUNTIME_ERROR),
                    "case {case:?}: expected {RUNTIME_ERROR}, got {described:?}"
                );
                let reported = described
                    .get("cause")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                let path = rule
                    .as_object()
                    .and_then(|rule| {
                        rule.iter()
                            .find(|(member, _)| member.ends_with("Path"))
                            .and_then(|(_, path)| path.as_str())
                    })
                    .expect("the row's rule carries a *Path comparator");
                assert!(
                    reported.contains(path),
                    "case {case:?}: the cause must name the operand path {path:?}, got \
                     {reported:?}"
                );
            }
            taken => {
                assert_eq!(
                    status,
                    Some("SUCCEEDED"),
                    "case {case:?}: a judgement about the data decides rather than failing, got \
                     {described:?}"
                );
                let took = match taken {
                    Decided::Rule => "rule",
                    _ => "default",
                };
                assert_eq!(
                    output(&described),
                    json!({ "took": took }),
                    "case {case:?}: expected the {took:?} branch, got {described:?}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// 4. A JSONata expression that produced nothing
// ---------------------------------------------------------------------------

/// A JSONata expression that evaluates to `undefined` fails the execution with
/// `States.QueryEvaluationError` wherever it is written, and the `EvaluationFailed`
/// event names the field it is written in.
///
/// The rows vary WHERE the expression sits — the whole value of a field, a member of a
/// template, a member nested inside one, an element of an array, an `Assign` member
/// and a `Task`'s `Arguments` member — which is the dimension the rule generalises
/// over, with an expression that DOES produce a value as the control.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_jsonata_expression_that_produced_nothing_fails_the_execution() {
    // (case, the state, the field the failure must be located at, or `None` for the
    // control row that evaluates)
    let rows: Vec<(&str, Value, Option<&str>)> = vec![
        (
            "an Output that is one whole expression",
            json!({ "Type": "Pass", "Output": "{% $states.input.absent %}", "End": true }),
            Some("Output"),
        ),
        (
            "an Output member",
            json!({
                "Type": "Pass",
                "Output": { "kept": "literal", "missing": "{% $states.input.absent %}" },
                "End": true,
            }),
            Some("Output/missing"),
        ),
        (
            "an Output member nested inside another",
            json!({
                "Type": "Pass",
                "Output": { "detail": { "why": "{% $states.input.absent %}" } },
                "End": true,
            }),
            Some("Output/detail/why"),
        ),
        (
            "an element of an Output array",
            json!({
                "Type": "Pass",
                "Output": { "all": ["kept", "{% $states.input.absent %}"] },
                "End": true,
            }),
            Some("Output/all[1]"),
        ),
        (
            "an Assign member",
            json!({
                "Type": "Pass",
                "Assign": { "picked": "{% $states.input.absent %}" },
                "End": true,
            }),
            Some("Assign/picked"),
        ),
        (
            "a Succeed state's Output",
            json!({ "Type": "Succeed", "Output": "{% $states.input.absent %}" }),
            Some("Output"),
        ),
        (
            "an Output that produces a value",
            json!({
                "Type": "Pass",
                "Output": { "seen": "{% $states.input.here %}" },
                "End": true,
            }),
            None,
        ),
    ];

    let emulator = Emulator::new();
    for (index, (case, state, located)) in rows.iter().enumerate() {
        let definition = json!({
            "QueryLanguage": "JSONata",
            "StartAt": "Query",
            "States": { "Query": state },
        })
        .to_string();
        let machine = create_machine(&emulator, &format!("undefined-{index}"), &definition).await;
        let execution = start_execution(&emulator, &machine, "only", r#"{"here":1}"#).await;
        let described = settled(&emulator, case, &execution).await;
        let events = history(&emulator, &execution, json!({})).await;
        let failures: Vec<&Value> = events
            .iter()
            .filter(|event| {
                event.get("type").and_then(Value::as_str) == Some(EVENT_EVALUATION_FAILED)
            })
            .collect();

        let Some(located) = located else {
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some("SUCCEEDED"),
                "case {case:?}: an expression that produced a value runs on, got {described:?}"
            );
            assert_eq!(
                output(&described),
                json!({ "seen": 1 }),
                "case {case:?}: expected the value the expression produced, got {described:?}"
            );
            assert!(
                failures.is_empty(),
                "case {case:?}: expected no evaluation failure, got {failures:?}"
            );
            continue;
        };

        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: an expression that produced nothing fails the execution, got \
             {described:?}"
        );
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            Some(QUERY_EVALUATION_ERROR),
            "case {case:?}: expected {QUERY_EVALUATION_ERROR}, got {described:?}"
        );
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
            Some(*located),
            "case {case:?}: expected the field the expression is written in, got {details:?}"
        );
        assert_eq!(
            details.get("state").and_then(Value::as_str),
            Some("Query"),
            "case {case:?}: expected the state the expression is written in, got {details:?}"
        );
        assert_eq!(
            details.get("cause").and_then(Value::as_str),
            described.get("cause").and_then(Value::as_str),
            "case {case:?}: the event and the execution report one cause, got {details:?}"
        );
    }
}

/// The `location` an `EvaluationFailed` event reports is the field path when it fits
/// the member it leaves through and the state's BASE field when it does not, and the
/// execution fails either way.
///
/// The rows are the published bound on that member (`EvaluationFailureLocation`,
/// length 0..256, from
/// `smithy_query(query="shape",shape="EvaluationFailedEventDetails")`): a path well
/// inside it, one exactly at it, one a character past it, and one far past it. The
/// dimension is the length of the template member the expression is written in, which
/// a definition chooses.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_evaluation_failure_locates_the_field_within_the_member_it_leaves_through() {
    /// The field an `Output` member of `name` is located at.
    fn located_at(name: &str) -> String {
        format!("Output/{name}")
    }

    let at_bound = MAX_LOCATION_CHARS - located_at("").chars().count();
    let rows: Vec<(&str, usize)> = vec![
        ("a path well inside the bound", 8),
        ("a path one character inside the bound", at_bound - 1),
        ("a path exactly at the bound", at_bound),
        ("a path one character past the bound", at_bound + 1),
        ("a path far past the bound", at_bound * 3),
    ];

    let emulator = Emulator::new();
    for (index, (case, length)) in rows.iter().enumerate() {
        let member = "m".repeat(*length);
        let path = located_at(&member);
        let fits = path.chars().count() <= MAX_LOCATION_CHARS;
        let definition = json!({
            "QueryLanguage": "JSONata",
            "StartAt": "Query",
            "States": {
                "Query": {
                    "Type": "Pass",
                    "Output": { member.clone(): "{% $states.input.absent %}" },
                    "End": true,
                }
            },
        })
        .to_string();
        let machine = create_machine(&emulator, &format!("location-{index}"), &definition).await;
        let execution = start_execution(&emulator, &machine, "only", "{}").await;
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("FAILED"),
            "case {case:?}: the expression produced nothing, so the execution fails, got \
             {described:?}"
        );
        let events = history(&emulator, &execution, json!({})).await;
        let located = events
            .iter()
            .find(|event| {
                event.get("type").and_then(Value::as_str) == Some(EVENT_EVALUATION_FAILED)
            })
            .and_then(|event| event.get("evaluationFailedEventDetails"))
            .and_then(|details| details.get("location"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected a located failure in {events:?}"))
            .to_string();
        let expected = match fits {
            true => path.clone(),
            false => "Output".to_string(),
        };
        assert_eq!(
            located, expected,
            "case {case:?}: expected the location {expected:?}, got {located:?}"
        );
        assert!(
            located.chars().count() <= MAX_LOCATION_CHARS,
            "case {case:?}: the location leaves through a member of {MAX_LOCATION_CHARS} \
             characters, got {}",
            located.chars().count()
        );
    }
}

// ---------------------------------------------------------------------------
// 5. `Arguments` is the document the work is handed
// ---------------------------------------------------------------------------

/// A JSONata `Arguments` that evaluates to anything but a JSON object fails the
/// execution `States.Runtime`: the member is the document the call is made with, and a
/// value of another kind names no request.
///
/// The rows span the JSON kinds an expression can produce — string, number, boolean,
/// array — with an object as the control.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn task_arguments_that_are_not_an_object_fail_the_execution() {
    // (case, the Arguments expression, whether the state may run)
    let rows: Vec<(&str, Value, bool)> = vec![
        ("a string", json!("{% 'hello' %}"), false),
        ("a number", json!("{% 1 + 1 %}"), false),
        ("a boolean", json!("{% true %}"), false),
        ("an array", json!("{% [1, 2] %}"), false),
        ("an object", json!("{% { 'Name': 'ok' } %}"), true),
    ];

    let emulator = Emulator::new();
    for (index, (case, arguments, runs)) in rows.iter().enumerate() {
        let definition = json!({
            "QueryLanguage": "JSONata",
            "StartAt": "T",
            "States": {
                "T": {
                    "Type": "Task",
                    // An activity this emulator holds no record of: the request is
                    // built — and judged — before anything is called, so the row that
                    // runs fails at the activity instead and never at `Arguments`.
                    "Resource": "arn:aws:states:::aws-sdk:sfn:listStateMachines",
                    "Arguments": arguments,
                    "End": true,
                }
            }
        })
        .to_string();
        let described = ran(&emulator, &format!("arguments-{index}"), &definition, "{}").await;
        let cause = described
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !*runs {
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some("FAILED"),
                "case {case:?}: Arguments of another kind fails the execution, got {described:?}"
            );
            assert_eq!(
                described.get("error").and_then(Value::as_str),
                Some(RUNTIME_ERROR),
                "case {case:?}: expected {RUNTIME_ERROR}, got {described:?}"
            );
            assert!(
                cause.contains("Arguments"),
                "case {case:?}: the cause names the member that cannot carry it, got {cause:?}"
            );
            continue;
        }
        assert_eq!(
            described.get("error").and_then(Value::as_str),
            None,
            "case {case:?}: an object Arguments is handed over, so nothing refuses it, got \
             {described:?}"
        );
    }
}
