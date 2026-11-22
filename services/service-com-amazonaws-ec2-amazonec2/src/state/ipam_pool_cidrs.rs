// SPDX-License-Identifier: CC0-1.0

//! The CIDRs provisioned to an IPAM pool: the store `ProvisionIpamPoolCidr` writes,
//! `GetIpamPoolCidrs` reports and `DeprovisionIpamPoolCidr` takes back.
//!
//! "A CIDR provisioned to an IPAM pool" ([`IpamPoolCidr`]) is a resource of its own — it
//! carries an id, a state and the netmask length it was provisioned with — so it is held in
//! a store of its own, keyed by that id, rather than inside the pool's modelled shape, which
//! has no member for it. The pool it belongs to is held beside the view for the same reason.
//!
//! This is also the store "you cannot delete an IPAM pool if there are allocations in it or
//! CIDRs provisioned to it" (`DeleteIpamPool`) is asked of: every reader of "what is
//! provisioned to this pool" goes through
//! [`ComAmazonawsEc2Amazonec2State::ipam_pool_provisioned_cidrs`], which answers these
//! records beside the BYOIP ranges `MoveByoipCidrToIpam` put in the pool, so one pool cannot
//! be judged empty by one path and occupied by another.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.

use super::*;

/// The id prefix an IPAM pool CIDR is minted with, which is also how its own ARN-free id
/// reads on the wire (`IpamPoolCidr$IpamPoolCidrId`).
pub const IPAM_POOL_CIDR_ID_PREFIX: &str = "ipam-pool-cidr";

/// How many lowercase hexadecimal digits an `IpamPoolCidr$IpamPoolCidrId` carries after
/// [`IPAM_POOL_CIDR_ID_PREFIX`].
///
/// A provisioned CIDR's id is NOT an EC2 resource id: the 8- and 17-digit forms
/// [`prefixed_id_is_well_formed`] reads are what a resource a request NAMES BY ID carries,
/// and nothing names a pool CIDR by id — `DeprovisionIpamPoolCidr` names the RANGE
/// (`DeprovisionIpamPoolCidrRequest$Cidr`), and no operation of this service takes an
/// `IpamPoolCidrId` as a parameter at all. AWS mints this one as an opaque per-CIDR
/// identifier of its own, wider than a resource id, which is the form the wire corpus
/// records for it — so it is minted by [`ComAmazonawsEc2Amazonec2State::mint_ipam_pool_cidr_id`]
/// rather than by the resource-id mint every id a caller may name goes through. An even count,
/// which that mint fills in two halves.
pub const IPAM_POOL_CIDR_ID_DIGITS: usize = 32;

/// A CIDR whose provisioning has been asked for and has not settled yet: the
/// `IpamPoolCidrState` enum's `pending-provision` member
/// (`smithy_query(query="shape",shape="IpamPoolCidrState")`), which is what
/// `ProvisionIpamPoolCidr` answers with.
pub const IPAM_POOL_CIDR_STATE_PENDING_PROVISION: &str = "pending-provision";

/// A CIDR whose provisioning has settled: the `IpamPoolCidrState` enum's `provisioned`
/// member, which the clock owes an [`IPAM_POOL_CIDR_STATE_PENDING_PROVISION`] CIDR one
/// [`TRANSITION_DELAY`] after the call that provisioned it.
pub const IPAM_POOL_CIDR_STATE_PROVISIONED: &str = "provisioned";

/// A CIDR whose provisioning settled as a FAILURE: the `IpamPoolCidrState` enum's
/// `failed-provision` member.
///
/// "Details related to why an IPAM pool CIDR FAILED TO BE PROVISIONED"
/// (`IpamPoolCidr$FailureReason`) is the member such a record carries, so work a provision
/// cannot carry out is reported as a settled failure OF THE RANGE rather than as a refusal of
/// the request: the request itself was well formed and was accepted, and what could not be
/// done is the provisioning. The clock owes this state one [`TRANSITION_DELAY`] after the call
/// that asked for it, exactly as it owes [`IPAM_POOL_CIDR_STATE_PROVISIONED`] to a provision
/// that can be carried out.
pub const IPAM_POOL_CIDR_STATE_FAILED_PROVISION: &str = "failed-provision";

