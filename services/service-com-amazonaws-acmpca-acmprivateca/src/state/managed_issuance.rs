// SPDX-License-Identifier: CC0-1.0

//! The issuance a CERTIFICATE MANAGER asks a private CA for.
//!
//! A certificate manager that accepts a request naming a private CA does not sign anything
//! itself: it mints a key pair, publishes a certification request for it and has THIS
//! service's CA sign a certificate for it, which is why a certificate issued that way
//! chains up through the CA's own certificate and is listed under the CA that signed it.
//! So the issuance is performed HERE, by the owner of the CA, and the asking service reads
//! back only what the CA answered ([`ManagedIssuance`]).
//!
//! Two entry points, in the order the asking service uses them:
//!
//! * [`certificate_authority_facts`] reads what a CA publishes about itself — its status,
//!   its usage mode, its kind, the algorithm it signs with and the name it signs under —
//!   so the asking service can decide, under ITS own rules, whether it will issue through
//!   this CA at all;
//! * [`issue_through_certificate_authority`] performs the issuance, under the rules
//!   `IssueCertificate` applies to any end-entity request: the CA has to be active, the
//!   per-CA ceiling still has room, the certification request has to be one this service
//!   can read, and the validity window may not outlive the CA's own certificate.
//!
//! What the HANDLER path does beside the issuance itself is not performed here: the
//! `IssueCertificate` idempotency index belongs to that operation's token, and the CRL
//! publication and the CloudWatch issuance series are reached through a request context
//! this path has none of. The certificate itself, which is what a relying party verifies,
//! is identical — it is minted by the one [`CertificateAuthorityRecord::sign_issued_certificate`]
//! both paths go through.
//!
//! LOCK ORDER: the asking service holds its OWN store and takes this one under it, the
//! order every cross-service read in this emulator uses.

use super::*;

/// What a private CA publishes about itself to a service deciding whether to issue through
/// it. Every member is read off the CA's own record.
#[derive(Debug, Clone)]
pub struct CertificateAuthorityFacts {
    /// `CertificateAuthorityStatus`: only [`STATUS_ACTIVE`] signs end-entity certificates.
    pub status: String,
    /// `CertificateAuthorityUsageMode`.
    pub usage_mode: String,
    /// `CertificateAuthorityType`: ROOT or SUBORDINATE.
    pub ca_type: String,
    /// The `SigningAlgorithm` the CA was created with, which is the algorithm it signs
    /// every certificate it issues under.
    pub signing_algorithm: String,
    /// The common name of the CA's configured subject, which is the name a certificate it
    /// signs carries as its issuer. `None` when the CA was configured without one.
    pub subject_common_name: Option<String>,
}

/// What a CA answered one managed issuance with: the certificate it signed and the facts
/// that certificate carries.
#[derive(Debug, Clone)]
pub struct ManagedIssuance {
    /// `<ca arn>/certificate/<serial>`, the ARN this service's own `GetCertificate`
    /// resolves the certificate under.
    pub certificate_arn: String,
    /// The hexadecimal serial the CA allocated for it.
    pub serial: String,
    /// The PEM-encoded certificate the CA signed.
    pub certificate_pem: String,
    /// The CA-led chain it chains up through ([`CertificateAuthorityRecord::issued_chain`]):
    /// the CA's own certificate first, then whatever chain was imported with it.
    pub certificate_chain_pem: Option<String>,
    /// The `SigningAlgorithm` the CA signed it under.
    pub signing_algorithm: String,
    /// The start of its validity window, as the CERTIFICATE carries it: a certificate's
    /// validity field is stated to the second (RFC 5280 section 4.1.2.5), so this is the
    /// instant a relying party reading the certificate sees rather than the instant the
    /// issuance was asked for.
    pub not_before: SystemTime,
    /// The end of it, read the same way.
    pub not_after: SystemTime,
}

