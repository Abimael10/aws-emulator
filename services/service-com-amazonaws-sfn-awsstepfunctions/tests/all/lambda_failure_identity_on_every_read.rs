// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHOSE failure identity an execution's history records when a
//! `Task` state's Lambda function RAN and raised, and for that identity being a stored
//! fact of the run rather than a product of the read — driven over both services' own wire
//! protocols through the production `MultiServiceRouter` with Step Functions and Lambda
//! mounted on ONE engine, the shape `core/router/src/lib.rs` mounts them in, so the
//! function really runs and every event read back is one that run wrote.
//!
//! RULE: the `error`/`cause` pair a failed `Task` state records names what actually stopped
//! the work. A function that ran and RAISED fails the state under the `errorType` of the
//! error document that run produced, with that document as the cause, and that pair is
//! carried by the state's own failure event of the family its `Resource` names
//! (`TaskFailed` for a `Resource` naming a service integration,
//! `LambdaFunctionFailed` for a `Resource` that is the function's own ARN) and repeated
//! verbatim by the terminal `ExecutionFailed` event; an error name of the invoked service's
//! own API, with that API's refusal as the cause, belongs to a call the API refused, which
//! is not what a run that raised is. Neither member is execution data and neither is
//! computed by the read, so EVERY read of the stored history reports the same pair — the
//! whole history, the reverse order, one event a page walked by token, and a read that
//! asked for no execution data — and so does the read after a redrive, whose further
//! attempt records its own failure again while every event still names the event before it
//! (`previousEventId`, "the id of the previous event"), so a walk back from a terminal
//! event reaches the entry of the state whose work failed.
//!
//! The rows differ along what the rule generalises over: the form the `Task` names its work
//! in (the optimized `arn:aws:states:::lambda:invoke` integration and the function's own
//! ARN, which record the two different event families), the class the handler raised (one
//! of the language's own and one the package declares itself), and the order the input
//! carries, which the raised message is built from.
//!
//! The harness (`Chain`, `given_function_of`, `ran`, `settled`, `sfn`) is the one
//! `lambda_task_control_flow` builds for these two services, the two `Resource` forms are
//! `task_work_event_families`' own, and the event types are read through
//! `lambda_function_timeout`'s reading of them; `tests/all/main.rs` is this binary's crate
//! root, so each is reached through `crate::<suite>`.

use std::collections::BTreeMap;

use serde_json::{json, Value};

use crate::lambda_function_timeout::event_types;
use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::string;
use crate::task_work_event_families::{function_arn_task, optimized_task};

/// The handler every row invokes: the class it raises is decided by the `kind` the EVENT
/// carries and the message it builds embeds the event's `order`, so the error document the
/// task fails under is the run's own and this execution's input's rather than anything
/// fixed here.
const HANDLER_SOURCE: &str = r#"
class OrderRejected(Exception):
    pass


def handler(event, context):
    order = event["order"]
    if event.get("kind") == "builtin":
        raise RuntimeError("order %s cannot be processed" % order)
    if event.get("kind") == "declared":
        raise OrderRejected("order %s cannot be processed" % order)
    return {"order": order, "handled": True}
"#;

/// The one state every row's definition carries, whose work is the invocation.
const WORK_STATE: &str = "Work";

/// The event the walk back from a terminal event must reach: the entry of the state whose
/// work failed.
const STATE_ENTERED: &str = "TaskStateEntered";

/// The event a redrive opens the further attempt with, which is the boundary between the
/// attempt that was preserved and the one that continued it.
const EXECUTION_REDRIVEN: &str = "ExecutionRedriven";

/// The execution's own terminal failure event, which repeats the state's pair.
const EXECUTION_FAILED: &str = "ExecutionFailed";

/// The fixed text of the authorization refusal an invoked service answers a call its
/// caller's policies do not allow with, which is the identity of a call that was REFUSED
/// and so of a function that never ran: no read of a run that raised may carry it.
const REFUSAL_TEXT: &str = "is not authorized to perform:";

/// How many events one page of the paged read asks for, which is what makes that read walk
/// the whole history a token at a time.
const ONE_EVENT_A_PAGE: u64 = 1;

