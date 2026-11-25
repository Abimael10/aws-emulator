// SPDX-License-Identifier: CC0-1.0

//! The Recycle Bin RETENTION RULES a delete of an EBS resource is decided by.
//!
//! "Only retention rules that are in the `available` state retain resources"
//! (`CreateRuleResponse$Status`): whether a deleted resource is permanently deleted or kept
//! in the Recycle Bin is a fact of the RULES the account holds, and a retention rule is the
//! `rbin` service's resource rather than EC2's. So this module reads that service's OWN store
//! — through that crate's own
//! [`state accessor`](::service_com_amazonaws_rbin_amazonrecyclebin::state::service_state) —
//! and answers the one question a delete asks of it: for how long must this resource be
//! retained? NOTHING is copied into this service's store, the way nothing of the EBS direct
//! APIs' snapshots is ([`super::direct_api_snapshots`]), so a rule created, updated or
//! deleted through `rbin` is in force here from the moment that call returns.
//!
//! The question is asked of the rules of ONE Region — "a Region-level retention rule […]
//! retains all deleted resources of the specified resource type in the Region in which the
//! rule is created" (`CreateRuleRequest$ResourceTags`) — which is the Region the deleting
//! request was signed for.
//!
//! This is a READ: the `pending` -> `available` move a freshly created rule owes is DERIVED
//! here from the instant that store already holds, exactly as that store's own
//! materialisation compares it, so asking this question writes nothing to another service's
//! state.
//!
//! LOCK ORDER: an EC2 operation takes its OWN store first and this one second (every function
//! here is called with the EC2 store already locked, or with none locked at all), and no
//! `rbin` handler reads this service's store — so the two orders cannot invert. That is the
//! order [`super::direct_api_snapshots`] keeps for the EBS store too.
//!
//! BOUNDS: one pass over the rules of one Region, which is bounded by that service's own
//! ceiling on stored rules — no EC2 request can grow it — and nothing is retained here.

use std::time::{Duration, SystemTime};

use ::service_com_amazonaws_rbin_amazonrecyclebin::ops::types::{
    RETENTION_PERIOD_UNIT_VALUES, ResourceTag, RetentionPeriod,
};
use ::service_com_amazonaws_rbin_amazonrecyclebin::state::{
    self as rbin_store, RESOURCE_TYPE_EBS_SNAPSHOT, RESOURCE_TYPE_EBS_VOLUME,
    RESOURCE_TYPE_EC2_IMAGE, RULE_STATUS_AVAILABLE, RULE_STATUS_PENDING, Rule, SECONDS_PER_DAY,
};

use crate::ops::types::Tag;

/// The `ResourceType` a retention rule names to retain deleted EBS SNAPSHOTS
/// (`rbin`'s `ResourceType` enum), under this crate's own name for it — so a handler asking
/// [`recycle_bin_retention_period`] names the type through the other service's own catalog
/// rather than a word of its own.
pub const RECYCLE_BIN_RESOURCE_TYPE_SNAPSHOT: &str = RESOURCE_TYPE_EBS_SNAPSHOT;

/// The `ResourceType` a retention rule names to retain deleted EBS VOLUMES (`rbin`'s
/// `ResourceType` enum), under this crate's own name for it — the sibling of
/// [`RECYCLE_BIN_RESOURCE_TYPE_SNAPSHOT`], so `DeleteVolume` asks about the type the other
/// service's own catalog names rather than a word of its own.
pub const RECYCLE_BIN_RESOURCE_TYPE_VOLUME: &str = RESOURCE_TYPE_EBS_VOLUME;

/// The `ResourceType` a retention rule names to retain deregistered AMIs (`rbin`'s
/// `ResourceType` enum), under this crate's own name for it — the type `DeregisterImage` asks
/// about: "if a deregistered EBS-backed AMI matches a Recycle Bin retention rule, it moves to
/// the Recycle Bin for the specified retention period" (`DeregisterImage`).
pub const RECYCLE_BIN_RESOURCE_TYPE_IMAGE: &str = RESOURCE_TYPE_EC2_IMAGE;

/// The seconds one day of a retention period holds, as the service that owns retention rules
/// counts a day ([`SECONDS_PER_DAY`]): "the unit of time in which the retention period is
/// measured. Currently, only `DAYS` is supported"
/// (`RetentionPeriod$RetentionPeriodUnit`), and a retention period's value is "measured in
/// days" (`RetentionPeriod$RetentionPeriodValue`).
const SECONDS_PER_RETENTION_DAY: u64 = SECONDS_PER_DAY;

/// The one unit a retention period may be measured in: "the unit of time in which the
/// retention period is measured. Currently, only `DAYS` is supported"
/// (`RetentionPeriod$RetentionPeriodUnit`), which is also the single member the generated
/// `RetentionPeriodUnit` catalog holds ([`RETENTION_PERIOD_UNIT_VALUES`]). A unit outside
/// this ONE value is a unit this reading does not know — including a second unit a later
/// model adds, which would be measured in something other than days and so cannot be read as
/// a count of them.
const RETENTION_PERIOD_UNIT_DAYS: &str = "DAYS";

