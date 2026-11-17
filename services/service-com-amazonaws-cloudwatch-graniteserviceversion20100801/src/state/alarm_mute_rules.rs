// SPDX-License-Identifier: CC0-1.0

//! The alarm mute rules this store holds, and the mute WINDOWS each rule's schedule names.
//!
//! One concern in one file: the stored rule ([`StoredAlarmMuteRule`]), the identity it is keyed
//! by ([`AlarmMuteRuleKey`]), the schedule a rule's `Rule` carries read as the windows it opens
//! ([`MuteWindows`]), and the `Status` a read reports from the clock
//! ([`alarm_mute_rule_status`]).
//!
//! A schedule is read through the maintained parsers rather than a second grammar written here:
//! the cron expression through `cron`, the local instant of an `at` expression and the time zone
//! it is read in through `chrono` / `chrono-tz`, and the window's length through the one ISO 8601
//! duration reading this project shares (`service_common::iso8601_span`).

use ::chrono::{DateTime, NaiveDateTime, TimeZone, Utc};
use ::chrono_tz::Tz;
use ::serde_json::{Map, Value};

use crate::ops::types::{
    AlarmHistoryItem, GetAlarmMuteRuleOutput, ReportedAlarmMuteRule, Schedule, Tag,
};

/// The identity of one alarm mute rule: the Region it lives in and its name.
///
/// A mute rule's name "must be unique within your Amazon Web Services account and region"
/// (`PutAlarmMuteRuleInput$Name`) and its ARN carries the Region
/// (`arn:aws:cloudwatch:[REGION]:123456789012:alarm-mute-rule:*`, `PutAlarmMuteRule`), so one
/// name in two Regions is two independent rules and the Region is part of the key this store
/// holds a rule under rather than merely part of what it reports.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct AlarmMuteRuleKey {
    /// The Region the rule lives in, as the request that wrote it was signed for.
    pub region: String,
    /// The rule's name.
    pub name: String,
}

/// The key one Region's rule of this name is held under.
pub fn alarm_mute_rule_key(region: &str, name: &str) -> AlarmMuteRuleKey {
    AlarmMuteRuleKey {
        region: region.to_string(),
        name: name.to_string(),
    }
}

/// An alarm mute rule's ARN:
/// `arn:aws:cloudwatch:<region>:<account>:alarm-mute-rule:<Name>`, the form
/// `PutAlarmMuteRule` documents the resource by
/// (`arn:aws:cloudwatch:[REGION]:123456789012:alarm-mute-rule:*`).
pub fn alarm_mute_rule_arn(region: &str, name: &str) -> String {
    ::engine::emulator_arn("cloudwatch", region, &format!("alarm-mute-rule:{name}"))
}

/// The largest number of alarm mute rules this in-memory store holds.
///
/// A capacity bound, not an AWS quota: the records live in this process's memory, so their number
/// has to be finite. `PutAlarmMuteRule` answers its modelled limit error rather than growing
/// without limit once the bound is reached; updating a rule that already exists adds none, so it
/// is unaffected.
pub const MAX_ALARM_MUTE_RULES: usize = 10_000;

/// The most alarms ONE `DeleteAlarmMuteRule` releases the withheld actions of.
///
/// What one request does is bounded by the number of alarms the deleted rule names, and that is a
/// value an EARLIER request chose, so the bound is applied here rather than left to the stored
/// list's length: "You can specify up to 100 alarm names"
/// (`smithy_query(query="shape",shape="MuteTargets")`, whose `AlarmNames` is modelled
/// `@length 0..100`) is the published maximum, so a rule a caller can store names no more than
/// this many and the cap costs a compliant request nothing. A list past it — which no write this
/// service accepts can produce — releases this many and says so in the log.
pub const MAX_ALARMS_RELEASED_PER_DELETE: usize = 100;

/// A mute rule that is configured and will become active in the future:
/// "SCHEDULED: The mute rule is configured and will become active in the future"
/// (`GetAlarmMuteRule`).
pub const ALARM_MUTE_RULE_SCHEDULED: &str = "SCHEDULED";

/// A mute rule that is muting alarm actions now: "ACTIVE: The mute rule is currently muting
/// alarm actions" (`GetAlarmMuteRule`).
pub const ALARM_MUTE_RULE_ACTIVE: &str = "ACTIVE";

/// A mute rule that will not become active again: "EXPIRED: The mute rule has passed its
/// expiration date and will no longer become active" (`GetAlarmMuteRule`).
pub const ALARM_MUTE_RULE_EXPIRED: &str = "EXPIRED";

