// SPDX-License-Identifier: CC0-1.0

//! The account's subnets, a fact this service RESOLVES rather than holds.
//!
//! A subnet is not a resource of this service: it is Amazon EC2's. "The subnets and
//! security groups for the cluster must already exist before you use this API action"
//! (`CreateCluster`'s own documentation), and `NetworkingRequest$subnetIds` is "the list of
//! subnet IDs where PCS creates an Elastic Network Interface (ENI) to enable communication
//! between managed controllers and PCS resources" — so a create that places something in a
//! caller-named subnet resolves what it was given BEFORE it mints anything, and EC2 is the
//! one answer to "does this account hold that subnet".
//!
//! The hop is the service's one EC2 hop ([`super::ec2_client`]), so nothing leaves the
//! process and the Region a request was signed for is read as a Region LABEL before it can
//! reach an endpoint authority. Nothing is cached and nothing is stored here: a subnet
//! created a moment ago resolves at once, and one that was deleted stops resolving at once.
//!
//! Every refusal built here is grounded in what EC2 ANSWERED. A hop that could not be made
//! at all — an emulator with no EC2 mounted, an answer this emulator could not read — is
//! this service failing to ask rather than EC2 reporting a subnet the account does not have,
//! so it refuses nothing: this gate keeps nothing of the subnet it resolves, so a refusal
//! assumed from silence would cost a create that EC2 would have resolved.
//!
//! What the CALLER is told is PCS's own fixed sentence ([`SUBNETS_ABSENT_MESSAGE`],
//! [`SUBNETS_OUTSIDE_CLUSTER_VPC_MESSAGE`]): it names no subnet id of the request and
//! carries nothing of EC2's own answer — no VPC id either — because PCS answers for the
//! request it was given rather than reporting the call it made on the caller's behalf.
//!
//! EC2's answer carries more than whether an id resolves: it reports the VPC each subnet
//! belongs to, which is what decides a placement INSIDE a cluster
//! ([`placement_violation`]).

use std::collections::{HashMap, HashSet};

use super::{service_validation, ValidationException};

/// What PCS answers a create whose subnets it could not resolve against the account's
/// network.
///
/// A CONSTANT of the service: the whole sentence is fixed text, naming neither the ids the
/// request carried nor how many of them failed to resolve — "1 or more" is the service's own
/// wording, not a count.
pub const SUBNETS_ABSENT_MESSAGE: &str =
    "AWS PCS couldn't find 1 or more of the subnets you provided.";

/// What PCS answers a request that would place compute nodes outside the VPC the cluster
/// they belong to is in.
///
/// The RULE is the member's own: "The subnets must be in the same VPC as the cluster"
/// (`UpdateComputeNodeGroupRequest$subnetIds`; `CreateComputeNodeGroupRequest$subnetIds`
/// states the same requirement), so the sentence states it as the model words it. A
/// CONSTANT of the service like [`SUBNETS_ABSENT_MESSAGE`]: the whole sentence is fixed
/// text, naming neither the subnets the request carried nor the VPC either side is in.
pub const SUBNETS_OUTSIDE_CLUSTER_VPC_MESSAGE: &str =
    "The subnets must be in the same VPC as the cluster.";

/// The prefix of every EC2 error code that says a value names no subnet of the account:
/// `InvalidSubnetID.NotFound` for an id no subnet carries, `InvalidSubnetID.Malformed` for a
/// value that is not a subnet id at all. Both say the same thing about a placement.
const SUBNET_ID_REFUSAL_PREFIX: &str = "InvalidSubnetID.";

/// The modelled `ValidationException` (HTTP 400) a request naming `subnet_ids` is refused
/// with, or `None` when the placement resolves and the call may proceed. This is EXISTENCE
/// alone, which is the whole question for a placement no cluster's own VPC bounds — the
/// cluster's control plane itself; a resource placed INSIDE a cluster goes through
/// [`placement_violation`].
///
/// `Some` only where EC2 ANSWERED that an id names no subnet of this account in this Region:
/// it reports `InvalidSubnetID.*` for such an id, and an id it reported no subnet for says
/// the same thing. `None` for an empty list — there is nothing to resolve — and for every
/// answer that is not such a report, which is this emulator failing to ask (see the module
/// docs).
///
/// The caller must have bounded `subnet_ids` already: it is cloned into the call below, so an
/// unbounded list would size this work from the request. Every handler that reaches here has
/// put it through [`super::stored_list_violation`] first, because it is a list the create
/// would also PERSIST — and that ceiling is well inside the largest page EC2 answers a
/// `DescribeSubnets` that names its ids with, so the reported set below is never a partial
/// page.
pub async fn violation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> Option<ValidationException> {
    match describe(ctx, region, subnet_ids).await {
        Reported::Subnets(reported) => absent_violation(subnet_ids, &reported),
        Reported::NoSuchSubnet => Some(service_validation(SUBNETS_ABSENT_MESSAGE.to_string())),
        Reported::Unanswered => None,
    }
}

