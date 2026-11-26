// SPDX-License-Identifier: CC0-1.0

//! The error CLASS a listing refuses a `Marker` with, end to end over HTTP.
//!
//! A `Marker` is where the previous page ended, so which refusal an unusable one gets is
//! decided by what that listing's model says the value IS:
//!
//! * where the marker is the id of a resource of the kind the listing pages AND the model
//!   binds a not-found error NAMED for that kind, a marker that resolves to no such resource
//!   is refused under that code (HTTP 404) in the sentence that states the request's MARKER is
//!   no resource of that kind. The FORM of the submitted value is no separate condition —
//!   this service publishes no form for the ids it mints, so a value that could never have
//!   been one resolves to nothing just as surely as one that could;
//! * where the model publishes a CONSTRAINT for the shape such a value has to be, a value
//!   outside it is a value this API cannot read, which it answers `MalformedXML` (400) as it
//!   answers every value outside a published constraint — `ListKeyValueStores`' marker is a
//!   pagination token, whose pattern the API publishes;
//! * where the model offers neither, the refusal stays the modelled `InvalidArgument` (400)
//!   stating the constraint on the parameter, which is all such a listing can say.
//!
//! The rows differ along what each branch generalises over: the RESOURCE the listing pages
//! (four policy families, a key value store, a distribution, a public key), the FORM the
//! unusable marker takes (a word, another namespace's id, an id in the wrong case, a
//! never-minted UUID of each extreme, a name past the published length, a name drawing from
//! characters the published pattern excludes), and — for each branch — a marker that IS a
//! resume point, which is served rather than refused.
//!
//! The wire harness is [`crate::distribution_lifecycle`]'s.

use service_com_amazonaws_cloudfront_cloudfront2020_05_31::state::{
    INVALID_PUBLIC_KEY_MARKER_MESSAGE, MANAGED_CACHE_POLICIES, MANAGED_ORIGIN_REQUEST_POLICIES,
    MANAGED_RESPONSE_HEADERS_POLICIES, MAX_KEY_VALUE_STORE_NAME, PAGINATION_TOKEN_PATTERN,
};
use smithy_server_runtime::MultiServiceRouter;

use crate::distribution_lifecycle::{call, emulator, Answer, API_VERSION};
use crate::key_value_store_lifecycle::{create_key_value_store, Create};

/// Every form a `Marker` can take that resolves to no resource of the listing's kind: a word
/// that is no identifier at all, another resource kind's identifier, an identifier of the
/// right shape in the wrong case, and the two extremes of the UUID namespace this service
/// mints policy ids in — none of which any store below holds.
const UNRESOLVABLE_MARKERS: [(&str, &str); 6] = [
    ("a word that is no identifier", "not-an-id"),
    ("another resource kind's identifier", "E1AAAAAAAAAAAA"),
    (
        "an id of the right shape in the wrong case",
        "216ADEF6-5C7F-47E4-B989-5492EAFA07D3",
    ),
    (
        "the lowest UUID of the namespace",
        "00000000-0000-0000-0000-000000000000",
    ),
    (
        "the highest UUID of the namespace",
        "ffffffff-ffff-ffff-ffff-ffffffffffff",
    ),
    ("a bare number", "12345"),
];

/// `GET <path>?Marker=<marker>`, the one request path every query-bound listing below goes
/// down.
async fn resume(router: &MultiServiceRouter, path: &str, marker: &str) -> Answer {
    call(
        router,
        "GET",
        &format!("/{API_VERSION}/{path}?Marker={marker}"),
        None,
        None,
    )
    .await
}

/// One listing whose model binds a not-found error named for the resource its marker
/// identifies: the route, that error's code and the sentence it states, and a marker the
/// listing really does hand out.
struct NotFoundListing {
    case: &'static str,
    path: &'static str,
    code: &'static str,
    message: &'static str,
    /// An id the listing resumes at, or `None` for a family this account holds nothing of
    /// until something is created (the continuous deployment policies, which AWS publishes no
    /// catalogue of).
    resume_point: Option<&'static str>,
}

