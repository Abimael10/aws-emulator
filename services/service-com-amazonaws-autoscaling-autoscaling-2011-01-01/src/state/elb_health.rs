// SPDX-License-Identifier: CC0-1.0

//! What the ELASTIC LOAD BALANCING verdict on a group's instances MAKES OWED.
//!
//! # The rule this module implements
//!
//! A group's `HealthCheckType` says WHO decides its members are healthy: with "`ELB` — Use
//! Elastic Load Balancing health checks" the health of a member is the health the load
//! balancer reports for the TARGET that member occupies, which is the derived value
//! [`AutoScalingState::reconcile_elb_instance_health`] recomputes from that service's own
//! verdict — the one an Elastic Load Balancing v2 target group reports
//! ([`super::elbv2_targets::elb_unhealthy_instances`]) and the one a Classic Load Balancer
//! reports ([`super::elb_classic_instances::elb_unhealthy_instances`]), read together. What
//! this module owns is the
//! consequence: "Amazon EC2 Auto Scaling terminates and replaces the unhealthy instance"
//! (`SetInstanceHealth$HealthStatus`), so a member that verdict leaves reported
//! [`super::HEALTH_STATUS_UNHEALTHY`] owes the group's health check exactly what a member a
//! CALLER recorded unhealthy owes it ([`super::instance_health`]) — one debt, discharged by
//! one pass, through the one path a failed health check takes a member of a group out by.
//!
//! So nothing here replaces anything itself. It ARMS that debt
//! ([`super::HealthCheckDebt`]) from the load balancer's verdict, under this check's own
//! sentence ([`ELB_HEALTH_CHECK_FAILURE_REASON`]), and takes it back off again when the
//! verdict is taken back — which is what makes the replacement two steps owed at two
//! instants: the group reports the member unhealthy first, and acts on the check
//! [`super::UNHEALTHY_INSTANCE_REPLACEMENT`] later, so a client polling the group sees the
//! `Unhealthy` member it is about to lose.
//!
//! Only a member of a group that is load-balancer health checked
//! ([`super::elbv2_targets::elb_health_checks_enabled`]) is armed from here, and a group
//! that stops being one has the debt this check armed taken back off rather than left
//! standing: the verdict it was armed for is not one that group is held to any more. A debt
//! a CALLER armed is left exactly as it is — that is `SetInstanceHealth`'s to own — which is
//! why an already-armed debt is never re-armed or re-timed here.
//!
//! # A change the CLOCK owes
//!
//! The checks that reach the verdict run on Elastic Load Balancing's own clock, so what they
//! have decided is owed whether or not anybody calls: the arming is materialised by
//! [`AutoScalingState::materialize_due`], like every other due change of this store, right
//! after the verdict it reads is recomputed.

use std::time::SystemTime;

use super::{
    elbv2_targets, AutoScalingState, HealthCheckDebt, HEALTH_STATUS_UNHEALTHY,
    UNHEALTHY_INSTANCE_REPLACEMENT,
};

/// What the activity of an instance the group took out of service on the load balancer's
/// verdict records as the reason it began.
///
/// The sentence Amazon EC2 Auto Scaling words that activity with: its `ELB` health check
/// found the instance failing, which is what took it out of service. Fixed text, with
/// nothing of the request or of this run in it — the sibling of
/// [`super::EC2_INSTANCE_GONE_REASON`] and [`super::USER_HEALTH_CHECK_REASON`], which word
/// the same step for the other checks a group's members can fail.
pub const ELB_HEALTH_CHECK_FAILURE_REASON: &str = "an instance was taken out of service in \
                                                   response to a ELB system health check \
                                                   failure";

impl AutoScalingState {
    /// Arm the group's health check on every member the load balancer's verdict leaves
    /// reported unhealthy, and take that arming back off a member the verdict no longer
    /// condemns.
    ///
    /// Called AFTER the verdict itself has been recomputed
    /// ([`AutoScalingState::reconcile_elb_instance_health`]), so what it reads is the health
    /// of this instant rather than the health of the last one. The check it arms is acted on
    /// by [`AutoScalingState::reconcile_unhealthy_instances`], which is what replaces the
    /// member — so this pass writes one due instant and nothing else.
    pub fn arm_elb_health_check_debt(&mut self, now: SystemTime) {
        for group in self.groups.values_mut() {
            // A group whose `HealthCheckType` no longer names `ELB` is held to no verdict of
            // that service, so nothing this check armed stands: an update that changes the
            // type takes it with it rather than leaving it to fall due.
            let elb_health_checked = elbv2_targets::elb_health_checks_enabled(group);
            for instance in &mut group.instances {
                let condemned = elb_health_checked
                    && instance.health_status == HEALTH_STATUS_UNHEALTHY
                    && !instance.is_leaving_group();
                // Only the debt THIS check armed is taken back off: one a caller recorded is
                // `SetInstanceHealth`'s, and a healthy member of a group this check does not
                // govern has none of either.
                let armed_here = instance
                    .health_check_due
                    .is_some_and(|debt| debt.reason == ELB_HEALTH_CHECK_FAILURE_REASON);
                if condemned {
                    if instance.health_check_due.is_none() {
                        instance.health_check_due = Some(HealthCheckDebt {
                            due: now + UNHEALTHY_INSTANCE_REPLACEMENT,
                            reason: ELB_HEALTH_CHECK_FAILURE_REASON,
                        });
                    }
                } else if armed_here {
                    instance.health_check_due = None;
                }
            }
        }
    }
}
