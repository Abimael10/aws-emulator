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
//! action belongs to — exactly as `UpdateAutoScalingGroup` reads it.
//!
//! What a put is REFUSED for, though, is only what the action itself STATES: the occurrence
//! runs at a future instant, when the group's own bounds may be anything, so an inverted pair
//! of bounds and a capacity outside them are refused where the ACTION states them, and a
//! bound the action leaves to the group is brought to bear when the occurrence runs rather
//! than when it is stored. The ceiling this process holds a group to is absolute and is
//! checked whichever way the sizes were reached. On top of those:
//!
//! * "You must specify at least one of the following properties: MaxSize, MinSize, or
//!   DesiredCapacity" (`PutScheduledUpdateGroupActionType$DesiredCapacity`): an action that
//!   states no size would leave the group as it is, so there is no action to schedule;
//! * "When StartTime and EndTime are specified with Recurrence, they form the boundaries of
//!   when the recurring action starts and stops"
//!   (`PutScheduledUpdateGroupActionType$Recurrence`): boundaries that do not open — an end
//!   at or before the start — bound no run of the action;
//! * "If you try to schedule your action in the past, Amazon EC2 Auto Scaling returns an
//!   error message"
//!   (`smithy_query(query="operation",operation="PutScheduledUpdateGroupAction")`): an
//!   instant the clock has already passed names no occurrence the action could run at;
//! * a `Recurrence` is the crontab expression that member documents and a `TimeZone` is a
//!   canonical IANA name, both read by [`super::recurrence`]: a value either reader cannot
//!   read states no schedule. WHEN the action would run is read before WHICH sizes it would
//!   leave the group with, since a schedule that names no occurrence leaves nothing for the
//!   sizes to be taken against.
//!
//! # The instant a read reports
//!
//! A scheduled action reports the instant it next RUNS at. For an action the request timed
//! itself, that is the instant it stated; for a purely recurring one, it is the next
//! occurrence of its recurrence, resolved when the action is stored — and once an occurrence
//! has been reached, the next one after it. Both instants are held to WHOLE SECONDS
//! ([`ScheduledActionPut::in_whole_seconds`]), the granularity the schedule acts on.
//!
//! # What a due occurrence does, and when an action stops being held
//!
//! A stored action is a capacity change the CLOCK owes: when an occurrence comes due the
//! action's sizes are applied to the group it belongs to, through the one capacity path every
//! writer of a group's sizes takes ([`super::AutoScalingState::run_due_scheduled_actions`]).
//! Performing one is the group's own `ScheduledActions` process, so a group that has
//! suspended it skips the occurrence rather than deferring it.
//!
//! An action that has run its last occurrence, and one whose `EndTime` has passed, is no
//! longer scheduled: the store stops holding it
//! ([`StoredScheduledAction::has_elapsed`]), so a read reports what is still pending and
//! nothing that is spent.

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
    pub fn validate(
        &self,
        group: &StoredGroup,
        now: SystemTime,
    ) -> Result<(), ScheduledActionRefusal> {
        if self.min_size.is_none() && self.max_size.is_none() && self.desired_capacity.is_none()
        {
            return Err(ScheduledActionRefusal::NoSizeStated);
        }
        // WHEN the action would run is read before WHICH sizes it would leave the group with:
        // a schedule this service cannot read names no occurrence at all, so there is no run
        // for those sizes to be taken against, and the caller is pointed at the schedule
        // rather than at numbers that are not what the request got wrong.
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
        if let (Some(start_time), Some(end_time)) = (self.start_time, self.end_time) {
            if end_time.as_secs_f64() <= start_time.as_secs_f64() {
                return Err(ScheduledActionRefusal::EndNotAfterStart);
            }
        }
        // "If you try to schedule your action in the past, Amazon EC2 Auto Scaling returns an
        // error message"
        // (`smithy_query(query="operation",operation="PutScheduledUpdateGroupAction")`): an
        // instant the clock has already passed names no occurrence the action could still be
        // run at. Taken to the SECOND, which is the granularity a stored instant holds
        // ([`Self::in_whole_seconds`]), so an action timed at the current second is still in
        // time.
        if self
            .start_time
            .is_some_and(|start_time| start_time.secs() < whole_seconds_since_epoch(now))
        {
            return Err(ScheduledActionRefusal::StartTimeInPast);
        }
        if let Some(min_size) = self.min_size.filter(|min_size| *min_size < 0) {
            return Err(ScheduledActionRefusal::NegativeMinSize(min_size));
        }
        // The bounds the action leaves the group with: a bound it does not state keeps the
        // one the group already has, the way every other writer of those bounds reads them —
        // which is what the sentences below report, because that is what the caller has to
        // change.
        let min_size = self.min_size.unwrap_or(group.min_size);
        let max_size = self.max_size.unwrap_or(group.max_size);
        // What a put is REFUSED for, though, is only what the action itself states: the
        // action runs at a FUTURE instant, when the group's own bounds may be anything at
        // all, so a bound the action leaves to the group is not a bound this put could be
        // taken against — the run brings the capacity inside whatever is in force then
        // ([`super::AutoScalingState::run_due_scheduled_actions`]).
        if self.min_size.is_some() && self.max_size.is_some() && max_size < min_size {
            return Err(ScheduledActionRefusal::MaxBelowMin { max_size, min_size });
        }
        if let Some(desired_capacity) = self.desired_capacity.filter(|desired_capacity| {
            self.min_size
                .is_some_and(|min_size| *desired_capacity < min_size)
                || self
                    .max_size
                    .is_some_and(|max_size| *desired_capacity > max_size)
        }) {
            return Err(ScheduledActionRefusal::DesiredCapacityOutOfBounds {
                desired_capacity,
                min_size,
                max_size,
            });
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
        Ok(())
    }

    /// This put with every instant it states truncated to WHOLE SECONDS.
    ///
    /// `StartTime` and `EndTime` are "the date and time […] in UTC. For example,
    /// \"2021-06-01T00:00:00Z\"" (`PutScheduledUpdateGroupActionType$StartTime`,
    /// `$EndTime`), and an occurrence is reached at minute granularity, so the sub-second
    /// fraction of a stated instant names nothing a schedule can act on: it is dropped on
    /// write rather than carried, so what a read reports is what the schedule holds.
    #[must_use]
    pub fn in_whole_seconds(mut self) -> Self {
        self.start_time = self.start_time.map(whole_seconds);
        self.end_time = self.end_time.map(whole_seconds);
        self
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
    /// Whether the action's LAST occurrence has already been reached, so it names no
    /// further run: a one-time action that has run, or a recurring one whose recurrence
    /// has no occurrence left inside its own window.
    ///
    /// Distinct from "names no run at all": an action stating neither an instant nor a
    /// recurrence was never scheduled to run and is not spent.
    pub spent: bool,
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
            // A put states the action WHOLE, so the definition it leaves behind is one
            // nothing has run yet, however many occurrences the name it replaced had spent.
            spent: false,
        }
    }

    /// The occurrence of this action the clock has made due at `now`, when one is.
    ///
    /// `None` for an action whose next run is still ahead, and for one that names no run at
    /// all. When several occurrences fell due while nobody called, this answers the LATEST of
    /// them: the group's sizes after a run of identical occurrences are the sizes after the
    /// last one, so the schedule is caught up without replaying what it would have done in
    /// between. Bounded by [`MAX_CAUGHT_UP_OCCURRENCES`] per pass, with whatever is left
    /// still due and caught up by the next one.
    #[must_use]
    pub fn due_occurrence(&self, now: SystemTime) -> Option<DueOccurrence> {
        let first = super::from_date_time(self.start_time?);
        if self.spent || first > now {
            return None;
        }
        // Read ONCE for the whole catch-up below: the expression and the zone are the same
        // at every occurrence of one action, so nothing re-reads them per step.
        let schedule = self.schedule();
        let end_time = self.end_time.map(super::from_date_time);
        let mut at = first;
        let mut next = occurrence_after(schedule.as_ref(), end_time, first);
        let mut caught_up = 0usize;
        while let Some(candidate) = next.filter(|candidate| *candidate <= now) {
            if caught_up >= MAX_CAUGHT_UP_OCCURRENCES {
                break;
            }
            at = candidate;
            next = occurrence_after(schedule.as_ref(), end_time, candidate);
            caught_up += 1;
        }
        Some(DueOccurrence { at, next })
    }

    /// The recurring schedule this action states, read: the crontab expression and the zone
    /// it is read in.
    ///
    /// `None` for an action with no recurrence — it runs at the one instant it states and no
    /// other — and for a value neither reader can read, which states no schedule at all
    /// ([`super::recurrence`]).
    #[must_use]
    fn schedule(&self) -> Option<(super::recurrence::Recurrence, ::chrono_tz::Tz)> {
        let recurrence = super::recurrence::parse(self.recurrence.as_deref()?)?;
        let zone = super::recurrence::time_zone(self.time_zone.as_deref())?;
        Some((recurrence, zone))
    }

    /// Whether this action is no longer SCHEDULED at `now`, so the store stops holding it.
    ///
    /// Either it has reached its last occurrence ([`Self::spent`]) or its own `EndTime` has
    /// passed: the boundaries a window states are where the action stops, so neither leaves
    /// anything for a later occurrence to run.
    #[must_use]
    pub fn has_elapsed(&self, now: SystemTime) -> bool {
        self.spent
            || self
                .end_time
                .map(super::from_date_time)
                .is_some_and(|end_time| end_time <= now)
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
        // The instants the action is held to are the whole seconds the schedule acts on, so
        // the put is normalized BEFORE it is taken against any rule: what the rules read and
        // what the store holds are then the one value ([`ScheduledActionPut::in_whole_seconds`]).
        let put = put.in_whole_seconds();
        put.validate(group, now)?;
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

    /// Run every scheduled action the clock has made due at `now`, and stop holding the
    /// actions that are no longer scheduled.
    ///
    /// A stored action is not only a record: it is a capacity change the CLOCK owes. When an
    /// occurrence comes due the action's `MinSize`, `MaxSize` and `DesiredCapacity` are
    /// applied to the group it belongs to — a bound it does not state keeping the one the
    /// group already has, exactly as `UpdateAutoScalingGroup` leaves it — so the change is
    /// observable on every read of the group and the instances follow it through the ONE
    /// capacity path ([`super::AutoScalingState::apply_capacity`]).
    ///
    /// Performing a due occurrence is the group's `ScheduledActions` process
    /// ([`super::suspended_processes::PROCESS_SCHEDULED_ACTIONS`]), so a group that has
    /// suspended it SKIPS the occurrence: the schedule moves on to the occurrence after it
    /// and the group's sizes are left exactly where they are, which is why resuming the
    /// process does not bring back the occurrence that was missed.
    ///
    /// An action that has run its last occurrence, and one whose `EndTime` has passed, is no
    /// longer scheduled and is dropped ([`StoredScheduledAction::has_elapsed`]) — so a client
    /// reconciling its schedule against `DescribeScheduledActions` sees a spent action drop
    /// out, and the per-group count of them falls with it.
    ///
    /// A change the clock owes, which is why [`super::AutoScalingState::materialize_due`]
    /// retires it and a REPORTING operation materialises it: the occurrence was due at `now`
    /// whether or not anybody called.
    pub fn run_due_scheduled_actions<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        now: SystemTime,
    ) {
        // Read whole before anything is applied: applying one occurrence borrows the group it
        // belongs to, and launches instances in the account's own Amazon EC2 store.
        let due: Vec<DueScheduledAction> = self
            .groups
            .values()
            .flat_map(|group| {
                group
                    .scheduled_actions
                    .iter()
                    .filter_map(move |(action_name, action)| {
                        action.due_occurrence(now).map(|occurrence| {
                            DueScheduledAction {
                                group_name: group.name.clone(),
                                action_name: action_name.clone(),
                                occurrence,
                                performed: group.performs_scheduled_actions(),
                                min_size: action.min_size,
                                max_size: action.max_size,
                                desired_capacity: action.desired_capacity,
                            }
                        })
                    })
            })
            .collect();
        for action in due {
            // The occurrence is CONSUMED whether or not the group performed it: a schedule
            // moves with the clock, so an occurrence a suspended process skipped is one the
            // group never goes back for.
            if let Some(stored) = self
                .groups
                .get_mut(&action.group_name)
                .and_then(|group| group.scheduled_actions.get_mut(&action.action_name))
            {
                match action.occurrence.next {
                    Some(next) => stored.start_time = Some(super::to_date_time(next)),
                    None => stored.spent = true,
                }
            }
            if action.performed {
                self.apply_scheduled_action(engine, &action);
            } else {
                self.record_skipped_scheduled_action(&action);
            }
        }
        for group in self.groups.values_mut() {
            group
                .scheduled_actions
                .retain(|_, action| !action.has_elapsed(now));
        }
    }

    /// Apply one due occurrence's sizes to the group it belongs to, at the instant it came
    /// due.
    ///
    /// Applied AT that instant rather than at the instant somebody looked, exactly as a
    /// deferred capacity change is: the instances it launches and the cooldown it starts are
    /// anchored there, so what a caller sees does not depend on when they read. The capacity
    /// is re-applied only when there is something to do — a change of capacity, or instances
    /// still to launch for it — so an occurrence that leaves the group where it already was
    /// starts no cooldown.
    fn apply_scheduled_action<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        action: &DueScheduledAction,
    ) {
        let Some(group) = self.groups.get_mut(&action.group_name) else {
            return;
        };
        let held_capacity = group.desired_capacity;
        // The bounds the action leaves the group with, read the way every writer of them
        // reads them, and kept consistent with each other however the group's own bounds
        // have moved since the action was stored.
        let min_size = action.min_size.unwrap_or(group.min_size).max(0);
        let max_size = action.max_size.unwrap_or(group.max_size).max(min_size);
        let desired_capacity = super::capacity_within_bounds(
            action.desired_capacity.unwrap_or(held_capacity),
            min_size,
            max_size,
        )
        .clamp(0, super::MAX_GROUP_CAPACITY);
        group.min_size = min_size;
        group.max_size = max_size;
        // The group's own sizes are one half of the difference its warm pool maintains, so
        // this change owes the preparation of whatever the pool is now short of, from the
        // instant the occurrence came due.
        group.owe_warm_pool_preparation(action.occurrence.at);
        let group_arn = group.arn.clone();
        let owes_launch = group.launches_needed(desired_capacity) > 0;
        let cause = Self::constraints_change_cause(
            action.occurrence.at,
            &scheduled_action_cause_subject(&action.action_name),
            (min_size, max_size, desired_capacity),
            held_capacity,
            desired_capacity,
        );
        // The EXECUTION of the action is itself a step the group took, so it is recorded as
        // one activity of the group's own history — a step that is over the instant it
        // happened, naming the schedule that took it. The steps the capacity change then
        // takes (an instance launched, one scaled in) are activities of their own, recorded
        // by the one capacity path below.
        self.record_activity(
            engine,
            &super::ActivityRecord {
                group_name: &action.group_name,
                group_arn: &group_arn,
                description: scheduled_action_description(&action.action_name),
                cause: cause.clone(),
                start_time: action.occurrence.at,
                completes_at: None,
                milestone: super::ActivityMilestone::InProgress,
                instance_id: None,
            },
        );
        if desired_capacity != held_capacity || owes_launch {
            self.apply_capacity(
                engine,
                &action.group_name,
                desired_capacity,
                &cause,
                action.occurrence.at,
            );
        }
    }

    /// Record the occurrence a group SKIPPED because the process that performs one is
    /// suspended.
    ///
    /// A skip is a step of the group's history as much as an execution is: the occurrence
    /// came due and the group did not take it, which is the only signal a client has that
    /// the suspension — rather than a schedule that never fired — is why the group did not
    /// move. The activity is terminal the instant it was due, carries the sizes the group
    /// still holds, and states the process that is suspended as the reason nothing was done.
    fn record_skipped_scheduled_action(&mut self, action: &DueScheduledAction) {
        let Some(group) = self.groups.get(&action.group_name) else {
            return;
        };
        let group_arn = group.arn.clone();
        self.record_failed_activity(
            &action.group_name,
            &group_arn,
            skipped_scheduled_action_description(&action.action_name),
            SKIPPED_SCHEDULED_ACTION_MESSAGE.to_string(),
            super::ACTIVITY_CANCELLED,
            Self::reason_cause(
                action.occurrence.at,
                &skipped_scheduled_action_subject(&action.action_name),
            ),
            action.occurrence.at,
        );
    }
}

