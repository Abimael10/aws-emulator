// SPDX-License-Identifier: CC0-1.0

//! The NitroTPM ENDORSEMENT KEY one instance holds, and the two forms
//! `GetInstanceTpmEkPub` reports it in.
//!
//! "NitroTPM is a virtual device … that follows the TPM 2.0 specification" and "the
//! endorsement key (EK) is a primary key … created by the TPM" (Amazon EC2 User Guide,
//! "NitroTPM"): the key belongs to the INSTANCE, so the public half this emulator reports is
//! real key material minted once per instance and stored beside it — two reads of one
//! instance answer the same key, and no two instances share one.
//!
//! Both published key types are minted, because "the required public endorsement key type"
//! (`GetInstanceTpmEkPubRequest$KeyType`) is the caller's to pick, and only the PUBLIC parts
//! are kept: the modulus and exponent of the RSA key, and the point of the P-384 key. Both
//! published formats are then DERIVED from those parts —
//! "specify `der` for a DER-encoded public key that is compatible with OpenSSL. Specify
//! `tpmt` for a TPM 2.0 format that is compatible with `tpm2-tools`"
//! (`$KeyFormat`) — so the two formats of one key can never disagree about the key they
//! carry.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

/// "`rsa-2048`" — a member of the modelled `EkPubKeyType` catalog
/// ([`crate::ops::types::EK_PUB_KEY_TYPE_VALUES`]).
pub const EK_PUB_KEY_TYPE_RSA_2048: &str = "rsa-2048";

/// "`ecc-sec-p384`" — the other member of that catalog.
pub const EK_PUB_KEY_TYPE_ECC_SEC_P384: &str = "ecc-sec-p384";

/// "`der`" — a member of the modelled `EkPubKeyFormat` catalog
/// ([`crate::ops::types::EK_PUB_KEY_FORMAT_VALUES`]): "a DER-encoded public key that is
/// compatible with OpenSSL".
pub const EK_PUB_KEY_FORMAT_DER: &str = "der";

/// "`tpmt`" — the other member of that catalog: "a TPM 2.0 format that is compatible with
/// tpm2-tools".
pub const EK_PUB_KEY_FORMAT_TPMT: &str = "tpmt";

/// Is `allowed` the `EkPubKeyFormat` or the `EkPubKeyType` catalog — the value sets the model
/// declares for the FORM and the TYPE of the endorsement key a caller asks for
/// ([`crate::ops::types::EK_PUB_KEY_FORMAT_VALUES`],
/// [`crate::ops::types::EK_PUB_KEY_TYPE_VALUES`])?
///
/// Keyed to the TYPES' value sets, the way every catalog-keyed refusal of this crate is
/// ([`is_modelled_catalog`] records that rule): EC2 answers a value outside either of them
/// with its own `InvalidParameterValue` rather than as a validation clause, so the boundary
/// walk that refuses one states that code ([`crate::service`]).
pub fn is_ek_pub_key_catalog(allowed: &[String]) -> bool {
    is_modelled_catalog(allowed, crate::ops::types::EK_PUB_KEY_FORMAT_VALUES)
        || is_modelled_catalog(allowed, crate::ops::types::EK_PUB_KEY_TYPE_VALUES)
}

/// The sentence EC2 answers beside `InvalidParameterValue` for a key type or format outside
/// those catalogs.
///
/// RECORDED: FIXED text — it names neither the member nor the value, so nothing of the
/// request is interpolated into it, which is also why this is the whole wording for this
/// error class rather than a template.
pub const EK_PUB_KEY_REFUSAL_MESSAGE: &str = "Invalid parameter value provided";

/// `InstanceTypeInfo$NitroTpmSupport`'s `supported` member: the value a type that offers the
/// virtual trusted platform module reports
/// ([`crate::ops::types::NITRO_TPM_SUPPORT_VALUES`]).
const NITRO_TPM_SUPPORT_SUPPORTED: &str = "supported";

/// The bits of the RSA endorsement key: the `rsa-2048` key type's own number.
const EK_RSA_MODULUS_BITS: u16 = 2048;

/// The bytes a 2048-bit modulus occupies.
const EK_RSA_MODULUS_BYTES: usize = 256;

/// The bytes one coordinate of a P-384 point occupies.
const EK_ECC_COORDINATE_BYTES: usize = 48;

