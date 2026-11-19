// SPDX-License-Identifier: CC0-1.0

//! WHICH network a private graph endpoint is created in, when the request leaves that to
//! the service.
//!
//! `CreatePrivateGraphEndpoint` takes `vpcId` and `subnetIds` as OPTIONAL input and
//! reports both as REQUIRED output
//! (`smithy_query(query="operation",operation="CreatePrivateGraphEndpoint")`), and models
//! no error for their absence — so a request that names neither still names a network:
//! what the caller left out, the service resolves from the account's own network
//! inventory in the Region the request was signed for. The `vpcId` is then the VPC the
//! endpoint is created in ("The VPC in which the private graph endpoint needs to be
//! created",
//! `smithy_query(query="doc",shape="CreatePrivateGraphEndpointInput",member="vpcId")`):
//! the VPC that holds the subnets the request named, since a subnet belongs to exactly one
//! VPC, and the Region's DEFAULT VPC when the request named no subnet of this account
//! either. The `subnetIds` are the subnets the VPC the endpoint is created in holds
//! ("Subnets in which private graph endpoint ENIs are created", same shape), since an
//! endpoint's network interfaces live in the subnets of its own VPC.
//!
//! ## The inventory is read, never re-answered
//!
//! VPCs and subnets are Amazon EC2 resources, and this emulator's account holds them in
//! THAT service's store, so the resolution reads them from it in the same engine — the
//! same way this service reads the identities IAM and STS own rather than keeping a
//! second answer to who a credential speaks for ([`super::query_authorization`]). The
//! default VPC is materialised by the one place the owning service has for it
//! (`ec2:DescribeVpcs` and `ec2:DescribeSubnets` reach exactly that place), with one
//! default subnet per Availability Zone of the Region, and it is the ids THOSE reads
//! answer with that an endpoint reports: an endpoint of this service can therefore not
//! name a VPC or a subnet this emulator does not hold.
//!
//! A member the request DID carry is used as sent, by the modelled constraints the
//! boundary validator has already held it to; only an absent one is resolved here, so a
//! request that names both never consults the inventory at all.
//!
//! ## When the account has no network to put an endpoint in
//!
//! The resolution answers the modelled `ValidationException` (HTTP 400) when the member
//! the request left out cannot be resolved at all: the account holds no default VPC in
//! the Region to create the endpoint in — a credential scope naming something merely
//! SHAPED like a Region code holds none, an account's networks being in the Regions that
//! exist — the request named a VPC this account does not hold, or the VPC it named holds
//! no subnet for the endpoint's interfaces. Each is a well-formed request the account's
//! state leaves nothing to resolve from, which is the condition this service's
//! [`super::BAD_REQUEST_REASON`] refusals report (see
//! [`super::graph_data_plane_unavailable`]), and they are worded apart so a caller learns
//! which one it sent — the one thing none of them may do is answer with a VPC or a subnet
//! nothing holds.

use ::service_com_amazonaws_ec2_amazonec2::state as vpc_inventory;

use super::*;

/// The most subnets one private graph endpoint reports: `SubnetIds` is `@length(1..6)` on
/// both sides of the call
/// (`smithy_query(query="operation",operation="CreatePrivateGraphEndpoint")`), so a
/// resolved list is capped at the bound the answer may carry, whatever the VPC holds.
pub const MAX_ENDPOINT_SUBNET_IDS: usize = 6;

/// The network one private graph endpoint is created in: the VPC it lives in and the
/// subnets its network interfaces are created in, each either the one the request named
/// or the one resolved from the account's inventory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointNetwork {
    /// The VPC the endpoint is created in — the `vpcId` the record is addressed by.
    pub vpc_id: String,
    /// The subnets the endpoint's interfaces are created in, in the order the owning
    /// service reports them.
    pub subnet_ids: Vec<String>,
}