/// The occurrence of one action the clock has made due, and the occurrence that follows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DueOccurrence {
    /// The instant the occurrence came due at, which the change it makes is anchored to.
    pub at: SystemTime,
    /// The instant the action next runs at after it, or `None` when it has none left.
    pub next: Option<SystemTime>,
}

/// One due occurrence as the pass that runs it reads it: which action of which group, what it
/// sets, and whether the group performs its scheduled actions at all.
#[derive(Debug, Clone)]
struct DueScheduledAction {
    group_name: String,
    action_name: String,
    occurrence: DueOccurrence,
    performed: bool,
    min_size: Option<i32>,
    max_size: Option<i32>,
    desired_capacity: Option<i32>,
}

/// How many occurrences of ONE action a single pass catches up on.
///
/// The pass answers the LATEST occurrence that fell due, so a bound is needed only because a
/// per-minute recurrence names arbitrarily many of them across a long gap; whatever is left
/// is still due and is caught up by the next pass. Kept small, because the pass runs under
/// this store's own lock.
const MAX_CAUGHT_UP_OCCURRENCES: usize = 100;

/// The occurrence of a recurring schedule strictly after `after`, within the window its
/// `EndTime` closes.
///
/// `None` for an action with no readable recurrence — it runs at the one instant it states and
/// no other — and `None` past that end: "when StartTime and EndTime are specified with
/// Recurrence, they form the boundaries of when the recurring action starts and stops"
/// (`PutScheduledUpdateGroupActionType$Recurrence`), so an occurrence beyond the end is
/// outside the schedule.
#[must_use]
fn occurrence_after(
    schedule: Option<&(super::recurrence::Recurrence, ::chrono_tz::Tz)>,
    end_time: Option<SystemTime>,
    after: SystemTime,
) -> Option<SystemTime> {
    let (recurrence, zone) = schedule?;
    let next = recurrence.next_occurrence(after, *zone)?;
    end_time
        .is_none_or(|end_time| next <= end_time)
        .then_some(next)
}

