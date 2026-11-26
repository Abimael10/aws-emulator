// SPDX-License-Identifier: CC0-1.0

//! WHICH members of an Auto Scaling group an instance refresh replaces.
//!
//! # The rule this module implements
//!
//! A refresh does not replace every member it finds blindly: its preferences carry "the
//! behaviors that you want Amazon EC2 Auto Scaling to use if instances that are in Standby
//! state or protected from scale in are found"
//! (`smithy_query(query="doc",shape="StartInstanceRefreshType",member="Preferences")`), one
//! member per class of member, and each of the two states the same three choices for its own
//! class
//! (`smithy_query(query="doc",shape="RefreshPreferences",member="ScaleInProtectedInstances")`
//! and `…member="StandbyInstances"`):
//!
//! * the value that REPLACES them — `Refresh` for a member protected from scale in,
//!   `Terminate` for one parked in `Standby` — has the refresh replace that member like any
//!   other ([`MemberDisposition::Replace`]);
//! * `Ignore` has it "ignore instances that are protected from scale in and continue to
//!   replace instances that are not protected", so the member is SPARED: it stays in the
//!   group under the instance id it already had and the refresh completes over the rest
//!   ([`MemberDisposition::Spare`]);
//! * `Wait`, which both members document as their default, has it "wait one hour for you to
//!   remove scale-in protection. Otherwise, the instance refresh will fail" — so the member
//!   is one the refresh means to replace and the refresh HOLDS while the condition lasts,
//!   replacing the members it may, and fails once that wait is over
//!   ([`MemberDisposition::Wait`], [`MEMBER_CONDITION_WAIT`]).
//!
//! So a member's disposition is read off the refresh's own preferences and the state the
//! member is in at the instant it is asked about ([`disposition`]) — THE ONE reading of that
//! question, for the selection the refresh records when it starts
//! ([`members_to_replace`], the set `InstancesToUpdate` counts) and for the member each of
//! its replacements picks next ([`AutoScalingState::next_refresh_member`]) alike. Reading it
//! again at each replacement is what lets a caller clear the condition the refresh is waiting
//! on and have the refresh carry on with no further call.

use std::time::{Duration, SystemTime};

use crate::ops::types::RefreshPreferences;

use super::{
    AutoScalingState, StoredGroup, StoredInstance, INSTANCE_REFRESH_REPLACEMENT, REFRESH_FAILED,
};

/// How long a refresh WAITS for a caller to clear the condition of a member its preferences
/// say to wait on, before the refresh fails.
///
/// The documented duration of that wait — "Amazon EC2 Auto Scaling waits one hour for you to
/// remove scale-in protection. Otherwise, the instance refresh will fail"
/// (`smithy_query(query="doc",shape="RefreshPreferences",member="ScaleInProtectedInstances")`,
/// and the same sentence for `StandbyInstances` with the instances returned to service) — so
/// it keeps its value rather than being shortened here. Never slept for: it is weighed
/// against the refresh's own start time as each of its steps falls due.
pub const MEMBER_CONDITION_WAIT: Duration = Duration::from_secs(60 * 60);

/// The value either member carries to have the refresh leave the members of its class alone.
pub const BEHAVIOR_IGNORE: &str = "Ignore";

/// The value `ScaleInProtectedInstances` carries to have the refresh REPLACE a member
/// protected from scale in.
pub const SCALE_IN_PROTECTED_REPLACE: &str = "Refresh";

/// The value `StandbyInstances` carries to have the refresh replace a member parked in
/// `Standby`.
pub const STANDBY_REPLACE: &str = "Terminate";

/// The value either member carries — and the one both document as their default — to have the
/// refresh WAIT for a caller to clear the condition of the members of its class.
pub const BEHAVIOR_WAIT: &str = "Wait";

/// What a refresh does with the members of ONE class its preferences govern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberBehavior {
    /// Replace them like any other member.
    Replace,
    /// Leave them in the group and replace the rest.
    Ignore,
    /// Wait for the caller to take them out of that class, and fail once the wait is over.
    Wait,
}

/// What a refresh does with ONE member, weighing every class that member falls in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberDisposition {
    /// The refresh replaces it.
    Replace,
    /// The refresh leaves it exactly where it is: it is not one of the instances the refresh
    /// sets out to replace at all.
    Spare,
    /// The refresh means to replace it and is waiting for the condition to be cleared.
    Wait,
}

