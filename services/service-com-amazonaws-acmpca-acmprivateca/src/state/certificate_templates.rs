// SPDX-License-Identifier: CC0-1.0

//! What a certificate template says the certificate issued under it carries.
//!
//! `IssueCertificate`'s `TemplateArn` "Specifies a custom configuration template to use
//! when issuing a certificate. If this parameter is not provided, Amazon Web Services
//! Private CA defaults to the `EndEntityCertificate/V1` template. For CA certificates, you
//! should choose the shortest path length that meets your needs. The path length is
//! indicated by the PathLenN portion of the ARN, where N is the CA depth."
//! (`IssueCertificateRequest$TemplateArn` docs.)
//!
//! A template is a named set of X.509 extensions, and the template's NAME says which
//! certificate it defines, so the definition is read off that name. Every published
//! definition turns on `basicConstraints` (RFC 5280 section 4.2.1.9) — what makes a CA
//! certificate usable as an issuer, what bounds the hierarchy below it, and what tells a CA
//! certificate apart from an end-entity one — and `keyUsage` (section 4.2.1.3), the purposes
//! the certified key may be used for; the kinds that certify a key for a NAMED purpose add
//! `extendedKeyUsage` (section 4.2.1.12):
//!
//! * `RootCACertificate` — a CA certificate: `cA` TRUE with no `pathLenConstraint`, so the
//!   depth under a root is bounded by its subordinates' own constraints, and a key usage of
//!   certificate signing, CRL signing and digital signature — what an authority does with
//!   its key;
//! * `SubordinateCACertificate_PathLenN` — the same CA certificate, with the
//!   `pathLenConstraint` the name's depth N publishes;
//! * the end-entity kinds — `cA` FALSE, which is what forbids issuing under them, with the
//!   key usage and the key purposes each kind is for:
//!   * `EndEntityCertificate` — digital signature and key encipherment, for TLS server and
//!     TLS client authentication;
//!   * `EndEntityServerAuthCertificate` / `EndEntityClientAuthCertificate` — the same key
//!     usage for one side of that handshake alone;
//!   * `CodeSigningCertificate` — digital signature, for code signing;
//!   * `OCSPSigningCertificate` — digital signature, for OCSP signing;
//! * the `Blank…` kinds define no extensions of their own, which is what makes them blank:
//!   an `ApiPassthrough` is what fills such a certificate in. The one exception is the
//!   `…CriticalBasicConstraints…` variants, whose name says the single extension they do
//!   define — which is why the plain blank kinds cannot be defining it too.
//!
//! `basicConstraints` and `keyUsage` are critical, as the published definitions state and as
//! RFC 5280 has a conforming authority mark them; `extendedKeyUsage` is not, so a relying
//! party that does not know a purpose may still accept the certificate. A name's
//! `_APIPassthrough` / `_APICSRPassthrough` suffix selects where the REST of the
//! certificate's content is taken from rather than what the template defines, so it changes
//! none of the above; a template name outside these kinds defines nothing this service
//! knows, and the certificate issued under it carries no extension of the template's.
//!
//! Beside the extensions a template DEFINES sits the one it passes through: the subject
//! alternative name the certification request asks for ([`requested_subject_alternative_name`]).

use super::*;

use ::x509_cert::der::oid::AssociatedOid;
use ::x509_cert::ext::pkix::{BasicConstraints, ExtendedKeyUsage};

/// The extensions a certificate issued under one template carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CertificateTemplate {
    /// The basic constraints the template defines, or `None` for a template that defines
    /// none.
    pub basic_constraints: Option<BasicConstraintsDefinition>,
    /// The key usage the template defines, or `None` for a template that defines none.
    pub key_usage: Option<KeyUsageDefinition>,
    /// The key purposes the template's `extendedKeyUsage` names, as their object
    /// identifiers; empty for a template that names none.
    pub extended_key_usage: &'static [ObjectIdentifier],
}

/// A template's `basicConstraints` definition: whether the certificate issued under it is a
/// CA certificate, and how deep a hierarchy may sit below that CA.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicConstraintsDefinition {
    /// The `cA` flag: TRUE for the CA-certificate templates.
    pub ca: bool,
    /// The `pathLenConstraint` the template's `PathLenN` portion publishes, when it
    /// publishes one.
    pub path_length: Option<u8>,
}