/// A ceiling on how many pages the paged read walks before the test gives up, so a token
/// that never ends is a failure rather than a loop: far more than any row's history holds.
const PAGE_CEILING: usize = 200;

/// Which form a row's `Task` names its work in.
#[derive(Debug, Clone, Copy)]
enum Resource {
    /// The optimized integration token, with the function named by a parameter: the unit of
    /// work is one API call, recorded under the `Task*` family.
    Optimized,
    /// The function's own ARN: the unit of work is one invocation of that function,
    /// recorded under the `LambdaFunction*` family.
    FunctionArn,
}

impl Resource {
    /// The `Task` state this form names `function`'s work in.
    fn task(self, function: &str) -> Value {
        match self {
            Self::Optimized => optimized_task(function),
            Self::FunctionArn => function_arn_task(function),
        }
    }

    /// The event the state's own failure is recorded as in this form's family, and the
    /// detail member that event carries the failure under.
    fn failure(self) -> (&'static str, &'static str) {
        match self {
            Self::Optimized => ("TaskFailed", "taskFailedEventDetails"),
            Self::FunctionArn => ("LambdaFunctionFailed", "lambdaFunctionFailedEventDetails"),
        }
    }
}

/// One row: the form the work is named in, the class the handler is asked to raise, the
/// `errorType` the document that raise produces names, and the order the input carries.
struct Row {
    case: &'static str,
    resource: Resource,
    /// The `kind` the input carries, which selects the class the handler raises.
    kind: &'static str,
    /// The class that raise reports, which is the `errorType` of the run's error document
    /// and so the name the failed state reports.
    error_type: &'static str,
    /// The `order` the input carries, which the raised message is built from.
    order: u64,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the optimized integration, whose function raised one of the language's \
                   own classes",
            resource: Resource::Optimized,
            kind: "builtin",
            error_type: "RuntimeError",
            order: 17,
        },
        Row {
            case: "the optimized integration, whose function raised a class the package \
                   declares",
            resource: Resource::Optimized,
            kind: "declared",
            error_type: "OrderRejected",
            order: 4096,
        },
        Row {
            case: "the function's own ARN, whose invocation raised one of the language's \
                   own classes",
            resource: Resource::FunctionArn,
            kind: "builtin",
            error_type: "RuntimeError",
            order: 5,
        },
        Row {
            case: "the function's own ARN, whose invocation raised a class the package \
                   declares",
            resource: Resource::FunctionArn,
            kind: "declared",
            error_type: "OrderRejected",
            order: 23,
        },
    ]
}

/// The message the handler builds for `order`, built the way the handler builds it.
fn raised_message(order: u64) -> String {
    format!("order {order} cannot be processed")
}

/// The definition a row runs: ONE `Task` state naming its work in `resource`'s form, with
/// no `Retry` and no `Catch`, so the state's failure is the execution's.
fn definition(resource: Resource, function: &str) -> Value {
    json!({
        "StartAt": WORK_STATE,
        "States": { WORK_STATE: resource.task(function) },
    })
}

/// One page of an execution's history: the events it carries and the cursor that continues
/// it.
async fn page(chain: &Chain, case: &str, request: &Value) -> (Vec<Value>, Option<String>) {
    let answered = chain.sfn(case, "GetExecutionHistory", request).await;
    let events = answered
        .get("events")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: expected events in {answered}"));
    let token = answered
        .get("nextToken")
        .and_then(Value::as_str)
        .map(str::to_string);
    (events, token)
}

/// The whole history of `execution` as one read with the members `extra` carries answers
/// it.
async fn read(chain: &Chain, case: &str, execution: &str, extra: &Value) -> Vec<Value> {
    let mut request = json!({ "executionArn": execution });
    let members = request.as_object_mut().expect("the request is an object");
    for (member, value) in extra.as_object().expect("the extras are an object") {
        members.insert(member.clone(), value.clone());
    }
    page(chain, case, &request).await.0
}

