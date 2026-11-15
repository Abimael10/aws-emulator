// SPDX-License-Identifier: CC0-1.0

//! Where a SERVICE JOB's work is DISPATCHED, and what the dispatched work reports back.
//!
//! A service job is "a unit of work that you submit to Batch for execution on SageMaker AI"
//! (`SubmitServiceJob`), its `serviceRequestPayload` is "the request, in JSON, for the
//! service that the `SubmitServiceJob` operation is queueing", and the only identifier the
//! model lets an attempt of one report is a `ServiceResourceId` whose `name` has exactly one
//! member, `TrainingJobArn` (`smithy_query(query="shape", shape="ServiceResourceIdName")`).
//! So Batch's own work on a service job is: accept it, order it in its queue, HAND THE
//! PAYLOAD ON to SageMaker AI as a `CreateTrainingJob` request, and then report the training
//! job's own progress as the service job's `ServiceJobStatus`.
//!
//! That is what this module does, over the in-process callback transport
//! (`OperationContext::callback`): the request is signed as the identity the framework hands
//! this service ([`::service_common::in_process_sigv4`]) and dispatched back into this same
//! emulator, so the training job a dispatch creates is one SageMaker's OWN
//! `DescribeTrainingJob` answers for. Nothing here opens a socket or leaves the host, and
//! nothing about the run is invented: every status, reason and instant a dispatched service
//! job reports is read off the training job SageMaker holds for it.
//!
//! # What the clock owes, and what a call does
//!
//! The clock owes a submitted service job one move — `SUBMITTED` -> `RUNNABLE`, where a
//! service job waits for the capacity its queue's service environments publish
//! ([`ComAmazonawsBatchAwsbatchv20160810State::advance_service_jobs`]). From `RUNNABLE` on,
//! what moves the job is the DOWNSTREAM service, so the step is a call and not a clock tick:
//! a placed job's dispatch is owed ([`ServiceJobDispatchState::Due`]), and the operations of
//! this service's job plane drive it ([`advance_service_job_dispatch`]) before they read
//! anything, exactly as they materialise what the clock owed. Both are changes that were
//! already due at the same instant whoever called, so two reads at one instant still answer
//! identically.
//!
//! Every call does a BOUNDED amount of that work ([`MAX_DISPATCH_STEPS_PER_CALL`]), takes no
//! store guard across an await, and leaves a dispatch whose answer never came to be attempted
//! again once [`DISPATCH_RETRY_AFTER_MILLIS`] has passed.
//!
//! An emulator that holds no SageMaker AI at all answers the dispatch with the front door's
//! own refusal rather than that service's ([`::service_common::in_process_refusal`]), which
//! resolves nothing: the call reached nothing, so the job stays `RUNNABLE` — where a service
//! job waits — instead of being reported as having run.

use super::*;

use ::serde_json::Value;
use ::smithy_server_runtime::OperationContext;

use crate::ops::types::{
    LatestServiceJobAttempt, ServiceJobAttemptDetail, ServiceJobCapacityUsageDetail,
    ServiceJobCapacityUsageSummary, ServiceResourceId,
};

/// The signing name of the service a `SAGEMAKER_TRAINING` service job's work is dispatched
/// to (`smithy_query(query="service_info")` of that service: `sigv4Name` `sagemaker`).
const SAGEMAKER_SIGNING_NAME: &str = "sagemaker";

/// The `X-Amz-Target` prefix that service's awsJson1_1 API names its operations under.
const SAGEMAKER_TARGET_PREFIX: &str = "SageMaker";

/// The operation a service job's payload is handed on as.
const CREATE_TRAINING_JOB: &str = "CreateTrainingJob";

/// The operation the dispatched work's own progress is read with.
const DESCRIBE_TRAINING_JOB: &str = "DescribeTrainingJob";

/// The operation that ends the dispatched work a `TerminateServiceJob` ended.
const STOP_TRAINING_JOB: &str = "StopTrainingJob";

/// The media type of an awsJson1_1 body.
const AWS_JSON_1_1: &str = "application/x-amz-json-1.1";

/// `CreateTrainingJob`'s own member for the name of the job it creates, which is also how
/// the job is addressed afterwards.
const TRAINING_JOB_NAME: &str = "TrainingJobName";

