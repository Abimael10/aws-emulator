// SPDX-License-Identifier: CC0-1.0

//! The instances a group holds that the account's Amazon EC2 store NO LONGER RUNS.
//!
//! An instance of an Auto Scaling group is an Amazon EC2 instance of the account
//! ([`super::ec2_instances`]), so anything that can terminate an Amazon EC2 instance can
//! take a member of a group away under it — a caller of Amazon EC2's own
//! `TerminateInstances` among them. Amazon EC2 Auto Scaling performs its own health check
//! on the instances of a group and "if an instance terminates unexpectedly, the group
//! launches a replacement" so that the group goes on holding the capacity it is configured
//! for: a group whose member Amazon EC2 is no longer running is short of the units that
//! member was providing, and the group launches what that shortfall needs.
//!
//! That is a change the CLOCK owes rather than one a later request asks for — the health
//! check runs whether or not anybody calls — so it is materialised by
//! [`super::AutoScalingState::materialize_due`] like every other due change of this store,
//! and the instance leaves the group through the one path a member of a group leaves it by:
//! reported `Terminating` until [`super::LIFECYCLE_TRANSITION`] is due, with the lifecycle
//! event and the scaling activity every other termination records.
//!
//! Nothing is taken out on an assumption: the ids come from Amazon EC2's own store
//! ([`super::ec2_instances::gone`]), and an engine carrying no Amazon EC2 store at all
//! leaves every group exactly as it stands.

use std::collections::BTreeSet;
use std::time::SystemTime;

use super::{
    ec2_instances, AutoScalingState, InstanceLifecycle, InstanceLifecycleChange,
    LIFECYCLE_TRANSITION,
};

/// What the activity of an instance the group lost to an Amazon EC2 state change records,
/// and what asked for the capacity its replacement adds.
///
/// The sentence Amazon EC2 Auto Scaling words that activity with: the group's health check
/// found the instance terminated or stopped, which is what took it out of service. Fixed
/// text, with nothing of the request or of this run in it.
pub const EC2_INSTANCE_GONE_REASON: &str = "an instance was taken out of service in response \
                                            to an EC2 health check indicating it has been \
                                            terminated or stopped";

/// What asked for the capacity the group then launches: the instance that went away has to
/// be replaced, which is what Amazon EC2 Auto Scaling words the activity of that launch
/// with.
///
/// A DIFFERENT sentence from the one above, because it records a different thing: one
/// activity says why an instance left the group, the other why a new one was launched into
/// it. Fixed text, with nothing of the request or of this run in it.
pub const UNHEALTHY_REPLACEMENT_REASON: &str = "an instance was launched in response to an \
                                               unhealthy instance needing to be replaced";

impl AutoScalingState {
    /// Take out of every group the instances the account's Amazon EC2 store no longer runs,
    /// and let each group converge on the capacity it is configured for — which launches a
    /// replacement for every unit those instances were providing.
    ///
    /// An instance already on its way out of its group ([`StoredInstance::is_leaving_group`])
    /// is left alone: it is leaving for a reason the group itself recorded, and Amazon EC2
    /// reports it shutting down precisely because the group asked for that.
    ///
    /// Reads Amazon EC2's store ONCE for every member of every group, and writes nothing
    /// for a group that has lost nothing, so the pass every handler makes costs one read of
    /// each group's members and one read of that store.
    pub fn reconcile_terminated_instances<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        now: SystemTime,
    ) {
        // The members of each group that could still be counted towards its capacity, read
        // out before Amazon EC2's store is reached so this store is not borrowed across it.
        let membership: Vec<(String, Vec<String>)> = self
            .groups
            .values()
            .map(|group| {
                let instance_ids: Vec<String> = group
                    .instances
                    .iter()
                    .filter(|instance| !instance.is_leaving_group())
                    .map(|instance| instance.instance_id.clone())
                    .collect();
                (group.name.clone(), instance_ids)
            })
            .filter(|(_, instance_ids)| !instance_ids.is_empty())
            .collect();
        if membership.is_empty() {
            return;
        }

        // Amazon EC2's store is read ONCE for every member of every group, so this pass
        // costs one read of that store however many groups the account holds. No Amazon EC2
        // store is no evidence about any of them, so the pass then leaves them all where
        // they are.
        let every_member: Vec<String> = membership
            .iter()
            .flat_map(|(_, instance_ids)| instance_ids.iter().cloned())
            .collect();
        let Some(gone) = ec2_instances::gone(engine, &every_member) else {
            return;
        };
        if gone.is_empty() {
            return;
        }
        let gone: BTreeSet<String> = gone.into_iter().collect();

        for (group_name, instance_ids) in membership {
            let lost: Vec<String> = instance_ids
                .into_iter()
                .filter(|instance_id| gone.contains(instance_id))
                .collect();
            if lost.is_empty() {
                continue;
            }
            // Out of service and replaced through THE ONE path a failed health check takes
            // a member out by, under this check's own sentence for why it left the group.
            self.replace_unhealthy_instances(
                engine,
                &group_name,
                &lost,
                EC2_INSTANCE_GONE_REASON,
                now,
            );
        }
    }

    /// Take the instances `instance_ids` names out of `group_name` because a HEALTH CHECK
    /// of the group found them unhealthy, and let the group converge on the capacity it is
    /// configured for — which launches a replacement for every unit they were providing.
    ///
    /// THE ONE path a failed health check takes a member of a group out by, whichever check
    /// failed: the Amazon EC2 state change this module notices
    /// ([`EC2_INSTANCE_GONE_REASON`]) and the status a caller records
    /// ([`super::instance_health`]) are the same replacement, so the instance leaves and is
    /// replaced identically for both. `reason` is what the check that failed records as
    /// having taken the instance out of service; the launch that follows records its OWN
    /// ([`UNHEALTHY_REPLACEMENT_REASON`]), since what it did was launch an instance rather
    /// than take one out.
    ///
    /// Does nothing for a name no group answers to, and nothing for an id the group does
    /// not hold or is already taking out of service.
    pub(super) fn replace_unhealthy_instances<Eng: Send + Sync + 'static>(
        &mut self,
        engine: &Eng,
        group_name: &str,
        instance_ids: &[String],
        reason: &str,
        now: SystemTime,
    ) {
        let Some(group) = self.groups.get(group_name) else {
            return;
        };
        let arn = group.arn.clone();
        // The capacity the group is configured for does not change: the instances went out
        // of service under it, so what it is short of is what it launches.
        let desired_capacity = group.desired_capacity;
        let cause = Self::reason_cause(now, reason);
        let replacement_cause = Self::reason_cause(now, UNHEALTHY_REPLACEMENT_REASON);
        let due = now + LIFECYCLE_TRANSITION;
        self.terminate_group_instances(engine, group_name, instance_ids, now, due);
        for instance_id in instance_ids {
            self.record_instance_lifecycle_change(
                engine,
                &InstanceLifecycleChange {
                    kind: InstanceLifecycle::Terminated,
                    group_name,
                    group_arn: &arn,
                    instance_id,
                    cause: &cause,
                    start_time: now,
                    due,
                },
            );
        }
        // The same convergence every change to a group's size goes through, which is what
        // launches the replacements the group is now short of — recorded under its own
        // cause, since what it did was launch an instance rather than take one out.
        self.apply_capacity(engine, group_name, desired_capacity, &replacement_cause, now);
    }
}
