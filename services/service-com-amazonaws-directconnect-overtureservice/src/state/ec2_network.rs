// SPDX-License-Identifier: CC0-1.0

//! The GATEWAYS the account holds, and the VPCs they attach, as the service that
//! owns them reports them.
//!
//! A virtual private gateway, a transit gateway and a VPC are not Direct Connect's
//! resources: they are Amazon EC2's. A client mints them with
//! `ec2:CreateVpnGateway` / `ec2:CreateTransitGateway` / `ec2:CreateVpc`, attaches
//! a virtual private gateway to a VPC with `ec2:AttachVpnGateway`, and only then
//! names that gateway in `CreateDirectConnectGatewayAssociation`. So EC2 is the ONE
//! answer to "does the account hold this gateway, and which VPCs does it attach",
//! and an identifier a request names is resolved there and nowhere else — reached
//! the way any emulated service reaches another, through the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no
//! other service's crate and nothing leaves the process.
//!
//! Everything an association then publishes about the associated gateway — its
//! type, its owner, and for a virtual private gateway the VPC prefixes advertised
//! to the Direct Connect gateway — comes from the records EC2 answered with, never
//! from anything built here.
//!
//! Nothing is cached: a gateway attached a moment ago resolves at once, a VPC block
//! associated a moment ago is advertised at once, and a gateway that was deleted
//! stops resolving at once.

use std::collections::BTreeMap;

/// The `DescribeVpnGateways` filter that selects virtual private gateways BY ID
/// (`vpn-gateway-id`, listed by that operation's own `Filters` documentation).
///
/// A filter rather than `VpnGatewayIds`: an identifier the account holds no gateway
/// for is then an absent row instead of a failure of the whole call, so this module
/// answers "the account does not hold it" rather than relaying someone else's
/// refusal — and so one hop can ask about several identifiers at once.
const VPN_GATEWAY_ID_FILTER: &str = "vpn-gateway-id";

/// The `DescribeTransitGateways` filter that selects transit gateways BY ID, on the
/// contract of [`VPN_GATEWAY_ID_FILTER`].
const TRANSIT_GATEWAY_ID_FILTER: &str = "transit-gateway-id";

/// The `DescribeVpcs` filter that selects VPCs BY ID, on the contract of
/// [`VPN_GATEWAY_ID_FILTER`].
const VPC_ID_FILTER: &str = "vpc-id";

/// The `AttachmentStatus` an attachment that is IN FORCE reports: a gateway whose
/// attachment has been detached attaches no VPC any more, so its prefixes are not
/// the account's either.
const ATTACHMENT_ATTACHED: &str = "attached";

/// The `VpcCidrBlockStateCode` an IPv4 or IPv6 block that is in service reports: a
/// block still associating, or already disassociated, is not one a VPC advertises.
const CIDR_BLOCK_ASSOCIATED: &str = "associated";

/// The states a gateway EC2 is tearing down, or has torn down, reports: one of those
/// forwards nothing, so it is not a gateway an association can be made on.
const TORN_DOWN_GATEWAY_STATES: [&str; 2] = ["deleting", "deleted"];

/// The most identifiers ONE hop asks about, as values of one id filter.
///
/// The owner evaluates a filter against every record the account stores, so the cost
/// of a hop is (identifiers asked x records stored): this caps the first factor, so
/// one hop costs a constant times what the account stores rather than something the
/// request chooses.
const IDS_PER_HOP: usize = 64;

/// `GatewayType`'s `virtualPrivateGateway` member, the value an association reports
/// for a gateway minted by `ec2:CreateVpnGateway`.
pub const VIRTUAL_PRIVATE_GATEWAY: &str = "virtualPrivateGateway";

/// `GatewayType`'s `transitGateway` member.
pub const TRANSIT_GATEWAY: &str = "transitGateway";

/// The identifier prefix EC2 mints a virtual private gateway's id with.
const VPN_GATEWAY_ID_PREFIX: &str = "vgw-";

/// The identifier prefix EC2 mints a transit gateway's id with.
const TRANSIT_GATEWAY_ID_PREFIX: &str = "tgw-";

