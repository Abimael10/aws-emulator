// SPDX-License-Identifier: CC0-1.0

//! The EBS VOLUME a HyperPod node attachment names, as the service that OWNS it reports it.
//!
//! An Amazon EBS volume is not a resource of this service: it is Amazon EC2's. A client
//! creates it with `ec2:CreateVolume` and then names it in `AttachClusterNodeVolume`, whose
//! `VolumeId` is documented as "the unique identifier of YOUR EBS volume to attach" — so EC2
//! is the ONE answer to whether the account holds a volume under that identifier. It is asked
//! the way any emulated service reaches another — the AWS SDK over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), through this crate's one EC2
//! client ([`super::domain_network::ec2`]) — so this crate names no other service's crate and
//! nothing leaves the process.
//!
//! # Only what EC2 ANSWERED is refused
//!
//! A hop that could not be made (no Amazon EC2 in this process, an answer that could not be
//! read) establishes nothing, and nothing is refused on it: a refusal has to be grounded in
//! what the owning service said, which is the rule every cross-service precondition in this
//! tree follows (`services/common/src/network.rs` states which resolution is which). What
//! IS refused is an identifier EC2 itself answered it holds no volume for.
//!
//! # The refusal travels as EC2 worded it
//!
//! The HyperPod volume path does not re-word what EC2 answered: it relays that answer behind
//! its own fixed opening, which names the plane that made the call and the EC2 error code the
//! answer carried, and then EC2's own sentence ([`relayed_ec2_refusal`]). Both variable parts
//! are the owning service's — a volume this service does not own is a fact only EC2 can
//! state.

/// The fixed opening the HyperPod plane relays an EC2 answer behind, up to the error code.
const HYPERPOD_EC2_OPENING: &str = "HyperPod - EC2ErrCode:";

/// What separates the relayed EC2 error code from the sentence EC2 worded.
const HYPERPOD_EC2_SEPARATOR: &str = " : ";

/// The EC2 error codes that ESTABLISH that the account holds no volume under the identifier
/// the request named.
///
/// EC2 answers a readable identifier it holds no volume for as `InvalidVolume.NotFound`, and
/// one it cannot read as a volume identifier at all as a bad parameter value
/// (`InvalidParameterValue`) or a malformed id (`InvalidVolumeID.Malformed`) — an identifier
/// EC2 could never have issued names no volume it could ever have held, so each of the three
/// answers the same question. Every OTHER answer (an authorization failure, an internal
/// fault, a transport error) establishes nothing about the identifier and refuses nothing.
const VOLUME_IS_NOT_HELD_CODES: [&str; 3] = [
    "InvalidVolume.NotFound",
    "InvalidVolumeID.Malformed",
    "InvalidParameterValue",
];

/// How the HyperPod volume path reports an answer EC2 gave it: the plane's own fixed opening,
/// the EC2 error `code`, then `reason` — the sentence EC2 itself worded — verbatim.
///
/// Stated once here so every question this plane puts to EC2 relays the answer the same way.
#[must_use]
pub fn relayed_ec2_refusal(code: &str, reason: &str) -> String {
    format!("{HYPERPOD_EC2_OPENING} {code}{HYPERPOD_EC2_SEPARATOR}{reason}")
}

/// How this service reports that the account holds no EBS volume under the `VolumeId` a
/// request named, or `None` when EC2 holds one — or could not be asked.
///
/// The question is put with `DescribeVolumes` over the one identifier, which is the operation
/// EC2 documents for resolving a volume ("Describes the specified EBS volumes"), and the
/// answer is read as [`VOLUME_IS_NOT_HELD_CODES`] says. The message is EC2's own answer
/// relayed ([`relayed_ec2_refusal`]).
///
/// Call it with NO state guard held — it awaits.
pub async fn volume_not_held_by_ec2(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    volume_id: &str,
) -> Option<String> {
    let client = super::domain_network::ec2(ctx, region)?;
    let error = client
        .describe_volumes()
        .volume_ids(volume_id)
        .send()
        .await
        .err()?;
    let code = ::aws_smithy_types::error::metadata::ProvideErrorMetadata::code(&error)?;
    if !VOLUME_IS_NOT_HELD_CODES.contains(&code) {
        // EC2 refused for a reason that says nothing about whether it holds the volume, so
        // this establishes nothing and the attach is not refused on it.
        ::tracing::warn!(
            %region,
            %code,
            "the volume a cluster-node attachment named could not be resolved against EC2"
        );
        return None;
    }
    // The sentence is EC2's own, so an answer that carries none leaves nothing to relay:
    // that establishes nothing either, exactly as an unrecognised code does, and is never
    // relayed as an opening with an empty sentence behind it.
    let reason = ::aws_smithy_types::error::metadata::ProvideErrorMetadata::message(&error)?;
    Some(relayed_ec2_refusal(code, reason))
}
