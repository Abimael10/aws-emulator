// SPDX-License-Identifier: CC0-1.0

//! Issuance through a PRIVATE certificate authority: what a request naming a
//! `CertificateAuthorityArn` asks of ACM Private CA, and what that CA answers it with.
//!
//! "The Amazon Resource Name (ARN) of the private certificate authority (CA) that will be
//! used to issue the certificate" (`RequestCertificateRequest$CertificateAuthorityArn`): a
//! request naming one is a request for a certificate ACM does not sign itself. ACM mints
//! the key pair (`leaf_key_material`), publishes a certification request for it, and the
//! named CA signs the certificate — so the certificate is `PRIVATE`
//! ([`TYPE_PRIVATE`]), its issuer is that CA, it chains up through that CA's own
//! certificate, and the key pair behind it is the one thing an export hands out.
//!
//! The CA is another service's resource, so every fact about it is READ from the Private CA
//! store in the shared engine and the issuance is performed BY that service
//! (`service_com_amazonaws_acmpca_acmprivateca::state`): this service copies no CA
//! configuration and signs nothing. Two of its facts decide the request:
//!
//! * a CA created with the `SHORT_LIVED_CERTIFICATE` usage mode is one ACM does not issue
//!   through at all, so the request is accepted and then settles `FAILED`
//!   ([`FAILURE_REASON_PCA_NOT_SUPPORTED_VIA_ACM`]);
//! * anything else the CA refuses the issuance for is the request's failure too, under the
//!   `FailureReason` that names the CA's refusal — the `PCA_*` members of that enum, which
//!   exist for exactly these answers.
//!
//! An ARN naming a CA no Private CA store in this engine holds is a CA this emulator cannot
//! read, and so is a `KeyAlgorithm` the in-process provider mints no key pair for: there is
//! no issuance to perform and no material to hold ([`PrivateCaOutcome::NoIssuance`]), and
//! the request settles as the private request it is with no certificate material behind it.
//!
//! LOCK ORDER: this service's store is held by the handler that calls in and the Private CA
//! store is taken under it, the same order every other cross-service read here uses
//! (`dns_validation`).

use ::service_com_amazonaws_acmpca_acmprivateca::state as private_ca_store;

use super::*;

/// The `CertificateAuthorityUsageMode` ACM cannot issue through: "short-lived certificates
/// that may optionally omit revocation because they expire quickly"
/// (`CreateCertificateAuthorityRequest$UsageMode` doc) are issued by Private CA itself and
/// not through ACM.
pub const USAGE_MODE_SHORT_LIVED_CERTIFICATE: &str = "SHORT_LIVED_CERTIFICATE";

/// The `FailureReason` of a request naming a private CA ACM does not issue through, which
/// is the value the recorded responses of such a request carry. The `FailureReason` enum
/// the model publishes does not declare it
/// (`smithy_query(query="shape", shape="com.amazonaws.acm#FailureReason")`); the recording
/// is what states it.
pub const FAILURE_REASON_PCA_NOT_SUPPORTED_VIA_ACM: &str = "PCA_NOT_SUPPORTED_VIA_ACM";

/// The `FailureReason` of a request whose `CertificateAuthorityArn` resolves to no CA.
pub const FAILURE_REASON_PCA_RESOURCE_NOT_FOUND: &str = "PCA_RESOURCE_NOT_FOUND";

/// The `FailureReason` of a request whose CA is in no state to issue a certificate.
pub const FAILURE_REASON_PCA_INVALID_STATE: &str = "PCA_INVALID_STATE";

/// The `FailureReason` of a request the CA has no room left to answer.
pub const FAILURE_REASON_PCA_LIMIT_EXCEEDED: &str = "PCA_LIMIT_EXCEEDED";

/// The `FailureReason` of a request whose certificate would outlive the CA that signs it.
pub const FAILURE_REASON_PCA_INVALID_DURATION: &str = "PCA_INVALID_DURATION";

/// The `FailureReason` of a request the CA could not answer at all.
pub const FAILURE_REASON_PCA_REQUEST_FAILED: &str = "PCA_REQUEST_FAILED";

/// What a private CA answered the issuance ACM asked it for.
#[derive(Debug)]
pub enum PrivateCaOutcome {
    /// The CA signed the certificate, and this is the material it signed.
    Issued(Box<PrivateIssuance>),
    /// The request cannot be issued through this CA, and settles `FAILED` with this
    /// `FailureReason`.
    Failed {
        /// A `FailureReason` value naming why.
        failure_reason: String,
    },
    /// Nothing was issued, because there was nothing this emulator could issue through or
    /// with: the ARN names a CA no Private CA store here holds, or a key algorithm the
    /// in-process provider mints no key pair for.
    NoIssuance,
}

