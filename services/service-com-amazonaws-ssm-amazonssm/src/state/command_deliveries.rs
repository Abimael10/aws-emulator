// SPDX-License-Identifier: CC0-1.0

//! The DELIVERY a sent command still owes a node whose agent has not picked it up yet.
//!
//! # What a delivery is, and why one can be outstanding
//!
//! Run Command delivers a document's script to the AGENT in a node's guest, and this emulator
//! performs that script in the guest as the send goes through
//! ([`crate::ops::send_command`]) — the agent is in that guest and the guest is Amazon EC2's
//! own ([`super::guest_execution`]). An agent whose guest has no operating system up yet has
//! taken nothing: a node is a managed node from the instant its instance was launched
//! ([`super::node_registration`], because the agent is part of the machine the launch starts),
//! while the guest it runs in comes up when that service's own clock brings it up. A send that
//! reaches such a node therefore leaves its invocation exactly where Run Command leaves one
//! nothing has reached — "Pending: The command hasn't been sent to the managed node"
//! (`CommandInvocation$StatusDetails`) — with the script it is to run kept beside it
//! ([`super::commands::AwaitedDelivery`]).
//!
//! # Why a read may make the write
//!
//! The agent takes the command once its guest is up, whether or not anybody calls in between:
//! the delivery is owed to the CLOCK, like every other move this store materialises on a read
//! ([`super::association_patch_runs`] is the same arrangement). So the operations that report
//! what a command did take this first, and what it writes is only ever what was already owed:
//! a run that has happened, recorded on the invocation that was waiting for it, with the move
//! off `Pending` owed at the instant the SEND fixed ([`super::commands::AwaitedDelivery::due_at`])
//! rather than at this call, so a deferred delivery cannot report a command as finished
//! earlier than the send did.
//!
//! An invocation whose guest is still not up is left waiting, and one whose guest never comes
//! up is not left waiting for ever: the placeholder the send recorded times out at the
//! command's own delivery timeout ("`ExpiresAfter` is calculated based on the total timeout
//! for the overall command", `Command$ExpiresAfter`), which is what reports it as a command
//! that was never delivered.

use std::sync::Arc;
use std::time::SystemTime;

use super::commands::{InvocationOutcome, MAX_GUEST_DELIVERIES_PER_SEND};
use super::ComAmazonawsSsmAmazonssmState;

/// One delivery a command owes one node: everything the run needs, read while the store's
/// guard was held.
#[derive(Debug, Clone)]
struct OwedDelivery {
    /// The command whose invocation is waiting.
    command_id: String,
    /// The node whose agent has not taken it yet.
    instance_id: String,
    /// The step of the document whose plugin that agent runs.
    plugin_name: String,
    /// The lines it delivers, as ONE script.
    commands: Vec<String>,
    /// The instant the send fixed for the invocation's move off `Pending`.
    due_at: SystemTime,
}

/// Perform every delivery `region`'s commands already owe, so a read of a command reports
/// what the agents in their nodes' guests have run whether or not anybody called.
///
/// Call it with NO guard of this service's store held: each run reaches the store that owns
/// the guest, and the programs one delivery installs in that guest are resolved from several
/// other services' stores ([`super::guest_execution::delivery_in_guest`]).
///
/// Bounded the way a send's own deliveries are ([`MAX_GUEST_DELIVERIES_PER_SEND`]): the work
/// one request can make this process do is the product of the nodes it delivers to and the
/// bound on one run, and this caps the first factor. A delivery past the cap stays owed and is
/// performed by the next read.
pub async fn settle_due_deliveries<Eng: Send + Sync + 'static>(
    engine: &Arc<Eng>,
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
) {
    let Some(state) = super::service_state(&**engine) else {
        return;
    };
    let owed = {
        let store = state.lock();
        owed_deliveries(&store, region)
    };
    if owed.is_empty() {
        return;
    }
    // What the guest PROGRAMS of this one request may spend between them, made once and
    // handed to every delivery of it — the same bound a send's own deliveries share.
    let programs = super::guest_python::ProgramAllowance::for_one_request();
    for owed in owed {
        // One delivery per node, each performing a bounded run: the yield between them is
        // what keeps the calling handler's task cancellable while they are performed.
        ::tokio::task::yield_now().await;
        let delivery = super::guest_execution::delivery_in_guest(
            &**engine,
            ctx,
            region,
            &owed.instance_id,
            SystemTime::now(),
            &programs,
        );
        // The run is synchronous and CPU-bound and is performed with the guard of the store
        // that owns the guest held, so it happens on a BLOCKING thread rather than on a
        // worker of this runtime, exactly as a send's own delivery does.
        let engine = Arc::clone(engine);
        let node = owed.instance_id.clone();
        let commands = owed.commands.clone();
        let performed = ::tokio::task::spawn_blocking(move || {
            super::guest_execution::perform_delivery(&*engine, &node, &commands, delivery)
        })
        .await;
        // A guest that is still not up has taken nothing, so the delivery stays owed and
        // nothing is written for it: no outcome is reported for a run that did not happen.
        let Some(run) = performed.ok().flatten() else {
            continue;
        };
        let mut store = state.lock();
        let Some(invocation) = store
            .command_mut(region, &owed.command_id)
            .and_then(|command| command.invocations.get_mut(&owed.instance_id))
        else {
            continue;
        };
        // Read again under this guard: another caller may have settled the same delivery
        // while this run was being performed, and an agent takes one command once.
        if invocation.awaiting.is_none() {
            continue;
        }
        invocation.delivered(InvocationOutcome {
            plugin_name: owed.plugin_name,
            output: run.output,
            errors: run.errors,
            response_code: run.status,
            // The instant the SEND fixed, or now for a run whose instant has already passed:
            // a delivery that was owed does not report the invocation finished before the
            // send said it would be.
            due_at: owed.due_at.max(SystemTime::now()),
            delivered: true,
        });
    }
}

/// Every delivery `region`'s commands owe: one per invocation whose node's agent has not
/// picked the script up yet and whose invocation has not been cancelled or finished since.
///
/// A READ of the store, so the walk that writes above takes the guard again rather than
/// holding one while it reaches another service's state.
fn owed_deliveries(store: &ComAmazonawsSsmAmazonssmState, region: &str) -> Vec<OwedDelivery> {
    store
        .commands_in_region(region)
        .flat_map(|(command_id, command)| {
            command
                .invocations
                .values()
                .filter(|invocation| invocation.cancel_settles_at.is_none())
                .filter_map(|invocation| {
                    let awaited = invocation.awaiting.as_ref()?;
                    Some(OwedDelivery {
                        command_id: command_id.to_string(),
                        instance_id: invocation.instance_id.clone(),
                        plugin_name: awaited.plugin_name.clone(),
                        commands: awaited.commands.clone(),
                        due_at: awaited.due_at,
                    })
                })
                .collect::<Vec<OwedDelivery>>()
        })
        .take(MAX_GUEST_DELIVERIES_PER_SEND)
        .collect()
}
