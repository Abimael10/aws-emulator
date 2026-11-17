// SPDX-License-Identifier: CC0-1.0

//! The task a service REPLACES because its health check failed: what the ECS service
//! scheduler does with a task whose essential container reports `UNHEALTHY`, and what the
//! rollout that placed it makes of that stop.
//!
//! # The rule this module implements
//!
//! A service "runs and maintains your desired number of tasks" and "replaces tasks that fail
//! or stop" (`CreateService`), and a task that FAILED ITS HEALTH CHECK is one of those: the
//! scheduler acts on the container health check a task definition declares, which is exactly
//! what `CreateService$healthCheckGracePeriodSeconds` is a period OF — "the period of time,
//! in seconds, that the Amazon ECS service scheduler ignores unhealthy Elastic Load
//! Balancing, VPC Lattice, and container health checks after a task has first started" —
//! since a scheduler that never acted on such a check would have nothing to ignore. The
//! `minimumHealthyPercent` doc describes the same reaction from the other side: "if any tasks
//! are unhealthy ... the scheduler stops the unhealthy tasks one-by-one ... to clear up
//! capacity to launch replacement tasks".
//!
//! So, for a task of an ACTIVE service whose own scheduler places its tasks:
//!
//! * an ESSENTIAL container reporting `UNHEALTHY` ([`super::container_health`]) is what makes
//!   the task one the scheduler stops — the task-level reading of the same results
//!   (`Task$healthStatus`, "determined by the health of the essential containers in the
//!   task"), so a failing check on a container the task can live without stops nothing;
//! * the stop is held off until the service's health-check GRACE PERIOD has run out from the
//!   instant the task started, because that period is one the scheduler "ignores unhealthy
//!   ... container health checks" for;
//! * the stop is the scheduler's own ([`STOP_CODE_SERVICE_SCHEDULER`], "the service scheduler
//!   stopped the task") and carries the sentence Amazon ECS words that stop in
//!   ([`HEALTH_CHECK_FAILED_REASON`]), beside the event the scheduler writes on the service's
//!   stream;
//! * the task is then REPLACED like any other task the service lost, by the scheduler's
//!   steady-state pass ([`super::service_steady_state`]) — "stops the unhealthy tasks
//!   one-by-one ... to clear up capacity to launch replacement tasks", which is why at most
//!   one task of one service is stopped per pass;
//! * and the ROLLOUT that placed it counts the stop as one of its failed tasks
//!   ([`TaskRecord::failed_for_deployment`]): `Deployment$failedTasks` counts a task that
//!   "fails any of its defined health checks and is stopped" beside the ones the scheduler
//!   could not launch, so a revision whose containers never pass their probe is what the
//!   deployment circuit breaker fails the rollout for ([`super::circuit_breaker`]).
//!
//! A task NO service placed is not replaced by anybody — the contrast
//! [`super::service_steady_state`] states — so a standalone `RunTask`/`StartTask` task whose
//! container is unhealthy keeps running and reports that reading, and so does a task of a
//! service whose deployment controller is not the ECS scheduler (its tasks belong to a task
//! set).
//!
//! # And the rollout that is still waiting for a verdict
//!
//! The same results decide when a rollout has reached its steady state: a task that declares
//! a health check is one the scheduler is still waiting on while that check has produced no
//! pass ([`Self::rollout_awaits_container_health`]), which is why the deployment reports a
//! `healthyTaskCount` of its own. Without that wait a rollout whose containers are still
//! being evaluated could be declared COMPLETED in the moment before its first probe failed,
//! and the breaker watching it would have stopped watching.
//!
//! # Why the clock owes it
//!
//! Every transition here falls due at the instant the probe that produced the reading ran at
//! ([`ContainerHealthRecord::unhealthy_at`]) — or at the end of the grace period, for a
//! service that asked for one — whether or not anybody called, so it is materialised by the
//! clock pass and dated there: stored and compared, never slept on.

use super::*;

/// `Task$stoppedReason` of a task the service scheduler stopped because an essential
/// container failed its health check — Amazon ECS's own words for that stop, in the form its
/// stopped-task reasons take (beside "Essential container in task exited",
/// [`ESSENTIAL_CONTAINER_EXITED_REASON`]).
pub const HEALTH_CHECK_FAILED_REASON: &str = "Task failed container health checks";