/// The certificate a private CA signed for one request, and the key pair ACM minted for it.
///
/// Held on the request's pending outcome until the issuance falls due, then moved onto the
/// certificate's own record ([`Certificate::issue_privately`]) — the certificate exists
/// from the moment the CA signed it, and the ACM resource reports it from the moment ACM
/// owes the issuance.
#[derive(Clone, PartialEq, Eq)]
pub struct PrivateIssuance {
    /// The PEM-encoded certificate the CA signed.
    pub certificate_pem: String,
    /// The PEM-encoded chain it chains up through: the issuing CA's certificate first.
    pub certificate_chain_pem: Option<String>,
    /// The private half of the key pair the certificate's public key belongs to. SECRET.
    pub private_key: LeafPrivateKey,
    /// "The serial number of the certificate" (`CertificateDetail$Serial` doc), as the
    /// issuing CA allocated it.
    pub serial: String,
    /// The start of the certificate's validity window, as the certificate carries it.
    pub not_before: SystemTime,
    /// The end of it.
    pub not_after: SystemTime,
    /// "The algorithm that was used to sign the certificate"
    /// (`CertificateDetail$SignatureAlgorithm` doc): the one the issuing CA signs with.
    pub signature_algorithm: String,
    /// "The name of the certificate authority that issued and signed the certificate"
    /// (`CertificateDetail$Issuer` doc): the common name of the issuing CA's subject, when
    /// it was configured with one.
    pub issuer: Option<String>,
}

impl ::std::fmt::Debug for PrivateIssuance {
    /// Everything about the issuance except the private key, which [`LeafPrivateKey`]
    /// redacts itself: stated here as well so the omission is visible where the record is.
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("PrivateIssuance")
            .field("certificate_pem", &self.certificate_pem)
            .field("certificate_chain_pem", &self.certificate_chain_pem)
            .field("private_key", &self.private_key)
            .field("serial", &self.serial)
            .field("not_before", &self.not_before)
            .field("not_after", &self.not_after)
            .field("signature_algorithm", &self.signature_algorithm)
            .field("issuer", &self.issuer)
            .finish()
    }
}

/// Ask the private CA `certificate_authority_arn` for the certificate a request covering
/// `domain_names` asks for, with the material `minted` holds.
///
/// The private-CA side of one `RequestCertificate`: read what the CA publishes about
/// itself, decide whether ACM issues through it at all, and take the certificate the CA
/// signs for the certification request. Every answer is one of [`PrivateCaOutcome`];
/// nothing here reports an issuance the CA did not perform.
///
/// `minted` is the key pair and the certification request the HANDLER minted before it
/// locked any store ([`mint_leaf_material`]), because minting is the one piece of this
/// whose cost the caller chooses; `None` is a key algorithm the in-process provider mints
/// no pair for, which leaves nothing to be signed.
pub fn issue_through_private_ca<Eng: ::std::marker::Send + ::std::marker::Sync + 'static>(
    engine: &Eng,
    certificate_authority_arn: &str,
    minted: Option<MintedLeafMaterial>,
    now: SystemTime,
) -> PrivateCaOutcome {
    let Some(facts) =
        private_ca_store::certificate_authority_facts(engine, certificate_authority_arn, now)
    else {
        return PrivateCaOutcome::NoIssuance;
    };

    // ACM's own rule, not the CA's: Private CA issues short-lived certificates itself, and a
    // CA created in that usage mode is not one ACM will issue a certificate through, so the
    // request it was named in fails.
    if facts.usage_mode == USAGE_MODE_SHORT_LIVED_CERTIFICATE {
        return PrivateCaOutcome::Failed {
            failure_reason: FAILURE_REASON_PCA_NOT_SUPPORTED_VIA_ACM.to_string(),
        };
    }

    let Some(minted) = minted else {
        return PrivateCaOutcome::NoIssuance;
    };

    let issuance = private_ca_store::issue_through_certificate_authority(
        engine,
        certificate_authority_arn,
        &private_ca_store::ManagedIssuanceRequest {
            certification_request_pem: &minted.certification_request_pem,
            // The window ACM's own certificates carry ([`CERTIFICATE_VALIDITY`]), which is
            // what ACM asks the CA to sign for.
            validity: CERTIFICATE_VALIDITY,
        },
        now,
    );
    match issuance {
        Ok(issued) => PrivateCaOutcome::Issued(Box::new(PrivateIssuance {
            certificate_pem: issued.certificate_pem,
            certificate_chain_pem: issued.certificate_chain_pem,
            private_key: minted.key_pair.into_private_key(),
            serial: issued.serial,
            not_before: issued.not_before,
            not_after: issued.not_after,
            signature_algorithm: issued.signing_algorithm,
            issuer: facts.subject_common_name,
        })),
        Err(refusal) => PrivateCaOutcome::Failed {
            failure_reason: failure_reason_for(&refusal).to_string(),
        },
    }
}

/// The `FailureReason` naming one refusal of the issuing CA.
///
/// Each `PCA_*` member of that enum is ACM's report of one answer Private CA gives a
/// request ACM made on the caller's behalf, so the mapping is one refusal to the member
/// that names it.
fn failure_reason_for(refusal: &private_ca_store::ManagedIssuanceRefusal) -> &'static str {
    match refusal {
        private_ca_store::ManagedIssuanceRefusal::NoSuchCertificateAuthority => {
            FAILURE_REASON_PCA_RESOURCE_NOT_FOUND
        }
        private_ca_store::ManagedIssuanceRefusal::NotActive => FAILURE_REASON_PCA_INVALID_STATE,
        private_ca_store::ManagedIssuanceRefusal::LimitExceeded => {
            FAILURE_REASON_PCA_LIMIT_EXCEEDED
        }
        private_ca_store::ManagedIssuanceRefusal::InvalidDuration => {
            FAILURE_REASON_PCA_INVALID_DURATION
        }
        private_ca_store::ManagedIssuanceRefusal::RequestFailed => {
            FAILURE_REASON_PCA_REQUEST_FAILED
        }
    }
}
