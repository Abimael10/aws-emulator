// SPDX-License-Identifier: CC0-1.0

//! The REPORT JOB side of this service's state: the on-demand run of a stored
//! report plan `StartReportJob` begins, and what every call that names a
//! `ReportJobId` resolves against.
//!
//! # The rules this module holds
//!
//! * a report job is a run OF A STORED REPORT PLAN: the plan the start names is
//!   resolved first (`ReportPlanName` is the call's path label), so a name the
//!   account holds no plan under starts nothing
//!   ([`super::report_plan_not_found`]), and the job that IS started takes its
//!   report template and its delivery destination from that stored plan rather
//!   than from the request — the start carries neither;
//! * a started job is addressed by the `ReportJobId` this service mints for it,
//!   which is the only identifier the start answers with
//!   ([`ComAmazonawsBackupCryocontrollerusermanagerState::start_report_job`]);
//! * a repeated `IdempotencyToken` takes no second action: it answers with the
//!   job that token already started for the plan it was first used with
//!   ([`ComAmazonawsBackupCryocontrollerusermanagerState::report_job_by_idempotency_token`]),
//!   so a retried start neither runs a second report nor reports an id that
//!   differs from the one the first call answered;
//! * starting a report is an ATTEMPT of the plan, so the plan's
//!   `LastAttemptedExecutionTime` is the instant the start was made and its
//!   `LastSuccessfulExecutionTime` is the instant one of its jobs completed —
//!   both read from the jobs this module stores, so a plan read and a job read
//!   can never disagree about whether the plan has run;
//! * a `ReportJobId` that is not of the form a report job's id takes names no job
//!   at all: it is refused as an invalid parameter value naming the member and
//!   stating that form, before any lookup ([`rejected_report_job_id`]), while an id
//!   of that form this account holds no job under is the not-found condition
//!   ([`report_job_not_found`]);
//! * a report is not compiled the instant the start answers: a job passes
//!   through the statuses AWS lists for one — `CREATED`, then `RUNNING`, then
//!   `COMPLETED` — on this service's own clock
//!   ([`ComAmazonawsBackupCryocontrollerusermanagerState::settle_report_jobs`]),
//!   so a caller reading the job straight after the start still observes the
//!   non-terminal status;
//! * the report a job publishes exists only once that job has COMPLETED:
//!   `ReportDestination` names the bucket the stored plan delivers to from the
//!   start, and carries the object key of each delivered format only after the
//!   job reaches its terminal status ([`ReportJobRecord::reported`]), so a client
//!   never reads a key for a report nothing has written yet.

use std::time::{Duration, SystemTime};

use super::ComAmazonawsBackupCryocontrollerusermanagerState;
use crate::ops::types::{
    InvalidParameterValueException, ReportDestination, ReportJob, ResourceNotFoundException,
};

/// The most report jobs this emulator keeps.
///
/// An EMULATOR-CHOSEN ceiling, not an AWS quota, for the reason given on
/// [`super::MAX_VAULTS`]: the store is in memory and every start adds to it.
/// `StartReportJob` models no class for a quota (`smithy_query` operation
/// `StartReportJob` lists `InvalidParameterValueException`,
/// `MissingParameterValueException`, `ResourceNotFoundException` and
/// `ServiceUnavailableException`), so the ceiling is kept by FORGETTING the
/// oldest job the store holds, the finished ones first
/// ([`ComAmazonawsBackupCryocontrollerusermanagerState::start_report_job`]).
pub const MAX_REPORT_JOBS: usize = 500;

/// The statuses a report job carries: "CREATED | RUNNING | COMPLETED | FAILED"
/// (model doc for `ReportJob$Status`). A start reports the first of them and the
/// clock moves it through the second to the third; nothing in this emulator
/// fails the compilation of a report, so no stored job reaches the fourth.
pub const REPORT_JOB_STATUS_CREATED: &str = "CREATED";
pub const REPORT_JOB_STATUS_RUNNING: &str = "RUNNING";
pub const REPORT_JOB_STATUS_COMPLETED: &str = "COMPLETED";

/// How long after the start the report run falls due, so a caller reading the
/// job straight after the start still observes the status AWS reports for a job
/// that has not begun compiling yet. The same span this service gives a report
/// plan's deployment ([`super::REPORT_PLAN_SETTLE_AFTER`]), which is the sibling
/// Backup Audit Manager resource.
pub const REPORT_JOB_RUNS_AFTER: Duration = Duration::from_secs(3);

