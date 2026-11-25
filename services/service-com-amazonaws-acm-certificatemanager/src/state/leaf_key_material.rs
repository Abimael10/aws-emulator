// SPDX-License-Identifier: CC0-1.0

//! The key pair ACM mints for a certificate it has a private certificate authority issue:
//! the certification request it publishes that key in, and the passphrase-encrypted form
//! an export hands the key back in.
//!
//! "The exported file contains the certificate, the certificate chain, and the encrypted
//! private key associated with the public key that is embedded in the certificate"
//! (`ExportCertificate` doc), so a certificate a caller can export is one ACM holds a key
//! pair for — a pair ACM generated, which is what `CertificateKeyPairOrigin`
//! `AWS_MANAGED` says of every certificate ACM issues. The pair is minted ONCE, by the
//! request that creates the certificate, because the public half is what the issuing
//! authority signs into the certificate: a second pair would not match the certificate the
//! CA already signed.
//!
//! The private half never leaves this process in the clear: it is held on the certificate's
//! own record, written to no file and named in no log — [`LeafPrivateKey`]'s own `Debug`
//! keeps it out of a formatted record — and the ONE thing that takes it out of the store is
//! an export, which hands back the PBES2-encrypted PKCS#8 [`passphrase_encrypted_pkcs8`]
//! builds under the passphrase the caller assigned to it.
//!
//! The algorithms are the `KeyAlgorithm` members the in-process cryptography provider can
//! mint: the RSA sizes it generates and the three NIST curves. For a member it cannot
//! ([`LeafKeyPair::generate`] answers `None`) ACM holds no key pair, so there is no
//! certification request to publish and nothing to export.
//!
//! This is the SUBJECT's key pair, which is why it is not the key type the private CA
//! service holds for an AUTHORITY: that one's private half is read only to make a
//! signature and has no accessor at all, deliberately, while this one's whole purpose is to
//! be handed to the caller who asked for the certificate. The two therefore mint the same
//! algorithms through the same provider and share nothing else — a CA key that could be
//! read out would be the one thing a certificate authority must never allow.

use ::aws_lc_rs::encoding::{AsDer, Pkcs8V1Der, PublicKeyX509Der};
use ::aws_lc_rs::rand::{SecureRandom, SystemRandom};
use ::aws_lc_rs::rsa::KeySize;
use ::aws_lc_rs::signature::{
    EcdsaKeyPair, EcdsaSigningAlgorithm, KeyPair as _, RsaEncoding, RsaKeyPair,
    ECDSA_P256_SHA256_ASN1_SIGNING, ECDSA_P384_SHA384_ASN1_SIGNING,
    ECDSA_P521_SHA512_ASN1_SIGNING, RSA_PKCS1_SHA256,
};
use ::x509_cert::attr::{Attribute, AttributeTypeAndValue};
use ::x509_cert::der::asn1::{Any, BitString, Ia5String, OctetString, SetOfVec};
use ::x509_cert::der::oid::AssociatedOid;
use ::x509_cert::der::{Decode, Encode, Tag as DerTag};
use ::x509_cert::ext::pkix::name::{GeneralName, GeneralNames};
use ::x509_cert::ext::pkix::SubjectAltName;
use ::x509_cert::ext::Extension;
use ::x509_cert::name::{Name, RdnSequence, RelativeDistinguishedName};
use ::x509_cert::request::{CertReq, CertReqInfo, Version as CertReqVersion};
use ::x509_cert::spki::{AlgorithmIdentifierOwned, ObjectIdentifier, SubjectPublicKeyInfoOwned};

/// The material one private-CA issuance needs, minted together and off the request path:
/// the key pair and the certification request that publishes it.
pub struct MintedLeafMaterial {
    /// The key pair, whose private half the certificate's record holds.
    pub key_pair: LeafKeyPair,
    /// The PEM-framed PKCS#10 certification request a certificate authority is handed.
    pub certification_request_pem: String,
}

