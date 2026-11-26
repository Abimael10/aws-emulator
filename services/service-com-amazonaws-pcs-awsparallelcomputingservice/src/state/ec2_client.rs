// SPDX-License-Identifier: CC0-1.0

//! The ONE client this service asks Amazon EC2 with.
//!
//! Several facts a request carries belong to EC2 rather than to this service — the subnets a
//! control-plane ENI would be created in ([`super::ec2_subnets`]) and the security groups it
//! would carry ([`super::ec2_security_groups`]) — and every one of them is asked over this
//! same hop, built here once so no two questions can be addressed differently.
//!
//! EC2 is reached the way any emulated service reaches another, over the in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), so this crate names no other
//! service's crate and nothing leaves the process: the callback IS the transport, so asking
//! EC2 opens no socket and resolves no host name — the endpoint URL below only tells the SDK
//! which service and Region the question is for.

/// The EC2 client this service asks about the network resources a request named, addressed to
/// the Region the request was signed for, or `None` when there is no Region label to ask in.
///
/// The Region a request is signed for is CALLER-SUPPLIED text, and here it ends up in an
/// endpoint URL's authority rather than in a response, so it is read as a Region label
/// (`service_common::arn::region_label`, the tree's one reading of that grammar) instead of
/// being interpolated as given: a value carrying a host or path delimiter would name an
/// authority the caller chose rather than the peer this emulator asks.
///
/// The transport is in-process, so there is nothing to retry, time out or watch for a
/// stalled stream — and each of those would need a sleep implementation this client is not
/// given. The same goes for caching the identity: the credentials the callback hands over
/// are used directly.
pub fn ec2_client(
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
