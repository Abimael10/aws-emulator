// SPDX-License-Identifier: CC0-1.0

//! The tasks a CONTAINER INSTANCE still holds, and the deregistration of it they refuse.
//!
//! # The rule this module implements
//!
//! `DeregisterContainerInstance` takes a host out of its cluster, and the work already
//! placed on that host is what the call has to be told about: `force` is "forces the
//! container instance to be deregistered. If you have tasks running on the container
//! instance when you deregister it with the `force` option, these tasks remain running until
//! you terminate the instance or the tasks stop through some other means, but they're
//! orphaned (no longer monitored or accounted for by Amazon ECS)". So orphaning a host's
//! tasks is something the CALLER asks for: a deregistration that does not ask for it, of a
//! host that still holds a task, is refused — with the host left exactly as it was, still
//! registered and still counted as the cluster's capacity — and the same call is served once
//! every task on that host has stopped, because then there is nothing left to orphan.
//!
//! Which tasks hold the host is the same question every count of a cluster's work asks: a
//! task that has reached `STOPPED` holds nothing ([`TaskRecord::is_active`]), whatever
//! placed it and whatever its desired status is, so a host whose tasks have all stopped is
//! free even though their records are still readable.

use super::*;

/// `InvalidParameterException$message` for a deregistration of a host that still holds a
/// task, made without forcing it: one fixed sentence, carrying neither the host, nor the
/// cluster, nor how many tasks it holds. RECORDED behaviour.
pub const CONTAINER_INSTANCE_RUNNING_TASKS_MESSAGE: &str = "Found running tasks on the instance.";

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Whether the container instance stored under `arn` still holds a task — one placed on
    /// it that has not reached `STOPPED`.
    ///
    /// A reporting-only question: nothing is written here, so two reads at one instant
    /// answer identically.
    pub fn container_instance_holds_tasks(&self, arn: &str) -> bool {
        self.tasks
            .values()
            .any(|task| task.container_instance_arn.as_deref() == Some(arn) && task.is_active())
    }
}
