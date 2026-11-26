// SPDX-License-Identifier: CC0-1.0

//! The resource CIDRs IPAM MONITORS in one of its scopes: the ranges of the account's own
//! resources, as `GetIpamResourceCidrs` reports them and `ModifyIpamResourceCidr` changes the
//! monitoring of them.
//!
//! A resource CIDR is not a record a caller creates: "a resource discovery is an IPAM component
//! that enables IPAM to manage and MONITOR resources that belong to the owning account"
//! (`GetIpamResourceCidrs`), so the rows are DERIVED from the resources the store already holds
//! — every IPv4 range of the account's VPCs and subnets in a Region the scope's IPAM operates in
//! — rather than stored a second time beside them. That is what keeps this report, `DescribeVpcs`
//! and `GetIpamPoolAllocations` from disagreeing about one range.
//!
//! The one thing a caller DOES decide about a resource CIDR is whether IPAM monitors it and which
//! scope it sits in ("you can use this action to transfer resource CIDRs between scopes and ignore
//! resource CIDRs that you do not want to manage", `ModifyIpamResourceCidr`), so that — and only
//! that — is stored, in [`ComAmazonawsEc2Amazonec2State::ipam_resource_cidr_settings`].
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and re-exported
//! from it, so a handler reaches these as `crate::state::<item>` like every other primitive there.

use super::*;

/// The `IpamManagementState` of a resource whose range IPAM hands out of one of the scope's pools:
/// `managed` (`smithy_query(query="shape",shape="IpamManagementState")`).
pub const IPAM_MANAGEMENT_STATE_MANAGED: &str = "managed";

/// The `IpamManagementState` of a resource IPAM monitors whose range comes out of no pool of the
/// scope: `unmanaged` (same catalog).
pub const IPAM_MANAGEMENT_STATE_UNMANAGED: &str = "unmanaged";

/// The `IpamManagementState` of a resource CIDR a caller asked IPAM NOT to monitor: `ignored`
/// (same catalog) — what `ModifyIpamResourceCidr` with `Monitored` false leaves behind.
pub const IPAM_MANAGEMENT_STATE_IGNORED: &str = "ignored";

/// The `IpamComplianceStatus` of a managed resource CIDR that meets the allocation rules of the
/// pool its range came out of: `compliant`
/// (`smithy_query(query="shape",shape="IpamComplianceStatus")`).
pub const IPAM_COMPLIANCE_STATUS_COMPLIANT: &str = "compliant";

/// The `IpamComplianceStatus` of a managed resource CIDR that does NOT meet those rules:
/// `noncompliant` (same catalog).
pub const IPAM_COMPLIANCE_STATUS_NONCOMPLIANT: &str = "noncompliant";

/// The `IpamComplianceStatus` of a monitored resource CIDR no pool of the scope hands out:
/// `unmanaged` (same catalog) — there are no allocation rules to hold it to.
pub const IPAM_COMPLIANCE_STATUS_UNMANAGED: &str = "unmanaged";

/// The `IpamComplianceStatus` of a resource CIDR IPAM does not monitor: `ignored` (same catalog).
pub const IPAM_COMPLIANCE_STATUS_IGNORED: &str = "ignored";

/// The `IpamOverlapStatus` of a resource CIDR that shares addresses with another resource's range
/// in the same scope: `overlapping`
/// (`smithy_query(query="shape",shape="IpamOverlapStatus")`).
pub const IPAM_OVERLAP_STATUS_OVERLAPPING: &str = "overlapping";

/// The `IpamOverlapStatus` of a resource CIDR that shares addresses with no other: `nonoverlapping`
/// (same catalog).
pub const IPAM_OVERLAP_STATUS_NONOVERLAPPING: &str = "nonoverlapping";

/// The `IpamOverlapStatus` of a resource CIDR IPAM does not monitor: `ignored` (same catalog) —
/// "if set to false, the resource will not be tracked for overlap" (`ModifyIpamResourceCidr`).
pub const IPAM_OVERLAP_STATUS_IGNORED: &str = "ignored";

/// The `IpamResourceType` of a reported VPC range: `vpc`
/// (`smithy_query(query="shape",shape="IpamResourceType")`).
pub const IPAM_RESOURCE_TYPE_VPC: &str = "vpc";

