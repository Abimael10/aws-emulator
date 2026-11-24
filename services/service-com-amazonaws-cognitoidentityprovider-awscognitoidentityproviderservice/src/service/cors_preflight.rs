// SPDX-License-Identifier: CC0-1.0

//! The CORS PREFLIGHT this endpoint answers: what a user-pool API endpoint tells a browser
//! about the request it is about to send.
//!
//! Several operations of this API are called with NO credentials — `InitiateAuth`, `SignUp`,
//! `ConfirmSignUp`, `ForgotPassword` and the rest of the sign-in surface carry
//! `@optionalAuth` with no auth scheme, since a client has no credentials until it has
//! signed in — so the user-pool endpoint is called straight from a web page, by Amplify and
//! the JavaScript SDK. A browser about to send such a request, which no simple form could
//! send (it carries `X-Amz-Target` and a JSON media type), asks the endpoint first: "the
//! browser sends a preflight `OPTIONS` request" carrying the origin it acts for (`Origin`),
//! the method it intends to use ([`REQUEST_METHOD_HEADER`]) and, when it intends to send
//! headers of its own, their names ([`REQUEST_HEADERS_HEADER`]).
//!
//! The answer is the SAME for every caller, because the endpoint shares its API with every
//! origin: the recorded answer of the public user-pool endpoint is `200` with an empty body,
//! [`ALLOW_ORIGIN`] as the origin a page may act for, the method the browser asked about
//! echoed back, the headers it asked about echoed back verbatim (and no such header at all
//! when it asked about none), the [`EXPOSE_HEADERS`] a page may read off the answer, and
//! [`MAX_AGE_SECONDS`] as how long it may cache this answer.
//!
//! A preflight that asks about a method but states NO `Origin` asks a question no answer
//! fits: a CORS permission is a permission for one origin, and there is none to answer for.
//! Such a request is no preflight, and it names no operation of this API and carries no
//! credential either — it has neither `X-Amz-Target` nor an `Authorization` header — so the
//! endpoint answers it the refusal an unauthenticated request of this API earns:
//! [`MISSING_ORIGIN_STATUS`] with [`MISSING_AUTHENTICATION_TOKEN_CLASS`] and
//! [`MISSING_AUTHENTICATION_TOKEN_MESSAGE`], stated the way every refusal of this endpoint
//! states its class — in the body's `__type` and in the header an AWS client resolves one
//! from ([`super::error_wire`]) — and no CORS permission on it.
//!
//! The preflight is UNSIGNED — a browser cannot sign it — and it names no operation of this
//! model, so no routing key resolves it and it is answered through
//! [`super::EmulatedService::override_routing_error`], the seam the router consults for a
//! request that reached this service and named no modelled operation. An `OPTIONS` request
//! that is not a preflight (it states no origin, or asks about no method) asks this endpoint
//! nothing a CORS answer could answer, so it keeps the refusal every unrouted request earns.

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse, StatusCode};
use ::aws_smithy_types::body::SdkBody;

/// The method a CORS preflight is sent with.
const PREFLIGHT_METHOD: &str = "OPTIONS";

/// The header a preflight states the origin the page acts for in.
const ORIGIN_HEADER: &str = "origin";

/// The header a preflight states the method it is asking about in (Fetch,
/// "CORS-preflight request").
const REQUEST_METHOD_HEADER: &str = "access-control-request-method";

/// The header a preflight lists the request headers it intends to send in.
const REQUEST_HEADERS_HEADER: &str = "access-control-request-headers";

/// The status this endpoint answers a preflight with.
const PREFLIGHT_STATUS: u16 = 200;

/// The status this endpoint refuses a preflight that states no `Origin` with: the request
/// asks about a cross-origin call while naming no origin to answer for, and it presents no
/// credential and names no operation either — which is the status every refusal of a caller
/// of this endpoint carries.
const MISSING_ORIGIN_STATUS: u16 = 400;

/// The error class that refusal names: the one this endpoint answers a request that presents
/// no credential with.
const MISSING_AUTHENTICATION_TOKEN_CLASS: &str = "MissingAuthenticationTokenException";

/// The sentence it carries: fixed text, with nothing of the request in it.
const MISSING_AUTHENTICATION_TOKEN_MESSAGE: &str = "Missing Authentication Token";

/// The media type this protocol's documents travel in (`awsJson1_1`, this service's own
/// protocol).
const JSON_MEDIA_TYPE: &str = "application/x-amz-json-1.1";

/// The response header naming the origins a page may act for, and the one value this
/// endpoint answers it with: the API is shared with every origin, so the answer is the
/// wildcard rather than the origin that asked — which is what lets one answer be cached for
/// every page.
const ALLOW_ORIGIN_HEADER: &str = "access-control-allow-origin";
/// See [`ALLOW_ORIGIN_HEADER`].
const ALLOW_ORIGIN: &str = "*";

