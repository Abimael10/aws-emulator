// SPDX-License-Identifier: CC0-1.0

//! The classification-job plane of this service's state: the sensitive data
//! discovery jobs `CreateClassificationJob` mints,
//! `DescribeClassificationJob` projects and `UpdateClassificationJob` moves
//! between statuses, together with the bounds their members are documented
//! with and the values the service applies to a member a request omits.
//!
//! Everything a read reports about a job is a field of [`ClassificationJob`] or
//! projected from one, so no response member is computed from the request that
//! asked for it.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::{JobScheduleFrequency, S3JobDefinition, Statistics, UserPausedDetails};

/// The resource-type segment of a classification job's ARN, read from the only
/// Macie ARN grammar the model pins (`CreateAllowListResponse$arn`) with this
/// resource's own type in place of `allow-list`.
pub const CLASSIFICATION_JOB_RESOURCE_TYPE: &str = "classification-job";

/// The `JobType` of a job that "runs only once", per the member's doc.
/// An entry of [`crate::ops::types::JOB_TYPE_VALUES`].
pub const JOB_TYPE_ONE_TIME: &str = "ONE_TIME";

/// The `JobType` of a job that "runs on a daily, weekly, or monthly basis",
/// per the same doc. An entry of [`crate::ops::types::JOB_TYPE_VALUES`].
pub const JOB_TYPE_SCHEDULED: &str = "SCHEDULED";

/// The `JobStatus` of a job with a run in progress: the status doc states
/// "RUNNING - For a one-time job, the job is in progress. For a recurring job,
/// a scheduled run is in progress." An entry of
/// [`crate::ops::types::JOB_STATUS_VALUES`].
pub const JOB_STATUS_RUNNING: &str = "RUNNING";

/// The `JobStatus` of a recurring job with no run in progress and a scheduled
/// run pending: the status doc states "IDLE - For a recurring job, the previous
/// scheduled run is complete and the next scheduled run is pending. This value
/// doesn't apply to one-time jobs." An entry of
/// [`crate::ops::types::JOB_STATUS_VALUES`].
pub const JOB_STATUS_IDLE: &str = "IDLE";

/// The `JobStatus` of a one-time job whose run has finished: the status doc
/// states "COMPLETE - For a one-time job, Amazon Macie finished processing the
/// data specified for the job. This value doesn't apply to recurring jobs." An
/// entry of [`crate::ops::types::JOB_STATUS_VALUES`].
pub const JOB_STATUS_COMPLETE: &str = "COMPLETE";

/// The `JobStatus` of a job the caller paused temporarily: the status doc
/// states "USER_PAUSED - You paused the job. If you paused the job while it had
/// a status of RUNNING and you don't resume it within 30 days of pausing it,
/// the job or job run will expire and be cancelled". An entry of
/// [`crate::ops::types::JOB_STATUS_VALUES`].
pub const JOB_STATUS_USER_PAUSED: &str = "USER_PAUSED";

/// The `JobStatus` of a job the caller cancelled, which is terminal: the
/// `jobStatus` member doc of `UpdateClassificationJobRequest` states "CANCELLED
/// - Stops the job permanently and cancels it … You can't resume or restart a
/// job after you cancel it." An entry of
/// [`crate::ops::types::JOB_STATUS_VALUES`].
pub const JOB_STATUS_CANCELLED: &str = "CANCELLED";

/// The `JobStatus` of a job Macie itself paused because further processing
/// would exceed an account's monthly sensitive data discovery quota: the status
/// doc states "PAUSED - Macie started running the job but additional processing
/// would exceed the monthly sensitive data discovery quota". An entry of
/// [`crate::ops::types::JOB_STATUS_VALUES`]. Nothing in this emulator moves a
/// job to it — it is named because it is one of the current statuses a
/// requested status is valid FROM
/// ([`statuses_a_requested_status_is_valid_from`]).
pub const JOB_STATUS_PAUSED: &str = "PAUSED";

/// The statuses a request may ask a job to move to, each with the current
/// statuses it is valid from, read off the `jobStatus` member doc of
/// `UpdateClassificationJobRequest`: "CANCELLED … is valid only if the job's
/// current status is IDLE, PAUSED, RUNNING, or USER_PAUSED", "RUNNING - Resumes
/// the job. This value is valid only if the job's current status is
/// USER_PAUSED" and "USER_PAUSED - Pauses the job temporarily. This value is
/// valid only if the job's current status is IDLE, PAUSED, or RUNNING."
///
/// The statuses the doc lists no transition for — the ones the SERVICE moves a
/// job to as a run ends or a quota is reached — are absent, so a request asking
/// for one of those is refused.
pub const REQUESTABLE_JOB_STATUSES: [(&str, &[&str]); 3] = [
    (
        JOB_STATUS_CANCELLED,
        &[
            JOB_STATUS_IDLE,
            JOB_STATUS_PAUSED,
            JOB_STATUS_RUNNING,
            JOB_STATUS_USER_PAUSED,
        ],
    ),
    (JOB_STATUS_RUNNING, &[JOB_STATUS_USER_PAUSED]),
    (
        JOB_STATUS_USER_PAUSED,
        &[JOB_STATUS_IDLE, JOB_STATUS_PAUSED, JOB_STATUS_RUNNING],
    ),
];

/// The statuses in which a job is done with the resources it is configured to
/// use: `DeleteAllowListRequest$ignoreJobChecks` singles out jobs that "have a
/// status other than COMPLETE or CANCELLED", so these two are the statuses a
/// job's hold on an allow list does not survive. Entries of
/// [`crate::ops::types::JOB_STATUS_VALUES`].
pub const TERMINAL_JOB_STATUSES: [&str; 2] = [JOB_STATUS_COMPLETE, JOB_STATUS_CANCELLED];

