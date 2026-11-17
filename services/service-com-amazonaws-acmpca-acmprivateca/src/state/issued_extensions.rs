// SPDX-License-Identifier: CC0-1.0

//! What an issued certificate carries beside the content of the certification request: the
//! values the API supplied, and the ones the issuing CA's own configuration publishes.
//!
//! Three sources decide the subject and the extensions of a certificate this service issues,
//! and they are ordered:
//!
//! 1. the certificate template the request named, whose definition is the certificate's kind
//!    ([`super::certificate_template`]) and therefore overrides everything a caller supplies;
//! 2. the `ApiPassthrough` the request carried — "An APIPassthrough or APICSRPassthrough
//!    template variant must be selected, or else this parameter is ignored"
//!    (`IssueCertificateRequest$ApiPassthrough`), so the template's name is what says whether
//!    it is read at all, and where it and the certification request say different things the
//!    API-supplied value is the one used;
//! 3. the certification request being signed, whose subject a certificate is issued for and
//!    whose `subjectAltName` is passed through ([`super::requested_subject_alternative_name`]).
//!
//! Beside the three sits what belongs to the ISSUER rather than to the request: the key
//! identifiers that bind the certificate to a key (RFC 5280 sections 4.2.1.1 and 4.2.1.2) and
//! the revocation endpoints the CA's `RevocationConfiguration` publishes — the CRL
//! distribution point of an enabled `CrlConfiguration` (section 4.2.1.13) and the OCSP
//! responder of an enabled `OcspConfiguration` (section 4.2.1.1). A caller may name those
//! itself through passthrough, which is why they are assembled last: "the CDP extension will
//! not be present in any certificates issued by that CA unless otherwise specified through CSR
//! or API passthrough" (`CrlDistributionPointExtensionConfiguration$OmitExtension`).

use ::std::net::IpAddr;

use ::x509_cert::der::asn1::Ia5String;
use ::x509_cert::der::oid::AssociatedOid;
use ::x509_cert::ext::pkix::certpolicy::{
    CertificatePolicies, PolicyInformation, PolicyQualifierInfo,
};
use ::x509_cert::ext::pkix::crl::dp::DistributionPoint;
use ::x509_cert::ext::pkix::name::{DistributionPointName, GeneralName, GeneralNames, OtherName};
use ::x509_cert::ext::pkix::{
    AccessDescription, AuthorityInfoAccessSyntax, AuthorityKeyIdentifier, CrlDistributionPoints,
    ExtendedKeyUsage as ExtendedKeyUsageExtension, SubjectAltName, SubjectKeyIdentifier,
};

use super::*;

use crate::ops::types::{
    CustomExtension, ExtendedKeyUsage as ApiExtendedKeyUsage, GeneralName as ApiGeneralName,
    OcspConfiguration, PolicyInformation as ApiPolicyInformation,
};

/// What one issuance asks the certificate it mints to carry, beside what the issuing CA
/// itself decides: the certification request being signed, the template that defines the
/// certificate's kind, the `ApiPassthrough` that applies to it
/// ([`super::applied_api_passthrough`]) and the validity window resolved for it.
#[derive(Debug, Clone, Copy)]
pub struct IssuedCertificateContent<'a> {
    /// The content of the certification request the CA is signing.
    pub request: &'a CertificationRequestInfo,
    /// The `SigningAlgorithm` the request asked the signature to be made under.
    pub signing_algorithm: &'a str,
    /// The definition the request's `TemplateArn` selected.
    pub template: &'a CertificateTemplate,
    /// The `ApiPassthrough` the request carried, when the template applies it.
    pub passthrough: Option<&'a ApiPassthrough>,
    /// The start of the certificate's validity period.
    pub not_before: SystemTime,
    /// The end of it.
    pub not_after: SystemTime,
}