/// Mint the key pair of a certificate covering `domain_names` with `key_algorithm`,
/// together with the certification request that publishes it, or `None` for a key
/// algorithm the in-process provider mints no pair for.
///
/// CPU work whose cost the CALLER chooses — searching for RSA primes takes orders of
/// magnitude longer than generating an elliptic-curve key — so it is called BEFORE any
/// store is locked and from the blocking pool rather than from an async worker, exactly as
/// `CreateCertificateAuthority` mints a CA's pair: no other request of this service waits
/// on the store for it, and the request's own timeout and the emulator's shutdown keep
/// their yield point. The pool itself is what bounds how many such mints run at once.
pub fn mint_leaf_material(
    key_algorithm: &str,
    domain_names: &[String],
) -> Option<MintedLeafMaterial> {
    let key_pair = LeafKeyPair::generate(key_algorithm)?;
    let certification_request_pem = key_pair.certification_request_pem(domain_names)?;
    Some(MintedLeafMaterial {
        key_pair,
        certification_request_pem,
    })
}

/// The PRIVATE half of a key pair ACM minted, as the DER PKCS#8 `PrivateKeyInfo`
/// (RFC 5208) every reader of it decodes from.
///
/// SECRET. A newtype rather than a bare `Vec<u8>` so the key cannot be formatted by
/// accident: the record it sits on derives `Debug`, and this prints no key material.
#[derive(Clone, PartialEq, Eq)]
pub struct LeafPrivateKey(Vec<u8>);

impl ::std::fmt::Debug for LeafPrivateKey {
    /// The fact that a key is held, never the key.
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_tuple("LeafPrivateKey").field(&"[redacted]").finish()
    }
}

impl LeafPrivateKey {
    /// The PKCS#8 `PrivateKeyInfo` DER of this key, for the one caller that needs it: the
    /// export that encrypts it under a caller-assigned passphrase.
    pub fn pkcs8_der(&self) -> &[u8] {
        &self.0
    }
}

/// The key pair ACM mints for one certificate it has a private CA issue.
///
/// Held as the two DER encodings the structures around it need: the public key as the
/// `SubjectPublicKeyInfo` a certification request and a certificate carry (RFC 5280
/// section 4.1.2.7), and the private key as the PKCS#8 `PrivateKeyInfo` (RFC 5208).
pub struct LeafKeyPair {
    /// The `KeyAlgorithm` member this pair was minted for.
    key_algorithm: String,
    /// The public key, as a DER `SubjectPublicKeyInfo`.
    public_key_der: Vec<u8>,
    /// The private key.
    private_key: LeafPrivateKey,
}

impl ::std::fmt::Debug for LeafKeyPair {
    /// Prints the pair WITHOUT its private key.
    fn fmt(&self, f: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
        f.debug_struct("LeafKeyPair")
            .field("key_algorithm", &self.key_algorithm)
            .field("public_key_der", &self.public_key_der)
            .finish_non_exhaustive()
    }
}

impl LeafKeyPair {
    /// A new key pair for a certificate requested with `key_algorithm`, or `None` for a
    /// member the in-process provider cannot mint.
    ///
    /// The members of `KeyAlgorithm` (`KEY_ALGORITHM_VALUES` in `ops::types`): the RSA
    /// sizes the provider generates and the three NIST curves. `RSA_1024` is the one
    /// member it mints no pair for — the provider generates no key that short.
    pub fn generate(key_algorithm: &str) -> Option<Self> {
        let (public_key_der, private_key_der) = match key_algorithm {
            "RSA_2048" => rsa_key_pair(KeySize::Rsa2048)?,
            "RSA_3072" => rsa_key_pair(KeySize::Rsa3072)?,
            "RSA_4096" => rsa_key_pair(KeySize::Rsa4096)?,
            "EC_prime256v1" => ecdsa_key_pair(&ECDSA_P256_SHA256_ASN1_SIGNING)?,
            "EC_secp384r1" => ecdsa_key_pair(&ECDSA_P384_SHA384_ASN1_SIGNING)?,
            "EC_secp521r1" => ecdsa_key_pair(&ECDSA_P521_SHA512_ASN1_SIGNING)?,
            _ => return None,
        };
        Some(Self {
            key_algorithm: key_algorithm.to_string(),
            public_key_der,
            private_key: LeafPrivateKey(private_key_der),
        })
    }

