// SPDX-License-Identifier: CC0-1.0

//! A PROBE of the browser-origin refusal on this endpoint's non-modelled surfaces: what the
//! OAuth2 endpoints of a pool's domain answer a request a browser sent from another site.
//!
//! The rule under test: a web page cannot drive this emulator. The token and revocation
//! endpoints take a form-encoded body, which a page may send cross-site with no preflight to
//! stop it, and they mint and revoke credentials — so each of them refuses a request the
//! browser-set headers classify as cross-site (`Origin`, `Sec-Fetch-Site`), by the same
//! reading the core applies to every request it serves itself, before any of them acts. A
//! client that is no browser sets neither header and is served, and a page's own
//! same-origin request is served too.
//!
//! The rows differ along which headers the request carries and which endpoint it addresses.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// A request's SigV4 credential scope is what routes the modelled calls of this suite to the
/// service. The signature is never checked here: the suite turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// The loopback address a browser of this emulator reaches it on.
const LOCAL_HOST: &str = "127.0.0.1:9297";

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::new(
        engine::InMemoryEngine::new(),
    )));
    router.set_request_authentication(false);
    router
}

/// Call `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", "cognito-idp.us-east-1.amazonaws.com")
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        )
        .header("authorization", AUTHORIZATION)
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// Send a form-encoded POST to `path` with `headers`, and report the status and the parsed
/// body.
async fn post_form(
    router: &MultiServiceRouter,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> (u16, Value) {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(path)
        .header("host", LOCAL_HOST)
        .header("content-type", "application/x-www-form-urlencoded");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(Bytes::from(body.to_string()))
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    (status, payload)
}

/// One row: the case and the headers the request carries.
type HeaderRow<'r> = (&'r str, &'r [(&'r str, &'r str)]);

/// A request a browser sent from another site is refused on every endpoint of a pool's
/// authorization server, while the same request from a non-browser client, and a page's own
/// same-origin request, are served.
#[tokio::test]
async fn a_cross_site_browser_request_is_refused_on_every_hosted_endpoint() {
    let router = router();
    let user_pool_id = ok(
        &router,
        "CreateUserPool",
        &json!({ "PoolName": "cross-site-probe-pool" }),
    )
    .await["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();
    let client_id = ok(
        &router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "cross-site-probe-client",
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();
    let domain = "cross-site-probe";
    ok(
        &router,
        "CreateUserPoolDomain",
        &json!({ "UserPoolId": user_pool_id, "Domain": domain }),
    )
    .await;

    // The rows differ along the headers a browser sets: a page of another site, a page whose
    // browser classifies the request itself, a sandboxed page, and a client that is no
    // browser at all.
    let refused: [HeaderRow<'_>; 4] = [
        (
            "a page of another site, by its Origin alone",
            &[("origin", "https://attacker.example")],
        ),
        (
            "a page whose browser classifies the request cross-site",
            &[
                ("origin", "https://attacker.example"),
                ("sec-fetch-site", "cross-site"),
            ],
        ),
        (
            "a page of the same site but another origin",
            &[("sec-fetch-site", "same-site"), ("origin", "https://sub.example")],
        ),
        ("a sandboxed page, whose origin is null", &[("origin", "null")]),
    ];
    let endpoints = [
        ("the token endpoint", format!("/{domain}/oauth2/token")),
        ("the revocation endpoint", format!("/{domain}/oauth2/revoke")),
        ("the user-info endpoint", format!("/{domain}/oauth2/userInfo")),
    ];

    for (case, headers) in refused {
        for (endpoint, path) in &endpoints {
            let (status, payload) = post_form(
                &router,
                path,
                headers,
                &format!("grant_type=refresh_token&client_id={client_id}&token=nothing"),
            )
            .await;
            assert_eq!(
                (status, payload["error"].as_str()),
                (403, Some("invalid_request")),
                "case {case:?} on {endpoint}: expected the cross-site refusal, got {status} \
                 with {payload}"
            );
        }
    }

    // A client that is no browser sets neither header, and a page's own same-origin request
    // is one the core serves too: both reach the endpoint, which answers the OAuth2 refusal
    // of the grant itself rather than the cross-site one.
    let served: [HeaderRow<'_>; 3] = [
        ("a client that sets neither header", &[]),
        (
            "a page of this same origin",
            &[("sec-fetch-site", "same-origin"), ("origin", "https://127.0.0.1:9297")],
        ),
        ("a request a browser made of its own accord", &[("sec-fetch-site", "none")]),
    ];
    for (case, headers) in served {
        let (status, payload) = post_form(
            &router,
            &format!("/{domain}/oauth2/token"),
            headers,
            &format!("grant_type=refresh_token&client_id={client_id}"),
        )
        .await;
        assert_eq!(
            (status, payload["error"].as_str()),
            (400, Some("invalid_request")),
            "case {case:?}: expected the endpoint to serve the request and refuse the grant \
             for the field it left out, got {status} with {payload}"
        );
    }
}
