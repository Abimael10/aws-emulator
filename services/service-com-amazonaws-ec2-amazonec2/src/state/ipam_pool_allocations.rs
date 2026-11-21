// SPDX-License-Identifier: CC0-1.0

//! The CIDRs ALLOCATED out of an IPAM pool: the store `AllocateIpamPoolCidr` writes and
//! `GetIpamPoolAllocations` reports, and the one search for a free block inside a pool's
//! space.
//!
//! "In IPAM, an allocation is a CIDR assignment from an IPAM pool to another IPAM pool or to
//! a resource" ([`IpamPoolAllocation`]): an allocation is a resource of its own — it carries
//! an id, the range it assigns and what that range was assigned TO — so it is held in a store
//! of its own, keyed by that id, rather than inside the pool's modelled shape, which has no
//! member for it. The pool the range came OUT of is held beside the view for the same reason.
//!
//! Both kinds of assignment the shape's own sentence names are in this store, so one reading
//! answers "what space of this pool is already assigned"
//! ([`ComAmazonawsEc2Amazonec2State::ipam_pool_allocated_cidrs`]): the allocation a caller
//! asked for with `AllocateIpamPoolCidr`, and the one a pool WITHIN this pool holds for a
//! range provisioned to it out of this pool's space
//! ([`ComAmazonawsEc2Amazonec2State::provision_ipam_pool_cidr`]). That makes the space one
//! call hands out a function of the store rather than of the call, so no two calls can hand
//! out overlapping space.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.

use super::*;

/// The id prefix an IPAM pool allocation is minted with, which is also how its id reads on
/// the wire (`IpamPoolAllocation$IpamPoolAllocationId`).
pub const IPAM_POOL_ALLOCATION_ID_PREFIX: &str = "ipam-pool-alloc";

/// `InvalidIpamPoolAllocationId.NotFound` (HTTP 400): the request named an allocation id the
/// pool holds no allocation for.
pub const INVALID_IPAM_POOL_ALLOCATION_ID_NOT_FOUND: &str =
    "InvalidIpamPoolAllocationId.NotFound";

/// `InvalidIpamPoolAllocationId.Malformed` (HTTP 400): the request named something that is
/// not an allocation id at all, so there is nothing to look up.
pub const INVALID_IPAM_POOL_ALLOCATION_ID_MALFORMED: &str =
    "InvalidIpamPoolAllocationId.Malformed";

/// How an allocation of this kind is NAMED in a refusal that cannot resolve its id: the
/// resource type, as EC2 words every such sentence ([`id_not_found_message`]).
pub const IPAM_POOL_ALLOCATION_ID_KIND: &str = "ipam-pool-allocation ID";

/// The `ResourceType` member an IPAM pool allocation is tagged under
/// (`smithy_query(query="shape",shape="ResourceType")`), which is the only type an
/// `AllocateIpamPoolCidr` `TagSpecifications` entry may name.
pub const RESOURCE_TYPE_IPAM_POOL_ALLOCATION: &str = "ipam-pool-allocation";

/// The `IpamPoolAllocationResourceType` member an allocation a CALLER asked for carries:
/// `custom` (`smithy_query(query="shape",shape="IpamPoolAllocationResourceType")`).
///
/// `AllocateIpamPoolCidr` assigns a range to NO resource of the account — "an allocation is a
/// CIDR assignment from an IPAM pool to another IPAM pool or to a resource"
/// ([`IpamPoolAllocation`]) and this call names neither — so the range is held by the caller
/// itself, which is what the catalog's `custom` member reports.
pub const IPAM_POOL_ALLOCATION_RESOURCE_TYPE_CUSTOM: &str = "custom";

/// The `IpamPoolAllocationResourceType` member the allocation of a pool WITHIN this pool
/// carries: `ipam-pool`, the first of the two assignments [`IpamPoolAllocation`]'s own
/// sentence names.
pub const IPAM_POOL_ALLOCATION_RESOURCE_TYPE_IPAM_POOL: &str = "ipam-pool";

