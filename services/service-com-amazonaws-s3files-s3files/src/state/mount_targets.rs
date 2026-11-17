// SPDX-License-Identifier: CC0-1.0

//! The mount-target slice of this service's state: the record a create stores, the rules
//! every operation over a mount target applies, and the ONE projection the responses that
//! report a mount target are built from.
//!
//! A mount target is an endpoint of ONE file system in ONE subnet, so everything about
//! its placement — the VPC, the Availability Zone, the address its network interface
//! takes — comes off the subnet record the service that owns subnets answered with
//! ([`super::ec2_network`]), never from anything derived here.
//!
//! The two rules more than one operation applies live here rather than in a handler:
//! a file system holds at most one mount target per Availability Zone, and an address a
//! mount target takes is one of its subnet's own, free of every other mount target in
//! that subnet.

use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::SystemTime;

use super::{ComAmazonawsS3filesS3filesState, MINTED_ID_HEX};
use crate::ops::types::{
    CreateMountTargetOutput, GetMountTargetOutput, ListMountTargetsDescription,
    UpdateMountTargetOutput,
};

/// What a `ConflictException` names in `resourceType` when the conflicting resource is a
/// mount target.
///
/// `ConflictException$resourceType` is "The type of the resource that caused the
/// conflict", spelled — as for a file system
/// ([`super::RESOURCE_TYPE_FILE_SYSTEM`]) — with the model's own shape name rather
/// than a path segment.
pub const RESOURCE_TYPE_MOUNT_TARGET: &str = "MountTarget";

/// The `message` the modelled `ResourceNotFoundException` reports for a mount target this
/// service does not hold — including one a `DeleteMountTarget` has already removed.
///
/// Same rule as [`super::FILE_SYSTEM_NOT_FOUND_MESSAGE`]: ONE constant sentence naming
/// the KIND of resource that is missing, carrying nothing from the request.
pub const MOUNT_TARGET_NOT_FOUND_MESSAGE: &str = "Mount target does not exist.";

/// The `message` the modelled `ResourceNotFoundException` reports when the account has no
/// subnet with the id the request named, so there is nowhere to place the mount target.
///
/// Same rule again: the condition, not the identifier, is what the sentence states.
pub const SUBNET_NOT_FOUND_MESSAGE: &str = "Subnet does not exist.";

/// `IpAddressType` `IPV4_ONLY`, the value `CreateMountTargetRequest$ipAddressType`
/// documents as the default: "If not specified, IPV4_ONLY is used."
pub const IP_ADDRESS_TYPE_IPV4_ONLY: &str = "IPV4_ONLY";

/// `IpAddressType` `IPV6_ONLY`.
pub const IP_ADDRESS_TYPE_IPV6_ONLY: &str = "IPV6_ONLY";

/// `IpAddressType` `DUAL_STACK`.
pub const IP_ADDRESS_TYPE_DUAL_STACK: &str = "DUAL_STACK";

/// How many mount targets this emulator will hold under one file system.
///
/// The Availability Zone rule below already bounds a file system's mount targets by the
/// number of zones the account's network has, but the store must be bounded by something
/// it states itself rather than by another service's size, and `CreateMountTarget` models
/// `ServiceQuotaExceededException` (HTTP 402) for refusing past a quota. Same reasoning as
/// [`super::MAX_ACCESS_POINTS_PER_FILE_SYSTEM`]: not a fidelity claim about any published
/// AWS quota, just this emulator's own ceiling, set far above the zone count of any
/// Region.
pub const MAX_MOUNT_TARGETS_PER_FILE_SYSTEM: usize = 100;

/// Where a mount target is placed, exactly as the service that OWNS the network reported
/// it. Nothing in this struct is derived by this service.
#[derive(Debug, Clone)]
pub struct SubnetPlacement {
    /// The subnet's own id, as the owner reports it.
    pub subnet_id: String,
    /// The VPC the subnet belongs to, empty when the owner reported none for it.
    pub vpc_id: String,
    /// The subnet's Availability Zone ID (`use1-az1` and the like), empty when the owner
    /// reported none for it.
    pub availability_zone_id: String,
    /// The subnet's Availability Zone NAME (`us-east-1a` and the like), empty when the
    /// owner reported none for it. No response member carries it — the zone a mount target
    /// reports is its ID — but the conflict a second mount target in one zone is refused
    /// with names the zone the way a caller asked for it.
    pub availability_zone_name: String,
    /// The subnet's IPv4 CIDR block, when it has one.
    pub cidr_block: Option<String>,
    /// The subnet's IPv6 CIDR block, when it has one.
    pub ipv6_cidr_block: Option<String>,
}

