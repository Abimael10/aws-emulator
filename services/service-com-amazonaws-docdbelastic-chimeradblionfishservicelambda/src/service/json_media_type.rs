// SPDX-License-Identifier: CC0-1.0

//! The media type this service ANNOUNCES a JSON payload with, in the `content-type`
//! response header.
//!
//! This service's wire is JSON over REST, and the framework's JSON renderers announce
//! every payload they write as the generic `application/json`, whatever the request asked
//! for. This service NEGOTIATES that media type instead: a response carrying a JSON
//! payload is announced with the JSON media type the REQUEST named for its own payload,
//! and with this service's native AWS JSON 1.1 media type when the request carried no
//! payload and so named none.
//!
//! ```text
//! GET /clusters                              POST /cluster
//! (no content-type: no request payload)      content-type: application/json
//!
//! HTTP/1.1 200 OK                            HTTP/1.1 200 OK
//! content-type: application/x-amz-json-1.1   content-type: application/json
//! ```
//!
//! The negotiation is per RESPONSE, not per operation: the same operation answers in the
//! media type each of its callers named, and a success and a failure of one request are
//! announced alike — so a client that reads `content-type` back sees the JSON dialect it
//! asked for, and one that asked for nothing sees this service's own.
//!
//! ## The ARN-addressed tagging surface answers in the generic JSON media type
//!
//! One SURFACE of this service does not negotiate: the three operations that address a
//! resource generically, by the ARN of any class this service owns, announce every payload
//! they answer with as the generic `application/json`, whatever the request named.
//!
//! ```text
//! GET /tags/{resourceArn}
//! (no content-type: no request payload)
//!
//! HTTP/1.1 404 Not Found
//! content-type: application/json
//! ```
//!
//! That surface is model-derived rather than enumerated here: `TagResource`,
//! `UntagResource` and `ListTagsForResource` are the only operations of this service bound
//! under `/tags`, and they are also the only ones modelled with their own
//! `…Request`/`…Response` shapes rather than this service's `…Input`/`…Output` pair
//! (`smithy_query(query="digest")`), so the route is what tells the two surfaces apart —
//! not a list of operation names that a renamed or added tagging operation would leave
//! stale.
//!
//! Nothing else about a response changes: the status, the body and every other header
//! stay as the renderer wrote them, and the payload itself keeps every byte — only the
//! media type the response announces that payload with is restated.
//!
//! The header belongs to the RESPONSE, not to any modelled shape, so no handler can set
//! it. [`with_json_media_type`] is the one place it is set, and
//! [`super::error_type_locator`] applies it at the two layers every response of this
//! service passes through, so one operation's success and another's refusal cannot
//! announce two media types for one request.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};

/// The header a payload's media type is named in, on a request and on a response alike.
const CONTENT_TYPE: &str = "content-type";

/// The media type this service announces a JSON payload with when the request named none.
pub const JSON_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// The generic JSON media type: what the ARN-addressed tagging surface announces every
/// payload with, and what a request carrying a JSON payload usually names.
pub const GENERIC_JSON_MEDIA_TYPE: &str = "application/json";

/// The path the ARN-addressed tagging operations are bound under, with the `/` that
/// separates it from the `@httpLabel` carrying the ARN: all three of them are
/// `/tags/{resourceArn}` and nothing else of this service is bound under `/tags`
/// (`smithy_query(query="digest")`).
const TAGGING_PATH_PREFIX: &str = "/tags/";

/// Whether `req` addresses the ARN-addressed tagging surface — the route all three tagging
/// operations share, read off the request rather than off the operation that serves it, so
/// a response no operation produced (a refusal raised before dispatch) is announced the
/// same way as one an operation answered.
///
/// The literal segment is compared exactly, as the framework's REST router compares the
/// literal segments of a modelled URI: a path the router does not resolve to a tagging
/// operation is not this surface, so the two cannot disagree about one request.
fn addresses_tagging_surface(req: &RtRequest) -> bool {
    ::service_common::request_path(req.uri()).starts_with(TAGGING_PATH_PREFIX)
}

/// The media type `named` states, without the parameters that follow it (a `charset`,
/// say) and case-folded, since a media type is case-insensitive.
fn media_type_of(named: &str) -> String {
    named
        .split(';')
        .next()
        .unwrap_or(named)
        .trim()
        .to_ascii_lowercase()
}

