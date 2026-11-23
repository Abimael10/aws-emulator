// SPDX-License-Identifier: CC0-1.0

//! The key-agreement half of this service: the elliptic curve key pair behind a KMS key
//! whose `KeyUsage` is `KEY_AGREEMENT`, and the primitive `DeriveSharedSecret` runs over
//! it and a peer's public key.
//!
//! One concern of [`super`]'s store, in its own file: the helpers here are reached as
//! `crate::state::…` exactly as they were before, because the parent re-exports them.
//!
//! A key of a key-agreement spec holds an ordinary elliptic curve key pair — the same
//! material a signing key of that spec holds — so nothing new is minted for this concern:
//! the private half is the PKCS#8 one [`KeyMaterial::private_key_pkcs8`] already holds and
//! the public half is the `SubjectPublicKeyInfo` `GetPublicKey` already publishes. What
//! this file adds is the ONE direction KMS runs with them: "the Elliptic Curve
//! Cryptography Cofactor Diffie-Hellman Primitive (ECDH)"
//! (`smithy_query(query="doc",shape="DeriveSharedSecret")`), whose output is the raw
//! agreed secret and not a key derived from it — "KMS recommends that you follow NIST
//! recommendations for key derivation when using the raw shared secret to derive a
//! symmetric key", which is the CALLER's step, not this one.
//!
//! The primitive itself comes from the maintained curve libraries (`p256`, `p384`, `p521`
//! and `sm2`, driven through `elliptic_curve::ecdh`), so what runs here is the agreement
//! those libraries implement over the two halves; this file composes no arithmetic of its
//! own. Because the primitive is a function of the two key pairs alone, two agreements
//! over the same pair of keys are identical, and the two peers reach the same secret from
//! opposite halves — which is what makes the result a shared secret rather than a value
//! this store made up.

use super::*;

/// The only `KeyAgreementAlgorithmSpec` the model declares
/// (`smithy_query(shape="KeyAgreementAlgorithmSpec")`): "The only valid value is ECDH"
/// (`smithy_query(query="doc",shape="DeriveSharedSecretRequest",member="KeyAgreementAlgorithm")`).
pub const KEY_AGREEMENT_ALGORITHM_ECDH: &str = "ECDH";

/// Why no shared secret could be derived for a request whose key and algorithm are
/// otherwise right for the operation.
#[derive(Debug)]
pub enum KeyAgreementRefusal {
    /// The key holds no private key material to agree with — a key whose material was
    /// imported and has since been deleted — or no library reachable from here runs the
    /// agreement over its curve. Either way there is no shared secret to answer with, and
    /// the caller is told the key is unavailable rather than handed bytes that are not the
    /// secret its peer computes.
    Unsupported,
    /// The submitted bytes are not a public key this key pair agrees with: not a
    /// DER-encoded X.509 `SubjectPublicKeyInfo`, not an elliptic curve public key, or one
    /// on a curve other than this key pair's own.
    PeerPublicKey,
    /// The library implements the agreement and refused this particular one, with the
    /// request already judged against every rule the primitive states. Nothing about the
    /// request explains it, so it is a server fault rather than a refusal.
    Failed,
}

