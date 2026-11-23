// SPDX-License-Identifier: CC0-1.0

//! The KEY MATERIAL a public key carries, read by the crates that own its formats.
//!
//! `PublicKeyConfig$EncodedKey` is "the public key that you can use with signed URLs and
//! signed cookies, or with field-level encryption" — a PEM-enveloped RSA PUBLIC key, which is
//! what `openssl rsa -pubout` writes. So the material a caller submits is DECODED here, once,
//! and what decides whether it is such a key is the decode succeeding: the PEM envelope is
//! split by the crate that owns that format, the structure inside it by the one that owns
//! `SubjectPublicKeyInfo`, and the RSA key itself by the one that owns PKCS#1 — no scan of the
//! text, and nothing but an RSA public key gets through.
//!
//! The one reading, for both of the things this service does with the material: the create
//! that ADMITS it ([`is_rsa_public_key`]) and the signature verification that USES it
//! ([`rsa_public_key_bits`]) read the same bytes the same way, so a key the control plane
//! stored is a key the edge can verify a signed URL against.

/// The RSA public key `encoded_key` carries, as the PKCS#1 `RSAPublicKey` DER a verifier
/// takes, or `None` when the material is not an RSA public key at all.
///
/// The envelope is split by the crate that owns the PEM format and the structure inside it
/// decoded by the one that owns `SubjectPublicKeyInfo`, whose algorithm has to be RSA: a
/// `SubjectPublicKeyInfo` of another algorithm carries no RSA key, and neither does any other
/// structure — a PRIVATE key's `PrivateKeyInfo` included, which is a different structure
/// under a different label. The bits inside are then decoded as the RSA public key they claim
/// to be, so material whose envelope and header are right but whose key is not a readable RSA
/// key is no more an RSA public key than material with no envelope at all.
pub fn rsa_public_key_bits(encoded_key: &str) -> Option<Vec<u8>> {
    decoded_rsa_public_key(encoded_key).map(|decoded| decoded.pkcs1_der)
}

/// The same RSA public key as the X.509 `SubjectPublicKeyInfo` DER an ENCRYPTION takes — the
/// structure the PEM envelope carries as its own contents — or `None` when the material is not
/// an RSA public key at all.
///
/// The same single reading as [`rsa_public_key_bits`], handed over in the other encoding: the
/// field-level encryption of a request body encrypts under this key
/// (`crate::service::edge::field_level_encryption`), so the material a create ADMITTED is the
/// material an encryption uses.
pub fn rsa_public_key_spki_der(encoded_key: &str) -> Option<Vec<u8>> {
    decoded_rsa_public_key(encoded_key).map(|decoded| decoded.spki_der)
}

/// One decoded `EncodedKey`, in the two encodings this crate's uses of a public key take.
struct DecodedRsaPublicKey {
    /// The X.509 `SubjectPublicKeyInfo` DER the PEM envelope carried.
    spki_der: Vec<u8>,
    /// The PKCS#1 `RSAPublicKey` DER inside it.
    pkcs1_der: Vec<u8>,
}

/// `encoded_key` decoded, or `None` when it is not an RSA public key at all.
fn decoded_rsa_public_key(encoded_key: &str) -> Option<DecodedRsaPublicKey> {
    use ::x509_cert::der::Decode as _;

    let block = ::pem::parse(encoded_key).ok()?;
    let info = ::x509_cert::spki::SubjectPublicKeyInfoRef::from_der(block.contents()).ok()?;
    if info.algorithm.oid != RSA_ENCRYPTION_OID {
        return None;
    }
    let bits = info.subject_public_key.raw_bytes();
    ::pkcs1::RsaPublicKey::from_der(bits).ok()?;
    Some(DecodedRsaPublicKey {
        spki_der: block.contents().to_vec(),
        pkcs1_der: bits.to_vec(),
    })
}

/// Whether `encoded_key` is the RSA public key `EncodedKey` is documented to carry.
///
/// The decode itself is the answer ([`rsa_public_key_bits`]): material that does not decode
/// as an RSA public key — a private key's PEM, a certificate, a key of another algorithm,
/// base64 that is not a key, or text that is no PEM at all — is not one.
pub fn is_rsa_public_key(encoded_key: &str) -> bool {
    rsa_public_key_bits(encoded_key).is_some()
}

/// The algorithm an RSA key's `SubjectPublicKeyInfo` names: `rsaEncryption`
/// (`1.2.840.113549.1.1.1`, PKCS#1), the one algorithm `EncodedKey` carries a key of.
const RSA_ENCRYPTION_OID: ::x509_cert::der::asn1::ObjectIdentifier =
    ::x509_cert::der::asn1::ObjectIdentifier::new_unwrap("1.2.840.113549.1.1.1");

/// The modelled `InvalidArgument` (HTTP 400) for `EncodedKey` material that is not an RSA
/// public key.
///
/// FIXED service text ([`INVALID_ENCODED_KEY_MESSAGE`]): what the refusal states is the RULE
/// the member broke, so none of the material a caller submitted belongs in it — which matters
/// most here, the material of a rejected upload being whatever the caller pasted.
pub fn invalid_encoded_key() -> crate::ops::types::InvalidArgument {
    crate::ops::types::InvalidArgument {
        message: Some(INVALID_ENCODED_KEY_MESSAGE.to_string()),
    }
}

/// The sentence CloudFront refuses `EncodedKey` material that is not an RSA public key with,
/// on the wire: it states the condition of the member and carries nothing of the request.
pub const INVALID_ENCODED_KEY_MESSAGE: &str =
    "Your request contains empty/invalid/out of limits RSA Encoded Key";
