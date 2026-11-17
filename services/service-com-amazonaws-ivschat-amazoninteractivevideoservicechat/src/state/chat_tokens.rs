// SPDX-License-Identifier: CC0-1.0

//! What a chat token IS: the session `CreateChatToken` issues it for, and the encrypted
//! value a participant is handed to carry that session.
//!
//! `CreateChatToken` "creates an encrypted token that is used by a chat participant to
//! establish an individual WebSocket chat connection to a room", answers it as
//! `ChatToken` — "the issued client token, encrypted" — and says of the session's
//! attributes that they are "application-provided attributes to encode into the token and
//! attach to a chat session", with "encryption keys ... owned by Amazon IVS Chat and never
//! used directly by your application"
//! (`smithy_query(query="doc",shape="CreateChatTokenRequest",member="attributes")`). The
//! shape itself states no form at all (`ChatToken` is a `@sensitive` string with no
//! `@length` and no `@pattern`), so what the value has to be is what those sentences say:
//! the session's own claims sealed under a key the service alone holds, and therefore an
//! opaque, high-entropy blob whose size follows the claims it carries.
//!
//! That is what this module mints: a payload carrying the session and a per-token
//! identifier, sealed with the maintained `aes-gcm` AEAD under a key drawn once per
//! process, rendered as base64 ([`seal_chat_token`]). [`chat_token_claims`] is the inverse
//! — the session a token carries — which is what distinguishes genuinely encrypted material
//! from a digest or a formatted sequence number, neither of which anything can be read back
//! out of. No part of the value is a function of how many tokens the store has minted, so
//! one token says nothing about the next.

use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

/// The nonce a token's envelope carries, in bytes: the width AES-GCM is specified at.
const CHAT_TOKEN_NONCE_LEN: usize = 12;

/// The authentication tag AES-GCM appends to the ciphertext, in bytes.
const CHAT_TOKEN_TAG_LEN: usize = 16;

/// The payload layout a token is sealed under, so a value sealed under a layout this build
/// does not know is not read as though it were this one.
const CHAT_TOKEN_PAYLOAD_LAYOUT: u64 = 1;

/// How many bytes of fresh entropy each token's own identifier carries.
///
/// The rest of the payload is the session the request asked for, so without it two mints
/// for one session would seal the same plaintext; with it, no token can be derived from the
/// request that asked for it.
const CHAT_TOKEN_ID_BYTES: usize = 32;

/// How many times a mint re-draws when the value it sealed is one the store already holds.
///
/// A collision needs two mints to agree on [`CHAT_TOKEN_ID_BYTES`] of entropy AND on the
/// nonce, so this never runs twice in practice; it is what makes "unique within the store"
/// a property of the store rather than an assumption about the draw.
const CHAT_TOKEN_MINT_ATTEMPTS: usize = 8;

/// Fresh random bytes, for a value that must be unpredictable per call: the key this
/// process seals under, each envelope's nonce, and each token's own identifier.
///
/// Drawn from the `rand` crate's thread generator, which the operating system seeds. A chat
/// token is the bearer of a chat session, so it may not be predictable from another token,
/// from the request that asked for it, or from how many tokens this store has minted.
fn fresh_entropy<const N: usize>() -> [u8; N] {
    use ::rand::RngCore as _;

    let mut bytes = [0_u8; N];
    ::rand::rng().fill_bytes(&mut bytes);
    bytes
}

/// The AEAD a chat token's payload is sealed with, keyed for the lifetime of the process.
///
/// The 256-bit key is drawn once from [`fresh_entropy`] and never leaves this module: it is
/// in no response, no log and no file, which is the emulator's stand-in for keys the service
/// "owns ... and never used directly by your application". The AEAD is the maintained
/// `aes-gcm` implementation; nothing here hand-rolls one.
fn chat_token_cipher() -> &'static ::aes_gcm::Aes256Gcm {
    use ::aes_gcm::KeyInit;

    static CIPHER: ::std::sync::OnceLock<::aes_gcm::Aes256Gcm> = ::std::sync::OnceLock::new();
    CIPHER.get_or_init(|| {
        let secret = fresh_entropy::<32>();
        ::aes_gcm::Aes256Gcm::new(::aes_gcm::Key::<::aes_gcm::Aes256Gcm>::from_slice(&secret))
    })
}