/// The `IpamPoolAllocationResourceType` member the allocation a VPC holds carries: `vpc`
/// (`smithy_query(query="shape",shape="IpamPoolAllocationResourceType")`).
///
/// The second of the two assignments [`IpamPoolAllocation`]'s own sentence names — "to a
/// RESOURCE" — for a VPC whose CIDR block was allocated out of the pool
/// (`CreateVpcRequest$Ipv4IpamPoolId`).
pub const IPAM_POOL_ALLOCATION_RESOURCE_TYPE_VPC: &str = "vpc";

/// One CIDR allocated out of one IPAM pool: the modelled [`IpamPoolAllocation`] every
/// operation that reports one answers with, and the pool the range came out of.
///
/// The pool id is held beside the view rather than on it for the reason the module docs give:
/// the modelled shape carries no member for it.
#[derive(Debug, Clone)]
pub struct IpamPoolAllocationRecord {
    /// The `IpamPoolId` of the pool this range was allocated out of.
    pub ipam_pool_id: String,
    /// The allocation as every reader of it reports it.
    pub view: IpamPoolAllocation,
}

/// The caller-chosen bytes one stored allocation holds: the range, the description and the
/// tags, which is all the text the caller chose.
///
/// The fixed [`STORED_RECORD_OVERHEAD_BYTES`] every stored record costs is charged on top,
/// exactly as [`ipam_pool_cidr_payload_bytes`] is charged for a provisioned range.
pub fn ipam_pool_allocation_payload_bytes(view: &IpamPoolAllocation) -> usize {
    STORED_RECORD_OVERHEAD_BYTES
        .saturating_add(view.cidr.as_deref().map_or(0, str::len))
        .saturating_add(view.description.as_deref().map_or(0, str::len))
        .saturating_add(tag_list_bytes(view.tags.as_deref().unwrap_or_default()))
}

impl ComAmazonawsEc2Amazonec2State {
    /// Whether another IPAM pool allocation fits inside the store's ceiling
    /// ([`MAX_STORED_RECORDS_PER_STORE`]) — the same contract as
    /// [`Self::has_ipam_pool_cidr_capacity`]: a writer asks first and refuses when it answers
    /// `false`, so repeated allocations cannot grow memory without bound.
    pub fn has_ipam_pool_allocation_capacity(&self) -> bool {
        self.ipam_pool_allocations.len() < MAX_STORED_RECORDS_PER_STORE
    }

    /// The allocations made out of the pool `pool_id` names, as the records the store holds,
    /// in the one order every reader of them reports: the order of their ids.
    pub fn ipam_pool_allocation_records(&self, pool_id: &str) -> Vec<&IpamPoolAllocationRecord> {
        self.ipam_pool_allocations
            .values()
            .filter(|record| record.ipam_pool_id == pool_id)
            .collect()
    }

    /// The ranges of the pool `pool_id` names that are already ASSIGNED — the space no
    /// further call may hand out.
    ///
    /// The ONE reader of that question, so the handler that allocates a range out of a pool
    /// and the one that provisions a range to a pool within it cannot disagree about which
    /// space is free: both kinds of assignment the module docs name are rows of this store.
    pub fn ipam_pool_allocated_cidrs(&self, pool_id: &str) -> Vec<String> {
        self.ipam_pool_allocation_records(pool_id)
            .into_iter()
            .filter_map(|record| record.view.cidr.clone())
            .collect()
    }

