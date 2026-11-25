// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a PAGED read of the history of an execution whose Lambda
//! `Task` state failed reports, driven over both services' own wire protocols through the
//! production `MultiServiceRouter` with Step Functions and Lambda mounted on ONE engine —
//! the shape `core/router/src/lib.rs` mounts them in — so the function really runs, the
//! failure every page carries is the one that run recorded, and each cursor is one this
//! service really handed out.
//!
//! RULE: how a history is PAGED decides nothing about what it says. A failure event names
//! what actually stopped the work — a function that ran and raised fails the state under the
//! `errorType` of the error document that run produced, with that document as the cause,
//! carried by the state's own failure event of the family its `Resource` names and repeated
//! by the terminal `ExecutionFailed` event, never an error name and refusal sentence of a
//! call the invoked API turned down — and the `nextToken` that continues the read is an
//! opaque continuation value that names no position in the stored order, so the decimal
//! index the next page begins at is a token this service never issued and is refused
//! `InvalidToken` (400) rather than served as that page.
//!
//! The rows differ along what the rule generalises over: the form the `Task` names its work
//! in (the optimized `arn:aws:states:::lambda:invoke` integration and the function's own
//! ARN, which record the two different failure-event families) and the data the input
//! carries, which the raised message is built from; and each row is read at every page
//! boundary its own history has — one event a page, two, three, the page size that ends the
//! page exactly before the state's failure event, the one that ends it exactly after, and
//! the sizes at and just under the whole history — so no page size holds the failure event
//! in a fixed place.
//!
//! The modelled bounds asserted here are this service's own
//! (`smithy_query(query="operation",operation="GetExecutionHistory")` for `PageToken`
//! @length 1..1024, and `query="shape"` of `TaskFailedEventDetails`,
//! `LambdaFunctionFailedEventDetails` and `ExecutionFailedEventDetails` for
//! `SensitiveError` @length 0..256 and `SensitiveCause` @length 0..32768).
//!
//! The harness (`Chain`, `given_function_of`, `ran`, `sfn`, `sfn_answered`) is the one
//! `lambda_task_control_flow` builds for these two services, the handler and the two
//! `Resource` forms are `task_work_event_families`' own, the failure pairs are read through
//! `lambda_failure_identity_on_every_read`'s reading of them, and the error code of a
//! refusal through `state_machine_lifecycle`'s; `tests/all/main.rs` is this binary's crate
//! root, so each is reached through `crate::<suite>`.

use std::collections::BTreeSet;

use serde_json::{json, Value};

use crate::lambda_failure_identity_on_every_read::identities;
use crate::lambda_function_timeout::event_types;
use crate::lambda_task_control_flow::Chain;
use crate::state_machine_lifecycle::{error_code, string};
use crate::task_work_event_families::{
    function_arn_task, optimized_task, HANDLER_SOURCE, RAISED_ERROR_TYPE,
};

/// The one state every row's definition carries, whose work is the invocation.
const WORK_STATE: &str = "Work";

/// The execution's own terminal failure event, which repeats the state's pair.
const EXECUTION_FAILED: &str = "ExecutionFailed";

/// The fixed text of the authorization refusal an invoked service answers a call its
/// caller's policies do not allow with, which is the identity of a call that was REFUSED
/// and so of a function that never ran: no page of a run that raised may carry it.
const REFUSAL_TEXT: &str = "is not authorized to perform:";

/// The widest a `nextToken` may be: `PageToken` @length 1..1024 of this service's model.
const MAX_TOKEN_BYTES: usize = 1024;

/// The widest a failure's `error` may be: `SensitiveError` @length 0..256.
const MAX_ERROR_BYTES: usize = 256;

/// The widest a failure's `cause` may be: `SensitiveCause` @length 0..32768.
const MAX_CAUSE_BYTES: usize = 32768;

/// A ceiling on how many pages one read walks before the test gives up, so a cursor that
/// never ends is a failure rather than a loop: far more than any row's history holds.
const PAGE_CEILING: usize = 200;

/// The error a token no read of this history was handed is refused with, and the status it
/// is reported under (`InvalidToken`, HTTP 400, of this operation's modelled errors).
const INVALID_TOKEN: (u16, &str) = (400, "InvalidToken");

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

