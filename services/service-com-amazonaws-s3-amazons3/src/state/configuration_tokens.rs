// SPDX-License-Identifier: CC0-1.0

//! The ONE continuation token every PAGED listing of this service hands back, and the one
//! reading of a token a caller hands in.
//!
//! (The file keeps the name it was written under, when the four listings of a bucket's
//! id-addressed configurations were the only pagers that minted one; nothing but the module
//! path depends on it.)
//!
//! Each of those listings pages, and the token that resumes a truncated page is the
//! service's to mint and the caller's to hand back unread: "the continuation token is an
//! opaque value that Amazon S3 understands"
//! (`ListBucketMetricsConfigurationsOutput$NextContinuationToken`), "the token is
//! obfuscated and is not a usable value"
//! (`ListBucketAnalyticsConfigurationsOutput$NextContinuationToken`), "ContinuationToken
//! is obfuscated and is not a real bucket name"
//! (`ListDirectoryBucketsRequest$ContinuationToken`) and "NextContinuationToken is
//! obfuscated and is not a real key" (`ListObjectsV2Output$NextContinuationToken`). Two
//! things follow, and neither is served by rendering the cursor itself:
//!
//! * a token must not be a plain rendering of the position it resumes at — a value whose
//!   text IS a stored id invites a caller to compute the next one rather than round-trip
//!   what it was handed; and
//! * a value the caller did not receive from this service names no page of the listing, so
//!   it is refused ([`super::MALFORMED_CONTINUATION_TOKEN_CODE`]) rather than read as a
//!   cursor into a position the caller chose.
//!
//! So a token carries an AUTHENTICATOR over its cursor, computed under a secret the store
//! draws from the system's randomness and never reveals
//! ([`::service_common::page_tokens::PageTokenKey`], the workspace's one reading of that):
//! no caller can produce the authenticator for a cursor of its own, and a token minted by
//! another store — another process, a restarted one — names no page here either. The
//! cursor is SCOPED to the listing it was issued for, so a token from one listing is not a
//! cursor into another. Opaque, NOT confidential, exactly as that module
//! puts it: what a caller cannot do is make a token of its own.
//!
//! WHICH ALPHABET the authenticated bytes are written in is the listing's own, because it is
//! the one AWS writes that listing's token in ([`PagedListing::spelling`]): the bucket
//! listing and the four listings of a bucket's id-addressed configurations hand back a
//! lowercase-hex value, and the object listings and the directory-bucket listing hand back a
//! base64 one. A client round-trips either unread, so WHICH of the two a listing writes is a
//! fidelity question — which is why it is read off the recordings rather than chosen.
//!
//! What is NOT free in either alphabet is the CHARACTER SET. The value a caller hands back
//! travels on the `continuation-token` query parameter of a SIGNED request, and a character a
//! query component does not carry unchanged has two spellings on the wire — the character
//! itself and its percent-encoding — of which a request's SigV4 canonical form covers one: a
//! token carrying such a character resumes the listing for a client that encodes it and is
//! refused for the signature for a client that hands the value back verbatim, which "you can
//! use this ContinuationToken for the pagination of the list results"
//! (`ListDirectoryBucketsRequest$ContinuationToken`) gives a caller no way to tell apart. So
//! every token of this service is written in the characters a query component carries
//! unchanged (RFC 3986's unreserved set): lowercase hex already is, and the base64 one is
//! written in base64's URL-SAFE alphabet without padding (RFC 4648 §5) — the same base64
//! width, with `-` and `_` for `+` and `/` and no `=`.
//!
//! No page is kept on the side: the cursor is the position the next page resumes at — a
//! configuration id, a bucket name, an object key, an annotation name — which is why every
//! one of these listings walks what it reports in ascending order.

pub use ::service_common::page_tokens::PageTokenKey;

