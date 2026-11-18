// SPDX-License-Identifier: CC0-1.0

//! The CHECKPOINTS a durable execution's own run writes: one history event per OPERATION of
//! the execution that started, succeeded, failed an attempt of or was cancelled.
//!
//! # The rule this module implements
//!
//! A durable execution's history is "all the steps, callbacks, and events that occurred
//! during the execution" (`GetDurableExecutionHistory`), and the model carries one event type
//! per thing that happens to one OPERATION of it: an operation is of an `OperationType`
//! (`EXECUTION`, `CONTEXT`, `STEP`, `WAIT`, `CALLBACK`, `CHAINED_INVOKE`), an
//! `OperationAction` is taken on it (`START`, `SUCCEED`, `FAIL`, `RETRY`, `CANCEL`), and the
//! `EventType` the pair names is the event the history takes — `StepStarted`,
//! `StepSucceeded` and `StepFailed` for a step, `WaitStarted`, `WaitSucceeded` and
//! `WaitCancelled` for a wait, `ContextStarted`, `ContextSucceeded` and `ContextFailed` for a
//! context — each carrying the one details member of its own type ([`append_checkpoint`]). So
//! EVERY operation a durable handler runs is recorded, not only the units of work it runs as
//! steps: a client that reconstructs an execution's progress reads the wait it took where it
//! took it.
//!
//! The durable-execution SDK a managed runtime ships is what drives those operations and
//! checkpoints each one as it happens ("this API is used by the Lambda durable functions SDK
//! to checkpoint completed steps", `CheckpointDurableExecution`), so the events are written
//! WHILE the handler runs rather than invented afterwards by a read: a client polling the
//! history of a running execution sees what has already happened, and one reading a settled
//! execution sees every attempt the run really made.
//!
//! # How an operation reaches the history
//!
//! The run of a durable function is handed a journal ([`DurableStepJournal`]) addressing the
//! execution it IS — the function it belongs to and the execution's name — and the runtime's
//! own durable SDK reports each checkpoint through it
//! ([`super::execution::durable_sdk_guest`]). Every record is one thing the run DID, so this
//! module only appends what it is told: no operation is reported that the run did not take,
//! and an execution that is no longer `RUNNING` — one a caller stopped, or one the clock timed
//! out — takes no further event, because its history is closed.
//!
//! The SAME appender serves the modelled `CheckpointDurableExecution` call
//! ([`super::durable_checkpoint`]), so a checkpoint reaching this execution's history through
//! the API becomes the same event, by the same rule, as one the run reports in process.

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
/// See [`TYPE_MEMBER`]: the span a WAIT was asked for, which is the model's own
/// `WaitOptions$WaitSeconds` ("the number of seconds to wait").
const WAIT_SECONDS_MEMBER: &str = "waitSeconds";

/// The `OperationType` of the unit of work a durable handler runs through `context.step(…)`
/// (`smithy_query(query="shape", shape="OperationType")`: `EXECUTION`, `CONTEXT`, `STEP`,
/// `WAIT`, `CALLBACK`, `CHAINED_INVOKE`).
pub const STEP_TYPE: &str = "STEP";

/// The `OperationType` of the span a durable handler waits through `context.wait(…)`; see
/// [`STEP_TYPE`].
pub const WAIT_TYPE: &str = "WAIT";

/// The `OperationType` of a child CONTEXT a durable handler runs its operations inside; see
/// [`STEP_TYPE`].
pub const CONTEXT_TYPE: &str = "CONTEXT";

/// The `OperationType` of the execution ITSELF, which is the operation whose outcome settles
/// the execution rather than one its history takes a step-shaped event for; see
/// [`STEP_TYPE`].
pub const EXECUTION_TYPE: &str = "EXECUTION";

/// The `OperationAction` values a checkpoint carries (`smithy_query(query="shape",
/// shape="OperationAction")`: `START`, `SUCCEED`, `FAIL`, `RETRY`, `CANCEL`).
pub const START_ACTION: &str = "START";
/// See [`START_ACTION`].
pub const SUCCEED_ACTION: &str = "SUCCEED";
/// See [`START_ACTION`].
pub const FAIL_ACTION: &str = "FAIL";
/// See [`START_ACTION`].
pub const CANCEL_ACTION: &str = "CANCEL";

