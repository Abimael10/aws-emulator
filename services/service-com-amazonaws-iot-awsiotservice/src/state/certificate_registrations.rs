// SPDX-License-Identifier: CC0-1.0

//! The REGISTRATION of a device certificate the caller owns: what `RegisterCertificate`
//! reads out of the material it is handed, which registered CA it resolves as the signing
//! one, and what the request or the account's state refuses.
//!
//! A registered device certificate is the other half of the device-certificate store
//! ([`super::certificates`]): one record this service ISSUED and one record a caller
//! REGISTERED live side by side, described, listed, updated and deleted by the same
//! readers and writers, because both are the same resource. What differs is where the
//! record's members come from — a registration reads the certificate material, its
//! validity period and its ISSUER out of the caller's own certificate, so nothing about
//! the certificate is invented here, exactly as a CA registration reads its record out of
//! the CA material ([`super::ca_certificates`]).
//!
//! The certificate is registered under the CA that signed it: "Registers a device
//! certificate with IoT in the same certificate mode as the signing CA. If you have more
//! than one CA certificate that has the same subject field, you must specify the CA
//! certificate that was used to sign the device certificate being registered"
//! (`RegisterCertificate`). So the signing CA is resolved from the account's CA registry —
//! by the material the request names when it names one, and otherwise by the certificate's
//! own issuer — and the record carries that CA's id and its certificate mode. A CA this
//! account has not registered, and an issuer more than one registered CA shares, are the
//! two ways that resolution has no single answer, which is the `CertificateConflictException`
//! the model declares: "Unable to verify the CA certificate used to sign the device
//! certificate you are attempting to register. This is happens when you have registered
//! more than one CA certificate that has the same subject field and public key".

use std::time::{SystemTime, UNIX_EPOCH};

use x509_cert::der::Encode;

use super::{
    certificate_arn, certificate_id, existing_certificate_message, fresh_uuid,
    read_certificate_material, unreadable_certificate_material_message, RegisteredCertificate,
    CERTIFICATE_STATUS_ACTIVE, CERTIFICATE_STATUS_INACTIVE, FIRST_CERTIFICATE_CUSTOMER_VERSION,
    INTERNAL_CERTIFICATE_STATUSES, MAX_CERTIFICATES,
};

/// The member a registration carries the DEVICE certificate at, which is what a refusal of
/// unreadable material names.
pub const CERTIFICATE_PEM_MEMBER: &str = "certificatePem";

/// The member a registration names the SIGNING CA certificate at.
pub const CA_CERTIFICATE_PEM_MEMBER: &str = "caCertificatePem";

/// What one `RegisterCertificate` asks for, as the registration reads it.
///
/// A struct rather than a parameter list because the request is what the one write takes:
/// the certificate material, the CA material that disambiguates its signer, and the two
/// members a caller states the status it wants the record in with.
#[derive(Debug, Clone, Default)]
pub struct CertificateRegistration {
    /// `certificatePem` — "The certificate data, in PEM format".
    pub certificate_pem: String,
    /// `caCertificatePem` — "The CA certificate used to sign the device certificate being
    /// registered", absent when the request named none.
    pub ca_certificate_pem: Option<String>,
    /// `status` — "The status of the register certificate request. Valid values that you
    /// can use include ACTIVE, INACTIVE, and REVOKED".
    pub status: Option<String>,
    /// `setAsActive` — "A boolean value that specifies if the certificate is set to
    /// active", the DEPRECATED member `status` replaced, so it decides the status only
    /// when the request names no `status` at all.
    pub set_as_active: Option<bool>,
}

