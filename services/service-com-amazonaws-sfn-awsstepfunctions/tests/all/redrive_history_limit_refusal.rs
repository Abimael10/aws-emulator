// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the refusal `RedriveExecution` answers an execution the
//! HISTORY-EVENT quota ended, driven over this service's own wire protocol
//! (`awsJson1_0`, `POST /` with `X-Amz-Target: AWSStepFunctions.<Operation>`) through
//! the production `MultiServiceRouter`, so what is asserted is what a signed AWS SDK
//! request observes.
//!
//! The rule under test: an execution that reached a terminal status by filling its
//! event history is not eligible for a redrive, and the `ExecutionNotRedrivable` that
//! refuses it is a 400 whose message names the execution the request identified and
//! then STATES THE LIMIT the execution went past — one wording for that ineligibility,
//! whatever the definition did to reach it, while `redriveStatusReason` carries the
//! sentence that member's own documentation lists. A client distinguishes the several
//! ineligibilities `ExecutionNotRedrivable` covers only by that reason clause, because
//! the code is the same for all of them.
//!
//! The table's rows differ along WHAT filled the history — a cycle the definition's own
//! data never leaves, and an inline `Map` over more items than one history carries —
//! and so also along the execution the refusal is about, which is the message's only
//! variable part. The control row is an ordinary `FAILED` execution well inside the
//! quota: it is `REDRIVABLE` and the same call performs the redrive, so the refusal
//! belongs to the quota rather than to every failure.
//!
//! The other ineligibilities (`RUNNING`, `SUCCEEDED`, a state machine being deleted)
//! are driven by `tests/all/client_error_wrappers.rs`; this suite holds the one whose
//! execution has to be run into the quota first.
//!
//! The harness (`Emulator`, `create_machine`, `start_execution`, `describe_execution`)
//! is the one `state_machine_lifecycle` builds, the `(status, code, message)` reader of a
//! refused call is `error_messages`', and the poll-until-settled wait, the item count and
//! the chained `Map` builder are `distributed_map_children`'s; `tests/all/main.rs`
//! declares every suite of this target, so each is reached through `crate::<suite>`.

use serde_json::{json, Value};

use crate::distributed_map_children::{chained_mapping, settled, ITEMS_PAST_ONE_HISTORY};
use crate::error_messages::failure;
use crate::state_machine_lifecycle::{
    create_machine, describe_execution, start_execution, Emulator,
};

/// The ASL error a run the history-event quota ended reports.
const RUNTIME_ERROR: &str = "States.Runtime";

/// What such a run reports as its cause, which names the quota and nothing of the
/// definition or of the request.
const HISTORY_LIMIT_CAUSE: &str = "maximum number of history events";

/// The error the control row's own `Fail` state declares.
const FAIL_ERROR: &str = "Boom";

/// What the refusal's reason clause must be, spelled LITERALLY rather than read from the
/// constant that builds it, so a rewording of either fails here.
const REFUSED_REASON: &str = "Execution has exceeded the maximum history event limit";

/// The sentence `DescribeExecution`'s `redriveStatusReason` lists for the same
/// ineligibility — a different one, documented on that member.
const PROJECTED_REASON: &str = "Execution history event limit exceeded";

/// A definition that fails immediately, so its execution is terminal with a history of a
/// handful of events: the control that a `FAILED` execution inside the quota redrives.
const FAIL_DEFINITION: &str =
    r#"{"StartAt":"Bang","States":{"Bang":{"Type":"Fail","Error":"Boom","Cause":"on purpose"}}}"#;

/// One row of the table: how the execution got where it is, and what it must end as.
struct Row {
    case: &'static str,
    definition: String,
    input: String,
    /// The ASL error the run must report, which says WHAT ended it.
    error: &'static str,
    /// Whether the history-event quota is what ended it, and so whether the redrive is
    /// refused.
    at_the_limit: bool,
}

/// A definition whose own data never reaches its exit, so the run goes round until the
/// history-event quota ends it.
fn cycle_definition() -> String {
    json!({
        "StartAt": "Step",
        "States": {
            "Step": {
                "Type": "Pass",
                "Parameters": { "n.$": "States.MathAdd($.n, 1)" },
                "Next": "Check",
            },
            // A turn count no run reaches inside the quota, so the terminal state this
            // definition declares is never got to.
            "Check": {
                "Type": "Choice",
                "Choices": [{ "Variable": "$.n", "NumericLessThan": 1_000_000, "Next": "Step" }],
                "Default": "Ending",
            },
            "Ending": { "Type": "Succeed" },
        },
    })
    .to_string()
}

/// More items than the events one execution's history holds, each run through the
/// chained `Map` processor `distributed_map_children` declares.
fn mapped_items() -> String {
    Value::Array(
        (0..ITEMS_PAST_ONE_HISTORY)
            .map(|index| json!({ "n": index }))
            .collect(),
    )
    .to_string()
}

/// `redriveStatus`, `redriveStatusReason` and `redriveCount` as a description carries
/// them.
fn redrive_projection(body: &Value) -> (Option<&str>, Option<&str>, Option<u64>) {
    (
        body.get("redriveStatus").and_then(Value::as_str),
        body.get("redriveStatusReason").and_then(Value::as_str),
        body.get("redriveCount").and_then(Value::as_u64),
    )
}

