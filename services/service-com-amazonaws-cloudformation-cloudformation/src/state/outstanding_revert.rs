// SPDX-License-Identifier: CC0-1.0

//! The ROLLBACK a failed stack operation owes the services that own the stack's resources:
//! what it has to put back there, and the pass that performs it.
//!
//! "If the stack update fails, CloudFormation rolls back the stack to the previous stack
//! configuration" — and putting that configuration back is CALLS into other services, which
//! the store cannot make under its own lock and a stack's clock cannot make at all. So the
//! work is recorded as owed ([`OwedRollback`], taken as [`DueRollback`]) and performed by the
//! next operation of this service that can reach those services ([`settle_due_rollbacks`]),
//! with the rollback's own terminal status held back until it has.

use super::*;

/// One stack's rollback work, taken off the store for the operation that is about to
/// perform it ([`ComAmazonawsCloudformationCloudformationState::take_due_rollbacks`]).
#[derive(Debug, Clone)]
pub struct DueRollback {
    /// The Region the stack's resources live in, which is the credential scope every call
    /// the rollback makes carries.
    pub region: String,
    pub stack_id: String,
    /// The stack's identity and tags, which a resource the rollback re-declares is created
    /// carrying ([`provisioning::StackProvenance`]).
    pub provenance: provisioning::StackProvenance,
    /// The service role in force on the stack, whose permissions decide the calls this
    /// rollback makes in the services that own its resources ([`service_role`]).
    pub role_arn: Option<String>,
    /// What this rollback does with the `DeletionPolicy` of a resource the failed operation
    /// had created, which the revert removes again
    /// ([`StoredStack::rolled_back_resource_policies`]).
    pub policies: provisioning::Policies,
    pub rollback: OwedRollback,
}

/// Perform the rollbacks the stacks of this store owe the services that own their
/// resources, and record what those services answered.
///
/// An update held at a wait condition is failed by the CLOCK when its wait runs out
/// ([`StoredStack::settle_wait_conditions`]), and CloudFormation rolls such an update back
/// to the previous stack configuration exactly as it rolls back one an owning service
/// refused. The rollback's own calls — putting back what the failed update changed there —
/// reach other services' handlers, which the store cannot do under its lock and a stack's
/// clock cannot do at all, so they are made HERE: by the next operation of this service to
/// be called, before it reads or writes the stack.
///
/// This materialises what was already due at the same instant whether or not anybody
/// called: the failure is the clock's, and the rollback is what that failure owes. It is
/// therefore correct for a REPORTING operation to drive it — the status a later caller reads
/// is the one the clock had already made due — and it is what keeps a stack from reporting a
/// rollback as complete that nothing carried out: [`StoredStack::settle`] holds the
/// rollback's terminal status back until the work recorded here is done.
///
/// Call it with the handler's `ctx` and `&*self.engine`, holding no store lock.
pub async fn settle_due_rollbacks<Eng: Send + Sync + 'static>(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    engine: &Eng,
) {
    let Some(state) = service_state(engine) else {
        return;
    };
    // The failures the clock has made due, and the rollback work each one owes — taken
    // under the lock, performed with it RELEASED.
    let due: Vec<DueRollback> = {
        let mut store = state.lock();
        store.settle(SystemTime::now());
        // ...and how this emulator is reached, so a provider answer that lands out of band
        // can drive this same work itself ([`provider_driver`]).
        store.remember_provisioning_reach(ctx);
        store.take_due_rollbacks()
    };
    for owed in due {
        let outstanding = owed.rollback;
        // The rollback's calls are the STACK's, made under its service role: a role denied
        // the call that would put a resource back cannot make it, which is what leaves the
        // stack in `UPDATE_ROLLBACK_FAILED` below.
        let role = service_role::ServiceRole::of(engine, owed.role_arn.as_deref());
        let reverted = service_role::acting_as(
            role,
            provisioning::reconcile(
                ctx,
                &owed.region,
                &owed.provenance,
                outstanding.previous.clone(),
                &outstanding.applied,
                // A rollback puts back a configuration this stack already deployed, so
                // there is nothing for CloudFormation to refuse about it.
                None,
                owed.policies,
            ),
        )
        .await;
        let mut store = state.lock();
        let Some(stack) = store.resolve_mut(&owed.region, &owed.stack_id) else {
            continue;
        };
        // A resource whose owning service accepted the revert without having carried it out
        // — or whose PROVIDER has been sent the request and has not answered — holds this
        // rollback open, exactly as it holds the operation being rolled back open: the
        // rollback is not reported complete while a resource it put back is still coming up,
        // and a wait that runs out fails it ([`provisioning::advance_stabilisations`]).
        //
        // Recorded BEFORE the walk's events are scheduled, as every other deployment does
        // it: a resource still being put back is one the walk's own terminal event for it is
        // GATED behind ([`StoredStack::schedule_resource_events`]), so nothing reports the
        // revert as complete until the answer that decides it has landed.
        stack.record_stabilisations(&reverted.stabilising, SystemTime::now());
        stack.schedule_resource_events(&reverted.transitions);
        stack.record_materialised_resources(reverted.resources);
        // What the rollback left each resource in, which is what a client reads back —
        // and what says which resource a resumed rollback may skip.
        stack.record_rollback_resource_statuses(&reverted.transitions, SystemTime::now());
        if let Some(failure) = reverted.failure {
            // "A stack goes into the UPDATE_ROLLBACK_FAILED state when CloudFormation can't
            // roll back all changes after a failed stack update": a resource the owning
            // service would not put back leaves the stack there — the state
            // `ContinueUpdateRollback` resumes from — with that service's own refusal as
            // the reason, rather than reporting a rollback that did not happen. What those
            // services are still holding is kept, because that is what a resumed rollback
            // has to put back — the plan the rollback was opened with
            // ([`StoredStack::begin_owed_update_rollback`] holds it as the outstanding one
            // for exactly this, so there is nothing to put back on the stack here).
            stack.fail_transition(
                SystemTime::now(),
                status::UPDATE_ROLLBACK_FAILED,
                failure,
            );
        }
    }
}

/// What the ROLLBACK of the UPDATE in flight would have to put back in the services that
/// own the stack's resources, held against the chance that the update fails with no call of
/// its own to compute it.
///
/// An update held at a wait condition has already asked the owning services for everything
/// it wanted ([`provisioning::reconcile`], driven by the handler), and the thing that can
/// still end it is its wait running out — which falls due on the CLOCK. CloudFormation
/// "rolls back the stack to the previous stack configuration" when an update fails,
/// whichever way it failed, so the work that rollback owes is computed by the handler that
/// knows what the update changed, carried here, and performed by the next operation that
/// can reach those services ([`settle_due_rollbacks`]).
///
/// One record per stack with an update in flight, carrying one entry per resource that
/// update declared ([`MAX_CONCURRENT_RESOURCES`] across the store), retired with the
/// operation that holds it.
#[derive(Debug, Clone)]
pub struct OwedRollback {
    /// The resources in force before the update — the configuration to put back.
    pub previous: Vec<StoredResource>,
    /// What the update left in the owning services, which the revert runs against.
    pub applied: Vec<StoredResource>,
    /// The resources whose change the owning service was ASKED for and REFUSED, which this
    /// rollback therefore declares the previous configuration of again whatever the records
    /// say ([`provisioning::Materialised::part_changed`] records why a record is not enough).
    pub part_changed: ::std::collections::BTreeSet<String>,
}