    /// Hold the range `cidr` to the SPACE the pool `pool_id` has to hand out: it has to sit
    /// inside something provisioned to the pool, and may overlap nothing already assigned out
    /// of it.
    ///
    /// "The CIDR you would like to allocate from the IPAM pool"
    /// (`AllocateIpamPoolCidrRequest$Cidr`): a range outside everything provisioned to the pool
    /// is space the pool does not hold, and one meeting a range already assigned would hand the
    /// same addresses out twice. `member` is the request member the range came from, so the
    /// refusal says which one is at fault — the ONE reading of that question, so a caller
    /// naming the range of an allocation and one naming the range of a VPC are held to the same
    /// space.
    pub fn require_ipam_pool_space<E>(
        &self,
        pool_id: &str,
        member: &str,
        cidr: &str,
    ) -> Result<(), Scaffolded<E>> {
        if !self
            .ipam_pool_provisioned_cidrs(pool_id)
            .iter()
            .any(|held| cidr_block_contains_block(held, cidr))
        {
            return Err(invalid_parameter_value(format!(
                "Invalid value '{cidr}' for {member}. It is not within a CIDR provisioned to \
                 the IPAM pool '{pool_id}'"
            )));
        }
        if let Some(held) = self
            .ipam_pool_allocated_cidrs(pool_id)
            .into_iter()
            .find(|held| cidr_blocks_overlap(held, cidr))
        {
            return Err(invalid_parameter_value(format!(
                "Invalid value '{cidr}' for {member}. It overlaps with the CIDR '{held}' \
                 already allocated from the IPAM pool '{pool_id}'"
            )));
        }
        Ok(())
    }

    /// The first block of `netmask_length` bits the pool `pool_id` has free to hand out, inside
    /// `allowed` and outside `disallowed` — or the refusal the request earns.
    ///
    /// The space and what is already assigned out of it come from the ONE reader of each
    /// ([`Self::ipam_pool_provisioned_cidrs`] / [`Self::ipam_pool_allocated_cidrs`]), and the
    /// choice from the ONE search ([`first_free_cidr_block`]), so no two callers can hand out
    /// overlapping space. `member` is the request member the SIZE came from, so the refusal
    /// says which one could not be served. Nothing is written: the caller records the
    /// allocation that holds the range.
    pub fn choose_free_ipam_pool_cidr<E>(
        &self,
        pool_id: &str,
        member: &str,
        netmask_length: u32,
        allowed: &[String],
        disallowed: &[String],
    ) -> Result<String, Scaffolded<E>> {
        let space = self.ipam_pool_provisioned_cidrs(pool_id);
        let taken = self.ipam_pool_allocated_cidrs(pool_id);
        let mut budget = MatchBudget::new(MAX_FILTER_MATCH_STEPS);
        first_free_cidr_block(
            &FreeCidrBlockSearch {
                space: &space,
                netmask_length,
                taken: &taken,
                allowed,
                disallowed,
            },
            &mut budget,
        )
        .map_err(|_| {
            // An INCOMPLETE search is not something to report as a result: the request asked
            // for more work than one call will do, and is told so rather than given a range
            // the search had not finished clearing.
            Scaffolded::request_limit_exceeded(format!(
                "Choosing a CIDR for this pool would take more than {MAX_FILTER_MATCH_STEPS} \
                 comparisons; allocate the cidr itself instead"
            ))
        })?
        .ok_or_else(|| {
            invalid_parameter_value(format!(
                "Invalid value '{netmask_length}' for {member}. The IPAM pool '{pool_id}' has \
                 no free CIDR of that size"
            ))
        })
    }

    /// Allocate `view` out of the pool `pool_id` names and answer the allocation as the store
    /// then holds it — `None` when the store is at a ceiling and NOTHING was stored.
    ///
    /// The ONE way an allocation is stored, so the member the SERVICE owns cannot be built two
    /// ways: the `IpamPoolAllocationId` is minted here, the way
    /// [`Self::provision_ipam_pool_cidr`] mints a provisioned range's id. An allocation has no
    /// state member of its own, so there is no clock-owed move to record for one: a pool
    /// reports the range as assigned from this call onwards.
    ///
    /// `client_token` names the allocation from here on, so a replay of the same request
    /// answers it instead of handing out a second range; the bytes that token retains are
    /// charged to the store's budget, the way [`Self::insert_creating_ipam_pool`] charges a
    /// pool's.
    pub fn allocate_ipam_pool_cidr(
        &mut self,
        pool_id: &str,
        client_token: Option<&str>,
        view: IpamPoolAllocation,
    ) -> Option<IpamPoolAllocation> {
        let charge = ipam_pool_allocation_payload_bytes(&view)
            .saturating_add(client_token.unwrap_or_default().len());
        if !self.has_ipam_pool_allocation_capacity() || self.would_exceed_stored_bytes(charge) {
            return None;
        }
        let allocation_id = self.next_id(IPAM_POOL_ALLOCATION_ID_PREFIX);
        let mut view = view;
        view.ipam_pool_allocation_id = Some(allocation_id.clone());
        self.add_stored_bytes(charge);
        self.ipam_pool_allocations.insert(
            allocation_id.clone(),
            IpamPoolAllocationRecord {
                ipam_pool_id: pool_id.to_string(),
                view: view.clone(),
            },
        );
        if let Some(token) = client_token {
            self.ipam_pool_allocation_client_tokens
                .insert(token.to_string(), allocation_id);
        }
        Some(view)
    }

