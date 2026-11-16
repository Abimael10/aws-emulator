// SPDX-License-Identifier: CC0-1.0

//! WHICH activity a `TaskTimedOut` refusal attributes a lapsed task to when a worker
//! fleet holds tokens of SEVERAL activities at once, driven over this service's own wire
//! protocol (`awsJson1_0`, `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`)
//! through the production `MultiServiceRouter`.
//!
//! The rule under test: a `SendTask*` call quoting a `taskToken` whose task is no longer
//! outstanding is refused 400 `TaskTimedOut` with the condition and then, quoted, the
//! ACTIVITY that handed THAT token out — resolved from the token, which names it — so the
//! quoted value is never a constant sentence, never a sibling activity's ARN and never a
//! value the refused request carried; and the refusal is inert, so repeating it answers
//! the same words and leaves the execution exactly as the lapsed bound left it.
//!
//! What this adds over its neighbours: `crate::task_timed_out_quotes_the_activity` closes
//! its tasks by a CALLER's act, `crate::task_timed_out_closed_by_a_bound` drives one
//! activity at a time with one fixed reply body, and
//! `crate::concurrent_activity_bound_isolation` is about which SERIES each close moves.
//! None of them has several lapsed tokens of DIFFERENTLY named activities answered side by
//! side, nor varies what the late reply carries, which is what separates "the quoted value
//! identifies this token's own activity" from "the quoted value is some fixed text, or
//! something of the request".
//!
//! The rows differ along what the rule generalises over: the ACTIVITY the lapsed token
//! names (a one-character name, a hyphenated and digit-bearing one, a dotted and
//! underscored one, and the longest the modelled `Name` takes), how long the bound it
//! missed was, WHICH of the three `SendTask*` calls arrives under the stale token, and
//! WHAT that call carries beside the token — including a document that itself spells the
//! sentence a task of no activity is refused with, which a refusal built from the request
//! or from a literal would leak.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`) is
//! the one `state_machine_lifecycle` builds, the activity helpers (`create_activity`,
//! `activity_definition`, `claim`) are `activity_callbacks`', the wait for a bound to close a
//! task (`closed_by_the_bound`) is `task_timed_out_closed_by_a_bound`'s, and the test-side
//! spelling of the message (`timed_out_message`, `NO_ACTIVITY_DETAIL`) is
//! `task_timed_out_wording`'s; `tests/all.rs` is this binary's crate root, so each is reached
//! through `crate::<suite>` rather than copied.

use serde_json::{json, Value};

use service_com_amazonaws_sfn_awsstepfunctions::state::asl::TIMEOUT_ERROR;

use crate::activity_callbacks::{activity_definition, claim, create_activity};
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, error_code, start_execution, Emulator,
};
use crate::task_timed_out_closed_by_a_bound::closed_by_the_bound;
use crate::task_timed_out_wording::{timed_out_message, NO_ACTIVITY_DETAIL};

/// The longest activity name this API takes: the modelled `Name` is `@length(1..80)`
/// (`smithy_query(query="shape",shape="Name")`), so 80 characters is the published upper
/// bound.
const LONGEST_NAME_CHARS: usize = 80;

/// One row: the activity whose task lapses, the bound its `Task` state declares, and the
/// late call its token-holder then makes.
struct Row {
    case: &'static str,
    /// The activity's name, which is what the quoted ARN must end in.
    name: String,
    /// The whole-task bound that state declares, in seconds.
    bound_seconds: u32,
    /// The `SendTask*` operation the late call makes.
    operation: &'static str,
    /// The members that call carries beside its `taskToken`.
    payload: Value,
    /// Text of that payload which the refusal must not echo.
    echoes: Vec<&'static str>,
}

fn rows() -> Vec<Row> {
    // The names span what the modelled `Name` admits: its documentation allows 0-9, A-Z,
    // a-z, `-` and `_` (and the ARN segment takes `.` as well), within 1..80 characters.
    vec![
        Row {
            case: "a one-character activity, answered late with an empty document",
            name: "a".to_string(),
            bound_seconds: 1,
            operation: "SendTaskSuccess",
            payload: json!({ "output": "{}" }),
            echoes: Vec::new(),
        },
        Row {
            case: "a hyphenated, digit-bearing activity, failed late in the reason's own words",
            name: "worker-pool-7".to_string(),
            bound_seconds: 1,
            operation: "SendTaskFailure",
            payload: json!({ "error": "WorkerFailed", "cause": NO_ACTIVITY_DETAIL }),
            echoes: vec!["WorkerFailed"],
        },
        Row {
            case: "a dotted and underscored activity, beaten late with a bare heartbeat",
            name: "team_a.billing.v2".to_string(),
            bound_seconds: 2,
            operation: "SendTaskHeartbeat",
            payload: json!({}),
            echoes: Vec::new(),
        },
        Row {
            case: "an activity named at the modelled upper bound, answered late with a nested \
                   document",
            name: "l".repeat(LONGEST_NAME_CHARS),
            bound_seconds: 1,
            operation: "SendTaskSuccess",
            payload: json!({
                "output": json!({ "late": true, "why": NO_ACTIVITY_DETAIL }).to_string(),
            }),
            echoes: vec!["late"],
        },
    ]
}