/// The listing a continuation token was issued for.
///
/// Named inside the token so a `NextContinuationToken` from one listing names no
/// page of another's: the four families a bucket holds per-id configurations of, each bound
/// at its own subresource (`?analytics`, `?metrics`, `?inventory`,
/// `?intelligent-tiering`), the account's buckets — the general purpose ones `ListBuckets`
/// reports and the DIRECTORY ones `ListDirectoryBuckets` reports on its own endpoint — the
/// objects a bucket holds, and the annotations one object version holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PagedListing {
    /// `ListBucketAnalyticsConfigurations`.
    Analytics,
    /// `ListBuckets`.
    Buckets,
    /// `ListDirectoryBuckets`.
    DirectoryBuckets,
    /// `ListBucketIntelligentTieringConfigurations`.
    IntelligentTiering,
    /// `ListBucketInventoryConfigurations`.
    Inventory,
    /// `ListBucketMetricsConfigurations`.
    Metrics,
    /// `ListObjectAnnotations`.
    ObjectAnnotations,
    /// `ListObjectsV2`.
    Objects,
}

impl PagedListing {
    /// The name this listing is authenticated under, inside the token. One name per listing
    /// and no part of any response — the subresource a configuration listing is bound at,
    /// and the resource the listing reports.
    fn label(self) -> &'static str {
        match self {
            Self::Analytics => "analytics",
            Self::Buckets => "buckets",
            Self::DirectoryBuckets => "directory-buckets",
            Self::IntelligentTiering => "intelligent-tiering",
            Self::Inventory => "inventory",
            Self::Metrics => "metrics",
            Self::ObjectAnnotations => "object-annotations",
            Self::Objects => "objects",
        }
    }

    /// The alphabet THIS listing's token is written in, which is the alphabet the recorded
    /// answers of that listing write it in: a lowercase-hex value for the account's own
    /// bucket listing and for the four listings of a bucket's id-addressed configurations, a
    /// base64 one for the listings of what a bucket or an object version holds and for the
    /// directory-bucket listing.
    ///
    /// The token is round-tripped unread either way — "an opaque value that Amazon S3
    /// understands" — so which of the two it is changes nothing a client can do with the
    /// value; it is what a client that reads the value's FORM, or a recorded-answer
    /// comparison, sees. Both are written in the characters a query component carries
    /// unchanged, which is NOT a free choice (the module docs).
    fn spelling(self) -> Spelling {
        match self {
            Self::Analytics
            | Self::Buckets
            | Self::IntelligentTiering
            | Self::Inventory
            | Self::Metrics => Spelling::Hex,
            Self::DirectoryBuckets | Self::ObjectAnnotations | Self::Objects => Spelling::Base64,
        }
    }
}

/// The alphabet a token's authenticated bytes are written in. See
/// [`PagedListing::spelling`] for which listing is written in which.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Spelling {
    /// Lowercase hexadecimal (RFC 4648 §8's alphabet, lowercased).
    Hex,
    /// Base64 in the URL-SAFE alphabet, unpadded (RFC 4648 §5, `A-Za-z0-9-_`): base64 of the
    /// same width as the standard alphabet's, written in the characters a URI query component
    /// carries unchanged, so the value a response hands out is the value a signed request can
    /// hand back (the module docs).
    Base64,
}

impl Spelling {
    /// `bytes` as the string a response carries.
    fn write(self, bytes: &[u8]) -> String {
        use ::base64::Engine as _;
        match self {
            Self::Hex => ::hex::encode(bytes),
            Self::Base64 => ::base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes),
        }
    }

    /// The bytes `token` carries, or `None` for a value that is not written in this
    /// alphabet at all — which is one of the ways a value this store never minted names no
    /// page.
    ///
    /// Each alphabet is read in ONE spelling, the one this store writes: a hex value is
    /// lowercase, so a token whose case a caller changed is a value this store did not
    /// hand out rather than the same bytes spelled differently.
    fn read(self, token: &str) -> Option<Vec<u8>> {
        match self {
            Self::Hex => {
                if !token
                    .chars()
                    .all(|character| matches!(character, '0'..='9' | 'a'..='f'))
                {
                    return None;
                }
                ::hex::decode(token).ok()
            }
            Self::Base64 => {
                use ::base64::Engine as _;
                ::base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(token).ok()
            }
        }
    }
}

/// The separator between the three parts of a cursor — the listing a token was issued for,
/// the resource it was issued AGAINST and the position in it: a unit separator.
///
/// The first two parts carry none of it: a listing's label is a fixed word and the resource
/// travels as a fixed-width digest ([`scope_digest`]). So the reading below never has to
/// guess where a part ends, and the POSITION — which for an object listing is a key, and a
/// key is arbitrary UTF-8 — is whatever follows the second separator, separator bytes
/// included.
const CURSOR_SEPARATOR: char = '\u{1f}';

