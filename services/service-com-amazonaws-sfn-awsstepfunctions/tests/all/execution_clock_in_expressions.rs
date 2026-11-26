// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the clock an execution's own expressions read, driven over this
//! service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: every clock an execution reads is the EXECUTION's own, and that
//! clock advances by each span the execution is charged — the seconds a `Wait` state
//! declares, and the interval a `Retry` waits between two attempts of a state. The
//! query language's current-instant functions read it just as the context object's
//! instants do, so a definition that measures elapsed time against the instant its own
//! run started measures the spans that run was charged, and a condition that becomes true
//! only after a span really becomes true once that span has been charged.
//!
//! The rows differ along what the rule generalises over: the span charged (none, one
//! second, several), what charges it (a `Wait` state, a retrier's interval), which state
//! kind carries the retrier (a `Parallel`, a `Task`), and whether the attempts a retrier
//! allows add up to the span the condition waits for.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`) is the one `state_machine_lifecycle` builds; `tests/all/main.rs` picks this
//! file up, so each is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator,
};

/// The error a gated branch fails with before its span has been charged, which the
/// retrier names.
const GATE_ERROR: &str = "Gate.TooEarly";

/// How long a row waits for an execution to reach a terminal status: generous, and never
/// the span a row is about — the spans below are owed to the emulator's own clock, so
/// what a poll waits for is the STATUS and not this figure.
const SETTLE_DEADLINE: Duration = Duration::from_secs(120);

/// Poll `execution` until it reports a status other than `RUNNING`, and answer that
/// record. Panics naming `case` when [`SETTLE_DEADLINE`] passes first.
async fn settled(emulator: &Emulator, case: &str, execution: &str) -> Value {
    let deadline = Instant::now() + SETTLE_DEADLINE;
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

/// The expression that measures the execution's own elapsed span, in milliseconds.
const ELAPSED: &str =
    "$toMillis($now()) - $toMillis($states.context.Execution.StartTime)";

/// A JSONata definition that waits `seconds` and then puts out the span its own clock has
/// advanced by.
fn waiting_then_measuring(seconds: i64) -> String {
    json!({
        "QueryLanguage": "JSONata",
        "StartAt": "Hold",
        "States": {
            "Hold": { "Type": "Wait", "Seconds": seconds, "Next": "Measure" },
            "Measure": {
                "Type": "Pass",
                "Output": format!("{{% {{ \"elapsed\": {ELAPSED} }} %}}"),
                "End": true,
            },
        },
    })
    .to_string()
}

/// The span a `Wait` state declares is charged to the execution's clock, which its own
/// expressions then read.
#[tokio::test]
async fn a_waits_span_is_charged_to_the_clock_the_expressions_read() {
    // (case, the seconds the Wait declares)
    let rows: [(&str, i64); 4] = [
        ("no wait at all, so nothing is charged", 0),
        ("one second", 1),
        ("two seconds", 2),
        ("three seconds", 3),
    ];

    let emulator = Emulator::new();
    let mut started = Vec::with_capacity(rows.len());
    for (index, (_, seconds)) in rows.iter().enumerate() {
        let machine = create_machine(
            &emulator,
            &format!("charged-{index}"),
            &waiting_then_measuring(*seconds),
        )
        .await;
        started.push(start_execution(&emulator, &machine, "only", "{}").await);
    }
    for ((case, seconds), execution) in rows.iter().zip(started) {
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the run to succeed, got {described:?}"
        );
        let output: Value = serde_json::from_str(&string(&described, "output"))
            .unwrap_or_else(|why| panic!("case {case:?}: expected JSON output: {why}"));
        let elapsed = output
            .get("elapsed")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: expected an elapsed span, got {output:?}"));
        let charged = seconds * 1_000;
        assert!(
            (charged..charged + 1_000).contains(&elapsed),
            "case {case:?}: expected the expression to read the {charged} ms charged to the \
             run, got {elapsed}"
        );
    }
}

