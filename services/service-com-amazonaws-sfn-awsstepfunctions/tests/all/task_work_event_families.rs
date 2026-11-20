// SPDX-License-Identifier: CC0-1.0

//! Which FAMILY of history events a `Task` state's own work is recorded under, driven
//! over both services' own wire protocols through the production `MultiServiceRouter`
//! with Step Functions and Lambda mounted on ONE engine — so the function really runs
//! and every event read back is one the run wrote.
//!
//! RULE: `HistoryEventType` names the lifecycle of a `Task` state's work twice over, and
//! which family one state's work is recorded under is decided by WHAT its `Resource`
//! names rather than by what the work turned out to be. A `Resource` that is the ARN of
//! a Lambda FUNCTION makes the task's unit of work one invocation of that function, so
//! its hand-over, start and outcome are `LambdaFunctionScheduled`,
//! `LambdaFunctionStarted` and `LambdaFunctionSucceeded`/`LambdaFunctionFailed`, with
//! the scheduled event naming the work by that function's own ARN and carrying the input
//! the invocation was handed; every other `Resource` names a service INTEGRATION, whose
//! unit of work is one API call, recorded under `TaskScheduled`, `TaskStarted` and
//! `TaskSucceeded`/`TaskFailed`, each naming the work by the kind of resource and the
//! action. Both families record the start of the work as its own event, so an attempt
//! that reached its work reports three events inside the state's own entered/exited
//! pair.
//!
//! RULE: `taskScheduledEventDetails.parameters` is "the JSON data passed to the resource
//! referenced in a task state" and is REQUIRED, with no `inputDetails` companion to say
//! a document was withheld — unlike every `input` and `output` member — so a read that
//! asked for no execution data still reports it, while those optional payload members go.
//!
//! RULE: resolving what the work is handed is the INTERPRETER's own step, taken before
//! anything is handed over: a `Parameters` path that cannot be resolved against the
//! state's input means the work was never submitted, so the state records no lifecycle
//! event for it at all and the execution fails on the state it was entered at.
//!
//! RULE: what each family's OUTCOME event reports is the result that form of `Resource`
//! gives its state: a `Resource` naming the function reports the document the function
//! RETURNED, bare, because the members around it on the wire — the status the call
//! answered with, the version that ran — are facts of the API call and not of the
//! function's output; the optimized integration's result is that call's own SDK response,
//! which carries the returned document under its payload member beside those facts. So
//! one execution naming the SAME function both ways reports two different results, each
//! under its own family's succeeded event.
//!
//! RULE: an attempt is recorded per attempt. A `Retry` whose `ErrorEquals` names the
//! class of the failure the work reported re-does the work, so the family writes its
//! hand-over, start and failure once per attempt — `MaxAttempts` of them beside the first
//! — and never an outcome event of the other kind; a retrier naming a class the failure
//! is not leaves the state at one attempt. Once the attempts are spent the state's
//! `Catch` routes the execution to its own `Next`, whose state then decides the
//! execution's output, so the error document the work produced is never what the
//! execution puts out.
//!
//! The rows differ along what the rule generalises over: the form of the `Resource` (a
//! bare function ARN and the optimized `arn:aws:states:::lambda:invoke` integration, plus
//! a non-Lambda integration), the outcome of the work (the function returned, the
//! function raised), whether the work was handed over at all, the JSON type the function
//! returned, and what a retrier names beside how many attempts it allows and the rate it
//! grows its interval by.
//!
//! The harness (`Chain`, `given_function_of`, `ran`, `sfn`) is the one
//! `lambda_task_control_flow` builds for the same two services; `tests/all.rs` is this
//! binary's crate root, so it is reached through `crate::<suite>`.

use serde_json::{json, Value};

use crate::lambda_function_timeout::event_types;
use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;

/// The handler every row invokes: it returns what the event asked it to carry, and
/// raises when the event asks for it, so the task's outcome is the run's own.
///
/// `pub(crate)`, not private: `tests/all/main.rs` is this binary's crate root, so the
/// sibling suite whose rule is about the same two families of events drives the same
/// handler rather than carrying a second copy of it — and so for every item below that
/// says the same.
pub(crate) const HANDLER_SOURCE: &str = r#"
class OrderRejected(Exception):
    pass


def handler(event, context):
    if event.get("mode") == "raise":
        raise OrderRejected("order %s cannot be processed" % event.get("orderId"))
    return {"orderId": event.get("orderId"), "handled": True}
"#;

