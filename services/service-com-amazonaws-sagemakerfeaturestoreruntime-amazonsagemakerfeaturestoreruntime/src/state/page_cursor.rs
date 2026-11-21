// SPDX-License-Identifier: CC0-1.0

//! The page cursor `ListRecords` resumes a listing at, and the token it travels as.
//!
//! A token this service hands back comes STRAIGHT BACK as the next request's `NextToken`,
//! so it has to be inside the maximum the model declares for the member it is serialized
//! into: `ListRecordsNextToken` is `@length 1..8192` on the response member and on the
//! request member alike (`smithy_query(query="shape", shape="ListRecordsNextToken")`),
//! while the identifier a page resumes at is a `ValueAsString`, bounded only at
//! `@length 0..358400`. A token that carried the resume identifier whole would therefore
//! grow with it and cross its own maximum, and the boundary validator would refuse the
//! very token this service minted — dead-ending a listing whose caller did nothing wrong.
//!
//! So a cursor is encoded in a form whose length is bounded no matter how long the
//! identifier it resumes at. An identifier short enough to fit travels WHOLE, which is
//! what makes a cursor name an identifier and not a position. A longer one travels as its
//! leading characters plus the digest of the whole identifier and how many records of that
//! prefix the page had already passed: the resumed walk starts at the prefix, recognises
//! the record the digest names wherever it now sits among the records sharing that prefix,
//! and falls back on the count when that record is gone — so the page boundary survives
//! records being added or removed between calls just as the whole form does.

use ::sha2::{Digest, Sha256};

use super::{FeatureGroupStore, StoredRecord};

/// The modelled maximum length of a `ListRecords` page token.
///
/// `ListRecordsRequest$NextToken` and `ListRecordsResponse$NextToken` both target
/// `ListRecordsNextToken`, modelled `@length 1..8192`, so this one bound governs both the
/// token this service mints and the token a client resumes with.
pub const MAX_NEXT_TOKEN_LENGTH: usize = 8192;

/// What one token's payload may hold: a token is the hex of its payload, two characters
/// per byte.
const MAX_PAYLOAD_BYTES: usize = MAX_NEXT_TOKEN_LENGTH / 2;

/// The characters the per-process key takes in a payload (a `u64` as 16 hex digits).
const KEY_LENGTH: usize = 16;

/// The characters a digest takes in a payload (SHA-256 as hex).
const DIGEST_LENGTH: usize = 64;

/// The most characters a skipped count can take: `usize::MAX` in decimal.
const MAX_SKIPPED_LENGTH: usize = 20;

/// The field marking a payload that carries the resume identifier WHOLE.
const WHOLE_FORM: &str = "w";

/// The field marking a payload that carries only the identifier's leading characters.
const TRUNCATED_FORM: &str = "t";

/// The longest identifier a token carries whole: the payload is `<key>:w:<identifier>`,
/// whose three other fields take `KEY_LENGTH + 3` characters.
const MAX_WHOLE_IDENTIFIER_BYTES: usize = MAX_PAYLOAD_BYTES - KEY_LENGTH - 3;

/// The longest prefix a truncated cursor carries: the payload is
/// `<key>:t:<skipped>:<digest>:<prefix>`, whose other fields are each bounded above.
const MAX_PREFIX_BYTES: usize =
    MAX_PAYLOAD_BYTES - KEY_LENGTH - 5 - MAX_SKIPPED_LENGTH - DIGEST_LENGTH;

/// The arithmetic above, checked where it cannot go stale: whichever form a cursor takes,
/// every field of its payload fits one payload's worth of bytes, so the hex of that
/// payload is inside the modelled maximum. A bound that holds by construction needs no
/// check on the request path.
const _: () = assert!(
    KEY_LENGTH + 3 + MAX_WHOLE_IDENTIFIER_BYTES <= MAX_PAYLOAD_BYTES
        && KEY_LENGTH + 5 + MAX_SKIPPED_LENGTH + DIGEST_LENGTH + MAX_PREFIX_BYTES
            <= MAX_PAYLOAD_BYTES
);

