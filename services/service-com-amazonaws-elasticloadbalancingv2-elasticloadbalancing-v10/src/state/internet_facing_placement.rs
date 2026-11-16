// SPDX-License-Identifier: CC0-1.0

//! Whether the VPC a load balancer is placed in can carry an INTERNET-FACING one.
//!
//! "The nodes of an Internet-facing load balancer have public IP addresses. The DNS name of
//! an Internet-facing load balancer is publicly resolvable to the public IP addresses of the
//! nodes. Therefore, Internet-facing load balancers can route requests from clients over the
//! internet" (`CreateLoadBalancerInput$Scheme`). A VPC's traffic reaches the internet only
//! through an internet gateway attached to it, so a public address inside a VPC that has none
//! routes nowhere: such a load balancer cannot exist, and the placement is refused rather
//! than provisioned.
//!
//! What the rule reads is the ATTACHMENT of a gateway to the VPC — the one fact that makes
//! public addresses in it reachable — and nothing about the route tables of its subnets,
//! which decide where traffic of a subnet goes once the VPC has a gateway at all. The
//! attachment is EC2's ([`super::network::vpc_internet_gateway`]), so the rule is judged on
//! what EC2 answered and refuses nothing when EC2 answered nothing.
//!
//! An INTERNAL load balancer's nodes "have only private IP addresses"
//! (`CreateLoadBalancerInput$Scheme`), which need no gateway, so the same subnets carry one
//! whether or not the VPC has a gateway.

use super::{network::InternetGatewayLookup, LoadBalancerFamily};

/// The `Scheme` whose nodes have public IP addresses, from the model's
/// `LoadBalancerSchemeEnum` (`internal`, `internet-facing`) — the scheme
/// `CreateLoadBalancer` documents as its default: "The default is an Internet-facing load
/// balancer."
pub const INTERNET_FACING_SCHEME: &str = "internet-facing";

/// Whether a load balancer of `family` with this `scheme` needs the VPC it is placed in to
/// have an internet gateway — which is also whether the gateway has to be read at all.
///
/// Only a load balancer whose nodes carry PUBLIC addresses does: the internet-facing scheme
/// of a family that carries a scheme. A Gateway Load Balancer carries none — "You can't
/// specify a scheme for a Gateway Load Balancer" (`CreateLoadBalancerInput$Scheme`) — so its
/// placement is judged on no scheme, and an internal load balancer's private addresses need
/// no gateway.
pub fn placement_needs_internet_gateway(family: LoadBalancerFamily, scheme: &str) -> bool {
    family != LoadBalancerFamily::Gateway && scheme == INTERNET_FACING_SCHEME
}

/// The sentence Elastic Load Balancing refuses a public placement in a gateway-less VPC
/// with: the VPC the placement would put the load balancer in, and what it lacks.
///
/// The VPC id comes from the subnets the REQUEST named — it is what tells the caller which
/// network it has to attach a gateway to.
pub fn no_internet_gateway_message(vpc_id: &str) -> String {
    format!("VPC {vpc_id} has no internet gateway")
}

/// Whether a load balancer of `family` with this `scheme` can be placed in `vpc_id`, and —
/// when it cannot — the rule the request broke.
///
/// `gateway` is what EC2 answered about that VPC's internet gateway, and `vpc_id` is the VPC
/// the subnets of the placement belong to ([`super::subnets_vpc`]); a placement whose VPC
/// could not be resolved, and one EC2 answered nothing about, is refused on nothing, because
/// a refusal has to be grounded in what EC2 reported rather than assumed from silence.
///
/// The ONE reading of that rule, which `CreateLoadBalancer` applies to the subnets a load
/// balancer starts in.
pub fn check_internet_gateway(
    family: LoadBalancerFamily,
    scheme: &str,
    vpc_id: Option<&str>,
    gateway: InternetGatewayLookup,
) -> Result<(), String> {
    if !placement_needs_internet_gateway(family, scheme) {
        return Ok(());
    }
    let Some(vpc_id) = vpc_id else {
        return Ok(());
    };
    match gateway {
        InternetGatewayLookup::Detached => Err(no_internet_gateway_message(vpc_id)),
        InternetGatewayLookup::Attached | InternetGatewayLookup::Unknown => Ok(()),
    }
}