/// The `IpamPoolCidrFailureReason$Code` of a provision there was no space for: the
/// `IpamPoolCidrFailureCode` enum's `cidr-not-available` member
/// (`smithy_query(query="shape",shape="IpamPoolCidrFailureCode")`), that catalog's own name
/// for "the range this pool was to be given could not be had".
pub const IPAM_POOL_CIDR_FAILURE_CODE_NOT_AVAILABLE: &str = "cidr-not-available";

/// A CIDR whose deprovisioning has been asked for and has not settled yet: the
/// `IpamPoolCidrState` enum's `pending-deprovision` member, which
/// `DeprovisionIpamPoolCidr` answers with.
pub const IPAM_POOL_CIDR_STATE_PENDING_DEPROVISION: &str = "pending-deprovision";

/// A CIDR whose deprovisioning has settled: the `IpamPoolCidrState` enum's `deprovisioned`
/// member. The record is REMOVED at that same instant
/// ([`ComAmazonawsEc2Amazonec2State::materialize_due_ipam_pool_cidr_moves`]) — the range is
/// no longer provisioned to the pool, so the pool reports it no more — which is why this
/// state is never one a reader sees stored.
pub const IPAM_POOL_CIDR_STATE_DEPROVISIONED: &str = "deprovisioned";

/// One CIDR provisioned to one IPAM pool: the modelled [`IpamPoolCidr`] every operation that
/// reports one answers with, the pool it belongs to, and the state move the clock still owes
/// it.
///
/// The pool id and the move are held beside the view rather than on it for the reason the
/// module docs give: the modelled shape carries neither.
#[derive(Debug, Clone)]
pub struct IpamPoolCidrRecord {
    /// The `IpamPoolId` of the pool this CIDR is provisioned to.
    pub ipam_pool_id: String,
    /// The CIDR as every reader of it reports it.
    pub view: IpamPoolCidr,
    /// The state the clock owes this CIDR, if it owes one — `provisioned` after a
    /// provision, `deprovisioned` after a deprovision.
    pub pending: Option<PendingStateMove>,
    /// What the CIDR reports about the failure once the move the clock owes it is a FAILED
    /// one; nothing at all for a move that settles as a success.
    ///
    /// Held beside the view rather than on it because the record carries no failure WHILE the
    /// work is still under way: "details related to why an IPAM pool CIDR failed to be
    /// provisioned" (`IpamPoolCidr$FailureReason`) is what a SETTLED failure reports, so the
    /// reason is applied at the instant the state becomes the failed one
    /// ([`ComAmazonawsEc2Amazonec2State::materialize_due_ipam_pool_cidr_moves`]).
    pub failure_reason_when_due: Option<IpamPoolCidrFailureReason>,
}

/// Whether a CIDR in `state` still holds space in its pool — whether it is one of the
/// "CIDRs provisioned to it" a `DeleteIpamPool` is refused for.
///
/// A range whose deprovisioning the caller has ALREADY asked for is not: the caller has
/// taken it back, and "to deprovision pool CIDRs, see DeprovisionIpamPoolCidr"
/// (`DeleteIpamPool`) is exactly the step that asking performs, so the pool no longer counts
/// it against a delete. Neither is a range whose provisioning FAILED: nothing was ever given
/// to the pool, so there is no space for it to hold. Everything else does.
pub fn ipam_pool_cidr_holds_space(state: Option<&str>) -> bool {
    !matches!(
        state,
        Some(IPAM_POOL_CIDR_STATE_PENDING_DEPROVISION)
            | Some(IPAM_POOL_CIDR_STATE_DEPROVISIONED)
            | Some(IPAM_POOL_CIDR_STATE_FAILED_PROVISION)
    )
}

/// The caller-chosen bytes one stored pool CIDR holds: the range itself, which is the only
/// text the caller chose.
///
/// The fixed [`STORED_RECORD_OVERHEAD_BYTES`] every stored record costs is charged on top by
/// the writer, exactly as [`ipam_policy_payload_bytes`] is charged for a policy.
pub fn ipam_pool_cidr_payload_bytes(view: &IpamPoolCidr) -> usize {
    STORED_RECORD_OVERHEAD_BYTES + view.cidr.as_deref().map_or(0, str::len)
}

