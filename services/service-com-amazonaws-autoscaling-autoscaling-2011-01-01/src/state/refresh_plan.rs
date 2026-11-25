// SPDX-License-Identifier: CC0-1.0

//! The WINDOW an instance refresh replaces a group's instances inside, and the moves one
//! replacement is made of.
//!
//! A refresh does not swap an instance for its replacement in one indivisible step: it
//! either makes room for the replacement FIRST or takes the instance it replaces out
//! first, and which of the two it does is what the healthy-percentage window the refresh
//! runs under decides.
//!
//! * `MaxHealthyPercentage` "specifies the maximum percentage of the group that can be in
//!   service and healthy, or pending, to support your workload when replacing instances.
//!   The value is expressed as a percentage of the desired capacity of the Auto Scaling
//!   group. Value range is 100 to 200"
//!   (`smithy_query(query="doc",shape="RefreshPreferences",member="MaxHealthyPercentage")`).
//!   A refresh that runs under a ceiling ABOVE its group's desired capacity may therefore
//!   hold more instances than that capacity while it works, and it uses that room: it
//!   launches the replacement before taking out the instance it replaces, so the group —
//!   and the account's Amazon EC2 instances — SURGE above the capacity for as long as the
//!   two exist side by side ([`ReplacementWindow::Surge`]).
//! * A refresh that runs under no such ceiling has no room to surge into, so it replaces
//!   in place: the instance it replaces goes out of service first and its replacement is
//!   launched after, which is why the group dips BELOW its desired capacity while a
//!   replacement is in flight ([`ReplacementWindow::InPlace`]).
//!
//! `MinHealthyPercentage` is "the amount of capacity in the Auto Scaling group that must
//! pass your group's health checks to allow the operation to continue" and, of the pair,
//! "a larger range increases the number of instances that can be replaced at the same
//! time" (the same two members' own documentation). This service replaces ONE instance at
//! a time — the fewest any window admits, and so inside every range a request can state —
//! so the window decides the ORDER of the two moves and nothing here replaces more at once
//! than the narrowest range would allow.
//!
//! That one instance is taken out even where the floor leaves no room for it: a refresh
//! stating 100 percent and no ceiling to surge into is a refresh that could replace nothing
//! at all if the floor were read as a veto, and the recorded behaviour of such a refresh is
//! that it replaces the group's instances with the group DIPPING below its desired capacity
//! while each replacement is in flight. So the floor bounds how many may be out at once,
//! which is one, and the ceiling is what says whether that one goes out before its
//! replacement arrives or after.
//!
//! # One replacement, two moves and the span between them
//!
//! A replacement is a COMPOSITE of two moves the service owes an instance, and neither is
//! instant: the instance that is launched is reported `Pending` until it is in service, and
//! the one that is taken out is reported `Terminating` until it has left. A refresh waits
//! for the instance its first move started to reach that steady state before it makes the
//! second, so a replacement is retired over the phases of [`ReplacementPhase`] — the first
//! move, the span that instance takes to settle, then the second move — each one
//! [`super::INSTANCE_REFRESH_REPLACEMENT`] after the last, stored as due instants and never
//! slept for. A refresh of a group holding several instances therefore takes
//! [`STEPS_PER_REPLACEMENT`] spans per instance plus one for its own completion, which is
//! what lets a caller polling it see the group's membership — and the account's instances —
//! surge or dip while it runs.

use std::time::SystemTime;

use crate::ops::types::DesiredConfiguration;

use super::{
    AutoScalingState, FailedLifecycleStep, GroupLaunch, InstanceLifecycle,
    InstanceLifecycleChange, LaunchDestination, StoredGroup, StoredInstanceRefresh,
    INSTANCE_REFRESH_REPLACEMENT, LAUNCH_TRANSITION, LIFECYCLE_TRANSITION, REFRESH_FAILED,
    REFRESH_IN_PROGRESS, REFRESH_ROLLBACK_FAILED, UNPLACED_INSTANCE_MESSAGE,
};

