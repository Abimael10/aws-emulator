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
//! [`addresses_query_request`] is the claim such a request is admitted by: the wire form,
//! and nothing in the request naming ANOTHER service. Nothing weaker is claimed — a request
//! that resolves to some other service's routing key is that service's however it is
//! addressed, and a request on another path or method, or carrying another body type,
//! reaches nobody exactly as before.
//!
//! Claiming by the wire form alone is not deciding: the form is shared by every query
//! API, so two services can claim one request. The router then keeps the claimants that
//! name an OPERATION for it — `Service::override_operation`, the one seam that sees the
//! BODY, where the `Action` and `Version` fields say which API the request is for — so a
//! service claiming here has to recognise its OWN API's request there, and no other's.
//! [`names_no_service`] is how that seam tells the two kinds of claim apart.
//!
//! ## An `X-Amz-Target` is not one of this protocol's routing signals
//!
//! The framework's tiered resolver reads an `X-Amz-Target` ahead of the endpoint host, and
//! that header belongs to the `awsJson` protocols: it names an operation out of band, which
//! is precisely what a query API does NOT do — the operation is the body's `Action`. A
//! client, proxy or SDK middleware that attaches one to a query request (and anything
//! replaying a captured request that carries it) therefore made the request resolve to the
//! target's prefix, a key no query service answers to, so the request reached NOBODY even
//! though its endpoint and its `Action` name a service that is emulated.
//!
//! [`addresses_query_request`] is the claim that reads a query request the way its own
//! protocol addresses it: the wire form, plus the signals this protocol does carry — the
//! SigV4 credential scope and the endpoint host — with the target left out of them. A
//! target naming THIS service, or naming none, is carried along and ignored; one naming
//! another service is left to that service, so a request cannot be claimed by two services
//! at once over a header neither protocol agrees about.

use aws_smithy_runtime_api::http::Request as RtRequest;

/// The media type an AWS Query API request carries its form-encoded body in.
pub const QUERY_CONTENT_TYPE: &str = "application/x-www-form-urlencoded";

/// Whether `parts` is an AWS Query API request addressed to the service whose SigV4
/// signing name is `signing_name` and whose service shape is `service_shape_id`, read with
/// `X-Amz-Target` left out of the routing signals.
///
/// The claim a query service makes on the wire form, stated over the signals the query
/// protocol actually carries (see the [module docs](self)):
///
/// * the request has the wire form — a `POST` of a form-encoded body to the root path;
/// * an `X-Amz-Target` beside it names THIS service or no service at all, so nothing in
///   the request names another service that would claim it too; and
/// * the signals this protocol does carry — the SigV4 credential scope, then the endpoint
///   host or `:authority` — name this service, or name no service at all, in which case
///   the wire form claims it alone (see the [module docs](self) for why a conforming query
///   request can name none).
///
/// The operation is still the body's `Action`, resolved by the service's own protocol
/// wherever this claim admitted the request from: nothing here reads a target's operation
/// segment, and a target naming an operation no model binds changes no answer.
///
/// `service_shape_id` is the service shape's id (or its local name), as
/// `definition().identity().shape_id()` states it; the target's service segment is read
/// against it through [`crate::rpc_target::names_service`], this crate's one reading of
/// that header.
#[must_use]
pub fn addresses_query_request(
    parts: &http::request::Parts,
    signing_name: &str,
    service_shape_id: &str,
) -> bool {
    if !addresses_query_wire_form(parts) {
        return false;
    }
    if !target_names_this_service_or_none(parts, service_shape_id) {
        return false;
    }
    match route_key_without_target(parts) {
        Some(key) => key.eq_ignore_ascii_case(signing_name.trim()),
        None => true,
    }
}

