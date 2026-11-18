// SPDX-License-Identifier: CC0-1.0

//! Whether a certificate a request NAMES is one the ACCOUNT holds and one a listener can
//! actually present — neither of which is this service's to say.
//!
//! `Certificate$CertificateArn` is "The Amazon Resource Name (ARN) of the certificate", and
//! the certificate it names is another service's resource: a Certificate Manager
//! certificate (`acm:RequestCertificate` / `acm:ImportCertificate` create one and
//! `acm:DescribeCertificate` reports it), or a server certificate uploaded to IAM
//! (`iam:UploadServerCertificate` creates one and `iam:GetServerCertificate` reports it).
//! So an operation of this API that takes a certificate ARN keeps no list of which
//! certificates exist — it ASKS the service that owns them, over the emulator's in-process
//! callback ([`smithy_server_runtime::OperationContext::callback`]), and a certificate that
//! service holds none for cannot be presented by a listener of this one.
//!
//! The FORM of the value is read first and without any call
//! ([`super::names_a_certificate`]): a value that is no certificate ARN at all names no
//! certificate whatever those services hold. This module answers the question that form
//! cannot — whether the certificate it names is there.
//!
//! Nothing here opens a socket or leaves the host: the transport is the emulator's own
//! in-process channel into its own services, and the endpoint is only the address that
//! channel resolves the owning service by. The credentials are the emulator's own, used to
//! sign the read, and are never returned or logged.
//!
//! READ ONLY, and nothing is cached: two reads of one ARN ask twice, so a certificate
//! imported between them is seen. An owning service that did not ANSWER establishes
//! nothing, so its silence reads as "held" rather than refusing a request for want of
//! another service — a refusal is never assumed from silence, exactly as the facts
//! [`super::network`] resolves against EC2 are not.
//!
//! # Held is not the same as usable
//!
//! A listener that terminates TLS presents the certificate, so it needs one the issuing
//! service can hand over: "the default certificate for the listener" is key material the
//! load balancer serves a handshake with, not a reference it merely records. Certificate
//! Manager reports whether a certificate it holds reached that point
//! (`CertificateDetail$Status`: "A certificate enters status PENDING_VALIDATION upon being
//! requested … unless it fails"), and only an `ISSUED` certificate has key material
//! behind it — a request that never validated, timed out, failed, was revoked or expired
//! holds none. So a certificate the owning service reports in any other status is one no
//! listener can present, and the operation that named it is refused under
//! [`UNSUPPORTED_CERTIFICATE`] rather than creating a listener that could never serve a
//! handshake. That is a DIFFERENT verdict from a certificate the account does not hold at
//! all ([`CertificateDefect`]), and the two are reported apart because a caller fixes them
//! differently: one by naming another certificate, the other by waiting for or repairing
//! the one it named.

use ::smithy_server_runtime::OperationContext;

use super::{names_a_certificate, request_region, IAM_SERVER_CERTIFICATE};
use crate::ops::types::Certificate;

/// The wire error code Elastic Load Balancing refuses a certificate a listener cannot
/// present with.
///
/// The model declares no error shape for it, so the code lives here beside the rule that
/// decides it and is answered through [`crate::scaffolded::Scaffolded::unsupported_certificate`]
/// — the same arrangement as the service's other unmodelled codes
/// (`crate::scaffolded::VALIDATION_ERROR`,
/// `crate::service::target_group_authorization::ACCESS_DENIED`).
pub const UNSUPPORTED_CERTIFICATE: &str = "UnsupportedCertificate";

/// What makes a certificate a request NAMES one the operation cannot be served with.
///
/// Both verdicts come from the service that OWNS the certificate, and each has its own
/// refusal, because the caller fixes them differently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertificateDefect {
    /// That service holds no certificate under the ARN at all, which is the modelled
    /// `CertificateNotFoundException`.
    Absent,
    /// It holds one, and reports it in a status other than `ISSUED` — so there is no key
    /// material to present and the certificate is [`UNSUPPORTED_CERTIFICATE`].
    NotIssued,
}

/// The first `CertificateArn` of `certificates` the service that OWNS the certificate
/// cannot serve a TLS-terminating listener with, and why — what an operation that resolves
/// a requested certificate refuses on, read in the order the request listed them.
///
/// Called with this service's store NOT locked: every answer comes from another service,
/// and no store of this one is held across the call.
pub async fn unusable_certificate<'a>(
    ctx: &OperationContext<'_>,
    certificates: &'a [Certificate],
) -> Option<(&'a str, CertificateDefect)> {
    for arn in certificates
        .iter()
        .filter_map(|certificate| certificate.certificate_arn.as_deref())
        .filter(|arn| !arn.is_empty())
    {
        if let Some(defect) = certificate_defect(ctx, arn).await {
            return Some((arn, defect));
        }
    }
    None
}

/// The refusal a certificate no listener can present earns, naming the value the request
/// carried — the only thing the caller can act on.
///
/// Elastic Load Balancing's own sentence for the code: the fixed text names what a
/// presentable certificate must have, and the one variable part is the ARN the request
/// asked for. Generic over the operation's error channel so every operation that attaches
/// a certificate states one sentence under one code.
pub fn unsupported_certificate<E>(arn: &str) -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::unsupported_certificate(format!(
        "The certificate '{arn}' must have a fully-qualified domain name, a supported \
         signature, and a supported key size."
    ))
}

