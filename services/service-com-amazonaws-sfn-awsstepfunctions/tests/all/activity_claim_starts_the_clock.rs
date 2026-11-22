// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHERE an activity task's `TimeoutSeconds` clock has its ORIGIN,
//! driven over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: the span an activity-backed `Task` state allows its task runs
//! from the instant a worker CLAIMS that task and not from the instant the state
//! scheduled it, so while no worker holds it the task is bounded by nothing — the
//! execution stays `RUNNING` however many multiples of the span pass, its history holds
//! the one `ActivityScheduled` carrying the span the definition declared and neither a
//! start nor a closure, and a later poll is still handed the token together with the
//! input the execution was started on — while a task a worker claimed and abandoned is
//! closed that span after the CLAIM: the execution fails with the language's own timeout
//! error and reports no output, the one closure event carries that same error name, and a
//! worker that answers inside the span completes the execution with its own output
//! instead.
//!
//! The rows differ along what the rule generalises over: the span the state declares, how
//! many multiples of it pass with nobody polling, the shape of the input the claim has to
//! hand back, and whether the worker that finally claims the task answers it at all.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`) is
//! the one `state_machine_lifecycle` builds, the activity helpers are `activity_tasks`'
//! and the history reader is `execution_history`'s; `tests/all/main.rs` is this binary's
//! crate root, so each is reached through `crate::<suite>`.

use std::time::{Duration, Instant};

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::TIMEOUT_ERROR;
use service_com_amazonaws_sfn_awsstepfunctions::state::{
    EVENT_ACTIVITY_SCHEDULED, EVENT_ACTIVITY_STARTED, EVENT_ACTIVITY_SUCCEEDED,
    EVENT_ACTIVITY_TIMED_OUT,
};

use crate::activity_tasks::{activity_definition, create_activity, poll};
use crate::execution_history::history;
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, Emulator,
};

/// The status an execution waiting at an activity task reports.
const RUNNING: &str = "RUNNING";

/// How long one sample of the unclaimed window pauses before looking again.
const SAMPLE_PAUSE: Duration = Duration::from_millis(200);

/// How long a poll for a terminal status waits before giving up: generous next to every
/// span the rows declare, because what it waits for is the STATUS and never this span.
const TERMINAL_DEADLINE: Duration = Duration::from_secs(120);

/// How long one poll for a terminal status pauses between looks.
const POLL_PAUSE: Duration = Duration::from_millis(25);

/// What the recorded stamps are rounded to, which is all a comparison of two of them can
/// resolve: the modelled `Timestamp` the wire carries them as is whole milliseconds.
const STAMP_RESOLUTION_SECONDS: f64 = 0.001;

/// The name the rows' workers poll under, which the service only logs.
const WORKER: &str = "clock-origin-worker";

/// What the worker of the row that answers in time reports.
const ANSWERED_OUTPUT: &str = r#"{"claimed":"and answered"}"#;

/// What a row's worker does once it has claimed the task.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Worker {
    /// It abandons the task, so the span the state allowed it is what closes it.
    Abandons,
    /// It answers inside that span, so its own report is the state's outcome.
    Answers,
}

/// One row: the span the state declares, how long nobody polls for, the input the
/// execution is started on, and what the worker that claims the task then does.
struct Row {
    case: &'static str,
    /// The `TimeoutSeconds` the `Task` state declares, in whole seconds.
    bound_seconds: u64,
    /// How many multiples of that span pass with NO worker polling, so the window is
    /// stated in the rule's own unit rather than in seconds of its own.
    unclaimed_multiples: u32,
    /// The document `StartExecution` is given, which the claim must hand back byte for
    /// byte.
    input: &'static str,
    worker: Worker,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a one-second span, three multiples unclaimed, then abandoned",
            bound_seconds: 1,
            unclaimed_multiples: 3,
            input: r#"{"leg":"unclaimed-then-claimed"}"#,
            worker: Worker::Abandons,
        },
        Row {
            case: "a two-second span, two multiples unclaimed, then abandoned",
            bound_seconds: 2,
            unclaimed_multiples: 2,
            input: r#"{"nested":{"items":[1,2,3]},"text":"ünïcode ✓"}"#,
            worker: Worker::Abandons,
        },
        Row {
            case: "a one-second span, four multiples unclaimed, then answered in time",
            bound_seconds: 1,
            unclaimed_multiples: 4,
            input: "{}",
            worker: Worker::Answers,
        },
    ]
}