/// Spread `value` across all of its bits, so the second half of an id built out of the store's
/// counter reads as the opaque identifier AWS hands out rather than as a count.
///
/// Every step is invertible — a multiplication by an odd constant and a xor with a shift of
/// the value itself — so this is a BIJECTION of its input: two counter values can never be
/// spread onto one value. Nothing is read off a clock or a random source, so the same sequence
/// of calls mints the same ids.
fn spread_counter(value: u64) -> u64 {
    let mut spread = value.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    spread ^= spread >> 30;
    spread = spread.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    spread ^= spread >> 27;
    spread = spread.wrapping_mul(0x94d0_49bb_1331_11eb);
    spread ^ (spread >> 31)
}

impl ComAmazonawsEc2Amazonec2State {
    /// Mint the id one provisioned CIDR is reported by: [`IPAM_POOL_CIDR_ID_PREFIX`] and
    /// [`IPAM_POOL_CIDR_ID_DIGITS`] lowercase hexadecimal digits, the opaque per-CIDR form
    /// AWS hands out for an `IpamPoolCidr$IpamPoolCidrId`.
    ///
    /// The ONE mint of this id, so every stored pool CIDR — a provision that settles as a
    /// success and one that settles as a failure alike — is reported by an id of one form.
    ///
    /// The id is derived from the store's own monotonic counter, the source [`Self::next_id`]
    /// draws on, so it is read off no clock and no random source and a run that provisions the
    /// same ranges reports the same ids. Two things differ from a resource id, which is why
    /// this does not go through that mint: the suffix is WIDER, and only its FIRST half is the
    /// counter — the second is that counter spread across its digits ([`spread_counter`]),
    /// because what AWS hands out here is an opaque identifier rather than a count.
    ///
    /// The counter leads, so the ids of one pool sort in the order the ranges were provisioned
    /// — which is the order every reader of them reports
    /// ([`Self::ipam_pool_cidr_records`]) and the order the page cursor of
    /// `GetIpamPoolCidrs` names a position in.
    fn mint_ipam_pool_cidr_id(&mut self) -> String {
        self.id_counter = self.id_counter.saturating_add(1);
        let counter = self.id_counter;
        format!(
            "{IPAM_POOL_CIDR_ID_PREFIX}-{counter:0half$x}{spread:0half$x}",
            spread = spread_counter(counter),
            half = IPAM_POOL_CIDR_ID_DIGITS / 2,
        )
    }

    /// Whether another IPAM pool CIDR fits inside the store's ceiling
    /// ([`MAX_STORED_RECORDS_PER_STORE`]) — the same contract as
    /// [`Self::has_ipam_pool_capacity`]: a writer asks first and refuses when it answers
    /// `false`, so repeated provisions cannot grow memory without bound.
    pub fn has_ipam_pool_cidr_capacity(&self) -> bool {
        self.ipam_pool_cidrs.len() < MAX_STORED_RECORDS_PER_STORE
    }

    /// The CIDRs provisioned to the pool `pool_id` names, as the records the store holds,
    /// in the one order every reader of them reports: the order of their ids.
    pub fn ipam_pool_cidr_records(&self, pool_id: &str) -> Vec<&IpamPoolCidrRecord> {
        self.ipam_pool_cidrs
            .values()
            .filter(|record| record.ipam_pool_id == pool_id)
            .collect()
    }

    /// The record the pool `pool_id` holds for the range `cidr`, if it holds one.
    ///
    /// One pool holds at most one record per range — a second provision of the same range
    /// would say nothing the first does not — which is what makes this the lookup
    /// `DeprovisionIpamPoolCidr` resolves its `Cidr` through.
    pub fn ipam_pool_cidr(&self, pool_id: &str, cidr: &str) -> Option<&IpamPoolCidrRecord> {
        self.ipam_pool_cidrs.values().find(|record| {
            record.ipam_pool_id == pool_id && record.view.cidr.as_deref() == Some(cidr)
        })
    }