/// What the status reason of a refresh Amazon EC2 could not place a replacement for says.
const UNLAUNCHABLE_REPLACEMENT_REASON: &str =
    "The instance refresh failed because a replacement instance could not be launched.";

/// And what the status reason of its ROLLBACK says, when the rollback is what could not
/// place one: a different sentence, because a different thing could not be done.
const UNLAUNCHABLE_ROLLBACK_REPLACEMENT_REASON: &str =
    "The instance refresh rollback failed because a replacement instance could not be launched.";

/// How a refresh makes room for the replacement of one instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacementWindow {
    /// The replacement is launched FIRST and the instance it replaces is taken out after,
    /// so the group holds one instance more than its desired capacity in between: the
    /// window a `MaxHealthyPercentage` above that capacity opens.
    Surge,
    /// The instance is taken out FIRST and its replacement launched after, so the group
    /// holds one instance fewer in between: what a refresh with no such ceiling does.
    InPlace,
}

/// How many of a refresh's own steps ONE replacement takes: the move the window makes room
/// with, the span the instance that move started takes to reach its own steady state, and
/// the move that completes the replacement (see the module docs).
///
/// So the steps a refresh of a group holding `n` instances owes are `n` times this, plus
/// the one that brings the refresh itself to a terminal status.
pub const STEPS_PER_REPLACEMENT: u32 = 3;

/// How far the replacement a refresh is part way through has got.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplacementPhase {
    /// The first of the replacement's two moves has been made.
    Moved,
    /// And the instance that move started has reached its own steady state, so the
    /// second move is what the replacement owes next.
    Settled,
}

/// The replacement of ONE instance, while the refresh is part way through it.
#[derive(Debug, Clone)]
pub struct RefreshReplacement {
    /// The instance being replaced.
    pub replaced_instance_id: String,
    /// The replacement launched for it, once this replacement has launched one.
    pub launched_instance_id: Option<String>,
    /// How far it has got.
    pub phase: ReplacementPhase,
}

/// What STOPPED a refresh that was part way through a replacement.
///
/// The two calls that stop one, told apart because a rollback is itself a refresh replacing
/// the group's instances BACK: the stop that starts one is reading a refresh whose status
/// still says it was going forward, while a cancellation reads whatever the refresh is doing
/// now ([`AutoScalingState::settle_stopped_replacement`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshStop {
    /// `RollbackInstanceRefresh`, which starts the rollback.
    Rollback,
    /// `CancelInstanceRefresh`, which stops the refresh — or the rollback — where it stands.
    Cancellation,
}

/// What the SECOND of a replacement's two moves is made from: the replacement it completes,
/// which way round the window makes the pair, the launch source a move that launches
/// launches from, what the activity it records says and the instant it is made at.
#[derive(Debug)]
struct ReplacementMove<'a> {
    replacement: &'a RefreshReplacement,
    window: ReplacementWindow,
    desired_configuration: Option<&'a DesiredConfiguration>,
    cause: &'a str,
    at: SystemTime,
}

