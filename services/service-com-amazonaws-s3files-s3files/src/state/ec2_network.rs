// SPDX-License-Identifier: CC0-1.0

//! The network a mount target is PLACED in, as the service that OWNS that network reports
//! it.
//!
//! A subnet, the VPC it belongs to, its Availability Zone, its address range, the VPC's
//! default security group and the elastic network interface a mount target answers on are
//! not resources of this service: they are Amazon EC2's. A client builds its network with
//! `ec2:CreateVpc`/`ec2:CreateSubnet`, or uses the default VPC EC2 materialises for every
//! Region of the account, and then names one of those subnets in
//! `CreateMountTargetRequest$subnetId`. So EC2 is the ONE answer to "which subnets does the
//! account have", and the identifier a request names is resolved there and nowhere else —
//! reached the way any emulated service reaches another, through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate and nothing leaves the process.
//!
//! Everything a mount target reports about its placement — `vpcId`, `availabilityZoneId`,
//! `networkInterfaceId`, the address that interface holds, the security groups it sits in
//! when the request named none — is read off the records EC2 answered with, never built
//! here. The interface itself is CREATED there
//! ([`create_mount_target_interface`]) and released there again when the mount target goes
//! ([`release_mount_target_interface`]): an interface this service only recorded would hold
//! no address of the account's network, answer no `DescribeNetworkInterfaces` and block no
//! `DeleteSubnet`.
//!
//! FAIL-CLOSED on the subnet: a hop that could not be made resolves nothing, and a mount
//! target cannot be placed in a subnet the account cannot be shown to have, because this
//! operation PLACES it there and REPORTS that subnet's VPC, zone and address — with no
//! record there is nothing to place it in or to report. The default security group is the
//! opposite case: `securityGroups` is an optional member describing the interface, so a
//! group that could not be resolved is simply not reported and nothing is refused for it.
//!
//! Nothing is cached: a subnet created a moment ago resolves at once, and one that was
//! deleted stops resolving at once.

use super::SubnetPlacement;

/// The `DescribeSubnets` filter that selects subnets BY ID (`subnet-id`, listed by that
/// operation's own `Filters` documentation).
///
/// A filter is what asks the question rather than `SubnetIds`: an identifier the account
/// has no subnet for is then an absent row instead of a failure of the whole call.
const SUBNET_ID_FILTER: &str = "subnet-id";

/// The `DescribeSecurityGroups` filter that selects the groups of ONE VPC (`vpc-id`).
const VPC_ID_FILTER: &str = "vpc-id";

/// The `DescribeSecurityGroups` filter that selects a group by name (`group-name`).
const GROUP_NAME_FILTER: &str = "group-name";

/// The name of the group every VPC has from the moment it is created ("Your VPC comes with
/// a default security group", Amazon VPC User Guide), which is the group
/// `CreateMountTargetRequest$securityGroups` documents as the fall-back: "If not
/// specified, the default security group for the subnet's VPC is used."
const DEFAULT_SECURITY_GROUP_NAME: &str = "default";

