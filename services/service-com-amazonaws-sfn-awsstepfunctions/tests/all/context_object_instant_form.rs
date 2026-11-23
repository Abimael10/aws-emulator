// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the SHAPE of an instant this service writes into a STRING a
//! caller reads, driven over this service's own wire protocol (`awsJson1_0`, `POST /`
//! with `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: an instant the Amazon States Language carries as TEXT — the
//! context object's `Execution.StartTime` and `State.EnteredTime`, which a definition
//! reads through `$$` and which a diagnosis about a path that could not be resolved
//! against that object quotes WHOLE — is written as the millisecond-precision ISO 8601
//! instant Step Functions writes: a UTC calendar instant, a fraction of exactly three
//! digits and the `Z` designator. The fraction is part of the FORM, so its width does not
//! vary with the value of the instant: an instant stamped on a whole tenth of a second
//! carries `.820`, and one stamped on the whole second carries `.000`. A client compares
//! these strings — the context object reaches it inside one cause string — so a rendering
//! whose width is a function of the value cannot be compared at all.
//!
//! The class the rule generalises over is every instant such a document carries, crossed
//! with how it reaches the caller: READ by a payload template into a state's output, and
//! QUOTED inside the cause of a path the object does not carry (from a `Map` state's
//! `ItemSelector`, from an ordinary payload template and from an intrinsic function's path
//! argument). The instants themselves are the clock's, so the successful read is driven
//! over a series of executions rather than one — each stamped at a different
//! millisecond — and the value of every rendered instant is checked against the date the
//! execution's own record reports.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`,
//! `string`) is the one `state_machine_lifecycle` builds; `tests/all/main.rs` picks this
//! file up, so each is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, string, Emulator,
};

/// How often an execution is looked at again, and how long it is waited for at all.
///
/// The wait is for the CONDITION — the execution leaving `RUNNING` — so the deadline is
/// generous and a host that settles one quickly spends none of it: nothing here measures
/// how fast the host runs.
const POLL: Duration = Duration::from_millis(25);
/// See [`POLL`].
const DEADLINE: Duration = Duration::from_secs(120);

/// How many characters the rendering holds: `1970-01-01T00:00:00.000Z`.
const RENDERED_LENGTH: usize = 24;

/// The record of `execution_arn` once it has left `RUNNING`, looked for again until it has
/// or [`DEADLINE`] passes.
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

/// The instant `rendered` names, in whole milliseconds since the Unix epoch, asserting
/// first that it is written in the one form this service writes an instant in: the fixed
/// length, the separators where the form puts them, a fraction of three digits and the
/// UTC designator.
///
/// `label` names which instant it is, so a failure says which of them is misshapen.
fn instant_millis(case: &str, label: &str, rendered: &str) -> i64 {
    assert_eq!(
        rendered.len(),
        RENDERED_LENGTH,
        "case {case:?}: expected {label} to be the {RENDERED_LENGTH}-character ISO 8601 \
         instant, got {rendered:?}"
    );
    assert_eq!(
        (&rendered[10..11], &rendered[19..20], &rendered[23..]),
        ("T", ".", "Z"),
        "case {case:?}: expected {label} to carry the separators of the ISO 8601 form, got \
         {rendered:?}"
    );
    let fraction = &rendered[20..23];
    assert!(
        fraction.len() == 3 && fraction.bytes().all(|digit| digit.is_ascii_digit()),
        "case {case:?}: expected {label} to carry a three-digit fraction, got {rendered:?}"
    );
    ::chrono::DateTime::parse_from_rfc3339(rendered)
        .unwrap_or_else(|why| {
            panic!("case {case:?}: expected {label} to be an instant, got {rendered:?}: {why}")
        })
        .timestamp_millis()
}

/// The instant the record `described` reports in its timestamp member `member`, in whole
/// milliseconds since the Unix epoch: the modelled `Timestamp` the protocol carries as a
/// number of seconds, which is the same instant the text above renders.
fn record_millis(case: &str, described: &Value, member: &str) -> i64 {
    let seconds = described
        .get(member)
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a {member} in {described:?}"));
    (seconds * 1_000.0).round() as i64
}

/// The value the member `member` of a quoted JSON document carries, read out of the one
/// string `cause` is: the diagnosis quotes the context object whole, so the members this
/// suite asks about are read back out of that quotation.
fn quoted(case: &str, cause: &str, member: &str) -> String {
    let opening = format!("\"{member}\":\"");
    let at = cause.find(&opening).unwrap_or_else(|| {
        panic!("case {case:?}: expected the cause to quote a {member}, got {cause:?}")
    }) + opening.len();
    let rest = &cause[at..];
    let end = rest.find('"').unwrap_or_else(|| {
        panic!("case {case:?}: expected the quoted {member} to end, got {cause:?}")
    });
    rest[..end].to_string()
}

// ---------------------------------------------------------------------------
// The instants a payload template READS into a state's output
// ---------------------------------------------------------------------------