/// What the scaling activity a scheduled action's occurrence records names as having asked
/// for the capacity: the scheduled action itself, BY NAME, in the clause this service states
/// a change of a group's CONSTRAINTS in
/// ([`super::AutoScalingState::constraints_change_cause`]).
///
/// The resource that drove the change is what the subject of a cause names — the policy a
/// request performed, the alarm and policy an alarm-driven execution performed
/// ([`super::policy_execution::alarm_execution_cause`]) — so a client reading a group's
/// activity history can tell WHICH schedule moved it, and tell one occurrence of it from
/// another change of the same group.
#[must_use]
fn scheduled_action_cause_subject(action_name: &str) -> String {
    format!("a scheduled action {action_name} update of AutoScalingGroup constraints")
}

/// What the activity the EXECUTION of an occurrence records says the group set out to do:
/// that it performed the scheduled action, and which one.
///
/// `Activity$Description` is "a friendly, more verbose description of the scaling activity"
/// (`smithy_query(query="shape",shape="Activity")`), so it states the step rather than the
/// reason the cause above carries. The action's name is its only variable part.
#[must_use]
fn scheduled_action_description(action_name: &str) -> String {
    format!("Executing scheduled action {action_name}")
}

/// What the activity a SKIPPED occurrence records says the group set out to do: that it
/// skipped the scheduled action, which one, and the process that is turned off — so the one
/// line a client reads in the history says both what did not happen and why. The action's
/// name is its only variable part.
#[must_use]
fn skipped_scheduled_action_description(action_name: &str) -> String {
    format!("Skipped scheduled action {action_name}")
}