/// A template's `keyUsage` definition: what the certified key may be used for (the module
/// docs). Every published definition asserts digital signature; what the kinds differ in is
/// what they assert beside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyUsageDefinition {
    /// A CA certificate's: it signs the certificates it issues and the CRLs it publishes.
    CertificateAndCrlSigning,
    /// An end-entity certificate whose key also establishes keys — the TLS kinds.
    SignatureAndKeyEncipherment,
    /// An end-entity certificate whose key only signs — the code-signing and OCSP-signing
    /// kinds.
    Signature,
}

impl KeyUsageDefinition {
    /// The named `keyUsage` bits this definition asserts, as the service's own key-usage
    /// shape — so the extension is encoded by the one encoder of those bits
    /// ([`super::key_usage_extension`]).
    fn bits(self) -> KeyUsage {
        let mut usage = KeyUsage {
            digital_signature: true,
            ..KeyUsage::default()
        };
        match self {
            Self::CertificateAndCrlSigning => {
                usage.key_cert_sign = true;
                usage.crl_sign = true;
            }
            Self::SignatureAndKeyEncipherment => usage.key_encipherment = true,
            Self::Signature => {}
        }
        usage
    }
}

/// The key purposes of `extendedKeyUsage` the published template definitions name, by the
/// object identifiers RFC 5280 section 4.2.1.12 assigns them (`id-kp-*`).
const TLS_SERVER_AUTHENTICATION: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.1");
const TLS_CLIENT_AUTHENTICATION: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.2");
const CODE_SIGNING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.3");
const OCSP_SIGNING: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.3.6.1.5.5.7.3.9");

/// The template name a request that names none is answered under: "If this parameter is not
/// provided, Amazon Web Services Private CA defaults to the `EndEntityCertificate/V1`
/// template." (`IssueCertificateRequest$TemplateArn` docs.)
pub const DEFAULT_TEMPLATE_NAME: &str = "EndEntityCertificate";

/// The version portion of the template ARN that documentation publishes for that default.
const DEFAULT_TEMPLATE_VERSION: &str = "V1";

/// The `TemplateArn` of the template a request that named none was answered under, which is
/// what a certificate issued by such a request was issued under
/// ([`DEFAULT_TEMPLATE_NAME`]).
///
/// A template is a definition of this service rather than a resource of an account, so its
/// ARN names no region and no account, exactly as the published `TemplateArn` values do.
pub fn default_template_arn() -> String {
    format!(
        "arn:aws:{}:::template/{DEFAULT_TEMPLATE_NAME}/{DEFAULT_TEMPLATE_VERSION}",
        super::ARN_SERVICE
    )
}

/// The template kind that defines a ROOT CA's own certificate.
const ROOT_CA_TEMPLATE_KIND: &str = "RootCACertificate";

/// The template kind that defines a SUBORDINATE CA's certificate, the one whose name
/// publishes the CA depth below it.
const SUBORDINATE_CA_TEMPLATE_KIND: &str = "SubordinateCACertificate";

/// The template kinds that define an END-ENTITY certificate, from the `TemplateArn` values
/// Amazon Web Services Private CA publishes, each with the key usage and the key purposes
/// its published definition states (the module docs). A name carrying none of these and no CA
/// marker is a kind this service does not know, and defines nothing.
const END_ENTITY_TEMPLATE_KINDS: &[(&str, KeyUsageDefinition, &[ObjectIdentifier])] = &[
    (
        "EndEntityCertificate",
        KeyUsageDefinition::SignatureAndKeyEncipherment,
        &[TLS_SERVER_AUTHENTICATION, TLS_CLIENT_AUTHENTICATION],
    ),
    (
        "EndEntityClientAuthCertificate",
        KeyUsageDefinition::SignatureAndKeyEncipherment,
        &[TLS_CLIENT_AUTHENTICATION],
    ),
    (
        "EndEntityServerAuthCertificate",
        KeyUsageDefinition::SignatureAndKeyEncipherment,
        &[TLS_SERVER_AUTHENTICATION],
    ),
    (
        "CodeSigningCertificate",
        KeyUsageDefinition::Signature,
        &[CODE_SIGNING],
    ),
    (
        "OCSPSigningCertificate",
        KeyUsageDefinition::Signature,
        &[OCSP_SIGNING],
    ),
];