/// The `IpamResourceType` of a reported subnet range: `subnet` (same catalog).
pub const IPAM_RESOURCE_TYPE_SUBNET: &str = "subnet";

/// The tag key a resource's NAME is read from, which is what `IpamResourceCidr$ResourceName`
/// reports: EC2 resources carry no name member of their own, so a console name is the value of
/// this tag (Amazon EC2 User Guide, "Tag your Amazon EC2 resources").
pub const RESOURCE_NAME_TAG_KEY: &str = "Name";

/// `InvalidResourceCidr.NotFound` (HTTP 400): the request named a resource CIDR IPAM monitors none
/// of, so there is no row to act on — the code AWS answers a `ModifyIpamResourceCidr` that cannot
/// resolve the resource CIDR its members name with, as the wire corpus records.
pub const INVALID_RESOURCE_CIDR_NOT_FOUND: &str = "InvalidResourceCidr.NotFound";

/// The sentence EC2 answers [`INVALID_RESOURCE_CIDR_NOT_FOUND`] with.
///
/// FIXED text: it says that the resource CIDR the request asked about could not be found and
/// repeats no part of the request — the form this service's IPAM family words a not-found in, from
/// the two such sentences the wire corpus records for it ([`BYOASN_NOT_FOUND_MESSAGE`], and the one
/// [`cidr_not_found_in_ipam`] carries), where the caller sent the values and the message names the
/// condition rather than echoing them.
pub const RESOURCE_CIDR_NOT_FOUND_MESSAGE: &str = "The requested resource CIDR could not be found.";

/// Refuse a request naming a resource CIDR IPAM monitors none of: HTTP 400
/// [`INVALID_RESOURCE_CIDR_NOT_FOUND`], with EC2's own sentence for that code
/// ([`RESOURCE_CIDR_NOT_FOUND_MESSAGE`]).
///
/// A resource CIDR is named by the resource, the range, the Region the resource is in and the scope
/// it is monitored in, and ANY of the four can be the reason the row cannot be resolved — which is
/// one condition, stated once, rather than four refusals.
pub fn invalid_resource_cidr_not_found<E>() -> Scaffolded<E> {
    Scaffolded::service_error(
        INVALID_RESOURCE_CIDR_NOT_FOUND,
        400,
        RESOURCE_CIDR_NOT_FOUND_MESSAGE,
    )
}

/// Hold the scope `scope_id` to being a PRIVATE one ([`IPAM_SCOPE_TYPE_PRIVATE`]).
///
/// The resource CIDRs a caller modifies are the ranges of the account's own resources, which is
/// private address space — "the private scope is intended for all private IP address space"
/// ([`IpamScope`]) — so a scope of the other type holds none of them and is refused with EC2's own
/// sentence for that cause ("The scope … is not private."), which names the scope the request sent.
///
/// A REPORT of a scope's resource CIDRs asks this of no scope: a public scope has resource CIDRs of
/// its own to report, it is only the monitoring of a resource's private range that cannot be
/// decided through one.
pub fn require_private_ipam_scope<E>(scope: &IpamScope, scope_id: &str) -> Result<(), Scaffolded<E>> {
    if scope.ipam_scope_type.as_deref() == Some(IPAM_SCOPE_TYPE_PRIVATE) {
        return Ok(());
    }
    Err(invalid_parameter_value(format!(
        "The scope {scope_id} is not private."
    )))
}

/// What a `ModifyIpamResourceCidr` has recorded about ONE resource CIDR of one IPAM: the only two
/// facts about a resource CIDR a caller decides rather than the resource itself.
///
/// Everything else a reported [`IpamResourceCidr`] carries is derived from the resource and from
/// the pools of its scope ([`ComAmazonawsEc2Amazonec2State::ipam_resource_cidrs_in_scope`]), so
/// nothing here can drift from what `DescribeVpcs` or `GetIpamPoolAllocations` reports.
#[derive(Debug, Clone)]
pub struct IpamResourceCidrSettings {
    /// "Determines if the resource is monitored by IPAM"
    /// (`ModifyIpamResourceCidrRequest$Monitored`). A resource CIDR nothing has modified is
    /// monitored, so the absence of a record means `true`.
    pub monitored: bool,
    /// The `IpamScopeId` the resource CIDR now sits in: the scope a transfer moved it to ("you can
    /// use this action to transfer resource CIDRs between scopes", `ModifyIpamResourceCidr`), which
    /// for a resource CIDR nothing has transferred is its IPAM's private default scope.
    pub ipam_scope_id: String,
}