/// The resource a cursor was issued against, as it travels inside the token: the SHA-256 of
/// the scope, in lowercase hex.
///
/// A fixed-width field of a character class the separator is not in, which is what makes a
/// cursor's parts unambiguous whatever the scope is written out of — a bucket name, or a
/// bucket, key and version id, where the key may carry any byte. It is also one less thing
/// the token carries in readable form: the reading compares the digest of the scope the
/// REQUEST addresses against it, which is all a cursor is asked about its resource.
fn scope_digest(scope: &str) -> String {
    use ::sha2::Digest as _;
    ::hex::encode(::sha2::Sha256::digest(scope.as_bytes()))
}

/// The scope a listing of what ONE OBJECT VERSION holds is taken over: the bucket, the key,
/// and the version the request addressed (empty for a request that addressed the key's
/// current object), so a token issued over one object names no page of another's listing.
///
/// Built here rather than at the call site, so the separator a cursor is laid out with stays
/// this module's own.
pub fn object_version_scope(bucket: &str, key: &str, version_id: Option<&str>) -> String {
    format!(
        "{bucket}{CURSOR_SEPARATOR}{key}{CURSOR_SEPARATOR}{}",
        version_id.unwrap_or_default()
    )
}

/// The `NextContinuationToken` that resumes a listing at `first_not_returned` (inclusive):
/// the authenticated bytes of that position, written in the alphabet the listing's recorded
/// tokens are written in ([`PagedListing::spelling`]).
///
/// The bytes are the workspace's one rendering of an authenticated cursor
/// ([`::service_common::page_tokens::PageTokenKey::mint`]), so a token of this service names
/// its position the way every other paginated read's does, and a client hands it back on the
/// `continuation-token` query parameter the way it hands back any other query value. The
/// authenticator inside is what makes a token this store did not mint detectable, which
/// is what lets the next page refuse it instead of serving from a position the caller chose.
///
/// `scope` is the resource the listing was taken OVER — the bucket a configuration listing is
/// bound to, and the empty string for a listing of the account itself — so a token issued for
/// one bucket names no page of the same listing on another.
pub fn encode_continuation_token(
    key: &PageTokenKey,
    listing: PagedListing,
    scope: &str,
    first_not_returned: &str,
) -> String {
    listing.spelling().write(&key.mint(&format!(
        "{}{CURSOR_SEPARATOR}{}{CURSOR_SEPARATOR}{first_not_returned}",
        listing.label(),
        scope_digest(scope)
    )))
}

/// The position `token` resumes `listing` over `scope` at (inclusive), or `None` for any
/// value this store did not mint for that listing and that scope: one that is not written in
/// the listing's own alphabet, one whose authenticator does not match the cursor it travels
/// with, one issued for another listing, one issued for another BUCKET, and one carrying no
/// cursor at all.
pub fn decode_continuation_token(
    key: &PageTokenKey,
    listing: PagedListing,
    scope: &str,
    token: &str,
) -> Option<String> {
    match read_continuation_token(key, listing, scope, token) {
        ContinuationTokenReading::Position(resume_at) => Some(resume_at),
        _ => None,
    }
}

/// What `token` is, read against the listing and the scope the request addresses.
///
/// Three outcomes, because S3 answers them differently: a token that names a position here
/// resumes the listing, a token this service issued over ANOTHER resource names a position
/// the listing cannot be resumed from, and a value this service never issued names no page at
/// all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContinuationTokenReading {
    /// The position the listing resumes at (inclusive).
    Position(String),
    /// A token this store minted for this listing, but over another scope — another bucket's
    /// listing of the same family.
    OtherScope,
    /// Not a token this store minted for this listing at all: not written in the listing's
    /// own alphabet, authenticated under another key, issued for another listing, or
    /// carrying no cursor.
    Unknown,
}

