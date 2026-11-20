// SPDX-License-Identifier: CC0-1.0

//! The LIFECYCLE EVENTS a deployment target reports: the one projection every
//! reader of a target answers them from.
//!
//! "The lifecycle events of the deployment to this target Lambda function"
//! (`smithy_query(query="doc",shape="LambdaTarget",member="lifecycleEvents")`) is
//! the deployment's own progression through the lifecycle events its compute
//! platform RUNS, each with the `LifecycleEventStatus` it reached
//! (`smithy_query(query="shape",shape="LifecycleEventStatus")`: Pending |
//! InProgress | Succeeded | Failed | Skipped | Unknown) — not a log of the
//! reports that happened to arrive. So for a platform whose whole sequence this
//! service runs ([`lifecycle_hook::platform_lifecycle_events`]) the target
//! carries one event per step of that sequence, in the order the platform runs
//! them, whether or not the revision declared a validation function for the step:
//! a client reads which step the deployment is on, which one failed, and what
//! each one ended as.
//!
//! Where a step's own result is HELD it is that result: the verdict the
//! revision's validation function reported for it ([`lifecycle_hook`]), else a
//! result reported directly against that lifecycle event. Where none is held, the
//! step is as far along as the deployment itself has taken it — the step before
//! the traffic move completed once the move began, the step the deployment is at
//! ends with the deployment, and a step the deployment never reached was never
//! run.
//!
//! A platform whose lifecycle events this service does not run — EC2/on-premises,
//! whose events the CodeDeploy agent runs — reports the events it HAS: the hooks
//! its revision declared and the results reported against it, rather than events
//! nothing ran.

use crate::ops::types::LifecycleEvent;

use super::lifecycle_hook::{self, HookExecution};
use super::{
    target_last_updated_at_ms, timestamp, Deployment, LifecycleEventHookExecution,
    DEPLOYMENT_STATUS_FAILED, DEPLOYMENT_STATUS_STOPPED, DEPLOYMENT_STATUS_SUCCEEDED,
    LIFECYCLE_EVENT_STATUS_FAILED, LIFECYCLE_EVENT_STATUS_IN_PROGRESS,
    LIFECYCLE_EVENT_STATUS_PENDING, LIFECYCLE_EVENT_STATUS_SKIPPED,
    LIFECYCLE_EVENT_STATUS_SUCCEEDED, LIFECYCLE_EVENT_STATUS_UNKNOWN,
};

/// The lifecycle events of `deployment`, as the modelled `LifecycleEvent` list
/// every one of its targets carries; `None` for a deployment that has no
/// lifecycle event to report at all.
///
/// This emulator deploys to every target of a deployment together, so the events
/// are the DEPLOYMENT's — the same list on each of its targets, and the same list
/// whichever reader asks for one.
pub fn events_of(deployment: &Deployment) -> Option<Vec<LifecycleEvent>> {
    let sequence = lifecycle_hook::platform_lifecycle_events(&deployment.compute_platform);
    let mut events: Vec<LifecycleEvent> = match sequence {
        Some(sequence) => sequence
            .iter()
            .map(|name| sequence_event(deployment, name))
            .collect(),
        None => events_held(deployment),
    };
    // A result reported against a lifecycle event the sequence above does not
    // carry is a report of one of the platform's own events made directly rather
    // than by a function this service invoked, and is reported as it arrived. One
    // that landed on an event already above is not reported twice.
    events.extend(reports_outside(deployment, sequence));
    super::non_empty(&events)
}

/// One step of the platform's lifecycle sequence, as the event a target reports
/// for it.
fn sequence_event(deployment: &Deployment, name: &str) -> LifecycleEvent {
    // The validation function the revision declared for this lifecycle event: the
    // result it reported IS what the event completed with.
    if let Some(hook) = deployment
        .hook_executions
        .iter()
        .find(|hook| hook.lifecycle_event_name == name)
    {
        return hook_event(deployment, hook);
    }
    // Else a result reported directly against this lifecycle event — the last
    // one, since each report completes the event it was made for.
    if let Some(reported) = deployment
        .lifecycle_hook_executions
        .iter()
        .rev()
        .find(|execution| execution.lifecycle_event_name == name)
    {
        return reported_event(reported);
    }
    // Else the step carries no result of its own, so it is as far along as the
    // deployment has taken it.
    let status = step_status(deployment, name);
    LifecycleEvent {
        lifecycle_event_name: Some(name.to_string()),
        diagnostics: None,
        start_time: None,
        // A step that ENDED ended when the deployment last moved, which is the
        // only instant this service holds for it; one still ahead of the
        // deployment has not ended at all.
        end_time: matches!(
            status,
            LIFECYCLE_EVENT_STATUS_SUCCEEDED | LIFECYCLE_EVENT_STATUS_FAILED
        )
        .then(|| timestamp(target_last_updated_at_ms(deployment))),
        status: Some(status.to_string()),
    }
}