/// The listings that branch: each policy family whose model binds its own `NoSuch…Policy` for
/// the resource it pages. The sentences are written out as LITERALS rather than read back off
/// the crate's producer, so the WORDING is what these rows are held to. The resume points are
/// the published catalogues this crate serves, so the served row needs nothing created for it.
fn not_found_listings() -> Vec<NotFoundListing> {
    vec![
        NotFoundListing {
            case: "the cache policy listing",
            path: "cache-policy",
            code: "NoSuchCachePolicy",
            message: "The request contains an invalid cache policy marker.",
            resume_point: MANAGED_CACHE_POLICIES.first().map(|managed| managed.id),
        },
        NotFoundListing {
            case: "the origin request policy listing",
            path: "origin-request-policy",
            code: "NoSuchOriginRequestPolicy",
            message: "The request contains an invalid origin request policy marker.",
            resume_point: MANAGED_ORIGIN_REQUEST_POLICIES
                .first()
                .map(|managed| managed.id),
        },
        NotFoundListing {
            case: "the response headers policy listing",
            path: "response-headers-policy",
            code: "NoSuchResponseHeadersPolicy",
            message: "The request contains an invalid response headers policy marker.",
            resume_point: MANAGED_RESPONSE_HEADERS_POLICIES
                .first()
                .map(|managed| managed.id),
        },
        NotFoundListing {
            case: "the continuous deployment policy listing",
            path: "continuous-deployment-policy",
            code: "NoSuchContinuousDeploymentPolicy",
            message: "The request contains an invalid continuous deployment policy marker.",
            resume_point: None,
        },
    ]
}

/// A `Marker` that resolves to no resource of the kind the listing pages is refused as that
/// resource not existing — whatever form the submitted value takes.
#[tokio::test]
async fn an_unresolvable_marker_is_the_resource_that_does_not_exist() {
    let router = emulator();

    for listing in not_found_listings() {
        for (form, marker) in UNRESOLVABLE_MARKERS {
            let case = format!("{} / {form}", listing.case);
            let answer = resume(&router, listing.path, marker).await;
            assert_eq!(
                answer.status, 404,
                "case {case} ({marker:?}): expected HTTP 404, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                listing.code,
                "case {case} ({marker:?}): expected {}, got {:?} in {}",
                listing.code,
                answer.error_code(),
                answer.raw
            );
            let message = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                message, listing.message,
                "case {case} ({marker:?}): expected the sentence {:?}, got {message:?}",
                listing.message
            );
            assert!(
                !message.contains(marker),
                "case {case}: the refusal must not report the submitted marker {marker:?} back, \
                 got {message:?}"
            );
        }
    }
}

/// The control for that branch: a `Marker` naming a resource the listing DOES hold is a resume
/// point, so the refusals above are about the value resolving to nothing and not about the
/// parameter being carried at all.
#[tokio::test]
async fn a_marker_a_policy_listing_holds_is_a_resume_point() {
    let router = emulator();

    for listing in not_found_listings() {
        let Some(resume_point) = listing.resume_point else {
            continue;
        };
        let answer = resume(&router, listing.path, resume_point).await;
        assert_eq!(
            answer.status, 200,
            "case {}: expected HTTP 200 resuming at {resume_point:?}, got {} with {}",
            listing.case, answer.status, answer.raw
        );
    }
}