/// Why a CA would not issue the certificate a managed request asked it for.
///
/// Each variant is one of the conditions `IssueCertificate` refuses an end-entity request
/// under, so the asking service can report the failure its own model names for that
/// condition rather than a single opaque one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ManagedIssuanceRefusal {
    /// This store holds no CA under that ARN.
    NoSuchCertificateAuthority,
    /// The CA holds no certificate to sign with, or is no longer allowed to sign: its
    /// status is not [`STATUS_ACTIVE`].
    NotActive,
    /// The CA already holds [`MAX_CERTIFICATES_PER_CA`] certificates.
    LimitExceeded,
    /// The validity window asked for would outlive the CA's own certificate, which
    /// "must not exceed the limit set on its parents in the CA hierarchy"
    /// (`IssueCertificateRequest$Validity` docs), or is not a window a certificate can
    /// carry.
    InvalidDuration,
    /// The certification request is not one this service can read, or the certificate it
    /// asks for is not one this service can encode.
    RequestFailed,
}

/// What one managed request asks a CA for: the certification request to sign and how long
/// the certificate it signs is to be valid for.
#[derive(Debug, Clone, Copy)]
pub struct ManagedIssuanceRequest<'a> {
    /// The PEM-framed PKCS#10 certification request, as `IssueCertificate` takes it.
    pub certification_request_pem: &'a str,
    /// The validity the asking service's own certificates carry, measured from the
    /// issuance.
    pub validity: Duration,
}

/// What `certificate_authority_arn` publishes about itself at `now`, or `None` when this
/// store holds no CA under that ARN.
///
/// A read, apart from the status change the clock already owed the CA, which
/// [`ComAmazonawsAcmpcaAcmprivatecaState::certificate_authority_mut`] materialises for
/// every reader alike.
pub fn certificate_authority_facts<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    certificate_authority_arn: &str,
    now: SystemTime,
) -> Option<CertificateAuthorityFacts> {
    let state = service_state(engine)?;
    let mut state = state.lock();
    let record = state.certificate_authority_mut(certificate_authority_arn, now)?;
    Some(CertificateAuthorityFacts {
        status: record.status.clone(),
        usage_mode: record.usage_mode.clone(),
        ca_type: record.ca_type.clone(),
        signing_algorithm: record.configuration.signing_algorithm.clone(),
        subject_common_name: record.configuration.subject.common_name.clone(),
    })
}

/// Have the CA `certificate_authority_arn` sign the certificate `request` asks for, and
/// record it under that CA.
///
/// The certificate is issued under the default end-entity certificate template — the
/// template `IssueCertificate` answers a request that names none under
/// ([`DEFAULT_TEMPLATE_NAME`]) — so it carries the key usage and the key purposes that
/// definition states, with the subject and the subject alternative names the certification
/// request asks for. The signature is the CA's own, made with the key pair the CA was
/// created with under the `SigningAlgorithm` it was created with, so the certificate
/// verifies against the public key the CA publishes.
///
/// The validity window runs from the issuance to `request.validity` later, backdated at
/// its start by the span this service backdates every issuance by
/// ([`ISSUANCE_BACKDATED_BY`]) so the certificate is usable at once on a relying party
/// whose clock lags.
pub fn issue_through_certificate_authority<
    Eng: ::std::marker::Send + ::std::marker::Sync + 'static,
