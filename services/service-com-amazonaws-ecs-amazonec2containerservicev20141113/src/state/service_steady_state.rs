// SPDX-License-Identifier: CC0-1.0

//! The ECS service scheduler's STEADY STATE: the task a service replaces when one of its
//! tasks stops, and the task nobody replaces.
//!
//! # The rule this module implements
//!
//! A service "runs and maintains your desired number of tasks from a specified task
//! definition" (`CreateService`), and that maintenance is CONTINUOUS: "if any of your tasks
//! fail or stop, the Amazon ECS service scheduler launches another instance of your task
//! definition to replace it in order to maintain the desired number of tasks in the
//! service". So whenever a service holds fewer tasks of its current deployment than its
//! `desiredCount`, the scheduler places the shortfall — however the shortfall arose: a task
//! a caller stopped with `StopTask`, a task that failed, or a task that ended on its own.
//!
//! A task that belongs to NO service has no scheduler maintaining a count, so a stopped
//! `RunTask`/`StartTask` task is never relaunched. That contrast is the whole of this
//! module: it needs no code of its own, because a replacement is only ever placed for a
//! count a SERVICE wants.
//!
//! A DAEMON-scheduled service is the same work over a count nobody states: "The daemon
//! scheduling strategy deploys exactly one task on each active container instance", so its
//! `desiredCount` IS the capacity registered to its cluster and it moves as instances
//! register and leave — which this pass keeps, so an instance that joins gets a task of
//! every daemon service of its cluster without a further call.
//!
//! # Why the clock owes it
//!
//! The reconcile is the work the scheduler was already doing when the task stopped: the
//! desired count is the caller's own standing instruction, so bringing the task set back up
//! to it was owed at the instant the task stopped whether or not anybody called. It is
//! therefore materialised by the clock pass
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::advance_clock`]) — the same
//! pass that materialises a task's own transitions, and the same pass the clock pump drives
//! between calls — and it places nothing a service does not want: a service that is not
//! `ACTIVE`, one whose deployment controller is not the ECS one (its tasks come from a task
//! set), one whose rollout the deployment circuit breaker failed (`Deployment$rolloutState`:
//! "A deployment in `FAILED` state doesn't launch any new tasks") and one that already holds
//! the tasks it wants are all left exactly as they are.
//!
//! The placement itself is the scheduler's ordinary one
//! ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::scale_service`]): a
//! replacement is a task of the service's current deployment, placed under the service's own
//! placement, `PENDING` with its start owed by the clock — so a replacement is
//! indistinguishable from the task it replaces except for being a new task, which is what
//! Amazon ECS does.

use super::*;

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Place the tasks every ACTIVE service is short of, as of `now`.
    ///
    /// The scheduler's steady-state pass: for each service that places its own tasks, the
    /// task set is brought back in line with the `desiredCount` the caller asked for
    /// ([`Self::scale_service`], the same reconcile the call that changes that count runs).
    /// A service whose cluster has nowhere to put the task places nothing and records why
    /// on its event stream, exactly as that call does.
    ///
    /// A placement this emulator has no room for ([`MAX_TASKS`]) is left for later rather
    /// than failing the pass: the clock pass has no caller to answer.
    pub(super) fn maintain_service_task_counts(&mut self, now: SystemTime) {
        // The counts the scheduler is owed TAKING ON, materialised first so this pass works
        // to the count it has just committed to: every change to what a service wants is
        // work for the scheduler, and taking it on was owed at the instant it fell due
        // whether or not anybody called ([`ServiceRecord::scheduled_count`]).
        for service in self.services.values_mut() {
            if service.scheduler_due_at.is_some_and(|due| due <= now) {
                service.scheduler_due_at = None;
                service.scheduled_count = service.desired_count;
            }
        }
        let keys: Vec<(String, String)> = self.services.keys().cloned().collect();
        for key in keys {
            let Some(service) = self.services.get(&key) else {
                continue;
            };
            if !service.is_active() || !service.places_tasks() {
                continue;
            }
            // `Deployment$rolloutState`: "A deployment in FAILED state doesn't launch any new
            // tasks" — so a rollout the deployment circuit breaker failed
            // ([`super::circuit_breaker`]) is the one service the scheduler places nothing
            // for, however short of its desired count it is.
            if service.deployment_rollout_failure.is_some() {
                continue;
            }
            // The revision the service's CURRENT deployment places: a replacement runs what
            // the service is deploying now, not what an earlier deployment ran.
            let definition = service
                .task_definition
                .clone()
                .and_then(|revision| self.task_definitions.get(&revision).cloned());
            let Some(definition) = definition else {
                continue;
            };
            // The Region the service's own ARNs carry, so a replacement's ARN names the
            // Region the service lives in rather than whichever Region last called.
            let region = super::request_arn::RequestArn::parse(&service.cluster_arn)
                .map(|arn| arn.region.to_string())
                .unwrap_or_else(|| DEFAULT_REGION.to_string());
            // "The daemon scheduling strategy deploys exactly one task on each ACTIVE
            // container instance that meets all of the task placement constraints" — so a
            // DAEMON service's desired count is not a number its caller gave: it is the
            // capacity its cluster holds that a placement can be made on at all
            // ([`ContainerInstanceRecord::accepts_task_placement`], the same capacity the
            // MANAGED DAEMON resource places one task on, so the two schedulers count one
            // cluster the same way), and it MOVES as instances register, drain and leave.
            // Keeping it is this pass's work, exactly as keeping the task set at it is: an
            // instance that joined was owed a task of every daemon service of its cluster
            // from the instant it joined, whether or not anybody called.
            if service.scheduling_strategy == SCHEDULING_DAEMON {
                let capacity = i32::try_from(self.placeable_container_instances(&key.0))
                    .unwrap_or(i32::MAX);
                let _ = self.scale_service_to(&key, capacity, &region, now);
            }
            let _ = self.scale_service(&key, &definition, &region, now);
        }
        self.report_placement_shortfalls(now);
    }

    /// Write the scheduler's own account of a placement it could not make onto the event
    /// stream of every service that is short of the tasks it wants.
    ///
    /// The event is what the scheduler says about a pass it has MADE — "(service ...) was
    /// unable to place a task because no container instance met all of its requirements" —
    /// so it belongs to the pass and not to the call that gave the scheduler the work: a
    /// call that scales a service up, or rolls a new deployment, is answered before the
    /// scheduler has tried, and the attempt reported here is the one that follows it.
    ///
    /// The same shortfall the service reports as its `runStatus`
    /// ([`Self::service_placement_shortfall`]), so the sentence and the code cannot
    /// disagree, and a repeat of the same unchanged sentence is not appended twice
    /// ([`Self::record_placement_failure`]).
    pub(super) fn report_placement_shortfalls(&mut self, now: SystemTime) {
        let shortfalls: Vec<((String, String), &'static str)> = self
            .services
            .values()
            .filter(|service| self.service_short_of_its_desired_count(service))
            .filter_map(|service| {
                self.service_placement_shortfall(service).map(|shortfall| {
                    (
                        (service.cluster_name.clone(), service.name.clone()),
                        shortfall.reason,
                    )
                })
            })
            .collect();
        for (key, reason) in shortfalls {
            self.record_placement_failure(&key, reason, now);
        }
    }
}