/// A stored mount target: one file system's endpoint in one subnet.
#[derive(Debug, Clone)]
pub struct MountTarget {
    pub mount_target_id: String,
    pub file_system_id: String,
    pub subnet_id: String,
    /// The VPC the mount target's subnet belongs to, as EC2 reported it.
    pub vpc_id: String,
    /// The Availability Zone ID of that subnet, as EC2 reported it. The key the
    /// one-mount-target-per-zone rule is stated over.
    pub availability_zone_id: String,
    /// The Availability Zone NAME of that subnet, as EC2 reported it — what the zone
    /// conflict's own sentence names ([`mount_target_zone_in_use`]).
    pub availability_zone_name: String,
    pub ipv4_address: Option<String>,
    pub ipv6_address: Option<String>,
    /// The network interface this service created with the mount target: "The ID of the
    /// network interface that S3 Files created when it created the mount target."
    pub network_interface_id: String,
    pub security_groups: Vec<String>,
    pub owner_id: String,
    /// A `LifeCycleState` value: [`super::STATUS_CREATING`] until the clock owes
    /// [`super::STATUS_AVAILABLE`].
    pub status: String,
    pub status_message: Option<String>,
    /// When `creating` becomes `available`. See [`super::PROVISIONING_SPAN`].
    pub available_due_at: SystemTime,
    /// Creation order, so a listing paginates deterministically.
    pub sequence: u64,
}

impl ComAmazonawsS3filesS3filesState {
    /// Mint a `fsmt-` mount target id.
    ///
    /// `MountTargetId` is `^fsmt-[0-9a-f]{17,40}$` with `@length(22, 45)`, so
    /// [`MINTED_ID_HEX`] hex characters after the prefix is the shortest value the
    /// pattern and the length bound both accept. The mount target is this service's own
    /// resource, so this is the one identifier it mints for one: the network interface it
    /// answers on is EC2's, created and identified there
    /// ([`super::create_mount_target_interface`]).
    pub fn mint_mount_target_id(&mut self) -> String {
        format!("fsmt-{}", self.mint_hex("mount-target", MINTED_ID_HEX))
    }

    /// The mount targets bound to `file_system_id`, in creation order — or, when it is
    /// `None`, every stored mount target in that same order, so a filtered and an
    /// unfiltered listing cannot order them differently.
    pub fn mount_targets_in_order(&self, file_system_id: Option<&str>) -> Vec<&MountTarget> {
        let mut ordered: Vec<&MountTarget> = self
            .mount_targets
            .values()
            .filter(|mount_target| {
                file_system_id.is_none_or(|wanted| mount_target.file_system_id == wanted)
            })
            .collect();
        ordered.sort_by_key(|mount_target| mount_target.sequence);
        ordered
    }

    /// The ids of the mount targets bound to `file_system_id`, in creation order.
    pub fn mount_target_ids_of(&self, file_system_id: &str) -> Vec<String> {
        self.mount_targets_in_order(Some(file_system_id))
            .into_iter()
            .map(|mount_target| mount_target.mount_target_id.clone())
            .collect()
    }

    /// The mount target `file_system_id` already holds in the Availability Zone
    /// `availability_zone_id`, if any.
    ///
    /// A mount target is "an endpoint for mounting the S3 File System from compute
    /// resources in a specific Availability Zone and VPC", and every instance of a zone
    /// shares the one endpoint, so a file system has at most one mount target per zone and
    /// a second create in a zone it already has one in is the modelled conflict.
    pub fn mount_target_in_zone(
        &self,
        file_system_id: &str,
        availability_zone_id: &str,
    ) -> Option<&MountTarget> {
        self.mount_targets_in_order(Some(file_system_id))
            .into_iter()
            .find(|mount_target| mount_target.availability_zone_id == availability_zone_id)
    }
}