impl ContinuationTokenReading {
    /// The refusal a bucket-configuration listing answers an UNUSABLE reading with, as
    /// `(code, status, message)` — and the not-a-token refusal for a reading that names a
    /// position, which a caller matching `Position` first never reaches.
    ///
    /// Recorded behavior, and the two readings are answered DIFFERENTLY. A token the caller
    /// FORGED — one S3 never issued — names no page of the listing, which S3 reports against
    /// the token itself ([`super::MALFORMED_CONTINUATION_TOKEN_CODE`], HTTP 400, the input
    /// error a paginating client branches on). A token S3 DID issue, for another bucket's
    /// listing of the same family, is a cursor into a listing this request does not address:
    /// the recorded answer to one is [`super::INTERNAL_ERROR_CODE`] and its own published
    /// sentence, rather than the addressed bucket's first page or a page resolved against
    /// the addressed bucket's contents. The issue this was filed under reads the second case
    /// as the first; the recording is what is followed here.
    pub fn refusal(&self) -> (&'static str, u16, &'static str) {
        match self {
            Self::OtherScope => (
                super::INTERNAL_ERROR_CODE,
                super::INTERNAL_ERROR_STATUS,
                super::INTERNAL_ERROR_MESSAGE,
            ),
            Self::Position(_) | Self::Unknown => (
                super::MALFORMED_CONTINUATION_TOKEN_CODE,
                super::MALFORMED_CONTINUATION_TOKEN_STATUS,
                super::MALFORMED_CONTINUATION_TOKEN_MESSAGE,
            ),
        }
    }
}