/// The subject of the cause a skipped occurrence records: that the schedule came due and the
/// group did not perform it, naming the action and the process that is turned off.
#[must_use]
fn skipped_scheduled_action_subject(action_name: &str) -> String {
    format!(
        "a scheduled action {action_name} was not performed because the {} process of the \
         group is suspended",
        super::suspended_processes::PROCESS_SCHEDULED_ACTIONS
    )
}

/// What a skipped occurrence's activity reports as its `StatusMessage`: why nothing was done,
/// naming the suspended process. Fixed text — the reason is the same whichever action was
/// skipped, which the description above names.
pub const SKIPPED_SCHEDULED_ACTION_MESSAGE: &str =
    "The scheduled action came due, but the ScheduledActions process is suspended.";

/// `time` truncated to whole seconds: the granularity a scheduled action's instants are held
/// and acted on at ([`ScheduledActionPut::in_whole_seconds`]).
#[must_use]
fn whole_seconds(time: DateTime) -> DateTime {
    DateTime::from_secs(time.secs())
}

/// `at` as whole seconds since the epoch, for comparing a stated instant against the clock at
/// the granularity the store holds it.
#[must_use]
fn whole_seconds_since_epoch(at: SystemTime) -> i64 {
    at.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since_epoch| i64::try_from(since_epoch.as_secs()).ok())
        .unwrap_or(0)
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
    /// The instant the action would start at has already passed, so it names no occurrence
    /// the action could still run at. Carries nothing: the refusal states the rule rather
    /// than the instant that broke it ([`START_TIME_IN_PAST_MESSAGE`]).
    StartTimeInPast,
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
            Self::StartTimeInPast => START_TIME_IN_PAST_MESSAGE.to_string(),
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

/// The sentence a put whose start time the clock has already passed is refused with: the
/// defect is that the instant is behind the clock rather than anything about the value, so no
/// part of the request appears in it. Entirely fixed text.
pub const START_TIME_IN_PAST_MESSAGE: &str = "Given start time is in the past";

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