/// The condition of a member a refresh is WAITING on, which is what the sentence a refresh
/// that waited it out records says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitedCondition {
    /// The member is protected from scale in.
    ScaleInProtected,
    /// The member is parked in `Standby`.
    Standby,
}

impl WaitedCondition {
    /// The status reason a refresh that waited this condition out reports.
    ///
    /// The operation itself states WHY such a refresh failed: "an instance refresh might fail
    /// for several reasons, such as EC2 launch failures, misconfigured health checks, or not
    /// ignoring or allowing the termination of instances that are in Standby state or
    /// protected from scale in"
    /// (`smithy_query(query="operation",operation="StartInstanceRefresh")`) — so each sentence
    /// here is that documented reason, for the one class of member the refresh could not get
    /// past, in the "The instance refresh failed because …" form this service's other failure
    /// reasons state. Fixed text, with nothing of the request or of this run in it.
    #[must_use]
    pub fn status_reason(self) -> &'static str {
        match self {
            Self::ScaleInProtected => {
                "The instance refresh failed because it did not ignore or allow the \
                 termination of instances that are protected from scale in."
            }
            Self::Standby => {
                "The instance refresh failed because it did not ignore or allow the \
                 termination of instances that are in Standby state."
            }
        }
    }
}

/// The behaviour `stated` names for a class of member whose replacement is asked for by
/// `replaces`, and the one both members document as their default when the request states
/// none.
fn stated_behavior(stated: Option<&str>, replaces: &str) -> MemberBehavior {
    match stated {
        Some(value) if value == replaces => MemberBehavior::Replace,
        Some(value) if value == BEHAVIOR_IGNORE => MemberBehavior::Ignore,
        // [`BEHAVIOR_WAIT`], "the default", which is also what a value no behaviour answers
        // to leaves in force: a value outside the member's own enumeration never reaches a
        // handler, because the boundary's own constraint walk refuses it with that type's
        // value set (pinned for THIS member by `tests/all/enum_value_set_table_order.rs`).
        _ => MemberBehavior::Wait,
    }
}

/// What a refresh running under `preferences` does with the members PROTECTED FROM SCALE IN.
#[must_use]
pub fn scale_in_protected_behavior(preferences: Option<&RefreshPreferences>) -> MemberBehavior {
    stated_behavior(
        preferences
            .and_then(|preferences| preferences.scale_in_protected_instances.as_deref()),
        SCALE_IN_PROTECTED_REPLACE,
    )
}

/// What a refresh running under `preferences` does with the members parked in `Standby`.
#[must_use]
pub fn standby_behavior(preferences: Option<&RefreshPreferences>) -> MemberBehavior {
    stated_behavior(
        preferences.and_then(|preferences| preferences.standby_instances.as_deref()),
        STANDBY_REPLACE,
    )
}

/// What a refresh running under `preferences` does with `instance`.
///
/// A member can fall in both classes at once — one parked in `Standby` and protected from
/// scale in — so the behaviours of the classes it falls in are weighed together: one that
/// says to leave the member alone takes it out of the refresh altogether, and short of that
/// one that says to wait holds the refresh. A member in neither class is replaced whatever
/// the preferences state about members that are.
#[must_use]
pub fn disposition(
    preferences: Option<&RefreshPreferences>,
    instance: &StoredInstance,
) -> MemberDisposition {
    let behaviors = [
        instance
            .protected_from_scale_in
            .then(|| scale_in_protected_behavior(preferences)),
        instance
            .is_in_standby()
            .then(|| standby_behavior(preferences)),
    ];
    let mut held = MemberDisposition::Replace;
    for behavior in behaviors.into_iter().flatten() {
        match behavior {
            MemberBehavior::Ignore => return MemberDisposition::Spare,
            MemberBehavior::Wait => held = MemberDisposition::Wait,
            MemberBehavior::Replace => {}
        }
    }
    held
}

/// The condition `instance` is in that a refresh running under `preferences` WAITS on, and
/// `None` for a member it is not waiting on at all.
///
/// The scale-in protection first where a member is in both classes, so one sentence is
/// stated for one member rather than the two being guessed between.
#[must_use]
pub fn waited_condition(
    preferences: Option<&RefreshPreferences>,
    instance: &StoredInstance,
) -> Option<WaitedCondition> {
    if disposition(preferences, instance) != MemberDisposition::Wait {
        return None;
    }
    if instance.protected_from_scale_in
        && scale_in_protected_behavior(preferences) == MemberBehavior::Wait
    {
        return Some(WaitedCondition::ScaleInProtected);
    }
    Some(WaitedCondition::Standby)
}