/// The current statuses `requested` is a valid new status from, or `None` when
/// a request may not ask for `requested` at all. The one reading of
/// [`REQUESTABLE_JOB_STATUSES`].
pub fn statuses_a_requested_status_is_valid_from(requested: &str) -> Option<&'static [&'static str]> {
    REQUESTABLE_JOB_STATUSES
        .iter()
        .find(|(status, _)| *status == requested)
        .map(|(_, valid_from)| *valid_from)
}

/// How long a paused job or job run stays paused before it expires and the
/// service cancels it. A duration the service DOCUMENTS, so it keeps its value:
/// the `jobStatus` member doc of `UpdateClassificationJobRequest` states "If you
/// pause a one-time job and you don't resume it within 30 days, the job expires
/// and Macie cancels the job. If you pause a recurring job when its status is
/// RUNNING and you don't resume it within 30 days, the job run expires and Macie
/// cancels the run."
pub const PAUSE_EXPIRES_AFTER: std::time::Duration =
    std::time::Duration::from_secs(30 * 24 * 60 * 60);

/// How long a run this service has started stays in progress before the clock
/// owes its end: a one-time job's run finishes (COMPLETE) and a recurring job's
/// run leaves the job waiting for its next scheduled one (IDLE).
///
/// The span is the emulator's, not a duration AWS documents — a real run takes
/// as long as the data it analyzes. It is never zero, so a caller acting
/// straight after the create still observes the run in progress, and the end is
/// a DUE INSTANT read off the clock rather than anything this process waits
/// for.
pub const RUN_SETTLES_AFTER: std::time::Duration = std::time::Duration::from_secs(3);

/// The widest `name` a job may carry: its member doc states "The name can
/// contain as many as 500 characters."
pub const MAX_JOB_NAME_CHARS: usize = 500;

/// The widest `description` a job may carry: its member doc states "The
/// description can contain as many as 200 characters."
pub const MAX_JOB_DESCRIPTION_CHARS: usize = 200;

/// The smallest `samplingPercentage` a job may carry. The member is "the
/// sampling depth, as a percentage", so the value it accepts is a percentage.
pub const MIN_SAMPLING_PERCENTAGE: i32 = 0;

/// The largest `samplingPercentage` a job may carry, from the same sentence as
/// [`MIN_SAMPLING_PERCENTAGE`]; the member doc reads a value of 100 as the whole
/// of the eligible data ("If this value is less than 100, Amazon Macie selects
/// the objects to analyze at random").
pub const MAX_SAMPLING_PERCENTAGE: i32 = 100;

/// The `samplingPercentage` a job takes when the request names none: the member
/// doc describes a value below 100 as analyzing a random sample "up to the
/// specified percentage", so a job that asks for no shallower depth analyzes
/// every eligible object — a sampling depth of
/// [`MAX_SAMPLING_PERCENTAGE`] percent.
pub const DEFAULT_SAMPLING_PERCENTAGE: i32 = MAX_SAMPLING_PERCENTAGE;

/// The earliest `dayOfMonth` a monthly schedule may name: its member doc states
/// "This value can be an integer from 1 through 31."
pub const MIN_DAY_OF_MONTH: i32 = 1;

/// The latest `dayOfMonth` a monthly schedule may name, from the same sentence
/// as [`MIN_DAY_OF_MONTH`].
pub const MAX_DAY_OF_MONTH: i32 = 31;

/// The most tags one job may carry: the `tags` member doc states "A job can
/// have a maximum of 50 tags."
pub const MAX_JOB_TAGS: usize = 50;

/// The widest tag key: the same doc sentence continues "The maximum length of a
/// tag key is 128 characters."
pub const MAX_TAG_KEY_CHARS: usize = 128;

/// The widest tag value: "The maximum length of a tag value is 256
/// characters."
pub const MAX_TAG_VALUE_CHARS: usize = 256;

/// The `ManagedDataIdentifierSelector` a job takes when the request names none:
/// the member doc states "RECOMMENDED (default) - Use the recommended set of
/// managed data identifiers" and "If you don't specify a value for this
/// property, the job uses the recommended set of managed data identifiers". An
/// entry of [`crate::ops::types::MANAGED_DATA_IDENTIFIER_SELECTOR_VALUES`].
pub const DEFAULT_MANAGED_DATA_IDENTIFIER_SELECTOR: &str = "RECOMMENDED";

/// The selection types a job's `managedDataIdentifierIds` apply to, which is
/// also the only case a read reports that list for: its member doc states
/// "This value is null if the job's managed data identifier selection type is
/// ALL, NONE, or RECOMMENDED", leaving the two types that name identifiers
/// explicitly. Entries of
/// [`crate::ops::types::MANAGED_DATA_IDENTIFIER_SELECTOR_VALUES`].
pub const SELECTORS_NAMING_MANAGED_DATA_IDENTIFIERS: [&str; 2] = ["EXCLUDE", "INCLUDE"];

/// The selection type that uses no managed data identifier at all: "NONE -
/// Don't use any managed data identifiers."
pub const MANAGED_DATA_IDENTIFIER_SELECTOR_NONE: &str = "NONE";

/// The selection types that name no managed data identifier: the member doc
/// states, of ALL, NONE and RECOMMENDED alike, "If you specify this value,
/// don't specify any values for the managedDataIdentifierIds property".
pub const SELECTORS_NAMING_NO_MANAGED_DATA_IDENTIFIERS: [&str; 3] = ["ALL", "NONE", "RECOMMENDED"];

/// The most classification jobs one emulator process stores, across every
/// region. The EMULATOR's own memory bound, not an AWS quota: without it any
/// caller could grow one process's heap without limit. A create past it answers
/// the `ServiceQuotaExceededException` the operation models.
pub const MAX_CLASSIFICATION_JOBS: usize = 10_000;

/// The most list entries one job's settings may carry in total — the allow
/// list, custom and managed data identifier ids it names, the accounts and
/// buckets it analyzes, and the criteria terms and values that scope it.
///
/// The EMULATOR's own bound, on the same footing as
/// [`MAX_CLASSIFICATION_JOBS`]: the stored settings of a job are as large as
/// the request that defined it, so bounding the number of jobs alone would
/// leave one caller able to grow the heap without limit. Well above the number
/// of accounts, buckets and identifiers one job names.
pub const MAX_JOB_LIST_ENTRIES: usize = 10_000;

