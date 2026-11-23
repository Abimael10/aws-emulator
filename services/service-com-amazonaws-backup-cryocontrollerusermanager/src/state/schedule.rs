// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of a backup rule's `ScheduleExpression`, shared by the
//! operations that STORE a plan body and the read that reports its upcoming
//! runs.
//!
//! A rule's schedule is the expression AWS Backup initiates its jobs from, so
//! exactly one reading of it decides both halves of the service's behaviour: a
//! plan body is stored only when every rule's expression is one this reading
//! resolves to a schedule ([`rule_schedule_refusal`], which
//! `super::rejected_backup_plan_body` applies), and the upcoming runs a plan
//! read previews are computed from that same schedule
//! (`crate::ops::get_backup_plan`). Neither can therefore store a rule whose
//! schedule nothing can be computed from.

use super::BackupRuleInput;

/// The cron expression a rule's jobs are initiated by when the rule carries
/// none: "When no CRON expression is provided, Backup will use the default
/// expression `cron(0 5 ? * * *)`" (model doc for
/// `BackupRule$ScheduleExpression`).
pub const DEFAULT_SCHEDULE_EXPRESSION: &str = "cron(0 5 ? * * *)";

/// The time zone a rule's schedule is read in when the rule names none: "the
/// timezone in which the schedule expression is set. By default,
/// ScheduleExpressions are in UTC" (model doc for
/// `BackupRule$ScheduleExpressionTimezone`), named as the IANA zone UTC is
/// published under. A rule stored without one carries this zone
/// (`super::stored_backup_rule`), so every read of the rule reports the zone its
/// schedule really runs in.
pub const DEFAULT_SCHEDULE_EXPRESSION_TIMEZONE: &str = "Etc/UTC";

/// The zone a STORED schedule is read in: the stated one, or
/// [`DEFAULT_SCHEDULE_EXPRESSION_TIMEZONE`] where the request stated none.
///
/// The ONE materialisation of that default, shared by every resource of this
/// service whose body carries a `ScheduleExpressionTimezone` — a plan's rules
/// (`super::stored_backup_rule`) and a restore testing plan
/// (`super::RestoreTestingPlanRecord`) — so the zone a schedule really runs in
/// is what is stored and read back whichever resource carries it.
#[must_use]
pub fn stored_schedule_expression_timezone(timezone: Option<String>) -> String {
    timezone.unwrap_or_else(|| DEFAULT_SCHEDULE_EXPRESSION_TIMEZONE.to_string())
}

/// How many whitespace-separated fields a rule's cron expression carries: the
/// minute, hour, day-of-month, month, day-of-week and year of an Amazon Web
/// Services cron expression (the model's own examples for
/// `BackupRule$ScheduleExpression`, `0 12 * * ? *` and `15 * ? * * *`, carry six
/// each).
const CRON_FIELDS: usize = 6;

/// The cron fields a `?` ("no specific value") may stand in, as positions within
/// the six: the day-of-month and the day-of-week, the pair the model's examples
/// put it in.
const NO_SPECIFIC_VALUE_FIELDS: [usize; 2] = [2, 4];

/// Why the schedule `rule` states is not one AWS Backup initiates jobs from, or
/// `None` when it is one (including a rule that states none at all, which runs
/// on [`DEFAULT_SCHEDULE_EXPRESSION`]).
///
/// The sentence is the PER-RULE validation reason, which the refusal carries
/// whole in `Context` and after its own fixed prefix in `Message` (see
/// `super::rejected_backup_plan_rule`): the condition is the expression failing
/// to parse as a cron expression, so the reason states that and nothing the
/// request sent.
#[must_use]
pub fn rule_schedule_refusal(rule: &BackupRuleInput) -> Option<String> {
    let expression = rule.schedule_expression.as_deref()?;
    parse_schedule_expression(expression)
        .is_none()
        .then(|| format!("{}{}", super::MESSAGE_CRON_INVALID_PREFIX, super::MESSAGE_CRON_INVALID))
}

/// A rule's `ScheduleExpression` as a parsed schedule, or `None` when it is not
/// an expression this service can compute an execution from.
///
/// "A cron expression in UTC specifying when Backup initiates a backup job" —
/// six fields (minute, hour, day-of-month, month, day-of-week, year), which the
/// model writes both wrapped in `cron(…)` (its default expression) and bare (its
/// two examples), so both spellings read as the same schedule. The `cron` crate
/// parses the seven-field form that starts with seconds, so a literal `0`
/// seconds field is prepended; a `?` is "no specific value", which that grammar
/// spells `*`, and states nothing outside the day fields.
#[must_use]
pub fn parse_schedule_expression(expression: &str) -> Option<::cron::Schedule> {
    let expression = expression.trim();
    let body = expression
        .strip_prefix("cron(")
        .and_then(|rest| rest.strip_suffix(')'))
        .unwrap_or(expression);
    let fields: Vec<&str> = body.split_whitespace().collect();
    if fields.len() != CRON_FIELDS {
        return None;
    }
    let seconds = ::std::iter::once("0");
    let normalised: Vec<&str> = seconds
        .chain(fields.iter().enumerate().map(|(position, field)| {
            if *field == "?" && NO_SPECIFIC_VALUE_FIELDS.contains(&position) {
                "*"
            } else {
                *field
            }
        }))
        .collect();
    normalised.join(" ").parse::<::cron::Schedule>().ok()
}

/// The time zone a rule's schedule is read in: "The timezone in which the
/// schedule expression is set. By default, ScheduleExpressions are in UTC"
/// (model doc for `BackupRule$ScheduleExpressionTimezone`) — so a rule naming
/// none is read in [`DEFAULT_SCHEDULE_EXPRESSION_TIMEZONE`]. `None` when the
/// stored name resolves to no zone at all.
#[must_use]
pub fn schedule_timezone(timezone: Option<&str>) -> Option<::chrono_tz::Tz> {
    timezone
        .unwrap_or(DEFAULT_SCHEDULE_EXPRESSION_TIMEZONE)
        .parse::<::chrono_tz::Tz>()
        .ok()
}
