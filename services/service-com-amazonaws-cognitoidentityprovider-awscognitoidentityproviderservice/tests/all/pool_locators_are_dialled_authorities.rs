// SPDX-License-Identifier: CC0-1.0

//! Every LOCATOR a user pool publishes for a relying party to dereference, over the
//! deployments a pool can be created through.
//!
//! The rule the rows assert, one level above the recorded case (a pool whose tokens announced
//! an issuer on `cognito-idp.<region>.localhost:<port>`, a name no client of this emulator
//! resolves to it, so nothing could fetch the key set the `iss` claim led it to):
//!
//! > A URL this service publishes for a client to dereference LATER — a pool's token issuer,
//! > the discovery document and key set under it, and the hosted authorization server's
//! > endpoints that document names — carries the AUTHORITY of an endpoint this emulator
//! > answers on, as that endpoint was addressed: the `host[:port]` the creating request
//! > reached when it reached an endpoint of this emulator, and otherwise the address this
//! > process is served at. No label is inserted in front of it, because a client resolves
//! > the authority it is handed and dials it, and the only authority it resolves to this
//! > process is one this emulator is reached at. Only a deployment with no address of its own
//! > — an emulator served on no port, reached by a client that resolves AWS's own hostnames —
//! > publishes the host its model documents (`cognito-idp.<region>.amazonaws.com`).
//!
//! The issuer itself is token content, which this service's model does not describe:
//! `IssuerConfigurationType.Type`
//! (`smithy_query(query="shape",shape="IssuerConfigurationType")`) is the only place the model
//! states an issuer, and it states the regional host a client that resolves AWS's own
//! hostnames is given — "Original issuers have the format of
//! `https://cognito-idp.[region].amazonaws.com/[userPoolId]`". That host answers nowhere near
//! this process, so an emulator with an address of its own publishes that address instead.
//!
//! The rows span what the rule generalises over: WHICH address the creating request reached
//! (the served data port, another local address, a bracketed IPv6 literal, a name an operator
//! serves this emulator under, and one of AWS's own hostnames on a served emulator — where
//! the served address is published instead), WHETHER the emulator is served on a port at all,
//! WHICH Region the pool lives in and WHICH issuer configuration it holds. Each row asserts
//! every locator the pool publishes, and dereferences each of them at the authority it names
//! — anonymously, exactly as a relying party would.
//!
//! `pool_issuer_address`, `pool_issuer_is_reachable_when_served` and `token_issuer_host_form`
//! hold the same member's rule for the `iss` claim alone; this suite is about every locator
//! published under it, including the hosted authorization server's.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

// The fixture's shared parts — the wire prefix, the credential scope, the anonymous fetch and
// the JOSE reading — are `crate::pool_issuer_address`'s own, imported rather than written
// again here.
use crate::pool_issuer_address::{
    authorization, fetch, header_and_claims, member, GOOD_PASSWORD, JWKS, OPENID_CONFIGURATION,
    TARGET_PREFIX, TOKEN_MEMBERS,
};

/// The user every row signs in.
const USERNAME: &str = "dialled";

/// The members of the discovery document that carry a URL a client then dereferences: the
/// issuer itself, the key set, and the four endpoints the hosted authorization server serves
/// (RFC 8414 §2, and `smithy_query(query="operation",operation="CreateUserPoolDomain")` for
/// the server that hosts them).
const PUBLISHED_LOCATORS: [&str; 7] = [
    "issuer",
    "jwks_uri",
    "authorization_endpoint",
    "token_endpoint",
    "userinfo_endpoint",
    "revocation_endpoint",
    "end_session_endpoint",
];

/// One deployment: how this emulator is served, which endpoint the creating request reaches
/// it at, and the authority every locator of the pool it creates must therefore carry.
struct Row {
    case: &'static str,
    /// The base URL the emulator reports as its endpoint, or `None` for one served on no port
    /// at all.
    served_base: Option<&'static str>,
    /// The `Host` the creating request carries — `None` for a request naming no host.
    host: Option<&'static str>,
    region: &'static str,
    /// The `IssuerConfiguration.Type` the create asks for, or `None` for a pool that
    /// configures none.
    issuer_type: Option<&'static str>,
    /// The authority every published locator must carry, written out in full so the row
    /// states the whole value rather than re-deriving it the way the code under test does.
    expected_authority: &'static str,
}

