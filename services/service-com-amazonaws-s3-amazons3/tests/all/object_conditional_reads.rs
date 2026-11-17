// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the CONDITIONS an object read states: `If-Match`,
//! `If-Unmodified-Since`, `If-None-Match` and `If-Modified-Since` on `GetObject` and
//! `HeadObject`.
//!
//! Every assertion drives a real HTTP request through the router (the harness is the one
//! [`super::bucket_lifecycle`] owns), so what is pinned is the wire behaviour — status, the
//! validator headers and whether a body is carried — rather than any handler's internals.
//!
//! The rules under test, with their sources, all from the four members' own documentation on
//! `GetObjectRequest` (and `HeadObject` "has the same options as a GET operation on an
//! object"):
//!
//! * "Return the object only if its entity tag (ETag) is the same as the one specified in
//!   this header; otherwise, return a 412 Precondition Failed error" (`$IfMatch`), and the
//!   same 412 for a read whose object "has not been modified since the specified time"
//!   condition does not hold (`$IfUnmodifiedSince`).
//! * "Return the object only if its entity tag (ETag) is different from the one specified in
//!   this header; otherwise, return a 304 Not Modified error" (`$IfNoneMatch`), and the same
//!   304 when the object "has been modified since the specified time" condition does not
//!   hold (`$IfModifiedSince`).
//! * The PRECEDENCE within each pair: "If-Match condition evaluates to true, and;
//!   If-Unmodified-Since condition evaluates to false; then, S3 returns 200 OK and the data
//!   requested", and "If-None-Match condition evaluates to false, and; If-Modified-Since
//!   condition evaluates to true; then, S3 returns 304 Not Modified" — so the entity-tag
//!   condition of each pair decides the read on its own.
//! * The dates are HTTP-dates (the members' `timestampFormat` is `http-date`), compared at
//!   the one-second resolution such a date carries (RFC 9110 §5.6.7). An `If-Modified-Since`
//!   LATER than the server's current time is invalid — "a date which is later than the
//!   server's current time is invalid" (RFC 2616 §14.25) — and an invalid date is one "a
//!   recipient MUST ignore" (RFC 7232 §3.3, the RFC `$IfModifiedSince` cites), so the read is
//!   served.
//! * A 304 carries no content and does carry the validators of the representation the caller
//!   already holds: "the server generating a 304 response MUST generate … ETag,
//!   Last-Modified" and "MUST NOT generate content" (RFC 9110 §15.4.5).
//! * A condition is evaluated against the version the request RESOLVED, whatever else that
//!   request carries — the span it asks for, the version it addresses, the response header it
//!   overrides, the bucket owner it names — because the model binds no error to any of the
//!   four headers at all: this operation's whole modelled error set is `NoSuchKey`,
//!   `InvalidObjectState` and `NoSuchBucket`
//!   (`smithy_query(query="operation",operation="GetObject")`). So the condition alone
//!   decides between the bytes of the span asked for (200, or the 206 a `Range` or a
//!   `partNumber` earns — each "performs a 'ranged' GET request for the part specified"
//!   (`$PartNumber`) or "downloads the specified byte range" (`$Range`)), the bodiless 304 and
//!   the 412; no read carrying one of them is refused as a feature this emulator does not
//!   serve.

use std::time::{Duration, SystemTime};

use aws_smithy_types::date_time::Format;
use aws_smithy_types::DateTime;
use service_com_amazonaws_s3_amazons3::state::{
    IF_MATCH_CONDITION, IF_UNMODIFIED_SINCE_CONDITION, PRECONDITION_FAILED_CODE,
    PRECONDITION_FAILED_MESSAGE,
};
use smithy_server_runtime::MultiServiceRouter;

use super::bucket_lifecycle::{router, send_bytes, Answer};
use super::object_and_tagging_roundtrip::{bucket, get_object, put_object, text};
use super::object_versioning::set_versioning;

/// The bytes every read in this suite is of.
const BODY: &[u8] = b"conditional-read-payload";