impl StoredInstanceRefresh {
    /// The window this refresh replaces instances inside (see the module docs).
    ///
    /// The ceiling is the refresh's OWN `MaxHealthyPercentage` where it states one, and
    /// otherwise the one the group's instance maintenance policy states — the same pair of
    /// thresholds, stated for the group rather than for this refresh — so a refresh that
    /// states none runs under the window its group is configured with. A ceiling of 100
    /// percent is the desired capacity itself: it opens no room, so such a refresh replaces
    /// in place exactly as one under no ceiling at all does.
    ///
    /// The ceiling is read against the group's DESIRED capacity and nothing else: the member
    /// is "expressed as a percentage of the desired capacity of the Auto Scaling group"
    /// (`smithy_query(query="doc",shape="RefreshPreferences",member="MaxHealthyPercentage")`),
    /// so what decides the window is whether the one extra instance a replacement launches
    /// fits inside the count that percentage of the capacity admits — a group whose `MaxSize`
    /// leaves no room above that capacity still surges, because its maximum bounds the
    /// capacity a CHANGE may ask the group to hold (which the handlers refuse outside it),
    /// not the instances a refresh holds side by side while it replaces one. The one ceiling
    /// on the instances themselves is this store's own
    /// ([`StoredGroup::launch_headroom`]).
    #[must_use]
    pub fn replacement_window(&self, group: Option<&StoredGroup>) -> ReplacementWindow {
        let stated = self
            .preferences
            .as_ref()
            .and_then(|preferences| preferences.max_healthy_percentage)
            .or_else(|| {
                group
                    .and_then(|group| group.instance_maintenance_policy.as_ref())
                    .and_then(|policy| policy.max_healthy_percentage)
            });
        // How many instances that ceiling ADMITS, which is what says whether the one extra
        // a replacement launches fits inside it: the percentage is of the group's desired
        // capacity, so a ceiling that rounds down to that capacity itself (105 percent of
        // ten instances is ten) opens no room, exactly as 100 percent opens none.
        let desired_capacity = group.map_or(0, |group| group.desired_capacity).max(0);
        let admitted = stated
            .map(|ceiling| {
                i64::from(desired_capacity)
                    .saturating_mul(i64::from(ceiling.max(0)))
                    / 100
            })
            .unwrap_or_default();
        if admitted > i64::from(desired_capacity) {
            ReplacementWindow::Surge
        } else {
            ReplacementWindow::InPlace
        }
    }
}