/// Whether `media_type` is a JSON one — the generic media type, or any AWS JSON version.
///
/// This is what tells a JSON payload from any other: a renderer that announced one wrote
/// JSON, and a request that named one carries a JSON payload.
fn is_json(media_type: &str) -> bool {
    media_type == "application/json" || media_type.starts_with("application/x-amz-json")
}

/// The media type a response to `req` announces its JSON payload with: the generic
/// [`GENERIC_JSON_MEDIA_TYPE`] for the ARN-addressed tagging surface, and otherwise the
/// JSON media type `req` named for its own payload, or [`JSON_MEDIA_TYPE`] when it named
/// none (or named one that is not JSON, which no payload of this wire is).
pub fn announced_media_type(req: &RtRequest) -> String {
    if addresses_tagging_surface(req) {
        return GENERIC_JSON_MEDIA_TYPE.to_string();
    }
    req.headers()
        .get(CONTENT_TYPE)
        .map(media_type_of)
        .filter(|media_type| is_json(media_type))
        .unwrap_or_else(|| JSON_MEDIA_TYPE.to_string())
}

/// `resp` with its JSON payload announced as `media_type`.
///
/// A response the renderer announced as something other than JSON is returned untouched,
/// as is one that announced no media type at all (a body-less answer): this restates the
/// media type of a JSON payload and invents no header a renderer did not write.
/// IDEMPOTENT for one request's `media_type`, so a response passing through both layers
/// announces exactly one.
pub fn with_json_media_type(mut resp: RtResponse, media_type: &str) -> RtResponse {
    if !resp
        .headers()
        .get(CONTENT_TYPE)
        .map(media_type_of)
        .is_some_and(|announced| is_json(&announced))
    {
        return resp;
    }
    let _ = resp.headers_mut().try_insert(CONTENT_TYPE, media_type.to_string());
    resp
}

#[cfg(test)]
mod tests {
    //! The header over the forms a request and a renderer can name a media type in. The
    //! wire behaviour — that every response of every operation announces the negotiated
    //! media type — is asserted through the router in `tests/all/response_media_type.rs`.

    use super::*;
    use aws_smithy_runtime_api::http::StatusCode;
    use aws_smithy_types::body::SdkBody;