    /// Provision `cidr` to the pool `pool_id` names and answer the CIDR as the store then
    /// holds it — `None` when the store is at its ceiling and NOTHING was stored.
    ///
    /// The ONE way a pool CIDR is stored, so the members the SERVICE owns cannot be built two
    /// ways: the id and the `pending-provision` state it starts in are minted here, the way
    /// [`Self::insert_creating_ipam_pool`] mints a pool's, and the netmask length is the
    /// range's own rather than a caller's claim about it. The clock owes the CIDR
    /// `provisioned` one [`TRANSITION_DELAY`] later, which is RECORDED rather than slept for,
    /// so a caller acting straight after the call still sees the transient state.
    ///
    /// A pool created WITHIN another draws this range out of that pool's space, which is "a
    /// CIDR assignment from an IPAM pool to another IPAM pool" ([`IpamPoolAllocation`]) — so
    /// the SOURCE pool is given the allocation that assignment is, through the one writer of
    /// that store ([`Self::allocate_ipam_pool_cidr`]), and the space stops being free of the
    /// source pool at the same instant this range starts being held by this one. Either both
    /// records are stored or neither is: the ceilings are asked for the pair before the first
    /// write.
    pub fn provision_ipam_pool_cidr(
        &mut self,
        pool_id: &str,
        cidr: &str,
        netmask_length: Option<i32>,
    ) -> Option<IpamPoolCidr> {
        let view = IpamPoolCidr {
            cidr: Some(cidr.to_string()),
            state: Some(IPAM_POOL_CIDR_STATE_PENDING_PROVISION.to_string()),
            // "Details related to why an IPAM pool CIDR failed to be provisioned"
            // (`IpamPoolCidr$FailureReason`): nothing has failed, so there is nothing to
            // report about it.
            failure_reason: None,
            ipam_pool_cidr_id: None,
            netmask_length,
        };
        let charge = ipam_pool_cidr_payload_bytes(&view);
        let assignment = self.ipam_pool_assignment_to(pool_id, cidr);
        let assignment_charge = assignment
            .as_ref()
            .map_or(0, |(_, allocation)| ipam_pool_allocation_payload_bytes(allocation));
        if !self.has_ipam_pool_cidr_capacity()
            || self.would_exceed_stored_bytes(charge.saturating_add(assignment_charge))
            || (assignment.is_some() && !self.has_ipam_pool_allocation_capacity())
        {
            return None;
        }
        let cidr_id = self.mint_ipam_pool_cidr_id();
        let mut view = view;
        view.ipam_pool_cidr_id = Some(cidr_id.clone());
        self.add_stored_bytes(charge);
        self.ipam_pool_cidrs.insert(
            cidr_id,
            IpamPoolCidrRecord {
                ipam_pool_id: pool_id.to_string(),
                view: view.clone(),
                pending: Some(PendingStateMove::starting_now(
                    IPAM_POOL_CIDR_STATE_PROVISIONED,
                )),
                // The move this provision owes settles as a SUCCESS, so there is no failure
                // to report when it falls due.
                failure_reason_when_due: None,
            },
        );
        if let Some((source_pool_id, allocation)) = assignment {
            // No `ClientToken` names this allocation: the request that made it names the
            // provisioned RANGE, which is what a replay of it is answered by.
            self.allocate_ipam_pool_cidr(&source_pool_id, None, allocation);
        }
        Some(view)
    }

    /// Record a provision of a range of `netmask_length` bits the pool CANNOT be given, and
    /// answer the CIDR as the store then holds it — `None` when the store is at its ceiling
    /// and NOTHING was stored.
    ///
    /// A request that names a SIZE asks the service to choose the range, and the space it
    /// would choose out of may hold no free block of that size. That is not something wrong
    /// with the REQUEST — it names a size the pool's own rules admit, and the space a pool
    /// holds changes without the caller — so the provision is accepted and the range it would
    /// have created settles as a failure: the record starts
    /// [`IPAM_POOL_CIDR_STATE_PENDING_PROVISION`] like any other and the clock owes it
    /// [`IPAM_POOL_CIDR_STATE_FAILED_PROVISION`] with `reason` beside it, rather than
    /// `provisioned`.
    ///
    /// Nothing is assigned out of any pool for it: no range was chosen, so the record carries
    /// no `Cidr` at all and no source pool gives up space for one.
    pub fn provision_failing_ipam_pool_cidr(
        &mut self,
        pool_id: &str,
        netmask_length: Option<i32>,
        reason: IpamPoolCidrFailureReason,
    ) -> Option<IpamPoolCidr> {
        let view = IpamPoolCidr {
            // No range was chosen, so there is none to report: the size asked for is the only
            // thing this record knows about the range it would have held.
            cidr: None,
            state: Some(IPAM_POOL_CIDR_STATE_PENDING_PROVISION.to_string()),
            // Reported once the failure has SETTLED, which is the move recorded below.
            failure_reason: None,
            ipam_pool_cidr_id: None,
            netmask_length,
        };
        // The message beside the failure is the SERVICE's own wording rather than anything the
        // caller chose, so what this record costs the store's budget of caller-supplied data
        // is what every other pool CIDR record costs — which is also what is refunded when it
        // is removed.
        let charge = ipam_pool_cidr_payload_bytes(&view);
        if !self.has_ipam_pool_cidr_capacity() || self.would_exceed_stored_bytes(charge) {
            return None;
        }
        let cidr_id = self.mint_ipam_pool_cidr_id();
        let mut view = view;
        view.ipam_pool_cidr_id = Some(cidr_id.clone());
        self.add_stored_bytes(charge);
        self.ipam_pool_cidrs.insert(
            cidr_id,
            IpamPoolCidrRecord {
                ipam_pool_id: pool_id.to_string(),
                view: view.clone(),
                pending: Some(PendingStateMove::starting_now(
                    IPAM_POOL_CIDR_STATE_FAILED_PROVISION,
                )),
                failure_reason_when_due: Some(reason),
            },
        );
        Some(view)
    }

