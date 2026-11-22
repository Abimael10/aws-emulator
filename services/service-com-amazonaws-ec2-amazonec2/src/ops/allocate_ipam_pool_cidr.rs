// SPDX-License-Identifier: CC0-1.0

//! @op AllocateIpamPoolCidr
//!
//! Allocates ONE range out of the space an IPAM pool holds: "in IPAM, an allocation is a CIDR
//! assignment from an IPAM pool to another IPAM pool or to a resource"
//! ([`IpamPoolAllocation`]), and the assignment this call makes is held by the caller itself,
//! which is the catalog's `custom` resource type.
//!
//! The range comes out of the CIDRs provisioned to the pool
//! ([`crate::state::ComAmazonawsEc2Amazonec2State::ipam_pool_provisioned_cidrs`]) less
//! everything already assigned out of it
//! ([`crate::state::ComAmazonawsEc2Amazonec2State::ipam_pool_allocated_cidrs`]) — so "the
//! returned CIDR will not overlap with any other allocations from the same pool", and a
//! request naming a SIZE rather than a range is given the first free block of that size
//! through the crate's one search for one ([`crate::state::first_free_cidr_block`]).
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
//! # Modeled contract for `AllocateIpamPoolCidr`
//!
//! What AWS documents for this operation, from this service's Smithy model. HTML is
//! stripped and long entries are clipped at ` …`. For the FULL text of any one item,
//! ask the model rather than reading the raw JSON:
//!
//! ```text
//! smithy_query(query="doc",shape="<Shape>",member="<Member>",model_path="services/service-com-amazonaws-ec2-amazonec2/smithy-model/com-amazonaws-ec2-amazonec2.json")
//! ```
//!
//! Allocate a CIDR from an IPAM pool. The Region you use should be the IPAM pool locale.
//! The locale is the Amazon Web Services Region where this IPAM pool is available for
//! allocations. In IPAM, an allocation is a CIDR assignment from an IPAM pool to another
//! IPAM pool or to a resource. For more information, see Allocate CIDRs in the Amazon VPC
//! IPAM User Guide. This action creates an allocation with strong consistency. The returned
//! CIDR will not overlap with any other allocations from the same pool.
//!
//! ## Input
//!
//! * `AllowedCidrs` (IpamPoolAllocationAllowedCidrs) — Include a particular CIDR range that
//!   can be returned by the pool. Allowed CIDRs are only allowed if using netmask length for
//!   allocation.
//! * `Cidr` (String) — The CIDR you would like to allocate from the IPAM pool. Note the
//!   following: If there is no DefaultNetmaskLength allocation rule set on the pool, you must
//!   specify either the NetmaskLength or the CIDR. If the DefaultNetmaskLength allocation
//!   rule is set on the pool, you can specify either the NetmaskLength or the CIDR and the
//!   DefaultNetmaskLength allocation rule will be ignored. Possible values: Any available
//!   IPv4 or IPv6 CIDR.
//! * `ClientToken` (String, @idempotencyToken) — A unique, case-sensitive identifier that you
//!   provide to ensure the idempotency of the request. For more information, see Ensuring
//!   idempotency.
//! * `Description` (String) — A description for the allocation.
//! * `DisallowedCidrs` (IpamPoolAllocationDisallowedCidrs) — Exclude a particular CIDR range
//!   from being returned by the pool. Disallowed CIDRs are only allowed if using netmask
//!   length for allocation.
//! * `DryRun` (Boolean) — A check for whether you have the required permissions for the
//!   action without actually making the request and provides an error response. If you have
//!   the required permissions, the error response is DryRunOperation. Otherwise, it is
//!   UnauthorizedOperation.
//! * `IpamPoolId` (IpamPoolId) **required** — The ID of the IPAM pool from which you would
//!   like to allocate a CIDR.
//! * `NetmaskLength` (Integer) — The netmask length of the CIDR you would like to allocate
//!   from the IPAM pool. Note the following: If there is no DefaultNetmaskLength allocation
//!   rule set on the pool, you must specify either the NetmaskLength or the CIDR. If the
//!   DefaultNetmaskLength allocation rule is set on the pool, you can specify either the
//!   NetmaskLength or the CIDR and the DefaultNetmaskLength allocation rule will be ignored.
//!   Possible netmask lengths for IPv4 addresses are 0 - 32. Possible netmask lengths for
//!   IPv6 addresses are 0 - 128.
//! * `PreviewNextCidr` (Boolean) — A preview of the next available CIDR in a pool.
//! * `TagSpecifications` (TagSpecificationList) — The key/value combination of a tag assigned
//!   to the resource. Use the tag key in the filter name and the tag value as the filter
//!   value. For example, to find all resources that have a tag with the key Owner and the
//!   value TeamA, specify tag:Owner for the filter name and TeamA for the filter value. If
//!   you specify tags, the request is authorized against the allocation resource in addition
//!   to the pool resource.
//!
//! ## Output
//!
//! * `IpamPoolAllocation` (IpamPoolAllocation) — Information about the allocation created.