    /// One request, built from the media type it named for its own payload.
    fn request(content_type: Option<&str>) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::empty());
        if let Some(content_type) = content_type {
            req.headers_mut().insert(CONTENT_TYPE, content_type.to_string());
        }
        req
    }

    /// The same request, addressed at `uri`.
    fn addressed(uri: &str, content_type: Option<&str>) -> RtRequest {
        let mut req = request(content_type);
        req.set_uri(uri).expect("a valid request URI");
        req
    }

    /// One response, built from a status and the media type its renderer announced.
    fn response(status: u16, content_type: Option<&str>) -> RtResponse {
        let mut resp = RtResponse::new(
            StatusCode::try_from(status).expect("a valid status"),
            SdkBody::from(r#"{"clusters":[]}"#),
        );
        if let Some(content_type) = content_type {
            resp.headers_mut().insert(CONTENT_TYPE, content_type.to_string());
        }
        resp
    }

    /// A route of this service's OWN operations, which negotiate the media type
    /// (`smithy_query(query="digest")`: `ListClusters` is `GET /clusters`).
    const RESOURCE_ROUTE: &str = "/clusters";

    /// The route all three ARN-addressed tagging operations share, with the ARN
    /// percent-encoded into its `@httpLabel` the way an SDK sends it.
    const TAGGING_ROUTE: &str =
        "/tags/arn%3Aaws%3Adocdb-elastic%3Aeu-west-2%3A000000000000%3Acluster%2Fabc";

    /// The media type to announce follows the request: the JSON dialect it named for its
    /// own payload, and this service's own when it named none or named no JSON at all.
    #[test]
    fn the_announced_media_type_is_the_one_the_request_named() {
        for (case, named, expected) in [
            ("no payload", None, JSON_MEDIA_TYPE),
            ("the generic JSON media type", Some("application/json"), "application/json"),
            (
                "a JSON media type with a parameter",
                Some("application/json; charset=utf-8"),
                "application/json",
            ),
            ("an uppercase media type", Some("Application/JSON"), "application/json"),
            (
                "another AWS JSON version",
                Some("application/x-amz-json-1.0"),
                "application/x-amz-json-1.0",
            ),
            ("this service's own", Some(JSON_MEDIA_TYPE), JSON_MEDIA_TYPE),
            ("a media type that is not JSON", Some("application/xml"), JSON_MEDIA_TYPE),
        ] {
            let actual = announced_media_type(&addressed(RESOURCE_ROUTE, named));
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?}, got {actual:?}"
            );
        }
    }

    /// The ARN-addressed tagging surface does not negotiate: whatever the request named,
    /// and whether it named anything at all, its answer is announced in the generic JSON
    /// media type. The rows span the media types a request can name and both forms the
    /// request URI arrives in (origin and absolute).
    #[test]
    fn the_tagging_surface_is_announced_in_the_generic_json_media_type() {
        let absolute = format!("https://docdb-elastic.eu-west-2.amazonaws.com{TAGGING_ROUTE}");
        for (case, uri, named) in [
            ("no payload", TAGGING_ROUTE, None),
            ("the generic JSON media type", TAGGING_ROUTE, Some("application/json")),
            ("this service's own", TAGGING_ROUTE, Some(JSON_MEDIA_TYPE)),
            (
                "another AWS JSON version",
                TAGGING_ROUTE,
                Some("application/x-amz-json-1.0"),
            ),
            ("a media type that is not JSON", TAGGING_ROUTE, Some("application/xml")),
            ("a query string beside the label", "/tags/abc?tagKeys=Project", None),
            ("an absolute request URI", absolute.as_str(), None),
        ] {
            let actual = announced_media_type(&addressed(uri, named));
            assert_eq!(
                actual, GENERIC_JSON_MEDIA_TYPE,
                "case {case:?}: expected {GENERIC_JSON_MEDIA_TYPE:?}, got {actual:?}"
            );
        }
    }

    /// The scope of that one: a route outside the tagging surface keeps negotiating, so
    /// the surface is told apart by the route the model binds and not by a prefix match
    /// that catches the rest of the service.
    #[test]
    fn a_route_outside_the_tagging_surface_still_negotiates() {
        for (case, uri) in [
            ("a listing", RESOURCE_ROUTE),
            ("a cluster's own route", "/cluster/abc"),
            ("a snapshot listing", "/cluster-snapshots"),
            ("the root", "/"),
            // `/tags` alone binds no operation of this service: only `/tags/{resourceArn}`
            // does, so the prefix includes the separator.
            ("the tagging path with no label", "/tags"),
            ("a route that merely starts with the same letters", "/tagsets/abc"),
        ] {
            let actual = announced_media_type(&addressed(uri, None));
            assert_eq!(
                actual, JSON_MEDIA_TYPE,
                "case {case:?}: expected {JSON_MEDIA_TYPE:?}, got {actual:?}"
            );
        }
    }

    /// Every JSON payload, at every status, is announced with the negotiated media type,
    /// and restating a media type a response already announces leaves exactly one.
    #[test]
    fn a_json_payload_is_announced_with_the_negotiated_media_type() {
        for (case, status, rendered) in [
            ("a success", 200, "application/json"),
            ("a refusal", 404, "application/json"),
            ("a server failure", 500, "application/json"),
            ("an uppercase media type", 200, "Application/JSON"),
            ("a media type with a parameter", 200, "application/json; charset=utf-8"),
            ("already this media type", 404, JSON_MEDIA_TYPE),
        ] {
            let answered = with_json_media_type(response(status, Some(rendered)), JSON_MEDIA_TYPE);
            assert_eq!(
                answered.headers().get(CONTENT_TYPE),
                Some(JSON_MEDIA_TYPE),
                "case {case:?}: expected {JSON_MEDIA_TYPE:?}, got {:?}",
                answered.headers().get(CONTENT_TYPE)
            );
            assert_eq!(
                answered.status().as_u16(),
                status,
                "case {case:?}: the status must not move, got {}",
                answered.status().as_u16()
            );
        }
    }

    /// Nothing to restate: a payload that is not JSON, and a response announcing no
    /// media type, are left exactly as they were.
    #[test]
    fn a_response_that_announces_no_json_is_left_as_it_was() {
        for (case, rendered) in [
            ("no media type", None),
            ("an XML payload", Some("application/xml")),
            ("an octet stream", Some("application/octet-stream")),
            ("plain text", Some("text/plain")),
        ] {
            let answered = with_json_media_type(response(200, rendered), JSON_MEDIA_TYPE);
            assert_eq!(
                answered.headers().get(CONTENT_TYPE),
                rendered,
                "case {case:?}: expected the header left as it was ({rendered:?}), got {:?}",
                answered.headers().get(CONTENT_TYPE)
            );
        }
    }
}