/// The `errorType` of the error document that handler's exception produces, which is the
/// name the failed unit of work reports. `pub(crate)` for the reason
/// [`HANDLER_SOURCE`] is.
pub(crate) const RAISED_ERROR_TYPE: &str = "OrderRejected";

/// One row: a `Resource` form, the input the state is handed, and the events its work
/// must write.
struct Row {
    case: &'static str,
    /// Built from the function's ARN, so each row names the same function its own way.
    state: fn(&str) -> Value,
    input: Value,
    /// The status the execution must reach.
    status: &'static str,
    /// The family's own hand-over, start and outcome events, in the order the work
    /// writes them. Empty for work that was never handed over.
    lifecycle: &'static [&'static str],
    /// The detail member the hand-over event carries, and what it must name the work by.
    scheduled: Option<(&'static str, &'static str, &'static str)>,
    /// The error the execution must report, where it failed.
    error: Option<&'static str>,
}

/// A `Task` state whose `Resource` is the function's ARN: the legacy form, whose unit of
/// work is one invocation of that function. `pub(crate)` for the reason
/// [`HANDLER_SOURCE`] is.
pub(crate) fn function_arn_task(function: &str) -> Value {
    json!({
        "Type": "Task",
        "Resource": function,
        "End": true,
    })
}

/// The same work asked for through the optimized integration, which names the function
/// in its `Parameters` instead. `pub(crate)` for the reason [`HANDLER_SOURCE`] is.
pub(crate) fn optimized_task(function: &str) -> Value {
    json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::lambda:invoke",
        "Parameters": { "FunctionName": function, "Payload.$": "$" },
        "End": true,
    })
}

/// A `Parameters` whose path resolves to nothing, so the interpreter fails the execution
/// before any work is handed over.
fn unresolvable_task(function: &str) -> Value {
    json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::lambda:invoke",
        "Parameters": { "FunctionName": function, "Payload.$": "$.absent" },
        "End": true,
    })
}

/// A `Resource` naming an integration of ANOTHER service, which records the `Task*`
/// family exactly as the optimized Lambda one does. `pub(crate)` for the reason
/// [`HANDLER_SOURCE`] is.
pub(crate) fn other_integration_task(_function: &str) -> Value {
    json!({
        "Type": "Task",
        "Resource": "arn:aws:states:::sqs:sendMessage",
        "Parameters": { "QueueUrl": "https://sqs.us-west-2.amazonaws.com/000000000000/absent", "MessageBody": "m" },
        "End": true,
    })
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a function ARN whose invocation returns",
            state: function_arn_task,
            input: json!({ "orderId": "o-1" }),
            status: "SUCCEEDED",
            lifecycle: &[
                "LambdaFunctionScheduled",
                "LambdaFunctionStarted",
                "LambdaFunctionSucceeded",
            ],
            scheduled: Some(("lambdaFunctionScheduledEventDetails", "resource", "")),
            error: None,
        },
        Row {
            case: "a function ARN whose invocation raises",
            state: function_arn_task,
            input: json!({ "orderId": "o-2", "mode": "raise" }),
            status: "FAILED",
            lifecycle: &[
                "LambdaFunctionScheduled",
                "LambdaFunctionStarted",
                "LambdaFunctionFailed",
            ],
            scheduled: Some(("lambdaFunctionScheduledEventDetails", "resource", "")),
            error: Some(RAISED_ERROR_TYPE),
        },
        Row {
            case: "the optimized integration on the same function, which returns",
            state: optimized_task,
            input: json!({ "orderId": "o-3" }),
            status: "SUCCEEDED",
            lifecycle: &["TaskScheduled", "TaskStarted", "TaskSucceeded"],
            scheduled: Some(("taskScheduledEventDetails", "resourceType", "lambda")),
            error: None,
        },
        Row {
            case: "the optimized integration whose function raises",
            state: optimized_task,
            input: json!({ "orderId": "o-4", "mode": "raise" }),
            status: "FAILED",
            lifecycle: &["TaskScheduled", "TaskStarted", "TaskFailed"],
            scheduled: Some(("taskScheduledEventDetails", "resourceType", "lambda")),
            error: Some(RAISED_ERROR_TYPE),
        },
        Row {
            case: "an integration of another service, whose call the service refuses",
            state: other_integration_task,
            input: json!({ "orderId": "o-5" }),
            status: "FAILED",
            lifecycle: &["TaskScheduled", "TaskStarted", "TaskFailed"],
            scheduled: Some(("taskScheduledEventDetails", "resourceType", "sqs")),
            error: None,
        },
        Row {
            case: "a Parameters path that resolves to nothing, so nothing was handed over",
            state: unresolvable_task,
            input: json!({ "orderId": "o-6" }),
            status: "FAILED",
            lifecycle: &[],
            scheduled: None,
            error: Some("States.Runtime"),
        },
    ]
}

