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
///
/// Listed in the order this API states them in the refusal of any other value
/// ([`invalid_job_operation_type`]): the pair that derives registrations from a brand
/// profile, then the pair that derives a brand profile from a registration, the create of
/// each before its update. Membership is all any other reader asks of the set.
pub const JOB_OPERATION_TYPE_VALUES: &[&str] = &[
    "CreateRegistrationsFromBrandProfile",
    "UpdateRegistrationsFromBrandProfile",
    "CreateBrandProfileFromRegistration",
    "UpdateBrandProfileFromRegistration",
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

/// The `resourceType` a job reports for a brand profile it created or updated: a member
/// of [`crate::ops::types::JOB_RESOURCE_TYPE_VALUES`], the enum `JobResource$resourceType`
/// targets — not the hyphenated resource vocabulary a refusal names a resource by.
pub const BRAND_PROFILE_JOB_RESOURCE_TYPE: &str = "BRAND_PROFILE";

/// The `errorCode` a job that could not reach a resource it was to operate on reports.
///
/// `JobErrorCode` is "a machine-readable code identifying why a job failed" and is modelled
/// as a plain string with no value set, so the code names the error CLASS the failure
/// belongs to — the modelled error a synchronous operation refusing the same request would
/// answer with — rather than a token of its own.
pub const RESOURCE_NOT_FOUND_JOB_ERROR_CODE: &str = "ResourceNotFoundException";

/// The `errorCode` a job whose work would push a store past one of this service's ceilings
/// reports, in that same vocabulary.
pub const SERVICE_QUOTA_EXCEEDED_JOB_ERROR_CODE: &str = "ServiceQuotaExceededException";

/// What performing a job's work produced: the terminal status the job reports from then on,
/// and what it reports beside it.
///
/// One shape whether the outcome was decided when the job was accepted or computed by the
/// work itself, so a job report states it the same way either way.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobOutcome {
    /// The `JobStatus` the job reports from then on: terminal by construction, since both
    /// constructors below name a terminal status.
    pub status: String,
    /// `errorCode`, which a job reports only once it has failed.
    pub error_code: Option<String>,
    /// `errorMessage`, which a job reports only once it has failed.
    pub error_message: Option<String>,
    /// The resources the job created or updated, which it reports once it has finished.
    pub resources: Vec<JobResource>,
}

impl JobOutcome {
    /// The outcome of a job that finished its work, carrying the resources it created or
    /// updated.
    #[must_use]
    pub fn succeeded(resources: Vec<JobResource>) -> Self {
        Self {
            status: SUCCESS_JOB_STATUS.to_string(),
            error_code: None,
            error_message: None,
            resources,
        }
    }

    /// The outcome of a job whose work did not finish, carrying the machine-readable code
    /// and the human-readable description the API reports only for a failed job.
    #[must_use]
    pub fn failed(error_code: &str, error_message: &str) -> Self {
        Self {
            status: FAILED_JOB_STATUS.to_string(),
            error_code: Some(error_code.to_string()),
            error_message: Some(error_message.to_string()),
            resources: Vec::new(),
        }
    }
}

/// What a job does when the clock makes its transition due, and what it then reports.
///
/// The work is recorded rather than performed by the call that started the job, because
/// such an operation "runs asynchronously": the API reports the job as `PROCESSING` and the
/// change it makes becomes visible when the job finishes. Performing it is therefore owed
/// by the clock alone ([`ComAmazonawsEndusermessagingEndusermessagingState::settle_jobs`]),
/// at the instant it came due, whether or not anybody polled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobWork {
    /// Nothing to perform: the job reports the outcome that was decided when it was
    /// accepted.
    Decided(JobOutcome),
    /// Import the fields of a registration into a brand profile's attributes, as
    /// `UpdateBrandProfileFromRegistration` asks
    /// ([`ComAmazonawsEndusermessagingEndusermessagingState::import_registration_into_brand_profile`]).
    ImportRegistration {
        /// The `brandProfileId` the fields are imported into.
        brand_profile_id: String,
        /// The `registrationId` they are imported from.
        registration_id: String,
        /// Whether a registration value overwrites an attribute value already there, which
        /// is what `onAttributeConflict` chose.
        replace_existing_values: bool,
        /// `smartMatch`, which decides which attributes map at all.
        smart_match: bool,
    },
}

