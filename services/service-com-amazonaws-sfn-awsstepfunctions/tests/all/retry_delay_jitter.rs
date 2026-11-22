// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the WAIT one `Retry` retrier puts between the attempts it
//! owes, driven over both services' own wire protocols through the production
//! `MultiServiceRouter` with Step Functions and Lambda mounted on ONE engine — so the
//! function really runs, the task really fails, and the gaps between its attempts are
//! read off the execution's own history.
//!
//! RULE: the wait before a retrier's next attempt is its `IntervalSeconds` scaled by
//! `BackoffRate` once per attempt already taken, no more than the `MaxDelaySeconds`
//! the retrier declares, and — when its `JitterStrategy` is `FULL` — a figure drawn
//! uniformly between zero and that capped wait, so jitter only ever SHORTENS a wait.
//! A retrier that declares no strategy keeps the computed wait itself, because `NONE`
//! is the member's documented default.
//!
//! The rows differ along what the rule generalises over: the interval and backoff a
//! retrier declares, how many attempts it has already spent (below the cap, at it and
//! past it), whether it declares a cap at all, and which `JitterStrategy` it asks for.
//!
//! The harness (`Chain`, `given_function_of`, `ran`, `sfn`) is the one
//! `lambda_task_control_flow` builds for the same two services, and the history's
//! event types are read through `lambda_function_timeout`'s own reading of them;
//! `tests/all.rs` is this binary's crate root, so both are reached through
//! `crate::<suite>`.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::activity_retry;

use crate::lambda_function_timeout::event_types;
use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;

/// The one state every definition below declares, whose `Retry` is the subject.
const STATE: &str = "Work";

/// An activity ARN, which is what makes the state above one whose retries
/// [`activity_retry`] answers.
const ACTIVITY: &str = "arn:aws:states:us-west-2:000000000000:activity:a";

/// The error every retrier below is asked about.
const ERROR: &str = "States.TaskFailed";

/// How close two second-figures must be to count as the same wait: the waits are
/// recorded in whole milliseconds, so the comparison allows for that rounding and
/// nothing more.
///
/// `pub(crate)`, not private: `tests/all/main.rs` is this binary's crate root, so a
/// sibling suite that reads the same recorded waits compares them to the same
/// precision rather than choosing a second one.
pub(crate) const TOLERANCE: f64 = 0.01;

/// What a retrier owes before its next attempt.
#[derive(Debug)]
enum Owed {
    /// Exactly this many seconds: a retrier that asks for no jitter waits the figure
    /// its own arithmetic reached.
    Exactly(f64),
    /// Somewhere in `[0, this]`: a retrier asking for full jitter draws its wait from
    /// zero up to that figure.
    UpTo(f64),
}

/// One row of the arithmetic table: the retrier the state declares, how many attempts
/// it has already spent, and what it owes.
struct Row {
    case: &'static str,
    retrier: Value,
    taken: u64,
    owed: Owed,
}