/// The portion of a template name that publishes the CA depth of the certificate it
/// defines.
const PATH_LENGTH_SEGMENT: &str = "PathLen";

/// The prefix of the template kinds that define no extensions of their own.
const BLANK_TEMPLATE_PREFIX: &str = "Blank";

/// The portion of a blank template's name that says it defines critical basic constraints
/// after all.
const CRITICAL_BASIC_CONSTRAINTS_SEGMENT: &str = "CriticalBasicConstraints";

/// The template definition a request's `TemplateArn` selects, and the default template's
/// when the request named none.
pub fn certificate_template(template_arn: Option<&str>) -> CertificateTemplate {
    let name = template_arn
        .and_then(template_name)
        .unwrap_or(DEFAULT_TEMPLATE_NAME);
    definition_of(name)
}

/// Whether the template a request names defines a CA's OWN certificate — the certificate
/// an authority of the hierarchy holds and signs with — rather than a certificate the CA
/// issues to a subject.
///
/// The KIND portion of the template's name is what says so: the `RootCACertificate` and
/// `SubordinateCACertificate` kinds define a CA certificate, including their `Blank…`
/// variants, which take the rest of their content from an `ApiPassthrough` but still define
/// the certificate of a CA. Every other published kind defines an end-entity certificate,
/// and a request that names no template is answered under the default end-entity template
/// ([`DEFAULT_TEMPLATE_NAME`]), so neither of those is one.
pub fn names_ca_certificate_template(template_arn: Option<&str>) -> bool {
    template_arn.and_then(template_name).is_some_and(|name| {
        name.contains(ROOT_CA_TEMPLATE_KIND) || name.contains(SUBORDINATE_CA_TEMPLATE_KIND)
    })
}

/// The template name the ARN `arn:<partition>:acm-pca:::template/<name>/<version>` carries,
/// or `None` for an ARN that names no template.
fn template_name(template_arn: &str) -> Option<&str> {
    let (_, after) = template_arn.split_once(":template/")?;
    let name = after.split('/').next().unwrap_or(after);
    (!name.is_empty()).then_some(name)
}

/// The extensions the template called `name` defines (the module docs), which is nothing at
/// all for a plain blank kind or a kind this service does not know.
fn definition_of(name: &str) -> CertificateTemplate {
    let blank = name.starts_with(BLANK_TEMPLATE_PREFIX);
    // A blank kind defines nothing of its own — an `ApiPassthrough` fills the certificate in
    // — except the variants whose name says they define critical basic constraints, and
    // those define that one extension and no other.
    let basic_constraints_only = blank && name.contains(CRITICAL_BASIC_CONSTRAINTS_SEGMENT);
    if blank && !basic_constraints_only {
        return CertificateTemplate::default();
    }
    // A CA kind's name is what publishes the depth below it, so the depth is read only for
    // a kind this service knows: a name outside the published kinds defines nothing,
    // whatever portions it carries.
    if name.contains(ROOT_CA_TEMPLATE_KIND) || name.contains(SUBORDINATE_CA_TEMPLATE_KIND) {
        return CertificateTemplate {
            basic_constraints: Some(BasicConstraintsDefinition {
                ca: true,
                path_length: path_length_in(name),
            }),
            key_usage: (!basic_constraints_only)
                .then_some(KeyUsageDefinition::CertificateAndCrlSigning),
            extended_key_usage: &[],
        };
    }
    let Some((_, key_usage, extended_key_usage)) = END_ENTITY_TEMPLATE_KINDS
        .iter()
        .find(|(kind, _, _)| name.contains(kind))
    else {
        return CertificateTemplate::default();
    };
    CertificateTemplate {
        basic_constraints: Some(BasicConstraintsDefinition {
            ca: false,
            path_length: None,
        }),
        key_usage: (!basic_constraints_only).then_some(*key_usage),
        extended_key_usage: if basic_constraints_only {
            &[]
        } else {
            extended_key_usage
        },
    }
}

