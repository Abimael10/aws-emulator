// SPDX-License-Identifier: CC0-1.0

//! Whether the certificate a custom domain is created with can serve that domain at all:
//! whether a client could VERIFY that certificate, and whether it covers the NAME the domain
//! is created under.
//!
//! A custom domain answers HTTPS on the name the create states, and the only thing that
//! handshake can be terminated with is the certificate the same create names — so the
//! certificate is a resource the create RESOLVES rather than a string it stores, and the two
//! members are not independent:
//!
//! * the certificate has to be anchored in an authority a client's own trust store carries.
//!   The distribution serving a custom domain presents only such a certificate, so one whose
//!   trust chain ends anywhere else could never terminate that handshake — a certificate that
//!   signed itself, one a certificate authority of the caller's own making signed, and one a
//!   PRIVATE certificate authority issued alike, however many authorities stand between it and
//!   that root;
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
//! WHICH certificates the account holds and which names each covers are the facts of the
//! SERVICES that hold them: they are read in those services' own stores — the one Certificate
//! Manager's `RequestCertificate` and `ImportCertificate` write and its `DescribeCertificate`
//! reports, and the one Identity and Access Management's `UploadServerCertificate` writes and
//! its `GetServerCertificate` reports — through the SAME shared engine this service's handlers
//! reach their own store through. So this service cannot decide any of it differently from the
//! service that holds the certificate, and a certificate that service no longer holds stops
//! serving new domains at once.
//!
//! WHERE a certificate is anchored is decided from the certificate MATERIAL whenever the
//! account brought material in itself — a certificate imported into Certificate Manager and a
//! server certificate uploaded to Identity and Access Management both carry the body and the
//! chain the caller supplied — by building the X.509 path that material describes up to the
//! public trust anchors ([`publicly_anchored`]), which is the same work a client does with the
//! certificate a handshake presents it. The one certificate no material exists for is one
//! Certificate Manager ISSUED, because this emulator operates no certificate authority and
//! signs nothing; such a certificate is anchored exactly when that service issued it from its
//! own PUBLIC authority rather than from a private one, which is what its record reports.
//!
//! Nothing is refused on evidence that is not there: an ARN NEITHER service resolves anything
//! for says nothing about the certificate, and leaves the pair to be created.
//!
//! LOCKING: this takes the other services' guards one at a time, and the handler may ask it
//! while holding this service's own. No Certificate Manager and no Identity and Access
//! Management operation reads this service's store, so the guards can never be taken in
//! opposite orders, and what is done under each is a single lookup: the path is built from the
//! material AFTER that service's guard is released, so one certificate's chain cannot hold up
//! another caller of the service that holds it.

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
/// It can serve the domain only when the certificate behind the ARN is anchored in an
/// authority a client's trust store carries and covers `domain_name`. A certificate a private
/// authority issued, one an authority of the caller's own making signed and one that signed
/// itself cannot, because none of them could terminate the handshake the custom domain answers
/// on that name.
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
/// `domain_name`, or `None` for one anchored in a public authority that covers the name — or
/// for an ARN it resolves nothing for.
///
/// A certificate the account IMPORTED is judged by the material the import carried, which is
/// what a client would be presented; one that service ISSUED exists as no material here, and
/// is anchored exactly when it came from that service's own public authority rather than from
/// the private one its record names.
fn managed_certificate_refusal<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    certificate_arn: &str,
) -> Option<String> {
    let store = acm_store::service_state(engine)?;
    // What that service holds is read under its guard and the guard is then RELEASED: the path
    // is built from the copied material, so no certificate's chain decides how long another
    // caller of Certificate Manager waits.
    let held = {
        let certificates = store.lock();
        let certificate = certificates.certificates.get(certificate_arn)?;
        HeldCertificate {
            material: certificate.certificate_body.clone(),
            chain: certificate.certificate_chain.clone(),
            publicly_issued: certificate.cert_type == acm_store::TYPE_AMAZON_ISSUED
                && certificate.certificate_authority_arn.is_none(),
            covers: certificate.covers(domain_name),
        }
    };
    let anchored = match held.material.as_deref() {
        Some(body) => publicly_anchored(body, held.chain.as_deref()),
        None => held.publicly_issued,
    };
    if anchored && held.covers {
        return None;
    }
    Some(CERTIFICATE_INVALID_MESSAGE.to_string())
}