/// An AWS SDK client for EC2 over the in-process callback into this same emulator.
///
/// `region` is the request's own signing region, so the question is asked in the Region
/// the caller addressed. The endpoint URL is only the address the callback's transport
/// routes on IN PROCESS: no socket is opened, no name is resolved and nothing leaves this
/// process. The credentials are the callback's, which sign this in-process hop and never
/// appear in a response.
///
/// The Region a request is signed for is CALLER-SUPPLIED text and here it ends up in an
/// endpoint URL's authority, so it is read as a Region LABEL
/// (`service_common::arn::region_label`) instead of being interpolated as given: `None`
/// for a value that would name an authority the caller chose rather than the peer service
/// this emulator asks.
fn ec2(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_ec2::Client> {
    let region = ::service_common::arn::region_label(region)?;
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_ec2::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://ec2.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client
        // is not given. The same goes for caching the identity: the credentials the
        // callback hands over are used directly.
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}

/// The subnet `subnet_id` names, as the account's own network reports it in `region`, or
/// `None` when the account has no such subnet — or when EC2 could not be asked at all,
/// which is evidence of nothing and therefore resolves nothing (see this module's own
/// documentation).
///
/// ONE hop, asking about the one identifier the request named.
pub async fn resolve_subnet(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_id: &str,
) -> Option<SubnetPlacement> {
    let client = ec2(ctx, region).or_else(|| {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        None
    })?;
    let by_id = ::aws_sdk_ec2::types::Filter::builder()
        .name(SUBNET_ID_FILTER)
        .values(subnet_id)
        .build();
    let described = match client.describe_subnets().filters(by_id).send().await {
        Ok(described) => described,
        Err(error) => {
            // At `warn`: a mount target cannot be placed when the owner of subnets cannot
            // be asked, so a deployment where this hop fails is one where no mount target
            // can be created at all — worth seeing, and it IS why the identifier did not
            // resolve.
            ::tracing::warn!(
                %region,
                %subnet_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the subnet a mount target names could not be resolved against EC2"
            );
            return None;
        }
    };
    let subnet = described
        .subnets()
        .iter()
        .find(|subnet| subnet.subnet_id() == Some(subnet_id))?;
    Some(SubnetPlacement {
        subnet_id: subnet_id.to_string(),
        // EC2 reports the VPC a subnet belongs to, the zone it sits in and the blocks it
        // addresses out of; a record missing one of them carries what this service can
        // report of it, and nothing is invented for it here.
        vpc_id: subnet.vpc_id().unwrap_or_default().to_string(),
        availability_zone_id: subnet.availability_zone_id().unwrap_or_default().to_string(),
        availability_zone_name: subnet.availability_zone().unwrap_or_default().to_string(),
        cidr_block: subnet.cidr_block().map(str::to_string),
        ipv6_cidr_block: subnet
            .ipv6_cidr_block_association_set()
            .iter()
            .find_map(|association| association.ipv6_cidr_block())
            .map(str::to_string),
    })
}

/// What a mount target's network interface is asked to carry out of its subnet's IPv6
/// range.
#[derive(Debug, Clone)]
pub enum RequestedIpv6 {
    /// The address the request named.
    Named(String),
    /// Whatever address the owner assigns out of the subnet's own range.
    Assigned,
}

/// One network interface this service placed for a mount target, exactly as EC2 — the
/// service that owns network interfaces — answered the create that placed it.
///
/// Only what EC2 itself reported is kept: the identifier `networkInterfaceId` reports and
/// the delete releases, and the addresses the OWNER assigned out of the subnet's ranges,
/// which are the addresses `ipv4Address` / `ipv6Address` report. Nothing here is counted or
/// chosen by this service.
#[derive(Debug, Clone)]
pub struct MountTargetInterface {
    pub network_interface_id: String,
    pub ipv4_address: Option<String>,
    pub ipv6_address: Option<String>,
    /// The security groups the interface SITS IN, as the owner reported them once it was
    /// created — which is what the mount target reports, so the member can never name a
    /// group the interface is not in (a request that named none gets the VPC's default
    /// group, and the owner is the one that applied it).
    pub security_groups: Vec<String>,
}

/// Why the owner of network interfaces placed none, as its own answer says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InterfaceNotPlaced {
    /// The owner reports the address the request named as one something in the account
    /// already holds (`InvalidIPAddress.InUse`).
    AddressInUse,
    /// The owner will not assign the address the request named at all
    /// (`InvalidParameterValue`) — it is not one of the subnet's assignable addresses.
    AddressRefused,
    /// The owner has no security group of the subnet's VPC under one of the ids the
    /// request named (`InvalidGroup.NotFound` and its siblings).
    GroupRefused,
    /// The owner no longer holds the subnet the placement resolved against
    /// (`InvalidSubnetID.NotFound` and its siblings).
    SubnetMissing,
    /// Nothing could be placed: the owner refused the create for another reason, or could
    /// not be asked.
    Unavailable,
}