/// Which KIND of gateway an identifier names.
///
/// `GatewayIdToAssociate` is "the ID of the virtual private gateway or transit
/// gateway", and the two are told apart by the identifier EC2 minted — which is also
/// what decides which of EC2's own readers resolves it and what `GatewayType` the
/// association reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayKind {
    /// A virtual private gateway (`vgw-…`).
    VirtualPrivate,
    /// A transit gateway (`tgw-…`).
    Transit,
}

impl GatewayKind {
    /// The kind `gateway_id` names, or `None` for an identifier that names neither
    /// kind of gateway — and so names no gateway the account could hold.
    pub fn of(gateway_id: &str) -> Option<Self> {
        if gateway_id.starts_with(VPN_GATEWAY_ID_PREFIX) {
            Some(Self::VirtualPrivate)
        } else if gateway_id.starts_with(TRANSIT_GATEWAY_ID_PREFIX) {
            Some(Self::Transit)
        } else {
            None
        }
    }

    /// The `GatewayType` an association reports for this kind.
    pub fn modelled(self) -> &'static str {
        match self {
            Self::VirtualPrivate => VIRTUAL_PRIVATE_GATEWAY,
            Self::Transit => TRANSIT_GATEWAY,
        }
    }

    /// How Direct Connect names this kind of gateway in a refusal.
    pub fn refusal_noun(self) -> &'static str {
        match self {
            Self::VirtualPrivate => "VirtualGateway",
            Self::Transit => "TransitGateway",
        }
    }
}

/// One gateway the ACCOUNT holds, as the service that owns it reports it.
#[derive(Debug, Clone)]
pub struct AccountGateway {
    /// The identifier EC2 holds the gateway under.
    pub id: String,
    /// The `GatewayType` this gateway is, from [`GatewayKind::modelled`].
    pub gateway_type: String,
    /// The account that owns it.
    pub owner_account: String,
    /// The Region it is held in: the Region the question was asked in.
    pub region: String,
    /// The VPCs this gateway attaches, and their CIDR blocks. Empty for a transit
    /// gateway, whose VPCs are attachments of its own.
    pub attached: AttachedNetwork,
}

/// The VPCs one virtual private gateway attaches, as EC2 reports them.
#[derive(Debug, Clone, Default)]
pub struct AttachedNetwork {
    /// The attached VPCs, in the order EC2 reports the attachments.
    pub vpc_ids: Vec<String>,
    /// The CIDR blocks of [`Self::vpc_ids`], in that order, each without
    /// repetition — the prefixes a virtual private gateway association advertises.
    pub cidrs: Vec<String>,
}

/// An AWS SDK client for EC2 over the in-process callback into this same emulator.
///
/// `region` is the request's own signing region, so the question is asked in the
/// Region the caller addressed. The endpoint URL is only the address the callback's
/// transport routes on IN PROCESS: no socket is opened, no name is resolved and
/// nothing leaves this process. The credentials are the callback's, which sign this
/// in-process hop and never appear in a response.
///
/// The Region a request is signed for is CALLER-SUPPLIED text and here it ends up in
/// an endpoint URL's authority, so it is read as a Region LABEL
/// (`service_common::arn::region_label`, the one place this tree states that grammar)
/// instead of being interpolated as given: `None` for a value that would name an
/// authority the caller chose rather than the peer service this emulator asks.
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
        // The transport is in-process: there is nothing to retry, time out or watch
        // for a stalled stream, and each of those would need a sleep implementation
        // this client is not given. The same goes for caching the identity: the
        // credentials the callback hands over are used directly.
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}

/// The gateway `gateway_id` names, as the account's own EC2 inventory reports it, or
/// `None` when the account holds no such gateway in `region`.
///
/// FAIL-CLOSED: a hop that could not be made at all resolves to nothing, so the
/// request is refused rather than answered from something invented here. An
/// association PUBLISHES the gateway's type, owner, Region and advertised prefixes,
/// so with no record there is nothing to store or to report.
pub async fn resolve_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    gateway_id: &str,
    kind: GatewayKind,
) -> Option<AccountGateway> {
    match kind {
        GatewayKind::VirtualPrivate => {
            let asked = vec![gateway_id.to_string()];
            let attached = attached_networks(ctx, region, &asked)
                .await
                .remove(gateway_id)?;
            Some(AccountGateway {
                id: gateway_id.to_string(),
                gateway_type: GatewayKind::VirtualPrivate.modelled().to_string(),
                // EC2 reports no owner on a virtual private gateway: it is one of the
                // calling account's own resources, which is the account this emulator
                // serves.
                owner_account: ::engine::emulator_account_id().to_string(),
                region: region.to_string(),
                attached,
            })
        }
        GatewayKind::Transit => transit_gateway(ctx, region, gateway_id).await,
    }
}