/// RULE: the span runs from the CLAIM.
///
/// Each row observes the task through the whole unclaimed window, claims it afterwards and
/// then grades what the claim started: the execution's outcome, the events the task wrote,
/// and the stamps those events carry.
#[tokio::test]
async fn an_activity_tasks_span_runs_from_the_claim_and_not_from_the_schedule() {
    let emulator = Emulator::new();

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            bound_seconds,
            unclaimed_multiples,
            input,
            worker,
        } = row;
        let bound = Duration::from_secs(bound_seconds);
        let unclaimed_window = bound * unclaimed_multiples;

        let activity = create_activity(&emulator, &format!("clock-origin-{index}")).await;
        let definition = activity_definition(&activity, json!({ "TimeoutSeconds": bound_seconds }));
        let machine =
            create_machine(&emulator, &format!("clock-origin-machine-{index}"), &definition).await;
        let execution = start_execution(&emulator, &machine, "only", input).await;

        // The whole unclaimed window, sampled: with nobody polling, the task has started
        // nothing, so there is no run for the span to be longer than and the execution
        // stays RUNNING across every multiple of it.
        let window_opened = Instant::now();
        while window_opened.elapsed() < unclaimed_window {
            let described = describe_execution(&emulator, &execution).await;
            assert_eq!(
                described.get("status").and_then(Value::as_str),
                Some(RUNNING),
                "case {case:?}: expected the execution to stay {RUNNING} while no worker polls, \
                 got {described:?}"
            );
            ::tokio::time::sleep(SAMPLE_PAUSE).await;
        }

        // What the history holds while the task is unclaimed: the scheduling event
        // carrying the span the definition declared, and nothing the claim or the closure
        // would have written.
        let events = history(&emulator, &execution, json!({})).await;
        let scheduled = only(&events, case, EVENT_ACTIVITY_SCHEDULED);
        let details = scheduled
            .get("activityScheduledEventDetails")
            .unwrap_or_else(|| panic!("case {case:?}: expected scheduling details in {scheduled}"));
        assert_eq!(
            details.get("resource").and_then(Value::as_str),
            Some(activity.as_str()),
            "case {case:?}: expected the activity the task was scheduled for, got {details}"
        );
        assert_eq!(
            details.get("timeoutInSeconds").and_then(Value::as_u64),
            Some(bound_seconds),
            "case {case:?}: expected the span the definition declared, got {details}"
        );
        for absent in [EVENT_ACTIVITY_STARTED, EVENT_ACTIVITY_TIMED_OUT] {
            assert_eq!(
                count(&events, absent),
                0,
                "case {case:?}: expected no {absent} while the task is unclaimed, got {:?}",
                types(&events)
            );
        }

        // The task is still deliverable after that window, and the claim hands back the
        // token together with the document the execution was started on.
        let answer = poll(&emulator, &activity, Some(WORKER)).await;
        let token = answer
            .get("taskToken")
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                panic!("case {case:?}: expected the unclaimed task to still be handed out, got {answer}")
            })
            .to_string();
        assert!(
            !token.is_empty(),
            "case {case:?}: expected a non-empty taskToken, got {answer}"
        );
        assert_eq!(
            answer.get("input").and_then(Value::as_str),
            Some(input),
            "case {case:?}: expected the claim to hand back the execution's own input, got {answer}"
        );

        match worker {
            // The worker abandons the task: the span the state allowed it is what closes
            // it, that span after the CLAIM.
            Worker::Abandons => {
                let described = terminal(&emulator, case, &execution).await;
                assert_eq!(
                    (
                        described.get("status").and_then(Value::as_str),
                        described.get("error").and_then(Value::as_str),
                    ),
                    (Some("FAILED"), Some(TIMEOUT_ERROR)),
                    "case {case:?}: expected the abandoned task to fail the execution with \
                     {TIMEOUT_ERROR}, got {described}"
                );
                assert!(
                    described.get("output").is_none_or(Value::is_null),
                    "case {case:?}: expected a failed execution to report no output, got \
                     {described}"
                );

                let events = history(&emulator, &execution, json!({})).await;
                let started = only(&events, case, EVENT_ACTIVITY_STARTED);
                let timed_out = only(&events, case, EVENT_ACTIVITY_TIMED_OUT);
                assert_eq!(
                    count(&events, EVENT_ACTIVITY_SUCCEEDED),
                    0,
                    "case {case:?}: expected no {EVENT_ACTIVITY_SUCCEEDED} for a task nobody \
                     answered, got {:?}",
                    types(&events)
                );
                assert_eq!(
                    timed_out
                        .get("activityTimedOutEventDetails")
                        .and_then(|details| details.get("error"))
                        .and_then(Value::as_str),
                    Some(TIMEOUT_ERROR),
                    "case {case:?}: expected the closure to carry the language's own timeout \
                     error, got {timed_out}"
                );

                // The stamps the service recorded are what place the span: the closure is
                // at least the whole span after the CLAIM, and therefore the unclaimed
                // window FURTHER than that after the schedule — which is the difference
                // between a span that starts at the claim and one that starts at the
                // schedule.
                let since_claim = stamped(timed_out, case) - stamped(started, case);
                let since_schedule = stamped(timed_out, case) - stamped(scheduled, case);
                let bound_seconds = bound.as_secs_f64();
                assert!(
                    since_claim + STAMP_RESOLUTION_SECONDS >= bound_seconds,
                    "case {case:?}: expected the closure at least {bound_seconds}s after the \
                     claim, got {since_claim}s"
                );
                let from_the_schedule = bound_seconds + unclaimed_window.as_secs_f64();
                assert!(
                    since_schedule + STAMP_RESOLUTION_SECONDS >= from_the_schedule,
                    "case {case:?}: expected the closure at least {from_the_schedule}s after the \
                     schedule, got {since_schedule}s"
                );
            }
            // The worker answers inside that span: its own report is what the state — and
            // so the execution — ends on.
            Worker::Answers => {
                emulator
                    .ok(
                        "SendTaskSuccess",
                        &json!({ "taskToken": token, "output": ANSWERED_OUTPUT }),
                    )
                    .await;
                let described = terminal(&emulator, case, &execution).await;
                assert_eq!(
                    described.get("status").and_then(Value::as_str),
                    Some("SUCCEEDED"),
                    "case {case:?}: expected the answered task to complete the execution, got \
                     {described}"
                );
                assert_eq!(
                    described
                        .get("output")
                        .and_then(Value::as_str)
                        .and_then(|output| serde_json::from_str::<Value>(output).ok()),
                    serde_json::from_str::<Value>(ANSWERED_OUTPUT).ok(),
                    "case {case:?}: expected the worker's own output, got {described}"
                );
                let events = history(&emulator, &execution, json!({})).await;
                only(&events, case, EVENT_ACTIVITY_STARTED);
                only(&events, case, EVENT_ACTIVITY_SUCCEEDED);
                assert_eq!(
                    count(&events, EVENT_ACTIVITY_TIMED_OUT),
                    0,
                    "case {case:?}: expected no {EVENT_ACTIVITY_TIMED_OUT} for a task answered \
                     inside its span, got {:?}",
                    types(&events)
                );
            }
        }
    }
}

