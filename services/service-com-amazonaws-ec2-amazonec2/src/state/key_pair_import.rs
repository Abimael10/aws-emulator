// SPDX-License-Identifier: CC0-1.0

//! The PUBLIC key material an `ImportKeyPair` carries: what counts as a key EC2 can take,
//! which `KeyType` it is, and the fingerprint EC2 publishes for an IMPORTED key of that
//! type.
//!
//! "Imports the public key from an RSA or ED25519 key pair that you created using a
//! third-party tool. You give Amazon Web Services only the public key. The private key is
//! never transferred between you and Amazon Web Services" (`ImportKeyPair`) — so nothing
//! here reads, derives or holds a private key, and the two algorithms that sentence names
//! are the only ones a key can be read as.
//!
//! The material is read by [`ssh_key`], the crate that implements the OpenSSH key formats,
//! rather than by matching on the text: what counts as a key is the format itself. Both
//! public-key forms the import requirements name are read — the `key-type key-body [comment]`
//! line `ssh-keygen` writes and the armoured "SSH public key file format as specified in RFC
//! 4716" — the material carries ONE key in one of them and nothing else, and which form a
//! key may arrive in is the ALGORITHM's: an ED25519 public key is read in the OpenSSH form,
//! which is the form EC2 names when it reports this algorithm's material unreadable, while
//! the armoured file carries the RSA keys whose lengths the requirements publish beside it.
//!
//! What is STORED out of the material is the KEY: "the public key material"
//! (`KeyPairInfo$PublicKey`) a later read reports is that key's one-line OpenSSH form under
//! the key pair's own name, written by [`super::key_pair_material::openssh_public_key_line`]
//! — so one key pair's public key has one spelling whichever form it was imported in and
//! whatever comment the file it came out of carried.
//!
//! An IMPORTED key pair's fingerprint rule is not the GENERATED one
//! ([`super::key_pair_material`], which fingerprints the private key EC2 made): "if you used
//! `ImportKeyPair` to provide Amazon Web Services the public key: for RSA key pairs, the key
//! fingerprint is the MD5 public key fingerprint as specified in section 4 of RFC4716. For
//! ED25519 key pairs, the key fingerprint is the base64-encoded SHA-256 digest, which is the
//! default for OpenSSH" (`KeyPairInfo$KeyFingerprint`). The STORE both kinds of key pair are
//! kept in is [`super::key_pairs`].
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every
//! other primitive there.

use ::md5::Digest as _;

use super::{
    base64_sha256_fingerprint, colon_separated_hex, openssh_public_key_line, KEY_TYPE_ED25519,
    KEY_TYPE_RSA,
};

/// The RSA key lengths an import admits, in bits: "supported lengths: 1024, 2048, and 4096"
/// (Amazon EC2 User Guide, "Create a key pair and import the public key to Amazon EC2" —
/// the key requirements `ImportKeyPair` points a caller at).
pub const IMPORTABLE_RSA_KEY_BITS: [usize; 3] = [1024, 2048, 4096];

/// The most bytes of public key material an import reads.
///
/// The longest key those requirements admit is a 4096-bit RSA key, whose OpenSSH line is
/// under a kilobyte and whose armoured file is not much longer, so this is well above every
/// key that can be imported and bounds the work a caller can ask the reader for. Material
/// above it is not a key in a supported format ([`read_public_key_material`]).
pub const MAX_PUBLIC_KEY_MATERIAL_BYTES: usize = 4096;

/// The code EC2 answers for public key material it cannot import: "the format of the public
/// key you are attempting to upload is not valid" — the `InvalidKey.Format` family, which
/// names the KEY rather than the key pair. Codes are compared exactly, so the
/// `InvalidKeyPair.*` spelling of the surrounding key-pair codes is a different code to every
/// client and is not what an import answers for unreadable material.
pub const INVALID_KEY_FORMAT: &str = "InvalidKey.Format";

