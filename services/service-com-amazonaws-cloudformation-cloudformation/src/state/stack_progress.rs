// SPDX-License-Identifier: CC0-1.0

//! What a STARTED stack operation owes, carried out without waiting for the next client call.
//!
//! ## The rule this module implements
//!
//! A status change CloudFormation records on a stack reaches the account's default event bus
//! because the change HAPPENED ([`super::stack_status_events`]), and the notifications a change
//! owes the stack's `NotificationARNs` go out for the same reason
//! ([`super::stack_notifications`]). Neither is owed to a READER: a client driven by those
//! events creates its rule, starts the operation and never calls CloudFormation again, so
//! every transition the operation's own call did not already record — the conclusion the
//! clock makes due once the call has answered, a wait that runs out, the rollback a failure
//! owes — has to be materialised and published by this service itself.
//!
//! The operations that REPORT a stack's progress materialise exactly that work
//! ([`super::provisioning::advance_stabilisations_in`]). This module is how the same thing
//! happens with NOBODY reading: the call that STARTS a stack operation hands the stack over
//! here, and a task of this service's own materialises what the clock has made due
//! ([`super::ComAmazonawsCloudformationCloudformationState::settle`]) and publishes what that
//! owes, pass after pass, until the stack has settled.
//!
//! A pass does the WHOLE of what it can of that work, the readiness questions another service
//! answers included: how far a deployment has got is never the READER's doing. A resource whose
//! owning service accepted the create without reporting it ready holds its stack's operation
//! open, and only that service's own answer ends the wait — so each pass asks it, exactly as a
//! reporting operation does. Were that left to the handlers a client calls, a deployment would
//! advance one step per read and a stack with a chain of dependencies would settle only for a
//! client that polls it often enough — which is the reader deciding when the deployment finishes.
//!
//! What a pass does NOT do is the work that is CLAIMED from this store before it is carried out:
//! the rollback a failure owes the services that own the stack's resources is taken under the
//! store's lock for the caller that is about to make those calls, and the stack's terminal status
//! stays held back until that caller has reported what they answered
//! ([`super::ComAmazonawsCloudformationCloudformationState::take_due_rollbacks`]). A task that is
//! cut short between the claim and the answer would hold that work for good, and a detached task
//! can be cut short — a host that drives this emulator in process drops the runtime its request
//! ran on. So that half stays with the handlers a client calls
//! ([`super::settle_due_rollbacks`]), and the asymmetry is deliberate rather than tidy: a FAILED
//! deployment nobody reads waits for a reader to carry its rollback out, which
//! `core/router/tests/all/cfn_redeploy_reaches_a_terminal_status.rs` pins. Everything a pass here
//! does leaves the store ready for the next pass to do it again, so being cut short costs it
//! nothing. The task a landed provider answer starts drives this same advance — and, because the
//! answer it carries IS the claim it was started for, the rollback half as well
//! ([`super::provider_driver::drive_landed_answer`]).
//!
//! Nothing here decides an outcome. Each pass materialises what the CLOCK had already made due
//! at the instant it runs — the same thing a reporting operation materialises, and nothing a
//! reporting operation would not — so a client reading the stack afterwards sees exactly what
//! it would have seen had it done the reading itself, and a client that never reads it at all
//! leaves it in the same place.
//!
//! ## Bounds
//!
//! One task per started operation, which STOPS as soon as the stack carries a status no longer
//! in flight and owes the bus nothing — a pass per level of the stack's own dependency chain for
//! the ordinary deployment. A stack whose progress waits on something further out (a wait
//! condition's own timeout, a provider that has not answered) is looked at again on a widening
//! interval ([`pass_interval`]) for at most [`MAX_PASSES`] passes, and then left to the next read
//! of it; and the tasks this service has outstanding at once are capped
//! ([`super::stack_set_events::TaskSlot`], the same ceiling every other event task of this
//! service takes its slot from), so a stream of stack operations cannot leave unbounded work
//! behind.

