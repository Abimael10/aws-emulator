// SPDX-License-Identifier: CC0-1.0

//! What a `SendTask*` call is refused with when the task its `taskToken` names was closed
//! by a worker's FAILURE reply, by a heartbeat window the scheduling state declared
//! through a reference PATH, or by a stop the execution was later REDRIVEN out of —
//! driven over this service's own wire protocol (`awsJson1_0`, `POST /` with
//! `X-Amz-Target: AWSStepFunctions.<Operation>`) through the production
//! `MultiServiceRouter`.
//!
//! The rule under test: a `SendTask*` call quoting a `taskToken` whose task is no longer
//! outstanding is refused 400 `TaskTimedOut` with the condition and then, quoted, the
//! ACTIVITY that handed that task out — resolved from the token, which names it, so a
//! store that no longer holds the task still answers with it — whatever closed the task
//! and whatever the store holds for that activity now; the refusal carries no other
//! identifier and is inert, leaving the execution exactly as it was.
//!
//! What this adds over its neighbours: `crate::task_timed_out_wording` and
//! `crate::task_timed_out_quotes_the_activity` close their tasks with a SUCCESS reply, a
//! stop or no scheduling at all, `crate::task_timed_out_closed_by_a_bound` and
//! `crate::task_timed_out_attributes_its_activity` close theirs by a bound the definition
//! states as a LITERAL. None of them has a token spent by a FAILURE reply, a window
//! declared as `HeartbeatSecondsPath` and resolved from the execution's own data, or a
//! stale token refused after its execution was redriven and its activity handed out a
//! LIVE task of the same kind — the last of which separates "the quoted value is resolved
//! from the token presented" from "the quoted value is whatever the activity holds now".
//!
//! The rows differ along what the rule generalises over: WHY the token answers nothing (a
//! failure reply spent it, a success reply spent it, a path-declared window elapsed under
//! it, its execution was stopped and then redriven past it), WHAT the spending reply
//! carried — including a `cause` spelling the very sentence a task of no activity is
//! refused with, which a message built from the request would leak — WHERE the window was
//! read from, and WHICH of the three `SendTask*` calls then arrives under the stale token.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`) is
//! the one `state_machine_lifecycle` builds, the activity helpers (`create_activity`,
//! `activity_definition`, `claim`) are `activity_callbacks`', the poll that waits for an
//! execution to stop running (`closed_by_the_bound`) is
//! `task_timed_out_closed_by_a_bound`'s, and the refusal assertion (`REPLIES`, `refused`)
//! and the test-side spelling of the message (`timed_out_message`, `NO_ACTIVITY_DETAIL`)
//! are `task_timed_out_wording`'s; `tests/all.rs` is this binary's crate root, so each is
//! reached through `crate::<suite>` rather than copied.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::TIMEOUT_ERROR;

use crate::activity_callbacks::{activity_definition, claim, create_activity};
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, Emulator,
};
use crate::task_timed_out_closed_by_a_bound::closed_by_the_bound;
use crate::task_timed_out_wording::{refused, timed_out_message, NO_ACTIVITY_DETAIL, REPLIES};

/// A window far longer than this suite's whole run, for the one row whose token must
/// still be LIVE when it is beaten: every bound a case does not test is given generous
/// room.
const FAR_OFF_WINDOW_SECONDS: u32 = 300;

/// The shortest window the language takes — "a positive integer" of seconds — which is
/// what the rows that wait for a window to elapse declare, so the outcome they need is
/// owed as early as the language allows.
const SHORTEST_WINDOW_SECONDS: u32 = 1;

/// The request `reply` sends for `token`: the members of the row, plus the token it
/// quotes.
fn request(reply: &Value, token: &str) -> Value {
    let mut request = reply.clone();
    request
        .as_object_mut()
        .expect("every row's reply is an object")
        .insert("taskToken".to_string(), json!(token));
    request
}

