// SPDX-License-Identifier: CC0-1.0

//! The Lambda TASK a `ScheduleLambdaFunction` decision creates: the invocation SWF makes
//! for it, and the events its outcome records.
//!
//! A Lambda task is the one piece of work a workflow execution performs WITHOUT a worker
//! polling for it: the decision records `LambdaFunctionScheduled`, and the service itself
//! then invokes the named function under the IAM role attached to the execution
//! (`lambdaRole`, "in order for this workflow execution to invoke AWS Lambda functions, an
//! appropriate IAM role must be specified"). So a scheduled task always reaches a TERMINAL
//! event of its own: `LambdaFunctionStarted` once the invocation is under way, and then
//! `LambdaFunctionCompleted` carrying what the function returned, `LambdaFunctionFailed`
//! carrying what it (or Lambda) reported instead, or `LambdaFunctionTimedOut` when the
//! invocation outlasts the task's own `startToCloseTimeout`. Each of those is an event a
//! decider has to act on, so a decision task is scheduled with it — which is what lets the
//! run carry on past the Lambda task. A run with no role to assume never gets that far:
//! that is `StartLambdaFunctionFailed`, recorded by the decision itself.
//!
//! The invocation is made through the in-process callback over AWS Lambda's own modelled
//! `Invoke`, so the function that runs is the one `CreateFunction` created and the result
//! is the function's own: nothing here synthesises a result, a failure or a duration.
//! Nothing about the RUN is this service's either — what the function may reach, how long
//! Lambda lets it run and what it may allocate are Lambda's own bounds on every invocation
//! it serves. The one bound chosen here is how long the TASK waits, which the decision
//! itself states ([`start_to_close_budget`]).
//!
//! It runs off the response path, in a task of its own, because
//! `RespondDecisionTaskCompleted` answers as soon as the decisions are recorded — the
//! decider goes back to polling while the Lambda task runs, which is exactly the window a
//! client reads the history in.

use std::sync::Arc;
use std::time::Duration;

use ::aws_smithy_types::DateTime;
use ::smithy_server_runtime::EmbeddedEmulator;

use super::{
    now, service_state, ComAmazonawsSwfSimpleworkflowserviceState, EventAttributes, RunClose,
};
use crate::ops::types::{
    LambdaFunctionCompletedEventAttributes, LambdaFunctionFailedEventAttributes,
    LambdaFunctionStartedEventAttributes, LambdaFunctionTimedOutEventAttributes,
};

/// The `timeoutType` a `LambdaFunctionTimedOut` event carries — the one member of the
/// modelled `LambdaFunctionTimeoutType`
/// ([`crate::ops::types::LAMBDA_FUNCTION_TIMEOUT_TYPE_VALUES`]), since the only clock a
/// Lambda task runs against is its own start-to-close one.
const TIMEOUT_START_TO_CLOSE: &str = "START_TO_CLOSE";

/// The start-to-close budget of a task whose decision named none: "If no value is supplied,
/// then a default value of 900s is assumed"
/// (`ScheduleLambdaFunctionDecisionAttributes$startToCloseTimeout`), which is also the
/// longest one a task can be given: the value is "any integer from 1-900 (1s-15m)", so this
/// is the ceiling a stated one is read against as well.
const MAX_START_TO_CLOSE: Duration = Duration::from_secs(900);

/// AWS Lambda's ARN namespace, for reading the Region out of a task whose `name` is the
/// function's ARN ("the name, or ARN, of the Lambda function to schedule").
const LAMBDA_ARN_NAMESPACE: &str = "lambda";

/// The member of a function's error payload naming the error class it raised, which Lambda
/// documents for every invocation whose `FunctionError` is set.
const ERROR_TYPE: &str = "errorType";

/// One Lambda task this service has to invoke: the task's identity in the run's history,
/// and what the decision that scheduled it said to invoke.
///
/// Built by the `ScheduleLambdaFunction` decision and handed to [`dispatch_lambda_tasks`].
/// The values are the DECISION's, read under the same lock that recorded the scheduling
/// event, so an invocation can never be made for a task the history does not hold.
#[derive(Debug, Clone)]
pub struct LambdaTask {
    /// The domain of the run this task belongs to.
    pub domain: String,
    pub workflow_id: String,
    pub run_id: String,
    /// The `eventId` of the `LambdaFunctionScheduled` event that created the task — its
    /// identity, which every later event of the task names.
    pub scheduled_event_id: i64,
    /// "The name, or ARN, of the Lambda function to schedule."
    pub name: String,
    /// "The optional input data to be supplied to the Lambda function", which is the event
    /// the function is invoked with.
    pub input: Option<String>,
    /// The task's own `startToCloseTimeout`, as the decision stated it.
    pub start_to_close_timeout: Option<String>,
    /// The Region the invocation is addressed to when the function is named by its bare
    /// name: the one the decider's own call was signed for.
    pub region: String,
}

