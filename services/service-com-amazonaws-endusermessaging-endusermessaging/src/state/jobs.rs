// SPDX-License-Identifier: CC0-1.0

//! The asynchronous jobs this service runs, and how `ListJobs` reports them.
//!
//! A job is the record of one mutating call the API answers asynchronously: the call
//! returns a `jobId` and the work finishes afterwards. Everything a job report carries
//! lives on the record here — the operation type that created it, the brand profile it
//! operates on, its current status, the resources it created or updated, and why it
//! failed — so one job has one meaning whichever operation looks at it.
//!
//! A job the service has accepted is reported `PROCESSING` and reaches its terminal
//! status later, which is a transition the CLOCK owes rather than one a caller asks
//! for: the record carries the instant it comes due ([`JobSettlement`]) and
//! [`ComAmazonawsEndusermessagingEndusermessagingState::settle_jobs`] materialises it,
//! so a report states the status the job had reached at that instant whether or not
//! anybody polled.

use std::time::{Duration, SystemTime};

use super::ComAmazonawsEndusermessagingEndusermessagingState;
use crate::ops::types::{JobResource, JobSummary};

/// The most jobs this emulator keeps for one account.
///
/// A ceiling, not a guess at AWS's published quota: it exists so a loop of
/// asynchronous calls cannot grow the emulator's heap without bound. The oldest job is
/// dropped once the store is full, so the jobs a caller can still reach are the most
/// recent ones.
pub const MAX_JOBS: usize = 100;

/// The `operationType` values a job of this service carries.
///
/// `JobOperationType` is modelled as a plain string, and documents the value as "the
/// type of mutating operation that created the job", so the set is the service's own
/// asynchronous operations rather than a modelled enum: the four operations whose
/// documentation states that they run asynchronously and whose output is a `JobResults`
/// list of the jobs they started (`smithy_query(query="digest")`:
/// `CreateBrandProfileFromRegistration`, `CreateRegistrationsFromBrandProfile`,
/// `UpdateBrandProfileFromRegistration`, `UpdateRegistrationsFromBrandProfile`). No
/// other operation of this service starts a job, so no other value can name one.
pub const JOB_OPERATION_TYPE_VALUES: &[&str] = &[
    "CreateBrandProfileFromRegistration",
    "CreateRegistrationsFromBrandProfile",
    "UpdateBrandProfileFromRegistration",
    "UpdateRegistrationsFromBrandProfile",
];

/// The `JobStatus` of a job whose work has not finished yet, as
/// [`crate::ops::types::JOB_STATUS_VALUES`] spells it.
pub const PROCESSING_JOB_STATUS: &str = "PROCESSING";

/// The `JobStatus` of a job that finished its work.
pub const SUCCESS_JOB_STATUS: &str = "SUCCESS";

/// The `JobStatus` of a job whose work did not finish.
pub const FAILED_JOB_STATUS: &str = "FAILED";

/// How long after the call that started it a job reaches its terminal status.
///
/// The API documents no duration for this, so it is the span this emulator chooses: a
/// span short enough that a caller polling `GetJob` or `ListJobs` sees the job finish,
/// and long enough that a caller reading immediately after the call that started the
/// job still observes the `PROCESSING` the API reports it in.
pub const JOB_SETTLEMENT_DELAY: Duration = Duration::from_secs(3);

/// The member a refused `operationType` arrived on, as this service's own gates name a
/// member at fault in `ValidationExceptionField$path`: by the member's own name.
const OPERATION_TYPE_MEMBER: &str = "operationType";

