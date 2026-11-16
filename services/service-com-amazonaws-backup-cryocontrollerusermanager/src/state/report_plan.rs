// SPDX-License-Identifier: CC0-1.0

//! The REPORT PLAN side of this service's state: the plan `CreateReportPlan`
//! stores, and what every call that names a `ReportPlanName` resolves against.
//!
//! # The rules this module holds
//!
//! * a report plan is addressed by NAME — every report-plan call binds the name
//!   as its path label — and that name is unique to the account: the store is
//!   keyed by it, so the first create of a name stores the one plan that name
//!   has and any other create naming it is refused as already existing
//!   ([`report_plan_already_exists`]), which is what the model documents for
//!   `CreateReportPlan` ("If you call CreateReportPlan with a plan that already
//!   exists, you receive an AlreadyExistsException exception");
//! * a name the account holds no plan under is the not-found condition
//!   ([`report_plan_not_found`]), which every read of a plan answers;
//! * the plan body a create carries is stored as submitted, except for the
//!   members the model gives a rule for — the delivery formats, which "default
//!   \[to\] CSV" where the request lists none, and `NumberOfFrameworks`, which is
//!   "the number of frameworks a report covers" and so is counted from the
//!   frameworks the setting names — and is reported back through the ONE read
//!   projection below ([`ReportPlanRecord::reported`]), so the plan a create
//!   stored and the plan a read reports can never differ;
//! * a report template or a delivery format outside the published values is
//!   refused before anything is stored
//!   ([`rejected_report_plan_body`]);
//! * the deployment of a stored plan is not finished the instant the create
//!   answers: a plan starts in the non-terminal status AWS reports for one and
//!   reaches [`REPORT_PLAN_DEPLOYMENT_STATUS_COMPLETED`] when the clock makes
//!   that due ([`ComAmazonawsBackupCryocontrollerusermanagerState::settle_report_plans`]);
//! * Backup performs ONE action on a report plan at a time, so a mutation that
//!   arrives while that plan's deployment is still in flight is refused
//!   ([`report_plan_action_in_progress`]) and the stored plan is left as it was,
//!   exactly as the sibling framework's mutations are refused
//!   ([`super::framework_action_in_progress`]).

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

use super::ComAmazonawsBackupCryocontrollerusermanagerState;
use crate::ops::types::{
    AlreadyExistsException, ConflictException, InvalidParameterValueException,
    ReportDeliveryChannel, ReportPlan, ReportSetting, ResourceNotFoundException,
};

/// The most report plans this emulator keeps.
///
/// An EMULATOR-CHOSEN ceiling, not an AWS quota, for the reason given on
/// [`super::MAX_VAULTS`]: the store is in memory and every request can add to
/// it. AWS caps the report plans an account may hold too, and answers the same
/// modelled `LimitExceededException` for a create past it.
pub const MAX_REPORT_PLANS: usize = 500;

/// The deployment statuses a report plan carries: "CREATE_IN_PROGRESS |
/// UPDATE_IN_PROGRESS | DELETE_IN_PROGRESS | COMPLETED" (model doc for
/// `ReportPlan$DeploymentStatus`). A create reports the first of them and the
/// clock moves it to the terminal one.
pub const REPORT_PLAN_DEPLOYMENT_STATUS_CREATE_IN_PROGRESS: &str = "CREATE_IN_PROGRESS";
pub const REPORT_PLAN_DEPLOYMENT_STATUS_UPDATE_IN_PROGRESS: &str = "UPDATE_IN_PROGRESS";
pub const REPORT_PLAN_DEPLOYMENT_STATUS_COMPLETED: &str = "COMPLETED";

/// How long after the call that starts it a report plan's deployment falls due,
/// so a caller reading the plan straight after the create still observes the
/// non-terminal status AWS reports for one. The same span this service gives a
/// framework's deployment ([`super::FRAMEWORK_SETTLE_AFTER`]), which is the
/// sibling Backup Audit Manager resource.
pub const REPORT_PLAN_SETTLE_AFTER: Duration = Duration::from_secs(3);