/// How long after the run begins the report is published, so a caller polling
/// the job observes the `RUNNING` status too rather than a job that jumps from
/// created to completed. The same span again, measured from
/// [`REPORT_JOB_RUNS_AFTER`].
pub const REPORT_JOB_COMPILES_FOR: Duration = Duration::from_secs(3);

/// The extension the report of one delivery format is written under: the format
/// a plan delivers in, lowercased ("CSV, JSON, or both" — model doc for
/// `ReportDeliveryChannel$Formats`).
fn report_key_extension(format: &str) -> String {
    format.to_ascii_lowercase()
}

/// The sentence AWS states for "the report job asked about is not one this
/// account holds": fixed text around the job the call addressed and the account
/// it was made in, as the refusals of the other Backup Audit Manager resources
/// state them ([`super::report_plan_not_found_message`],
/// [`super::framework_not_found_message`]). The variable parts are the id the
/// request carried and the account this emulator presents. No `Code`: this
/// condition has no observed token.
pub fn report_job_not_found_message(report_job_id: &str) -> String {
    format!(
        "Report job {report_job_id} not found in account {}.",
        ::engine::emulator_account_id()
    )
}

/// The modelled refusal for a `ReportJobId` this account holds no report job
/// under, worded by [`report_job_not_found_message`] from the id the call
/// addressed.
pub fn report_job_not_found(report_job_id: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(report_job_not_found_message(report_job_id)),
        ..Default::default()
    }
}

/// The sentence AWS states for "the value a call addresses a report job by is not
/// an id a report job can have": fixed text around the id the request carried,
/// which states the FORM the id must take, so a client learns what to fix from
/// the sentence alone. The variable part is that id, exactly as the request sent
/// it. No `Code` of its own: the class's token is the one on
/// [`super::ERROR_CODE_INVALID_PARAMETER_VALUE`].
pub fn invalid_report_job_id_message(report_job_id: &str) -> String {
    format!("Invalid report job ID {report_job_id}. The report job ID must be a valid UUID.")
}

/// Why the `ReportJobId` a call addresses a report job by is not a value this
/// service can resolve, or `None` when it is one.
///
/// The id targets a plain string the model bounds by nothing (`smithy_query`
/// shape `ReportJobId`), so the framework refuses nothing for it before a handler
/// runs: a value that is not of the form a report job's id takes — the hyphenated
/// UUID this service mints one in
/// ([`ComAmazonawsBackupCryocontrollerusermanagerState::next_id`], read by
/// [`super::is_minted_backup_identifier`]) — names no job at all and is refused as
/// an invalid parameter value naming the member, before any lookup, rather than
/// reported as a job this account does not hold.
///
/// The ONE reading of a report job id, so every call that addresses a report job
/// applies the same rule and answers the same sentence for the same condition.
pub fn rejected_report_job_id(report_job_id: &str) -> Option<InvalidParameterValueException> {
    if super::is_minted_backup_identifier(report_job_id) {
        return None;
    }
    Some(InvalidParameterValueException {
        message: Some(invalid_report_job_id_message(report_job_id)),
        ..super::rejected_parameter("ReportJobId")
    })
}

/// A report job: the run of a stored report plan `StartReportJob` begins and
/// `DescribeReportJob` reports.
///
/// The members taken from the plan are COPIED into the job when it starts — the
/// report a job compiles is the one the plan described at that moment, so a
/// later update of the plan does not rewrite the history of a job that already
/// ran — and the instants only this service can know are its own members.
#[derive(Debug, Clone)]
pub struct ReportJobRecord {
    pub id: String,
    /// The plan this job is a run of: its name, which the start addressed, and
    /// its ARN, which the job reports.
    pub report_plan_name: String,
    pub report_plan_arn: String,
    /// The template the report is built from, as the plan's setting states it.
    pub report_template: String,
    /// Where the report is published, as the plan's delivery channel states it:
    /// the bucket, the key prefix it carries if any, and the formats the plan
    /// delivers in.
    pub s3_bucket_name: String,
    pub s3_key_prefix: Option<String>,
    pub formats: Vec<String>,
    pub creation_time: SystemTime,
    /// When the report was published; `None` until the job reaches
    /// [`REPORT_JOB_STATUS_COMPLETED`].
    pub completion_time: Option<SystemTime>,
    /// Where this run has got to: one of the statuses on
    /// [`REPORT_JOB_STATUS_CREATED`].
    pub status: String,
    /// When the run begins and when the report is published — see
    /// [`REPORT_JOB_RUNS_AFTER`].
    pub runs_at: SystemTime,
    pub completes_at: SystemTime,
    /// The `IdempotencyToken` of the start that began this job, if it carried
    /// one; a repeat of it replays this job rather than starting a second.
    pub idempotency_token: Option<String>,
}

