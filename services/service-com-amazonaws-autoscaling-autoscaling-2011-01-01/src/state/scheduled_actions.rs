// SPDX-License-Identifier: CC0-1.0

//! The scheduled scaling actions of an Auto Scaling group: the resource
//! `PutScheduledUpdateGroupAction` and `BatchPutScheduledUpdateGroupAction` create or update
//! and `DescribeScheduledActions` reports.
//!
//! A scheduled action is a CHILD of one Auto Scaling group, named uniquely within it, so it
//! is stored on [`StoredGroup`] itself ([`StoredGroup::scheduled_actions`]): one collection
//! that the put upserts into, the read projects from, and the group's own delete takes with
//! the group — a scheduled action only exists against a group that does. Nothing in this
//! crate holds a second copy.
//!
//! # The upsert
//!
//! "Creates or updates a scheduled scaling action for an Auto Scaling group […] You can view
//! the scheduled actions for an Auto Scaling group using the DescribeScheduledActions API
//! call" (`smithy_query(query="operation",operation="PutScheduledUpdateGroupAction")`): the
//! request states the WHOLE action — when it runs and which sizes it leaves the group with —
//! so a second call naming the same action name replaces that definition rather than merging
//! into it, while the identity minted when the action was created (its
//! `ScheduledActionARN`) survives the update, because putting an action again updates that
//! action rather than replacing it with a different resource.
//!
//! "Creates or updates one or more scheduled scaling actions for an Auto Scaling group"
//! (`smithy_query(query="operation",operation="BatchPutScheduledUpdateGroupAction")`) states
//! the same upsert for a whole list of actions at once, so one call of either operation puts
//! an action through [`super::AutoScalingState::put_scheduled_action`] and is taken against
//! the rules below; the batch states the sizes per action, so an action the batch could not
//! store is reported on its own ([`ScheduledActionRefusal::error_code`]) rather than
//! refusing the actions beside it.
//!
//! # The name a batch entry may take
//!
//! A scheduled action's NAME is its identity within its group, and a name the group already
//! holds is held by an action a batch did not state: so an entry of a batch can only CREATE
//! an action, and a name already taken — by an action the group holds, or by an earlier entry
//! of the same call — is a CONFLICT rather than an update
//! ([`super::AutoScalingState::first_taken_scheduled_action_name`],
//! [`scheduled_action_already_exists_message`]). The conflict is found before any entry of
//! the call is committed, so such a call writes NOTHING: the action the name belongs to keeps
//! the definition it had, and every entry of the call — the free names beside the taken one
//! included — is reported under `FailedScheduledUpdateGroupActions`. Restating a held action
//! WHOLE is what `PutScheduledUpdateGroupAction` is for, and its upsert is unaffected.
//!
//! # What the action carries
//!
//! `Time` "is no longer used" (`PutScheduledUpdateGroupActionType$Time`) — it is the legacy
//! spelling of the one instant the action starts at, which `StartTime` states now — so the
//! two name ONE stored value and a read reports that value under both members.
//!
//! # The rules a put is taken against
//!
//! The sizes are the group's own three: a scheduled action is an operation that SETS a
//! group's bounds, so it is taken against the same rules as the other writers of them
//! ([`super::max_below_min_message`], [`super::desired_capacity_out_of_bounds_message`],
//! [`super::size_past_ceiling`]), with the bound the request omits read from the group the
//! action belongs to — exactly as `UpdateAutoScalingGroup` reads it. On top of those:
//!
//! * "You must specify at least one of the following properties: MaxSize, MinSize, or
//!   DesiredCapacity" (`PutScheduledUpdateGroupActionType$DesiredCapacity`): an action that
//!   states no size would leave the group as it is, so there is no action to schedule;
//! * "When StartTime and EndTime are specified with Recurrence, they form the boundaries of
//!   when the recurring action starts and stops"
//!   (`PutScheduledUpdateGroupActionType$Recurrence`): boundaries that do not open — an end
//!   at or before the start — bound no run of the action;
//! * a `Recurrence` is the crontab expression that member documents and a `TimeZone` is a
//!   canonical IANA name, both read by [`super::recurrence`]: a value either reader cannot
//!   read states no schedule.
//!
//! # The instant a read reports
//!
//! A scheduled action reports the instant it next RUNS at. For an action the request timed
//! itself, that is the instant it stated; for a purely recurring one, it is the next
//! occurrence of its recurrence, resolved when the action is stored — so a read reports what
//! the store holds and two reads of one action report the same instant.

