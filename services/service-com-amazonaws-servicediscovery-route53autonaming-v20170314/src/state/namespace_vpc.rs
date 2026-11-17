// SPDX-License-Identifier: CC0-1.0

//! The Amazon VPC a private DNS namespace is created for, resolved in Amazon EC2.
//!
//! "A private namespace based on DNS, which is visible only inside a specified Amazon VPC"
//! (`CreatePrivateDnsNamespace`): the namespace IS an association between a Route 53 private
//! hosted zone and that VPC, so a `Vpc` naming no VPC of the account is a namespace that can
//! never be created — the asynchronous step that associates the zone has nothing to
//! associate it with, and the operation the create answered with lands `FAIL` instead of
//! `SUCCESS`.
//!
//! EC2 is reached the way any emulated service reaches another: the AWS SDK driven over the
//! emulator's OWN in-process transport, through the operation callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so no socket is opened, no name
//! is resolved and nothing outside this process is reachable from it.
//!
//! The ask FAILS OPEN, for the reason the other cross-service pre-flights in this workspace
//! do: an emulator that mounts no EC2 at all must not make a Cloud Map call fail that the
//! real service serves. Only EC2's own "no such VPC" refusal resolves the VPC as absent;
//! anything else — and no EC2 to ask — leaves it unresolved, and the create proceeds.

/// The EC2 error code for a read that named a VPC id the account holds no VPC for, which is
/// the one answer that resolves a VPC as absent.
const VPC_NOT_FOUND_CODE: &str = "InvalidVpcID.NotFound";

/// What Amazon EC2 says about the VPC a private namespace named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VpcPresence {
    /// EC2 holds a VPC of this account with that id in this Region.
    Present,
    /// EC2 holds no VPC of this account with that id in this Region.
    Absent,
    /// EC2 answered nothing about it, so it is resolved neither way (see the module docs).
    Unanswered,
}

/// Resolve the VPC `vpc_id` of this account in `region` against Amazon EC2.
pub async fn resolve(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    vpc_id: &str,
) -> VpcPresence {
    let Some(client) = ec2_client(ctx, region) else {
        unresolved("there is no Amazon EC2 in this process to ask");
        return VpcPresence::Unanswered;
    };
    match client.describe_vpcs().vpc_ids(vpc_id).send().await {
        // EC2 refuses a read naming a VPC it holds none of, so an answer with no row for
        // the id asked about is the same answer as that refusal.
        Ok(output) => {
            if output
                .vpcs()
                .iter()
                .any(|vpc| vpc.vpc_id() == Some(vpc_id))
            {
                VpcPresence::Present
            } else {
                VpcPresence::Absent
            }
        }
        Err(error) => {
            use ::aws_sdk_ec2::error::ProvideErrorMetadata;
            if error.code() == Some(VPC_NOT_FOUND_CODE) {
                return VpcPresence::Absent;
            }
            unresolved(&format!("Amazon EC2 answered {:?}", error.code()));
            VpcPresence::Unanswered
        }
    }
}

/// The `Operation$ErrorMessage` the asynchronous step reports when the VPC is absent.
///
/// A private namespace's asynchronous work is the association of its Route 53 private hosted
/// zone with that VPC, and the refusal a caller sees is the one that association fails with:
/// Route 53's invalid-VPC sentence, naming the VPC the request asked for and the Region it
/// was asked in. Both variable parts come from the request.
pub fn absent_vpc_message(vpc_id: &str, region: &str) -> String {
    format!("The VPC: {vpc_id} not found in region {region}")
}

/// Trace a VPC Amazon EC2 answered nothing about, which this service resolves neither way
/// (see the module docs).
///
/// `why` is this service's own text, never a caller value.
fn unresolved(why: &str) {
    ::tracing::warn!(
        why = %why,
        "the VPC a private DNS namespace is visible inside was not resolved in Amazon EC2"
    );
}

/// An AWS SDK client for Amazon EC2 over the in-process callback into this same emulator, or
/// `None` when `region` is not a Region LABEL.
///
/// The callback is the framework's documented seam for calling another emulated service: it
/// dispatches IN PROCESS to a service registered in this very emulator, so no socket is
/// opened and nothing leaves the host. The endpoint URL is only the address the SDK needs to
/// name the service and the Region it is addressing — the callback's HTTP client never
/// resolves or connects to it — which is also why there is nothing to retry, time out, watch
/// for a stalled stream or cache an identity for.
///
/// The Region a request is signed for is CALLER-SUPPLIED text, and here it ends up in that
/// URL's authority, so it is read as a Region label (`service_common::arn::region_label`, the
/// one place this tree states that grammar) instead of being interpolated as given.
fn ec2_client(
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
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}
