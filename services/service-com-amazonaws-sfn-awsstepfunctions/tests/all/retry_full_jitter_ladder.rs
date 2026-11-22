// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the LADDER a `Retry` retrier of a `Task` state walks: that it
//! ends, and that the gap it leaves before each attempt is inside the backoff that
//! attempt computed. Driven over both services' own wire protocols through the
//! production `MultiServiceRouter` with Step Functions and Lambda mounted on ONE engine,
//! so the function really runs, every attempt really fails, and the gaps are read off
//! the execution's own history.
//!
//! RULE: a retrier schedules the attempt after `n` already taken at
//! `IntervalSeconds * BackoffRate^n`, held down by the `MaxDelaySeconds` it declares and
//! — under `JitterStrategy` `FULL` — drawn uniformly from zero up to that figure, so the
//! gap between consecutive `TaskScheduled` events is never longer than the backoff
//! computed for that attempt; after `MaxAttempts` of them the state's last failure is
//! the execution's own, so the execution reaches a terminal status and the history holds
//! `MaxAttempts + 1` attempts.
//!
//! The rows differ along what the rule generalises over: the interval and the rate the
//! ladder grows by, how many attempts it owes, whether a cap holds the late attempts
//! down, and whether the retrier asks for jitter at all — a row whose cap grows per
//! attempt, a row the cap flattens, a row with no jitter (where the gap is the computed
//! figure itself, which is what makes the bound above a tight one) and a row whose
//! interval leaves no gap to spread.
//!
//! `retry_delay_jitter` asserts the same rule on the arithmetic alone and over the wire
//! under ONE flat cap; this suite is the per-attempt ladder: the bound each gap is
//! measured against is that attempt's own computed backoff, and the subject is that the
//! ladder TERMINATES. The harness (`Chain`, `given_function_of`, `ran`, `sfn`), the
//! always-raising function and the history readers are that suite's and
//! `lambda_task_control_flow`'s own; `tests/all/main.rs` is this binary's crate root, so
//! both are reached through `crate::<suite>`.

use serde_json::{json, Value};

use crate::lambda_function_timeout::event_types;
use crate::lambda_task_control_flow::Chain;
use crate::retry_delay_jitter::{
    count, stamped_at, ALL_ERRORS, RAISED_ERROR_TYPE, RAISING_HANDLER, RESOURCE, TOLERANCE,
};
use crate::state_machine_lifecycle::string;

/// One ladder: the retrier a row declares, and the attempts it owes.
struct Row {
    case: &'static str,
    /// `IntervalSeconds`: the wait before the first retry.
    interval_seconds: f64,
    /// `BackoffRate`: what the wait is scaled by once per attempt already taken.
    backoff_rate: f64,
    /// `MaxAttempts`: how many retries the retrier owes, and so how many gaps the
    /// history holds.
    max_attempts: usize,
    /// `MaxDelaySeconds`, where the row declares one.
    max_delay_seconds: Option<f64>,
    /// `JitterStrategy`, where the row declares one; `None` leaves the member out, so
    /// the retrier keeps the figure its own arithmetic reached.
    jitter: Option<&'static str>,
}

/// The rows. Each ladder is short and its waits are small, because every one of them is
/// a wait the execution really takes before it goes terminal.
fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a ladder that doubles under full jitter, with no cap declared",
            interval_seconds: 1.0,
            backoff_rate: 2.0,
            max_attempts: 4,
            max_delay_seconds: None,
            jitter: Some("FULL"),
        },
        Row {
            case: "a ladder under full jitter that a cap flattens after the first gap",
            interval_seconds: 1.0,
            backoff_rate: 3.0,
            max_attempts: 4,
            max_delay_seconds: Some(2.0),
            jitter: Some("FULL"),
        },
        Row {
            case: "the same ladder with no JitterStrategy, so each gap is the computed figure",
            interval_seconds: 1.0,
            backoff_rate: 2.0,
            max_attempts: 3,
            max_delay_seconds: None,
            jitter: None,
        },
        Row {
            case: "full jitter over an interval of zero, which leaves no gap to spread",
            interval_seconds: 0.0,
            backoff_rate: 2.0,
            max_attempts: 2,
            max_delay_seconds: None,
            jitter: Some("FULL"),
        },
    ]
}

/// What `row`'s retrier computes for the attempt after `taken` already taken: its
/// interval scaled by its rate once per attempt taken, held down by the cap it declares.
/// This is the figure the gap before that attempt must not exceed — and, with no jitter
/// asked for, the gap itself.
fn computed_backoff(row: &Row, taken: usize) -> f64 {
    let grown = row.interval_seconds * row.backoff_rate.powi(i32::try_from(taken).unwrap_or(0));
    row.max_delay_seconds.map_or(grown, |cap| grown.min(cap))
}