const ROWS: &[Row] = &[
    Row {
        case: "the recorded case: a client configured with the emulator's served data port",
        served_base: Some("https://127.0.0.1:9297"),
        host: Some("127.0.0.1:9297"),
        region: "us-west-2",
        issuer_type: None,
        expected_authority: "127.0.0.1:9297",
    },
    Row {
        case: "another local address of the same emulator, named by the loopback NAME",
        served_base: Some("https://127.0.0.1:9297"),
        host: Some("localhost:4566"),
        region: "eu-central-1",
        issuer_type: Some("ORIGINAL"),
        expected_authority: "localhost:4566",
    },
    Row {
        case: "a bracketed IPv6 loopback literal on a port of its own",
        served_base: Some("https://127.0.0.1:9297"),
        host: Some("[::1]:8443"),
        region: "ap-southeast-2",
        issuer_type: Some("UPDATED"),
        expected_authority: "[::1]:8443",
    },
    Row {
        case: "a client that resolves AWS's own hostnames, on a served emulator, which is \
               published the address this process answers on",
        served_base: Some("https://127.0.0.1:9297"),
        host: Some("cognito-idp.us-east-1.amazonaws.com"),
        region: "us-east-1",
        issuer_type: None,
        expected_authority: "127.0.0.1:9297",
    },
    Row {
        case: "an emulator an operator serves under a name of their own",
        served_base: Some("https://emulator.internal:8443"),
        host: None,
        region: "eu-west-1",
        issuer_type: Some("UPDATED"),
        expected_authority: "emulator.internal:8443",
    },
    Row {
        case: "an emulator served on no port at all, reached by a client that resolves AWS's \
               own hostnames: the one deployment with no address of its own",
        served_base: None,
        host: Some("cognito-idp.sa-east-1.amazonaws.com"),
        region: "sa-east-1",
        issuer_type: None,
        expected_authority: "cognito-idp.sa-east-1.amazonaws.com",
    },
];

/// One emulator with just this service mounted, served as `row` says.
fn router(row: &Row) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::new(
        engine::InMemoryEngine::new(),
    )));
    // Signature verification belongs to the emulator, not to this check.
    router.set_request_authentication(false);
    if let Some(base) = row.served_base {
        router.set_url_base(EndpointUrl::parse(base).expect("the row's base URL must parse"));
    }
    router
}

/// Call `operation` on the endpoint `row.host` addresses, requiring success, and answer the
/// response body.
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

/// A pool with a hosted domain, an app client and a `CONFIRMED` user, signed in once.
struct SignedIn {
    user_pool_id: String,
    /// The hosted authorization server's domain prefix, the first path segment of every
    /// endpoint the discovery document names for it.
    domain: String,
    /// The tokens of one admin sign-in.
    tokens: Value,
}

