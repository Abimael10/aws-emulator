// SPDX-License-Identifier: CC0-1.0

//! The staging-area subnet a replication configuration names: a fact this service
//! RESOLVES rather than holds.
//!
//! A subnet is not a resource of this service — it is Amazon EC2's.
//! `stagingAreaSubnetId` is "the subnet to be used by the replication staging area"
//! (`CreateReplicationConfigurationTemplateRequest$stagingAreaSubnetId`), a subnet of
//! the account that must already exist for a replication server to be placed in it, so
//! a call that stores a caller-named subnet resolves it BEFORE it stores anything, and
//! EC2 is the one answer to "does this account hold that subnet".
//!
//! EC2 is reached the way any emulated service reaches another, over the in-process
//! callback ([`smithy_server_runtime::OperationContext::callback`]), so this crate
//! names no other service's crate and nothing leaves the process: the callback IS the
//! transport, so asking EC2 opens no socket and resolves no host name — the endpoint
//! URL below only tells the SDK which service and Region the question is for, and the
//! Region is read as a Region LABEL first, so a caller-supplied value can never
//! re-point that authority. Nothing is cached and nothing is stored here: a subnet
//! created a moment ago resolves at once, and one that was deleted stops resolving at
//! once.
//!
//! Every refusal built here is grounded in what EC2 ANSWERED. A hop that could not be
//! made at all — an emulator with no EC2 mounted, an answer this emulator could not
//! read — is this service failing to ask rather than EC2 reporting a subnet the
//! account does not have, so it refuses nothing: a refusal assumed from silence would
//! cost a call that EC2 would have resolved.

use super::field_validation_failed;
use crate::ops::types::ValidationException;

/// The prefix of every EC2 error code that says a value names no subnet of the
/// account: `InvalidSubnetID.NotFound` for an id no subnet carries,
/// `InvalidSubnetID.Malformed` for a value that is not a subnet id at all. Both say
/// the same thing about a staging area.
const SUBNET_ID_REFUSAL_PREFIX: &str = "InvalidSubnetID.";

/// What this service answers a call whose staging-area subnet it could not resolve
/// against the account's network: the fixed text, with the subnet id the REQUEST named.
pub fn subnet_absent_message(subnet_id: &str) -> String {
    format!("SubnetId {subnet_id} wasn't found")
}

/// The EC2 client this service asks about the subnet a request named, addressed to the
/// Region the request was signed for, or `None` when there is no Region label to ask
/// in.
///
/// The Region a request is signed for is CALLER-SUPPLIED text, and here it ends up in
/// an endpoint URL's authority rather than in a response, so it is read as a Region
/// label (`service_common::arn::region_label`, the tree's one reading of that grammar)
/// instead of being interpolated as given: a value carrying a host or path delimiter
/// would name an authority the caller chose rather than the peer this emulator asks.
///
/// The transport is in-process, so there is nothing to retry, time out or watch for a
/// stalled stream — and each of those would need a sleep implementation this client is
/// not given. The same goes for caching the identity: the credentials the callback
/// hands over are used directly.
fn ec2_client(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
) -> Option<::aws_sdk_ec2::Client> {
    let label = ::service_common::arn::region_label(region)?;
    let config = ::aws_sdk_ec2::Config::builder()
        .behavior_version(::aws_sdk_ec2::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_ec2::config::Region::new(label.to_string()))
        .endpoint_url(format!("https://ec2.{label}.amazonaws.com"))
        .retry_config(::aws_sdk_ec2::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_ec2::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_ec2::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_ec2::config::StalledStreamProtectionConfig::disabled())
        .build();
    Some(::aws_sdk_ec2::Client::from_conf(config))
}

/// The modelled `ValidationException` (HTTP 400) a call naming `subnet_id` for its
/// replication staging area is refused with, or `None` when the subnet resolves and
/// the call may proceed.
///
/// `Some` only where EC2 ANSWERED that the id names no subnet of this account in this
/// Region: it reports `InvalidSubnetID.*` for such an id, and an id it reported no
/// subnet for says the same thing. `None` for every answer that is not such a report,
/// which is this emulator failing to ask (see the module docs).
pub async fn violation(
    ctx: &smithy_server_runtime::OperationContext<'_>,
    region: &str,
    subnet_id: &str,
) -> Option<ValidationException> {
    use ::aws_smithy_types::error::metadata::ProvideErrorMetadata;

    // No Region label, so no endpoint to ask EC2 at: the same answer an unreachable
    // EC2 gives, which grounds no refusal.
    let client = ec2_client(ctx, region)?;
    let answer = client
        .describe_subnets()
        .subnet_ids(subnet_id)
        .send()
        .await;
    let error = match answer {
        Ok(output) => {
            // EC2 reports one subnet per id it resolved, so an answer carrying none
            // for this id says the account holds no such subnet.
            let resolved = output
                .subnets()
                .iter()
                .filter_map(::aws_sdk_ec2::types::Subnet::subnet_id)
                .any(|reported| reported == subnet_id);
            return (!resolved).then(|| field_validation_failed(subnet_absent_message(subnet_id)));
        }
        Err(error) => error,
    };
    let code = error
        .as_service_error()
        .and_then(ProvideErrorMetadata::code)
        .unwrap_or_default();
    if !code.starts_with(SUBNET_ID_REFUSAL_PREFIX) {
        // At `warn`: a deployment where this hop fails is one where this resolution
        // does not run at all, which is worth seeing, and it IS why a staging area was
        // not resolved.
        ::tracing::warn!(
            %region,
            error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
            "the staging-area subnet a DRS call named could not be resolved against EC2"
        );
        return None;
    }
    Some(field_validation_failed(subnet_absent_message(subnet_id)))
}
