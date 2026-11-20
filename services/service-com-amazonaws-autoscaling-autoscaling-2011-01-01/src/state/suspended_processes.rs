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

use std::collections::BTreeMap;
use std::time::SystemTime;

use crate::ops::types::SuspendedProcess;

use super::StoredGroup;

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
    format!("User suspended at {}", whole_seconds_text(suspended_at))
}

/// The cause the capacity change a RESUMPTION makes records: that the group is doing again
/// what the resumed process does, and the capacity it brings its instances to.
///
/// One variable part — the capacity the group holds, which the changes made while the
/// process was suspended recorded; the rest is fixed text.
pub fn resumed_convergence_cause(desired_capacity: i32) -> String {
    format!(
        "a user request resumed the scaling processes of the group: bringing its instances to \
         the desired capacity of {desired_capacity}."
    )
}

/// `at` as RFC 3339 UTC text truncated to the second, which is the precision a suspension
/// reason states an instant to.
fn whole_seconds_text(at: SystemTime) -> String {
    let seconds = at
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    ::aws_smithy_types::DateTime::from_secs(i64::try_from(seconds).unwrap_or(i64::MAX))
        .fmt(::aws_smithy_types::date_time::Format::DateTime)
        .unwrap_or_default()
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
