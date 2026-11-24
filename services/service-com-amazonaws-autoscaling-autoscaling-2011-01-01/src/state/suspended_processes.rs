// SPDX-License-Identifier: CC0-1.0

//! The scaling processes a group has suspended: which of the service's own processes are
//! turned off for one Auto Scaling group, and when each was turned off.
//!
//! A suspension is a fact about ONE group, so it is stored on [`StoredGroup`] itself
//! ([`StoredGroup::suspended_processes`]): the one collection `SuspendProcesses` writes
//! into, `DescribeAutoScalingGroups` projects from, and the group's own delete takes with
//! the group. Nothing in this crate holds a second copy.
//!
//! # What a suspension names
//!
//! The processes a request may name are the service's own catalog
//! ([`super::SCALING_PROCESS_TYPES`], the same set `DescribeScalingProcessTypes`
//! publishes), and "if you omit this property, all processes are specified"
//! (`ScalingProcessQuery$ScalingProcesses`) — so a request that names none suspends every
//! process of the catalog. A name outside the catalog names no process of this service, so
//! the request is refused and nothing is written
//! ([`ProcessRefusal::UnknownProcesses`]).
//!
//! The collection is keyed by process name, so naming a process twice in one request, or
//! naming one a later request names again, leaves ONE suspension: a process is either
//! suspended or it is not. The instant kept beside the name is when the process was
//! suspended, which is what the group reports as the reason it is
//! ([`suspension_reason`]) — so a process that is already suspended keeps the instant its
//! own suspension was recorded at.
//!
//! # What a suspended process stops
//!
//! A suspension is not only a record: the process is what the group would otherwise do.
//! `Launch` "adds instances to the Auto Scaling group when the group scales out" and
//! `Terminate` "removes instances from the Auto Scaling group when the group scales in"
//! (`ProcessType$ProcessName`'s own catalog, as the user guide describes each type), so
//! while one of them is suspended a capacity change still records the capacity the caller
//! asked for while the group adds — or removes — nothing
//! ([`super::AutoScalingState::apply_capacity`]).
//!
//! So a resumption is not only a record either: the group does again what the resumed
//! process does, which is to bring its instances to the capacity it holds — the capacity
//! the changes made while the process was suspended recorded
//! ([`resumed_convergence_cause`] is the cause that convergence records).
//!
//! The health of a group's members is two processes rather than one, and each stops only
//! what it does: `HealthCheck` is what REACHES the verdict — it marks a member unhealthy
//! when the member's Amazon EC2 state, or the load balancer checking it, says it is out of
//! service ([`PROCESS_HEALTH_CHECK`]) — and `ReplaceUnhealthy` is what ACTS on that verdict,
//! terminating the member and launching a replacement for the capacity it was providing
//! ([`PROCESS_REPLACE_UNHEALTHY`]). So a group that has suspended the replacement alone goes
//! on reporting a member it found unhealthy, in the lifecycle state that member already
//! held, and one that has suspended the check alone reaches no verdict to act on.
//!
//! `AddToLoadBalancer` is the registration a group's load balancing is made of
//! ([`PROCESS_ADD_TO_LOAD_BALANCER`]): while it is suspended the group still launches the
//! instance and brings it into service, and registers it with none of the load balancers or
//! target groups it is attached to ([`super::elbv2_targets`]). That registration is owed by
//! the instance coming into service, so an instance that came into service while the process
//! was suspended is not registered by the resumption either — the mark stays on the instance
//! ([`super::StoredInstance::load_balancer_registration_withheld`]) and only the instances
//! that come into service after the resumption are registered.

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::SuspendedProcess;

use super::{StoredGroup, StoredInstance};

impl StoredInstance {
    /// Whether this instance is one the group's load balancing owes a registration for:
    /// nothing was withheld from it when the group brought it into service
    /// ([`StoredInstance::load_balancer_registration_withheld`]).
    ///
    /// THE ONE reading of that mark, for every load balancer kind a group registers its
    /// instances with — a Classic Load Balancer ([`super::elb_classic_instances`]) and an
    /// Elastic Load Balancing v2 target group ([`super::elbv2_targets`]) — so the suspension
    /// withholds the registration from both alike.
    #[must_use]
    pub fn takes_load_balancer_registration(&self) -> bool {
        !self.load_balancer_registration_withheld
    }
}

/// The process that adds instances to a group when it scales out, as the catalog names it.
pub const PROCESS_LAUNCH: &str = "Launch";

/// The process that removes instances from a group when it scales in, as the catalog names
/// it.
pub const PROCESS_TERMINATE: &str = "Terminate";

/// The process that RECEIVES the CloudWatch alarm notifications the group's scaling policies
/// are driven by, as the catalog names it.
///
/// What it does is take the alarm notification and perform the policy the alarm's action
/// names, so while it is suspended an alarm-driven execution of a policy of this group is
/// declined and the group's capacity is left where it is
/// ([`super::alarm_driven_policies`]) — a policy performed by a request of its own is
/// another process and is not affected.
pub const PROCESS_ALARM_NOTIFICATION: &str = "AlarmNotification";

