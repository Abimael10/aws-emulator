// SPDX-License-Identifier: CC0-1.0

//! The CORS PREFLIGHT of the user-pool API endpoint, over the wire: what a browser is told
//! before it sends a call to this API.
//!
//! The rule under test: an `OPTIONS` request to the path this API's operations are posted to
//! — the endpoint's root — that carries an `Origin` and an `Access-Control-Request-Method` is
//! answered `200` with an empty body, `Access-Control-Allow-Origin: *`,
//! `Access-Control-Allow-Methods` echoing the method asked about,
//! `Access-Control-Allow-Headers` echoing `Access-Control-Request-Headers` verbatim (and
//! absent when the preflight asked about none),
//! `Access-Control-Expose-Headers: x-amzn-RequestId,x-amzn-ErrorType,x-amzn-ErrorMessage,Date`
//! and `Access-Control-Max-Age: 172800`.
//!
//! Such a request is UNSIGNED and carries no `X-Amz-Target` — it is asking whether that very
//! header may be sent — so nothing in it names a service, which is why it has to be claimed
//! and answered by the endpoint's own seams. The rows therefore drive it both the way a
//! browser of this emulator addresses it (a loopback `Host`, out of which the framework
//! reads no service at all) and the way one that resolves AWS's own hostnames does.
//!
//! An `OPTIONS` request that asks about a method but states NO `Origin` asks a question no
//! permission fits, and is answered `400` with no permission on it. One that asks about no
//! method at all, or that addresses a document path rather than the API, asks nothing a CORS
//! answer could answer and keeps the refusal every request naming no operation earns.
//!
//! Each row names the case it asserts and every assertion reports the actual value.

use std::sync::Arc;

use bytes::Bytes;
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// The response headers a preflight answer carries, and the fixed values two of them hold.
const ALLOW_ORIGIN: &str = "access-control-allow-origin";
const ALLOW_METHODS: &str = "access-control-allow-methods";
const ALLOW_HEADERS: &str = "access-control-allow-headers";
const EXPOSE_HEADERS: &str = "access-control-expose-headers";
const MAX_AGE: &str = "access-control-max-age";

/// The origins this API is shared with, as the answer states them.
const EVERY_ORIGIN: &str = "*";

/// The response headers a page may read off an answer of this endpoint.
const EXPOSED: &str = "x-amzn-RequestId,x-amzn-ErrorType,x-amzn-ErrorMessage,Date";

/// How long a browser may cache one answer, in seconds.
const CACHE_SECONDS: &str = "172800";

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(engine));
    router.set_request_authentication(false);
    router
}

/// The answer to one `OPTIONS` request: its status, its headers and its body.
struct Answered {
    status: u16,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl Answered {
    /// The one value `name` carries, or `None` when the answer states no such header.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(stated, _)| stated.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// How many bytes the answer's body carries.
    fn body_length(&self) -> usize {
        self.body.len()
    }
}

/// Send `OPTIONS path` to the emulator with `headers` and report what it answered.
async fn options(router: &MultiServiceRouter, host: &str, path: &str, headers: &[(&str, &str)]) -> Answered {
    let mut request = http::Request::builder()
        .method("OPTIONS")
        .uri(path)
        .header("host", host);
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Bytes::new())
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    Answered {
        status: response.status().as_u16(),
        headers: response
            .headers()
            .iter()
            .map(|(name, value)| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or_default().to_string(),
                )
            })
            .collect(),
        body: response.body().to_vec(),
    }
}

/// One preflight row: the case, the host it is addressed to, the method it asks about, the
/// header list it asks about, and the `Access-Control-Allow-Headers` the answer must state.
struct PreflightCase<'a> {
    case: &'a str,
    host: &'a str,
    asks_method: &'a str,
    asks_headers: Option<&'a str>,
    allowed_headers: Option<&'a str>,
}

/// A browser addresses this emulator by the loopback endpoint it was started on, which names
/// no service at all; a client that resolves AWS's own hostnames addresses the published one.
const LOOPBACK_HOST: &str = "127.0.0.1:9297";
const PUBLISHED_HOST: &str = "cognito-idp.us-west-2.amazonaws.com";