/// The definition one row runs: ONE `Task` state on the optimized Lambda integration,
/// retried by the row's own retrier and with nothing to catch the failure, so the
/// ladder's last attempt is the execution's own outcome.
fn definition(row: &Row, function_arn: &str) -> Value {
    let mut retrier = json!({
        "ErrorEquals": [ALL_ERRORS],
        "IntervalSeconds": row.interval_seconds,
        "BackoffRate": row.backoff_rate,
        "MaxAttempts": row.max_attempts,
    });
    if let Some(cap) = row.max_delay_seconds {
        retrier["MaxDelaySeconds"] = json!(cap);
    }
    if let Some(strategy) = row.jitter {
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

/// The seconds between consecutive `TaskScheduled` events, which is the gap the retrier
/// put before each attempt after the first.
fn scheduled_gaps(events: &[Value]) -> Vec<f64> {
    let scheduled: Vec<f64> = events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some("TaskScheduled"))
        .map(stamped_at)
        .collect();
    scheduled
        .windows(2)
        .map(|pair| pair[1] - pair[0])
        .collect()
}

/// A retried `lambda:invoke` task walks its whole retry ladder to a terminal status, and
/// every gap it leaves is inside the backoff that attempt computed — the figure itself
/// where the retrier asks for no jitter, and a draw from under it where it asks for
/// `FULL`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retry_ladder_ends_with_every_scheduled_gap_inside_its_computed_backoff() {
    let chain = Chain::new();
    let function = chain
        .given_function_of("ladder-always-raises", RAISING_HANDLER, None)
        .await;

    // Every gap drawn under full jitter, beside the figure it was drawn from: the draw
    // is pooled over the rows so that jitter really SHORTENING a wait is asserted once,
    // over many draws, rather than row by row over a handful.
    let mut drawn: Vec<(&str, f64, f64)> = Vec::new();

    for (index, row) in rows().into_iter().enumerate() {
        let case = row.case;
        let described = chain
            .ran(
                case,
                &format!("retry-ladder-{index}"),
                &definition(&row, &function),
                &json!({}),
            )
            .await;

        // The ladder ENDED: the spent retrier leaves the handler's own failure as the
        // execution's, which is the terminal status a caller acts on.
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RAISED_ERROR_TYPE)),
            "case {case:?}: expected the spent retrier to end the execution on the handler's own \
             failure, got {described}"
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

        // One hand-over and one failure per attempt: the first, and the `MaxAttempts`
        // the retrier owes.
        let attempts = row.max_attempts + 1;
        assert_eq!(
            (count(&events, "TaskScheduled"), count(&events, "TaskFailed")),
            (attempts, attempts),
            "case {case:?}: expected {attempts} attempts of the retried task, got {:?}",
            event_types(&events)
        );

        let gaps = scheduled_gaps(&events);
        assert_eq!(
            gaps.len(),
            row.max_attempts,
            "case {case:?}: expected {} gaps between the hand-overs, got {gaps:?} from {:?}",
            row.max_attempts,
            event_types(&events)
        );

        for (taken, gap) in gaps.iter().copied().enumerate() {
            let computed = computed_backoff(&row, taken);
            assert!(
                (0.0..=computed + TOLERANCE).contains(&gap),
                "case {case:?}: expected gap {taken} inside the {computed}s its backoff computed \
                 (interval {}s, rate {}, cap {:?}), got {gap}s from {gaps:?}",
                row.interval_seconds,
                row.backoff_rate,
                row.max_delay_seconds
            );
            match row.jitter {
                // No strategy is the member's documented default, so the gap is the
                // computed figure itself — which is what makes the bound above tight.
                None => assert!(
                    (gap - computed).abs() < TOLERANCE,
                    "case {case:?}: expected gap {taken} to be the computed {computed}s, got \
                     {gap}s from {gaps:?}"
                ),
                Some(_) => drawn.push((case, gap, computed)),
            }
        }
    }

    // Jitter SHORTENS: over every draw of every full-jitter row, at least one gap lands
    // well under the figure it was drawn from. A retrier that parsed `JitterStrategy`
    // and then ignored it would answer the computed figure every time.
    let under: Vec<&(&str, f64, f64)> = drawn
        .iter()
        .filter(|(_, gap, computed)| *computed > 0.0 && *gap < 0.9 * *computed)
        .collect();
    assert!(
        !under.is_empty(),
        "case \"full jitter draws from under the computed figure\": expected at least one of \
         {drawn:?} to be a gap well under the figure it was drawn from"
    );
}
