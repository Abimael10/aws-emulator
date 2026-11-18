// SPDX-License-Identifier: CC0-1.0

//! The subnet a mount target is PLACED in, as the service that OWNS that subnet reports it.
//!
//! A subnet, the VPC it belongs to, its Availability Zone and its address range are not
//! resources of this service: they are Amazon EC2's. A client builds its network with
//! `ec2:CreateVpc`/`ec2:CreateSubnet`, or uses the default VPC EC2 materialises for every
//! Region of the account, and then names one of those subnets in
//! `CreateMountTarget$SubnetId`. So EC2 is the ONE answer to "which subnets does the account
//! have", and the identifier a request names is resolved there and nowhere else — reached the
//! way any emulated service reaches another, through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate and nothing leaves the process.
//!
//! Everything a mount target reports about its placement — `VpcId`, `AvailabilityZoneName`,
//! `AvailabilityZoneId`, and the `IpAddress`/`Ipv6Address` it "can be mounted by using" — is
//! read off the record EC2 answered with, never built here. In particular the address comes
//! out of the block EC2 reports for that subnet, which is what makes it an address the
//! instances in the subnet can reach.
//!
//! FAIL-CLOSED: a hop that could not be made resolves nothing, and a mount target cannot be
//! placed in a subnet the account cannot be shown to have, because this operation PLACES it
//! there and REPORTS that subnet's VPC, zone and address — with no record there is nothing to
//! place it in or to report. (A service that only GATES on a subnet existing, keeping nothing
//! of it, refuses nothing on silence instead; `services/common/src/network.rs` states which
//! reading belongs to which.)
//!
//! Nothing is cached: a subnet created a moment ago resolves at once, and one that was
//! deleted stops resolving at once.

use std::net::{Ipv4Addr, Ipv6Addr};

/// The `DescribeSubnets` filter that selects subnets BY ID (`subnet-id`, listed by that
/// operation's own `Filters` documentation).
///
/// A filter is what asks the question rather than `SubnetIds`: an identifier the account has
/// no subnet for is then an absent row instead of a failure of the whole call.
const SUBNET_ID_FILTER: &str = "subnet-id";

/// How many addresses at the START of a subnet's CIDR block carry no resource of the account,
/// and how many at its end.
///
/// "AWS reserves five IP addresses in each subnet CIDR block" — the network address, the VPC
/// router, the Amazon DNS address, one reserved for future use, and the broadcast address
/// (Amazon VPC User Guide, "Subnet CIDR blocks"), which is the first four addresses and the
/// last one. So the first address a mount target can hold is the fifth of the block.
const RESERVED_LEADING_ADDRESSES: u32 = 4;

/// One subnet of the account, exactly as Amazon EC2 reports it.
///
/// Every member is the OWNER's value; a member EC2 reported nothing for stays `None` and
/// nothing is invented for it here.
#[derive(Debug, Clone)]
pub struct SubnetPlacement {
    /// The `subnet-…` identifier EC2 minted, which is also the one the request named.
    pub subnet_id: String,
    /// The VPC the subnet belongs to — `MountTargetDescription$VpcId`.
    pub vpc_id: String,
    /// `<region><letter>` — `MountTargetDescription$AvailabilityZoneName`.
    pub availability_zone_name: String,
    /// `<region abbreviation>-az<n>` — `MountTargetDescription$AvailabilityZoneId`.
    pub availability_zone_id: String,
    /// The subnet's IPv4 CIDR block, the range an IPv4 address of the mount target is taken
    /// out of; `None` for a subnet EC2 reports none for.
    pub cidr_block: Option<String>,
    /// The subnet's IPv6 CIDR block, the range an IPv6 address of the mount target is taken
    /// out of; `None` for a subnet EC2 reports none for.
    pub ipv6_cidr_block: Option<String>,
}

