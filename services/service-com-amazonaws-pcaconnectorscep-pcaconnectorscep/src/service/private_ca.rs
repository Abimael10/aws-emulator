// SPDX-License-Identifier: CC0-1.0

//! Amazon Web Services Private CA, as this service reaches it.
//!
//! A connector is a link between a SCEP endpoint and a private certificate authority that
//! Private CA — not this service — owns. So every question about that CA is answered by that
//! service's own API, over the emulator's in-process callback: whether the CA a request
//! names exists at all and whether that service reports it in service
//! ([`resolve`], which `CreateConnector` resolves its `CertificateAuthorityArn` through
//! before admitting a connector, for a CA its own partition, Region and account place within
//! that connector's reach), and the certificate it holds
//! (`GetCertificateAuthorityCertificate`, which the SCEP data plane answers `GetCACert`
//! from).
//!
//! Whether the CALLER may have a certificate issued from that CA is a question about the
//! caller's own permissions rather than about the CA, and it is answered where those are held
//! ([`super::provisioning_authorization`]).
//!
//! NOTHING here opens a socket or leaves the host: the transport is
//! `ctx.callback().http_client()`, the emulator's in-process channel into its own services,
//! and the endpoint is only the address that channel resolves the private CA by. The
//! credentials are the emulator's own and are used to sign these calls; they are never
//! returned or logged.

use ::aws_sdk_acmpca::types::CertificateAuthorityStatus;
use ::smithy_server_runtime::OperationContext;

/// The endpoint prefix the in-process channel resolves Amazon Web Services Private CA by,
/// which is also the service segment of every `CertificateAuthorityArn` this service accepts
/// (`^arn:aws(-[a-z]+)*:acm-pca:…`).
const ENDPOINT_PREFIX: &str = "acm-pca";

/// What Amazon Web Services Private CA answered about the certificate authority one request
/// named.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolved {
    /// That service answered its own not-found error, which is the one answer that
    /// establishes the CA is absent.
    Absent,
    /// That service holds the CA and reports it in a status other than `ACTIVE`: a CA on its
    /// way into service, or one taken out of it, is not a CA a connector can broker through.
    NotActive,
    /// That service holds a CA it reports `ACTIVE` — and equally, the answer when the
    /// question could not be put at all, because absence and status are established by the
    /// owning service's own read and never inferred from a dependency that did not answer.
    Active,
}

impl Resolved {
    /// Whether Private CA reports it holds no certificate authority under the ARN asked
    /// about, which is one of the two outcomes that refuses a `CreateConnector` call outright.
    pub fn is_absent(self) -> bool {
        self == Self::Absent
    }

    /// Whether Private CA reports the CA in a status other than `ACTIVE`, which is the other.
    pub fn is_not_active(self) -> bool {
        self == Self::NotActive
    }
}

/// Resolve `certificate_authority_arn` against Amazon Web Services Private CA: ONE read of
/// that service, answering both whether it holds the CA and whether the status it reports is
/// the one in which a CA issues certificates.
pub async fn resolve(
    ctx: &OperationContext<'_>,
    region: &str,
    certificate_authority_arn: &str,
) -> Resolved {
    let answer = client(ctx, region)
        .describe_certificate_authority()
        .certificate_authority_arn(certificate_authority_arn)
        .send()
        .await;
    let error = match answer {
        // The status is the owning service's own report of the CA, so a CA described without
        // one says nothing about whether it is in service.
        Ok(output) => {
            let status = output
                .certificate_authority()
                .and_then(|authority| authority.status());
            return match status {
                None | Some(CertificateAuthorityStatus::Active) => Resolved::Active,
                Some(_) => Resolved::NotActive,
            };
        }
        Err(error) => error,
    };
    match error.as_service_error() {
        Some(service_error) if service_error.is_resource_not_found_exception() => Resolved::Absent,
        Some(_) => Resolved::Active,
        None => {
            ::tracing::warn!(
                certificate_authority = %certificate_authority_arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the private CA service did not answer whether the certificate authority exists"
            );
            Resolved::Active
        }
    }
}

/// The Amazon Web Services Private CA client this service reads that service through: the AWS
/// SDK driven over the emulator's own in-process transport, signing in the emulator's name —
/// Connector for SCEP reads the CA it was given with its own service principal.
pub fn client(ctx: &OperationContext<'_>, region: &str) -> ::aws_sdk_acmpca::Client {
    let config = ::aws_sdk_acmpca::Config::builder()
        .behavior_version(::aws_sdk_acmpca::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_acmpca::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://{ENDPOINT_PREFIX}.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client is
        // not given. The same goes for caching the identity: the credentials are handed over
        // directly.
        .retry_config(::aws_sdk_acmpca::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_acmpca::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_acmpca::config::IdentityCache::no_cache())
        .stalled_stream_protection(
            ::aws_sdk_acmpca::config::StalledStreamProtectionConfig::disabled(),
        )
        .build();
    ::aws_sdk_acmpca::Client::from_conf(config)
}