impl AutoScalingState {
    /// Retire the one move the refresh at `index` owes at `due` while it still has an
    /// instance to replace, answering whether it had one.
    ///
    /// `false` says the refresh has replaced every instance it set out to and nothing is in
    /// flight, which is what brings it to a terminal status — the caller's own step
    /// ([`AutoScalingState::advance_instance_refreshes`]). `true` says this call retired a
    /// move of its own and left the next one due.
    pub(super) fn advance_refresh_replacement<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        index: usize,
        due: SystemTime,
    ) -> bool {
        let Some(refresh) = self.instance_refreshes.get(index) else {
            return false;
        };
        let group_name = refresh.auto_scaling_group_name.clone();
        let instance_refresh_id = refresh.instance_refresh_id.clone();
        let rolling_back = refresh.is_rolling_back();
        // A rollback launches from the group's OWN launch source — the configuration it was
        // using before the refresh started, since a desired configuration reaches the
        // group's settings only once the refresh has succeeded — while the refresh's own
        // replacements launch from the configuration it is moving the group to.
        let desired_configuration = if rolling_back {
            None
        } else {
            refresh.desired_configuration.clone()
        };
        let cause = if rolling_back {
            AutoScalingState::instance_refresh_rollback_cause(due, &instance_refresh_id)
        } else {
            AutoScalingState::instance_refresh_cause(due, &instance_refresh_id)
        };
        let window = refresh.replacement_window(self.groups.get(&group_name));
        let in_flight = refresh.in_flight.clone();
        let percentage_before = refresh.percentage_complete;

        match in_flight {
            // Nothing in flight: the next instance's replacement starts with the move the
            // window makes room with.
            None => {
                let Some(instance_id) = refresh.pending_instance_ids.first().cloned() else {
                    return false;
                };
                let launched = match window {
                    ReplacementWindow::Surge => {
                        let Some(launched) = self.launch_refresh_replacement(
                            engine,
                            &group_name,
                            desired_configuration.as_ref(),
                            &cause,
                            due,
                        ) else {
                            self.fail_refresh(index, rolling_back, due);
                            return true;
                        };
                        self.remember_refresh_replacement(index, &launched, rolling_back);
                        Some(launched)
                    }
                    ReplacementWindow::InPlace => {
                        self.take_refresh_instance_out(
                            engine,
                            &group_name,
                            &instance_id,
                            &cause,
                            due,
                        );
                        None
                    }
                };
                if let Some(refresh) = self.instance_refreshes.get_mut(index) {
                    // A refresh that has started replacing instances is IN PROGRESS, which
                    // is what it reports from its first move until it has none left.
                    if !rolling_back {
                        refresh.status = REFRESH_IN_PROGRESS.to_string();
                    }
                    refresh.in_flight = Some(RefreshReplacement {
                        replaced_instance_id: instance_id,
                        launched_instance_id: launched,
                        phase: ReplacementPhase::Moved,
                    });
                    refresh.next_step_due = Some(due + INSTANCE_REFRESH_REPLACEMENT);
                }
                true
            }
            // The instance that move started reaches its own steady state before the
            // replacement's second move: the launch comes into service, the termination
            // leaves the group.
            Some(replacement) if replacement.phase == ReplacementPhase::Moved => {
                if let Some(refresh) = self.instance_refreshes.get_mut(index) {
                    refresh.in_flight = Some(RefreshReplacement {
                        phase: ReplacementPhase::Settled,
                        ..replacement
                    });
                    refresh.next_step_due = Some(due + INSTANCE_REFRESH_REPLACEMENT);
                }
                true
            }
            // And the second move completes the replacement, which is the progress a
            // reader sees.
            Some(replacement) => {
                if !self.complete_refresh_replacement(
                    engine,
                    index,
                    &ReplacementMove {
                        replacement: &replacement,
                        window,
                        desired_configuration: desired_configuration.as_ref(),
                        cause: &cause,
                        at: due,
                    },
                ) {
                    self.fail_refresh(index, rolling_back, due);
                    return true;
                }
                // The replacement this step launched came into service over the refresh's
                // own span, and only once it is in service does the refresh's warmup start
                // running: "a time period, in seconds, during which an instance refresh
                // waits before moving on to replacing the next instance after a new
                // instance enters the `InService` state"
                // ([`StoredInstanceRefresh::instance_warmup`]). So the next move — the next
                // replacement's first, or this refresh's own completion once none is left —
                // comes due at the end of that wait, plus the delay the refresh holds for
                // where this replacement's progress reached a checkpoint it runs under
                // ([`StoredInstanceRefresh::checkpoint_hold`]); a rollback replaces the
                // refresh's own replacements back rather than making progress through it, so
                // it passes no checkpoint.
                if let Some(refresh) = self.instance_refreshes.get_mut(index) {
                    let warmup = refresh.instance_warmup();
                    let checkpoint_hold = if rolling_back {
                        std::time::Duration::ZERO
                    } else {
                        refresh.checkpoint_hold(percentage_before, refresh.percentage_complete)
                    };
                    refresh.next_step_due =
                        Some(due + INSTANCE_REFRESH_REPLACEMENT + warmup + checkpoint_hold);
                }
                true
            }
        }
    }

    /// Make the SECOND of a replacement's two moves and record the replacement it
    /// completed, answering whether the move could be made.
    ///
    /// `false` says Amazon EC2 could not place the replacement this move had to launch,
    /// which is a launch failure the caller ends the refresh on ([`Self::fail_refresh`]).
    /// The move itself is the other half of the window's pair: a surge takes the instance
    /// its replacement already stands in for out of service, and an in-place replacement
    /// launches the replacement for the instance it already took out.
    fn complete_refresh_replacement<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        index: usize,
        second: &ReplacementMove<'_>,
    ) -> bool {
        let Some(refresh) = self.instance_refreshes.get(index) else {
            return true;
        };
        let rolling_back = refresh.is_rolling_back();
        let group_name = refresh.auto_scaling_group_name.clone();
        let replaced_instance_id = second.replacement.replaced_instance_id.clone();
        match second.window {
            ReplacementWindow::Surge => {
                self.take_refresh_instance_out(
                    engine,
                    &group_name,
                    &replaced_instance_id,
                    second.cause,
                    second.at,
                );
            }
            ReplacementWindow::InPlace => {
                // The replacement for the instance this refresh took out is launched only
                // while the group is still SHORT of the capacity it is configured for: a
                // convergence of its own (an unrelated capacity change, a health check's
                // replacement) may have filled that gap while the replacement was in
                // flight, and the capacity is met either way — so the refresh records the
                // replacement rather than launching a second instance for capacity the
                // group already holds. The instance it set out to replace is gone whichever
                // happened, so there is nothing left for the refresh to do for it and
                // leaving it pending would be a step no later pass could ever retire.
                let short = self
                    .groups
                    .get(&group_name)
                    .is_some_and(|group| group.launches_needed(group.desired_capacity) > 0);
                if short {
                    let Some(launched) = self.launch_refresh_replacement(
                        engine,
                        &group_name,
                        second.desired_configuration,
                        second.cause,
                        second.at,
                    ) else {
                        return false;
                    };
                    self.remember_refresh_replacement(index, &launched, rolling_back);
                }
            }
        }
        self.record_completed_replacement(index, &replaced_instance_id, rolling_back);
        true
    }

    /// Remember that the refresh at `index` LAUNCHED `instance_id` as a replacement.
    ///
    /// Recorded as soon as the instance exists, which is what makes
    /// [`StoredInstanceRefresh::replaced_instance_ids`] the whole of what the refresh added
    /// to the group at every instant: a rollback asked for while a replacement is still in
    /// flight replaces that instance back like any other. A ROLLBACK's own launches are not
    /// among them — they put the group back rather than moving it on, so nothing replaces
    /// them back in turn.
    fn remember_refresh_replacement(
        &mut self,
        index: usize,
        instance_id: &str,
        rolling_back: bool,
    ) {
        if rolling_back {
            return;
        }
        if let Some(refresh) = self.instance_refreshes.get_mut(index) {
            refresh.replaced_instance_ids.push(instance_id.to_string());
        }
    }

    /// Record a replacement the refresh at `index` has COMPLETED: the instance it replaced
    /// is no longer pending, and the progress that makes is what a reader sees.
    fn record_completed_replacement(
        &mut self,
        index: usize,
        replaced_instance_id: &str,
        rolling_back: bool,
    ) {
        let Some(refresh) = self.instance_refreshes.get_mut(index) else {
            return;
        };
        refresh.in_flight = None;
        refresh
            .pending_instance_ids
            .retain(|instance_id| instance_id != replaced_instance_id);
        let outstanding = i32::try_from(refresh.pending_instance_ids.len()).unwrap_or(i32::MAX);
        if rolling_back {
            // The instances the refresh never got to "don't need to be replaced as part
            // of the rollback" (smithy_query(query="doc",shape="InstanceRefresh",
            // member="InstancesToUpdate")), so what is left to update is what it was
            // when the rollback started; what comes down is the progress the refresh had
            // made, one replaced-back instance at a time.
            refresh.percentage_complete = if refresh.instances_at_start > 0 {
                (outstanding.saturating_mul(100) / refresh.instances_at_start).clamp(0, 100)
            } else {
                0
            };
        } else {
            refresh.instances_to_update = outstanding;
            let replaced = (refresh.instances_at_start - outstanding).max(0);
            refresh.percentage_complete = if refresh.instances_at_start > 0 {
                (replaced.saturating_mul(100) / refresh.instances_at_start).clamp(0, 100)
            } else {
                100
            };
        }
    }

    /// End the refresh at `index` because Amazon EC2 could not place a replacement for it.
    ///
    /// The model documents that "an instance refresh might fail for several reasons, such
    /// as EC2 launch failures", so the instance stays pending, unreplaced, and the refresh
    /// fails rather than reporting progress nothing made.
    fn fail_refresh(&mut self, index: usize, rolling_back: bool, due: SystemTime) {
        let Some(refresh) = self.instance_refreshes.get_mut(index) else {
            return;
        };
        refresh.in_flight = None;
        refresh.status = if rolling_back {
            REFRESH_ROLLBACK_FAILED.to_string()
        } else {
            REFRESH_FAILED.to_string()
        };
        refresh.status_reason = Some(
            if rolling_back {
                UNLAUNCHABLE_ROLLBACK_REPLACEMENT_REASON
            } else {
                UNLAUNCHABLE_REPLACEMENT_REASON
            }
            .to_string(),
        );
        refresh.end_time = Some(due);
        refresh.next_step_due = None;
    }

    /// Launch ONE replacement into `group_name`, from the configuration
    /// `desired_configuration` names or the group's own launch source, recording the
    /// activity the launch produces under `cause`.
    ///
    /// Answers the id of the instance it launched, and `None` for a launch Amazon EC2 could
    /// not place — which records the failed launch instead.
    pub(super) fn launch_refresh_replacement<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        desired_configuration: Option<&DesiredConfiguration>,
        cause: &str,
        now: SystemTime,
    ) -> Option<String> {
        let arn = self.groups.get(group_name).map(|group| group.arn.clone())?;
        // The replacement is an instance coming INTO service, so it settles at the launch
        // span ([`LAUNCH_TRANSITION`]).
        let launch_due = now + LAUNCH_TRANSITION;
        let launch = self.launch_into_group(
            engine,
            &GroupLaunch {
                group_name,
                count: 1,
                launched_at: now,
                due: launch_due,
                desired_configuration,
                destination: LaunchDestination::Group,
                // A replacement places across the whole of the group's own configuration.
                placement: None,
            },
        );
        let launched_ids = launch.launched_ids.clone();
        let unplaced_message = launch.unplaced_message(UNPLACED_INSTANCE_MESSAGE);
        let unplaced_status_code = launch.unplaced_status_code();
        let unplaced_steps = launch.unplaced_steps(&unplaced_message);
        for launched_id in &launched_ids {
            self.record_instance_lifecycle_change(
                engine,
                &InstanceLifecycleChange {
                    kind: InstanceLifecycle::Launched,
                    group_name,
                    group_arn: &arn,
                    instance_id: launched_id,
                    cause,
                    start_time: now,
                    due: launch_due,
                },
            );
        }
        for (instance_id, description) in unplaced_steps {
            self.record_failed_lifecycle_step(
                engine,
                &FailedLifecycleStep {
                    kind: InstanceLifecycle::Launched,
                    group_name,
                    group_arn: &arn,
                    instance_id: instance_id.as_deref(),
                    description,
                    status_message: unplaced_message.clone(),
                    status_code: unplaced_status_code,
                    cause,
                    at: now,
                },
            );
        }
        launched_ids.first().cloned()
    }

    /// Take ONE instance of `group_name` out of service because a refresh is replacing it,
    /// recording the activity that produces under `cause`.
    ///
    /// The instance leaves through the one path a member of a group leaves it by: it is
    /// reported `Terminating` until its span is due, the Amazon EC2 instance is shut down
    /// where it lives, and a termination hook of the group holds it exactly as it holds
    /// every other termination ([`AutoScalingState::terminate_group_instances`]).
    pub(super) fn take_refresh_instance_out<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        instance_id: &str,
        cause: &str,
        now: SystemTime,
    ) {
        let Some(arn) = self.groups.get(group_name).map(|group| group.arn.clone()) else {
            return;
        };
        let due = now + LIFECYCLE_TRANSITION;
        let replaced = [instance_id.to_string()];
        self.terminate_group_instances(engine, group_name, &replaced, now, due);
        self.record_instance_lifecycle_change(
            engine,
            &InstanceLifecycleChange {
                kind: InstanceLifecycle::Terminated,
                group_name,
                group_arn: &arn,
                instance_id,
                cause,
                start_time: now,
                due,
            },
        );
    }

    /// Finish the replacement a refresh was part way through when it was STOPPED — rolled
    /// back or cancelled — so what stopped it leaves the group holding exactly the capacity
    /// it is configured for rather than the gap, or the surge, that replacement had opened.
    ///
    /// A refresh replacing IN PLACE has already taken an instance out, and the replacement
    /// it owed for it has not been launched yet, so the launch is made here. WHICH launch
    /// source it comes from is the one every other launch of this module reads: the
    /// configuration the refresh is moving the group to, and the group's own — the one a
    /// rollback restores — while the refresh is being rolled back. A cancellation finds that
    /// out from the refresh's own status, because `CancelInstanceRefresh` "cancels an
    /// instance refresh OR ROLLBACK that is in progress"
    /// ([`StoredInstanceRefresh::is_cancellable`]), so cancelling a rollback mid-replacement
    /// finishes the replacement THAT rollback was making.
    ///
    /// A refresh replacing with a SURGE has already launched its replacement while the
    /// instance it stands in for is still in service, so that instance is capacity the group
    /// holds TWICE. A cancellation completes the replacement, taking the older of the two
    /// out; a rollback is putting the group back, so it takes the SURGE out instead — the
    /// instance the refresh was replacing simply stays, and the replacement the refresh had
    /// launched stops being one of its own
    /// ([`StoredInstanceRefresh::replaced_instance_ids`]), since a rollback that replaced it
    /// back would launch a third instance for capacity the group already holds.
    ///
    /// Leaves the refresh's next due instant alone: WHEN the stop itself completes is the
    /// caller's own ([`AutoScalingState::cancel_instance_refresh`],
    /// [`AutoScalingState::roll_back_instance_refresh`]).
    pub(super) fn settle_stopped_replacement<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        index: usize,
        stop: RefreshStop,
        now: SystemTime,
    ) {
        let Some(refresh) = self.instance_refreshes.get(index) else {
            return;
        };
        let Some(replacement) = refresh.in_flight.clone() else {
            return;
        };
        let instance_refresh_id = refresh.instance_refresh_id.clone();
        let group_name = refresh.auto_scaling_group_name.clone();
        let window = refresh.replacement_window(self.groups.get(&group_name));
        // Whether what is being stopped is replacing the group's instances BACK: the stop
        // that starts a rollback is one (its status has not been written yet), and so is a
        // cancellation of a rollback already under way.
        let rolling_back = stop == RefreshStop::Rollback || refresh.is_rolling_back();
        // A rollback launches from the group's OWN launch source — the configuration it was
        // using before the refresh started — while the refresh's own replacements launch
        // from the configuration it is moving the group to.
        let desired_configuration = if rolling_back {
            None
        } else {
            refresh.desired_configuration.clone()
        };
        let cause = if rolling_back {
            AutoScalingState::instance_refresh_rollback_cause(now, &instance_refresh_id)
        } else {
            AutoScalingState::instance_refresh_cause(now, &instance_refresh_id)
        };
        if stop == RefreshStop::Rollback {
            if let Some(refresh) = self.instance_refreshes.get_mut(index) {
                refresh.in_flight = None;
            }
            if window == ReplacementWindow::Surge {
                // The surge goes back out, and the refresh stops counting it among the
                // replacements it made, so nothing replaces it back in turn.
                let Some(launched) = replacement.launched_instance_id.clone() else {
                    return;
                };
                if let Some(refresh) = self.instance_refreshes.get_mut(index) {
                    refresh
                        .replaced_instance_ids
                        .retain(|instance_id| *instance_id != launched);
                }
                self.take_refresh_instance_out(engine, &group_name, &launched, &cause, now);
                return;
            }
            // The gap the replacement left is filled from the group's own launch source —
            // and only while the group is still short of its capacity, for the reason
            // [`Self::complete_refresh_replacement`] states.
            let short = self
                .groups
                .get(&group_name)
                .is_some_and(|group| group.launches_needed(group.desired_capacity) > 0);
            if short {
                self.launch_refresh_replacement(engine, &group_name, None, &cause, now);
            }
            return;
        }
        if !self.complete_refresh_replacement(
            engine,
            index,
            &ReplacementMove {
                replacement: &replacement,
                window,
                desired_configuration: desired_configuration.as_ref(),
                cause: &cause,
                at: now,
            },
        ) {
            // A replacement Amazon EC2 could not place leaves nothing in flight either: the
            // instance it was for stays pending, unreplaced, and the refresh stops where it
            // stands.
            if let Some(refresh) = self.instance_refreshes.get_mut(index) {
                refresh.in_flight = None;
            }
        }
    }
}