/// The public halves of one instance's endorsement keys, as the parts the TPM holds them in.
///
/// Stored rather than re-derived, because "the public endorsement key"
/// (`GetInstanceTpmEkPubResult$KeyValue`) is a fact of the instance: a caller that reads it
/// twice has to be given the same key both times. Only the PUBLIC parts are kept — nothing
/// this emulator does needs the private halves, and a key it does not hold cannot leak.
#[derive(Debug, Clone)]
pub struct InstanceEndorsementKeys {
    /// The modulus of the RSA endorsement key, big-endian, without a leading zero.
    pub rsa_modulus: Vec<u8>,
    /// Its public exponent, big-endian.
    pub rsa_public_exponent: Vec<u8>,
    /// The P-384 endorsement key's point, SEC1 uncompressed (`0x04 || X || Y`).
    pub ecc_point: Vec<u8>,
}

impl InstanceEndorsementKeys {
    /// A fresh pair of endorsement keys, or `None` when neither key generates — which a
    /// caller reports as a server error rather than answering key material nothing produced.
    ///
    /// Both keys come from the generator every other real key of this crate comes from
    /// ([`super::key_pair_material`]'s `aws-lc-rs`).
    pub fn generate() -> Option<Self> {
        let rsa = ::aws_lc_rs::rsa::PrivateDecryptingKey::generate(
            ::aws_lc_rs::rsa::KeySize::Rsa2048,
        )
        .ok()?;
        let pkcs8: Vec<u8> = ::aws_lc_rs::encoding::AsDer::as_der(&rsa).ok()?.as_ref().to_vec();
        let private_key_info = ::pkcs8::PrivateKeyInfo::try_from(pkcs8.as_slice()).ok()?;
        let rsa_key = <::pkcs1::RsaPrivateKey as ::pkcs1::der::Decode>::from_der(
            private_key_info.private_key,
        )
        .ok()?;
        let ecc_point = generate_ecc_p384_point()?;
        Some(Self {
            rsa_modulus: rsa_key.modulus.as_bytes().to_vec(),
            rsa_public_exponent: rsa_key.public_exponent.as_bytes().to_vec(),
            ecc_point,
        })
    }

    /// The bytes one stored pair of endorsement keys holds: the public parts themselves, on
    /// top of the fixed per-record overhead every stored record of this crate is charged
    /// ([`STORED_RECORD_OVERHEAD_BYTES`]).
    ///
    /// The ONE definition of that charge, so the writer that records a pair and the prune that
    /// drops one give the budget exactly the same number.
    pub fn payload_bytes(&self) -> usize {
        STORED_RECORD_OVERHEAD_BYTES
            .saturating_add(self.rsa_modulus.len())
            .saturating_add(self.rsa_public_exponent.len())
            .saturating_add(self.ecc_point.len())
    }

    /// The key of type `key_type` in format `key_format`, as the bytes
    /// `GetInstanceTpmEkPub` base64-encodes into `KeyValue` — or `None` for a type or format
    /// outside the modelled catalogs, and for key parts that do not encode.
    pub fn key_value(&self, key_type: &str, key_format: &str) -> Option<Vec<u8>> {
        match (key_type, key_format) {
            (EK_PUB_KEY_TYPE_RSA_2048, EK_PUB_KEY_FORMAT_DER) => self.rsa_subject_public_key_info(),
            (EK_PUB_KEY_TYPE_RSA_2048, EK_PUB_KEY_FORMAT_TPMT) => Some(self.rsa_tpmt_public()),
            (EK_PUB_KEY_TYPE_ECC_SEC_P384, EK_PUB_KEY_FORMAT_DER) => {
                self.ecc_subject_public_key_info()
            }
            (EK_PUB_KEY_TYPE_ECC_SEC_P384, EK_PUB_KEY_FORMAT_TPMT) => Some(self.ecc_tpmt_public()),
            _ => None,
        }
    }

    /// The RSA key as the X.509 `SubjectPublicKeyInfo` (RFC 5280) OpenSSL reads: the
    /// `rsaEncryption` algorithm identifier over the PKCS#1 `RSAPublicKey` of this key's own
    /// modulus and exponent.
    fn rsa_subject_public_key_info(&self) -> Option<Vec<u8>> {
        use ::pkcs1::der::Encode;
        let public = ::pkcs1::RsaPublicKey {
            modulus: ::pkcs1::UintRef::new(&self.rsa_modulus).ok()?,
            public_exponent: ::pkcs1::UintRef::new(&self.rsa_public_exponent).ok()?,
        };
        let rsa_public_key = public.to_der().ok()?;
        let info = ::spki::SubjectPublicKeyInfoRef {
            algorithm: ::pkcs1::ALGORITHM_ID,
            subject_public_key: ::spki::der::asn1::BitStringRef::new(0, &rsa_public_key).ok()?,
        };
        info.to_der().ok()
    }