/// The rows: every published default and member of a retrier's arithmetic, each read
/// with and without the jitter the `JitterStrategy` member asks for.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the published defaults, which ask for no jitter",
            retrier: json!({ "ErrorEquals": [ERROR] }),
            taken: 0,
            owed: Owed::Exactly(1.0),
        },
        Row {
            case: "an interval and a backoff the retrier declares, with no cap",
            retrier: json!({
                "ErrorEquals": [ERROR],
                "IntervalSeconds": 3,
                "BackoffRate": 2.0,
                "MaxAttempts": 9,
            }),
            taken: 2,
            owed: Owed::Exactly(12.0),
        },
        Row {
            case: "a cap the product has gone past",
            retrier: json!({
                "ErrorEquals": [ERROR],
                "IntervalSeconds": 3,
                "BackoffRate": 2.0,
                "MaxAttempts": 9,
                "MaxDelaySeconds": 10,
            }),
            taken: 2,
            owed: Owed::Exactly(10.0),
        },
        Row {
            case: "the same cap, with the strategy that is the member's own default",
            retrier: json!({
                "ErrorEquals": [ERROR],
                "IntervalSeconds": 3,
                "BackoffRate": 2.0,
                "MaxAttempts": 9,
                "MaxDelaySeconds": 10,
                "JitterStrategy": "NONE",
            }),
            taken: 2,
            owed: Owed::Exactly(10.0),
        },
        Row {
            case: "the same cap under full jitter, which draws from under it",
            retrier: json!({
                "ErrorEquals": [ERROR],
                "IntervalSeconds": 3,
                "BackoffRate": 2.0,
                "MaxAttempts": 9,
                "MaxDelaySeconds": 10,
                "JitterStrategy": "FULL",
            }),
            taken: 2,
            owed: Owed::UpTo(10.0),
        },
        Row {
            case: "full jitter on an attempt the cap has not reached yet",
            retrier: json!({
                "ErrorEquals": [ERROR],
                "IntervalSeconds": 5,
                "BackoffRate": 3.0,
                "MaxAttempts": 9,
                "MaxDelaySeconds": 10,
                "JitterStrategy": "FULL",
            }),
            taken: 0,
            owed: Owed::UpTo(5.0),
        },
        Row {
            case: "full jitter on a late attempt, which the cap holds down",
            retrier: json!({
                "ErrorEquals": [ERROR],
                "IntervalSeconds": 5,
                "BackoffRate": 3.0,
                "MaxAttempts": 9,
                "MaxDelaySeconds": 10,
                "JitterStrategy": "FULL",
            }),
            taken: 5,
            owed: Owed::UpTo(10.0),
        },
        Row {
            case: "full jitter with no cap at all, bounded by the product itself",
            retrier: json!({
                "ErrorEquals": [ERROR],
                "IntervalSeconds": 2,
                "BackoffRate": 1.0,
                "MaxAttempts": 9,
                "JitterStrategy": "FULL",
            }),
            taken: 3,
            owed: Owed::UpTo(2.0),
        },
        Row {
            case: "full jitter over a zero interval, which has no span to spread",
            retrier: json!({
                "ErrorEquals": [ERROR],
                "IntervalSeconds": 0,
                "MaxAttempts": 9,
                "JitterStrategy": "FULL",
            }),
            taken: 4,
            owed: Owed::Exactly(0.0),
        },
    ]
}

/// A definition whose one state retries with `retrier`.
fn retried(retrier: &Value) -> String {
    json!({
        "StartAt": STATE,
        "States": {
            STATE: {
                "Type": "Task",
                "Resource": ACTIVITY,
                "Retry": [retrier],
                "End": true,
            },
        },
    })
    .to_string()
}

/// The seconds `retrier` owes after `taken` attempts, as the one reader of a retrier's
/// arithmetic answers it.
fn owed_seconds(retrier: &Value, taken: u64) -> f64 {
    let definition = retried(retrier);
    let (index, delay) = activity_retry(&definition, STATE, ERROR, &[taken])
        .unwrap_or_else(|| panic!("expected the retrier to owe an attempt for {definition}"));
    assert_eq!(
        index, 0,
        "expected the only retrier to answer, got retrier {index}"
    );
    delay.as_secs_f64()
}

// ---------------------------------------------------------------------------
// The arithmetic: the cap, and the band full jitter draws from
// ---------------------------------------------------------------------------

/// A retrier's wait is its grown interval capped by `MaxDelaySeconds`, and a retrier
/// asking for full jitter waits a figure between zero and that capped one.
#[test]
fn a_retrier_waits_its_capped_interval_and_draws_under_it_under_full_jitter() {
    for row in rows() {
        let Row {
            case,
            retrier,
            taken,
            owed,
        } = row;
        let seconds = owed_seconds(&retrier, taken);
        match owed {
            Owed::Exactly(expected) => assert!(
                (seconds - expected).abs() < TOLERANCE,
                "case {case:?}: expected exactly {expected}s before the next attempt, got \
                 {seconds}s"
            ),
            Owed::UpTo(bound) => assert!(
                (0.0..=bound + TOLERANCE).contains(&seconds),
                "case {case:?}: expected a wait drawn from [0, {bound}]s, got {seconds}s"
            ),
        }
    }
}

/// How many waits the draw is sampled over below. Large enough that the band and the
/// mean of a uniform draw are pinned many standard errors away from the figures a
/// retrier that ignored the strategy would produce, and small enough to cost nothing:
/// nothing waits for these, they are arithmetic.
const SAMPLES: usize = 400;

