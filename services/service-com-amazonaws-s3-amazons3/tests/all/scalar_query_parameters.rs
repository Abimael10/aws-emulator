// SPDX-License-Identifier: CC0-1.0

//! End-to-end cover for the SCALAR QUERY PARAMETERS a request states, and what S3 answers a
//! value one of them cannot carry.
//!
//! Every call goes over the wire through the harnesses the other suites own, so what is
//! pinned is the wire document — status, error code, `<Message>`, the `<ArgumentName>` /
//! `<ArgumentValue>` pair and the bound response headers — rather than any handler's
//! internals.
//!
//! The rules under test, each asserted over a TABLE spanning what it generalises over rather
//! than over the one value that showed it:
//!
//! 1. A scalar query parameter whose text is not a value of the bound member's type is
//!    refused by the OPERATION with S3's own `InvalidArgument` (400): the `<Message>` states
//!    the rule the text broke, and the `<ArgumentName>` / `<ArgumentValue>` pair names the
//!    parameter as the API publishes it and echoes the text as sent. The deserializer's own
//!    report of such a text — a sentence about the Rust type it could not parse, under a code
//!    no S3 client matches — is never what a caller sees. The rows span the ways a text fails
//!    to be an integer (a decimal, a word, scientific notation, a value past the type's
//!    range, a malformed sign, a radix prefix) and the ways an integer falls outside the
//!    bound a page may be asked for, over the bucket listing and all three object listings.
//! 2. An integer AT the bounds a page may be asked for is SERVED, and so is the parameter
//!    stated with NO value, which states no bound at all: the request is answered exactly as
//!    one that left the parameter out.
//! 3. `ListBuckets`' Region filter must name an AWS Region — "the Amazon Web Services Region
//!    must be expressed according to the Amazon Web Services Region code, such as us-west-2"
//!    (`ListBucketsRequest$BucketRegion`) — and a value that names none, the EMPTY value
//!    among them, is refused with the sentence that echoes it.
//! 4. A `response-*` override is TEXT the read echoes into the matching response header:
//!    `response-expires` "sets the Expires header of the response"
//!    (`GetObjectRequest$ResponseExpires`) and that header is published as a string
//!    (`smithy_query(query="shape",shape="Expires")`), so a read whose override is no date at
//!    all is served, with the text as sent.
//! 5. An `If-Modified-Since` LATER than the server's current time states no condition and is
//!    ignored — "a date which is later than the server's current time is invalid" (RFC 2616
//!    §14.25) and an invalid date is one "a recipient MUST ignore" (RFC 7232 §3.3, the RFC
//!    `GetObjectRequest$IfModifiedSince` itself cites) — so such a read is served rather than
//!    answered 304.

use std::time::{Duration, SystemTime};

use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{create_bucket, router, send, Answer};
use super::list_buckets::list_buckets;
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::object_conditional_reads::{head_object, http_date};
use super::object_storage::store_keys;

/// The code and status S3 refuses a value a request field cannot carry with.
const INVALID_ARGUMENT: &str = "InvalidArgument";

/// The bytes every read in this suite is of.
const BODY: &[u8] = b"scalar-parameter-payload";

/// The keys the object listings in this suite walk.
const KEYS: &[&str] = &["notes/april.txt", "notes/may.txt", "plain.txt"];

/// The ways a text fails to be an integer of a page bound's own type, which is the class rule
/// 1 is about: nothing here is a number that type holds.
const NOT_AN_INTEGER: &[(&str, &str)] = &[
    ("a decimal", "1.5"),
    ("a word", "many"),
    ("scientific notation", "1e3"),
    ("a value one past the type's range", "2147483648"),
    ("a value far past the type's range", "99999999999999999999"),
    ("a malformed sign", "--1"),
    ("a radix prefix", "0x10"),
];