/// Create the pool `row` asks for — with a hosted domain, an app client and a user — and sign
/// that user in.
async fn signed_in(router: &MultiServiceRouter, row: &Row, index: usize) -> SignedIn {
    let mut create = json!({ "PoolName": format!("dialled-authority-pool-{index}") });
    if let Some(issuer_type) = row.issuer_type {
        create["IssuerConfiguration"] = json!({ "Type": issuer_type });
    }
    let user_pool_id = member(
        row.case,
        &ok(router, row, "CreateUserPool", &create).await["UserPool"],
        "Id",
    );
    // The hosted authorization server, so the document names the endpoints a client drives
    // under the domain it is reached on as well as the two well-known documents.
    let domain = format!("dialled-authority-{index}");
    ok(
        router,
        row,
        "CreateUserPoolDomain",
        &json!({ "UserPoolId": user_pool_id, "Domain": domain }),
    )
    .await;
    let client_id = member(
        row.case,
        &ok(
            router,
            row,
            "CreateUserPoolClient",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientName": "dialled-authority-client",
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
    SignedIn {
        user_pool_id,
        domain,
        tokens: authenticated["AuthenticationResult"].clone(),
    }
}

/// The authority of an `https` URL: what stands between the scheme and the first `/` of the
/// path.
fn authority_of(case: &str, url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case:?}: {url:?} must be an https URL"));
    rest.split('/').next().unwrap_or(rest).to_string()
}

/// Every locator a pool publishes carries the authority of the endpoint this emulator answers
/// on, and every one of them is served there.
#[tokio::test]
async fn every_locator_a_pool_publishes_carries_an_authority_this_emulator_answers_on() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let router = router(row);
        let pool = signed_in(&router, row, index).await;
        let issuer = format!("https://{}/{}", row.expected_authority, pool.user_pool_id);

        // Both signed tokens of one sign-in announce that issuer as the discovery root a
        // verifier reads the key set from.
        for token_member in TOKEN_MEMBERS {
            let token = pool.tokens[token_member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns {token_member}"));
            let announced = member(case, &header_and_claims(case, token).1, "iss");
            assert_eq!(
                announced, issuer,
                "case {case:?}: {token_member} must announce iss {issuer:?}, got {announced:?}"
            );
        }

        // The discovery document, fetched FROM the issuer at the authority it names.
        let (status, document) = fetch(&router, case, &format!("{issuer}{OPENID_CONFIGURATION}"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the discovery document under {issuer} must be served, got {status} \
             with {document}"
        );

        // Every locator it publishes carries the SAME authority — the one this emulator
        // answers on — and the two documents and the hosted server's endpoints are the paths
        // under it.
        for locator in PUBLISHED_LOCATORS {
            let published = member(case, &document, locator);
            let authority = authority_of(case, &published);
            assert_eq!(
                authority, row.expected_authority,
                "case {case:?}: {locator} must be published on {:?}, got {published:?}",
                row.expected_authority
            );
            // ...and no locator names a host AWS publishes its own endpoints at, unless this
            // deployment has no address of its own to publish at all.
            let names_aws =
                ::service_common::data_plane_addressing::addresses_an_aws_endpoint(&authority);
            assert_eq!(
                names_aws,
                row.served_base.is_none(),
                "case {case:?}: {locator} {published:?} names an AWS endpoint host, which no \
                 client of a served emulator resolves"
            );
        }
        let jwks_uri = member(case, &document, "jwks_uri");
        assert_eq!(
            jwks_uri,
            format!("{issuer}{JWKS}"),
            "case {case:?}: the key set must sit under the issuer, got {jwks_uri:?}"
        );
        // The hosted authorization server's endpoints are served under the domain it was
        // created on, on that same authority.
        let token_endpoint = member(case, &document, "token_endpoint");
        assert_eq!(
            token_endpoint,
            format!(
                "https://{}/{}/oauth2/token",
                row.expected_authority, pool.domain
            ),
            "case {case:?}: the token endpoint must sit under the hosted domain, got \
             {token_endpoint:?}"
        );

        // The key set, fetched from the locator the document published, verifies both tokens
        // — so a relying party that follows nothing but the `iss` claim can check what the
        // pool minted.
        let (status, key_set) = fetch(&router, case, &jwks_uri).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the key set at {jwks_uri} must be served, got {status} with \
             {key_set}"
        );
        for token_member in TOKEN_MEMBERS {
            let token = pool.tokens[token_member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns {token_member}"));
            let verified = ::service_common::jwt::verify(token, &key_set).unwrap_or_else(|defect| {
                panic!(
                    "case {case:?}: {token_member} must verify against the key set at \
                     {jwks_uri}, got {defect:?}"
                )
            });
            assert_eq!(
                verified.claims.get("iss").and_then(Value::as_str),
                Some(issuer.as_str()),
                "case {case:?}: {token_member}'s SIGNED claims must name {issuer:?}, got {:?}",
                verified.claims
            );
        }
    }
}

/// The authority a pool publishes is the one the CREATE reached, so it does not move when a
/// later caller addresses this emulator somewhere else: the `iss` claim is what a verifier
/// compares against the issuer it was configured with, and one pool announces one issuer.
#[tokio::test]
async fn a_later_caller_elsewhere_does_not_move_the_authority_a_pool_published() {
    // The deployment of the recorded case, with the sign-ins sent to every OTHER address the
    // rows above reach this emulator at.
    let created_through = &ROWS[0];
    let router = router(created_through);
    let pool = signed_in(&router, created_through, 100).await;
    let issuer = format!(
        "https://{}/{}",
        created_through.expected_authority, pool.user_pool_id
    );

    for elsewhere in ROWS.iter().skip(1) {
        let case = elsewhere.case;
        // The same pool, signed in through the endpoint THIS row addresses.
        let row = Row {
            case,
            served_base: created_through.served_base,
            host: elsewhere.host,
            region: created_through.region,
            issuer_type: None,
            expected_authority: created_through.expected_authority,
        };
        let client_id = member(
            case,
            &ok(
                &router,
                &row,
                "CreateUserPoolClient",
                &json!({
                    "UserPoolId": pool.user_pool_id,
                    "ClientName": format!("elsewhere-{}", case.len()),
                    "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH"],
                }),
            )
            .await["UserPoolClient"],
            "ClientId",
        );
        let authenticated = ok(
            &router,
            &row,
            "AdminInitiateAuth",
            &json!({
                "UserPoolId": pool.user_pool_id,
                "ClientId": client_id,
                "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
            }),
        )
        .await;
        let token = authenticated["AuthenticationResult"]["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns an IdToken"));
        let announced = member(case, &header_and_claims(case, token).1, "iss");
        assert_eq!(
            announced, issuer,
            "case {case:?}: the pool must keep announcing {issuer:?} whatever endpoint a \
             later caller reaches, got {announced:?}"
        );
        // ...and the document fetched from it still states it.
        let (status, document) =
            fetch(&router, case, &format!("{announced}{OPENID_CONFIGURATION}")).await;
        assert_eq!(
            (status, member(case, &document, "issuer")),
            (200, issuer.clone()),
            "case {case:?}: the discovery document must state {issuer:?}, got {status} with \
             {document}"
        );
    }
}