use std::collections::{BTreeMap, BTreeSet};
use std::time::SystemTime;

use ::aws_smithy_types::DateTime;

use super::StoredGroup;
use crate::ops::types::ScheduledUpdateGroupAction;

/// The scheduled actions of ONE group, keyed by `ScheduledActionName`.
///
/// Ordered, so the collection iterates action name by action name — which is the order
/// every read of it reports.
pub type ScheduledActions = BTreeMap<String, StoredScheduledAction>;

/// The most scheduled actions one Auto Scaling group may hold: the published Amazon EC2
/// Auto Scaling quota "Scheduled actions per Auto Scaling group", whose value is 125.
///
/// The member carries no modelled bound, so the handler checks it and answers the
/// operation's modelled `LimitExceededFault` past it.
pub const MAX_SCHEDULED_ACTIONS_PER_GROUP: usize = 125;

/// The most scheduled actions ONE call may state.
///
/// From the model's own documentation for
/// `BatchPutScheduledUpdateGroupActionType$ScheduledUpdateGroupActions`: "One or more
/// scheduled actions. The maximum number allowed is 50." The member carries no modelled
/// length, so the bound is the service's own and the handler checks it.
pub const MAX_SCHEDULED_ACTIONS_PER_CALL: usize = 50;

/// The sentence a call stating more than [`MAX_SCHEDULED_ACTIONS_PER_CALL`] actions is
/// refused with: the one sentence this service states for a list member past the bound it
/// has to satisfy ([`super::too_many_items_message`]), naming the member as the wire names
/// it.
#[must_use]
pub fn too_many_scheduled_actions_in_one_call_message(count: usize) -> String {
    super::too_many_items_message(
        count,
        "scheduledUpdateGroupActions",
        MAX_SCHEDULED_ACTIONS_PER_CALL,
    )
}

/// The action one `PutScheduledUpdateGroupAction` states: when it runs, and the sizes it
/// leaves the group with.
///
/// The legacy `Time` is resolved into [`Self::start_time`] by the handler that reads the
/// request, so everything below sees the ONE instant the action starts at.
#[derive(Debug, Clone, Default)]
pub struct ScheduledActionPut {
    pub start_time: Option<DateTime>,
    pub end_time: Option<DateTime>,
    pub recurrence: Option<String>,
    pub min_size: Option<i32>,
    pub max_size: Option<i32>,
    pub desired_capacity: Option<i32>,
    pub time_zone: Option<String>,
}

impl ScheduledActionPut {
    /// Take this put against every rule in this module's own documentation, against the
    /// group it would be stored on.
    ///
    /// Checked BEFORE anything is written, so a refused put leaves the group's scheduled
    /// actions exactly as they were.
    pub fn validate(&self, group: &StoredGroup) -> Result<(), ScheduledActionRefusal> {
        if self.min_size.is_none() && self.max_size.is_none() && self.desired_capacity.is_none()
        {
            return Err(ScheduledActionRefusal::NoSizeStated);
        }
        if let Some(min_size) = self.min_size.filter(|min_size| *min_size < 0) {
            return Err(ScheduledActionRefusal::NegativeMinSize(min_size));
        }
        // The bounds the action leaves the group with: a bound it does not state keeps the
        // one the group already has, the way every other writer of those bounds reads them.
        let min_size = self.min_size.unwrap_or(group.min_size);
        let max_size = self.max_size.unwrap_or(group.max_size);
        if max_size < min_size {
            return Err(ScheduledActionRefusal::MaxBelowMin { max_size, min_size });
        }
        if let Some(desired_capacity) = self.desired_capacity {
            if desired_capacity < min_size || desired_capacity > max_size {
                return Err(ScheduledActionRefusal::DesiredCapacityOutOfBounds {
                    desired_capacity,
                    min_size,
                    max_size,
                });
            }
        }
        // Those checks are all RELATIVE, so none of them bounds the absolute size: the
        // capacity this action would have the group RUN is stored instances, so THAT is
        // what is held to the same ceiling every writer of a group's sizes holds a
        // capacity to — the bounds the action states are stored verbatim and hold no
        // instance of their own.
        let capacity_after_run = super::capacity_within_bounds(
            self.desired_capacity.unwrap_or(group.desired_capacity),
            min_size,
            max_size,
        );
        if let Some(size) = super::size_past_ceiling(&[capacity_after_run]) {
            return Err(ScheduledActionRefusal::SizePastCeiling(size));
        }
        if let (Some(start_time), Some(end_time)) = (self.start_time, self.end_time) {
            if end_time.as_secs_f64() <= start_time.as_secs_f64() {
                return Err(ScheduledActionRefusal::EndNotAfterStart);
            }
        }
        if let Some(recurrence) = self
            .recurrence
            .as_deref()
            .filter(|recurrence| super::recurrence::parse(recurrence).is_none())
        {
            return Err(ScheduledActionRefusal::InvalidRecurrence(
                recurrence.to_string(),
            ));
        }
        if self
            .time_zone
            .as_deref()
            .is_some_and(|time_zone| super::recurrence::time_zone(Some(time_zone)).is_none())
        {
            return Err(ScheduledActionRefusal::InvalidTimeZone);
        }
        Ok(())
    }