    /// The private half, to hold on the certificate's record.
    pub fn into_private_key(self) -> LeafPrivateKey {
        self.private_key
    }

    /// The PKCS#10 certification request ACM publishes this key pair in, as the PEM-framed
    /// DER a certificate authority takes (RFC 2986 section 4, RFC 7468 section 7).
    ///
    /// Its subject is the certificate's common name — the first name the certificate covers,
    /// which is the `DomainName` the request named — its `SubjectPublicKeyInfo` is this
    /// pair's public key, and the extension it asks for is the `subjectAltName` naming
    /// EVERY name the certificate covers (RFC 5280 section 4.2.1.6), so the certificate the
    /// authority signs for it covers them all. The signature is made with this pair's
    /// private half, so the authority that answers the request can verify that the key it
    /// is about to certify is one the requester holds.
    ///
    /// `None` when the names cannot be encoded as the request's DER demands, or when no
    /// signature can be made with this pair.
    pub fn certification_request_pem(&self, domain_names: &[String]) -> Option<String> {
        let common_name = domain_names.first()?;
        let info = CertReqInfo {
            version: CertReqVersion::V1,
            subject: common_name_subject(common_name)?,
            public_key: self.public_key_info()?,
            attributes: subject_alternative_name_attributes(domain_names)?,
        };
        let algorithm = self.signature_algorithm_identifier()?;
        let signature = BitString::from_bytes(&self.signature(&info.to_der().ok()?)?).ok()?;
        let request = CertReq {
            info,
            algorithm,
            signature,
        };
        Some(::service_com_amazonaws_acmpca_acmprivateca::state::encode_pem(
            ::service_com_amazonaws_acmpca_acmprivateca::state::PEM_LABEL_CSR,
            &request.to_der().ok()?,
        ))
    }

    /// The public half, as the `SubjectPublicKeyInfo` a certification request carries.
    fn public_key_info(&self) -> Option<SubjectPublicKeyInfoOwned> {
        SubjectPublicKeyInfoOwned::from_der(&self.public_key_der).ok()
    }

    /// The `AlgorithmIdentifier` of the signature this pair makes: RSASSA-PKCS1-v1_5 with
    /// SHA-256 for an RSA pair (RFC 8017 appendix C, which has it carry a NULL parameter),
    /// and ECDSA with the digest the curve is paired with for an elliptic-curve one
    /// (RFC 5758 section 3.2, where the identifier takes no parameters).
    fn signature_algorithm_identifier(&self) -> Option<AlgorithmIdentifierOwned> {
        let (algorithm, parameters) = match self.key_algorithm.as_str() {
            "RSA_2048" | "RSA_3072" | "RSA_4096" => (
                SHA256_WITH_RSA_ENCRYPTION,
                Some(Any::new(DerTag::Null, Vec::new()).ok()?),
            ),
            "EC_prime256v1" => (ECDSA_WITH_SHA256, None),
            "EC_secp384r1" => (ECDSA_WITH_SHA384, None),
            "EC_secp521r1" => (ECDSA_WITH_SHA512, None),
            _ => return None,
        };
        Some(AlgorithmIdentifierOwned {
            oid: algorithm,
            parameters,
        })
    }

    /// The signature this key makes over `message`, under the algorithm
    /// [`Self::signature_algorithm_identifier`] names, or `None` when the provider makes
    /// none for this pair.
    fn signature(&self, message: &[u8]) -> Option<Vec<u8>> {
        match self.key_algorithm.as_str() {
            "RSA_2048" | "RSA_3072" | "RSA_4096" => self.rsa_signature(&RSA_PKCS1_SHA256, message),
            "EC_prime256v1" => self.ecdsa_signature(&ECDSA_P256_SHA256_ASN1_SIGNING, message),
            "EC_secp384r1" => self.ecdsa_signature(&ECDSA_P384_SHA384_ASN1_SIGNING, message),
            "EC_secp521r1" => self.ecdsa_signature(&ECDSA_P521_SHA512_ASN1_SIGNING, message),
            _ => None,
        }
    }