/// A mute rule whose schedule opens ONE window: an `at` expression is the form documented "For
/// one-time mute windows" (`Schedule$Expression`).
///
/// `MuteType` is an unconstrained string in the model
/// (`smithy_query(query="shape",shape="MuteType")`) and its member prose names the two kinds of
/// window in prose capitals ("Indicates whether the mute rule is one-time or recurring. Valid
/// values are ONE_TIME or RECURRING.", `GetAlarmMuteRuleOutput$MuteType`), so the TOKEN is the one
/// the service emits: measured against `monitoring` in us-west-2, a read of a one-time rule
/// reports `one-time` and a read of a recurring rule `recurring`, on the single read and in a
/// listing summary alike.
pub const MUTE_TYPE_ONE_TIME: &str = "one-time";

/// A mute rule whose schedule opens a window per occurrence — the `cron` expression form,
/// documented "For recurring mute windows" (`Schedule$Expression`), in the token the service emits
/// ([`MUTE_TYPE_ONE_TIME`]).
pub const MUTE_TYPE_RECURRING: &str = "recurring";

/// A stored alarm mute rule: the modelled shape its reader returns, plus the creation identity and
/// the tags the write accepted (no member of that shape carries either).
///
/// Held as [`GetAlarmMuteRuleOutput`] itself rather than as a parallel record, so every member
/// `PutAlarmMuteRule` accepts round-trips through `GetAlarmMuteRule` without a field-by-field
/// copy that can silently drop one. Its `Status` is the one member NOT stored: the status is
/// what the CLOCK makes of the rule's schedule, so it is computed on every read
/// ([`StoredAlarmMuteRule::reported`]) rather than frozen at the instant of the write.
#[derive(Debug, Clone, Default)]
pub struct StoredAlarmMuteRule {
    /// The rule as its reader reports it, with `Status` left unset.
    pub configuration: GetAlarmMuteRuleOutput,
    /// The identity of the write that CREATED this rule, as every read of it reports the rule's
    /// creation identity ([`super::alarm_creation_id`]). A write that replaces the configuration of
    /// a rule that already exists keeps it; a rule deleted and written again is created afresh and
    /// carries a new one.
    pub creation_id: String,
    /// The tags the rule carries.
    pub tags: Vec<Tag>,
}

impl StoredAlarmMuteRule {
    /// The rule as `GetAlarmMuteRule` reports it at `now`: every member as the write left it,
    /// with the `Status` the clock decides.
    pub fn reported(&self, now: f64) -> GetAlarmMuteRuleOutput {
        let mut reported = self.configuration.clone();
        reported.status = Some(alarm_mute_rule_status(&self.configuration, now).to_string());
        reported
    }

    /// The rule as a READ of it answers with: the modelled shape [`Self::reported`] builds, and
    /// the `CreationId` of the write that created it beside it — the member the model does not
    /// declare and CloudWatch reports on every read ([`ReportedAlarmMuteRule`]).
    pub fn read(&self, now: f64) -> ReportedAlarmMuteRule {
        ReportedAlarmMuteRule {
            rule: self.reported(now),
            creation_id: Some(self.creation_id.clone()),
        }
    }

    /// Whether this rule applies to the alarm `alarm_name`: "The list of alarm names that this
    /// mute rule targets." (`MuteTargets$AlarmNames`).
    ///
    /// A rule carrying no `MuteTargets` names no alarm, so it targets none: the member is what
    /// "specifies which alarms an alarm mute rule applies to" (`MuteTargets`).
    pub fn targets(&self, alarm_name: &str) -> bool {
        self.configuration
            .mute_targets
            .as_ref()
            .is_some_and(|targets| {
                targets
                    .alarm_names
                    .iter()
                    .any(|targeted| targeted == alarm_name)
            })
    }
}