/// Full jitter really SPREADS the waits: over many attempts of one retrier the draws
/// fill the band under the cap, average about half of it, and are never the same figure
/// twice over.
///
/// The invariant is a distribution, so it is asserted over a sample rather than on one
/// draw: a retrier that ignored `JitterStrategy` would answer the cap every time, which
/// every bound below excludes.
#[test]
fn full_jitter_spreads_the_waits_across_the_band_under_the_cap() {
    /// The cap the retrier declares, which is also the band every draw must sit in.
    const CAP: f64 = 10.0;

    let retrier = json!({
        "ErrorEquals": [ERROR],
        "IntervalSeconds": 5,
        "BackoffRate": 3.0,
        "MaxAttempts": 1_000,
        "MaxDelaySeconds": CAP,
        "JitterStrategy": "FULL",
    });

    // Every draw is for an attempt the uncapped product has long gone past, so the
    // figure under test is the cap itself on every sample.
    let drawn: Vec<f64> = (0..SAMPLES).map(|_| owed_seconds(&retrier, 7)).collect();
    let outside: Vec<f64> = drawn
        .iter()
        .copied()
        .filter(|seconds| !(0.0..=CAP + TOLERANCE).contains(seconds))
        .collect();
    assert!(
        outside.is_empty(),
        "case \"inside the band\": expected every draw in [0, {CAP}]s, got {outside:?}"
    );

    let mean = drawn.iter().sum::<f64>() / drawn.len() as f64;
    assert!(
        (0.4 * CAP..=0.6 * CAP).contains(&mean),
        "case \"a uniform band\": expected the mean of {SAMPLES} draws near {}s, got {mean}s",
        0.5 * CAP
    );

    let lowest = drawn.iter().copied().fold(f64::INFINITY, f64::min);
    let highest = drawn.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    assert!(
        lowest < 0.1 * CAP && highest > 0.9 * CAP,
        "case \"the whole band\": expected draws in both the lowest and the highest tenth of \
         [0, {CAP}]s, got {lowest}s..{highest}s"
    );

    // Distinct figures, not one figure repeated: a wait that is a draw differs between
    // attempts of the very same retrier.
    let distinct: BTreeSet<u64> = drawn
        .iter()
        .map(|seconds| (seconds * 1_000.0) as u64)
        .collect();
    assert!(
        distinct.len() > SAMPLES / 2,
        "case \"distinct draws\": expected most of {SAMPLES} draws to differ, got \
         {} distinct figures",
        distinct.len()
    );
}

// ---------------------------------------------------------------------------
// The same rule over the wire: the gaps a retried Lambda task really leaves
// ---------------------------------------------------------------------------

/// The handler every execution below invokes: it always raises, so every attempt the
/// retrier owes really fails.
///
/// `pub(crate)` for the reason [`TOLERANCE`] is: a sibling suite whose rule is about a
/// retried task drives the very same always-failing function.
pub(crate) const RAISING_HANDLER: &str = r#"
def handler(event, context):
    raise Exception("this handler always raises")
"#;

/// The class a bare `Exception` reports, which is the name the failed task carries and
/// so the error the execution ends under.
pub(crate) const RAISED_ERROR_TYPE: &str = "Exception";

/// The optimized integration the state under test names.
pub(crate) const RESOURCE: &str = "arn:aws:states:::lambda:invoke";

/// The wildcard the Amazon States Language documents for every retriable error.
pub(crate) const ALL_ERRORS: &str = "States.ALL";

/// What the retried state declares: a one-second interval grown by this rate, which
/// without the cap would ask for 1, 3, 9, 27, 81, 243 and 729 seconds between the
/// attempts, and the cap that holds every one of them at one second.
const INTERVAL_SECONDS: f64 = 1.0;
/// See [`INTERVAL_SECONDS`].
const BACKOFF_RATE: f64 = 3.0;
/// See [`INTERVAL_SECONDS`].
const MAX_DELAY_SECONDS: f64 = 1.0;
/// See [`INTERVAL_SECONDS`].
const MAX_ATTEMPTS: usize = 7;

/// The definition one row runs: ONE `Task` state on the optimized Lambda integration,
/// retried with the row's own `JitterStrategy` and nothing to catch the failure, so the
/// task's last attempt is the execution's own outcome.
fn definition(function_arn: &str, jitter: Option<&str>) -> Value {
    let mut retrier = json!({
        "ErrorEquals": [ALL_ERRORS],
        "IntervalSeconds": INTERVAL_SECONDS,
        "MaxAttempts": MAX_ATTEMPTS,
        "BackoffRate": BACKOFF_RATE,
        "MaxDelaySeconds": MAX_DELAY_SECONDS,
    });
    if let Some(strategy) = jitter {
        retrier["JitterStrategy"] = json!(strategy);
    }
    json!({
        "StartAt": "Invoke",
        "States": {
            "Invoke": {
                "Type": "Task",
                "Resource": RESOURCE,
                "Parameters": { "FunctionName": function_arn, "Payload": {} },
                "Retry": [retrier],
                "End": true,
            },
        },
    })
}