/// Where a page of record identifiers resumes.
///
/// Built by the store itself ([`FeatureGroupStore::page_identifiers`]) and read back from
/// a token with [`PageCursor::decode`]; nothing else constructs one, so every cursor in
/// flight is one this service minted for a record it holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageCursor {
    /// Where the ordered store is RESUMED: the identifier the next page starts at, or its
    /// leading characters when the identifier is too long for a token to carry whole.
    resume_at: String,
    /// What names the identifier when [`Self::resume_at`] is only its prefix.
    truncated: Option<TruncatedIdentifier>,
}

/// The bounded stand-in for an identifier a token cannot carry whole.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TruncatedIdentifier {
    /// The digest of the WHOLE identifier, so the resumed walk recognises the record the
    /// cursor names wherever it sorts among the records that share the prefix.
    digest: String,
    /// How many listable records of that prefix the page had already passed: where the
    /// walk resumes when the record the digest names is no longer there.
    skipped: usize,
}

impl PageCursor {
    /// Where the ordered store is resumed — the whole identifier, or its prefix.
    pub fn resume_at(&self) -> &str {
        &self.resume_at
    }

    /// The opaque token a client resumes this cursor with.
    ///
    /// Hex of the per-process key and the cursor's own fields: opaque to a caller (the
    /// modelled `NextToken` promises nothing about its content), carrying no server state,
    /// and — because the only unbounded field is cut to [`MAX_PREFIX_BYTES`] — never longer
    /// than [`MAX_NEXT_TOKEN_LENGTH`], whatever the identifier it resumes at.
    pub fn encode(&self) -> String {
        let key = page_token_key();
        let payload = match &self.truncated {
            None => format!("{key:016x}:{WHOLE_FORM}:{}", self.resume_at),
            Some(truncated) => format!(
                "{key:016x}:{TRUNCATED_FORM}:{}:{}:{}",
                truncated.skipped, truncated.digest, self.resume_at
            ),
        };
        ::hex::encode(payload.as_bytes())
    }

    /// The cursor `token` names, or `None` when it is not a token this service minted.
    pub fn decode(token: &str) -> Option<Self> {
        if token.len() > MAX_NEXT_TOKEN_LENGTH {
            return None;
        }
        let payload = String::from_utf8(::hex::decode(token).ok()?).ok()?;
        let fields = payload.strip_prefix(&format!("{:016x}:", page_token_key()))?;
        match fields.split_once(':')? {
            (WHOLE_FORM, identifier) => Some(Self {
                resume_at: identifier.to_owned(),
                truncated: None,
            }),
            (TRUNCATED_FORM, rest) => {
                // The prefix is the LAST field, so a prefix holding the separator itself
                // still arrives whole.
                let (skipped, rest) = rest.split_once(':')?;
                let (digest, prefix) = rest.split_once(':')?;
                let skipped: usize = skipped.parse().ok()?;
                let well_formed = digest.len() == DIGEST_LENGTH
                    && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && !prefix.is_empty();
                well_formed.then(|| Self {
                    resume_at: prefix.to_owned(),
                    truncated: Some(TruncatedIdentifier {
                        digest: digest.to_owned(),
                        skipped,
                    }),
                })
            }
            _ => None,
        }
    }
}

impl FeatureGroupStore {
    /// One page of this group's record identifiers: at most `page_size` of them from
    /// `cursor` on, and the cursor the NEXT page resumes at when the group holds more.
    ///
    /// Soft-deleted records are listed only when the caller asks for them, and the cursor
    /// handed back names the first record this page did NOT return, so the pages of a
    /// listing are the group's records in ascending identifier order, each once.
    pub fn page_identifiers(
        &self,
        cursor: Option<&PageCursor>,
        include_soft_deleted: bool,
        page_size: usize,
    ) -> (Vec<String>, Option<PageCursor>) {
        let skipped = cursor.map_or(0, |cursor| self.records_to_skip(cursor, include_soft_deleted));
        let mut remaining = self
            .records_from(cursor.map(PageCursor::resume_at))
            .filter(|record| listable(record, include_soft_deleted))
            .skip(skipped);
        let page: Vec<String> = remaining
            .by_ref()
            .take(page_size)
            .map(|record| record.identifier.clone())
            .collect();
        let next = remaining.next().map(|record| record.identifier.clone());
        (
            page,
            next.map(|identifier| self.cursor_at(&identifier, include_soft_deleted)),
        )
    }