/// The positions of every event of type `event_type`, in the order they happened.
fn positions(events: &[Value], event_type: &str) -> Vec<usize> {
    events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.get("type").and_then(Value::as_str) == Some(event_type))
        .map(|(position, _)| position)
        .collect()
}

/// The one event of type `event_type`, where the history holds exactly one.
///
/// `pub(crate)`, not private: `tests/all.rs` is this binary's crate root, so the sibling
/// suite that reads the same history through all three services reaches this rather than
/// carrying a second copy of it, and so does `numbered` below.
pub(crate) fn only<'a>(events: &'a [Value], event_type: &str, case: &str) -> &'a Value {
    let found = positions(events, event_type);
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {event_type}, got {:?}",
        event_types(events)
    );
    &events[found[0]]
}

/// The detail `member` of the one event of type `event_type`, for the same reason
/// [`only`] is reachable.
pub(crate) fn detail(events: &[Value], event_type: &str, member: &str, case: &str) -> Value {
    let event = only(events, event_type, case);
    event
        .get(member)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected {member} on {event}"))
}

/// The events of one execution, read with the members `extra` carries. `pub(crate)` for
/// the reason [`HANDLER_SOURCE`] is.
pub(crate) async fn history(
    chain: &Chain,
    case: &str,
    execution: &str,
    extra: Value,
) -> Vec<Value> {
    let mut request = json!({ "executionArn": execution });
    let members = request.as_object_mut().expect("the request is an object");
    for (member, value) in extra.as_object().expect("the extras are an object") {
        members.insert(member.clone(), value.clone());
    }
    let answered = chain.sfn(case, "GetExecutionHistory", &request).await;
    answered
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected events in {answered}"))
}

/// The family of events a `Task` state's work writes is the one its `Resource` names,
/// and every event of that family names the work the way that family names it.
#[tokio::test]
async fn the_resource_decides_the_family_of_events_the_work_writes() {
    let chain = Chain::new();

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            state,
            input,
            status,
            lifecycle,
            scheduled,
            error,
        } = row;
        let function = chain
            .given_function_of(&format!("families-{index}"), HANDLER_SOURCE, None)
            .await;
        let definition = json!({
            "StartAt": "Work",
            "States": { "Work": state(&function) },
        });
        let described = chain
            .ran(case, &format!("families-{index}"), &definition, &input)
            .await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some(status),
            "case {case:?}: expected the execution to end {status}, got {described}"
        );
        if let Some(error) = error {
            assert_eq!(
                described.get("error").and_then(Value::as_str),
                Some(error),
                "case {case:?}: expected the execution to report {error}, got {described}"
            );
        }

        let execution = string(&described, "executionArn");
        let events = history(&chain, case, &execution, json!({})).await;
        let present = event_types(&events);

        // Exactly the family's own events, each once, in the order the work writes
        // them — and none of the OTHER family's, whatever this work turned out to be.
        for event_type in [
            "TaskScheduled",
            "TaskStarted",
            "TaskSucceeded",
            "TaskFailed",
            "TaskTimedOut",
            "LambdaFunctionScheduled",
            "LambdaFunctionStarted",
            "LambdaFunctionSucceeded",
            "LambdaFunctionFailed",
            "LambdaFunctionTimedOut",
        ] {
            let expected = usize::from(lifecycle.contains(&event_type));
            let counted = positions(&events, event_type).len();
            assert_eq!(
                counted, expected,
                "case {case:?}: expected {expected} {event_type} events, got {present:?}"
            );
        }
        let order: Vec<usize> = lifecycle
            .iter()
            .map(|event_type| {
                *positions(&events, event_type)
                    .first()
                    .unwrap_or_else(|| panic!("case {case:?}: expected a {event_type}"))
            })
            .collect();
        assert!(
            order.windows(2).all(|pair| pair[0] < pair[1]),
            "case {case:?}: expected the work's events in order, got {present:?}"
        );
        // Each of them sits inside the pair the state was entered with, after the state
        // was entered.
        let entered = *positions(&events, "TaskStateEntered")
            .first()
            .unwrap_or_else(|| panic!("case {case:?}: expected a TaskStateEntered in {present:?}"));
        assert!(
            order.iter().all(|position| *position > entered),
            "case {case:?}: expected the work's events after the state was entered, got \
             {present:?}"
        );

        // The hand-over names the work the way its own family names it: the function's
        // ARN alone for an invocation the `Resource` named, the kind of resource and the
        // action for an integration's call.
        if let Some((member, naming, expected)) = scheduled {
            let event = only(&events, lifecycle[0], case);
            let detail = event
                .get(member)
                .unwrap_or_else(|| panic!("case {case:?}: expected {member} on {event}"));
            let expected = if expected.is_empty() {
                function.clone()
            } else {
                expected.to_string()
            };
            assert_eq!(
                detail.get(naming).and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case:?}: expected the hand-over to name the work {expected:?}, got {detail}"
            );
            assert!(
                detail.get("resourceType").is_some() != naming.eq("resource"),
                "case {case:?}: expected only the {member} family's naming members, got {detail}"
            );
        }

        // The outcome of a failed invocation carries the error and the cause the run
        // produced, under the family's own failure detail.
        if let (Some(error), Some(outcome)) = (error, lifecycle.last()) {
            if outcome.ends_with("Failed") {
                let member = format!(
                    "{}EventDetails",
                    outcome[..1].to_lowercase() + &outcome[1..]
                );
                let event = only(&events, outcome, case);
                let detail = event
                    .get(&member)
                    .unwrap_or_else(|| panic!("case {case:?}: expected {member} on {event}"));
                assert_eq!(
                    detail.get("error").and_then(Value::as_str),
                    Some(error),
                    "case {case:?}: expected the failure to report {error}, got {detail}"
                );
            }
        }
    }
}

