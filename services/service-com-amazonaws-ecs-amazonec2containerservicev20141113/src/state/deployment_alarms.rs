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
//! # How long the watch LASTS
//!
//! Past the instant the rollout's tasks are in place. An alarm reports what the revision now
//! running is doing only once it has graded the series it watches, which takes its own
//! evaluation configuration's span, so a watch that ended with the task set could never catch
//! the one thing this method exists to catch — and a rolling deployment is not complete when
//! its tasks are: "After this percentage of tasks is healthy and the bake time elapses, Amazon
//! ECS completes the deployment" (`DeploymentEarlySuccessCriteria$healthyPercent`). The bake
//! this method earns is the span the named alarms need to speak, as the service that owns them
//! computes it ([`AlarmWatchWindow`], [`MAX_ALARM_WATCH`]), and the rollout reaches its
//! `COMPLETED` state at its end ([`ComAmazonawsEcsAmazonec2containerservicev20141113State::rollout_alarm_watch_end`]).
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

/// The LONGEST a rollout is held open by the watch its alarms hold it under, whatever span
/// their own evaluation configuration asks for.
///
/// A capacity bound of this emulator, not an AWS quota: an alarm's total evaluation period may
/// cover up to a week ("`Period` multiplied by `EvaluationPeriods` can't be more than 604,800
/// seconds", `PutMetricAlarmInput$Period`), and a rollout held open for days by one would never
/// be observed to finish at all. Within it the watch is the alarms' own span
/// ([`AlarmWatchWindow`]), so every alarm a deployment is realistically gated on — the
/// sub-minute and one-minute periods a rollout is graded on — keeps the whole of it.
const MAX_ALARM_WATCH: Duration = Duration::from_secs(600);