/// A stored classification job.
///
/// Every member of `DescribeClassificationJobResponse` is a field here or
/// projected from one.
#[derive(Debug, Clone)]
pub struct ClassificationJob {
    pub job_id: String,
    pub job_arn: String,
    /// The region the create that minted this job addressed. Macie is a
    /// regional service, so a job is only visible to a request naming the
    /// region it was created in — which is also the region in its ARN.
    pub region: String,
    /// The `clientToken` the create carried, which a read reports as the token
    /// "that was provided to ensure the idempotency of the request to create
    /// the job".
    pub client_token: String,
    pub name: String,
    pub description: Option<String>,
    /// `JobType` as the wire words it: an entry of
    /// [`crate::ops::types::JOB_TYPE_VALUES`].
    pub job_type: String,
    /// `JobStatus` as the wire words it: an entry of
    /// [`crate::ops::types::JOB_STATUS_VALUES`].
    pub job_status: String,
    /// Whether a recurring job was configured to analyze all existing, eligible
    /// objects immediately after it was created. Always `false` for a one-time
    /// job: the output member's doc states "This value is also false for a
    /// one-time job."
    pub initial_run: bool,
    pub allow_list_ids: Vec<String>,
    pub custom_data_identifier_ids: Vec<String>,
    pub managed_data_identifier_ids: Vec<String>,
    /// `ManagedDataIdentifierSelector` as the wire words it. Always stored,
    /// because the member carries a documented default
    /// ([`DEFAULT_MANAGED_DATA_IDENTIFIER_SELECTOR`]) that the service applies
    /// when the request names none.
    pub managed_data_identifier_selector: String,
    /// The buckets the job analyzes and the scope of that analysis, as the
    /// create defined them.
    pub s3_job_definition: S3JobDefinition,
    /// Always stored, for the same reason as
    /// [`Self::managed_data_identifier_selector`]:
    /// [`DEFAULT_SAMPLING_PERCENTAGE`] is the depth a job runs at when the
    /// request asks for no shallower one.
    pub sampling_percentage: i32,
    /// The recurrence pattern of a recurring job, and `None` for a one-time
    /// job: the output member's doc states "This value is null if the job is
    /// configured to run only once."
    pub schedule_frequency: Option<JobScheduleFrequency>,
    /// Tags in a `BTreeMap` so a read reports them in a stable order.
    pub tags: BTreeMap<String, String>,
    pub created_at: SystemTime,
    /// When the job's most recent run STARTED, and `None` while no run has
    /// started.
    pub last_run_started_at: Option<SystemTime>,
    /// When the run [`Self::last_run_started_at`] began is OWED its end, which
    /// is [`RUN_SETTLES_AFTER`] past that instant. `None` while no run has
    /// started. [`Self::status_at`] is what reports the status the clock owes
    /// once this instant has passed.
    pub run_settles_at: Option<SystemTime>,
    /// How many runs of this job the service has started, which is what
    /// `Statistics$numberOfRuns` reports.
    pub runs_started: u32,
    /// When the CALLER paused this job (`UpdateClassificationJob` with a
    /// `jobStatus` of [`JOB_STATUS_USER_PAUSED`]), which is what
    /// `UserPausedDetails$jobPausedAt` reports. `None` while nothing has paused
    /// it.
    pub paused_at: Option<SystemTime>,
    /// When the paused job or job run is owed its expiry — `None` when no pause
    /// is in effect, and when the pause interrupted no run in progress, since
    /// then there is nothing to expire. [`PAUSE_EXPIRES_AFTER`] past
    /// [`Self::paused_at`], which is what `UserPausedDetails$jobExpiresAt`
    /// reports, and [`Self::status_at`] is what reports the status the clock
    /// owes once the instant has passed.
    pub pause_expires_at: Option<SystemTime>,
    /// How much of the run in progress was left when a pause interrupted it,
    /// and `None` when the pause interrupted no run. A resume continues "from
    /// the point where you paused the job", which is this much of the run still
    /// to go from the instant of the resume.
    pub paused_run_remaining: Option<std::time::Duration>,
}

impl ClassificationJob {
    /// Whether this job is done, as of `now`: its status is one of
    /// [`TERMINAL_JOB_STATUSES`]. Read off [`Self::status_at`], so a job whose
    /// run the CLOCK has already ended counts as done even though nothing has
    /// written that status back.
    pub fn is_done_at(&self, now: SystemTime) -> bool {
        TERMINAL_JOB_STATUSES.contains(&self.status_at(now))
    }

    /// This job's `allowListIds`, as a read reports them: the allow lists the
    /// job is configured to use, and absent when it uses none.
    pub fn allow_list_ids_output(&self) -> Option<Vec<String>> {
        if self.allow_list_ids.is_empty() {
            return None;
        }
        Some(self.allow_list_ids.clone())
    }

    /// This job's `customDataIdentifierIds`, as a read reports them. Absent
    /// when the job names none: the member's doc states "This value is null if
    /// the job is configured to use only managed data identifiers to analyze
    /// data."
    pub fn custom_data_identifier_ids_output(&self) -> Option<Vec<String>> {
        if self.custom_data_identifier_ids.is_empty() {
            return None;
        }
        Some(self.custom_data_identifier_ids.clone())
    }

    /// This job's `managedDataIdentifierIds`, as a read reports them: the
    /// identifiers the job is EXPLICITLY configured to include or exclude, and
    /// absent under every other selection type, per
    /// [`SELECTORS_NAMING_MANAGED_DATA_IDENTIFIERS`].
    pub fn managed_data_identifier_ids_output(&self) -> Option<Vec<String>> {
        if self.managed_data_identifier_ids.is_empty()
            || !SELECTORS_NAMING_MANAGED_DATA_IDENTIFIERS
                .contains(&self.managed_data_identifier_selector.as_str())
        {
            return None;
        }
        Some(self.managed_data_identifier_ids.clone())
    }