/// The network a new private graph endpoint is created in, resolving whatever the request
/// left to the service out of the account's inventory in `region` (see the module docs).
///
/// Called with NO store guard of this service held — the inventory belongs to another
/// store in the same engine, and no handler holds two of them at once — and before the
/// endpoint record is built, so the VPC the record is keyed by is the resolved one.
///
/// `Err` is the modelled refusal for an account that holds no network to put an endpoint
/// in (see the module docs); it is never reached by a request that names both members,
/// which is resolved without consulting the inventory.
pub fn resolve_endpoint_network<Eng: Send + Sync + 'static>(
    engine: &Eng,
    region: &str,
    requested_vpc_id: Option<String>,
    requested_subnet_ids: Option<Vec<String>>,
) -> Result<EndpointNetwork, ValidationException> {
    // A request that named both members is answered from the request alone — nothing is
    // left to resolve, so the inventory is not read at all.
    let (requested_vpc_id, requested_subnet_ids) = match (requested_vpc_id, requested_subnet_ids) {
        (Some(vpc_id), Some(subnet_ids)) => {
            return Ok(EndpointNetwork {
                vpc_id,
                subnet_ids,
            })
        }
        left_to_resolve => left_to_resolve,
    };

    // One guard over the owning store for the whole resolution, so the VPC answered and
    // the subnets answered cannot come from two different views of the inventory.
    let Some(inventory) = vpc_inventory::service_state(engine) else {
        return Err(no_default_vpc(region));
    };
    let mut inventory = inventory.lock();

    let vpc_id = match (requested_vpc_id, requested_subnet_ids.as_deref()) {
        (Some(named), _) => named,
        // A subnet belongs to exactly ONE VPC, so a request that named the subnets has
        // named the VPC with them: the endpoint is created in the VPC that holds them,
        // which is what keeps the record's VPC and its subnets the same network.
        (None, Some(named_subnets)) => match vpc_holding(&inventory, named_subnets) {
            Some(holder) => holder,
            // Subnets the account does not hold name no VPC, so there is nothing to read
            // them off: the member the request DID leave out falls back to the Region's
            // default VPC, as it does for a request that named no subnet either.
            None => default_vpc(&mut inventory, region)?,
        },
        (None, None) => default_vpc(&mut inventory, region)?,
    };

    let subnet_ids = match requested_subnet_ids {
        Some(named) => named,
        None => {
            let held = subnets_in_vpc(&inventory, &vpc_id);
            if held.is_empty() {
                // Nothing to resolve the member from, reported as the condition it is:
                // the VPC the request named is not one the account holds, or it is and
                // holds no subnet the endpoint's interfaces could be created in.
                return Err(if inventory.vpcs.contains_key(&vpc_id) {
                    no_subnets_in_vpc(&vpc_id)
                } else {
                    vpc_not_found(&vpc_id)
                });
            }
            held
        }
    };
    if subnet_ids.is_empty() {
        // Unreachable for a caller-named list (`SubnetIds` is `@length(1..6)`, so the
        // boundary validator refuses an empty one) and already answered above for a
        // resolved one; kept so the required output member can never be empty.
        return Err(no_subnets_in_vpc(&vpc_id));
    }
    Ok(EndpointNetwork {
        vpc_id,
        subnet_ids,
    })
}

/// The Region's default VPC, materialised by the owning service's ONE place for it —
/// which is also what its own `DescribeVpcs` reaches, so the id answered here is the id
/// that read reports.
///
/// `Err` is the Region the account has no default VPC in at all.
fn default_vpc(
    inventory: &mut vpc_inventory::ComAmazonawsEc2Amazonec2State,
    region: &str,
) -> Result<String, ValidationException> {
    inventory
        .ensure_default_network(region)
        .ok_or_else(|| no_default_vpc(region))
}