/// The key one [`IpamResourceCidrSettings`] record is held under: the IPAM whose monitoring it
/// decides, and the resource CIDR it decides it for — the `ResourceId` and `ResourceCidr` pair
/// `ModifyIpamResourceCidr` names one by.
pub type IpamResourceCidrKey = (String, String, String);

/// The bytes one stored [`IpamResourceCidrSettings`] record holds: its key and the scope the
/// resource CIDR sits in, on top of the fixed [`STORED_RECORD_OVERHEAD_BYTES`] every stored record
/// costs — so no path can store one uncharged.
pub fn ipam_resource_cidr_settings_payload_bytes(
    key: &IpamResourceCidrKey,
    settings: &IpamResourceCidrSettings,
) -> usize {
    STORED_RECORD_OVERHEAD_BYTES
        .saturating_add(key.0.len())
        .saturating_add(key.1.len())
        .saturating_add(key.2.len())
        .saturating_add(settings.ipam_scope_id.len())
}

/// One resource of the account whose range IPAM monitors, as the store holds the resource: what
/// the derivation below reads off a VPC or a subnet before the scope's own pools decide the
/// management, compliance and overlap of it.
#[derive(Debug, Clone)]
struct MonitoredResource {
    /// "The ID of an IPAM resource" (`IpamResourceCidr$ResourceId`).
    resource_id: String,
    /// "The type of IPAM resource" (`IpamResourceCidr$ResourceType`), from the modelled catalog.
    resource_type: &'static str,
    /// "The CIDR for an IPAM resource" (`IpamResourceCidr$ResourceCidr`).
    resource_cidr: String,
    /// "The Amazon Web Services Region for an IPAM resource"
    /// (`IpamResourceCidr$ResourceRegion`): the Region the resource was created in.
    resource_region: String,
    /// "The Amazon Web Services account number of the owner of an IPAM resource"
    /// (`IpamResourceCidr$ResourceOwnerId`), read off the resource itself.
    resource_owner_id: Option<String>,
    /// "The name of an IPAM resource" (`IpamResourceCidr$ResourceName`): the resource's `Name` tag
    /// ([`RESOURCE_NAME_TAG_KEY`]), absent for a resource carrying none.
    resource_name: Option<String>,
    /// "The tags for an IPAM resource" (`IpamResourceCidr$ResourceTags`).
    resource_tags: Vec<IpamResourceTag>,
    /// "The ID of a VPC" (`IpamResourceCidr$VpcId`): the VPC the range belongs to — the resource
    /// itself for a VPC range, the subnet's VPC for a subnet range.
    vpc_id: Option<String>,
    /// "The Availability Zone ID" (`IpamResourceCidr$AvailabilityZoneId`), which only a resource
    /// of one zone has.
    availability_zone_id: Option<String>,
    /// "The percentage of IP address space in use" (`IpamResourceCidr$IpUsage`), derived as that
    /// member's own documentation defines it per resource type.
    ip_usage: Option<f64>,
}

/// The tags of a resource as an [`IpamResourceTag`] list — the shape a reported resource CIDR
/// carries them in, which is the EC2 `Tag` pair under another name.
fn ipam_resource_tags_of(tags: &[Tag]) -> Vec<IpamResourceTag> {
    tags.iter()
        .map(|tag| IpamResourceTag {
            key: tag.key.clone(),
            value: tag.value.clone(),
        })
        .collect()
}

/// The value of the `Name` tag among `tags`, which is what a reported resource CIDR's
/// `ResourceName` carries — `None` for a resource carrying no such tag.
fn resource_name_of(tags: &[Tag]) -> Option<String> {
    tags.iter()
        .find(|tag| tag.key.as_deref() == Some(RESOURCE_NAME_TAG_KEY))
        .and_then(|tag| tag.value.clone())
}

/// How many addresses `cidr` covers, or `None` when it is not a CIDR block at all.
fn cidr_block_address_count(cidr: &str) -> Option<u128> {
    let (_, first, last) = cidr_block_bounds(cidr)?;
    last.checked_sub(first)?.checked_add(1)
}