/// Why a device certificate was NOT registered.
///
/// Each variant names the modelled error class the refusal answers with, so the one rule
/// `RegisterCertificate` follows is stated once, beside the store it writes, rather than
/// in the handler.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CertificateRegistrationRefusal {
    /// Material this service cannot read as an X.509 certificate:
    /// `CertificateValidationException` (HTTP 400).
    InvalidCertificate(String),
    /// A signing CA that cannot be resolved to exactly one registered CA certificate:
    /// `CertificateConflictException` (HTTP 409).
    UnverifiableSigningCa(String),
    /// A `status` no caller may ask for — one of the statuses only AWS IoT's own machinery
    /// sets ([`INTERNAL_CERTIFICATE_STATUSES`]): `CertificateStateException` (HTTP 406),
    /// the class this operation models for a status it will not record.
    UnsettableStatus(String),
    /// Material the account has already registered: `ResourceAlreadyExistsException`
    /// (HTTP 409), which also reports the record that already exists.
    AlreadyRegistered {
        /// The sentence the refusal states.
        message: String,
        /// `resourceId` — the id the existing record is held under.
        certificate_id: String,
        /// `resourceArn` — the ARN that record carries.
        certificate_arn: String,
    },
    /// An account already holding [`MAX_CERTIFICATES`] certificates: the emulator's own
    /// capacity, reported with `ServiceUnavailableException` (HTTP 503), the error the
    /// model has for a request this service cannot complete.
    RegistryFull(String),
}

impl CertificateRegistrationRefusal {
    /// The message the refusal states, whichever class it names.
    pub fn message(&self) -> &str {
        match self {
            Self::InvalidCertificate(message)
            | Self::UnverifiableSigningCa(message)
            | Self::UnsettableStatus(message)
            | Self::AlreadyRegistered { message, .. }
            | Self::RegistryFull(message) => message,
        }
    }
}

/// The fixed clause every refusal of an unresolvable signing CA states, with the reason
/// the resolution had no single answer appended.
///
/// AWS publishes the CLASS for this and the sentence its documentation states it in —
/// "Unable to verify the CA certificate used to sign the device certificate you are
/// attempting to register" — and not the message for each way the verification fails, so
/// that published clause is the fixed text and the reason is this emulator's rendering,
/// naming which part of the request or of the account's registry left the signer
/// unresolved.
fn unverifiable_signing_ca_message(reason: &str) -> String {
    format!(
        "Unable to verify the CA certificate used to sign the device certificate you are \
         attempting to register: {reason}"
    )
}

/// The message a registration naming CA material the account holds no CA certificate for
/// is refused with.
pub fn unregistered_signing_ca_message() -> String {
    unverifiable_signing_ca_message(&format!(
        "the {CA_CERTIFICATE_PEM_MEMBER} field names a CA certificate this account has not \
         registered"
    ))
}

/// The message a registration whose named CA did not issue the certificate is refused
/// with: the certificate's own issuer is the variable part, because that is what a
/// registered CA's subject field has to match for the CA to be the signer.
pub fn unmatched_signing_ca_message(issuer: &str) -> String {
    unverifiable_signing_ca_message(&format!(
        "the certificate was issued by {issuer}, which is not the subject field of the CA \
         certificate the {CA_CERTIFICATE_PEM_MEMBER} field names"
    ))
}

/// The message a registration that names no CA material is refused with when the
/// account's registry holds no CA certificate under the certificate's issuer.
pub fn unknown_signing_ca_message(issuer: &str) -> String {
    unverifiable_signing_ca_message(&format!(
        "this account has registered no CA certificate whose subject field is {issuer}"
    ))
}

/// The message a registration that names no CA material is refused with when MORE than one
/// registered CA certificate carries the certificate's issuer as its subject field: the
/// case `RegisterCertificate` documents as the one where "you must specify the CA
/// certificate that was used to sign the device certificate being registered".
pub fn ambiguous_signing_ca_message(issuer: &str) -> String {
    unverifiable_signing_ca_message(&format!(
        "this account has registered more than one CA certificate whose subject field is \
         {issuer}, so the {CA_CERTIFICATE_PEM_MEMBER} field has to name the one that signed the \
         certificate"
    ))
}

/// The message a registration refuses a `status` no caller may ask for with: the one
/// sentence every certificate write states an unsettable status in
/// ([`super::certificates::unsettable_status_message`]), named after the operation the
/// caller asked for it with.
pub fn unsettable_registration_status_message(status: &str) -> String {
    super::certificates::unsettable_status_message("to", status, "RegisterCertificate")
}