    /// The allocation the pool `pool_id` draws on holds for the range `cidr` assigned to that
    /// pool, and the pool holding it — `None` when `pool_id` names a top-level pool, which
    /// draws on no pool at all.
    ///
    /// The one place that assignment is built, so the allocation a provision makes and the one
    /// a deprovision gives back ([`Self::release_ipam_pool_allocation_of`]) name the same
    /// range, resource and pool. What the allocation reports about the resource is the POOL's
    /// own: "the Amazon Web Services Region of the resource" is the Region that pool allocates
    /// in — its locale, or the Region of its IPAM when it has none — and the owner is the
    /// account holding it.
    fn ipam_pool_assignment_to(
        &self,
        pool_id: &str,
        cidr: &str,
    ) -> Option<(String, IpamPoolAllocation)> {
        let pool = self.ipam_pools.get(pool_id)?;
        let source_pool_id = pool.source_ipam_pool_id.clone()?;
        Some((
            source_pool_id,
            IpamPoolAllocation {
                cidr: Some(cidr.to_string()),
                // Minted by the store's one writer of an allocation.
                ipam_pool_allocation_id: None,
                // "A description of the pool allocation"
                // (`IpamPoolAllocation$Description`): nobody described this assignment —
                // provisioning a range to a pool carries no description at all.
                description: None,
                resource_id: Some(pool_id.to_string()),
                resource_type: Some(
                    IPAM_POOL_ALLOCATION_RESOURCE_TYPE_IPAM_POOL.to_string(),
                ),
                resource_region: pool.locale.clone().or_else(|| pool.ipam_region.clone()),
                resource_owner: pool.owner_id.clone(),
                // "The tags for the IPAM pool allocation" (`IpamPoolAllocation$Tags`):
                // nothing tagged this assignment, which no request made.
                tags: None,
            },
        ))
    }