/// `HEAD /<bucket>/<key><query>` — `HeadObject` with the conditional headers the case
/// carries.
///
/// `pub(crate)`, like [`http_date`] below: the suite that pins which VERSION a 304 names
/// ([`super::not_modified_version_headers`]) drives its HEADs and builds its dates through
/// these rather than keeping a second copy.
pub(crate) async fn head_object(
    router: &MultiServiceRouter,
    bucket: &str,
    key: &str,
    query: &str,
    headers: &[(&str, &str)],
) -> Answer {
    send_bytes(router, "HEAD", &format!("/{bucket}/{key}{query}"), headers, b"").await
}

/// `instant` as the HTTP-date a conditional header carries.
pub(crate) fn http_date(instant: SystemTime) -> String {
    DateTime::from(instant)
        .fmt(Format::HttpDate)
        .expect("an instant this test built must render as an HTTP date")
}

/// The instant an HTTP-date names.
fn instant_of(rendered: &str) -> SystemTime {
    let date = DateTime::from_str(rendered, Format::HttpDate)
        .expect("the response's Last-Modified must be an HTTP date");
    SystemTime::UNIX_EPOCH + Duration::from_secs(date.secs().unsigned_abs())
}

/// What a conditional read must answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Answered {
    /// 200, with the object's bytes (a GET) and its metadata.
    Served,
    /// 304, with the validators of the representation and no content at all.
    NotModified,
    /// 412 `PreconditionFailed`.
    Refused,
}

/// One row of the condition table: the case, the headers the read carries, and the answer.
struct ConditionRow {
    case: &'static str,
    headers: Vec<(String, String)>,
    answered: Answered,
}

/// The conditions a read states, over the object whose entity tag is `etag` and whose
/// modification instant is `last_modified`.
///
/// The rows differ along which condition is stated, the FORM of the entity-tag value (the
/// exact tag, a tag that names another representation, the `*` wildcard, a comma-separated
/// list and the weak marker), where the date falls relative to the object's own instant
/// (before it, exactly on it, and later than the server's own clock), and which pair of
/// conditions is stated together —
/// which is what the precedence rule is about.
fn condition_rows(etag: &str, last_modified: SystemTime) -> Vec<ConditionRow> {
    let other_etag = "\"00000000000000000000000000000000\"";
    let before = http_date(last_modified - Duration::from_secs(1));
    let exactly = http_date(last_modified);
    let later = http_date(last_modified + Duration::from_secs(3_600));
    let row = |case: &'static str, headers: Vec<(&str, String)>, answered: Answered| ConditionRow {
        case,
        headers: headers
            .into_iter()
            .map(|(name, value)| (name.to_string(), value))
            .collect(),
        answered,
    };
    vec![
        row("no condition at all", Vec::new(), Answered::Served),
        row(
            "If-None-Match naming the object's own tag",
            vec![("if-none-match", etag.to_string())],
            Answered::NotModified,
        ),
        row(
            "If-None-Match naming another representation",
            vec![("if-none-match", other_etag.to_string())],
            Answered::Served,
        ),
        row(
            "If-None-Match as the wildcard, which names any existing representation",
            vec![("if-none-match", "*".to_string())],
            Answered::NotModified,
        ),
        row(
            "If-None-Match as a list holding the object's tag",
            vec![("if-none-match", format!("{other_etag}, {etag}"))],
            Answered::NotModified,
        ),
        row(
            "If-None-Match with the weak marker on the object's tag",
            vec![("if-none-match", format!("W/{etag}"))],
            Answered::NotModified,
        ),
        row(
            "If-Modified-Since one second before the object was written",
            vec![("if-modified-since", before.clone())],
            Answered::Served,
        ),
        row(
            "If-Modified-Since exactly when the object was written",
            vec![("if-modified-since", exactly.clone())],
            Answered::NotModified,
        ),
        row(
            "If-Modified-Since later than the server's own clock, which states no condition",
            vec![("if-modified-since", later.clone())],
            Answered::Served,
        ),
        row(
            "If-Match naming the object's own tag",
            vec![("if-match", etag.to_string())],
            Answered::Served,
        ),
        row(
            "If-Match naming another representation",
            vec![("if-match", other_etag.to_string())],
            Answered::Refused,
        ),
        row(
            "If-Unmodified-Since one second before the object was written",
            vec![("if-unmodified-since", before.clone())],
            Answered::Refused,
        ),
        row(
            "If-Unmodified-Since after the object was written",
            vec![("if-unmodified-since", later.clone())],
            Answered::Served,
        ),
        // The precedence inside each pair.
        row(
            "If-Match holding beside an If-Unmodified-Since that does not",
            vec![
                ("if-match", etag.to_string()),
                ("if-unmodified-since", before.clone()),
            ],
            Answered::Served,
        ),
        row(
            "If-None-Match matching beside an If-Modified-Since that holds",
            vec![
                ("if-none-match", etag.to_string()),
                ("if-modified-since", before.clone()),
            ],
            Answered::NotModified,
        ),
        row(
            "If-None-Match not matching beside an If-Modified-Since that does not hold",
            vec![
                ("if-none-match", other_etag.to_string()),
                ("if-modified-since", exactly.clone()),
            ],
            Answered::Served,
        ),
        // Across the pairs: the 412 the first pair earns is answered whatever the caller's
        // cached copy is.
        row(
            "an If-Match that does not hold beside a matching If-None-Match",
            vec![
                ("if-match", other_etag.to_string()),
                ("if-none-match", etag.to_string()),
            ],
            Answered::Refused,
        ),
        row(
            "an If-Match that holds beside a matching If-None-Match",
            vec![
                ("if-match", etag.to_string()),
                ("if-none-match", etag.to_string()),
            ],
            Answered::NotModified,
        ),
    ]
}