/// The subject a certificate issued for `request` carries.
///
/// The subject of the certification request being signed, unless an `ApiPassthrough` that
/// applies ([`super::applied_api_passthrough`]) supplies one of its own: API-supplied values
/// take priority over the request's, so a passthrough subject REPLACES it rather than being
/// merged into it — the subject names one entity, and two subjects cannot name it together.
///
/// `None` when the subject cannot be encoded as an X.500 name.
pub fn issued_certificate_subject(
    request: &CertificationRequestInfo,
    passthrough: Option<&ApiPassthrough>,
) -> Option<Name> {
    match passthrough
        .and_then(|passthrough| passthrough.subject.as_ref())
        .filter(|subject| !subject_is_empty(subject))
    {
        Some(subject) => subject_name(subject),
        None => Some(request.subject.clone()),
    }
}

/// The DER extensions a certificate issued by `ca` under `template` for `request` carries, or
/// `None` when it carries none.
///
/// Assembled in the order of the module docs — the template's definition, then the
/// API-supplied extensions, then the certification request's, then the issuer's own — and an
/// extension is carried once: the FIRST source that states an extension is the one that
/// decides it, which is what makes that order the order of priority.
pub fn issued_certificate_extensions(
    ca: &CertificateAuthorityRecord,
    template: &CertificateTemplate,
    request: &CertificationRequestInfo,
    passthrough: Option<&ApiPassthrough>,
    subject_public_key: &SubjectPublicKeyInfoOwned,
) -> Option<Extensions> {
    let mut extensions: Extensions = Vec::new();
    for extension in template_extensions(template).unwrap_or_default() {
        carry(&mut extensions, extension);
    }
    for extension in api_passthrough_extensions(passthrough) {
        carry(&mut extensions, extension);
    }
    if let Some(alternative_name) = requested_subject_alternative_name(request) {
        carry(&mut extensions, alternative_name);
    }
    if let Some(identifier) = subject_key_identifier_extension(subject_public_key) {
        carry(&mut extensions, identifier);
    }
    if let Some(identifier) = issuing_authority_key_identifier(ca, subject_public_key) {
        carry(&mut extensions, identifier);
    }
    for extension in revocation_extensions(ca) {
        carry(&mut extensions, extension);
    }
    (!extensions.is_empty()).then_some(extensions)
}

/// Carry `extension` unless an extension of the same object identifier is already carried: a
/// certificate states each extension at most once (RFC 5280 section 4.2), and the one already
/// carried came from a source of higher priority.
fn carry(extensions: &mut Extensions, extension: Extension) {
    if extensions
        .iter()
        .any(|carried| carried.extn_id == extension.extn_id)
    {
        return;
    }
    extensions.push(extension);
}

/// The extensions an `ApiPassthrough` that applies supplies, in the order the `Extensions`
/// shape states its members.
fn api_passthrough_extensions(passthrough: Option<&ApiPassthrough>) -> Extensions {
    let Some(supplied) = passthrough.and_then(|passthrough| passthrough.extensions.as_ref())
    else {
        return Vec::new();
    };
    let mut extensions: Extensions = Vec::new();
    extensions.extend(certificate_policies_extension(
        supplied.certificate_policies.as_deref(),
    ));
    extensions.extend(api_extended_key_usage_extension(
        supplied.extended_key_usage.as_deref(),
    ));
    extensions.extend(supplied.key_usage.as_ref().and_then(key_usage_extension));
    extensions.extend(subject_alternative_name_extension(
        supplied.subject_alternative_names.as_deref(),
    ));
    extensions.extend(custom_extensions(supplied.custom_extensions.as_deref()));
    extensions
}

/// The `certificatePolicies` extension (RFC 5280 section 4.2.1.4) the API supplied, or `None`
/// when it supplied no policy.
///
/// Each policy is its `CertPolicyId` with the qualifiers stated for it; the one qualifier
/// Amazon Web Services Private CA supports is the certification practice statement pointer
/// ("Amazon Web Services Private CA supports the use of a URI for a CPS qualifier in this
/// field", `PolicyQualifierInfo$Qualifier`), whose qualifier is that URI as an IA5String.
fn certificate_policies_extension(policies: Option<&[ApiPolicyInformation]>) -> Option<Extension> {
    let supplied = policies?;
    let mut carried: Vec<PolicyInformation> = Vec::new();
    for policy in supplied {
        carried.push(PolicyInformation {
            policy_identifier: ObjectIdentifier::new(&policy.cert_policy_id).ok()?,
            policy_qualifiers: policy_qualifiers(policy)?,
        });
    }
    if carried.is_empty() {
        return None;
    }
    encoded_extension(
        CertificatePolicies::OID,
        false,
        &CertificatePolicies(carried),
    )
}

