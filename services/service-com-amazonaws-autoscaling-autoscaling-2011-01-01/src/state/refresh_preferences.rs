// SPDX-License-Identifier: CC0-1.0

//! The preferences an instance refresh actually RUNS UNDER.
//!
//! `RefreshPreferences` is optional on the request and on every one of its members, but a
//! running refresh has an answer for each of them: the model documents what Amazon EC2
//! Auto Scaling applies when the request states none —
//!
//! * `MinHealthyPercentage`: "if you do not specify this property, the default is 90
//!   percent, or the percentage set in the instance maintenance policy for the Auto
//!   Scaling group, if defined";
//! * `InstanceWarmup`: "if you do not specify this property, the instance warmup by
//!   default is the value of the `DefaultInstanceWarmup` property, if defined …, or the
//!   `HealthCheckGracePeriod` property otherwise";
//! * `SkipMatching`: "the default is `false`";
//! * `AutoRollback`: "the default is `false`" — and a request that asks for `true` where
//!   "a rollback is not supported" is refused rather than served with the preference
//!   quietly turned off ([`AUTO_ROLLBACK_WITHOUT_DESIRED_CONFIGURATION_MESSAGE`]);
//! * `AlarmSpecification`: the CloudWatch alarms the refresh fails on, which is an empty
//!   specification for a refresh that watches none;
//! * `CheckpointDelay`: "if you specify a value for `CheckpointPercentages` and not for
//!   `CheckpointDelay`, the `CheckpointDelay` defaults to `3600` (1 hour)", so a refresh
//!   that stated thresholds runs under a delay either way
//!   ([`DEFAULT_CHECKPOINT_DELAY`]).
//!
//! So a client that reads a running refresh back is told the effective value of each,
//! never an absent member it would have to know the default of. The preferences are
//! resolved ONCE, when the refresh is created, against the group's configuration as it
//! then stood: that is the configuration the refresh runs under, so a later change to the
//! group does not rewrite the preferences of a refresh already under way.
//!
//! Every member the request DID state is kept exactly as written, including the ones this
//! service applies no default to (`CheckpointPercentages`, `MaxHealthyPercentage`,
//! `ScaleInProtectedInstances`, `StandbyInstances`, `BakeTime`): the group has no value of
//! its own for any of them, so a refresh that states none runs under the service's own
//! behaviour rather than a value invented here.

use crate::ops::types::{AlarmSpecification, RefreshPreferences};

use super::StoredGroup;

/// `RefreshPreferences$MinHealthyPercentage` when neither the request nor the group's
/// instance maintenance policy states one, from that member's own documentation: "the
/// default is 90 percent".
pub const DEFAULT_MIN_HEALTHY_PERCENTAGE: i32 = 90;

/// `RefreshPreferences$InstanceWarmup` when neither the request, the group's
/// `DefaultInstanceWarmup` nor its `HealthCheckGracePeriod` states one.
///
/// The documented fallbacks are the group's own two members, and a group's
/// `HealthCheckGracePeriod` is itself "Default: 0 seconds"
/// ([`super::DEFAULT_HEALTH_CHECK_GRACE_PERIOD`]), so this is the floor under that chain.
pub const DEFAULT_INSTANCE_WARMUP: i32 = super::DEFAULT_HEALTH_CHECK_GRACE_PERIOD;

/// `RefreshPreferences$CheckpointDelay`, in seconds, when the request states checkpoint
/// thresholds but no delay to hold at them: "if you specify a value for
/// `CheckpointPercentages` and not for `CheckpointDelay`, the `CheckpointDelay` defaults to
/// `3600` (1 hour)".
///
/// A request that states NO thresholds has no checkpoint to hold at, so this default does
/// not reach it: the member stays as the request left it.
pub const DEFAULT_CHECKPOINT_DELAY: i32 = 3600;

/// `RefreshPreferences$SkipMatching` when the request states none: "the default is
/// `false`".
pub const DEFAULT_SKIP_MATCHING: bool = false;

/// `RefreshPreferences$AutoRollback` when the request states none: "the default is
/// `false`".
pub const DEFAULT_AUTO_ROLLBACK: bool = false;

/// What Amazon EC2 Auto Scaling answers a request that asks for a rollback the refresh
/// could not perform.
///
/// "A rollback is not supported in the following situations: there is no desired
/// configuration specified for the instance refresh…"
/// (`smithy_query(query="doc",shape="RefreshPreferences",member="AutoRollback")`), so a
/// request that asks for one and states no desired configuration asks for something the
/// service cannot do: it is refused rather than served with the preference quietly turned
/// off. Fixed text naming the two members and the two ways to fix the request, with
/// nothing taken from it.
pub const AUTO_ROLLBACK_WITHOUT_DESIRED_CONFIGURATION_MESSAGE: &str =
    "The request isn\u{2019}t valid. The AutoRollback parameter cannot be set to true when the \
     DesiredConfiguration parameter is empty. Set AutoRollback to false or specify a desired \
     configuration and try again.";

/// The preferences the refresh of `group` runs under, given the ones `requested` states.
///
/// A preference the request states is kept exactly as written: the request that states one
/// the refresh could not honour is refused outright
/// ([`AUTO_ROLLBACK_WITHOUT_DESIRED_CONFIGURATION_MESSAGE`]) rather than served with a
/// quietly different value.
#[must_use]
pub fn effective(
    requested: Option<&RefreshPreferences>,
    group: &StoredGroup,
) -> RefreshPreferences {
    let mut preferences = requested.cloned().unwrap_or_default();
    if preferences.min_healthy_percentage.is_none() {
        preferences.min_healthy_percentage = Some(
            group
                .instance_maintenance_policy
                .as_ref()
                .and_then(|policy| policy.min_healthy_percentage)
                .unwrap_or(DEFAULT_MIN_HEALTHY_PERCENTAGE),
        );
    }
    if preferences.instance_warmup.is_none() {
        preferences.instance_warmup = Some(
            group
                .default_instance_warmup
                .or(group.health_check_grace_period)
                .unwrap_or(DEFAULT_INSTANCE_WARMUP),
        );
    }
    if preferences.checkpoint_delay.is_none() && preferences.checkpoint_percentages.is_some() {
        preferences.checkpoint_delay = Some(DEFAULT_CHECKPOINT_DELAY);
    }
    preferences.skip_matching = Some(preferences.skip_matching.unwrap_or(DEFAULT_SKIP_MATCHING));
    preferences.auto_rollback = Some(preferences.auto_rollback.unwrap_or(DEFAULT_AUTO_ROLLBACK));
    if preferences.alarm_specification.is_none() {
        preferences.alarm_specification = Some(AlarmSpecification { alarms: None });
    }
    preferences
}
