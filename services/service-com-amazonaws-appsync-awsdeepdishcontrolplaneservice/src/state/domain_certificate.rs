// SPDX-License-Identifier: CC0-1.0

//! Whether the certificate a custom domain is created with can serve that domain at all:
//! WHERE that certificate came from, and whether it covers the NAME the domain is created
//! under.
//!
//! A custom domain answers HTTPS on the name the create states, and the only thing that
//! handshake can be terminated with is the certificate the same create names — so the
//! certificate is a resource the create RESOLVES rather than a string it stores, and the two
//! members are not independent. One certificate can serve one custom domain, and only one:
//!
//! * the certificate has to have been issued by an authority a client can VERIFY it against.
//!   The distribution serving a custom domain presents only such a certificate, so one the
//!   account came by any other way could never be attached to the domain — one a client
//!   imported into Certificate Manager ([`acm_store::TYPE_IMPORTED`], whoever signed it), one
//!   a PRIVATE certificate authority issued (the record carries that authority's ARN, which
//!   `CertificateDetail$CertificateAuthorityArn` reports and no public trust store carries),
//!   and one the account uploaded to Identity and Access Management — the other ARN form
//!   `CertificateArn` admits (`^arn:[a-z-]*:(acm|iam):…:(certificate|server-certificate)/…$`)
//!   — that SIGNED ITSELF, which has no authority above it at all
//!   ([`iam_store::ServerCertificateRecord::is_self_signed`]);
//! * and it has to COVER the requested name, or it would present every client a certificate
//!   for some other host — asked of the service that holds it, Certificate Manager's own
//!   [`acm_store::Certificate::covers`] or
//!   [`iam_store::ServerCertificateRecord::covers`], which apply the same TLS name match.
//!
//! `CreateDomainName` therefore refuses the PAIR, with the `BadRequestException` (HTTP 400)
//! it models and the one sentence AppSync states a certificate it will not serve with
//! ([`CERTIFICATE_INVALID_MESSAGE`]), before anything is stored — so a refused create leaves
//! no domain behind.
//!
//! WHICH certificates the account holds, where each came from and which names each covers are
//! the facts of the SERVICES that hold them: they are read in those services' own stores —
//! the one Certificate Manager's `RequestCertificate` and `ImportCertificate` write and its
//! `DescribeCertificate` reports, and the one Identity and Access Management's
//! `UploadServerCertificate` writes and its `GetServerCertificate` reports — through the SAME
//! shared engine this service's handlers reach their own store through. Whether one
//! certificate covers one host is Certificate Manager's own answer
//! ([`acm_store::Certificate::covers`]). So this service cannot decide any of it differently
//! from the service that holds the certificate, and a certificate that service no longer
//! holds stops serving new domains at once.
//!
//! Nothing is refused on evidence that is not there: an ARN NEITHER service resolves anything
//! for says nothing about the certificate, and leaves the pair to be created.
//!
//! LOCKING: this takes the other services' guards one at a time, and the handler may ask it
//! while holding this service's own. No Certificate Manager and no Identity and Access
//! Management operation reads this service's store, so the guards can never be taken in
//! opposite orders, and the work done under each is a single lookup.

use ::service_com_amazonaws_acm_certificatemanager::state as acm_store;
use ::service_com_amazonaws_iam_awsidentitymanagementv20100508::state as iam_store;

/// The sentence a `BadRequestException` states a certificate that cannot serve the
/// requested custom domain with.
///
/// Recorded behavior, and entirely FIXED text: AppSync names neither the domain nor the
/// certificate it refused, so nothing of the request is interpolated into it.
pub const CERTIFICATE_INVALID_MESSAGE: &str = "Certificate is invalid.";

/// `Some(message)` when the certificate `certificate_arn` names cannot serve `domain_name`,
/// and `None` when it can — or when neither service that holds certificates for this account
/// resolves anything for that ARN, which says nothing about the certificate.
///
/// It can serve the domain only when the certificate behind the ARN was issued by an
/// authority a client can verify it against and covers `domain_name`: one Certificate Manager
/// issued from its own public authority, or an uploaded server certificate that some
/// authority above it signed. A certificate a client imported, one a private authority issued
/// and one that signed itself cannot, because none of them could terminate the handshake the
/// custom domain answers on that name.
pub fn certificate_refusal<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    certificate_arn: &str,
) -> Option<String> {
    let arn = certificate_arn.trim();
    if let Some(refusal) = managed_certificate_refusal(engine, domain_name, arn) {
        return Some(refusal);
    }
    server_certificate_refusal(engine, domain_name, arn)
}

/// Why the certificate Certificate Manager holds at `certificate_arn` cannot serve
/// `domain_name`, or `None` for one it issued publicly that covers the name — or for an ARN
/// it resolves nothing for.
fn managed_certificate_refusal<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    certificate_arn: &str,
) -> Option<String> {
    let store = acm_store::service_state(engine)?;
    let certificates = store.lock();
    let certificate = certificates.certificates.get(certificate_arn)?;
    let publicly_issued = certificate.cert_type == acm_store::TYPE_AMAZON_ISSUED
        && certificate.certificate_authority_arn.is_none();
    if publicly_issued && certificate.covers(domain_name) {
        return None;
    }
    Some(CERTIFICATE_INVALID_MESSAGE.to_string())
}

/// The refusal of a certificate the account holds as an Identity and Access Management
/// server certificate: `None` for one an authority above it signed that COVERS
/// `domain_name`, and `None` for an ARN that service resolves nothing for.
///
/// `CertificateArn` admits a server certificate, so holding one is not itself a ground to
/// refuse. The two halves of the rule are asked of the record that service wrote: whether
/// any authority stands above the certificate at all
/// ([`iam_store::ServerCertificateRecord::is_self_signed`]) and whether it serves the name
/// the domain is created under ([`iam_store::ServerCertificateRecord::covers`], the same TLS
/// name match Certificate Manager's own `covers` applies). The record is found by the ARN
/// that service minted for it (`ServerCertificateMetadata$Arn`), which is the value a create
/// names it by.
fn server_certificate_refusal<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    certificate_arn: &str,
) -> Option<String> {
    let store = iam_store::service_state(engine)?;
    let entities = store.lock();
    let held = entities
        .server_certificates
        .values()
        .find(|certificate| certificate.metadata.arn == certificate_arn)?;
    if !held.is_self_signed() && held.covers(domain_name) {
        return None;
    }
    Some(CERTIFICATE_INVALID_MESSAGE.to_string())
}
