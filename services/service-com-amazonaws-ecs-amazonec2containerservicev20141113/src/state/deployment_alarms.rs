// SPDX-License-Identifier: CC0-1.0

//! The CloudWatch ALARMS a rollout is watched by: the alarms its deployment configuration
//! named, the ones that MOVED into their `ALARM` state while it was deploying, and the
//! failure and rollback that earns it.
//!
//! # The rule this module implements
//!
//! `DeploymentAlarms` is "one of the methods which provide a way for you to quickly identify
//! when a deployment has failed, and then to optionally roll back the failure to the last
//! working deployment. ... When the alarms are generated, Amazon ECS sets the service
//! deployment to failed. Set the `rollback` parameter to have Amazon ECS to roll back your
//! service to the last completed deployment after a failure" — and `ServiceDeploymentAlarms`
//! says WHEN: "Amazon ECS considers the service deployment as failed when any of the alarms
//! move to the `ALARM` state."
//!
//! So a rollout whose configuration turned alarms on (`DeploymentAlarms$enable`) is watched
//! while it deploys, and an alarm of `alarmNames` that MOVES into `ALARM` meanwhile fails it:
//! the rollout reaches the terminal `FAILED` state of the deployment state machine
//! (`Deployment$rolloutState`) with the reason that names the alarms that fired, those alarms
//! are what `ServiceDeploymentAlarms$triggeredAlarmNames` reports ("one or more CloudWatch
//! alarm names that have been triggered during the service deployment"), and where
//! `DeploymentAlarms$rollback` asked for it the service is taken back to the last deployment
//! that completed successfully. That end is the circuit breaker's own end, reached through the
//! one path both monitors share ([`RolloutFailure`],
//! [`ComAmazonawsEcsAmazonec2containerservicev20141113State::fail_rollout`]) — which is also
//! why the rollback falls due one [`CIRCUIT_BREAKER_ROLLBACK_SPAN`] after the failure, so a
//! client polling the rollout reads the `FAILED` state before the rollback replaces it.
//!
//! Only alarms that moved while THIS rollout was deploying count, which is what the
//! `triggeredAlarmNames` doc states: an alarm that was already in `ALARM` before the rollout
//! started was not triggered during it. That is also what keeps a rollback terminal — the
//! redeployment a rollback starts is a rollout of its own, and an alarm that is still firing
//! from before it began does not fail it again.
//!
//! # Where the alarm LIVES
//!
//! In CloudWatch, which owns the alarm and its state: an alarm is moved into `ALARM` by the
//! metric it watches or by a caller forcing its state, and this module only READS the state
//! that service's own store holds, through that service's own reader of it
//! ([`settle_deployment_alarms`]). Nothing here evaluates an alarm, and nothing here writes to
//! CloudWatch; a rollout watching an alarm no CloudWatch store holds is watching an alarm that
//! has not fired.
//!
//! The pass is driven from the same clock pump the rest of the service lifecycle is
//! ([`super::task_events::drive_task_clock`]), because an alarm moves with no further Amazon
//! ECS call: a client that drives a metric and then polls the service makes no call that
//! would otherwise notice.

use ::service_com_amazonaws_cloudwatch_graniteserviceversion20100801::state as cloudwatch;

use super::*;

/// `Deployment$rolloutStateReason` of a rollout its CloudWatch alarms failed.
///
/// The fixed text states what the monitor detected — the condition
/// `ServiceDeploymentAlarms` words as the alarms moving "to the `ALARM` state" — and the
/// variable part is the alarms that did, named as the deployment configuration named them.
pub fn alarm_failure_reason(triggered: &[String]) -> String {
    format!(
        "ECS deployment alarms: {} in ALARM state.",
        triggered.join(", ")
    )
}

/// `ServiceDeploymentBrief$statusReason` ("information about why the service deployment is in
/// the current status") of a rollout its alarms ROLLED BACK, and the `Rollback$reason` that
/// rollback carries ("the reason the rollback happened").
pub fn alarm_rolled_back_reason(triggered: &[String]) -> String {
    format!(
        "Service deployment rolled back because CloudWatch alarm(s) {} moved to the ALARM \
         state.",
        triggered.join(", ")
    )
}