    /// An RSA signature over `message` under `padding`.
    fn rsa_signature(
        &self,
        padding: &'static dyn RsaEncoding,
        message: &[u8],
    ) -> Option<Vec<u8>> {
        let key = RsaKeyPair::from_pkcs8(self.private_key.pkcs8_der()).ok()?;
        let mut signature = vec![0u8; key.public_modulus_len()];
        key.sign(padding, &SystemRandom::new(), message, &mut signature)
            .ok()?;
        Some(signature)
    }

    /// An ECDSA signature over `message`, as the DER `Ecdsa-Sig-Value` a certification
    /// request's signature field carries (RFC 5480 section 2.1.1).
    fn ecdsa_signature(
        &self,
        algorithm: &'static EcdsaSigningAlgorithm,
        message: &[u8],
    ) -> Option<Vec<u8>> {
        let key = EcdsaKeyPair::from_pkcs8(algorithm, self.private_key.pkcs8_der()).ok()?;
        Some(key.sign(&SystemRandom::new(), message).ok()?.as_ref().to_vec())
    }
}

/// `sha256WithRSAEncryption` (RFC 8017 appendix C).
const SHA256_WITH_RSA_ENCRYPTION: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.11");
/// `ecdsa-with-SHA256` (RFC 5758 section 3.2).
const ECDSA_WITH_SHA256: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.2");
/// `ecdsa-with-SHA384` (RFC 5758 section 3.2).
const ECDSA_WITH_SHA384: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.3");
/// `ecdsa-with-SHA512` (RFC 5758 section 3.2).
const ECDSA_WITH_SHA512: ObjectIdentifier = ObjectIdentifier::new_unwrap("1.2.840.10045.4.3.4");
/// `id-at-commonName` (RFC 4519 section 2.3).
const COMMON_NAME: ObjectIdentifier = ObjectIdentifier::new_unwrap("2.5.4.3");
/// `extensionRequest`, the PKCS#9 attribute a certification request states the extensions
/// it asks for in (RFC 2985 section 5.4.2).
const EXTENSION_REQUEST: ObjectIdentifier =
    ObjectIdentifier::new_unwrap("1.2.840.113549.1.9.14");

/// A fresh RSA pair of `size`, as (`SubjectPublicKeyInfo`, PKCS#8) DER.
fn rsa_key_pair(size: KeySize) -> Option<(Vec<u8>, Vec<u8>)> {
    let key = RsaKeyPair::generate(size).ok()?;
    let public: PublicKeyX509Der = AsDer::as_der(key.public_key()).ok()?;
    let private: Pkcs8V1Der = AsDer::as_der(&key).ok()?;
    Some((public.as_ref().to_vec(), private.as_ref().to_vec()))
}

/// A fresh ECDSA pair on the curve `algorithm` names, as (`SubjectPublicKeyInfo`, PKCS#8)
/// DER.
fn ecdsa_key_pair(algorithm: &'static EcdsaSigningAlgorithm) -> Option<(Vec<u8>, Vec<u8>)> {
    let private = EcdsaKeyPair::generate_pkcs8(algorithm, &SystemRandom::new()).ok()?;
    let key = EcdsaKeyPair::from_pkcs8(algorithm, private.as_ref()).ok()?;
    let public: PublicKeyX509Der = AsDer::as_der(key.public_key()).ok()?;
    Some((public.as_ref().to_vec(), private.as_ref().to_vec()))
}

/// The DER `Name` of a subject whose one relative distinguished name is the common name
/// `common_name`, as a UTF8String (RFC 5280 section 4.1.2.4).
fn common_name_subject(common_name: &str) -> Option<Name> {
    let pair = AttributeTypeAndValue {
        oid: COMMON_NAME,
        value: Any::new(DerTag::Utf8String, common_name.as_bytes()).ok()?,
    };
    let rdn = RelativeDistinguishedName::from(SetOfVec::try_from(vec![pair]).ok()?);
    Some(Name::from(RdnSequence(vec![rdn])))
}

