// SPDX-License-Identifier: CC0-1.0

//! HOW a continuation token this service mints is SPELLED, and how a client hands it back.
//!
//! RULE under test: the token a truncated page hands back is an opaque cursor the service
//! mints and the caller hands back unread — "NextContinuationToken is obfuscated and is not a
//! real key" (`ListObjectsV2Output$NextContinuationToken`), "ContinuationToken is obfuscated
//! and is not a real bucket" (`ListBucketsOutput$ContinuationToken`) — written in the base64
//! alphabet AWS's own tokens are written in: the STANDARD one (RFC 4648 §4, `A-Za-z0-9+/`
//! with `=` padding), so a client that reads the value as base64 reads the same character
//! class whichever listing minted it. The value travels on the `continuation-token` query
//! parameter, where a client percent-encodes the characters a query component reads otherwise
//! ([`handed_back_in_query`]) — and a client that hands the value back verbatim, leaving its
//! `+` to arrive as the space form decoding makes of it, resumes the same page, because a
//! token is round-tripped rather than parsed.
//!
//! The rows span what the rule generalises over: the LISTING that minted the token (the
//! objects a bucket holds and the buckets an account holds), the shape of the position it
//! resumes at (a one-character name, a flat name, a nested path), and — through those lengths
//! — each of the three padding classes base64 has (no `=`, one, two), so a token is never
//! read back only at one alignment.
//!
//! Every call goes over the wire through the harness the other bucket suites own, so what is
//! pinned is the wire document.

use percent_encoding::{utf8_percent_encode, AsciiSet, CONTROLS};

use super::bucket_lifecycle::{create_bucket, router, Answer};
use super::list_buckets::list_buckets;
use super::object_and_tagging_roundtrip::text;
use super::object_storage::{list_objects_v2, listing_of, store_keys};

/// The characters of the base64 alphabet a QUERY COMPONENT reads otherwise: `+` is the
/// form-encoded space, `/` is a path separator and `=` separates a parameter from its value.
const QUERY_VALUE: &AsciiSet = &CONTROLS.add(b'+').add(b'/').add(b'=');

/// `token` as a client puts it on the `continuation-token` query parameter: percent-encoded,
/// through the RFC 3986 encoder this workspace already uses rather than a second reading of
/// that rule.
///
/// `pub(crate)`: every suite that hands a minted token back on a query parameter sends it
/// through this one encoder, so no suite's resume depends on which characters one run's token
/// happened to draw.
pub(crate) fn handed_back_in_query(token: &str) -> String {
    utf8_percent_encode(token, QUERY_VALUE).to_string()
}

/// Whether `token` is written in the standard base64 alphabet (RFC 4648 §4).
fn is_standard_base64(token: &str) -> bool {
    !token.is_empty()
        && token.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '+' | '/' | '=')
        })
}

/// How many padding characters `token` ends with: base64's three alignment classes.
fn padding(token: &str) -> usize {
    token.chars().filter(|character| *character == '=').count()
}

/// The `NextContinuationToken` of a `ListObjectsV2` answer, which a truncated page carries.
fn object_token(case: &str, answer: &Answer) -> String {
    let token = listing_of(answer).field("NextContinuationToken");
    assert!(
        !token.is_empty(),
        "case {case:?}: a truncated page carries a NextContinuationToken, got {:?}",
        text(answer)
    );
    token
}

/// The `ContinuationToken` of a `ListBuckets` answer, which a truncated page carries.
fn bucket_token(case: &str, answer: &Answer) -> String {
    let token = answer
        .element("ContinuationToken")
        .unwrap_or_else(|| panic!("case {case:?}: no ContinuationToken in {:?}", text(answer)));
    assert!(
        !token.is_empty(),
        "case {case:?}: a truncated page carries a ContinuationToken, got {:?}",
        text(answer)
    );
    token
}

