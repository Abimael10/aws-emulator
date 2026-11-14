// SPDX-License-Identifier: CC0-1.0

//! The SHAPE of the two correlation tokens every answer of this service names its request
//! by, and the alphabet each of them is drawn from.
//!
//! The rule under test: every response — served or refused — is named by the pair of
//! correlation ids S3 publishes for it ("`x-amz-request-id`: A value created by Amazon S3
//! that uniquely identifies the request" and "`x-amz-id-2`: A special token that helps
//! Amazon Web Services troubleshoot problems", Amazon S3 API Reference, *Common Response
//! Headers*), and each is an OPAQUE token of the shape that header publishes: the request
//! id a fixed-width run of digits and UPPERCASE letters, the extended id a longer base64
//! token. The value itself is minted per request, so what a client, a proxy, a log
//! correlator or a support request can read of it is its WIDTH and its ALPHABET — which is
//! why a token drawn from a narrower alphabet (the hexadecimal digits) or from another one
//! altogether (the lowercase and hyphens of a UUID) is the wrong value however unique it
//! is. The error document mirrors the pair in `<RequestId>` and `<HostId>`, so the id a
//! client reads out of a refusal is the one an operator reads off its headers.
//!
//! The tables span what the rule generalises over:
//!
//! * the OPERATION and the CLASS of answer — a create, a listing and a subresource read
//!   that are served, the refusal of a `HEAD`, two subresource reads of a bucket that
//!   carries no such configuration, two bucket-scoped not-founds, an object-scoped
//!   not-found and a document that does not validate — because the pair is a property of a
//!   RESPONSE and not of any one operation or refusal;
//! * the DRAW the token comes of, over a bounded run of mints of the service's own
//!   correlation mint, because the rule is an invariant of every token it ever mints and
//!   not of the handful one table of requests happens to draw.
//!
//! The request table drives real HTTP requests through the router (the harness
//! [`super::bucket_lifecycle`] owns), so what is pinned is the wire response; the shape
//! predicates are [`super::served_envelope_values`]'s, so the two suites state the shape
//! once.

use std::collections::BTreeSet;

use service_com_amazonaws_s3_amazons3::state::request_ids::RequestCorrelation;

use super::bucket_lifecycle::{create_bucket, router, send, send_bytes, Answer};
use super::object_and_tagging_roundtrip::{bucket, text};
use super::served_envelope_values::{
    correlation_headers, is_extended_request_id, is_request_id, REQUEST_ID_CHARS,
};

/// The characters an `x-amz-request-id` is drawn from: the ten digits and the twenty-six
/// uppercase letters, the alphabet the fixed-width token S3 names a request by is written
/// in (Amazon S3 API Reference, *Common Response Headers*).
const REQUEST_ID_ALPHABET: &str = "0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ";

/// The characters a hexadecimal rendering would be confined to — the SUBSET of that
/// alphabet a digest written in hex can reach, which is what the coverage assertion below
/// tells apart from the published alphabet.
const HEXADECIMAL_DIGITS: &str = "0123456789ABCDEF";

/// How many pairs the invariant test mints: enough draws that every character of the
/// published alphabet has to appear somewhere among their tokens, and few enough that the
/// test is one tight loop.
const MINTED_PAIRS_UNDER_TEST: usize = 4_096;

/// What the table expects of one answer: the ids are a property of a RESPONSE, so the rows
/// span served answers and refusals alike.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Expected {
    /// A served answer, which carries no error document.
    Served,
    /// A refusal, whose error document mirrors the two headers.
    Refused,
}

/// Assert that `request_id` and `extended` are shaped the way S3's own pair is, naming
/// `case` and the token in every failure.
fn assert_published_shapes(request_id: &str, extended: &str, case: &str) {
    assert!(
        is_request_id(request_id),
        "case {case:?}: expected a {REQUEST_ID_CHARS}-character token of digits and \
         uppercase letters, got {request_id:?}"
    );
    assert!(
        request_id
            .chars()
            .all(|character| REQUEST_ID_ALPHABET.contains(character)),
        "case {case:?}: every character must come from the published alphabet \
         {REQUEST_ID_ALPHABET:?}, got {request_id:?}"
    );
    assert!(
        is_extended_request_id(extended),
        "case {case:?}: expected a base64 extended id of the width S3 publishes, got \
         {extended:?}"
    );
}