/// EC2's error code for an address something in the account already holds.
const ADDRESS_IN_USE_CODE: &str = "InvalidIPAddress.InUse";

/// EC2's error code for a value it will not accept for a parameter.
const INVALID_PARAMETER_VALUE_CODE: &str = "InvalidParameterValue";

/// The prefix of EC2's error codes for a security group it holds none of
/// (`InvalidGroup.NotFound`, `InvalidGroupId.Malformed`).
const INVALID_GROUP_CODE_PREFIX: &str = "InvalidGroup";

/// The prefix of EC2's error codes for a subnet it holds none of
/// (`InvalidSubnetID.NotFound`, `InvalidSubnetId.Malformed`).
const INVALID_SUBNET_CODE_PREFIX: &str = "InvalidSubnet";

/// The description the network interface of a mount target carries: a sentence naming the
/// service that created it and the file system whose endpoint answers on it, which is what
/// a client reconciling a file system against the owner of network interfaces recognises
/// its interfaces by.
pub fn mount_target_interface_description(file_system_id: &str) -> String {
    format!("S3 Files created network interface for file system {file_system_id}")
}

/// Place the elastic network interface a mount target answers on, in the service that OWNS
/// network interfaces, and report the identifier and addresses EC2 minted for it.
///
/// `CreateMountTargetResponse$networkInterfaceId` is "The ID of the network interface that
/// S3 Files created when it created the mount target", so the create really does provision
/// an EC2 resource: it is created THERE, over the same hop the subnet was resolved through,
/// and this service records only what the owner answered. An interface this service minted
/// itself would answer no `DescribeNetworkInterfaces` and hold no address of the subnet.
///
/// The ADDRESS is the owner's to assign — "If you don't specify an IPv4 address, Amazon EC2
/// selects one for you from the subnet's IPv4 CIDR range"
/// (`CreateNetworkInterfaceRequest$PrivateIpAddress`) — so an address the request named is
/// passed through and anything else is read back off the answer. That is also what decides
/// whether an address is free: the owner holds every address of the subnet, this service
/// holds none of them.
///
/// Called with this service's store RELEASED, so no guard is held across the hop, and only
/// once every refusal that does not depend on the owner has been applied, so a refused
/// create places no interface.
pub async fn create_mount_target_interface(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_id: &str,
    description: &str,
    security_groups: &[String],
    ipv4_address: Option<&str>,
    ipv6: Option<RequestedIpv6>,
) -> Result<MountTargetInterface, InterfaceNotPlaced> {
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!(
            "the request names no Region label, so no network interface can be created for \
             the mount target"
        );
        return Err(InterfaceNotPlaced::Unavailable);
    };
    let mut request = client
        .create_network_interface()
        .subnet_id(subnet_id)
        .description(description)
        .set_groups(Some(security_groups.to_vec()));
    if let Some(address) = ipv4_address {
        request = request.private_ip_address(address);
    }
    match ipv6 {
        Some(RequestedIpv6::Named(address)) => {
            request = request.ipv6_addresses(
                ::aws_sdk_ec2::types::InstanceIpv6Address::builder()
                    .ipv6_address(address)
                    .build(),
            );
        }
        // One address, which the owner picks out of the subnet's IPv6 range.
        Some(RequestedIpv6::Assigned) => request = request.ipv6_address_count(1),
        None => {}
    }
    match request.send().await {
        Ok(output) => {
            let interface = output.network_interface.ok_or(InterfaceNotPlaced::Unavailable)?;
            let network_interface_id = interface
                .network_interface_id
                .ok_or(InterfaceNotPlaced::Unavailable)?;
            Ok(MountTargetInterface {
                network_interface_id,
                ipv4_address: interface.private_ip_address,
                ipv6_address: interface
                    .ipv6_addresses
                    .iter()
                    .flatten()
                    .find_map(|address| address.ipv6_address.clone()),
                security_groups: interface
                    .groups
                    .iter()
                    .flatten()
                    .filter_map(|group| group.group_id.clone())
                    .collect(),
            })
        }
        Err(error) => {
            let code = ::aws_smithy_types::error::metadata::ProvideErrorMetadata::code(&error)
                .unwrap_or_default()
                .to_string();
            // At `warn`: a mount target that cannot be given an interface cannot be
            // created at all, and this IS why.
            ::tracing::warn!(
                %region,
                %subnet_id,
                %code,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the network interface for a mount target could not be created in EC2"
            );
            // The owner's own verdict decides which refusal the caller is answered with.
            Err(match code.as_str() {
                ADDRESS_IN_USE_CODE => InterfaceNotPlaced::AddressInUse,
                INVALID_PARAMETER_VALUE_CODE => InterfaceNotPlaced::AddressRefused,
                group if group.starts_with(INVALID_GROUP_CODE_PREFIX) => {
                    InterfaceNotPlaced::GroupRefused
                }
                subnet if subnet.starts_with(INVALID_SUBNET_CODE_PREFIX) => {
                    InterfaceNotPlaced::SubnetMissing
                }
                _ => InterfaceNotPlaced::Unavailable,
            })
        }
    }
}

