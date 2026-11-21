// SPDX-License-Identifier: CC0-1.0

//! The USAGE a usage plan's quota is metered against: how many requests each API key of a
//! plan sent on each day, and the daily `[used quota, remaining quota]` log a read of that
//! usage reports from it.
//!
//! A usage plan carries two kinds of limit, and this module holds the second one's
//! accounting. The `throttle` is a RATE — the token bucket beside this
//! ([`super::ThrottleBucket`]) admits it — while the `quota` is a COUNT: "the target
//! maximum number of requests that can be made in a given time period"
//! (`QuotaSettings$limit`), counted per `(usage plan, API key)` pair for the same reason
//! the throttle is, and reset at the start of every period the plan's `period` names.
//!
//! Both halves of the count live here so the `execute-api` data plane that WRITES it
//! (`crate::service::invoke`, one count per request a plan admitted) and the read that
//! REPORTS it answer from one rule: what the report calls used is what the data plane
//! counted, and what it calls remaining is what the next request would be admitted
//! against.

use std::collections::BTreeMap;

use ::chrono::{Datelike, Days, Months, NaiveDate};

use super::{metering_key, ComAmazonawsApigatewayBackplanecontrolserviceState, UsagePlanEntry};

/// The format every date of this surface travels in: `GetUsage`'s `startDate` and
/// `endDate` are "the starting date (e.g., 2016-01-01)" and "the ending date (e.g.,
/// 2016-12-31) of the usage data" (`smithy_query(query="doc", shape="GetUsageRequest",
/// member="startDate")`), and the reported `startDate`/`endDate` are the same days in the
/// same spelling.
const USAGE_DATE_FORMAT: &str = "%Y-%m-%d";

/// The day `value` names, or `None` for a value that is no such date.
///
/// Whole days only, in the one format above: a value carrying a time, an offset or any
/// other calendar spelling names no day of the usage data and is refused
/// ([`INVALID_USAGE_START_DATE`]).
pub fn usage_date(value: &str) -> Option<NaiveDate> {
    NaiveDate::parse_from_str(value, USAGE_DATE_FORMAT).ok()
}

/// `day` as this surface spells a date, so a reported `startDate`/`endDate` is the day the
/// report covers rather than the bytes the request happened to carry.
pub fn usage_date_string(day: NaiveDate) -> String {
    day.format(USAGE_DATE_FORMAT).to_string()
}

/// The UTC day `at` falls on — the day a metered request is counted against.
///
/// UTC because a usage plan's quota is: a period "resets at midnight UTC", so the day a
/// request belongs to is its UTC day and not the day of any local zone.
pub fn usage_day_of(at: ::std::time::SystemTime) -> NaiveDate {
    ::chrono::DateTime::<::chrono::Utc>::from(at).date_naive()
}

/// The UTC day now, as the data plane counts a request it is metering.
pub fn usage_today() -> NaiveDate {
    usage_day_of(::std::time::SystemTime::now())
}

/// The widest interval of days one read of the usage data may cover.
///
/// The window API Gateway's usage data is available over: an interval wider than it names
/// days no usage could be reported for, and is refused as a bad request
/// ([`usage_interval_too_wide`], which states this same count of days). It also bounds what
/// is KEPT: a count older than this is dropped as the data plane records a newer one
/// ([`UsageLog::record`]), so the store holds exactly the days a report may cover.
pub const MAX_USAGE_INTERVAL_DAYS: u64 = 90;

/// The requests one metered `(usage plan, API key)` pair sent, counted by the UTC day each
/// one fell on.
///
/// Only the days the pair SENT something on are held — a day with no entry is a day of no
/// requests, which is what a report of zero used is — so the log of an idle key costs
/// nothing and a busy one grows by one entry per day, bounded by
/// [`MAX_USAGE_INTERVAL_DAYS`].
#[derive(Debug, Clone, Default)]
pub struct UsageLog {
    /// The request count of each day the pair sent a request on.
    pub per_day: BTreeMap<NaiveDate, i64>,
    /// What a grant of remaining quota RECORDED for this pair, keyed by the day the grant
    /// was made on.
    ///
    /// Keyed by the day of the grant, and carrying the day the grant reaches through, because
    /// a grant is "a temporary extension to the remaining quota" (`UpdateUsage`'s own
    /// documentation) of the metering period it was made in: it is a RECORDED FACT about
    /// those days, so it is held as the plan's quota stood when the grant was made and is
    /// read back unchanged however the plan's `quota` is edited afterwards. It is gone once
    /// the period it was granted in ends, which is what makes the plan's quota whole again —
    /// and no more than whole — at the next reset. Bounded like [`Self::per_day`], by
    /// [`MAX_USAGE_INTERVAL_DAYS`].
    pub grants: BTreeMap<NaiveDate, GrantedRemaining>,
}