/// The members of `group` a refresh running under `preferences` SETS OUT to replace: every
/// member that is not already on its way out of the group and not one the preferences spare.
///
/// The one selection a refresh records when it starts, so the instances it reports as left to
/// update are the instances it will actually replace — a member it spares is not counted
/// against its progress and is never picked for a replacement.
pub fn members_to_replace<'a>(
    group: &'a StoredGroup,
    preferences: Option<&'a RefreshPreferences>,
) -> impl Iterator<Item = &'a StoredInstance> {
    group.instances.iter().filter(move |instance| {
        !instance.is_leaving_group() && disposition(preferences, instance) != MemberDisposition::Spare
    })
}

/// The member the refresh at `index` replaces next, or what stands in the way of its next
/// replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum NextRefreshMember {
    /// The id of the member its next replacement replaces.
    Member(String),
    /// Every member it has left is one it is WAITING on, under the condition named.
    Waiting(WaitedCondition),
    /// It has replaced every member it set out to.
    None,
}

impl AutoScalingState {
    /// The member the refresh at `index` replaces NEXT: the first one it has left that
    /// nothing holds up, in the order it recorded them.
    ///
    /// A member its preferences say to wait on is passed over for one it may replace, so a
    /// refresh goes on replacing the members it can while a caller clears the condition
    /// ([`disposition`]); [`NextRefreshMember::Waiting`] says every member it has left is
    /// one it is waiting on. An id the group no longer holds is one the refresh has nothing
    /// to wait for, so it is picked like any other and retired by the replacement.
    pub(super) fn next_refresh_member(&self, index: usize) -> NextRefreshMember {
        let Some(refresh) = self.instance_refreshes.get(index) else {
            return NextRefreshMember::None;
        };
        // A ROLLBACK replaces the refresh's own replacements back rather than the members the
        // preferences were stated about, so nothing it has left is waited on: what it puts
        // back, it launched itself.
        let rolling_back = refresh.is_rolling_back();
        let group = self.groups.get(&refresh.auto_scaling_group_name);
        let mut waiting: Option<WaitedCondition> = None;
        for instance_id in &refresh.pending_instance_ids {
            let held = if rolling_back {
                None
            } else {
                group
                    .and_then(|group| {
                        group
                            .instances
                            .iter()
                            .find(|instance| instance.instance_id == *instance_id)
                    })
                    .and_then(|instance| {
                        waited_condition(refresh.preferences.as_ref(), instance)
                    })
            };
            match held {
                Some(condition) => waiting = waiting.or(Some(condition)),
                None => return NextRefreshMember::Member(instance_id.clone()),
            }
        }
        match waiting {
            Some(condition) => NextRefreshMember::Waiting(condition),
            None => NextRefreshMember::None,
        }
    }

    /// Hold the refresh at `index` while the condition it is waiting on lasts, and FAIL it
    /// once that wait is over.
    ///
    /// The wait runs from the refresh's own start — the instant it first found the members it
    /// is waiting on — and lasts [`MEMBER_CONDITION_WAIT`]. While it is still running the
    /// refresh is owed another look one of its own steps later, so a caller that clears the
    /// condition has the refresh carry on with no further call; the refresh's status and its
    /// progress are left exactly where they stand, since it is waiting rather than doing
    /// anything.
    pub(super) fn wait_for_refresh_member(
        &mut self,
        index: usize,
        condition: WaitedCondition,
        due: SystemTime,
    ) {
        let Some(refresh) = self.instance_refreshes.get_mut(index) else {
            return;
        };
        let waited_out = due >= refresh.start_time + MEMBER_CONDITION_WAIT;
        if !waited_out {
            refresh.next_step_due = Some(
                (due + INSTANCE_REFRESH_REPLACEMENT)
                    .min(refresh.start_time + MEMBER_CONDITION_WAIT),
            );
            return;
        }
        refresh.in_flight = None;
        refresh.status = REFRESH_FAILED.to_string();
        refresh.status_reason = Some(condition.status_reason().to_string());
        refresh.end_time = Some(due);
        refresh.next_step_due = None;
    }
}