/// The model's `EventType` for a step that was scheduled (`smithy_query(query="shape",
/// shape="EventType")`).
pub const EVENT_TYPE_STEP_STARTED: &str = "StepStarted";

/// The model's `EventType` for an attempt of a step that RETURNED; see
/// [`EVENT_TYPE_STEP_STARTED`].
pub const EVENT_TYPE_STEP_SUCCEEDED: &str = "StepSucceeded";

/// The model's `EventType` for an attempt of a step that RAISED; see
/// [`EVENT_TYPE_STEP_STARTED`].
pub const EVENT_TYPE_STEP_FAILED: &str = "StepFailed";

/// The model's `EventType` for a WAIT that began; see [`EVENT_TYPE_STEP_STARTED`].
pub const EVENT_TYPE_WAIT_STARTED: &str = "WaitStarted";

/// The model's `EventType` for a WAIT whose span elapsed; see [`EVENT_TYPE_STEP_STARTED`].
pub const EVENT_TYPE_WAIT_SUCCEEDED: &str = "WaitSucceeded";

/// The model's `EventType` for a WAIT that was cut off before its span elapsed; see
/// [`EVENT_TYPE_STEP_STARTED`].
pub const EVENT_TYPE_WAIT_CANCELLED: &str = "WaitCancelled";

/// The model's `EventType` for a child CONTEXT that began; see [`EVENT_TYPE_STEP_STARTED`].
pub const EVENT_TYPE_CONTEXT_STARTED: &str = "ContextStarted";

/// The model's `EventType` for a child CONTEXT that returned; see
/// [`EVENT_TYPE_STEP_STARTED`].
pub const EVENT_TYPE_CONTEXT_SUCCEEDED: &str = "ContextSucceeded";

/// The model's `EventType` for a child CONTEXT that raised; see [`EVENT_TYPE_STEP_STARTED`].
pub const EVENT_TYPE_CONTEXT_FAILED: &str = "ContextFailed";

/// The member names the runtime's error document carries inside a failed operation's `error`.
const ERROR_TYPE_MEMBER: &str = "errorType";
/// See [`ERROR_TYPE_MEMBER`].
const ERROR_MESSAGE_MEMBER: &str = "errorMessage";
/// See [`ERROR_TYPE_MEMBER`].
const STACK_TRACE_MEMBER: &str = "stackTrace";

/// How many checkpoints ONE execution's history may be grown by.
///
/// The history is append-only and nothing removes an execution, so the number of events a
/// single run can write is bounded here rather than by how many operations a handler chooses
/// to take. Past it the run still runs — an operation is the handler's own work, and nothing
/// about it depends on being recorded — and its further checkpoints are not recorded.
pub const MAX_STEP_EVENTS: usize = 1_000;

/// How many updates ONE checkpoint may carry.
///
/// `OperationUpdates` carries no `@length` in the model, so the number of updates a
/// `CheckpointDurableExecution` request carries is the CALLER's; this is the emulator's own
/// ceiling on it, set at the number of events one execution's history may be grown by
/// ([`MAX_STEP_EVENTS`]) — past that a checkpoint carries updates no history could take. It is
/// what keeps the work of applying one checkpoint bounded, the way every other
/// caller-driven batch of this crate is bounded, so a request past it is refused rather than
/// applied.
pub const MAX_CHECKPOINT_UPDATES: usize = MAX_STEP_EVENTS;

/// The most one checkpoint may carry of what the operation produced, in bytes.
///
/// AWS publishes it for this very operation: `OperationUpdate$Payload` is "the payload for
/// successful operations. The maximum payload size is 6 MB for synchronous `EXECUTION`
/// operations […] and 256 KB for `CONTEXT`, `STEP`, `WAIT`, and `CALLBACK` operations"
/// (`smithy_query(query="shape", shape="OperationUpdate")`). An operation's result and the
/// error of a failed attempt are each held to it, so what one checkpoint adds to the store is
/// bounded whatever the handler returned, and `Truncated` — "indicates if the … payload was
/// truncated due to size limits" — reports a value this ceiling cut.
pub const MAX_STEP_PAYLOAD_BYTES: usize = 256 * 1024;

