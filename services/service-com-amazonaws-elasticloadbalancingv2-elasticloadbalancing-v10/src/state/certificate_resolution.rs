// SPDX-License-Identifier: CC0-1.0

//! Whether a certificate a request NAMES is one the ACCOUNT holds — which is not this
//! service's to say.
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

use ::smithy_server_runtime::OperationContext;

use super::{names_a_certificate, request_region, IAM_SERVER_CERTIFICATE};
use crate::ops::types::Certificate;

/// The first `CertificateArn` of `certificates` the service that OWNS the certificate
/// reports holding no certificate for, if any — what an operation that resolves a
/// requested certificate refuses on, read in the order the request listed them.
///
/// Called with this service's store NOT locked: every answer comes from another service,
/// and no store of this one is held across the call.
pub async fn absent_certificate<'a>(
    ctx: &OperationContext<'_>,
    certificates: &'a [Certificate],
) -> Option<&'a str> {
    for arn in certificates
        .iter()
        .filter_map(|certificate| certificate.certificate_arn.as_deref())
        .filter(|arn| !arn.is_empty())
    {
        if certificate_is_absent(ctx, arn).await {
            return Some(arn);
        }
    }
    None
}

/// Whether the account holds NO certificate under `arn`.
///
/// True only where the owning service establishes the absence itself. A value of no
/// certificate ARN form is left to the form reader ([`names_a_certificate`]), which refuses
/// it with the same modelled error and needs no call to settle it, so it is reported as
/// present here rather than refused twice.
async fn certificate_is_absent(ctx: &OperationContext<'_>, arn: &str) -> bool {
    if !names_a_certificate(arn) {
        return false;
    }
    if IAM_SERVER_CERTIFICATE.admits(arn) {
        return iam_server_certificate_is_absent(ctx, arn).await;
    }
    acm_certificate_is_absent(ctx, arn).await
}

/// The endpoint prefix the in-process channel resolves Certificate Manager by, which is
/// also the vendor field of every Certificate Manager certificate ARN
/// (`::service_common::arn::ACM_CERTIFICATE`).
const ACM_ENDPOINT_PREFIX: &str = "acm";

/// Whether Certificate Manager reports no certificate under `arn`.
///
/// A certificate is a REGIONAL resource of that service and its ARN carries the Region it
/// belongs to, so the read is addressed to the ARN's own Region: a certificate of another
/// Region is resolved where it lives rather than looked for where the request arrived.
async fn acm_certificate_is_absent(ctx: &OperationContext<'_>, arn: &str) -> bool {
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