    /// This job's `tags`, as a read reports them: the modelled `TagMap` is a
    /// hash map, and absent rather than empty when the job carries none.
    pub fn tags_output(&self) -> Option<std::collections::HashMap<String, String>> {
        if self.tags.is_empty() {
            return None;
        }
        Some(
            self.tags
                .iter()
                .map(|(key, value)| (key.clone(), value.clone()))
                .collect(),
        )
    }

    /// When the job started, which is what `lastRunTime` reports: its member
    /// doc states "If the job is a recurring job, this value indicates when the
    /// most recent run started or, if the job hasn't run yet, when the job was
    /// created."
    pub fn last_run_time(&self) -> SystemTime {
        self.last_run_started_at.unwrap_or(self.created_at)
    }

    /// The job's status as of `now`: the status it is stored in, or the status
    /// the CLOCK owes it once the run it started is due to have finished.
    ///
    /// A one-time job's run leaves it [`JOB_STATUS_COMPLETE`] ("Amazon Macie
    /// finished processing the data specified for the job"), and a recurring
    /// job's run leaves it [`JOB_STATUS_IDLE`] ("the previous scheduled run is
    /// complete and the next scheduled run is pending"), so a caller that polls
    /// a job it started sees the run end instead of a job in progress for ever.
    /// Only a job whose stored status is [`JOB_STATUS_RUNNING`] is owed that
    /// transition: a status something else moved the job to is the job's status.
    ///
    /// A pause the caller has left in place past its expiry
    /// ([`Self::pause_expires_at`]) is owed its end the same way: a one-time
    /// job's pause expires with "the job expires and Macie cancels the job", so
    /// the job is [`JOB_STATUS_CANCELLED`], and a recurring job's expires with
    /// "the job run expires and Macie cancels the run", which leaves the job
    /// itself waiting for its next scheduled run — [`JOB_STATUS_IDLE`].
    pub fn status_at(&self, now: SystemTime) -> &str {
        let (due, ended) = match self.job_status.as_str() {
            JOB_STATUS_RUNNING => (self.run_settles_at, self.run_ends_as()),
            JOB_STATUS_USER_PAUSED => (self.pause_expires_at, self.pause_expires_as()),
            _ => return &self.job_status,
        };
        match due {
            Some(due) if now >= due => ended,
            _ => &self.job_status,
        }
    }

    /// The status a run of this job that reaches its end leaves it in.
    fn run_ends_as(&self) -> &'static str {
        if self.job_type == JOB_TYPE_SCHEDULED {
            JOB_STATUS_IDLE
        } else {
            JOB_STATUS_COMPLETE
        }
    }

    /// The status an expired pause of this job leaves it in: a one-time job is
    /// cancelled outright, while a recurring job loses only the run the pause
    /// interrupted and waits for its next scheduled one.
    fn pause_expires_as(&self) -> &'static str {
        if self.job_type == JOB_TYPE_SCHEDULED {
            JOB_STATUS_IDLE
        } else {
            JOB_STATUS_CANCELLED
        }
    }

    /// This job's `userPausedDetails`, as of `now`.
    ///
    /// The member "is present only if the value for jobStatus is USER_PAUSED",
    /// so it is reported exactly while the pause is in effect — and absent once
    /// the clock owes that pause its end.
    /// `jobImminentExpirationHealthEventArn` is left absent: it names "the
    /// Health event that Amazon Macie sent to notify you of the job or job
    /// run's pending expiration", and this emulator sends no such event, so
    /// there is no event ARN to report.
    pub fn user_paused_details_at(&self, now: SystemTime) -> Option<UserPausedDetails> {
        if self.status_at(now) != JOB_STATUS_USER_PAUSED {
            return None;
        }
        Some(UserPausedDetails {
            job_expires_at: self.pause_expires_at.map(super::to_date_time),
            job_imminent_expiration_health_event_arn: None,
            job_paused_at: self.paused_at.map(super::to_date_time),
        })
    }

    /// Pause this job at `now`, as a `jobStatus` of
    /// [`JOB_STATUS_USER_PAUSED`] asks: the job stops being in progress, the
    /// pause is recorded, and what was left of a run in progress is kept so a
    /// resume continues from the point the pause interrupted it. Only such a
    /// run has an expiry: the doc gives one for a paused one-time job and for a
    /// recurring job "paused when its status is RUNNING".
    ///
    /// Whether a run was in progress is read off the CLOCK, not off the stored
    /// status: a run whose end was already due at `now` is a run that has
    /// finished, so the pause interrupted nothing and nothing is owed an
    /// expiry.
    pub fn pause_at(&mut self, now: SystemTime) {
        self.paused_run_remaining = self
            .run_settles_at
            .filter(|due| *due > now)
            .map(|due| due.duration_since(now).unwrap_or_default());
        self.pause_expires_at = self
            .paused_run_remaining
            .map(|_| now + PAUSE_EXPIRES_AFTER);
        self.paused_at = Some(now);
        self.run_settles_at = None;
        self.job_status = JOB_STATUS_USER_PAUSED.to_string();
    }

    /// Resume this job at `now`, as a `jobStatus` of [`JOB_STATUS_RUNNING`]
    /// asks.
    ///
    /// A pause that interrupted a run resumes THAT run "from the point where
    /// you paused the job", so what was left of it is what is left to go and
    /// the run count does not move. A pause that interrupted no run leaves the
    /// job to resume "according to the schedule and other settings for the
    /// job", which starts a run of its own.
    pub fn resume_at(&mut self, now: SystemTime) {
        match self.paused_run_remaining.take() {
            Some(remaining) => self.run_settles_at = Some(now + remaining),
            None => {
                self.last_run_started_at = Some(now);
                self.runs_started = self.runs_started.saturating_add(1);
                self.run_settles_at = Some(now + RUN_SETTLES_AFTER);
            }
        }
        self.paused_at = None;
        self.pause_expires_at = None;
        self.job_status = JOB_STATUS_RUNNING.to_string();
    }

    /// Cancel this job, as a `jobStatus` of [`JOB_STATUS_CANCELLED`] asks:
    /// "Stops the job permanently and cancels it." No run of it is owed an end
    /// and no pause of it is owed an expiry afterwards, and the status is
    /// terminal — "You can't resume or restart a job after you cancel it."
    pub fn cancel(&mut self) {
        self.run_settles_at = None;
        self.paused_at = None;
        self.pause_expires_at = None;
        self.paused_run_remaining = None;
        self.job_status = JOB_STATUS_CANCELLED.to_string();
    }

    /// This job's `statistics`, as a read reports them.
    ///
    /// `numberOfRuns` is "the number of times that the job has run", which is
    /// the number of runs this store has started for the job: a one-time job's
    /// only run, and a recurring job's immediate first run, start when the
    /// create does, which is why such a job is reported RUNNING from that
    /// instant and reaches the end of that run at
    /// [`Self::run_settles_at`]. `approximateNumberOfObjectsToProcess` is left
    /// absent: it counts the objects "the job has yet to process during its
    /// current run", and this emulator holds no object inventory for the
    /// buckets a job addresses, so there is no such count to report.
    pub fn statistics(&self) -> Statistics {
        Statistics {
            approximate_number_of_objects_to_process: None,
            number_of_runs: Some(f64::from(self.runs_started)),
        }
    }
}