/// The history of `execution` read ONE event a page, each page continuing the one before
/// through the cursor it handed out.
async fn paged(chain: &Chain, case: &str, execution: &str) -> Vec<Value> {
    let mut collected: Vec<Value> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..PAGE_CEILING {
        let mut request = json!({
            "executionArn": execution,
            "maxResults": ONE_EVENT_A_PAGE,
        });
        if let Some(cursor) = &token {
            request["nextToken"] = json!(cursor);
        }
        let (events, next) = page(chain, case, &request).await;
        assert!(
            !events.is_empty(),
            "case {case:?}: expected every page of the history to carry an event, got none \
             after {} of them",
            collected.len()
        );
        collected.extend(events);
        token = next;
        if token.is_none() {
            return collected;
        }
    }
    panic!(
        "case {case:?}: expected the paged read to end within {PAGE_CEILING} pages, got {} \
         events and a cursor still",
        collected.len()
    )
}

/// The `(error, cause)` pair every event of type `event_type` carries under `member`, in
/// the order those events are answered in.
fn identities(
    events: &[Value],
    event_type: &str,
    member: &str,
    case: &str,
    reading: &str,
) -> Vec<(String, String)> {
    events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .map(|event| {
            let detail = event.get(member).unwrap_or_else(|| {
                panic!("case {case:?}, read {reading:?}: expected {member} on {event}")
            });
            let text = |name: &str| {
                detail
                    .get(name)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("case {case:?}, read {reading:?}: expected a {name} on {detail}")
                    })
                    .to_string()
            };
            (text("error"), text("cause"))
        })
        .collect()
}

/// The two events one attempt's failure is recorded on: the state's own, in the family its
/// `Resource` names, and the execution's terminal one.
fn failure_events(resource: Resource) -> [(&'static str, &'static str); 2] {
    let (event_type, member) = resource.failure();
    [
        (event_type, member),
        (EXECUTION_FAILED, "executionFailedEventDetails"),
    ]
}

/// Assert that `reading` of the history reports the pair `expected` — the very bytes one
/// stored event carries — on the state's own failure event and on the execution's terminal
/// one, which is what makes the identity a fact of the run rather than of the read.
fn assert_same_identity(
    events: &[Value],
    resource: Resource,
    expected: &(String, String),
    case: &str,
    reading: &str,
) {
    for (event_type, member) in failure_events(resource) {
        let found = identities(events, event_type, member, case, reading);
        assert_eq!(
            found,
            vec![expected.clone()],
            "case {case:?}, read {reading:?}: expected one {event_type} event carrying \
             {expected:?}, got {found:?} out of {:?}",
            event_types(events)
        );
    }
}

/// Assert that `reading` reports, once per attempt the history holds, the class the run
/// raised and THAT run's own error document — the class and the message it built from this
/// execution's input, each attempt's document being the one its own invocation answered
/// (which is why the documents are compared by what the run decided rather than whole:
/// the invocation each attempt reports is its own).
fn assert_raised_identity(
    events: &[Value],
    resource: Resource,
    error_type: &str,
    message: &str,
    attempts: usize,
    case: &str,
    reading: &str,
) {
    for (event_type, member) in failure_events(resource) {
        let found = identities(events, event_type, member, case, reading);
        assert_eq!(
            found.len(),
            attempts,
            "case {case:?}, read {reading:?}: expected {attempts} {event_type} event(s), got \
             {found:?} out of {:?}",
            event_types(events)
        );
        for (error, cause) in &found {
            assert_eq!(
                error, error_type,
                "case {case:?}, read {reading:?}: expected {event_type} to name \
                 {error_type:?}, got {error:?}"
            );
            // An authorization refusal is the identity of a call that was never made, so no
            // read of a run that RAN may carry one.
            assert!(
                !cause.contains(REFUSAL_TEXT),
                "case {case:?}, read {reading:?}: expected the function's own failure on \
                 {event_type}, got the refusal {cause:?}"
            );
            let reported = document(cause, case);
            assert_eq!(
                (
                    reported.get("errorType").and_then(Value::as_str),
                    reported.get("errorMessage").and_then(Value::as_str),
                ),
                (Some(error_type), Some(message)),
                "case {case:?}, read {reading:?}: expected {event_type} to carry the run's own \
                 error document, got {reported}"
            );
        }
    }
}