/// The report templates a report may be built from: "RESOURCE_COMPLIANCE_REPORT
/// | CONTROL_COMPLIANCE_REPORT | BACKUP_JOB_REPORT | COPY_JOB_REPORT |
/// RESTORE_JOB_REPORT | SCAN_JOB_REPORT" (model doc for
/// `ReportSetting$ReportTemplate`).
///
/// The member targets a plain string, so the framework refuses nothing for it
/// before a handler runs: the published list is named here instead, so no plan
/// is stored for a template that builds no report.
pub const REPORT_TEMPLATES: [&str; 6] = [
    "RESOURCE_COMPLIANCE_REPORT",
    "CONTROL_COMPLIANCE_REPORT",
    "BACKUP_JOB_REPORT",
    "COPY_JOB_REPORT",
    "RESTORE_JOB_REPORT",
    "SCAN_JOB_REPORT",
];

/// The formats a report is delivered in: "The format of your reports: CSV,
/// JSON, or both" (model doc for `ReportDeliveryChannel$Formats`), named here
/// for the reason given on [`REPORT_TEMPLATES`].
pub const REPORT_FORMATS: [&str; 2] = ["CSV", "JSON"];

/// The format a plan delivers in when its create lists none: "If not specified,
/// the default format is CSV" (model doc for the member).
pub const DEFAULT_REPORT_FORMAT: &str = "CSV";

/// The ARN of a report plan:
/// `arn:aws:backup:<region>:<account>:report-plan:<name>-<uniquifier>`.
///
/// A report plan is addressed by NAME, and the name alone is not unique over
/// time — a deleted plan's name can be taken again — so the ARN AWS mints for
/// one carries a uniquifier after the name, as a framework's does
/// ([`super::framework_arn`]); `uniquifier` is a value only this service mints
/// ([`ComAmazonawsBackupCryocontrollerusermanagerState::next_id`]).
pub fn report_plan_arn(region: &str, name: &str, uniquifier: &str) -> String {
    ::engine::emulator_arn(
        super::ARN_NAMESPACE,
        region,
        &format!("report-plan:{name}-{uniquifier}"),
    )
}

/// The sentence AWS states for "the report plan asked about is not one this
/// account holds": fixed text around the plan the call addressed and the account
/// it was made in, as the framework refusal in the same Backup Audit Manager
/// family states them ([`super::framework_not_found_message`]). The variable
/// parts are the name the request carried and the account this emulator
/// presents. No `Code`: this condition has no observed token.
pub fn report_plan_not_found_message(name: &str) -> String {
    format!(
        "Report Plan {name} not found in account {}.",
        ::engine::emulator_account_id()
    )
}

/// The sentence AWS states for "this account already holds the report plan the
/// one being created would duplicate": fixed text naming the plan the create
/// asked for and the account it was made in, as the refusal above and the
/// framework's state them. No observed `Code`.
pub fn report_plan_already_exists_message(name: &str) -> String {
    format!(
        "Report Plan {name} already exists in account {}.",
        ::engine::emulator_account_id()
    )
}

/// The modelled refusal for a report plan name this account does not hold,
/// worded by [`report_plan_not_found_message`] from the name the call addressed.
pub fn report_plan_not_found(name: &str) -> ResourceNotFoundException {
    ResourceNotFoundException {
        message: Some(report_plan_not_found_message(name)),
        ..Default::default()
    }
}

/// The sentence AWS states for "a previous action on this report plan is still
/// running, so the one just requested cannot be performed": the sentence this
/// service states for the sibling Backup Audit Manager resource
/// ([`super::framework_action_in_progress_message`]) around the report plan the
/// call addressed and the account it was made in, which are its variable parts.
/// No `Code`: this condition has no observed token.
pub fn report_plan_action_in_progress_message(name: &str) -> String {
    format!(
        "Operation failed because of a concurrent update for the report plan {name} in account \
         {}. Try again later.",
        ::engine::emulator_account_id()
    )
}

