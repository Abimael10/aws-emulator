// SPDX-License-Identifier: CC0-1.0

//! The DRAIN a `DeleteService` starts: the tasks it stops, and the account the scheduler gives
//! of stopping them.
//!
//! `DeleteService`: "the service status moves from `ACTIVE` to `DRAINING` ... After all tasks
//! have transitioned to either `STOPPING` or `STOPPED` status, the service status moves from
//! `DRAINING` to `INACTIVE`." The tasks the delete finds RUNNING are tasks the scheduler
//! stops, which is work it accounts for on the service's own event stream exactly as it does
//! for the tasks it retires from a task set
//! ([`super::service_scheduler_task_reports::tasks_stopped_message`]) — so the entries a
//! client reads while the service drains name the tasks that went with it.

use super::*;

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// The RUNNING tasks among `task_arns` that the drain of `key` is about to stop, as the
    /// entries that account for them
    /// ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::record_service_task_reports`]).
    ///
    /// Collected BEFORE the stops are made, since what the entry states is that the tasks were
    /// running: a task already on its way out was stopped by whatever put it there, and the
    /// drain is not the account of that.
    pub(super) fn draining_task_stops(
        &self,
        key: &(String, String),
        task_arns: &[String],
        now: SystemTime,
    ) -> Vec<ServiceTaskReport> {
        task_arns
            .iter()
            .filter_map(|arn| self.tasks.get(arn))
            .filter(|task| task.last_status == STATUS_RUNNING)
            .map(|task| ServiceTaskReport {
                service: key.clone(),
                task_id: task.task_id.clone(),
                at: now,
            })
            .collect()
    }
}
