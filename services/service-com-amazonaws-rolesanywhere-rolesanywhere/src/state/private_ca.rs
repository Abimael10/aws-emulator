// SPDX-License-Identifier: CC0-1.0

//! The private certificate authority a trust anchor's source names, as this service reaches
//! it.
//!
//! A trust anchor of the private-CA type holds no certificate of its own: "The root
//! certificate of the Private Certificate Authority specified by this ARN is used in trust
//! validation for temporary credential requests"
//! (`smithy_query(query="shape", shape="SourceData")`, member `acmPcaArn`) — so the authority
//! is a resource ANOTHER service owns, and whether this service can read that root
//! certificate is a question only that service's own API answers. Both trust-anchor writes
//! put it before they store anything ([`source_rejection`]), so a stored anchor is one whose
//! authority was read — and a value that is not the ARN of such an authority at all is
//! refused here without asking, because no authority could carry it.
//!
//! NOTHING here opens a socket or leaves the host: the transport is
//! `ctx.callback().http_client()`, the emulator's in-process channel into its own services,
//! and the endpoint is only the address that channel resolves the private CA service by. The
//! credentials are the emulator's own and only sign this call; they are never returned or
//! logged.

use ::smithy_server_runtime::OperationContext;

use crate::ops::types::{Source, SourceData};
use crate::state::{SourceRejection, ACM_PCA_SOURCE_TYPE};

/// The endpoint prefix the in-process channel resolves the private CA service by, which is
/// also the service segment of every private certificate authority's ARN.
const ENDPOINT_PREFIX: &str = "acm-pca";

/// The resource segment's prefix in such an ARN: the resource type, then the separator before
/// the authority's own id.
const CERTIFICATE_AUTHORITY_RESOURCE: &str = "certificate-authority/";

/// How many digits an Amazon Web Services account id carries.
const ACCOUNT_ID_DIGITS: usize = 12;

/// Why the `Source` a trust-anchor write submitted describes no certificate authority this
/// service can store, or `None` when it describes one.
///
/// The WHOLE judgement, so both writes reach the same verdict on one source: the form checks
/// [`crate::state::source_rejection`] states, and then — for a source naming a private
/// certificate authority — whether the value is an ARN of such an authority at all, and
/// whether the service that owns it answers its root certificate. An ARN that could name no
/// authority is refused here rather than put to that service, which would only refuse the
/// same value back.
pub async fn source_rejection(
    ctx: &OperationContext<'_>,
    region: &str,
    source: &Source,
) -> Option<SourceRejection> {
    if let Some(rejection) = crate::state::source_rejection(source) {
        return Some(rejection);
    }
    let arn = certificate_authority_arn(source)?;
    if is_certificate_authority_arn(arn) && holds_root_certificate(ctx, region, arn).await {
        None
    } else {
        Some(SourceRejection::UnreadableCertificateAuthority)
    }
}

/// Whether `candidate` is the ARN of a private certificate authority:
/// `arn:<partition>:acm-pca:<region>:<account>:certificate-authority/<id>`, the form the
/// owning service documents for it ("This is of the form:
/// arn:aws:acm-pca:region:account:certificate-authority/12345678-1234-1234-1234-123456789012",
/// `GetCertificateAuthorityCertificate$CertificateAuthorityArn`).
///
/// The segments are checked rather than just the prefix, so a value naming another service's
/// resource, another kind of resource of this one, or no account at all is not taken for an
/// authority's ARN.
fn is_certificate_authority_arn(candidate: &str) -> bool {
    let mut segments = candidate.splitn(6, ':');
    let (Some("arn"), Some(partition), Some(service), Some(region), Some(account), Some(resource)) = (
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ) else {
        return false;
    };
    let Some(id) = resource.strip_prefix(CERTIFICATE_AUTHORITY_RESOURCE) else {
        return false;
    };
    !partition.is_empty()
        && service == ENDPOINT_PREFIX
        && !region.is_empty()
        && account.len() == ACCOUNT_ID_DIGITS
        && account.chars().all(|character| character.is_ascii_digit())
        && !id.is_empty()
}

/// The certificate authority ARN a source of the private-CA type names, or `None` for a
/// source of any other type: that type is the only one whose data names a resource another
/// service owns, so it is the only one with an authority to resolve.
fn certificate_authority_arn(source: &Source) -> Option<&str> {
    if source.source_type.as_deref() != Some(ACM_PCA_SOURCE_TYPE) {
        return None;
    }
    match source.source_data.as_ref()? {
        SourceData::acmPcaArn(arn) => Some(arn.as_str()),
        SourceData::x509CertificateData(_) => None,
    }
}

/// Whether the private CA service answers the root certificate of the authority `arn` names.
///
/// ONE read of that service, of the certificate a trust anchor of this type validates
/// against. A refusal that service answers with one of its OWN errors — it holds no authority
/// under the ARN, the value is not an ARN of its resources, or the authority holds no
/// certificate to answer — is the one answer that establishes this service cannot read the
/// authority. Every other outcome leaves it readable, the dependency not answering at all
/// included: whether an authority is there is established by the owning service's own read,
/// never inferred from a dependency that did not answer.
async fn holds_root_certificate(ctx: &OperationContext<'_>, region: &str, arn: &str) -> bool {
    let answer = client(ctx, region)
        .get_certificate_authority_certificate()
        .certificate_authority_arn(arn)
        .send()
        .await;
    let error = match answer {
        // The certificate is what the read is for, so an answer carrying none is an
        // authority whose root certificate this service still has not read.
        Ok(output) => return output.certificate().is_some(),
        Err(error) => error,
    };
    match error.as_service_error() {
        Some(refusal)
            if refusal.is_resource_not_found_exception()
                || refusal.is_invalid_arn_exception()
                || refusal.is_invalid_state_exception() =>
        {
            false
        }
        _ => {
            ::tracing::warn!(
                certificate_authority = %arn,
                error = %::aws_smithy_types::error::display::DisplayErrorContext(&error),
                "the private CA service did not answer whether it holds the certificate \
                 authority a trust anchor's source names"
            );
            true
        }
    }
}

/// The private CA client this service reads that service through: the AWS SDK driven over the
/// emulator's own in-process transport, signing in the emulator's name — IAM Roles Anywhere
/// reads the authority it was given with its own service principal.
fn client(ctx: &OperationContext<'_>, region: &str) -> ::aws_sdk_acmpca::Client {
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
