// SPDX-License-Identifier: CC0-1.0

//! The ADDRESS a user pool publishes its token issuer on when this emulator IS served on a
//! port — the deployment every client of the emulator meets.
//!
//! The RULE the rows assert, one level above the recorded case (a pool whose tokens
//! announced an issuer on a real-AWS host no client of this emulator can fetch anything
//! from):
//!
//! > A pool's token issuer is a URL its relying parties DEREFERENCE — every token it mints
//! > carries it as `iss`, and a verifier reads `<iss>/.well-known/openid-configuration` and
//! > `<iss>/.well-known/jwks.json` from it — so its authority is one THIS emulator answers
//! > on: the `host[:port]` of the endpoint the creating request reached when that endpoint is
//! > the emulator's own, and otherwise of the address this emulator is served at. Its whole
//! > path is the pool's id, and both documents are served there.
//!
//! The issuer is token content, which this service's model does not describe:
//! `IssuerConfigurationType.Type`
//! (`smithy_query(query="shape",shape="IssuerConfigurationType")`) is the only place the
//! model states an issuer at all, and it states the regional form a client that resolves
//! AWS's own hostnames is given — "the issuer URL is hosted in the user pool's region …
//! Original issuers have the format of
//! `https://cognito-idp.[region].amazonaws.com/[userPoolId]`". An emulator that is served on
//! a port has an address of its OWN to publish instead, and publishes it: a host under a
//! zone AWS serves its endpoints in answers nowhere near this process, and so does a name
//! this emulator invents under any other zone.
//!
//! The rows span what the rule generalises over: WHICH host the creating request addressed
//! (an address of this emulator — the served data port, another local port, a bracketed IPv6
//! literal — this service's own published regional host, ANOTHER service's published
//! regional host, and a request naming no host at all), WHICH Region the pool lives in, and
//! WHICH issuer configuration it holds (none, `ORIGINAL`, `UPDATED`). The recorded case is
//! ONE row.
//!
//! `pool_issuer_address` holds the same member's rule for an emulator served on NO port,
//! where there is no address of this emulator to publish; `token_issuer_endpoint` holds the
//! rest of it: one pool announces ONE issuer however each later caller addresses this
//! emulator. The parts of the fixture that are not about the deployment — the wire prefix, the
//! credential scope, the anonymous fetch and the JOSE reading — are that suite's own
//! `pub(crate)` helpers, imported here rather than written a second time.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

// The sign-in fixture's shared parts, held by the suite that pins the unserved half of this
// rule (`crate::pool_issuer_address`).
use crate::pool_issuer_address::{
    authorization, fetch, header_and_claims, member, GOOD_PASSWORD, JWKS, OPENID_CONFIGURATION,
    TARGET_PREFIX, TOKEN_MEMBERS, USERNAME,
};

/// The base URL this emulator is SERVED at in this suite — the data port a client of the
/// emulator is configured with (`MultiServiceRouter::set_url_base`, which is what an
/// emulator that is served on a port reports as its endpoint).
const SERVED_BASE: &str = "https://127.0.0.1:9297";

/// The `host[:port]` of [`SERVED_BASE`], which is the address every pool created by a
/// request that addressed no endpoint of this emulator publishes its issuer on.
const SERVED_AUTHORITY: &str = "127.0.0.1:9297";

/// The DNS zone an endpoint of this emulator addressed as an ADDRESS is announced in: RFC
/// 6761 reserves it, and every name under it, for the loopback interface the core binds on
/// (`service_common::data_plane_addressing::LOOPBACK_ZONE`).
const LOOPBACK_ZONE: &str = "localhost";