/// The CA depth the `PathLenN` portion of `name` publishes, when the name carries one.
fn path_length_in(name: &str) -> Option<u8> {
    let (_, after) = name.split_once(PATH_LENGTH_SEGMENT)?;
    let digits: String = after.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

/// The DER extensions a certificate issued under `template` for `request` carries: the ones
/// the template defines, plus the subject alternative name the certification request asks
/// for ([`requested_subject_alternative_name`]).
pub fn certificate_extensions(
    template: &CertificateTemplate,
    request: &CertificationRequestInfo,
) -> Option<Extensions> {
    let mut extensions: Extensions = template_extensions(template).unwrap_or_default();
    if let Some(alternative_name) = requested_subject_alternative_name(request) {
        extensions.push(alternative_name);
    }
    (!extensions.is_empty()).then_some(extensions)
}

/// The DER extensions a certificate issued under `template` carries, or `None` for a
/// template that defines none: the basic constraints, the key usage and the key purposes its
/// definition states (the module docs), in that order.
pub fn template_extensions(template: &CertificateTemplate) -> Option<Extensions> {
    let mut extensions: Extensions = Vec::new();
    if let Some(constraints) = template.basic_constraints {
        extensions.extend(basic_constraints_extension(constraints));
    }
    if let Some(key_usage) = template.key_usage {
        extensions.extend(super::key_usage_extension(&key_usage.bits()));
    }
    extensions.extend(extended_key_usage_extension(template.extended_key_usage));
    (!extensions.is_empty()).then_some(extensions)
}

/// The `extendedKeyUsage` extension (RFC 5280 section 4.2.1.12) naming `purposes`, or `None`
/// for a template that names none.
///
/// Not marked critical, as the published definitions state it: "if the extension is present,
/// then the certificate MUST only be used for one of the purposes indicated" is the
/// certificate's claim either way, and a non-critical extension leaves a relying party that
/// does not recognise a purpose free to accept the certificate.
fn extended_key_usage_extension(purposes: &[ObjectIdentifier]) -> Option<Extension> {
    if purposes.is_empty() {
        return None;
    }
    let value = ExtendedKeyUsage(purposes.to_vec());
    Some(Extension {
        extn_id: ExtendedKeyUsage::OID,
        critical: false,
        extn_value: OctetString::new(value.to_der().ok()?).ok()?,
    })
}

/// The `subjectAltName` extension (RFC 5280 section 4.2.1.6) the certification request asks
/// for, when it asks for one.
///
/// Every published template definition carries subject alternative name as a passthrough
/// from the certification request: the names a certificate is to be valid for are the
/// requester's to state and a relying party matches a certificate against them, so the
/// extension the request asks for is copied into the certificate as it stands rather than
/// being defined by the template. A PKCS#10 request states the extensions it asks for in its
/// `extensionRequest` attribute (RFC 2985 section 5.4.2).
pub fn requested_subject_alternative_name(
    request: &CertificationRequestInfo,
) -> Option<Extension> {
    requested_extensions(request)
        .into_iter()
        .find(|extension| extension.extn_id == super::oid::SUBJECT_ALT_NAME)
}

/// Every extension the `extensionRequest` attributes of `request` ask for, in the order they
/// are stated; none for a request that asks for no extension this service can read.
fn requested_extensions(request: &CertificationRequestInfo) -> Vec<Extension> {
    request
        .attributes
        .iter()
        .filter(|attribute| attribute.oid == super::oid::EXTENSION_REQUEST)
        .flat_map(|attribute| attribute.values.iter())
        .filter_map(|value| {
            let der = value.to_der().ok()?;
            Extensions::from_der(&der).ok()
        })
        .flatten()
        .collect()
}

/// The `basicConstraints` extension (RFC 5280 section 4.2.1.9) a template defines, marked
/// critical.
fn basic_constraints_extension(constraints: BasicConstraintsDefinition) -> Option<Extension> {
    let value = BasicConstraints {
        ca: constraints.ca,
        path_len_constraint: constraints.path_length,
    };
    Some(Extension {
        extn_id: BasicConstraints::OID,
        critical: true,
        extn_value: OctetString::new(value.to_der().ok()?).ok()?,
    })
}