/// The raw shared secret the cofactor ECDH primitive derives from the PRIVATE half of
/// `key`'s key pair and the peer public key `peer_public_key`, which is "a DER-encoded
/// X.509 public key, also known as SubjectPublicKeyInfo (SPKI), as defined in RFC 5280"
/// (`smithy_query(query="doc",shape="DeriveSharedSecretRequest",member="PublicKey")`).
///
/// "Both key pairs must be on the same elliptic curve"
/// (`smithy_query(query="doc",shape="DeriveSharedSecret")`), so the curve is the one
/// `key`'s own spec names and a public key on any other curve is refused rather than
/// agreed with: it decides which library runs the primitive, and that library then
/// rejects an encoding that does not name that same curve.
///
/// The bytes are the primitive's own output — the x coordinate of the agreed point, as
/// many bytes as the curve's field — so a peer running the agreement from the other two
/// halves reaches exactly these bytes. The private key is only read here, never copied
/// out.
pub fn derive_shared_secret(
    key: &Cmk,
    peer_public_key: &[u8],
) -> Result<Vec<u8>, KeyAgreementRefusal> {
    let Some(private_key) = key.material.private_key_pkcs8() else {
        return Err(KeyAgreementRefusal::Unsupported);
    };

    // Every `KeySpec` whose profile names a key-agreement algorithm, with the curve its
    // key pair is on: the three NIST-standard curves and the SM2 curve of GM/T 0003-2012
    // (`KEY_SPEC_PROFILES`, whose `key_agreement_algorithms` is that table). A spec that
    // names none never reaches here — its key cannot carry `KEY_AGREEMENT` at all.
    match key.key_spec.as_str() {
        "ECC_NIST_P256" => agree::<::p256::NistP256>(private_key, peer_public_key),
        "ECC_NIST_P384" => agree::<::p384::NistP384>(private_key, peer_public_key),
        "ECC_NIST_P521" => agree::<::p521::NistP521>(private_key, peer_public_key),
        "SM2" => agree::<::sm2::Sm2>(private_key, peer_public_key),
        _ => Err(KeyAgreementRefusal::Unsupported),
    }
}

/// One run of the cofactor ECDH primitive on the curve `C`, between the private key
/// `private_key_pkcs8` (a DER-encoded PKCS#8 `PrivateKeyInfo`) and the public key
/// `peer_public_key` (a DER-encoded X.509 `SubjectPublicKeyInfo`).
///
/// Both halves are decoded by the curve's own library, which is what makes a public key
/// naming another curve a refusal rather than an agreement on the wrong curve.
fn agree<C>(
    private_key_pkcs8: &[u8],
    peer_public_key: &[u8],
) -> Result<Vec<u8>, KeyAgreementRefusal>
where
    C: ::elliptic_curve::CurveArithmetic
        + ::elliptic_curve::point::PointCompression
        + ::elliptic_curve::pkcs8::AssociatedOid,
    C::AffinePoint: ::elliptic_curve::sec1::FromSec1Point<C>
        + ::elliptic_curve::sec1::ToSec1Point<C>,
    C::FieldBytesSize: ::elliptic_curve::sec1::ModulusSize,
{
    use ::elliptic_curve::pkcs8::DecodePrivateKey;
    use ::spki::DecodePublicKey;

    // The private half is this store's own material, so bytes it cannot decode are a
    // server fault rather than anything about the request.
    let secret = ::elliptic_curve::SecretKey::<C>::from_pkcs8_der(private_key_pkcs8)
        .map_err(|_| KeyAgreementRefusal::Failed)?;
    let peer = ::elliptic_curve::PublicKey::<C>::from_public_key_der(peer_public_key)
        .map_err(|_| KeyAgreementRefusal::PeerPublicKey)?;
    let shared_secret =
        ::elliptic_curve::ecdh::diffie_hellman(secret.to_nonzero_scalar(), peer.as_affine());
    Ok(shared_secret.raw_secret_bytes().to_vec())
}

/// How a request is refused when the public key it submits is not one the named key pair
/// agrees with ([`KeyAgreementRefusal::PeerPublicKey`]).
///
/// The message states the requirement the submitted bytes fail: the encoding the member
/// is documented to carry, and the curve the key pair is on — "both key pairs must be on
/// the same elliptic curve" (`smithy_query(query="doc",shape="DeriveSharedSecret")`) —
/// with the key's own spec read from the key.
pub fn invalid_peer_public_key_message(key_spec: &str) -> String {
    format!(
        "The public key must be a DER-encoded X.509 public key (SubjectPublicKeyInfo) on \
         the same elliptic curve as the KMS key, whose KeySpec is {key_spec}."
    )
}
