// SPDX-License-Identifier: CC0-1.0

//! THE HOP: asking the service that OWNS subnets what the subnets a create names really are.
//!
//! A subnet is not a resource of this service: it is Amazon EC2's. So the ONE answer to what
//! block a subnet carries, which zone it sits in and whether the account has it at all is
//! EC2 — asked the way any emulated service reaches another, over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate and nothing leaves the process (`services/common/src/network.rs` states
//! that contract, and the services that already place a resource in a subnet keep this hop
//! in a module of this name).
//!
//! The VPC those subnets sit in is EC2's resource too, and [`vpc_absence`] is the same hop
//! asked about it: a create that places a directory in a VPC the account does not have is
//! naming a resource that does not exist, which is not something this service can decide on
//! its own either.
//!
//! What the answer is KEPT as, and what a server's address is counted out of it, is
//! [`super::directory_network`]; this module only asks.

use std::collections::BTreeMap;

use super::directory_network::SubnetNetwork;

/// The `DescribeSubnets` filter that selects subnets BY ID (`subnet-id`, listed by that
/// operation's own `Filters` documentation).
///
/// A filter rather than `SubnetIds`: an identifier the account has no subnet for is then an
/// absent row instead of a failure of the whole call, so every identifier the create named
/// is still resolved against what EC2 does hold.
const SUBNET_ID_FILTER: &str = "subnet-id";

/// The most subnet identifiers ONE hop asks about, as values of one `subnet-id` filter —
/// and the most this resolution asks about at all.
///
/// The owner evaluates a filter against every subnet the account stores, so the cost of a hop
/// is (identifiers asked x subnets stored): capping the first factor is what keeps this
/// resolution a CONSTANT times what the account stores rather than something the request
/// chooses. The modelled `SubnetIds` carries no length bound, and a directory's servers sit
/// in [`super::REQUIRED_SUBNETS`] subnets, so a settings block naming more than this is
/// naming far more than any placement can use and the surplus resolves to nothing.
const SUBNET_IDS_PER_HOP: usize = 64;

/// An AWS SDK client for EC2 over the in-process callback into this same emulator.
///
/// Shared with [`super::ec2_security_groups`], the other hop this service makes into the
/// owner of VPC resources, so both ask the same way.
///
/// `region` is the request's own signing region, so the question is asked in the Region the
/// caller addressed. The endpoint URL is only the address the callback's transport routes on
/// IN PROCESS: no socket is opened, no name is resolved and nothing leaves this process. The
/// credentials are the callback's, which sign this in-process hop and never appear in a
/// response.
///
/// The Region a request is signed for is CALLER-SUPPLIED text and here it ends up in an
/// endpoint URL's authority, so it is read as a Region LABEL
/// (`service_common::arn::region_label`, the one place this tree states that grammar)
/// instead of being interpolated as given: `None` for a value that would name an authority
/// the caller chose rather than the peer service this emulator asks.
pub(super) fn ec2(
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
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}

/// The `DescribeVpcs` filter that selects VPCs BY ID (`vpc-id`, listed by that operation's
/// own `Filters` documentation).
///
/// A filter rather than `VpcIds`: an identifier the account has no VPC for is then an empty
/// result instead of a failure of the whole call, so "the account does not have this VPC"
/// and "the owner of VPCs could not be asked" stay two different answers.
const VPC_ID_FILTER: &str = "vpc-id";

/// The VPC `vpc_id` as the owner of VPCs reports it, asked BY ID: `None` when the hop could
/// not be made at all, `Some(None)` when it was made and the account has no such VPC, and
/// `Some(Some(vpc))` when it holds it.
///
/// THE one `DescribeVpcs` of this service, so the two readings of a placement's VPC — whether
/// the account has it ([`vpc_absence`]) and what block it carries ([`vpc_cidr_block`]) — cannot
/// drift on the filter they ask with or on what counts as a match. The three-way answer is what
/// keeps "could not be asked" apart from "asked, and the account does not have it": a caller
/// that folds them together says so where it does.
async fn described_vpc(
    client: &::aws_sdk_ec2::Client,
    region: &str,
    vpc_id: &str,
) -> Option<Option<::aws_sdk_ec2::types::Vpc>> {
    let by_id = ::aws_sdk_ec2::types::Filter::builder()
        .name(VPC_ID_FILTER)
        .values(vpc_id)
        .build();
    match client.describe_vpcs().filters(by_id).send().await {
        Ok(output) => Some(
            output
                .vpcs()
                .iter()
                .find(|vpc| vpc.vpc_id() == Some(vpc_id))
                .cloned(),
        ),
        Err(error) => {
            // At `warn` for the same reason the subnet hop logs one: a create served without
            // the owner of VPCs having been asked is worth seeing, and this IS why.
            ::tracing::warn!(
                %region,
                %vpc_id,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the VPC a directory is placed in could not be resolved against EC2"
            );
            None
        }
    }
}

/// Why `vpc_id` cannot hold a directory's servers — that the account has no such VPC in
/// `region` — or `None` when it has one, and `None` when the owner of VPCs could not be
/// asked at all.
///
/// A VPC is not a resource of this service either, so whether the account has the one a
/// create names is EC2's answer, asked over the same in-process hop the subnets are resolved
/// through. Nothing is refused where EC2 could not be asked: that is the same direction
/// [`resolve_subnet_networks`] takes, so an emulator mounted without the owner of VPCs still
/// serves a create rather than refusing every one of them.
///
/// Called by a create BEFORE it takes this service's store, so no guard is held across the
/// hop. ONE hop about ONE identifier, whatever the request body carries.
pub async fn vpc_absence(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: &str,
) -> Option<&'static str> {
    let client = ec2(ctx, region)?;
    match described_vpc(&client, region, vpc_id).await? {
        Some(_) => None,
        None => Some(super::INVALID_VPC_ID),
    }
}

