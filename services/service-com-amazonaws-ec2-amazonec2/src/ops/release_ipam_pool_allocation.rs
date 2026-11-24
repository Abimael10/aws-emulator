// SPDX-License-Identifier: CC0-1.0

//! @op ReleaseIpamPoolAllocation
//!
//! Gives one allocation's range back to the IPAM pool it came out of: the record the store
//! holds for it goes, so the space is free for the pool to hand out again and
//! `GetIpamPoolAllocations` stops reporting it.
//!
//! "Release an allocation within an IPAM pool" (`ReleaseIpamPoolAllocation`) takes the
//! assignment back whatever holds the range: the recorded wire answers a release of the
//! allocation a VPC's CIDR came out of with a success, and reports the pool's allocation set
//! empty afterwards. What goes is IPAM's record that the space is assigned — the resource
//! itself keeps the address block it was created with, which is why the API documents
//! `ModifyIpamResourceCidr` as the way to stop MONITORING a resource's CIDR rather than as
//! another way to free the space.
//!
//! ## The wording of the refusals here
//!
//! The model declares no error shape for this operation (ec2Query reports a fault through the
//! generic envelope) and the wire corpus records only the SUCCESS of a release, so the only
//! refusals whose sentence is AWS's own are the ones every resource of this crate shares: the
//! malformed-id and not-found templates. The one refusal this operation words for itself — a
//! `Cidr` that is not the range the allocation named by `IpamPoolAllocationId` holds — is
//! therefore this crate's statement of the modelled rule rather than reproduced EC2 text: its
//! CODE is the class EC2 answers a parameter that does not resolve with, and every value in it
//! comes from the request and from the stored allocation.
//!
//! ## Reaching this service's state
//!
//! The engine is held on `self` (constructor-injected), NOT passed per request. The store is
//! reached through [`crate::state::require_service_state`], the way every handler beside this
//! one reaches it.
//!
//! Shared types live in `super::types`; the error channel in `crate::scaffolded`.
//!
//! ---
//!
//! # Modeled contract for `ReleaseIpamPoolAllocation`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")
//! ```
//!
//! Release an allocation within an IPAM pool. The Region you use should be the IPAM pool
//! locale. The locale is the Amazon Web Services Region where this IPAM pool is available
//! for allocations. You can only use this action to release manual allocations. To remove
//! an allocation for a resource without deleting the resource, set its monitored state to
//! false using ModifyIpamResourceCidr. For more information, see Release an allocation in
//! the Amazon VPC IPAM User Guide. All EC2 API actions follow an eventual consistency
//! model.
//!
//! ## Input
//!
//! * `Cidr` (String) **required** — The CIDR of the allocation you want to release.
//! * `DryRun` (Boolean) — A check for whether you have the required permissions for the
//!   action without actually making the request and provides an error response. If you have
//!   the required permissions, the error response is DryRunOperation. Otherwise, it is
//!   UnauthorizedOperation.
//! * `IpamPoolAllocationId` (IpamPoolAllocationId) **required** — The ID of the allocation.
//! * `IpamPoolId` (IpamPoolId) **required** — The ID of the IPAM pool which contains the
//!   allocation you want to release.
//!
//! ## Output
//!
//! * `Success` (Boolean) — Indicates if the release was successful.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ReleaseIpamPoolAllocationError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ec2-amazonec2.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ReleaseIpamPoolAllocationError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.ec2#ReleaseIpamPoolAllocation")]
#[derive(Debug)]
pub enum ReleaseIpamPoolAllocationErrorModeled {
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::release_ipam_pool_allocation::ReleaseIpamPoolAllocationError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type ReleaseIpamPoolAllocationError = crate::scaffolded::Scaffolded<ReleaseIpamPoolAllocationErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ReleaseIpamPoolAllocationErrorModeled> for ReleaseIpamPoolAllocationError {
    fn from(modeled: ReleaseIpamPoolAllocationErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ReleaseIpamPoolAllocation` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ReleaseIpamPoolAllocation<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ReleaseIpamPoolAllocation<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ReleaseIpamPoolAllocation<Eng>
{
    type Input = ReleaseIpamPoolAllocationInput;
    type Output = ReleaseIpamPoolAllocationOutput;
    type Error = ReleaseIpamPoolAllocationError;

    const NAME: &'static str = "ReleaseIpamPoolAllocation";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::RELEASE_IPAM_POOL_ALLOCATION_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::RELEASE_IPAM_POOL_ALLOCATION_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        use crate::state;

        // `ctx` carries request metadata only; this operation reads none of it — "the Region
        // you use should be the IPAM pool locale", which the POOL carries, so the pool the
        // request names is what the allocation is resolved in. The store is reached through
        // the engine held on `self`.
        let _ = ctx;

        // The FORM of each id the request names, before anything is resolved and before the
        // DryRun arm below: "not an id of this kind" is a fact about the request, so it is
        // answered `.Malformed` rather than as a pool or an allocation that does not exist —
        // the order every resource of this crate asks the two questions in.
        let pool_id = input.ipam_pool_id.as_str();
        state::require_well_formed_id(
            state::IPAM_POOL_ID_PREFIX,
            pool_id,
            state::INVALID_IPAM_POOL_ID_MALFORMED,
        )?;
        let allocation_id = input.ipam_pool_allocation_id.as_str();
        state::require_well_formed_ipam_pool_allocation_id(allocation_id)?;

        // "The CIDR of the allocation you want to release" (`$Cidr`): the range is part of
        // WHICH allocation the request names, so a value that is no CIDR block at all names
        // none — refused on its own form, in the canonical spelling every range of this crate
        // is compared in, so a range given with host bits set still names the range it is
        // part of.
        let requested_cidr = input.cidr.as_str();
        let canonical_cidr = state::canonical_cidr_block(requested_cidr).ok_or_else(|| {
            state::invalid_parameter_value(format!(
                "Invalid value '{cidr}' for cidr. It must be a CIDR block",
                cidr = crate::ops::delete_coip_cidr::echoed_cidr(requested_cidr),
            ))
        })?;

        // "A check for whether you have the required permissions for the action without
        // actually making the request" (`$DryRun`): a question about the CALLER, answered once
        // the request's own form has been checked and before anything is resolved or written.
        // Every caller of this emulator is permitted.
        if input.dry_run.unwrap_or(false) {
            return Err(state::dry_run_operation());
        }

        let state_handle = state::require_service_state(&*self.engine)?;
        let mut store = state_handle.lock();
        // The clock first, the way every other state-touching handler in this crate starts, so
        // every question below is asked of the state the store is ACTUALLY in.
        store.materialize_due_transitions();

        // "The ID of the IPAM pool which contains the allocation you want to release"
        // (`$IpamPoolId`): an id this account holds no pool for is `.NotFound`, exactly as it
        // is for the read of the same pool's allocations.
        if !store.ipam_pools.contains_key(pool_id) {
            return Err(state::id_not_found(
                state::INVALID_IPAM_POOL_ID_NOT_FOUND,
                state::IPAM_POOL_ID_KIND,
                pool_id,
            ));
        }

        // "The ID of the allocation" (`$IpamPoolAllocationId`), resolved IN that pool through
        // the store's one lookup of an allocation: an id the pool holds no allocation for is a
        // dangling reference rather than a release that quietly did nothing.
        let held = store
            .ipam_pool_allocation(pool_id, allocation_id)
            .ok_or_else(|| {
                state::id_not_found(
                    state::INVALID_IPAM_POOL_ALLOCATION_ID_NOT_FOUND,
                    state::IPAM_POOL_ALLOCATION_ID_KIND,
                    allocation_id,
                )
            })?;
        let held_cidr = held.view.cidr.clone().unwrap_or_default();

        // The two have to name ONE allocation: a `Cidr` that is not the range the allocation
        // holds names a different assignment than the id does, so the request says two things
        // and neither is acted on. The sentence is this crate's statement of that rule (see
        // the module docs on the wording of the refusals here), built from the request's own
        // value and the stored range.
        if held_cidr != canonical_cidr {
            return Err(state::invalid_parameter_value(format!(
                "Invalid value '{cidr}' for cidr. The allocation '{allocation_id}' holds the \
                 CIDR '{held_cidr}'",
                cidr = crate::ops::delete_coip_cidr::echoed_cidr(requested_cidr),
            )));
        }

        // The range stops being assigned here: the record goes through the store's one
        // release, which gives the bytes it held back and leaves the pool reporting the space
        // as free — what a later `GetIpamPoolAllocations` of the pool answers.
        if !store.release_ipam_pool_allocation(pool_id, allocation_id) {
            // The allocation resolved a moment ago and the store is locked throughout, so
            // this cannot be reached; kept so a release that removed nothing can never read
            // as a success over an allocation the pool still holds.
            return Err(state::internal_error(
                "the IPAM pool allocation resolved a moment ago could not be released",
            ));
        }

        // "Indicates if the release was successful" (`$Success`): the allocation this call
        // named is gone from the pool, which is the whole of what it does.
        Ok(ReleaseIpamPoolAllocationOutput {
            success: Some(true),
        })
    }
}
