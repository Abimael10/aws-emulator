// SPDX-License-Identifier: CC0-1.0

//! What ONE scaling activity records: the step it reports, the milestone that step is at,
//! and the placement of the instance it moved.
//!
//! # The rules this module implements
//!
//! `Activity$Cause` is "the reason the activity began"
//! (`smithy_query(query="doc",shape="Activity",member="Cause")`), and Amazon EC2 Auto
//! Scaling words it as a sequence of TIMESTAMPED clauses, each `At <instant> <what
//! happened>`: the clause of the request that asked for the change, and — where the group's
//! own convergence on its desired capacity is what moved the instance the activity is about
//! — the clause naming that step as well. So an activity states both when a caller asked
//! and when the group acted, rather than one sentence of this emulator's own.
//!
//! `Activity$Details` is "the details about the activity"
//! (`smithy_query(query="doc",shape="Activity",member="Details")`), and for an activity
//! about ONE instance Amazon EC2 Auto Scaling states there WHERE that instance is: a JSON
//! document naming its Availability Zone id, the subnet it was placed in and the name of
//! that Availability Zone, read from the placement the instance actually has rather than
//! from what the group asked for. So every activity this service records for an instance
//! carries that document, and one that is about the group itself carries none — there is
//! no one placement for it to report.
//!
//! `Activity$StatusCode` is a value of `ScalingActivityStatusCode` and `Activity$Progress`
//! is the percentage of the way through
//! (`smithy_query(query="shape",shape="ScalingActivityStatusCode")`). The pair an activity
//! that is still running reports is the MILESTONE of the transition it records, not one
//! in-flight pair for every transition: an instance that already exists and is being
//! brought back into service has reached a step the vocabulary has its own value for
//! ([`ActivityMilestone::PreInService`]), and a step a lifecycle hook is holding is at
//! another ([`ActivityMilestone::MidLifecycleAction`]) — neither of which a generic
//! in-flight reading distinguishes. An activity the clock has completed is `Successful` at
//! 100 whichever milestone it passed through
//! ([`super::AutoScalingState::materialize_due`]).

use super::*;

/// How Amazon EC2 Auto Scaling separates the clauses of one cause.
const CLAUSE_SEPARATOR: &str = "  ";

/// One clause of a cause: the instant it happened at, and what happened then.
///
/// THE ONE place a cause states an instant, so every clause of every cause this service
/// records reads the same way.
fn clause(at: SystemTime, sentence: &str) -> String {
    format!("At {} {sentence}", super::whole_seconds_text(at))
}

/// `Availability Zone ID` — the first member of the placement document, naming the id of
/// the zone the instance is in.
const DETAILS_AVAILABILITY_ZONE_ID: &str = "Availability Zone ID";

/// `Subnet ID` — the subnet the instance was placed in.
const DETAILS_SUBNET_ID: &str = "Subnet ID";

/// `Availability Zone` — the NAME of the zone the instance is in, the same zone the id
/// above identifies.
const DETAILS_AVAILABILITY_ZONE: &str = "Availability Zone";

/// The move a termination a CALLER NAMED states: the instance the request named went out
/// of service at that caller's request.
///
/// Reads as the `moved` clause of [`user_requested_instance_move_cause`], beside the
/// standby moves that word the same class ([`super::standby`]).
pub const TAKEN_OUT_OF_SERVICE: &str = "was taken out of service";