/// The document `cause` carries, which must be the error document the run produced.
fn document(cause: &str, case: &str) -> Value {
    serde_json::from_str(cause).unwrap_or_else(|error| {
        panic!(
            "case {case:?}: expected the cause to be the run's own error document, got {error} \
             for {cause:?}"
        )
    })
}

/// The position of the one event of type `event_type`, which the history must carry exactly
/// once.
fn position_of(events: &[Value], event_type: &str, case: &str) -> usize {
    let found: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, event)| event.get("type").and_then(Value::as_str) == Some(event_type))
        .map(|(position, _)| position)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "case {case:?}: expected exactly one {event_type}, got {:?}",
        event_types(events)
    );
    found[0]
}

/// The id of the one event of type `event_type`.
fn only_id(events: &[Value], event_type: &str, case: &str) -> i64 {
    let event = &events[position_of(events, event_type, case)];
    event
        .get("id")
        .and_then(Value::as_i64)
        .unwrap_or_else(|| panic!("case {case:?}: expected an id on {event}"))
}

/// The events of one history by their own id, which is how a reader walks the chain.
fn by_id(events: &[Value], case: &str) -> BTreeMap<i64, Value> {
    events
        .iter()
        .map(|event| {
            let id = event
                .get("id")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| panic!("case {case:?}: expected an id on {event}"));
            (id, event.clone())
        })
        .collect()
}

/// Walk `previousEventId` back from the LAST event of type `event_type` and answer the
/// events the walk visited, newest first. Every link must name an event this history holds
/// with a smaller id, so the walk is finite and reconstructs one chain.
fn chain_back(events: &[Value], event_type: &str, case: &str) -> Vec<Value> {
    let held = by_id(events, case);
    let mut at = events
        .iter()
        .filter(|event| event.get("type").and_then(Value::as_str) == Some(event_type))
        .filter_map(|event| event.get("id").and_then(Value::as_i64))
        .next_back()
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a {event_type} event to walk back from, got {:?}",
                event_types(events)
            )
        });
    let mut walked: Vec<Value> = Vec::new();
    loop {
        let event = held
            .get(&at)
            .unwrap_or_else(|| panic!("case {case:?}: expected event {at} in this history"));
        walked.push(event.clone());
        let previous = event
            .get("previousEventId")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| panic!("case {case:?}: expected a previousEventId on {event}"));
        if previous == 0 {
            return walked;
        }
        assert!(
            previous < at && held.contains_key(&previous),
            "case {case:?}: expected event {at} to name an earlier event of this history, got \
             previousEventId {previous}"
        );
        at = previous;
    }
}

