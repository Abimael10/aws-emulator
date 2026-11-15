// SPDX-License-Identifier: CC0-1.0

//! End-to-end test for SEVERAL activity-backed `Task` states running AT THE SAME TIME,
//! each on its own activity under a bound of its own: every one of them hands its task to
//! its own long-polling worker, and what closes each task — the bound that state declared,
//! or the answer its own worker sent while it still held — is decided for that task alone,
//! so a sibling that is still outstanding, or that has already answered, changes neither
//! the execution nor the series of the other. Driven over each service's own wire protocol
//! through the production `MultiServiceRouter` with Step Functions and CloudWatch mounted
//! on ONE engine, so the series are read back through CloudWatch's own
//! `GetMetricStatistics` exactly as a client charting them does.
//!
//! The rule under test: an activity task's fate is its OWN — it is queued the moment its
//! execution enters the `Task` state, handed to the next poll of the activity it names
//! with that state's input, and closed either by the answer its token-holder sent inside
//! the state's bound or by that bound running out — and every close is counted under the
//! dimension of the resource it belongs to: the timed-out series of the activity whose
//! task lapsed and the failed series of that task's state machine, the succeeded series of
//! the activity and the state machine whose worker answered in time, and nothing in any
//! other activity's or state machine's series.
//!
//! What this adds over its neighbours: `crate::activity_timeout_late_answer` drives one
//! activity per fate, one at a time, and `crate::bounded_activity_task_outcomes` drives
//! several fates through ONE activity's queue. Neither has SEVERAL activities outstanding
//! at once, which is what separates "this task's bound closed this task" from "a bound
//! that lapsed closed whatever was open".
//!
//! The rows differ along what the rule generalises over: how many pairs run concurrently,
//! in which order the lapsing and the answering ones were queued, how long the bound each
//! missed was, and which of the three calls a token-holder can make arrives too late.
//!
//! The harness (`Metered`, `activity_dimensions`, `machine_dimensions`) is the one
//! `states_metric_families` builds, the create/start/claim helpers, the late-reply request
//! builder and the two constants it carries are `activity_timeout_late_answer`'s, the
//! expected refusal wording is `task_timed_out_wording`'s and the definition builder is
//! `activity_tasks`'; `tests/all.rs` is this binary's crate root, so each is reached through
//! `crate::<module>`.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::{
    asl::TIMEOUT_ERROR, observability, task_timed_out_message,
};

use crate::activity_tasks::activity_definition;
use crate::activity_timeout_late_answer::{claimed, create_activity, reply_request, start_execution};
use crate::state_machine_lifecycle::{error_code, string};
use crate::states_metric_families::{activity_dimensions, machine_dimensions, Metered};
use crate::task_timed_out_wording::timed_out_message;

/// One concurrent pair: a state machine whose single `Task` state hands its work to an
/// activity of its own under the bound it declares, and what that task's worker does with
/// the token it was handed.
#[derive(Debug, Clone, Copy)]
struct Pair {
    /// The bound that pair's `Task` state declares on its task, in seconds.
    bound_seconds: u32,
    /// `Some(operation)` — the worker goes quiet until the bound has run out and only then
    /// makes that call; `None` — it reports its result while the bound still holds.
    late: Option<&'static str>,
}

/// One row: the pairs that run side by side, every one of them started and claimed before
/// any of them is answered.
struct Row {
    case: &'static str,
    pairs: &'static [Pair],
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a one-second bound beside a generous one",
            pairs: &[
                Pair {
                    bound_seconds: 1,
                    late: Some("SendTaskSuccess"),
                },
                Pair {
                    bound_seconds: 300,
                    late: None,
                },
            ],
        },
        Row {
            // The same two fates, queued the other way round and with the other report
            // arriving too late: which pair was queued first decides nothing.
            case: "the generous bound queued first, a two-second one after it",
            pairs: &[
                Pair {
                    bound_seconds: 300,
                    late: None,
                },
                Pair {
                    bound_seconds: 2,
                    late: Some("SendTaskFailure"),
                },
            ],
        },
        Row {
            case: "two bounds of different lengths lapse while a third pair answers",
            pairs: &[
                Pair {
                    bound_seconds: 1,
                    late: Some("SendTaskHeartbeat"),
                },
                Pair {
                    bound_seconds: 2,
                    late: Some("SendTaskSuccess"),
                },
                Pair {
                    bound_seconds: 300,
                    late: None,
                },
            ],
        },
        Row {
            // Nothing lapses here, so no timed-out series anywhere may hold a thing: a
            // bound that never ran out closes nothing.
            case: "two generous bounds, both answered in time",
            pairs: &[
                Pair {
                    bound_seconds: 300,
                    late: None,
                },
                Pair {
                    bound_seconds: 600,
                    late: None,
                },
            ],
        },
    ]
}

