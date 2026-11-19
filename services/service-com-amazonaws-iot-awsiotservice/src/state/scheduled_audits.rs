// SPDX-License-Identifier: CC0-1.0

//! The account's DEVICE DEFENDER SCHEDULED AUDITS: the record one holds, the SCHEDULE a
//! frequency demands, and what every reader of one reports.
//!
//! A scheduled audit is "a scheduled audit that is run at a specified time interval"
//! (`CreateScheduledAudit`), and the name is its identity — the model binds it as the
//! `@httpLabel` of `/audit/scheduledaudits/{scheduledAuditName}` on every single-audit
//! operation — so the store is a `BTreeMap` keyed by it. `BTreeMap` (not `HashMap`)
//! because `ListScheduledAudits` is `@paginated` and a page boundary is only meaningful
//! over a stable order.
//!
//! One record carries everything a reader hands back, so `DescribeScheduledAudit` after
//! `CreateScheduledAudit` reports the created schedule member for member: a reader clones
//! what the writer stored and the two cannot drift.
//!
//! WHICH SCHEDULE MEMBER IS REQUIRED FOLLOWS FROM THE FREQUENCY, which is what the model
//! documents on the two day members: `dayOfMonth` "is required if the frequency parameter
//! is set to MONTHLY" and `dayOfWeek` "is required if the frequency parameter is set to
//! WEEKLY or BIWEEKLY"
//! (`smithy_query(query="operation",operation="CreateScheduledAudit")`), so a writer
//! checks the pair rather than the members one at a time ([`schedule_refusal`]) and a
//! frequency that names no day at all (DAILY) demands neither.
//!
//! Which CHECKS a scheduled audit may target is the account's own audit configuration,
//! exactly as it is for an audit a caller starts on demand
//! ([`super::audit_tasks::targetable_checks`]): "Checks must be enabled for your
//! account", so the writers read that one rule and the two kinds of audit cannot
//! disagree about which checks may run at all. A check named twice is one check of the
//! schedule, for the reason [`ScheduledAudit::new`] records.

use std::collections::{BTreeMap, BTreeSet};

use crate::ops::types::DescribeScheduledAuditOutput;

/// How many scheduled audits this emulator keeps for the account.
///
/// AWS publishes a service quota on an account's scheduled audits; the exact figure is not
/// in the model, so this is the emulator's own ceiling on a store a caller can grow — the
/// same reason [`super::MAX_MITIGATION_ACTIONS`] exists — and `CreateScheduledAudit` past
/// it refuses with the modelled `LimitExceededException` rather than growing without
/// bound.
pub const MAX_SCHEDULED_AUDITS: usize = 100;

/// How many tags one scheduled audit may carry: the same documented IoT tagging limit
/// [`super::MAX_POLICY_TAGS`] records, for the same reason.
pub const MAX_SCHEDULED_AUDIT_TAGS: usize = super::MAX_POLICY_TAGS;

/// The `AuditFrequency` of an audit that runs every day
/// ([`crate::ops::types::AUDIT_FREQUENCY_VALUES`]), which names no day of the week or of
/// the month.
pub const AUDIT_FREQUENCY_DAILY: &str = "DAILY";

/// The `AuditFrequency` of an audit that runs once a week, which names the day of the week
/// it runs on.
pub const AUDIT_FREQUENCY_WEEKLY: &str = "WEEKLY";

/// The `AuditFrequency` of an audit that runs every other week, which names the day of the
/// week it runs on.
pub const AUDIT_FREQUENCY_BIWEEKLY: &str = "BIWEEKLY";

/// The `AuditFrequency` of an audit that runs once a month, which names the day of the
/// month it runs on.
pub const AUDIT_FREQUENCY_MONTHLY: &str = "MONTHLY";

/// The ARN of a scheduled audit:
/// `arn:aws:iot:<region>:<account>:scheduledaudit/<scheduledAuditName>`.
///
/// `scheduledaudit/` is the resource type AWS IoT's scheduled-audit ARNs use — it is what
/// `CreateScheduledAudit$scheduledAuditArn` / `DescribeScheduledAudit$scheduledAuditArn`
/// report and what an `iot:DescribeScheduledAudit` policy statement names one by; the
/// namespace and account segments are the ones [`super::thing_arn`] documents.
pub fn scheduled_audit_arn(region: &str, scheduled_audit_name: &str) -> String {
    ::emulator_server::emulator_arn(
        "iot",
        region,
        &format!("scheduledaudit/{scheduled_audit_name}"),
    )
}

