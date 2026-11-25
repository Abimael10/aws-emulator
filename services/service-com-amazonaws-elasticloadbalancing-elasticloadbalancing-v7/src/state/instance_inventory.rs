// SPDX-License-Identifier: CC0-1.0

//! Whether the EC2 INSTANCES a request names exist, which is EC2's to say.
//!
//! An instance is EC2's resource, not this service's: a registration holds an instance id,
//! and whether the account has an instance under that id is a fact of the service that
//! launches them. So every id a request names is resolved against EC2 before this service
//! acts on it, through the same seam the subnets and security groups a request names are
//! resolved through ([`super::resolve_subnet_placement`],
//! [`super::resolve_source_security_group`]) — the in-process callback, so this service
//! reaches EC2 through its API rather than reaching into its store.
//!
//! What a caller does with the answer: an id EC2 holds no instance for names no endpoint,
//! which is the modelled `InvalidEndPointException` (`InvalidInstance`), stated with the
//! EC2 fault relayed whole ([`super::relayed_network_refusal_message`]) where the operation
//! relays it and with the sentence Elastic Load Balancing words itself
//! ([`instance_not_found_message`]) where it words one. An id EC2 DOES hold an instance for
//! is this service's own business again: whether it is registered with the load balancer,
//! and what its health checks make of it, is decided here as before.
//!
//! With no EC2 to ask, nothing is known and so nothing is refused: a refusal is grounded in
//! what EC2 answered rather than assumed from silence.

use service_com_amazonaws_elasticloadbalancingv2_elasticloadbalancing_v10 as elbv2;

pub use elbv2::state::network::InstanceInventory;

/// What EC2 says about the instance ids a request named, in the order it named them.
///
/// The ids come from the request and are bounded by [`super::MAX_INSTANCES_PER_REQUEST`]
/// before this is called, so the reads it makes are bounded too. A request naming no
/// instance resolves to [`InstanceInventory::Resolved`]: there is nothing to ask about.
pub async fn resolve_instances(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    instance_ids: &[String],
) -> InstanceInventory {
    elbv2::state::network::resolve_instances(ctx, region, instance_ids).await
}

/// The MESSAGE `DescribeInstanceHealth` states for an id EC2 holds no instance for: one
/// fixed sentence naming that id, which is the value the caller has to correct.
///
/// Elastic Load Balancing words this one itself rather than relaying the EC2 fault, and
/// says nothing about the load balancer the health was asked about: the verdict is that
/// there is no such EC2 instance to report the health of.
pub fn instance_not_found_message(instance_id: &str) -> String {
    format!("Could not find EC2 instance {instance_id}.")
}