/// Refuse an import whose material is not a public key EC2 can take: HTTP 400
/// [`INVALID_KEY_FORMAT`].
///
/// ONE sentence for every way the material fails [`read_public_key_material`] — not a key at
/// all, a key of an algorithm an import does not take, an RSA key of a length the
/// requirements do not list, material longer than any of them — because what EC2 reports is
/// that the key it was given is not one it can read: the sentence names neither which clause
/// failed nor the material itself, and carries no variable part.
pub fn invalid_key_format<E>() -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::service_error(
        INVALID_KEY_FORMAT,
        400,
        UNREADABLE_PUBLIC_KEY_MESSAGE,
    )
}

/// EC2's sentence for public key material it cannot read as a key.
pub const UNREADABLE_PUBLIC_KEY_MESSAGE: &str = "Key is not in valid OpenSSH public key format";

/// A public key an import can take: the form it is stored as, the `KeyType` it reports, and
/// the fingerprint EC2 publishes for an imported key of that type.
#[derive(Debug, Clone)]
pub struct ImportedPublicKey {
    /// "The public key material" (`KeyPairInfo$PublicKey`): the key the material carried, as
    /// the one-line OpenSSH public key commented with the key PAIR's name
    /// ([`openssh_public_key_line`]) — the algorithm and the body are the imported key's own,
    /// whichever of the forms it arrived in, and the comment is the name the pair is held
    /// under rather than whatever the file it was read out of happened to carry.
    pub public_key: String,
    /// The pair's `KeyType`: [`KEY_TYPE_RSA`] or [`KEY_TYPE_ED25519`], a word out of the
    /// modelled catalog rather than anything the request said.
    pub key_type: &'static str,
    /// The pair's `KeyFingerprint`, as [`imported_key_fingerprint`] derives it from the key
    /// material and from nothing else.
    pub key_fingerprint: String,
}

/// The public key `material` carries, or `None` when the material is not one an import can
/// take.
///
/// Exactly two algorithms are read, because `ImportKeyPair` imports "the public key from an
/// RSA or ED25519 key pair", and the key must be a real key OF that algorithm: an RSA key of
/// one of the lengths the requirements list ([`IMPORTABLE_RSA_KEY_BITS`]) and with a modulus
/// and exponent an RSA key has, an ED25519 key whose bytes encode a point on that curve.
/// Which FORM each is read in is the algorithm's too ([`PublicKeyForm`]). Anything else —
/// material that is not UTF-8, is longer than [`MAX_PUBLIC_KEY_MATERIAL_BYTES`], is no key at
/// all, is more than one key or a key with anything else beside it ([`parse_public_key`]), is
/// a key of another algorithm, or carries bytes of the right shape that are not a key of its
/// own algorithm — is not a key this operation can take, and its caller answers
/// [`invalid_key_format`].
///
/// Only the END of the material is trimmed, so the key begins at its first byte: the line
/// terminator the file a caller read the key out of ended with is not part of the key, while
/// anything BEFORE it — a blank line, an indent, a line of prose — leaves material that is not
/// one key in one of those forms.
pub fn read_public_key_material(material: &[u8], key_name: &str) -> Option<ImportedPublicKey> {
    if material.len() > MAX_PUBLIC_KEY_MATERIAL_BYTES {
        return None;
    }
    let text = ::std::str::from_utf8(material).ok()?;
    let (form, key) = parse_public_key(text.trim_end())?;
    let key_type = match key.key_data() {
        ::ssh_key::public::KeyData::Rsa(rsa) => {
            if !IMPORTABLE_RSA_KEY_BITS.contains(&integer_bits(rsa.n.as_positive_bytes()?))
                || !is_rsa_public_key(rsa)?
            {
                return None;
            }
            KEY_TYPE_RSA
        }
        ::ssh_key::public::KeyData::Ed25519(ed25519) => {
            // An ED25519 public key is read in the OpenSSH form: recorded behaviour is that
            // EC2 refuses this algorithm's key presented in the armoured file, in the
            // sentence that names the form it does read ([`UNREADABLE_PUBLIC_KEY_MESSAGE`]),
            // while the armoured file the requirements name carries the RSA keys whose
            // lengths they publish beside it.
            if form != PublicKeyForm::OpenSshLine {
                return None;
            }
            // The 32 bytes have to BE a public key of that curve — the encoding of a point
            // on it — and not merely 32 bytes in the place one goes: a key body a caller
            // mangled keeps its length and its algorithm name while standing for no key at
            // all, and a pair whose fingerprint stood for that could never be used to reach
            // anything.
            ::ed25519_dalek::VerifyingKey::from_bytes(&ed25519.0).ok()?;
            KEY_TYPE_ED25519
        }
        // Every other algorithm the OpenSSH formats carry (ECDSA, DSA, the security-key
        // variants): a readable key, but not one of the two an import takes.
        _ => return None,
    };
    Some(ImportedPublicKey {
        // The imported KEY, under the name the pair is held as: EC2 reports a stored public
        // key as the OpenSSH line of the key it holds ([`openssh_public_key_line`]), so the
        // armoured file and the line of one key are reported the same way and the comment is
        // the pair's name.
        public_key: openssh_public_key_line(&key, key_name)?,
        key_type,
        key_fingerprint: imported_key_fingerprint(&key)?,
    })
}