/// How this service states that a `scheduledAuditName` names no scheduled audit this
/// account holds — for a reader of one, a writer of one and a delete of one.
///
/// The DEVICE DEFENDER family states the miss of the resource the request named, so the
/// fixed text is the family's and the only variable part is the name asked about, exactly
/// as that family states a missing audit task ([`super::missing_audit_task_message`]).
pub fn missing_scheduled_audit_message(scheduled_audit_name: &str) -> String {
    format!("Scheduled audit {scheduled_audit_name} does not exist.")
}

/// Why a writer refused a scheduled audit's SCHEDULE, mapped to the operation's modelled
/// `InvalidRequestException` ("The request is not valid", HTTP 400) by the handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScheduleRefusal {
    /// A `MONTHLY` schedule that names no `dayOfMonth`: that member "is required if the
    /// frequency parameter is set to MONTHLY".
    DayOfMonthRequired(String),
    /// A `WEEKLY` or `BIWEEKLY` schedule that names no `dayOfWeek`: that member "is
    /// required if the frequency parameter is set to WEEKLY or BIWEEKLY".
    DayOfWeekRequired(String),
}

impl ScheduleRefusal {
    /// The message a handler refuses the request with: the member the schedule left out
    /// and the frequency that demands it, so the caller can fix the request from the text
    /// alone.
    pub fn message(&self) -> String {
        match self {
            Self::DayOfMonthRequired(frequency) => {
                format!("dayOfMonth is required for frequency {frequency}")
            }
            Self::DayOfWeekRequired(frequency) => {
                format!("dayOfWeek is required for frequency {frequency}")
            }
        }
    }
}

/// Why the schedule `frequency` names together with `day_of_month` / `day_of_week` is not
/// one this service can run, or `None` when it is.
///
/// The ONE reading of a scheduled audit's schedule: every writer of one calls it, so a
/// create and an update cannot disagree about which day a frequency demands.
pub fn schedule_refusal(
    frequency: &str,
    day_of_month: Option<&str>,
    day_of_week: Option<&str>,
) -> Option<ScheduleRefusal> {
    match frequency {
        AUDIT_FREQUENCY_MONTHLY if day_of_month.is_none() => {
            Some(ScheduleRefusal::DayOfMonthRequired(frequency.to_string()))
        }
        AUDIT_FREQUENCY_WEEKLY | AUDIT_FREQUENCY_BIWEEKLY if day_of_week.is_none() => {
            Some(ScheduleRefusal::DayOfWeekRequired(frequency.to_string()))
        }
        _ => None,
    }
}

/// One scheduled audit in the store, as this service persists it.
///
/// It holds what `DescribeScheduledAudit` reports member for member — the name is its
/// identity, the ARN is the identity minted for it, and the frequency, the day it runs on
/// and the checks it targets are the schedule — so a reader clones this record and the
/// writer and the reader cannot drift.
#[derive(Debug, Clone)]
pub struct ScheduledAudit {
    /// The name the audit was created under; also its key in the store.
    pub scheduled_audit_name: String,
    /// `arn:aws:iot:<region>:<account>:scheduledaudit/<scheduledAuditName>`, minted on
    /// create.
    pub scheduled_audit_arn: String,
    /// "How often the scheduled audit takes place": one of
    /// [`crate::ops::types::AUDIT_FREQUENCY_VALUES`].
    pub frequency: String,
    /// "The day of the month on which the scheduled audit takes place", for a schedule
    /// whose frequency names one.
    pub day_of_month: Option<String>,
    /// "The day of the week on which the scheduled audit takes place", for a schedule
    /// whose frequency names one.
    pub day_of_week: Option<String>,
    /// "Which checks are performed during the scheduled audit", in the order the request
    /// named them.
    pub target_check_names: Vec<String>,
    /// `Tag$Key` -> `Tag$Value` for the tags the create request carried — "Metadata that
    /// can be used to manage the scheduled audit" — which is also the set the tagging
    /// operations read and write ([`super::resource_tags`]).
    pub tags: BTreeMap<String, Option<String>>,
}

