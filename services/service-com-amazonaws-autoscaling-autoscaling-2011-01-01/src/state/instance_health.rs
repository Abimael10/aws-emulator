// SPDX-License-Identifier: CC0-1.0

//! The health of an Auto Scaling group's instances as a CALLER records it.
//!
//! # The rule this module implements
//!
//! `SetInstanceHealth` "sets the health status of the specified instance": the status the
//! request states becomes the `HealthStatus` the group reports for that instance, and
//! "set to `Unhealthy` to have the instance be out of service. Amazon EC2 Auto Scaling
//! terminates and replaces the unhealthy instance"
//! (`smithy_query(query="doc",shape="SetInstanceHealthQuery",member="HealthStatus")`).
//!
//! So a recorded status is TWO things, and they are kept apart:
//!
//! * what the group reports for the instance, which is what the call writes
//!   ([`AutoScalingState::set_instance_health`]) and every read of that instance projects
//!   ([`super::StoredInstance::to_instance`]);
//! * the replacement an unhealthy instance is owed, which the group's own health check
//!   performs. That is a change the CLOCK owes rather than one a later request asks for —
//!   the check runs whether or not anybody calls — so it is stored as a due instant
//!   ([`super::StoredInstance::health_check_due`]) and materialised by
//!   [`AutoScalingState::materialize_due`], through the one path a failed health check
//!   takes a member of a group out by ([`AutoScalingState::replace_unhealthy_instances`]).
//!
//! The instance an id names is resolved against the group that HOLDS it: the request names
//! no group ([`AutoScalingState::group_holding_instance`]), so an id no group of the
//! account manages is refused and nothing is written.
//!
//! # The grace period the call respects
//!
//! "If the Auto Scaling group of the specified instance has a `HealthCheckGracePeriod`
//! specified for the group, by default, this call respects the grace period. Set this to
//! `False`, to have the call not respect the grace period associated with the group"
//! (`smithy_query(query="doc",shape="SetInstanceHealthQuery",member="ShouldRespectGracePeriod")`).
//!
//! The grace period is the wait a group gives an instance it has just brought into service
//! before a failed health check replaces it, so a call that respects it records the status
//! at once and leaves the replacement owed from the END of that period; one that does not
//! leaves it owed from the call. Either way the status itself is recorded: the grace period
//! is about when the group ACTS on a failed check, not about whether the check was made.
//!
//! An instance parked in `Standby` is left in place while it is parked — it is out of
//! service already, and the group performs no health check on an instance it has parked —
//! and the replacement it is owed falls due again once it is back in service.

use std::time::{Duration, SystemTime};

use super::{
    AutoScalingState, StoredInstance, DEFAULT_HEALTH_CHECK_GRACE_PERIOD, HEALTH_STATUS_HEALTHY,
    HEALTH_STATUS_UNHEALTHY, INSTANCE_REFRESH_REPLACEMENT,
};

/// What a failed health check of this instance's group OWES it: the instant the group ACTS
/// on that check, and the sentence the check records for taking the instance out of service.
///
/// The reason travels with the debt because WHICH check failed is what the activity of the
/// instance leaving the group reports, and the checks a group performs are not one check: a
/// caller's own ([`USER_HEALTH_CHECK_REASON`]) and the load balancer's
/// ([`super::ELB_HEALTH_CHECK_FAILURE_REASON`]) are the same replacement under different
/// sentences, so one debt carries one of them rather than the acting pass guessing.
#[derive(Debug, Clone, Copy)]
pub struct HealthCheckDebt {
    /// When the group's health check acts on the instance.
    pub due: SystemTime,
    /// What the activity of the instance leaving the group records as having taken it out of
    /// service.
    pub reason: &'static str,
}

/// The `HealthStatus` values a request may record, in the order the member's own
/// documentation states them ([`HEALTH_STATUS_HEALTHY`], [`HEALTH_STATUS_UNHEALTHY`]).
///
/// The model targets the member at a plain string
/// (`smithy_query(query="operation",operation="SetInstanceHealth")`), so it carries no
/// enumeration for the boundary validator to refuse a value against: the set is checked
/// here instead.
pub const HEALTH_STATUS_VALUES: &[&str] = &[HEALTH_STATUS_HEALTHY, HEALTH_STATUS_UNHEALTHY];

/// How long after a health check has recorded an instance unhealthy the group acts on it.
///
/// Acting on it is a COMPOSITE of the moves this service owes an instance: the unhealthy
/// instance goes out of service and the group launches a replacement for the capacity it
/// was providing, so it is owed the span this emulator gives such a replacement
/// ([`INSTANCE_REFRESH_REPLACEMENT`]) rather than the shorter span one lifecycle move
/// takes. Chosen, not documented, and never slept for: it is stored as the instant the
/// check falls due ([`super::StoredInstance::health_check_due`]) and retired by
/// [`AutoScalingState::materialize_due`], so a caller reading the instance straight after
/// recording it unhealthy still sees it in service with the status the call wrote.
pub const UNHEALTHY_INSTANCE_REPLACEMENT: Duration = INSTANCE_REFRESH_REPLACEMENT;