    /// The P-384 key as that same `SubjectPublicKeyInfo`: the `id-ecPublicKey` algorithm
    /// identifier naming the `secp384r1` curve ([`ID_EC_PUBLIC_KEY`], [`SECP384R1`]), over
    /// this key's own point, written by the crate this service writes every other X.509
    /// structure with.
    fn ecc_subject_public_key_info(&self) -> Option<Vec<u8>> {
        use ::spki::der::Encode;
        let info = ::spki::SubjectPublicKeyInfoRef {
            algorithm: ::spki::AlgorithmIdentifierRef {
                oid: ID_EC_PUBLIC_KEY,
                parameters: Some(::spki::der::asn1::AnyRef::from(&SECP384R1)),
            },
            subject_public_key: ::spki::der::asn1::BitStringRef::new(0, &self.ecc_point).ok()?,
        };
        info.to_der().ok()
    }

    /// The RSA key as the `TPMT_PUBLIC` structure of the TPM 2.0 specification (Trusted
    /// Platform Module Library Part 2, "TPMT_PUBLIC"), which is what `tpm2-tools` reads.
    ///
    /// The template is the endorsement key's: the object is fixed to this TPM and parent, its
    /// sensitive data came from the TPM, it is administered by policy, and it is a restricted
    /// decryption key ([`EK_OBJECT_ATTRIBUTES`]) under the published EK authorization policy
    /// ([`EK_AUTH_POLICY_SHA256`]), with the symmetric and scheme parameters that template
    /// publishes. The `unique` field is this key's own modulus, so the structure carries the
    /// same key the DER form does.
    fn rsa_tpmt_public(&self) -> Vec<u8> {
        let mut out = tpmt_public_header(TPM_ALG_RSA);
        // TPMS_RSA_PARMS: the symmetric algorithm the key wraps with, no signing scheme, the
        // key's size, and the exponent — `0` being the TPM's own encoding of the default
        // public exponent (65537), which is what this key carries.
        out.extend_from_slice(&TPM_SYM_DEF_OBJECT_AES_128_CFB);
        out.extend_from_slice(&TPM_ALG_NULL.to_be_bytes());
        out.extend_from_slice(&EK_RSA_MODULUS_BITS.to_be_bytes());
        out.extend_from_slice(&0u32.to_be_bytes());
        // TPMU_PUBLIC_ID: TPM2B_PUBLIC_KEY_RSA, the modulus with its length before it, padded
        // to the key's size the way the TPM carries a fixed-width modulus.
        let modulus = left_padded(&self.rsa_modulus, EK_RSA_MODULUS_BYTES);
        out.extend_from_slice(&tpm2b(&modulus));
        out
    }

    /// The P-384 key as that same `TPMT_PUBLIC`, with the ECC parameters of the endorsement
    /// key template: the same symmetric algorithm, no signing scheme, the NIST P-384 curve and
    /// no key derivation scheme, over this key's own point as a `TPMS_ECC_POINT`.
    fn ecc_tpmt_public(&self) -> Vec<u8> {
        let mut out = tpmt_public_header(TPM_ALG_ECC);
        out.extend_from_slice(&TPM_SYM_DEF_OBJECT_AES_128_CFB);
        out.extend_from_slice(&TPM_ALG_NULL.to_be_bytes());
        out.extend_from_slice(&TPM_ECC_NIST_P384.to_be_bytes());
        out.extend_from_slice(&TPM_ALG_NULL.to_be_bytes());
        // TPMS_ECC_POINT: the two coordinates of the SEC1 uncompressed point, each with its
        // length before it. A point this store did not mint is reported as the zero point
        // rather than as a truncated structure.
        let coordinates = self.ecc_point.get(1..).unwrap_or_default();
        let (x, y) = coordinates.split_at(coordinates.len().min(EK_ECC_COORDINATE_BYTES));
        out.extend_from_slice(&tpm2b(&left_padded(x, EK_ECC_COORDINATE_BYTES)));
        out.extend_from_slice(&tpm2b(&left_padded(y, EK_ECC_COORDINATE_BYTES)));
        out
    }
}

/// `TPM_ALG_RSA`, the `TPMI_ALG_PUBLIC` of an RSA key (TPM 2.0 Library Part 2, "TPM_ALG_ID").
const TPM_ALG_RSA: u16 = 0x0001;