use ::std::sync::Arc;
use ::std::time::Duration;

use ::smithy_server_runtime::OperationContext;

use super::provider_driver::ProvisioningReach;
use super::stack_set_events::TaskSlot;

/// How many passes one started operation's task makes before leaving the stack to the next
/// read of it.
///
/// With [`pass_interval`] this is a window of about a minute, which is longer than any
/// transition this service puts on a stack's clock ([`super::STATUS_TRANSITION_DELAY`]) and
/// long enough for the waits a template asks for in that window to run out.
const MAX_PASSES: usize = 24;

/// How many passes run one transition apart: the span this service puts between a stack's
/// statuses, so each of those passes runs with the instant it waited for already past rather
/// than racing it.
const FIRST_PASSES: usize = 8;

/// The interval between the passes after those.
const LATER_PASS_INTERVAL: Duration = Duration::from_secs(2);

/// The pass from which the interval widens again, counted from the first.
const LATER_PASSES: usize = 16;

/// The widest interval between two passes: what a stack still in flight after the window above
/// is looked at on.
const WIDEST_PASS_INTERVAL: Duration = Duration::from_secs(5);

/// How long the task waits before pass `pass`.
///
/// It starts at one transition — the span this service puts between a stack's statuses — and
/// widens, so the deployment that settles at once costs two passes while the stack held at a
/// wait nothing has answered is looked at a bounded number of times over a window of about a
/// minute.
fn pass_interval(pass: usize) -> Duration {
    if pass < FIRST_PASSES {
        super::STATUS_TRANSITION_DELAY
    } else if pass < LATER_PASSES {
        LATER_PASS_INTERVAL
    } else {
        WIDEST_PASS_INTERVAL
    }
}

/// Carry the stack `stack_id` of `region` through the transitions its started operation still
/// owes, in a task of its own, publishing what each one owes.
///
/// Called by the operation that STARTS a stack operation, after it has answered everything it
/// records itself: the task holds only cloneable handles to this emulator — the callback every
/// provisioning call rides and this service's identity — so it borrows nothing of the request
/// that started it, exactly as [`super::provider_driver::drive_landed_answer`]'s does, and the
/// Region it runs for is passed to it rather than read off a request.
///
/// `stack_id` is anything a read of this store resolves a stack by (its id or its name), since
/// all it decides is WHEN there is nothing left to carry: a stack this store no longer holds at
/// all, and one whose status is not an operation in flight and which owes the bus no event, is
/// done.
pub fn drive_started_operation<Eng: Send + Sync + 'static>(
    ctx: &OperationContext<'_>,
    engine: &Arc<Eng>,
    region: &str,
    stack_id: &str,
) {
    drive(ctx, engine, region, Some(stack_id));
}

/// [`drive_started_operation`] for a call that started one on SEVERAL stacks of `region` at
/// once — a stack-set rollout, whose stack instances are stacks of this store like any other,
/// each owing the bus its own status changes.
///
/// It carries every stack of the Region that is still in flight, which is what such a call left
/// behind, and stops once none is.
pub fn drive_started_operations_in<Eng: Send + Sync + 'static>(
    ctx: &OperationContext<'_>,
    engine: &Arc<Eng>,
    region: &str,
) {
    drive(ctx, engine, region, None);
}