/// The `LifecycleEventStatus` of a step no result is held for: how far the
/// deployment itself has taken it.
fn step_status(deployment: &Deployment, name: &str) -> &'static str {
    // The step that MOVES the traffic is work this service does itself rather
    // than through a function, so it is reported from that move's own progress.
    if name == lifecycle_hook::LIFECYCLE_EVENT_ALLOW_TRAFFIC {
        if let Some(status) = lifecycle_hook::allow_traffic_event(deployment) {
            return status;
        }
    }
    phase_status(
        deployment,
        name == lifecycle_hook::LIFECYCLE_EVENT_AFTER_ALLOW_TRAFFIC,
    )
}

/// The `LifecycleEventStatus` of a step on the deployment's own progress alone,
/// for a step that runs after the traffic move (`runs_after_traffic`) or before
/// it.
///
/// A deployment that SUCCEEDED ran every step it has. Otherwise the traffic move
/// is what says which steps are behind the deployment: a move that has begun
/// completed the steps before it, a step the deployment is AT ends with the
/// deployment — failed where the deployment failed, skipped where a caller or an
/// alarm stopped it — and a step the deployment never reached was never run.
fn phase_status(deployment: &Deployment, runs_after_traffic: bool) -> &'static str {
    if deployment.status == DEPLOYMENT_STATUS_SUCCEEDED {
        return LIFECYCLE_EVENT_STATUS_SUCCEEDED;
    }
    if runs_after_traffic {
        if !traffic_is_complete(deployment) {
            return match deployment.status.as_str() {
                DEPLOYMENT_STATUS_FAILED | DEPLOYMENT_STATUS_STOPPED => {
                    LIFECYCLE_EVENT_STATUS_SKIPPED
                }
                _ => LIFECYCLE_EVENT_STATUS_PENDING,
            };
        }
    } else if traffic_has_begun(deployment) {
        return LIFECYCLE_EVENT_STATUS_SUCCEEDED;
    }
    match deployment.status.as_str() {
        DEPLOYMENT_STATUS_FAILED => LIFECYCLE_EVENT_STATUS_FAILED,
        DEPLOYMENT_STATUS_STOPPED => LIFECYCLE_EVENT_STATUS_SKIPPED,
        _ if deployment.is_started() => LIFECYCLE_EVENT_STATUS_IN_PROGRESS,
        _ => LIFECYCLE_EVENT_STATUS_PENDING,
    }
}

/// Whether the traffic move this deployment performs has BEGUN: some of the
/// traffic the revision declared is on the version it deploys, which is what
/// every step before the move having completed is what allowed.
fn traffic_has_begun(deployment: &Deployment) -> bool {
    deployment.traffic_increments_applied > 0
}

/// Whether that move is carried out in FULL, which is what the steps after it
/// wait on; see [`lifecycle_hook::allow_traffic_event`], which reports the move
/// itself from the same progress.
fn traffic_is_complete(deployment: &Deployment) -> bool {
    !deployment.traffic_increments.is_empty()
        && deployment.traffic_increments_applied >= deployment.traffic_increments.len()
}

/// The events a deployment of a platform whose sequence this service does not run
/// HOLDS: the hooks its revision declared, in the order that platform runs them,
/// around the traffic move for a deployment that performs one.
fn events_held(deployment: &Deployment) -> Vec<LifecycleEvent> {
    let before_traffic = deployment
        .hook_executions
        .iter()
        .filter(|hook| !hook.runs_after_traffic())
        .map(|hook| hook_event(deployment, hook));
    let allow_traffic = lifecycle_hook::allow_traffic_event(deployment).map(|status| {
        LifecycleEvent {
            lifecycle_event_name: Some(
                lifecycle_hook::LIFECYCLE_EVENT_ALLOW_TRAFFIC.to_string(),
            ),
            diagnostics: None,
            start_time: None,
            end_time: (status == LIFECYCLE_EVENT_STATUS_SUCCEEDED)
                .then(|| timestamp(target_last_updated_at_ms(deployment))),
            status: Some(status.to_string()),
        }
    });
    let after_traffic = deployment
        .hook_executions
        .iter()
        .filter(|hook| hook.runs_after_traffic())
        .map(|hook| hook_event(deployment, hook));
    before_traffic
        .chain(allow_traffic)
        .chain(after_traffic)
        .collect()
}