/// Assert the token's SPELLING: the standard base64 alphabet, decodable as standard base64,
/// and no rendering of the position it resumes at.
fn assert_spelled_in_base64(case: &str, token: &str, position: &str) {
    assert!(
        is_standard_base64(token),
        "case {case:?}: a continuation token is standard base64, got {token:?}"
    );
    assert!(
        aws_smithy_types::base64::decode(token).is_ok(),
        "case {case:?}: a continuation token decodes as standard base64, got {token:?}"
    );
    assert_ne!(
        token, position,
        "case {case:?}: the token must not be the position it resumes at"
    );
    // Read only for a position wide enough that a chance run of base64 characters could not
    // be one.
    if position.len() >= 3 {
        assert!(
            !token.contains(position),
            "case {case:?}: the token must carry no plain rendering of {position:?}, got \
             {token:?}"
        );
    }
}

/// (case, the keys one bucket holds — the listing walks them in this order.)
///
/// A token names the first key the page did NOT return, so the SECOND key's length is what
/// decides the token's base64 alignment: the rows carry one key of each length class modulo
/// three — `9`, `1` and `8` characters — so every padding class is read back.
fn object_rows() -> Vec<(&'static str, Vec<&'static str>)> {
    vec![
        ("a one-character name", vec!["a", "b"]),
        ("a flat name", vec!["alpha.txt", "beta.txt"]),
        (
            "a nested path",
            vec!["media/2024/april.jpg", "notes.txt"],
        ),
    ]
}

/// RULE: every token the object listing mints is spelled in the standard base64 alphabet and
/// resumes the listing exactly where the page stopped when it is handed back on the query
/// parameter the way a client hands a query value back.
#[tokio::test]
async fn an_object_listing_token_is_standard_base64_and_resumes_the_page() {
    let router = router();
    let mut seen_padding: Vec<usize> = Vec::new();

    for (index, (case, keys)) in object_rows().into_iter().enumerate() {
        let bucket_name = format!("token-alphabet-objects-{index}");
        create_bucket(&router, &bucket_name, None, None).await;
        store_keys(&router, &bucket_name, &keys).await;

        let first = list_objects_v2(&router, &bucket_name, "&max-keys=1").await;
        assert_eq!(
            first.status,
            200,
            "case {case:?}: the first page is served, got {} with body {:?}",
            first.status,
            text(&first)
        );
        let page = listing_of(&first);
        assert_eq!(
            page.keys(),
            vec![keys[0].to_string()],
            "case {case:?}: the first page carries the first key alone, got {:?}",
            page.keys()
        );
        let token = object_token(case, &first);
        assert_spelled_in_base64(case, &token, keys[1]);
        assert_spelled_in_base64(case, &token, keys[0]);
        seen_padding.push(padding(&token));

        // The token a client hands back, percent-encoded as a query value is.
        let resumed = list_objects_v2(
            &router,
            &bucket_name,
            &format!("&continuation-token={}", handed_back_in_query(&token)),
        )
        .await;
        assert_eq!(
            resumed.status,
            200,
            "case {case:?}: the token this listing issued resumes it, got {} with body {:?}",
            resumed.status,
            text(&resumed)
        );
        assert_eq!(
            listing_of(&resumed).keys(),
            keys[1..]
                .iter()
                .map(|key| (*key).to_string())
                .collect::<Vec<_>>(),
            "case {case:?}: the resumed page reports the keys the first page left, got {:?}",
            listing_of(&resumed).keys()
        );

        // The same token, echoed back as the request carried it.
        assert_eq!(
            listing_of(&resumed).field("ContinuationToken"),
            token,
            "case {case:?}: the page echoes the token it was given"
        );
    }

    // The rows covered every base64 alignment, so the rule was read back at each of them.
    for class in [0_usize, 1, 2] {
        assert!(
            seen_padding.contains(&class),
            "the rows must cover every base64 padding class, got {seen_padding:?} with {class} \
             missing"
        );
    }
}

