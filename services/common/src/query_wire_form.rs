// SPDX-License-Identifier: CC0-1.0

//! The wire form every AWS Query API request arrives in, and the claim a service makes on
//! one that names no service at all.
//!
//! An AWS Query API request is a `POST` of an `application/x-www-form-urlencoded` body to
//! the ROOT path, with the operation in the body's `Action` field. The protocol publishes
//! no `smithy.api#http` binding and no `X-Amz-Target`, so that method, that path and that
//! media type are the whole signature such a request carries in its request line and
//! headers.
//!
//! That signature is what routing has left for some of these requests. The framework
//! resolves a request's routing key from a SigV4 credential scope, an `X-Amz-Target` or a
//! service-naming endpoint host, and a query request can carry NONE of the three:
//!
//! * an operation whose model lists no auth scheme is sent UNSIGNED, so there is no
//!   credential scope to read a signing name from;
//! * an API still signed with Signature Version 2 carries its credential in the
//!   form-encoded body rather than in an `Authorization` header, so a conforming client
//!   of one sends no scope either; and
//! * a client pointed at this emulator's own address reaches it on a loopback host, which
//!   names no service.
//!
//! [`addresses_unattributed_query_request`] is the claim such a request is admitted by:
//! the wire form, and nothing in the request naming any service. Nothing weaker is
//! claimed — a request that resolves to SOME service's routing key is that service's
//! however it is addressed, and a request on another path or method, or carrying another
//! body type, reaches nobody exactly as before.
//!
//! Claiming by the wire form alone is not deciding: the form is shared by every query
//! API, so two services can claim one request. The router then keeps the claimants that
//! name an OPERATION for it — `Service::override_operation`, the one seam that sees the
//! BODY, where the `Action` and `Version` fields say which API the request is for — so a
//! service claiming here has to recognise its OWN API's request there, and no other's.
//! [`names_no_service`] is how that seam tells the two kinds of claim apart.

use aws_smithy_runtime_api::http::Request as RtRequest;

/// The media type an AWS Query API request carries its form-encoded body in.
pub const QUERY_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// Whether `parts` addresses the AWS Query API wire form while naming NO service at all.
///
/// See the [module docs](self) for why a request can name none, and for what a service
/// that claims one still has to decide from the body.
pub fn addresses_unattributed_query_request(parts: &http::request::Parts) -> bool {
    if ::emulator_server::resolve_route_key(parts).is_some() {
        // SOMETHING in the request names a service — a SigV4 credential scope, an
        // `X-Amz-Target`, or an endpoint host. That request is the named service's,
        // however it is addressed, and a claim on it is that service's own to make.
        return false;
    }
    addresses_query_wire_form(parts)
}

/// Whether `parts` has the wire form every AWS Query API request has: a `POST` of a
/// form-encoded body to the root path.
fn addresses_query_wire_form(parts: &http::request::Parts) -> bool {
    parts.method == http::Method::POST
        && parts.uri.path().trim_matches('/').is_empty()
        && declares_form_encoded_body(parts)
}

/// Whether `req` names NO service — the same tiered rule
/// [`addresses_unattributed_query_request`] applies, over the runtime-API request instead
/// of over `Parts`.
///
/// A service that claims the shared wire form at `Service::can_handle` (which is handed
/// `Parts`) has to know, at `Service::override_operation` (which is handed the runtime-API
/// request), whether THIS request is one of those: a request that names this service is its
/// own however it is written, while one that names none is claimed by the form alone and has
/// to state which API it is for. The framework's resolver takes `Parts`, so the parts are
/// lifted off the request — the same lift `crate::unusable_credential::presented_access_key_id`
/// makes for the same reason — rather than the tiered rule being written a second time.
///
/// A request whose method or URI will not rebuild reads as naming a service, which is the
/// stricter answer of the two.
#[must_use]
pub fn names_no_service(req: &RtRequest) -> bool {
    let mut builder = ::http::Request::builder().method(req.method()).uri(req.uri());
    for (name, value) in req.headers().iter() {
        builder = builder.header(name, value);
    }
    let Ok(request) = builder.body(()) else {
        return false;
    };
    let (parts, ()) = request.into_parts();
    ::emulator_server::resolve_route_key(&parts).is_none()
}

