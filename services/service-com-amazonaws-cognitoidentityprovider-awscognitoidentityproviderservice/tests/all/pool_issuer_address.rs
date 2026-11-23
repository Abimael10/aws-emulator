// SPDX-License-Identifier: CC0-1.0

//! End-to-end HTTP tests for the ADDRESS a user pool publishes its token ISSUER on.
//!
//! The rule, one level above the recorded case (a pool created through a client configured
//! with the emulator's own endpoint, whose tokens announced an issuer that client could not
//! fetch anything from):
//!
//! > A pool's token issuer is a URL its relying parties FETCH — the `iss` claim of every
//! > token it mints is the discovery root a verifier reads `<iss>/.well-known/jwks.json`
//! > from — so it names the endpoint of THIS emulator the pool was created through, with
//! > the pool's id as the whole path, and the documents it announces are served there. Only
//! > a pool created by a client that resolves AWS's own hostnames, on an emulator that is
//! > served on no port at all, has no address of this emulator to publish, and announces
//! > the regional form its model documents instead.
//!
//! The rows span what the rule generalises over: the endpoint the creating request was
//! addressed at (an IPv4 loopback literal with a port, a loopback NAME on another port, a
//! bracketed IPv6 literal, this service's own published regional host, and a request naming
//! no host at all), the Region the pool lives in, and the issuer configuration it holds
//! (none, `ORIGINAL`, `UPDATED`). The recorded case is ONE row.
//!
//! What each row asserts is the whole round trip the issuer exists for: both signed tokens
//! announce it, the discovery document fetched FROM it names it and locates the key set
//! under it, and the key set fetched from that locator holds the key the token's `kid`
//! names. Every fetch is addressed at the authority the issuer itself publishes and carries
//! no credential, exactly as a relying party's would.
//!
//! `token_issuer_endpoint` holds the rest of the same member's rule: one pool announces ONE
//! issuer however each LATER caller addresses this emulator, and
//! `pool_issuer_is_reachable_when_served` the half of this one for an emulator that IS served
//! on a port. The parts of the fixture that are not about the endpoint a request reached — the
//! wire prefix, the credential scope, the anonymous fetch and the JOSE reading — are
//! `pub(crate)` here and shared with that suite, so the sign-in fixture is written once.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// The local name of this service's Smithy shape, which is the `X-Amz-Target` prefix a
/// client writes.
pub(crate) const TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// This service's SigV4 signing name, the 4th field of a credential scope.
pub(crate) const SIGNING_NAME: &str = "cognito-idp";

/// A password satisfying a default pool's password policy, written for this suite.
pub(crate) const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The user every row signs in.
pub(crate) const USERNAME: &str = "heron";

/// The two members of `AuthenticationResult` that carry a SIGNED token, both of which
/// announce the minting pool's issuer.
pub(crate) const TOKEN_MEMBERS: [&str; 2] = ["IdToken", "AccessToken"];

/// The path under an issuer its OpenID Connect discovery document is published at, and the
/// path under it its key set is: the two well-known locations every OIDC relying party
/// reads (RFC 8414 §3, and the Amazon Cognito Developer Guide's "Verifying a JSON web
/// token").
pub(crate) const OPENID_CONFIGURATION: &str = "/.well-known/openid-configuration";
pub(crate) const JWKS: &str = "/.well-known/jwks.json";

/// One row: the endpoint the creating request is addressed at, the Region it is signed for,
/// the issuer type it configures, and whether the pool therefore publishes its issuer on an
/// address of this emulator.
struct Row {
    case: &'static str,
    /// The `Host` the creating request carries — `None` for a request naming no host.
    host: Option<&'static str>,
    region: &'static str,
    /// The `IssuerConfiguration.Type` the create asks for, or `None` for a pool that
    /// configures none.
    issuer_type: Option<&'static str>,
    /// Whether the creating client resolves nothing but the endpoint it reached, which is
    /// the address the pool then publishes.
    reaches_only_the_emulator: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "the recorded case: a client configured with the emulator's loopback \
                   endpoint",
            host: Some("127.0.0.1:45923"),
            region: "us-west-2",
            issuer_type: None,
            reaches_only_the_emulator: true,
        },
        Row {
            case: "the same endpoint named by its loopback NAME on another port",
            host: Some("localhost:9297"),
            region: "eu-west-1",
            issuer_type: Some("ORIGINAL"),
            reaches_only_the_emulator: true,
        },
        Row {
            case: "the same endpoint named by a bracketed IPv6 literal",
            host: Some("[::1]:9297"),
            region: "ap-southeast-2",
            issuer_type: Some("UPDATED"),
            reaches_only_the_emulator: true,
        },
        Row {
            case: "a client that resolves AWS's own hosts: this service's regional endpoint",
            host: Some("cognito-idp.us-east-1.amazonaws.com"),
            region: "us-east-1",
            issuer_type: None,
            reaches_only_the_emulator: false,
        },
        Row {
            case: "a client that resolves AWS's own hosts, with the updated issuer in \
                   another Region",
            host: Some("cognito-idp.eu-north-1.amazonaws.com"),
            region: "eu-north-1",
            issuer_type: Some("UPDATED"),
            reaches_only_the_emulator: false,
        },
        Row {
            case: "a request naming no host at all, which addresses no endpoint to publish",
            host: None,
            region: "us-east-2",
            issuer_type: None,
            reaches_only_the_emulator: false,
        },
    ]
}

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::new(
        engine::InMemoryEngine::new(),
    )));
    // Signature verification belongs to the emulator, not to this check.
    router.set_request_authentication(false);
    router
}