/// The only `ServiceResourceIdName` the model declares, so the only identifier an attempt of
/// a service job can report.
pub const SERVICE_RESOURCE_ID_TRAINING_JOB_ARN: &str = "TrainingJobArn";

/// How long a PLACED service job waits in `RUNNABLE` before its work is handed on to the
/// service that runs it.
///
/// A span this emulator chooses — AWS documents no duration for it — and the one the project
/// fixes for a transition the clock owes. It is deliberately longer than
/// [`JOB_PHASE_MILLIS`], the span every other move of this service takes, because `RUNNABLE`
/// is the status a caller OBSERVES a service job in: it is what `GetJobQueueSnapshot` reports
/// at the front of its queue and what `ListServiceJobs` filters on, so a caller polling
/// either has a whole span in which to see the job waiting there before it is dispatched.
pub const SERVICE_JOB_DISPATCH_MILLIS: i64 = 3_000;

/// How many dispatch steps ONE call performs.
///
/// The EMULATOR's own bound: each step is a call into this same process, so how many a single
/// request can make is fixed here rather than by how many service jobs the store happens to
/// hold. The store itself is capped ([`limits::MAX_SERVICE_JOBS`]), and what is left over is
/// picked up by the next call.
pub const MAX_DISPATCH_STEPS_PER_CALL: usize = 8;

/// How long a dispatch already in flight is left alone before it is attempted again.
///
/// A dispatch is marked in flight so two concurrent callers cannot hand one payload on twice;
/// this is what keeps a job whose dispatching task never came back from staying that way for
/// ever.
pub const DISPATCH_RETRY_AFTER_MILLIS: i64 = 5_000;

/// How much of the downstream answer is read. Each one is a handful of members, so the bytes
/// a read costs are fixed here rather than by whatever that service answers.
const MAX_ANSWER_BYTES: usize = 64 * 1024;

/// The header an AWS protocol names a refusal's own shape in.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// How far a service job's work has got with the service it is dispatched to.
///
/// Not a modelled member: the modelled `ServiceJobStatus` is what a reader sees, and this is
/// the bookkeeping that decides which dispatch step — if any — this service owes the job.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ServiceJobDispatchState {
    /// Nothing is owed: the job has not been placed yet, or the scheduler is holding it in
    /// `RUNNABLE` until its queue's service environments publish the capacity it asked for.
    #[default]
    Waiting,
    /// The scheduler placed the job, so its payload is owed to the downstream service.
    Due,
    /// A dispatch call is out ([`DISPATCH_RETRY_AFTER_MILLIS`]).
    InFlight,
    /// The downstream service accepted the work and holds the resource it created, whose ARN
    /// is on the record ([`ServiceJob::service_resource_arn`]).
    Dispatched,
    /// The downstream service has been asked to end the work, which is what a
    /// `TerminateServiceJob` of a dispatched job asks of it.
    Stopped,
}

/// One step this service owes the downstream service for one job.
#[derive(Debug, Clone)]
enum Step {
    /// Hand the job's payload on as a `CreateTrainingJob` request.
    Create {
        job_id: String,
        body: Vec<u8>,
    },
    /// Read the dispatched work's own progress.
    Poll { job_id: String, name: String },
    /// End the dispatched work.
    Stop { job_id: String, name: String },
}

impl Step {
    /// The service job this step belongs to.
    fn job_id(&self) -> &str {
        match self {
            Self::Create { job_id, .. } | Self::Poll { job_id, .. } | Self::Stop { job_id, .. } => {
                job_id
            }
        }
    }
}

/// What one step answered.
#[derive(Debug, Clone)]
enum Outcome {
    /// The downstream service accepted the work, under this resource ARN.
    Accepted { resource_arn: String },
    /// The downstream service REFUSED the work, saying this.
    Refused { reason: String },
    /// The call reached nothing at all, so it says nothing about the job.
    Unreached,
    /// The dispatched work's own progress, as the downstream service reports it.
    Reported {
        status: String,
        failure_reason: Option<String>,
        started_at: Option<i64>,
        ended_at: Option<i64>,
    },
    /// The end of the work was asked for.
    StopAsked,
}