/// What makes the certificate `arn` names unusable, as the service that owns it reports it.
///
/// `None` is "nothing establishes a defect": the certificate is there and presentable, or
/// the owning service did not answer. A value of no certificate ARN form is left to the
/// form reader ([`names_a_certificate`]), which refuses it with the same modelled error
/// and needs no call to settle it, so it is reported as sound here rather than refused
/// twice.
async fn certificate_defect(ctx: &OperationContext<'_>, arn: &str) -> Option<CertificateDefect> {
    if !names_a_certificate(arn) {
        return None;
    }
    if IAM_SERVER_CERTIFICATE.admits(arn) {
        // A server certificate uploaded to IAM is uploaded WITH its private key
        // (`iam:UploadServerCertificate` takes `PrivateKey`), so one IAM holds is one a
        // listener can present: the only defect that service establishes is its absence.
        return iam_server_certificate_is_absent(ctx, arn)
            .await
            .then_some(CertificateDefect::Absent);
    }
    acm_certificate_defect(ctx, arn).await
}

/// The endpoint prefix the in-process channel resolves Certificate Manager by, which is
/// also the vendor field of every Certificate Manager certificate ARN
/// (`::service_common::arn::ACM_CERTIFICATE`).
const ACM_ENDPOINT_PREFIX: &str = "acm";

/// What Certificate Manager establishes about the certificate `arn` names: that it holds
/// none under it, or that the one it holds is in no state to be presented.
///
/// A certificate is a REGIONAL resource of that service and its ARN carries the Region it
/// belongs to, so the read is addressed to the ARN's own Region: a certificate of another
/// Region is resolved where it lives rather than looked for where the request arrived.
async fn acm_certificate_defect(
    ctx: &OperationContext<'_>,
    arn: &str,
) -> Option<CertificateDefect> {
    let region = ::service_common::arn::parse(arn)
        .map(|parsed| parsed.region)
        .filter(|region| !region.is_empty())
        .map_or_else(|| request_region(ctx), str::to_string);
    let answer = acm_client(ctx, &region)
        .describe_certificate()
        .certificate_arn(arn)
        .send()
        .await;
    let error = match answer {
        Ok(reported) => {
            // The certificate is there. Whether it can be PRESENTED is the status that
            // service reports for it: only an issued certificate has key material behind
            // it, so any other status is a certificate no listener can serve a handshake
            // with.
            let status = reported
                .certificate()
                .and_then(::aws_sdk_acm::types::CertificateDetail::status);
            return match status {
                Some(status) if *status != ::aws_sdk_acm::types::CertificateStatus::Issued => {
                    Some(CertificateDefect::NotIssued)
                }
                // A certificate reported with no status at all establishes nothing about
                // its key material, so nothing is refused on it.
                _ => None,
            };
        }
        Err(error) => error,
    };
    match error.as_service_error() {
        // The two answers that establish the absence: that service holds no certificate
        // under the ARN, and that the value is not one it could hold a certificate under.
        Some(refused) => (refused.is_resource_not_found_exception()
            || refused.is_invalid_arn_exception())
        .then_some(CertificateDefect::Absent),
        None => {
            ::tracing::debug!(
                certificate = %arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the certificate manager did not answer whether the certificate exists"
            );
            None
        }
    }
}

/// Whether IAM reports no server certificate under `arn`.
///
/// `iam:GetServerCertificate` takes the certificate's NAME rather than its ARN, and the
/// name is the last segment of the ARN's resource field: IAM writes a server certificate's
/// resource as `server-certificate/<path>/<name>`, where the path is optional. IAM is a
/// partition-global service, so the read is addressed to the Region the request arrived in.
async fn iam_server_certificate_is_absent(ctx: &OperationContext<'_>, arn: &str) -> bool {
    let Some(name) = ::service_common::arn::resource(arn)
        .and_then(|resource| resource.rsplit(['/', ':']).next())
        .filter(|name| !name.is_empty())
    else {
        return false;
    };
    let answer = ::service_common::iam_client::iam_client(ctx, &request_region(ctx))
        .get_server_certificate()
        .server_certificate_name(name)
        .send()
        .await;
    let error = match answer {
        Ok(_) => return false,
        Err(error) => error,
    };
    match error.as_service_error() {
        // IAM's own answer for an entity it holds nothing under, which is what a value
        // naming no server certificate of the account earns.
        Some(refused) => refused.is_no_such_entity_exception(),
        None => {
            ::tracing::debug!(
                certificate = %arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "identity management did not answer whether the server certificate exists"
            );
            false
        }
    }
}

/// The Certificate Manager client this service resolves a requested certificate through:
/// the AWS SDK driven over the emulator's own in-process transport, so this crate names no
/// other service's crate.
fn acm_client(ctx: &OperationContext<'_>, region: &str) -> ::aws_sdk_acm::Client {
    let config = ::aws_sdk_acm::Config::builder()
        .behavior_version(::aws_sdk_acm::config::BehaviorVersion::latest())
        .http_client(ctx.callback().http_client())
        .credentials_provider(ctx.callback().aws_credentials_provider())
        .region(::aws_sdk_acm::config::Region::new(region.to_string()))
        .endpoint_url(format!("https://{ACM_ENDPOINT_PREFIX}.{region}.amazonaws.com"))
        // The transport is in-process: there is nothing to retry, time out or watch for a
        // stalled stream, and each of those would need a sleep implementation this client
        // is not given. The same goes for caching the identity: the credentials are handed
        // over directly.
        .retry_config(::aws_sdk_acm::config::retry::RetryConfig::disabled())
        .timeout_config(::aws_sdk_acm::config::timeout::TimeoutConfig::disabled())
        .identity_cache(::aws_sdk_acm::config::IdentityCache::no_cache())
        .stalled_stream_protection(::aws_sdk_acm::config::StalledStreamProtectionConfig::disabled())
        .build();
    ::aws_sdk_acm::Client::from_conf(config)
}