/// The `KeyFingerprint` EC2 publishes for the IMPORTED key `key`.
///
/// * RSA — "the MD5 public key fingerprint as specified in section 4 of RFC4716"
///   (`KeyPairInfo$KeyFingerprint`): the MD5 digest of the key's DER
///   `SubjectPublicKeyInfo`, as the colon-separated lowercase hex pairs EC2 publishes a hex
///   fingerprint in. That is the value `openssl rsa -in key -pubout -outform DER | openssl
///   md5 -c` prints, which is how the User Guide has a caller verify an imported RSA pair's
///   fingerprint.
/// * ED25519 — "the base64-encoded SHA-256 digest, which is the default for OpenSSH": that
///   digest, base64-encoded ([`base64_sha256_fingerprint`]), which is also the rendering a
///   generated Ed25519 key pair's fingerprint takes ([`super::key_pair_material`]) — one
///   sentence of the model, one rendering.
///
/// Both are computed from the key material ALONE, so one key always fingerprints to one
/// string and every later read of the pair answers that one value.
fn imported_key_fingerprint(key: &::ssh_key::PublicKey) -> Option<String> {
    match key.key_data() {
        ::ssh_key::public::KeyData::Rsa(rsa) => Some(colon_separated_hex(&::md5::Md5::digest(
            rsa_subject_public_key_info(rsa)?,
        ))),
        _ => Some(base64_sha256_fingerprint(key)),
    }
}

/// The DER `SubjectPublicKeyInfo` of the RSA public key `rsa` — the X.509 public key
/// `openssl rsa -pubout -outform DER` writes, which an imported RSA pair's MD5 fingerprint
/// is taken over.
///
/// The structure is written by the crates that implement the standards it comes from: the
/// PKCS#1 `RSAPublicKey` body by [`pkcs1`] (RFC 8017), under the `rsaEncryption` algorithm
/// identifier that crate publishes, and the `SubjectPublicKeyInfo` that carries it by
/// [`spki`] (RFC 5280).
fn rsa_subject_public_key_info(rsa: &::ssh_key::public::RsaPublicKey) -> Option<Vec<u8>> {
    use ::pkcs1::der::Encode as _;

    let body = ::pkcs1::RsaPublicKey {
        modulus: ::pkcs1::der::asn1::UintRef::new(rsa.n.as_positive_bytes()?).ok()?,
        public_exponent: ::pkcs1::der::asn1::UintRef::new(rsa.e.as_positive_bytes()?).ok()?,
    }
    .to_der()
    .ok()?;
    ::spki::SubjectPublicKeyInfo {
        algorithm: ::spki::AlgorithmIdentifier {
            oid: ::pkcs1::ALGORITHM_OID,
            parameters: Some(::pkcs1::der::asn1::AnyRef::NULL),
        },
        subject_public_key: ::pkcs1::der::asn1::BitStringRef::new(0, &body).ok()?,
    }
    .to_der()
    .ok()
}

