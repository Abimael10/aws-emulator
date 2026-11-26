// SPDX-License-Identifier: CC0-1.0

//! What the SERVICE SCHEDULER reports about the tasks it starts and the tasks it takes out of
//! a service's task set: the entry it writes on the service's own event stream for the tasks
//! it has started, and the `stoppedReason` of a task it retired.
//!
//! # The rules this module implements
//!
//! `Service$events` is "The event stream for your service", and what the scheduler puts there
//! is its own account of the work it did. Placing and retiring tasks is such work: when the
//! scheduler has started tasks of a service, that service's stream names them — `(service
//! <name>) has started <n> tasks: (task <id>).` — and when it has taken running tasks out of
//! the set it names those — `(service <name>) has stopped <n> running tasks: (task <id>).` —
//! beside the deployment and steady-state entries the rollout earns, so a client reading the
//! stream can attribute each launch and each retirement to the service that asked for it. The
//! start entry is written where the tasks really START (the clock pass that moves them into
//! `RUNNING`), so a service whose scheduler has placed nothing yet reports nothing and a task
//! that never comes up is never announced as started; the stop entry is written where the
//! scheduler RETIRES them, which is the action it is an account of.
//!
//! `Task$stoppedReason` is the other half: a task the scheduler retires carries
//! [`STOP_CODE_SERVICE_SCHEDULER`] and ONE sentence — `Scaling activity initiated by
//! (deployment <id>)` — naming the deployment that STARTED the task (`Task$startedBy`, "for
//! a task the service scheduler placed, the id of the deployment that placed it"). So the
//! sentence is the same whether the task is retired because the service wants fewer tasks or
//! because a newer deployment is replacing it, and on a rolling update the retired task names
//! its OWN deployment rather than the one that replaced it.
//!
//! Both are RECORDED behaviour: `ServiceEvent` models only `id`/`createdAt`/`message` and
//! `Task$stoppedReason` is a plain string, so the model fixes neither sentence. The recorded
//! entries each account for ONE task — `has started 1 tasks: (task <id>)`, `has stopped 1
//! running tasks: (task <id>)` — so that is what is written: one entry per task the pass acted
//! on, in the order the tasks moved. The templates still carry the COUNT and a space-joined
//! list of clauses, because that is the shape of the sentence ([`ServiceTaskReport`] is what
//! one entry accounts for), so an entry accounting for several tasks reads correctly if a
//! recording ever pins one.

use super::*;

/// `ServiceEvent$message` for the tasks of `service_name` the scheduler has started, naming
/// each of them.
///
/// The template Amazon ECS publishes: the service, how many tasks the entry accounts for, and
/// one `(task <id>)` clause per task, in the order they are given. The ids are the task ids
/// (`Task$taskArn`'s last segment), which is how every entry of this stream names a task.
pub fn tasks_started_message(service_name: &str, task_ids: &[&str]) -> String {
    format!(
        "(service {service_name}) has started {} tasks: {}.",
        task_ids.len(),
        named_tasks(task_ids)
    )
}

/// `ServiceEvent$message` for the RUNNING tasks of `service_name` the scheduler has taken out
/// of the set, naming each of them.
///
/// The same template as [`tasks_started_message`] for the other half of the scheduler's work:
/// the tasks it retired were running, which is what the entry calls them.
pub fn tasks_stopped_message(service_name: &str, task_ids: &[&str]) -> String {
    format!(
        "(service {service_name}) has stopped {} running tasks: {}.",
        task_ids.len(),
        named_tasks(task_ids)
    )
}

/// The `(task <id>)` clauses an entry names its tasks by, in the order given.
fn named_tasks(task_ids: &[&str]) -> String {
    task_ids
        .iter()
        .map(|task_id| format!("(task {task_id})"))
        .collect::<Vec<String>>()
        .join(" ")
}

/// ONE entry of the scheduler's account of a task it acted on: the service whose stream it
/// belongs to, the task it names, and the instant the task started or was stopped.
///
/// The instant is the TASK's, not the pass's, so entries about several tasks are written in
/// the order the tasks moved however the pass found them.
#[derive(Debug, Clone)]
pub struct ServiceTaskReport {
    /// The service's key in the store: its cluster and its name.
    pub service: (String, String),
    /// `Task$taskArn`'s last segment, which is how an entry of this stream names a task.
    pub task_id: String,
    /// When the task started, or when it was stopped.
    pub at: SystemTime,
}