/// The chat session a token stands for: the claims `CreateChatToken` sealed into the token,
/// and the record the store holds beside it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatSession {
    /// The `RoomID` of the room the token admits its holder to.
    pub room_id: String,
    /// The `UserID` the request named for this participant.
    pub user_id: String,
    /// The members of the modelled `ChatTokenCapability` enum the request asked for, in the
    /// order it listed them.
    pub capabilities: Vec<String>,
    /// The attributes the request asked to "encode into the token and attach to a chat
    /// session".
    pub attributes: BTreeMap<String, String>,
    /// When the token stops being usable for opening a connection.
    pub token_expiration_time: SystemTime,
    /// When the session the token opens stops being valid.
    pub session_expiration_time: SystemTime,
}

/// A chat token minted by `CreateChatToken` against a room: the value handed to the
/// participant, and the session it was issued for.
#[derive(Debug, Clone)]
pub struct ChatToken {
    /// The `ChatToken` value the response carried, as [`seal_chat_token`] rendered it.
    pub token: String,
    /// The session sealed inside that value.
    pub session: ChatSession,
}

/// `time` as milliseconds since the Unix epoch, which is how a claim records an instant:
/// the precision this service reports a `Time` member at (`super::as_date_time`), so a
/// sealed instant round-trips exactly as the response's own timestamp does.
fn epoch_millis(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| i64::try_from(since.as_millis()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// The instant `millis` milliseconds after the Unix epoch names, for an instant read back
/// out of a sealed payload.
fn instant_of(millis: i64) -> SystemTime {
    u64::try_from(millis).map_or(SystemTime::UNIX_EPOCH, |since| {
        SystemTime::UNIX_EPOCH + Duration::from_millis(since)
    })
}

/// `time` with sub-millisecond precision dropped: the resolution this service reports a
/// `Time` member at, and therefore the resolution a session's instants are recorded at — so
/// a stored record and the claims sealed into its token carry the same instant, and both
/// carry the one the response reported.
pub fn at_reported_precision(time: SystemTime) -> SystemTime {
    instant_of(epoch_millis(time))
}

/// The bytes a token's envelope seals: the payload layout, this token's own identifier, and
/// the session it was issued for.
///
/// Written as a JSON document with `serde_json` — the crate this service already reads and
/// writes JSON with — so the payload has one reader and one writer and neither hand-rolls a
/// format.
fn chat_token_payload(session: &ChatSession) -> Vec<u8> {
    ::serde_json::json!({
        "layout": CHAT_TOKEN_PAYLOAD_LAYOUT,
        "id": ::hex::encode(fresh_entropy::<CHAT_TOKEN_ID_BYTES>()),
        "room": session.room_id,
        "user": session.user_id,
        "capabilities": session.capabilities,
        "attributes": session.attributes,
        "tokenExpirationMillis": epoch_millis(session.token_expiration_time),
        "sessionExpirationMillis": epoch_millis(session.session_expiration_time),
    })
    .to_string()
    .into_bytes()
}

/// A fresh chat token for `session`: its claims sealed under this process's key.
///
/// The value is a [`CHAT_TOKEN_NONCE_LEN`]-byte nonce drawn fresh for this token, the
/// ciphertext of [`chat_token_payload`] and its [`CHAT_TOKEN_TAG_LEN`]-byte tag, rendered as
/// base64 — so it is opaque encrypted material of the order the session it carries, with no
/// fixed width and no padding run, and two mints for one session are two different values.
/// `None` only when the AEAD refuses the payload, which for a payload of this size it does
/// not.
pub fn seal_chat_token(session: &ChatSession) -> Option<String> {
    use ::aes_gcm::aead::Aead;

    let nonce = fresh_entropy::<CHAT_TOKEN_NONCE_LEN>();
    let sealed = chat_token_cipher()
        .encrypt(
            ::aes_gcm::Nonce::from_slice(&nonce),
            chat_token_payload(session).as_slice(),
        )
        .ok()?;
    let mut envelope: Vec<u8> = Vec::with_capacity(nonce.len() + sealed.len());
    envelope.extend_from_slice(&nonce);
    envelope.extend_from_slice(&sealed);
    Some(::aws_smithy_types::base64::encode(&envelope))
}

/// The session `token` carries, or `None` when it is not a token this process sealed: a
/// value that is not base64, one too short to hold an envelope at all, one sealed under
/// another payload layout, or one whose authentication tag does not hold.
///
/// This is the inverse of [`seal_chat_token`], and what makes the issued value genuinely
/// encrypted material: the session can be read back OUT of it, which is true of a token the
/// service encrypted and of nothing it merely formatted.
pub fn chat_token_claims(token: &str) -> Option<ChatSession> {
    use ::aes_gcm::aead::Aead;

    let envelope = ::aws_smithy_types::base64::decode(token).ok()?;
    if envelope.len() < CHAT_TOKEN_NONCE_LEN + CHAT_TOKEN_TAG_LEN {
        return None;
    }
    let (nonce, sealed) = envelope.split_at(CHAT_TOKEN_NONCE_LEN);
    let payload = chat_token_cipher()
        .decrypt(::aes_gcm::Nonce::from_slice(nonce), sealed)
        .ok()?;
    let document: ::serde_json::Value = ::serde_json::from_slice(&payload).ok()?;
    if document.get("layout").and_then(::serde_json::Value::as_u64)
        != Some(CHAT_TOKEN_PAYLOAD_LAYOUT)
    {
        return None;
    }
    let text = |member: &str| -> Option<String> {
        document
            .get(member)
            .and_then(::serde_json::Value::as_str)
            .map(str::to_string)
    };
    let millis = |member: &str| -> Option<SystemTime> {
        document
            .get(member)
            .and_then(::serde_json::Value::as_i64)
            .map(instant_of)
    };
    let capabilities = document
        .get("capabilities")?
        .as_array()?
        .iter()
        .map(|value| value.as_str().map(str::to_string))
        .collect::<Option<Vec<String>>>()?;
    let attributes = document
        .get("attributes")?
        .as_object()?
        .iter()
        .map(|(key, value)| value.as_str().map(|text| (key.clone(), text.to_string())))
        .collect::<Option<BTreeMap<String, String>>>()?;
    Some(ChatSession {
        room_id: text("room")?,
        user_id: text("user")?,
        capabilities,
        attributes,
        token_expiration_time: millis("tokenExpirationMillis")?,
        session_expiration_time: millis("sessionExpirationMillis")?,
    })
}

/// How many characters the shortest token this module mints carries, as
/// [`seal_chat_token`] renders it: the smallest envelope is the nonce, the tag and a payload
/// carrying at least this module's own fields, and base64 writes four characters per three
/// bytes.
///
/// Stated so a caller — this crate's own suites included — can say what "a long opaque blob"
/// means here without pinning the exact width, which the claims decide.
pub const CHAT_TOKEN_MIN_LENGTH: usize =
    (CHAT_TOKEN_NONCE_LEN + CHAT_TOKEN_TAG_LEN + CHAT_TOKEN_ID_BYTES * 2).div_ceil(3) * 4;

/// A fresh token for `session`, unique among the tokens `held` already contains.
///
/// Separate from [`seal_chat_token`] so the store's own uniqueness is checked where the
/// store is known, and the sealing stays independent of it.
pub fn fresh_chat_token(
    session: &ChatSession,
    held: &BTreeMap<String, ChatToken>,
) -> Option<String> {
    (0..CHAT_TOKEN_MINT_ATTEMPTS)
        .filter_map(|_| seal_chat_token(session))
        .find(|token| !held.contains_key(token))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A session, varied along what the token's size and content follow: the room, the user,
    /// how many capabilities, how many attributes and how long they are.
    fn session(user: &str, capabilities: &[&str], attributes: &[(&str, &str)]) -> ChatSession {
        // A claim records an instant at the millisecond precision this service's timestamps
        // carry, so the rows' own instants are whole milliseconds.
        let now = SystemTime::UNIX_EPOCH
            + Duration::from_millis(u64::try_from(epoch_millis(SystemTime::now())).unwrap_or(0));
        ChatSession {
            room_id: "abc123ABC456".to_string(),
            user_id: user.to_string(),
            capabilities: capabilities.iter().map(|value| (*value).to_string()).collect(),
            attributes: attributes
                .iter()
                .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
                .collect(),
            token_expiration_time: now + Duration::from_secs(60),
            session_expiration_time: now + Duration::from_secs(3_600),
        }
    }

    #[test]
    fn a_token_carries_the_session_it_was_sealed_for() {
        // Rows differ in the user id, the capability count and the number and size of the
        // attributes — everything the sealed claims carry.
        let rows = vec![
            ("no capabilities, no attributes", session("alice", &[], &[])),
            (
                "one capability",
                session("bob", &["SEND_MESSAGE"], &[("displayName", "Bob")]),
            ),
            (
                "two capabilities and several attributes",
                session(
                    "carol",
                    &["SEND_MESSAGE", "DISCONNECT_USER"],
                    &[("displayName", "Carol"), ("icon", "https://example/i.png")],
                ),
            ),
            (
                "a long user id and a long attribute value",
                session(
                    &"u".repeat(128),
                    &["DELETE_MESSAGE"],
                    &[("bio", &"x".repeat(400))],
                ),
            ),
        ];

        for (case, expected) in rows {
            let token = seal_chat_token(&expected)
                .unwrap_or_else(|| panic!("case {case:?}: sealing a session must answer a token"));
            let actual = chat_token_claims(&token).unwrap_or_else(|| {
                panic!("case {case:?}: the token must decrypt back to its session")
            });
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the sealed session back, got {actual:?}"
            );
        }
    }

    #[test]
    fn a_token_is_opaque_material_whose_size_follows_its_claims() {
        let small = session("alice", &[], &[]);
        let large = session(
            "alice",
            &["SEND_MESSAGE", "DISCONNECT_USER", "DELETE_MESSAGE"],
            &[("bio", &"x".repeat(600))],
        );

        for (case, claims) in [("the smallest session", &small), ("a large session", &large)] {
            let token = seal_chat_token(claims)
                .unwrap_or_else(|| panic!("case {case:?}: sealing must answer a token"));
            assert!(
                token.len() >= CHAT_TOKEN_MIN_LENGTH,
                "case {case:?}: expected at least {CHAT_TOKEN_MIN_LENGTH} characters, got {}",
                token.len()
            );
            assert!(
                token
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'/' | b'=')),
                "case {case:?}: expected a base64 rendering, got {token}"
            );
            let distinct: ::std::collections::BTreeSet<char> = token.chars().collect();
            assert!(
                distinct.len() >= 24,
                "case {case:?}: expected high-entropy material, got {} distinct characters in \
                 {token}",
                distinct.len()
            );
        }

        let smallest = seal_chat_token(&small).unwrap_or_else(|| panic!("sealing answers a token"));
        let largest = seal_chat_token(&large).unwrap_or_else(|| panic!("sealing answers a token"));
        assert!(
            largest.len() > smallest.len(),
            "case size follows the claims: expected the larger session's token to be longer \
             than {} characters, got {}",
            smallest.len(),
            largest.len()
        );
    }

    #[test]
    fn two_mints_for_one_session_share_no_prefix() {
        let claims = session("alice", &["SEND_MESSAGE"], &[]);
        let held = BTreeMap::new();
        let first = fresh_chat_token(&claims, &held).unwrap_or_else(|| panic!("a mint answers"));
        let second = fresh_chat_token(&claims, &held).unwrap_or_else(|| panic!("a mint answers"));
        assert_ne!(
            first, second,
            "two mints for one session must not answer the same token"
        );
        let shared = first
            .chars()
            .zip(second.chars())
            .take_while(|(left, right)| left == right)
            .count();
        assert!(
            shared < 8,
            "case consecutive mints: expected values that differ from their first characters, \
             got {shared} shared leading characters between {first} and {second}"
        );
    }

    #[test]
    fn a_value_this_process_did_not_seal_carries_no_session() {
        let claims = session("alice", &[], &[]);
        let token = seal_chat_token(&claims).unwrap_or_else(|| panic!("sealing answers a token"));
        // Rows differ in HOW the value is not a token: not base64, base64 but far too
        // short, a token with its last character changed, and a token with its first
        // character changed.
        let mut tampered_tail = token.clone();
        tampered_tail.pop();
        let rows = vec![
            ("not base64 at all", "not a token".to_string()),
            ("base64 of too few bytes", ::aws_smithy_types::base64::encode("short")),
            ("the empty string", String::new()),
            ("a truncated envelope", tampered_tail),
            (
                "a flipped leading character",
                format!(
                    "{}{}",
                    if token.starts_with('A') { 'B' } else { 'A' },
                    &token[1..]
                ),
            ),
        ];

        for (case, value) in rows {
            let actual = chat_token_claims(&value);
            assert!(
                actual.is_none(),
                "case {case:?}: expected no session, got {actual:?}"
            );
        }
        assert!(
            chat_token_claims(&token).is_some(),
            "case the token as sealed: it must still carry its session"
        );
    }

    #[test]
    fn a_mint_never_answers_a_token_the_store_already_holds() {
        let claims = session("alice", &[], &[]);
        let mut held = BTreeMap::new();
        for _ in 0..16 {
            let token = fresh_chat_token(&claims, &held)
                .unwrap_or_else(|| panic!("a mint answers while the store has room"));
            assert!(
                !held.contains_key(&token),
                "case a repeated mint: token {token} was issued twice"
            );
            held.insert(
                token.clone(),
                ChatToken {
                    token,
                    session: claims.clone(),
                },
            );
        }
    }
}