/// A read that asked for no execution data still reports what the work was handed under
/// `taskScheduledEventDetails.parameters`, and withholds the optional `input`/`output`
/// payload members beside it.
#[tokio::test]
async fn the_required_parameters_of_a_hand_over_survive_a_read_without_execution_data() {
    let chain = Chain::new();
    let function = chain
        .given_function_of("parameters-kept", HANDLER_SOURCE, None)
        .await;

    // (case, the state, the member its hand-over reports the document under, the detail)
    let rows: Vec<(&str, Value, &str, &str)> = vec![
        (
            "the optimized integration, whose hand-over carries required parameters",
            optimized_task(&function),
            "taskScheduledEventDetails",
            "parameters",
        ),
        (
            "a function ARN, whose hand-over carries an optional input",
            function_arn_task(&function),
            "lambdaFunctionScheduledEventDetails",
            "input",
        ),
    ];

    for (index, (case, state, member, document)) in rows.into_iter().enumerate() {
        let definition = json!({ "StartAt": "Work", "States": { "Work": state } });
        let described = chain
            .ran(
                case,
                &format!("parameters-kept-{index}"),
                &definition,
                &json!({ "orderId": "o-1" }),
            )
            .await;
        let execution = string(&described, "executionArn");

        // Read WITH the data, the hand-over reports the document the attempt built.
        let events = history(&chain, case, &execution, json!({})).await;
        let event_type = if member.starts_with("task") {
            "TaskScheduled"
        } else {
            "LambdaFunctionScheduled"
        };
        let with_data = only(&events, event_type, case)
            .get(member)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected {member} in the history"));
        let built = with_data
            .get(document)
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("case {case:?}: expected {document} on {with_data}"));
        assert!(
            built.contains("orderId"),
            "case {case:?}: expected the document the attempt built, got {built:?}"
        );

        // Read WITHOUT it, the REQUIRED member stays and the optional one goes.
        let events = history(
            &chain,
            case,
            &execution,
            json!({ "includeExecutionData": false }),
        )
        .await;
        let without = only(&events, event_type, case)
            .get(member)
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected {member} without the data"));
        let required = document == "parameters";
        let reported = without.get(document).and_then(Value::as_str);
        assert_eq!(
            reported.is_some_and(|text| !text.is_empty()),
            required,
            "case {case:?}: expected {document} to be reported={required} without execution \
             data, got {without}"
        );
        // And the state's own entered event, whose `input` is optional, loses it either
        // way: what `includeExecutionData` withholds is the payload members the model
        // pairs with an `inputDetails`/`outputDetails` flag.
        let entered = only(&events, "TaskStateEntered", case)
            .get("stateEnteredEventDetails")
            .cloned()
            .unwrap_or_else(|| panic!("case {case:?}: expected the entered detail"));
        assert!(
            entered.get("input").is_none_or(Value::is_null),
            "case {case:?}: expected no state input without execution data, got {entered}"
        );
    }
}