#[tokio::test]
async fn a_conditional_read_is_served_refused_or_answered_not_modified() {
    let router = router();
    bucket(&router, "conditional-reads").await;
    let written = put_object(&router, "conditional-reads", "held.bin", BODY, &[]).await;
    assert_eq!(
        written.status,
        200,
        "the object the conditions are judged against must be written first, got {} with \
         body {:?}",
        written.status,
        text(&written)
    );
    let etag = written
        .header("etag")
        .expect("a stored object must report its entity tag");
    let probe = head_object(&router, "conditional-reads", "held.bin", "", &[]).await;
    let last_modified = instant_of(
        &probe
            .header("last-modified")
            .expect("a stored object must report when it was last modified"),
    );

    for row in condition_rows(&etag, last_modified) {
        let case = row.case;
        let headers: Vec<(&str, &str)> = row
            .headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();

        // Both reads are judged by the same conditions: a HEAD "has the same options as a
        // GET operation on an object", so only the body differs.
        let read = get_object(&router, "conditional-reads", "held.bin", "", &headers).await;
        let headed = head_object(&router, "conditional-reads", "held.bin", "", &headers).await;
        let (expected_status, expected_code) = match row.answered {
            Answered::Served => (200, ""),
            Answered::NotModified => (304, ""),
            Answered::Refused => (412, "PreconditionFailed"),
        };
        for (operation, answer) in [("GetObject", &read), ("HeadObject", &headed)] {
            assert_eq!(
                answer.status, expected_status,
                "case {case:?} on {operation}: expected HTTP {expected_status}, got {} with \
                 body {:?}",
                answer.status,
                text(answer)
            );
        }
        if !expected_code.is_empty() {
            assert_eq!(
                read.error_code(),
                expected_code,
                "case {case:?}: expected the code {expected_code:?}, got {:?}",
                read.error_code()
            );
        }

        match row.answered {
            Answered::Served => {
                assert_eq!(
                    read.body.as_ref(),
                    BODY,
                    "case {case:?}: a served read carries the object's bytes, got {:?}",
                    text(&read)
                );
                assert_eq!(
                    read.header("etag").as_deref(),
                    Some(etag.as_str()),
                    "case {case:?}: a served read reports the object's entity tag, got {:?}",
                    read.header("etag")
                );
            }
            Answered::NotModified => {
                // No content, and the validators of the representation the caller holds.
                assert!(
                    read.body.is_empty(),
                    "case {case:?}: a 304 carries no content, got {:?}",
                    text(&read)
                );
                assert_eq!(
                    read.header("etag").as_deref(),
                    Some(etag.as_str()),
                    "case {case:?}: a 304 reports the representation's entity tag, got {:?}",
                    read.header("etag")
                );
                assert_eq!(
                    read.header("last-modified").as_deref(),
                    Some(http_date(last_modified).as_str()),
                    "case {case:?}: a 304 reports when the representation was written, got \
                     {:?}",
                    read.header("last-modified")
                );
            }
            Answered::Refused => {
                assert!(
                    text(&read).contains("<Condition>"),
                    "case {case:?}: a 412 names the condition that did not hold, got {:?}",
                    text(&read)
                );
            }
        }
    }
}

