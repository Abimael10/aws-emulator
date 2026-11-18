// SPDX-License-Identifier: CC0-1.0

//! WHETHER a viewer that already holds a copy of the object is sent the object again, or told
//! that the copy it holds is still the current one.
//!
//! The edge serves the origin's VALIDATORS through to the viewer — the `ETag` and the
//! `Last-Modified` the origin stated about the representation travel with it, on a miss and out
//! of the cache alike ([`super::RESPONSE_HEADERS_NEVER_SERVED`] is the short list of headers
//! that do not) — so a viewer can come back and ask for the object only if the copy it holds is
//! stale. That second request is what this module answers.
//!
//! The RULE is HTTP's own (RFC 9110 §13, *Conditional Requests*), evaluated against the
//! representation the edge is about to serve — the entry its cache held, or the answer the
//! origin has just produced:
//!
//! * `If-None-Match` decides on its own, by WEAK comparison over a list (§13.1.2): the
//!   condition fails — so the viewer's copy is current — when any entity tag it lists is the
//!   representation's own, with the weak marker ignored on either side, or when it is the `*`
//!   that names "any current representation";
//! * `If-Modified-Since` is read only when no `If-None-Match` is present (§13.2.2 evaluates the
//!   entity-tag precondition first), and the viewer's copy is current when the representation
//!   was last modified no later than the date it carries, at the one-second resolution an
//!   HTTP-date has (§5.6.7). A date LATER than the edge's own clock states no condition a
//!   request can have meant — no viewer holds a copy read at an instant that has not happened —
//!   so it is ignored and the object is served;
//! * a condition that HOLDS (the viewer's copy is not the current one) is served the object,
//!   which is the request's unconditional answer.
//!
//! A viewer whose copy is current is answered `304 Not Modified` carrying NONE of the object's
//! bytes and no header that describes a body (§15.4.5), and keeping every header that describes
//! the representation — its validators above all, so the next conditional request can be made
//! against the same values.
//!
//! Conditional evaluation is defined for the safe methods that retrieve a representation, so
//! only a `GET` or a `HEAD` is evaluated; and only an answer that IS one complete
//! representation has validators to judge, so a page CloudFront generated itself, a partial
//! answer and an origin's own error are left exactly as they are.
//!
//! The edge evaluates the condition ITSELF, which is why the viewer's conditional headers are
//! not forwarded to the origin ([`super::HEADERS_NEVER_FORWARDED`] holds them, for the reason
//! it holds the viewer's `Range`): the origin is asked for the COMPLETE representation, so one
//! viewer's revalidation can never be what the cache ends up holding for the object, and every
//! later viewer — conditional or not — is answered from that one entry.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::aws_smithy_types::body::SdkBody;
use ::std::time::{SystemTime, UNIX_EPOCH};

use super::status_code;

/// The request header a viewer states the entity tag of the copy it holds in.
pub const IF_NONE_MATCH: &str = "if-none-match";

/// The request header a viewer states when the copy it holds was last modified in.
pub const IF_MODIFIED_SINCE: &str = "if-modified-since";

/// The response header carrying the representation's entity tag.
const ETAG: &str = "etag";

/// The response header carrying when the representation was last modified.
const LAST_MODIFIED: &str = "last-modified";

/// The headers that describe a BODY, which a `304` carries none of because it carries no body.
const BODY_HEADERS: [&str; 3] = ["content-type", "content-encoding", "content-length"];

/// The entity-tag list value that names any current representation (RFC 9110 §13.1.2).
const ANY_REPRESENTATION: &str = "*";

/// The marker a WEAK entity tag is prefixed with, ignored by the weak comparison
/// `If-None-Match` is judged under.
const WEAK_MARKER: &str = "W/";

/// The status an answer carrying one whole representation has — the only answer a condition is
/// evaluated against.
const COMPLETE: u16 = 200;

/// The status of the answer a viewer whose copy is current receives.
const NOT_MODIFIED: u16 = 304;