/// The one VPC the account holds EVERY subnet of `subnet_ids` in, or `None` when it holds
/// none of them, or holds them in more than one VPC.
///
/// A subnet belongs to exactly one VPC, so subnets that are the account's own name the VPC
/// they sit in; subnets spread over two VPCs name no single network, and subnets the
/// account does not hold name nothing at all.
fn vpc_holding(
    inventory: &vpc_inventory::ComAmazonawsEc2Amazonec2State,
    subnet_ids: &[String],
) -> Option<String> {
    let mut holder: Option<&str> = None;
    for subnet_id in subnet_ids {
        let held = inventory.subnets.get(subnet_id)?.vpc_id.as_deref()?;
        match holder {
            Some(already) if already != held => return None,
            _ => holder = Some(held),
        }
    }
    holder.map(str::to_string)
}

/// The subnets the account holds in `vpc_id`, in the order the owning service reports
/// them (its store is keyed by subnet id, which is the order its describe pages over),
/// capped at [`MAX_ENDPOINT_SUBNET_IDS`].
///
/// A subnet belongs to exactly one VPC, and a VPC to exactly one Region, so naming the
/// VPC is what scopes this to the Region the endpoint is created in.
fn subnets_in_vpc(
    inventory: &vpc_inventory::ComAmazonawsEc2Amazonec2State,
    vpc_id: &str,
) -> Vec<String> {
    inventory
        .subnets
        .values()
        .filter(|subnet| subnet.vpc_id.as_deref() == Some(vpc_id))
        .filter_map(|subnet| subnet.subnet_id.clone())
        .take(MAX_ENDPOINT_SUBNET_IDS)
        .collect()
}

/// The modelled `ValidationException` (HTTP 400) for a request that left the VPC to the
/// service in a Region where the account holds no default VPC — a Region that does not
/// exist, or one whose inventory this engine does not carry.
///
/// Reported under [`BAD_REQUEST_REASON`] for the reason that constant states: the request
/// is well-formed and it is the account's own state that leaves nothing to resolve from.
/// Worded in the grammar this service's other bad-request refusals are worded in
/// ([`graph_deletion_protected_validation`], [`graph_not_available_for_snapshot`]): what
/// could not be resolved, then what the caller can send instead. The Region is named
/// because it is the one thing the caller can change about it.
fn no_default_vpc(region: &str) -> ValidationException {
    ValidationException {
        message: format!(
            "No default VPC was found in {region}. Specify the vpcId of a VPC of this \
             account to create the private graph endpoint in."
        ),
        reason: Some(BAD_REQUEST_REASON.to_string()),
    }
}

/// The modelled `ValidationException` (HTTP 400) for a request that left the subnets to
/// the service and named a VPC this account does not hold: there is no VPC to read them
/// off.
///
/// The sibling of [`no_default_vpc`], in the same grammar, and distinct from
/// [`no_subnets_in_vpc`]: a VPC the account does not have is not a VPC that holds no
/// subnet, and a caller fixing its request needs to know which of the two it sent.
fn vpc_not_found(vpc_id: &str) -> ValidationException {
    ValidationException {
        message: format!(
            "VPC [{vpc_id}] was not found in this account. You can only create a private \
             graph endpoint in a VPC of this account."
        ),
        reason: Some(BAD_REQUEST_REASON.to_string()),
    }
}

/// The modelled `ValidationException` (HTTP 400) for a request that left the subnets to
/// the service in a VPC of the account that holds no subnet.
///
/// The sibling of [`no_default_vpc`], in the same grammar: `subnetIds` is
/// `@length(1..6)`, so an endpoint cannot be created with no subnet at all, and a VPC
/// holding none is nothing the service can resolve the member from.
fn no_subnets_in_vpc(vpc_id: &str) -> ValidationException {
    ValidationException {
        message: format!(
            "VPC [{vpc_id}] has no subnet the private graph endpoint's network interfaces \
             can be created in. Specify the subnetIds to create them in."
        ),
        reason: Some(BAD_REQUEST_REASON.to_string()),
    }
}