/// `Task$stoppedReason` for a task the service scheduler took out of the set, naming the
/// deployment that started it.
///
/// ONE template for every retirement the scheduler makes — the deployment clause in
/// PARENTHESES, as this API spells a resource clause everywhere else it names one — so a
/// client that parses the sentence to attribute a stop reads the same shape whether the
/// service was scaled in or rolled onto another revision.
pub fn scaling_activity_reason(deployment_id: &str) -> String {
    format!("Scaling activity initiated by (deployment {deployment_id})")
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Ask at most `wanted` of the service tasks `candidates` offers to stop as tasks the
    /// SCHEDULER has retired, each with its own deployment named in its `stoppedReason`.
    ///
    /// The selection is [`Self::stop_unprotected_service_tasks`]': offered in the order given,
    /// every task whose caller has protected it passed over. What this adds is the REASON,
    /// which is per task rather than per pass ([`scaling_activity_reason`]): the deployment a
    /// task names is the one that started it (`Task$startedBy`), so a rolling update's
    /// retirement names the deployment being replaced and not the replacement.
    ///
    /// `deployment_id` is the service's current deployment, used only for a task that carries
    /// no `startedBy` at all — every task this scheduler places carries one.
    ///
    /// A task that was RUNNING when it was picked is one the scheduler has STOPPED, so the
    /// service's event stream carries its account of that too
    /// ([`tasks_stopped_message`]) — one entry per task, written here, where the retirement
    /// is made.
    pub(super) fn retire_unprotected_service_tasks<'a>(
        &mut self,
        candidates: impl IntoIterator<Item = &'a str>,
        wanted: usize,
        deployment_id: &str,
        now: SystemTime,
    ) {
        let mut left = wanted;
        let mut retired: Vec<ServiceTaskReport> = Vec::new();
        for task_arn in candidates {
            if left == 0 {
                break;
            }
            let Some(task) = self
                .tasks
                .get(task_arn)
                .filter(|task| !task.is_protected())
            else {
                continue;
            };
            let started_by = task.started_by.clone();
            let was_running = task.last_status == STATUS_RUNNING;
            let announced = task.service_name.as_deref().filter(|_| was_running).map(|name| {
                ServiceTaskReport {
                    service: (task.cluster_name.clone(), name.to_string()),
                    task_id: task.task_id.clone(),
                    at: now,
                }
            });
            let reason =
                scaling_activity_reason(started_by.as_deref().unwrap_or(deployment_id));
            self.stop_service_task(task_arn, now, &reason);
            retired.extend(announced);
            left -= 1;
        }
        self.record_service_task_reports(retired, tasks_stopped_message);
    }

    /// Append ONE entry per task of `reports` to its service's event stream, oldest first,
    /// worded by `message` — [`tasks_started_message`] for the tasks a pass started and
    /// [`tasks_stopped_message`] for the running tasks it stopped.
    ///
    /// A task whose service this store no longer holds is dropped: the stream belongs to the
    /// service.
    pub(super) fn record_service_task_reports(
        &mut self,
        mut reports: Vec<ServiceTaskReport>,
        message: fn(&str, &[&str]) -> String,
    ) {
        reports.sort_by_key(|report| report.at);
        for report in reports {
            let Some(name) = self
                .services
                .get(&report.service)
                .map(|service| service.name.clone())
            else {
                continue;
            };
            let event = self.mint_service_event(report.at, message(&name, &[&report.task_id]));
            if let Some(service) = self.services.get_mut(&report.service) {
                service.record_events([event]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the start entry names the service, the number of tasks it accounts for and each
    /// task; the stop entry is the same account of the tasks the scheduler retired.
    #[test]
    fn the_start_entry_names_every_task_it_accounts_for() {
        // (case, the task ids the entry accounts for, the sentence it carries)
        let rows: &[(&str, &[&str], &str)] = &[
            (
                "one task",
                &["0a1b2c3d"],
                "(service web) has started 1 tasks: (task 0a1b2c3d).",
            ),
            (
                "two tasks",
                &["0a1b2c3d", "4e5f6a7b"],
                "(service web) has started 2 tasks: (task 0a1b2c3d) (task 4e5f6a7b).",
            ),
        ];
        for (case, task_ids, message) in rows {
            let written = tasks_started_message("web", task_ids);
            assert_eq!(
                written, *message,
                "case {case:?}: expected {message:?}, got {written:?}"
            );
        }
        let stopped = tasks_stopped_message("web", &["0a1b2c3d"]);
        assert_eq!(
            stopped, "(service web) has stopped 1 running tasks: (task 0a1b2c3d).",
            "case \"one retired task\": expected the scheduler's stop entry, got {stopped:?}"
        );
    }

    /// RULE: the retirement sentence names the deployment in parentheses, whichever
    /// deployment it is.
    #[test]
    fn the_retirement_sentence_names_its_deployment_in_parentheses() {
        for deployment_id in ["ecs-svc/0000000000000000001", "ecs-svc/9999999999999999999"] {
            let written = scaling_activity_reason(deployment_id);
            assert_eq!(
                written,
                format!("Scaling activity initiated by (deployment {deployment_id})"),
                "case {deployment_id:?}: expected the deployment clause in parentheses, got \
                 {written:?}"
            );
        }
    }
}