/// The `Activity$Cause` of the activity ONE instance a CALLER NAMED records: when the move
/// began, which instance moved, that a user asked for it and — when the group's desired
/// capacity moved with the instance — the capacities it moved between.
///
/// THE ONE wording of this class in this crate. An instance a REQUEST named and a capacity
/// change the group chose for itself are different activities: the second states the
/// difference the group was closing ([`InstanceLifecycle::convergence_sentence`]), while
/// this one states the instance and the move the caller asked for. A park a caller asked
/// for ([`super::standby`]) and a termination a caller asked for
/// ([`AutoScalingState::terminate_instances_in_group`]) are both of this class, so they
/// read the same way and differ only in the move each names.
///
/// The fixed text states that a user asked for the move and the direction the capacity went
/// with it; the variable parts are the instant, the instance id from the request and the
/// two capacities from the group's own state. `moved` is the move itself, in the past tense
/// the sentence reads in (`was moved to standby`, [`TAKEN_OUT_OF_SERVICE`]). A move that
/// left the group's capacity where it was states no capacity clause: there is no change for
/// it to name. The instant is stated by [`clause`], the one place a cause states one.
#[must_use]
pub fn user_requested_instance_move_cause(
    at: SystemTime,
    instance_id: &str,
    moved: &str,
    from: i32,
    to: i32,
) -> String {
    let asked = format!("instance {instance_id} {moved} in response to a user request");
    if from == to {
        return clause(at, &format!("{asked}."));
    }
    let direction = if to > from { "increasing" } else { "shrinking" };
    clause(
        at,
        &format!("{asked}, {direction} the capacity from {from} to {to}."),
    )
}

/// The milestone a scaling activity that is still running has reached.
///
/// WHICH milestone is a fact of the transition the activity records, so the door that
/// records one states it: the `StatusCode` and `Progress` a reader sees are read off this
/// rather than off "has it finished" alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivityMilestone {
    /// The transition is under way and the service reports no finer step than that.
    InProgress,
    /// The instance exists and is on its way into service, but is not in service yet —
    /// where a return from `Standby` is while the instance is coming back.
    PreInService,
    /// The step is PAUSED at a lifecycle hook: what it is waiting on is the hold that hook
    /// has taken on the instance, not the transition itself.
    MidLifecycleAction,
}

impl ActivityMilestone {
    /// The `ScalingActivityStatusCode` this milestone is reported under.
    #[must_use]
    pub fn status_code(self) -> &'static str {
        match self {
            Self::InProgress => ACTIVITY_IN_PROGRESS,
            Self::PreInService => ACTIVITY_PRE_IN_SERVICE,
            Self::MidLifecycleAction => ACTIVITY_MID_LIFECYCLE_ACTION,
        }
    }

    /// The `Progress` reading reported at this milestone: how far through the transition
    /// the step it names is, which is a property of the milestone and not of the clock.
    #[must_use]
    pub fn progress(self) -> i32 {
        match self {
            Self::InProgress | Self::MidLifecycleAction => 50,
            Self::PreInService => 30,
        }
    }
}

/// One scaling activity to record: what the group set out to do, why, when, the milestone
/// the step is at, and the instance whose placement the activity reports.
#[derive(Debug)]
pub struct ActivityRecord<'a> {
    /// The group the activity belongs to.
    pub group_name: &'a str,
    /// That group's ARN, which the activity reports it by.
    pub group_arn: &'a str,
    /// `Activity$Description`: what the group set out to do.
    pub description: String,
    /// `Activity$Cause`: the reason the activity began.
    pub cause: String,
    /// `Activity$AutoScalingGroupState`.
    pub group_state: &'a str,
    /// `Activity$StartTime`.
    pub start_time: SystemTime,
    /// When the activity completes, or `None` for one that is already finished.
    pub completes_at: Option<SystemTime>,
    /// The milestone it is at while it runs.
    pub milestone: ActivityMilestone,
    /// The instance the activity is about, whose placement its `Details` reports; `None`
    /// for an activity about the group itself.
    pub instance_id: Option<&'a str>,
}

impl AutoScalingState {
    /// The cause a capacity change records: when it was asked for, what asked for it, and
    /// the capacities it moved between.
    ///
    /// ONE clause, the whole cause of an activity the request itself is the step of (a park,
    /// a detach, a termination a caller named); the activity of an instance the group's own
    /// convergence then moved carries this clause and the convergence's own
    /// ([`Self::convergence_cause`]).
    pub fn capacity_change_cause(at: SystemTime, reason: &str, from: i32, to: i32) -> String {
        clause(
            at,
            &format!("{reason} changing the desired capacity from {from} to {to}."),
        )
    }

