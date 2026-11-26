// SPDX-License-Identifier: CC0-1.0

//! The work a launched cluster has ACCEPTED, carried out whether or not anybody asks about
//! it.
//!
//! ## The rule this module implements
//!
//! Amazon EMR runs a cluster's queue itself: "Amazon EMR executes each step in the order
//! listed" (`AddJobFlowSteps` doc), from the instant the cluster is ready, and a client learns
//! what a step came to by polling `DescribeStep` on an interval of its own choosing. So the
//! work does not wait for the observer — the observer waits for the work.
//!
//! Every operation that REPORTS a step or its cluster performs the work that cluster already
//! owes before it answers ([`super::run_due_bootstrap_actions`],
//! [`super::resolve_due_step_catalog_access`], [`super::run_due_step_applications`],
//! [`super::resolve_due_step_commands`], [`super::deliver_due_step_logs`]), because a handler
//! is the one place an in-process call can be made from. Driven only from there, a cluster
//! advances one piece of work per CALL: a submitted application takes longer than the slice a
//! client's call may be held for ([`super::APPLICATION_FIRST_WAIT`]), so its answer is
//! recorded by the next call, which is also the first call that can start the step behind it
//! — and a client polling every twenty seconds then pays a whole interval per step, however
//! fast the work itself was. A two-step queue takes three polls to report a verdict that the
//! work reached in seconds.
//!
//! This module is what closes that gap: a task of this process drives the SAME functions a
//! reporting operation drives, for one cluster, until that cluster owes no step work. Nothing
//! here decides an outcome and nothing happens sooner than it was due — the task materialises
//! what the clock has already made due and records what a submitted application answered, so
//! a client reading the cluster afterwards sees exactly what it would have seen had its own
//! read driven the work.
//!
//! ## What the task runs under
//!
//! An in-process call needs a handler's [`::smithy_server_runtime::OperationContext`], which
//! carries the callback this emulator's own calls ride. A task serves no request, so the
//! context is built from what the launching call left behind — the callback handle and this
//! service's identity ([`ClusterReach`]) — and the Region is the one that call was signed for,
//! passed to the task rather than read off a request.
//!
//! ## Bounds
//!
//! One task per cluster ([`ComAmazonawsEmrElasticmapreduceState::step_work_drivers`]), each
//! making at most [`MAX_DRIVE_PASSES`] passes spaced [`DRIVE_INTERVAL`] apart and stopping as
//! soon as the cluster owes nothing, so a launch costs a bounded amount of work. A queue still
//! in flight after that window is one a client is polling anyway, and every such poll drives
//! the same functions.

use std::sync::Arc;
use std::time::{Duration, SystemTime};

use ::smithy_server_runtime::{OperationContext, ServiceIdentity};

use super::*;

/// How long the driver waits between passes.
///
/// CHOSEN: short beside the span a client polls a step at and beside the step's own run span
/// ([`super::STEP_RUN_SPAN`]), so the queue advances as the work answers rather than at the
/// pace of whoever is watching; long enough that a cluster with nothing to do costs a wake-up
/// every half second until it stops. The pass itself waits for the work
/// ([`super::APPLICATION_FIRST_WAIT`]), so a pass with work to do takes as long as that work.
pub const DRIVE_INTERVAL: Duration = Duration::from_millis(500);

/// How many passes one launch's driver makes.
///
/// CHOSEN as the bound on what one launch costs this process: a queue whose steps are still
/// in flight after this window is one a client is reading anyway, and each of those reads
/// drives the same work. A driver that finds the cluster's queue settled stops at once, so
/// this is a ceiling and not a cost.
pub const MAX_DRIVE_PASSES: usize = 240;