>(
    engine: &Eng,
    certificate_authority_arn: &str,
    request: &ManagedIssuanceRequest<'_>,
    now: SystemTime,
) -> Result<ManagedIssuance, ManagedIssuanceRefusal> {
    let Some(state) = service_state(engine) else {
        return Err(ManagedIssuanceRefusal::NoSuchCertificateAuthority);
    };
    let mut state = state.lock();
    let Some(record) = state.certificate_authority_mut(certificate_authority_arn, now) else {
        return Err(ManagedIssuanceRefusal::NoSuchCertificateAuthority);
    };

    // Only an ACTIVE CA signs a certificate for a subject: the one issuance a CA outside
    // that status answers is the self-signed certificate that activates a root, which is
    // asked for with the CA's OWN certification request under a CA-certificate template
    // and is never a managed request.
    if record.status != STATUS_ACTIVE {
        return Err(ManagedIssuanceRefusal::NotActive);
    }
    if record.certificates.len() >= MAX_CERTIFICATES_PER_CA {
        return Err(ManagedIssuanceRefusal::LimitExceeded);
    }

    let Some(certification_request) =
        parse_pem_block(request.certification_request_pem, PEM_LABEL_CSR)
            .and_then(|der| parse_certification_request(&der))
    else {
        return Err(ManagedIssuanceRefusal::RequestFailed);
    };

    let not_before = now.checked_sub(ISSUANCE_BACKDATED_BY).unwrap_or(now);
    let Some(not_after) = now.checked_add(request.validity) else {
        return Err(ManagedIssuanceRefusal::InvalidDuration);
    };
    // "The end of the validity period configured on a certificate must not exceed the
    // limit set on its parents in the CA hierarchy."
    // (`IssueCertificateRequest$Validity` docs.)
    if record
        .own_certificate_not_after()
        .is_some_and(|limit| not_after > limit)
    {
        return Err(ManagedIssuanceRefusal::InvalidDuration);
    }

    let signing_algorithm = record.configuration.signing_algorithm.clone();
    let template = certificate_template(None);
    let issued = record
        .sign_issued_certificate(
            &IssuedCertificateContent {
                request: &certification_request.info,
                signing_algorithm: &signing_algorithm,
                template: &template,
                // "An APIPassthrough or APICSRPassthrough template variant must be
                // selected, or else this parameter is ignored"
                // (`IssueCertificateRequest$ApiPassthrough`): the default template is
                // neither, so no API-supplied value applies to a managed issuance.
                passthrough: None,
                not_before,
                not_after,
            },
            // The template the certificate was issued under is the default one, which a
            // request naming no template is answered under: it is named on the resource
            // exactly as `IssueCertificate` names it there.
            None,
            now,
        )
        .ok_or(ManagedIssuanceRefusal::RequestFailed)?;

    // The window the CERTIFICATE carries, read back out of the DER that was just written:
    // a certificate's validity is stated to the second, so the instants a reader of the
    // certificate sees are these and not the ones the issuance was measured with.
    let (not_before, not_after) = parse_pem_block(&issued.certificate_pem, PEM_LABEL_CERTIFICATE)
        .and_then(|der| parse_x509_certificate(&der))
        .map(|certificate| certificate_validity_window(&certificate))
        .ok_or(ManagedIssuanceRefusal::RequestFailed)?;

    Ok(ManagedIssuance {
        certificate_arn: issued.arn,
        serial: issued.serial,
        certificate_pem: issued.certificate_pem,
        certificate_chain_pem: issued.certificate_chain_pem,
        signing_algorithm: issued.signing_algorithm,
        not_before,
        not_after,
    })
}

impl CertificateAuthorityRecord {
    /// Mint the next serial, sign the certificate this CA issues for `content`, and record
    /// it under this CA.
    ///
    /// The ONE place an issued end-entity certificate comes into being: both the
    /// `IssueCertificate` operation and the managed issuance above go through it, so the
    /// serial, the ARN, the DER and the instant issuance completes are the same whichever
    /// asked for it.
    ///
    /// `None` when the certificate cannot be encoded — the certification request's subject,
    /// the allocated serial or the resolved validity window is not one a certificate can
    /// carry ([`derive_certificate_pem`]) — in which case nothing is recorded.
    pub fn sign_issued_certificate(
        &mut self,
        content: &IssuedCertificateContent<'_>,
        template_arn: Option<String>,
        now: SystemTime,
    ) -> Option<IssuedCertificate> {
        let serial = mint_serial(&format!("{}|{}", self.arn, self.next_serial));
        let arn = format!("{}/certificate/{serial}", self.arn);
        let certificate_pem = derive_certificate_pem(self, &serial, content)?;
        let certificate = IssuedCertificate {
            certificate_pem,
            certificate_chain_pem: self.issued_chain(),
            signing_algorithm: content.signing_algorithm.to_string(),
            template_arn,
            not_before: content.not_before,
            not_after: content.not_after,
            issued_at: now,
            // Issuance completes on the clock, so a caller that reads the certificate
            // straight back still sees the request in progress.
            available_at: now + SETTLE_AFTER,
            serial,
            arn: arn.clone(),
            // Nothing is revoked at issuance; `RevokeCertificate` is what records one.
            revocation: None,
        };
        self.next_serial += 1;
        self.certificates.insert(arn.clone(), certificate.clone());
        Some(certificate)
    }
}