impl ReportJobRecord {
    /// The job as a READ reports it — the ONE projection of a stored report job,
    /// so a read can report nothing the start did not store.
    pub fn reported(&self) -> ReportJob {
        ReportJob {
            report_job_id: Some(self.id.clone()),
            report_plan_arn: Some(self.report_plan_arn.clone()),
            report_template: Some(self.report_template.clone()),
            creation_time: Some(super::modelled_timestamp(self.creation_time)),
            completion_time: self.completion_time.map(super::modelled_timestamp),
            status: Some(self.status.clone()),
            // "A message explaining the status of the report job" (model doc for
            // the member): AWS states one for a job that FAILED, and no stored
            // job of this emulator does.
            status_message: None,
            report_destination: Some(ReportDestination {
                s3_bucket_name: Some(self.s3_bucket_name.clone()),
                s3_keys: Some(self.published_keys()),
            }),
        }
    }

    /// The object keys the report of this job was published under: one per
    /// format the plan delivers in, and NONE until the job has completed —
    /// "the object key that uniquely identifies your reports in your S3 bucket"
    /// (model doc for `ReportDestination$S3Keys`), and a report an unfinished job
    /// has not written yet has no key.
    ///
    /// The layout is this emulator's: the plan's own key prefix where it carries
    /// one, then the plan name, then the job id under the format's extension —
    /// derived from the stored plan and the job's own id, so it identifies this
    /// one run of this one plan.
    pub fn published_keys(&self) -> Vec<String> {
        if self.status != REPORT_JOB_STATUS_COMPLETED {
            return Vec::new();
        }
        self.formats
            .iter()
            .map(|format| {
                let leaf = format!(
                    "{}/{}.{}",
                    self.report_plan_name,
                    self.id,
                    report_key_extension(format)
                );
                match self.s3_key_prefix.as_deref() {
                    Some(prefix) if !prefix.is_empty() => {
                        format!("{}/{leaf}", prefix.trim_end_matches('/'))
                    }
                    _ => leaf,
                }
            })
            .collect()
    }

    /// Whether this run has not published its report yet, so the clock still
    /// owes it a transition.
    pub fn run_in_flight(&self) -> bool {
        self.status != REPORT_JOB_STATUS_COMPLETED
    }
}

impl ComAmazonawsBackupCryocontrollerusermanagerState {
    /// Resolve the report job a `ReportJobId` addresses, or the modelled refusal
    /// for one this account does not hold.
    ///
    /// The ONE resolution step every call that names a report job shares, so
    /// none of them can answer a different class or a different sentence for the
    /// same condition.
    pub fn report_job(
        &self,
        report_job_id: &str,
    ) -> Result<&ReportJobRecord, ResourceNotFoundException> {
        self.report_jobs
            .get(report_job_id)
            .ok_or_else(|| report_job_not_found(report_job_id))
    }

    /// The report job an `IdempotencyToken` started FOR THIS PLAN, if this
    /// account still holds one: "Retrying a successful request with the same
    /// idempotency token results in a success message with no action taken"
    /// (model doc for the member), so a start that repeats a token answers with
    /// the job it already began rather than beginning a second one.
    ///
    /// The token is read against the plan it was first used with, because the
    /// token distinguishes "otherwise identical calls" (the same model doc) and a
    /// start naming another report plan is not one of those: it starts a run of
    /// that other plan rather than replaying this one's.
    pub fn report_job_by_idempotency_token(
        &self,
        plan_name: &str,
        token: &str,
    ) -> Option<&ReportJobRecord> {
        self.report_jobs.values().find(|job| {
            job.report_plan_name == plan_name && job.idempotency_token.as_deref() == Some(token)
        })
    }

