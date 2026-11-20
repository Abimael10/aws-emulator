// SPDX-License-Identifier: CC0-1.0

//! The START of a `Task` state's work is an event of the history in its own right, and
//! the ids of everything after it follow from that. Driven over both services' own wire
//! protocols through the production `MultiServiceRouter` with Step Functions and Lambda
//! mounted on ONE engine, so every event read back is one the run really wrote.
//!
//! RULE: a `Task` state whose work was handed over records the hand-over, the START of
//! that work and its outcome as three separate events of the family its `Resource` names,
//! in that order, whatever the work turned out to be — so no outcome event ever takes the
//! start's place. `HistoryEvent.id` is "numbered sequentially, starting at one" and
//! `previousEventId` is "the id of the previous event", so the start occupying its own
//! slot is what makes every later id, and every chain of ids, the one a client reading the
//! same history twice — forwards, backwards, or after a further attempt — gets both times.
//!
//! RULE: each event of the pair that brackets the work carries its own family's detail
//! structure: the `Task*` family names the work by "the service name of the resource in a
//! task state" and "the action of the resource called by a task state", both REQUIRED on
//! `TaskStartedEventDetails` and on `TaskFailedEventDetails`
//! (`smithy_query(query="shape",shape="TaskStartedEventDetails,TaskFailedEventDetails")`),
//! while the `LambdaFunction*` family reports its failure's `error`/`cause` under
//! `LambdaFunctionFailedEventDetails` and gives its START event no detail structure at all
//! — `HistoryEvent` declares no `lambdaFunctionStartedEventDetails` member
//! (`smithy_query(query="shape",shape="HistoryEvent")`).
//!
//! The rows differ along what the rule generalises over: the form of the `Resource` (a
//! bare Lambda function ARN, the optimized `arn:aws:states:::lambda:invoke` integration,
//! and an integration of another service, which the issue this suite pins did not cite),
//! the outcome of the work (the function returned, the function raised, the call was
//! refused), and how the history is read back (forwards, newest-first, and after a
//! redrive).
//!
//! The harness (`Chain`, `given_function_of`, `ran`, `sfn`) is the one
//! `lambda_task_control_flow` builds for the same two services, and the handler, the forms
//! of `Resource` and the readers of one history (`history`, `numbered`, `only`,
//! `expected_numbering`) are `task_work_event_families`', whose rule is about the same two
//! families; `tests/all/main.rs` is this binary's crate root, so each is reached through
//! `crate::<suite>`.

use serde_json::{json, Value};

use crate::lambda_function_timeout::event_types;
use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;
use crate::task_work_event_families::{
    expected_numbering, function_arn_task, history, numbered, only, optimized_task,
    other_integration_task, HANDLER_SOURCE, RAISED_ERROR_TYPE,
};