/// Mint the ARN of a classification job:
/// `arn:aws:macie2:<region>:<account>:classification-job/<jobId>`.
pub fn classification_job_arn(region: &str, job_id: &str) -> String {
    ::engine::emulator_arn(
        super::ARN_SERVICE,
        region,
        &format!("{CLASSIFICATION_JOB_RESOURCE_TYPE}/{job_id}"),
    )
}

/// How the service refuses a call addressing a classification job the addressed
/// region holds none of. The fixed text is the service's, worded as it words the
/// same refusal for a custom data identifier
/// ([`super::custom_data_identifier_not_found_message`]) with this resource's
/// own name; the only variable part is the id the request named.
pub fn classification_job_not_found_message(job_id: &str) -> String {
    format!("Classification Job not found for id: {job_id}")
}

/// How the service refuses an `UpdateClassificationJob` asking for a status the
/// job's CURRENT status may not move to.
///
/// The fixed text is the service's; the variable parts are the status the job
/// is in, as the refusal words it, and the ARN of the job the request
/// addressed.
pub fn job_status_not_updatable_message(current_status: &str, job_arn: &str) -> String {
    format!(
        "Input validation failed, cannot update {} job for job: {job_arn}",
        current_status.to_lowercase()
    )
}

/// How the service refuses an `UpdateClassificationJob` asking for a status no
/// request may ask for — one the SERVICE moves a job to as a run ends or a
/// quota is reached, which the member documents no transition for.
///
/// This is a different refusal from [`job_status_not_updatable_message`]: what
/// is wrong is the status the request asked for, not the status the job is in.
/// The variable parts are the statuses a request may ask for, which is the
/// member's own list of valid values ([`REQUESTABLE_JOB_STATUSES`]), and the
/// ARN of the job the request addressed.
pub fn job_status_not_requestable_message(job_arn: &str) -> String {
    let requestable: Vec<&str> = REQUESTABLE_JOB_STATUSES
        .iter()
        .map(|(status, _)| *status)
        .collect();
    format!(
        "Input validation failed, jobStatus must be one of {} for job: {job_arn}",
        requestable.join(", ")
    )
}

#[cfg(test)]
mod tests {
    use super::{
        ClassificationJob, DEFAULT_MANAGED_DATA_IDENTIFIER_SELECTOR, JOB_STATUS_CANCELLED,
        JOB_STATUS_COMPLETE, JOB_STATUS_IDLE, JOB_STATUS_PAUSED, JOB_STATUS_RUNNING,
        JOB_STATUS_USER_PAUSED, JOB_TYPE_ONE_TIME, JOB_TYPE_SCHEDULED,
        MANAGED_DATA_IDENTIFIER_SELECTOR_NONE, PAUSE_EXPIRES_AFTER, REQUESTABLE_JOB_STATUSES,
        RUN_SETTLES_AFTER, SELECTORS_NAMING_MANAGED_DATA_IDENTIFIERS,
        SELECTORS_NAMING_NO_MANAGED_DATA_IDENTIFIERS, TERMINAL_JOB_STATUSES,
        statuses_a_requested_status_is_valid_from,
    };
    use crate::ops::types::{
        JOB_STATUS_VALUES, JOB_TYPE_VALUES, MANAGED_DATA_IDENTIFIER_SELECTOR_VALUES,
    };
    use std::time::{Duration, SystemTime};

    /// A job to vary one member of per case.
    fn job() -> ClassificationJob {
        ClassificationJob {
            job_id: "0123456789abcdefghijkl".to_string(),
            job_arn: "arn".to_string(),
            region: "us-east-1".to_string(),
            client_token: "token".to_string(),
            name: "demo".to_string(),
            description: None,
            job_type: JOB_TYPE_ONE_TIME.to_string(),
            job_status: JOB_STATUS_RUNNING.to_string(),
            initial_run: false,
            allow_list_ids: Vec::new(),
            custom_data_identifier_ids: Vec::new(),
            managed_data_identifier_ids: Vec::new(),
            managed_data_identifier_selector: DEFAULT_MANAGED_DATA_IDENTIFIER_SELECTOR.to_string(),
            s3_job_definition: Default::default(),
            sampling_percentage: 100,
            schedule_frequency: None,
            tags: std::collections::BTreeMap::new(),
            created_at: SystemTime::UNIX_EPOCH,
            last_run_started_at: None,
            run_settles_at: None,
            runs_started: 0,
            paused_at: None,
            pause_expires_at: None,
            paused_run_remaining: None,
        }
    }