#[tokio::test]
async fn a_conditional_read_of_a_key_that_is_not_there_is_not_modified_for_nobody() {
    let router = router();
    bucket(&router, "conditional-absent").await;

    // The conditions are judged against a version the read RESOLVED, so a key the bucket
    // does not hold is answered by its absence whatever the request's conditions are.
    for (case, headers) in [
        ("If-None-Match as the wildcard", vec![("if-none-match", "*")]),
        (
            "If-Modified-Since in the past",
            vec![("if-modified-since", "Thu, 29 Oct 2026 20:56:50 GMT")],
        ),
        ("If-Match as the wildcard", vec![("if-match", "*")]),
    ] {
        let read = get_object(&router, "conditional-absent", "missing.bin", "", &headers).await;
        assert_eq!(
            read.status,
            404,
            "case {case:?}: a key that is not there answers 404, got {} with body {:?}",
            read.status,
            text(&read)
        );
        assert_eq!(
            read.error_code(),
            "NoSuchKey",
            "case {case:?}: expected the NoSuchKey code, got {:?}",
            read.error_code()
        );
    }
}

/// The bucket every read of the table below that does not address a version is served from.
const BESIDE_PLAIN: &str = "beside-members-plain";
/// The bucket of that table that MINTS version ids, for the read addressing one.
const BESIDE_VERSIONED: &str = "beside-members-versioned";
/// The key both of those buckets hold.
const BESIDE_KEY: &str = "held.bin";
/// The header S3 names the version an answer is about in.
const VERSION_ID_HEADER: &str = "x-amz-version-id";

