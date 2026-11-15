// SPDX-License-Identifier: CC0-1.0

//! @op ModifyIpamPool
//!
//! Applies one modification to a stored IPAM pool and reports the pool as the store then
//! holds it, so a later reader — `DescribeIpamPools`, `GetIpamPoolCidrs`, the create that
//! puts a pool inside this one — sees exactly what this call answered.
//!
//! A member the request carries replaces the stored one; a member it leaves out leaves the
//! stored value alone rather than returning it to a create's default, and the allocation
//! resource tags are the set the pool requires now less the ones the request removes and
//! with the ones it adds written over them — all of it through the one definition of what a
//! modification does to a pool ([`crate::state::IpamPoolModification::applied_to`]), so the
//! pool this checks is the pool the store keeps.
//!
//! Nothing the pool's creation decided moves: the id, the ARN, the owner, the scope and the
//! IPAM it lives in, its address family, its depth in its tree and its state are all carried
//! over, and the POOL THE MODIFICATION LEAVES BEHIND is held to the same documented
//! allocation rules a create holds a new pool to — the per-family netmask range, a minimum
//! below the maximum, and a default inside both.
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
//! # Modeled contract for `ModifyIpamPool`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")
//! ```
//!
//! Modify the configurations of an IPAM pool. For more information, see Modify a pool in
//! the Amazon VPC IPAM User Guide.
//!
//! ## Input
//!
//! * `AddAllocationResourceTags` (RequestIpamResourceTagList) — Add tag allocation rules to a
//!   pool. For more information about allocation rules, see Create a top-level pool in the
//!   Amazon VPC IPAM User Guide.
//! * `AllocationDefaultNetmaskLength` (IpamNetmaskLength, range 0..128) — The default netmask
//!   length for allocations added to this pool. If, for example, the CIDR assigned to this
//!   pool is 10.0.0.0/8 and you enter 16 here, new allocations will default to 10.0.0.0/16.
//! * `AllocationMaxNetmaskLength` (IpamNetmaskLength, range 0..128) — The maximum netmask
//!   length possible for CIDR allocations in this IPAM pool to be compliant. Possible netmask
//!   lengths for IPv4 addresses are 0 - 32. Possible netmask lengths for IPv6 addresses are 0
//!   - 128.The maximum netmask length must be greater than the minimum netmask length.
//! * `AllocationMinNetmaskLength` (IpamNetmaskLength, range 0..128) — The minimum netmask
//!   length required for CIDR allocations in this IPAM pool to be compliant. Possible netmask
//!   lengths for IPv4 addresses are 0 - 32. Possible netmask lengths for IPv6 addresses are 0
//!   - 128. The minimum netmask length must be less than the maximum netmask length.
//! * `AutoImport` (Boolean) — If true, IPAM will continuously look for resources within the
//!   CIDR range of this pool and automatically import them as allocations into your IPAM. The
//!   CIDRs that will be allocated for these resources must not already be allocated to other
//!   resources in order for the import to succeed. IPAM will import a CIDR regardless of its
//!   compliance with the pool's allocation rules, so a resource might be imported and
//!   subsequently marked as noncompliant. If IPAM discovers multiple CIDRs that overlap, IPAM
//!   will import the largest CIDR only. If IPAM discovers multiple CIDRs with matching CIDRs,
//!   IPAM will randomly import one of them only. A locale must be set on the pool for this
//!   feature to work.
//! * `ClearAllocationDefaultNetmaskLength` (Boolean) — Clear the default netmask length
//!   allocation rule for this pool.
//! * `Description` (String) — The description of the IPAM pool you want to modify.
//! * `DryRun` (Boolean) — A check for whether you have the required permissions for the
//!   action without actually making the request and provides an error response. If you have
//!   the required permissions, the error response is DryRunOperation. Otherwise, it is
//!   UnauthorizedOperation.
//! * `IpamPoolId` (IpamPoolId) **required** — The ID of the IPAM pool you want to modify.
//! * `RemoveAllocationResourceTags` (RequestIpamResourceTagList) — Remove tag allocation
//!   rules from a pool.
//!
//! ## Output
//!
//! * `IpamPool` (IpamPool) — The results of the modification.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `ModifyIpamPoolError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ec2-amazonec2.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `ModifyIpamPoolError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.ec2#ModifyIpamPool")]
#[derive(Debug)]
pub enum ModifyIpamPoolErrorModeled {
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::modify_ipam_pool::ModifyIpamPoolError`). Wrapping the modeled enum is what
/// lets an operation whose model declares no errors at all answer EC2's own documented error
/// codes.
pub type ModifyIpamPoolError = crate::scaffolded::Scaffolded<ModifyIpamPoolErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<ModifyIpamPoolErrorModeled> for ModifyIpamPoolError {
    fn from(modeled: ModifyIpamPoolErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `ModifyIpamPool` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct ModifyIpamPool<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> ModifyIpamPool<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for ModifyIpamPool<Eng>
{
    type Input = ModifyIpamPoolInput;
    type Output = ModifyIpamPoolOutput;
    type Error = ModifyIpamPoolError;

    const NAME: &'static str = "ModifyIpamPool";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::MODIFY_IPAM_POOL_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::MODIFY_IPAM_POOL_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        use crate::state;

        // `ctx` carries request metadata only; this operation reads none of it — "the Amazon
        // Web Services Region of the IPAM pool" (`IpamPool$IpamRegion`) was settled by the
        // scope the pool was created in, and a modification cannot move a pool to another
        // Region or another scope. The store is reached through the engine held on `self`.
        let _ = ctx;

        // The request's own FORM first, before anything is resolved and before the DryRun arm
        // below: "not an IPAM pool id at all" is a malformed request rather than a pool that
        // does not exist, which is the order every resource of this crate asks the two
        // questions in (`crate::state::require_well_formed_id`).
        let pool_id = input.ipam_pool_id.as_str();
        state::require_well_formed_id(
            state::IPAM_POOL_ID_PREFIX,
            pool_id,
            state::INVALID_IPAM_POOL_ID_MALFORMED,
        )?;

        // "Clear the default netmask length allocation rule for this pool"
        // (`ClearAllocationDefaultNetmaskLength`) and "the default netmask length for
        // allocations added to this pool" (`AllocationDefaultNetmaskLength`) ask two
        // incompatible things of ONE rule, so a request carrying both is refused rather than
        // one of the two silently winning.
        let clear_default = input
            .clear_allocation_default_netmask_length
            .unwrap_or(false);
        if clear_default && input.allocation_default_netmask_length.is_some() {
            return Err(state::invalid_parameter_combination(
                "AllocationDefaultNetmaskLength cannot be specified together with \
                 ClearAllocationDefaultNetmaskLength",
            ));
        }

        // "Add tag allocation rules to a pool" (`AddAllocationResourceTags`) and "remove tag
        // allocation rules from a pool" (`RemoveAllocationResourceTags`). Both lists are read
        // HERE, before anything is looked up or written, by the ONE reader of such a list in
        // this crate, which `CreateIpamPool` reads the rules it stores by — so the two cannot
        // disagree about which tag rules a pool admits, and each list is resolved as the SET
        // it is.
        let add_allocation_resource_tags = state::ipam_allocation_resource_tags(
            input.add_allocation_resource_tags.as_deref(),
            "AddAllocationResourceTags",
        )?;
        let remove_allocation_resource_tags = state::ipam_allocation_resource_tags(
            input.remove_allocation_resource_tags.as_deref(),
            "RemoveAllocationResourceTags",
        )?;
        let remove_allocation_resource_tag_keys: Vec<String> = remove_allocation_resource_tags
            .iter()
            .filter_map(|tag| tag.key.clone())
            .collect();

        // One tag key both added and removed asks for two incompatible things of the same
        // allocation rule, so the request is refused rather than one of the two winning —
        // the same reading `crate::ops::modify_ipam` has of one operating Region named in
        // both of its lists.
        if let Some(conflicting) = add_allocation_resource_tags
            .iter()
            .filter_map(|tag| tag.key.as_deref())
            .find(|added| {
                remove_allocation_resource_tag_keys
                    .iter()
                    .any(|removed| removed == added)
            })
        {
            return Err(state::invalid_parameter_combination(format!(
                "The allocation resource tag key '{conflicting}' cannot be added and removed \
                 by the same request"
            )));
        }

        let modification = state::IpamPoolModification {
            ipam_pool_id: pool_id.to_string(),
            description: input.description.clone(),
            auto_import: input.auto_import,
            allocation_min_netmask_length: input.allocation_min_netmask_length,
            allocation_max_netmask_length: input.allocation_max_netmask_length,
            allocation_default_netmask_length: input.allocation_default_netmask_length,
            clear_allocation_default_netmask_length: clear_default,
            add_allocation_resource_tags,
            remove_allocation_resource_tag_keys,
        };

        let state_handle = state::require_service_state(&*self.engine)?;
        let mut store = state_handle.lock();
        // The clock first, the way every other state-touching handler in this crate starts:
        // what this modification is applied to is the pool as it ACTUALLY stands rather than
        // as the call before it left it. Those moves were owed at this instant whether or not
        // anybody called.
        store.materialize_due_transitions();

        // "The ID of the IPAM pool you want to modify" (`IpamPoolId`): an id this account
        // holds no pool for has nothing to modify, which is the pool's own `NotFound` refusal
        // every reader of an `IpamPoolId` in this crate answers.
        let current = store.ipam_pools.get(pool_id).cloned().ok_or_else(|| {
            state::id_not_found(
                state::INVALID_IPAM_POOL_ID_NOT_FOUND,
                state::IPAM_POOL_ID_KIND,
                pool_id,
            )
        })?;
        // A pool a delete has already been asked for has nothing left to configure, so a
        // modification of it is refused rather than written — in EC2's own wording for a
        // resource refused SOLELY for the state it is in, through the one writer of it.
        if state::ipam_pool_is_deleting(&current) {
            return Err(state::incorrect_state_of(
                "IPAM pool",
                pool_id,
                current.state.as_deref().unwrap_or_default(),
            ));
        }

        // Every netmask length the request carries, held to the range the POOL's address
        // family admits — a modification cannot change which family a pool holds — by the ONE
        // reader of that bound in this crate, which `crate::ops::create_ipam_pool` holds a new
        // pool's lengths to.
        let family = current.address_family.as_deref().unwrap_or_default();
        state::require_netmask_lengths_for_family(
            family,
            &[
                (
                    "AllocationMinNetmaskLength",
                    input.allocation_min_netmask_length,
                ),
                (
                    "AllocationMaxNetmaskLength",
                    input.allocation_max_netmask_length,
                ),
                (
                    "AllocationDefaultNetmaskLength",
                    input.allocation_default_netmask_length,
                ),
            ],
        )?;

        // What this call would leave behind, computed by the ONE definition of what a
        // modification does to a pool, so the pool the rules below are asked about is exactly
        // the one the write stores. The rules are asked of the RESULT rather than of the
        // request, because a request that sets only one bound of a range is still answered
        // against the other bound the pool already holds.
        let applied = modification.applied_to(&current);

        // The pool the modification leaves behind, held to the pool's documented allocation
        // netmask rules by the ONE reader of those rules in this crate — which
        // `crate::ops::create_ipam_pool` holds a NEW pool to. They are asked of the RESULT
        // rather than of the request, because a call that sets one of the three rules is
        // answered against the two the pool already holds.
        state::require_ipam_pool_netmask_rules(
            applied.allocation_min_netmask_length,
            applied.allocation_max_netmask_length,
            applied.allocation_default_netmask_length,
        )?;

        // The tag rules the pool would end up requiring, bounded the way every tag set in
        // this crate is bounded: each list is within the ceiling on its own, so only their
        // union with the rules the pool already holds can pass it.
        let required_tags = applied
            .allocation_resource_tags
            .as_deref()
            .unwrap_or_default();
        if required_tags.len() > state::MAX_TAGS_PER_RESOURCE {
            return Err(crate::scaffolded::Scaffolded::tag_limit_exceeded(format!(
                "The modification would leave the IPAM pool '{pool_id}' requiring {count} \
                 allocation resource tags, above the maximum of {max} tags per resource",
                count = required_tags.len(),
                max = state::MAX_TAGS_PER_RESOURCE,
            )));
        }

        // The caller-chosen bytes this modification would leave the store holding, by the ONE
        // definition of that charge, so the figure checked here is exactly the one the write
        // charges. The budget is asked about the NET change: this call stores no further
        // pool, so the pool ceiling cannot bound it, while the description and the tag rules
        // it sets are caller-chosen text.
        let refund = state::ipam_pool_modifiable_bytes(&current);
        let charge = state::ipam_pool_modifiable_bytes(&applied);
        if store.would_exceed_stored_bytes_after_refund(charge, refund) {
            return Err(state::resource_limit_exceeded(format!(
                "This emulator holds {held} of at most {max} bytes of caller-supplied data, \
                 so it cannot store this modification's {charge} bytes",
                held = store.stored_bytes(),
                max = state::MAX_STORED_BYTES,
            )));
        }

        // Every parameter has been read and the pool resolved, and nothing has been written
        // yet: "If you have the required permissions, the error response is DryRunOperation"
        // (`DryRun`), so a request that WOULD have worked stops here, and one that would have
        // been refused was refused above with its own reason — the place
        // `crate::ops::modify_ipam` answers the same question in.
        if input.dry_run.unwrap_or(false) {
            return Err(state::dry_run_operation());
        }

        // Written through the store's ONE writer of a modified pool, and reported through the
        // ONE projection every reader of a pool reports it by
        // ([`crate::state::reported_ipam_pool`]), so what this answers is the stored record a
        // later `DescribeIpamPools` of the same pool reports.
        let Some(pool) = store.modify_ipam_pool(&modification) else {
            // Unreachable: the pool was resolved above and the store is locked throughout.
            return Err(state::internal_error(format!(
                "the IPAM pool '{pool_id}' could not be read back after its modification"
            )));
        };
        Ok(ModifyIpamPoolOutput {
            ipam_pool: Some(state::reported_ipam_pool(pool)),
        })
    }
}