/// The modelled refusal for an action on a report plan whose previous action has
/// not finished — see [`report_plan_action_in_progress_message`]. Backup
/// serializes the deployment of one report plan, so the action that loses the
/// race is refused rather than blended into the one already running.
pub fn report_plan_action_in_progress(name: &str) -> ConflictException {
    ConflictException {
        message: Some(report_plan_action_in_progress_message(name)),
        ..Default::default()
    }
}

/// The modelled refusal for a create this account already holds the report plan
/// for, worded by [`report_plan_already_exists_message`] from the name the
/// request carried.
///
/// `Arn` and `CreatorRequestId` are left UNSET for the reason given on
/// [`super::vault_already_exists`].
pub fn report_plan_already_exists(name: &str) -> AlreadyExistsException {
    AlreadyExistsException {
        message: Some(report_plan_already_exists_message(name)),
        ..Default::default()
    }
}

/// Why the body a create or an update carries is not one a report plan can be
/// stored from, or `None` when it is one.
///
/// The ONE reading of a report plan body, so every operation that stores one
/// applies the same published rules and names the same member in its refusal:
/// the report template is one of [`REPORT_TEMPLATES`] and every delivery format
/// is one of [`REPORT_FORMATS`].
pub fn rejected_report_plan_body(
    report_setting: Option<&ReportSetting>,
    report_delivery_channel: Option<&ReportDeliveryChannel>,
) -> Option<InvalidParameterValueException> {
    if let Some(setting) = report_setting {
        if !REPORT_TEMPLATES.contains(&setting.report_template.as_str()) {
            return Some(super::rejected_parameter("ReportTemplate"));
        }
    }
    if let Some(channel) = report_delivery_channel {
        let formats = channel.formats.as_deref().unwrap_or_default();
        if formats
            .iter()
            .any(|format| !REPORT_FORMATS.contains(&format.as_str()))
        {
            return Some(super::rejected_parameter("Formats"));
        }
    }
    None
}

/// The delivery channel a stored plan carries: as submitted, with the formats
/// filled in where the request listed none — "If not specified, the default
/// format is CSV" (model doc for the member) — so a read of the plan reports the
/// format it will deliver in rather than nothing.
pub fn stored_report_delivery_channel(channel: ReportDeliveryChannel) -> ReportDeliveryChannel {
    let formats = match channel.formats {
        Some(formats) if !formats.is_empty() => formats,
        _ => vec![DEFAULT_REPORT_FORMAT.to_string()],
    };
    ReportDeliveryChannel {
        formats: Some(formats),
        ..channel
    }
}

/// The report setting a stored plan carries: as submitted, with
/// `NumberOfFrameworks` counted from the frameworks the setting names — it is
/// "the number of frameworks a report covers" (model doc for the member), so it
/// is derived from the stored `FrameworkArns` rather than taken from the
/// request, and the two can never disagree.
pub fn stored_report_setting(setting: ReportSetting) -> ReportSetting {
    let number_of_frameworks = i32::try_from(
        setting
            .framework_arns
            .as_deref()
            .unwrap_or_default()
            .len(),
    )
    .unwrap_or(i32::MAX);
    ReportSetting {
        number_of_frameworks,
        ..setting
    }
}