impl ComAmazonawsEc2Amazonec2State {
    /// Whether another stored [`IpamResourceCidrSettings`] record fits inside the store's ceiling
    /// ([`MAX_STORED_RECORDS_PER_STORE`]) — the same contract as
    /// [`Self::has_ipam_pool_allocation_capacity`]: a writer asks first and refuses when it answers
    /// `false`, so repeated modifications cannot grow memory without bound.
    pub fn has_ipam_resource_cidr_setting_capacity(&self) -> bool {
        self.ipam_resource_cidr_settings.len() < MAX_STORED_RECORDS_PER_STORE
    }

    /// What a `ModifyIpamResourceCidr` has recorded about the resource CIDR `resource_id` plus
    /// `cidr` of the IPAM `ipam_id` — `None` when nothing has modified it, which is a monitored
    /// resource CIDR in its IPAM's private default scope.
    ///
    /// A READ: a resource CIDR nothing has modified is not given a record here, so reporting one
    /// cannot change what a later caller sees.
    pub fn ipam_resource_cidr_settings_of(
        &self,
        ipam_id: &str,
        resource_id: &str,
        cidr: &str,
    ) -> Option<&IpamResourceCidrSettings> {
        self.ipam_resource_cidr_settings.get(&(
            ipam_id.to_string(),
            resource_id.to_string(),
            cidr.to_string(),
        ))
    }

    /// The `IpamPoolId` of the pool of the scope `scope_id` that hands out the range `cidr` to the
    /// resource `resource_id`, or `None` when no pool of that scope does.
    ///
    /// Read off the ALLOCATION store ([`Self::ipam_pool_allocations`]) — "in IPAM, an allocation is
    /// a CIDR assignment from an IPAM pool to another IPAM pool or to a resource"
    /// ([`IpamPoolAllocation`]) — so a resource is reported as managed by exactly the pool
    /// `GetIpamPoolAllocations` reports as holding its range, and a release of that allocation is
    /// seen here at once.
    pub fn ipam_pool_of_resource_cidr(
        &self,
        scope_id: &str,
        resource_id: &str,
        cidr: &str,
    ) -> Option<String> {
        let scope_arn = self
            .ipam_scopes
            .get(scope_id)
            .and_then(|scope| scope.ipam_scope_arn.as_deref())?;
        self.ipam_pool_allocations
            .values()
            .find(|record| {
                record.view.resource_id.as_deref() == Some(resource_id)
                    && record.view.cidr.as_deref() == Some(cidr)
                    && self
                        .ipam_pools
                        .get(&record.ipam_pool_id)
                        .and_then(|pool| pool.ipam_scope_arn.as_deref())
                        == Some(scope_arn)
            })
            .map(|record| record.ipam_pool_id.clone())
    }

