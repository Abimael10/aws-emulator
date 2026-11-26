// SPDX-License-Identifier: CC0-1.0

//! The ALPHABET the DIRECTORY-BUCKET listing writes its continuation token in, and what that
//! buys a client handing the value back.
//!
//! RULE under test: the `ContinuationToken` `ListDirectoryBuckets` mints is an opaque cursor
//! the caller hands back unread on the `continuation-token` QUERY PARAMETER of a request to
//! the S3 Express control endpoint — "ContinuationToken is obfuscated and is not a real
//! bucket" (`ListDirectoryBucketsRequest$ContinuationToken`) — so it is written in characters
//! a URI query component carries as THEMSELVES (RFC 3986's unreserved set, which this service
//! writes as lowercase hexadecimal): the value a client percent-encodes and the value it hands
//! back verbatim are then the SAME bytes, and both resume the listing at the page the previous
//! one stopped at. A character with two spellings on the wire would otherwise be covered by a
//! signed request's signature in only one of them, so which spelling the client chose would
//! decide whether its own resume verifies.
//!
//! The rows span what the rule generalises over: the PAGE SIZE the walk is taken at (one and
//! two buckets per page, so the cursor is minted at more than one position), the LENGTH of the
//! bucket name the token resumes at (three lengths, so the cursor's own length is not read back
//! at one size alone), and the hand-back FORM (verbatim and percent-encoded, which must be
//! interchangeable). The account's whole directory-bucket membership is walked out in every
//! row, so a token that resumed at the wrong position is caught by the names the walk reports
//! rather than only by its own spelling.
//!
//! Every call goes over the wire through the harness the sibling directory-bucket suites own,
//! addressed to `s3express-control.<region>.amazonaws.com` and signed for `s3express`, so what
//! is pinned is the wire document.

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send_bytes_signed, Answer};
use super::continuation_token_alphabet::{handed_back_in_query, is_lowercase_hex};
use super::continuation_token_query_safety::is_unreserved;
use super::object_and_tagging_roundtrip::text;

/// The credential scope a client of the S3 EXPRESS control endpoint presents, which is what
/// tells that endpoint's `GET /` (the directory-bucket listing) from the general purpose
/// endpoint's. Neither the key id nor the signature is a credential here (this suite's router
/// verifies none, as the sibling suites' do).
const S3_EXPRESS_AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/s3express/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// The host the S3 Express control endpoint is addressed as — "the HTTP Host header syntax is
/// `s3express-control.region.amazonaws.com`" (`ListDirectoryBuckets`).
const S3_EXPRESS_CONTROL_HOST: &str = "s3express-control.us-east-1.amazonaws.com";

/// The zone the buckets below are placed in.
const ZONE_ID: &str = "use1-az4";

/// `GET /<query>` on the S3 Express control endpoint — `ListDirectoryBuckets`.
async fn list_directory_buckets(router: &MultiServiceRouter, query: &str) -> Answer {
    send_bytes_signed(
        router,
        S3_EXPRESS_AUTHORIZATION,
        "GET",
        S3_EXPRESS_CONTROL_HOST,
        &format!("/{query}"),
        &[],
        b"",
    )
    .await
}

/// Assert the rule about the token ITSELF: it is written in the hex alphabet, every character
/// of which a query component carries as itself, so percent-encoding it leaves the same bytes
/// — and it is no plain rendering of the bucket it resumes at.
fn assert_travels_unchanged(case: &str, token: &str, resumes_at: &str) {
    assert!(
        is_lowercase_hex(token),
        "case {case:?}: the directory-bucket listing's token is lowercase hex, got {token:?}"
    );
    assert!(
        is_unreserved(token),
        "case {case:?}: every character of the token is one a query component carries \
         unchanged, got {token:?}"
    );
    let encoded = handed_back_in_query(token);
    assert_eq!(
        encoded, token,
        "case {case:?}: the encoded hand-back and the verbatim hand-back must be the same \
         bytes, got {encoded:?} for {token:?}"
    );
    assert_ne!(
        token, resumes_at,
        "case {case:?}: the token must not be the bucket it resumes at"
    );
    assert!(
        !token.contains(resumes_at),
        "case {case:?}: the token must carry no plain rendering of {resumes_at:?}, got \
         {token:?}"
    );
}

