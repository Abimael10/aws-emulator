// SPDX-License-Identifier: CC0-1.0

//! The ROLLBACK of a service deployment: what putting the previous service revision back
//! does to the rollout being undone, and the end the clock gives it.
//!
//! A rollback is asked for in three ways — a `StopServiceDeployment` with the `ROLLBACK`
//! stop type, the `ROLLBACK` action on a PAUSE lifecycle hook, and the monitor that failed
//! the rollout by itself ([`super::super::circuit_breaker`]) — and all three mean the same
//! thing: "Rolls back the deployment to the previous service revision."
//!
//! That rollback happens WITHIN the service deployment it undoes. `ServiceDeployment$rollback`
//! is "The rollback options the service deployment uses when the deployment fails" and
//! `Rollback$serviceRevisionArn` is "The ARN of the service revision deployed as part of the
//! rollback", so the revision put back is reported as part of the rolled-back deployment:
//! that deployment keeps its own identity, moves to
//! [`SERVICE_DEPLOYMENT_ROLLBACK_IN_PROGRESS`] and then to
//! [`SERVICE_DEPLOYMENT_ROLLBACK_SUCCESSFUL`], and carries the reason, the instant and the
//! revision of the rollback. No SECOND service deployment is recorded for the redeployment
//! that puts the revision back — a rollback is one rollout's ending, not another rollout's
//! release, so a listing of a service's deployments reports exactly the releases it had.
//!
//! The service itself still redeploys: the scheduler replaces the task set the undone
//! rollout placed with one of the revision going back in, under a `Deployment` of its own
//! ([`ServiceRecord::deployment_id`]), and the service deployment that rollback belongs to is
//! the one being rolled back ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::current_service_deployment`]).