    /// The resource CIDRs the scope `scope_id` holds, as every reader of them reports them.
    ///
    /// The ONE derivation of that set, so `GetIpamResourceCidrs` and the resource CIDR
    /// `ModifyIpamResourceCidr` answers with cannot disagree about one range. A scope this store
    /// holds no scope for, or one whose IPAM is gone, holds none.
    ///
    /// Which resources are the scope's: every IPv4 range of the account's VPCs and subnets in a
    /// Region the scope's IPAM OPERATES in ("the operating Regions for an IPAM",
    /// `Ipam$OperatingRegions`), placed in the IPAM's private default scope unless a transfer moved
    /// it ([`IpamResourceCidrSettings::ipam_scope_id`]) — the private space of the account, which
    /// is what a private scope contains ("the private scope is intended for all private IP address
    /// space", `IpamScope`).
    ///
    /// A READ ONLY: nothing is stored, and the states below follow from the resources, the pools of
    /// the scope and what a caller asked IPAM to ignore, so two callers reading the same store are
    /// told the same thing.
    pub fn ipam_resource_cidrs_in_scope(&self, scope_id: &str) -> Vec<IpamResourceCidr> {
        let Some(ipam_id) = self.ipam_id_of_scope(scope_id) else {
            return Vec::new();
        };
        let Some(ipam) = self.ipams.get(&ipam_id) else {
            return Vec::new();
        };
        // Where a resource CIDR nothing has transferred sits: its IPAM's private default scope.
        let home_scope_id = ipam.private_default_scope_id.clone().unwrap_or_default();
        let operating_regions: Vec<&str> = ipam
            .operating_regions
            .iter()
            .flatten()
            .filter_map(|operating| operating.region_name.as_deref())
            .collect();

        let mut selected: Vec<IpamResourceCidr> = Vec::new();
        for resource in self.monitored_resources(&operating_regions) {
            // The scope this resource CIDR is in, which is the one question that decides whether it
            // is reported here at all.
            let settings =
                self.ipam_resource_cidr_settings_of(&ipam_id, &resource.resource_id, &resource.resource_cidr);
            let in_scope = settings
                .map(|settings| settings.ipam_scope_id.as_str())
                .unwrap_or(home_scope_id.as_str());
            if in_scope != scope_id {
                continue;
            }
            let monitored = settings.is_none_or(|settings| settings.monitored);
            // "The pool ID for an IPAM resource" (`IpamResourceCidr$IpamPoolId`): the pool of this
            // scope whose space the resource holds. A resource IPAM does not monitor is in no pool
            // — a modification that stopped the monitoring released the allocation.
            let pool_id = monitored
                .then(|| {
                    self.ipam_pool_of_resource_cidr(
                        scope_id,
                        &resource.resource_id,
                        &resource.resource_cidr,
                    )
                })
                .flatten();
            let management_state = if !monitored {
                IPAM_MANAGEMENT_STATE_IGNORED
            } else if pool_id.is_some() {
                IPAM_MANAGEMENT_STATE_MANAGED
            } else {
                IPAM_MANAGEMENT_STATE_UNMANAGED
            };
            // The compliance of a managed range is whether it meets the allocation rules of the
            // pool it came out of; a range no pool hands out has no rules to meet, and one IPAM
            // does not monitor is not evaluated at all.
            let pool = pool_id
                .as_deref()
                .and_then(|pool_id| self.ipam_pools.get(pool_id));
            let compliance_status = if !monitored {
                IPAM_COMPLIANCE_STATUS_IGNORED
            } else if let Some(pool) = pool {
                if resource_cidr_meets_pool_rules(
                    pool,
                    &resource.resource_cidr,
                    &resource.resource_tags,
                ) {
                    IPAM_COMPLIANCE_STATUS_COMPLIANT
                } else {
                    IPAM_COMPLIANCE_STATUS_NONCOMPLIANT
                }
            } else {
                IPAM_COMPLIANCE_STATUS_UNMANAGED
            };
            selected.push(IpamResourceCidr {
                ipam_id: Some(ipam_id.clone()),
                ipam_scope_id: Some(scope_id.to_string()),
                ipam_pool_id: pool_id,
                resource_region: Some(resource.resource_region.clone()),
                resource_owner_id: resource.resource_owner_id.clone(),
                resource_id: Some(resource.resource_id.clone()),
                resource_name: resource.resource_name.clone(),
                resource_cidr: Some(resource.resource_cidr.clone()),
                resource_type: Some(resource.resource_type.to_string()),
                resource_tags: Some(resource.resource_tags.clone()),
                ip_usage: resource.ip_usage,
                compliance_status: Some(compliance_status.to_string()),
                management_state: Some(management_state.to_string()),
                // Filled in below: whether a range overlaps another is a fact about the whole
                // scope, not about one row of it.
                overlap_status: None,
                vpc_id: resource.vpc_id.clone(),
                availability_zone_id: resource.availability_zone_id.clone(),
            });
        }
        mark_overlap_statuses(&mut selected);
        selected
    }

    /// The resource CIDR of the scope `scope_id` for the resource `resource_id` and the range
    /// `cidr`, or `None` when the scope holds no such row.
    ///
    /// The single-row reading of [`Self::ipam_resource_cidrs_in_scope`], so what a modification
    /// answers with is exactly the row the report carries.
    pub fn ipam_resource_cidr_in_scope(
        &self,
        scope_id: &str,
        resource_id: &str,
        cidr: &str,
    ) -> Option<IpamResourceCidr> {
        self.ipam_resource_cidrs_in_scope(scope_id)
            .into_iter()
            .find(|view| {
                view.resource_id.as_deref() == Some(resource_id)
                    && view.resource_cidr.as_deref() == Some(cidr)
            })
    }