/// The characters a query-string value is NOT escaped over: RFC 3986's unreserved set, the
/// same set this service's own query encoders leave alone
/// (`crate::service::post_object`'s `QUERY_UNRESERVED`).
const QUERY_UNRESERVED: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// `value` as a query parameter carries it: everything outside RFC 3986's unreserved set
/// percent-encoded, so the text the server reads is the text the case names.
fn query_encoded(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, QUERY_UNRESERVED).to_string()
}

/// Assert that `answer` is S3's own `InvalidArgument` naming `argument` and echoing `value`,
/// with `message` as its sentence.
fn assert_refused(answer: &Answer, case: &str, argument: &str, value: &str, message: &str) {
    assert_eq!(
        answer.status,
        400,
        "case {case:?}: expected HTTP 400, got {} with body {:?}",
        answer.status,
        text(answer)
    );
    assert_eq!(
        answer.error_code(),
        INVALID_ARGUMENT,
        "case {case:?}: expected the {INVALID_ARGUMENT:?} code, got {:?} in {:?}",
        answer.error_code(),
        text(answer)
    );
    assert_eq!(
        answer.error_message(),
        message,
        "case {case:?}: expected the sentence {message:?}, got {:?}",
        answer.error_message()
    );
    assert_eq!(
        answer.element("ArgumentName").as_deref(),
        Some(argument),
        "case {case:?}: the envelope must name the parameter it rejected, got {:?} in {:?}",
        answer.element("ArgumentName"),
        text(answer)
    );
    assert_eq!(
        answer.element("ArgumentValue").as_deref(),
        Some(value),
        "case {case:?}: the envelope must echo the value as sent, got {:?} in {:?}",
        answer.element("ArgumentValue"),
        text(answer)
    );
}

#[tokio::test]
async fn a_bucket_page_bound_that_bounds_no_page_is_refused_by_naming_the_parameter() {
    let router = router();
    for name in ["bound-refusal-a", "bound-refusal-b"] {
        let created = create_bucket(&router, name, None, None).await;
        assert_eq!(
            created.status,
            200,
            "case {name:?}: the account must hold a bucket for the listing to report, got {} \
             with body {:?}",
            created.status,
            text(&created)
        );
    }

    // A text that is no integer of the member's type: the sentence says so and says nothing
    // about Rust or about the model's own types.
    for (why, value) in NOT_AN_INTEGER {
        let case = format!("ListBuckets/max-buckets as {why}");
        let answer = list_buckets(&router, &format!("&max-buckets={}", query_encoded(value))).await;
        assert_refused(
            &answer,
            &case,
            "max-buckets",
            value,
            "Provided max-buckets not an integer or within integer range",
        );
    }

    // An INTEGER outside the bound a page of buckets may be asked for — `MaxBuckets` is
    // `range {min 1, max 10000}` (`smithy_query(query="operation",operation="ListBuckets")`) —
    // which states the BOUNDS rather than the value, each published bound and one past it.
    for (why, value) in [
        ("one below the minimum", "0"),
        ("negative", "-1"),
        ("one above the maximum", "10001"),
        ("far above the maximum", "2147483647"),
    ] {
        let case = format!("ListBuckets/max-buckets {why}");
        let answer = list_buckets(&router, &format!("&max-buckets={value}")).await;
        assert_refused(
            &answer,
            &case,
            "max-buckets",
            value,
            "Argument max-buckets must be an integer between 1 and 10000",
        );
    }

    // The published bounds themselves are SERVED, and so is the parameter stated with no
    // value at all, which asks for no bound.
    for (why, value) in [
        ("the minimum", "1"),
        ("the maximum", "10000"),
        ("no value at all", ""),
    ] {
        let case = format!("ListBuckets/max-buckets {why}");
        let answer = list_buckets(&router, &format!("&max-buckets={value}")).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: a bound a page may be asked for is served, got {} with body {:?}",
            answer.status,
            text(&answer)
        );
        assert!(
            answer.element("ListAllMyBucketsResult").is_some()
                || answer.element("Buckets").is_some(),
            "case {case:?}: a served listing carries its result element, got {:?}",
            text(&answer)
        );
    }
}