/// A credential scope naming this service in `region`, which is what routes a call here
/// whatever host it addressed — and what decides the Region a created pool lives in.
pub(crate) fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{SIGNING_NAME}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// Call `operation` on the endpoint `host` addresses, signed for `region`, requiring
/// success, and answer its response body.
async fn ok(
    router: &MultiServiceRouter,
    row: &Row,
    operation: &str,
    body: &Value,
) -> Value {
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(row.region));
    if let Some(host) = row.host {
        request = request.header("host", host);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(body.to_string()))
                .expect("the request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let payload: Value = serde_json::from_slice(response.body()).unwrap_or_else(|error| {
        panic!(
            "case {:?}: {operation} answered a body that is not JSON ({error}): {:?}",
            row.case,
            String::from_utf8_lossy(response.body())
        )
    });
    assert_eq!(
        status, 200,
        "case {:?}: {operation} must succeed, got {status} with {payload}",
        row.case
    );
    payload
}

/// An ANONYMOUS GET of the URL `url`, addressed at the authority the URL itself names —
/// which is what a relying party holding a token does with the locators it reads.
///
/// Answers the status and the body, which must be JSON for a document this emulator serves.
pub(crate) async fn fetch(router: &MultiServiceRouter, case: &str, url: &str) -> (u16, Value) {
    let rest = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case:?}: {url:?} must be an https URL"));
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    let response = router
        .dispatch(
            http::Request::builder()
                .method("GET")
                .uri(path)
                .header("host", authority)
                .body(Bytes::new())
                .expect("the request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let body = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: {url} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, body)
}

/// A base64url segment's JSON, with the padding RFC 7515 §2 strips restored.
fn segment_json(case: &str, segment: &str) -> Value {
    let mut standard = segment.replace('-', "+").replace('_', "/");
    while !standard.len().is_multiple_of(4) {
        standard.push('=');
    }
    let bytes = aws_smithy_types::base64::decode(&standard)
        .unwrap_or_else(|error| panic!("case {case:?}: {segment:?} must be base64url ({error})"));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("case {case:?}: {segment:?} must carry JSON ({error})"))
}

/// The JOSE header and the claim set of the compact JWS `token`.
pub(crate) fn header_and_claims(case: &str, token: &str) -> (Value, Value) {
    let mut segments = token.split('.');
    let header = segments
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: a token is a compact JWS"));
    let claims = segments
        .next()
        .unwrap_or_else(|| panic!("case {case:?}: a token carries a claims segment"));
    (
        segment_json(case, header),
        segment_json(case, claims),
    )
}

/// The string member `name` of `document`.
pub(crate) fn member(case: &str, document: &Value, name: &str) -> String {
    document[name]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a {name} in {document}"))
        .to_string()
}

/// The issuer a pool of `region` with the issuer type `issuer_type` publishes for a client
/// that resolves AWS's own hostnames.
///
/// The two forms are the ones `IssuerConfigurationType.Type` states
/// (`smithy_query(query="shape",shape="IssuerConfigurationType")`): the original
/// `https://cognito-idp.[region].amazonaws.com/[userPoolId]`, and the updated
/// `https://issuer-cognito-idp.[region].amazonaws.com/[userPoolId]`.
fn documented_issuer(region: &str, issuer_type: Option<&str>, pool_id: &str) -> String {
    let host_prefix = if issuer_type == Some("UPDATED") {
        "issuer-cognito-idp"
    } else {
        "cognito-idp"
    };
    format!("https://{host_prefix}.{region}.amazonaws.com/{pool_id}")
}