    /// Every resource of the account IPAM monitors in `operating_regions`, as the store holds it.
    ///
    /// A VPC is monitored through each IPv4 range it HOLDS ([`held_ipv4_cidr_blocks`], the one
    /// reading of a VPC's own address space), and a subnet through its own range; both are placed
    /// in the Region of the VPC they belong to ([`Self::vpc_region`], the one reading of "which
    /// Region is this VPC in"), so a resource of a Region the IPAM does not operate in is monitored
    /// by nothing.
    fn monitored_resources(&self, operating_regions: &[&str]) -> Vec<MonitoredResource> {
        let mut resources: Vec<MonitoredResource> = Vec::new();
        for (vpc_id, vpc) in &self.vpcs {
            let Some(region) = self.vpc_region(vpc_id) else {
                continue;
            };
            if !operating_regions.contains(&region) {
                continue;
            }
            let tags = vpc.tags.as_deref().unwrap_or_default();
            for cidr in held_ipv4_cidr_blocks(vpc) {
                resources.push(MonitoredResource {
                    resource_id: vpc_id.clone(),
                    resource_type: IPAM_RESOURCE_TYPE_VPC,
                    // "For resources that are VPCs, this is the percentage of IP address space in
                    // the VPC that's taken up by subnet CIDRs" (`IpamResourceCidr$IpUsage`).
                    ip_usage: self.vpc_subnet_ip_usage(vpc_id, &cidr),
                    resource_cidr: cidr,
                    resource_region: region.to_string(),
                    resource_owner_id: vpc.owner_id.clone(),
                    resource_name: resource_name_of(tags),
                    resource_tags: ipam_resource_tags_of(tags),
                    vpc_id: Some(vpc_id.clone()),
                    // A VPC spans every zone of its Region, so it is in no one zone.
                    availability_zone_id: None,
                });
            }
        }
        for (subnet_id, subnet) in &self.subnets {
            let Some(vpc_id) = subnet.vpc_id.as_deref() else {
                continue;
            };
            let Some(region) = self.vpc_region(vpc_id) else {
                continue;
            };
            if !operating_regions.contains(&region) {
                continue;
            }
            let Some(cidr) = subnet
                .cidr_block
                .as_deref()
                .filter(|cidr| !cidr.is_empty())
            else {
                continue;
            };
            let tags = subnet.tags.as_deref().unwrap_or_default();
            resources.push(MonitoredResource {
                resource_id: subnet_id.clone(),
                resource_type: IPAM_RESOURCE_TYPE_SUBNET,
                ip_usage: subnet_ip_usage(subnet, cidr),
                resource_cidr: cidr.to_string(),
                resource_region: region.to_string(),
                resource_owner_id: subnet.owner_id.clone(),
                resource_name: resource_name_of(tags),
                resource_tags: ipam_resource_tags_of(tags),
                vpc_id: Some(vpc_id.to_string()),
                availability_zone_id: subnet.availability_zone_id.clone(),
            });
        }
        resources
    }

    /// "The percentage of IP address space in the VPC that's taken up by subnet CIDRs"
    /// (`IpamResourceCidr$IpUsage`), for the range `cidr` of the VPC `vpc_id`: the addresses of the
    /// subnets that lie INSIDE that range over the addresses the range covers.
    ///
    /// Inside that range, because a VPC can hold several ranges and a subnet's addresses are taken
    /// out of exactly one of them. `None` for a range this store cannot read as a CIDR block, which
    /// is a usage nothing could be computed from rather than a usage of none.
    fn vpc_subnet_ip_usage(&self, vpc_id: &str, cidr: &str) -> Option<f64> {
        let (_, first, last) = cidr_block_bounds(cidr)?;
        let covered = last.checked_sub(first)?.checked_add(1)?;
        let taken: u128 = self
            .subnets
            .values()
            .filter(|subnet| subnet.vpc_id.as_deref() == Some(vpc_id))
            .filter_map(|subnet| subnet.cidr_block.as_deref())
            .filter_map(cidr_block_bounds)
            .filter(|(_, subnet_first, subnet_last)| {
                *subnet_first >= first && *subnet_last <= last
            })
            .filter_map(|(_, subnet_first, subnet_last)| {
                subnet_last.checked_sub(subnet_first)?.checked_add(1)
            })
            .sum();
        Some(taken as f64 / covered as f64)
    }