/// Describe `execution` until it reports a status other than `RUNNING`, and answer that
/// record.
///
/// The wait is for the STATUS and never for a span of its own: the span the clock has to
/// run out of, and the settle the outcome is owed after it, take as long as the host
/// takes, up to [`TERMINAL_DEADLINE`].
async fn terminal(emulator: &Emulator, case: &str, execution: &str) -> Value {
    let deadline = Instant::now() + TERMINAL_DEADLINE;
    loop {
        let described = describe_execution(emulator, execution).await;
        if described.get("status").and_then(Value::as_str) != Some(RUNNING) {
            return described;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the execution to leave {RUNNING}, got {described}"
        );
        ::tokio::time::sleep(POLL_PAUSE).await;
    }
}

/// The `type` of each event, in the order they were answered.
fn types(events: &[Value]) -> Vec<String> {
    events
        .iter()
        .map(|event| {
            event
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("<no type>")
                .to_string()
        })
        .collect()
}

/// How many events of `event_type` the history holds.
fn count(events: &[Value], event_type: &str) -> usize {
    events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .count()
}

/// The one event of `event_type`, which the history must carry exactly once.
fn only<'a>(events: &'a [Value], case: &str, event_type: &str) -> &'a Value {
    let matching: Vec<&Value> = events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "case {case:?}: expected exactly one {event_type}, got {:?}",
        types(events)
    );
    matching[0]
}

/// The epoch seconds one event is stamped at.
fn stamped(event: &Value, case: &str) -> f64 {
    event
        .get("timestamp")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("case {case:?}: expected a timestamp on {event}"))
}
