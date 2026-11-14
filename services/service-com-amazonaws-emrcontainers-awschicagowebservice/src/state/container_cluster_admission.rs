// SPDX-License-Identifier: CC0-1.0

//! Whether the CONTAINER CLUSTER a virtual cluster would be registered against EXISTS: a
//! fact this service resolves rather than holds.
//!
//! `ContainerProvider$id` is "The ID of the container cluster", and a container cluster is
//! not a resource of this service: it belongs to Amazon EKS
//! (`ContainerProviderType`'s one value). A virtual cluster "is a Kubernetes namespace that
//! Amazon EMR is registered with", so there is nothing to register when the container
//! cluster the request names does not exist — which is why the registration resolves the id
//! against the service that OWNS the cluster before anything is minted, instead of storing
//! a provider reference every later read would report as live.
//!
//! The question is asked the way any emulated service reaches another — `eks:DescribeCluster`
//! over the in-process callback ([`smithy_server_runtime::OperationContext::callback`]), in
//! the Region the request was signed for, through the ONE Amazon EKS client of this crate
//! ([`super::container_cluster_network::eks_client`]) — so this crate names no other
//! service's crate and nothing leaves the process.
//!
//! Every refusal built on this is grounded in what Amazon EKS ANSWERED. A hop that could not
//! be made at all — no Amazon EKS mounted in this process, an answer that could not be read
//! — is evidence of nothing, so it refuses nothing: `services/common/src/network.rs` is the
//! contract this follows, in its "gates on existence only" reading.

use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

/// What the owner of container clusters answered about one `ContainerProvider$id`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerClusterResolution {
    /// Amazon EKS reported a cluster for the id in this Region.
    Held,
    /// Amazon EKS reported that it holds no such cluster: the ONE answer a registration is
    /// refused on.
    NotHeld,
    /// Nothing was learned — there was no Amazon EKS to ask, or its answer could not be
    /// read. A refusal here would be assumed rather than grounded, so the registration
    /// proceeds exactly as it did before anything asked.
    Unresolved,
}

/// The Amazon EKS error codes that say the value names no cluster of this Region:
/// `ResourceNotFoundException` for a name no cluster carries, `InvalidParameterException`
/// for a value that is not a cluster name at all. Both are the owner reporting that it
/// holds nothing under the id, which is the same thing about the registration.
const CLUSTER_ABSENT_CODES: &[&str] = &["ResourceNotFoundException", "InvalidParameterException"];

/// Whether the container cluster `container_cluster_id` names exists, as the service that
/// owns it reports it.
///
/// ONE hop, about ONE cluster. `ContainerProvider$id` is the container cluster's own
/// identifier, which is the name `eks:DescribeCluster` takes.
pub async fn resolve_container_cluster(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    container_cluster_id: &str,
) -> ContainerClusterResolution {
    let Some(client) = super::container_cluster_network::eks_client(ctx, region) else {
        ::tracing::warn!("the request names no Region label, so Amazon EKS cannot be asked");
        return ContainerClusterResolution::Unresolved;
    };
    let answer = client
        .describe_cluster()
        .name(container_cluster_id)
        .send()
        .await;
    let error = match answer {
        // A described cluster is one Amazon EKS holds; an answer carrying no cluster at all
        // reports nothing about one, which grounds no refusal.
        Ok(output) if output.cluster.is_some() => return ContainerClusterResolution::Held,
        Ok(_) => return ContainerClusterResolution::Unresolved,
        Err(error) => error,
    };
    let code = error
        .as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .unwrap_or_default()
        .to_string();
    if CLUSTER_ABSENT_CODES.contains(&code.as_str()) {
        return ContainerClusterResolution::NotHeld;
    }
    // Any other answer is this emulator failing to ASK rather than Amazon EKS reporting a
    // cluster the account does not have — worth a warning, because a deployment with no
    // Amazon EKS to ask is one where this gate does not run.
    ::tracing::warn!(
        %region,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
        "the container cluster a request named could not be resolved against Amazon EKS"
    );
    ContainerClusterResolution::Unresolved
}

/// The `message` a registration refused for a container cluster its owner holds nothing for
/// carries: `Cluster <id> not found`.
///
/// The fixed text is the service's own; the one variable part is the `ContainerProvider$id`
/// the request named, as the request spelled it, held to what the member carrying the
/// message can hold ([`super::message_about`]) — a `ClusterId` is len 1..100 and so may be
/// longer than the sentence around it.
pub fn container_cluster_not_found_message(container_cluster_id: &str) -> String {
    super::message_about("Cluster ", container_cluster_id, " not found")
}
