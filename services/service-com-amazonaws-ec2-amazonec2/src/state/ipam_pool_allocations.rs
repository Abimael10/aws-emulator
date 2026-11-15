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