    /// Release every allocation the pool `pool_id` names holds, and answer how many went.
    ///
    /// "Enables you to quickly delete an IPAM pool and all resources within that pool,
    /// including provisioned CIDRs, ALLOCATIONS, and other pools"
    /// (`DeleteIpamPoolRequest$Cascade`): a released allocation stops holding its range at
    /// once — the range is free space of the pool again — so the record goes rather than being
    /// parked in a state, which the modelled shape carries no member for anyway. The bytes it
    /// held go back to the store's budget.
    pub fn release_ipam_pool_allocations(&mut self, pool_id: &str) -> usize {
        let doomed: Vec<String> = self
            .ipam_pool_allocations
            .iter()
            .filter(|(_, record)| record.ipam_pool_id == pool_id)
            .map(|(allocation_id, _)| allocation_id.clone())
            .collect();
        let released = doomed.len();
        for allocation_id in doomed {
            self.remove_ipam_pool_allocation(&allocation_id);
        }
        released
    }

    /// Release every allocation held by the RESOURCE `resource_id` — the space a resource of    /// the account holds out of an IPAM pool — and answer how many went.
    ///
    /// "In IPAM, an allocation is a CIDR assignment from an IPAM pool to another IPAM pool or
    /// to a resource" ([`IpamPoolAllocation`]): an assignment to a resource lasts exactly as
    /// long as that resource, so the resource's own removal gives the space back and the pool
    /// may hand it out again. The counterpart of [`Self::release_ipam_pool_allocations`],
    /// which gives back the allocations OF a pool.
    pub fn release_ipam_pool_allocations_of_resource(&mut self, resource_id: &str) -> usize {
        let doomed: Vec<String> = self
            .ipam_pool_allocations
            .iter()
            .filter(|(_, record)| record.view.resource_id.as_deref() == Some(resource_id))
            .map(|(allocation_id, _)| allocation_id.clone())
            .collect();
        let released = doomed.len();
        for allocation_id in doomed {
            self.remove_ipam_pool_allocation(&allocation_id);
        }
        released
    }

    /// Release the allocation the pool `pool_id` holds for the range `cidr` assigned to the
    /// resource `resource_id`, if it holds one.
    ///
    /// What a range leaving a pool WITHIN this one gives back: the space stops being assigned
    /// at the instant the pool it was assigned to stops holding it
    /// ([`ipam_pool_cidr_holds_space`]), so the source pool can hand it out again — the
    /// counterpart of the allocation [`Self::provision_ipam_pool_cidr`] made for it.
    pub(super) fn release_ipam_pool_allocation_of(
        &mut self,
        pool_id: &str,
        resource_id: &str,
        cidr: &str,
    ) {
        let doomed: Vec<String> = self
            .ipam_pool_allocations
            .iter()
            .filter(|(_, record)| {
                record.ipam_pool_id == pool_id
                    && record.view.resource_id.as_deref() == Some(resource_id)
                    && record.view.cidr.as_deref() == Some(cidr)
            })
            .map(|(allocation_id, _)| allocation_id.clone())
            .collect();
        for allocation_id in doomed {
            self.remove_ipam_pool_allocation(&allocation_id);
        }
    }