/// The name the one `TaskStateEntered` event the walk ENDED on carries, which is the state
/// a reader walking back from a terminal event arrives at.
fn entered_state(walked: &[Value], case: &str) -> String {
    let last = walked
        .last()
        .unwrap_or_else(|| panic!("case {case:?}: expected the walk to visit an event"));
    assert_eq!(
        last.get("type").and_then(Value::as_str),
        Some(STATE_ENTERED),
        "case {case:?}: expected the chain to end at the state's own entry, got {last}"
    );
    last.pointer("/stateEnteredEventDetails/name")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("case {case:?}: expected the entered state's name on {last}"))
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_raised_functions_identity_is_what_every_read_of_the_history_reports() {
    let chain = Chain::new();
    let function = chain
        .given_function_of("identity-raiser", HANDLER_SOURCE, None)
        .await;

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            resource,
            kind,
            error_type,
            order,
        } = row;
        let described = chain
            .ran(
                case,
                &format!("identity-{index}"),
                &definition(resource, &function),
                &json!({ "kind": kind, "order": order }),
            )
            .await;
        let execution = string(&described, "executionArn");

        // What the execution ended as: the class the run raised, never the invoked API's
        // own error name.
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(error_type)),
            "case {case:?}: expected the execution to fail under {error_type:?}, got {described}"
        );

        // The identity the stored history carries, read whole and in order: the class the
        // run raised on the state's own failure event, the error document that run produced
        // as the cause, and the very same pair on the execution's terminal event.
        let message = raised_message(order);
        let whole = read(&chain, case, &execution, &json!({})).await;
        assert_raised_identity(
            &whole,
            resource,
            error_type,
            &message,
            1,
            case,
            "the whole history",
        );
        let (failure_event, failure_member) = resource.failure();
        let expected = identities(&whole, failure_event, failure_member, case, "the whole history")
            .first()
            .cloned()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected a {failure_event} event, got {:?}",
                    event_types(&whole)
                )
            });
        assert_same_identity(&whole, resource, &expected, case, "the whole history");

        // Every other read of the same stored history reports the same pair: the reverse
        // order, one event a page walked by the cursors this history handed out, and a read
        // that asked for no execution data — of which the failure's `error` and `cause` are
        // neither.
        let mut backwards = whole.clone();
        backwards.reverse();
        let reversed = read(&chain, case, &execution, &json!({ "reverseOrder": true })).await;
        assert_eq!(
            reversed, backwards,
            "case {case:?}: expected the reverse order to be the same events read from the end"
        );
        assert_same_identity(&reversed, resource, &expected, case, "the reverse order");

        let walked = paged(&chain, case, &execution).await;
        assert_eq!(
            walked, whole,
            "case {case:?}: expected the pages to concatenate to the whole history"
        );
        assert_same_identity(&walked, resource, &expected, case, "one event a page");

        let bare = read(
            &chain,
            case,
            &execution,
            &json!({ "includeExecutionData": false }),
        )
        .await;
        assert_same_identity(&bare, resource, &expected, case, "no execution data");

        // The chain the ids state: the events are numbered sequentially from one, and
        // walking back from the terminal event reaches the entry of the state whose work
        // failed, so a reader reconstructs the failure's own step.
        let numbered: Vec<i64> = whole
            .iter()
            .filter_map(|event| event.get("id").and_then(Value::as_i64))
            .collect();
        let sequential: Vec<i64> = (1..=i64::try_from(whole.len()).expect("the count fits"))
            .collect();
        assert_eq!(
            numbered, sequential,
            "case {case:?}: expected the events to be numbered sequentially from one"
        );
        assert_eq!(
            entered_state(&chain_back(&whole, EXECUTION_FAILED, case), case),
            WORK_STATE,
            "case {case:?}: expected the chain from the failure to reach the work state's entry"
        );

        // Redriven, the further attempt invokes the same function again and records its own
        // failure under the same identity, while what the first attempt wrote is still
        // there, unchanged, and the chain from the new terminal event still reaches the
        // entry the preserved attempt wrote for that step.
        chain
            .sfn(case, "RedriveExecution", &json!({ "executionArn": &execution }))
            .await;
        chain.settled(case, &execution).await;
        let redriven = read(&chain, case, &execution, &json!({})).await;
        assert_eq!(
            redriven.get(..whole.len()).map(<[Value]>::to_vec),
            Some(whole.clone()),
            "case {case:?}: expected the preserved attempt's events first, got {:?}",
            event_types(&redriven)
        );
        assert_raised_identity(
            &redriven,
            resource,
            error_type,
            &message,
            2,
            case,
            "after a redrive",
        );
        assert_eq!(
            entered_state(&chain_back(&redriven, EXECUTION_FAILED, case), case),
            WORK_STATE,
            "case {case:?}: expected the redriven attempt's chain to reach the same entry"
        );
        // And the link itself, which is what a client walking the chain of a redriven
        // execution follows: the step the redrive resumed was ENTERED by the attempt being
        // redriven and is not entered again, so the FIRST event the further attempt writes
        // for that step names that preserved entry — not the boundary event the attempt
        // opened with, and not the event the previous attempt ended on.
        let entry = only_id(&redriven, STATE_ENTERED, case);
        let boundary = position_of(&redriven, EXECUTION_REDRIVEN, case);
        let resumed = redriven
            .get(boundary + 1)
            .unwrap_or_else(|| panic!("case {case:?}: expected an event after the boundary"));
        assert_eq!(
            resumed.get("previousEventId").and_then(Value::as_i64),
            Some(entry),
            "case {case:?}: expected the resumed step's first event to follow the preserved \
             entry {entry}, got {resumed}"
        );
    }
}