    /// The cursor for the identifier a page stopped at.
    ///
    /// An identifier a token can carry whole becomes the resume point itself; a longer one
    /// is named by its prefix, its digest and how many listable records of that prefix
    /// sort before it. Only the records sharing the prefix are counted: every identifier at
    /// or after the prefix and below this one CARRIES that prefix, so the count is not
    /// linear in the store.
    fn cursor_at(&self, identifier: &str, include_soft_deleted: bool) -> PageCursor {
        if identifier.len() <= MAX_WHOLE_IDENTIFIER_BYTES {
            return PageCursor {
                resume_at: identifier.to_owned(),
                truncated: None,
            };
        }
        let prefix = leading_characters(identifier, MAX_PREFIX_BYTES);
        let skipped = self
            .records_from(Some(prefix))
            .take_while(|record| record.identifier.as_str() < identifier)
            .filter(|record| listable(record, include_soft_deleted))
            .count();
        PageCursor {
            resume_at: prefix.to_owned(),
            truncated: Some(TruncatedIdentifier {
                digest: identifier_digest(identifier),
                skipped,
            }),
        }
    }

    /// How many records a resumed walk steps over before the record `cursor` names.
    ///
    /// None for a cursor carrying its identifier whole: the walk simply starts there. For
    /// a truncated one, the record the digest names wherever it now sorts among the
    /// records sharing the prefix — and the count the cursor carries when that record is
    /// no longer there, which is where the page stopped.
    ///
    /// The search reaches only the records that CARRY the prefix, of which one group holds
    /// at most [`super::MAX_RECORDS_PER_GROUP`], and only for a cursor whose identifier a
    /// token cannot carry whole: a listing of identifiers short enough to travel in a
    /// token digests nothing at all.
    fn records_to_skip(&self, cursor: &PageCursor, include_soft_deleted: bool) -> usize {
        let Some(truncated) = &cursor.truncated else {
            return 0;
        };
        self.records_from(Some(&cursor.resume_at))
            .take_while(|record| record.identifier.starts_with(&cursor.resume_at))
            .filter(|record| listable(record, include_soft_deleted))
            .position(|record| identifier_digest(&record.identifier) == truncated.digest)
            .unwrap_or(truncated.skipped)
    }
}

/// Whether a listing serves this record: a soft-deleted record only when the caller asked
/// for it ("If set to true, the result includes records that have been soft deleted").
fn listable(record: &StoredRecord, include_soft_deleted: bool) -> bool {
    include_soft_deleted || !record.soft_deleted
}

/// `identifier` cut to at most `max_bytes`, at a character boundary.
///
/// The cut is in bytes and lands on a boundary, so the prefix is still valid UTF-8 and is
/// inside the bound whether the token's length is counted in bytes or in characters.
fn leading_characters(identifier: &str, max_bytes: usize) -> &str {
    if identifier.len() <= max_bytes {
        return identifier;
    }
    let cut = (0..=max_bytes)
        .rev()
        .find(|at| identifier.is_char_boundary(*at))
        .unwrap_or_default();
    &identifier[..cut]
}

/// The digest that names one whole record identifier inside a truncated cursor, from the
/// maintained SHA-256 implementation.
fn identifier_digest(identifier: &str) -> String {
    ::hex::encode(Sha256::digest(identifier.as_bytes()))
}

/// The per-process key every page token this service issues carries.
///
/// Derived once at first use from `RandomState`, which the standard library seeds randomly
/// per process, so a caller cannot mint a cursor: only a token this emulator issued
/// decodes. It is only a cursor — no authorization decision rides on it — so a 64-bit
/// marker is enough, and it deliberately does not survive a restart, exactly as a page
/// token should not.
fn page_token_key() -> u64 {
    static KEY: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
    *KEY.get_or_init(|| {
        use std::hash::{BuildHasher, Hasher};
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write(b"ListRecords page token");
        hasher.finish()
    })
}