/// The same for a rollout its alarms failed with no rollback configured, which ends where it
/// stands.
pub fn alarm_stopped_reason(triggered: &[String]) -> String {
    format!(
        "Service deployment stopped because CloudWatch alarm(s) {} moved to the ALARM state.",
        triggered.join(", ")
    )
}

/// The service event the scheduler writes when the alarms fail a rollout, in the shape the
/// Amazon ECS service event messages use for a deployment that failed — the counterpart of
/// the "deployment completed." entry a rollout that reached its steady state earns.
fn deployment_alarm_event(service_name: &str, deployment_id: &str, triggered: &[String]) -> String {
    format!(
        "(service {service_name}) (deployment {deployment_id}) deployment failed: alarm {} \
         in ALARM state.",
        triggered.join(", ")
    )
}

/// One rollout that is being WATCHED by alarms: which service it belongs to, the alarms its
/// deployment configuration named, whether that configuration asked for a rollback, and the
/// instant the rollout started.
#[derive(Debug, Clone)]
pub struct AlarmWatch {
    /// The `(cluster, service)` key the store holds the service under.
    key: (String, String),
    /// `Service$serviceName` and the rollout's `Deployment$id`, for the event the failure
    /// writes on the service's event stream.
    service_name: String,
    deployment_id: String,
    /// `DeploymentAlarms$alarmNames`: "one or more CloudWatch alarm names".
    alarm_names: Vec<String>,
    /// `DeploymentAlarms$rollback`: "determines whether to configure Amazon ECS to roll back
    /// the service if a service deployment fails".
    rollback: bool,
    /// When this rollout began, which is what makes an alarm one that moved "during the
    /// service deployment".
    started_at: SystemTime,
}

impl ComAmazonawsEcsAmazonec2containerservicev20141113State {
    /// Every rollout this store holds that its alarms are WATCHING.
    ///
    /// A rollout is watched while it is DEPLOYING: one that has already reached its steady
    /// state has completed ("when the service reaches a steady state, the deployment
    /// transitions to a `COMPLETED` state") and one a monitor has already failed is at the
    /// end of the state machine ("a deployment in `FAILED` state doesn't launch any new
    /// tasks"), so neither is one an alarm can still fail — the same two conditions the
    /// circuit breaker stands down on ([`super::circuit_breaker`]). Only the ECS scheduler's
    /// own rolling update has such a rollout at all: "you can only use the
    /// `DeploymentAlarms` method to detect failures when the `DeploymentController` is set to
    /// `ECS`."
    pub(super) fn rollout_alarm_watches(&self) -> Vec<AlarmWatch> {
        self.services
            .iter()
            .filter(|(_, service)| service.is_active() && service.places_tasks())
            .filter(|(_, service)| {
                service.deployment_rollout_failure.is_none() && !service.deployment_rollout_completed
            })
            .filter_map(|(key, service)| {
                let alarms = service
                    .deployment_configuration
                    .alarms
                    .as_ref()
                    .filter(|alarms| alarms.enable)?;
                // "One or more CloudWatch alarm names": a configuration that turned the
                // method on without naming an alarm watches nothing.
                if alarms.alarm_names.is_empty() {
                    return None;
                }
                Some(AlarmWatch {
                    key: key.clone(),
                    service_name: service.name.clone(),
                    deployment_id: service.deployment_id.clone(),
                    alarm_names: alarms.alarm_names.clone(),
                    rollback: alarms.rollback,
                    started_at: SystemTime::try_from(service.deployment_created_at).ok()?,
                })
            })
            .collect()
    }