/// Which of the three statuses the rule `configuration` describes is in at `now`.
///
/// The statuses are documented by what they say about the rule rather than by a transition:
/// ACTIVE is "currently muting alarm actions", SCHEDULED is "configured and will become active in
/// the future", and EXPIRED is "has passed its expiration date and will no longer become active"
/// (`GetAlarmMuteRule`). So the question each is answered from is whether a mute window of the
/// rule's schedule holds `now`, whether one is still to come, or whether none ever will be:
///
/// * a rule past its `ExpireDate` "is no longer evaluated" (`GetAlarmMuteRuleOutput$ExpireDate`),
///   so no window of it can open again: EXPIRED;
/// * otherwise, a window opens at each instant the schedule names at or after the rule's
///   `StartDate` ("the date and time after which the mute rule takes effect",
///   `PutAlarmMuteRuleInput$StartDate`) and stays open for the schedule's `Duration` ("the length
///   of time that alarms remain muted when the schedule activates", `Schedule$Duration`). The
///   FIRST window to open after `now - Duration` is the only one that can hold `now`, since every
///   earlier one has closed: it holds `now` when it opens at or before it (ACTIVE) and is still to
///   come when it opens after it (SCHEDULED);
/// * a schedule with no window left to open at all — a one-time window already closed — can never
///   make the rule active again, which is the same statement EXPIRED makes: EXPIRED.
///
/// A configuration whose schedule this cannot read names no window, and so no window holding
/// `now`: it reports SCHEDULED, the status of a rule that is not muting anything. A write is
/// refused unless its schedule reads ([`alarm_mute_rule_schedule_error`]), so this is the status
/// of nothing a caller can store.
pub fn alarm_mute_rule_status(configuration: &GetAlarmMuteRuleOutput, now: f64) -> &'static str {
    if configuration
        .expire_date
        .as_ref()
        .map(super::from_timestamp)
        .is_some_and(|expires_at| now >= expires_at)
    {
        return ALARM_MUTE_RULE_EXPIRED;
    }
    let Some(windows) = configuration
        .rule
        .as_ref()
        .and_then(|rule| MuteWindows::of(&rule.schedule))
    else {
        return ALARM_MUTE_RULE_SCHEDULED;
    };
    // The earliest instant a window that could still hold `now` opens at: one that opened a whole
    // duration ago has closed, and one that opened before the rule took effect never opened.
    let takes_effect_at = configuration
        .start_date
        .as_ref()
        .map_or(f64::NEG_INFINITY, super::from_timestamp);
    let floor = (now - windows.duration_secs).max(takes_effect_at);
    match windows.opens_after(floor) {
        Some(opens_at) if opens_at <= now => ALARM_MUTE_RULE_ACTIVE,
        Some(_) => ALARM_MUTE_RULE_SCHEDULED,
        None => ALARM_MUTE_RULE_EXPIRED,
    }
}

/// The mute question this store answers about one alarm: whether a rule it holds is muting that
/// alarm's actions right now.
impl super::ComAmazonawsCloudwatchGraniteserviceversion20100801State {
    /// The name of the ACTIVE mute rule targeting `alarm_name` at `now`, or `None` when the
    /// alarm's actions are muted by nothing.
    ///
    /// "When a mute rule is active, the targeted alarms continue to evaluate metrics and
    /// transition between states, but their configured actions are muted." (`MuteTargets`) — so
    /// the question a transition asks is whether a rule that NAMES this alarm among its targets
    /// (`MuteTargets$AlarmNames`, "The list of alarm names that this mute rule targets") is ACTIVE
    /// at the instant of the transition, which is the one status reading every mute-rule operation
    /// shares ([`alarm_mute_rule_status`]).
    ///
    /// A rule is a resource of ONE Region ([`AlarmMuteRuleKey`]), and so is an alarm — its ARN
    /// carries the Region it was created in — so only the rules of the alarm's own Region can mute
    /// it. An alarm whose ARN names no Region is matched against every rule, because nothing then
    /// says which Region's rules are not its own.
    ///
    /// Several rules may target one alarm; the one reported is the first in name order, so what the
    /// history records about a muted transition does not depend on the store's iteration order.
    pub fn muting_alarm_mute_rule(
        &self,
        alarm_name: &str,
        alarm_arn: Option<&str>,
        now: f64,
    ) -> Option<String> {
        let region = alarm_arn
            .and_then(::service_common::arn::parse)
            .map(|arn| arn.region.to_string());
        self.alarm_mute_rules
            .iter()
            .filter(|(key, _)| region.as_ref().is_none_or(|region| &key.region == region))
            .filter(|(_, stored)| stored.targets(alarm_name))
            .filter(|(_, stored)| {
                alarm_mute_rule_status(&stored.configuration, now) == ALARM_MUTE_RULE_ACTIVE
            })
            .map(|(key, _)| key.name.clone())
            .min()
    }