    /// Start deprovisioning the range `cidr` of the pool `pool_id` names, and answer the CIDR
    /// as the store then holds it — `None` when the pool holds no such range.
    ///
    /// "Deprovision a CIDR provisioned from an IPAM pool" is work AWS reports as under way:
    /// the `IpamPoolCidrState` catalog carries both `pending-deprovision` and
    /// `deprovisioned`, so the record is left in the TRANSIENT state with the clock owing it
    /// the terminal one — the same arrangement, on the same clock, as the provision above.
    /// Reaching the terminal state REMOVES the record
    /// ([`Self::materialize_due_ipam_pool_cidr_moves`]), which is what gives the bytes it
    /// held back to the store's budget.
    ///
    /// A BYOIP address range IPAM manages is provisioned to the pool too
    /// (`ByoipCidr$IpamPoolId`), and it is the pool's record of that range: it stops being
    /// managed by this pool at once, the way `MoveByoipCidrToIpam` put it in at once, and the
    /// range itself is left exactly as it was — it is once again a range the account holds
    /// outside IPAM.
    ///
    /// A range leaving a pool created WITHIN another is space that pool no longer holds, so the
    /// allocation the source pool held for it goes back
    /// ([`Self::release_ipam_pool_allocation_of`]) — at this instant rather than when the
    /// deprovision settles, because a `pending-deprovision` range already holds no space
    /// ([`ipam_pool_cidr_holds_space`]).
    pub fn deprovision_ipam_pool_cidr(
        &mut self,
        pool_id: &str,
        cidr: &str,
    ) -> Option<IpamPoolCidr> {
        let held = self
            .ipam_pool_cidr(pool_id, cidr)
            .and_then(|record| record.view.ipam_pool_cidr_id.clone());
        if let Some(cidr_id) = held {
            let view = {
                let record = self.ipam_pool_cidrs.get_mut(&cidr_id)?;
                record.view.state =
                    Some(IPAM_POOL_CIDR_STATE_PENDING_DEPROVISION.to_string());
                record.pending = Some(PendingStateMove::starting_now(
                    IPAM_POOL_CIDR_STATE_DEPROVISIONED,
                ));
                record.view.clone()
            };
            if let Some(source_pool_id) = self
                .ipam_pools
                .get(pool_id)
                .and_then(|pool| pool.source_ipam_pool_id.clone())
            {
                self.release_ipam_pool_allocation_of(&source_pool_id, pool_id, cidr);
            }
            return Some(view);
        }
        let byoip = self.byoip_cidrs.get_mut(cidr)?;
        if byoip.byoip_cidr.ipam_pool_id.as_deref() != Some(pool_id) {
            return None;
        }
        byoip.byoip_cidr.ipam_pool_id = None;
        byoip.byoip_cidr.pool_id = None;
        Some(IpamPoolCidr {
            cidr: Some(cidr.to_string()),
            state: Some(IPAM_POOL_CIDR_STATE_DEPROVISIONED.to_string()),
            failure_reason: None,
            // A BYOIP range is held by its range rather than by an id of this store, so
            // there is no `IpamPoolCidrId` to report for it.
            ipam_pool_cidr_id: None,
            netmask_length: cidr_block_prefix_length(cidr).and_then(|length| i32::try_from(length).ok()),
        })
    }

    /// Remove every CIDR record of the pool `pool_id` names, giving the bytes they held back
    /// to the store's budget.
    ///
    /// A record names its pool for as long as it is stored, so a pool that LEAVES the store
    /// takes its records with it rather than leaving rows naming a pool nothing resolves.
    pub(super) fn remove_ipam_pool_cidrs(&mut self, pool_id: &str) {
        let doomed: Vec<String> = self
            .ipam_pool_cidrs
            .iter()
            .filter(|(_, record)| record.ipam_pool_id == pool_id)
            .map(|(cidr_id, _)| cidr_id.clone())
            .collect();
        for cidr_id in doomed {
            if let Some(record) = self.ipam_pool_cidrs.remove(&cidr_id) {
                self.release_stored_bytes(ipam_pool_cidr_payload_bytes(&record.view));
            }
        }
    }

    /// Apply the state moves the clock owes the stored pool CIDRs, as of `now`.
    ///
    /// A `pending-provision` range is `provisioned` once its own instant has passed — or
    /// `failed-provision`, carrying the reason recorded for it, when the work it asked for
    /// could not be carried out ([`Self::provision_failing_ipam_pool_cidr`]) — and a
    /// `pending-deprovision` one has left the pool at its instant. All of them are owed whether
    /// or not anybody called, which is why they are applied from
    /// [`Self::materialize_due_transitions`], its one caller, before anything reads the
    /// store.
    pub(super) fn materialize_due_ipam_pool_cidr_moves(&mut self, now: Instant) {
        let mut deprovisioned: Vec<String> = Vec::new();
        for (cidr_id, record) in &mut self.ipam_pool_cidrs {
            let Some(due) = record
                .pending
                .as_ref()
                .and_then(|pending| pending.due_by(now))
            else {
                continue;
            };
            if due == IPAM_POOL_CIDR_STATE_DEPROVISIONED {
                deprovisioned.push(cidr_id.clone());
                continue;
            }
            record.view.state = Some(due.to_string());
            // "Details related to why an IPAM pool CIDR failed to be provisioned"
            // (`IpamPoolCidr$FailureReason`): the reason is part of the SETTLED failure, so it
            // starts being reported at the instant the failed state does.
            record.view.failure_reason = record.failure_reason_when_due.take();
            record.pending = None;
        }
        for cidr_id in deprovisioned {
            if let Some(record) = self.ipam_pool_cidrs.remove(&cidr_id) {
                self.release_stored_bytes(ipam_pool_cidr_payload_bytes(&record.view));
            }
        }
    }
}