/// What the activity of an instance a CALLER's health check took out of service records as
/// the reason it began.
///
/// A different sentence from the one the Amazon EC2 state change records
/// ([`super::EC2_INSTANCE_GONE_REASON`]) because it records a different check: this
/// instance was running as far as Amazon EC2 was concerned and a health check of the
/// caller's own found it unhealthy. Fixed text, with nothing of the request or of this run
/// in it.
pub const USER_HEALTH_CHECK_REASON: &str =
    "an instance was taken out of service in response to a user health-check";

/// The message a request stating a `HealthStatus` outside [`HEALTH_STATUS_VALUES`] is
/// refused with.
///
/// The sentence states WHAT THE MEMBER ADMITS rather than what the request sent, so it has
/// no variable part: the fixed text, and the admissible set in the bracketed form this
/// service states a value set in ([`super::stated_value_set`]), in the order the member's
/// own documentation names the two states.
#[must_use]
pub fn health_status_violation_message() -> String {
    format!(
        "Valid instance health states are: {}.",
        super::stated_value_set(HEALTH_STATUS_VALUES.iter().copied())
    )
}

/// The message a request recording a health status for an instance no Auto Scaling group of
/// the account manages is refused with.
///
/// The same `<subject> not found - <detail>` shape as this service's other absent-instance
/// refusal ([`super::ABSENT_INSTANCE_PREFIX`]), with the detail form THIS operation states:
/// the instance, named, was not found. Its ONE variable part is the instance id the request
/// carried; the rest, punctuation included, is fixed text.
#[must_use]
pub fn absent_instance_message(instance_id: &str) -> String {
    format!("{}Instance {instance_id} not found", super::ABSENT_INSTANCE_PREFIX)
}

/// Why a health status could not be recorded, for the operation to state the refusal each
/// situation belongs to.
#[derive(Debug)]
pub enum HealthRefusal {
    /// The request stated a status that names neither of the two states the member admits.
    UnknownStatus,
    /// No Auto Scaling group of the account holds the instance the request named; carries
    /// that instance id.
    ManagedByNoGroup(String),
}

impl HealthRefusal {
    /// The sentence this refusal carries, each about ONE thing the request got wrong: the
    /// status it may state, or the instance it named.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::UnknownStatus => health_status_violation_message(),
            Self::ManagedByNoGroup(instance_id) => absent_instance_message(instance_id),
        }
    }
}

impl AutoScalingState {
    /// Record `health_status` for the instance `instance_id`, and answer the name of the
    /// group that holds it.
    ///
    /// An unhealthy status leaves the group owing the instance's replacement, at the
    /// instant the group's health check acts on it: [`UNHEALTHY_INSTANCE_REPLACEMENT`] from
    /// now, and no earlier than the end of the group's own `HealthCheckGracePeriod` when
    /// the call respects that period (see the module docs). A healthy status takes that
    /// debt back off, which is how an instance recorded unhealthy is returned to service
    /// before the check has acted on it.
    ///
    /// Writes nothing when the request cannot be satisfied: a status outside the admissible
    /// set, or an instance no group of the account manages.
    pub fn set_instance_health(
        &mut self,
        instance_id: &str,
        health_status: &str,
        respect_grace_period: bool,
        now: SystemTime,
    ) -> Result<String, HealthRefusal> {
        if !HEALTH_STATUS_VALUES.contains(&health_status) {
            return Err(HealthRefusal::UnknownStatus);
        }
        // The request names the instance alone, so the group it is resolved against is the
        // one that holds it — and an id no group holds names no instance this service
        // manages.
        let Some(group) = self.group_holding_instance(instance_id) else {
            return Err(HealthRefusal::ManagedByNoGroup(instance_id.to_string()));
        };
        let group_name = group.name.clone();
        // A group that states no grace period of its own has the one every group carries by
        // default, so the wait is read off the group either way.
        let grace_period = group
            .health_check_grace_period
            .unwrap_or(DEFAULT_HEALTH_CHECK_GRACE_PERIOD);
        let Some(instance) = self
            .groups
            .get_mut(&group_name)
            .and_then(|group| {
                group
                    .instances
                    .iter_mut()
                    .find(|instance| instance.instance_id == instance_id)
            })
        else {
            return Err(HealthRefusal::ManagedByNoGroup(instance_id.to_string()));
        };
        instance.health_status = health_status.to_string();
        instance.health_check_due = (health_status == HEALTH_STATUS_UNHEALTHY).then(|| {
            HealthCheckDebt {
                due: unhealthy_check_due(instance, grace_period, respect_grace_period, now),
                reason: USER_HEALTH_CHECK_REASON,
            }
        });
        Ok(group_name)
    }

