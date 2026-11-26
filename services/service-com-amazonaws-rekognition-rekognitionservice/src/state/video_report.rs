// SPDX-License-Identifier: CC0-1.0

//! REPORTING a stored-video analysis job: the one sequence every `Get…` operation of this API
//! runs, so the family shares it instead of carrying a copy per operation.
//!
//! Each of these operations is documented the same way — it "gets the … results of a Amazon
//! Rekognition Video analysis started by `Start…`", is passed "the job identifier (`JobId`)
//! from the initial call", answers with "the current status of the … job" and serves the
//! results a page at a time — so the ORDER of the steps is one rule for all of them:
//!
//! 1. the identifier is resolved to a job of THIS analysis ([`report`]): the `Get…` call of an
//!    analysis reads the jobs its own `Start…` minted, so an identifier naming none — one this
//!    service never minted, one the bounded store has since forgotten, or one belonging to
//!    another analysis of the family — is the modelled `ResourceNotFoundException`;
//! 2. the job's status is read off the clock ([`super::video_jobs::VideoJob::status_at`]) and
//!    nothing is written: reporting a job is a READ, so a job's status is the same for the
//!    caller that polls it and the caller that never does;
//! 3. the results are served one page at a time ([`results_page`]), which a `NextToken` no
//!    `Get…` of this service minted is refused at with the modelled
//!    `InvalidPaginationTokenException`.
//!
//! WHAT A FINISHED ANALYSIS REPORTS: no video decoder runs in this process, so nothing here
//! measures a property of the media and nothing is found IN it (see [`super::video_source`]).
//! A finished job therefore reports an EMPTY results list rather than a detection nothing
//! measured, the same way every other answer of this service leaves a judgement nothing made
//! absent. What IS real about a stored-video analysis is the JOB — that it exists, which
//! analysis started it, on which video, under which tag, and where the clock has taken its
//! status — and that is what the `Get…` operations of the family report.

use std::time::SystemTime;

use super::video_jobs::{JobStatus, VideoJob};
use super::ComAmazonawsRekognitionRekognitionserviceState;

/// The largest page of analysis results a `Get…` of this family serves, and the page it
/// serves when the caller asks for no particular size.
///
/// It is what `MaxResults` documents on these operations: "maximum number of results to
/// return per paginated call. The largest value you can specify is 1000. If you specify a
/// value greater than 1000, a maximum of 1000 results is returned. The default value is 1000"
/// (`smithy_query(query="operation",operation="GetLabelDetection")`).
pub const MAX_RESULTS_PAGE_SIZE: usize = 1_000;

/// Why a job could not be reported. Each variant has exactly one modelled answer, which the
/// handler picks, and carries the `Message` that answer is worded with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotReported {
    /// The `JobId` names no job this analysis started. Answered with
    /// `ResourceNotFoundException`.
    UnknownJob(String),
    /// The `NextToken` is not a cursor a `Get…` of this service minted for this analysis.
    /// Answered with `InvalidPaginationTokenException`.
    InvalidPaginationToken(String),
}

impl NotReported {
    /// The reason, as the modelled error's `Message`.
    pub fn message(&self) -> &str {
        match self {
            Self::UnknownJob(message) | Self::InvalidPaginationToken(message) => message,
        }
    }

    /// The refusal for an identifier naming no job of the analysis that was asked.
    pub fn unknown_job(job_id: &str) -> Self {
        Self::UnknownJob(super::modeled_error::video_job_not_found_message(job_id))
    }

    /// The refusal for a `NextToken` this analysis's own pages did not mint.
    pub fn invalid_pagination_token() -> Self {
        Self::InvalidPaginationToken(
            super::modeled_error::INVALID_PAGINATION_TOKEN_MESSAGE.to_string(),
        )
    }
}

/// A job as a `Get…` operation reports it: the record its `Start…` call wrote, and the status
/// the clock has taken it to.
#[derive(Debug, Clone)]
pub struct ReportedJob {
    /// The job record, as the start that minted it wrote it.
    pub job: VideoJob,
    /// The job's status at the instant it was read.
    pub status: JobStatus,
}

/// The job `job_id` names, when the analysis `analysis` started it, at `now`.
///
/// This is the resolution step of every `Get…` operation of the family — see this module's
/// docs for the order of the steps and what each of them refuses. It takes the store's guard
/// for the read and releases it, and writes nothing.
pub fn report(
    state: &::engine::ServiceState<ComAmazonawsRekognitionRekognitionserviceState>,
    analysis: &str,
    job_id: &str,
    now: SystemTime,
) -> Result<ReportedJob, NotReported> {
    let held = state.lock();
    let job = held
        .video_jobs
        .get(job_id)
        // A job belongs to the analysis that started it, so the `Get…` call of one analysis
        // does not report another's job: that identifier names nothing it can report on.
        .filter(|job| job.analysis == analysis)
        .ok_or_else(|| NotReported::unknown_job(job_id))?;
    Ok(ReportedJob {
        status: job.status_at(now),
        job: job.clone(),
    })
}

/// One page of an analysis's results: the elements this call reports, and the cursor the page
/// after them is read by.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResultsPage<T> {
    /// The elements of this page, in the order the result list holds them.
    pub results: Vec<T>,
    /// The `NextToken` the next page is asked for with, or `None` once every element has been
    /// reported.
    pub next_token: Option<String>,
}

/// The page of `results` a `Get…` call of `analysis` asks for with `max_results` and
/// `next_token`.
///
/// Every element is reported exactly once to a caller that follows the cursors, and the last
/// page carries none: a cursor naming the position the caller already reached would be
/// followed forever without advancing. The cursor is scoped to the analysis that minted it, so
/// one `Get…` operation's token cannot be replayed against another's results, and a string no
/// page of this analysis minted names no position at all — the refusal the model declares
/// `InvalidPaginationTokenException` for.
pub fn results_page<T: Clone>(
    analysis: &str,
    results: &[T],
    max_results: Option<i32>,
    next_token: Option<&str>,
) -> Result<ResultsPage<T>, NotReported> {
    let start_at = match next_token {
        // No token asks for the first page; the model admits an empty `PaginationToken`
        // (`length 0..255`), which carries no position either.
        None | Some("") => 0,
        Some(token) => super::decode_cursor(analysis, token)
            .and_then(|position| position.parse::<usize>().ok())
            .ok_or_else(NotReported::invalid_pagination_token)?,
    };
    let page: Vec<T> = results
        .iter()
        .skip(start_at)
        .take(page_size(max_results))
        .cloned()
        .collect();
    let reported = start_at.saturating_add(page.len());
    let next_token = (reported < results.len())
        .then(|| super::encode_cursor(analysis, &reported.to_string()));
    Ok(ResultsPage {
        results: page,
        next_token,
    })
}

/// The page size a `MaxResults` of `requested` asks a `Get…` of this family for: what the
/// caller asked for, held at [`MAX_RESULTS_PAGE_SIZE`], which is also the page a caller that
/// asked for no size gets.
///
/// The modelled range starts at 1, which the boundary validator has already enforced, so this
/// only keeps the page size a page size for a value that reached here anyway.
pub fn page_size(requested: Option<i32>) -> usize {
    requested.map_or(MAX_RESULTS_PAGE_SIZE, |max| {
        usize::try_from(max)
            .unwrap_or(1)
            .clamp(1, MAX_RESULTS_PAGE_SIZE)
    })
}