    /// Fail every rollout whose alarms have fired, as of `now`.
    ///
    /// `triggered` pairs one watched rollout with the alarms of its own `alarmNames` that
    /// moved into `ALARM` while it was deploying — read out of the CloudWatch store by
    /// [`settle_deployment_alarms`], which is the only component that holds an alarm's state.
    /// The failure is dated by the instant the LAST of those alarms moved, which is when
    /// Amazon ECS had the evidence, rather than by whichever pass noticed.
    pub(super) fn fail_rollouts_on_triggered_alarms(
        &mut self,
        triggered: &[(AlarmWatch, Vec<(String, SystemTime)>)],
    ) {
        for (watch, fired) in triggered {
            let names: Vec<String> = fired.iter().map(|(name, _)| name.clone()).collect();
            let Some(at) = fired.iter().map(|(_, at)| *at).max() else {
                continue;
            };
            // The rollout has to be the one that was watched: the pass read the store, let
            // the guard go to read CloudWatch, and a rollout the service has since left
            // behind is not the one these alarms were watching.
            if self
                .services
                .get(&watch.key)
                .is_none_or(|service| service.deployment_id != watch.deployment_id)
            {
                continue;
            }
            self.fail_rollout(
                &watch.key,
                at,
                watch.rollback,
                &RolloutFailure {
                    monitor: RolloutMonitor::Alarms,
                    reason: alarm_failure_reason(&names),
                    event: deployment_alarm_event(
                        &watch.service_name,
                        &watch.deployment_id,
                        &names,
                    ),
                    stopped_reason: alarm_stopped_reason(&names),
                    triggered_alarms: names,
                },
            );
        }
    }
}

/// Fail every rollout whose CloudWatch alarms have moved into their `ALARM` state — the ONE
/// call the clock pump makes for it.
///
/// Called with NO guard of this store held: it takes this store's lock to read which rollouts
/// are being watched, releases it, reads the alarm states out of the CloudWatch store, and
/// only then takes this store's lock again to fail what those alarms failed — the
/// one-directional, guard-free discipline every other cross-store access in this crate
/// follows ([`super::service_utilization::settle_service_utilization`]), which is what keeps
/// a cycle between the stores unreachable.
///
/// Nothing is failed on an engine that holds no alarm store to read: an alarm has one home,
/// and a rollout cannot be failed by an alarm nothing holds the state of.
pub fn settle_deployment_alarms<Eng: Send + Sync + 'static>(engine: &Eng) {
    let Some(state) = service_state(engine) else {
        return;
    };
    // The guard this statement takes is a TEMPORARY: it is released at the end of the
    // statement, so nothing below holds it.
    let watches = state.lock().rollout_alarm_watches();
    if watches.is_empty() {
        return;
    }
    let Some(alarms) = cloudwatch::service_state(engine) else {
        return;
    };
    let triggered: Vec<(AlarmWatch, Vec<(String, SystemTime)>)> = {
        let store = alarms.lock();
        watches
            .into_iter()
            .filter_map(|watch| {
                // ONE CloudWatch read per watched rollout, through that service's own reader
                // of the state each named alarm is in, whichever kind of alarm it is
                // (`ComAmazonawsCloudwatchGraniteserviceversion20100801State::child_alarm_states`).
                let fired: Vec<(String, SystemTime)> = store
                    .child_alarm_states(&watch.alarm_names)
                    .into_iter()
                    .filter(|alarm| {
                        alarm.state_value.as_deref() == Some(cloudwatch::ALARM_STATE_ALARM)
                    })
                    .filter_map(|alarm| {
                        let moved_at = moved_at(alarm.transitioned_secs)?;
                        // "One or more CloudWatch alarm names that have been triggered
                        // DURING the service deployment": an alarm that was already in that
                        // state before this rollout began was not triggered during it.
                        (moved_at >= watch.started_at).then_some((alarm.name, moved_at))
                    })
                    .collect();
                (!fired.is_empty()).then_some((watch, fired))
            })
            .collect()
    };
    if triggered.is_empty() {
        return;
    }
    state
        .lock()
        .fail_rollouts_on_triggered_alarms(&triggered);
}

/// The instant an alarm MOVED into the state it reports, from the epoch seconds CloudWatch
/// holds it at (`MetricAlarm$stateTransitionedTimestamp`, "the timestamp of the last change to
/// the alarm's `StateValue`").
///
/// `None` for an alarm that has never transitioned, and for an instant that is not one at all
/// — a value that is no number, that is before the epoch, or that no duration can hold: an
/// alarm in its `ALARM` state with no instant on the record moved there at some instant this
/// store does not hold, which is no evidence that it moved during a rollout. Every step is
/// fallible, so a stored value of any magnitude answers `None` rather than ending the pass.
fn moved_at(transitioned_secs: Option<f64>) -> Option<SystemTime> {
    let seconds = transitioned_secs.filter(|secs| secs.is_finite() && *secs >= 0.0)?;
    let since_epoch = Duration::try_from_secs_f64(seconds).ok()?;
    ::std::time::UNIX_EPOCH.checked_add(since_epoch)
}