/// A preflight is answered the endpoint's own terms, with the method and the header list it
/// asked about echoed back. The rows differ along the host the preflight is addressed to, the
/// method it asks about, and the number and spelling of the headers it asks about.
#[tokio::test]
async fn a_preflight_of_the_api_is_answered_with_this_endpoints_cors_terms() {
    let router = router();
    let rows = [
        PreflightCase {
            case: "a browser of this emulator asking about POST and no headers",
            host: LOOPBACK_HOST,
            asks_method: "POST",
            asks_headers: None,
            allowed_headers: None,
        },
        PreflightCase {
            case: "a browser asking about POST and the two headers an SDK call sends",
            host: LOOPBACK_HOST,
            asks_method: "POST",
            asks_headers: Some("content-type,x-amz-target"),
            allowed_headers: Some("content-type,x-amz-target"),
        },
        PreflightCase {
            case: "a browser asking about a header spelled in mixed case",
            host: LOOPBACK_HOST,
            asks_method: "POST",
            asks_headers: Some("X-Amz-User-Agent"),
            allowed_headers: Some("X-Amz-User-Agent"),
        },
        PreflightCase {
            case: "a browser asking about PUT",
            host: LOOPBACK_HOST,
            asks_method: "PUT",
            asks_headers: None,
            allowed_headers: None,
        },
        PreflightCase {
            case: "a client that resolves the published endpoint host",
            host: PUBLISHED_HOST,
            asks_method: "POST",
            asks_headers: Some("content-type"),
            allowed_headers: Some("content-type"),
        },
        PreflightCase {
            case: "a browser asking about four headers at once",
            host: PUBLISHED_HOST,
            asks_method: "POST",
            asks_headers: Some("content-type,x-amz-target,x-amz-user-agent,x-amz-date"),
            allowed_headers: Some("content-type,x-amz-target,x-amz-user-agent,x-amz-date"),
        },
    ];

    for row in rows {
        let case = row.case;
        let mut headers = vec![
            ("origin", "https://app.example.com"),
            ("access-control-request-method", row.asks_method),
        ];
        if let Some(asked) = row.asks_headers {
            headers.push(("access-control-request-headers", asked));
        }
        let answered = options(&router, row.host, "/", &headers).await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: expected the preflight to be answered 200, got {}",
            answered.status
        );
        assert_eq!(
            answered.body_length(),
            0,
            "case {case:?}: expected an empty body, got {} bytes",
            answered.body_length()
        );
        assert_eq!(
            answered.header(ALLOW_ORIGIN),
            Some(EVERY_ORIGIN),
            "case {case:?}: expected the API to be shared with every origin, got {:?}",
            answered.header(ALLOW_ORIGIN)
        );
        assert_eq!(
            answered.header(ALLOW_METHODS),
            Some(row.asks_method),
            "case {case:?}: expected the method asked about to be echoed, got {:?}",
            answered.header(ALLOW_METHODS)
        );
        assert_eq!(
            answered.header(ALLOW_HEADERS),
            row.allowed_headers,
            "case {case:?}: expected the headers asked about to be echoed verbatim, got {:?}",
            answered.header(ALLOW_HEADERS)
        );
        assert_eq!(
            answered.header(EXPOSE_HEADERS),
            Some(EXPOSED),
            "case {case:?}: expected the readable response headers to be stated, got {:?}",
            answered.header(EXPOSE_HEADERS)
        );
        assert_eq!(
            answered.header(MAX_AGE),
            Some(CACHE_SECONDS),
            "case {case:?}: expected the cache lifetime to be stated, got {:?}",
            answered.header(MAX_AGE)
        );
    }
}

/// One row of [`a_preflight_that_names_no_origin_is_refused_without_a_permission`]: the case
/// and the headers the request carries.
type NoOriginRow<'r> = (&'r str, &'r [(&'r str, &'r str)]);