/// One row: a form of `Resource`, the input its state is handed, and the events its work
/// must write.
struct Form {
    case: &'static str,
    /// Built from the function's ARN, so each row names the same work its own way.
    state: fn(&str) -> Value,
    input: Value,
    /// The hand-over, the START and the outcome of the work, in the order the family
    /// writes them.
    work: [&'static str; 3],
    /// What the START event reports and under which member — `None` for the family whose
    /// start event the model gives no detail structure — as
    /// `(member, resourceType, resource)`.
    started: Option<(&'static str, &'static str, &'static str)>,
    /// The detail member the OUTCOME event carries.
    outcome: &'static str,
    /// The error that outcome must name, where the run decides it.
    error: Option<&'static str>,
    /// The events after the work: a state whose work failed records no exited event and
    /// the execution fails, while a state that produced a result exits and the execution
    /// succeeds.
    tail: &'static [&'static str],
}

fn forms() -> Vec<Form> {
    vec![
        Form {
            case: "a function ARN whose invocation raises",
            state: function_arn_task,
            input: json!({ "orderId": "o-1", "mode": "raise" }),
            work: [
                "LambdaFunctionScheduled",
                "LambdaFunctionStarted",
                "LambdaFunctionFailed",
            ],
            started: None,
            outcome: "lambdaFunctionFailedEventDetails",
            error: Some(RAISED_ERROR_TYPE),
            tail: &["ExecutionFailed"],
        },
        Form {
            case: "a function ARN whose invocation returns",
            state: function_arn_task,
            input: json!({ "orderId": "o-2" }),
            work: [
                "LambdaFunctionScheduled",
                "LambdaFunctionStarted",
                "LambdaFunctionSucceeded",
            ],
            started: None,
            outcome: "lambdaFunctionSucceededEventDetails",
            error: None,
            tail: &["TaskStateExited", "ExecutionSucceeded"],
        },
        Form {
            case: "the optimized integration whose function raises",
            state: optimized_task,
            input: json!({ "orderId": "o-3", "mode": "raise" }),
            work: ["TaskScheduled", "TaskStarted", "TaskFailed"],
            started: Some(("taskStartedEventDetails", "lambda", "invoke")),
            outcome: "taskFailedEventDetails",
            error: Some(RAISED_ERROR_TYPE),
            tail: &["ExecutionFailed"],
        },
        Form {
            case: "the optimized integration whose function returns",
            state: optimized_task,
            input: json!({ "orderId": "o-4" }),
            work: ["TaskScheduled", "TaskStarted", "TaskSucceeded"],
            started: Some(("taskStartedEventDetails", "lambda", "invoke")),
            outcome: "taskSucceededEventDetails",
            error: None,
            tail: &["TaskStateExited", "ExecutionSucceeded"],
        },
        // An integration of ANOTHER service, which the report this suite pins did not cite.
        Form {
            case: "an integration of another service, whose call the service refuses",
            state: other_integration_task,
            input: json!({ "orderId": "o-5" }),
            work: ["TaskScheduled", "TaskStarted", "TaskFailed"],
            started: Some(("taskStartedEventDetails", "sqs", "sendMessage")),
            outcome: "taskFailedEventDetails",
            error: None,
            tail: &["ExecutionFailed"],
        },
    ]
}

/// The detail members an event carries, as the names the response spells them with: what
/// tells an event that reports a structure from one the model gives none.
fn detail_members(event: &Value) -> Vec<String> {
    event
        .as_object()
        .map(|members| {
            members
                .keys()
                .filter(|member| member.ends_with("EventDetails"))
                .cloned()
                .collect()
        })
        .unwrap_or_default()
}

/// Every form of `Resource` records the start of its work as its own event between the
/// hand-over and the outcome, so the whole history is numbered and chained exactly as AWS
/// numbers it — read forwards and read from the end.
#[tokio::test]
async fn the_start_of_a_tasks_work_is_its_own_event_and_the_ids_follow_from_it() {
    let chain = Chain::new();
    let function = chain
        .given_function_of("started-numbering", HANDLER_SOURCE, None)
        .await;

    for (index, form) in forms().into_iter().enumerate() {
        let Form {
            case,
            state,
            input,
            work,
            started,
            outcome,
            error,
            tail,
        } = form;
        let definition = json!({
            "StartAt": "Work",
            "States": { "Work": state(&function) },
        });
        let described = chain
            .ran(
                case,
                &format!("started-numbering-{index}"),
                &definition,
                &input,
            )
            .await;
        let execution = string(&described, "executionArn");

        // The whole history: the state's own entry, the three events its work wrote with
        // the START in the middle, and what the outcome left behind — numbered from one,
        // each naming the event before it.
        let mut whole: Vec<&str> = vec!["ExecutionStarted", "TaskStateEntered"];
        whole.extend(work);
        whole.extend(tail.iter().copied());
        let events = history(&chain, case, &execution, json!({})).await;
        assert_eq!(
            numbered(&events),
            expected_numbering(&whole),
            "case {case:?}: expected the work's start to hold its own slot, got {:?}",
            event_types(&events)
        );

        // The START event reports what its own family names the work by, with both
        // required members — and nothing at all for the family the model gives that event
        // no detail structure.
        let start = only(&events, work[1], case);
        match started {
            Some((member, resource_type, resource)) => {
                let detail = start
                    .get(member)
                    .unwrap_or_else(|| panic!("case {case:?}: expected {member} on {start}"));
                assert_eq!(
                    (
                        detail.get("resourceType").and_then(Value::as_str),
                        detail.get("resource").and_then(Value::as_str),
                    ),
                    (Some(resource_type), Some(resource)),
                    "case {case:?}: expected the start to name the work \
                     {resource_type}/{resource}, got {detail}"
                );
            }
            None => assert!(
                detail_members(start).is_empty(),
                "case {case:?}: expected no detail structure on {}, got {:?}",
                work[1],
                detail_members(start)
            ),
        }

        // And the OUTCOME event carries its own family's structure where the report said
        // nothing was there: the same naming the start reported for the `Task*` family,
        // and the error the run produced where the work failed.
        let closed = only(&events, work[2], case);
        let detail = closed
            .get(outcome)
            .unwrap_or_else(|| panic!("case {case:?}: expected {outcome} on {closed}"));
        if let Some((_, resource_type, resource)) = started {
            assert_eq!(
                (
                    detail.get("resourceType").and_then(Value::as_str),
                    detail.get("resource").and_then(Value::as_str),
                ),
                (Some(resource_type), Some(resource)),
                "case {case:?}: expected the outcome to name the same work the start did, \
                 got {detail}"
            );
        }
        if let Some(error) = error {
            assert_eq!(
                detail.get("error").and_then(Value::as_str),
                Some(error),
                "case {case:?}: expected the outcome to report {error}, got {detail}"
            );
        }
        // Whatever decided it, a failed unit of work is reported under SOME error name and
        // explained by a cause: the row whose call the invoked service refuses names the
        // refusal rather than closing with an empty failure.
        if work[2].ends_with("Failed") {
            for member in ["error", "cause"] {
                assert!(
                    detail
                        .get(member)
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty()),
                    "case {case:?}: expected the failure to carry a {member}, got {detail}"
                );
            }
        }

        // Read newest-first, the same events from the end: the terminal event keeps the id
        // it has forwards, so an id a client obtained from one read means the same event in
        // the other.
        let reversed = history(&chain, case, &execution, json!({ "reverseOrder": true })).await;
        let mut backwards = expected_numbering(&whole);
        backwards.reverse();
        assert_eq!(
            numbered(&reversed),
            backwards,
            "case {case:?}: expected the reverse read to be the same chain from the end, got \
             {:?}",
            event_types(&reversed)
        );
    }
}