/// A definition whose `kind` state (a `Parallel` or a `Task`) is gated on the execution
/// having been charged `threshold` milliseconds and is retried every `interval` seconds,
/// at most `attempts` times.
fn gated_and_retried(kind: &str, threshold: i64, interval: i64, attempts: i64) -> String {
    let gate = json!({
        "StartAt": "Gate",
        "States": {
            "Gate": {
                "Type": "Choice",
                "Choices": [{
                    "Condition": format!("{{% {ELAPSED} >= {threshold} %}}"),
                    "Next": "Steady",
                }],
                "Default": "Flake",
            },
            "Flake": { "Type": "Fail", "Error": GATE_ERROR, "Cause": "not charged yet" },
            "Steady": { "Type": "Pass", "Output": "steady", "End": true },
        },
    });
    let retry = json!([{
        "ErrorEquals": [GATE_ERROR],
        "IntervalSeconds": interval,
        "MaxAttempts": attempts,
        "BackoffRate": 1.0,
    }]);
    // A `Parallel` carries the gate as a branch beside a branch that always succeeds, so
    // the retry re-runs the whole state; a `Map` over one item carries it as its
    // processor. Both are states the language gives a `Retry`.
    let attempt = match kind {
        "Parallel" => json!({
            "Type": "Parallel",
            "Branches": [
                { "StartAt": "Sibling", "States": { "Sibling": { "Type": "Pass", "Output": "sibling", "End": true } } },
                gate,
            ],
            "Retry": retry,
            "End": true,
        }),
        // "Map" is the only other kind these rows use.
        _ => json!({
            "Type": "Map",
            "Items": "{% [1] %}",
            "ItemProcessor": {
                "ProcessorConfig": { "Mode": "INLINE" },
                "StartAt": gate["StartAt"].clone(),
                "States": gate["States"].clone(),
            },
            "Retry": retry,
            "End": true,
        }),
    };
    json!({
        "QueryLanguage": "JSONata",
        "StartAt": "Attempt",
        "States": { "Attempt": attempt },
    })
    .to_string()
}

/// A retrier's interval is charged to that same clock, so a state gated on a span clears
/// its gate on the attempt the interval has carried it past — and does not clear one the
/// attempts it allows cannot reach.
#[tokio::test]
async fn a_retriers_interval_is_charged_to_the_same_clock() {
    // (case, the state kind, the gate in ms, the interval in seconds, the attempts,
    //  whether the run must succeed)
    let rows: Vec<(&str, &str, i64, i64, i64, bool)> = vec![
        (
            "a Parallel whose second attempt is past the gate",
            "Parallel",
            1_000,
            2,
            2,
            true,
        ),
        (
            "a Parallel whose gate the first interval exactly reaches",
            "Parallel",
            1_000,
            1,
            2,
            true,
        ),
        (
            "a Parallel needing two intervals to pass the gate",
            "Parallel",
            1_500,
            1,
            2,
            true,
        ),
        (
            "a Parallel whose allowed attempts never reach the gate",
            "Parallel",
            9_000,
            1,
            2,
            false,
        ),
        (
            "a Map, the other kind the language gives a Retry",
            "Map",
            1_000,
            2,
            2,
            true,
        ),
    ];

    let emulator = Emulator::new();
    let mut started = Vec::with_capacity(rows.len());
    for (index, (_, kind, threshold, interval, attempts, _)) in rows.iter().enumerate() {
        let machine = create_machine(
            &emulator,
            &format!("retried-{index}"),
            &gated_and_retried(kind, *threshold, *interval, *attempts),
        )
        .await;
        started.push(start_execution(&emulator, &machine, "only", "{}").await);
    }
    for ((case, _, _, _, _, succeeds), execution) in rows.iter().zip(started) {
        let described = settled(&emulator, case, &execution).await;
        let status = described.get("status").and_then(Value::as_str);
        match succeeds {
            true => assert_eq!(
                status,
                Some("SUCCEEDED"),
                "case {case:?}: expected the retried state to clear its gate, got {described:?}"
            ),
            false => assert_eq!(
                (status, described.get("error").and_then(Value::as_str)),
                (Some("FAILED"), Some(GATE_ERROR)),
                "case {case:?}: expected the gate to hold for every allowed attempt, got \
                 {described:?}"
            ),
        }
    }
}