    /// Record that the IPAM `ipam_id` monitors the resource CIDR `resource_id` plus `cidr` — or
    /// does not — with the row sitting in the scope `scope_id`, and answer the resource CIDR as the
    /// store then holds it.
    ///
    /// `None` when nothing was written: the store is at [`MAX_STORED_RECORDS_PER_STORE`] and this
    /// resource CIDR has no record yet, so a writer that forgot
    /// [`Self::has_ipam_resource_cidr_setting_capacity`] cannot grow the store past its ceiling
    /// either; or the scope holds no such resource CIDR to report back.
    ///
    /// "If set to false, the resource will not be tracked for overlap, it cannot be auto-imported
    /// into a pool, and it will be REMOVED FROM ANY POOL IT HAS AN ALLOCATION IN"
    /// (`ModifyIpamResourceCidr`): a resource IPAM stops monitoring gives the space back, so the
    /// allocation that held its range in a pool of the scope goes — through the one release of such
    /// an allocation, so the pool reports the range as free from this moment on.
    pub fn set_ipam_resource_cidr_monitoring(
        &mut self,
        ipam_id: &str,
        scope_id: &str,
        resource_id: &str,
        cidr: &str,
        monitored: bool,
    ) -> Option<IpamResourceCidr> {
        let key: IpamResourceCidrKey = (
            ipam_id.to_string(),
            resource_id.to_string(),
            cidr.to_string(),
        );
        let settings = IpamResourceCidrSettings {
            monitored,
            ipam_scope_id: scope_id.to_string(),
        };
        match self
            .ipam_resource_cidr_settings
            .get(&key)
            .map(|held| held.ipam_scope_id.len())
        {
            // A resource CIDR this IPAM already holds a record for is MODIFIED, which stores no
            // further record: only the scope the row sits in can differ in size, so that is the
            // only charge that changes.
            Some(held_scope_bytes) => {
                self.release_stored_bytes(held_scope_bytes);
                self.add_stored_bytes(settings.ipam_scope_id.len());
                self.ipam_resource_cidr_settings.insert(key, settings);
            }
            None => {
                if !self.has_ipam_resource_cidr_setting_capacity() {
                    return None;
                }
                self.add_stored_bytes(ipam_resource_cidr_settings_payload_bytes(&key, &settings));
                self.ipam_resource_cidr_settings.insert(key, settings);
            }
        }
        if !monitored {
            for pool_id in self.ipam_pool_ids_in_scope(scope_id) {
                self.release_ipam_pool_allocation_of(&pool_id, resource_id, cidr);
            }
        }
        self.ipam_resource_cidr_in_scope(scope_id, resource_id, cidr)
    }
}

/// "The percentage of IPv4 address space in the subnet that's in use"
/// (`IpamResourceCidr$IpUsage`), for the subnet `subnet` holding the range `cidr`: the addresses of
/// the range that are NOT available to a network interface, over the addresses the range covers.
///
/// Read off `Subnet$AvailableIpAddressCount`, which every writer of a subnet keeps as the addresses
/// an interface may still take ([`available_ipv4_addresses`] and the writers that draw it down), so
/// this usage and that count cannot disagree. `None` for a range this store cannot read as a CIDR
/// block.
fn subnet_ip_usage(subnet: &Subnet, cidr: &str) -> Option<f64> {
    let covered = cidr_block_address_count(cidr)?;
    let available = u128::try_from(subnet.available_ip_address_count.unwrap_or_default()).ok()?;
    let in_use = covered.saturating_sub(available);
    Some(in_use as f64 / covered as f64)
}

