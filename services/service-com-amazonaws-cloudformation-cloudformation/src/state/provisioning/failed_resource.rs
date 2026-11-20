// SPDX-License-Identifier: CC0-1.0

//! What a deployment does to ONE resource whose change the service that owns it refused.
//!
//! CloudFormation does not leave such a resource half-changed: the configuration in force
//! before the deployment is declared again in that service. WHOSE work that is depends on
//! what becomes of the stack, and that is the whole of this module:
//!
//! * a stack that goes on to ROLL BACK puts the resource back as part of that rollback,
//!   which runs after this deployment and declares the previous configuration of every
//!   resource it is putting back ([`super::StackProvenance::reapplied`]) — so nothing is
//!   put back here, and the revert is reported where it happens, after the stack's own
//!   `UPDATE_ROLLBACK_IN_PROGRESS`;
//! * a stack that RESTS in the failed status (`DisableRollback`) has no rollback of its own
//!   to report, so the failed resource — and only that resource — is put back by the
//!   deployment itself, reported on that resource's own `UPDATE_ROLLBACK_*` transitions
//!   ([`roll_back_failed_resource`]).

use super::*;

use crate::state::resource_status;

/// The sentence CloudFormation words on the event that OPENS the rollback of the failed
/// resource alone: what it is doing, which is putting back that resource and nothing else of
/// the update.
///
/// FIXED text. WHICH resource is being put back is the event's own `LogicalResourceId`, so no
/// part of the sentence comes from the template or from any one run.
const ROLLING_BACK_THE_FAILED_RESOURCE: &str = "Rolling back the failed resource only.";

/// The sentence it words on the event that ENDS it, once the owning service has taken the
/// previous configuration back: FIXED text, for the same reason.
const ROLLBACK_SUCCEEDED: &str = "Rollback succeeded for the failed resources.";

/// Put ONE resource whose change this deployment could not carry through back the way it
/// was, through the owning service's own update API, and report that revert on the
/// resource's own transitions.
///
/// Only for the deployment that is the LAST word on that resource — one whose stack rests in
/// the failed status ([`FailedResourceRevert::OfTheResourceAlone`]). Where the stack rolls
/// back, the rollback is what declares the previous configuration again, so this does
/// nothing: the module docs record why.
///
/// `in_force` is the record of the same resource the stack had BEFORE this deployment, whose
/// properties are the configuration being put back. The record is left stating that
/// configuration, and a service that would not take it leaves the resource in the failed
/// rollback status with its own refusal, never a revert nothing carried out.
pub(super) async fn roll_back_failed_resource(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    stack: &StackProvenance,
    resource: &mut StoredResource,
    in_force: &StoredResource,
    outcome: &mut Materialised,
) {
    match stack.failed_resource_revert {
        FailedResourceRevert::None => return,
        FailedResourceRevert::OfTheResourceAlone => (),
    }
    outcome.transitions.push(
        ResourceTransition::starting(resource, resource_status::UPDATE_ROLLBACK_IN_PROGRESS)
            .because(ROLLING_BACK_THE_FAILED_RESOURCE),
    );
    let restored = in_force.properties.clone();
    let owner = owner_for(&resource.resource_type);
    let put_back = match owner {
        Some(provisioner) => {
            // BOXED, like every other provider call this walk makes from inside a future
            // this big: the call's own future is the owning service's whole handler, and
            // holding a second one of those inline in [`reconcile`]'s state machine is what
            // takes a deployment of a wide template past the thread's stack.
            Box::pin(crate::state::service_role::on_resource(
                crate::state::service_role::target_of(resource),
                provisioner.update(
                    ctx,
                    region,
                    resource,
                    &restored,
                    &stack.tags_for(&resource.logical_id),
                    &stack.capabilities,
                ),
            ))
            .await
        }
        // Nothing exists in any owning service for this type, so there is nothing anywhere
        // to put back: the stack's own record IS the resource, and restoring the record
        // below is the whole revert.
        None => Ok(Some(Created::default())),
    };
    match put_back {
        // Either the owning service took the previous configuration, or the type has no
        // update path at all and so was never changed: the resource carries the
        // configuration it had before this deployment, which is what the record states.
        Ok(_) => {
            resource.properties = restored;
            outcome.transitions.push(ResourceTransition::finished(
                resource,
                resource_status::UPDATE_ROLLBACK_COMPLETE,
                Some(ROLLBACK_SUCCEEDED.to_string()),
            ));
        }
        Err(refusal) => {
            outcome.transitions.push(ResourceTransition::finished(
                resource,
                resource_status::UPDATE_ROLLBACK_FAILED,
                Some(handler_failure_reason(
                    owner,
                    &refusal,
                    &resource.physical_resource_id,
                )),
            ));
        }
    }
}
