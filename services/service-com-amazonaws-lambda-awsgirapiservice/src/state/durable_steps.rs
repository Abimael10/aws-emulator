// SPDX-License-Identifier: CC0-1.0

//! The CHECKPOINTS a durable execution's own run writes: one history event per step the run
//! started, succeeded at or failed an attempt of.
//!
//! # The rule this module implements
//!
//! A durable execution's history is "all the steps, callbacks, and events that occurred
//! during the execution" (`GetDurableExecutionHistory`), and the model carries one event type
//! per thing that happens to a STEP: `StepStarted`, `StepSucceeded` and `StepFailed`, the
//! last two carrying `RetryDetails$CurrentAttempt`, "the current attempt number for this
//! operation". The durable-execution SDK a managed runtime ships is what drives those steps
//! and checkpoints each one as it happens ("this API is used by the Lambda durable functions
//! SDK to checkpoint completed steps", `CheckpointDurableExecution`), so the events are
//! written WHILE the handler runs rather than invented afterwards by a read: a client polling
//! the history of a running execution sees the steps that have already happened, and one
//! reading a settled execution sees every attempt the run really made.
//!
//! # How a step reaches the history
//!
//! The run of a durable function is handed a journal ([`DurableStepJournal`]) addressing the
//! execution it IS — the function it belongs to and the execution's name — and the runtime's
//! own durable SDK reports each checkpoint through it
//! ([`super::execution::durable_sdk_guest`]). Every record is one thing the run DID, so this
//! module only appends what it is told: no step is reported that the run did not take, and an
//! execution that is no longer `RUNNING` — one a caller stopped, or one the clock timed out —
//! takes no further event, because its history is closed.

use super::*;

/// The member names of the document the runtime's durable SDK reports one checkpoint as.
///
/// They are the model's own members of an `OperationUpdate`: the operation's `Type`, the
/// `Action` taken on it, its `Id` and `Name`, the attempt it is of, the `Payload` a
/// successful one produced and the `Error` a failed one carried.
const TYPE_MEMBER: &str = "type";
/// See [`TYPE_MEMBER`].
const ACTION_MEMBER: &str = "action";
/// See [`TYPE_MEMBER`].
const ID_MEMBER: &str = "id";
/// See [`TYPE_MEMBER`].
const NAME_MEMBER: &str = "name";
/// See [`TYPE_MEMBER`].
const ATTEMPT_MEMBER: &str = "attempt";
/// See [`TYPE_MEMBER`].
const PAYLOAD_MEMBER: &str = "payload";
/// See [`TYPE_MEMBER`].
const ERROR_MEMBER: &str = "error";
/// See [`TYPE_MEMBER`].
const NEXT_ATTEMPT_DELAY_MEMBER: &str = "nextAttemptDelaySeconds";

/// The `OperationType` this journal records checkpoints of: the unit of work a durable
/// handler runs through `context.step(…)` (`smithy_query(query="shape",
/// shape="OperationType")`: `EXECUTION`, `CONTEXT`, `STEP`, `WAIT`, `CALLBACK`,
/// `CHAINED_INVOKE`).
const STEP_TYPE: &str = "STEP";

/// The `OperationAction` values a step checkpoint carries (`smithy_query(query="shape",
/// shape="OperationAction")`: `START`, `SUCCEED`, `FAIL`, `RETRY`, `CANCEL`).
const START_ACTION: &str = "START";
/// See [`START_ACTION`].
const SUCCEED_ACTION: &str = "SUCCEED";
/// See [`START_ACTION`].
const FAIL_ACTION: &str = "FAIL";

/// The model's `EventType` for a step that was scheduled (`smithy_query(query="shape",
/// shape="EventType")`).
pub const EVENT_TYPE_STEP_STARTED: &str = "StepStarted";

/// The model's `EventType` for an attempt of a step that RETURNED; see
/// [`EVENT_TYPE_STEP_STARTED`].
pub const EVENT_TYPE_STEP_SUCCEEDED: &str = "StepSucceeded";

/// The model's `EventType` for an attempt of a step that RAISED; see
/// [`EVENT_TYPE_STEP_STARTED`].
pub const EVENT_TYPE_STEP_FAILED: &str = "StepFailed";

/// The member names the runtime's error document carries inside a failed step's `error`.
const ERROR_TYPE_MEMBER: &str = "errorType";
/// See [`ERROR_TYPE_MEMBER`].
const ERROR_MESSAGE_MEMBER: &str = "errorMessage";
/// See [`ERROR_TYPE_MEMBER`].
const STACK_TRACE_MEMBER: &str = "stackTrace";

