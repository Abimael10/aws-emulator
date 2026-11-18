// SPDX-License-Identifier: CC0-1.0

//! STARTING a stored-video analysis: the one sequence every `Start…` operation of this API
//! runs, so the family shares it instead of carrying a copy per operation.
//!
//! Each of these operations is documented the same way — it "starts asynchronous detection …
//! in a stored video" and "returns a job identifier (`JobId`)" the matching `Get…` call reads
//! the job by — and each models the same failures for it, so the ORDER of the steps is one
//! rule for all of them:
//!
//! 1. a `ClientRequestToken` the SAME operation already committed decides the answer before
//!    anything else is looked at: the same request again is the job it already started, and a
//!    token naming a different request is refused ([`super::idempotency`]);
//! 2. the object store is asked whether it serves the video the request names, which is the
//!    one thing a start establishes about the media ([`super::video_source`]);
//! 3. a start past the ceiling on concurrently running jobs is refused, the condition
//!    `LimitExceededException` is modelled for ([`super::video_jobs::MAX_CONCURRENT_JOBS`]);
//! 4. the job is recorded with a freshly minted `JobId`, which is the answer.
//!
//! A refused start records nothing and commits no token, so a caller may send the same
//! request again; the steps that write run under one lock, and no lock is held across the
//! read of the object store.
//!
//! What each operation keeps for itself is its OWN input members — the ones that configure
//! its analysis — which it hands over in [`StartRequest::parameters`], and which modelled
//! error it answers each [`NotStarted`] with: the reason type names the reason, the handler
//! names the exception.

use std::time::SystemTime;

use crate::ops::types::{NotificationChannel, Video};

use super::video_jobs::{self, VideoJob};
use super::video_source::{self, UnreachableVideo};
use super::ComAmazonawsRekognitionRekognitionserviceState;

/// One input member of a start request, by its modelled member name, and the value the
/// request stated for it — `None` for a member the request left out.
///
/// The distinction is what the job RECORD keeps: a member the request left out is left off
/// the record, so the job reports what the request asked for and nothing it did not. For the
/// idempotency COMPARISON an absent member is rendered as the empty value, which is how
/// [`video_jobs::committed_parameters`] renders every member the whole family compares.
pub type StatedMember = (String, Option<String>);

/// What a `Start…` operation of this API was asked for: the members the whole family carries,
/// plus the ones that configure the operation's own analysis.
#[derive(Debug)]
pub struct StartRequest<'a> {
    /// The operation starting the analysis (`StartLabelDetection`, …): what the job records
    /// as the analysis that minted it, and the scope a `ClientRequestToken` is compared in.
    pub analysis: &'a str,
    /// `Video` — the video the analysis is to run on.
    pub video: &'a Video,
    /// `ClientRequestToken`, when the request carried one.
    pub client_request_token: Option<&'a str>,
    /// `JobTag`, when the request carried one.
    pub job_tag: Option<&'a str>,
    /// `NotificationChannel`, when the request carried one.
    pub notification_channel: Option<&'a NotificationChannel>,
    /// The members that configure THIS analysis, by their modelled member names; see
    /// [`StatedMember`].
    pub parameters: Vec<StatedMember>,
}

/// Why a start was not carried out. Each variant has exactly one modelled answer, which the
/// handler picks, and carries the `Message` that answer is worded with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotStarted {
    /// The `ClientRequestToken` was reused for a request that differs from the one it already
    /// committed. Answered with `IdempotentParameterMismatchException`.
    IdempotentParameterMismatch(String),
    /// As many jobs are already running as may run at one instant. Answered with
    /// `LimitExceededException`.
    ConcurrencyLimitReached(String),
    /// The video the request names cannot be reached; the reason picks between the two
    /// modelled answers (see [`UnreachableVideo`]).
    Unreachable(UnreachableVideo),
}

impl NotStarted {
    /// The reason, as the modelled error's `Message`.
    pub fn message(&self) -> &str {
        match self {
            Self::IdempotentParameterMismatch(message)
            | Self::ConcurrencyLimitReached(message) => message,
            Self::Unreachable(reason) => reason.message(),
        }
    }
}