/// The event the scheduler writes on the service's own stream for that stop, in the wording
/// the Amazon ECS service event messages use for a task that failed those checks.
fn failed_health_checks_event(service_name: &str, task_id: &str) -> String {
    format!("(service {service_name}) (task {task_id}) failed container health checks.")
}

impl ContainerRecord {
    /// When this container's health check was found FAILED, and `None` for a container whose
    /// reading is anything else — including one that declares no check at all, which reports
    /// `UNKNOWN` for ever and so fails nothing.
    fn failed_health_check_at(&self) -> Option<SystemTime> {
        self.health
            .as_ref()
            .filter(|health| health.status == HEALTH_UNHEALTHY)
            .and_then(|health| health.unhealthy_at)
    }

    /// Whether this container declares a health check that has NOT passed yet: a reading of
    /// anything but `HEALTHY` on a container that HAS a check, which is a container the
    /// scheduler is still waiting on. A container with no check declares nothing to wait for.
    fn awaits_health_check(&self) -> bool {
        self.health
            .as_ref()
            .is_some_and(|health| health.status != HEALTH_HEALTHY)
    }
}

impl TaskRecord {
    /// When an ESSENTIAL container of this task was found to have failed its health check —
    /// the EARLIEST such reading, since the task has been one the scheduler replaces since
    /// then — and `None` for a task whose essential containers report anything else.
    ///
    /// Essential containers only: `Task$healthStatus` is "determined by the health of the
    /// essential containers in the task", so a failing check on a container the task can live
    /// without is not a task the scheduler stops.
    pub fn failed_health_check_at(&self) -> Option<SystemTime> {
        self.containers
            .iter()
            .filter(|container| container.essential)
            .filter_map(ContainerRecord::failed_health_check_at)
            .min()
    }

    /// Whether an ESSENTIAL container of this task declares a health check that has not
    /// passed yet — the task a rollout is still waiting on a verdict for.
    pub fn awaits_container_health(&self) -> bool {
        self.containers
            .iter()
            .filter(|container| container.essential)
            .any(ContainerRecord::awaits_health_check)
    }

    /// Whether the stop this task carries is the one a FAILED HEALTH CHECK earns.
    ///
    /// The scheduler makes this stop and the stops a rollout and a scale-in ask for, so the
    /// stop code they share cannot tell them apart; what does is the sentence Amazon ECS
    /// words this one in ([`HEALTH_CHECK_FAILED_REASON`]).
    pub fn stopped_for_failed_health_check(&self) -> bool {
        self.stop_code.as_deref() == Some(STOP_CODE_SERVICE_SCHEDULER)
            && self.stopped_reason.as_deref() == Some(HEALTH_CHECK_FAILED_REASON)
    }
}

/// One stop this pass owes: which service's stream the event goes on, which task is stopped,
/// the id that event names it by, and the instant it all falls due at.
struct UnhealthyTaskStop {
    /// `(clusterName, serviceName)` — the service whose scheduler makes the stop.
    service: (String, String),
    /// The task it stops.
    task_arn: String,
    /// `Task$taskArn`'s id segment, which the scheduler's event names the task by.
    task_id: String,
    /// The instant the stop fell due at: the reading's own instant, or the end of the grace
    /// period the service asked for, whichever is later.
    due: SystemTime,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Stop the tasks whose essential containers have failed their health checks, as of
    /// `now` — at most one per service, as "the scheduler stops the unhealthy tasks
    /// one-by-one" describes, with the replacement left to the steady-state pass that keeps
    /// every service at its desired count.
    ///
    /// Each stop was owed at the instant the reading was produced whether or not anybody
    /// called, so a reporting operation may run this — and each happens at most once, because
    /// the task it stops leaves the state that owed it.
    pub(super) fn stop_unhealthy_service_tasks(&mut self, now: SystemTime) {
        for stop in self.unhealthy_service_task_stops(Some(now)) {
            self.stop_service_task(&stop.task_arn, stop.due, HEALTH_CHECK_FAILED_REASON);
            let event = self.mint_service_event(
                stop.due,
                failed_health_checks_event(&stop.service.1, &stop.task_id),
            );
            if let Some(service) = self.services.get_mut(&stop.service) {
                service.record_events([event]);
            }
        }
    }