/// How a row hands the minted token back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HandBack {
    /// Exactly the bytes the listing answered, with nothing encoded — what a client that
    /// round-trips an opaque value unread sends.
    Verbatim,
    /// The same value run through the encoder a client puts a query value on the wire with.
    PercentEncoded,
}

impl HandBack {
    /// `token` as this hand-back puts it on the query parameter.
    fn write(self, token: &str) -> String {
        match self {
            HandBack::Verbatim => token.to_string(),
            HandBack::PercentEncoded => handed_back_in_query(token),
        }
    }
}

/// One walk of the account's directory buckets.
struct Row {
    case: &'static str,
    /// How many buckets one page may carry.
    page_size: usize,
    /// How the row hands a minted token back.
    hand_back: HandBack,
}

const ROWS: &[Row] = &[
    Row {
        case: "one bucket per page, handed back verbatim",
        page_size: 1,
        hand_back: HandBack::Verbatim,
    },
    Row {
        case: "one bucket per page, handed back percent-encoded",
        page_size: 1,
        hand_back: HandBack::PercentEncoded,
    },
    Row {
        case: "two buckets per page, handed back verbatim",
        page_size: 2,
        hand_back: HandBack::Verbatim,
    },
    Row {
        case: "two buckets per page, handed back percent-encoded",
        page_size: 2,
        hand_back: HandBack::PercentEncoded,
    },
];

/// RULE: whichever page size the walk takes and whichever of the two hand-backs a client
/// chooses, the token the listing minted resumes it at the bucket the page stopped at, and the
/// whole membership is reported exactly once.
#[tokio::test]
async fn the_directory_bucket_listing_token_is_hex_and_either_hand_back_resumes_it() {
    for row in ROWS {
        let router = router();
        // Three names of three LENGTHS, so the cursor the token carries is minted at more
        // than one size; the listing walks them in ascending order, which is the order the
        // walk below must report.
        let names = [
            format!("tokhex-a--{ZONE_ID}--x-s3"),
            format!("tokhex-beta--{ZONE_ID}--x-s3"),
            format!("tokhex-gamma-delta--{ZONE_ID}--x-s3"),
        ];
        for name in &names {
            super::directory_bucket_sessions_and_appends::directory_bucket(
                &router, name, ZONE_ID,
            )
            .await;
        }

        let mut reported: Vec<String> = Vec::new();
        let mut query = format!("?max-directory-buckets={}", row.page_size);
        // One page per group of `page_size` buckets, plus the page that ends the walk; the
        // bound is the membership itself, so a token that never stopped is caught here.
        for page_number in 1..=names.len() + 1 {
            let case = format!("{} / page {page_number}", row.case);
            let page = list_directory_buckets(&router, &query).await;
            assert_eq!(
                page.status,
                200,
                "case {case:?}: expected HTTP 200, got {} with body {:?}",
                page.status,
                text(&page)
            );
            let names_on_page = page.elements("Name");
            assert!(
                names_on_page.len() <= row.page_size,
                "case {case:?}: a page carries at most {} buckets, got {names_on_page:?}",
                row.page_size
            );
            reported.extend(names_on_page);
            let token = page
                .element("ContinuationToken")
                .filter(|token| !token.is_empty());
            let Some(token) = token else {
                assert_eq!(
                    reported.len(),
                    names.len(),
                    "case {case:?}: only the page that ends the walk owes no cursor, got body \
                     {:?}",
                    text(&page)
                );
                break;
            };
            // The bucket this token resumes at: the first one the walk has not reported yet.
            let resumes_at = names
                .get(reported.len())
                .map(String::as_str)
                .unwrap_or_default();
            assert_travels_unchanged(&case, &token, resumes_at);
            query = format!(
                "?max-directory-buckets={}&continuation-token={}",
                row.page_size,
                row.hand_back.write(&token)
            );
        }

        assert_eq!(
            reported,
            names.to_vec(),
            "case {:?}: the walk must report every directory bucket exactly once, in order, \
             got {reported:?}",
            row.case
        );
    }
}