/// An AWS SDK client for EC2 over the in-process callback into this same emulator.
///
/// `region` is the request's own signing region, so the question is asked in the Region the
/// caller addressed. The endpoint URL is only the address the callback's transport routes on
/// IN PROCESS: no socket is opened, no name is resolved and nothing leaves this process. The
/// credentials are the callback's, which sign this in-process hop and never appear in a
/// response.
///
/// The Region a request is signed for is CALLER-SUPPLIED text and here it ends up in an
/// endpoint URL's authority, so it is read as a Region LABEL
/// (`service_common::arn::region_label`, the one place this tree states that grammar) instead
/// of being interpolated as given: `None` for a value that would name an authority the caller
/// chose rather than the peer service this emulator asks.
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
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The same goes for caching the identity: the credentials the callback
        // hands over are used directly.
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
/// `None` when the account has no such subnet — or when EC2 could not be asked at all, which
/// is evidence of nothing and therefore resolves nothing (see this module's documentation).
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
            // At `warn`: a mount target cannot be placed when the owner of subnets cannot be
            // asked, so a deployment where this hop fails is one where no mount target can be
            // created at all — worth seeing, and it IS why the identifier did not resolve.
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
        vpc_id: subnet.vpc_id().unwrap_or_default().to_string(),
        availability_zone_name: subnet.availability_zone().unwrap_or_default().to_string(),
        availability_zone_id: subnet.availability_zone_id().unwrap_or_default().to_string(),
        cidr_block: subnet.cidr_block().map(str::to_string),
        ipv6_cidr_block: subnet
            .ipv6_cidr_block_association_set()
            .iter()
            .find_map(|association| association.ipv6_cidr_block())
            .map(str::to_string),
    })
}

/// The `ordinal`-th address a subnet addressing out of `cidr_block` can assign, counting from
/// its first assignable one ([`RESERVED_LEADING_ADDRESSES`]).
///
/// THE one place this service turns a subnet's IPv4 block into an address of a mount target,
/// so every address it reports lies inside the block EC2 reported for the subnet the mount
/// target was placed in. `None` when `cidr_block` is no IPv4 CIDR block, or when the block has
/// no such address — one counted past its last assignable address.
#[must_use]
pub fn assignable_ipv4(cidr_block: &str, ordinal: u32) -> Option<String> {
    let block: ::ipnet::Ipv4Net = cidr_block.parse().ok()?;
    let address = u32::from(block.network())
        .checked_add(RESERVED_LEADING_ADDRESSES)?
        .checked_add(ordinal)?;
    // The last address of the block is reserved too, so an address that reaches it is past
    // what the block can assign.
    if address >= u32::from(block.broadcast()) {
        return None;
    }
    Some(Ipv4Addr::from(address).to_string())
}

/// The IPv6 counterpart of [`assignable_ipv4`], over the subnet's IPv6 block.
///
/// An IPv6 subnet block reserves the same first four addresses, and it is a `/64` or larger,
/// so the counted address stays inside the block for every ordinal this service reaches.
#[must_use]
pub fn assignable_ipv6(cidr_block: &str, ordinal: u32) -> Option<String> {
    let block: ::ipnet::Ipv6Net = cidr_block.parse().ok()?;
    let address = u128::from(block.network())
        .checked_add(u128::from(RESERVED_LEADING_ADDRESSES))?
        .checked_add(u128::from(ordinal))?;
    let candidate = Ipv6Addr::from(address);
    if !block.contains(&candidate) {
        return None;
    }
    Some(candidate.to_string())
}

/// Whether `address` is one `cidr_block` can assign to a mount target: inside the block, and
/// not one of the addresses the subnet reserves at its ends.
///
/// `CreateMountTarget$IpAddress` is "the IPv4 address within the address range of the
/// specified subnet", so an address outside it is not one the mount target could answer on.
#[must_use]
pub fn ipv4_assignable(cidr_block: &str, address: &str) -> bool {
    let Ok(block) = cidr_block.parse::<::ipnet::Ipv4Net>() else {
        return false;
    };
    let Ok(address) = address.parse::<Ipv4Addr>() else {
        return false;
    };
    let host = u32::from(address);
    block.contains(&address)
        && host >= u32::from(block.network()).saturating_add(RESERVED_LEADING_ADDRESSES)
        && host < u32::from(block.broadcast())
}

/// The IPv6 counterpart of [`ipv4_assignable`]: `CreateMountTarget$Ipv6Address` is "the IPv6
/// address within the address range of the specified subnet".
#[must_use]
pub fn ipv6_assignable(cidr_block: &str, address: &str) -> bool {
    let Ok(block) = cidr_block.parse::<::ipnet::Ipv6Net>() else {
        return false;
    };
    let Ok(address) = address.parse::<Ipv6Addr>() else {
        return false;
    };
    block.contains(&address)
        && u128::from(address)
            >= u128::from(block.network()).saturating_add(u128::from(RESERVED_LEADING_ADDRESSES))
}
