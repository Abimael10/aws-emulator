// SPDX-License-Identifier: CC0-1.0

//! The extensions a CA's published certification request ASKS FOR.
//!
//! `CertificateAuthorityConfiguration$CsrExtensions` "Describes the certificate extensions
//! to be added to the certificate signing request (CSR)", and a PKCS#10 request states the
//! extensions it asks for in its `extensionRequest` attribute (RFC 2985 section 5.4.2). So
//! the configuration a CA was created with is what fills that attribute of the request the
//! CA publishes ([`super::derive_csr_pem`]): the signing authority that answers the request
//! reads the key purposes out of it, and a configuration that never reached the request
//! would be accepted and silently dropped.
//!
//! `KeyUsage` is "one or more purposes for which the key contained in the certificate can be
//! used. Default value for each option is false" — nine booleans, which are exactly the
//! named bits of the `keyUsage` extension (RFC 5280 section 4.2.1.3), in the order that
//! section lists them. The request therefore asserts the bits set true in the configuration
//! and no others, and a configuration that asks for no key usage at all leaves the extension
//! out rather than asking for an empty one.

use super::*;

use ::x509_cert::attr::Attribute;
use ::x509_cert::der::oid::AssociatedOid;
use ::x509_cert::ext::pkix::{KeyUsage as KeyUsageExtension, KeyUsages};

/// The attributes the certification request of a CA configured with `csr_extensions`
/// carries: the `extensionRequest` attribute holding the extensions it asks for, or none
/// when the configuration asks for no extension this service encodes.
pub fn requested_extension_attributes(
    csr_extensions: Option<&CsrExtensions>,
) -> Option<SetOfVec<Attribute>> {
    let extensions = requested_extensions(csr_extensions)?;
    let mut values = SetOfVec::new();
    values.insert(Any::from_der(&extensions.to_der().ok()?).ok()?).ok()?;
    let mut attributes = SetOfVec::new();
    attributes
        .insert(Attribute {
            oid: super::oid::EXTENSION_REQUEST,
            values,
        })
        .ok()?;
    Some(attributes)
}

/// The extensions `csr_extensions` asks for, in the order the shape states its members, or
/// `None` when it asks for none.
fn requested_extensions(csr_extensions: Option<&CsrExtensions>) -> Option<Extensions> {
    let configured = csr_extensions?;
    let extensions: Extensions = configured
        .key_usage
        .as_ref()
        .and_then(key_usage_extension)
        .into_iter()
        .collect();
    (!extensions.is_empty()).then_some(extensions)
}

/// The `keyUsage` extension (RFC 5280 section 4.2.1.3) that asserts the named bits
/// `configured` sets, marked critical as that section has a conforming authority mark this
/// extension ("When present, conforming CAs SHOULD mark this extension as critical").
///
/// The ONE encoder of those bits: the extensions a CA's certification request ASKS FOR are
/// built from it, and so are the ones a certificate template DEFINES
/// ([`super::KeyUsageDefinition`]).
///
/// `None` when no member is set: a key usage that permits nothing is not a purpose to ask
/// for, and RFC 5280 forbids an extension whose bit string asserts no bit.
pub(crate) fn key_usage_extension(configured: &KeyUsage) -> Option<Extension> {
    let mut usage = KeyUsageExtension(Default::default());
    for (asked_for, bit) in [
        (configured.digital_signature, KeyUsages::DigitalSignature),
        (configured.non_repudiation, KeyUsages::NonRepudiation),
        (configured.key_encipherment, KeyUsages::KeyEncipherment),
        (configured.data_encipherment, KeyUsages::DataEncipherment),
        (configured.key_agreement, KeyUsages::KeyAgreement),
        (configured.key_cert_sign, KeyUsages::KeyCertSign),
        (configured.crl_sign, KeyUsages::CRLSign),
        (configured.encipher_only, KeyUsages::EncipherOnly),
        (configured.decipher_only, KeyUsages::DecipherOnly),
    ] {
        if asked_for {
            usage.0 |= bit;
        }
    }
    if usage.0.is_empty() {
        return None;
    }
    Some(Extension {
        extn_id: KeyUsageExtension::OID,
        critical: true,
        extn_value: OctetString::new(usage.to_der().ok()?).ok()?,
    })
}