/// Create a pool as `row` asks, with an app client and a `CONFIRMED` user, and answer the
/// pool id and the tokens of one admin sign-in.
async fn signed_in(router: &MultiServiceRouter, row: &Row, index: usize) -> (String, Value) {
    let mut create = json!({ "PoolName": format!("issuer-address-pool-{index}") });
    if let Some(issuer_type) = row.issuer_type {
        create["IssuerConfiguration"] = json!({ "Type": issuer_type });
    }
    let user_pool_id = member(
        row.case,
        &ok(router, row, "CreateUserPool", &create).await["UserPool"],
        "Id",
    );
    let client_id = member(
        row.case,
        &ok(
            router,
            row,
            "CreateUserPoolClient",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientName": "issuer-address-client",
                "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH"],
            }),
        )
        .await["UserPoolClient"],
        "ClientId",
    );
    ok(
        router,
        row,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "MessageAction": "SUPPRESS",
        }),
    )
    .await;
    ok(
        router,
        row,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    let authenticated = ok(
        router,
        row,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    (
        user_pool_id,
        authenticated["AuthenticationResult"].clone(),
    )
}

/// Every token a pool mints announces the issuer the pool publishes on the endpoint it was
/// created through, and the documents that issuer announces are served at it.
#[tokio::test]
async fn a_pool_publishes_its_issuer_on_the_endpoint_it_was_created_through() {
    let router = router();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (user_pool_id, result) = signed_in(&router, row, index).await;

        let expected = match (row.reaches_only_the_emulator, row.host) {
            (true, Some(host)) => format!("https://{host}/{user_pool_id}"),
            _ => documented_issuer(row.region, row.issuer_type, &user_pool_id),
        };

        // Both signed tokens of one sign-in announce that issuer.
        let mut key_ids = Vec::new();
        for token_member in TOKEN_MEMBERS {
            let token = result[token_member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns {token_member}"));
            let (header, claims) = header_and_claims(case, token);
            let announced = member(case, &claims, "iss");
            assert_eq!(
                announced, expected,
                "case {case:?}: {token_member} must announce iss {expected:?}, got \
                 {announced:?}"
            );
            key_ids.push(member(case, &header, "kid"));
        }

        // The discovery document, fetched FROM the issuer the tokens announce, names that
        // same issuer and locates the key set under it.
        let discovery_url = format!("{expected}{OPENID_CONFIGURATION}");
        let (status, document) = fetch(&router, case, &discovery_url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {discovery_url} must be served by this emulator, got {status} \
             with {document}"
        );
        let stated = member(case, &document, "issuer");
        assert_eq!(
            stated, expected,
            "case {case:?}: the discovery document must state issuer {expected:?}, got \
             {stated:?}"
        );
        let jwks_uri = member(case, &document, "jwks_uri");
        assert_eq!(
            jwks_uri,
            format!("{expected}{JWKS}"),
            "case {case:?}: the discovery document must locate the key set under the \
             issuer, got {jwks_uri:?}"
        );

        // The key set, fetched from the locator the document published, holds the key each
        // token's `kid` names — so a verifier that follows the issuer can check the token.
        let (status, key_set) = fetch(&router, case, &jwks_uri).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {jwks_uri} must be served by this emulator, got {status} with \
             {key_set}"
        );
        let published: Vec<String> = key_set["keys"]
            .as_array()
            .unwrap_or_else(|| panic!("case {case:?}: a key set holds a keys array, got {key_set}"))
            .iter()
            .map(|key| member(case, key, "kid"))
            .collect();
        for key_id in &key_ids {
            assert!(
                published.contains(key_id),
                "case {case:?}: the key set at {jwks_uri} must hold the key {key_id:?} a \
                 token names, got {published:?}"
            );
        }
    }
}

/// A pool created through this emulator's own endpoint publishes its issuer on THAT
/// endpoint — the one authority the creating client resolves — rather than on a host
/// nothing on this machine resolves.
///
/// The assertion the recorded case needs, stated on its own so a failure says WHICH part of
/// the address is wrong: the authority is the one the create reached, the whole path is the
/// pool's id, and no AWS endpoint zone is named.
#[tokio::test]
async fn an_issuer_published_to_a_local_client_is_the_endpoint_it_reached() {
    let router = router();

    for (index, row) in rows()
        .iter()
        .enumerate()
        .filter(|(_, row)| row.reaches_only_the_emulator)
    {
        let case = row.case;
        let host = row.host.expect("a row reaching only the emulator names a host");
        let (user_pool_id, result) = signed_in(&router, row, 100 + index).await;
        let token = result["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns an IdToken"));
        let (_, claims) = header_and_claims(case, token);
        let announced = member(case, &claims, "iss");

        let rest = announced
            .strip_prefix("https://")
            .unwrap_or_else(|| panic!("case {case:?}: expected an https issuer, got {announced:?}"));
        let (authority, path) = rest
            .split_once('/')
            .unwrap_or_else(|| panic!("case {case:?}: expected a path, got {announced:?}"));
        assert_eq!(
            authority, host,
            "case {case:?}: the issuer must be published on the endpoint the create \
             reached, got {announced:?}"
        );
        assert_eq!(
            path, user_pool_id,
            "case {case:?}: the issuer's whole path must be the pool's id, got {announced:?}"
        );
        assert!(
            !authority.contains("amazonaws.com"),
            "case {case:?}: a client that resolves only this emulator must not be given an \
             AWS endpoint host, got {announced:?}"
        );
    }
}