/// Carry the stacks of `region` the call that started this through what they still owe: the one
/// stack `target` names, or every stack of the Region for a caller that started one on several.
fn drive<Eng: Send + Sync + 'static>(
    ctx: &OperationContext<'_>,
    engine: &Arc<Eng>,
    region: &str,
    target: Option<&str>,
) {
    if super::service_state(&**engine).is_none() {
        return;
    }
    let Some(slot) = TaskSlot::claim() else {
        ::tracing::warn!(
            "this service's event tasks are all in flight; this stack operation's remaining \
             transitions will be materialised by the next read of it"
        );
        return;
    };
    let reach = ProvisioningReach::of(ctx);
    let engine = Arc::clone(engine);
    let region = region.to_string();
    let target = target.map(ToString::to_string);
    // DETACHED on purpose: the work outlives the call that started it, exactly as the
    // deployment it carries on does. Nothing of it is unbounded, for the reasons above.
    ::tokio::spawn(async move {
        let _slot = slot;
        for pass in 0..MAX_PASSES {
            ::tokio::time::sleep(pass_interval(pass)).await;
            let Some(state) = super::service_state(&*engine) else {
                return;
            };
            // The parts a context is built over name nothing: this task serves no request, so
            // there is no caller, no credential and no body — the Region the work runs for is
            // passed to it, and the calls it makes are signed by the callback with the
            // emulator's own credentials exactly as a handler's are.
            let parts = ::http::Request::new(()).into_parts().0;
            let ctx = OperationContext::new(&parts, reach.identity(), reach.callback().clone());
            // The waits the stack's resources are held at: the owning services are asked whether
            // they have them ready, and the answers are recorded — the work a reporting operation
            // does, done here because a deployment's progress is this emulator's and not its
            // reader's. Nothing of it is CLAIMED from the store before it is carried out, so a
            // pass that is cut short leaves the work for the next one. The rollback a failure owes
            // those services is NOT driven here for that very reason: it is taken from the store
            // for the caller that performs it, so it belongs to a handler
            // ([`super::settle_due_rollbacks`]).
            super::provisioning::advance_stabilisations_in(&ctx, &state, &region).await;
            // What the clock owes the stack, and the two fan-outs each transition it recorded
            // owes: the notifications the stack's `NotificationARNs` are owed
            // ([`super::stack_notifications`]) and the status-change events the account's
            // default event bus is ([`super::stack_status_events`]). Both settle this store
            // first, so what they publish is every change owed AS OF NOW.
            super::stack_notifications::deliver(&ctx, &state, &region).await;
            super::stack_status_events::deliver(&ctx, &state, &region).await;
            if nothing_left_to_carry(&state, &region, target.as_deref()) {
                return;
            }
        }
        // The window is over and the operation is still in flight: this task carries it no
        // further, exactly as a task that could claim no slot does not start, so the hand-off is
        // reported the same way — what is left is materialised by the next read of the stack.
        //
        // Both values are recorded as DEBUG rather than as display text, so the identifier a
        // request chose is quoted and escaped in the record instead of being written into it
        // verbatim; the stack field is left off altogether for the Region-wide carrier, which
        // names no one stack.
        ::tracing::warn!(
            region = ?region,
            stack = ?target,
            passes = MAX_PASSES,
            "this stack operation was still in flight after the window this service carries one \
             for; its remaining transitions will be materialised by the next read of it"
        );
    });
}

/// Whether the stacks this task is carrying have nothing left for it: no operation of theirs is
/// in flight and every event they have recorded has been published.
///
/// `target` is the one stack a single-stack caller named, addressed the way a read of this store
/// addresses one (its id or its name), or `None` for every stack of the Region. A stack this
/// store no longer holds is done too — there is nothing left to carry for a record that is gone.
fn nothing_left_to_carry(
    state: &::engine::ServiceState<super::ComAmazonawsCloudformationCloudformationState>,
    region: &str,
    target: Option<&str>,
) -> bool {
    let store = state.lock();
    match target {
        Some(stack_id) => store.resolve_any(region, stack_id).is_none_or(settled),
        None => store
            .stacks
            .iter()
            .filter(|stack| stack.region == region)
            .all(settled),
    }
}

/// Whether one stack has nothing left for this task ([`nothing_left_to_carry`]).
fn settled(stack: &super::StoredStack) -> bool {
    !stack.status.ends_with(super::IN_PROGRESS_SUFFIX)
        && !stack.events.iter().any(|event| event.owed_bus_event)
}