/// One address a read is of: the case, the bucket, and the request members that select WHAT
/// is read, beside the status and the bytes a SERVED read of it carries.
struct AddressRow {
    case: &'static str,
    bucket: &'static str,
    query: String,
    headers: Vec<(&'static str, String)>,
    served_status: u16,
    served_body: &'static [u8],
}

/// The addresses the conditions are stated about, for the table that pins a condition
/// stated BESIDE the other members a read carries.
///
/// The rows differ along which served member travels beside the condition — none, a byte
/// range, a part number, a response-header override, the expected bucket owner, the key of a
/// bucket that mints version ids, and a `versionId` naming one stored version — which is
/// what that rule generalises over.
fn address_rows(version_id: &str) -> Vec<AddressRow> {
    let row = |case: &'static str,
               bucket: &'static str,
               query: String,
               headers: Vec<(&'static str, String)>,
               served_status: u16,
               served_body: &'static [u8]| AddressRow {
        case,
        bucket,
        query,
        headers,
        served_status,
        served_body,
    };
    vec![
        row(
            "the whole object",
            BESIDE_PLAIN,
            String::new(),
            Vec::new(),
            200,
            BODY,
        ),
        row(
            "a byte range of the object",
            BESIDE_PLAIN,
            String::new(),
            vec![("range", "bytes=0-7".to_string())],
            206,
            &BODY[..8],
        ),
        row(
            "the only part of the object",
            BESIDE_PLAIN,
            "?partNumber=1".to_string(),
            Vec::new(),
            206,
            BODY,
        ),
        row(
            "a read overriding a response header",
            BESIDE_PLAIN,
            "?response-content-type=text/plain".to_string(),
            Vec::new(),
            200,
            BODY,
        ),
        row(
            "a read naming the bucket's owner",
            BESIDE_PLAIN,
            String::new(),
            vec![(
                "x-amz-expected-bucket-owner",
                engine::emulator_account_id().to_string(),
            )],
            200,
            BODY,
        ),
        row(
            "the key of a bucket that mints version ids",
            BESIDE_VERSIONED,
            String::new(),
            Vec::new(),
            200,
            BODY,
        ),
        row(
            "one stored version addressed by its id",
            BESIDE_VERSIONED,
            format!("?versionId={version_id}"),
            Vec::new(),
            200,
            BODY,
        ),
    ]
}

/// One condition stated about an object written at `last_modified`, and the answer it earns.
///
/// The rows differ along WHICH condition is stated and, for the dated ones, where the date
/// falls relative to the object's own instant — the axis a condition is evaluated on. The
/// 412 rows are the opposite family, whose refusal is the other answer a condition decides.
fn beside_condition_rows(last_modified: SystemTime) -> Vec<ConditionRow> {
    let before = http_date(last_modified - Duration::from_secs(1));
    let exactly = http_date(last_modified);
    let other_etag = "\"00000000000000000000000000000000\"";
    let row = |case: &'static str, name: &str, value: String, answered: Answered| ConditionRow {
        case,
        headers: vec![(name.to_string(), value)],
        answered,
    };
    vec![
        row(
            "If-Modified-Since one second before the object was written",
            "if-modified-since",
            before.clone(),
            Answered::Served,
        ),
        row(
            "If-Modified-Since exactly when the object was written",
            "if-modified-since",
            exactly,
            Answered::NotModified,
        ),
        row(
            "If-Unmodified-Since one second before the object was written",
            "if-unmodified-since",
            before,
            Answered::Refused,
        ),
        row(
            "If-Match naming another representation",
            "if-match",
            other_etag.to_string(),
            Answered::Refused,
        ),
    ]
}

/// The `<Condition>` the 412 of `headers` must name: the header the failing member is bound
/// to, as this service's own catalog spells it.
fn expected_condition(headers: &[(String, String)]) -> &'static str {
    match headers.first().map(|(name, _)| name.as_str()) {
        Some("if-match") => IF_MATCH_CONDITION,
        Some("if-unmodified-since") => IF_UNMODIFIED_SINCE_CONDITION,
        other => panic!("only the If-Match family earns a 412, not {other:?}"),
    }
}

/// The instant the object of `bucket` reports as its own, read back through `HeadObject`.
async fn written_at(router: &MultiServiceRouter, bucket: &str) -> SystemTime {
    let probe = head_object(router, bucket, BESIDE_KEY, "", &[]).await;
    assert_eq!(
        probe.status, 200,
        "the object of {bucket:?} must read back, got {} with body {:?}",
        probe.status,
        text(&probe)
    );
    instant_of(
        &probe
            .header("last-modified")
            .expect("a stored object must report when it was last modified"),
    )
}

/// Write `BODY` at [`BESIDE_KEY`] of `bucket`, answering with the write's response.
async fn write_held_object(router: &MultiServiceRouter, bucket: &str) -> Answer {
    let written = put_object(router, bucket, BESIDE_KEY, BODY, &[]).await;
    assert_eq!(
        written.status,
        200,
        "the object of {bucket:?} the conditions are judged against must be written first, \
         got {} with body {:?}",
        written.status,
        text(&written)
    );
    written
}