    /// The instant the action this put states next RUNS at, resolved as the action is
    /// stored.
    ///
    /// The instant the request stated, if it stated one; otherwise the next occurrence of
    /// its recurrence after `now`, read in the zone the put named — which is what a purely
    /// recurring action's own start time is. `None` for a put that states neither, and for a
    /// recurrence with no further occurrence.
    #[must_use]
    pub fn resolved_start_time(&self, now: SystemTime) -> Option<DateTime> {
        if let Some(stated) = self.start_time {
            return Some(stated);
        }
        let recurrence = super::recurrence::parse(self.recurrence.as_deref()?)?;
        let zone = super::recurrence::time_zone(self.time_zone.as_deref())?;
        recurrence
            .next_occurrence(now, zone)
            .map(super::to_date_time)
    }
}

/// A stored scheduled action — the `ScheduledUpdateGroupAction` resource the put creates or
/// updates.
#[derive(Debug, Clone)]
pub struct StoredScheduledAction {
    /// The ARN minted when the action was CREATED, which every later put of the same name
    /// keeps.
    pub scheduled_action_arn: String,
    /// The instant the action next runs at, which `StartTime` and the legacy `Time` both
    /// name: the one the put stated, or the next occurrence of its recurrence as the put
    /// resolved it ([`ScheduledActionPut::resolved_start_time`]).
    pub start_time: Option<DateTime>,
    pub end_time: Option<DateTime>,
    pub recurrence: Option<String>,
    pub min_size: Option<i32>,
    pub max_size: Option<i32>,
    pub desired_capacity: Option<i32>,
    pub time_zone: Option<String>,
}

impl StoredScheduledAction {
    /// The stored action as the put would leave it, keeping `scheduled_action_arn`: the
    /// definition is stated whole by each put, so an action's members are the ones its last
    /// put stated and never a merge of several.
    #[must_use]
    pub fn from_put(scheduled_action_arn: String, put: ScheduledActionPut, now: SystemTime) -> Self {
        Self {
            scheduled_action_arn,
            start_time: put.resolved_start_time(now),
            end_time: put.end_time,
            recurrence: put.recurrence,
            min_size: put.min_size,
            max_size: put.max_size,
            desired_capacity: put.desired_capacity,
            time_zone: put.time_zone,
        }
    }

    /// THE ONE projection of a stored action onto the wire shape.
    ///
    /// `Time` and `StartTime` report the one stored instant, because the first is the
    /// legacy spelling of the second; every other member is reported as the put stated it,
    /// absent when it stated none.
    #[must_use]
    pub fn view(&self, group_name: &str, action_name: &str) -> ScheduledUpdateGroupAction {
        ScheduledUpdateGroupAction {
            auto_scaling_group_name: Some(group_name.to_string()),
            scheduled_action_name: Some(action_name.to_string()),
            scheduled_action_arn: Some(self.scheduled_action_arn.clone()),
            time: self.start_time,
            start_time: self.start_time,
            end_time: self.end_time,
            recurrence: self.recurrence.clone(),
            min_size: self.min_size,
            max_size: self.max_size,
            desired_capacity: self.desired_capacity,
            time_zone: self.time_zone.clone(),
        }
    }
}