/// One grant of remaining quota, as the grant itself states it.
///
/// What is stored is what the grant SAYS — the requests the pair has left from the grant on
/// — rather than a difference against the plan's budget at grant time: a difference would be
/// rewritten by the next edit of the plan's `quota`, and the remaining quota of a day the
/// pair already has usage data for is a fact about that day.
#[derive(Debug, Clone, Copy)]
pub struct GrantedRemaining {
    /// The requests the metering period admits from the pair from the grant on — the
    /// `remaining` the grant states.
    pub remaining: i64,
    /// The last day of the metering period the grant was made in, as the plan's quota stood
    /// when it was made: the grant covers that period and no later one, so the day it reaches
    /// through is recorded with it rather than recomputed from the plan's present `period`.
    pub through: NaiveDate,
    /// How many requests the pair had already sent on the day of the grant when it was made,
    /// so what counts the granted remaining down is the requests that came AFTER the grant.
    pub sent_by_grant: i64,
}

impl UsageLog {
    /// How many requests the pair sent on `day`.
    pub fn used_on(&self, day: NaiveDate) -> i64 {
        self.per_day.get(&day).copied().unwrap_or_default()
    }

    /// The grant whose reach covers `day`, with the day it was made on — the LATEST grant
    /// made on or before that day that still reaches it — or `None` for a day no grant
    /// covers.
    ///
    /// The latest one because a grant states the remaining quota outright, so a later grant
    /// is the one that stands; a day before every grant is a day no grant had been made yet,
    /// and a day past a grant's own period is a day its extension no longer reaches.
    pub fn grant_covering(&self, day: NaiveDate) -> Option<(NaiveDate, GrantedRemaining)> {
        let (granted_on, grant) = self.grants.range(..=day).next_back()?;
        (day <= grant.through).then_some((*granted_on, *grant))
    }

    /// Record `grant`, made on `granted_on`, REPLACING whatever that day had been granted
    /// before: the remaining quota a grant names is the one the pair has from then on, not an
    /// increment on an earlier grant.
    ///
    /// Forgets the grants of days older than the window usage data is kept over, for the
    /// reason [`Self::record`] forgets a count.
    fn grant(&mut self, granted_on: NaiveDate, grant: GrantedRemaining) {
        self.grants.insert(granted_on, grant);
        self.forget_before(granted_on);
    }

    /// How many requests the pair sent from `from` through `through`, both days included —
    /// what a quota period's own limit is counted down by.
    pub fn used_between(&self, from: NaiveDate, through: NaiveDate) -> i64 {
        self.per_day
            .range(from..=through)
            .map(|(_, count)| *count)
            .fold(0i64, i64::saturating_add)
    }

    /// Count one request sent on `day`, forgetting the counts that fell outside the window
    /// a report may cover ([`MAX_USAGE_INTERVAL_DAYS`]).
    fn record(&mut self, day: NaiveDate) {
        let counted = self.per_day.entry(day).or_default();
        *counted = counted.saturating_add(1);
        self.forget_before(day);
    }

    /// Open this pair's usage record for `day` without counting a request against it: the
    /// day becomes one the pair HAS usage data on, of zero requests so far.
    ///
    /// What a grant of remaining quota does beside writing the grant itself — the usage data
    /// the extension applies to is data the pair has from then on, which is what makes the
    /// grant visible in a read of the usage over that day.
    fn open(&mut self, day: NaiveDate) {
        self.per_day.entry(day).or_default();
        self.forget_before(day);
    }

    /// Forget the days and the grants that fell outside the window usage data is kept over,
    /// counted back from `day`.
    fn forget_before(&mut self, day: NaiveDate) {
        if let Some(oldest) = day.checked_sub_days(Days::new(MAX_USAGE_INTERVAL_DAYS)) {
            self.per_day.retain(|counted, _| *counted > oldest);
            self.grants.retain(|granted, _| *granted > oldest);
        }
    }
}

/// The quota window a day falls in: when the metering period that holds it began, and how
/// many requests the plan admits in that period.
#[derive(Debug, Clone, Copy)]
pub struct QuotaWindow {
    /// The first day of the period — where the count the limit is applied to starts, which
    /// is what makes the quota RESET at a period boundary.
    pub start: NaiveDate,
    /// The last day of the period — the day a grant made in it reaches through, after which
    /// the next period starts the count again.
    pub through: NaiveDate,
    /// The requests admissible in the period: the plan's `limit`, less the `offset` in the
    /// initial period.
    pub budget: i64,
}