/// An `OPTIONS` request that asks about a method while naming no origin is refused 400, with
/// no permission on the answer: there is no origin for a CORS permission to be about.
#[tokio::test]
async fn a_preflight_that_names_no_origin_is_refused_without_a_permission() {
    let router = router();
    let rows: [NoOriginRow<'_>; 3] = [
        (
            "an OPTIONS that asks about POST and states no origin",
            &[("access-control-request-method", "POST")],
        ),
        (
            "an OPTIONS that asks about POST and about headers, and states no origin",
            &[
                ("access-control-request-method", "POST"),
                ("access-control-request-headers", "content-type"),
            ],
        ),
        (
            "an OPTIONS whose origin is empty",
            &[("origin", "   "), ("access-control-request-method", "POST")],
        ),
    ];
    for (case, headers) in rows {
        let answered = options(&router, LOOPBACK_HOST, "/", headers).await;
        assert_eq!(
            answered.status, 400,
            "case {case:?}: expected the preflight to be refused 400, got {}",
            answered.status
        );
        for header in [ALLOW_ORIGIN, ALLOW_METHODS, ALLOW_HEADERS, EXPOSE_HEADERS, MAX_AGE] {
            assert_eq!(
                answered.header(header),
                None,
                "case {case:?}: expected no {header} on a refused preflight, got {:?}",
                answered.header(header)
            );
        }
        let document: serde_json::Value = serde_json::from_slice(&answered.body)
            .unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: expected the refusal to carry a JSON document ({error}), got \
                     {:?}",
                    String::from_utf8_lossy(&answered.body)
                )
            });
        assert_eq!(
            document,
            serde_json::json!({
                "__type": "MissingAuthenticationTokenException",
                "message": "Missing Authentication Token",
            }),
            "case {case:?}: expected the refusal an unauthenticated request of this endpoint \
             earns, got {document}"
        );
        assert_eq!(
            answered.header("x-amzn-errortype"),
            Some("MissingAuthenticationTokenException:"),
            "case {case:?}: expected the refusal to name its error shape in the header a \
             client resolves it from, got {:?}",
            answered.header("x-amzn-errortype")
        );
    }
}

/// One row of [`an_options_request_that_is_no_preflight_is_not_answered_a_cors_permission`]:
/// the case, the path the request addresses and the headers it carries.
type NoQuestionRow<'r> = (&'r str, &'r str, &'r [(&'r str, &'r str)]);

/// An `OPTIONS` request that asks no CORS question is not answered one: it asks about no
/// method, or addresses a path the API's operations are not posted to.
#[tokio::test]
async fn an_options_request_that_is_no_preflight_is_not_answered_a_cors_permission() {
    let router = router();
    let rows: [NoQuestionRow<'_>; 4] = [
        (
            "an OPTIONS that states an origin but asks about no method",
            "/",
            &[("origin", "https://app.example.com")],
        ),
        ("an OPTIONS of the API that carries neither", "/", &[]),
        (
            "a preflight of a pool's key-set document path",
            "/us-west-2_abc123def/.well-known/jwks.json",
            &[
                ("origin", "https://app.example.com"),
                ("access-control-request-method", "GET"),
            ],
        ),
        (
            "an OPTIONS asking about an empty method",
            "/",
            &[
                ("origin", "https://app.example.com"),
                ("access-control-request-method", " "),
            ],
        ),
    ];

    for (case, path, headers) in rows {
        let answered = options(&router, LOOPBACK_HOST, path, headers).await;
        assert_ne!(
            answered.status, 200,
            "case {case:?}: expected no CORS permission to be answered, got 200 with {:?}",
            answered.headers
        );
        for header in [ALLOW_ORIGIN, ALLOW_METHODS, ALLOW_HEADERS, MAX_AGE] {
            assert_eq!(
                answered.header(header),
                None,
                "case {case:?}: expected no {header} on an answer that is no CORS permission, \
                 got {:?}",
                answered.header(header)
            );
        }
    }
}

/// The preflight answer is a READ of the request alone: the same question asked twice is
/// answered identically, and asking it leaves the endpoint serving its operations exactly as
/// before.
#[tokio::test]
async fn asking_the_same_preflight_twice_is_answered_the_same_and_changes_nothing() {
    let router = router();
    let asked = [
        ("origin", "https://app.example.com"),
        ("access-control-request-method", "POST"),
        ("access-control-request-headers", "content-type,x-amz-target"),
    ];
    let first = options(&router, LOOPBACK_HOST, "/", &asked).await;
    let again = options(&router, LOOPBACK_HOST, "/", &asked).await;
    for header in [ALLOW_ORIGIN, ALLOW_METHODS, ALLOW_HEADERS, EXPOSE_HEADERS, MAX_AGE] {
        assert_eq!(
            first.header(header),
            again.header(header),
            "expected a repeated preflight to state the same {header}, got {:?} then {:?}",
            first.header(header),
            again.header(header)
        );
    }
    assert_eq!(
        (first.status, again.status),
        (200, 200),
        "expected both preflights to be answered 200, got {} then {}",
        first.status,
        again.status
    );

    // The endpoint still serves its operations: a preflight is a question, not a call.
    let created = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", PUBLISHED_HOST)
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            "AWSCognitoIdentityProviderService.CreateUserPool",
        )
        .body(Bytes::from(r#"{"PoolName":"cors-preflight-pool"}"#))
        .expect("the request is well formed");
    let response = router.dispatch(created).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "expected the endpoint to keep serving its operations, got {} with {:?}",
        response.status(),
        String::from_utf8_lossy(response.body())
    );
}