/// One row: the form the work is named in and the order the input carries, which the
/// raised message is built from.
struct Row {
    case: &'static str,
    resource: Resource,
    order_id: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the optimized integration, whose function raised",
            resource: Resource::Optimized,
            order_id: "o-paged-1",
        },
        Row {
            case: "the function's own ARN, whose invocation raised",
            resource: Resource::FunctionArn,
            order_id: "o-paged-2",
        },
    ]
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

/// The message the handler builds for `order_id`, built the way that handler builds it.
fn raised_message(order_id: &str) -> String {
    format!("order {order_id} cannot be processed")
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

/// The whole history of `execution` as one read, which is the stored order every paged read
/// of it has to reconstruct.
async fn whole(chain: &Chain, case: &str, execution: &str) -> Vec<Value> {
    page(chain, case, &json!({ "executionArn": execution }))
        .await
        .0
}

/// `execution`'s history read `page_size` events a page: the events the pages carried, in
/// order, and every cursor handed out with the position in the stored order the page it
/// continues begins at.
async fn paged(
    chain: &Chain,
    case: &str,
    execution: &str,
    page_size: u64,
) -> (Vec<Value>, Vec<(usize, String)>) {
    let mut collected: Vec<Value> = Vec::new();
    let mut cursors: Vec<(usize, String)> = Vec::new();
    let mut token: Option<String> = None;
    for _ in 0..PAGE_CEILING {
        let mut request = json!({ "executionArn": execution, "maxResults": page_size });
        if let Some(cursor) = &token {
            request["nextToken"] = json!(cursor);
        }
        let (events, next) = page(chain, case, &request).await;
        assert!(
            !events.is_empty(),
            "case {case:?}, {page_size} a page: expected every page to carry an event, got none \
             after {} of them",
            collected.len()
        );
        assert!(
            u64::try_from(events.len()).expect("a page's size fits") <= page_size,
            "case {case:?}, {page_size} a page: expected at most {page_size} events, got {}",
            events.len()
        );
        collected.extend(events);
        match next {
            None => return (collected, cursors),
            Some(cursor) => {
                cursors.push((collected.len(), cursor.clone()));
                token = Some(cursor);
            }
        }
    }
    panic!(
        "case {case:?}, {page_size} a page: expected the read to end within {PAGE_CEILING} \
         pages, got {} events and a cursor still",
        collected.len()
    )
}

/// Assert that every cursor this read handed out is an opaque continuation value: within
/// the modelled width, carrying no decimal position at all (so neither the index the next
/// page begins at nor any other number is legible in it), and its own page's, which is what
/// stops a client advancing one arithmetically.
fn assert_opaque(case: &str, page_size: u64, cursors: &[(usize, String)]) {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for (offset, token) in cursors {
        assert!(
            !token.is_empty() && token.len() <= MAX_TOKEN_BYTES,
            "case {case:?}, {page_size} a page: expected a cursor of 1..={MAX_TOKEN_BYTES} \
             bytes, got {} in {token:?}",
            token.len()
        );
        assert!(
            token.trim().parse::<i128>().is_err(),
            "case {case:?}, {page_size} a page: expected an opaque cursor at position \
             {offset}, got the position itself in {token:?}"
        );
        assert!(
            seen.insert(token.as_str()),
            "case {case:?}, {page_size} a page: expected each page's cursor to be its own, got \
             {token:?} twice"
        );
    }
}

/// Assert that the pages this read walked report the failure the run recorded: the class the
/// function raised on the state's own failure event and on the execution's terminal one, the
/// error document that run produced as the cause, both within their modelled widths, and no
/// refusal of a call that was never made.
fn assert_raised_identity(
    events: &[Value],
    resource: Resource,
    message: &str,
    case: &str,
    reading: &str,
) {
    for (event_type, member) in failure_events(resource) {
        let found = identities(events, event_type, member, case, reading);
        assert_eq!(
            found.len(),
            1,
            "case {case:?}, read {reading:?}: expected one {event_type} event, got {found:?} out \
             of {:?}",
            event_types(events)
        );
        let (error, cause) = &found[0];
        assert_eq!(
            error, RAISED_ERROR_TYPE,
            "case {case:?}, read {reading:?}: expected {event_type} to name \
             {RAISED_ERROR_TYPE:?}, got {error:?} with cause {cause:?}"
        );
        assert!(
            !cause.contains(REFUSAL_TEXT),
            "case {case:?}, read {reading:?}: expected the function's own failure on \
             {event_type}, got the refusal {cause:?}"
        );
        assert!(
            error.len() <= MAX_ERROR_BYTES && cause.len() <= MAX_CAUSE_BYTES,
            "case {case:?}, read {reading:?}: expected {event_type} within the modelled widths, \
             got an error of {} and a cause of {} bytes",
            error.len(),
            cause.len()
        );
        let reported: Value = serde_json::from_str(cause).unwrap_or_else(|error| {
            panic!(
                "case {case:?}, read {reading:?}: expected the cause to be the run's own error \
                 document, got {error} for {cause:?}"
            )
        });
        assert_eq!(
            (
                reported.get("errorType").and_then(Value::as_str),
                reported.get("errorMessage").and_then(Value::as_str),
            ),
            (Some(RAISED_ERROR_TYPE), Some(message)),
            "case {case:?}, read {reading:?}: expected {event_type} to carry the run's own error \
             document, got {reported}"
        );
    }
}

/// The position of the one event of type `event_type` in the stored order.
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

/// Every page size this row is read at: the smallest few, the two that put a page boundary
/// either side of the state's own failure event, and the sizes at and just under the whole
/// history — so the failure event falls in a different place in every read.
fn page_sizes(total: usize, failure_at: usize) -> Vec<u64> {
    let candidates = [1, 2, 3, failure_at, failure_at + 1, total - 1, total];
    let sizes: BTreeSet<u64> = candidates
        .into_iter()
        .filter(|size| *size >= 1)
        .map(|size| u64::try_from(size).expect("a page size fits"))
        .collect();
    sizes.into_iter().collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_page_of_a_failed_lambda_tasks_history_reports_the_run_and_an_opaque_cursor() {
    let chain = Chain::new();
    let function = chain
        .given_function_of("paged-raiser", HANDLER_SOURCE, None)
        .await;

    for (index, row) in rows().into_iter().enumerate() {
        let Row {
            case,
            resource,
            order_id,
        } = row;
        let described = chain
            .ran(
                case,
                &format!("paged-failure-{index}"),
                &definition(resource, &function),
                &json!({ "orderId": order_id, "mode": "raise" }),
            )
            .await;
        let execution = string(&described, "executionArn");
        assert_eq!(
            (
                described.get("status").and_then(Value::as_str),
                described.get("error").and_then(Value::as_str),
            ),
            (Some("FAILED"), Some(RAISED_ERROR_TYPE)),
            "case {case:?}: expected the execution to fail under {RAISED_ERROR_TYPE:?}, got \
             {described}"
        );

        // The stored order, read whole: what every paged read of it has to reconstruct, and
        // the failure the run recorded.
        let message = raised_message(order_id);
        let stored = whole(&chain, case, &execution).await;
        assert_raised_identity(&stored, resource, &message, case, "the whole history");
        let (failure_event, _) = resource.failure();
        let failure_at = position_of(&stored, failure_event, case);

        for size in page_sizes(stored.len(), failure_at) {
            let reading = format!("{size} event(s) a page");
            let (walked, cursors) = paged(&chain, case, &execution, size).await;
            assert_eq!(
                walked, stored,
                "case {case:?}, read {reading:?}: expected the pages to concatenate to the \
                 stored history"
            );
            assert_raised_identity(&walked, resource, &message, case, &reading);
            assert_opaque(case, size, &cursors);

            // The position each cursor stands for is not itself a cursor: offered as one,
            // the decimal index the next page begins at is a token no read of this history
            // was handed, which this operation refuses rather than serving that page.
            for (offset, _) in &cursors {
                let (status, answered) = chain
                    .sfn_answered(
                        "GetExecutionHistory",
                        &json!({
                            "executionArn": &execution,
                            "maxResults": size,
                            "nextToken": offset.to_string(),
                        }),
                    )
                    .await;
                assert_eq!(
                    (status, error_code(&answered).as_str()),
                    INVALID_TOKEN,
                    "case {case:?}, read {reading:?}: expected the position {offset} to be \
                     refused as a token, got {status} with {answered}"
                );
            }
        }
    }
}
