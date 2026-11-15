// SPDX-License-Identifier: CC0-1.0

//! The one way this service asks AWS KMS about a key: the in-process client, and the note an
//! answer that stated nothing leaves behind.
//!
//! Two decisions of this service turn on facts only KMS holds — whether a topic an alarm action
//! publishes to may have its notification sealed under the key it names
//! ([`super::alarm_actions::sns_topic_authorization`]), and whether the key a dataset
//! association names can be used at all ([`super::dataset_kms_key`]) — and both ask the SAME
//! service the SAME way, so the client is built here once rather than per caller.
//!
//! It is asked of KMS's own API rather than of its store, because this crate cannot depend on
//! the KMS service crate at all: that edge is a cycle cargo refuses
//! (`cloudwatch -> kms -> sts -> cognitoidentityprovider -> cloudwatch`).

/// A KMS client over the in-process callback into this same emulator, addressed in `region`.
pub(crate) fn client(
    callback: &::smithy_server_runtime::EmbeddedEmulator,
    region: &str,
) -> ::aws_sdk_kms::Client {
    let config = ::aws_sdk_kms::Config::builder()
        .behavior_version(::aws_sdk_kms::config::BehaviorVersion::latest())
        .http_client(callback.http_client())
        .credentials_provider(callback.aws_credentials_provider())
        .region(::aws_sdk_kms::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://kms.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The same goes for caching the identity: the credentials are handed over
        // directly.
        .retry_config(::aws_sdk_kms::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_kms::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_kms::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_kms::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_kms::Client::from_conf(config)
}

/// Note that KMS did not state the facts of the key `key_id` names, and so that the key decides
/// nothing where `note` says it would have.
///
/// Worth a line to an operator, because a deployment with no KMS to ask is one where that half
/// of the decision does not run at all; the identifier is rendered in its escaped `Debug` form,
/// so one carrying a newline cannot forge a log line.
pub(crate) fn unstated_key<E: ::std::error::Error + 'static, R: ::std::fmt::Debug>(
    key_id: &str,
    failure: &::aws_sdk_kms::error::SdkError<E, R>,
    note: &str,
) {
    ::tracing::warn!(
        kms_key = ?key_id,
        error = %::aws_smithy_types::error::display::DisplayErrorContext(failure),
        "{note}"
    );
}