    /// Remove every allocation that NAMES the pool `pool_id` — the ones made out of it and
    /// the one another pool holds for a range assigned to it — giving the bytes they held back
    /// to the store's budget.
    ///
    /// A record names its pool and its resource for as long as it is stored, so a pool that
    /// LEAVES the store takes both with it rather than leaving rows naming a pool nothing
    /// resolves, exactly as [`Self::remove_ipam_pool_cidrs`] takes its ranges.
    pub(super) fn remove_ipam_pool_allocations_naming(&mut self, pool_id: &str) {
        let doomed: Vec<String> = self
            .ipam_pool_allocations
            .iter()
            .filter(|(_, record)| {
                record.ipam_pool_id == pool_id
                    || record.view.resource_id.as_deref() == Some(pool_id)
            })
            .map(|(allocation_id, _)| allocation_id.clone())
            .collect();
        for allocation_id in doomed {
            self.remove_ipam_pool_allocation(&allocation_id);
        }
    }

    /// Drop one allocation and give the bytes it held back to the store's budget — the one
    /// place a row of this store is removed, so no path can drop a record without refunding
    /// exactly what [`ipam_pool_allocation_payload_bytes`] charged for it.
    ///
    /// The `ClientToken` that minted it goes with it, for the reason every other IPAM record
    /// drops its tokens: a token left naming an allocation the store no longer holds would
    /// replay into a dangling id.
    fn remove_ipam_pool_allocation(&mut self, allocation_id: &str) {
        let doomed: Vec<String> = self
            .ipam_pool_allocation_client_tokens
            .iter()
            .filter(|(_, minted)| minted.as_str() == allocation_id)
            .map(|(token, _)| token.clone())
            .collect();
        let mut released = 0usize;
        for token in doomed {
            self.ipam_pool_allocation_client_tokens.remove(&token);
            released = released.saturating_add(token.len());
        }
        if let Some(record) = self.ipam_pool_allocations.remove(allocation_id) {
            released =
                released.saturating_add(ipam_pool_allocation_payload_bytes(&record.view));
        }
        self.release_stored_bytes(released);
    }
}

/// `length` held to the netmask rules the pool itself carries.
///
/// "The MINIMUM netmask length required for CIDR allocations in this IPAM pool to be
/// compliant" and "the MAXIMUM netmask length possible for CIDR allocations in this IPAM pool
/// to be compliant" (`IpamPool$AllocationMinNetmaskLength`,
/// `IpamPool$AllocationMaxNetmaskLength`) are the two bounds of the sizes a pool allocates, so
/// a request outside them asks for a range the pool does not hand out — refused before any
/// space is searched or taken, rather than stored as a non-compliant allocation.
///
/// `member` is the request member the length came from, so the refusal says which one is at
/// fault. The ONE reader of those rules, so every caller that takes space out of a pool is
/// held to the sizes that pool allocates.
pub fn require_compliant_allocation_length<E>(
    pool: &IpamPool,
    pool_id: &str,
    member: &str,
    length: i32,
) -> Result<(), Scaffolded<E>> {
    require_netmask_within_pool_rules(
        pool,
        length,
        |min| {
            format!(
                "Invalid value '{length}' for {member}. The IPAM pool '{pool_id}' allocates \
                 CIDRs with a netmask length of at least '{min}'"
            )
        },
        |max| {
            format!(
                "Invalid value '{length}' for {member}. The IPAM pool '{pool_id}' allocates \
                 CIDRs with a netmask length of at most '{max}'"
            )
        },
    )
}