/// The service the request names through a signal the QUERY protocol carries: the SigV4
/// credential scope, in either of SigV4's two presentations, then the endpoint host and the
/// HTTP/2 `:authority`.
///
/// The framework's own `resolve_route_key` applies the same tiers with `X-Amz-Target`
/// between them; each tier is read here through the framework's own reader for it, so the
/// only difference is the tier a query request has no use for.
fn route_key_without_target(parts: &http::request::Parts) -> Option<String> {
    ::emulator_server::service_from_sigv4(&parts.headers)
        .or_else(|| ::emulator_server::service_from_sigv4_query(&parts.uri))
        .or_else(|| ::emulator_server::service_from_host(&parts.headers))
        .or_else(|| ::emulator_server::service_from_authority(&parts.uri))
}

/// Whether the request's `X-Amz-Target`, if it carries a readable one, names the service
/// whose shape is `service_shape_id`.
///
/// A request carrying none names no service there, and so does one whose header value is
/// not text — which is also what the framework's resolver reads out of it.
fn target_names_this_service_or_none(
    parts: &http::request::Parts,
    service_shape_id: &str,
) -> bool {
    parts
        .headers
        .get(crate::rpc_target::TARGET_HEADER)
        .and_then(|target| target.to_str().ok())
        .is_none_or(|target| crate::rpc_target::names_service(target, service_shape_id))
}

/// Whether `parts` has the wire form every AWS Query API request has: a `POST` of a
/// form-encoded body to the root path.
fn addresses_query_wire_form(parts: &http::request::Parts) -> bool {
    parts.method == http::Method::POST
        && parts.uri.path().trim_matches('/').is_empty()
        && declares_form_encoded_body(parts)
}