/// The qualifiers a supplied policy states, or `None` for a policy that states none.
fn policy_qualifiers(policy: &ApiPolicyInformation) -> Option<Option<Vec<PolicyQualifierInfo>>> {
    let Some(supplied) = policy.policy_qualifiers.as_ref() else {
        return Some(None);
    };
    let mut qualifiers: Vec<PolicyQualifierInfo> = Vec::new();
    for qualifier in supplied {
        qualifiers.push(PolicyQualifierInfo {
            policy_qualifier_id: policy_qualifier_id(&qualifier.policy_qualifier_id)?,
            qualifier: Some(
                Any::new(DerTag::Ia5String, qualifier.qualifier.cps_uri.as_bytes()).ok()?,
            ),
        });
    }
    Some((!qualifiers.is_empty()).then_some(qualifiers))
}

/// The object identifier a `PolicyQualifierId` member names: the certification practice
/// statement pointer `id-qt-cps` of RFC 5280 section 4.2.1.4, the one qualifier the enum
/// publishes.
fn policy_qualifier_id(policy_qualifier_id: &str) -> Option<ObjectIdentifier> {
    match policy_qualifier_id {
        "CPS" => Some(oid::CPS_QUALIFIER),
        _ => None,
    }
}

/// The `extendedKeyUsage` extension (RFC 5280 section 4.2.1.12) the API supplied, naming the
/// key purposes it asked for, or `None` when it asked for none.
fn api_extended_key_usage_extension(purposes: Option<&[ApiExtendedKeyUsage]>) -> Option<Extension> {
    let supplied = purposes?;
    let mut named: Vec<ObjectIdentifier> = Vec::new();
    for purpose in supplied {
        named.push(extended_key_usage_identifier(purpose)?);
    }
    if named.is_empty() {
        return None;
    }
    encoded_extension(
        ExtendedKeyUsageExtension::OID,
        false,
        &ExtendedKeyUsageExtension(named),
    )
}

/// The object identifier one supplied `ExtendedKeyUsage` names: the OID of the
/// `ExtendedKeyUsageType` member it states, or the custom one it carries
/// (`ExtendedKeyUsage$ExtendedKeyUsageObjectIdentifier`).
fn extended_key_usage_identifier(purpose: &ApiExtendedKeyUsage) -> Option<ObjectIdentifier> {
    if let Some(custom) = purpose
        .extended_key_usage_object_identifier
        .as_deref()
        .filter(|identifier| !identifier.is_empty())
    {
        return ObjectIdentifier::new(custom).ok();
    }
    extended_key_usage_type_identifier(purpose.extended_key_usage_type.as_deref()?)
}

/// The object identifier each `ExtendedKeyUsageType` member names.
///
/// The members of that enum (`EXTENDED_KEY_USAGE_TYPE_VALUES` in `ops::types`), each with the
/// key purpose it publishes: the `id-kp-*` purposes of RFC 5280 section 4.2.1.12, the
/// Microsoft smart-card-logon and document-signing purposes of the `1.3.6.1.4.1.311` arc, and
/// the certificate-transparency purpose of RFC 6962 section 3.1
/// (`1.3.6.1.4.1.11129.2.4.4`).
fn extended_key_usage_type_identifier(
    extended_key_usage_type: &str,
) -> Option<ObjectIdentifier> {
    Some(match extended_key_usage_type {
        "SERVER_AUTH" => oid::TLS_SERVER_AUTH,
        "CLIENT_AUTH" => oid::TLS_CLIENT_AUTH,
        "CODE_SIGNING" => oid::CODE_SIGNING,
        "EMAIL_PROTECTION" => oid::EMAIL_PROTECTION,
        "TIME_STAMPING" => oid::TIME_STAMPING,
        "OCSP_SIGNING" => oid::OCSP_SIGNING,
        "SMART_CARD_LOGIN" => oid::SMART_CARD_LOGIN,
        "DOCUMENT_SIGNING" => oid::DOCUMENT_SIGNING,
        "CERTIFICATE_TRANSPARENCY" => oid::CERTIFICATE_TRANSPARENCY,
        _ => return None,
    })
}

