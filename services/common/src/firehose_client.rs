// SPDX-License-Identifier: CC0-1.0

//! The ONE construction of the in-process Amazon Data Firehose client an emulated service
//! reaches a delivery stream through, and the ONE question of whether Firehose holds the
//! stream a caller-supplied destination names — the sibling of [`crate::kinesis_client`] and
//! [`crate::s3_client`] for the account's delivery streams.
//!
//! A service POINTED at a delivery stream — a logging configuration's destination, a flow
//! log's, a metric stream's — reaches Firehose the way any emulated service reaches another:
//! the AWS SDK driven over the emulator's own in-process transport, through the operation
//! callback ([`smithy_server_runtime::OperationContext::callback`]). The transport is what
//! makes the hop in-process: no socket is opened, no name is resolved, and nothing outside
//! this process is reachable from it. The endpoint URL below is addressing for that
//! transport, not a host this process connects to, and it is built from the Region alone — no
//! part of a caller's request is a destination.
//!
//! The copies that predate this module — one per service that reaches a delivery stream, and
//! one per service that resolves such a destination
//! (`service-com-amazonaws-ivschat-amazoninteractivevideoservicechat`'s
//! `state::log_destinations`, `service-com-amazonaws-cognitoidentityprovider-…`'s
//! `set_log_delivery_configuration::log_destinations`) — are to be MIGRATED onto it, each in
//! the change that owns its service. Nothing new should build the client, or restate the
//! existence question, by hand.

use smithy_server_runtime::EmbeddedEmulator;

/// The Amazon Data Firehose client a service reaches a delivery stream through, addressed to
/// `region`.
///
/// The transport is in-process, so there is nothing to retry, time out or watch for a stalled
/// stream — each of those would need a sleep implementation this client is not given — and the
/// credentials the callback hands over are used directly rather than cached. Those credentials
/// are the emulator's own: the caller's `Authorization` header is never forwarded.
#[must_use]
pub fn firehose_client(callback: &EmbeddedEmulator, region: &str) -> ::aws_sdk_firehose::Client {
    let config = ::aws_sdk_firehose::Config::builder()
        .behavior_version(::aws_sdk_firehose::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_firehose::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://firehose.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_firehose::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_firehose::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_firehose::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_firehose::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_firehose::Client::from_conf(config)
}

/// Whether Amazon Data Firehose ANSWERED that `region` holds no delivery stream under `name`.
///
/// A service POINTED at a delivery stream it is to deliver into has to resolve that stream
/// where Firehose holds it before it stores the destination, or it leaves an operator
/// believing a delivery exists where no record is ever written. The question is put with
/// `DescribeDeliveryStream`, whose own `ResourceNotFoundException` is Firehose saying the
/// stream is not there.
///
/// `false` — establish nothing — for every answer that is NOT that one: an emulator assembled
/// with no Firehose to ask, and an answer that could not be read. A refusal has to be grounded
/// in what the owning service said, which is the rule every cross-service pre-condition in
/// this tree follows (`services/common/src/network.rs` states which resolution is which).
///
/// The hop is the in-process callback into this same emulator ([`firehose_client`]), so
/// nothing here opens a socket or leaves the process. Call it with NO state guard held — it
/// awaits.
pub async fn delivery_stream_is_absent(
    ctx: &::smithy_server_runtime::OperationContext<'_>,
    region: &str,
    name: &str,
) -> bool {
    match firehose_client(ctx.callback(), region)
        .describe_delivery_stream()
        .delivery_stream_name(name)
        .send()
        .await
    {
        Ok(_) => false,
        Err(error)
            if error.as_service_error().is_some_and(
                ::aws_sdk_firehose::operation::describe_delivery_stream::DescribeDeliveryStreamError
                    ::is_resource_not_found_exception,
            ) =>
        {
            true
        }
        Err(error) => {
            // Not Firehose reporting what it holds, so this establishes nothing about the
            // stream — and it is worth a warning, because an emulator with no Firehose mounted
            // to ask is one where the pre-condition cannot be decided. The caller's own value
            // is rendered in its escaped `Debug` form, so a name carrying a newline cannot
            // forge a log line.
            ::tracing::warn!(
                delivery_stream = ?name,
                detail = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the delivery stream a destination names could not be resolved in this \
                 emulator's Amazon Data Firehose"
            );
            false
        }
    }
}