/// The process that CHECKS the health of a group's instances, as the catalog names it.
///
/// What it does is reach the verdict: it is what marks a member unhealthy when the Amazon
/// EC2 state of that member, or the load balancer checking it, says it is out of service. So
/// while it is suspended the group reaches no such verdict and leaves the health it reports
/// for its members exactly as it stands ([`super::ec2_health`], [`super::elb_health`]) — a
/// status a CALLER records with `SetInstanceHealth` is that caller's check rather than this
/// process and is recorded whatever this group has suspended.
pub const PROCESS_HEALTH_CHECK: &str = "HealthCheck";

/// The process that REPLACES a member a health check has found unhealthy, as the catalog
/// names it.
///
/// What it does is act on the verdict: it terminates the member the check condemned and
/// launches a replacement for the capacity that member was providing. The marking is the
/// other process ([`PROCESS_HEALTH_CHECK`]), so while this one is suspended the group goes
/// on reporting the member it found unhealthy — with the `LifecycleState` it already held —
/// and takes nothing out and launches nothing for it
/// ([`super::AutoScalingState::reconcile_unhealthy_instances`]); resuming it leaves the
/// replacement owed, so the group then performs it with no further call from anybody.
pub const PROCESS_REPLACE_UNHEALTHY: &str = "ReplaceUnhealthy";

/// The process that REGISTERS a group's instances with the load balancers and target groups
/// the group is attached to, as the catalog names it.
///
/// What it does is "add instances to the attached load balancer or target group when they
/// launch", so while it is suspended the group brings an instance into service without
/// registering it anywhere ([`super::elbv2_targets`]) — and because the registration belongs
/// to the instance coming into service rather than to the attachment, resuming the process
/// does not register the instances that came into service while it was suspended: only the
/// ones that come into service afterwards are registered, and an earlier one is registered
/// by whoever registers it with the load balancing service itself.
pub const PROCESS_ADD_TO_LOAD_BALANCER: &str = "AddToLoadBalancer";

/// The suspended processes of ONE group: the instant each process was suspended at, keyed
/// by the process name.
///
/// A map rather than a list, because a process is either suspended or it is not: a request
/// naming the same process twice suspends one, and the group reports one entry per
/// suspended process, in the order this collection holds them.
pub type SuspendedProcesses = BTreeMap<String, SystemTime>;

/// Why a suspension could not be recorded, for the operation to answer the error class the
/// situation belongs to.
#[derive(Debug, PartialEq, Eq)]
pub enum ProcessRefusal {
    /// The request names processes outside this service's own catalog; carries every one
    /// it named, in the order the request wrote them.
    UnknownProcesses(Vec<String>),
}

impl ProcessRefusal {
    /// What the caller is told. ONE spelling per situation, so no two writers of a
    /// suspension refuse the same request differently.
    pub fn message(&self) -> String {
        match self {
            Self::UnknownProcesses(processes) => unknown_process_message(processes),
        }
    }
}

impl StoredGroup {
    /// Suspend the processes `requested` names — or every process of the catalog when it
    /// names none — as of `now`.
    ///
    /// Every name is checked BEFORE the first write, so a refused request leaves the
    /// group's suspensions exactly as they were. A process already suspended keeps the
    /// instant it was suspended at, since it has been suspended since then.
    pub fn suspend_processes(
        &mut self,
        requested: &[String],
        now: SystemTime,
    ) -> Result<(), ProcessRefusal> {
        let named = named_processes(requested)?;
        for process in named {
            self.suspended_processes.entry(process).or_insert(now);
        }
        Ok(())
    }

    /// Resume the processes `requested` names — or every process of the catalog when it
    /// names none — and answer the ones that were actually suspended until now.
    ///
    /// Every name is checked BEFORE the first write, exactly as a suspension checks it, so a
    /// refused request leaves the group's suspensions exactly as they were. Resuming a
    /// process the group was already running is not an error and changes nothing: a process
    /// is either suspended or it is not.
    ///
    /// The names answered are what the group has STOPPED having suspended, which is what
    /// tells a caller whether a process the group's own behaviour depends on
    /// ([`PROCESS_LAUNCH`], [`PROCESS_TERMINATE`]) is now running again.
    pub fn resume_processes(&mut self, requested: &[String]) -> Result<Vec<String>, ProcessRefusal> {
        let named = named_processes(requested)?;
        Ok(named
            .into_iter()
            .filter(|process| self.suspended_processes.remove(process).is_some())
            .collect())
    }

    /// Whether `process` is suspended for this group, so the group does not do what that
    /// process does.
    #[must_use]
    pub fn is_process_suspended(&self, process: &str) -> bool {
        self.suspended_processes.contains_key(process)
    }