/// Assert that every `SendTask*` call under `token` is refused 400 `TaskTimedOut` with
/// exactly `expected`, carrying none of `absent`.
async fn every_reply_is_refused(
    emulator: &Emulator,
    case: &str,
    token: &str,
    expected: &str,
    absent: &[&str],
) {
    for reply in REPLIES {
        refused(emulator, case, *reply, token, expected, absent).await;
    }
}

/// One row of the table below: the reply that spends the token, and what the execution
/// then reports.
struct Spent {
    case: &'static str,
    /// The activity's name, which is what the quoted ARN must end in.
    name: &'static str,
    /// The `SendTask*` operation whose reply closes the task.
    operation: &'static str,
    /// The members that reply carries beside its `taskToken`.
    reply: Value,
    /// The status the spent task leaves the execution in.
    settles_as: &'static str,
    /// Text of that reply which the refusal must not echo.
    echoes: Vec<&'static str>,
}

fn spent_rows() -> Vec<Spent> {
    vec![
        Spent {
            case: "a failure reply naming an error and a cause",
            name: "gave-up",
            operation: "SendTaskFailure",
            reply: json!({ "error": "Worker.GaveUp", "cause": "the worker could not finish" }),
            settles_as: "FAILED",
            echoes: vec!["Worker.GaveUp", "the worker could not finish"],
        },
        Spent {
            case: "a failure reply carrying neither of its optional members",
            name: "bare_failure.v2",
            operation: "SendTaskFailure",
            reply: json!({}),
            settles_as: "FAILED",
            echoes: Vec::new(),
        },
        Spent {
            case: "a failure reply whose cause spells the sentence a task of no activity is \
                   refused with",
            name: "l",
            operation: "SendTaskFailure",
            reply: json!({ "error": "Worker.LateReport", "cause": NO_ACTIVITY_DETAIL }),
            settles_as: "FAILED",
            echoes: vec!["Worker.LateReport"],
        },
        Spent {
            case: "a success reply answering with a nested document",
            name: "answered-pool-7",
            operation: "SendTaskSuccess",
            reply: json!({ "output": r#"{"done":true,"items":[1,2]}"# }),
            settles_as: "SUCCEEDED",
            echoes: vec!["done", "items"],
        },
    ]
}

/// A token a worker's own reply SPENT answers nothing more: every `SendTask*` call under
/// it is refused with the condition and that token's own activity, whichever reply spent
/// it and whatever that reply carried — and the refusals leave the settled execution
/// exactly as its reply left it.
#[tokio::test]
async fn every_send_task_call_refuses_a_spent_token_naming_its_activity() {
    let emulator = Emulator::new();

    for (index, row) in spent_rows().into_iter().enumerate() {
        let Spent {
            case,
            name,
            operation,
            reply,
            settles_as,
            echoes,
        } = row;
        let activity = create_activity(&emulator, name).await;
        assert!(
            activity.ends_with(&format!(":activity:{name}")),
            "case {case:?}: expected the activity ARN to name {name:?}, got {activity:?}"
        );
        let machine = create_machine(
            &emulator,
            &format!("spent-{index}"),
            &activity_definition(&activity, json!({})),
        )
        .await;
        let execution = start_execution(&emulator, &machine, "only", r#"{"job":"work"}"#).await;
        let (token, _) = claim(&emulator, case, &activity).await;

        // The reply is accepted once: it is the task's outcome, which closes the task.
        let (status, body) = emulator.call(operation, &request(&reply, &token)).await;
        assert_eq!(
            (status, &body),
            (200, &json!({})),
            "case {case:?}: expected {operation} to answer the task, got {status} with {body}"
        );
        let settled = closed_by_the_bound(&emulator, case, &execution).await;
        assert_eq!(
            settled.get("status").and_then(Value::as_str),
            Some(settles_as),
            "case {case:?}: expected the reply to settle the execution as {settles_as}, got \
             {settled}"
        );

        // Spelled out here, not read from the service: the condition, then the activity
        // this token names, quoted.
        let expected = timed_out_message(&activity);
        // What the refusal must NOT carry: the sentence a task of NO activity is refused
        // with, the other identifiers of this task, and the text of the reply that spent
        // the token.
        let mut absent: Vec<&str> = vec![
            NO_ACTIVITY_DETAIL,
            machine.as_str(),
            execution.as_str(),
            token.as_str(),
        ];
        absent.extend(echoes.iter().copied());
        every_reply_is_refused(&emulator, case, &token, &expected, &absent).await;

        // And the refusals changed nothing: the same read answers what the reply left.
        let after = describe_execution(&emulator, &execution).await;
        assert_eq!(
            after, settled,
            "case {case:?}: expected the refusals to leave the execution exactly as its reply \
             left it, got {after} after {settled}"
        );
    }
}

/// One row of the window table: where the scheduling state's `HeartbeatSecondsPath` reads
/// its window from, and the execution input that holds it.
struct Window {
    case: &'static str,
    name: &'static str,
    /// The reference path the `Task` state declares its window under.
    path: &'static str,
    /// The execution input that path reads, carrying the window in seconds.
    input: Value,
}

fn window_rows() -> Vec<Window> {
    vec![
        Window {
            case: "a window read from a top-level path",
            name: "window-path",
            path: "$.beat",
            input: json!({ "beat": SHORTEST_WINDOW_SECONDS, "job": "work" }),
        },
        Window {
            case: "a window read from a nested path",
            name: "window_path.nested",
            path: "$.limits.window",
            input: json!({ "limits": { "window": SHORTEST_WINDOW_SECONDS }, "job": "work" }),
        },
    ]
}

/// A heartbeat window the scheduling state declared through a reference PATH — resolved
/// from the execution's own data rather than written as a literal — closes the task when
/// nobody beats it, and every `SendTask*` call under the stale token is then refused with
/// the condition and that token's own activity.
#[tokio::test]
async fn a_path_declared_window_closes_the_task_and_its_refusal_quotes_the_activity() {
    let emulator = Emulator::new();

    for (index, row) in window_rows().into_iter().enumerate() {
        let Window {
            case,
            name,
            path,
            input,
        } = row;
        let activity = create_activity(&emulator, name).await;
        let machine = create_machine(
            &emulator,
            &format!("window-{index}"),
            &activity_definition(&activity, json!({ "HeartbeatSecondsPath": path })),
        )
        .await;
        let execution = start_execution(&emulator, &machine, "only", &input.to_string()).await;
        // A claimed task is what a window runs against: an activity task's clock starts
        // when a poll takes it.
        let (token, _) = claim(&emulator, case, &activity).await;

        let closed = closed_by_the_bound(&emulator, case, &execution).await;
        assert_eq!(
            (
                closed.get("status").and_then(Value::as_str),
                closed.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(TIMEOUT_ERROR)),
            "case {case:?}: expected the path-declared window to fail the execution with the \
             language's own timeout error, got {closed}"
        );

        let expected = timed_out_message(&activity);
        let absent = [
            NO_ACTIVITY_DETAIL,
            machine.as_str(),
            execution.as_str(),
            token.as_str(),
        ];
        every_reply_is_refused(&emulator, case, &token, &expected, &absent).await;

        let after = describe_execution(&emulator, &execution).await;
        assert_eq!(
            after, closed,
            "case {case:?}: expected the refusals to leave the execution exactly as the window \
             left it, got {after} after {closed}"
        );
    }
}

/// A window declared through a path is really a window, not a closed task: while it is far
/// from running out, the token it belongs to is LIVE, so a heartbeat under it is accepted
/// and the execution goes on running.
#[tokio::test]
async fn a_live_token_under_a_path_declared_window_is_beaten_rather_than_refused() {
    let emulator = Emulator::new();
    let case = "a far-off window read from a path";
    let activity = create_activity(&emulator, "live-window").await;
    let machine = create_machine(
        &emulator,
        "live-window-machine",
        &activity_definition(&activity, json!({ "HeartbeatSecondsPath": "$.beat" })),
    )
    .await;
    let execution = start_execution(
        &emulator,
        &machine,
        "only",
        &json!({ "beat": FAR_OFF_WINDOW_SECONDS }).to_string(),
    )
    .await;
    let (token, _) = claim(&emulator, case, &activity).await;

    let (status, body) = emulator
        .call("SendTaskHeartbeat", &json!({ "taskToken": token }))
        .await;
    assert_eq!(
        (status, &body),
        (200, &json!({})),
        "case {case:?}: expected the in-window heartbeat to be accepted, got {status} with {body}"
    );
    let described = describe_execution(&emulator, &execution).await;
    assert_eq!(
        described.get("status").and_then(Value::as_str),
        Some("RUNNING"),
        "case {case:?}: expected the beaten task's execution to go on running, got {described}"
    );
}

/// A token that went stale when its execution was STOPPED is still refused by the same
/// rule after a redrive has run that execution again and its activity has handed out a
/// LIVE task: the quoted activity is resolved from the token presented, not from whatever
/// the store holds for that activity now, and the live token of the fresh task answers
/// normally.
#[tokio::test]
async fn a_stale_token_is_refused_after_a_redrive_handed_out_a_fresh_task() {
    let emulator = Emulator::new();
    let case = "a token whose execution was stopped and then redriven";
    let activity = create_activity(&emulator, "resumed").await;
    let machine = create_machine(
        &emulator,
        "resumed-machine",
        &activity_definition(&activity, json!({})),
    )
    .await;
    let execution = start_execution(&emulator, &machine, "only", r#"{"job":"work"}"#).await;
    let (stale, _) = claim(&emulator, case, &activity).await;
    emulator
        .ok("StopExecution", &json!({ "executionArn": &execution }))
        .await;

    let expected = timed_out_message(&activity);
    let absent = [
        NO_ACTIVITY_DETAIL,
        machine.as_str(),
        execution.as_str(),
        stale.as_str(),
    ];
    let stopped = "a token whose execution was stopped under its task";
    every_reply_is_refused(&emulator, stopped, &stale, &expected, &absent).await;

    // The redrive runs the execution again, which schedules a task of its own: the stale
    // token is NOT the fresh one.
    emulator
        .ok("RedriveExecution", &json!({ "executionArn": &execution }))
        .await;
    let redriven = describe_execution(&emulator, &execution).await;
    assert_eq!(
        (
            redriven.get("status").and_then(Value::as_str),
            redriven.get("redriveCount").and_then(Value::as_i64),
        ),
        (Some("RUNNING"), Some(1)),
        "case {case:?}: expected the redrive to run the execution again, got {redriven}"
    );
    let (fresh, _) = claim(&emulator, "after the redrive", &activity).await;
    assert_ne!(
        fresh, stale,
        "case {case:?}: expected the re-run's task to carry a token of its own, got {stale:?} \
         again"
    );

    // The stale token answers nothing even now that its activity holds a live task, and
    // the refusal still quotes that activity.
    every_reply_is_refused(&emulator, case, &stale, &expected, &absent).await;
    let still_running = describe_execution(&emulator, &execution).await;
    assert_eq!(
        still_running.get("status").and_then(Value::as_str),
        Some("RUNNING"),
        "case {case:?}: expected the refusals to leave the re-run running, got {still_running}"
    );

    // The FRESH token is live: it is beaten and then answered, which is what makes the
    // refusals above about the stale token rather than about this activity.
    for (operation, reply) in [
        ("SendTaskHeartbeat", json!({})),
        ("SendTaskSuccess", json!({ "output": r#"{"done":true}"# })),
    ] {
        let (status, body) = emulator.call(operation, &request(&reply, &fresh)).await;
        assert_eq!(
            (status, &body),
            (200, &json!({})),
            "case {case:?}: expected {operation} under the fresh token to be accepted, got \
             {status} with {body}"
        );
    }
    let settled = closed_by_the_bound(&emulator, "the re-run", &execution).await;
    assert_eq!(
        settled.get("status").and_then(Value::as_str),
        Some("SUCCEEDED"),
        "case {case:?}: expected the answered re-run to succeed, got {settled}"
    );
}