/// Whether `req` names NO service — the same tiered rule
/// [`addresses_query_request`] applies to the signals a request carries, over the
/// runtime-API request instead of over `Parts`.
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

    /// The emulator's own address, which names no service.
    const LOOPBACK: &str = "127.0.0.1:9297";

    /// One query service's names, for the rows below: a signing name and the service shape
    /// its API is declared by.
    const SIGNING_NAME: &str = "sts";
    const SERVICE_SHAPE_ID: &str = "com.amazonaws.sts#AWSSecurityTokenServiceV20110615";

    /// One request's parts, with the `X-Amz-Target` a client may have attached to it.
    fn parts(
        method: &str,
        uri: &str,
        content_type: Option<&str>,
        authorization: Option<&str>,
        host: &str,
        target: Option<&str>,
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
        if let Some(target) = target {
            builder = builder.header(crate::rpc_target::TARGET_HEADER, target);
        }
        builder
            .body(())
            .expect("a well-formed request")
            .into_parts()
            .0
    }

    /// A query request is claimed by the service its OWN protocol's signals name, and an
    /// `X-Amz-Target` beside them is carried along rather than routed on.
    ///
    /// The rows differ along what the rule generalises over: whether a target is present,
    /// the FORM the target's prefix is written in (the shape's local name, the same name
    /// cased differently, the fully qualified Coral name whose prefix carries dots of its
    /// own, a target naming an operation no model binds), which service the target names,
    /// which other signal names a service (none, the endpoint host, the credential scope),
    /// and the wire form itself (the method, the path, and how the media type is written).
    #[test]
    fn a_query_request_is_claimed_by_its_own_signals_whatever_target_it_carries() {
        /// One row: how the request is addressed, the target attached to it, and whether
        /// this service claims it.
        struct Row<'a> {
            case: &'a str,
            method: &'a str,
            uri: &'a str,
            content_type: Option<&'a str>,
            authorization: Option<&'a str>,
            host: &'a str,
            target: Option<&'a str>,
            claimed: bool,
        }
        let scope = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/sts/\
                     aws4_request, SignedHeaders=host, Signature=x";
        let other_scope = "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/us-east-1/iam/\
                           aws4_request, SignedHeaders=host, Signature=x";
        let rows: Vec<Row<'_>> = vec![
            Row {
                case: "an unsigned form POST naming nothing at all",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                target: None,
                claimed: true,
            },
            Row {
                case: "the same POST carrying this service's own shape as a target",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                target: Some("AWSSecurityTokenServiceV20110615.GetCallerIdentity"),
                claimed: true,
            },
            Row {
                case: "a target naming this service, cased as no model spells it",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                target: Some("awssecuritytokenservicev20110615.GetSessionToken"),
                claimed: true,
            },
            Row {
                // A fully qualified Coral prefix, whose dots are not operation separators.
                case: "a target naming this service by its fully qualified name",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                target: Some(
                    "com.amazonaws.sts.AWSSecurityTokenServiceV20110615.GetCallerIdentity",
                ),
                claimed: true,
            },
            Row {
                // The operation segment is not read at all: the body's `Action` is the call.
                case: "a target naming an operation no model binds",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                target: Some("AWSSecurityTokenServiceV20110615.NotAnOperation"),
                claimed: true,
            },
            Row {
                case: "this service's endpoint host, with a target naming it too",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: "sts.us-east-1.amazonaws.com",
                target: Some("AWSSecurityTokenServiceV20110615.GetCallerIdentity"),
                claimed: true,
            },
            Row {
                case: "a credential scope naming this service, with a target beside it",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: Some(scope),
                host: LOOPBACK,
                target: Some("AWSSecurityTokenServiceV20110615.GetCallerIdentity"),
                claimed: true,
            },
            Row {
                // Another service's target: left to that service, so one request is never
                // claimed by two services over a header the protocols disagree about.
                case: "a target naming another service",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                target: Some("AWSCognitoIdentityService.GetId"),
                claimed: false,
            },
            Row {
                case: "another service's endpoint host",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: "iam.amazonaws.com",
                target: None,
                claimed: false,
            },
            Row {
                case: "another service's credential scope, with this service's target",
                method: "POST",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: Some(other_scope),
                host: LOOPBACK,
                target: Some("AWSSecurityTokenServiceV20110615.GetCallerIdentity"),
                claimed: false,
            },
            Row {
                case: "a GET of the root path, which no query request is",
                method: "GET",
                uri: "/",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                target: Some("AWSSecurityTokenServiceV20110615.GetCallerIdentity"),
                claimed: false,
            },
            Row {
                case: "a form POST of a path no query request uses",
                method: "POST",
                uri: "/2011-06-15/assume-role",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                target: Some("AWSSecurityTokenServiceV20110615.GetCallerIdentity"),
                claimed: false,
            },
            Row {
                case: "a POST declaring a JSON body",
                method: "POST",
                uri: "/",
                content_type: Some("application/json"),
                authorization: None,
                host: LOOPBACK,
                target: Some("AWSSecurityTokenServiceV20110615.GetCallerIdentity"),
                claimed: false,
            },
            Row {
                case: "a POST declaring no body type at all",
                method: "POST",
                uri: "/",
                content_type: None,
                authorization: None,
                host: LOOPBACK,
                target: None,
                claimed: false,
            },
            Row {
                case: "a form POST with a charset on the media type",
                method: "POST",
                uri: "/",
                content_type: Some("application/x-www-form-urlencoded; charset=utf-8"),
                authorization: None,
                host: LOOPBACK,
                target: None,
                claimed: true,
            },
            Row {
                case: "a form POST with the media type cased differently",
                method: "POST",
                uri: "/",
                content_type: Some("Application/X-WWW-Form-Urlencoded"),
                authorization: None,
                host: LOOPBACK,
                target: None,
                claimed: true,
            },
            Row {
                case: "a form POST of a root path written with redundant separators",
                method: "POST",
                uri: "//",
                content_type: Some(QUERY_CONTENT_TYPE),
                authorization: None,
                host: LOOPBACK,
                target: Some("AWSSecurityTokenServiceV20110615.GetCallerIdentity"),
                claimed: true,
            },
        ];

        for row in rows {
            let parts = parts(
                row.method,
                row.uri,
                row.content_type,
                row.authorization,
                row.host,
                row.target,
            );
            let actual = addresses_query_request(&parts, SIGNING_NAME, SERVICE_SHAPE_ID);
            assert_eq!(
                actual, row.claimed,
                "case {:?}: expected the claim to be {}, got {actual}",
                row.case, row.claimed
            );
        }
    }
}