    /// The cause a change of the group's own CONSTRAINTS records: the bounds and the
    /// capacity the request left the group with, and the capacities the group moved between.
    ///
    /// A change stated as constraints names them, because what asked for the capacity is the
    /// new bounds rather than a capacity on its own.
    pub fn constraints_change_cause(
        at: SystemTime,
        reason: &str,
        bounds: (i32, i32, i32),
        from: i32,
        to: i32,
    ) -> String {
        let (min_size, max_size, desired_capacity) = bounds;
        clause(
            at,
            &format!(
                "{reason} to min: {min_size}, max: {max_size}, desired: {desired_capacity} \
                 changed the desired capacity from {from} to {to}."
            ),
        )
    }

    /// The cause of an activity the request or the condition itself is the step of, where
    /// the group's capacity did not move: when the step was asked for, and what asked for
    /// it.
    ///
    /// ONE clause, like [`Self::capacity_change_cause`], and without the capacity clause
    /// that one carries: there is no change of capacity for it to name.
    pub fn reason_cause(at: SystemTime, reason: &str) -> String {
        clause(at, &format!("{reason}."))
    }

    /// The cause the activity of ONE instance the group's own convergence moved records:
    /// the clause of the request that changed the capacity, and then the clause of the step
    /// the convergence took to meet it.
    pub(crate) fn convergence_cause(
        requested: &str,
        kind: InstanceLifecycle,
        at: SystemTime,
        from: i32,
        to: i32,
    ) -> String {
        format!(
            "{requested}{CLAUSE_SEPARATOR}{}",
            clause(at, &kind.convergence_sentence(from, to))
        )
    }

