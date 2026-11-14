// SPDX-License-Identifier: CC0-1.0

//! The id ONE response of this service names the request it answers by, and the FORM that
//! id takes on the wire.
//!
//! The form is the wire contract; the value is not. Every recorded response of this
//! endpoint — the `RequestID` member of a refusal's error document and the
//! `x-amzn-requestid` header beside it alike — names its request by a fixed-width
//! base64 token of [`ID_BYTES`] opaque bytes: [`ID_CHARACTERS`] characters of the standard
//! base64 alphabet, the last of which is the single `=` that padding an 11-byte value
//! needs. No client may depend on a particular VALUE (nothing can predict one and two
//! responses never share one), but anything that length-checks, parses, logs or quotes the
//! id reads its form, so a token of another width or another alphabet is a different
//! contract. The id is not modelled on any shape — no operation's output and no error shape
//! declares it (`smithy_query(query="shape",
//! shape="ValidationException,ResourceNotFoundException")` lists `exceptionMessage` and the
//! header-bound CORS members alone) — so it belongs to the RESPONSE, which is why it is
//! minted here, at the one envelope every response of this service passes through, rather
//! than by any handler.
//!
//! The encoding is the workspace's own base64 codec (`aws_smithy_types::base64`), which
//! this crate already reaches, rather than a second codec declared or hand-written for it.
//!
//! The bytes behind it are opaque: a digest of a draw on a monotonic counter, salted once
//! per process, so two responses of one process never name the same request and nothing
//! about the REQUEST — no caller value, no resource name, no credential — reaches the id.
//! It is a correlation value and never a secret: nothing authenticates with it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

/// How many bytes of opaque value one id carries.
///
/// 11 bytes is what the recorded form encodes: base64 renders them as 15 characters of
/// payload plus one `=` of padding, the [`ID_CHARACTERS`]-character token every recorded
/// response of this endpoint carries.
const ID_BYTES: usize = 11;

/// How many characters that encoding is wide — base64 of [`ID_BYTES`] bytes, padding
/// included. Stated so the module's own tests pin the width the wire contract fixes.
#[cfg(test)]
const ID_CHARACTERS: usize = 16;

/// How many ids this process has minted. A request id names ONE request, so two responses
/// may never carry the same one: the ids are derived from this monotonic counter rather
/// than from anything about the request, so two identical requests are still told apart.
static MINTED_IDS: AtomicU64 = AtomicU64::new(0);

/// The per-process salt mixed into every id, so the ids of one run are not the ids of the
/// next and nothing can predict the next id from the ones it has seen.
static PROCESS_SALT: OnceLock<String> = OnceLock::new();

/// That salt, drawn once from the `uuid`-backed random id newtype of `emulator_telemetry`
/// — a crate this service ALREADY depends on — rather than by declaring a second path to a
/// random source.
fn process_salt() -> &'static str {
    PROCESS_SALT.get_or_init(|| ::emulator_telemetry::BatchId::random().as_str().to_string())
}

/// Mint the id ONE response names the request it answers with.
///
/// A free function rather than a mint on the store: a failure the request boundary refused
/// never reaches a handler, so the envelope that answers it has no state to reach.
pub(super) fn new_request_id() -> String {
    use ::sha2::Digest;

    let drawn = MINTED_IDS.fetch_add(1, Ordering::Relaxed);
    let mut digest = ::sha2::Sha256::new();
    digest.update(process_salt().as_bytes());
    digest.update(b":");
    digest.update(drawn.to_be_bytes());
    let bytes = digest.finalize();
    ::aws_smithy_types::base64::encode(&bytes[..ID_BYTES])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The form every minted id takes: the recorded width, the standard base64 alphabet,
    /// the single trailing pad, and a payload that decodes back to [`ID_BYTES`] bytes.
    #[test]
    fn every_minted_id_takes_the_recorded_token_form() {
        for case in 0..64 {
            let id = new_request_id();
            assert_eq!(
                id.chars().count(),
                ID_CHARACTERS,
                "case {case}: expected a {ID_CHARACTERS}-character token, got {id:?}"
            );
            let (payload, padding) = id.split_at(ID_CHARACTERS - 1);
            assert_eq!(
                padding, "=",
                "case {case}: expected the token to end in one '=' pad, got {id:?}"
            );
            assert!(
                payload.bytes().all(|byte| byte.is_ascii_alphanumeric()
                    || byte == b'+'
                    || byte == b'/'),
                "case {case}: expected base64-alphabet characters, got {id:?}"
            );
            let decoded = ::aws_smithy_types::base64::decode(&id)
                .unwrap_or_else(|error| panic!("case {case}: {id:?} must decode: {error}"));
            assert_eq!(
                decoded.len(),
                ID_BYTES,
                "case {case}: expected {ID_BYTES} opaque bytes behind {id:?}, got {}",
                decoded.len()
            );
        }
    }

    /// Two responses never name the same request.
    #[test]
    fn no_two_minted_ids_name_the_same_request() {
        let minted: Vec<String> = (0..256).map(|_| new_request_id()).collect();
        let distinct: std::collections::BTreeSet<&String> = minted.iter().collect();
        assert_eq!(
            distinct.len(),
            minted.len(),
            "expected {} distinct ids, got {} (ids {minted:?})",
            minted.len(),
            distinct.len()
        );
    }
}