/// The `TrainingJobStatus` of work that is still being done.
const TRAINING_IN_PROGRESS: &str = "InProgress";

/// `TrainingJobStatus` of work that is being brought to an end.
const TRAINING_STOPPING: &str = "Stopping";

/// `TrainingJobStatus` of work that finished.
const TRAINING_COMPLETED: &str = "Completed";

/// `TrainingJobStatus` of work that failed.
const TRAINING_FAILED: &str = "Failed";

/// `TrainingJobStatus` of work that was ended before it finished.
const TRAINING_STOPPED: &str = "Stopped";

/// The `ServiceJobStatus` a service job reports for a `TrainingJobStatus` of the work it was
/// dispatched as, or `None` for a status that says nothing about the service job.
///
/// A service job IS the downstream unit of work, so the two statuses are the same fact in two
/// vocabularies: work still being done or being brought to an end is `RUNNING`, work that
/// finished is `SUCCEEDED`, and work that failed or was ended before it finished is `FAILED` —
/// the only terminal members `ServiceJobStatus` has besides `SUCCEEDED`
/// (`smithy_query(query="shape", shape="ServiceJobStatus")`).
pub fn service_job_status_of_training_status(training_status: &str) -> Option<&'static str> {
    match training_status {
        TRAINING_IN_PROGRESS | TRAINING_STOPPING => Some(SERVICE_JOB_RUNNING),
        TRAINING_COMPLETED => Some(SERVICE_JOB_SUCCEEDED),
        TRAINING_FAILED | TRAINING_STOPPED => Some(SERVICE_JOB_FAILED),
        _ => None,
    }
}

/// Drive every dispatch step this service owes at `now`, and record what each one answered.
///
/// Called by the operations of the service-job plane BEFORE they read anything, so what they
/// report is the job as it stands at `now`. Takes no store guard across an await: the steps
/// are collected under one guard, performed with none held, and recorded under another.
pub async fn advance_service_job_dispatch<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    ctx: &OperationContext<'_>,
    now: i64,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    let steps = {
        let mut store = state.lock();
        store.materialize_due(now);
        store.take_dispatch_steps(now)
    };
    if steps.is_empty() {
        return;
    }
    let region = request_region(ctx);
    let mut answered: Vec<(String, Outcome)> = Vec::with_capacity(steps.len());
    for step in steps {
        let outcome = perform(ctx, &region, &step).await;
        answered.push((step.job_id().to_string(), outcome));
    }
    let mut store = state.lock();
    let mut freed = false;
    for (job_id, outcome) in answered {
        freed |= store.record_dispatch_outcome(&job_id, &outcome, now);
    }
    // A job that reached a terminal status has given back the capacity it held, and the call
    // that freed it is what makes a job RESTING on that capacity placeable again — one
    // dispatch span later, like every other transition this service owes.
    if freed {
        store.rearm_resting_service_jobs(now);
    }
}

/// Perform one step against the downstream service.
async fn perform(ctx: &OperationContext<'_>, region: &str, step: &Step) -> Outcome {
    match step {
        Step::Create { body, .. } => {
            match call(ctx, region, CREATE_TRAINING_JOB, body.clone()).await {
                None => Outcome::Unreached,
                Some(answered) if answered.unserved() => Outcome::Unreached,
                Some(answered) if answered.is_success() => Outcome::Accepted {
                    resource_arn: answered
                        .document()
                        .as_ref()
                        .and_then(|document| document.get("TrainingJobArn"))
                        .and_then(Value::as_str)
                        .unwrap_or_default()
                        .to_string(),
                },
                Some(answered) => Outcome::Refused {
                    reason: answered.message(),
                },
            }
        }
        Step::Poll { name, .. } => {
            let body = addressed(name);
            match call(ctx, region, DESCRIBE_TRAINING_JOB, body).await {
                Some(answered) if answered.is_success() && !answered.unserved() => {
                    let document = answered.document().unwrap_or(Value::Null);
                    Outcome::Reported {
                        status: document
                            .get("TrainingJobStatus")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        failure_reason: document
                            .get("FailureReason")
                            .and_then(Value::as_str)
                            .map(str::to_string),
                        started_at: epoch_millis(&document, "TrainingStartTime"),
                        ended_at: epoch_millis(&document, "TrainingEndTime"),
                    }
                }
                // A refusal of the read says nothing about the work: the service job keeps
                // the status it already had and the read is made again.
                _ => Outcome::Unreached,
            }
        }
        Step::Stop { name, .. } => {
            let body = addressed(name);
            // Whatever the stop answered, it has been ASKED: the service job is already in
            // its terminal status, which the caller's terminate put it in, so nothing the
            // answer carries changes what a reader sees.
            let _ = call(ctx, region, STOP_TRAINING_JOB, body).await;
            Outcome::StopAsked
        }
    }
}