#[tokio::test]
async fn every_answer_names_its_request_by_the_pair_of_tokens_s3_publishes() {
    let router = router();
    let name = "correlation-token-shape";
    bucket(&router, name).await;

    // (case, the answer, what the rule expects of it). The rows differ along the operation
    // and the CLASS of answer, served and refused alike.
    let rows: Vec<(&str, Answer, Expected)> = vec![
        (
            "CreateBucket, which is served",
            create_bucket(&router, "correlation-token-shape-two", None, None).await,
            Expected::Served,
        ),
        (
            "ListBuckets, which is served",
            send(&router, "GET", "/", &[], "").await,
            Expected::Served,
        ),
        (
            "HeadBucket on a bucket that is not there",
            send(&router, "HEAD", "/correlation-token-absent", &[], "").await,
            Expected::Refused,
        ),
        (
            "GetBucketPolicy on a bucket carrying no policy",
            send(&router, "GET", &format!("/{name}?policy"), &[], "").await,
            Expected::Refused,
        ),
        (
            "GetBucketOwnershipControls, which is served",
            send(
                &router,
                "GET",
                &format!("/{name}?ownershipControls"),
                &[],
                "",
            )
            .await,
            Expected::Served,
        ),
        (
            "GetBucketOwnershipControls on a bucket that is not there",
            send(
                &router,
                "GET",
                "/correlation-token-absent?ownershipControls",
                &[],
                "",
            )
            .await,
            Expected::Refused,
        ),
        (
            "GetBucketWebsite on a bucket carrying no website configuration",
            send(&router, "GET", &format!("/{name}?website"), &[], "").await,
            Expected::Refused,
        ),
        (
            "DeleteBucket naming a bucket that is not there",
            send(&router, "DELETE", "/correlation-token-absent", &[], "").await,
            Expected::Refused,
        ),
        (
            "PutObjectTagging on a key the bucket holds nothing at",
            send(
                &router,
                "PUT",
                &format!("/{name}/never-written.bin?tagging"),
                &[],
                "<Tagging xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><TagSet><Tag>\
                 <Key>project</Key><Value>two</Value></Tag></TagSet></Tagging>",
            )
            .await,
            Expected::Refused,
        ),
        (
            "PutBucketWebsite carrying a document that does not validate",
            super::bucket_website::put_website(&router, name, "<Suffix>index.html</Suffix>").await,
            Expected::Refused,
        ),
        (
            "GetObject on a key the bucket holds nothing at",
            send_bytes(
                &router,
                "GET",
                &format!("/{name}/never-written.bin"),
                &[],
                b"",
            )
            .await,
            Expected::Refused,
        ),
    ];

    let mut seen: Vec<(String, String)> = Vec::new();
    for (case, answer, expected) in &rows {
        let (request_id, extended) = correlation_headers(answer, case);
        assert_published_shapes(&request_id, &extended, case);
        assert_eq!(
            answer.status >= 400,
            *expected != Expected::Served,
            "case {case:?}: expected a {} answer, got {} with body {:?}",
            match expected {
                Expected::Served => "served",
                _ => "refused",
            },
            answer.status,
            text(answer)
        );
        if *expected == Expected::Refused {
            assert!(
                !answer.error_code().trim().is_empty(),
                "case {case:?}: a refusal must carry an error document, got {:?}",
                text(answer)
            );
            // The document mirrors the two headers, both the id and its partner.
            assert_eq!(
                answer.element("RequestId").as_deref(),
                Some(request_id.as_str()),
                "case {case:?}: the envelope's RequestId must be the header's \
                 {request_id:?}, got {:?} in {:?}",
                answer.element("RequestId"),
                text(answer)
            );
            assert_eq!(
                answer.element("HostId").as_deref(),
                Some(extended.as_str()),
                "case {case:?}: the envelope's HostId must be the header's {extended:?}, \
                 got {:?} in {:?}",
                answer.element("HostId"),
                text(answer)
            );
        }
        // "A value created by Amazon S3 that uniquely identifies the request": no two of
        // these answers may be named by the same token.
        assert!(
            !seen.iter().any(|(earlier, _)| earlier == &request_id),
            "case {case:?}: the request id {request_id:?} already named an earlier answer, \
             got {seen:?}"
        );
        seen.push((request_id, extended));
    }
}

#[test]
fn every_minted_token_is_unique_and_spans_the_published_alphabet() {
    let mut request_ids: BTreeSet<String> = BTreeSet::new();
    let mut extended_ids: BTreeSet<String> = BTreeSet::new();
    let mut characters: BTreeSet<char> = BTreeSet::new();

    for draw in 0..MINTED_PAIRS_UNDER_TEST {
        let case = format!("draw {draw}");
        let minted = RequestCorrelation::mint();
        assert_published_shapes(&minted.request_id, &minted.extended_request_id, &case);
        characters.extend(minted.request_id.chars());
        assert!(
            request_ids.insert(minted.request_id.clone()),
            "case {case:?}: the request id {:?} was minted twice",
            minted.request_id
        );
        assert!(
            extended_ids.insert(minted.extended_request_id.clone()),
            "case {case:?}: the extended id {:?} was minted twice",
            minted.extended_request_id
        );
    }

    // The tokens are drawn from the WHOLE published alphabet: a mint confined to the
    // hexadecimal digits would satisfy every assertion above and still publish a token of
    // a narrower shape than the one S3's ids have.
    let missing: Vec<char> = REQUEST_ID_ALPHABET
        .chars()
        .filter(|character| !characters.contains(character))
        .collect();
    assert!(
        missing.is_empty(),
        "over {MINTED_PAIRS_UNDER_TEST} draws every character of {REQUEST_ID_ALPHABET:?} \
         must appear in some token, got none of {missing:?}"
    );
    let beyond_hexadecimal: Vec<char> = characters
        .iter()
        .copied()
        .filter(|character| !HEXADECIMAL_DIGITS.contains(*character))
        .collect();
    assert!(
        !beyond_hexadecimal.is_empty(),
        "the minted tokens must reach past the hexadecimal digits {HEXADECIMAL_DIGITS:?}, \
         got only {characters:?}"
    );
}