/// What came of one Lambda task's invocation — the three terminal events a Lambda task can
/// reach once it has started.
#[derive(Debug, Clone)]
enum Outcome {
    /// The function returned: `result` is "the results of the Lambda task", which is exactly
    /// what the invocation answered with.
    Completed { result: Option<String> },
    /// The function raised, or Lambda refused to run it: `reason` and `details` are "the
    /// reason provided for the failure" and "the details of the failure", taken from what
    /// the invocation reported.
    Failed {
        reason: Option<String>,
        details: Option<String>,
    },
    /// The invocation outlasted the task's start-to-close budget.
    TimedOut,
}

impl Outcome {
    /// The event this outcome records for the task `(scheduled_event_id, started_event_id)`
    /// names.
    fn event(self, scheduled_event_id: i64, started_event_id: i64) -> EventAttributes {
        match self {
            Self::Completed { result } => {
                EventAttributes::LambdaFunctionCompleted(LambdaFunctionCompletedEventAttributes {
                    scheduled_event_id,
                    started_event_id,
                    result,
                })
            }
            Self::Failed { reason, details } => {
                EventAttributes::LambdaFunctionFailed(LambdaFunctionFailedEventAttributes {
                    scheduled_event_id,
                    started_event_id,
                    reason,
                    details,
                })
            }
            Self::TimedOut => {
                EventAttributes::LambdaFunctionTimedOut(LambdaFunctionTimedOutEventAttributes {
                    scheduled_event_id,
                    started_event_id,
                    timeout_type: Some(TIMEOUT_START_TO_CLOSE.to_string()),
                })
            }
        }
    }
}

/// Invoke every Lambda task `tasks` names and record what came of each.
///
/// Called by `RespondDecisionTaskCompleted` once its decisions are recorded and the store
/// lock is released. Each task runs in a task of its own, so one function's run holds up
/// neither the answer nor another Lambda task of the same completion.
pub fn dispatch_lambda_tasks<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
    callback: &EmbeddedEmulator,
    tasks: Vec<LambdaTask>,
) {
    for task in tasks {
        let engine = Arc::clone(engine);
        let callback = callback.clone();
        ::tokio::spawn(async move {
            run(&engine, &callback, &task).await;
        });
    }
}

/// Record one task's start, invoke its function, and record the outcome.
async fn run<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
    callback: &EmbeddedEmulator,
    task: &LambdaTask,
) {
    let Some(state) = service_state(&**engine) else {
        return;
    };
    // The store lock is taken, written under and given back around the invocation: the call
    // to Lambda is served by this same process, so holding the store across it would stop
    // the very service the function may call back into.
    let started_event_id = {
        let mut guard = state.lock();
        record_started(&mut guard, task, now())
    };
    let Some(started_event_id) = started_event_id else {
        return;
    };
    let outcome = invoke(callback, task).await;
    let mut guard = state.lock();
    record_outcome(&mut guard, task, started_event_id, outcome, now());
}

/// Record the `LambdaFunctionStarted` event of `task` and answer its `eventId`.
///
/// `None` when there is nothing to start after all: the run has closed, or the task is no
/// longer in flight, since the decision that scheduled it and this invocation are two
/// instants and the clock can close a run between them.
fn record_started(
    state: &mut ComAmazonawsSwfSimpleworkflowserviceState,
    task: &LambdaTask,
    at: DateTime,
) -> Option<i64> {
    state.materialise_due_timeouts_in(&task.domain, at);
    let domain = state.domains.get_mut(&task.domain)?;
    let key = (task.workflow_id.clone(), task.run_id.clone());
    let execution = domain.executions.get_mut(&key)?;
    if !execution.is_open() || !execution.lambda_function_is_in_flight(task.scheduled_event_id) {
        return None;
    }
    // This start, the terminal event that follows it and the decision task that carries that
    // event to a decider are three events; a run with no room for them has reached the
    // history quota, and is terminated by the service rather than grown past it.
    if !execution.has_history_room_for(3) {
        domain.close_run(&key, RunClose::event_limit_exceeded(), at);
        return None;
    }
    Some(execution.append_event(
        at,
        EventAttributes::LambdaFunctionStarted(LambdaFunctionStartedEventAttributes {
            scheduled_event_id: task.scheduled_event_id,
        }),
    ))
}

/// Record the terminal event of `task` and the decision task that carries it to a decider.
fn record_outcome(
    state: &mut ComAmazonawsSwfSimpleworkflowserviceState,
    task: &LambdaTask,
    started_event_id: i64,
    outcome: Outcome,
    at: DateTime,
) {
    state.materialise_due_timeouts_in(&task.domain, at);
    let Some(domain) = state.domains.get_mut(&task.domain) else {
        return;
    };
    let key = (task.workflow_id.clone(), task.run_id.clone());
    let Some(execution) = domain.executions.get_mut(&key) else {
        return;
    };
    if !execution.is_open() || !execution.lambda_function_is_in_flight(task.scheduled_event_id) {
        return;
    }
    if !execution.has_history_room_for(2) {
        domain.close_run(&key, RunClose::event_limit_exceeded(), at);
        return;
    }
    execution.append_event(at, outcome.event(task.scheduled_event_id, started_event_id));
    // `openLambdaFunctions` is "the count of Lambda tasks whose status is OPEN", and this
    // event is what took this one out of flight.
    execution.open_lambda_functions = execution.open_lambda_functions.saturating_sub(1);
    // The decider has to act on the outcome, and the decision task scheduled here is what
    // carries it to one.
    execution.schedule_decision_task_if_idle(at);
}