    /// A job is done with the resources it is configured to use in exactly the
    /// two statuses the allow-list delete check singles out, and in no other
    /// modelled status — and the status the CLOCK owes a finished run counts,
    /// so a one-time job whose run has settled is done without anything having
    /// written that status back.
    #[test]
    fn a_job_is_done_only_in_a_terminal_status() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(600);
        for status in JOB_STATUS_VALUES {
            let mut stored = job();
            stored.job_status = status.to_string();
            let expected = TERMINAL_JOB_STATUSES.contains(status);
            let actual = stored.is_done_at(now);
            assert_eq!(
                actual, expected,
                "case {status:?}: expected {expected:?}, got {actual:?}"
            );
        }

        for (case, job_type, run_has_ended, expected) in [
            ("a one-time run that has ended", JOB_TYPE_ONE_TIME, true, true),
            (
                "a one-time run still in progress",
                JOB_TYPE_ONE_TIME,
                false,
                false,
            ),
            (
                "a recurring run that has ended",
                JOB_TYPE_SCHEDULED,
                true,
                false,
            ),
            (
                "a recurring run still in progress",
                JOB_TYPE_SCHEDULED,
                false,
                false,
            ),
        ] {
            let mut stored = job();
            stored.job_type = job_type.to_string();
            stored.job_status = JOB_STATUS_RUNNING.to_string();
            stored.run_settles_at = Some(if run_has_ended {
                now - Duration::from_secs(1)
            } else {
                now + Duration::from_secs(1)
            });
            let actual = stored.is_done_at(now);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// Every status, type and selector this module names is a value the model
    /// declares, so none of them can drift from the wire.
    #[test]
    fn every_named_wire_value_is_one_the_model_declares() {
        for case in [
            JOB_STATUS_RUNNING,
            JOB_STATUS_IDLE,
            JOB_STATUS_COMPLETE,
            JOB_STATUS_USER_PAUSED,
            JOB_STATUS_CANCELLED,
            JOB_STATUS_PAUSED,
        ] {
            assert!(
                JOB_STATUS_VALUES.contains(&case),
                "case {case:?}: expected a modelled JobStatus, got one of {JOB_STATUS_VALUES:?}"
            );
        }
        for (requested, valid_from) in REQUESTABLE_JOB_STATUSES {
            assert!(
                JOB_STATUS_VALUES.contains(&requested),
                "case {requested:?}: expected a modelled JobStatus, got one of \
                 {JOB_STATUS_VALUES:?}"
            );
            for from in valid_from {
                assert!(
                    JOB_STATUS_VALUES.contains(from),
                    "case {requested:?} from {from:?}: expected a modelled JobStatus, got one \
                     of {JOB_STATUS_VALUES:?}"
                );
            }
        }
        for case in [JOB_TYPE_ONE_TIME, JOB_TYPE_SCHEDULED] {
            assert!(
                JOB_TYPE_VALUES.contains(&case),
                "case {case:?}: expected a modelled JobType, got one of {JOB_TYPE_VALUES:?}"
            );
        }
        let selectors = SELECTORS_NAMING_MANAGED_DATA_IDENTIFIERS
            .iter()
            .chain(SELECTORS_NAMING_NO_MANAGED_DATA_IDENTIFIERS.iter())
            .copied()
            .chain([
                DEFAULT_MANAGED_DATA_IDENTIFIER_SELECTOR,
                MANAGED_DATA_IDENTIFIER_SELECTOR_NONE,
            ]);
        for case in selectors {
            assert!(
                MANAGED_DATA_IDENTIFIER_SELECTOR_VALUES.contains(&case),
                "case {case:?}: expected a modelled ManagedDataIdentifierSelector, got one of \
                 {MANAGED_DATA_IDENTIFIER_SELECTOR_VALUES:?}"
            );
        }
        // The two partitions of the selector values are exactly the modelled
        // enum: every selector either names identifiers explicitly or names
        // none.
        assert_eq!(
            SELECTORS_NAMING_MANAGED_DATA_IDENTIFIERS.len()
                + SELECTORS_NAMING_NO_MANAGED_DATA_IDENTIFIERS.len(),
            MANAGED_DATA_IDENTIFIER_SELECTOR_VALUES.len(),
            "the two selector partitions must cover the modelled enum exactly, got \
             {MANAGED_DATA_IDENTIFIER_SELECTOR_VALUES:?}"
        );
    }

    /// `managedDataIdentifierIds` is reported only under the selection types
    /// that name identifiers explicitly, over every value the enum carries.
    #[test]
    fn managed_data_identifier_ids_are_reported_only_where_they_apply() {
        for selector in MANAGED_DATA_IDENTIFIER_SELECTOR_VALUES {
            let listed = ClassificationJob {
                managed_data_identifier_selector: (*selector).to_string(),
                managed_data_identifier_ids: vec!["PERSON_NAME".to_string()],
                ..job()
            };
            let expected = SELECTORS_NAMING_MANAGED_DATA_IDENTIFIERS.contains(selector);
            assert_eq!(
                listed.managed_data_identifier_ids_output().is_some(),
                expected,
                "case {selector:?}: expected reported={expected}, got {:?}",
                listed.managed_data_identifier_ids_output()
            );
            let none_listed = ClassificationJob {
                managed_data_identifier_selector: (*selector).to_string(),
                ..job()
            };
            assert_eq!(
                none_listed.managed_data_identifier_ids_output(),
                None,
                "case {selector:?} with no identifiers listed: expected none, got {:?}",
                none_listed.managed_data_identifier_ids_output()
            );
        }
    }

    /// `lastRunTime` is the instant the most recent run started, or the create
    /// instant while no run has started, and the run count follows the same
    /// evidence.
    #[test]
    fn a_run_that_has_not_started_reports_the_create_instant_and_no_runs() {
        let never_run = job();
        assert_eq!(
            never_run.last_run_time(),
            never_run.created_at,
            "case \"no run started\": expected the create instant, got {:?}",
            never_run.last_run_time()
        );
        assert_eq!(
            never_run.statistics().number_of_runs,
            Some(0.0),
            "case \"no run started\": expected no runs, got {:?}",
            never_run.statistics()
        );

        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(90);
        let running = ClassificationJob {
            last_run_started_at: Some(started_at),
            runs_started: 1,
            ..job()
        };
        assert_eq!(
            running.last_run_time(),
            started_at,
            "case \"a run started\": expected the run instant, got {:?}",
            running.last_run_time()
        );
        assert_eq!(
            running.statistics().number_of_runs,
            Some(1.0),
            "case \"a run started\": expected one run, got {:?}",
            running.statistics()
        );
    }

    /// The status the clock owes a job whose run has started: a one-time job's
    /// run leaves it COMPLETE and a recurring job's leaves it IDLE, from the
    /// instant that run is due to have finished and not before — while a job
    /// whose stored status is anything else keeps that status whatever the
    /// clock says.
    #[test]
    fn a_started_runs_end_is_owed_at_its_due_instant_and_not_before() {
        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(60);
        let due = started_at + RUN_SETTLES_AFTER;
        let one_time = ClassificationJob {
            last_run_started_at: Some(started_at),
            run_settles_at: Some(due),
            runs_started: 1,
            ..job()
        };
        let recurring = ClassificationJob {
            job_type: JOB_TYPE_SCHEDULED.to_string(),
            initial_run: true,
            ..one_time.clone()
        };
        // A job no run has started for: a recurring job waiting for its first
        // scheduled run keeps the status it was created in for ever.
        let waiting = ClassificationJob {
            job_type: JOB_TYPE_SCHEDULED.to_string(),
            job_status: JOB_STATUS_IDLE.to_string(),
            ..job()
        };

        let long_after = due + Duration::from_secs(86_400);
        let cases: Vec<(&str, &ClassificationJob, SystemTime, &str)> = vec![
            ("a one-time run at its start", &one_time, started_at, JOB_STATUS_RUNNING),
            (
                "a one-time run one tick before it is due to end",
                &one_time,
                due - Duration::from_nanos(1),
                JOB_STATUS_RUNNING,
            ),
            (
                "a one-time run at the instant it is due to end",
                &one_time,
                due,
                JOB_STATUS_COMPLETE,
            ),
            (
                "a one-time run long after it ended",
                &one_time,
                long_after,
                JOB_STATUS_COMPLETE,
            ),
            (
                "a recurring run at its start",
                &recurring,
                started_at,
                JOB_STATUS_RUNNING,
            ),
            (
                "a recurring run at the instant it is due to end",
                &recurring,
                due,
                JOB_STATUS_IDLE,
            ),
            (
                "a recurring job waiting for its first scheduled run",
                &waiting,
                long_after,
                JOB_STATUS_IDLE,
            ),
        ];
        for (case, job, now, expected) in cases {
            assert_eq!(
                job.status_at(now),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                job.status_at(now)
            );
        }
    }

    /// The statuses a request may ask for, and the current statuses each is
    /// valid from, are exactly the table the member documents — including the
    /// statuses that are NOT requestable at all.
    #[test]
    fn only_the_documented_statuses_are_requestable() {
        for status in JOB_STATUS_VALUES {
            let requestable = statuses_a_requested_status_is_valid_from(status);
            let expected = REQUESTABLE_JOB_STATUSES
                .iter()
                .any(|(requested, _)| requested == status);
            assert_eq!(
                requestable.is_some(),
                expected,
                "case {status:?}: expected requestable={expected}, got {requestable:?}"
            );
        }
        for case in ["", "running", "DELETED"] {
            assert!(
                statuses_a_requested_status_is_valid_from(case).is_none(),
                "case {case:?}: expected no transition, got {:?}",
                statuses_a_requested_status_is_valid_from(case)
            );
        }
    }

    /// A pause stops the run in progress, keeps what was left of it, and is
    /// owed an expiry a documented span later; a resume puts exactly that much
    /// of the run back, from the instant of the resume, without starting a
    /// second run.
    #[test]
    fn a_pause_keeps_the_rest_of_the_run_and_a_resume_continues_it() {
        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(60);
        for elapsed in [
            Duration::ZERO,
            Duration::from_millis(500),
            RUN_SETTLES_AFTER - Duration::from_millis(1),
        ] {
            let paused_at = started_at + elapsed;
            let mut job = ClassificationJob {
                last_run_started_at: Some(started_at),
                run_settles_at: Some(started_at + RUN_SETTLES_AFTER),
                runs_started: 1,
                ..job()
            };
            job.pause_at(paused_at);
            let remaining = RUN_SETTLES_AFTER.saturating_sub(elapsed);
            assert_eq!(
                (
                    job.status_at(paused_at),
                    job.paused_at,
                    job.pause_expires_at,
                    job.run_settles_at,
                    job.paused_run_remaining,
                ),
                (
                    JOB_STATUS_USER_PAUSED,
                    Some(paused_at),
                    Some(paused_at + PAUSE_EXPIRES_AFTER),
                    None,
                    Some(remaining),
                ),
                "case \"paused {elapsed:?} into the run\": expected the pause recorded with \
                 {remaining:?} of the run left, got {job:?}"
            );

            let resumed_at = paused_at + Duration::from_secs(7);
            job.resume_at(resumed_at);
            assert_eq!(
                (
                    job.status_at(resumed_at),
                    job.run_settles_at,
                    job.runs_started,
                    job.last_run_started_at,
                    job.paused_at,
                    job.user_paused_details_at(resumed_at).is_some(),
                ),
                (
                    JOB_STATUS_RUNNING,
                    Some(resumed_at + remaining),
                    1,
                    Some(started_at),
                    None,
                    false,
                ),
                "case \"resumed {elapsed:?} into the run\": expected the same run with \
                 {remaining:?} left, got {job:?}"
            );
        }
    }

    /// A pause that interrupted no run has nothing to expire, so it carries no
    /// expiry and never ends on its own — and resuming it starts a run of its
    /// own, as the job's schedule and settings ask.
    #[test]
    fn a_pause_that_interrupted_no_run_has_no_expiry() {
        let paused_at = SystemTime::UNIX_EPOCH + Duration::from_secs(60);
        let mut job = ClassificationJob {
            job_type: JOB_TYPE_SCHEDULED.to_string(),
            job_status: JOB_STATUS_IDLE.to_string(),
            ..job()
        };
        job.pause_at(paused_at);
        let long_after = paused_at + PAUSE_EXPIRES_AFTER + Duration::from_secs(86_400);
        assert_eq!(
            (
                job.pause_expires_at,
                job.paused_run_remaining,
                job.status_at(long_after)
            ),
            (None, None, JOB_STATUS_USER_PAUSED),
            "case \"a recurring job paused while idle\": expected a pause with no expiry, \
             got {job:?}"
        );
        let details = job
            .user_paused_details_at(paused_at)
            .expect("a paused job reports its pause");
        assert_eq!(
            (details.job_paused_at, details.job_expires_at),
            (Some(super::super::to_date_time(paused_at)), None),
            "case \"a recurring job paused while idle\": expected the pause instant and no \
             expiry, got {details:?}"
        );

        job.resume_at(long_after);
        assert_eq!(
            (
                job.status_at(long_after),
                job.runs_started,
                job.last_run_started_at,
                job.run_settles_at
            ),
            (
                JOB_STATUS_RUNNING,
                1,
                Some(long_after),
                Some(long_after + RUN_SETTLES_AFTER)
            ),
            "case \"resuming a job paused while idle\": expected a run of its own, got {job:?}"
        );
    }

    /// Whether a pause interrupted a run is read off the CLOCK: a job whose run
    /// has already reached its end is not in progress whatever its stored
    /// status says, so pausing it keeps no run and is owed no expiry.
    #[test]
    fn a_pause_after_the_run_ended_interrupts_nothing() {
        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(10);
        let mut settled = ClassificationJob {
            job_type: JOB_TYPE_SCHEDULED.to_string(),
            job_status: JOB_STATUS_RUNNING.to_string(),
            last_run_started_at: Some(started_at),
            run_settles_at: Some(started_at + RUN_SETTLES_AFTER),
            runs_started: 1,
            ..job()
        };
        let after_the_run = started_at + RUN_SETTLES_AFTER + Duration::from_secs(1);
        assert_eq!(
            settled.status_at(after_the_run),
            JOB_STATUS_IDLE,
            "case \"a recurring run that has ended\": expected IDLE, got {:?}",
            settled.status_at(after_the_run)
        );
        settled.pause_at(after_the_run);
        assert_eq!(
            (settled.paused_run_remaining, settled.pause_expires_at),
            (None, None),
            "case \"pausing after the run ended\": expected no run kept and no expiry, got \
             {settled:?}"
        );
    }

    /// A pause the caller leaves in place is owed its end at its expiry: a
    /// one-time job is cancelled, and a recurring job loses only the run the
    /// pause interrupted, so it waits for its next scheduled one. A cancel is
    /// terminal whatever the clock says.
    #[test]
    fn an_expired_pause_and_a_cancel_are_owed_the_documented_status() {
        let started_at = SystemTime::UNIX_EPOCH + Duration::from_secs(60);
        let mut one_time = ClassificationJob {
            last_run_started_at: Some(started_at),
            run_settles_at: Some(started_at + RUN_SETTLES_AFTER),
            runs_started: 1,
            ..job()
        };
        one_time.pause_at(started_at);
        let recurring = ClassificationJob {
            job_type: JOB_TYPE_SCHEDULED.to_string(),
            ..one_time.clone()
        };
        let mut cancelled = ClassificationJob {
            last_run_started_at: Some(started_at),
            run_settles_at: Some(started_at + RUN_SETTLES_AFTER),
            runs_started: 1,
            ..job()
        };
        cancelled.cancel();

        let due = started_at + PAUSE_EXPIRES_AFTER;
        let cases: Vec<(&str, &ClassificationJob, SystemTime, &str)> = vec![
            (
                "a one-time pause one tick before it expires",
                &one_time,
                due - Duration::from_nanos(1),
                JOB_STATUS_USER_PAUSED,
            ),
            (
                "a one-time pause at the instant it expires",
                &one_time,
                due,
                JOB_STATUS_CANCELLED,
            ),
            (
                "a recurring pause at the instant it expires",
                &recurring,
                due,
                JOB_STATUS_IDLE,
            ),
            (
                "a cancelled job long afterwards",
                &cancelled,
                due + Duration::from_secs(86_400),
                JOB_STATUS_CANCELLED,
            ),
        ];
        for (case, job, now, expected) in cases {
            assert_eq!(
                job.status_at(now),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                job.status_at(now)
            );
            assert_eq!(
                job.user_paused_details_at(now).is_some(),
                expected == JOB_STATUS_USER_PAUSED,
                "case {case:?}: userPausedDetails is reported only while the pause is in \
                 effect, got {:?}",
                job.user_paused_details_at(now)
            );
        }
        // A cancel leaves no run owed an end and no pause owed an expiry.
        assert_eq!(
            (
                cancelled.run_settles_at,
                cancelled.paused_at,
                cancelled.pause_expires_at,
                cancelled.paused_run_remaining
            ),
            (None, None, None, None),
            "case \"a cancelled job\": expected nothing owed, got {cancelled:?}"
        );
        // Nothing above moved the two paused jobs: the clock reports their end,
        // it does not rewrite them.
        for (case, job) in [("one-time", &one_time), ("recurring", &recurring)] {
            assert_eq!(
                job.job_status, JOB_STATUS_USER_PAUSED,
                "case {case:?}: expected the stored status untouched, got {job:?}"
            );
        }
    }
}