/// Whether the range `cidr` held by a resource carrying `tags` meets the ALLOCATION RULES of the
/// pool it came out of — which is what a managed resource CIDR's `ComplianceStatus` reports.
///
/// The rules are the pool's own: the netmask length bounds it hands space out within
/// (`IpamPool$AllocationMinNetmaskLength` / `$AllocationMaxNetmaskLength`) and the tags it requires
/// of the resources that use its CIDRs ("resources that do not have these tags will not be allowed
/// to allocate space from the pool", `IpamPool$AllocationResourceTags`) — the same rules
/// [`require_resource_allocation_cidr`] and [`require_pool_allocation_resource_tags`] hold a
/// resource to when it TAKES space out of the pool, asked here of a resource that already holds it,
/// because a pool's rules can change after an allocation was made.
pub fn resource_cidr_meets_pool_rules(
    pool: &IpamPool,
    cidr: &str,
    tags: &[IpamResourceTag],
) -> bool {
    if let Some(length) = cidr_block_prefix_length(cidr).and_then(|length| i32::try_from(length).ok())
    {
        if pool
            .allocation_min_netmask_length
            .is_some_and(|min| length < min)
        {
            return false;
        }
        if pool
            .allocation_max_netmask_length
            .is_some_and(|max| length > max)
        {
            return false;
        }
    }
    pool.allocation_resource_tags
        .iter()
        .flatten()
        .all(|required| {
            tags.iter().any(|tag| {
                tag.key.as_deref().unwrap_or_default() == required.key.as_deref().unwrap_or_default()
                    && tag.value.as_deref().unwrap_or_default()
                        == required.value.as_deref().unwrap_or_default()
            })
        })
}

/// One row IPAM tracks for overlap, with its range read into numbers ONCE: the comparison below is
/// integer work, so no range is parsed per comparison.
struct TrackedRange<'a> {
    /// Which row of the reported set this is.
    index: usize,
    /// The first and last address the range covers.
    first: u128,
    last: u128,
    /// What the row is, which decides whether two ranges are compared at all.
    resource_type: Option<&'a str>,
    resource_id: Option<&'a str>,
}

/// Give every reported resource CIDR of one scope its `OverlapStatus`.
///
/// "The overlap status tells you if the CIDR for a resource overlaps with another CIDR in the
/// scope" (`IpamResourceCidr$OverlapStatus`): a range that shares an address with another
/// RESOURCE's range of the same kind is overlapping, and a range IPAM does not monitor is neither —
/// "if set to false, the resource will not be tracked for overlap" (`ModifyIpamResourceCidr`), so
/// it is `ignored` and it makes no other range overlapping either.
///
/// Of the same kind, because a subnet's range lies inside its own VPC's by construction: the
/// question the status answers is whether two resources of one kind claim the same addresses.
///
/// The tracked ranges are sorted by the address they start at, so each one is compared only with
/// the ranges that START inside it and the walk stops at the first that starts past its end: every
/// comparison that is made either finds a pair that really overlaps or ends a row's walk, which is
/// what keeps this linear in the rows plus the overlapping pairs rather than in their product.
fn mark_overlap_statuses(selected: &mut [IpamResourceCidr]) {
    let mut overlapping: BTreeSet<usize> = BTreeSet::new();
    {
        let mut tracked: Vec<TrackedRange<'_>> = selected
            .iter()
            .enumerate()
            .filter(|(_, view)| {
                // A range IPAM does not monitor is not tracked for overlap at all.
                view.management_state.as_deref() != Some(IPAM_MANAGEMENT_STATE_IGNORED)
            })
            .filter_map(|(index, view)| {
                let (_, first, last) = cidr_block_bounds(view.resource_cidr.as_deref()?)?;
                Some(TrackedRange {
                    index,
                    first,
                    last,
                    resource_type: view.resource_type.as_deref(),
                    resource_id: view.resource_id.as_deref(),
                })
            })
            .collect();
        tracked.sort_by_key(|range| (range.first, range.last));
        for (position, range) in tracked.iter().enumerate() {
            for other in tracked.iter().skip(position + 1) {
                if other.first > range.last {
                    break;
                }
                if other.resource_type != range.resource_type
                    || other.resource_id == range.resource_id
                {
                    continue;
                }
                overlapping.insert(range.index);
                overlapping.insert(other.index);
            }
        }
    }
    for (index, view) in selected.iter_mut().enumerate() {
        let status = if view.management_state.as_deref() == Some(IPAM_MANAGEMENT_STATE_IGNORED) {
            IPAM_OVERLAP_STATUS_IGNORED
        } else if overlapping.contains(&index) {
            IPAM_OVERLAP_STATUS_OVERLAPPING
        } else {
            IPAM_OVERLAP_STATUS_NONOVERLAPPING
        };
        view.overlap_status = Some(status.to_string());
    }
}