/// Release the network interface this service placed for a mount target: delete it in the
/// service that OWNS network interfaces.
///
/// The interface exists FOR that one mount target — nothing else of the account was ever
/// given it — so the delete that takes the mount target away takes its interface with it.
/// An interface left behind would go on holding an address of the caller's subnet and go on
/// blocking the caller's own `DeleteSubnet`. The identifier is the one the create recorded
/// off EC2's own answer, so this deletes the interface EC2 minted and never an identifier
/// of this service's own.
///
/// Called with this service's store RELEASED, so no guard is held across the hop. A hop the
/// owner refused is reported at `warn`: the mount target the caller asked to delete is
/// gone either way, and nothing of this service still claims the interface.
pub async fn release_mount_target_interface(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    network_interface_id: &str,
) {
    if network_interface_id.is_empty() {
        return;
    }
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!(
            "the request names no Region label, so the mount target's network interface \
             cannot be released"
        );
        return;
    };
    if let Err(error) = client
        .delete_network_interface()
        .network_interface_id(network_interface_id)
        .send()
        .await
    {
        ::tracing::warn!(
            %region,
            %network_interface_id,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the network interface of a deleted mount target could not be released in EC2"
        );
    }
}

/// Put the mount target's network interface in `security_groups`, in the service that OWNS
/// that interface.
///
/// `UpdateMountTargetRequest$securityGroups` is "An array of VPC security group IDs to
/// associate with the mount target's network interface. This replaces the existing security
/// groups", so the groups are an attribute of an EC2 resource: the replacement is applied
/// THERE (`ec2:ModifyNetworkInterfaceAttribute`, whose own `Groups` is "the new set of
/// groups you specify replaces the current set"), and what this service reports is then the
/// set the interface really carries rather than a value only it holds.
///
/// `false` when the owner did not apply it — a group it holds none of, or a set it will not
/// put an interface in — which is what the caller is refused with: a mount target must not
/// report groups its interface does not sit in.
///
/// An EMPTY set is never asked of the owner: "You must specify at least one group"
/// (`ModifyNetworkInterfaceAttribute$Groups`), so there is nothing it could apply, and the
/// caller is refused before this is reached.
pub async fn apply_mount_target_interface_groups(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    network_interface_id: &str,
    security_groups: &[String],
) -> bool {
    if security_groups.is_empty() || network_interface_id.is_empty() {
        return false;
    }
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!(
            "the request names no Region label, so the mount target's security groups \
             cannot be applied to its network interface"
        );
        return false;
    };
    match client
        .modify_network_interface_attribute()
        .network_interface_id(network_interface_id)
        .set_groups(Some(security_groups.to_vec()))
        .send()
        .await
    {
        Ok(_) => true,
        Err(error) => {
            ::tracing::warn!(
                %region,
                %network_interface_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the mount target's security groups could not be applied to its network \
                 interface in EC2"
            );
            false
        }
    }
}