/// What ONE pass reads out of the store that owns a watched rollout's alarms: the rollout, the
/// span its own `alarmNames` need to reach a verdict about the revision it released, and the
/// alarms of those names that MOVED into `ALARM` while it was being watched, each with the
/// instant it moved.
type AlarmReading = (AlarmWatch, Option<Duration>, Vec<(String, SystemTime)>);

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
    ///
    /// Deploying LASTS as long as the watch does ([`AlarmWatchWindow`]): a rollout whose
    /// tasks are in place is still being watched until its alarms could have spoken about the
    /// revision they are now running, which is why the two conditions above stay false for
    /// the whole of that window ([`Self::rollout_alarm_watch_end`]).
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

    /// Record the WINDOW each watched rollout's alarms watch it over, as of the spans those
    /// alarms need to speak.
    ///
    /// `spans` pairs one watched rollout with the span its own `alarmNames` need to reach a
    /// verdict, read out of the CloudWatch store by [`settle_deployment_alarms`] — the service
    /// that owns those alarms is the one that knows how long their evaluation takes. A watch
    /// already standing over THIS rollout keeps the end it has: the window is measured once,
    /// from the instant the rollout's tasks reached their steady state, so a later pass cannot
    /// push a bake that is already running further out. A rollout whose alarms this emulator
    /// holds none of gets no window, so it waits for nothing: an alarm nothing holds reaches
    /// no verdict.
    pub(super) fn record_rollout_alarm_watches(&mut self, spans: &[(AlarmWatch, Option<Duration>)]) {
        for (watch, span) in spans {
            let Some(span) = *span else {
                continue;
            };
            let Some(service) = self
                .services
                .get_mut(&watch.key)
                .filter(|service| service.deployment_id == watch.deployment_id)
            else {
                continue;
            };
            match service.deployment_alarm_watch.as_mut() {
                Some(standing) if standing.deployment_id == watch.deployment_id => {
                    standing.verdict_span = span;
                }
                _ => {
                    service.deployment_alarm_watch = Some(AlarmWatchWindow {
                        deployment_id: watch.deployment_id.clone(),
                        verdict_span: span,
                        ends_at: None,
                    });
                }
            }
        }
    }

    /// When the watch this service's current rollout is under ENDS, for a rollout whose tasks
    /// were found at the steady state they were rolling out at `reached_at`; `None` for a
    /// rollout no alarm watch holds back.
    ///
    /// The instant the watch was already measured to, else one verdict span past the steady
    /// state the tasks reached — the bake a rolling deployment completes after
    /// ([`AlarmWatchWindow`]), which is why it begins where those tasks came up and not where
    /// the rollout did. `None` for a service whose configuration turned the method off or
    /// named no alarm — "Amazon ECS is not using alarms for service deployment failures when
    /// the status is `DISABLED`" — and for a window left behind by an earlier rollout.
    pub(super) fn rollout_alarm_watch_end(
        &self,
        service: &ServiceRecord,
        reached_at: SystemTime,
    ) -> Option<SystemTime> {
        service
            .deployment_configuration
            .alarms
            .as_ref()
            .filter(|alarms| alarms.enable && !alarms.alarm_names.is_empty())?;
        let watch = service
            .deployment_alarm_watch
            .as_ref()
            .filter(|watch| watch.deployment_id == service.deployment_id)?;
        Some(watch.ends_at.unwrap_or(reached_at + watch.verdict_span))
    }

    /// The earliest instant in the FUTURE at which an alarm watch of this store ENDS, if any —
    /// one of the instants the clock pump waits for
    /// ([`super::service_action_events`]): the rollout that watch holds back completes there,
    /// with no further call from anybody.
    ///
    /// Only a watch over a rollout that is still open: one a monitor already failed and one
    /// that already completed have reached the end of the deployment state machine, so the
    /// instant their watch was measured to is one no pass would act on.
    pub(super) fn next_alarm_watch_due_at(&self, now: SystemTime) -> Option<SystemTime> {
        self.services
            .values()
            .filter(|service| {
                service.deployment_rollout_failure.is_none() && !service.deployment_rollout_completed
            })
            .filter_map(|service| {
                service
                    .deployment_alarm_watch
                    .as_ref()
                    .filter(|watch| watch.deployment_id == service.deployment_id)
                    .and_then(|watch| watch.ends_at)
            })
            .filter(|due| *due > now)
            .min()
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

/// Settle what the CloudWatch alarms a rollout is watched by make of it — the ONE call the
/// clock pump makes for them: the WINDOW those alarms watch each rollout over, and the failure
/// of every rollout whose alarms have moved into their `ALARM` state inside it.
///
/// Called with NO guard of this store held: it takes this store's lock to read which rollouts
/// are being watched, releases it, reads the alarm states out of the CloudWatch store, and
/// only then takes this store's lock again to fail what those alarms failed — the
/// one-directional, guard-free discipline every other cross-store access in this crate
/// follows ([`super::service_utilization::settle_service_utilization`]), which is what keeps
/// a cycle between the stores unreachable.
///
/// Nothing is failed on an engine that holds no alarm store to read: an alarm has one home,
/// and a rollout cannot be failed by an alarm nothing holds the state of. Nor is one held
/// back: a rollout watched by alarms nothing holds is waiting for a verdict that cannot come.
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
    let read: Vec<AlarmReading> = {
        let store = alarms.lock();
        watches
            .into_iter()
            .map(|watch| {
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
                // And how long those same alarms need to speak about the revision this
                // rollout released, which is what the watch over it lasts
                // ([`AlarmWatchWindow`]) — read through that service's own account of its
                // alarms' evaluation, since the span is their configuration's and not this
                // service's.
                let span = store
                    .alarms_verdict_span_secs(&watch.alarm_names)
                    .and_then(|secs| Duration::try_from_secs_f64(secs).ok())
                    .map(|span| span.min(MAX_ALARM_WATCH));
                (watch, span, fired)
            })
            .collect()
    };
    let spans: Vec<(AlarmWatch, Option<Duration>)> = read
        .iter()
        .map(|(watch, span, _)| (watch.clone(), *span))
        .collect();
    let triggered: Vec<(AlarmWatch, Vec<(String, SystemTime)>)> = read
        .into_iter()
        .filter(|(_, _, fired)| !fired.is_empty())
        .map(|(watch, _, fired)| (watch, fired))
        .collect();
    let mut store = state.lock();
    store.record_rollout_alarm_watches(&spans);
    store.fail_rollouts_on_triggered_alarms(&triggered);
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