/// The most one checkpoint DOCUMENT may carry at the host seam the guest reports it through
/// ([`super::execution::python`]), in bytes: the payload ceiling above, with room for the
/// members named beside it (the operation's id, its name, and a failed attempt's error).
///
/// The seam is bounded for the reason every other guest-to-host seam of this crate is: a
/// handler decides what it checkpoints, so a document past this is refused rather than stored,
/// and one invocation cannot turn a step into an unbounded write.
pub const MAX_CHECKPOINT_BYTES: usize = 2 * MAX_STEP_PAYLOAD_BYTES;

/// The longest an operation's NAME is kept, in bytes: the model's own `OperationName` length
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

/// ONE checkpoint of ONE operation of a durable execution: the operation it is of, what
/// happened to it, and what that carried.
///
/// The members are the model's own `OperationUpdate` ones, so a checkpoint the run reports
/// in process and a checkpoint the modelled `CheckpointDurableExecution` call carries are
/// the same thing and become the same event ([`append_checkpoint`]).
#[derive(Debug, Clone, Default)]
pub struct Checkpoint {
    /// The `OperationType` the operation is of ([`STEP_TYPE`], [`WAIT_TYPE`], …).
    pub operation: String,
    /// The `OperationAction` taken on it ([`START_ACTION`], …).
    pub action: String,
    /// The operation's own id, which `Event$Id` reports.
    pub id: Option<String>,
    /// The operation's customer-provided name, which `Event$Name` reports.
    pub name: Option<String>,
    /// Which attempt of the operation this checkpoint is of, which
    /// `RetryDetails$CurrentAttempt` reports.
    pub attempt: i32,
    /// How long before the attempt that follows a failed one, which
    /// `RetryDetails$NextAttemptDelaySeconds` reports.
    pub next_attempt_delay_seconds: Option<i32>,
    /// What a successful operation produced ("the payload for successful operations").
    pub payload: Option<String>,
    /// The error a failed or cancelled operation carried.
    pub error: Option<ErrorObject>,
    /// The span a WAIT was asked for, in seconds, which `WaitOptions$WaitSeconds` names and
    /// `WaitStartedDetails$Duration` reports.
    pub wait_seconds: Option<i32>,
}

impl Checkpoint {
    /// The model's `EventType` the operation and the action taken on it name, and `None` for
    /// a pair the model declares no event for — a retried attempt, whose next attempt is the
    /// event the history takes, or an operation whose outcome is not one of this execution's
    /// history events at all.
    #[must_use]
    pub fn event_type(&self) -> Option<&'static str> {
        match (self.operation.as_str(), self.action.as_str()) {
            (STEP_TYPE, START_ACTION) => Some(EVENT_TYPE_STEP_STARTED),
            (STEP_TYPE, SUCCEED_ACTION) => Some(EVENT_TYPE_STEP_SUCCEEDED),
            (STEP_TYPE, FAIL_ACTION) => Some(EVENT_TYPE_STEP_FAILED),
            (WAIT_TYPE, START_ACTION) => Some(EVENT_TYPE_WAIT_STARTED),
            (WAIT_TYPE, SUCCEED_ACTION) => Some(EVENT_TYPE_WAIT_SUCCEEDED),
            (WAIT_TYPE, CANCEL_ACTION) => Some(EVENT_TYPE_WAIT_CANCELLED),
            (CONTEXT_TYPE, START_ACTION) => Some(EVENT_TYPE_CONTEXT_STARTED),
            (CONTEXT_TYPE, SUCCEED_ACTION) => Some(EVENT_TYPE_CONTEXT_SUCCEEDED),
            (CONTEXT_TYPE, FAIL_ACTION) => Some(EVENT_TYPE_CONTEXT_FAILED),
            _ => None,
        }
    }

    /// The checkpoint `document` reports, read as one: the JSON one `OperationUpdate` is
    /// reported as by the runtime's own durable SDK, and `None` for a document that is no
    /// checkpoint of an operation — the history holds what the run did, so a record this
    /// cannot place becomes no event.
    #[must_use]
    pub fn from_document(document: &str) -> Option<Self> {
        let update = ::serde_json::from_str::<::serde_json::Value>(document).ok()?;
        let text = |name: &str| {
            update
                .get(name)
                .and_then(::serde_json::Value::as_str)
                .map(str::to_string)
        };
        let whole = |name: &str| {
            update
                .get(name)
                .and_then(::serde_json::Value::as_i64)
                .and_then(|value| i32::try_from(value).ok())
        };
        Some(Self {
            operation: text(TYPE_MEMBER)?,
            action: text(ACTION_MEMBER)?,
            id: text(ID_MEMBER),
            name: text(NAME_MEMBER),
            attempt: whole(ATTEMPT_MEMBER).unwrap_or_default().max(0),
            next_attempt_delay_seconds: whole(NEXT_ATTEMPT_DELAY_MEMBER)
                .filter(|seconds| *seconds >= 0),
            payload: text(PAYLOAD_MEMBER),
            error: checkpoint_error(update.get(ERROR_MEMBER)),
            wait_seconds: whole(WAIT_SECONDS_MEMBER).filter(|seconds| *seconds >= 0),
        })
    }
}