/// Whether `address` lies inside the IPv4 `cidr_block` — the requirement
/// `CreateMountTargetRequest$ipv4Address` states: "The address must be within the subnet's
/// CIDR block and not already in use."
///
/// `false` when either value is not a well-formed address or block.
pub fn ipv4_within(cidr_block: &str, address: &str) -> bool {
    match (
        cidr_block.parse::<::ipnet::Ipv4Net>(),
        address.parse::<Ipv4Addr>(),
    ) {
        (Ok(block), Ok(address)) => block.contains(&address),
        _ => false,
    }
}

/// Whether `address` lies inside the IPv6 `ipv6_cidr_block`, the same requirement
/// `CreateMountTargetRequest$ipv6Address` states for the IPv6 half.
pub fn ipv6_within(ipv6_cidr_block: &str, address: &str) -> bool {
    match (
        ipv6_cidr_block.parse::<::ipnet::Ipv6Net>(),
        address.parse::<Ipv6Addr>(),
    ) {
        (Ok(block), Ok(address)) => block.contains(&address),
        _ => false,
    }
}

/// Whether an `ipAddressType` asks for an IPv4 address: every value but `IPV6_ONLY` does,
/// and a request that named none takes the documented `IPV4_ONLY` default.
pub fn wants_ipv4(ip_address_type: Option<&str>) -> bool {
    ip_address_type.unwrap_or(IP_ADDRESS_TYPE_IPV4_ONLY) != IP_ADDRESS_TYPE_IPV6_ONLY
}

/// Whether an `ipAddressType` asks for an IPv6 address: `IPV6_ONLY` and `DUAL_STACK` do.
pub fn wants_ipv6(ip_address_type: Option<&str>) -> bool {
    let requested = ip_address_type.unwrap_or(IP_ADDRESS_TYPE_IPV4_ONLY);
    requested == IP_ADDRESS_TYPE_IPV6_ONLY || requested == IP_ADDRESS_TYPE_DUAL_STACK
}

/// The modelled 500 for a mount target whose network interface the service that owns
/// network interfaces did not place.
///
/// A mount target IS its network interface — the endpoint a client mounts the file system
/// at — so when none could be created there is nothing to report and nothing to store:
/// that is this emulator failing to provision, not the request being wrong, which is what
/// the modelled server error says.
pub fn interface_unavailable(subnet_id: &str) -> super::InternalServerException {
    super::InternalServerException {
        error_code: "InternalServerException".to_string(),
        message: Some(format!(
            "The mount target's network interface could not be created in subnet \
             {subnet_id}. Retry your request."
        )),
    }
}

/// The modelled 409 for a create in an Availability Zone the file system already has a
/// mount target in.
///
/// The refusal hands the caller back the mount target that already serves that zone — in
/// `resourceId` and in the sentence — because that endpoint is the one its compute
/// resources in the zone must mount, which is what a caller does next about this
/// condition. The zone is named the way a caller names one when it places a subnet, by
/// zone name.
pub fn mount_target_zone_in_use(existing: &MountTarget) -> super::ConflictException {
    super::conflict(
        &existing.mount_target_id,
        RESOURCE_TYPE_MOUNT_TARGET,
        format!(
            "A mount target {} already exists for this file system in {}'s availability zone.",
            existing.mount_target_id, existing.availability_zone_name
        ),
    )
}

/// The modelled 409 for a `DeleteFileSystem` on a file system that still has mount
/// targets.
///
/// A mount target is the file system's network endpoint, so the file system cannot go while
/// one stands: the sentence states the condition and what the caller does about it, and
/// carries the file system the request named. This is the sentence S3 Files answers with.
pub fn file_system_has_mount_targets(file_system_id: &str) -> super::ConflictException {
    super::conflict(
        file_system_id,
        super::RESOURCE_TYPE_FILE_SYSTEM,
        format!(
            "The file system '{file_system_id}' cannot be deleted because it has mount \
             targets. You must delete the mount targets, and then try again."
        ),
    )
}

/// An optional response member taken off a stored string: the value, or nothing at all
/// when the owner of that fact reported none, so a member is absent rather than reported
/// as an empty string.
fn reported_text(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_string())
}