/// Every event of one history as `(type, id, previousEventId)`.
pub(crate) fn numbered(events: &[Value]) -> Vec<(String, i64, i64)> {
    events
        .iter()
        .map(|event| {
            (
                event
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                event.get("id").and_then(Value::as_i64).unwrap_or_default(),
                event
                    .get("previousEventId")
                    .and_then(Value::as_i64)
                    .unwrap_or_default(),
            )
        })
        .collect()
}

/// The numbering a list of event types takes when it is the WHOLE history: "numbered
/// sequentially, starting at one", each naming the event before it — except the event
/// written straight after the execution's own `ExecutionStarted`, which has no
/// predecessor and so names none, exactly as the start itself does. `pub(crate)` for the
/// reason [`HANDLER_SOURCE`] is.
pub(crate) fn expected_numbering(types: &[&str]) -> Vec<(String, i64, i64)> {
    types
        .iter()
        .enumerate()
        .map(|(position, event_type)| {
            let id = i64::try_from(position).expect("the count fits") + 1;
            let previous = if id <= 2 { 0 } else { id - 1 };
            ((*event_type).to_string(), id, previous)
        })
        .collect()
}

/// A `Task` state whose work failed records ONE event per lifecycle transition and then
/// the execution's own failure, with contiguous ids — and a redrive appends
/// `ExecutionRedriven` and a fresh cycle that continues the same numbering.
#[tokio::test]
async fn each_lifecycle_transition_is_its_own_event_and_a_redrive_continues_the_numbering() {
    let chain = Chain::new();
    let case = "a Task whose invocation raised, then redriven";
    let function = chain
        .given_function_of("lifecycle-raiser", HANDLER_SOURCE, None)
        .await;
    let definition = json!({
        "StartAt": "Work",
        "States": { "Work": optimized_task(&function) },
    });
    let described = chain
        .ran(
            case,
            "lifecycle-raiser",
            &definition,
            &json!({ "orderId": "o-1", "mode": "raise" }),
        )
        .await;
    let execution = string(&described, "executionArn");

    // The first attempt: the state's own pair around the work's three events, and then
    // the execution's failure. A failed state records no exited event.
    let first = &[
        "ExecutionStarted",
        "TaskStateEntered",
        "TaskScheduled",
        "TaskStarted",
        "TaskFailed",
        "ExecutionFailed",
    ];
    let events = history(&chain, case, &execution, json!({})).await;
    assert_eq!(
        numbered(&events),
        expected_numbering(first),
        "case {case:?}: expected one event per lifecycle transition, got {events:?}"
    );

    // Redriven, the attempt takes up the state it stopped at: the boundary is its own
    // event and the re-run's lifecycle events continue the same id sequence, so a
    // consumer that stored a `previousEventId` reconstructs one chain.
    chain
        .sfn(case, "RedriveExecution", &json!({ "executionArn": execution }))
        .await;
    chain.settled(case, &execution).await;
    let mut whole: Vec<&str> = first.to_vec();
    whole.extend([
        "ExecutionRedriven",
        "TaskScheduled",
        "TaskStarted",
        "TaskFailed",
        "ExecutionFailed",
    ]);
    let events = history(&chain, case, &execution, json!({})).await;
    // The step the redrive resumed was ENTERED by the attempt being redriven and is not
    // entered again, so the first event the new attempt writes for that step follows that
    // preserved entry — the `TaskStateEntered` the first attempt wrote, event 2 — rather
    // than the boundary event the attempt opened with.
    let preserved_entry = 2;
    let resumed = first.len() + 1;
    let mut expected = expected_numbering(&whole);
    expected[resumed].2 = preserved_entry;
    assert_eq!(
        numbered(&events),
        expected,
        "case {case:?}: expected the redriven attempt to continue the numbering, got {events:?}"
    );
    // The boundary names how many times the execution had been redriven when it was
    // written, which is what makes a redriven attempt tell itself from the first.
    let boundary = only(&events, "ExecutionRedriven", case)
        .get("executionRedrivenEventDetails")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected the redrive detail"));
    assert_eq!(
        boundary.get("redriveCount").and_then(Value::as_i64),
        Some(1),
        "case {case:?}: expected the first redrive to be counted, got {boundary}"
    );

    // Read from the END, the same events in reverse: the page a client that wants the
    // latest first asks for opens on the terminal event of the redriven attempt.
    let reversed = history(&chain, case, &execution, json!({ "reverseOrder": true })).await;
    let mut backwards = expected;
    backwards.reverse();
    assert_eq!(
        numbered(&reversed),
        backwards,
        "case {case:?}: expected the reverse order to be the same events read from the end, got \
         {reversed:?}"
    );
}

// ---------------------------------------------------------------------------
// What each family's OUTCOME event reports, for the two forms over one function
// ---------------------------------------------------------------------------