impl super::AutoScalingState {
    /// The first name of `action_names`, in the order the request states them, that a
    /// scheduled action could not be CREATED under on `group_name` because the name is
    /// already taken — by an action the group already holds, or by an earlier entry of this
    /// same list.
    ///
    /// A scheduled action's name is its identity within its group, so a call that CREATES
    /// one can only create a name the group does not hold yet; this is the check a batch is
    /// taken against ([`MAX_SCHEDULED_ACTIONS_PER_GROUP`]'s sibling rule, stated in this
    /// module's header). `None` when every name is free, and `None` for a name no group
    /// answers to: the caller resolves the group and refuses that with its own absent-group
    /// sentence.
    #[must_use]
    pub fn first_taken_scheduled_action_name(
        &self,
        group_name: &str,
        action_names: &[String],
    ) -> Option<String> {
        let group = self.groups.get(group_name)?;
        let mut stated: BTreeSet<&str> = BTreeSet::new();
        action_names
            .iter()
            .find(|name| {
                group.scheduled_actions.contains_key(name.as_str())
                    || !stated.insert(name.as_str())
            })
            .cloned()
    }

    /// Create or replace the scheduled action `action_name` of `group_name`, per the upsert
    /// rule in this module's own documentation, and answer the action as it now stands.
    ///
    /// `None` when `group_name` names no group: a scheduled action only exists against a
    /// group that does, and the caller — which is what resolves the group — answers that with
    /// its own absent-group refusal.
    ///
    /// Every rule is taken BEFORE anything is minted or written, so a refused put leaves the
    /// group's actions, and this store's resource-id sequence, exactly as they were; and the
    /// ARN is minted only for an action this put CREATES, so the identity of an existing
    /// action survives every later put of its name.
    pub fn put_scheduled_action(
        &mut self,
        group_name: &str,
        action_name: &str,
        put: ScheduledActionPut,
        now: SystemTime,
    ) -> Result<Option<StoredScheduledAction>, ScheduledActionRefusal> {
        let Some(group) = self.groups.get(group_name) else {
            return Ok(None);
        };
        put.validate(group)?;
        let region = group.region.clone();
        let held_arn = group
            .scheduled_actions
            .get(action_name)
            .map(|held| held.scheduled_action_arn.clone());
        if held_arn.is_none() && group.scheduled_actions.len() >= MAX_SCHEDULED_ACTIONS_PER_GROUP
        {
            return Err(ScheduledActionRefusal::LimitExceeded);
        }
        let arn = match held_arn {
            Some(arn) => arn,
            None => {
                let resource_id = self.mint_resource_id();
                scheduled_action_arn(&region, &resource_id, group_name, action_name)
            }
        };
        let stored = StoredScheduledAction::from_put(arn, put, now);
        if let Some(group) = self.groups.get_mut(group_name) {
            group
                .scheduled_actions
                .insert(action_name.to_string(), stored.clone());
        }
        Ok(Some(stored))
    }

    /// Delete the scheduled actions `action_names` names from `group_name`, and answer the
    /// names that could not be deleted, in the order the request states them.
    ///
    /// A name the group does not hold names no action to delete, so it is answered back for
    /// the caller to report; a name the request states TWICE names one action, which this
    /// call deletes once. Deleting a name the group holds is all a delete does — the group's
    /// own bounds and instances are untouched, because a scheduled action is a future change
    /// and not a current one.
    pub fn delete_scheduled_actions(
        &mut self,
        group_name: &str,
        action_names: &[String],
    ) -> Vec<String> {
        let Some(group) = self.groups.get_mut(group_name) else {
            return action_names.to_vec();
        };
        let mut deleted: BTreeSet<&str> = BTreeSet::new();
        let mut absent: Vec<String> = Vec::new();
        for action_name in action_names {
            if group.scheduled_actions.remove(action_name.as_str()).is_some()
                || deleted.contains(action_name.as_str())
            {
                deleted.insert(action_name.as_str());
            } else {
                absent.push(action_name.clone());
            }
        }
        absent
    }
}