/// The terminal state the clock owes a job that is still `PROCESSING`: when it comes
/// due, and what the job reports once it does.
///
/// Held beside the job's current status rather than applied at once, because the API
/// reports an accepted job as `PROCESSING` and only then as finished; it is a change
/// the clock makes due at [`Self::due_at`] whether or not anybody asks for it.
#[derive(Debug, Clone)]
pub struct JobSettlement {
    /// The instant this job's terminal status becomes due.
    pub due_at: SystemTime,
    /// The `JobStatus` the job reports from then on.
    pub status: String,
    /// `errorCode`, which a job reports only once it has failed.
    pub error_code: Option<String>,
    /// `errorMessage`, which a job reports only once it has failed.
    pub error_message: Option<String>,
    /// The resources the job created or updated, which it reports once it has
    /// finished.
    pub resources: Vec<JobResource>,
}

impl JobSettlement {
    /// The settlement of a job that finishes its work, carrying the resources it
    /// created or updated.
    pub fn succeeds_at(due_at: SystemTime, resources: Vec<JobResource>) -> Self {
        Self {
            due_at,
            status: SUCCESS_JOB_STATUS.to_string(),
            error_code: None,
            error_message: None,
            resources,
        }
    }

    /// The settlement of a job whose work does not finish, carrying the machine-readable
    /// code and the human-readable description the API reports only for a failed job.
    pub fn fails_at(due_at: SystemTime, error_code: &str, error_message: &str) -> Self {
        Self {
            due_at,
            status: FAILED_JOB_STATUS.to_string(),
            error_code: Some(error_code.to_string()),
            error_message: Some(error_message.to_string()),
            resources: Vec::new(),
        }
    }
}

/// One asynchronous job: everything the job reports carry.
#[derive(Debug, Clone)]
pub struct Job {
    /// The minted `jobId` this record is keyed by.
    pub id: String,
    /// `operationType` — one of [`JOB_OPERATION_TYPE_VALUES`], the operation that
    /// created this job.
    pub operation_type: String,
    /// `brandProfileId` — the profile the job operates on, absent for a job that
    /// creates one.
    pub brand_profile_id: Option<String>,
    /// `status`, one of [`crate::ops::types::JOB_STATUS_VALUES`].
    pub status: String,
    /// `errorCode`, present only once the job has failed.
    pub error_code: Option<String>,
    /// `errorMessage`, present only once the job has failed.
    pub error_message: Option<String>,
    /// The resources the job created or updated. Empty until the job has finished,
    /// and reported as absent while it is, since `JobResourceList` is modelled
    /// `@length(1..10)`.
    pub resources: Vec<JobResource>,
    pub created_at: SystemTime,
    pub updated_at: SystemTime,
    /// The terminal status the clock still owes this job, or `None` for a job that
    /// has already reached one.
    pub settlement: Option<JobSettlement>,
}

impl Job {
    /// A job the service has just accepted: `PROCESSING` as of `at`, settling into
    /// `settlement` when the clock makes that due.
    pub fn accepted(
        id: &str,
        operation_type: &str,
        brand_profile_id: Option<&str>,
        at: SystemTime,
        settlement: JobSettlement,
    ) -> Self {
        Self {
            id: id.to_string(),
            operation_type: operation_type.to_string(),
            brand_profile_id: brand_profile_id.map(str::to_string),
            status: PROCESSING_JOB_STATUS.to_string(),
            error_code: None,
            error_message: None,
            resources: Vec::new(),
            created_at: at,
            updated_at: at,
            settlement: Some(settlement),
        }
    }

    /// Apply the terminal status the clock has made due by `now`, if any.
    ///
    /// The job is reported as having changed when the transition came due, not when
    /// something got around to reading it, so `updated_at` is the instant it was owed.
    fn settle(&mut self, now: SystemTime) {
        if self
            .settlement
            .as_ref()
            .is_none_or(|settlement| now < settlement.due_at)
        {
            return;
        }
        let Some(settlement) = self.settlement.take() else {
            return;
        };
        self.status = settlement.status;
        self.error_code = settlement.error_code;
        self.error_message = settlement.error_message;
        self.resources = settlement.resources;
        self.updated_at = settlement.due_at;
    }
}