/// What EC2 holds for each virtual private gateway of `gateway_ids`: the VPCs it
/// attaches and those VPCs' CIDR blocks, keyed by gateway identifier.
///
/// A gateway EC2 does not hold, or is tearing down, is ABSENT from the map — which is
/// how a caller tells "the account does not hold it" from "it attaches nothing". One
/// hop per [`IDS_PER_HOP`] gateways plus one per [`IDS_PER_HOP`] VPCs, so asking about
/// a page of associations costs a bounded number of hops rather than one each.
pub async fn attached_networks(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    gateway_ids: &[String],
) -> BTreeMap<String, AttachedNetwork> {
    let mut held: BTreeMap<String, AttachedNetwork> = BTreeMap::new();
    if gateway_ids.is_empty() {
        return held;
    }
    let Some(client) = ec2(ctx, region) else {
        ::tracing::warn!("the request names no Region label, so EC2 cannot be asked");
        return held;
    };

    let mut distinct: Vec<String> = Vec::new();
    for gateway_id in gateway_ids {
        if !distinct.contains(gateway_id) {
            distinct.push(gateway_id.clone());
        }
    }
    for chunk in distinct.chunks(IDS_PER_HOP) {
        let by_id = ::aws_sdk_ec2::types::Filter::builder()
            .name(VPN_GATEWAY_ID_FILTER)
            .set_values(Some(chunk.to_vec()))
            .build();
        match client.describe_vpn_gateways().filters(by_id).send().await {
            Ok(described) => {
                for gateway in described.vpn_gateways() {
                    let Some(gateway_id) = gateway.vpn_gateway_id() else {
                        continue;
                    };
                    let state = gateway
                        .state()
                        .map(::aws_sdk_ec2::types::VpnState::as_str)
                        .unwrap_or_default();
                    if TORN_DOWN_GATEWAY_STATES.contains(&state) {
                        continue;
                    }
                    // "Any VPCs attached to the virtual private gateway"
                    // (`VpnGateway$VpcAttachments`), narrowed to the attachments that
                    // are in force.
                    let vpc_ids: Vec<String> = gateway
                        .vpc_attachments()
                        .iter()
                        .filter(|attachment| {
                            attachment
                                .state()
                                .map(::aws_sdk_ec2::types::AttachmentStatus::as_str)
                                == Some(ATTACHMENT_ATTACHED)
                        })
                        .filter_map(|attachment| attachment.vpc_id().map(str::to_string))
                        .collect();
                    held.insert(
                        gateway_id.to_string(),
                        AttachedNetwork {
                            vpc_ids,
                            cidrs: Vec::new(),
                        },
                    );
                }
            }
            Err(error) => {
                // At `warn`: an association cannot be made, and its prefixes cannot be
                // reported, when the owner of gateways cannot be asked — worth seeing,
                // and it IS why an identifier did not resolve.
                ::tracing::warn!(
                    %region,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the virtual private gateways an association named could not be \
                     resolved against EC2"
                );
            }
        }
    }

    // Every attached VPC of every gateway, asked about together.
    let mut vpc_ids: Vec<String> = Vec::new();
    for network in held.values() {
        for vpc_id in &network.vpc_ids {
            if !vpc_ids.contains(vpc_id) {
                vpc_ids.push(vpc_id.clone());
            }
        }
    }
    let blocks = vpc_blocks(&client, region, &vpc_ids).await;
    for network in held.values_mut() {
        for vpc_id in &network.vpc_ids {
            for block in blocks.get(vpc_id).into_iter().flatten() {
                push_once(&mut network.cidrs, Some(block.as_str()));
            }
        }
    }
    held
}