/// The attributes a certification request covering `domain_names` carries: the
/// `extensionRequest` holding the `subjectAltName` extension that names each of them as a
/// DNS name.
fn subject_alternative_name_attributes(
    domain_names: &[String],
) -> Option<SetOfVec<Attribute>> {
    let names: GeneralNames = domain_names
        .iter()
        .map(|name| Ia5String::new(name).ok().map(GeneralName::DnsName))
        .collect::<Option<Vec<GeneralName>>>()?;
    let extensions = vec![Extension {
        extn_id: SubjectAltName::OID,
        critical: false,
        extn_value: OctetString::new(SubjectAltName(names).to_der().ok()?).ok()?,
    }];
    let values = SetOfVec::try_from(vec![Any::from_der(&extensions.to_der().ok()?).ok()?]).ok()?;
    SetOfVec::try_from(vec![Attribute {
        oid: EXTENSION_REQUEST,
        values,
    }])
    .ok()
}

/// How many iterations the key-derivation function of an export runs.
///
/// An export's passphrase is "any ASCII character except #, $, or %"
/// (`ExportCertificateRequest$Passphrase`) of at most 128 of them — a human-chosen secret,
/// which is what an expensive derivation protects: the work factor is what stands between
/// the exported file and an offline guess at that passphrase. RFC 8018 section 4.2 states
/// the floor ("a minimum of 1000 iterations is recommended"); this is well above it and
/// still cheap enough that an export is a request rather than a wait.
const KEY_DERIVATION_ITERATIONS: u32 = 10_000;
/// How many bytes of salt that derivation takes. Sixteen is the length RFC 8018 appendix
/// A.2 states for a PBKDF2 salt.
const KEY_DERIVATION_SALT_BYTES: usize = 16;
/// How many bytes of initialisation vector the cipher takes: one AES block.
const CIPHER_IV_BYTES: usize = 16;

/// The PEM-framed, passphrase-encrypted PKCS#8 of the private key `pkcs8_der`, as an export
/// hands a private key back.
///
/// "The exported file contains ... the encrypted private key associated with the public key
/// that is embedded in the certificate. For security, you must assign a passphrase for the
/// private key when exporting it" (`ExportCertificate` doc) — so the key leaves ACM
/// encrypted under the caller's own passphrase and under nothing else, and whoever holds
/// that passphrase is the only one who can read it back. The encryption is PBES2
/// (RFC 8018 section 6.2): the passphrase is run through PBKDF2 with HMAC-SHA-256 over a
/// fresh salt, and the key is enciphered with AES-256 in CBC mode under a fresh
/// initialisation vector, which is the scheme an encrypted PKCS#8 is read back with.
///
/// A fresh salt and vector per export are what keep two exports of one key from being the
/// same file, so neither reveals that the other was made under the same passphrase.
///
/// `None` when the stored key is not a PKCS#8 private key, when the random source fails, or
/// when the encryption does.
pub fn passphrase_encrypted_pkcs8(pkcs8_der: &[u8], passphrase: &[u8]) -> Option<String> {
    let random = SystemRandom::new();
    let mut salt = [0u8; KEY_DERIVATION_SALT_BYTES];
    let mut iv = [0u8; CIPHER_IV_BYTES];
    random.fill(&mut salt).ok()?;
    random.fill(&mut iv).ok()?;
    let parameters = ::pkcs5::pbes2::Parameters::pbkdf2_sha256_aes256cbc(
        KEY_DERIVATION_ITERATIONS,
        &salt,
        &iv,
    )
    .ok()?;
    let key = ::pkcs8::PrivateKeyInfo::try_from(pkcs8_der).ok()?;
    let encrypted = key.encrypt_with_params(parameters, passphrase).ok()?;
    Some(
        encrypted
            .to_pem(ENCRYPTED_PRIVATE_KEY_PEM_LABEL, ::pkcs8::LineEnding::LF)
            .ok()?
            .to_string(),
    )
}

/// PEM label of an encrypted PKCS#8 private key (RFC 7468 section 11).
const ENCRYPTED_PRIVATE_KEY_PEM_LABEL: &str = "ENCRYPTED PRIVATE KEY";