/// One object listing: the operation, and the query that addresses it.
struct Listing {
    call: &'static str,
    address: &'static str,
    /// Whether this is the V2 listing, which words a NEGATIVE page bound in the member's own
    /// spelling where the two legacy listings word it in the parameter's. Recorded behavior.
    v2: bool,
}

/// The three listings that bind a `max-keys`
/// (`smithy_query(query="operation",operation="ListObjects,ListObjectsV2,ListObjectVersions")`
/// reports `MaxKeys: {httpQuery: "max-keys"}` on each).
const LISTINGS: &[Listing] = &[
    Listing {
        call: "ListObjects",
        address: "",
        v2: false,
    },
    Listing {
        call: "ListObjectsV2",
        address: "list-type=2",
        v2: true,
    },
    Listing {
        call: "ListObjectVersions",
        address: "versions",
        v2: false,
    },
];

/// `GET /<bucket>?<address>&max-keys=<value>` — one listing, however it is addressed.
async fn list(
    router: &MultiServiceRouter,
    bucket_name: &str,
    listing: &Listing,
    parameter: &str,
) -> Answer {
    let uri = match listing.address.is_empty() {
        true => format!("/{bucket_name}?{parameter}"),
        false => format!("/{bucket_name}?{}&{parameter}", listing.address),
    };
    send(router, "GET", &uri, &[], "").await
}

#[tokio::test]
async fn an_object_page_bound_that_bounds_no_page_is_refused_by_naming_the_parameter() {
    let router = router();
    let bucket_name = "object-bound-refusal";
    bucket(&router, bucket_name).await;
    store_keys(&router, bucket_name, KEYS).await;

    for listing in LISTINGS {
        for (why, value) in NOT_AN_INTEGER {
            let case = format!("{}/max-keys as {why}", listing.call);
            let answer = list(
                &router,
                bucket_name,
                listing,
                &format!("max-keys={}", query_encoded(value)),
            )
            .await;
            assert_refused(
                &answer,
                &case,
                "max-keys",
                value,
                "Provided max-keys not an integer or within integer range",
            );
        }

        // An integer BELOW the bound a page of objects may be asked for. The V2 listing and
        // the two legacy ones word it differently and name the parameter differently —
        // recorded behavior — and each wording states the rule and no value.
        let (argument, message) = match listing.v2 {
            true => (
                "maxKeys",
                "Argument maxKeys must be an integer between 0 and 2147483647",
            ),
            false => ("max-keys", "max-keys cannot be negative"),
        };
        for (why, value) in [
            ("one below the minimum", "-1"),
            ("far below the minimum", "-1000"),
            ("the least value the type holds", "-2147483648"),
        ] {
            let case = format!("{}/max-keys {why}", listing.call);
            let answer = list(&router, bucket_name, listing, &format!("max-keys={value}")).await;
            assert_refused(&answer, &case, argument, value, message);
        }

        // Every bound a page MAY be asked for is served — the minimum, the smallest page, the
        // published page ceiling, the largest value the type holds — and so is the parameter
        // stated with no value, which asks for no bound and is answered with the default.
        for (why, value, echoed) in [
            ("the minimum", "0", "0"),
            ("the smallest non-empty page", "1", "1"),
            ("the page ceiling", "1000", "1000"),
            ("the largest the type holds", "2147483647", "2147483647"),
            ("no value at all", "", "1000"),
        ] {
            let case = format!("{}/max-keys {why}", listing.call);
            let answer = list(&router, bucket_name, listing, &format!("max-keys={value}")).await;
            assert_eq!(
                answer.status,
                200,
                "case {case:?}: a bound a page may be asked for is served, got {} with body \
                 {:?}",
                answer.status,
                text(&answer)
            );
            assert_eq!(
                answer.element("MaxKeys").as_deref(),
                Some(echoed),
                "case {case:?}: the listing reports the bound it was asked for, got {:?} in \
                 {:?}",
                answer.element("MaxKeys"),
                text(&answer)
            );
        }
    }
}