/// The state every execution of the successful series runs: a payload template that reads
/// both instants the context object carries as text into the execution's output.
fn reading_definition() -> String {
    json!({
        "StartAt": "Read",
        "States": {
            "Read": {
                "Type": "Pass",
                "Parameters": {
                    "start.$": "$$.Execution.StartTime",
                    "entered.$": "$$.State.EnteredTime",
                },
                "End": true,
            },
        },
    })
    .to_string()
}

/// How many executions the series runs: the instants are the clock's, so the series is
/// what spans the fractions a rendering could drop digits from — a fraction ending in a
/// zero is one millisecond in ten.
const SERIES: usize = 16;

/// Every instant a definition reads out of the context object is the one its own record
/// reports, rendered in the one form, whatever millisecond the clock stamped it at.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_instant_a_definition_reads_is_rendered_in_the_one_form() {
    let emulator = Emulator::new();
    let machine = create_machine(&emulator, "instant-form", &reading_definition()).await;
    let mut started = Vec::with_capacity(SERIES);
    for index in 0..SERIES {
        let name = format!("run-{index}");
        started.push((
            name.clone(),
            start_execution(&emulator, &machine, &name, "{}").await,
        ));
    }

    for (name, execution) in started {
        let case = name.as_str();
        let described = settled(&emulator, case, &execution).await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the execution to succeed, got {described:?}"
        );
        let output: Value = serde_json::from_str(&string(&described, "output"))
            .unwrap_or_else(|why| panic!("case {case:?}: expected a JSON output: {why}"));
        let started_at = record_millis(case, &described, "startDate");
        let stopped_at = record_millis(case, &described, "stopDate");

        // The execution's own start, as the record reports it.
        let start = instant_millis(case, "Execution.StartTime", &string(&output, "start"));
        assert_eq!(
            start, started_at,
            "case {case:?}: expected the StartTime the definition read to be the record's \
             startDate ({started_at} ms), got {start} ms"
        );
        // And the state's entry, which falls inside the run the record bounds.
        let entered = instant_millis(case, "State.EnteredTime", &string(&output, "entered"));
        assert!(
            (started_at..=stopped_at).contains(&entered),
            "case {case:?}: expected the EnteredTime the definition read to fall between the \
             record's startDate ({started_at} ms) and its stopDate ({stopped_at} ms), got \
             {entered} ms"
        );
    }
}

// ---------------------------------------------------------------------------
// The instants a cause QUOTES, when the object carries no such path
// ---------------------------------------------------------------------------

/// The ASL error name the interpreter raises its own failures under.
const RUNTIME_ERROR: &str = "States.Runtime";

/// One row: the state whose member reads a context path that resolves to nothing, and the
/// input the execution runs over.
struct QuotingRow {
    case: &'static str,
    state: Value,
    input: &'static str,
}

/// The rows differ along WHICH member reads the object and how: a `Map` state's
/// `ItemSelector`, an ordinary payload template, and an intrinsic function's path
/// argument.
fn quoting_rows() -> Vec<QuotingRow> {
    vec![
        QuotingRow {
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
            input: r#"{"items":[10,20,30]}"#,
        },
        QuotingRow {
            case: "a payload template's field reading a member of $$.Execution",
            state: json!({
                "Type": "Pass",
                "Parameters": { "bad.$": "$$.Execution.Nope" },
                "End": true,
            }),
            input: "{}",
        },
        QuotingRow {
            case: "an intrinsic function's path argument",
            state: json!({
                "Type": "Pass",
                "Parameters": { "bad.$": "States.Format('{}', $$.State.Nope)" },
                "End": true,
            }),
            input: "{}",
        },
    ]
}

/// The context object a cause quotes carries its instants in the one form too, and they
/// are the instants the execution's own record reports.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_context_object_a_cause_quotes_carries_its_instants_in_the_one_form() {
    let rows = quoting_rows();
    let emulator = Emulator::new();
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let definition = json!({
            "StartAt": "Quoting",
            "States": { "Quoting": row.state.clone() },
        })
        .to_string();
        let machine = create_machine(&emulator, &format!("quoted-{index}"), &definition).await;
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
        let started_at = record_millis(case, &described, "startDate");
        let stopped_at = record_millis(case, &described, "stopDate");

        let start = instant_millis(case, "Execution.StartTime", &quoted(case, &cause, "StartTime"));
        assert_eq!(
            start, started_at,
            "case {case:?}: expected the quoted StartTime to be the record's startDate \
             ({started_at} ms), got {start} ms"
        );
        let entered = instant_millis(
            case,
            "State.EnteredTime",
            &quoted(case, &cause, "EnteredTime"),
        );
        assert!(
            (started_at..=stopped_at).contains(&entered),
            "case {case:?}: expected the quoted EnteredTime to fall between the record's \
             startDate ({started_at} ms) and its stopDate ({stopped_at} ms), got {entered} ms"
        );
    }
}