/// The handle a task makes this emulator's own calls through, kept from the call that
/// launched the cluster.
///
/// Both halves are cheap, cloneable handles to THIS emulator: the callback
/// ([`::smithy_server_runtime::OperationContext::callback`]) is the in-process transport every
/// call of this service already rides, and the identity is this service's own. Neither carries
/// anything of the request it came from — no caller, no credential of a caller's and no body.
#[derive(Debug, Clone)]
pub struct ClusterReach {
    callback: ::smithy_server_runtime::EmbeddedEmulator,
    identity: ServiceIdentity,
}

impl ClusterReach {
    /// The reach of the handler running `ctx`.
    #[must_use]
    pub fn of(ctx: &OperationContext<'_>) -> Self {
        Self {
            callback: ctx.callback().clone(),
            identity: ctx.identity().clone(),
        }
    }
}

/// Drive the work the cluster `cluster_id` owes until it owes none, in a task of its own.
///
/// Called by the operations that ACCEPT work for a cluster — the launch and a later step
/// submission — once they have released this store's guard. A cluster that already has a
/// driver gets no second one.
pub fn drive_cluster_work<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
    ctx: &OperationContext<'_>,
    region: &str,
    cluster_id: &str,
) {
    let Some(state) = service_state(&**engine) else {
        return;
    };
    // With no async runtime there is nothing to drive the work from, so it stays owed and the
    // operations that report the cluster perform it, exactly as before.
    let Ok(runtime) = ::tokio::runtime::Handle::try_current() else {
        return;
    };
    let reach = ClusterReach::of(ctx);
    let engine = Arc::clone(engine);
    let region = region.to_string();
    let driven = cluster_id.to_string();
    let held = ::engine::ServiceState::clone(&state);
    // Claimed and held in ONE critical section, so two launches of the same cluster cannot
    // both start a driver for it. Spawning takes no lock of this store: the task's first act
    // is to wait.
    let mut store = state.lock();
    if store
        .step_work_drivers
        .get(cluster_id)
        .is_some_and(|handle| !handle.is_finished())
    {
        return;
    }
    let handle = runtime.spawn(async move {
        for _ in 0..MAX_DRIVE_PASSES {
            ::tokio::time::sleep(DRIVE_INTERVAL).await;
            // The parts a context is built over name nothing: this task serves no request, so
            // there is no caller, no credential and no body — the Region the work runs for is
            // the one the launching call was signed for.
            let parts = ::http::Request::new(()).into_parts().0;
            let ctx = OperationContext::new(&parts, &reach.identity, reach.callback.clone());
            let now = SystemTime::now();
            run_due_bootstrap_actions(&*engine, &ctx, &region, &driven, now).await;
            resolve_due_step_catalog_access(&*engine, &ctx, &region, &driven).await;
            run_due_step_applications(&*engine, &ctx, &region, &driven, now).await;
            resolve_due_step_commands(&*engine, &ctx, &region, &driven, now).await;
            deliver_due_step_logs(&*engine, &ctx, &region, &driven, SystemTime::now()).await;
            if !owes_step_work(&*engine, &driven) {
                break;
            }
        }
        // This driver's own entry: a later driver exists only once this task has ended, so
        // the entry removed here is never another's.
        held.lock().step_work_drivers.remove(&driven);
    });
    store
        .step_work_drivers
        .insert(cluster_id.to_string(), handle);
}

/// Whether the cluster `cluster_id` still owes the run of any step: it is held, it has not
/// finished, and a step of it has not reached a state it never leaves.
///
/// A cluster whose every step is terminal owes nothing a driver could perform, and neither
/// does one that is gone or finished — which is what ends the pass loop above.
#[must_use]
pub fn owes_step_work<Eng: Send + Sync + 'static>(engine: &Eng, cluster_id: &str) -> bool {
    let Some(state) = service_state(engine) else {
        return false;
    };
    let store = state.lock();
    store.clusters.get(cluster_id).is_some_and(|cluster| {
        !cluster.is_terminal() && cluster.steps.iter().any(|step| !step.is_terminal())
    })
}