/// The message a registration past the emulator's own capacity is refused with, naming the
/// ceiling that refused it ([`MAX_CERTIFICATES`]).
pub fn certificate_store_full_message() -> String {
    format!("this emulator stores at most {MAX_CERTIFICATES} certificates")
}

impl super::ComAmazonawsIotAwsiotserviceState {
    /// Register the device certificate `request` carries, answering the record that was
    /// written, or the reason the request or the account's state refused it.
    ///
    /// The one write every registration of a device certificate makes, so the rules are
    /// stated once rather than per handler:
    ///
    /// * the status the record is written in is resolved from the request alone — from
    ///   `status` where the request names one, and otherwise from the deprecated
    ///   `setAsActive` — and one of the statuses only IoT's own machinery sets
    ///   ([`INTERNAL_CERTIFICATE_STATUSES`]) is refused before anything else is read,
    ///   because it is refused whatever the material is and whatever the account holds;
    /// * the material has to be a readable X.509 certificate, since every member the
    ///   record reports about the certificate — its PEM, its validity period, its issuer —
    ///   is read out of it;
    /// * the signing CA is exactly one CA certificate of the account's registry: the one
    ///   the request's own CA material names, and otherwise the one whose subject field is
    ///   the certificate's issuer. The record carries that CA's id and is registered "in
    ///   the same certificate mode as the signing CA";
    /// * the material's own fingerprint is the record's id, so material already registered
    ///   is a collision rather than a second record of one certificate;
    /// * the account holds at most [`MAX_CERTIFICATES`] certificates altogether, this
    ///   emulator's own ceiling on a store a caller grows.
    pub fn register_certificate(
        &mut self,
        region: &str,
        owned_by: &str,
        request: CertificateRegistration,
        now: SystemTime,
    ) -> Result<RegisteredCertificate, CertificateRegistrationRefusal> {
        let status = registered_status(&request)?;

        let Some(certificate) = read_certificate_material(&request.certificate_pem) else {
            return Err(CertificateRegistrationRefusal::InvalidCertificate(
                unreadable_certificate_material_message(CERTIFICATE_PEM_MEMBER),
            ));
        };
        // The ISSUER of the certificate is what names its signer: a certificate authority
        // signs under its own subject field (RFC 5280 section 4.1.2.4), so the issuer and
        // the registered CA's subject are the two ends of the same name.
        let issuer = certificate.tbs_certificate.issuer.to_string();

        // The CA material the request names, as the id the account's CA registry holds a
        // CA certificate under: a CA certificate's identity is its own fingerprint
        // ([`super::ca_certificates`]), so the material the request carries names the
        // record without the request having to carry an id at all.
        let named_ca = match request
            .ca_certificate_pem
            .as_deref()
            .map(str::trim)
            .filter(|material| !material.is_empty())
        {
            Some(material) => {
                let Some(ca_certificate) = read_certificate_material(material) else {
                    return Err(CertificateRegistrationRefusal::InvalidCertificate(
                        unreadable_certificate_material_message(CA_CERTIFICATE_PEM_MEMBER),
                    ));
                };
                let Ok(ca_certificate_der) = ca_certificate.to_der() else {
                    return Err(CertificateRegistrationRefusal::InvalidCertificate(
                        unreadable_certificate_material_message(CA_CERTIFICATE_PEM_MEMBER),
                    ));
                };
                Some(certificate_id(&ca_certificate_der))
            }
            None => None,
        };

        let (ca_certificate_id, certificate_mode) = match named_ca {
            Some(ca_certificate_id) => {
                let Some(registered_ca) = self.ca_certificates.get(&ca_certificate_id) else {
                    return Err(CertificateRegistrationRefusal::UnverifiableSigningCa(
                        unregistered_signing_ca_message(),
                    ));
                };
                if registered_ca.subject != issuer {
                    return Err(CertificateRegistrationRefusal::UnverifiableSigningCa(
                        unmatched_signing_ca_message(&issuer),
                    ));
                }
                (
                    registered_ca.certificate_id.clone(),
                    registered_ca.certificate_mode.clone(),
                )
            }
            // A request that names no CA material leaves the signer to be resolved by the
            // certificate's issuer, which is what "If you have more than one CA
            // certificate that has the same subject field, you must specify the CA
            // certificate that was used to sign the device certificate being registered"
            // makes optional: one match is the signer, and both no match and more than one
            // leave the signer unresolved.
            None => {
                let mut signers = self
                    .ca_certificates
                    .values()
                    .filter(|registered_ca| registered_ca.subject == issuer);
                match (signers.next(), signers.next()) {
                    (Some(signer), None) => (
                        signer.certificate_id.clone(),
                        signer.certificate_mode.clone(),
                    ),
                    (Some(_), Some(_)) => {
                        return Err(CertificateRegistrationRefusal::UnverifiableSigningCa(
                            ambiguous_signing_ca_message(&issuer),
                        ))
                    }
                    _ => {
                        return Err(CertificateRegistrationRefusal::UnverifiableSigningCa(
                            unknown_signing_ca_message(&issuer),
                        ))
                    }
                }
            }
        };

        let Ok(certificate_der) = certificate.to_der() else {
            return Err(CertificateRegistrationRefusal::InvalidCertificate(
                unreadable_certificate_material_message(CERTIFICATE_PEM_MEMBER),
            ));
        };
        let registered_id = certificate_id(&certificate_der);

        if let Some(existing) = self.certificates.get(&registered_id) {
            return Err(CertificateRegistrationRefusal::AlreadyRegistered {
                message: existing_certificate_message(&existing.certificate_id),
                certificate_id: existing.certificate_id.clone(),
                certificate_arn: existing.certificate_arn.clone(),
            });
        }

        // The store is one a caller grows, so the account's whole registry is bounded. The
        // ceiling is read AFTER the collision check, because material already registered
        // adds no record.
        if self.certificates.len() >= MAX_CERTIFICATES {
            return Err(CertificateRegistrationRefusal::RegistryFull(
                certificate_store_full_message(),
            ));
        }

        let validity = &certificate.tbs_certificate.validity;
        let registered = RegisteredCertificate {
            certificate_arn: certificate_arn(region, &registered_id),
            certificate_id: registered_id.clone(),
            certificate_pem: request.certificate_pem,
            status,
            owned_by: owned_by.to_string(),
            creation_date: now,
            last_modified_date: now,
            customer_version: FIRST_CERTIFICATE_CUSTOMER_VERSION,
            generation_id: fresh_uuid(),
            not_before: UNIX_EPOCH + validity.not_before.to_unix_duration(),
            not_after: UNIX_EPOCH + validity.not_after.to_unix_duration(),
            certificate_mode,
            ca_certificate_id: Some(ca_certificate_id),
            // A freshly registered certificate is in no transfer: the only write that puts
            // one in a transfer is `TransferCertificate`.
            transfer: None,
        };
        self.certificates
            .insert(registered_id, registered.clone());
        Ok(registered)
    }
}

/// The status the record is written in, or the refusal of a status no caller may ask for.
///
/// `status` is "The status of the register certificate request" and `setAsActive` is the
/// DEPRECATED member it replaced — "A boolean value that specifies if the certificate is
/// set to active" — so a request naming a status is recorded in it, a request naming only
/// the flag is recorded [`CERTIFICATE_STATUS_ACTIVE`] when it is set, and a request naming
/// neither is recorded [`CERTIFICATE_STATUS_INACTIVE`]: the certificate exists but nothing
/// may connect with it yet.
fn registered_status(
    request: &CertificateRegistration,
) -> Result<String, CertificateRegistrationRefusal> {
    if let Some(status) = request.status.as_deref() {
        if INTERNAL_CERTIFICATE_STATUSES.contains(&status) {
            return Err(CertificateRegistrationRefusal::UnsettableStatus(
                unsettable_registration_status_message(status),
            ));
        }
        return Ok(status.to_string());
    }
    Ok(if request.set_as_active.unwrap_or(false) {
        CERTIFICATE_STATUS_ACTIVE
    } else {
        CERTIFICATE_STATUS_INACTIVE
    }
    .to_string())
}