/// How many checkpoints ONE execution's history may be grown by.
///
/// The history is append-only and nothing removes an execution, so the number of events a
/// single run can write is bounded here rather than by how many steps a handler chooses to
/// take. Past it the run still runs — a step is the handler's own work, and nothing about it
/// depends on being recorded — and its further checkpoints are not recorded.
pub const MAX_STEP_EVENTS: usize = 1_000;

/// The most one STEP checkpoint may carry of what the step produced, in bytes.
///
/// AWS publishes it for this very operation: `OperationUpdate$Payload` is "the payload for
/// successful operations. The maximum payload size is 6 MB for synchronous `EXECUTION`
/// operations […] and 256 KB for `CONTEXT`, `STEP`, `WAIT`, and `CALLBACK` operations"
/// (`smithy_query(query="shape", shape="OperationUpdate")`). A step's result and the error of a
/// failed attempt are each held to it, so what one checkpoint adds to the store is bounded
/// whatever the handler returned, and `Truncated` — "indicates if the … payload was truncated
/// due to size limits" — reports a value this ceiling cut.
pub const MAX_STEP_PAYLOAD_BYTES: usize = 256 * 1024;

/// The most one checkpoint DOCUMENT may carry at the host seam the guest reports it through
/// ([`super::execution::python`]), in bytes: the payload ceiling above, with room for the
/// members named beside it (the operation's id, its name, and a failed attempt's error).
///
/// The seam is bounded for the reason every other guest-to-host seam of this crate is: a
/// handler decides what it checkpoints, so a document past this is refused rather than stored,
/// and one invocation cannot turn a step into an unbounded write.
pub const MAX_CHECKPOINT_BYTES: usize = 2 * MAX_STEP_PAYLOAD_BYTES;

/// The longest a step's NAME is kept, in bytes: the model's own `OperationName` length
/// (`smithy_query(query="shape", shape="Operation")`: `@length(min: 1, max: 256)`), which is
/// what a read reports it as.
const MAX_STEP_NAME_BYTES: usize = 256;

/// `text` cut to at most `ceiling` bytes, and whether it had to be cut.
///
/// The cut is taken on a character boundary, so what is kept is still text — the same reading
/// the run's own log is capped by ([`super::execution`]).
fn clipped(text: String, ceiling: usize) -> (String, bool) {
    if text.len() <= ceiling {
        return (text, false);
    }
    let mut end = ceiling;
    while end > 0 && !text.is_char_boundary(end) {
        end -= 1;
    }
    let mut kept = text;
    kept.truncate(end);
    (kept, true)
}

/// The journal one RUN of a durable function writes its step checkpoints into: the handle on
/// the execution that run IS.
///
/// Cloned onto the run's own thread, so it carries the store rather than borrowing it, and
/// addresses the execution by the identity the run was started under: the function's name and
/// the execution's name, which is what identifies an execution within its function
/// (`Invoke$DurableExecutionName`: "each durable execution name must be unique within the
/// function").
#[derive(Clone)]
pub struct DurableStepJournal {
    /// This service's store, where the execution's history lives.
    state: ::engine::ServiceState<ComAmazonawsLambdaAwsgirapiserviceState>,
    /// The function the execution belongs to.
    function: String,
    /// The execution's own name.
    execution: String,
}

impl ::std::fmt::Debug for DurableStepJournal {
    /// The store itself is not reported: what identifies a journal is the execution it writes
    /// to.
    fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        formatter
            .debug_struct("DurableStepJournal")
            .field("function", &self.function)
            .field("execution", &self.execution)
            .finish_non_exhaustive()
    }
}

impl DurableStepJournal {
    /// The journal for the execution `execution` of `function`.
    #[must_use]
    pub fn new(
        state: &::engine::ServiceState<ComAmazonawsLambdaAwsgirapiserviceState>,
        function: &str,
        execution: &str,
    ) -> Self {
        Self {
            state: ::engine::ServiceState::clone(state),
            function: function.to_string(),
            execution: execution.to_string(),
        }
    }