    /// Append a scaling activity that finishes at `completes_at`, answering its id.
    ///
    /// `engine` is the shared engine the caller holds: the subnet the activity's `Details`
    /// names is the one the account's own Amazon EC2 store placed the instance in, read
    /// from there rather than guessed from the group's `VPCZoneIdentifier`.
    pub fn record_activity<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        record: &ActivityRecord<'_>,
    ) -> String {
        let finished = record.completes_at.is_none();
        let status_code = if finished {
            ACTIVITY_SUCCESSFUL
        } else {
            record.milestone.status_code()
        };
        // Where the instance this activity moved actually is, when it is about one
        // instance: read while the activity is written, so what a later read reports is the
        // placement the step happened at.
        let details = record.instance_id.and_then(|instance_id| {
            self.instance_placement_details(engine, record.group_name, instance_id)
        });
        self.push_activity(StoredActivity {
            activity_id: String::new(),
            auto_scaling_group_name: record.group_name.to_string(),
            auto_scaling_group_arn: Some(record.group_arn.to_string()),
            description: Some(record.description.clone()),
            cause: record.cause.clone(),
            start_time: record.start_time,
            end_time: if finished {
                Some(record.start_time)
            } else {
                None
            },
            status_code: status_code.to_string(),
            // An activity that did not fail carries no status message.
            status_message: None,
            progress: if finished {
                100
            } else {
                record.milestone.progress()
            },
            details,
            auto_scaling_group_state: Some(record.group_state.to_string()),
            completes_at: record.completes_at,
        })
    }

    /// Append a scaling activity that placed NOTHING: the capacity change asked for an
    /// instance that was not placed, so the group reports no instance for it and the
    /// activity it recorded says so instead of claiming a launch that never happened.
    ///
    /// `description` is what the activity set out to do and `status_message` is why it
    /// did not — the two the API reports apart
    /// ([`StoredActivity::to_activity`]); [`failed_launch_description`] builds the one
    /// from the other for a launch. `status_code` is the terminal code the reason ends the
    /// activity in ([`BlockedLaunch`]), so the one recorder serves both of the two the
    /// operation documents for a launch that placed nothing.
    ///
    /// Carries no placement `Details`: nothing was placed, so there is no placement to
    /// report.
    #[allow(clippy::too_many_arguments)]
    pub fn record_failed_activity(
        &mut self,
        group_name: &str,
        group_arn: &str,
        description: String,
        status_message: String,
        status_code: &str,
        cause: String,
        start_time: SystemTime,
    ) -> String {
        self.push_activity(StoredActivity {
            activity_id: String::new(),
            auto_scaling_group_name: group_name.to_string(),
            auto_scaling_group_arn: Some(group_arn.to_string()),
            description: Some(description),
            cause,
            start_time,
            // Nothing is owed on a launch that placed nothing: it ended when it did.
            end_time: Some(start_time),
            status_code: status_code.to_string(),
            status_message: Some(status_message),
            progress: 100,
            details: None,
            auto_scaling_group_state: Some(GROUP_STATE_IN_SERVICE.to_string()),
            completes_at: None,
        })
    }

    /// Store one activity under a freshly minted id, keeping the log bounded, and answer
    /// that id.
    ///
    /// The ONE writer of [`Self::activities`], so the id every activity carries comes
    /// from one minter and the retention bound is applied in one place however the
    /// activity ended: `activity_id` on the way in is ignored.
    pub(super) fn push_activity(&mut self, activity: StoredActivity) -> String {
        let activity_id = self.mint_activity_id();
        self.activities.push(StoredActivity {
            activity_id: activity_id.clone(),
            ..activity
        });
        // Keep the activity log bounded: the oldest activity is the first to go.
        if self.activities.len() > MAX_RETAINED_ACTIVITIES {
            let excess = self.activities.len() - MAX_RETAINED_ACTIVITIES;
            self.activities.drain(..excess);
        }
        activity_id
    }

    /// The placement document an activity about `instance_id` reports in
    /// `Activity$Details`, or `None` when nothing about the instance's placement is known
    /// — in which case the member is left out rather than filled with a placeholder.
    ///
    /// THE ONE reading of an instance's placement for an activity, so the activity an
    /// operation answers with, the one a later read reports and the lifecycle event the
    /// same step publishes all name one place.
    pub(crate) fn instance_placement_details<Eng: Send + Sync + 'static>(
        &self,
        engine: &Eng,
        group_name: &str,
        instance_id: &str,
    ) -> Option<String> {
        let zone = self.instance_zone(group_name, instance_id);
        // A zone has a name AND an id ([`super::availability_zone_ids`]): the document
        // states both, so neither reader has to resolve the other.
        let zone_id = zone
            .as_deref()
            .and_then(super::availability_zone_ids::zone_id_of);
        let subnet_id = super::ec2_instances::subnet_of(engine, instance_id);
        placement_details(zone_id.as_deref(), subnet_id.as_deref(), zone.as_deref())
    }

    /// The Availability Zone the group's own record holds for one of its instances —
    /// whether the instance is a member of the group or of its warm pool, both of which
    /// are instances of the group.
    pub(crate) fn instance_zone(&self, group_name: &str, instance_id: &str) -> Option<String> {
        let group = self.groups.get(group_name)?;
        group
            .instances
            .iter()
            .chain(
                group
                    .warm_pool
                    .as_ref()
                    .into_iter()
                    .flat_map(|pool| pool.instances.iter()),
            )
            .find(|instance| instance.instance_id == instance_id)
            .map(|instance| instance.availability_zone.clone())
    }
}

/// The JSON placement document an activity about one instance carries: the members whose
/// value is known, under the keys Amazon EC2 Auto Scaling names them by and in the order
/// it writes them.
///
/// Written member by member rather than through a JSON object, whose members would come out
/// in the order their keys sort in: the document's order is the service's own.
fn placement_details(
    zone_id: Option<&str>,
    subnet_id: Option<&str>,
    zone: Option<&str>,
) -> Option<String> {
    let members: Vec<String> = [
        (DETAILS_AVAILABILITY_ZONE_ID, zone_id),
        (DETAILS_SUBNET_ID, subnet_id),
        (DETAILS_AVAILABILITY_ZONE, zone),
    ]
    .into_iter()
    .filter_map(|(key, value)| {
        value.map(|value| format!("{}:{}", json_text(key), json_text(value)))
    })
    .collect();
    (!members.is_empty()).then(|| format!("{{{}}}", members.join(",")))
}

/// One JSON string literal, escaped the way JSON escapes it.
fn json_text(value: &str) -> String {
    ::serde_json::Value::String(value.to_string()).to_string()
}