/// `length` held to the two bounds a pool's allocation rules carry, refused in whichever
/// wording the CALLER of this reader answers them in.
///
/// The ONE implementation of the rule itself — "the MINIMUM netmask length required for CIDR
/// allocations in this IPAM pool to be compliant" and "the MAXIMUM netmask length possible"
/// (`IpamPool$AllocationMinNetmaskLength`, `IpamPool$AllocationMaxNetmaskLength`) — so the
/// two refusals EC2 words differently (an allocation call's and a resource creation's) cannot
/// come to different verdicts about the same pool and the same size. `too_large` is handed the
/// minimum a length SHORTER than it broke, `too_small` the maximum a longer one broke.
fn require_netmask_within_pool_rules<E>(
    pool: &IpamPool,
    length: i32,
    too_large: impl FnOnce(i32) -> String,
    too_small: impl FnOnce(i32) -> String,
) -> Result<(), Scaffolded<E>> {
    if let Some(min) = pool.allocation_min_netmask_length.filter(|min| length < *min) {
        return Err(invalid_parameter_value(too_large(min)));
    }
    if let Some(max) = pool.allocation_max_netmask_length.filter(|max| length > *max) {
        return Err(invalid_parameter_value(too_small(max)));
    }
    Ok(())
}

/// The netmask length `cidr` itself names, or `None` for a value that names none.
///
/// One reading of "how large is this range" for the two readers below, so a range the request
/// named is measured the same way whichever refusal it would be held to.
fn allocation_cidr_netmask_length(cidr: &str) -> Option<i32> {
    cidr_block_prefix_length(cidr).and_then(|length| i32::try_from(length).ok())
}

/// The netmask length `cidr` itself names, held to the pool's allocation rules through
/// [`require_compliant_allocation_length`].
///
/// A range the REQUEST named has a size of its own, so the rules the pool holds are asked of
/// that size rather than of a length the request did not carry.
pub fn require_compliant_allocation_cidr<E>(
    pool: &IpamPool,
    pool_id: &str,
    member: &str,
    cidr: &str,
) -> Result<(), Scaffolded<E>> {
    let Some(length) = allocation_cidr_netmask_length(cidr) else {
        return Ok(());
    };
    require_compliant_allocation_length(pool, pool_id, member, length)
}

/// `pool` held to being a pool a RESOURCE of `region` may take its range out of.
///
/// "The locale is the Amazon Web Services Region where this IPAM pool is available for
/// allocations" (`AllocateIpamPoolCidr`): a resource is created IN one Region, so the pool its
/// CIDR comes out of has to be available in that Region — one whose locale is another Region
/// is not, and neither is one that names no locale, which is available in no Region in
/// particular ("default is none and means any locale",
/// `CreateIpamPoolRequest$Locale`). Either way a resource of this Region cannot be given space
/// out of it, which EC2 refuses naming the resource type that is not valid for the pool and
/// the locale the pool reports ([`reported_ipam_pool`], so the sentence spells a pool without
/// one the way every read of it does).
///
/// This says nothing about what the pool itself may HOLD: a pool created without a locale
/// still takes space anywhere its IPAM operates ([`IPAM_POOL_LOCALE_NONE`] and the locale
/// readings beside it), and `AllocateIpamPoolCidr`, which assigns a range to no resource at
/// all, asks this of no pool. What a locale decides here is only whether a REGION-BOUND
/// RESOURCE can be the holder of one of the pool's ranges.
///
/// `resource_type` is the `IpamPoolAllocationResourceType` of the resource being created, so
/// the sentence names what the allocation would have been for. The ONE reader of that
/// question, so every path that takes space out of a pool FOR A RESOURCE is held to the
/// Region that pool allocates in.
pub fn require_ipam_pool_allocates_in<E>(
    pool: &IpamPool,
    pool_id: &str,
    resource_type: &str,
    region: &str,
) -> Result<(), Scaffolded<E>> {
    if pool.locale.as_deref() == Some(region) {
        return Ok(());
    }
    let locale = pool.locale.as_deref().unwrap_or(IPAM_POOL_LOCALE_NONE);
    Err(invalid_parameter_value(format!(
        "The resource type {resource_type} is not valid for pool {pool_id} with {locale} \
         locale."
    )))
}