/// The handler the two-form rows invoke: what it RETURNS is decided by the `shape` its
/// event carries, and the object form carries that event back inside the value it answers
/// with, so each form's result is this run's own rather than anything fixed here.
/// [`shaped`] is this handler in Rust, so a row's expectation is what the handler computes
/// for the event that form handed it.
const SHAPING_HANDLER: &str = r#"
def handler(event, context):
    shape = event.get("shape", "object")
    tag = event.get("tag")
    if shape == "list":
        return [tag, 7]
    if shape == "number":
        return 7
    if shape == "text":
        return "seven for %s" % tag
    if shape == "empty":
        return {}
    return {"tag": tag, "seven": 7, "saw": event}
"#;

/// What [`SHAPING_HANDLER`] returns for `event`, as the handler computes it.
fn shaped(event: &Value) -> Value {
    let tag = event.get("tag").cloned().unwrap_or(Value::Null);
    match event
        .get("shape")
        .and_then(Value::as_str)
        .unwrap_or("object")
    {
        "list" => json!([tag, 7]),
        "number" => json!(7),
        "text" => json!(format!("seven for {}", tag.as_str().unwrap_or_default())),
        "empty" => json!({}),
        _ => json!({ "tag": tag, "seven": 7, "saw": event }),
    }
}

/// The members the invocation's own SDK response carries the returned document and the
/// facts of the CALL under (`smithy_query(query="operation",operation="Invoke")` of the
/// Lambda model: `Payload`, `StatusCode`, `ExecutedVersion`).
const PAYLOAD_MEMBER: &str = "Payload";
/// See [`PAYLOAD_MEMBER`].
const STATUS_CODE_MEMBER: &str = "StatusCode";
/// See [`PAYLOAD_MEMBER`].
const EXECUTED_VERSION_MEMBER: &str = "ExecutedVersion";

/// The status a served `RequestResponse` invocation answers with (`Invoke`'s
/// `StatusCode`: "For the `RequestResponse` invocation type, this status code is 200").
const SERVED_STATUS: u64 = 200;

/// The version an UNQUALIFIED invocation runs, which that response names
/// (`FunctionConfiguration$Version`: "the version of the Lambda function", `$LATEST` for
/// the unpublished one).
const UNQUALIFIED_VERSION: &str = "$LATEST";

/// The JSON document the detail member `member` carries as text.
fn document(detail: &Value, member: &str, case: &str) -> Value {
    let text = detail
        .get(member)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected {member} on {detail}"));
    serde_json::from_str(text).unwrap_or_else(|error| {
        panic!("case {case:?}: expected {member} to carry a JSON document, got {error} for {text:?}")
    })
}

/// A definition that invokes the SAME function twice, once in each form: the state's own
/// `Resource` first, then the optimized integration, which is handed `optimized_event` as
/// the function's event.
fn two_form_definition(function: &str, optimized_event: &Value) -> Value {
    json!({
        "StartAt": "Direct",
        "States": {
            "Direct": { "Type": "Task", "Resource": function, "Next": "Optimized" },
            "Optimized": {
                "Type": "Task",
                "Resource": "arn:aws:states:::lambda:invoke",
                "Parameters": { "FunctionName": function, "Payload": optimized_event },
                "End": true,
            },
        },
    })
}