/// The quota window `day` falls in for `plan`, or `None` for a plan that meters no quota in
/// a window — one carrying no `quota` at all, and one whose quota names no `period`, which
/// is a limit with no window to apply it in.
///
/// `offset` is "the number of requests subtracted from the given limit in the initial time
/// period" (`QuotaSettings$offset`), and the INITIAL period is the one the plan's own
/// creation day falls in: every later period starts at the whole limit again, which is the
/// reset the period names.
pub fn quota_window(plan: &UsagePlanEntry, day: NaiveDate) -> Option<QuotaWindow> {
    let quota = plan.quota.as_ref()?;
    let period = quota.period.as_deref()?;
    let start = quota_period_start(period, day)?;
    let through = quota_period_end(period, day)?;
    let initial = plan
        .created_on
        .and_then(|created| quota_period_start(period, created))
        .is_some_and(|created_period| created_period == start);
    let offset = if initial { i64::from(quota.offset) } else { 0 };
    Some(QuotaWindow {
        start,
        through,
        budget: i64::from(quota.limit).saturating_sub(offset).max(0),
    })
}

/// The first day of the metering `period` that `day` falls in, or `None` for a period this
/// service knows no window for.
///
/// The periods are the modelled `QuotaPeriodType` values, matched as
/// [`super::max_quota_offset`] matches them. Each window is the CALENDAR one its name
/// gives it — a day, the week its day starts the count of, or the month it is a day of —
/// because a quota "resets at midnight UTC" at the start of its period rather than at the
/// end of a rolling count: that is what makes the first day of a month or a week the day a
/// quota is whole again.
pub fn quota_period_start(period: &str, day: NaiveDate) -> Option<NaiveDate> {
    match period {
        "DAY" => Some(day),
        "WEEK" => day.checked_sub_days(Days::new(u64::from(
            day.weekday().num_days_from_sunday(),
        ))),
        "MONTH" => day.with_day(1),
        _ => None,
    }
}

/// The LAST day of the metering `period` that `day` falls in, or `None` for a period this
/// service knows no window for.
///
/// The far end of the window [`quota_period_start`] opens, and so the day an extension
/// granted in the period reaches through: the calendar end of the period its name gives it —
/// the day itself, the last day of its week, or the last day of its month — because the next
/// period is where the quota is whole again.
pub fn quota_period_end(period: &str, day: NaiveDate) -> Option<NaiveDate> {
    let start = quota_period_start(period, day)?;
    match period {
        "DAY" => Some(start),
        "WEEK" => start.checked_add_days(Days::new(6)),
        "MONTH" => start
            .checked_add_months(Months::new(1))
            .and_then(|next_period| next_period.pred_opt()),
        _ => None,
    }
}

impl ComAmazonawsApigatewayBackplanecontrolserviceState {
    /// Grant the pair the remaining quota `remaining` for the rest of the metering period
    /// that `day` falls in, answering what the pair has left once the grant is recorded — or
    /// `None` for a plan that meters no quota in a window, which has no remaining quota to
    /// grant.
    ///
    /// The grant is stated as what the period has LEFT, so that is what is RECORDED: the
    /// requests the pair may still send, the day of the grant, and the day the period the
    /// grant was made in reaches through. Nothing of the plan's present `quota` is folded
    /// into the record, so an edit of the plan's limit or period afterwards leaves the
    /// recorded remaining of the days already recorded exactly as it was. Every later request
    /// is counted down from the grant by the one accounting in [`Self::remaining_quota`], and
    /// the next period starts at the plan's own limit again.
    pub fn grant_remaining_quota(
        &mut self,
        usage_plan_id: &str,
        key_id: &str,
        day: NaiveDate,
        remaining: i64,
    ) -> Option<i64> {
        // The plan's window is read for the PERIOD the grant covers, not for its budget: a
        // grant states the remaining quota outright, so what the plan's limit happens to be
        // right now is no part of the recorded fact.
        let window = quota_window(self.usage_plans.get(usage_plan_id)?, day)?;
        let log = self
            .usage_logs
            .entry(metering_key(usage_plan_id, key_id))
            .or_default();
        log.grant(
            day,
            GrantedRemaining {
                // A count of requests, so never below nothing.
                remaining: remaining.max(0),
                through: window.through,
                sent_by_grant: log.used_on(day),
            },
        );
        // The grant is usage data of its own: the pair has a usage record for the day it was
        // made on, so a read of the usage over that day reports the granted quota rather
        // than indexing nothing for a key that has one.
        log.open(day);
        self.remaining_quota(usage_plan_id, key_id, day)
    }