    /// Whether this group's own health checks REACH a verdict about its members: the process
    /// that performs them is running ([`PROCESS_HEALTH_CHECK`]).
    ///
    /// THE ONE reading of that question, for every check of the group that marks a member —
    /// the Amazon EC2 state change that leaves a member no longer running
    /// ([`super::ec2_health`]) and the load balancer's verdict on the target it occupies
    /// ([`super::elb_health`]) — so a group that has suspended the process marks a member for
    /// neither of them.
    #[must_use]
    pub fn performs_health_checks(&self) -> bool {
        !self.is_process_suspended(PROCESS_HEALTH_CHECK)
    }

    /// Whether this group ACTS on such a verdict: the process that replaces a member a health
    /// check found unhealthy is running ([`PROCESS_REPLACE_UNHEALTHY`]).
    ///
    /// THE ONE reading of that question, whichever check reached the verdict: while the
    /// process is suspended the group reports the member unhealthy and neither terminates it
    /// nor launches anything for it.
    #[must_use]
    pub fn replaces_unhealthy_instances(&self) -> bool {
        !self.is_process_suspended(PROCESS_REPLACE_UNHEALTHY)
    }

    /// Whether this group REGISTERS an instance it brings into service with the load
    /// balancers and target groups it is attached to: the process that performs the
    /// registration is running ([`PROCESS_ADD_TO_LOAD_BALANCER`]).
    ///
    /// THE ONE reading of that question, for every instance the group itself brings into
    /// service — one it launched for its capacity and one it took out of its warm pool — so
    /// an instance that came into service while the process was suspended carries no
    /// registration for the rest of its life in the group
    /// ([`super::StoredInstance::load_balancer_registration_withheld`]).
    #[must_use]
    pub fn registers_with_load_balancers(&self) -> bool {
        !self.is_process_suspended(PROCESS_ADD_TO_LOAD_BALANCER)
    }

    /// This group's suspensions as the API reports them — one `SuspendedProcess` each.
    ///
    /// THE ONE projection of a stored suspension onto the wire shape, so every read of the
    /// group reports a suspension identically.
    #[must_use]
    pub fn suspended_process_views(&self) -> Vec<SuspendedProcess> {
        self.suspended_processes
            .iter()
            .map(|(process_name, suspended_at)| SuspendedProcess {
                process_name: Some(process_name.clone()),
                suspension_reason: Some(suspension_reason(*suspended_at)),
            })
            .collect()
    }
}

/// The processes a request names: the ones it listed, or the whole catalog when it listed
/// none.
///
/// A name outside the catalog is refused here, so neither a suspension nor a resumption
/// can write a process this service does not have.
pub fn named_processes(requested: &[String]) -> Result<Vec<String>, ProcessRefusal> {
    if requested.is_empty() {
        return Ok(super::SCALING_PROCESS_TYPES
            .iter()
            .map(|process| (*process).to_string())
            .collect());
    }
    let unknown: Vec<String> = requested
        .iter()
        .filter(|candidate| !super::SCALING_PROCESS_TYPES.contains(&candidate.as_str()))
        .cloned()
        .collect();
    if !unknown.is_empty() {
        return Err(ProcessRefusal::UnknownProcesses(unknown));
    }
    Ok(requested.to_vec())
}

/// The reason a group reports a process as suspended for: that a caller suspended it, and
/// the instant they did.
///
/// One variable part — the instant, in RFC 3339 UTC to the second, which is the form this
/// member carries it in; the rest is fixed text.
pub fn suspension_reason(suspended_at: SystemTime) -> String {
    format!("User suspended at {}", super::whole_seconds_text(suspended_at))
}

/// The cause the capacity change a RESUMPTION makes records: when it was asked for, that
/// the group is doing again what the resumed process does, and the capacity it brings its
/// instances to.
///
/// One variable part beside the instant — the capacity the group holds, which the changes
/// made while the process was suspended recorded; the rest is fixed text.
pub fn resumed_convergence_cause(at: SystemTime, desired_capacity: i32) -> String {
    super::AutoScalingState::reason_cause(
        at,
        &format!(
            "a user request resumed the scaling processes of the group, bringing its instances \
             to the desired capacity of {desired_capacity}"
        ),
    )
}

/// The message a request naming processes outside this service's own catalog is refused
/// with.
///
/// The fixed text names what the request contained and then the catalog the caller has to
/// choose from, and closes the sentence with its full stop; the one variable part is the
/// names the request wrote that the catalog does not hold, EVERY one of them, since one
/// call may name several and none of them is suspended.
pub fn unknown_process_message(processes: &[String]) -> String {
    format!(
        "Request contained illegal suspendable process names: [{}].Valid names are :[{}].",
        processes.join(", "),
        super::SCALING_PROCESS_TYPES.join(", ")
    )
}