    /// Begin a run of the report plan `plan_name`, returning the `ReportJobId`
    /// the start answers with, or `None` when this account holds no plan under
    /// that name.
    ///
    /// The job's body is read out of the stored plan — the start request carries
    /// no template and no destination — and the start is recorded on the plan as
    /// its last attempted execution, so a read of either resource reports the
    /// same run.
    pub fn start_report_job(
        &mut self,
        plan_name: &str,
        idempotency_token: Option<String>,
        now: SystemTime,
    ) -> Option<String> {
        let plan = self.report_plans.get_mut(plan_name)?;
        // Starting the report IS an attempt of this plan, whatever the run goes
        // on to do.
        plan.last_attempted_execution_time = Some(now);
        let report_plan_arn = plan.arn.clone();
        let report_template = plan.report_setting.report_template.clone();
        let channel = plan.report_delivery_channel.clone();

        let runs_at = now.checked_add(REPORT_JOB_RUNS_AFTER).unwrap_or(now);
        let completes_at = runs_at.checked_add(REPORT_JOB_COMPILES_FOR).unwrap_or(runs_at);
        let id = self.next_id(now);
        let record = ReportJobRecord {
            id: id.clone(),
            report_plan_name: plan_name.to_string(),
            report_plan_arn,
            report_template,
            s3_bucket_name: channel.s3_bucket_name,
            s3_key_prefix: channel.s3_key_prefix,
            formats: channel.formats.unwrap_or_default(),
            creation_time: now,
            completion_time: None,
            status: REPORT_JOB_STATUS_CREATED.to_string(),
            runs_at,
            completes_at,
            idempotency_token,
        };
        self.forget_oldest_report_jobs();
        self.report_jobs.insert(id.clone(), record);
        Some(id)
    }

    /// Keep the report-job store within [`MAX_REPORT_JOBS`] by forgetting the
    /// jobs it holds, the ones that have already published their report first and
    /// the oldest of those first of all.
    ///
    /// A start AWS serves is never refused for this ceiling, which is this
    /// emulator's own: the operation models no class for a quota, so the store is
    /// bounded by dropping history rather than by answering a refusal AWS does not
    /// give — and the cap holds whatever statuses the stored jobs are in, so no
    /// run of starts can grow this store without end.
    fn forget_oldest_report_jobs(&mut self) {
        while self.report_jobs.len() >= MAX_REPORT_JOBS {
            let oldest = self
                .report_jobs
                .values()
                .min_by_key(|job| (job.run_in_flight(), job.creation_time))
                .map(|job| job.id.clone());
            let Some(oldest) = oldest else { break };
            self.report_jobs.remove(&oldest);
        }
    }

    /// Materialise the transitions the clock has already made due for every
    /// report job: the run a start owes, which begins as
    /// [`REPORT_JOB_STATUS_RUNNING`] and publishes its report as
    /// [`REPORT_JOB_STATUS_COMPLETED`]. Like [`Self::settle_report_plans`] this
    /// is the only write a reporting operation performs — each job was owed each
    /// transition at its own instant whether or not anybody called.
    ///
    /// A completed run is also the plan's last SUCCESSFUL execution, so the
    /// completion is recorded on the plan the job ran for in the same step.
    pub fn settle_report_jobs(&mut self, now: SystemTime) {
        let mut completed: Vec<(String, SystemTime)> = Vec::new();
        for job in self.report_jobs.values_mut() {
            if !job.run_in_flight() {
                continue;
            }
            if now >= job.completes_at {
                job.status = REPORT_JOB_STATUS_COMPLETED.to_string();
                job.completion_time = Some(job.completes_at);
                completed.push((job.report_plan_name.clone(), job.completes_at));
            } else if now >= job.runs_at && job.status == REPORT_JOB_STATUS_CREATED {
                job.status = REPORT_JOB_STATUS_RUNNING.to_string();
            }
        }
        for (plan_name, completed_at) in completed {
            // The plan a completed run belongs to may have been deleted since;
            // the job keeps reporting the run either way.
            if let Some(plan) = self.report_plans.get_mut(&plan_name) {
                if plan
                    .last_successful_execution_time
                    .is_none_or(|previous| previous < completed_at)
                {
                    plan.last_successful_execution_time = Some(completed_at);
                }
            }
        }
    }
}