/// The methods conditional evaluation is defined for: the safe ones that retrieve a
/// representation.
const CONDITIONAL_METHODS: [&str; 2] = ["GET", "HEAD"];

/// What one viewer request states about the copy it already holds.
#[derive(Debug, Default)]
pub struct Conditions<'r> {
    if_none_match: Option<&'r str>,
    if_modified_since: Option<&'r str>,
}

impl<'r> Conditions<'r> {
    /// The conditions `req` states.
    pub fn of_request(req: &'r RtRequest) -> Self {
        Self {
            if_none_match: req.headers().get(IF_NONE_MATCH),
            if_modified_since: req.headers().get(IF_MODIFIED_SINCE),
        }
    }

    /// Whether this request states a condition at all, so nothing is read off an answer for a
    /// viewer that holds no copy.
    fn stated(&self) -> bool {
        self.if_none_match.is_some() || self.if_modified_since.is_some()
    }
}

/// Answer `response` `304 Not Modified` when the conditions `req` stated say the copy that
/// viewer holds is the representation the edge is about to serve.
///
/// `response` is that answer — the cache's entry or what the origin produced — so the
/// validators judged are the ones the viewer would otherwise receive again. A request stating
/// no condition, a method conditional evaluation is not defined for, and an answer that is not
/// one complete representation are all left exactly as they are.
pub fn apply(req: &RtRequest, method: &str, now: SystemTime, response: &mut RtResponse) {
    let conditions = Conditions::of_request(req);
    if !conditions.stated() {
        return;
    }
    if !CONDITIONAL_METHODS
        .iter()
        .any(|conditional| method.eq_ignore_ascii_case(conditional))
    {
        return;
    }
    if response.status().as_u16() != COMPLETE {
        return;
    }
    let current = holds_the_viewers_copy(
        &conditions,
        response.headers().get(ETAG),
        response.headers().get(LAST_MODIFIED),
        now,
    );
    if !current {
        return;
    }
    // None of the object's bytes, and no header that describes a body: what the viewer already
    // holds is current, so the answer states that and nothing else.
    let mut not_modified = RtResponse::new(status_code(NOT_MODIFIED), SdkBody::empty());
    for (name, value) in response.headers().iter() {
        if BODY_HEADERS
            .iter()
            .any(|body| name.eq_ignore_ascii_case(body))
        {
            continue;
        }
        let _ = not_modified
            .headers_mut()
            .try_append(name.to_string(), value.to_string());
    }
    *response = not_modified;
}

/// Whether the copy the viewer's conditions describe is the representation `etag` and
/// `last_modified` describe — the reading of RFC 9110 §13 this module's docs state.
fn holds_the_viewers_copy(
    conditions: &Conditions<'_>,
    etag: Option<&str>,
    last_modified: Option<&str>,
    now: SystemTime,
) -> bool {
    if let Some(stated) = conditions.if_none_match {
        return stated.split(',').map(str::trim).any(|candidate| {
            candidate == ANY_REPRESENTATION
                || (!candidate.is_empty()
                    && etag.is_some_and(|tag| opaque_tag(candidate) == opaque_tag(tag)))
        });
    }
    let Some(stated) = conditions.if_modified_since else {
        return false;
    };
    let (Some(since), Some(modified)) = (
        http_date(stated),
        last_modified.and_then(http_date),
    ) else {
        return false;
    };
    since <= whole_seconds(now) && modified <= since
}

/// The opaque part of one entity tag: its quoted value, with the weak marker and the
/// surrounding whitespace off, which is what the WEAK comparison compares.
fn opaque_tag(tag: &str) -> &str {
    tag.trim().strip_prefix(WEAK_MARKER).unwrap_or(tag).trim()
}

/// `date` as whole seconds since the epoch, or `None` when it is no HTTP-date at all — a value
/// "a recipient MUST ignore" (RFC 9110 §13.1.3), which this reads as a condition that is not
/// stated.
fn http_date(date: &str) -> Option<u64> {
    ::httpdate::parse_http_date(date.trim())
        .ok()
        .map(whole_seconds)
}