/// Why a scheduled action could not be stored, for the operation to answer the error class
/// each situation belongs to.
#[derive(Debug, PartialEq, Eq)]
pub enum ScheduledActionRefusal {
    /// The request states none of the three sizes, so there is no change to schedule.
    NoSizeStated,
    /// The request states a negative `MinSize`; carries what it stated.
    NegativeMinSize(i32),
    /// The bounds the action would leave the group with are inverted; carries both, in the
    /// order the service reports them.
    MaxBelowMin { max_size: i32, min_size: i32 },
    /// The capacity the action would have the group hold is outside those bounds; carries
    /// the capacity and the bounds it has to sit between.
    DesiredCapacityOutOfBounds {
        desired_capacity: i32,
        min_size: i32,
        max_size: i32,
    },
    /// One of the sizes is past the ceiling this process holds a group to; carries it.
    SizePastCeiling(i32),
    /// The action's `EndTime` is not after its `StartTime`, so its boundaries bound no run.
    EndNotAfterStart,
    /// The `Recurrence` is not the crontab expression the member documents; carries what the
    /// request stated.
    InvalidRecurrence(String),
    /// The `TimeZone` is not a canonical IANA time zone name. Carries nothing: the
    /// refusal states the rule rather than the value that broke it
    /// ([`INVALID_TIME_ZONE_MESSAGE`]).
    InvalidTimeZone,
    /// The group already holds [`MAX_SCHEDULED_ACTIONS_PER_GROUP`] actions and this request
    /// names one more.
    LimitExceeded,
}

impl ScheduledActionRefusal {
    /// What the caller is told. ONE spelling per situation, so no two writers of a
    /// scheduled action refuse the same request differently; the sentences about the
    /// group's three sizes are the ones every writer of those sizes states
    /// ([`super::max_below_min_message`],
    /// [`super::desired_capacity_out_of_bounds_message`]).
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::NoSizeStated => SIZE_REQUIRED_MESSAGE.to_string(),
            Self::NegativeMinSize(min_size) => super::negative_min_size_message(*min_size),
            Self::MaxBelowMin { max_size, min_size } => {
                super::max_below_min_message(*max_size, *min_size)
            }
            Self::DesiredCapacityOutOfBounds {
                desired_capacity,
                min_size,
                max_size,
            } => super::desired_capacity_out_of_bounds_message(
                *desired_capacity,
                *min_size,
                *max_size,
            ),
            Self::SizePastCeiling(size) => format!(
                "The group size {size} exceeds the limit of {} instances per Auto Scaling group",
                super::MAX_GROUP_CAPACITY
            ),
            Self::EndNotAfterStart => END_TIME_MESSAGE.to_string(),
            Self::InvalidRecurrence(recurrence) => invalid_recurrence_message(recurrence),
            Self::InvalidTimeZone => INVALID_TIME_ZONE_MESSAGE.to_string(),
            Self::LimitExceeded => too_many_scheduled_actions_message(),
        }
    }

    /// Whether this refusal is a resource QUOTA rather than a bad request, which is what
    /// decides the error class the operation answers it with.
    #[must_use]
    pub fn is_quota(&self) -> bool {
        matches!(self, Self::LimitExceeded | Self::SizePastCeiling(_))
    }

    /// The wire error code this refusal is reported under when it is reported PER ACTION,
    /// inside the answer of a call that puts several at once rather than as the call's own
    /// error.
    ///
    /// The same two classes [`Self::is_quota`] decides between, under the codes a single put
    /// answers them with: a ceiling on the account's resources is the `LimitExceededFault`
    /// the operations model (wire code `LimitExceeded`), and everything else is about what
    /// the request carries, which this service answers `ValidationError` for — so one
    /// refusal reads the same whichever operation the caller reached it through.
    #[must_use]
    pub fn error_code(&self) -> &'static str {
        if self.is_quota() {
            LIMIT_EXCEEDED_ERROR_CODE
        } else {
            VALIDATION_ERROR_CODE
        }
    }
}

/// The wire error code a per-action report carries for a request about a scheduled action
/// this store has no record of, and for every other defect of what the request carries: the
/// code this service answers such a request with as its own error
/// ([`crate::scaffolded::Scaffolded::validation_error`]).
pub const VALIDATION_ERROR_CODE: &str = "ValidationError";

/// The wire error code a per-action report carries for a resource QUOTA: the `awsQueryError`
/// code of the `LimitExceededFault` the writers of a scheduled action model.
pub const LIMIT_EXCEEDED_ERROR_CODE: &str = "LimitExceeded";

/// The most scheduled action NAMES one call may state: "The names of the scheduled actions to
/// delete. The maximum number allowed is 50."
/// (`BatchDeleteScheduledActionType$ScheduledActionNames`) — the same bound a put is held to
/// per call, on the member the delete states its actions in.
#[must_use]
pub fn too_many_scheduled_action_names_in_one_call_message(count: usize) -> String {
    super::too_many_items_message(count, "scheduledActionNames", MAX_SCHEDULED_ACTIONS_PER_CALL)
}