/// Each form's own family reports the result that form gives its state: the function's
/// document bare under `LambdaFunctionSucceeded`, and the invocation's SDK response —
/// that same document under its payload member, beside the status and the version the
/// call answered with — under `TaskSucceeded`.
///
/// The rows differ along the JSON type the function returned, which is what pins the
/// difference to the FORM rather than to the shape of one answer.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_form_of_one_function_reports_its_own_result_in_its_own_family() {
    let chain = Chain::new();
    let function = chain
        .given_function_of("shaping", SHAPING_HANDLER, None)
        .await;

    for (index, shape) in ["object", "list", "number", "text", "empty"]
        .into_iter()
        .enumerate()
    {
        let case = format!("a function returning a {shape} document");
        let case = case.as_str();
        let direct_event = json!({ "shape": shape, "tag": "direct" });
        let optimized_event = json!({ "shape": shape, "tag": "optimized" });
        let described = chain
            .ran(
                case,
                &format!("two-forms-{index}"),
                &two_form_definition(&function, &optimized_event),
                &direct_event,
            )
            .await;
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected both invocations to answer, got {described}"
        );
        let events = history(
            &chain,
            case,
            &string(&described, "executionArn"),
            json!({ "maxResults": 200 }),
        )
        .await;

        // The state whose `Resource` named the function: its family's outcome event
        // carries what the function RETURNED, bare — none of the members the call itself
        // answered with sits in it.
        let succeeded = detail(
            &events,
            "LambdaFunctionSucceeded",
            "lambdaFunctionSucceededEventDetails",
            case,
        );
        let returned = document(&succeeded, "output", case);
        let expected = shaped(&direct_event);
        assert_eq!(
            returned, expected,
            "case {case:?}: expected the function's own document {expected}, got {returned}"
        );
        for member in [PAYLOAD_MEMBER, STATUS_CODE_MEMBER, EXECUTED_VERSION_MEMBER] {
            assert!(
                returned.get(member).is_none(),
                "case {case:?}: expected no {member} around the bare document, got {returned}"
            );
        }

        // The state that asked for the optimized integration: its family's outcome event
        // carries the invocation's SDK response, whose payload member is the document the
        // function returned for the event THAT state handed it.
        let task_succeeded = detail(&events, "TaskSucceeded", "taskSucceededEventDetails", case);
        let envelope = document(&task_succeeded, "output", case);
        let through_the_envelope = shaped(&optimized_event);
        assert_eq!(
            envelope.get(PAYLOAD_MEMBER),
            Some(&through_the_envelope),
            "case {case:?}: expected {through_the_envelope} under {PAYLOAD_MEMBER}, got {envelope}"
        );
        assert_eq!(
            envelope.get(STATUS_CODE_MEMBER).and_then(Value::as_u64),
            Some(SERVED_STATUS),
            "case {case:?}: expected {STATUS_CODE_MEMBER} {SERVED_STATUS}, got {envelope}"
        );
        assert_eq!(
            envelope
                .get(EXECUTED_VERSION_MEMBER)
                .and_then(Value::as_str),
            Some(UNQUALIFIED_VERSION),
            "case {case:?}: expected {EXECUTED_VERSION_MEMBER} {UNQUALIFIED_VERSION}, got \
             {envelope}"
        );
        // The two forms really did report different results for the same function, which
        // is what a client reading the history sees.
        assert_ne!(
            returned.get(PAYLOAD_MEMBER).is_some(),
            envelope.get(PAYLOAD_MEMBER).is_some(),
            "case {case:?}: expected the bare document and the envelope to differ, got \
             {returned} and {envelope}"
        );
    }
}

// ---------------------------------------------------------------------------
// One triple of the family's events per ATTEMPT, and where the Catch routes
// ---------------------------------------------------------------------------

/// The state a row's catcher routes to, and the `Result` that state carries: what the
/// execution puts out once the catcher took the failure over, which is the handling
/// state's own document rather than anything the failed work produced.
const RECOVERED_STATE: &str = "Recovered";

/// See [`RECOVERED_STATE`].
fn recovered_result() -> Value {
    json!({ "recovered": true, "by": RECOVERED_STATE })
}

/// The wildcard the Amazon States Language documents for the class of failure a unit of
/// work reports: "States.TaskFailed […] matches any known error name except
/// States.Timeout".
const TASK_FAILED_WILDCARD: &str = "States.TaskFailed";

/// The class a retrier names in the row whose failure is NOT of it: the language's own
/// timeout error, which is what a bound that ran out reports and never what a handler
/// that raised reports.
const TIMEOUT_ERROR: &str = "States.Timeout";

/// One row: what the retrier names, how many further attempts it allows, the rate it
/// grows its interval by, and how many attempts the state therefore makes.
struct Retried {
    case: &'static str,
    retry_on: &'static str,
    max_attempts: u64,
    backoff_rate: f64,
    attempts: usize,
}

/// A definition whose one `Task` state invokes `function` through its own ARN, retries as
/// `row` asks and routes every failure it could not retry away to [`RECOVERED_STATE`].
fn retried_definition(function: &str, row: &Retried) -> Value {
    json!({
        "StartAt": "Work",
        "States": {
            "Work": {
                "Type": "Task",
                "Resource": function,
                "Retry": [{
                    "ErrorEquals": [row.retry_on],
                    "IntervalSeconds": 1,
                    "BackoffRate": row.backoff_rate,
                    "MaxAttempts": row.max_attempts,
                }],
                "Catch": [{ "ErrorEquals": ["States.ALL"], "Next": RECOVERED_STATE }],
                "End": true,
            },
            RECOVERED_STATE: { "Type": "Pass", "Result": recovered_result(), "End": true },
        },
    })
}