/// `ListKeyValueStores`' `Marker` is a pagination TOKEN the API publishes a pattern for, so a
/// value outside that pattern is a value this API cannot read: `MalformedXML` (400), stated in
/// the frame every constraint violation of this API is stated in.
#[tokio::test]
async fn a_marker_outside_a_published_constraint_is_malformed() {
    let router = emulator();

    // Rows: each character class the published token pattern excludes — punctuation of three
    // kinds and a letter outside the ASCII word alphabet — and the shape a bad marker takes
    // when a caller sends prose. Each travels percent-encoded, since `#` opens a URI fragment,
    // `,` is reserved and a space cannot travel raw.
    let rows: [(&str, &str, &str); 5] = [
        ("an exclamation mark", "a!b", "a%21b"),
        ("a hash", "a#b", "a%23b"),
        ("a comma", "a,b", "a%2Cb"),
        ("a letter outside the word alphabet", "héllo", "h%C3%A9llo"),
        (
            "prose where a token was expected",
            "not a valid marker !! ###",
            "not%20a%20valid%20marker%20%21%21%20%23%23%23",
        ),
    ];
    for (form, marker, encoded) in rows {
        let answer = resume(&router, "key-value-store", encoded).await;
        assert_eq!(
            answer.status, 400,
            "case {form} ({marker:?}): expected HTTP 400, got {} with {}",
            answer.status, answer.raw
        );
        assert_eq!(
            answer.error_code(),
            "MalformedXML",
            "case {form} ({marker:?}): expected MalformedXML, got {:?} in {}",
            answer.error_code(),
            answer.raw
        );
        let message = answer.root().find_text("Message").unwrap_or_default();
        let expected = format!(
            "1 validation error detected: Value '{marker}' at 'nextToken' failed to satisfy \
             constraint: Member must satisfy regular expression pattern: {PAGINATION_TOKEN_PATTERN}"
        );
        assert_eq!(
            message, expected,
            "case {form} ({marker:?}): expected the constraint the token broke, got {message:?}"
        );
    }

    // A value the pattern DOES admit is a position in the one name order, so the listing
    // resumes after it whether or not a store is held under that name — this listing's model
    // binds no not-found error at all.
    let created = create_key_value_store(
        &router,
        &Create {
            case: "the store the listing resumes at",
            name: "marker-class-store".to_string(),
            comment: None,
            tags: Vec::new(),
        }
        .xml(),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "creating the store under test: expected HTTP 201, got {} with {}",
        created.status, created.raw
    );
    let past_length = "x".repeat(MAX_KEY_VALUE_STORE_NAME + 1);
    for (form, marker) in [
        ("the name of a store the account holds", "marker-class-store"),
        ("a name the account holds nothing under", "no_such_store-1"),
        ("a value carrying the punctuation a token admits", "a.b:c+d=e/f-g"),
        ("a value longer than any store name", past_length.as_str()),
    ] {
        let answer = resume(&router, "key-value-store", marker).await;
        assert_eq!(
            answer.status, 200,
            "case {form} ({marker:?}): expected HTTP 200, got {} with {}",
            answer.status, answer.raw
        );
    }
}

/// The third branch: a listing whose model binds neither a not-found error for the resource
/// its marker names nor a constraint for the shape of the value keeps the modelled
/// `InvalidArgument`, stating the constraint on the parameter.
#[tokio::test]
async fn a_listing_with_no_other_error_states_the_constraint_on_the_parameter() {
    let router = emulator();

    // (the listing, its route, the sentence it states, the markers that are no resume point
    // of THAT listing) — a distribution listing and a public key listing, whose models bind
    // `InvalidArgument` alone. Each marker list is of forms that listing's own identifiers
    // never take.
    for (case, path, expected, markers) in [
        (
            "the distribution listing",
            "distribution",
            "Marker must be a valid Distribution Id",
            [
                ("a word that is no identifier", "not-an-id"),
                ("a UUID, which no distribution id is", "216adef6-5c7f-47e4-b989-5492eafa07d3"),
                ("a bare number", "12345"),
                ("an id of the right shape in the wrong case", "e1aaaaaaaaaaaa"),
            ],
        ),
        (
            "the public key listing",
            "public-key",
            INVALID_PUBLIC_KEY_MARKER_MESSAGE,
            [
                ("a word that is no identifier", "not-an-id"),
                ("a UUID, which no public key id is", "216adef6-5c7f-47e4-b989-5492eafa07d3"),
                ("a bare number", "12345"),
                ("another resource kind's identifier", "E1AAAAAAAAAAAA"),
            ],
        ),
    ] {
        for (form, marker) in markers {
            let answer = resume(&router, path, marker).await;
            assert_eq!(
                answer.status, 400,
                "case {case} / {form} ({marker:?}): expected HTTP 400, got {} with {}",
                answer.status, answer.raw
            );
            assert_eq!(
                answer.error_code(),
                "InvalidArgument",
                "case {case} / {form} ({marker:?}): expected InvalidArgument, got {:?} in {}",
                answer.error_code(),
                answer.raw
            );
            let message = answer.root().find_text("Message").unwrap_or_default();
            assert_eq!(
                message, expected,
                "case {case} / {form} ({marker:?}): expected the constraint on the parameter, \
                 got {message:?}"
            );
        }
    }
}