    /// The earliest instant at which this store owes one of those stops, if any — one of the
    /// instants the clock pump waits for, so a task the probes have condemned is replaced
    /// without waiting for the next caller.
    ///
    /// Read off the same candidates the pass above acts on, so the pump is never woken by an
    /// instant no pass will clear.
    pub(super) fn next_unhealthy_task_stop_due_at(&self) -> Option<SystemTime> {
        self.unhealthy_service_task_stops(None)
            .into_iter()
            .map(|stop| stop.due)
            .min()
    }

    /// The stops above, at most one per service: the task of each service whose check failed
    /// FIRST, of those whose grace period has run out.
    ///
    /// `as_of` holds the answer to the stops that have fallen DUE by an instant; `None`
    /// answers every stop this store owes whenever it falls due, which is what the clock
    /// pump waits for.
    fn unhealthy_service_task_stops(&self, as_of: Option<SystemTime>) -> Vec<UnhealthyTaskStop> {
        let mut stops = Vec::new();
        for service in self.services.values() {
            // A service that is being deleted is on its way out rather than maintaining a
            // task count, and one whose deployment controller is not the ECS scheduler has
            // its tasks placed by a task set instead.
            if !service.is_active() || !service.places_tasks() {
                continue;
            }
            // "The period of time ... that the Amazon ECS service scheduler ignores unhealthy
            // ... container health checks after a task has first started": the stop is held
            // off until that period has run out from the instant the task started.
            let grace = service
                .health_check_grace_period_seconds
                .filter(|seconds| *seconds > 0)
                .and_then(|seconds| u64::try_from(seconds).ok())
                .map(Duration::from_secs);
            let mut candidates: Vec<(&TaskRecord, SystemTime)> = self
                .tasks
                .values()
                .filter(|task| {
                    task.cluster_name == service.cluster_name
                        && task.service_name.as_deref() == Some(service.name.as_str())
                        && task.desired_status == STATUS_RUNNING
                        && task.last_status == STATUS_RUNNING
                })
                .filter_map(|task| {
                    let failed_at = task.failed_health_check_at()?;
                    let due = match grace {
                        // The period runs from "after a task has FIRST STARTED", so a task
                        // whose start instant this store cannot read is one whose period
                        // cannot be shown to have run out: it is left alone rather than
                        // stopped inside the span the service asked be ignored.
                        Some(grace) => {
                            let started = task
                                .started_at
                                .and_then(|at| SystemTime::try_from(at).ok())?;
                            (started + grace).max(failed_at)
                        }
                        None => failed_at,
                    };
                    Some((task, due))
                })
                .filter(|(_, due)| as_of.is_none_or(|as_of| *due <= as_of))
                .collect();
            // The one that fell due FIRST is the one this pass stops — the task whose check
            // failed earliest, which is the order "one-by-one" takes them in. Its ARN breaks
            // a tie, so one pass over one store always chooses the same task.
            candidates.sort_by(|left, right| {
                (left.1, &left.0.task_arn).cmp(&(right.1, &right.0.task_arn))
            });
            if let Some((task, due)) = candidates.first() {
                stops.push(UnhealthyTaskStop {
                    service: (service.cluster_name.clone(), service.name.clone()),
                    task_arn: task.task_arn.clone(),
                    task_id: task.task_id.clone(),
                    due: *due,
                });
            }
        }
        stops
    }

    /// Whether `service`'s CURRENT rollout is still waiting on a container health check: a
    /// task it placed that is still wanted running and whose essential container declares a
    /// check which has produced no pass yet.
    ///
    /// What holds a rollout's steady state off until the checks have spoken. A task being
    /// evaluated has not shown that the revision runs, and one that failed is about to be
    /// stopped and replaced ([`Self::stop_unhealthy_service_tasks`]), so a rollout settled in
    /// between would be a rollout declared complete on no verdict at all — and, with the
    /// steady state terminal, the circuit breaker watching it would have stopped watching.
    pub(super) fn rollout_awaits_container_health(&self, service: &ServiceRecord) -> bool {
        self.tasks.values().any(|task| {
            task.cluster_name == service.cluster_name
                && task.service_name.as_deref() == Some(service.name.as_str())
                && task.started_by.as_deref() == Some(service.deployment_id.as_str())
                && task.desired_status == STATUS_RUNNING
                && task.awaits_container_health()
        })
    }
}