/// `TPM_ALG_ECC`, the same for an elliptic-curve key.
const TPM_ALG_ECC: u16 = 0x0023;

/// `TPM_ALG_SHA256`, the `nameAlg` the published endorsement key template names.
const TPM_ALG_SHA256: u16 = 0x000B;

/// `TPM_ALG_NULL`, which a template uses where it names no algorithm at all.
const TPM_ALG_NULL: u16 = 0x0010;

/// `TPM_ALG_AES` and `TPM_ALG_CFB` with the 128-bit key size between them: the
/// `TPMT_SYM_DEF_OBJECT` of the published endorsement key template, written once because both
/// key types carry it.
const TPM_SYM_DEF_OBJECT_AES_128_CFB: [u8; 6] = [0x00, 0x06, 0x00, 0x80, 0x00, 0x43];

/// `TPM_ECC_NIST_P384`, the `TPMI_ECC_CURVE` of the P-384 endorsement key.
const TPM_ECC_NIST_P384: u16 = 0x0004;

/// The `TPMA_OBJECT` attributes of an endorsement key: `fixedTPM`, `fixedParent`,
/// `sensitiveDataOrigin`, `adminWithPolicy`, `restricted` and `decrypt` (TCG EK Credential
/// Profile, the EK templates).
const EK_OBJECT_ATTRIBUTES: u32 = 0x0003_00B2;

/// The SHA-256 `authPolicy` digest the published endorsement key templates carry (TCG EK
/// Credential Profile, "Policy A"), which is what makes the reported structure one a TPM tool
/// recognises as an EK rather than an unconstrained key.
const EK_AUTH_POLICY_SHA256: [u8; 32] = [
    0x83, 0x71, 0x97, 0x67, 0x44, 0x84, 0xb3, 0xf8, 0x1a, 0x90, 0xcc, 0x8d, 0x46, 0xa5, 0xd7, 0x24,
    0xfd, 0x52, 0xd7, 0x6e, 0x06, 0x52, 0x0b, 0x64, 0xf2, 0xa1, 0xda, 0x1b, 0x33, 0x14, 0x69, 0xaa,
];

/// The `type`, `nameAlg`, `objectAttributes` and `authPolicy` every `TPMT_PUBLIC` of an
/// endorsement key opens with, for the key algorithm `algorithm`.
fn tpmt_public_header(algorithm: u16) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&algorithm.to_be_bytes());
    out.extend_from_slice(&TPM_ALG_SHA256.to_be_bytes());
    out.extend_from_slice(&EK_OBJECT_ATTRIBUTES.to_be_bytes());
    out.extend_from_slice(&tpm2b(&EK_AUTH_POLICY_SHA256));
    out
}

/// `bytes` as a TPM `TPM2B_…` field: its length as a big-endian 16-bit count, then the bytes.
fn tpm2b(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 2);
    let length = u16::try_from(bytes.len()).unwrap_or(u16::MAX);
    out.extend_from_slice(&length.to_be_bytes());
    out.extend_from_slice(&bytes[..usize::from(length)]);
    out
}

/// `bytes` as a `width`-byte big-endian value: a value shorter than `width` carries leading
/// zeros, and one that is already that wide is unchanged.
fn left_padded(bytes: &[u8], width: usize) -> Vec<u8> {
    if bytes.len() >= width {
        return bytes.to_vec();
    }
    let mut out = vec![0u8; width - bytes.len()];
    out.extend_from_slice(bytes);
    out
}

/// A fresh P-384 key's point, SEC1 uncompressed, or `None` when no key of that curve could be
/// generated.
///
/// From the same generator the RSA half comes from — the one this service generates every real
/// key with — so the endorsement key of either type is real key material rather than bytes
/// assembled here. Only the public point is kept; the private half is dropped with the pair.
fn generate_ecc_p384_point() -> Option<Vec<u8>> {
    use ::aws_lc_rs::signature::KeyPair;
    let generated = ::aws_lc_rs::signature::EcdsaKeyPair::generate(
        &::aws_lc_rs::signature::ECDSA_P384_SHA384_ASN1_SIGNING,
    )
    .ok()?;
    let point = generated.public_key().as_ref().to_vec();
    // The X9.62 UNCOMPRESSED form, which is what a `TPMS_ECC_POINT` and a
    // `SubjectPublicKeyInfo` of this curve both carry: the `0x04` tag and the two
    // [`EK_ECC_COORDINATE_BYTES`] coordinates. A point in any other form is not one this
    // module can report, so it is refused rather than reported as a key.
    if point.len() != 1 + EK_ECC_COORDINATE_BYTES * 2 || point.first() != Some(&0x04) {
        return None;
    }
    Some(point)
}