/// The `subjectAltName` extension (RFC 5280 section 4.2.1.6) the API supplied, or `None` when
/// it supplied no name this service can encode.
fn subject_alternative_name_extension(names: Option<&[ApiGeneralName]>) -> Option<Extension> {
    let supplied = names?;
    let carried: GeneralNames = supplied.iter().filter_map(general_name).collect();
    if carried.is_empty() {
        return None;
    }
    encoded_extension(SubjectAltName::OID, false, &SubjectAltName(carried))
}

/// One supplied `GeneralName` as the ASN.1 `GeneralName` of RFC 5280 section 4.2.1.6.
///
/// "Only one of the following naming options should be provided" (`GeneralName` docs), so the
/// first option the structure carries is the name it states, read in the order the shape
/// declares its members.
fn general_name(name: &ApiGeneralName) -> Option<GeneralName> {
    if let Some(other) = name.other_name.as_ref() {
        return Some(GeneralName::OtherName(OtherName {
            type_id: ObjectIdentifier::new(&other.type_id).ok()?,
            value: Any::new(DerTag::Utf8String, other.value.as_bytes()).ok()?,
        }));
    }
    if let Some(mail) = stated(name.rfc822_name.as_deref()) {
        return Some(GeneralName::Rfc822Name(ia5(mail)?));
    }
    if let Some(dns) = stated(name.dns_name.as_deref()) {
        return Some(GeneralName::DnsName(ia5(dns)?));
    }
    if let Some(directory) = name.directory_name.as_ref() {
        return Some(GeneralName::DirectoryName(subject_name(directory)?));
    }
    if let Some(uri) = stated(name.uniform_resource_identifier.as_deref()) {
        return Some(GeneralName::UniformResourceIdentifier(ia5(uri)?));
    }
    if let Some(address) = stated(name.ip_address.as_deref()) {
        return Some(GeneralName::IpAddress(ip_address(address)?));
    }
    if let Some(registered) = stated(name.registered_id.as_deref()) {
        return Some(GeneralName::RegisteredId(
            ObjectIdentifier::new(registered).ok()?,
        ));
    }
    None
}

/// `value` when it states something: a member left out, or left empty, states no value.
fn stated(value: Option<&str>) -> Option<&str> {
    value.filter(|value| !value.is_empty())
}

/// `text` as the IA5String a `GeneralName` carries its text options in.
fn ia5(text: &str) -> Option<Ia5String> {
    Ia5String::new(text).ok()
}

/// An "IPv4 or IPv6 address" (`GeneralName$IpAddress`) as the OCTET STRING of its address
/// bytes that RFC 5280 section 4.2.1.6 has the `iPAddress` option carry.
fn ip_address(address: &str) -> Option<OctetString> {
    match address.parse::<IpAddr>().ok()? {
        IpAddr::V4(address) => OctetString::new(address.octets().as_slice()).ok(),
        IpAddr::V6(address) => OctetString::new(address.octets().as_slice()).ok(),
    }
}

/// The extensions a supplied `CustomExtension` list states, each with the criticality it
/// states ("Default value for each option is false", so an extension that states none is not
/// critical) and the DER value it carries, which the shape publishes "base64-encoded"
/// (`CustomExtension$Value`).
fn custom_extensions(custom: Option<&[CustomExtension]>) -> Extensions {
    let Some(supplied) = custom else {
        return Vec::new();
    };
    supplied
        .iter()
        .filter_map(|extension| {
            Some(Extension {
                extn_id: ObjectIdentifier::new(&extension.object_identifier).ok()?,
                critical: extension.critical.unwrap_or(false),
                extn_value: OctetString::new(
                    ::aws_smithy_types::base64::decode(&extension.value).ok()?,
                )
                .ok()?,
            })
        })
        .collect()
}