/// The tags `pool` requires of the resources it allocates to, held against the `tags` the
/// resource is being created with.
///
/// "Tags that are required for resources that use CIDRs from this IPAM pool. Resources that do
/// not have these tags will not be allowed to allocate space from the pool"
/// (`IpamPool$AllocationResourceTags`): the required list is a SET of key/value pairs the
/// resource has to carry, so a resource missing any one of them is not one the pool allocates
/// to — refused before any space is taken rather than stored as a resource the pool's own
/// rules exclude.
///
/// The ONE reader of that question, for the same reason
/// [`require_netmask_within_pool_rules`] is the one reader of a pool's netmask rules.
///
/// The sentence states the rule the model's own member states, naming the required pair the
/// resource does not carry and the pool requiring it. Only the CODE of this refusal is
/// recorded for the emulator to reproduce (`InvalidParameterValue`), so the wording here is
/// this crate's statement of the modelled rule rather than reproduced EC2 text; every value in
/// it comes from the pool's stored rules and the request's own tags.
pub fn require_pool_allocation_resource_tags<E>(
    pool: &IpamPool,
    pool_id: &str,
    tags: &[Tag],
) -> Result<(), Scaffolded<E>> {
    let required = pool.allocation_resource_tags.as_deref().unwrap_or_default();
    let Some(missing) = required.iter().find(|required| {
        !tags.iter().any(|tag| {
            tag.key.as_deref().unwrap_or_default()
                == required.key.as_deref().unwrap_or_default()
                && tag.value.as_deref().unwrap_or_default()
                    == required.value.as_deref().unwrap_or_default()
        })
    }) else {
        return Ok(());
    };
    Err(invalid_parameter_value(format!(
        "The resource does not have the tag {key}={value}, which pool {pool_id} requires of \
         the resources that use CIDRs from it.",
        key = missing.key.as_deref().unwrap_or_default(),
        value = missing.value.as_deref().unwrap_or_default(),
    )))
}

/// `length` held to the netmask rules of a pool a RESOURCE takes its range out of, in the
/// wording EC2 answers such a resource's creation with.
///
/// The same two bounds [`require_compliant_allocation_length`] reads — a pool's
/// `AllocationMinNetmaskLength` and `AllocationMaxNetmaskLength` — asked on behalf of a
/// resource rather than of an allocation call: a netmask length SHORTER than the pool's
/// minimum asks for a LARGER block than the pool hands out, and one longer than its maximum
/// for a smaller one, so the sentence names the length asked for and the bound the pool
/// allows, each spelled as a netmask.
pub fn require_resource_allocation_netmask_length<E>(
    pool: &IpamPool,
    pool_id: &str,
    length: i32,
) -> Result<(), Scaffolded<E>> {
    require_netmask_within_pool_rules(
        pool,
        length,
        |min| {
            format!(
                "The requested netmask length /{length} is larger than allowed by pool \
                 {pool_id}. Largest netmask length allowed is /{min}"
            )
        },
        |max| {
            format!(
                "The requested netmask length /{length} is smaller than allowed by pool \
                 {pool_id}. Smallest netmask length allowed is /{max}"
            )
        },
    )
}

/// The netmask length `cidr` itself names, held to the pool's rules through
/// [`require_resource_allocation_netmask_length`] — the resource-creation counterpart of
/// [`require_compliant_allocation_cidr`], for the same reason: a range the request named
/// carries its own size.
pub fn require_resource_allocation_cidr<E>(
    pool: &IpamPool,
    pool_id: &str,
    cidr: &str,
) -> Result<(), Scaffolded<E>> {
    let Some(length) = allocation_cidr_netmask_length(cidr) else {
        return Ok(());
    };
    require_resource_allocation_netmask_length(pool, pool_id, length)
}