/// The modelled `ValidationException` (HTTP 400) a request placing a resource of a CLUSTER
/// in `subnet_ids` is refused with, or `None` when the placement resolves and lies in the
/// cluster's own VPC.
///
/// THE RULE: a subnet a request places compute nodes in must belong to the same VPC as the
/// cluster those nodes join ("The subnets must be in the same VPC as the cluster",
/// `UpdateComputeNodeGroupRequest$subnetIds`), because an instance in another VPC could
/// never reach the controller the cluster's own subnets hold. `cluster_subnet_ids` is that
/// cluster's stored control-plane placement (`Networking$subnetIds`), so the VPC a request
/// is judged against is the one the cluster was created over rather than anything the
/// request itself asserts.
///
/// Both questions are EC2's and both answers come from EC2: whether every id the request
/// named resolves at all ([`SUBNETS_ABSENT_MESSAGE`], decided exactly as [`violation`]
/// decides it, and reported FIRST — an id that names no subnet has no VPC to compare), and
/// which VPC each resolved subnet is in ([`SUBNETS_OUTSIDE_CLUSTER_VPC_MESSAGE`]). A VPC
/// comparison EC2 did not ground refuses nothing: an emulator with no EC2 to ask, an answer
/// this emulator could not read, or a cluster whose own subnets EC2 reports no VPC for
/// leaves the placement served, exactly as the module docs say of silence.
///
/// Both lists are bounded before they reach here — the request's by
/// [`super::stored_list_violation`], the cluster's by that same ceiling when it was stored —
/// so neither hop below is sized by a caller.
pub async fn placement_violation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
    cluster_subnet_ids: &[String],
) -> Option<ValidationException> {
    let placed = match describe(ctx, region, subnet_ids).await {
        Reported::Subnets(reported) => {
            if let Some(refusal) = absent_violation(subnet_ids, &reported) {
                return Some(refusal);
            }
            reported
        }
        Reported::NoSuchSubnet => {
            return Some(service_validation(SUBNETS_ABSENT_MESSAGE.to_string()))
        }
        Reported::Unanswered => return None,
    };
    // The cluster's own subnets are asked about separately, so what EC2 answers about THEM
    // can only decide the VPC comparison: a control-plane subnet that has stopped resolving
    // says nothing about the subnets this request named.
    let Reported::Subnets(control_plane) = describe(ctx, region, cluster_subnet_ids).await else {
        return None;
    };
    let cluster_vpcs: HashSet<&str> = control_plane.values().filter_map(Option::as_deref).collect();
    if cluster_vpcs.is_empty() {
        return None;
    }
    let outside = placed
        .values()
        .filter_map(Option::as_deref)
        .any(|vpc| !cluster_vpcs.contains(vpc));
    outside.then(|| service_validation(SUBNETS_OUTSIDE_CLUSTER_VPC_MESSAGE.to_string()))
}

/// The refusal for a request naming an id EC2 ANSWERED with no subnet for, or `None` when
/// every id it named is in that answer.
///
/// EC2 reports one subnet per id it resolved, so an id it reported nothing for names no
/// subnet of the account — the same thing [`SUBNETS_ABSENT_MESSAGE`] says.
fn absent_violation(
    subnet_ids: &[String],
    reported: &HashMap<String, Option<String>>,
) -> Option<ValidationException> {
    let unresolved = subnet_ids.iter().any(|id| !reported.contains_key(id));
    unresolved.then(|| service_validation(SUBNETS_ABSENT_MESSAGE.to_string()))
}

/// What EC2 answered about the subnet ids it was asked for.
enum Reported {
    /// EC2 answered: the VPC of every id it resolved a subnet for, keyed by that subnet id.
    /// An id it reported no subnet for is absent from the map, and an id it reported a
    /// subnet without a `VpcId` for maps to `None`.
    Subnets(HashMap<String, Option<String>>),
    /// EC2 reported that a value names no subnet of this account
    /// ([`SUBNET_ID_REFUSAL_PREFIX`]).
    NoSuchSubnet,
    /// Nothing was answered: there was nothing to ask, no Region label to ask in, or the hop
    /// could not be made or read (see the module docs). Grounds no refusal.
    Unanswered,
}

/// Ask EC2 about `subnet_ids` once, over the in-process callback, and report what it
/// answered.
///
/// `subnet_ids` is cloned into the call, so the caller must have bounded it; an EMPTY list is
/// [`Reported::Unanswered`] without a hop, because a `DescribeSubnets` that names no id
/// answers about EVERY subnet of the account, which is a question no request put.
async fn describe(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_ids: &[String],
) -> Reported {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    if subnet_ids.is_empty() {
        return Reported::Unanswered;
    }
    // No Region label, so no endpoint to ask EC2 at: the same answer an unreachable EC2
    // gives, which grounds no refusal.
    let Some(client) = super::ec2_client::ec2_client(ctx, region) else {
        return Reported::Unanswered;
    };
    let answer = client
        .describe_subnets()
        .set_subnet_ids(Some(subnet_ids.to_vec()))
        .send()
        .await;
    let error = match answer {
        Ok(output) => {
            return Reported::Subnets(
                output
                    .subnets()
                    .iter()
                    .filter_map(|subnet| {
                        Some((
                            subnet.subnet_id()?.to_string(),
                            subnet.vpc_id().map(str::to_string),
                        ))
                    })
                    .collect(),
            )
        }
        Err(error) => error,
    };
    let code = error
        .as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .unwrap_or_default();
    if !code.starts_with(SUBNET_ID_REFUSAL_PREFIX) {
        // At `warn`: a deployment where this hop fails is one where this gate does not run
        // at all, which is worth seeing, and it IS why a placement was not resolved.
        ::tracing::warn!(
            %region,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the subnets a PCS create named could not be resolved against EC2"
        );
        return Reported::Unanswered;
    }
    Reported::NoSuchSubnet
}