#[tokio::test]
async fn a_region_filter_that_names_no_region_is_refused_by_echoing_it() {
    let router = router();
    let created = create_bucket(&router, "region-filter-bucket", None, None).await;
    assert_eq!(
        created.status,
        200,
        "the account must hold a bucket for the listing to report, got {} with body {:?}",
        created.status,
        text(&created)
    );

    // Values a `bucket-region` may carry that name no Region code: the EMPTY value, a word, a
    // Region name rather than its code, the legacy `LocationConstraint` alias (which names a
    // Region without being a Region code), a code in the wrong case, and a code with the
    // wrong separators.
    for (why, value) in [
        ("no value at all", ""),
        ("a word that names no Region", "not-a-region"),
        ("a Region name rather than a code", "US"),
        ("the legacy location-constraint alias", "EU"),
        ("a code in the wrong case", "US-West-2"),
        ("a code with the wrong separators", "us_west_2"),
    ] {
        let case = format!("ListBuckets/bucket-region as {why}");
        let answer = list_buckets(
            &router,
            &format!("&bucket-region={}", query_encoded(value)),
        )
        .await;
        assert_refused(
            &answer,
            &case,
            "bucket-region",
            value,
            &format!("Argument value {value} is not a valid AWS Region"),
        );
    }

    // Region CODES are served, whether or not this account holds a bucket in them. The codes
    // are the ones the model publishes for a bucket's location
    // (`smithy_query(query="shape",shape="BucketLocationConstraint")`), plus the Region that
    // enum expresses as the ABSENT constraint rather than as a value of its own.
    for (why, value) in [
        ("the Region an absent location constraint names", "us-east-1"),
        ("a Region this account holds no bucket in", "us-west-2"),
        ("a Region of another partition", "cn-north-1"),
        ("a Region of the government partition", "us-gov-west-1"),
        ("a Region the published set gained late", "ap-southeast-7"),
    ] {
        let case = format!("ListBuckets/bucket-region {why}");
        let answer = list_buckets(&router, &format!("&bucket-region={value}")).await;
        assert_eq!(
            answer.status,
            200,
            "case {case:?}: a Region code narrows the listing rather than refusing it, got {} \
             with body {:?}",
            answer.status,
            text(&answer)
        );
    }
}