/// The document the worker of pair `index` reports as its task's result: its own, so what
/// each execution reports says which token answered it.
fn in_time_output(index: usize) -> String {
    json!({ "answered": index }).to_string()
}

/// Several activity-backed `Task` states running at once each get their own task, their own
/// close and their own series: a bound that lapses fails only the execution whose task it
/// bounded, and the pair whose worker answered in time succeeds beside it.
#[tokio::test]
async fn concurrent_activity_tasks_are_closed_and_counted_pair_by_pair() {
    let metered = Metered::new();

    for (row_index, row) in rows().into_iter().enumerate() {
        let case = row.case;

        // One activity and one state machine per pair, so each pair's series are its own
        // and a sibling's datapoints can never be read as this pair's.
        let mut activities: Vec<String> = Vec::new();
        let mut machines: Vec<String> = Vec::new();
        for (index, pair) in row.pairs.iter().enumerate() {
            let name = format!("concurrent-{row_index}-{index}");
            let activity = create_activity(&metered, case, &name).await;
            let definition =
                activity_definition(&activity, json!({ "TimeoutSeconds": pair.bound_seconds }));
            machines.push(metered.machine(case, &name, &definition).await);
            activities.push(activity);
        }

        // Every execution is started before any poll, so every pair's task is queued at
        // once. A queued task is bounded by nothing — an activity task's own clock starts
        // when a poll takes it — so none of them can close before it is claimed.
        let mut executions: Vec<String> = Vec::new();
        for (index, machine) in machines.iter().enumerate() {
            let input = json!({ "pair": index }).to_string();
            executions.push(start_execution(&metered, case, machine, &input).await);
        }

        // Every task is claimed before any of them is answered, so no pair's answer can have
        // closed a sibling's task before that sibling was handed out, and each token carries
        // the input of the execution it was minted for.
        let mut tokens: Vec<String> = Vec::new();
        for (index, activity) in activities.iter().enumerate() {
            let (token, delivered) = claimed(&metered, case, activity).await;
            assert!(
                !token.is_empty(),
                "case {case:?}: pair {index} expected a non-empty taskToken, got {token:?}"
            );
            assert_eq!(
                delivered,
                json!({ "pair": index }),
                "case {case:?}: pair {index} expected the input its own state scheduled, got \
                 {delivered}"
            );
            tokens.push(token);
        }

        // The workers that answer inside their bound do so now — while the shorter bounds of
        // their siblings are still running, which is what makes the fates concurrent.
        for (index, pair) in row.pairs.iter().enumerate() {
            if pair.late.is_none() {
                metered
                    .ok(
                        case,
                        "SendTaskSuccess",
                        &json!({ "taskToken": tokens[index], "output": in_time_output(index) }),
                    )
                    .await;
            }
        }

        for (index, pair) in row.pairs.iter().enumerate() {
            let settled = metered.settled(case, &executions[index]).await;
            let (status, error, output) = match pair.late {
                // The bound this pair declared is what closed its task, so the call its
                // worker makes afterwards is too late for an execution that has already
                // failed on it — refused, and changing nothing it reports.
                Some(operation) => {
                    let (refused_status, refusal) = metered
                        .sfn(operation, &reply_request(operation, &tokens[index]))
                        .await;
                    let code = error_code(&refusal);
                    assert_eq!(
                        (refused_status, code.as_str()),
                        (400, "TaskTimedOut"),
                        "case {case:?}: pair {index} expected {operation} on the stale token to be \
                         refused 400 TaskTimedOut, got {refused_status} {code} with {refusal}"
                    );
                    // The refusal's WORDING, spelled out by the suite that owns this error
                    // class' one spelling rather than read from the builder under test, so
                    // wrong wording fails this row: the class' fixed text quoting the
                    // activity whose closed task the token names — this pair's own, never a
                    // value of the request.
                    let message = timed_out_message(&activities[index]);
                    assert_eq!(
                        refusal.get("message").and_then(Value::as_str),
                        Some(message.as_str()),
                        "case {case:?}: pair {index} expected the refusal {message:?}, got \
                         {refusal}"
                    );
                    // And it is the ONE construction every `SendTask*` answers this condition
                    // with, so no two of them word the same refusal differently.
                    assert_eq!(
                        message,
                        task_timed_out_message(&tokens[index]),
                        "case {case:?}: pair {index} expected every reply to this closed token \
                         to be refused in the same words"
                    );
                    let again = metered
                        .ok(
                            case,
                            "DescribeExecution",
                            &json!({ "executionArn": executions[index] }),
                        )
                        .await;
                    assert_eq!(
                        again, settled,
                        "case {case:?}: pair {index} expected the refused answer to leave the \
                         execution exactly as it was, got {again} after {settled}"
                    );
                    ("FAILED", Some(TIMEOUT_ERROR), None)
                }
                // Its worker answered while the bound held, so the execution ends on that
                // report whatever a sibling's bound did.
                None => ("SUCCEEDED", None, Some(in_time_output(index))),
            };

            assert_eq!(
                settled.get("status").and_then(Value::as_str),
                Some(status),
                "case {case:?}: pair {index} expected the execution to end {status}, got {settled}"
            );
            assert_eq!(
                settled.get("error").and_then(Value::as_str),
                error,
                "case {case:?}: pair {index} expected the error {error:?}, got {settled}"
            );
            assert_eq!(
                settled.get("output").and_then(Value::as_str),
                output.as_deref(),
                "case {case:?}: pair {index} expected the output {output:?}, got {settled}"
            );
            // And the execution's own ARN is the one that was started for this pair, so the
            // read above is about this pair and no sibling.
            assert_eq!(
                string(&settled, "executionArn"),
                executions[index],
                "case {case:?}: pair {index} expected its own execution to be described"
            );

            let lapsed = f64::from(u8::from(pair.late.is_some()));
            let answered = f64::from(u8::from(pair.late.is_none()));
            let of_activity = activity_dimensions(&activities[index]);
            let of_machine = machine_dimensions(&machines[index]);
            for (metric, dimensions, expected) in [
                // This pair's activity queued and handed out exactly its one task...
                (
                    observability::ACTIVITIES_SCHEDULED_METRIC,
                    &of_activity,
                    1.0,
                ),
                (observability::ACTIVITIES_STARTED_METRIC, &of_activity, 1.0),
                // ...and that task closed on its own terms alone.
                (
                    observability::ACTIVITIES_TIMED_OUT_METRIC,
                    &of_activity,
                    lapsed,
                ),
                (
                    observability::ACTIVITIES_SUCCEEDED_METRIC,
                    &of_activity,
                    answered,
                ),
                (observability::ACTIVITIES_FAILED_METRIC, &of_activity, 0.0),
                // The execution's outcome is counted under its own state machine, and the
                // execution-level timed-out series belongs to a state machine's own bound,
                // which no pair here declares.
                (
                    observability::EXECUTIONS_FAILED_METRIC,
                    &of_machine,
                    lapsed,
                ),
                (
                    observability::EXECUTIONS_SUCCEEDED_METRIC,
                    &of_machine,
                    answered,
                ),
                (
                    observability::EXECUTIONS_TIMED_OUT_METRIC,
                    &of_machine,
                    0.0,
                ),
                // And neither family is written under the other's dimension set.
                (
                    observability::ACTIVITIES_TIMED_OUT_METRIC,
                    &of_machine,
                    0.0,
                ),
                (
                    observability::EXECUTIONS_FAILED_METRIC,
                    &of_activity,
                    0.0,
                ),
            ] {
                let summed = metered.summed(case, metric, dimensions).await;
                assert_eq!(
                    summed, expected,
                    "case {case:?}: pair {index} expected {metric} to count {expected} under its \
                     own dimension set, got {summed}"
                );
            }
        }
    }
}