/// Every detail of type `event_type` the history carries, in the order they happened.
fn details(events: &[Value], event_type: &str, member: &str, case: &str) -> Vec<Value> {
    events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .map(|event| {
            event
                .get(member)
                .cloned()
                .unwrap_or_else(|| panic!("case {case:?}: expected {member} on {event}"))
        })
        .collect()
}

/// A handler that raised fails the task, so the state's `Retry` re-does the work — one
/// triple of the family's events per attempt, never an outcome of the other kind — and
/// its `Catch` routes the execution to the handling state, whose own result is what the
/// execution puts out.
///
/// The rows differ along what the retrier names (the language's wildcard for a failed unit
/// of work, the class the run itself reported, and a class this failure is not), how many
/// further attempts it allows and the rate it grows its interval by.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_retried_invocation_writes_its_family_s_events_once_per_attempt() {
    let chain = Chain::new();
    let function = chain
        .given_function_of("retried-raiser", HANDLER_SOURCE, None)
        .await;

    let rows = [
        Retried {
            case: "the wildcard for a failed unit of work, retried twice with a growing interval",
            retry_on: TASK_FAILED_WILDCARD,
            max_attempts: 2,
            backoff_rate: 2.0,
            attempts: 3,
        },
        Retried {
            case: "the class the run itself reported, retried once",
            retry_on: RAISED_ERROR_TYPE,
            max_attempts: 1,
            backoff_rate: 1.5,
            attempts: 2,
        },
        Retried {
            case: "a class this failure is not, which leaves the state at one attempt",
            retry_on: TIMEOUT_ERROR,
            max_attempts: 3,
            backoff_rate: 1.0,
            attempts: 1,
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let described = chain
            .ran(
                case,
                &format!("retried-{index}"),
                &retried_definition(&function, row),
                &json!({ "orderId": format!("o-{index}"), "mode": "raise" }),
            )
            .await;
        // The catcher took the failure over, so the execution reached the handling state
        // and put out THAT state's result — not the error document the run produced, and
        // not an invocation's response.
        assert_eq!(
            described.get("status").and_then(Value::as_str),
            Some("SUCCEEDED"),
            "case {case:?}: expected the catcher to route the execution, got {described}"
        );
        let output = document(&described, "output", case);
        assert_eq!(
            output,
            recovered_result(),
            "case {case:?}: expected the handling state's own result, got {output}"
        );

        let events = history(
            &chain,
            case,
            &string(&described, "executionArn"),
            json!({ "maxResults": 200 }),
        )
        .await;
        let present = event_types(&events);
        // One triple of the family's own events per attempt, and no outcome event of the
        // other kind whatever the work turned out to be.
        for event_type in [
            "LambdaFunctionScheduled",
            "LambdaFunctionStarted",
            "LambdaFunctionFailed",
        ] {
            assert_eq!(
                positions(&events, event_type).len(),
                row.attempts,
                "case {case:?}: expected {} {event_type} events, got {present:?}",
                row.attempts
            );
        }
        for event_type in [
            "LambdaFunctionSucceeded",
            "TaskScheduled",
            "TaskStarted",
            "TaskFailed",
            "TaskSucceeded",
        ] {
            assert_eq!(
                positions(&events, event_type).len(),
                0,
                "case {case:?}: expected no {event_type} events, got {present:?}"
            );
        }
        // Each attempt's failure carries the error document that run produced: the class
        // it raised, and the message it built from this execution's own input.
        for failed in details(
            &events,
            "LambdaFunctionFailed",
            "lambdaFunctionFailedEventDetails",
            case,
        ) {
            assert_eq!(
                failed.get("error").and_then(Value::as_str),
                Some(RAISED_ERROR_TYPE),
                "case {case:?}: expected the failure to name {RAISED_ERROR_TYPE}, got {failed}"
            );
            let reported = document(&failed, "cause", case);
            assert_eq!(
                reported.get("errorType").and_then(Value::as_str),
                Some(RAISED_ERROR_TYPE),
                "case {case:?}: expected the run's own error document, got {reported}"
            );
            assert_eq!(
                reported.get("errorMessage").and_then(Value::as_str),
                Some(format!("order o-{index} cannot be processed").as_str()),
                "case {case:?}: expected the message the handler built, got {reported}"
            );
        }
        // And the execution ended at the handling state, which is where the catcher sent
        // it rather than under the failure.
        assert_eq!(
            positions(&events, "PassStateEntered").len(),
            1,
            "case {case:?}: expected the handling state to be entered once, got {present:?}"
        );
        assert_eq!(
            positions(&events, "ExecutionFailed").len(),
            0,
            "case {case:?}: expected no failure for the execution, got {present:?}"
        );
    }
}