/// Whether the RSA public key `rsa` carries the integers an RSA public key does: a modulus
/// that is odd, under a public exponent that is odd and greater than one.
///
/// A necessary condition rather than a primality proof — the modulus of a real RSA key is the
/// product of two odd primes, so it is odd, and this rejects the integers that cannot be one
/// without factoring anything. `None` only when the key's integers cannot be read at all. The
/// counterpart of the point check an ED25519 key goes through in [`read_public_key_material`],
/// which is a full decode: bytes of the right shape in the right place are not yet a key of
/// the algorithm that names them.
fn is_rsa_public_key(rsa: &::ssh_key::public::RsaPublicKey) -> Option<bool> {
    let modulus = rsa.n.as_positive_bytes()?;
    let exponent = rsa.e.as_positive_bytes()?;
    let odd = |integer: &[u8]| integer.last().is_some_and(|last| last % 2 == 1);
    Some(odd(modulus) && odd(exponent) && integer_bits(exponent) > 1)
}

/// How many bits the big-endian unsigned integer `bytes` holds — an RSA modulus's key
/// length, which is what the import requirements list.
fn integer_bits(bytes: &[u8]) -> usize {
    let Some(leading) = bytes.iter().position(|byte| *byte != 0) else {
        return 0;
    };
    let Some(top) = bytes.get(leading) else {
        return 0;
    };
    bytes
        .len()
        .saturating_sub(leading)
        .saturating_mul(8)
        .saturating_sub(top.leading_zeros() as usize)
}

/// The opening line of the armoured public key file format RFC 4716 defines, which the
/// import requirements name as a supported form.
const RFC4716_BEGIN: &str = "---- BEGIN SSH2 PUBLIC KEY ----";

/// Its closing line.
const RFC4716_END: &str = "---- END SSH2 PUBLIC KEY ----";

/// The form the material presented its key in — which of the two the import requirements name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicKeyForm {
    /// The single `key-type key-body [comment]` line `ssh-keygen` writes, the form an
    /// `authorized_keys` entry is.
    OpenSshLine,
    /// The armoured "SSH public key file format as specified in RFC 4716": the begin and end
    /// lines around optional headers and the key's base64 body.
    Rfc4716File,
}

/// `text` as ONE public key, with the form it came in: the single-line OpenSSH form, or the
/// RFC 4716 armoured file.
///
/// The material carries one key and nothing else. The OpenSSH form is a single LINE —
/// `key-type key-body [comment]`, the line `ssh-keygen` writes and `authorized_keys` holds
/// one of per line — so material whose key is followed by anything on a line of its own (a
/// second key, a fingerprint line, prose pasted after it) is not that form. The armoured file
/// is the multi-line form, and it is read only between its own begin and end lines.
fn parse_public_key(text: &str) -> Option<(PublicKeyForm, ::ssh_key::PublicKey)> {
    if is_single_line(text) {
        return Some((
            PublicKeyForm::OpenSshLine,
            ::ssh_key::PublicKey::from_openssh(text).ok()?,
        ));
    }
    Some((
        PublicKeyForm::Rfc4716File,
        ::ssh_key::PublicKey::from_bytes(&rfc4716_body(text)?).ok()?,
    ))
}

/// Whether `text` is one line: it carries no line break of any of the three spellings a
/// caller's platform writes one in (LF, CR, CRLF).
fn is_single_line(text: &str) -> bool {
    !text.contains(['\n', '\r'])
}

/// The binary key blob an RFC 4716 armoured public key file carries, or `None` when `text`
/// is not one.
///
/// The file is the begin and end lines around header fields and the base64 body (RFC 4716
/// §3.2): a header is a `Name: value` line, continued while a line ends in a backslash, and
/// the base64 alphabet carries no colon, so a body line is never read as a header. A flat
/// loop over at most [`MAX_PUBLIC_KEY_MATERIAL_BYTES`] of text — no recursion, and no work
/// proportional to anything but the material's own length.
fn rfc4716_body(text: &str) -> Option<Vec<u8>> {
    use ::base64::Engine as _;

    let inner = text
        .strip_prefix(RFC4716_BEGIN)?
        .trim_end()
        .strip_suffix(RFC4716_END)?;
    let mut body = String::new();
    let mut continued = false;
    for line in inner.lines().map(str::trim).filter(|line| !line.is_empty()) {
        if continued || line.contains(':') {
            continued = line.ends_with('\\');
            continue;
        }
        body.push_str(line);
    }
    ::base64::engine::general_purpose::STANDARD
        .decode(body)
        .ok()
}