/// RULE, the verbatim hand-back: a client that returns the value it was given unread, so its
/// `+` arrives as the space a query component is read to carry, resumes the same page — the
/// token is round-tripped, not parsed.
#[tokio::test]
async fn a_token_handed_back_verbatim_resumes_the_same_page() {
    let router = router();

    for (index, (case, keys)) in object_rows().into_iter().enumerate() {
        let bucket_name = format!("token-verbatim-objects-{index}");
        create_bucket(&router, &bucket_name, None, None).await;
        store_keys(&router, &bucket_name, &keys).await;

        let first = list_objects_v2(&router, &bucket_name, "&max-keys=1").await;
        let token = object_token(case, &first);
        let resumed = list_objects_v2(
            &router,
            &bucket_name,
            &format!("&continuation-token={token}"),
        )
        .await;
        assert_eq!(
            resumed.status,
            200,
            "case {case:?}: the value the listing handed back resumes it, got {} with body {:?}",
            resumed.status,
            text(&resumed)
        );
        assert_eq!(
            listing_of(&resumed).keys(),
            keys[1..]
                .iter()
                .map(|key| (*key).to_string())
                .collect::<Vec<_>>(),
            "case {case:?}: the resumed page reports the keys the first page left, got {:?}",
            listing_of(&resumed).keys()
        );
    }
}

/// RULE, on the other listing: the account's own bucket listing spells its token the same
/// way and resumes the same way, so the spelling is the service's and not one listing's.
#[tokio::test]
async fn the_bucket_listing_token_is_standard_base64_and_resumes_the_account() {
    let router = router();
    let names = ["token-alphabet-a", "token-alphabet-b", "token-alphabet-c"];
    for name in names {
        create_bucket(&router, name, None, None).await;
    }

    let case = "the account's buckets, one per page";
    let first = list_buckets(&router, "&max-buckets=1").await;
    assert_eq!(
        first.status,
        200,
        "case {case:?}: the first page is served, got {} with body {:?}",
        first.status,
        text(&first)
    );
    let token = bucket_token(case, &first);
    assert_spelled_in_base64(case, &token, names[1]);

    let resumed = list_buckets(
        &router,
        &format!("&max-buckets=1&continuation-token={}", handed_back_in_query(&token)),
    )
    .await;
    assert_eq!(
        resumed.status,
        200,
        "case {case:?}: the token this listing issued resumes it, got {} with body {:?}",
        resumed.status,
        text(&resumed)
    );
    assert_eq!(
        resumed.elements("Name"),
        vec![names[1].to_string()],
        "case {case:?}: the resumed page reports the bucket the first page left, got {:?}",
        resumed.elements("Name")
    );
}

/// RULE: only a value this service MINTED names a page, so a token one character away from
/// the one a listing issued is refused rather than read as a cursor of the caller's own.
#[tokio::test]
async fn a_value_one_character_away_from_a_minted_token_is_refused() {
    let router = router();
    let bucket_name = "token-alphabet-forged";
    create_bucket(&router, bucket_name, None, None).await;
    store_keys(&router, bucket_name, &["alpha.txt", "beta.txt"]).await;

    let first = list_objects_v2(&router, bucket_name, "&max-keys=1").await;
    let token = object_token("a forged token", &first);
    // Each row changes ONE character of the minted token, at each end of it, which only the
    // authenticator inside can refuse.
    let rows: Vec<(&str, String)> = vec![
        (
            "the first character changed",
            format!("{}{}", flipped(&token[..1]), &token[1..]),
        ),
        (
            "a character in the middle changed",
            {
                let middle = token.len() / 2;
                format!(
                    "{}{}{}",
                    &token[..middle],
                    flipped(&token[middle..middle + 1]),
                    &token[middle + 1..]
                )
            },
        ),
    ];

    for (case, forged) in rows {
        let refused = list_objects_v2(
            &router,
            bucket_name,
            &format!("&continuation-token={}", handed_back_in_query(&forged)),
        )
        .await;
        assert_eq!(
            refused.status,
            400,
            "case {case:?}: a value no listing issued is refused, got {} with body {:?}",
            refused.status,
            text(&refused)
        );
        assert_eq!(
            refused.error_code(),
            "InvalidArgument",
            "case {case:?}: expected the InvalidArgument code, got {:?}",
            refused.error_code()
        );
    }
}

/// One base64 character with another in its place, so the value is still base64 text and
/// still the length a minted token has.
fn flipped(character: &str) -> String {
    match character {
        "A" => "B".to_string(),
        _ => "A".to_string(),
    }
}