/// Every member a mount-target response carries, taken off the stored record.
///
/// `CreateMountTargetResponse`, `GetMountTargetResponse` and `UpdateMountTargetResponse`
/// model the identical member set and `ListMountTargetsDescription` all of it but
/// `securityGroups`, so every one of them is built from this one view: no operation can
/// report a mount target differently from the create that stored it.
#[derive(Debug, Clone)]
pub struct MountTargetView {
    pub mount_target_id: String,
    pub file_system_id: String,
    pub subnet_id: String,
    pub vpc_id: Option<String>,
    pub availability_zone_id: Option<String>,
    pub ipv4_address: Option<String>,
    pub ipv6_address: Option<String>,
    pub network_interface_id: Option<String>,
    pub security_groups: Vec<String>,
    pub owner_id: String,
    pub status: String,
    pub status_message: Option<String>,
}

impl From<&MountTarget> for MountTargetView {
    fn from(record: &MountTarget) -> Self {
        Self {
            mount_target_id: record.mount_target_id.clone(),
            file_system_id: record.file_system_id.clone(),
            subnet_id: record.subnet_id.clone(),
            vpc_id: reported_text(&record.vpc_id),
            availability_zone_id: reported_text(&record.availability_zone_id),
            ipv4_address: record.ipv4_address.clone(),
            ipv6_address: record.ipv6_address.clone(),
            network_interface_id: reported_text(&record.network_interface_id),
            security_groups: record.security_groups.clone(),
            owner_id: record.owner_id.clone(),
            status: record.status.clone(),
            status_message: record.status_message.clone(),
        }
    }
}

impl From<MountTargetView> for CreateMountTargetOutput {
    fn from(view: MountTargetView) -> Self {
        Self {
            availability_zone_id: view.availability_zone_id,
            owner_id: view.owner_id,
            mount_target_id: view.mount_target_id,
            file_system_id: Some(view.file_system_id),
            subnet_id: view.subnet_id,
            ipv4_address: view.ipv4_address,
            ipv6_address: view.ipv6_address,
            network_interface_id: view.network_interface_id,
            vpc_id: view.vpc_id,
            // The groups the mount target's interface is in are ALWAYS serialized, as an
            // empty list for one in none, so a client that iterates them never reads a
            // null where a list is guaranteed — the same reporting rule as
            // `super::reported_tags`.
            security_groups: Some(view.security_groups),
            status: Some(view.status),
            status_message: view.status_message,
        }
    }
}

impl From<MountTargetView> for GetMountTargetOutput {
    fn from(view: MountTargetView) -> Self {
        Self {
            availability_zone_id: view.availability_zone_id,
            owner_id: view.owner_id,
            mount_target_id: view.mount_target_id,
            file_system_id: Some(view.file_system_id),
            subnet_id: view.subnet_id,
            ipv4_address: view.ipv4_address,
            ipv6_address: view.ipv6_address,
            network_interface_id: view.network_interface_id,
            vpc_id: view.vpc_id,
            security_groups: Some(view.security_groups),
            status: Some(view.status),
            status_message: view.status_message,
        }
    }
}

impl From<MountTargetView> for UpdateMountTargetOutput {
    fn from(view: MountTargetView) -> Self {
        Self {
            availability_zone_id: view.availability_zone_id,
            owner_id: view.owner_id,
            mount_target_id: view.mount_target_id,
            file_system_id: Some(view.file_system_id),
            subnet_id: view.subnet_id,
            ipv4_address: view.ipv4_address,
            ipv6_address: view.ipv6_address,
            network_interface_id: view.network_interface_id,
            vpc_id: view.vpc_id,
            security_groups: Some(view.security_groups),
            status: Some(view.status),
            status_message: view.status_message,
        }
    }
}

impl From<MountTargetView> for ListMountTargetsDescription {
    fn from(view: MountTargetView) -> Self {
        Self {
            availability_zone_id: view.availability_zone_id,
            file_system_id: Some(view.file_system_id),
            ipv4_address: view.ipv4_address,
            ipv6_address: view.ipv6_address,
            status: Some(view.status),
            status_message: view.status_message,
            mount_target_id: view.mount_target_id,
            network_interface_id: view.network_interface_id,
            owner_id: view.owner_id,
            subnet_id: view.subnet_id,
            vpc_id: view.vpc_id,
        }
    }
}
