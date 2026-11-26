// SPDX-License-Identifier: CC0-1.0

//! The ONE construction of the in-process Amazon CloudWatch client an emulated service drives,
//! so no two services configure that hop differently — the sibling of [`crate::sns_client`] and
//! [`crate::ssm_client`] for the account's metrics and alarms.
//!
//! A service that publishes an observation or reads a metric on another principal's behalf — a
//! container's own program publishing as the task role among them — reaches CloudWatch the way
//! any emulated service reaches another: the AWS SDK driven over the emulator's own in-process
//! transport, through the operation callback
//! ([`smithy_server_runtime::OperationContext::callback`]). The transport is what makes the call
//! in-process: no socket is opened, no name is resolved, and nothing outside this process is
//! reachable from it. The endpoint URL below is addressing for that transport, not a host this
//! process connects to, and it is built from the Region alone — so a Region a request carries
//! selects an endpoint of THIS emulator and can never be an address.

use smithy_server_runtime::EmbeddedEmulator;

/// The CloudWatch client a service drives, addressed to `region`.
///
/// CloudWatch holds its metrics and alarms per Region, so `region` is the Region of the work
/// being asked for. The transport is in-process, so there is nothing to retry, time out or watch
/// for a stalled stream — each of those would need a sleep implementation this client is not
/// given — and the credentials the callback hands over are used directly rather than cached.
/// Those credentials are the emulator's own: the caller's `Authorization` header is never
/// forwarded.
#[must_use]
pub fn cloudwatch_client(
    callback: &EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_cloudwatch::Client {
    client_signing_with(callback, region, callback.aws_credentials_provider())
}

/// The same client, signing every call it makes as `credentials` rather than with the emulator's
/// own.
///
/// What a service uses when the call it makes is one ANOTHER principal makes through it — a
/// container's own program publishing as the task role, say — so CloudWatch authorizes it against
/// the policies in force on that principal, exactly as it authorizes any other signed request.
/// The credentials are a session an emulated service minted through the token service; nothing
/// here constructs an access key id or a secret.
#[must_use]
pub fn cloudwatch_client_signed_as(
    callback: &EmbeddedEmulator,
    region: &str,
    credentials: ::aws_sdk_cloudwatch::config::Credentials,
) -> ::aws_sdk_cloudwatch::Client {
    client_signing_with(
        callback,
        region,
        ::aws_sdk_cloudwatch::config::SharedCredentialsProvider::new(credentials),
    )
}

/// The ONE builder both constructions above share, so the hop is configured one way whoever the
/// call is signed as.
fn client_signing_with(
    callback: &EmbeddedEmulator,
    region: &str,
    credentials: impl ::aws_sdk_cloudwatch::config::ProvideCredentials + 'static,
) -> ::aws_sdk_cloudwatch::Client {
    let config = ::aws_sdk_cloudwatch::Config::builder()
        .behavior_version(::aws_sdk_cloudwatch::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(credentials)
        .region(::aws_sdk_cloudwatch::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://monitoring.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_cloudwatch::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_cloudwatch::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_cloudwatch::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_cloudwatch::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_cloudwatch::Client::from_conf(config)
}