    /// Count one request from the API key `key_id` against the quota of `usage_plan_id` on
    /// the UTC day `day`, and report whether that quota admitted it.
    ///
    /// The count is the one thing a quota is: a period admits what it has left
    /// ([`Self::remaining_quota`]), so a request arriving once nothing is left is NOT
    /// admitted and NOT counted — the period is already at its limit, and a refused
    /// request consumed none of it. A plan that meters no quota ([`quota_window`]) admits
    /// every request and still counts it, because what it sent is what a read of the usage
    /// reports as used.
    ///
    /// This is the ONE place a usage plan's quota is accounted, so the data plane and every
    /// read of the usage data agree on how many requests a key has sent and how many it has
    /// left.
    pub fn quota_admits(&mut self, usage_plan_id: &str, key_id: &str, day: NaiveDate) -> bool {
        if self
            .remaining_quota(usage_plan_id, key_id, day)
            .is_some_and(|remaining| remaining <= 0)
        {
            return false;
        }
        self.usage_logs
            .entry(metering_key(usage_plan_id, key_id))
            .or_default()
            .record(day);
        true
    }

    /// Whether the pair has usage data over the days `from` through `through`: whether any
    /// of them is a day it has a usage record for.
    ///
    /// What a report of the usage data is INDEXED by: a key the plan meters that has no
    /// record over the interval has no daily log in it. A record is opened by a request the
    /// data plane counted and by a grant of remaining quota made against the pair
    /// ([`Self::grant_remaining_quota`]) — both are usage data of that `(usage plan, API
    /// key)` pair on that day.
    pub fn has_usage_data(
        &self,
        usage_plan_id: &str,
        key_id: &str,
        from: NaiveDate,
        through: NaiveDate,
    ) -> bool {
        self.usage_logs
            .get(&metering_key(usage_plan_id, key_id))
            .is_some_and(|log| log.per_day.range(from..=through).next().is_some())
    }

    /// What the metering period that holds `day` has LEFT for one metered `(usage plan, API
    /// key)` pair. `None` for a plan that meters no quota in a window and no grant covering
    /// that day, which has no remaining quota at all.
    ///
    /// Read from the GRANT when one covers the day — the remaining quota that grant recorded,
    /// less the requests the pair sent after it — and from the plan's own window otherwise:
    /// the period's budget less everything the pair sent in the period up to and including
    /// that day. A recorded grant is a fact about the days it covers, so what a read of those
    /// days reports does not move when the plan's `quota` is edited afterwards; every other
    /// day answers from the plan's quota as it stands.
    ///
    /// The ONE statement of what "remaining" means, read by the daily log a report carries
    /// and by the admission of the next request, so the number a caller is told and the
    /// number the next request is admitted against are the same one.
    pub fn remaining_quota(
        &self,
        usage_plan_id: &str,
        key_id: &str,
        day: NaiveDate,
    ) -> Option<i64> {
        let log = self.usage_logs.get(&metering_key(usage_plan_id, key_id));
        if let Some((granted_on, grant)) = log.and_then(|log| log.grant_covering(day)) {
            let since_grant = log
                .map_or(0, |log| log.used_between(granted_on, day))
                .saturating_sub(grant.sent_by_grant);
            return Some(grant.remaining.saturating_sub(since_grant).max(0));
        }
        let window = quota_window(self.usage_plans.get(usage_plan_id)?, day)?;
        let spent = log.map_or(0, |log| log.used_between(window.start, day));
        Some(window.budget.saturating_sub(spent).max(0))
    }

    /// The daily log of used and remaining quota one `(usage plan, API key)` pair earns
    /// over the days `from` through `through`, both included: one `[used, remaining]` entry
    /// per day, in date order.
    ///
    /// `used` is what the pair sent THAT DAY and `remaining` what the quota has left AFTER
    /// it — the period's budget less everything the pair sent in that period up to and
    /// including the day — so a day in a new period reports the whole budget again however
    /// much the one before it consumed. A pair metered by no quota has no budget to count
    /// down and reports `QuotaSettings$limit`'s own default of zero remaining.
    pub fn daily_usage(
        &self,
        usage_plan_id: &str,
        key_id: &str,
        from: NaiveDate,
        through: NaiveDate,
    ) -> Vec<Vec<i64>> {
        let log = self.usage_logs.get(&metering_key(usage_plan_id, key_id));
        let mut entries = Vec::new();
        let mut day = from;
        while day <= through {
            let used = log.map_or(0, |log| log.used_on(day));
            let remaining = self
                .remaining_quota(usage_plan_id, key_id, day)
                .unwrap_or_default();
            entries.push(vec![used, remaining]);
            let Some(next) = day.succ_opt() else { break };
            day = next;
        }
        entries
    }
}