/// The results reported against `deployment` that none of the events above
/// already carries; see [`events_of`].
fn reports_outside(
    deployment: &Deployment,
    sequence: Option<&'static [&'static str]>,
) -> Vec<LifecycleEvent> {
    deployment
        .lifecycle_hook_executions
        .iter()
        .filter(|execution| {
            // A report that landed on a hook execution this deployment DISPATCHED
            // is the result of that hook's own event, reported above.
            !deployment.hook_executions.iter().any(|hook| {
                hook.lifecycle_event_hook_execution_id.as_deref()
                    == Some(execution.lifecycle_event_hook_execution_id.as_str())
            })
        })
        .filter(|execution| {
            // And a report naming a step of the platform's sequence is that
            // step's own result, reported above.
            sequence.is_none_or(|sequence| {
                !sequence
                    .iter()
                    .any(|name| *name == execution.lifecycle_event_name)
            })
        })
        .map(reported_event)
        .collect()
}

/// The lifecycle event one DECLARED hook of `deployment` reports: how far its
/// own execution has got ([`hook_event_status`]).
fn hook_event(deployment: &Deployment, hook: &HookExecution) -> LifecycleEvent {
    LifecycleEvent {
        lifecycle_event_name: Some(hook.lifecycle_event_name.clone()),
        diagnostics: None,
        start_time: None,
        end_time: hook.update_time_ms.map(timestamp),
        status: Some(hook_event_status(deployment, hook).to_string()),
    }
}

/// The lifecycle event one direct report describes.
///
/// A report says what the lifecycle event ended as and when the report was made,
/// which is when that event ENDED; when it started is not something a report
/// carries, so it is left absent.
fn reported_event(execution: &LifecycleEventHookExecution) -> LifecycleEvent {
    LifecycleEvent {
        lifecycle_event_name: Some(execution.lifecycle_event_name.clone()),
        diagnostics: None,
        start_time: None,
        end_time: Some(timestamp(execution.update_time_ms)),
        status: Some(execution.status.clone()),
    }
}

/// The `LifecycleEventStatus` one declared hook's lifecycle event carries
/// (`smithy_query(query="shape",shape="LifecycleEventStatus")`: Failed |
/// InProgress | Pending | Skipped | Succeeded | Unknown).
///
/// The result its validation function reported, when it reported one: that result
/// IS what the lifecycle event completed with. Otherwise the event is as far along
/// as the deployment has taken it — dispatched to the function and awaiting its
/// report (`InProgress`), and otherwise on the deployment's own progress
/// ([`phase_status`]): a deployment that FAILED at the event its function was
/// validating has a failed lifecycle event, and one a caller stopped never ran the
/// rest of them.
fn hook_event_status(deployment: &Deployment, hook: &HookExecution) -> &'static str {
    if let Some(reported) = hook.status.as_deref() {
        return match reported {
            LIFECYCLE_EVENT_STATUS_FAILED => LIFECYCLE_EVENT_STATUS_FAILED,
            LIFECYCLE_EVENT_STATUS_SUCCEEDED => LIFECYCLE_EVENT_STATUS_SUCCEEDED,
            // Only the two results above are reportable, so anything else is a
            // status this service does not hold for the event.
            _ => LIFECYCLE_EVENT_STATUS_UNKNOWN,
        };
    }
    // While the deployment is still running, the event is either being validated
    // right now — the hook was dispatched to its function and no result has
    // arrived — or not reached yet.
    if !deployment.is_complete() {
        return if hook.lifecycle_event_hook_execution_id.is_some() {
            LIFECYCLE_EVENT_STATUS_IN_PROGRESS
        } else {
            LIFECYCLE_EVENT_STATUS_PENDING
        };
    }
    // The deployment is over and no result ever arrived for this event, so the
    // event ended with the deployment.
    phase_status(deployment, hook.runs_after_traffic())
}