/// The response header naming the methods the page may use, which carries the method the
/// preflight asked about.
const ALLOW_METHODS_HEADER: &str = "access-control-allow-methods";

/// The response header naming the request headers the page may send, which carries the list
/// the preflight asked about, as the preflight spelled it.
const ALLOW_HEADERS_HEADER: &str = "access-control-allow-headers";

/// The response header naming the response headers a page may READ off an answer of this
/// endpoint, and the ones it names: the id of the request a response answers, the error
/// class a refusal carries and its message, and the date the answer was made — the four a
/// client of this API reads beside the body.
const EXPOSE_HEADERS_HEADER: &str = "access-control-expose-headers";
/// See [`EXPOSE_HEADERS_HEADER`].
const EXPOSE_HEADERS: &str = "x-amzn-RequestId,x-amzn-ErrorType,x-amzn-ErrorMessage,Date";

/// The response header saying how long a browser may cache this answer, and the value this
/// endpoint answers it with, in seconds.
const MAX_AGE_HEADER: &str = "access-control-max-age";
/// See [`MAX_AGE_HEADER`].
const MAX_AGE_SECONDS: u32 = 172_800;

/// The length of the empty body the answer carries.
const EMPTY_BODY_LENGTH: &str = "0";

/// What a preflight ASKS this endpoint: the method the browser intends to use, and the
/// request headers it intends to send where it names any.
///
/// The origin is read to decide that the request IS a preflight and is not carried: the
/// answer states the same `Access-Control-Allow-Origin` for every origin, so nothing of the
/// asking page's address reaches the response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Preflight<'r> {
    /// The method [`REQUEST_METHOD_HEADER`] asks about.
    method: &'r str,
    /// The headers [`REQUEST_HEADERS_HEADER`] asks about, as the preflight spelled them, or
    /// `None` when it asked about none.
    headers: Option<&'r str>,
}

/// The CORS question `req` asks this endpoint, or `None` when it asks none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Asked<'r> {
    /// A preflight: it states the origin it acts for and the method it asks about.
    Preflight(Preflight<'r>),
    /// A preflight-shaped request that states no origin, so there is no origin to answer a
    /// permission for.
    OriginMissing,
}

/// The CORS question `req` asks, or `None` when it asks none.
///
/// A preflight is an `OPTIONS` request to the one path this endpoint's operations are posted
/// to — its ROOT — asking about a method ([`REQUEST_METHOD_HEADER`], the header a browser
/// states its intended method in): the Fetch standard's own definition of the request. An
/// `OPTIONS` request that asks about no method at all, or that is addressed to one of the
/// paths this endpoint serves documents at, asks nothing a CORS answer could answer and keeps
/// the refusal every unrouted request earns.
fn asked_of(req: &RtRequest) -> Option<Asked<'_>> {
    if !req.method().eq_ignore_ascii_case(PREFLIGHT_METHOD) {
        return None;
    }
    if !addresses_api_root(req.uri()) {
        return None;
    }
    let header = |name: &str| {
        req.headers()
            .get(name)
            .map(str::trim)
            .filter(|value| !value.is_empty())
    };
    let method = header(REQUEST_METHOD_HEADER)?;
    if header(ORIGIN_HEADER).is_none() {
        return Some(Asked::OriginMissing);
    }
    Some(Asked::Preflight(Preflight {
        method,
        headers: header(REQUEST_HEADERS_HEADER),
    }))
}

/// Whether `uri` addresses the path this endpoint's operations are posted to, which is the
/// path a preflight for one of them asks about: the root, as every `awsJson` operation is
/// bound to ([`super::request_path::addresses_root`], the one reader this service's
/// non-modelled seams share).
fn addresses_api_root(uri: &str) -> bool {
    super::request_path::addresses_root(uri)
}