    /// Record the checkpoint `document` reports, as the history event it is.
    ///
    /// `document` is the JSON one `OperationUpdate` is reported as by the runtime's own
    /// durable SDK. A document this module cannot read as a step checkpoint records nothing:
    /// the history holds what the run did, so a record it cannot place is not turned into an
    /// event.
    ///
    /// What one checkpoint can ADD to the store is bounded here, because a handler decides
    /// what it checkpoints: the step's result and the error of a failed attempt are each held
    /// to [`MAX_STEP_PAYLOAD_BYTES`] and the step's name to the length the model gives one,
    /// and a value this ceiling cut is reported as `Truncated`.
    pub fn record(&self, document: &str) {
        if document.len() > MAX_CHECKPOINT_BYTES {
            return;
        }
        let Ok(update) = ::serde_json::from_str::<::serde_json::Value>(document) else {
            return;
        };
        let text = |name: &str| {
            update
                .get(name)
                .and_then(::serde_json::Value::as_str)
                .map(str::to_string)
        };
        if text(TYPE_MEMBER).as_deref() != Some(STEP_TYPE) {
            return;
        }
        let Some(action) = text(ACTION_MEMBER) else {
            return;
        };
        let attempt = update
            .get(ATTEMPT_MEMBER)
            .and_then(::serde_json::Value::as_i64)
            .and_then(|attempt| i32::try_from(attempt).ok())
            .unwrap_or_default()
            .max(0);
        let delay = update
            .get(NEXT_ATTEMPT_DELAY_MEMBER)
            .and_then(::serde_json::Value::as_i64)
            .and_then(|seconds| i32::try_from(seconds).ok())
            .filter(|seconds| *seconds >= 0);
        let now = execution_instant(SystemTime::now());
        let mut store = self.state.lock();
        let Some(execution) = store
            .functions
            .get_mut(&self.function)
            .and_then(|function| function.durable_executions.get_mut(&self.execution))
        else {
            return;
        };
        // An execution whose history is already closed takes no further event: whatever the
        // clock owed is applied first, so a run still reporting steps past the
        // `ExecutionTimeout` that fell due does not append to an execution that had already
        // timed out.
        execution.materialize(now);
        if execution.status != EXECUTION_STATUS_RUNNING {
            return;
        }
        if execution.history.len() >= MAX_STEP_EVENTS {
            return;
        }
        let retry_details = RetryDetails {
            current_attempt: attempt,
            next_attempt_delay_seconds: delay,
        };
        let event_type = match action.as_str() {
            START_ACTION => EVENT_TYPE_STEP_STARTED,
            SUCCEED_ACTION => EVENT_TYPE_STEP_SUCCEEDED,
            FAIL_ACTION => EVENT_TYPE_STEP_FAILED,
            // An action this module records no event for: nothing happened to the history.
            _ => return,
        };
        let mut event = execution.next_event(event_type, now);
        event.id = text(ID_MEMBER);
        // The step's own name, kept at the length the model gives one.
        event.name = text(NAME_MEMBER).map(|name| clipped(name, MAX_STEP_NAME_BYTES).0);
        match action.as_str() {
            START_ACTION => event.step_started_details = Some(StepStartedDetails {}),
            SUCCEED_ACTION => {
                // "The JSON response payload from the step operation": what the step's own
                // work returned, held to what one checkpoint may carry of it and reported as
                // `Truncated` when that ceiling cut it.
                let (payload, truncated) = match text(PAYLOAD_MEMBER) {
                    Some(payload) => {
                        let (payload, truncated) = clipped(payload, MAX_STEP_PAYLOAD_BYTES);
                        (Some(payload), truncated)
                    }
                    None => (None, false),
                };
                event.step_succeeded_details = Some(StepSucceededDetails {
                    result: EventResult {
                        payload,
                        truncated: Some(truncated),
                    },
                    retry_details,
                });
            }
            _ => {
                let (error, truncated) = step_error(update.get(ERROR_MEMBER));
                event.step_failed_details = Some(StepFailedDetails {
                    error: EventError {
                        payload: error,
                        truncated: Some(truncated),
                    },
                    retry_details,
                });
            }
        }
        execution.history.push(event);
    }
}

/// The `ErrorObject` a FAILED step's checkpoint carried, read out of the error document the
/// runtime reports an exception as: its type, its message and the handler's own stack — and
/// whether what one checkpoint may carry ([`MAX_STEP_PAYLOAD_BYTES`]) cut any of them, which
/// is what the event reports as `Truncated`.
fn step_error(error: Option<&::serde_json::Value>) -> (Option<ErrorObject>, bool) {
    let Some(error) = error else {
        return (None, false);
    };
    let mut truncated = false;
    let mut member = |name: &str| {
        let held = error
            .get(name)
            .and_then(::serde_json::Value::as_str)
            .map(str::to_string)?;
        let (kept, cut) = clipped(held, MAX_STEP_PAYLOAD_BYTES);
        truncated |= cut;
        Some(kept)
    };
    let error_message = member(ERROR_MESSAGE_MEMBER);
    let error_type = member(ERROR_TYPE_MEMBER);
    let stack_trace = error
        .get(STACK_TRACE_MEMBER)
        .and_then(::serde_json::Value::as_array)
        .map(|frames| {
            frames
                .iter()
                .filter_map(::serde_json::Value::as_str)
                .map(|frame| {
                    let (kept, cut) = clipped(frame.to_string(), MAX_STEP_PAYLOAD_BYTES);
                    truncated |= cut;
                    kept
                })
                .collect()
        });
    (
        Some(ErrorObject {
            error_message,
            error_type,
            error_data: None,
            stack_trace,
        }),
        truncated,
    )
}