#[tokio::test]
async fn a_response_override_is_echoed_as_the_text_the_read_sent() {
    let router = router();
    let bucket_name = "response-override-reads";
    bucket(&router, bucket_name).await;
    let written = put_object(
        &router,
        bucket_name,
        "held.bin",
        BODY,
        &[("content-type", "text/csv"), ("expires", "stored-expiry")],
    )
    .await;
    assert_eq!(
        written.status,
        200,
        "the object the overrides are read through must be written first, got {} with body \
         {:?}",
        written.status,
        text(&written)
    );

    // What a `response-expires` may carry: an HTTP-date, a date in another format, a word that
    // is no date at all, a number, and a value that is no date in any format. Every one of
    // them is SERVED and echoed verbatim, because the header it sets is text.
    for (why, sent) in [
        ("an HTTP-date", "Sat, 01 Jan 2000 00:00:00 GMT"),
        ("an RFC 3339 date-time", "2000-01-01T00:00:00Z"),
        ("a word that is no date", "not-a-date"),
        ("a number", "0"),
        ("a sentence", "the day after tomorrow"),
    ] {
        let case = format!("response-expires as {why}");
        let query = format!("?response-expires={}", query_encoded(sent));
        let read = get_object(&router, bucket_name, "held.bin", &query, &[]).await;
        let headed = head_object(&router, bucket_name, "held.bin", &query, &[]).await;
        for (operation, answer) in [("GetObject", &read), ("HeadObject", &headed)] {
            assert_eq!(
                answer.status,
                200,
                "case {case:?} on {operation}: a response override never refuses the read, got \
                 {} with body {:?}",
                answer.status,
                text(answer)
            );
            assert_eq!(
                answer.header("expires").as_deref(),
                Some(sent),
                "case {case:?} on {operation}: the override is echoed as sent, got {:?}",
                answer.header("expires")
            );
        }
        assert_eq!(
            read.body.as_ref(),
            BODY,
            "case {case:?}: the read is served the object's bytes, got {:?}",
            text(&read)
        );
    }

    // A read that states no override, and one that states it with NO value, report what the
    // write STORED instead: the override is the request's value where it has one.
    for (why, query) in [
        ("no override at all", String::new()),
        ("an override stated with no value", "?response-expires=".to_string()),
    ] {
        let case = format!("response-expires: {why}");
        let read = get_object(&router, bucket_name, "held.bin", &query, &[]).await;
        assert_eq!(
            read.status,
            200,
            "case {case:?}: expected HTTP 200, got {} with body {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            read.header("expires").as_deref(),
            Some("stored-expiry"),
            "case {case:?}: the stored value is reported where the request overrides nothing, \
             got {:?}",
            read.header("expires")
        );
    }

    // The other overrides of the same read are the same rule, and they apply on both reads.
    let query = "?response-content-type=application/json&response-content-language=en-GB";
    let read = get_object(&router, bucket_name, "held.bin", query, &[]).await;
    let headed = head_object(&router, bucket_name, "held.bin", query, &[]).await;
    for (operation, answer) in [("GetObject", &read), ("HeadObject", &headed)] {
        assert_eq!(
            answer.header("content-type").as_deref(),
            Some("application/json"),
            "case response-content-type on {operation}: the override wins over the stored \
             value, got {:?}",
            answer.header("content-type")
        );
        assert_eq!(
            answer.header("content-language").as_deref(),
            Some("en-GB"),
            "case response-content-language on {operation}: the override is reported, got {:?}",
            answer.header("content-language")
        );
    }
}

#[tokio::test]
async fn a_conditional_date_the_server_has_not_reached_states_no_condition() {
    let router = router();
    let bucket_name = "conditional-future-dates";
    bucket(&router, bucket_name).await;
    let written = put_object(&router, bucket_name, "held.bin", BODY, &[]).await;
    assert_eq!(
        written.status,
        200,
        "the object the conditions are judged against must be written first, got {} with body \
         {:?}",
        written.status,
        text(&written)
    );

    // Where the date falls relative to the server's own clock is what decides the answer: a
    // date the server has not reached states no condition and the read is served, while a
    // date it has reached revalidates the copy the caller holds.
    let rows = [
        (
            "an hour past the server's clock",
            http_date(SystemTime::now() + Duration::from_secs(3_600)),
            200,
        ),
        (
            "a literal date far past the server's clock",
            "Fri, 01 Jan 2100 00:00:00 GMT".to_string(),
            200,
        ),
        (
            "long before the object was written",
            "Sat, 01 Jan 2000 00:00:00 GMT".to_string(),
            200,
        ),
        (
            "the instant of this read, which the server has reached",
            http_date(SystemTime::now()),
            304,
        ),
    ];
    for (why, date, expected) in rows {
        let case = format!("If-Modified-Since {why}");
        let headers = [("if-modified-since", date.as_str())];
        let read = get_object(&router, bucket_name, "held.bin", "", &headers).await;
        let headed = head_object(&router, bucket_name, "held.bin", "", &headers).await;
        for (operation, answer) in [("GetObject", &read), ("HeadObject", &headed)] {
            assert_eq!(
                answer.status, expected,
                "case {case:?} on {operation}: expected HTTP {expected}, got {} with body {:?}",
                answer.status,
                text(answer)
            );
        }
        if expected == 200 {
            assert_eq!(
                read.body.as_ref(),
                BODY,
                "case {case:?}: a served read carries the object's bytes, got {:?}",
                text(&read)
            );
        }
    }
}