/// The terminal state the clock owes a job that is still `PROCESSING`: when it comes
/// due, and what the job does and reports once it does.
///
/// Held beside the job's current status rather than applied at once, because the API
/// reports an accepted job as `PROCESSING` and only then as finished; it is a change
/// the clock makes due at [`Self::due_at`] whether or not anybody asks for it. It stays on
/// the record after it has been applied — the job's own `status` is terminal from then on —
/// because the work it names is also what a repeat of the `clientToken` that started the
/// job is compared against.
#[derive(Debug, Clone)]
pub struct JobSettlement {
    /// The instant this job's terminal status becomes due.
    pub due_at: SystemTime,
    /// What the job does when `due_at` arrives, and what it reports from then on.
    pub work: JobWork,
}

impl JobSettlement {
    /// The settlement of a job that finishes its work, carrying the resources it
    /// created or updated.
    pub fn succeeds_at(due_at: SystemTime, resources: Vec<JobResource>) -> Self {
        Self {
            due_at,
            work: JobWork::Decided(JobOutcome::succeeded(resources)),
        }
    }

    /// The settlement of a job whose work does not finish, carrying the machine-readable
    /// code and the human-readable description the API reports only for a failed job.
    pub fn fails_at(due_at: SystemTime, error_code: &str, error_message: &str) -> Self {
        Self {
            due_at,
            work: JobWork::Decided(JobOutcome::failed(error_code, error_message)),
        }
    }

    /// The settlement of a job whose terminal status is whatever performing `work`
    /// produces, due `JOB_SETTLEMENT_DELAY` after the call that started it.
    pub fn performs_at(due_at: SystemTime, work: JobWork) -> Self {
        Self { due_at, work }
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
    /// The identifier from the request this job processes, as
    /// `JobResult$resourceIdentifier` reports it — "the identifier from your request that
    /// this job is processing" — kept as the request spelled it. `None` for a job no
    /// request of this emulator started.
    pub resource_identifier: Option<String>,
    /// The `clientToken` the request that started this job carried, when it carried one.
    /// Kept on the record rather than in a map beside it, so the token lives and dies with
    /// the job it names and is bounded by the same ceiling.
    pub client_token: Option<String>,
    /// The transition the clock owes this job: when it comes due, and what the job does
    /// and reports then.
    pub settlement: JobSettlement,
}

/// What a request that starts a job asks of it, beside the instant it was accepted at.
///
/// One argument rather than six, so a call that starts a job names each of them.
#[derive(Debug)]
pub struct StartedJob<'a> {
    /// `operationType` — the operation starting the job, one of
    /// [`JOB_OPERATION_TYPE_VALUES`].
    pub operation_type: &'a str,
    /// The profile the job operates on.
    pub brand_profile_id: &'a str,
    /// The identifier from the request this job processes, as
    /// `JobResult$resourceIdentifier` reports it.
    pub resource_identifier: &'a str,
    /// The request's `clientToken`, when it carried one.
    pub client_token: Option<&'a str>,
    /// What the job does when the clock makes its transition due.
    pub work: JobWork,
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
            resource_identifier: None,
            client_token: None,
            settlement,
        }
    }

    /// A job the service has just accepted from a request: `PROCESSING` as of `at`,
    /// performing the work that request asked for — and reporting what that produced —
    /// when the clock makes the transition due, [`JOB_SETTLEMENT_DELAY`] later.
    pub fn started(id: &str, at: SystemTime, started: StartedJob<'_>) -> Self {
        let mut job = Self::accepted(
            id,
            started.operation_type,
            Some(started.brand_profile_id),
            at,
            JobSettlement::performs_at(at + JOB_SETTLEMENT_DELAY, started.work),
        );
        job.resource_identifier = Some(started.resource_identifier.to_string());
        job.client_token = started.client_token.map(str::to_string);
        job
    }
}