    /// DELETE the mute rule `name` holds in `region`, and release the actions it was muting.
    ///
    /// "Deletes a specific alarm mute rule. When you delete a mute rule, any alarms that are
    /// currently being muted by that rule are immediately unmuted. If those alarms are in an
    /// ALARM state, their configured actions will trigger. This operation is idempotent. If you
    /// delete a mute rule that does not exist, the operation succeeds without returning an
    /// error." (`DeleteAlarmMuteRule`) — so the delete is the whole of it:
    ///
    /// * the rule is GONE, so nothing it targeted is muted by it any more: the mute question is
    ///   answered from the rules the store holds ([`Self::muting_alarm_mute_rule`]), which is why
    ///   removing the record is all the unmuting there is to do, and a name no rule of this
    ///   Region carries removes nothing and is no error;
    /// * an alarm the rule WAS muting that stands in `ALARM` has the actions configured for that
    ///   state executed now — the ones the mute withheld — queued for the settle that delivers
    ///   them ([`super::alarm_actions::settle_alarms`]). Only a rule that was ACTIVE was muting
    ///   anything ("ACTIVE: The mute rule is currently muting alarm actions",
    ///   `GetAlarmMuteRule`), and an alarm another rule still mutes stays muted, so the question
    ///   is asked again once this rule is out of the store.
    ///
    /// The alarm does not MOVE: it was already in `ALARM` and still is, so no history item
    /// records a transition and no state-change event announces one
    /// ([`super::alarm_actions::AlarmNotification::publishes_state_change`]). What is released is
    /// the execution of the actions.
    pub fn delete_alarm_mute_rule(&mut self, region: &str, name: &str, now: f64) {
        let Some(removed) = self
            .alarm_mute_rules
            .remove(&alarm_mute_rule_key(region, name))
        else {
            return;
        };
        if alarm_mute_rule_status(&removed.configuration, now) != ALARM_MUTE_RULE_ACTIVE {
            return;
        }
        let named = removed
            .configuration
            .mute_targets
            .as_ref()
            .map_or(0, |targets| targets.alarm_names.len());
        if named > MAX_ALARMS_RELEASED_PER_DELETE {
            ::tracing::warn!(
                rule = %name,
                targeted = named,
                released = MAX_ALARMS_RELEASED_PER_DELETE,
                "this alarm mute rule names more alarms than one delete releases the actions \
                 of; the rest keep the actions the mute withheld"
            );
        }
        let targeted: Vec<String> = removed
            .configuration
            .mute_targets
            .as_ref()
            .map(|targets| {
                targets
                    .alarm_names
                    .iter()
                    .take(MAX_ALARMS_RELEASED_PER_DELETE)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        // Which alarms the rules LEFT in the store still mute, read ONCE: the question is asked
        // for every alarm this delete releases, and answering it from the store per alarm would
        // re-read every rule it holds and re-parse that rule's schedule for each of them
        // ([`MuteWindows::of`]), so the work would grow with the product of the two.
        let still_muting = self.active_mute_targets(now);
        let released: Vec<super::alarm_actions::AlarmNotification> = targeted
            .iter()
            .filter_map(|alarm_name| self.released_alarm_actions(alarm_name, now, &still_muting))
            .collect();
        for notification in released {
            self.queue_notification(notification);
        }
    }

    /// The ACTIVE mute rules this store holds, each as the Region it lives in beside the alarm
    /// names it targets ([`StoredAlarmMuteRule::targets`]).
    ///
    /// Read in ONE pass over the rules, so a caller asking the mute question about several alarms
    /// reads the store — and reads each rule's schedule — once rather than once per alarm.
    fn active_mute_targets(&self, now: f64) -> Vec<(String, Vec<String>)> {
        self.alarm_mute_rules
            .iter()
            .filter(|(_, stored)| {
                alarm_mute_rule_status(&stored.configuration, now) == ALARM_MUTE_RULE_ACTIVE
            })
            .map(|(key, stored)| {
                let targets = stored
                    .configuration
                    .mute_targets
                    .as_ref()
                    .map(|targets| targets.alarm_names.clone())
                    .unwrap_or_default();
                (key.region.clone(), targets)
            })
            .collect()
    }

    /// The record of the actions a released alarm executes now: the ones configured for the
    /// `ALARM` state it stands in, for an alarm that no longer has a mute rule withholding them.
    ///
    /// `None` for an alarm in any other state (there is nothing configured for a state it is not
    /// in), for one no store holds under that name, for one an ACTIVE rule of `still_muting`
    /// mutes, and for one that carries no action for `ALARM` at all — a record of no action is a
    /// delivery with nothing to deliver.
    fn released_alarm_actions(
        &self,
        alarm_name: &str,
        now: f64,
        still_muting: &[(String, Vec<String>)],
    ) -> Option<super::alarm_actions::AlarmNotification> {
        let released = if let Some(stored) = self.alarms.get(alarm_name) {
            super::alarm_actions::AlarmNotification::of_transition(
                &stored.alarm,
                // The state the alarm stands in is the state it moved to: the actions released
                // are the ones that state was configured with, and nothing moved.
                &super::AlarmStateSnapshot::of_metric_alarm(&stored.alarm),
                now,
            )
        } else if let Some(stored) = self.composite_alarms.get(alarm_name) {
            super::alarm_actions::AlarmNotification::of_composite_transition(
                &stored.alarm,
                &self.child_alarm_states(&stored.child_names),
                &super::AlarmStateSnapshot::of_composite_alarm(&stored.alarm),
                now,
            )
        } else if let Some(stored) = self.log_alarms.get(alarm_name) {
            // A log alarm's record names the EXECUTION whose aggregate decided the state it is
            // in, which is its newest run.
            stored.query_executions.last().and_then(|execution| {
                super::alarm_actions::AlarmNotification::of_log_transition(
                    &stored.alarm,
                    &execution.query_id,
                    &super::AlarmStateSnapshot::of_log_alarm(&stored.alarm),
                    now,
                )
            })
        } else {
            None
        };
        released
            .filter(|released| released.new_state_value == super::ALARM_STATE_ALARM)
            .filter(|released| !released.actions.is_empty())
            .filter(|released| {
                !muted_by(still_muting, alarm_name, released.alarm_arn.as_deref())
            })
            .map(|released| super::alarm_actions::AlarmNotification {
                // No state change happened, so none is announced.
                publishes_state_change: false,
                ..released
            })
    }
}

/// Whether one of the ACTIVE rules `still_muting` names mutes the alarm `alarm_name`, whose own
/// ARN is `alarm_arn`.
///
/// The same question [`super::ComAmazonawsCloudwatchGraniteserviceversion20100801State::muting_alarm_mute_rule`]
/// answers, over rules already read: a rule is a resource of ONE Region and so is an alarm, so
/// only the rules of the alarm's own Region can mute it, and an alarm whose ARN names no Region
/// is matched against every rule.
fn muted_by(
    still_muting: &[(String, Vec<String>)],
    alarm_name: &str,
    alarm_arn: Option<&str>,
) -> bool {
    let region = alarm_arn
        .and_then(::service_common::arn::parse)
        .map(|arn| arn.region.to_string());
    still_muting
        .iter()
        .filter(|(rule_region, _)| region.as_ref().is_none_or(|region| rule_region == region))
        .any(|(_, targets)| targets.iter().any(|targeted| targeted == alarm_name))
}

/// The `HistoryItemType` a muted transition is recorded under: `Action` is the type every record
/// of what became of a transition's actions carries (`smithy_query(query="shape",
/// shape="HistoryItemType")`), and a mute is what became of these.
const MUTED_HISTORY_ITEM_TYPE: &str = "Action";

/// What the record of a muted transition states happened to the actions, beside the `Succeeded`
/// and `Failed` an executed action is recorded with.
const MUTED_ACTION_STATE: &str = "Muted";

/// The history item recording that a transition's configured actions were MUTED by a mute rule,
/// rather than executed.
///
/// "When a mute rule is active, the targeted alarms continue to evaluate metrics and transition
/// between states, but their configured actions […] are muted." (`PutAlarmMuteRule`) — the
/// transition happens and is recorded as itself, and this is the account of the actions it did not
/// execute: which rule muted them, which state they were configured for, and which ARNs they were.
/// Without it a muted transition is indistinguishable, in the only record an operator has, from an
/// action that was executed and failed.
pub fn muted_actions_history_item(
    alarm_name: &str,
    alarm_type: &str,
    rule_name: &str,
    new_state_value: &str,
    actions: &[String],
    now_secs: f64,
) -> AlarmHistoryItem {
    let mut document = Map::new();
    document.insert(
        "version".to_string(),
        Value::from(super::HISTORY_DATA_VERSION),
    );
    document.insert(
        "actionState".to_string(),
        Value::from(MUTED_ACTION_STATE),
    );
    document.insert("muteRuleName".to_string(), Value::from(rule_name));
    document.insert("stateValue".to_string(), Value::from(new_state_value));
    let muted = Value::from(
        actions
            .iter()
            .map(|action| Value::from(action.as_str()))
            .collect::<Vec<Value>>(),
    );
    let mut with_actions = document.clone();
    with_actions.insert("mutedActions".to_string(), muted);
    AlarmHistoryItem {
        alarm_name: Some(alarm_name.to_string()),
        alarm_contributor_id: None,
        // The item belongs to the kind of alarm whose actions were muted, so a reader that asks
        // for one kind of alarm's history sees it.
        alarm_type: Some(alarm_type.to_string()),
        timestamp: Some(super::to_timestamp(now_secs)),
        history_item_type: Some(MUTED_HISTORY_ITEM_TYPE.to_string()),
        // A rule's `Name` is `@length 1..255`, so a summary naming one need not fit the summary's
        // own bound: it is cut at it by the one reading of that bound this store shares.
        history_summary: Some(super::alarm_actions::bounded(format!(
            "Alarm actions muted by alarm mute rule {rule_name}"
        ))),
        // An action ARN is `@length 1..1024` and a state may carry five of them, so the list of
        // muted ARNs is the part of this document that need not fit the modelled `HistoryData`
        // length: it is dropped rather than truncated, because a document is parsed rather than
        // read, and what identifies the mute — the rule's own name — is in both renderings.
        history_data: Some(super::bounded_history_data(&[
            Value::Object(with_actions),
            Value::Object(document),
        ])),
        alarm_contributor_attributes: None,
    }
}

/// The mute windows one rule's `Schedule` names: when each opens, and how long it stays open.
pub struct MuteWindows {
    /// The instants the windows open at, as the schedule's expression states them.
    form: ScheduleForm,
    /// The zone the expression is read in: "The time zone to use when evaluating the schedule
    /// expression […] If you don't specify a time zone, UTC is used by default."
    /// (`Schedule$Timezone`).
    timezone: Tz,
    /// How long each window stays open, from the instant it opens (`Schedule$Duration`).
    duration_secs: f64,
}

/// The two expression forms a mute schedule states its instants in (`Schedule$Expression`).
enum ScheduleForm {
    /// `at(yyyy-MM-ddThh:mm)` — one window, opening at the local instant the expression names.
    Once(NaiveDateTime),
    /// `cron(Minutes Hours Day-of-month Month Day-of-week)` — one window per occurrence. Boxed
    /// because the parsed cron schedule is far larger than the local instant beside it.
    Recurring(Box<::cron::Schedule>),
}

/// The format an `at` expression states its instant in: "Format: `at(yyyy-MM-ddThh:mm)`"
/// (`Schedule$Expression`) — a LOCAL instant, read in the schedule's own time zone, carrying no
/// zone of its own.
const AT_EXPRESSION_FORMAT: &str = "%Y-%m-%dT%H:%M";

/// How many fields a mute schedule's cron expression carries: "Format: `cron(Minutes Hours
/// Day-of-month Month Day-of-week)`" (`Schedule$Expression`).
const CRON_FIELDS: usize = 5;

/// The seconds field the shared cron grammar reads an expression's instants from, which the
/// five documented fields carry no place for: a mute window opens on the minute the expression
/// names.
const CRON_SECONDS_FIELD: &str = "0";

impl MuteWindows {
    /// The windows `schedule` names, or `None` when it names none this emulator can read — an
    /// expression in neither documented form, a time zone no IANA zone answers to, or a duration
    /// naming no fixed span.
    pub fn of(schedule: &Schedule) -> Option<Self> {
        Some(Self {
            form: schedule_form(&schedule.expression)?,
            timezone: schedule_timezone(schedule.timezone.as_deref())?,
            duration_secs: schedule_duration_secs(&schedule.duration)?,
        })
    }

    /// Whether these windows recur or there is only ever one of them — the rule's `MuteType`.
    pub fn mute_type(&self) -> &'static str {
        match self.form {
            ScheduleForm::Once(_) => MUTE_TYPE_ONE_TIME,
            ScheduleForm::Recurring(_) => MUTE_TYPE_RECURRING,
        }
    }

    /// The first instant strictly after `after` (epoch seconds) at which one of these windows
    /// opens, or `None` when none ever opens again.
    fn opens_after(&self, after: f64) -> Option<f64> {
        let after = instant(after)?.with_timezone(&self.timezone);
        let opens_at = match &self.form {
            ScheduleForm::Once(local) => {
                // The expression names a local wall-clock instant; the zone turns it into the
                // moment it happens at. A local instant its zone skips (the spring-forward gap)
                // is a moment that never happens, so the window never opens.
                let opens_at = self.timezone.from_local_datetime(local).earliest()?;
                (opens_at > after).then_some(opens_at)?
            }
            ScheduleForm::Recurring(schedule) => schedule.after(&after).next()?,
        };
        Some(epoch_secs(&opens_at))
    }
}

/// The instants `expression` names, or `None` when it is in neither documented form.
///
/// "You can use one of two expression formats: Cron expressions - For recurring mute windows.
/// Format: `cron(Minutes Hours Day-of-month Month Day-of-week)` […] At expressions - For one-time
/// mute windows. Format: `at(yyyy-MM-ddThh:mm)`" (`Schedule$Expression`). The cron body is read by
/// the shared grammar, which takes the seconds field the documented five do not carry.
fn schedule_form(expression: &str) -> Option<ScheduleForm> {
    let inside = expression.trim().strip_suffix(')')?;
    if let Some(at) = inside.strip_prefix("at(") {
        return NaiveDateTime::parse_from_str(at.trim(), AT_EXPRESSION_FORMAT)
            .ok()
            .map(ScheduleForm::Once);
    }
    let fields: Vec<&str> = inside.strip_prefix("cron(")?.split_whitespace().collect();
    if fields.len() != CRON_FIELDS {
        return None;
    }
    format!("{CRON_SECONDS_FIELD} {}", fields.join(" "))
        .parse::<::cron::Schedule>()
        .ok()
        .map(|schedule| ScheduleForm::Recurring(Box::new(schedule)))
}

/// The zone a schedule with no `Timezone` of its own is evaluated in: "If you don't specify a time
/// zone, UTC is used by default." (`Schedule$Timezone`).
///
/// The default is what the SCHEDULE means, not what a reader guesses, so a write materialises it
/// onto the schedule it stores ([`scheduled_in_its_timezone`]) and every reader of the rule reports
/// the zone its windows are actually read in.
pub const DEFAULT_SCHEDULE_TIMEZONE: &str = "UTC";

/// `schedule` as a write stores it: the schedule as submitted, carrying the zone it is evaluated in
/// even when the request named none ([`DEFAULT_SCHEDULE_TIMEZONE`]).
pub fn scheduled_in_its_timezone(schedule: &Schedule) -> Schedule {
    Schedule {
        expression: schedule.expression.clone(),
        duration: schedule.duration.clone(),
        timezone: Some(
            schedule
                .timezone
                .clone()
                .unwrap_or_else(|| DEFAULT_SCHEDULE_TIMEZONE.to_string()),
        ),
    }
}

/// The zone a schedule is evaluated in: the one it names, or UTC when it names none
/// ([`DEFAULT_SCHEDULE_TIMEZONE`]). `None` for a value that names no zone at all.
fn schedule_timezone(timezone: Option<&str>) -> Option<Tz> {
    match timezone {
        None => Some(Tz::UTC),
        Some(named) => named.parse::<Tz>().ok(),
    }
}

/// How long a window of this schedule stays open, or `None` for a `Duration` naming no fixed
/// span — read through the one ISO 8601 duration reading this project shares, so a mute window's
/// length is read exactly as every other service reads a duration member.
fn schedule_duration_secs(duration: &str) -> Option<f64> {
    ::service_common::iso8601_span::iso8601_span(duration).map(|span| span.as_secs_f64())
}

/// `secs` as the moment the schedule grammars measure from, or `None` for a value no instant
/// answers to.
fn instant(secs: f64) -> Option<DateTime<Utc>> {
    if !secs.is_finite() {
        return None;
    }
    #[allow(clippy::cast_possible_truncation)]
    DateTime::from_timestamp(secs.floor() as i64, 0)
}

/// One of those moments back as the epoch seconds this store measures its clock in.
fn epoch_secs<Zone: TimeZone>(at: &DateTime<Zone>) -> f64 {
    #[allow(clippy::cast_precision_loss)]
    let secs = at.timestamp() as f64;
    secs
}

/// The shortest span a mute window may stay open, and the longest: "Specify the duration using
/// ISO 8601 duration format with a minimum of 1 minute (`PT1M`) and maximum of 15 days (`P15D`)."
/// (`Schedule$Duration`).
pub const MIN_MUTE_DURATION_SECS: f64 = 60.0;

/// The other end of that published range (`Schedule$Duration`).
pub const MAX_MUTE_DURATION_SECS: f64 = 15.0 * 24.0 * 60.0 * 60.0;

/// The request member path a refusal of the window's length names.
const SCHEDULE_DURATION_PARAMETER: &str = "Rule.Schedule.Duration";

/// The request member path a refusal of the time zone names.
const SCHEDULE_TIMEZONE_PARAMETER: &str = "Rule.Schedule.Timezone";

/// How CloudWatch states that a schedule's expression is in neither form a mute window may be
/// stated in: one sentence about the SCHEDULE, carrying nothing of the expression the request
/// submitted.
///
/// "You can use one of two expression formats: Cron expressions […] At expressions"
/// (`Schedule$Expression`) — an expression in neither form leaves the schedule unreadable rather
/// than merely out of range, which is what this sentence says and why it names no value.
pub const SCHEDULE_MALFORMED_MESSAGE: &str = "Schedule is malformed.";

/// How CloudWatch states that a mute window would stay open longer than the longest published:
/// one sentence naming that published maximum, carrying nothing of the duration submitted.
///
/// "with a minimum of 1 minute (`PT1M`) and maximum of 15 days (`P15D`)" (`Schedule$Duration`) —
/// the cap the sentence states is that published maximum ([`MAX_MUTE_DURATION_SECS`]).
pub const MUTE_DURATION_TOO_LONG_MESSAGE: &str =
    "The Alarm Mute Rule Duration cannot exceed 15 days.";

/// How CloudWatch states that the rule a write asks for is expired before it ever mutes
/// anything — its window closes no later than the instant it takes effect.
pub const MUTE_RULE_EXPIRED_MESSAGE: &str = "The Alarm Mute Rule is expired.";

/// How CloudWatch states that a ONE-TIME schedule cannot be given window boundaries of its own.
pub const AT_EXPRESSION_DATES_MESSAGE: &str =
    "Can not set start or expire dates for At expressions";

/// Why `schedule` names no mute window this service can run, in the sentence CloudWatch states
/// that refusal in, or `None` when it names one.
///
/// Three rules the model cannot express, so the boundary walk lets all three through: the
/// expression must be in one of the two documented forms, the time zone must be a zone, and the
/// duration must name a fixed span inside the published range
/// ([`MIN_MUTE_DURATION_SECS`]..[`MAX_MUTE_DURATION_SECS`]). Stated here rather than in the
/// handler so every operation that stores or reads a schedule is judged by one rule.
///
/// Two of those have a sentence of their own, which is fixed text of their class and interpolates
/// nothing: an unreadable expression ([`SCHEDULE_MALFORMED_MESSAGE`]) and a window longer than the
/// published maximum ([`MUTE_DURATION_TOO_LONG_MESSAGE`]). Every other unusable value — a zone no
/// zone answers to, a duration that names no fixed span or one shorter than the published minimum
/// — is refused in the sentence this service states an unusable parameter value in
/// ([`super::value_is_invalid_message`]), naming the member that has to change.
pub fn alarm_mute_rule_schedule_error(schedule: &Schedule) -> Option<String> {
    if schedule_form(&schedule.expression).is_none() {
        return Some(SCHEDULE_MALFORMED_MESSAGE.to_string());
    }
    if schedule_timezone(schedule.timezone.as_deref()).is_none() {
        return Some(super::value_is_invalid_message(
            schedule.timezone.as_deref().unwrap_or_default(),
            SCHEDULE_TIMEZONE_PARAMETER,
        ));
    }
    match schedule_duration_secs(&schedule.duration) {
        Some(span) if span > MAX_MUTE_DURATION_SECS => {
            Some(MUTE_DURATION_TOO_LONG_MESSAGE.to_string())
        }
        Some(span) if span >= MIN_MUTE_DURATION_SECS => None,
        _ => Some(super::value_is_invalid_message(
            &schedule.duration,
            SCHEDULE_DURATION_PARAMETER,
        )),
    }
}

/// Why the mute window a write asks for can never mute anything, in the sentence CloudWatch
/// states that refusal in, or `None` when the request's schedule and its window boundaries hold
/// together.
///
/// Two rules BETWEEN members, which no single member's constraint can express:
///
/// * a ONE-TIME schedule states the instant its single window opens inside the expression itself
///   ("At expressions - For one-time mute windows. Format: `at(yyyy-MM-ddThh:mm)`",
///   `Schedule$Expression`), so there is no window for a `StartDate` to let in or an `ExpireDate`
///   to cut short: a request carrying either beside such an expression is refused
///   ([`AT_EXPRESSION_DATES_MESSAGE`]);
/// * the window a rule mutes in runs from the instant it takes effect ("The date and time after
///   which the mute rule takes effect", `PutAlarmMuteRuleInput$StartDate`) to the instant it is
///   "no longer evaluated" (`PutAlarmMuteRuleInput$ExpireDate`), so a request whose `ExpireDate`
///   is no later than its `StartDate` asks for a rule that is already expired when it is written
///   ([`MUTE_RULE_EXPIRED_MESSAGE`]).
///
/// A request naming only an `ExpireDate` states no instant to compare it against — such a rule
/// "takes effect immediately upon creation" (`PutAlarmMuteRuleInput$StartDate`) — so it is stored
/// and the clock decides its `Status` ([`alarm_mute_rule_status`]) rather than being refused here.
pub fn alarm_mute_rule_window_error(
    schedule: &Schedule,
    start_date: Option<f64>,
    expire_date: Option<f64>,
) -> Option<String> {
    if matches!(schedule_form(&schedule.expression), Some(ScheduleForm::Once(_)))
        && (start_date.is_some() || expire_date.is_some())
    {
        return Some(AT_EXPRESSION_DATES_MESSAGE.to_string());
    }
    let (Some(start), Some(expires)) = (start_date, expire_date) else {
        return None;
    };
    (expires <= start).then(|| MUTE_RULE_EXPIRED_MESSAGE.to_string())
}