/// What a search for a free block inside a pool's space is asked for.
///
/// One value rather than five arguments, because every caller asks the same five questions:
/// the space to look in, the size wanted, what is already assigned, and the ranges the
/// request allows and disallows.
#[derive(Debug, Clone, Copy)]
pub struct FreeCidrBlockSearch<'a> {
    /// The ranges to look inside, in the order the store holds them — the space the pool
    /// holds ([`ComAmazonawsEc2Amazonec2State::ipam_pool_provisioned_cidrs`]).
    pub space: &'a [String],
    /// The netmask length of the block wanted.
    pub netmask_length: u32,
    /// The ranges already assigned out of that space
    /// ([`ComAmazonawsEc2Amazonec2State::ipam_pool_allocated_cidrs`]) — a candidate meeting
    /// one of them is not free.
    pub taken: &'a [String],
    /// "Include a particular CIDR range that can be returned by the pool"
    /// (`AllocateIpamPoolCidrRequest$AllowedCidrs`): when the request named any, a candidate
    /// outside every one of them is not one the pool may return.
    pub allowed: &'a [String],
    /// "Exclude a particular CIDR range from being returned by the pool"
    /// (`AllocateIpamPoolCidrRequest$DisallowedCidrs`): a candidate meeting one of them is
    /// not one the pool may return.
    pub disallowed: &'a [String],
}

/// What a search answers when finding out would cost more than its budget allows: the walk
/// was CUT SHORT, so nothing is known about whether a free block exists.
///
/// A type of its own rather than `()`, so a caller cannot read "the search did not finish" as
/// "there is no such block": the two have different answers on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CidrSearchBudgetSpent;

/// The first block of `search.netmask_length` bits inside the space that is free to hand out,
/// or `None` when the space holds no such block — and [`CidrSearchBudgetSpent`] when finding
/// out would cost more than `budget` allows.
///
/// What IPAM hands a request that names a SIZE rather than a range: the choice is the first
/// free block of the asked-for size in the order the space is held, which makes it a function
/// of the STORE rather than of the call, so two calls never hand out overlapping space.
///
/// Each candidate comes from [`nth_cidr_block_in`], the crate's one reading of a CIDR's
/// numeric bounds, so one walk serves IPv4 and IPv6 and a block chosen here is spelled as
/// every other block of this crate is.
///
/// The cost of the walk is the PRODUCT of the ranges in the space, the candidates inside each
/// one (which the netmask length decides) and the ranges it is compared against — all grown by
/// what the store holds — so ONE [`MatchBudget`] is charged a step per candidate AND a step
/// per comparison, exactly as the filter matching of a describe charges its own product. A
/// spent budget is an answer the request cannot be served rather than a search that runs on
/// under the state lock.
///
/// The ONE such search in this crate, so `AllocateIpamPoolCidr` and `ProvisionIpamPoolCidr`
/// cannot choose differently out of one pool's space.
pub fn first_free_cidr_block(
    search: &FreeCidrBlockSearch<'_>,
    budget: &mut MatchBudget,
) -> Result<Option<String>, CidrSearchBudgetSpent> {
    for range in search.space {
        for index in 0..MAX_CANDIDATE_CIDR_BLOCKS {
            if !budget.spend() {
                return Err(CidrSearchBudgetSpent);
            }
            let Some(candidate) = nth_cidr_block_in(range, search.netmask_length, index) else {
                // Past the end of this range — or a size it cannot be cut into at all, which
                // the first index already answers.
                break;
            };
            if !search.allowed.is_empty() {
                let allowed = search.allowed.iter().try_fold(false, |held, allowed| {
                    if !budget.spend() {
                        return Err(CidrSearchBudgetSpent);
                    }
                    Ok(held || cidr_block_contains_block(allowed, &candidate))
                })?;
                if !allowed {
                    continue;
                }
            }
            let mut free = true;
            for held in search.taken.iter().chain(search.disallowed) {
                if !budget.spend() {
                    return Err(CidrSearchBudgetSpent);
                }
                if cidr_blocks_overlap(held, &candidate) {
                    free = false;
                    break;
                }
            }
            if free {
                return Ok(Some(candidate));
            }
        }
    }
    Ok(None)
}

/// How many candidate blocks [`first_free_cidr_block`] may try inside ONE range.
///
/// A bound on the WORK one request can ask for, not an AWS quota: without it a request naming
/// a long netmask length inside a short range would walk billions of candidates under the
/// state lock. The budget bounds the walk ACROSS ranges, which this per-range figure alone
/// cannot.
const MAX_CANDIDATE_CIDR_BLOCKS: u128 = 4_096;