/// What one row holds once its resources are made: the ARNs it created and the token its
/// worker is left with.
struct Held {
    activity: String,
    machine: String,
    execution: String,
    token: String,
}

/// The request a row's late call sends: its own members, plus the token it quotes.
fn late_request(payload: &Value, token: &str) -> Value {
    let mut request = payload.clone();
    request
        .as_object_mut()
        .expect("every row's payload is an object")
        .insert("taskToken".to_string(), json!(token));
    request
}

/// Several lapsed tokens, of activities named every way the API admits, answered side by
/// side: each refusal quotes the activity ITS token names and nothing else — no sibling's
/// ARN, no identifier of the task, no text of the request and not the sentence a task of no
/// activity is refused with.
#[tokio::test]
async fn each_lapsed_token_is_refused_quoting_the_activity_that_handed_it_out() {
    let emulator = Emulator::new();
    let rows = rows();

    // Every row's execution is started and its task claimed BEFORE any bound is waited
    // out, so the fleet really holds several lapsed tokens at once: an activity task's
    // clock starts when a poll takes it.
    let mut held = Vec::new();
    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let activity = create_activity(&emulator, &row.name).await;
        assert!(
            activity.ends_with(&format!(":activity:{}", row.name)),
            "case {case:?}: expected the activity ARN to name {:?}, got {activity:?}",
            row.name
        );
        let machine = create_machine(
            &emulator,
            &format!("attribution-{index}"),
            &activity_definition(&activity, json!({ "TimeoutSeconds": row.bound_seconds })),
        )
        .await;
        let execution = start_execution(&emulator, &machine, "only", r#"{"job":"work"}"#).await;
        let (token, _) = claim(&emulator, case, &activity).await;
        held.push(Held {
            activity,
            machine,
            execution,
            token,
        });
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let Held {
            activity,
            machine,
            execution,
            token,
        } = &held[index];
        let closed = closed_by_the_bound(&emulator, case, execution).await;
        assert_eq!(
            (
                closed.get("status").and_then(Value::as_str),
                closed.get("error").and_then(Value::as_str),
                closed.get("output").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(TIMEOUT_ERROR), None),
            "case {case:?}: expected the bound to fail the execution with the language's own \
             timeout error and no output, got {closed}"
        );

        // Spelled out here, not read from the service: the condition, then the activity
        // this token names, quoted.
        let expected = timed_out_message(activity);
        // What the refusal must NOT carry: the sentence a task of NO activity is refused
        // with, every SIBLING activity of the fleet, the other identifiers of this task,
        // and the text of the request it refused.
        let mut absent: Vec<&str> = vec![
            NO_ACTIVITY_DETAIL,
            machine.as_str(),
            execution.as_str(),
            token.as_str(),
        ];
        absent.extend(
            held.iter()
                .enumerate()
                .filter(|(other, _)| *other != index)
                .map(|(_, other)| other.activity.as_str()),
        );
        absent.extend(row.echoes.iter().copied());

        // The refusal is INERT, so the same call answers the same words however often it
        // is made: both looks are asserted, not just the first.
        for attempt in 1..=2 {
            let (status, body) = emulator
                .call(row.operation, &late_request(&row.payload, token))
                .await;
            let code = error_code(&body);
            let message = body.get("message").and_then(Value::as_str);
            assert_eq!(
                (status, code.as_str(), message),
                (400, "TaskTimedOut", Some(expected.as_str())),
                "case {case:?}: expected attempt {attempt} of {} to be refused with \
                 {expected:?}, got {status} with {body}",
                row.operation
            );
            let message = message.expect("the assertion above holds the message");
            for leaked in &absent {
                assert!(
                    !message.contains(leaked),
                    "case {case:?}: expected the refusal to carry this token's activity alone, \
                     got {leaked:?} in {message:?}"
                );
            }
        }

        // And the refusals changed nothing: the same read answers what the bound left.
        let after = describe_execution(&emulator, execution).await;
        assert_eq!(
            after, closed,
            "case {case:?}: expected the refusals to leave the execution exactly as the bound \
             left it, got {after} after {closed}"
        );
    }
}
