// SPDX-License-Identifier: CC0-1.0

//! Whether an ACM certificate a request names is one the account actually holds — which
//! is not this service's to say.
//!
//! Both certificate operations document their `Arn` as "the ARN of the ACM certificate",
//! and that certificate is another service's resource: `acm:RequestCertificate` and
//! `acm:ImportCertificate` are what create one and `acm:DescribeCertificate` is what
//! reports it. So this service keeps no list of which certificates exist — it ASKS the
//! service that OWNS them, over the emulator's in-process callback
//! ([`smithy_server_runtime::OperationContext::callback`]), and an ARN that service holds
//! no certificate under names nothing for this account to associate.
//!
//! The FORM of the value is read first and without any call
//! ([`super::is_acm_certificate_arn`]): a value that is no ACM certificate ARN at all is a
//! problem in the request whatever that service holds, so it is refused there rather than
//! asked about here. This module answers the question the form cannot — whether the
//! certificate it names is there.
//!
//! Nothing here opens a socket or leaves the host: the transport is the emulator's own
//! in-process channel into its own services, and the endpoint is only the address that
//! channel resolves the owning service by. The credentials are the emulator's own, used to
//! sign this read, and are never returned or logged.
//!
//! READ ONLY, and nothing is cached: two reads of one ARN ask twice, so a certificate
//! imported between them is seen.

use ::smithy_server_runtime::OperationContext;

/// The endpoint prefix the in-process channel resolves Certificate Manager by, which is
/// also the service field of every ACM certificate ARN
/// ([`super::is_acm_certificate_arn`]).
const ENDPOINT_PREFIX: &str = "acm";

/// Whether the account holds NO ACM certificate under `arn`.
///
/// True only where the owning service establishes the absence itself: it answers that it
/// holds no certificate under the ARN, or that the value is not one it could hold a
/// certificate under. A service that did not ANSWER establishes nothing, so its silence
/// reads as "held" rather than refusing a request for want of another service — the same
/// reading this crate gives an input it could not probe against S3
/// ([`super::unreadable_job_input`]).
///
/// A certificate is a REGIONAL resource of that service and its ARN carries the Region it
/// belongs to, so the read is addressed to the ARN's own Region: a certificate of another
/// Region is resolved where it lives rather than looked for where the request arrived.
/// The ARN is the whole key — it carries the partition, the Region and the account the
/// certificate belongs to — so a value naming another account's certificate resolves to
/// nothing, the same answer as an ARN naming nothing at all.
pub async fn acm_certificate_is_absent(
    ctx: &OperationContext<'_>,
    region: &str,
    arn: &str,
) -> bool {
    let region = ::service_common::arn::parse(arn)
        .map(|parsed| parsed.region)
        .filter(|parsed| is_region_label(parsed))
        .map_or_else(|| region.to_string(), str::to_string);
    let answer = acm_client(ctx, &region)
        .describe_certificate()
        .certificate_arn(arn)
        .send()
        .await;
    let error = match answer {
        Ok(_) => return false,
        Err(error) => error,
    };
    match error.as_service_error() {
        // The two answers that establish the absence: that service holds no certificate
        // under the ARN, and that the value is not one it could hold a certificate under.
        Some(refused) => {
            refused.is_resource_not_found_exception() || refused.is_invalid_arn_exception()
        }
        None => {
            ::tracing::debug!(
                certificate = %arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the certificate manager did not answer whether the certificate exists"
            );
            false
        }
    }
}

/// Whether a value is of the form an AWS Region name takes — ASCII letters, digits and
/// hyphens, and at least one character.
///
/// The ARN's Region field is CALLER-SUPPLIED and it is the only caller-derived part of the
/// address this read is sent to, so a value of any other form addresses nothing: the read
/// falls back to the Region the request itself was signed for.
fn is_region_label(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
}

/// The Certificate Manager client this service resolves a named certificate through: the
/// AWS SDK driven over the emulator's own in-process transport, so this crate names no
/// other service's crate.
fn acm_client(ctx: &OperationContext<'_>, region: &str) -> ::aws_sdk_acm::Client {
    let config = ::aws_sdk_acm::Config::builder()
        .behavior_version(::aws_sdk_acm::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_acm::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://{ENDPOINT_PREFIX}.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client
        // is not given. The same goes for caching the identity: the credentials are handed
        // over directly.
        .retry_config(::aws_sdk_acm::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_acm::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_acm::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_acm::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_acm::Client::from_conf(config)
}