#[allow(unused_imports)]
use super::types::*;

// `#[operation_error]` below expands to an invocation of the LOCAL declarative
// `smithy!` macro, which this line is what emits into the module. Without it the
// expansion fails with `cannot find macro 'smithy' in this scope` and the
// `AllocateIpamPoolCidrError` enum is never produced.
smithy_macros::smithy_model!("smithy-model/com-amazonaws-ec2-amazonec2.json");

// The operation's MODELED errors. `#[operation_error]` accepts only variants the
// model declares for this operation, so the scaffold's `NotImplemented` cannot live
// here — hence the `AllocateIpamPoolCidrError` alias below. An operation that models no errors
// generates an UNINHABITED enum here, which is correct and expected: there is nothing
// to construct until the model says otherwise.
#[smithy_macros::operation_error("com.amazonaws.ec2#AllocateIpamPoolCidr")]
#[derive(Debug)]
pub enum AllocateIpamPoolCidrErrorModeled {
}

/// This operation's error channel, as the generated `impl Operation` names it
/// (`crate::ops::allocate_ipam_pool_cidr::AllocateIpamPoolCidrError`). Wrapping the modeled enum is what
/// lets a scaffolded operation answer 501 for one whose model declares no errors at all.
pub type AllocateIpamPoolCidrError = crate::scaffolded::Scaffolded<AllocateIpamPoolCidrErrorModeled>;

/// Lets `?` and `.into()` lift a modeled failure into the error channel, so an
/// implemented handler writes `Err(SomeModeledError.into())` rather than naming the
/// wrapper variant. See `crate::scaffolded` for how to drop the wrapper entirely once
/// this operation is implemented.
impl From<AllocateIpamPoolCidrErrorModeled> for AllocateIpamPoolCidrError {
    fn from(modeled: AllocateIpamPoolCidrErrorModeled) -> Self {
        crate::scaffolded::Scaffolded::Modeled(modeled)
    }
}

