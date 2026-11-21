// SPDX-License-Identifier: CC0-1.0

//! The ONE reading of the payload hash a SigV4-signed request DECLARES, and of whether that
//! hash covers the body the request carried.
//!
//! SigV4 signs a canonical request whose last line is the hash of the payload, so the
//! signature a caller presents is a signature over ONE payload and no other. A client that
//! streams its body — a contribution encoder pushing segments as it captures them — cannot
//! hash a payload it has not finished producing, so the hash it signs is one it DECLARES in
//! `x-amz-content-sha256`: either a marker for a payload the signature does not cover byte
//! for byte (`UNSIGNED-PAYLOAD`, the `STREAMING-…` forms) or the hex digest of the payload
//! it does. A front end that is handed such a push therefore reads the declared value to
//! know what the signature covers at all; it is the same value the emulator's own verifier
//! signs as written when it re-signs the request
//! (`emulator_server::request_authentication`).
//!
//! Two questions follow from that value, and both are asked here so no ingest plane writes
//! its own reading of them:
//!
//! * WHAT does the signature cover ([`DeclaredPayloadHash::of`])? A request that declares
//!   nothing — or declares a value that is neither a marker nor a digest — presents a
//!   signature over a payload nothing can name, which cannot be held to the request as
//!   presented.
//! * Does it cover the body that ARRIVED ([`DeclaredPayloadHash::covers`])? A declared
//!   digest is checkable against a buffered body, and a body it does not hash to is not the
//!   payload that was signed. A marker says the signature covers no byte-for-byte hash at
//!   all, so there is nothing to disagree with.
//!
//! Nothing here verifies a SIGNATURE: that is the emulator's own check, made with the
//! caller's secret, which no service holds. This reads only the request's own declaration
//! and its own body, so it decides nothing about who the caller is and writes nothing.

/// The header a request declares the payload hash its SigV4 signature covers in.
pub const CONTENT_SHA256_HEADER: &str = "x-amz-content-sha256";

/// How many characters a hex SHA-256 digest is written in.
const DIGEST_CHARS: usize = 64;

/// Every marker SigV4 defines for a payload a signature does not cover byte for byte, as AWS
/// writes them: the unsigned payload, and the chunked forms (each with and without the
/// trailer that carries the final digest) under both signing algorithms.
///
/// Source: the `x-amz-content-sha256` values the AWS Signature Version 4 documentation
/// defines for a streamed payload. The set is FINITE, so a value outside it declares no
/// marker however it is spelled.
const PAYLOAD_MARKERS: [&str; 6] = [
    "UNSIGNED-PAYLOAD",
    "STREAMING-UNSIGNED-PAYLOAD-TRAILER",
    "STREAMING-AWS4-HMAC-SHA256-PAYLOAD",
    "STREAMING-AWS4-HMAC-SHA256-PAYLOAD-TRAILER",
    "STREAMING-AWS4-ECDSA-P256-SHA256-PAYLOAD",
    "STREAMING-AWS4-ECDSA-P256-SHA256-PAYLOAD-TRAILER",
];

/// What a request says its signature covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeclaredPayloadHash {
    /// One of the markers SigV4 defines for a payload the signature does not cover byte for
    /// byte ([`PAYLOAD_MARKERS`]): the value is signed exactly as written, so what it names
    /// is a payload no recipient recomputes.
    Marker(String),
    /// The hex SHA-256 digest of the payload the signature covers.
    Digest(String),
}

impl DeclaredPayloadHash {
    /// What `declared` — the value of [`CONTENT_SHA256_HEADER`] — says the signature covers,
    /// or `None` when it says nothing a signature could be held to.
    ///
    /// A value of exactly [`DIGEST_CHARS`] hex characters is a digest; a value SigV4 defines
    /// as a marker ([`PAYLOAD_MARKERS`], matched without regard to case, since the case of a
    /// header value is not what names the marker) is that marker. Everything else — an
    /// absent header, an empty value, a truncated digest, a word that is neither — is no
    /// declaration at all.
    #[must_use]
    pub fn of(declared: Option<&str>) -> Option<Self> {
        let declared = declared?.trim();
        if declared.len() == DIGEST_CHARS && declared.chars().all(|c| c.is_ascii_hexdigit()) {
            return Some(Self::Digest(declared.to_ascii_lowercase()));
        }
        PAYLOAD_MARKERS
            .iter()
            .any(|marker| marker.eq_ignore_ascii_case(declared))
            .then(|| Self::Marker(declared.to_string()))
    }