impl ComAmazonawsEndusermessagingEndusermessagingState {
    /// Mint a fresh, unused `jobId`.
    ///
    /// `JobId` documents an opaque service-generated identifier, so it takes the same
    /// shape as every other id this state hands out — a resource prefix plus
    /// clock-seeded and counted hex digits — which satisfies that shape's
    /// `@length(1..64)` and `^[A-Za-z0-9_-]+$` pattern. The loop re-draws rather than
    /// ever handing back an id already in use.
    pub fn mint_job_id(&mut self) -> String {
        loop {
            let id = self.next_candidate_id("job");
            if !self.jobs.contains_key(&id) {
                return id;
            }
        }
    }

    /// Store `job`, dropping the oldest job first if that would push the store past
    /// [`MAX_JOBS`].
    ///
    /// The ceiling is an invariant of the map rather than of its callers, so no number
    /// of asynchronous calls can grow the emulator's heap without bound.
    pub fn record_job(&mut self, job: Job) {
        while self.jobs.len() >= MAX_JOBS {
            let oldest = self
                .jobs
                .values()
                .min_by_key(|stored| (stored.created_at, stored.id.clone()))
                .map(|stored| stored.id.clone());
            match oldest {
                Some(id) => {
                    self.jobs.remove(&id);
                }
                None => break,
            }
        }
        self.jobs.insert(job.id.clone(), job);
    }

    /// Apply the terminal status the clock has made due by `now` to every stored job.
    ///
    /// Owed by the clock alone — each transition was due at its own instant whether or
    /// not anybody called — so materialising it is what lets every operation read one
    /// store in which no job is still reported as `PROCESSING` after its work was due
    /// to finish.
    pub fn settle_jobs(&mut self, now: SystemTime) {
        for job in self.jobs.values_mut() {
            job.settle(now);
        }
    }
}

/// Whether `operation_type` names an operation of this service that starts a job, as
/// [`JOB_OPERATION_TYPE_VALUES`] lists them.
pub fn is_job_operation_type(operation_type: &str) -> bool {
    JOB_OPERATION_TYPE_VALUES.contains(&operation_type)
}

/// `ValidationException` for an `operationType` that is not one this service runs jobs
/// for.
///
/// `JobOperationType` declares no value set, so the gate is the service's own rather
/// than a modelled constraint, and this API states such a refusal as a sentence of its
/// own — the value it refused, in single quotes, and the set the value had to come from
/// — rather than in the boundary validator's constraint envelope, which states a
/// modelled constraint this value does not break. The fixed text is this refusal's; the
/// variable parts are the value the request gave and the supported set. `fieldList`
/// names the member at fault by its own name, as this service's own gates report one.
pub fn invalid_job_operation_type(operation_type: &str) -> crate::ops::types::ValidationException {
    let stated = format!(
        "Invalid operationType '{operation_type}'. Supported values: {}",
        JOB_OPERATION_TYPE_VALUES.join(", ")
    );
    crate::ops::types::ValidationException {
        message: stated.clone(),
        field_list: Some(vec![crate::ops::types::ValidationExceptionField {
            path: OPERATION_TYPE_MEMBER.to_string(),
            message: stated,
        }]),
    }
}

/// A stored job as the modelled `JobSummary` the wire carries.
///
/// Every projection of a stored job goes through here, so one job has one wire form
/// whichever operation reports it. `resources` is reported as absent rather than as an
/// empty list, because `JobResourceList` is modelled `@length(1..10)`: a job that has
/// created nothing yet has no list to report.
pub fn job_summary(job: &Job) -> JobSummary {
    JobSummary {
        job_id: job.id.clone(),
        status: job.status.clone(),
        operation_type: job.operation_type.clone(),
        created_at: super::timestamp(job.created_at),
        updated_at: super::timestamp(job.updated_at),
        brand_profile_id: job.brand_profile_id.clone(),
        error_code: job.error_code.clone(),
        error_message: job.error_message.clone(),
        resources: (!job.resources.is_empty()).then(|| job.resources.clone()),
    }
}