/// A FURTHER attempt keeps the rule: the redriven attempt writes the start of its work as
/// its own event too, so the attempt's events continue the one numbering the first attempt
/// began — here on the legacy family, whose events the first test reads only on one
/// attempt.
#[tokio::test]
async fn a_redriven_attempt_records_the_start_of_its_work_and_continues_the_numbering() {
    let chain = Chain::new();
    let case = "a function ARN whose invocation raises, then redriven";
    let function = chain
        .given_function_of("redriven-start", HANDLER_SOURCE, None)
        .await;
    let definition = json!({
        "StartAt": "Work",
        "States": { "Work": function_arn_task(&function) },
    });
    let described = chain
        .ran(
            case,
            "redriven-start",
            &definition,
            &json!({ "orderId": "o-1", "mode": "raise" }),
        )
        .await;
    let execution = string(&described, "executionArn");

    let first = &[
        "ExecutionStarted",
        "TaskStateEntered",
        "LambdaFunctionScheduled",
        "LambdaFunctionStarted",
        "LambdaFunctionFailed",
        "ExecutionFailed",
    ];
    let events = history(&chain, case, &execution, json!({})).await;
    assert_eq!(
        numbered(&events),
        expected_numbering(first),
        "case {case:?}: expected the first attempt's own chain, got {:?}",
        event_types(&events)
    );

    chain
        .sfn(case, "RedriveExecution", &json!({ "executionArn": execution }))
        .await;
    chain.settled(case, &execution).await;
    let mut whole: Vec<&str> = first.to_vec();
    whole.extend([
        "ExecutionRedriven",
        "LambdaFunctionScheduled",
        "LambdaFunctionStarted",
        "LambdaFunctionFailed",
        "ExecutionFailed",
    ]);
    let mut expected = expected_numbering(&whole);
    // The step the redrive resumed was ENTERED by the attempt being redriven and is not
    // entered again, so the first event the new attempt writes for that step follows that
    // preserved entry rather than the boundary event the attempt opened with.
    let preserved_entry = 2;
    let resumed = first.len() + 1;
    expected[resumed].2 = preserved_entry;
    let events = history(&chain, case, &execution, json!({})).await;
    assert_eq!(
        numbered(&events),
        expected,
        "case {case:?}: expected the redriven attempt to record its own start and continue the \
         numbering, got {:?}",
        event_types(&events)
    );

    // The boundary the attempt opened with reports the redrive it was written for, so an
    // attempt's events can be lined up against the bookkeeping that counts it.
    let boundary = only(&events, "ExecutionRedriven", case)
        .get("executionRedrivenEventDetails")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the redrive detail"));
    assert_eq!(
        boundary.get("redriveCount").and_then(Value::as_i64),
        Some(1),
        "case {case:?}: expected the first redrive to be counted, got {boundary}"
    );

    // And the whole of it read from the end, where a client that wants the latest first
    // opens on the terminal event of the redriven attempt.
    let reversed = history(&chain, case, &execution, json!({ "reverseOrder": true })).await;
    let mut backwards = expected;
    backwards.reverse();
    assert_eq!(
        numbered(&reversed),
        backwards,
        "case {case:?}: expected the reverse read to be the same chain from the end, got {:?}",
        event_types(&reversed)
    );
}