/// The epoch seconds one history event is stamped at.
///
/// `pub(crate)` for the reason [`TOLERANCE`] is.
pub(crate) fn stamped_at(event: &Value) -> f64 {
    event
        .get("timestamp")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("expected a timestamp on {event}"))
}

/// The seconds between each failed attempt and the hand-over of the one that replaced
/// it, which is the wait the retrier put between them.
fn gaps(events: &[Value], case: &str) -> Vec<f64> {
    let mut measured = Vec::new();
    let mut failed_at: Option<f64> = None;
    for event in events {
        match event.get("type").and_then(Value::as_str) {
            Some("TaskFailed") => failed_at = Some(stamped_at(event)),
            Some("TaskScheduled") => {
                if let Some(failed) = failed_at.take() {
                    measured.push(stamped_at(event) - failed);
                }
            }
            _ => {}
        }
    }
    assert_eq!(
        measured.len(),
        MAX_ATTEMPTS,
        "case {case:?}: expected {MAX_ATTEMPTS} waits between the attempts, got {measured:?} from \
         {:?}",
        event_types(events)
    );
    measured
}

/// How many events of `kind` a run wrote.
///
/// `pub(crate)` for the reason [`TOLERANCE`] is.
pub(crate) fn count(events: &[Value], kind: &str) -> usize {
    event_types(events)
        .iter()
        .filter(|written| *written == kind)
        .count()
}

/// A retried `lambda:invoke` task really leaves the waits its retrier asks for: every
/// one inside the `MaxDelaySeconds` cap — far below the uncapped series — and, under
/// `FULL` jitter, drawn from under that cap instead of being the cap itself.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retried_lambda_task_waits_inside_its_cap_and_jitters_under_it() {
    let chain = Chain::new();
    let function = chain
        .given_function_of("always-raises", RAISING_HANDLER, None)
        .await;

    // (case, the JitterStrategy the retrier declares)
    let cases: [(&str, Option<&str>); 2] = [
        ("no JitterStrategy, so the capped wait itself", None),
        ("JitterStrategy FULL, so a draw under it", Some("FULL")),
    ];

    for (index, (case, jitter)) in cases.into_iter().enumerate() {
        let described = chain
            .ran(
                case,
                &format!("retry-jitter-{index}"),
                &definition(&function, jitter),
                &json!({}),
            )
            .await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RAISED_ERROR_TYPE)),
            "case {case:?}: expected the spent retrier to leave the handler's own failure, got \
             {described}"
        );

        let answered = chain
            .sfn(
                case,
                "GetExecutionHistory",
                &json!({ "executionArn": string(&described, "executionArn") }),
            )
            .await;
        let events = answered
            .get("events")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        // One hand-over and one failure per attempt: the first and the `MaxAttempts`
        // the retrier owes.
        assert_eq!(
            (count(&events, "TaskScheduled"), count(&events, "TaskFailed")),
            (MAX_ATTEMPTS + 1, MAX_ATTEMPTS + 1),
            "case {case:?}: expected {} attempts, got {:?}",
            MAX_ATTEMPTS + 1,
            event_types(&events)
        );

        let measured = gaps(&events, case);
        for (attempt, gap) in measured.iter().enumerate() {
            // What the retrier would have asked for with no cap, which every wait
            // after the first is far below.
            let uncapped = INTERVAL_SECONDS * BACKOFF_RATE.powi(attempt as i32);
            assert!(
                (0.0..=MAX_DELAY_SECONDS + TOLERANCE).contains(gap),
                "case {case:?}: expected wait {attempt} inside the {MAX_DELAY_SECONDS}s cap \
                 (uncapped it would be {uncapped}s), got {gap}s"
            );
        }

        match jitter {
            // No strategy is the documented default: every wait is the capped figure.
            None => {
                for (attempt, gap) in measured.iter().enumerate() {
                    assert!(
                        (gap - MAX_DELAY_SECONDS).abs() < TOLERANCE,
                        "case {case:?}: expected wait {attempt} to be the capped \
                         {MAX_DELAY_SECONDS}s, got {gap}s from {measured:?}"
                    );
                }
            }
            // Full jitter draws from under the cap, so over the waits of one run at
            // least one lands well below it.
            Some(_) => {
                let lowest = measured.iter().copied().fold(f64::INFINITY, f64::min);
                assert!(
                    lowest < 0.95 * MAX_DELAY_SECONDS,
                    "case {case:?}: expected at least one wait drawn below the \
                     {MAX_DELAY_SECONDS}s cap, got {measured:?}"
                );
            }
        }
    }
}