/// Whether `parts` asks this endpoint a CORS question — the request [`answer`] answers.
///
/// Read over the buffered parts, which is what the claiming seam
/// ([`super::EmulatedService::can_handle`]) is handed, by the same rule [`asked_of`]
/// applies to the request dispatch builds: the method, the root path and the header a browser
/// states its intended method in. Both readings are here so the request this service CLAIMS
/// and the request it ANSWERS cannot drift apart.
pub fn asks_a_cors_question(parts: &::http::request::Parts) -> bool {
    if !parts.method.as_str().eq_ignore_ascii_case(PREFLIGHT_METHOD) {
        return false;
    }
    if !addresses_api_root(&parts.uri.to_string()) {
        return false;
    }
    parts
        .headers
        .get(REQUEST_METHOD_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .is_some_and(|value| !value.is_empty())
}

/// What this endpoint answers the CORS question `req` asks, or `None` for a request that asks
/// none — which keeps whatever answer the caller would otherwise give.
///
/// A pure READ of the request: the answer is this endpoint's own standing statement about
/// the API it shares, so no state is consulted and none is written. The browser is told the
/// method and the headers it ASKED about, which is what it has to be told for the request it
/// is about to send; nothing else of the request is echoed.
pub fn answer(req: &RtRequest) -> Option<RtResponse> {
    let asked = match asked_of(req)? {
        Asked::Preflight(asked) => asked,
        // No origin to answer a permission for: refused the way a request that presents no
        // credential is, with no permission on the answer. The class travels in the body's
        // `__type` and in the header an AWS client resolves one from, as every refusal of
        // this endpoint states it ([`super::error_wire`]).
        Asked::OriginMissing => {
            let status = StatusCode::try_from(MISSING_ORIGIN_STATUS).ok()?;
            let document = ::serde_json::json!({
                "__type": MISSING_AUTHENTICATION_TOKEN_CLASS,
                "message": MISSING_AUTHENTICATION_TOKEN_MESSAGE,
            });
            let mut response = RtResponse::new(status, SdkBody::from(document.to_string()));
            insert_header(&mut response, "content-type", JSON_MEDIA_TYPE);
            return Some(super::error_wire::naming_the_error_shape(
                response,
                MISSING_AUTHENTICATION_TOKEN_CLASS,
            ));
        }
    };
    let status = StatusCode::try_from(PREFLIGHT_STATUS).ok()?;
    let mut response = RtResponse::new(status, SdkBody::empty());
    let mut stated: Vec<(&'static str, String)> = vec![
        (ALLOW_ORIGIN_HEADER, ALLOW_ORIGIN.to_string()),
        (ALLOW_METHODS_HEADER, asked.method.to_string()),
        (EXPOSE_HEADERS_HEADER, EXPOSE_HEADERS.to_string()),
        (MAX_AGE_HEADER, MAX_AGE_SECONDS.to_string()),
        ("content-length", EMPTY_BODY_LENGTH.to_string()),
    ];
    // Stated only when the browser asked about headers of its own: a preflight that named
    // none is answered with no `Access-Control-Allow-Headers` at all rather than with an
    // empty permission.
    if let Some(headers) = asked.headers {
        stated.push((ALLOW_HEADERS_HEADER, headers.to_string()));
    }
    for (name, value) in stated {
        insert_header(&mut response, name, &value);
    }
    Some(response)
}

/// Set one response header, dropping a value the wire cannot carry rather than turning an
/// answered preflight into a transport failure; the two values read from the REQUEST are the
/// only ones that could hold such a value.
fn insert_header(response: &mut RtResponse, name: &'static str, value: &str) {
    if let Err(refused) = response.headers_mut().try_insert(name, value.to_string()) {
        ::tracing::info!(
            header = name,
            error = %refused,
            "this header cannot be sent on the wire and is left off the preflight answer"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One `OPTIONS` request: the path it addresses and the headers it carries.
    fn options(path: &str, headers: &[(&str, &str)]) -> RtRequest {
        let mut req = RtRequest::new(SdkBody::empty());
        req.set_method(PREFLIGHT_METHOD).expect("a valid method");
        req.set_uri(path).expect("a valid URI");
        for (name, value) in headers {
            req.headers_mut()
                .append((*name).to_string(), (*value).to_string());
        }
        req
    }

    /// One row of [`a_cors_question_is_an_options_of_the_api_root_asking_about_a_method`]:
    /// the case, the path the request addresses, the headers it carries, and the status this
    /// endpoint answers it (or `None` for a request it answers not at all).
    type QuestionRow<'r> = (
        &'r str,
        &'r str,
        &'r [(&'r str, &'r str)],
        Option<u16>,
    );

    /// Which `OPTIONS` requests ask this endpoint a CORS question, and what each is answered:
    /// the rows differ along the path, which of the two headers a browser sends is present,
    /// and whether a present one carries a value at all.
    #[test]
    fn a_cors_question_is_an_options_of_the_api_root_asking_about_a_method() {
        let origin = ("origin", "https://app.example.com");
        let asks_post = (REQUEST_METHOD_HEADER, "POST");
        let rows: [QuestionRow<'_>; 8] = [
            ("the root path", "/", &[origin, asks_post], Some(PREFLIGHT_STATUS)),
            (
                "the root path with a query",
                "/?x=1",
                &[origin, asks_post],
                Some(PREFLIGHT_STATUS),
            ),
            (
                "an absolute form of the root",
                "https://cognito-idp.us-east-1.amazonaws.com/",
                &[origin, asks_post],
                Some(PREFLIGHT_STATUS),
            ),
            (
                "a document path of a pool",
                "/us-east-1_abc123def/.well-known/jwks.json",
                &[origin, asks_post],
                None,
            ),
            // A preflight-shaped request with no origin to answer for is refused.
            ("no origin", "/", &[asks_post], Some(MISSING_ORIGIN_STATUS)),
            ("no method asked about", "/", &[origin], None),
            (
                "an empty origin",
                "/",
                &[("origin", "  "), asks_post],
                Some(MISSING_ORIGIN_STATUS),
            ),
            (
                "an empty method",
                "/",
                &[origin, (REQUEST_METHOD_HEADER, " ")],
                None,
            ),
        ];
        for (case, path, headers, expected) in rows {
            let answered = answer(&options(path, headers));
            let status = answered.as_ref().map(|resp| resp.status().as_u16());
            assert_eq!(
                status, expected,
                "case {case:?}: expected this endpoint to answer {expected:?}, got {status:?}"
            );
            // A refusal carries no permission: only an answered preflight states one.
            if status == Some(MISSING_ORIGIN_STATUS) {
                let stated = answered
                    .as_ref()
                    .and_then(|resp| resp.headers().get(ALLOW_ORIGIN_HEADER))
                    .map(str::to_string);
                assert_eq!(
                    stated, None,
                    "case {case:?}: expected no permission on a refused preflight, got \
                     {stated:?}"
                );
            }
        }
    }

    /// One row of [`the_answer_echoes_what_the_browser_asked_about_and_states_this_endpoints_own_terms`]:
    /// the case, the headers the preflight carries, the method it asks about, and the header
    /// list the answer must echo.
    type EchoRow<'r> = (
        &'r str,
        &'r [(&'r str, &'r str)],
        &'r str,
        Option<&'r str>,
    );

    /// What the answer states: the fixed permissions of this endpoint, the method asked
    /// about, and the header list asked about — only when one was asked about.
    #[test]
    fn the_answer_echoes_what_the_browser_asked_about_and_states_this_endpoints_own_terms() {
        let origin = ("origin", "https://app.example.com");
        let rows: [EchoRow<'_>; 3] = [
            (
                "a POST asking about no headers",
                &[origin, (REQUEST_METHOD_HEADER, "POST")],
                "POST",
                None,
            ),
            (
                "a PUT asking about two headers",
                &[
                    origin,
                    (REQUEST_METHOD_HEADER, "PUT"),
                    (REQUEST_HEADERS_HEADER, "content-type,x-amz-target"),
                ],
                "PUT",
                Some("content-type,x-amz-target"),
            ),
            (
                "a POST asking about one header, spelled as the browser spelled it",
                &[
                    origin,
                    (REQUEST_METHOD_HEADER, "POST"),
                    (REQUEST_HEADERS_HEADER, "X-Amz-User-Agent"),
                ],
                "POST",
                Some("X-Amz-User-Agent"),
            ),
        ];
        for (case, headers, method, asked_headers) in rows {
            let response = answer(&options("/", headers)).expect("this request is a preflight");
            let stated = |name: &str| response.headers().get(name).map(str::to_string);
            assert_eq!(
                response.status().as_u16(),
                PREFLIGHT_STATUS,
                "case {case:?}: expected the preflight to be answered {PREFLIGHT_STATUS}, got \
                 {:?}",
                response.status()
            );
            assert_eq!(
                stated(ALLOW_ORIGIN_HEADER),
                Some(ALLOW_ORIGIN.to_string()),
                "case {case:?}: expected the API to be shared with every origin, got {:?}",
                stated(ALLOW_ORIGIN_HEADER)
            );
            assert_eq!(
                stated(ALLOW_METHODS_HEADER),
                Some(method.to_string()),
                "case {case:?}: expected the method asked about to be echoed, got {:?}",
                stated(ALLOW_METHODS_HEADER)
            );
            assert_eq!(
                stated(ALLOW_HEADERS_HEADER),
                asked_headers.map(str::to_string),
                "case {case:?}: expected the headers asked about to be echoed verbatim, got {:?}",
                stated(ALLOW_HEADERS_HEADER)
            );
            assert_eq!(
                stated(EXPOSE_HEADERS_HEADER),
                Some(EXPOSE_HEADERS.to_string()),
                "case {case:?}: expected the readable response headers to be stated, got {:?}",
                stated(EXPOSE_HEADERS_HEADER)
            );
            assert_eq!(
                stated(MAX_AGE_HEADER),
                Some(MAX_AGE_SECONDS.to_string()),
                "case {case:?}: expected the cache lifetime to be stated, got {:?}",
                stated(MAX_AGE_HEADER)
            );
            assert!(
                response.body().bytes().is_some_and(<[u8]>::is_empty),
                "case {case:?}: expected an empty body, got {:?}",
                response.body().bytes()
            );
        }
    }
}