/// The `JobId` of the analysis `request` asks for: the identifier the job it records is read
/// by, or the one the request's own token was already answered with.
///
/// This is the whole of what a start operation does — see this module's docs for the order of
/// the steps and what each of them refuses.
pub async fn start(
    state: &::engine::ServiceState<ComAmazonawsRekognitionRekognitionserviceState>,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    request: StartRequest<'_>,
) -> Result<String, NotStarted> {
    // "The other input parameters" a reused `ClientRequestToken` is compared against: every
    // input member beside the token itself, which is the video, the notification channel and
    // the job tag for the whole family, plus the members of this operation's own analysis.
    let own: Vec<(&str, &str)> = request
        .parameters
        .iter()
        .map(|(member, value)| (member.as_str(), value.as_deref().unwrap_or_default()))
        .collect();
    let committed = video_jobs::committed_parameters(
        request.video.s3_object.as_ref(),
        request.notification_channel,
        request.job_tag,
        &own,
    );
    let compared: Vec<(&str, &str)> = committed
        .iter()
        .map(|(member, value)| (member.as_str(), value.as_str()))
        .collect();

    // A token this operation already committed is decided before the video is looked at: "if
    // you use the same token with multiple … requests, the same JobId is returned", so a
    // retry answers with the job it already started and starts nothing, and a token reused
    // for a different request is the modelled refusal. The guard is released before the
    // object store is asked, so nothing is held across an await.
    if let Some(token) = request.client_request_token {
        let held = state.lock();
        match held
            .client_request_tokens
            .classify(request.analysis, token, &compared)
        {
            super::idempotency::TokenUse::Mismatch => {
                return Err(NotStarted::IdempotentParameterMismatch(
                    super::modeled_error::IDEMPOTENT_PARAMETER_MISMATCH_MESSAGE.to_string(),
                ));
            }
            super::idempotency::TokenUse::Retry => {
                if let Some(job) = held.video_jobs.started_under_token(request.analysis, token) {
                    return Ok(job.job_id.clone());
                }
                // The store is bounded, so the job that token started may since have been
                // forgotten. There is no job left to answer with, so the request is carried
                // out again below rather than answered with an identifier nothing backs.
            }
            super::idempotency::TokenUse::First => {}
        }
    }

    // The one thing a start operation establishes about the video: that the object store
    // serves the object it names. Nothing of the media is read — see
    // [`super::video_source`].
    let video = video_source::locate_video(ctx, request.video)
        .await
        .map_err(NotStarted::Unreachable)?;

    let started_at = SystemTime::now();
    let mut state = state.lock();

    // The jobs live in this process's memory and an analysis runs for a span, so there is a
    // ceiling on how many may run at once: past it a start is refused with the modelled limit
    // error — the condition that error is modelled for — rather than allowed to grow the
    // process without limit. The check precedes the write, so a refused request records
    // nothing and commits no token.
    if state.video_jobs.at_concurrency_limit(started_at) {
        return Err(NotStarted::ConcurrencyLimitReached(
            super::modeled_error::video_job_concurrency_limit_message(),
        ));
    }

    let job_id = super::new_job_id();
    let mut job = VideoJob::new(job_id.clone(), request.analysis, video, started_at);
    job.client_request_token = request.client_request_token.map(str::to_string);
    job.job_tag = request.job_tag.map(str::to_string);
    job.notification_channel = request.notification_channel.cloned();
    // The analysis's own parameters, kept as the request STATED them: a member the request
    // left out is left off the record rather than recorded at whatever the service would
    // apply for it.
    job.parameters = request
        .parameters
        .iter()
        .filter_map(|(member, value)| value.clone().map(|value| (member.clone(), value)))
        .collect();
    state.video_jobs.record(job);

    if let Some(token) = request.client_request_token {
        state
            .client_request_tokens
            .commit(request.analysis, token, &compared);
    }

    Ok(job_id)
}