/// Append the history event `checkpoint` is to `execution`, at the instant `now` it
/// happened, and report whether one was appended.
///
/// The event is the one the operation's type and the action taken on it name
/// ([`Checkpoint::event_type`]), carrying the details member of that type and nothing else:
/// the operation's own id and name, the attempt it is of, the payload a successful one
/// produced, the error a failed or cancelled one carried, and the span a wait was asked for.
///
/// NOTHING is appended when the execution's history is already closed — an execution a caller
/// stopped or the clock timed out takes no further event — when it has already taken as many
/// checkpoints as one may grow it by ([`MAX_STEP_EVENTS`]), or when the pair names no event.
///
/// What one checkpoint can ADD to the store is bounded here, because a handler decides what
/// it checkpoints: the payload and the error are each held to [`MAX_STEP_PAYLOAD_BYTES`] and
/// the operation's name to the length the model gives one, and a value a ceiling cut is
/// reported as `Truncated`.
pub fn append_checkpoint(
    execution: &mut DurableExecutionRecord,
    checkpoint: &Checkpoint,
    now: SystemTime,
) -> bool {
    if execution.status != EXECUTION_STATUS_RUNNING {
        return false;
    }
    if execution.history.len() >= MAX_STEP_EVENTS {
        return false;
    }
    let Some(event_type) = checkpoint.event_type() else {
        return false;
    };
    let now = execution_instant(now);
    let retry_details = RetryDetails {
        current_attempt: checkpoint.attempt,
        next_attempt_delay_seconds: checkpoint.next_attempt_delay_seconds,
    };
    let mut event = execution.next_event(event_type, now);
    event.id = checkpoint.id.clone();
    // The operation's own name, kept at the length the model gives one.
    event.name = checkpoint
        .name
        .clone()
        .map(|name| clipped(name, MAX_STEP_NAME_BYTES).0);
    // "The JSON response payload from the step operation": what the operation's own work
    // produced, held to what one checkpoint may carry of it and reported as `Truncated` when
    // that ceiling cut it.
    let result = || {
        let (payload, truncated) = match checkpoint.payload.clone() {
            Some(payload) => {
                let (payload, truncated) = clipped(payload, MAX_STEP_PAYLOAD_BYTES);
                (Some(payload), truncated)
            }
            None => (None, false),
        };
        EventResult {
            payload,
            truncated: Some(truncated),
        }
    };
    let failure = || {
        let (error, truncated) = clipped_error(checkpoint.error.clone());
        EventError {
            payload: error,
            truncated: Some(truncated),
        }
    };
    match event_type {
        EVENT_TYPE_STEP_STARTED => event.step_started_details = Some(StepStartedDetails {}),
        EVENT_TYPE_STEP_SUCCEEDED => {
            event.step_succeeded_details = Some(StepSucceededDetails {
                result: result(),
                retry_details,
            });
        }
        EVENT_TYPE_STEP_FAILED => {
            event.step_failed_details = Some(StepFailedDetails {
                error: failure(),
                retry_details,
            });
        }
        EVENT_TYPE_WAIT_STARTED => {
            // `WaitStartedDetails` carries "the duration to wait, in seconds" and "the date
            // and time when the wait operation is scheduled to complete", which is that span
            // after the instant the wait began. Both are REQUIRED, and the span a checkpoint
            // carries is already the whole number of seconds `WaitOptions$WaitSeconds` names
            // one in, held at or above the `DurationSeconds` floor of 0.
            let seconds = checkpoint.wait_seconds.unwrap_or_default().max(0);
            let scheduled_end = u64::try_from(seconds)
                .ok()
                .and_then(|seconds| now.checked_add(Duration::from_secs(seconds)))
                .unwrap_or(now);
            event.wait_started_details = Some(WaitStartedDetails {
                duration: seconds,
                scheduled_end_timestamp: ::aws_smithy_types::DateTime::from(scheduled_end),
            });
        }
        EVENT_TYPE_WAIT_SUCCEEDED => {
            // "The wait duration, in seconds" — the span the wait that just elapsed was
            // asked for, and none reported for a checkpoint that named none.
            event.wait_succeeded_details = Some(WaitSucceededDetails {
                duration: checkpoint.wait_seconds,
            });
        }
        EVENT_TYPE_WAIT_CANCELLED => {
            // `WaitCancelledDetails$Error` is "details about why the wait operation was
            // cancelled" and is optional, so a cancellation that carried no error reports
            // none rather than an empty envelope.
            event.wait_cancelled_details = Some(WaitCancelledDetails {
                error: checkpoint.error.is_some().then(failure),
            });
        }
        EVENT_TYPE_CONTEXT_STARTED => {
            event.context_started_details = Some(ContextStartedDetails {});
        }
        EVENT_TYPE_CONTEXT_SUCCEEDED => {
            event.context_succeeded_details = Some(ContextSucceededDetails { result: result() });
        }
        EVENT_TYPE_CONTEXT_FAILED => {
            event.context_failed_details = Some(ContextFailedDetails { error: failure() });
        }
        // Every event [`Checkpoint::event_type`] names is built above; a type it does not
        // name reached none of these arms.
        _ => return false,
    }
    execution.history.push(event);
    true
}