/// [`resolve_gateway`] for a transit gateway. A transit gateway's VPCs are
/// attachments of its own rather than of the gateway record, so it reports none here:
/// the prefixes a transit gateway association advertises are the ones the request
/// names (`addAllowedPrefixesToDirectConnectGateway`).
async fn transit_gateway(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    gateway_id: &str,
) -> Option<AccountGateway> {
    let client = ec2(ctx, region)?;
    let by_id = ::aws_sdk_ec2::types::Filter::builder()
        .name(TRANSIT_GATEWAY_ID_FILTER)
        .values(gateway_id)
        .build();
    let described = match client
        .describe_transit_gateways()
        .filters(by_id)
        .send()
        .await
    {
        Ok(described) => described,
        Err(error) => {
            ::tracing::warn!(
                %region,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the transit gateway an association named could not be resolved against EC2"
            );
            return None;
        }
    };
    let gateway = described
        .transit_gateways()
        .iter()
        .find(|gateway| gateway.transit_gateway_id() == Some(gateway_id))?;
    let state = gateway
        .state()
        .map(::aws_sdk_ec2::types::TransitGatewayState::as_str)
        .unwrap_or_default();
    if TORN_DOWN_GATEWAY_STATES.contains(&state) {
        return None;
    }

    Some(AccountGateway {
        id: gateway_id.to_string(),
        gateway_type: GatewayKind::Transit.modelled().to_string(),
        owner_account: gateway
            .owner_id()
            .unwrap_or(::engine::emulator_account_id())
            .to_string(),
        region: region.to_string(),
        attached: AttachedNetwork::default(),
    })
}

/// The CIDR blocks each VPC of `vpc_ids` carries, in the order EC2 reports them —
/// IPv4 first, then IPv6 — keyed by VPC identifier.
///
/// Only a block that is IN SERVICE counts ([`CIDR_BLOCK_ASSOCIATED`]): a block still
/// associating, or already disassociated, is not one the VPC advertises.
///
/// Bounded by [`IDS_PER_HOP`] identifiers per hop. A hop that could not be made
/// contributes nothing, so an association advertises only prefixes EC2 answered for.
async fn vpc_blocks(
    client: &::aws_sdk_ec2::Client,
    region: &str,
    vpc_ids: &[String],
) -> BTreeMap<String, Vec<String>> {
    let mut by_vpc: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for chunk in vpc_ids.chunks(IDS_PER_HOP) {
        let by_id = ::aws_sdk_ec2::types::Filter::builder()
            .name(VPC_ID_FILTER)
            .set_values(Some(chunk.to_vec()))
            .build();
        match client.describe_vpcs().filters(by_id).send().await {
            Ok(described) => {
                for vpc in described.vpcs() {
                    let Some(vpc_id) = vpc.vpc_id() else { continue };
                    let blocks = by_vpc.entry(vpc_id.to_string()).or_default();
                    for association in vpc.cidr_block_association_set() {
                        if block_is_in_service(
                            association
                                .cidr_block_state()
                                .and_then(::aws_sdk_ec2::types::VpcCidrBlockState::state)
                                .map(::aws_sdk_ec2::types::VpcCidrBlockStateCode::as_str),
                        ) {
                            push_once(blocks, association.cidr_block());
                        }
                    }
                    // "The primary IPv4 CIDR block for the VPC" (`Vpc$CidrBlock`), for
                    // a record that reports no association set at all.
                    push_once(blocks, vpc.cidr_block());
                    for association in vpc.ipv6_cidr_block_association_set() {
                        if block_is_in_service(
                            association
                                .ipv6_cidr_block_state()
                                .and_then(::aws_sdk_ec2::types::VpcCidrBlockState::state)
                                .map(::aws_sdk_ec2::types::VpcCidrBlockStateCode::as_str),
                        ) {
                            push_once(blocks, association.ipv6_cidr_block());
                        }
                    }
                }
            }
            Err(error) => {
                ::tracing::warn!(
                    %region,
                    error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                    "the VPCs a gateway attaches could not be resolved against EC2"
                );
            }
        }
    }
    by_vpc
}

/// Whether a CIDR block's state code says the block is in service.
fn block_is_in_service(state: Option<&str>) -> bool {
    // A record that reports no state at all is one EC2 holds without a lifecycle on
    // it, which is the block being in service.
    state.is_none_or(|state| state == CIDR_BLOCK_ASSOCIATED)
}

/// Append `value` to `values` unless it is already there, so a block reported twice
/// is advertised once.
fn push_once(values: &mut Vec<String>, value: Option<&str>) {
    if let Some(value) = value {
        if !values.iter().any(|held| held == value) {
            values.push(value.to_string());
        }
    }
}