use super::*;

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Materialise the end of every rollout a ROLLBACK undid, as of `now`: the previous
    /// service revision is the one the service runs again, and the rollback is CARRIED OUT
    /// once that revision is running.
    ///
    /// Putting a revision back means running it again, which is a rollout of its own
    /// ([`Self::roll_back_service_deployment_to`]), so a rollback whose redeployment has not
    /// taken the service over yet is still in progress: a client told
    /// [`SERVICE_DEPLOYMENT_ROLLBACK_SUCCESSFUL`] is told the service is back on the revision
    /// the rollback names, not that it will be. Run after the pass that settles that
    /// redeployment ([`Self::settle_services`]), so the rollback ends in the same pass the
    /// revision it put back came up in.
    pub(crate) fn finish_rolled_back_deployments(&mut self, now: SystemTime) {
        let arns: Vec<String> = self.service_deployments.keys().cloned().collect();
        for arn in arns {
            let rolled_back = self
                .service_deployments
                .get(&arn)
                .and_then(|record| record.rollback_due_at)
                .filter(|due| *due <= now);
            // The REDEPLOYMENT this rollback started, as the service stands now: the
            // rollback is carried out once that rollout has taken the service over, and a
            // rollback whose redeployment the service has itself left behind — somebody
            // released again — is over too, because the rollout that was putting the
            // revision back is not the one being made any more.
            let restored = self
                .service_deployments
                .get(&arn)
                .and_then(|record| {
                    let service = self
                        .services
                        .get(&(record.cluster_name.clone(), record.service_name.clone()))?;
                    let redeployment = record.rollback_deployment_id.as_deref()?;
                    Some(
                        service.deployment_id != redeployment
                            || service.deployment_rollout_completed,
                    )
                })
                .unwrap_or_default();
            if let Some(due) = rolled_back.filter(|_| restored) {
                if let Some(record) = self.service_deployments.get_mut(&arn) {
                    record.rollback_due_at = None;
                    if !record.is_finished() {
                        record.status = SERVICE_DEPLOYMENT_ROLLBACK_SUCCESSFUL.to_string();
                        record.finished_at = Some(reported_instant(due));
                    }
                }
            }
        }
    }

    /// Take `action` on the PAUSE lifecycle hook `hook_id` of the rollout `arn` records, as
    /// `ContinueServiceDeployment` asks: "Use this API to either continue the deployment to
    /// the next stage or roll back to the previous service revision."
    ///
    /// The action is taken on the hook that rollout is WAITING at
    /// (`ContinueServiceDeployment$hookId`: "The ID of the PAUSED lifecycle hook to act
    /// on"). A hookId naming a hook of that rollout that already took THIS action asks the
    /// rollout to leave a stage it has already left: that call has had its effect, so it is
    /// answered and changes nothing. Every other request is refused and changes nothing: a
    /// rollout that stands at no hook, a hook id that rollout holds none of, a hook whose
    /// wait was taken away or ran out, a hook that took the OTHER action — which this one
    /// cannot undo — and a rollback the service has no earlier revision for.
    pub fn act_on_lifecycle_hook(
        &mut self,
        arn: &str,
        hook_id: &str,
        action: &str,
        now: SystemTime,
    ) -> Result<(), LifecycleHookRefusal> {
        let record = self
            .service_deployments
            .get(arn)
            .ok_or(LifecycleHookRefusal::NoSuchDeployment)?;
        if record
            .awaiting_hook()
            .is_none_or(|hook| hook.hook_id != hook_id)
        {
            return if record.lifecycle_hooks.iter().any(|hook| {
                hook.hook_id == hook_id && hook.taken_action.as_deref() == Some(action)
            }) {
                Ok(())
            } else {
                Err(LifecycleHookRefusal::NotAwaitingAction)
            };
        }
        // The action is taken BEFORE the hook is marked, so a rollback that cannot be
        // carried out leaves the hook waiting for one that can.
        self.resume_service_deployment(arn, action, now, &hook_rollback_reason())?;
        if let Some(record) = self.service_deployments.get_mut(arn) {
            if let Some(hook) = record
                .lifecycle_hooks
                .iter_mut()
                .find(|hook| hook.hook_id == hook_id)
            {
                hook.status = LIFECYCLE_HOOK_SUCCEEDED.to_string();
                // The action this hook took, so a repeat of it is answered as the call that
                // has already had its effect and a different one is refused.
                hook.taken_action = Some(action.to_string());
            }
        }
        Ok(())
    }

    /// Resume the rollout `arn` records with `action`, the hook action a caller took or a
    /// hook's own timeout fell to.
    ///
    /// [`LIFECYCLE_HOOK_ACTION_CONTINUE`] "proceeds the deployment to the next lifecycle
    /// stage" — the next stage it pauses at, or, when it has none left, its own end. Both
    /// are spans the CLOCK settles: the rollout travels on from where it stood.
    pub(crate) fn resume_service_deployment(
        &mut self,
        arn: &str,
        action: &str,
        at: SystemTime,
        rollback_reason: &str,
    ) -> Result<(), LifecycleHookRefusal> {
        if action == LIFECYCLE_HOOK_ACTION_ROLLBACK {
            return self.roll_back_service_deployment(arn, at, rollback_reason);
        }
        let record = self
            .service_deployments
            .get_mut(arn)
            .ok_or(LifecycleHookRefusal::NoSuchDeployment)?;
        if record.pending_hook_stops.is_empty() {
            record.complete_due_at = Some(at + TRANSITION_SPAN);
        } else {
            record.hook_due_at = Some(at + TRANSITION_SPAN);
        }
        Ok(())
    }

    /// Roll the rollout `arn` records back: "ROLLBACK - Rolls back the deployment to the
    /// previous service revision."
    ///
    /// The previous revision is the one the rollout just before this one released
    /// (`ServiceDeployment$sourceServiceRevisions`: "the currently deployed workload
    /// configuration"), so a service's FIRST rollout has none and the rollback is refused.
    /// What putting it back means is [`Self::roll_back_service_deployment_to`]'s, which the
    /// deployment circuit breaker reaches with the rollout IT chose to put back.
    pub(crate) fn roll_back_service_deployment(
        &mut self,
        arn: &str,
        at: SystemTime,
        rollback_reason: &str,
    ) -> Result<(), LifecycleHookRefusal> {
        let record = self
            .service_deployments
            .get(arn)
            .ok_or(LifecycleHookRefusal::NoSuchDeployment)?
            .clone();
        let source = self
            .previous_service_deployment(&record)
            .cloned()
            .ok_or(LifecycleHookRefusal::NoPreviousRevision)?;
        self.roll_back_service_deployment_to(&record, &source, at, rollback_reason)
    }

    /// Roll `record` back to the revision `source` released — the rollback itself, whichever
    /// rollout a caller or the deployment circuit breaker chose to put back.
    ///
    /// Putting that revision back means running it again: the scheduler replaces the task
    /// set this rollout placed with one from `source`'s revision, under a `Deployment` of the
    /// service's own ([`Self::redeploy_rolled_back_revision`]). That redeployment is part of
    /// THIS rollback rather than a release of its own, so no second service deployment is
    /// recorded for it: the rollout being undone keeps its own identity and ends in
    /// [`SERVICE_DEPLOYMENT_ROLLBACK_SUCCESSFUL`], carrying the `rollback` that names the
    /// revision which was deployed in its place and the reason it was.
    pub(crate) fn roll_back_service_deployment_to(
        &mut self,
        record: &ServiceDeploymentRecord,
        source: &ServiceDeploymentRecord,
        at: SystemTime,
        rollback_reason: &str,
    ) -> Result<(), LifecycleHookRefusal> {
        let arn = record.arn.as_str();
        let definition = source
            .task_definition
            .as_ref()
            .and_then(|key| self.task_definitions.get(key))
            .cloned()
            .ok_or(LifecycleHookRefusal::NoPreviousRevision)?;
        let key = (record.cluster_name.clone(), record.service_name.clone());
        let timestamp = reported_instant(at);
        let deployment_id = self.next_deployment_id();
        let redeployment_id = deployment_id.clone();
        // The service a rollout belongs to outlives it, but a rollout of a service that is
        // gone has nothing to put a revision back onto.
        let Some(service) = self.services.get_mut(&key) else {
            return Err(LifecycleHookRefusal::NoPreviousRevision);
        };
        // The rollout being undone is the one whose task set is now being replaced, exactly
        // as a forced redeployment replaces the one it supersedes.
        service.supersede_deployment(at);
        service.deployment_id = deployment_id;
        service.deployment_created_at = timestamp;
        service.deployment_updated_at = timestamp;
        service.deployment_rollout_completed = false;
        service.deployment_steady_due_at = Some(at + TRANSITION_SPAN);
        service.deployment_stable_due_at = Some(at + TRANSITION_SPAN);
        // `Service$version` does NOT move with a rollout: the member carries its modelled
        // default, which the recorded services report after every rollout they take, this
        // one included.
        service.task_definition = source.task_definition.clone();
        service.task_definition_arn = Some(definition.arn.clone());
        // A service revision IS a workload configuration, so running the previous one again
        // runs that same revision rather than minting a new one — which is the revision
        // `Rollback$serviceRevisionArn` names: "The ARN of the service revision deployed as
        // part of the rollback."
        service.service_revision_arn = source.target_service_revision_arn.clone();
        // The scheduler brings the task set back in line with the revision the service runs
        // again. A placement this emulator cannot hold leaves the task set alone; the
        // rollback itself is recorded either way, as the deployment it undoes is over.
        let _ = self.scale_service(&key, &definition, &record.region, at);
        self.redeploy_rolled_back_revision(&key, timestamp, at);
        // Written AFTER that redeployment: this rollout did not merely stop, it was rolled
        // back, and its rollback is still under way until the clock settles it.
        if let Some(record) = self.service_deployments.get_mut(arn) {
            record.status = SERVICE_DEPLOYMENT_ROLLBACK_IN_PROGRESS.to_string();
            record.status_reason = format!(
                "ECS deployment {} was rolled back to service revision {}.",
                record.deployment_id, source.target_service_revision_arn
            );
            record.finished_at = None;
            record.complete_due_at = None;
            record.hook_due_at = None;
            record.pending_hook_stops.clear();
            record.rollback_due_at = Some(at + TRANSITION_SPAN);
            // WHICH rollout the rollback is waiting on: the redeployment that puts the
            // revision back, so a later release of the service settling is not read as this
            // rollback having been carried out
            // ([`Self::finish_rolled_back_deployments`]).
            record.rollback_deployment_id = Some(redeployment_id);
            record.rollback = Some(Rollback {
                // "The reason the rollback happened. For example, the circuit breaker
                // initiated the rollback operation" — whichever of them this rollback is,
                // as the caller of this rollback states it.
                reason: Some(rollback_reason.to_string()),
                started_at: Some(timestamp),
                service_revision_arn: Some(source.target_service_revision_arn.clone()),
            });
        }
        Ok(())
    }

    /// Run the revision a rollback puts back as a `Deployment` of `key`'s service, at `at`:
    /// the service's own rolling update of the revision going back in.
    ///
    /// The deployment state machine of that rollout begins here, exactly as it does for a
    /// release somebody asked for ([`Self::start_service_deployment`]), so a client watching
    /// the bus sees the service start deploying again
    /// ([`super::super::deployment_events`]). What it does NOT do is record a service
    /// deployment: the redeployment is how the rollback puts the revision back, and the
    /// service deployment it belongs to is the one being rolled back — which is also why it
    /// waits at no lifecycle hook, since a rollback that paused again would leave the service
    /// short of the revision it is going back to.
    ///
    /// The revision itself needs no capture: it was captured by the release that first rolled
    /// it out, and a revision IS a workload configuration, so running it again runs that same
    /// revision ([`Self::record_service_revision`]).
    fn redeploy_rolled_back_revision(
        &mut self,
        key: &(String, String),
        at: DateTime,
        now: SystemTime,
    ) {
        let Some(service) = self.services.get(key) else {
            return;
        };
        // Only the ECS scheduler's own rollout is a rollout of a service revision at all:
        // the other deployment controllers' tasks come from a task set
        // ([`ServiceRecord::places_tasks`]), so there is nothing here to announce.
        if !service.places_tasks() {
            return;
        }
        let service_arn = service.arn.clone();
        let deployment_id = service.deployment_id.clone();
        self.record_deployment_state_change(
            &service_arn,
            &deployment_id,
            ROLLOUT_IN_PROGRESS,
            &rollout_state_reason(&deployment_id, false),
            SystemTime::try_from(at).unwrap_or(now),
        );
    }
}
