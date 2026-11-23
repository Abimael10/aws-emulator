// SPDX-License-Identifier: CC0-1.0

//! The ONE construction of the in-process Amazon SNS client an emulated service PUBLISHES a
//! notification through, so no two services configure that hop differently — the sibling of
//! [`crate::s3_client`], [`crate::eventbridge_client`] and [`crate::kinesis_client`] for the
//! account's topics.
//!
//! A service that notifies a topic a client named — the status of an asynchronous task it
//! finished among them — reaches Amazon SNS the way any emulated service reaches another: the
//! AWS SDK driven over the emulator's own in-process transport, through the operation callback
//! ([`smithy_server_runtime::OperationContext::callback`]). The transport is what makes the
//! publish in-process: no socket is opened, no name is resolved, and nothing outside this
//! process is reachable from it. The endpoint URL below is addressing for that transport, not a
//! host this process connects to, and it is built from the Region alone — no part of a caller's
//! request is a destination, so a topic ARN a request carries selects a topic of THIS emulator
//! and can never be an address.
//!
//! The copies that predate this module — one per service that publishes — are to be MIGRATED
//! onto it, each in the change that owns its service. Nothing new should build the client by
//! hand.

use smithy_server_runtime::EmbeddedEmulator;

/// The Amazon SNS client a service publishes a notification through, addressed to `region`.
///
/// A publish only reaches "topics and endpoints in the same Amazon Web Services Region", so
/// `region` is the Region of the resource the notification is about. The transport is
/// in-process, so there is nothing to retry, time out or watch for a stalled stream — each of
/// those would need a sleep implementation this client is not given — and the credentials the
/// callback hands over are used directly rather than cached. Those credentials are the
/// emulator's own: the caller's `Authorization` header is never forwarded.
#[must_use]
pub fn sns_client(callback: &EmbeddedEmulator, region: &str) -> ::aws_sdk_sns::Client {
    client_signing_with(callback, region, callback.aws_credentials_provider())
}

/// The same client, signing every call it makes as `credentials` rather than with the
/// emulator's own.
///
/// What a service uses when the publish it makes is one ANOTHER principal makes through it — a
/// container's own program publishing as the task role, say — so Amazon SNS authorizes it
/// against the policies in force on that principal and against the topic's own policy, exactly
/// as it authorizes any other signed request. The credentials are a session an emulated service
/// minted through the token service; nothing here constructs an access key id or a secret.
#[must_use]
pub fn sns_client_signed_as(
    callback: &EmbeddedEmulator,
    region: &str,
    credentials: ::aws_sdk_sns::config::Credentials,
) -> ::aws_sdk_sns::Client {
    client_signing_with(
        callback,
        region,
        ::aws_sdk_sns::config::SharedCredentialsProvider::new(credentials),
    )
}

/// The ONE builder both constructions above share, so the hop is configured one way whoever the
/// call is signed as.
fn client_signing_with(
    callback: &EmbeddedEmulator,
    region: &str,
    credentials: impl ::aws_sdk_sns::config::ProvideCredentials + 'static,
) -> ::aws_sdk_sns::Client {
    let config = ::aws_sdk_sns::Config::builder()
        .behavior_version(::aws_sdk_sns::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(credentials)
        .region(::aws_sdk_sns::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://sns.{region}.amazonaws.com"))
        .retry_config(::aws_sdk_sns::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_sns::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_sns::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_sns::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_sns::Client::from_conf(config)
}