    /// Act on every instance a health check has recorded unhealthy and the clock has made
    /// due: each is taken out of service and the group launches a replacement for the
    /// capacity it was providing ([`AutoScalingState::replace_unhealthy_instances`]).
    ///
    /// Owed on the clock like every other due change of this store, so it is materialised
    /// by [`AutoScalingState::materialize_due`] — including from a reporting operation,
    /// which may write exactly this much: the check was due at that instant whether or not
    /// anybody called.
    ///
    /// An instance already on its way out of its group is left alone — it is leaving
    /// already, whether it is terminating outright or waiting at a termination hook
    /// ([`StoredInstance::is_leaving_group`]) — and so is one parked in `Standby`, which the
    /// group performs no health check on while it is parked. The debt is discharged as the
    /// check acts, so one recorded status is acted on exactly once.
    pub(crate) fn reconcile_unhealthy_instances<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        now: SystemTime,
    ) {
        // Read out before anything is written, so the groups are not borrowed across the
        // replacement each of them owes. One entry per (group, reason): the sentence the
        // activity records is the one the FAILED CHECK carries, so two checks of different
        // kinds falling due together are acted on under their own sentences.
        let due: Vec<(String, &'static str, Vec<String>)> = self
            .groups
            .values()
            .flat_map(|group| {
                let mut by_reason: Vec<(&'static str, Vec<String>)> = Vec::new();
                for instance in group.instances.iter().filter(|i| acts_now(i, now)) {
                    let reason = instance
                        .health_check_due
                        .map(|debt| debt.reason)
                        .unwrap_or(USER_HEALTH_CHECK_REASON);
                    match by_reason.iter_mut().find(|(held, _)| *held == reason) {
                        Some((_, ids)) => ids.push(instance.instance_id.clone()),
                        None => by_reason.push((reason, vec![instance.instance_id.clone()])),
                    }
                }
                by_reason
                    .into_iter()
                    .map(|(reason, ids)| (group.name.clone(), reason, ids))
                    .collect::<Vec<(String, &'static str, Vec<String>)>>()
            })
            .collect();
        for (group_name, reason, instance_ids) in due {
            // The check has ACTED on these instances, so it owes them nothing further: the
            // debt is discharged here, before the replacement, so this pass is owed exactly
            // once however long the instance then takes to leave the group — an instance a
            // termination hook holds is still a member for as long as the hold lasts, and a
            // later caller must not have it taken out a second time.
            if let Some(group) = self.groups.get_mut(&group_name) {
                for instance in group
                    .instances
                    .iter_mut()
                    .filter(|instance| instance_ids.contains(&instance.instance_id))
                {
                    instance.health_check_due = None;
                }
            }
            self.replace_unhealthy_instances(engine, &group_name, &instance_ids, reason, now);
        }
    }
}

/// When the group's health check acts on `instance`, which a failed check has just recorded
/// unhealthy.
///
/// [`UNHEALTHY_INSTANCE_REPLACEMENT`] from the call, and no earlier than the end of the
/// group's grace period for this instance when the call respects that period: the period
/// runs from the instant the instance came into service, so an instance that has been in
/// service longer than it is outside the period altogether and is acted on as soon as any
/// other. A grace period of no seconds — or of none at all — ends the moment the instance
/// came into service, so it holds nothing off.
fn unhealthy_check_due(
    instance: &StoredInstance,
    grace_period: i32,
    respect_grace_period: bool,
    now: SystemTime,
) -> SystemTime {
    let acts_at = now + UNHEALTHY_INSTANCE_REPLACEMENT;
    if !respect_grace_period {
        return acts_at;
    }
    let grace_ends = instance.launched_at
        + Duration::from_secs(u64::try_from(grace_period).unwrap_or_default());
    acts_at.max(grace_ends)
}

/// Whether the group's health check acts on `instance` at `now`: a check has recorded it
/// unhealthy, the clock has made that check due, and the instance is one the group still
/// performs health checks on (see the module docs).
///
/// "On its way out of the group" is the crate's own reading of it
/// ([`StoredInstance::is_leaving_group`]), which covers an instance waiting at a termination
/// hook as well as one terminating outright: either way it is leaving already, so the check
/// has nothing to take out.
fn acts_now(instance: &StoredInstance, now: SystemTime) -> bool {
    instance.health_check_due.is_some_and(|debt| debt.due <= now)
        && !instance.is_leaving_group()
        && !instance.is_in_standby()
}