/// What the service holding a certificate answers about it: the material it holds for the
/// certificate (`None` for one it holds none of), the authorities offered with that material,
/// whether that service ISSUED it publicly, and whether it covers the requested name.
struct HeldCertificate {
    material: Option<String>,
    chain: Option<String>,
    publicly_issued: bool,
    covers: bool,
}

/// The refusal of a certificate the account holds as an Identity and Access Management
/// server certificate: `None` for one anchored in a public authority that COVERS
/// `domain_name`, and `None` for an ARN that service resolves nothing for.
///
/// `CertificateArn` admits a server certificate, so holding one is not itself a ground to
/// refuse. The uploaded material is all there is to judge such a certificate by — that service
/// issues nothing — so the path is built from the body and the chain the upload carried, and
/// whether it serves the name the domain is created under is that service's own answer
/// ([`iam_store::ServerCertificateRecord::covers`], the same TLS name match Certificate
/// Manager's own `covers` applies). The record is found by the ARN that service minted for it
/// (`ServerCertificateMetadata$Arn`), which is the value a create names it by.
fn server_certificate_refusal<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    domain_name: &str,
    certificate_arn: &str,
) -> Option<String> {
    let store = iam_store::service_state(engine)?;
    // Read under that service's guard, judged after it is released, exactly as above.
    let held = {
        let entities = store.lock();
        let held = entities
            .server_certificates
            .values()
            .find(|certificate| certificate.metadata.arn == certificate_arn)?;
        HeldCertificate {
            material: Some(held.certificate_body.clone()),
            chain: held.certificate_chain.clone(),
            // That service issues nothing, so there is no public issuance to report.
            publicly_issued: false,
            covers: held.covers(domain_name),
        }
    };
    let anchored = held
        .material
        .as_deref()
        .is_some_and(|body| publicly_anchored(body, held.chain.as_deref()));
    if anchored && held.covers {
        return None;
    }
    Some(CERTIFICATE_INVALID_MESSAGE.to_string())
}

/// Whether the PEM certificate `certificate_pem`, with `chain_pem` as the authorities offered
/// with it, builds a server-authentication path that ends in a PUBLIC trust anchor.
///
/// This is the client's side of the handshake the custom domain would answer, done with the
/// path builder a client uses and against the trust anchors a client carries, so the answer is
/// the one a browser reaching the domain would reach: every authority between the certificate
/// and the anchor has to be offered and has to have signed the certificate below it. Material
/// that is not a certificate at all, a path that stops short of an anchor, and a signature that
/// does not verify are each simply not anchored.
///
/// How much work one answer costs is the path builder's own affair: it spends a fixed budget of
/// path steps and signature checks whatever is offered with the certificate, so a long chain
/// cannot make this cost more than a short one.
fn publicly_anchored(certificate_pem: &str, chain_pem: Option<&str>) -> bool {
    use ::rustls_pki_types::pem::PemObject;

    let Ok(leaf) = ::rustls_pki_types::CertificateDer::from_pem_slice(certificate_pem.as_bytes())
    else {
        return false;
    };
    let Ok(end_entity) = ::webpki::EndEntityCert::try_from(&leaf) else {
        return false;
    };
    // Every certificate the chain carries is offered as an intermediate, in the order it was
    // supplied; one the path does not need is simply not used.
    let offered: Vec<::rustls_pki_types::CertificateDer<'static>> = chain_pem
        .map(|pem| {
            ::rustls_pki_types::CertificateDer::pem_slice_iter(pem.as_bytes())
                .filter_map(Result::ok)
                .collect()
        })
        .unwrap_or_default();
    end_entity
        .verify_for_usage(
            ::webpki::ALL_VERIFICATION_ALGS,
            ::webpki_roots::TLS_SERVER_ROOTS,
            &offered,
            now(),
            ::webpki::KeyUsage::server_auth(),
            None,
            None,
        )
        .is_ok()
}

/// The instant the path is built at: a certificate serves a handshake only inside its own
/// validity window, which is the window a client would check it against.
fn now() -> ::rustls_pki_types::UnixTime {
    let since_epoch = ::std::time::SystemTime::now()
        .duration_since(::std::time::UNIX_EPOCH)
        .unwrap_or_default();
    ::rustls_pki_types::UnixTime::since_unix_epoch(since_epoch)
}