/// One row: the host the creating request addressed, the Region it is signed for, the issuer
/// type it configures, and the authority the pool therefore publishes its issuer on.
struct Row {
    case: &'static str,
    /// The `Host` the creating request carries — `None` for a request naming no host.
    host: Option<&'static str>,
    region: &'static str,
    /// The `IssuerConfiguration.Type` the create asks for, or `None` for a pool that
    /// configures none.
    issuer_type: Option<&'static str>,
    /// The authority the issuer must name: the `host[:port]` of the endpoint the create
    /// reached when that is an endpoint of this emulator, and of the served address
    /// otherwise.
    expected_authority: &'static str,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "a client configured with the emulator's served data port",
            host: Some(SERVED_AUTHORITY),
            region: "us-west-2",
            issuer_type: None,
            expected_authority: SERVED_AUTHORITY,
        },
        Row {
            case: "a client configured with another local address of this emulator",
            host: Some("localhost:7001"),
            region: "eu-west-1",
            issuer_type: Some("ORIGINAL"),
            expected_authority: "localhost:7001",
        },
        Row {
            case: "a client configured with a bracketed IPv6 address of this emulator",
            host: Some("[::1]:9443"),
            region: "ap-southeast-2",
            issuer_type: Some("UPDATED"),
            expected_authority: "[::1]:9443",
        },
        Row {
            case: "the recorded case: a client that resolves AWS's own hostnames, on this \
                   service's published regional host",
            host: Some("cognito-idp.us-west-2.amazonaws.com"),
            region: "us-west-2",
            issuer_type: None,
            expected_authority: SERVED_AUTHORITY,
        },
        Row {
            case: "the same, with the updated issuer type in another Region",
            host: Some("cognito-idp.eu-north-1.amazonaws.com"),
            region: "eu-north-1",
            issuer_type: Some("UPDATED"),
            expected_authority: SERVED_AUTHORITY,
        },
        Row {
            case: "an in-process call of another service, addressed at that service's \
                   published regional host",
            host: Some("cloudformation.us-east-1.amazonaws.com"),
            region: "us-east-1",
            issuer_type: Some("ORIGINAL"),
            expected_authority: SERVED_AUTHORITY,
        },
        Row {
            case: "a request naming no host at all",
            host: None,
            region: "us-east-2",
            issuer_type: None,
            expected_authority: SERVED_AUTHORITY,
        },
    ]
}

/// One emulator with just this service mounted, SERVED at [`SERVED_BASE`].
fn router() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::new(
        engine::InMemoryEngine::new(),
    )));
    // Signature verification belongs to the emulator, not to this check.
    router.set_request_authentication(false);
    router.set_url_base(
        EndpointUrl::parse(SERVED_BASE).expect("the suite's served base URL must parse"),
    );
    router
}