/// The `DescribeSecurityGroups` filter that selects groups BY ID (`group-id`).
///
/// A filter, not `GroupIds`: an id the account has no group for is then an absent row
/// instead of a failure of the whole call, so every id the request named is still resolved
/// against what EC2 does hold.
const GROUP_ID_FILTER: &str = "group-id";

/// The most security-group ids ONE hop asks about, as values of one `group-id` filter.
///
/// The owner evaluates a filter against every group the account stores, so this caps the
/// identifiers asked about per hop. `SecurityGroups` is `@length(0, 100)` and that bound is
/// enforced before the handler runs, so a request normally fits in two hops.
const GROUP_IDS_PER_HOP: usize = 64;

/// The ids of `security_groups` that EC2 reports in a VPC OTHER than `vpc_id`.
///
/// `CreateMountTargetRequest$securityGroups` and `UpdateMountTargetRequest$securityGroups`
/// both state the requirement — "All security groups must belong to the same VPC as the
/// subnet" — and a group's VPC is a fact of the group, which EC2 owns. So the refusal is
/// GROUNDED in what EC2 answered: an id it reports a group for in another VPC is refused,
/// while an id it reports no group for, or a hop that could not be made at all, is evidence
/// of nothing and refuses nothing — the same reading `CreateFileSystem`'s bucket gate
/// applies to S3.
pub async fn security_groups_outside_vpc(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: &str,
    security_groups: &[String],
) -> Vec<String> {
    if vpc_id.is_empty() || security_groups.is_empty() {
        return Vec::new();
    }
    let Some(client) = ec2(ctx, region) else {
        return Vec::new();
    };
    let mut elsewhere = Vec::new();
    for chunk in security_groups.chunks(GROUP_IDS_PER_HOP) {
        let by_id = ::aws_sdk_ec2::types::Filter::builder()
            .name(GROUP_ID_FILTER)
            .set_values(Some(chunk.to_vec()))
            .build();
        match client.describe_security_groups().filters(by_id).send().await {
            Ok(described) => elsewhere.extend(
                described
                    .security_groups()
                    .iter()
                    .filter(|group| group.vpc_id().is_some_and(|held| held != vpc_id))
                    .filter_map(|group| group.group_id().map(str::to_string)),
            ),
            Err(error) => {
                ::tracing::warn!(
                    %region,
                    %vpc_id,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the security groups a mount target names could not be resolved against EC2"
                );
            }
        }
    }
    elsewhere
}

/// The default security group of `vpc_id`, as EC2 reports it: the group a mount target's
/// network interface is placed in when the request named none.
///
/// Empty when EC2 holds no such group or could not be asked — the group is reported, not
/// gated on, so nothing is refused for a hop that answered nothing.
pub async fn default_security_groups(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: &str,
) -> Vec<String> {
    if vpc_id.is_empty() {
        return Vec::new();
    }
    let Some(client) = ec2(ctx, region) else {
        return Vec::new();
    };
    let in_vpc = ::aws_sdk_ec2::types::Filter::builder()
        .name(VPC_ID_FILTER)
        .values(vpc_id)
        .build();
    let named_default = ::aws_sdk_ec2::types::Filter::builder()
        .name(GROUP_NAME_FILTER)
        .values(DEFAULT_SECURITY_GROUP_NAME)
        .build();
    match client
        .describe_security_groups()
        .filters(in_vpc)
        .filters(named_default)
        .send()
        .await
    {
        Ok(described) => described
            .security_groups()
            .iter()
            .filter_map(|group| group.group_id().map(str::to_string))
            .collect(),
        Err(error) => {
            ::tracing::warn!(
                %region,
                %vpc_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the VPC's default security group could not be resolved against EC2"
            );
            Vec::new()
        }
    }
}