/// A report plan: what `CreateReportPlan` stores and `DescribeReportPlan`
/// reports.
///
/// The body is kept as the create submitted it (with the two members above
/// filled in), so a read reports what was stored rather than a value recomputed
/// per call; the instants only this service can know — when the plan was stored,
/// and when a report job of it last ran — are its own members.
#[derive(Debug, Clone)]
pub struct ReportPlanRecord {
    pub name: String,
    pub arn: String,
    pub description: Option<String>,
    pub report_setting: ReportSetting,
    pub report_delivery_channel: ReportDeliveryChannel,
    pub creation_time: SystemTime,
    /// Where this plan's deployment has got to: one of the statuses on
    /// [`REPORT_PLAN_DEPLOYMENT_STATUS_CREATE_IN_PROGRESS`].
    pub deployment_status: String,
    /// When that deployment falls due — see [`REPORT_PLAN_SETTLE_AFTER`].
    pub settles_at: SystemTime,
    /// The `IdempotencyToken` of the create that stored this plan, if it carried
    /// one; a repeat of it replays this plan rather than storing a second.
    pub idempotency_token: Option<String>,
    pub tags: BTreeMap<String, String>,
    /// When a report job of this plan last attempted to run, and when one last
    /// ran successfully; `None` until one has.
    pub last_attempted_execution_time: Option<SystemTime>,
    pub last_successful_execution_time: Option<SystemTime>,
}

impl ReportPlanRecord {
    /// The plan as a READ reports it — the ONE projection of a stored report
    /// plan, so a read can report nothing the create did not store.
    pub fn reported(&self) -> ReportPlan {
        ReportPlan {
            report_plan_arn: Some(self.arn.clone()),
            report_plan_name: Some(self.name.clone()),
            report_plan_description: self.description.clone(),
            report_setting: Some(self.report_setting.clone()),
            report_delivery_channel: Some(self.report_delivery_channel.clone()),
            deployment_status: Some(self.deployment_status.clone()),
            creation_time: Some(::aws_smithy_types::DateTime::from(self.creation_time)),
            last_attempted_execution_time: self
                .last_attempted_execution_time
                .map(::aws_smithy_types::DateTime::from),
            last_successful_execution_time: self
                .last_successful_execution_time
                .map(::aws_smithy_types::DateTime::from),
        }
    }

    /// Whether this plan's deployment has not finished yet, so the clock still
    /// owes it the terminal status.
    pub fn deployment_in_flight(&self) -> bool {
        self.deployment_status != REPORT_PLAN_DEPLOYMENT_STATUS_COMPLETED
    }
}

impl ComAmazonawsBackupCryocontrollerusermanagerState {
    /// Resolve the report plan a `ReportPlanName` addresses, or the modelled
    /// refusal for one this account does not hold.
    ///
    /// The ONE resolution step every call that names a report plan shares, so
    /// none of them can answer a different class or a different sentence for the
    /// same condition.
    pub fn report_plan(&self, name: &str) -> Result<&ReportPlanRecord, ResourceNotFoundException> {
        self.report_plans
            .get(name)
            .ok_or_else(|| report_plan_not_found(name))
    }

    /// The report plan an `IdempotencyToken` created, if this account still
    /// holds one: "Retrying a successful request with the same idempotency token
    /// results in a success message with no action taken" (model doc for the
    /// member), so a create that repeats a token answers with the plan it
    /// already made rather than making a second one.
    pub fn report_plan_by_idempotency_token(&self, token: &str) -> Option<&ReportPlanRecord> {
        self.report_plans
            .values()
            .find(|plan| plan.idempotency_token.as_deref() == Some(token))
    }

    /// Materialise the transition the clock has already made due for every
    /// report plan: the deployment a create or an update that reports a
    /// non-terminal status owes, which finishes as
    /// [`REPORT_PLAN_DEPLOYMENT_STATUS_COMPLETED`]. Like
    /// [`Self::settle_frameworks`] this is the only write a reporting operation
    /// performs — each plan was owed its deployment at its own `settles_at`
    /// whether or not anybody called.
    pub fn settle_report_plans(&mut self, now: SystemTime) {
        for plan in self.report_plans.values_mut() {
            if plan.deployment_in_flight() && now >= plan.settles_at {
                plan.deployment_status = REPORT_PLAN_DEPLOYMENT_STATUS_COMPLETED.to_string();
            }
        }
    }
}