impl ComAmazonawsEndusermessagingEndusermessagingState {
    /// Mint a fresh, unused `jobId`.
    ///
    /// `JobId` documents an opaque service-generated identifier, and the model's own
    /// example writes one as a `job-` prefix followed by a body of
    /// [`super::MINTED_ID_BODY_LENGTH`] alphanumerics (`job-abc12345678901234`); the
    /// body is drawn from fresh entropy ([`super::minted_id`]) and satisfies that
    /// shape's `@length(1..64)` and `^[A-Za-z0-9_-]+$` pattern. The loop re-draws
    /// rather than ever handing back an id already in use.
    pub fn mint_job_id(&mut self) -> String {
        loop {
            let id = super::minted_id("job");
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

    /// The job a request carrying `client_token` started, or `None` when this state has no
    /// record of that token.
    ///
    /// `clientToken` is `@idempotencyToken`, so a repeat of a request already served
    /// reports the job that request started instead of starting a second one. The scan is
    /// over a map bounded by [`MAX_JOBS`].
    pub fn job_started_by(&self, client_token: &str) -> Option<&Job> {
        self.jobs
            .values()
            .find(|job| job.client_token.as_deref() == Some(client_token))
    }

    /// Perform the work, and apply the terminal status, the clock has made due by `now`
    /// for every stored job.
    ///
    /// Owed by the clock alone — each transition was due at its own instant whether or
    /// not anybody called — so materialising it is what lets every operation read one
    /// store in which no job is still reported as `PROCESSING` after its work was due
    /// to finish, and none of that work is still outstanding.
    ///
    /// A job's work is performed once: the job's own `status` is terminal from then on, and
    /// only a job still reported as `PROCESSING` is due for anything. The jobs are taken in
    /// the order their transitions came due, so two whose work touches one resource land in
    /// the order the clock owed them rather than in the order their ids sort.
    pub fn settle_jobs(&mut self, now: SystemTime) {
        let mut due: Vec<(SystemTime, String)> = self
            .jobs
            .iter()
            .filter(|(_, job)| {
                job.status == PROCESSING_JOB_STATUS && job.settlement.due_at <= now
            })
            .map(|(id, job)| (job.settlement.due_at, id.clone()))
            .collect();
        due.sort();
        for (due_at, id) in due {
            let Some(work) = self.jobs.get(&id).map(|job| job.settlement.work.clone()) else {
                continue;
            };
            let outcome = match work {
                JobWork::Decided(outcome) => outcome,
                JobWork::ImportRegistration {
                    brand_profile_id,
                    registration_id,
                    replace_existing_values,
                    smart_match,
                } => self.import_registration_into_brand_profile(
                    &brand_profile_id,
                    &registration_id,
                    replace_existing_values,
                    smart_match,
                    due_at,
                ),
            };
            let Some(job) = self.jobs.get_mut(&id) else {
                continue;
            };
            job.status = outcome.status;
            job.error_code = outcome.error_code;
            job.error_message = outcome.error_message;
            job.resources = outcome.resources;
            // The job is reported as having changed when the transition came due, not when
            // something got around to reading it.
            job.updated_at = due_at;
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
/// own — the member it is about, the value it refused, and the set that value had to come
/// from, the four operations pipe-separated in the order
/// [`JOB_OPERATION_TYPE_VALUES`] lists them — rather than in the boundary validator's
/// constraint envelope, which states a modelled constraint this value does not break.
/// The fixed text is this refusal's; its one variable part is the value the request gave.
/// `fieldList` names the member at fault by its own name, as this service's own gates
/// report one.
pub fn invalid_job_operation_type(operation_type: &str) -> crate::ops::types::ValidationException {
    let stated = format!(
        "Invalid value for '{OPERATION_TYPE_MEMBER}': unknown operation type: \
         {operation_type}. Valid values: {}",
        JOB_OPERATION_TYPE_VALUES.join(" | ")
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