/// Read `token` against `listing` and `scope`. See [`ContinuationTokenReading`].
pub fn read_continuation_token(
    key: &PageTokenKey,
    listing: PagedListing,
    scope: &str,
    token: &str,
) -> ContinuationTokenReading {
    let Some(bytes) = listing.spelling().read(token) else {
        return ContinuationTokenReading::Unknown;
    };
    let Some(cursor) = key.cursor_of(&bytes) else {
        return ContinuationTokenReading::Unknown;
    };
    let Some((label, rest)) = cursor.split_once(CURSOR_SEPARATOR) else {
        return ContinuationTokenReading::Unknown;
    };
    let Some((issued_against, resume_at)) = rest.split_once(CURSOR_SEPARATOR) else {
        return ContinuationTokenReading::Unknown;
    };
    if label != listing.label() {
        return ContinuationTokenReading::Unknown;
    }
    match issued_against == scope_digest(scope) {
        true => ContinuationTokenReading::Position(resume_at.to_string()),
        false => ContinuationTokenReading::OtherScope,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::MAX_CONFIGURATION_ID_CHARS;

    /// Every listing's token names its position under the key that minted it, for the scope
    /// it was issued against, and nothing else does: another store's key, another family's
    /// listing, another bucket, or a value a caller wrote.
    #[test]
    fn a_token_names_a_position_only_for_the_store_and_listing_that_issued_it() {
        let key = PageTokenKey::new();
        let other_store = PageTokenKey::new();
        let listings = [
            PagedListing::Analytics,
            PagedListing::Buckets,
            PagedListing::DirectoryBuckets,
            PagedListing::IntelligentTiering,
            PagedListing::Inventory,
            PagedListing::Metrics,
            PagedListing::ObjectAnnotations,
            PagedListing::Objects,
        ];
        // Scopes spanning what a listing is taken over: one bucket, the account itself, one
        // object version, and one whose key carries the separator a cursor is laid out with.
        let object = object_version_scope("a-bucket", "media/a.jpg", None);
        let separated = object_version_scope(
            "a-bucket",
            &format!("media{CURSOR_SEPARATOR}a.jpg"),
            Some("0123456789abcdef0123456789abcdef"),
        );
        let scopes = ["a-bucket", "", object.as_str(), separated.as_str()];
        // Cursors spanning what one can be: one character, every character class a
        // configuration id allows, one at the published ceiling, a directory bucket's name,
        // an object key with a path in it, and one carrying the separator a cursor is laid
        // out with — which an object key may.
        let positions = [
            "a".to_string(),
            "Report.one-2_3".to_string(),
            "z".repeat(MAX_CONFIGURATION_ID_CHARS),
            "base--usw2-az1--x-s3".to_string(),
            "media/april report.jpg".to_string(),
            format!("media{CURSOR_SEPARATOR}a.jpg"),
        ];
        let mut cases: Vec<(PagedListing, &str, &str)> = Vec::new();
        for listing in listings {
            for scope in scopes {
                for position in &positions {
                    cases.push((listing, scope, position.as_str()));
                }
            }
        }
        for (listing, scope, id) in cases {
            let token = encode_continuation_token(&key, listing, scope, id);
            assert_eq!(
                decode_continuation_token(&key, listing, scope, &token).as_deref(),
                Some(id),
                "case {listing:?}/{id:?}: a minted token must name its position, got {token:?}"
            );
            // The same token handed back VERBATIM on a query parameter: every character of
            // either alphabet is one a query component carries unchanged, so nothing has to
            // be repaired on the way in (the module docs).
            assert!(
                token
                    .chars()
                    .all(|character| character.is_ascii_alphanumeric()
                        || matches!(character, '-' | '.' | '_' | '~')),
                "case {listing:?}/{id:?}: a token is written in the characters a query \
                 component carries unchanged, got {token:?}"
            );
            assert_eq!(
                decode_continuation_token(&other_store, listing, scope, &token),
                None,
                "case {listing:?}/{id:?}: another store's key must name no position"
            );
            for elsewhere in scopes.iter().filter(|other| **other != scope) {
                assert_eq!(
                    decode_continuation_token(&key, listing, elsewhere, &token),
                    None,
                    "case {listing:?}/{id:?}: scope {elsewhere:?} must name no position"
                );
            }
            for elsewhere in listings.iter().filter(|other| **other != listing) {
                assert_eq!(
                    decode_continuation_token(&key, *elsewhere, scope, &token),
                    None,
                    "case {listing:?}/{id:?}: must name no position in {elsewhere:?}"
                );
            }
            // The token is written in the alphabet this listing's recorded tokens are
            // written in ([`PagedListing::spelling`]), and is not a RENDERING of the
            // position it resumes at: neither the id itself nor any value a caller could
            // compute from it without the key.
            for (rendering, computed) in [
                ("the id itself", id.to_string()),
                ("hex of the id", ::hex::encode(id)),
                (
                    "base64 of the id",
                    ::aws_smithy_types::base64::encode(id),
                ),
            ] {
                assert_ne!(
                    token, computed,
                    "case {listing:?}/{id:?}: the token must not be {rendering}, got {token:?}"
                );
            }
            match listing.spelling() {
                Spelling::Hex => assert!(
                    !token.is_empty()
                        && token
                            .chars()
                            .all(|character| matches!(character, '0'..='9' | 'a'..='f')),
                    "case {listing:?}/{id:?}: a token of this listing is lowercase hex, got \
                     {token:?}"
                ),
                Spelling::Base64 => assert!(
                    !token.is_empty()
                        && token.chars().all(|character| character.is_ascii_alphanumeric()
                            || matches!(character, '-' | '_')),
                    "case {listing:?}/{id:?}: a token of this listing is unpadded url-safe \
                     base64, got {token:?}"
                ),
            }
        }

        // Values a caller could write rather than round-trip, including one with the SHAPE
        // of a minted token, which only the authenticator can refuse. The rows are read
        // against a hex-spelled listing and a base64-spelled one alike, so neither
        // alphabet's reading serves a value this store never minted.
        let shaped_hex =
            ::hex::encode([0_u8; ::service_common::page_tokens::AUTHENTICATOR_BYTES + 1]);
        let shaped_base64 = {
            use ::base64::Engine as _;
            ::base64::engine::general_purpose::URL_SAFE_NO_PAD
                .encode([0_u8; ::service_common::page_tokens::AUTHENTICATOR_BYTES + 1])
        };
        for listing in [PagedListing::Metrics, PagedListing::Objects] {
            for (case, token) in [
                ("an id", "kept".to_string()),
                ("empty", String::new()),
                ("neither alphabet", "not a token".to_string()),
                ("hex but too short", "6b657074".to_string()),
                ("base64 but too short", "a2VwdA==".to_string()),
                ("a minted token's shape, in hex", shaped_hex.clone()),
                ("a minted token's shape, in base64", shaped_base64.clone()),
            ] {
                assert_eq!(
                    decode_continuation_token(&key, listing, scopes[0], &token),
                    None,
                    "case {listing:?}/{case}: expected no position for {token:?}"
                );
            }
        }
    }
}