/// The `AllocateIpamPoolCidr` operation.
///
/// Owns the shared engine (the constructor seam) and implements
/// [`smithy_server_runtime::Operation`]. Generic over the engine type `Eng`, so this
/// crate names no concrete engine type: a migrated service that needs state picks a
/// concrete engine where it mounts the service, not here.
pub struct AllocateIpamPoolCidr<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> AllocateIpamPoolCidr<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> smithy_server_runtime::Operation
    for AllocateIpamPoolCidr<Eng>
{
    type Input = AllocateIpamPoolCidrInput;
    type Output = AllocateIpamPoolCidrOutput;
    type Error = AllocateIpamPoolCidrError;

    const NAME: &'static str = "AllocateIpamPoolCidr";
    const INPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::ALLOCATE_IPAM_POOL_CIDR_INPUT_SCHEMA;
    const OUTPUT_SCHEMA: &'static ::aws_smithy_schema::Schema = super::types::ALLOCATE_IPAM_POOL_CIDR_OUTPUT_SCHEMA;
    const SUCCESS_STATUS: u16 = 200u16;

    async fn handle(
        &self,
        input: Self::Input,
        ctx: &smithy_server_runtime::OperationContext<'_>,
    ) -> Result<Self::Output, Self::Error> {
        use crate::state;

        // `ctx` carries request metadata only; this operation reads none of it — "the Region
        // you use should be the IPAM pool locale", which the POOL carries, so the pool the
        // request names is what decides where the range comes from. The store is reached
        // through the engine held on `self`.
        let _ = ctx;

        // "If the DefaultNetmaskLength allocation rule is set on the pool, you can specify
        // EITHER the NetmaskLength OR the CIDR" (`$Cidr`): a request naming both says two
        // different things about the range it wants.
        //
        // Asked BEFORE the form of the pool id below, the way
        // [`crate::ops::provision_ipam_pool_cidr`] asks the same question of the same pair: a
        // combination of members the request may not carry is a fact about the REQUEST, which
        // EC2 settles before it reads any id the request names.
        let requested_cidr = input.cidr.as_deref().filter(|cidr| !cidr.is_empty());
        if requested_cidr.is_some() && input.netmask_length.is_some() {
            return Err(state::invalid_parameter_combination(
                "Only one of 'cidr' or 'netmaskLength' may be provided",
            ));
        }

        // The form of the id the request names, before anything is resolved and before the
        // DryRun arm below: "not an IPAM pool id at all" is a malformed request rather than a
        // pool that does not exist, which is the order every resource of this crate asks the
        // two questions in.
        let pool_id = input.ipam_pool_id.as_str();
        state::require_well_formed_id(
            state::IPAM_POOL_ID_PREFIX,
            pool_id,
            state::INVALID_IPAM_POOL_ID_MALFORMED,
        )?;

        // "Allowed CIDRs are only allowed if using netmask length for allocation"
        // (`$AllowedCidrs`), and the same of `$DisallowedCidrs`: both lists steer the CHOICE
        // of a range, so a request that names the range itself leaves nothing for them to
        // steer. Their length is bounded here, before any work proportional to it is done,
        // because the model bounds neither list.
        let requested_allowed = input.allowed_cidrs.as_deref().unwrap_or_default();
        let requested_disallowed = input.disallowed_cidrs.as_deref().unwrap_or_default();
        for (member, ranges) in [
            ("allowedCidrs", requested_allowed),
            ("disallowedCidrs", requested_disallowed),
        ] {
            if ranges.is_empty() {
                continue;
            }
            if requested_cidr.is_some() {
                return Err(state::invalid_parameter_combination(format!(
                    "{member} may be specified only when netmaskLength is provided"
                )));
            }
            if ranges.len() > MAX_ALLOCATION_CIDR_RULES {
                return Err(state::invalid_parameter_value(format!(
                    "The request carries {count} {member} entries, above the maximum of \
                     {MAX_ALLOCATION_CIDR_RULES}",
                    count = ranges.len(),
                )));
            }
        }

        // "A unique, case-sensitive identifier that you provide to ensure the idempotency of
        // the request" (`$ClientToken`): bounded the way every other caller-chosen token of
        // this crate is, since nothing reclaims the bytes it would retain.
        if input
            .client_token
            .as_ref()
            .is_some_and(|token| token.chars().count() > state::MAX_CLIENT_TOKEN_CHARS)
        {
            return Err(state::invalid_parameter_value(format!(
                "clientToken accepts at most {max} characters",
                max = state::MAX_CLIENT_TOKEN_CHARS,
            )));
        }

        // "The key/value combination of a tag assigned to the resource"
        // (`$TagSpecifications`): the allocation is the only resource this call creates, so a
        // specification naming any other resource type is refused rather than accepted and
        // discarded — by the one helper every create in this crate bounds its tag payload
        // with.
        let tags = state::tags_from_specifications(
            input.tag_specifications.as_deref(),
            state::RESOURCE_TYPE_IPAM_POOL_ALLOCATION,
            Self::NAME,
        )?;

        // "A check for whether you have the required permissions for the action WITHOUT
        // ACTUALLY MAKING THE REQUEST" (`$DryRun`): a question about the CALLER, answered once
        // the request's own form has been checked and before anything is resolved or written.
        // Every caller of this emulator is permitted.
        if input.dry_run.unwrap_or(false) {
            return Err(state::dry_run_operation());
        }

        let state_handle = state::require_service_state(&*self.engine)?;
        let mut store = state_handle.lock();
        // The clock first, the way every other state-touching handler in this crate starts: a
        // pool whose `create-complete` move has fallen due is already in that state, and so is
        // a CIDR whose own provision has settled, so the space this call draws on is the space
        // the pool ACTUALLY holds.
        store.materialize_due_transitions();

        // "The ID of the IPAM pool from which you would like to allocate a CIDR"
        // (`$IpamPoolId`): an id this account holds no pool for is `.NotFound`, and a pool on
        // its way out is not one a range can be taken out of.
        let pool = store.ipam_pools.get(pool_id).cloned().ok_or_else(|| {
            state::id_not_found(
                state::INVALID_IPAM_POOL_ID_NOT_FOUND,
                state::IPAM_POOL_ID_KIND,
                pool_id,
            )
        })?;
        if state::ipam_pool_is_deleting(&pool) {
            return Err(state::incorrect_state_of(
                "IPAM pool",
                pool_id,
                pool.state.as_deref().unwrap_or_default(),
            ));
        }

        // "You must choose either IPv4 or IPv6 protocol for a pool"
        // (`CreateIpamPoolRequest$AddressFamily`): every range of this call — the one it may
        // be given and the ones it is steered by — is read in the family the POOL holds, so a
        // range of the other family names no space this pool could hand out.
        let family = pool.address_family.as_deref().unwrap_or_default();
        // "Possible netmask lengths for IPv4 addresses are 0 - 32. Possible netmask lengths
        // for IPv6 addresses are 0 - 128" (`$NetmaskLength`), through the ONE reader of that
        // bound in this crate.
        state::require_netmask_lengths_for_family(
            family,
            &[("netmaskLength", input.netmask_length)],
        )?;
        let canonical_requested = match requested_cidr {
            Some(cidr) => Some(canonical_in_family(cidr, family, "cidr")?),
            None => None,
        };
        let allowed = canonical_rules(requested_allowed, family, "allowedCidrs")?;
        let disallowed = canonical_rules(requested_disallowed, family, "disallowedCidrs")?;

        // Idempotency: one token names the one allocation it minted, so a replay reports that
        // allocation and hands out no second range.
        if let Some(allocation_id) = input
            .client_token
            .as_deref()
            .and_then(|token| store.ipam_pool_allocation_client_tokens.get(token).cloned())
        {
            let held = store
                .ipam_pool_allocations
                .get(&allocation_id)
                .ok_or_else(|| {
                    state::internal_error(format!(
                        "a client token names IPAM pool allocation '{allocation_id}', which is \
                         not stored"
                    ))
                })?;
            // A token is only idempotent for the REQUEST it was first given with: replayed
            // against another pool, or with different parameters, it names an allocation the
            // caller did not ask for — which EC2 refuses rather than answers.
            if held.ipam_pool_id != pool_id
                || !same_request(&held.view, canonical_requested.as_deref(), &input, &tags)
            {
                return Err(state::idempotent_parameter_mismatch());
            }
            return Ok(AllocateIpamPoolCidrOutput {
                ipam_pool_allocation: Some(held.view.clone()),
            });
        }

        // The space the pool holds and what is already assigned out of it are read by the
        // store's own helpers below, each through the ONE reader of that question, so this
        // call, a provision to a pool within this one, a VPC taking its CIDR out of this pool
        // and the dependency `DeleteIpamPool` is refused for all read one answer.
        let cidr = match canonical_requested {
            // "The CIDR you would like to allocate from the IPAM pool" (`$Cidr`): a range
            // outside everything provisioned to the pool is space the pool does not hold, and
            // one meeting a range already assigned would hand the same addresses out twice.
            Some(canonical) => {
                state::require_compliant_allocation_cidr(
                    &pool,
                    pool_id,
                    "netmaskLength",
                    &canonical,
                )?;
                store.require_ipam_pool_space(pool_id, "cidr", &canonical)?;
                canonical
            }
            // "If there is no DefaultNetmaskLength allocation rule set on the pool, you must
            // specify either the NetmaskLength or the CIDR" (`$NetmaskLength`): a request
            // naming neither asks for a range of no size at all, unless the pool itself says
            // what size "new allocations will default to"
            // (`IpamPool$AllocationDefaultNetmaskLength`).
            None => {
                let netmask_length = input
                    .netmask_length
                    .or(pool.allocation_default_netmask_length)
                    .ok_or_else(|| {
                        state::missing_parameter(
                            "Either 'cidr' or 'netmaskLength' must be provided",
                        )
                    })?;
                state::require_compliant_allocation_length(
                    &pool,
                    pool_id,
                    "netmaskLength",
                    netmask_length,
                )?;
                store.choose_free_ipam_pool_cidr(
                    pool_id,
                    "netmaskLength",
                    u32::try_from(netmask_length).unwrap_or(u32::MAX),
                    &allowed,
                    &disallowed,
                )?
            }
        };

        // "Information about the allocation created" (`$IpamPoolAllocation`), as the store
        // holds it: the range, the description the caller gave it, and what holds it — which
        // this call assigns to no resource of the account, so the `custom` member of the
        // `IpamPoolAllocationResourceType` catalog is what it carries, in the Region the pool
        // allocates in and for the account that owns the pool.
        let view = IpamPoolAllocation {
            cidr: Some(cidr.clone()),
            // Minted by the store's one writer of an allocation.
            ipam_pool_allocation_id: None,
            description: input.description.clone(),
            // "The ID of the resource" (`$ResourceId`): no resource of the account holds this
            // range, so there is no id to report for one.
            resource_id: None,
            resource_type: Some(state::IPAM_POOL_ALLOCATION_RESOURCE_TYPE_CUSTOM.to_string()),
            resource_region: pool.locale.clone().or_else(|| pool.ipam_region.clone()),
            resource_owner: pool.owner_id.clone(),
            tags: Some(tags),
        };

        // "A preview of the next available CIDR in a pool" (`$PreviewNextCidr`): a question
        // about what the pool WOULD hand out, so nothing is stored and no id is minted — a
        // later caller sees exactly the pool this one saw, and the next allocation is free to
        // take the previewed range.
        if input.preview_next_cidr.unwrap_or(false) {
            return Ok(AllocateIpamPoolCidrOutput {
                ipam_pool_allocation: Some(view),
            });
        }

        // Stored through the one writer of this store, which mints the id and records the
        // `ClientToken` that named it; `None` means the store is at a ceiling and NOTHING was
        // stored.
        let stored = store
            .allocate_ipam_pool_cidr(pool_id, input.client_token.as_deref(), view)
            .ok_or_else(|| {
                state::resource_limit_exceeded(format!(
                    "This emulator holds {max} IPAM pool allocations, which is as many as it \
                     stores",
                    max = state::MAX_STORED_RECORDS_PER_STORE,
                ))
            })?;

        Ok(AllocateIpamPoolCidrOutput {
            ipam_pool_allocation: Some(stored),
        })
    }
}