#[tokio::test]
async fn a_condition_stated_beside_another_member_is_still_evaluated() {
    let router = router();
    bucket(&router, BESIDE_PLAIN).await;
    bucket(&router, BESIDE_VERSIONED).await;
    set_versioning(&router, BESIDE_VERSIONED, "Enabled").await;

    // Each bucket's own object: the entity tag its write reported and the instant it reports
    // as its own, so every row is judged against the representation of the bucket it reads.
    let plain_write = write_held_object(&router, BESIDE_PLAIN).await;
    let versioned_write = write_held_object(&router, BESIDE_VERSIONED).await;
    let etag_of = |write: &Answer| {
        write
            .header("etag")
            .expect("a stored object must report its entity tag")
    };
    let version_id = versioned_write
        .header(VERSION_ID_HEADER)
        .expect("a write to a bucket that mints version ids must report the id it minted");

    let written = [
        (
            BESIDE_PLAIN,
            etag_of(&plain_write),
            written_at(&router, BESIDE_PLAIN).await,
        ),
        (
            BESIDE_VERSIONED,
            etag_of(&versioned_write),
            written_at(&router, BESIDE_VERSIONED).await,
        ),
    ];

    for address in address_rows(&version_id) {
        let (etag, last_modified) = written
            .iter()
            .find_map(|(bucket, etag, instant)| {
                (*bucket == address.bucket).then_some((etag.as_str(), *instant))
            })
            .expect("every row reads one of the two buckets this test wrote");

        for row in beside_condition_rows(last_modified) {
            let case = format!("{} with {}", address.case, row.case);
            let headers: Vec<(&str, &str)> = address
                .headers
                .iter()
                .map(|(name, value)| (*name, value.as_str()))
                .chain(
                    row.headers
                        .iter()
                        .map(|(name, value)| (name.as_str(), value.as_str())),
                )
                .collect();
            let read =
                get_object(&router, address.bucket, BESIDE_KEY, &address.query, &headers).await;

            // Whatever the read carries beside its condition, the answer is one of the three
            // the condition can earn — never a refusal that says the header is not served.
            assert_ne!(
                read.status, 501,
                "case {case:?}: a conditional read is evaluated, not refused as unserved; got \
                 the body {:?}",
                text(&read)
            );

            let expected_status = match row.answered {
                Answered::Served => address.served_status,
                Answered::NotModified => 304,
                Answered::Refused => 412,
            };
            assert_eq!(
                read.status, expected_status,
                "case {case:?}: expected HTTP {expected_status}, got {} with body {:?}",
                read.status,
                text(&read)
            );

            match row.answered {
                Answered::Served => {
                    assert_eq!(
                        read.body.as_ref(),
                        address.served_body,
                        "case {case:?}: a served read carries the span it asked for, got {:?}",
                        text(&read)
                    );
                    assert_eq!(
                        read.header("etag").as_deref(),
                        Some(etag),
                        "case {case:?}: a served read reports the object's entity tag, got {:?}",
                        read.header("etag")
                    );
                }
                Answered::NotModified => {
                    assert!(
                        read.body.is_empty(),
                        "case {case:?}: a 304 carries no content, got {:?}",
                        text(&read)
                    );
                    assert_eq!(
                        read.header("etag").as_deref(),
                        Some(etag),
                        "case {case:?}: a 304 reports the representation's entity tag, got {:?}",
                        read.header("etag")
                    );
                    assert_eq!(
                        read.header("last-modified").as_deref(),
                        Some(http_date(last_modified).as_str()),
                        "case {case:?}: a 304 reports when the representation was written, got \
                         {:?}",
                        read.header("last-modified")
                    );
                }
                Answered::Refused => {
                    assert_eq!(
                        read.error_code(),
                        PRECONDITION_FAILED_CODE,
                        "case {case:?}: expected the PreconditionFailed code, got {:?}",
                        read.error_code()
                    );
                    assert_eq!(
                        read.error_message(),
                        PRECONDITION_FAILED_MESSAGE,
                        "case {case:?}: expected the documented sentence, got {:?}",
                        read.error_message()
                    );
                    assert_eq!(
                        read.element("Condition").as_deref(),
                        Some(expected_condition(&row.headers)),
                        "case {case:?}: a 412 names the header whose condition did not hold, \
                         got {:?}",
                        text(&read)
                    );
                }
            }
        }
    }
}