/// The body that addresses one training job by name.
fn addressed(name: &str) -> Vec<u8> {
    ::serde_json::to_vec(&::serde_json::json!({ TRAINING_JOB_NAME: name })).unwrap_or_default()
}

/// An epoch-millisecond instant from an awsJson timestamp member, which carries epoch
/// seconds.
fn epoch_millis(document: &Value, member: &str) -> Option<i64> {
    let seconds = document.get(member)?.as_f64()?;
    Some((seconds * 1000.0) as i64)
}

/// What one in-process call answered.
struct Answered {
    status: u16,
    code: String,
    payload: Vec<u8>,
}

impl Answered {
    /// Whether the downstream service answered a success.
    fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Whether this is the emulator's own FRONT DOOR refusing a call no mounted service
    /// served, rather than an answer from the service that was addressed.
    fn unserved(&self) -> bool {
        ::service_common::in_process_refusal::is_transport_refusal(&self.payload)
    }

    /// The document the answer carries, when the body is one.
    fn document(&self) -> Option<Value> {
        ::serde_json::from_slice(&self.payload).ok()
    }

    /// What a refusal SAID, as the refusing service worded it — its own message, else the
    /// shape it named itself by.
    fn message(&self) -> String {
        let said = self
            .document()
            .as_ref()
            .and_then(|document| {
                document
                    .get("message")
                    .or_else(|| document.get("Message"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        if said.is_empty() {
            self.code.clone()
        } else {
            said
        }
    }
}

/// Sign one awsJson1_1 request for the downstream service and dispatch it in process,
/// answering what that service answered — or `None` when the call could not be MADE at all,
/// which is a fact about this process rather than about the job.
async fn call(
    ctx: &OperationContext<'_>,
    region: &str,
    operation: &str,
    body: Vec<u8>,
) -> Option<Answered> {
    let mut request = ::http::Request::builder()
        .method("POST")
        .uri("/")
        .header(
            "host",
            format!("{SAGEMAKER_SIGNING_NAME}.{region}.amazonaws.com"),
        )
        .header("content-type", AWS_JSON_1_1)
        .header(
            "x-amz-target",
            format!("{SAGEMAKER_TARGET_PREFIX}.{operation}"),
        )
        .body(body)
        .ok()?;
    // The emulator verifies every request signature, so an in-process call carries one too:
    // the credential the framework hands this service, signed through the tree's one
    // in-process signer. It is never reported or logged.
    ::service_common::in_process_sigv4::sign_in_process_request(
        &mut request,
        SAGEMAKER_SIGNING_NAME,
        region,
        ctx,
    )
    .await
    .ok()?;
    let (parts, body) = request.into_parts();
    let request = ::aws_smithy_runtime_api::http::Request::try_from(::http::Request::from_parts(
        parts,
        ::aws_smithy_types::body::SdkBody::from(::bytes::Bytes::from(body)),
    ))
    .ok()?;
    let answer = {
        use ::aws_smithy_runtime_api::client::http::HttpConnector;
        let connector = ctx.callback().http_connector();
        HttpConnector::call(&connector, request).await.ok()?
    };
    let status = answer.status().as_u16();
    let error_type = answer
        .headers()
        .get(ERROR_TYPE_HEADER)
        .unwrap_or_default()
        .to_string();
    let payload: Vec<u8> = answer
        .body()
        .bytes()
        .unwrap_or_default()
        .iter()
        .take(MAX_ANSWER_BYTES)
        .copied()
        .collect();
    Some(Answered {
        status,
        code: refusal_code(&error_type, &payload),
        payload,
    })
}

/// The code a refusal names itself by: the shape in the error-type header, else the one the
/// body's `__type` member names — the shape's own NAME either way.
fn refusal_code(error_type: &str, payload: &[u8]) -> String {
    let named = if error_type.is_empty() {
        ::serde_json::from_slice::<Value>(payload)
            .ok()
            .as_ref()
            .and_then(|document| document.get("__type"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        error_type.to_string()
    };
    named
        .split(':')
        .next()
        .unwrap_or_default()
        .rsplit(['#', '/'])
        .next()
        .unwrap_or_default()
        .to_string()
}

impl ComAmazonawsBatchAwsbatchv20160810State {
    /// The dispatch steps this service owes at `now`, with each job marked as having its step
    /// taken, so two concurrent callers never hand one payload on twice.
    ///
    /// Walked in job-id order and capped at [`MAX_DISPATCH_STEPS_PER_CALL`], so which steps a
    /// call takes depends on the store and not on how a map happened to be iterated.
    fn take_dispatch_steps(&mut self, now: i64) -> Vec<Step> {
        let mut ids: Vec<String> = self.service_jobs.keys().cloned().collect();
        ids.sort();
        let mut steps: Vec<Step> = Vec::new();
        for id in ids {
            if steps.len() >= MAX_DISPATCH_STEPS_PER_CALL {
                break;
            }
            let Some(job) = self.service_jobs.get(&id) else {
                continue;
            };
            let terminal = is_terminal_service_job_status(&job.status);
            let in_flight_expired = job
                .dispatch_attempted_at
                .is_none_or(|at| now.saturating_sub(at) >= DISPATCH_RETRY_AFTER_MILLIS);
            let step = match job.dispatch_state {
                // A job ended before its work was ever handed on has none to hand on.
                ServiceJobDispatchState::Due | ServiceJobDispatchState::InFlight if terminal => {
                    if let Some(job) = self.service_jobs.get_mut(&id) {
                        job.dispatch_state = ServiceJobDispatchState::Waiting;
                    }
                    continue;
                }
                ServiceJobDispatchState::Due => create_step(job),
                ServiceJobDispatchState::InFlight if in_flight_expired => create_step(job),
                ServiceJobDispatchState::Dispatched => {
                    let name = job.service_resource_name.clone().unwrap_or_default();
                    if name.is_empty() {
                        None
                    } else if terminal {
                        // The caller's terminate ended the service job; the work it was
                        // dispatched as is ended with it.
                        Some(Step::Stop {
                            job_id: id.clone(),
                            name,
                        })
                    } else {
                        Some(Step::Poll {
                            job_id: id.clone(),
                            name,
                        })
                    }
                }
                _ => None,
            };
            let Some(step) = step else {
                continue;
            };
            if let Some(job) = self.service_jobs.get_mut(&id) {
                match &step {
                    Step::Create { .. } => {
                        job.dispatch_state = ServiceJobDispatchState::InFlight;
                        job.dispatch_attempted_at = Some(now);
                        // The name the create addresses is settled BEFORE the call goes out,
                        // so a poll or a stop that follows reaches the job the create made
                        // even though the create's own answer carries only its ARN.
                        job.service_resource_name = Some(dispatch_name(job));
                    }
                    Step::Stop { .. } => job.dispatch_state = ServiceJobDispatchState::Stopped,
                    Step::Poll { .. } => {}
                }
            }
            steps.push(step);
        }
        steps
    }

    /// Record what one dispatch step answered for `job_id`, and answer whether that ENDED
    /// the job — which is when it gives back the capacity it was holding.
    fn record_dispatch_outcome(&mut self, job_id: &str, outcome: &Outcome, now: i64) -> bool {
        let Some(job) = self.service_jobs.get_mut(job_id) else {
            return false;
        };
        match outcome {
            Outcome::Accepted { resource_arn } => {
                job.dispatch_state = ServiceJobDispatchState::Dispatched;
                job.service_resource_arn = (!resource_arn.is_empty()).then(|| resource_arn.clone());
                // The work has been handed on and the downstream service holds it: the job
                // was scheduled for execution at this instant and is now running it.
                job.scheduled_at = Some(now);
                if !is_terminal_service_job_status(&job.status) {
                    job.status = SERVICE_JOB_RUNNING.to_string();
                    job.status_reason = None;
                    job.started_at = Some(now);
                }
                false
            }
            Outcome::Refused { reason } => {
                // The downstream service would not take the work, so the service job ends in
                // the modelled failure state carrying what that service said.
                job.dispatch_state = ServiceJobDispatchState::Waiting;
                job.service_resource_name = None;
                if is_terminal_service_job_status(&job.status) {
                    return false;
                }
                job.status = SERVICE_JOB_FAILED.to_string();
                job.status_reason = Some(reason.clone());
                job.stopped_at = Some(now);
                job.next_phase_due_at = None;
                true
            }
            // Nothing served the call, so nothing is known: the job stays where a service job
            // waits, and the step is owed again.
            Outcome::Unreached => {
                if job.dispatch_state == ServiceJobDispatchState::InFlight {
                    job.dispatch_state = ServiceJobDispatchState::Due;
                    job.dispatch_attempted_at = None;
                }
                false
            }
            Outcome::Reported {
                status,
                failure_reason,
                started_at,
                ended_at,
            } => {
                if is_terminal_service_job_status(&job.status) {
                    return false;
                }
                let Some(reported) = service_job_status_of_training_status(status) else {
                    return false;
                };
                job.status = reported.to_string();
                job.started_at = started_at.or(job.started_at);
                if !is_terminal_service_job_status(reported) {
                    return false;
                }
                job.stopped_at = Some(ended_at.unwrap_or(now));
                job.status_reason = failure_reason.clone();
                job.next_phase_due_at = None;
                true
            }
            Outcome::StopAsked => {
                job.dispatch_state = ServiceJobDispatchState::Stopped;
                false
            }
        }
    }
}

/// The `Create` step for `job`: its payload handed on as a `CreateTrainingJob` request, with
/// the name the training job is created under settled here.
///
/// The payload is the caller's own request document, so it travels member for member; the one
/// member this service settles is [`TRAINING_JOB_NAME`], because the training job has to be
/// addressable afterwards and a payload that names none leaves the service job's own name as
/// the only name there is. A payload that is not a JSON object at all is handed on exactly as
/// it was written, and the downstream service's own deserializer is what judges it.
fn create_step(job: &ServiceJob) -> Option<Step> {
    let payload = job.service_request_payload.clone().unwrap_or_default();
    let document = ::serde_json::from_str::<Value>(&payload).ok();
    let named = document
        .as_ref()
        .and_then(|document| document.get(TRAINING_JOB_NAME))
        .and_then(Value::as_str)
        .map(str::to_string);
    let name = named.clone().unwrap_or_else(|| job.job_name.clone());
    let body = match (document, named.is_some()) {
        (Some(Value::Object(mut members)), false) => {
            members.insert(TRAINING_JOB_NAME.to_string(), Value::String(name.clone()));
            ::serde_json::to_vec(&Value::Object(members)).ok()?
        }
        _ => payload.into_bytes(),
    };
    Some(Step::Create {
        job_id: job.job_id.clone(),
        body,
    })
}

/// The name of the training job a `Create` step would address, so the record can be marked
/// with it before the call goes out.
fn dispatch_name(job: &ServiceJob) -> String {
    ::serde_json::from_str::<Value>(job.service_request_payload.as_deref().unwrap_or_default())
        .ok()
        .as_ref()
        .and_then(|document| document.get(TRAINING_JOB_NAME))
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| job.job_name.clone())
}

impl ServiceJob {
    /// Whether this job is HOLDING the capacity it asked for right now: its work has been
    /// handed on and has not finished.
    ///
    /// What a job holds is what the capacity reports of this service count
    /// ([`Self::capacity_usage`]): a job still waiting in its queue has been given nothing
    /// yet, and one in a terminal `ServiceJobStatus` has given back whatever it held.
    pub fn holds_capacity(&self) -> bool {
        self.service_resource_arn.is_some() && !is_terminal_service_job_status(&self.status)
    }

    /// Whether the scheduler has ALLOCATED this job the capacity it asked for: it has been
    /// placed — its work is owed to, in flight to, or already with the service it is
    /// dispatched to — and it has not finished.
    ///
    /// What the SCHEDULER counts, where [`Self::holds_capacity`] is what a report counts:
    /// the allocation is what makes a published maximum a concurrency bound, so it begins at
    /// the placement rather than when the downstream service answers, and the next job of
    /// the same share cannot be placed on capacity this one was already given.
    pub fn is_allocated_capacity(&self) -> bool {
        self.dispatch_state != ServiceJobDispatchState::Waiting
            && !is_terminal_service_job_status(&self.status)
    }

    /// `ServiceResourceId` of the resource the downstream service holds this job's work as,
    /// or `None` for a job nothing has been dispatched for.
    ///
    /// `ServiceResourceIdName` declares exactly one member
    /// ([`SERVICE_RESOURCE_ID_TRAINING_JOB_ARN`]), and the value is the ARN that service
    /// answered — never one built here.
    pub fn service_resource_id(&self) -> Option<ServiceResourceId> {
        Some(ServiceResourceId {
            name: SERVICE_RESOURCE_ID_TRAINING_JOB_ARN.to_string(),
            value: self.service_resource_arn.clone()?,
        })
    }

    /// `latestAttempt`: "information about the latest attempt for the service job" — the one
    /// attempt a dispatch started, or `None` before there has been one.
    pub fn latest_attempt(&self) -> Option<LatestServiceJobAttempt> {
        Some(LatestServiceJobAttempt {
            service_resource_id: Some(self.service_resource_id()?),
        })
    }

    /// `attempts`: the attempts this job has had, which is the ONE a dispatch started.
    ///
    /// `startedAt` and `stoppedAt` are the job's own, because the attempt IS the dispatched
    /// work; `statusReason` is what that work reported when it ended, and stays absent while
    /// it is still being done.
    pub fn attempt_details(&self) -> Option<Vec<ServiceJobAttemptDetail>> {
        let resource = self.service_resource_id()?;
        Some(vec![ServiceJobAttemptDetail {
            service_resource_id: Some(resource),
            started_at: self.started_at,
            stopped_at: self.stopped_at,
            status_reason: self.status_reason.clone(),
        }])
    }

    /// How much capacity this job is using, per unit, or `None` while it holds none.
    ///
    /// `ServiceJobCapacityUsageDetail$capacityUnit`: "for service jobs, this is the instance
    /// type", so each entry is one instance type the request asked for and how many instances
    /// of it; a request that names no type is counted in
    /// [`CAPACITY_UNIT_NUM_INSTANCES`], the unit the capacity-limit member documents for
    /// instances whatever their type. The quantities come from the demand derived when the
    /// submit was accepted ([`requested_capacity`]), so no later call re-reads the payload.
    pub fn capacity_usage(&self) -> Option<Vec<(String, f64)>> {
        if !self.holds_capacity() || self.requested_capacity.is_empty() {
            return None;
        }
        let mut per_unit: BTreeMap<String, f64> = BTreeMap::new();
        for demand in &self.requested_capacity {
            let unit = demand
                .instance_type
                .clone()
                .unwrap_or_else(|| CAPACITY_UNIT_NUM_INSTANCES.to_string());
            *per_unit.entry(unit).or_insert(0.0) += f64::from(demand.instances);
        }
        Some(per_unit.into_iter().collect())
    }

    /// [`Self::capacity_usage`] as `DescribeServiceJob` reports it.
    pub fn capacity_usage_detail(&self) -> Option<Vec<ServiceJobCapacityUsageDetail>> {
        Some(
            self.capacity_usage()?
                .into_iter()
                .map(|(capacity_unit, quantity)| ServiceJobCapacityUsageDetail {
                    capacity_unit: Some(capacity_unit),
                    quantity: Some(quantity),
                })
                .collect(),
        )
    }

    /// [`Self::capacity_usage`] as `ListServiceJobs` reports it.
    pub fn capacity_usage_summary(&self) -> Option<Vec<ServiceJobCapacityUsageSummary>> {
        Some(
            self.capacity_usage()?
                .into_iter()
                .map(|(capacity_unit, quantity)| ServiceJobCapacityUsageSummary {
                    capacity_unit: Some(capacity_unit),
                    quantity: Some(quantity),
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a dispatched service job's `ServiceJobStatus` is the status of the work it was
    /// dispatched as, in this service's own vocabulary.
    #[test]
    fn a_dispatched_jobs_status_is_the_status_of_the_work_it_was_dispatched_as() {
        // One row per `TrainingJobStatus` the downstream model declares, plus a value it does
        // not, which says nothing about the service job.
        let rows: &[(&str, Option<&str>)] = &[
            ("InProgress", Some(SERVICE_JOB_RUNNING)),
            ("Stopping", Some(SERVICE_JOB_RUNNING)),
            ("Completed", Some(SERVICE_JOB_SUCCEEDED)),
            ("Failed", Some(SERVICE_JOB_FAILED)),
            ("Stopped", Some(SERVICE_JOB_FAILED)),
            ("Deleting", None),
            ("", None),
        ];
        for (training_status, expected) in rows {
            let actual = service_job_status_of_training_status(training_status);
            assert_eq!(
                actual, *expected,
                "case {training_status:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// RULE: the training job a payload is handed on as is named by the payload when it names
    /// one, and by the service job otherwise — and the payload travels member for member
    /// either way.
    #[test]
    fn a_payload_that_names_no_training_job_is_named_by_the_service_job() {
        // Rows differ along what the payload IS: a document naming the training job, one that
        // does not, an empty document, and a value that is not a JSON object at all.
        let rows: &[(&str, &str, Option<&str>)] = &[
            (
                "a payload naming the training job",
                r#"{"TrainingJobName":"named-by-the-payload","RoleArn":"r"}"#,
                Some("named-by-the-payload"),
            ),
            (
                "a payload naming no training job",
                r#"{"RoleArn":"r"}"#,
                Some("the-service-job"),
            ),
            ("an empty document", "{}", Some("the-service-job")),
            ("a payload that is not an object", "[]", None),
        ];
        for (case, payload, expected_name) in rows {
            let job = ServiceJob {
                job_id: "id".to_string(),
                job_name: "the-service-job".to_string(),
                service_request_payload: Some((*payload).to_string()),
                ..ServiceJob::default()
            };
            let Some(Step::Create { body, .. }) = create_step(&job) else {
                panic!("case {case:?}: expected a create step for {payload:?}");
            };
            let sent = ::serde_json::from_slice::<Value>(&body).ok();
            let actual = sent
                .as_ref()
                .and_then(|document| document.get(TRAINING_JOB_NAME))
                .and_then(Value::as_str);
            assert_eq!(
                actual, *expected_name,
                "case {case:?}: expected the training job to be named {expected_name:?}, got \
                 {actual:?} in {sent:?}"
            );
            // Whatever the payload carried travels with it: a member the caller wrote is in
            // the request the downstream service receives.
            if let Ok(Value::Object(original)) = ::serde_json::from_str::<Value>(payload) {
                for (member, value) in original {
                    let carried = sent.as_ref().and_then(|document| document.get(&member));
                    assert_eq!(
                        carried,
                        Some(&value),
                        "case {case:?}: expected {member:?} to travel with the payload, got \
                         {carried:?}"
                    );
                }
            }
        }
    }

    /// RULE: the name a dispatch addresses is the payload's when it names one, so a later poll
    /// and a later stop reach the job the create made.
    #[test]
    fn the_name_a_dispatch_addresses_comes_from_the_payload_or_the_job() {
        let rows: &[(&str, &str, &str)] = &[
            (
                "a payload naming the training job",
                r#"{"TrainingJobName":"t-7"}"#,
                "t-7",
            ),
            ("a payload naming none", "{}", "the-service-job"),
            ("a payload that is not JSON", "not json", "the-service-job"),
        ];
        for (case, payload, expected) in rows {
            let job = ServiceJob {
                job_name: "the-service-job".to_string(),
                service_request_payload: Some((*payload).to_string()),
                ..ServiceJob::default()
            };
            let actual = dispatch_name(&job);
            assert_eq!(
                actual, *expected,
                "case {case:?}: expected the dispatch to address {expected:?}, got {actual:?}"
            );
        }
    }
}