/// The sentence a request naming a scheduled action the group does not hold is answered with.
///
/// The `<subject> not found - <detail>` shape this service states its absent-resource
/// refusals in ([`super::absent_group_message`]), for the resource word the model names a
/// scheduled action by. The action name, taken verbatim from the request, is its only
/// variable part.
#[must_use]
pub fn absent_scheduled_action_message(action_name: &str) -> String {
    format!("Scheduled action name not found - Scheduled action {action_name} not found")
}

/// The sentence a put stating none of the three sizes is refused with: the rule as the
/// model's own documentation for `PutScheduledUpdateGroupActionType$DesiredCapacity` states
/// it, which names the three members the caller may choose from. Fixed text — the defect is
/// the same whichever members the request did carry.
pub const SIZE_REQUIRED_MESSAGE: &str =
    "You must specify at least one of the following properties: MaxSize, MinSize, or \
     DesiredCapacity";

/// The sentence a put whose boundaries do not open is refused with: the defect is the ORDER
/// of the two instants rather than either of them, so no part of the request appears in it.
pub const END_TIME_MESSAGE: &str = "Given end time must be after start time";

/// The sentence a put whose `Recurrence` the crontab reader cannot read is refused with: the
/// wording this service's own recorded refusal states, which quotes the rejected value and
/// takes nothing else from the request.
#[must_use]
pub fn invalid_recurrence_message(recurrence: &str) -> String {
    format!("Given recurrence string: {recurrence} is invalid")
}

/// The sentence a put whose `TimeZone` names no zone of the IANA database is refused with:
/// the service states the catalogue the value had to come from — "the canonical names of the
/// IANA time zones, derived from the IANA Time Zone Database"
/// (`PutScheduledUpdateGroupActionType$TimeZone`) — and NOTHING of the request. Entirely
/// fixed text: the rejected zone name is not interpolated, so the refusal reads the same
/// whichever unknown zone was asked for.
pub const INVALID_TIME_ZONE_MESSAGE: &str =
    "Time zone specified does not conform to standardized time zones found in IANA Time Zone \
     Database";

/// The sentence a put that would take a group past [`MAX_SCHEDULED_ACTIONS_PER_GROUP`] is
/// refused with: Amazon EC2 Auto Scaling's own sentence for the scheduled-action cap, whose
/// one variable part is the per-group limit in force — fixed text otherwise, down to the
/// closing period.
#[must_use]
pub fn too_many_scheduled_actions_message() -> String {
    format!(
        "The maximum limit of {MAX_SCHEDULED_ACTIONS_PER_GROUP} scheduled actions has been \
         reached."
    )
}

/// The wire error code a per-action report carries for a name the group already holds: the
/// `awsQueryError` code of the `AlreadyExistsFault` both writers of a scheduled action model
/// (`smithy_query(query="shape",shape="AlreadyExistsFault")`), so the conflict reads the same
/// whether it is reported as a call's own error or per action inside a batch's answer.
pub const ALREADY_EXISTS_ERROR_CODE: &str = "AlreadyExists";

/// The sentence a request that would CREATE a scheduled action under a name the group
/// already holds is refused with.
///
/// The shape this service states its already-exists refusals in — the resource word, that a
/// resource of this name is already held, then the detail naming it — the same `<subject>
/// … - <detail>` shape its absent-resource refusals carry. The action name, taken verbatim
/// from the request, is its only variable part.
#[must_use]
pub fn scheduled_action_already_exists_message(action_name: &str) -> String {
    format!(
        "ScheduledUpdateGroupAction by this name already exists - A scheduled action with the \
         name {action_name} already exists"
    )
}

/// The ARN of a scheduled action, in the form the service reports:
/// `arn:aws:autoscaling:{region}:{account}:scheduledUpdateGroupAction:{id}:autoScalingGroupName/{group}:scheduledActionName/{name}`.
#[must_use]
pub fn scheduled_action_arn(
    region: &str,
    resource_id: &str,
    group_name: &str,
    action_name: &str,
) -> String {
    ::engine::emulator_arn(
        "autoscaling",
        region,
        &format!(
            "scheduledUpdateGroupAction:{resource_id}:autoScalingGroupName/{group_name}:\
             scheduledActionName/{action_name}"
        ),
    )
}