/// Whether `parts` declares a form-encoded body: the media type, with whatever parameters
/// (a `charset`, as an SDK sends) follow it.
fn declares_form_encoded_body(parts: &http::request::Parts) -> bool {
    parts
        .headers
        .get(http::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| {
            value
                .split(';')
                .next()
                .unwrap_or(value)
                .trim()
                .eq_ignore_ascii_case(QUERY_CONTENT_TYPE)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One request's parts: how it is addressed, and what it declares.
    fn parts(
        method: &str,
        uri: &str,
        content_type: Option<&str>,
        authorization: Option<&str>,
        host: &str,
    ) -> http::request::Parts {
        let mut builder = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", host);
        if let Some(content_type) = content_type {
            builder = builder.header("content-type", content_type);
        }
        if let Some(authorization) = authorization {
            builder = builder.header("authorization", authorization);
        }
        builder
            .body(())
            .expect("a well-formed request")
            .into_parts()
            .0
    }

    /// The emulator's own address, which names no service.
    const LOOPBACK: &str = "127.0.0.1:9297";

    /// One row: how the request is addressed and what it declares, and whether the claim
    /// admits it.
    struct Row<'a> {
        case: &'a str,
        method: &'a str,
        uri: &'a str,
        content_type: Option<&'a str>,
        authorization: Option<&'a str>,
        host: &'a str,
        claimed: bool,
    }

    #[test]
    fn the_query_wire_form_is_claimed_only_when_nothing_names_a_service() {
        let rows: Vec<Row<'_>> = vec![
            Row {
                case: "an unsigned form POST of the root path on this emulator's address",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                claimed: true,
            },
            Row {
                case: "the same POST with a charset on the media type",
                method: "POST",
                uri: "/",
                content_type: Some("application/x-www-form-urlencoded; charset=utf-8"),
                authorization: None,
                host: LOOPBACK,
                claimed: true,
            },
            Row {
                case: "the same POST with the media type cased differently",
                method: "POST",
                uri: "/",
                content_type: Some("Application/X-WWW-Form-Urlencoded"),
                authorization: None,
                host: LOOPBACK,
                claimed: true,
            },
            Row {
                case: "the same POST of a path written with redundant separators",
                method: "POST",
                uri: "//",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                claimed: true,
            },
            Row {
                case: "a signed request, which names its service in the credential scope",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: Some(
                    "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/sts/\
                     aws4_request, SignedHeaders=host, Signature=x",
                ),
                host: LOOPBACK,
                claimed: false,
            },
            Row {
                case: "an unsigned request addressed by a service-naming endpoint host",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: "sts.us-east-1.amazonaws.com",
                claimed: false,
            },
            Row {
                case: "a GET of the root path, which no query request is",
                method: "GET",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                claimed: false,
            },
            Row {
                case: "a POST of a path no query request uses",
                method: "POST",
                uri: "/2011-06-15/assume-role",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                claimed: false,
            },
            Row {
                case: "a POST declaring a JSON body",
                method: "POST",
                uri: "/",
                content_type: Some("application/json"),
                authorization: None,
                host: LOOPBACK,
                claimed: false,
            },
            Row {
                case: "a POST declaring no body type at all",
                method: "POST",
                uri: "/",
                content_type: None,
                authorization: None,
                host: LOOPBACK,
                claimed: false,
            },
        ];

        for row in rows {
            let parts = parts(
                row.method,
                row.uri,
                row.content_type,
                row.authorization,
                row.host,
            );
            let actual = addresses_unattributed_query_request(&parts);
            assert_eq!(
                actual, row.claimed,
                "case {:?}: expected the claim to be {}, got {actual}",
                row.case, row.claimed
            );
        }
    }
}