/// Call `operation` on the endpoint `row.host` addresses, signed for `row.region`, requiring
/// success, and answer its response body.
async fn ok(router: &MultiServiceRouter, row: &Row, operation: &str, body: &Value) -> Value {
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

/// Create a pool as `row` asks, with an app client and a `CONFIRMED` user, and answer the
/// pool id and the tokens of one admin sign-in.
async fn signed_in(router: &MultiServiceRouter, row: &Row, index: usize) -> (String, Value) {
    let mut create = json!({ "PoolName": format!("served-issuer-pool-{index}") });
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
                "ClientName": "served-issuer-client",
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
    (user_pool_id, authenticated["AuthenticationResult"].clone())
}

/// Every token a pool of a SERVED emulator mints announces an issuer on an address this
/// emulator answers on, and the two documents a verifier dereferences are served there.
#[tokio::test]
async fn a_served_pool_publishes_an_issuer_this_emulator_answers_on() {
    let router = router();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (user_pool_id, result) = signed_in(&router, row, index).await;
        let expected = format!("https://{}/{user_pool_id}", row.expected_authority);

        // Both signed tokens of one sign-in announce that issuer.
        for token_member in TOKEN_MEMBERS {
            let token = result[token_member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns {token_member}"));
            let announced = member(case, &header_and_claims(case, token).1, "iss");
            assert_eq!(
                announced, expected,
                "case {case:?}: {token_member} must announce iss {expected:?}, got {announced:?}"
            );
        }

        // The discovery document, fetched FROM the issuer the tokens announce, names that
        // same issuer and locates the key set under it.
        let discovery_url = format!("{expected}{OPENID_CONFIGURATION}");
        let (status, document) = fetch(&router, case, &discovery_url).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {discovery_url} must be served by this emulator, got {status} with \
             {document}"
        );
        let stated = member(case, &document, "issuer");
        assert_eq!(
            stated, expected,
            "case {case:?}: the discovery document must state issuer {expected:?}, got {stated:?}"
        );
        let jwks_uri = member(case, &document, "jwks_uri");
        assert_eq!(
            jwks_uri,
            format!("{expected}{JWKS}"),
            "case {case:?}: the discovery document must locate the key set under the issuer, got \
             {jwks_uri:?}"
        );

        // The key set, fetched from the locator that document published, verifies each
        // token of the sign-in — so a relying party that follows nothing but the `iss`
        // claim can check what the pool minted.
        let (status, key_set) = fetch(&router, case, &jwks_uri).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {jwks_uri} must be served by this emulator, got {status} with \
             {key_set}"
        );
        for token_member in TOKEN_MEMBERS {
            let token = result[token_member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns {token_member}"));
            let verified = service_common::jwt::verify(token, &key_set).unwrap_or_else(|defect| {
                panic!(
                    "case {case:?}: {token_member} must verify against the key set at {jwks_uri}, \
                     got {defect:?}"
                )
            });
            assert_eq!(
                verified.claims.get("iss").and_then(Value::as_str),
                Some(expected.as_str()),
                "case {case:?}: {token_member}'s SIGNED claims must name {expected:?}, got {:?}",
                verified.claims
            );
        }
    }
}

/// No pool of a served emulator announces an issuer under a zone AWS publishes its own
/// endpoints in: such a host answers nowhere near this process, so nothing a client of this
/// emulator holds could ever fetch the issuer's documents. What it announces instead is a
/// name a resolver answers with the interface this emulator binds on, on a port it answers.
///
/// The zone list and the loopback reading are the workspace's own
/// (`service_common::data_plane_addressing`), read through the predicates every announced
/// address is judged by, rather than spelled out again here.
#[tokio::test]
async fn a_served_pool_announces_no_aws_endpoint_host() {
    let router = router();

    for (index, row) in rows().iter().enumerate() {
        let case = row.case;
        let (user_pool_id, result) = signed_in(&router, row, 100 + index).await;
        let token = result["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns an IdToken"));
        let announced = member(case, &header_and_claims(case, token).1, "iss");

        let rest = announced.strip_prefix("https://").unwrap_or_else(|| {
            panic!("case {case:?}: expected an https issuer, got {announced:?}")
        });
        let (authority, path) = rest
            .split_once('/')
            .unwrap_or_else(|| panic!("case {case:?}: expected a path, got {announced:?}"));
        assert!(
            !service_common::data_plane_addressing::addresses_an_aws_endpoint(authority),
            "case {case:?}: a served emulator must publish an issuer on an address it answers \
             on, got {announced:?}"
        );
        assert!(
            service_common::data_plane_addressing::names_the_loopback_interface(authority),
            "case {case:?}: the issuer's host must be a name the {LOOPBACK_ZONE} zone resolves \
             to the interface this emulator binds on, got {announced:?}"
        );
        assert_eq!(
            service_common::data_plane_addressing::addressed_port(Some(authority)),
            service_common::data_plane_addressing::addressed_port(Some(row.expected_authority)),
            "case {case:?}: the issuer must carry the port the endpoint it publishes is \
             served on, got {announced:?}"
        );
        assert_eq!(
            path, user_pool_id,
            "case {case:?}: the issuer's whole path must be the pool's id, got {announced:?}"
        );
    }
}