/// How many `AllowedCidrs` or `DisallowedCidrs` entries one request may carry.
///
/// A bound on the WORK one request can ask for, not an AWS quota: the search below compares
/// every candidate block against every entry, and the model bounds neither list. It is the
/// number of values one EC2 filter admits ([`crate::state::MAX_FILTER_VALUES`]), so a request
/// that steers a choice is bounded exactly as one that filters a read is.
const MAX_ALLOCATION_CIDR_RULES: usize = crate::state::MAX_FILTER_VALUES;

/// `value` as the pool's address family spells a range, with its host bits cleared — or the
/// refusal for a range that is not one of that family at all.
///
/// The stored and compared form is the canonical one, so one range cannot be spelled two ways
/// in one pool. `member` is the request member the range came from, so a refusal says which
/// one is at fault.
fn canonical_in_family(
    value: &str,
    family: &str,
    member: &str,
) -> Result<String, AllocateIpamPoolCidrError> {
    let canonical = match family {
        crate::state::ADDRESS_FAMILY_IPV6 => crate::state::canonical_ipv6_cidr(value),
        _ => crate::state::canonical_ipv4_cidr(value),
    };
    canonical.map_err(|_| {
        crate::state::invalid_parameter_value(format!(
            "Invalid value '{value}' for {member}. It must be an {family} CIDR block"
        ))
    })
}