/// The `subjectKeyIdentifier` extension (RFC 5280 section 4.2.1.2) of a certificate over
/// `subject_public_key`: the identifier derived from that key
/// ([`super::derived_key_identifier`]), which is the value the certificates and CRLs signed
/// with the key then name as their authority key identifier.
fn subject_key_identifier_extension(
    subject_public_key: &SubjectPublicKeyInfoOwned,
) -> Option<Extension> {
    encoded_extension(
        SubjectKeyIdentifier::OID,
        false,
        &SubjectKeyIdentifier(derived_key_identifier(subject_public_key)?),
    )
}

/// The `authorityKeyIdentifier` extension (RFC 5280 section 4.2.1.1) of a certificate this CA
/// signs: the identifier of the key it signed with.
///
/// That key is the one the CA's OWN certificate holds, so the identifier is the one that
/// certificate publishes ([`super::issuer_key_identifier`]). A CA that holds no certificate
/// yet is signing its own self-signed root certificate — the one issuance that precedes its
/// activation — where the key that signs and the key being certified are the same key, so the
/// identifier is derived from the certified key itself.
fn issuing_authority_key_identifier(
    ca: &CertificateAuthorityRecord,
    subject_public_key: &SubjectPublicKeyInfoOwned,
) -> Option<Extension> {
    let key_identifier = match issuer_key_identifier(ca) {
        Some(identifier) => identifier,
        None => derived_key_identifier(subject_public_key)?,
    };
    encoded_extension(
        AuthorityKeyIdentifier::OID,
        false,
        &AuthorityKeyIdentifier {
            key_identifier: Some(key_identifier),
            authority_cert_issuer: None,
            authority_cert_serial_number: None,
        },
    )
}

/// The extensions the issuing CA's `RevocationConfiguration` publishes: where a relying party
/// fetches this CA's CRL, and where it asks this CA about a certificate's status.
fn revocation_extensions(ca: &CertificateAuthorityRecord) -> Extensions {
    let configured = ca.revocation_configuration.as_ref();
    let mut extensions: Extensions = Vec::new();
    if let Some(crl) = configured
        .and_then(|revocation| revocation.crl_configuration.as_ref())
        .filter(|crl| crl.enabled)
    {
        // "Configures whether the CRL Distribution Point extension should be populated with
        // the default URL to the CRL. If set to true, then the CDP extension will not be
        // present in any certificates issued by that CA"
        // (`CrlDistributionPointExtensionConfiguration$OmitExtension`) — and a configuration
        // that carries no such member takes the default, which is to publish it.
        let omitted = crl
            .crl_distribution_point_extension_configuration
            .as_ref()
            .is_some_and(|configuration| configuration.omit_extension);
        if !omitted {
            extensions.extend(crl_distribution_point_extension(ca, crl));
        }
    }
    if let Some(ocsp) = configured
        .and_then(|revocation| revocation.ocsp_configuration.as_ref())
        .filter(|ocsp| ocsp.enabled)
    {
        extensions.extend(ocsp_responder_extension(ca, ocsp));
    }
    extensions
}

/// The `cRLDistributionPoints` extension (RFC 5280 section 4.2.1.13) naming the CA's default
/// CRL URL ([`default_crl_url`]), as one distribution point whose `fullName` is that URI.
fn crl_distribution_point_extension(
    ca: &CertificateAuthorityRecord,
    crl: &CrlConfiguration,
) -> Option<Extension> {
    let url = default_crl_url(ca, crl)?;
    let point = DistributionPoint {
        distribution_point: Some(DistributionPointName::FullName(vec![
            GeneralName::UniformResourceIdentifier(ia5(&url)?),
        ])),
        reasons: None,
        crl_issuer: None,
    };
    encoded_extension(
        CrlDistributionPoints::OID,
        false,
        &CrlDistributionPoints(vec![point]),
    )
}