/// The `NotFoundException` message for a `keyId` that names no API key the usage plan
/// meters, as the GRANT of a remaining quota words it.
///
/// A sentence of its OWN, distinct from the one a read of an API key answers
/// ([`super::NO_SUCH_API_KEY`]): a recorded refusal of a grant naming a key the account does
/// not hold states the API key's CONTENT, so that is what this operation answers the
/// condition with. The same sentence for a key the account holds but this plan does not
/// meter, which names just as little usage data of this plan.
pub const NO_SUCH_METERED_API_KEY: &str = "Invalid API Key content specified";

/// The `NotFoundException` message for a `keyId` that names no API key a READ of the usage
/// data could report, which is a key this plan does not meter.
///
/// A sentence of its own, worded as neither the grant's
/// ([`NO_SUCH_METERED_API_KEY`]) nor an API key reader's ([`super::NO_SUCH_API_KEY`]): a
/// recorded refusal of a report naming a key the account does not hold states the KEY
/// IDENTIFIER the request narrowed the report by, and quotes the rejected id back nowhere.
/// The same sentence for a key the account holds but this plan does not meter, which names
/// just as little usage data of this plan.
pub const NO_SUCH_REPORTED_API_KEY: &str = "Invalid Key identifier specified";

/// The `BadRequestException` message for a grant of remaining quota made against a usage
/// plan that meters no quota in a window — one carrying no `quota` at all, or one whose
/// quota names no `period`.
///
/// A grant is "a temporary extension to the remaining quota of a usage plan" (`UpdateUsage`'s
/// own documentation), so a plan whose keys are metered against no quota has no remaining
/// quota a grant could name: the request names a setting the plan does not have, which is the
/// bad request this service answers such a request with. Worded like the plan's other quota
/// sentences ([`super::invalid_quota_count`]), naming the setting and nothing of the value
/// that rode in on the request.
pub const NO_USAGE_PLAN_QUOTA: &str = "Usage Plan does not have a quota configured";

/// The `BadRequestException` message for a `startDate` that names no day.
///
/// A SENTENCE OF ITS OWN, like [`super::NO_SUCH_USAGE_PLAN`]: it names the member that is
/// wrong and quotes the rejected value back nowhere.
pub const INVALID_USAGE_START_DATE: &str = "Invalid start date specified";

/// The `BadRequestException` message for an `endDate` that names no day; see
/// [`INVALID_USAGE_START_DATE`].
pub const INVALID_USAGE_END_DATE: &str = "Invalid end date specified";

/// The `BadRequestException` message for an interval whose end precedes its start: the
/// days it covers are none at all, so there is no usage data it could report.
///
/// WHOLLY FIXED text, as the recorded refusal of an inverted interval words it: it states
/// the condition — the end of the interval must come after its start — and quotes neither
/// date back.
pub const INVALID_USAGE_INTERVAL: &str = "Usage end date must be after start date";

/// The `BadRequestException` message for an interval wider than the window a report may
/// cover.
///
/// WHOLLY FIXED text, like the sentence for an inverted interval
/// ([`INVALID_USAGE_INTERVAL`]): it states the bound the usage data is available over — the
/// same count of days for every interval past it ([`MAX_USAGE_INTERVAL_DAYS`]) — and
/// interpolates nothing of the request, neither a date nor the span it asked for.
pub fn usage_interval_too_wide() -> String {
    format!("Usage date range cannot exceed {MAX_USAGE_INTERVAL_DAYS} days")
}

/// The interval `start_date` through `end_date` covers, or the sentence the pair is refused
/// with: both must name a day, in order, no wider than the window a report may cover.
///
/// One check for both reads of the usage data, so the interval a report covers is admitted
/// by one rule whichever operation asked for it.
pub fn usage_interval(start_date: &str, end_date: &str) -> Result<(NaiveDate, NaiveDate), String> {
    let from = usage_date(start_date).ok_or_else(|| INVALID_USAGE_START_DATE.to_string())?;
    let through = usage_date(end_date).ok_or_else(|| INVALID_USAGE_END_DATE.to_string())?;
    if through < from {
        return Err(INVALID_USAGE_INTERVAL.to_string());
    }
    // Both endpoints are covered, so an interval of one day is the pair that names the
    // same day twice.
    let covered = (through - from).num_days().saturating_add(1);
    if covered > i64::try_from(MAX_USAGE_INTERVAL_DAYS).unwrap_or(i64::MAX) {
        return Err(usage_interval_too_wide());
    }
    Ok((from, through))
}