/// `instant` as whole seconds since the epoch — the resolution an HTTP-date carries.
fn whole_seconds(instant: SystemTime) -> u64 {
    instant
        .duration_since(UNIX_EPOCH)
        .map_or(0, |since| since.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The representation one case is judged against: a `200` carrying an entity tag and a
    /// last-modified date, plus the headers an answer the edge serves carries.
    fn representation(etag: &str, last_modified: &str) -> RtResponse {
        let mut response = RtResponse::new(
            status_code(COMPLETE),
            SdkBody::from(b"the object's bytes".to_vec()),
        );
        let mut header = |name: &'static str, value: &str| {
            let _ = response.headers_mut().try_insert(name, value.to_string());
        };
        header(ETAG, etag);
        header(LAST_MODIFIED, last_modified);
        header("content-type", "text/plain");
        header("content-length", "18");
        header("x-cache", "Hit from cloudfront");
        response
    }

    /// One viewer request carrying `headers`.
    fn viewer(headers: &[(&str, &str)]) -> RtRequest {
        let mut builder = ::http::Request::builder().method("GET").uri("/obj.txt");
        for (name, value) in headers {
            builder = builder.header(*name, *value);
        }
        RtRequest::try_from(
            builder
                .body(SdkBody::empty())
                .expect("the request is well formed"),
        )
        .expect("the request renders")
    }

    /// The object's own tag, the wildcard, a list holding it and the weak marker all say the
    /// viewer's copy is current; a tag of another representation does not.
    #[test]
    fn an_entity_tag_condition_is_judged_by_weak_comparison() {
        let etag = "\"7dc880228978d156b35173546477a11d\"";
        let other = "\"0000000000000000000000000000beef\"";
        let cases: [(&str, String, bool); 7] = [
            ("the representation's own tag", etag.to_string(), true),
            ("a tag of another representation", other.to_string(), false),
            ("the wildcard", ANY_REPRESENTATION.to_string(), true),
            (
                "a list holding the representation's tag",
                format!("{other}, {etag}"),
                true,
            ),
            (
                "a list holding no tag of the representation",
                format!("{other}, \"another\""),
                false,
            ),
            (
                "the representation's tag marked weak",
                format!("{WEAK_MARKER}{etag}"),
                true,
            ),
            ("an unquoted tag", "7dc880".to_string(), false),
        ];
        for (case, header, expected) in &cases {
            let mut response = representation(etag, "Tue, 10 Nov 2026 10:56:38 GMT");
            apply(
                &viewer(&[(IF_NONE_MATCH, header)]),
                "GET",
                SystemTime::UNIX_EPOCH,
                &mut response,
            );
            let status = response.status().as_u16();
            assert_eq!(
                status,
                if *expected { NOT_MODIFIED } else { COMPLETE },
                "case {case:?} ({header:?}): expected {}, got {status}",
                if *expected { NOT_MODIFIED } else { COMPLETE }
            );
        }
    }

    /// A `304` carries none of the object's bytes and no header describing a body, and keeps
    /// the representation's validators and the edge's own headers.
    #[test]
    fn a_not_modified_answer_carries_no_body_and_keeps_the_validators() {
        let etag = "\"abc\"";
        let mut response = representation(etag, "Tue, 10 Nov 2026 10:56:38 GMT");
        apply(
            &viewer(&[(IF_NONE_MATCH, etag)]),
            "GET",
            SystemTime::UNIX_EPOCH,
            &mut response,
        );
        assert_eq!(
            response.body().bytes().map(<[u8]>::len),
            Some(0),
            "expected none of the object's bytes, got {:?}",
            response.body().bytes().map(<[u8]>::len)
        );
        assert_eq!(
            (
                response.headers().get(ETAG),
                response.headers().get(LAST_MODIFIED),
                response.headers().get("x-cache"),
            ),
            (
                Some(etag),
                Some("Tue, 10 Nov 2026 10:56:38 GMT"),
                Some("Hit from cloudfront"),
            ),
            "expected the validators and the cache outcome kept, got {:?}",
            response.headers().iter().collect::<Vec<_>>()
        );
        for body in BODY_HEADERS {
            assert_eq!(
                response.headers().get(body),
                None,
                "expected a 304 to state no {body}, got {:?}",
                response.headers().get(body)
            );
        }
    }

    /// One row of the date table: what it states, the conditions the viewer sends, and the
    /// status the edge is to answer.
    struct DateCase {
        case: &'static str,
        headers: Vec<(&'static str, &'static str)>,
        expected: u16,
    }

    /// `If-Modified-Since` is read only when no `If-None-Match` is present, at whole-second
    /// resolution, and a date later than the clock is ignored.
    #[test]
    fn a_date_condition_is_read_at_second_resolution_and_only_on_its_own() {
        let etag = "\"abc\"";
        let modified = "Tue, 10 Nov 2026 10:56:38 GMT";
        let now = ::httpdate::parse_http_date("Tue, 10 Nov 2026 12:00:00 GMT")
            .expect("the suite's clock reading is an HTTP-date");
        let cases = [
            DateCase {
                case: "the same instant the representation was modified at",
                headers: vec![(IF_MODIFIED_SINCE, modified)],
                expected: NOT_MODIFIED,
            },
            DateCase {
                case: "an instant after the representation was modified",
                headers: vec![(IF_MODIFIED_SINCE, "Tue, 10 Nov 2026 11:00:00 GMT")],
                expected: NOT_MODIFIED,
            },
            DateCase {
                case: "an instant before the representation was modified",
                headers: vec![(IF_MODIFIED_SINCE, "Tue, 10 Nov 2026 09:00:00 GMT")],
                expected: COMPLETE,
            },
            DateCase {
                case: "a date later than the edge's own clock",
                headers: vec![(IF_MODIFIED_SINCE, "Wed, 11 Nov 2026 10:56:38 GMT")],
                expected: COMPLETE,
            },
            DateCase {
                case: "a value that is no HTTP-date",
                headers: vec![(IF_MODIFIED_SINCE, "yesterday")],
                expected: COMPLETE,
            },
            DateCase {
                case: "a date that holds beside an entity tag that does not",
                headers: vec![
                    (IF_NONE_MATCH, "\"not-the-tag\""),
                    (IF_MODIFIED_SINCE, modified),
                ],
                expected: COMPLETE,
            },
        ];
        for row in &cases {
            let mut response = representation(etag, modified);
            apply(&viewer(&row.headers), "GET", now, &mut response);
            assert_eq!(
                response.status().as_u16(),
                row.expected,
                "case {:?}: expected {}, got {}",
                row.case,
                row.expected,
                response.status().as_u16()
            );
        }
    }

    /// An answer that is no complete representation, and a method conditional evaluation is not
    /// defined for, are left alone.
    #[test]
    fn an_answer_that_is_no_representation_is_left_alone() {
        let etag = "\"abc\"";
        let mut refused = RtResponse::new(status_code(403), SdkBody::from(b"<Error/>".to_vec()));
        let _ = refused.headers_mut().try_insert(ETAG, etag.to_string());
        apply(
            &viewer(&[(IF_NONE_MATCH, etag)]),
            "GET",
            SystemTime::UNIX_EPOCH,
            &mut refused,
        );
        assert_eq!(
            refused.status().as_u16(),
            403,
            "expected the origin's own refusal untouched, got {}",
            refused.status().as_u16()
        );

        let mut written = representation(etag, "Tue, 10 Nov 2026 10:56:38 GMT");
        apply(
            &viewer(&[(IF_NONE_MATCH, etag)]),
            "PUT",
            SystemTime::UNIX_EPOCH,
            &mut written,
        );
        assert_eq!(
            written.status().as_u16(),
            COMPLETE,
            "expected a method conditional evaluation is not defined for to be served, got {}",
            written.status().as_u16()
        );
    }
}