/// Every execution the history-event quota ended is refused with the SAME reason clause,
/// naming the execution the request identified — and an execution inside the quota is
/// redriven instead.
#[tokio::test]
async fn a_redrive_of_an_execution_at_the_history_limit_states_the_limit_it_went_past() {
    let rows = [
        Row {
            case: "a cycle the data never leaves",
            definition: cycle_definition(),
            input: json!({ "n": 0 }).to_string(),
            error: RUNTIME_ERROR,
            at_the_limit: true,
        },
        Row {
            case: "an inline Map over more items than one history carries",
            definition: chained_mapping(None),
            input: mapped_items(),
            error: RUNTIME_ERROR,
            at_the_limit: true,
        },
        Row {
            // The control: a FAILED execution well inside the quota, which the same
            // call redrives.
            case: "a failed run of a handful of events",
            definition: FAIL_DEFINITION.to_string(),
            input: json!({ "n": 0 }).to_string(),
            error: FAIL_ERROR,
            at_the_limit: false,
        },
    ];


    let emulator = Emulator::new();
    // Each row is started before any is read, so the runs the quota ends settle
    // alongside one another.
    let mut started = Vec::with_capacity(rows.len());
    for (index, row) in rows.iter().enumerate() {
        let machine =
            create_machine(&emulator, &format!("history-limit-{index}"), &row.definition).await;
        started.push(start_execution(&emulator, &machine, "only", &row.input).await);
    }

    let mut refusals = Vec::new();
    for (row, execution) in rows.iter().zip(&started) {
        let case = row.case;
        let described = settled(&emulator, execution, case).await;
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(row.error)),
            "case {case:?}: expected the run to have FAILED with {}, got {described:?}",
            row.error
        );
        let cause = described
            .get("cause")
            .and_then(Value::as_str)
            .unwrap_or_default();
        assert_eq!(
            cause.contains(HISTORY_LIMIT_CAUSE),
            row.at_the_limit,
            "case {case:?}: expected the quota's own cause exactly where the quota ended the \
             run, got {cause:?}"
        );

        if !row.at_the_limit {
            // The control: the projection says the redrive would be performed, and it is
            // — so the refusal below is the quota's and not every failure's.
            assert_eq!(
                redrive_projection(&described),
                (Some("REDRIVABLE"), None, Some(0)),
                "case {case:?}: expected a redrivable failure, got {described:?}"
            );
            let redriven = emulator
                .ok("RedriveExecution", &json!({ "executionArn": execution }))
                .await;
            assert!(
                redriven
                    .get("redriveDate")
                    .and_then(Value::as_f64)
                    .is_some_and(|seconds| seconds > 0.0),
                "case {case:?}: expected the redrive to answer its redriveDate, got {redriven:?}"
            );
            let after = describe_execution(&emulator, execution).await;
            assert_eq!(
                after.get("redriveCount").and_then(Value::as_u64),
                Some(1),
                "case {case:?}: expected the performed redrive to be counted, got {after:?}"
            );
            continue;
        }

        // The quota is where the history ended, so the projection reports the
        // ineligibility in the words that member documents, with no execution named.
        assert_eq!(
            redrive_projection(&described),
            (Some("NOT_REDRIVABLE"), Some(PROJECTED_REASON), Some(0)),
            "case {case:?}: expected the documented projection, got {described:?}"
        );

        let (http_status, code, message) = failure(
            &emulator,
            case,
            "RedriveExecution",
            &json!({ "executionArn": execution }),
        )
        .await;
        assert_eq!(
            (http_status, code.as_str()),
            (400, "ExecutionNotRedrivable"),
            "case {case:?}: expected 400 ExecutionNotRedrivable, got {http_status} {code} with \
             message {message:?}"
        );
        // Spelled literally: the fixed text, then the one variable part, which is the
        // executionArn of the request.
        let expected = format!("Execution {execution} invalid for redrive as: {REFUSED_REASON}");
        assert_eq!(
            message, expected,
            "case {case:?}: expected message {expected:?}, got {message:?}"
        );

        // A refused redrive leaves the execution exactly where it was: the refusal is
        // not an attempt.
        let after = describe_execution(&emulator, execution).await;
        assert_eq!(
            (
                after.get("status").and_then(Value::as_str),
                redrive_projection(&after),
            ),
            (
                Some("FAILED"),
                (Some("NOT_REDRIVABLE"), Some(PROJECTED_REASON), Some(0)),
            ),
            "case {case:?}: expected the refusal to have changed nothing, got {after:?}"
        );
        refusals.push((case, execution.clone(), message));
    }

    // The rows' refusals differ in the execution each names and in nothing else, which
    // is what makes the ARN the message's only variable part.
    for (case, execution, message) in &refusals {
        for (other_case, other, _) in &refusals {
            let names_other = other != execution && message.contains(other.as_str());
            assert!(
                !names_other,
                "case {case:?}: expected the refusal to name its own execution \
                 {execution:?} and not {other_case:?}'s {other:?}, got {message:?}"
            );
        }
        let reason = message
            .rsplit(" invalid for redrive as: ")
            .next()
            .unwrap_or_default();
        assert_eq!(
            reason, REFUSED_REASON,
            "case {case:?}: expected one reason clause for the quota however the history \
             filled, got {reason:?}"
        );
    }
    assert_eq!(
        refusals.len(),
        2,
        "expected both quota rows to have been refused, got {refusals:?}"
    );
}