/// Whether `unit` is the one unit a retention period this reading understands is measured in.
///
/// Equality with [`RETENTION_PERIOD_UNIT_DAYS`], not membership of the catalog: the catalog is
/// what that constant is checked against (the assertion below), while the DECISION this makes
/// is about the one unit [`retention_period_duration`] can convert.
fn retention_period_unit_is_days(unit: &str) -> bool {
    debug_assert!(
        RETENTION_PERIOD_UNIT_VALUES.contains(&RETENTION_PERIOD_UNIT_DAYS),
        "the unit this reading converts must be one the model declares"
    );
    unit == RETENTION_PERIOD_UNIT_DAYS
}

/// For how long a deleted resource of `resource_type` carrying `tags` must be kept in the
/// Recycle Bin, as the retention rules of `region` stand at `now` — `None` when no rule
/// retains it, which is the answer for an account that holds no rule at all and the one a
/// delete reads as "delete this permanently".
///
/// A resource several rules retain is kept for the LONGEST of their retention periods: the
/// bin holds the resource once, so a shorter period's end cannot be the moment it is
/// permanently deleted while another rule is still retaining it.
///
/// `None` as well on an engine the `rbin` service is not mounted on, which is the same answer
/// as an account that created no retention rule: a delete is served either way and is never
/// refused for want of the other service.
pub fn recycle_bin_retention_period<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    resource_type: &str,
    tags: &[Tag],
    now: SystemTime,
) -> Option<Duration> {
    let state = rbin_store::service_state(engine)?;
    let store = state.lock();
    store
        .rules_in_region(region)
        .filter(|rule| rule.resource_type == resource_type)
        .filter(|rule| rule_is_available(rule, now))
        .filter(|rule| rule_retains(rule, tags))
        .filter_map(|rule| retention_period_duration(&rule.retention_period))
        .max()
}

/// Whether `rule` is in the one state that retains anything, as of `now`.
///
/// "Only retention rules that are in the `available` state retain resources"
/// (`CreateRuleResponse$Status`) — so a rule in any OTHER state retains nothing, and the one
/// state this adds to it is the `available` the clock already owes a `pending` rule: that
/// move was due at an instant the other service's store already holds, whether or not
/// anybody called, so it is derived here from the same comparison rather than read as
/// `pending` until some `rbin` call next materialises it, exactly as
/// [`super::direct_api_snapshots`] derives a snapshot's own clock-owed move. A rule in
/// neither state retains nothing however long ago its instant passed.
fn rule_is_available(rule: &Rule, now: SystemTime) -> bool {
    rule.status == RULE_STATUS_AVAILABLE
        || (rule.status == RULE_STATUS_PENDING && now >= rule.available_at)
}

/// Whether `rule` retains a deleted resource carrying `tags`.
///
/// Two kinds of rule, as `CreateRuleRequest$ResourceTags` draws the line:
///
/// * a TAG-LEVEL rule names resource tags, and "only deleted resources, of the specified
///   resource type, that have one or more of the specified tag key and value pairs are
///   retained. If a resource is deleted, but it does not have any of the specified tag key
///   and value pairs, it is immediately deleted without being retained by the retention
///   rule";
/// * a REGION-LEVEL rule "does not have any resource tags specified. It retains all deleted
///   resources of the specified resource type in the Region in which the rule is created,
///   even if the resources are not tagged" — except for what its exclusion tags leave out:
///   "resources that have any of these tags are not retained by the retention rule upon
///   deletion" (`CreateRuleRequest$ExcludeResourceTags`).
fn rule_retains(rule: &Rule, tags: &[Tag]) -> bool {
    if rule.resource_tags.is_empty() {
        return !rule
            .exclude_resource_tags
            .iter()
            .any(|excluded| carries_resource_tag(tags, excluded));
    }
    rule.resource_tags
        .iter()
        .any(|wanted| carries_resource_tag(tags, wanted))
}

/// Whether `tags` carries the pair `wanted` names — the key together with its value, a tag
/// carrying no value being the empty value, which is how the other service's own reader
/// compares a pair.
fn carries_resource_tag(tags: &[Tag], wanted: &ResourceTag) -> bool {
    tags.iter().any(|tag| {
        tag.key.as_deref() == Some(wanted.resource_tag_key.as_str())
            && tag.value.as_deref().unwrap_or_default()
                == wanted.resource_tag_value.as_deref().unwrap_or_default()
    })
}

/// `retention_period` as a span of time, or `None` for a period measured in a unit this
/// reading does not know — the model declares exactly one
/// ([`retention_period_unit_is_days`]) and the other service refuses any other before it
/// stores a rule, so `None` is unreachable through that service's own API.
///
/// A negative value is no period either: the modelled `@range(min: 1, max: 3650)` on
/// `RetentionPeriod$RetentionPeriodValue` makes one unreachable the same way.
fn retention_period_duration(retention_period: &RetentionPeriod) -> Option<Duration> {
    if !retention_period_unit_is_days(&retention_period.retention_period_unit) {
        return None;
    }
    let days = u64::try_from(retention_period.retention_period_value).ok()?;
    days.checked_mul(SECONDS_PER_RETENTION_DAY)
        .map(Duration::from_secs)
}