/// The URL the certificates this CA issues name as their CRL distribution point.
///
/// A value the certificate CARRIES, for the relying party that reads it to fetch the list
/// with: nothing in this emulator connects to it. The list itself is deposited through the
/// in-process object-storage client ([`super::publish_crl`]), which is the only thing that
/// moves a CRL anywhere.
///
/// "If you do not provide a value for the `CustomCname` argument, the name of your S3 bucket
/// is placed into the `CRL Distribution Points` extension of the issued certificate"
/// (`CrlConfiguration$S3BucketName`), and the bucket is addressed in the Region the CA signs
/// in; a configuration that names a CNAME publishes that host instead
/// ("enables the use of an alias for the CRL distribution point. Use this value if you don't
/// want the name of your S3 bucket to be public", `CrlConfiguration$CustomCname`). The path is
/// the key the CRL object itself is written at ([`super::crl_object_key`]), so what the
/// extension names is where the CA puts its CRL.
///
/// The URL is `http`, not `https`: a relying party fetches a CRL to decide whether a
/// certificate is still valid, so fetching it cannot itself depend on validating one.
pub fn default_crl_url(ca: &CertificateAuthorityRecord, crl: &CrlConfiguration) -> Option<String> {
    let authority = parse_certificate_authority_arn(&ca.arn)?;
    let host = match crl
        .custom_cname
        .as_deref()
        .filter(|cname| !cname.is_empty())
    {
        Some(cname) => cname.to_string(),
        None => format!(
            "{}.s3.{}.amazonaws.com",
            crl.s3_bucket_name.as_deref()?,
            signing_region(&authority.region)
        ),
    };
    Some(format!(
        "http://{host}/{}",
        crl_object_key(&authority.id, crl)
    ))
}

/// The `authorityInformationAccess` extension (RFC 5280 section 4.2.1.1) naming this CA's
/// OCSP responder, as one access description whose access method is `id-ad-ocsp` and whose
/// location is the responder's URI ([`ocsp_responder_url`]).
fn ocsp_responder_extension(
    ca: &CertificateAuthorityRecord,
    ocsp: &OcspConfiguration,
) -> Option<Extension> {
    let url = ocsp_responder_url(ca, ocsp)?;
    let responder = AccessDescription {
        access_method: oid::OCSP_ACCESS_METHOD,
        access_location: GeneralName::UniformResourceIdentifier(ia5(&url)?),
    };
    encoded_extension(
        AuthorityInfoAccessSyntax::OID,
        false,
        &AuthorityInfoAccessSyntax(vec![responder]),
    )
}

/// The URL the certificates this CA issues name as their OCSP responder.
///
/// A value the certificate CARRIES, exactly as the CRL URL above is: this emulator serves no
/// Online Certificate Status Protocol responder and connects to none.
///
/// "By default, Amazon Web Services Private CA injects an Amazon Web Services domain into
/// certificates being validated by the Online Certificate Status Protocol (OCSP). A customer
/// can alternatively use this [custom CNAME]" (`OcspConfiguration$OcspCustomCname`): the
/// injected domain is this service's own responder in the Region the CA signs in, and a
/// configured CNAME is the host that replaces it.
///
/// `http`, for the same reason the CRL URL above is.
pub fn ocsp_responder_url(
    ca: &CertificateAuthorityRecord,
    ocsp: &OcspConfiguration,
) -> Option<String> {
    let authority = parse_certificate_authority_arn(&ca.arn)?;
    let host = match ocsp
        .ocsp_custom_cname
        .as_deref()
        .filter(|cname| !cname.is_empty())
    {
        Some(cname) => cname.to_string(),
        None => format!(
            "ocsp.{ARN_SERVICE}.{}.amazonaws.com",
            signing_region(&authority.region)
        ),
    };
    Some(format!("http://{host}"))
}

/// The Region a CA's published endpoints are addressed in: the one its ARN names, or this
/// service's default for the partition-global ARN form the pattern also admits.
fn signing_region(region: &str) -> &str {
    if region.is_empty() {
        DEFAULT_REGION
    } else {
        region
    }
}

/// One extension carrying the DER of `value`.
fn encoded_extension<T: Encode>(
    extn_id: ObjectIdentifier,
    critical: bool,
    value: &T,
) -> Option<Extension> {
    Some(Extension {
        extn_id,
        critical,
        extn_value: OctetString::new(value.to_der().ok()?).ok()?,
    })
}