impl ScheduledAudit {
    /// A newly created scheduled audit: the minted ARN plus the schedule and the tags the
    /// create was given.
    ///
    /// A check NAMED TWICE is still one check of this schedule — "which checks are
    /// performed during the scheduled audit" is a set of checks — so each distinct name is
    /// recorded once, in the order the request first named it, exactly as an audit task's
    /// checks are keyed by name ([`super::audit_tasks::run_checks`]). That is also what
    /// bounds one record by the catalog of audit checks rather than by the length of the
    /// list a request sent.
    pub fn new(
        scheduled_audit_name: String,
        scheduled_audit_arn: String,
        frequency: String,
        day_of_month: Option<String>,
        day_of_week: Option<String>,
        target_check_names: Vec<String>,
        tags: BTreeMap<String, Option<String>>,
    ) -> Self {
        let mut named = BTreeSet::new();
        let target_check_names = target_check_names
            .into_iter()
            .filter(|check_name| named.insert(check_name.clone()))
            .collect();
        Self {
            scheduled_audit_name,
            scheduled_audit_arn,
            frequency,
            day_of_month,
            day_of_week,
            target_check_names,
            tags,
        }
    }

    /// The record as `DescribeScheduledAudit` reports it — every member of the stored
    /// record, so the read and the write cannot drift.
    pub fn to_description(&self) -> DescribeScheduledAuditOutput {
        DescribeScheduledAuditOutput {
            frequency: Some(self.frequency.clone()),
            day_of_month: self.day_of_month.clone(),
            day_of_week: self.day_of_week.clone(),
            target_check_names: Some(self.target_check_names.clone()),
            scheduled_audit_name: Some(self.scheduled_audit_name.clone()),
            scheduled_audit_arn: Some(self.scheduled_audit_arn.clone()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::types::AUDIT_FREQUENCY_VALUES;

    /// Every frequency this module names is a value the modelled `AuditFrequency` enum
    /// accepts, read through the generated constant so the two cannot drift.
    #[test]
    fn each_named_frequency_is_a_modeled_one() {
        for frequency in [
            AUDIT_FREQUENCY_DAILY,
            AUDIT_FREQUENCY_WEEKLY,
            AUDIT_FREQUENCY_BIWEEKLY,
            AUDIT_FREQUENCY_MONTHLY,
        ] {
            assert!(
                AUDIT_FREQUENCY_VALUES.contains(&frequency),
                "case {frequency:?}: expected one of the modelled values \
                 {AUDIT_FREQUENCY_VALUES:?}, got a name the model does not carry"
            );
        }
    }

    /// The day a schedule must name follows from its frequency, for every frequency the
    /// model declares: `MONTHLY` demands a day of the month, `WEEKLY` and `BIWEEKLY` a day
    /// of the week, and `DAILY` neither.
    #[test]
    fn the_day_a_schedule_must_name_follows_from_its_frequency() {
        for frequency in AUDIT_FREQUENCY_VALUES {
            let demands_day_of_month = *frequency == AUDIT_FREQUENCY_MONTHLY;
            let demands_day_of_week =
                *frequency == AUDIT_FREQUENCY_WEEKLY || *frequency == AUDIT_FREQUENCY_BIWEEKLY;

            let neither = schedule_refusal(frequency, None, None);
            assert_eq!(
                neither.is_some(),
                demands_day_of_month || demands_day_of_week,
                "case {frequency:?} with no day: got {neither:?}"
            );

            let with_day_of_month = schedule_refusal(frequency, Some("LAST"), None);
            assert_eq!(
                with_day_of_month.is_some(),
                demands_day_of_week,
                "case {frequency:?} with a day of the month: got {with_day_of_month:?}"
            );

            let with_day_of_week = schedule_refusal(frequency, None, Some("MON"));
            assert_eq!(
                with_day_of_week.is_some(),
                demands_day_of_month,
                "case {frequency:?} with a day of the week: got {with_day_of_week:?}"
            );
        }
    }
}