/// The IPv4 block the VPC `vpc_id` carries, as the owner of VPCs reports it — `None` when
/// the account has no such VPC, when that owner reports no IPv4 block for it, or when the
/// hop could not be made at all.
///
/// What it is for: the network a directory's servers sit on is the one the traffic they serve
/// comes from, so the rules this service authorizes on the group it provisions for those
/// servers are scoped to THIS block ([`super::ec2_security_groups`]). The block is EC2's to
/// report — it is the VPC's own value, chosen when the caller created it — so it is asked for
/// over the same in-process hop every other fact about the placement's network is, and never
/// derived from a request.
///
/// The three answers are folded into one here, and that is the whole decision: a block nobody
/// could be asked for and a block of a VPC the account does not have are both "no block to
/// scope a rule to", and the caller then authorizes nothing rather than widening a rule.
///
/// Takes the client the caller already built, so provisioning a group and scoping its rules
/// is one client and one hop each rather than two of both.
pub(super) async fn vpc_cidr_block(
    client: &::aws_sdk_ec2::Client,
    region: &str,
    vpc_id: &str,
) -> Option<String> {
    described_vpc(client, region, vpc_id)
        .await
        .flatten()
        .and_then(|vpc| vpc.cidr_block().map(str::to_string))
}

/// What the owner of subnets ANSWERED about the subnets a placement named.
///
/// Two different things, kept apart because a placement decision turns on which one it is:
///
/// * `networks` is the network of every named subnet EC2 reported, keyed by identifier — an
///   identifier the account holds no subnet for is simply absent from it;
/// * `answered` says whether EC2 answered the question AT ALL, so an identifier missing from
///   `networks` is a subnet the account does not hold rather than a question nobody could put.
///
/// Without the second member the two are indistinguishable (both are an absent entry), and a
/// placement rule decided over `networks` alone can only treat an identifier the account does
/// not hold as undecided — which is how a pick naming no network of the account gets served.
#[derive(Debug, Clone, Default)]
pub struct SubnetResolution {
    /// The network of every named subnet the account HAS, as EC2 reports it.
    pub networks: BTreeMap<String, SubnetNetwork>,
    /// Whether the owner of subnets answered the hop at all.
    pub answered: bool,
}

/// The network of every subnet of `subnet_ids` the account has in `region`, keyed by
/// identifier, as EC2 reports it — an identifier it holds no subnet for is simply absent
/// from the map — together with whether EC2 ANSWERED at all
/// ([`SubnetResolution::answered`]), which is what tells an identifier the account does not
/// hold apart from a hop that could not be made.
///
/// Called by a create BEFORE it takes this service's store, so no guard is held across the
/// hop. ONE hop of at most [`SUBNET_IDS_PER_HOP`] identifiers, whatever the request body
/// carries: nothing caller-sized is asked about, allocated or stored here.
pub async fn resolve_subnet_networks(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> SubnetResolution {
    let mut resolution = SubnetResolution::default();
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return resolution;
    };
    // Asked for once each, and at most a hop's worth: an identifier the settings block
    // repeats costs no second value in the filter, and a block longer than one hop is
    // resolved as far as that hop reaches.
    let mut distinct: Vec<&str> = Vec::with_capacity(SUBNET_IDS_PER_HOP);
    for subnet_id in subnet_ids {
        if distinct.len() >= SUBNET_IDS_PER_HOP {
            break;
        }
        if !distinct.contains(&subnet_id.as_str()) {
            distinct.push(subnet_id);
        }
    }
    let by_id = ::aws_sdk_ec2::types::Filter::builder()
        .name(SUBNET_ID_FILTER)
        .set_values(Some(distinct.iter().map(|id| (*id).to_string()).collect()))
        .build();
    match client.describe_subnets().filters(by_id).send().await {
        Ok(output) => {
            // EC2 answered: from here on an identifier missing from the map is one the
            // account holds no subnet for, which is a fact about the REQUEST.
            resolution.answered = true;
            for subnet in output.subnets() {
                let Some(subnet_id) = subnet.subnet_id() else {
                    continue;
                };
                resolution.networks.insert(
                    subnet_id.to_string(),
                    SubnetNetwork {
                        subnet_id: subnet_id.to_string(),
                        cidr_block: subnet.cidr_block().map(str::to_string),
                        // Which address families the subnet carries is what a directory's
                        // default `NetworkType` is read from, so the IPv6 side of the
                        // placement comes off the owner of subnets too: EC2 reports a
                        // subnet's IPv6 blocks as the association set of that subnet.
                        carries_ipv6: subnet
                            .ipv6_cidr_block_association_set()
                            .iter()
                            .any(|association| association.ipv6_cidr_block().is_some()),
                        availability_zone: subnet.availability_zone().map(str::to_string),
                        vpc_id: subnet.vpc_id().map(str::to_string),
                    },
                );
            }
        }
        Err(error) => {
            // At `warn`: a directory created where the owner of subnets cannot be asked
            // serves no DNS address, which is worth seeing, and this IS why.
            ::tracing::warn!(
                %region,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the subnets a directory is placed in could not be resolved against EC2"
            );
        }
    }
    resolution
}