/// The OID of `id-ecPublicKey` (RFC 5480 section 2.1.1), the algorithm an elliptic-curve
/// public key's `SubjectPublicKeyInfo` names.
const ID_EC_PUBLIC_KEY: ::spki::ObjectIdentifier =
    ::spki::ObjectIdentifier::new_unwrap("1.2.840.10045.2.1");

/// The OID of the `secp384r1` curve (RFC 5480 section 2.1.1.1), which that algorithm carries
/// as its parameters for a P-384 key.
const SECP384R1: ::spki::ObjectIdentifier = ::spki::ObjectIdentifier::new_unwrap("1.3.132.0.34");

impl ComAmazonawsEc2Amazonec2State {
    /// Whether this store already holds the endorsement keys of the instance `instance_id`.
    ///
    /// Asked before the keys are MINTED, which is work a handler does with this store's lock
    /// released ([`Self::record_instance_endorsement_keys`] is where the result is written),
    /// so a first ask does not hold every other caller behind a key generation.
    pub fn holds_instance_endorsement_keys(&self, instance_id: &str) -> bool {
        self.instance_endorsement_keys.contains_key(instance_id)
    }

    /// The endorsement keys the instance `instance_id` holds, or `None` for an instance
    /// nothing has minted them for yet.
    ///
    /// A pure READ: an instance that is only asked about gains no keys, so the minting is the
    /// writer's ([`Self::record_instance_endorsement_keys`]) and never a read's.
    pub fn instance_endorsement_keys(
        &self,
        instance_id: &str,
    ) -> Option<&InstanceEndorsementKeys> {
        self.instance_endorsement_keys.get(instance_id)
    }

    /// Record `keys` as the endorsement keys of the instance `instance_id`, unless this store
    /// already holds that instance's keys — in which case the keys it HOLDS stay, and `keys`
    /// is dropped.
    ///
    /// NitroTPM provisions the endorsement key when the instance is created and it never
    /// changes while the instance exists, so the first keys recorded for an instance are its
    /// keys for good: two requests that minted a pair each (the generation runs with this
    /// store's lock released) still leave every reader answering ONE key per instance.
    ///
    /// The map is bounded the way the other per-instance stores of this crate are: the keys
    /// of instances this store no longer holds go first, so launch-and-terminate cycles
    /// cannot accumulate them, and what is kept is charged to the crate's one byte budget so
    /// the bytes it holds are accounted for like every other stored record's.
    pub fn record_instance_endorsement_keys(
        &mut self,
        instance_id: &str,
        keys: InstanceEndorsementKeys,
    ) {
        let orphaned: Vec<String> = self
            .instance_endorsement_keys
            .keys()
            .filter(|held| !self.instances.contains_key(*held))
            .cloned()
            .collect();
        for held in orphaned {
            if let Some(dropped) = self.instance_endorsement_keys.remove(&held) {
                self.release_stored_bytes(dropped.payload_bytes());
            }
        }
        if self.instance_endorsement_keys.contains_key(instance_id) {
            return;
        }
        self.add_stored_bytes(keys.payload_bytes());
        self.instance_endorsement_keys
            .insert(instance_id.to_string(), keys);
    }

    /// Whether the instance `instance` has a NitroTPM at all: the device is presented to a
    /// guest only where the INSTANCE TYPE offers it — "indicates whether NitroTPM is
    /// supported" (`InstanceTypeInfo$NitroTpmSupport`), read off the one projection of an
    /// instance type this crate has, so this and `DescribeInstanceTypes` cannot disagree about
    /// one type — and the AMI the instance was launched from is "configured for NitroTPM
    /// support" (`Image$TpmSupport`).
    pub fn instance_has_nitro_tpm(&self, instance: &InstanceRecord) -> bool {
        let type_supports = instance_type_info(&instance.instance_type).is_some_and(|info| {
            info.nitro_tpm_support.as_deref() == Some(NITRO_TPM_SUPPORT_SUPPORTED)
        });
        let image_supports = instance
            .image_id
            .as_deref()
            .and_then(|image_id| self.images.get(image_id))
            .is_some_and(|image| image.tpm_support.is_some());
        type_supports && image_supports
    }
}