/// Every range of one steering list, read in the pool's own family through
/// [`canonical_in_family`].
fn canonical_rules(
    requested: &[String],
    family: &str,
    member: &str,
) -> Result<Vec<String>, AllocateIpamPoolCidrError> {
    requested
        .iter()
        .map(|value| canonical_in_family(value, family, member))
        .collect()
}

/// Does `held` — the allocation a replayed `ClientToken` names — hold what THIS request asks
/// for?
///
/// Every member the request decides is compared, so a token replayed with any different
/// parameter is refused with `IdempotentParameterMismatch` rather than answered with an
/// allocation the caller did not ask for. A request that named a SIZE is compared against the
/// size of the range the allocation holds, which is what that request asked for; the POOL the
/// request names is compared by the caller, against the pool the record belongs to; and the
/// members this service derives (the id, the resource type, the Region, the owner) are not
/// compared: they follow from the pool and from the ones that are.
fn same_request(
    held: &IpamPoolAllocation,
    canonical_requested: Option<&str>,
    input: &AllocateIpamPoolCidrInput,
    tags: &[Tag],
) -> bool {
    let tag_pairs = |tags: &[Tag]| {
        let mut pairs: Vec<(Option<String>, Option<String>)> = tags
            .iter()
            .map(|tag| (tag.key.clone(), tag.value.clone()))
            .collect();
        pairs.sort();
        pairs
    };
    let held_length = held
        .cidr
        .as_deref()
        .and_then(crate::state::cidr_block_prefix_length)
        .and_then(|length| i32::try_from(length).ok());
    held.description == input.description
        && canonical_requested.is_none_or(|cidr| held.cidr.as_deref() == Some(cidr))
        && input
            .netmask_length
            .is_none_or(|length| held_length == Some(length))
        && tag_pairs(held.tags.as_deref().unwrap_or_default()) == tag_pairs(tags)
}