/// Invoke `task`'s function and report what the invocation answered.
///
/// The function is invoked the way a client's own `Invoke` of it would be — one
/// `RequestResponse` invocation whose payload is the task's `input` — so the result, the
/// error and the time it takes are the function's own.
async fn invoke(callback: &EmbeddedEmulator, task: &LambdaTask) -> Outcome {
    let mut request = client(callback, invocation_region(task))
        .invoke()
        .function_name(&task.name)
        .invocation_type(::aws_sdk_lambda::types::InvocationType::RequestResponse);
    if let Some(input) = &task.input {
        request = request.payload(::aws_sdk_lambda::primitives::Blob::new(
            input.clone().into_bytes(),
        ));
    }
    match ::tokio::time::timeout(start_to_close_budget(task), request.send()).await {
        Ok(Ok(answered)) => {
            let payload = answered
                .payload
                .as_ref()
                .map(|blob| String::from_utf8_lossy(blob.as_ref()).into_owned());
            // "FunctionError: If present, indicates that an error occurred during function
            // execution" — which is this Lambda task failing, with the error the function
            // itself produced as the failure's details.
            match answered.function_error() {
                Some(function_error) => Outcome::Failed {
                    reason: Some(raised_error(function_error, payload.as_deref())),
                    details: payload,
                },
                None => Outcome::Completed { result: payload },
            }
        }
        // Lambda would not run the function at all — there is no such function, or it
        // refused the invocation. The task has no result either way, and the failure reports
        // what Lambda answered rather than anything decided here.
        Ok(Err(error)) => {
            use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

            Outcome::Failed {
                reason: ProvideErrorMetadata::code(&error).map(str::to_string),
                details: Some(
                    ::aws_smithy_types::error::display::DisplayErrorContext(&error).to_string(),
                ),
            }
        }
        Err(_elapsed) => Outcome::TimedOut,
    }
}

/// The error class a failed invocation's payload names, which is what a decider branches on
/// — Lambda's own `FunctionError` ("Handled"/"Unhandled") when the payload names none.
fn raised_error(function_error: &str, payload: Option<&str>) -> String {
    payload
        .and_then(|payload| ::serde_json::from_str::<::serde_json::Value>(payload).ok())
        .as_ref()
        .and_then(|payload| payload.get(ERROR_TYPE))
        .and_then(::serde_json::Value::as_str)
        .unwrap_or(function_error)
        .to_string()
}

/// How long this task waits for its function: the `startToCloseTimeout` the decision stated,
/// at most [`MAX_START_TO_CLOSE`], which is also what a decision that stated none gets.
///
/// SWF's unlimited-duration sentinel is not one of this member's values — the timeout is "any
/// integer from 1-900" — so a value that is not a plain number of seconds, or one past the
/// documented maximum, leaves the task on that maximum. No task waits longer than the longest
/// one SWF gives.
fn start_to_close_budget(task: &LambdaTask) -> Duration {
    task.start_to_close_timeout
        .as_deref()
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
        .map_or(MAX_START_TO_CLOSE, |seconds| {
            Duration::from_secs(seconds).min(MAX_START_TO_CLOSE)
        })
}

/// The Region one task's invocation is addressed to: the one the function's own ARN names,
/// when the decision named it that way, else the Region the decider's call was signed for.
///
/// A Lambda function is a regional resource and the full-ARN form is the only one that names
/// a Region. The transport is in process either way: the label selects the endpoint the
/// client is configured for, and the invocation is served by the Lambda this emulator mounts.
fn invocation_region(task: &LambdaTask) -> &str {
    ::service_common::arn::region_of(&task.name, LAMBDA_ARN_NAMESPACE).unwrap_or(&task.region)
}

/// AWS Lambda's client for this hop, over the in-process callback into this same emulator:
/// the function is one this emulator holds, so the invocation never leaves the process.
///
/// What a network client needs and this one cannot have is disabled — retries, timeouts,
/// stalled-stream protection and identity caching each need a sleep implementation the
/// callback transport is not given, and the wait is bounded by [`start_to_close_budget`]
/// above.
fn client(callback: &EmbeddedEmulator, region: &str) -> ::aws_sdk_lambda::Client {
    let config = ::aws_sdk_lambda::Config::builder()
        .behavior_version(::aws_sdk_lambda::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_lambda::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://lambda.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_lambda::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_lambda::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_lambda::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_lambda::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_lambda::Client::from_conf(config)
}