/// The journal one RUN of a durable function writes its checkpoints into: the handle on the
/// execution that run IS.
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
    /// durable SDK. A document this module cannot read as a checkpoint records nothing: the
    /// history holds what the run did, so a record it cannot place is not turned into an
    /// event.
    pub fn record(&self, document: &str) {
        if document.len() > MAX_CHECKPOINT_BYTES {
            return;
        }
        let Some(checkpoint) = Checkpoint::from_document(document) else {
            return;
        };
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
        // clock owed is applied first, so a run still reporting checkpoints past the
        // `ExecutionTimeout` that fell due does not append to an execution that had already
        // timed out.
        execution.materialize(now);
        append_checkpoint(execution, &checkpoint, now);
    }
}

/// The `ErrorObject` a FAILED or CANCELLED operation's checkpoint carried, read out of the
/// error document the runtime reports an exception as: its type, its message and the
/// handler's own stack.
fn checkpoint_error(error: Option<&::serde_json::Value>) -> Option<ErrorObject> {
    let error = error?;
    let member = |name: &str| {
        error
            .get(name)
            .and_then(::serde_json::Value::as_str)
            .map(str::to_string)
    };
    Some(ErrorObject {
        error_message: member(ERROR_MESSAGE_MEMBER),
        error_type: member(ERROR_TYPE_MEMBER),
        error_data: None,
        stack_trace: error
            .get(STACK_TRACE_MEMBER)
            .and_then(::serde_json::Value::as_array)
            .map(|frames| {
                frames
                    .iter()
                    .filter_map(::serde_json::Value::as_str)
                    .map(str::to_string)
                    .collect()
            }),
    })
}

/// `error` with every text it carries held to what one checkpoint may carry
/// ([`MAX_STEP_PAYLOAD_BYTES`]), and whether that ceiling cut any of them — which is what
/// the event reports as `Truncated`.
fn clipped_error(error: Option<ErrorObject>) -> (Option<ErrorObject>, bool) {
    let Some(error) = error else {
        return (None, false);
    };
    let mut truncated = false;
    let mut hold = |held: Option<String>| {
        let (kept, cut) = clipped(held?, MAX_STEP_PAYLOAD_BYTES);
        truncated |= cut;
        Some(kept)
    };
    let error_message = hold(error.error_message);
    let error_type = hold(error.error_type);
    let error_data = hold(error.error_data);
    let stack_trace = error.stack_trace.map(|frames| {
        frames
            .into_iter()
            .map(|frame| {
                let (kept, cut) = clipped(frame, MAX_STEP_PAYLOAD_BYTES);
                truncated |= cut;
                kept
            })
            .collect()
    });
    (
        Some(ErrorObject {
            error_message,
            error_type,
            error_data,
            stack_trace,
        }),
        truncated,
    )
}