    /// Whether what this declaration names is the `body` that arrived.
    ///
    /// A marker names no byte-for-byte hash, so every body is the one it covers. A digest
    /// names one payload exactly, so it covers `body` only when `body` hashes to it.
    #[must_use]
    pub fn covers(&self, body: &[u8]) -> bool {
        match self {
            Self::Marker(_) => true,
            Self::Digest(digest) => *digest == payload_digest(body),
        }
    }
}

/// The hex SHA-256 of `body` — what a caller that hashed its payload declares — through the
/// maintained digest and encoder this crate already computes one with.
///
/// Public because it is the value a declared digest is CHECKED against: whoever needs that
/// digest asks for it here rather than hashing a second time of its own.
#[must_use]
pub fn payload_digest(body: &[u8]) -> String {
    use sha2::Digest as _;
    let mut digest = sha2::Sha256::new();
    digest.update(body);
    hex::encode(digest.finalize())
}

#[cfg(test)]
mod tests {
    use super::{payload_digest, DeclaredPayloadHash, DIGEST_CHARS, PAYLOAD_MARKERS};

    /// The body every row declares a hash of.
    const BODY: &[u8] = b"one segment of a stream";

    /// Every marker SigV4 defines is read as one — including the forms this module's own
    /// rows below do not spell out — and each covers whatever body arrives.
    #[test]
    fn every_marker_sigv4_defines_is_read_as_one() {
        for marker in PAYLOAD_MARKERS {
            let read = DeclaredPayloadHash::of(Some(marker));
            assert_eq!(
                read,
                Some(DeclaredPayloadHash::Marker(marker.to_string())),
                "case {marker:?}: expected the value to be read as that marker, got {read:?}"
            );
            let covers = read.is_some_and(|read| read.covers(BODY));
            assert!(
                covers,
                "case {marker:?}: a marker names no byte-for-byte hash, so it covers the body \
                 that arrived"
            );
        }
    }

    /// What a declaration is read as, and whether it covers the body that arrived — over the
    /// set of values a client can send: the markers SigV4 defines, a digest of the body, a
    /// digest of something else, and the values that declare nothing.
    #[test]
    fn a_declaration_is_read_and_held_to_the_body_that_arrived() {
        let digest = payload_digest(BODY);
        // One character of the digest altered: still a digest, and not this body's.
        let mut altered: Vec<char> = digest.chars().collect();
        altered[0] = if altered[0] == 'a' { 'b' } else { 'a' };
        let altered: String = altered.into_iter().collect();
        let empty_body_digest = payload_digest(b"");

        // (case, the declared value, whether it reads as a declaration, whether it covers)
        let rows: Vec<(&str, Option<String>, bool, bool)> = vec![
            ("no header at all", None, false, false),
            ("an empty value", Some(String::new()), false, false),
            (
                "the unsigned-payload marker",
                Some("UNSIGNED-PAYLOAD".to_string()),
                true,
                true,
            ),
            (
                "a streaming marker",
                Some("STREAMING-AWS4-HMAC-SHA256-PAYLOAD".to_string()),
                true,
                true,
            ),
            ("this body's digest", Some(digest.clone()), true, true),
            (
                "the same digest in upper case",
                Some(digest.to_ascii_uppercase()),
                true,
                true,
            ),
            ("that digest with one character altered", Some(altered), true, false),
            (
                "the digest of a different payload",
                Some(empty_body_digest),
                true,
                false,
            ),
            (
                "a value that is neither a marker nor a digest",
                Some("sha256".to_string()),
                false,
                false,
            ),
            (
                // Shaped like a marker and not one of them: the set SigV4 defines is
                // finite, so this declares nothing rather than covering everything.
                "a hyphenated word that is not one of the markers",
                Some("STREAMING-SOMETHING-ELSE".to_string()),
                false,
                false,
            ),
            (
                "a truncated digest",
                Some(digest[..DIGEST_CHARS - 1].to_string()),
                false,
                false,
            ),
        ];

        for (case, declared, declares, covers) in rows {
            let read = DeclaredPayloadHash::of(declared.as_deref());
            assert_eq!(
                read.is_some(),
                declares,
                "case {case:?}: expected declares={declares}, got {read:?}"
            );
            let actual = read.as_ref().is_some_and(|read| read.covers(BODY));
            assert_eq!(
                actual, covers,
                "case {case:?}: expected covers={covers}, got {actual} from {read:?}"
            );
        }
    }
}
