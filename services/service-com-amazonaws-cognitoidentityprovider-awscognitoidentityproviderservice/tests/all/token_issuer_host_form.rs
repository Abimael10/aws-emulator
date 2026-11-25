// SPDX-License-Identifier: CC0-1.0

//! The HOST FORM a user pool's token issuer carries, over the deployments a pool can be
//! created through.
//!
//! The RULE the rows assert, one level above the recorded case (a pool whose tokens
//! announced an issuer on a host carrying this service's endpoint prefix under the loopback
//! zone, a name no client of this emulator resolves to it):
//!
//! > A user pool's token issuer is `https://<authority>/<id>`: the single path segment is
//! > the pool's id as the create minted it, and the AUTHORITY is an address THIS emulator
//! > answers on — the `host[:port]` of the endpoint the create reached when that endpoint is
//! > the emulator's own, and otherwise the address this emulator is served at — carrying no
//! > label in front of it, because a relying party resolves that authority and dials it.
//! > Only an emulator served on no port at all has no address of its own to publish, and
//! > announces the host the model documents instead:
//! > `https://<endpoint-prefix>.<region>.amazonaws.com/<id>`, with the `issuer-` label in
//! > front of the endpoint prefix for the `UPDATED` issuer type. Either spelling of a pool's
//! > issuer names that pool for every verifier of a user-pool token, so a verifier
//! > configured with one and handed a token carrying the other resolves the same pool.
//!
//! The issuer is token content, which this service's model does not describe:
//! `IssuerConfigurationType.Type` (`smithy_query(query="shape",shape="IssuerConfigurationType")`)
//! is the only place the model states an issuer at all — "the issuer URL is hosted in the
//! user pool's region and provides OIDC endpoints specific to that region. Original issuers
//! have the format of `https://cognito-idp.[region].amazonaws.com/[userPoolId]`" and
//! "updated issuers have the format of
//! `https://issuer-cognito-idp.[region].amazonaws.com/[userPoolId]`, where region is the
//! primary Amazon Web Services Region of your user pool" — and the `iss` claim itself is
//! recorded behaviour. A zone AWS serves its endpoints in resolves nowhere near this
//! process, and so does a name this emulator invents under any other zone, so an emulator
//! with an endpoint of its own publishes that endpoint: the address its client already
//! dialled is the one address such a client resolves.
//!
//! The rows span what the rule generalises over: the ADDRESS the issuer is published at (an
//! emulator reached at a loopback address, at a loopback name, at a bracketed IPv6
//! literal, at a name an operator serves it under, at such a name on no port at all, and an
//! emulator served on no port, reached by a client that resolves AWS's own hostnames), the
//! REGION the pool lives in, and the ISSUER TYPE it is configured with. Each row asserts the
//! whole round trip the issuer exists for: both signed tokens announce it, it is the reading
//! every verifier of a user-pool token shares (`UserPool::publishes_issuer`) for that pool
//! and
//! for no pool of another Region, and the discovery document and key set a relying party
//! dereferences are served at it.
//!
//! `pool_issuer_address` and `pool_issuer_is_reachable_when_served` hold the same member's
//! rule for the endpoint a create reached, and `token_issuer_endpoint` that one pool
//! announces ONE issuer however each later caller addresses this emulator.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

// The fixture this suite shares with the one that pins the ENDPOINT a pool publishes on: the
// wire prefix, the credential scope, the anonymous fetch and the JOSE reading are that
// suite's `pub(crate)` ones rather than a second copy here.
use crate::pool_issuer_address::{
    authorization, fetch, header_and_claims, member, GOOD_PASSWORD, JWKS, OPENID_CONFIGURATION,
    SIGNING_NAME, TARGET_PREFIX, TOKEN_MEMBERS,
};

/// The user every row signs in.
const USERNAME: &str = "kestrel";

/// One deployment: the base URL this emulator is served at, the host the creating request
/// addresses, the Region it is signed for, the issuer type it configures, and the issuer
/// AUTHORITY the pool therefore announces.
struct Row {
    case: &'static str,
    /// The base URL the emulator reports as its endpoint, or `None` for one served on no
    /// port at all.
    served_base: Option<&'static str>,
    /// The `Host` the creating request carries — `None` for a request naming no host.
    host: Option<&'static str>,
    region: &'static str,
    /// The `IssuerConfiguration.Type` the create asks for, or `None` for a pool that
    /// configures none.
    issuer_type: Option<&'static str>,
    expected_authority: &'static str,
}

/// Every row's deployment. The expected authority is written out in full, so a row states
/// the whole value rather than re-deriving it the way the code under test does.
const ROWS: &[Row] = &[
    Row {
        case: "the recorded case: an emulator reached at a loopback address",
        served_base: Some("https://127.0.0.1:39819"),
        host: Some("127.0.0.1:39819"),
        region: "us-west-2",
        issuer_type: None,
        expected_authority: "127.0.0.1:39819",
    },
    Row {
        case: "the same deployment with the UPDATED issuer type, in another Region",
        served_base: Some("https://127.0.0.1:39819"),
        host: Some("127.0.0.1:39819"),
        region: "eu-central-1",
        issuer_type: Some("UPDATED"),
        expected_authority: "127.0.0.1:39819",
    },
    Row {
        case: "an emulator reached by the loopback NAME on another port",
        served_base: Some("https://localhost:4566"),
        host: Some("localhost:4566"),
        region: "ap-southeast-2",
        issuer_type: Some("ORIGINAL"),
        expected_authority: "localhost:4566",
    },
    Row {
        case: "an emulator reached at a bracketed IPv6 loopback literal",
        served_base: Some("https://[::1]:8443"),
        host: Some("[::1]:8443"),
        region: "us-east-1",
        issuer_type: None,
        expected_authority: "[::1]:8443",
    },
    Row {
        case: "an emulator an operator serves under a name of their own, on a port",
        served_base: Some("https://emulator.internal:8443"),
        host: None,
        region: "eu-west-1",
        issuer_type: None,
        expected_authority: "emulator.internal:8443",
    },
    Row {
        case: "the same name on no port at all, where the issuer carries none either",
        served_base: Some("https://emulator.internal"),
        host: None,
        region: "eu-west-1",
        issuer_type: Some("UPDATED"),
        expected_authority: "emulator.internal",
    },
    Row {
        case: "an emulator served on no port, reached by a client that resolves AWS's own \
               hostnames",
        served_base: None,
        host: Some("cognito-idp.sa-east-1.amazonaws.com"),
        region: "sa-east-1",
        issuer_type: None,
        expected_authority: "cognito-idp.sa-east-1.amazonaws.com",
    },
    Row {
        case: "the same client, for a pool configured with the UPDATED issuer type",
        served_base: None,
        host: Some("cognito-idp.sa-east-1.amazonaws.com"),
        region: "sa-east-1",
        issuer_type: Some("UPDATED"),
        expected_authority: "issuer-cognito-idp.sa-east-1.amazonaws.com",
    },
];

/// One emulator with just this service mounted, served as `row` says, beside the engine its
/// state lives in.
fn router(row: &Row) -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    // Signature verification belongs to the emulator, not to this check.
    router.set_request_authentication(false);
    if let Some(base) = row.served_base {
        router.set_url_base(EndpointUrl::parse(base).expect("the row's base URL must parse"));
    }
    (router, engine)
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

/// The claim set of the compact JWS `token`, read with the shared JOSE reading.
fn claims(case: &str, token: &str) -> Value {
    header_and_claims(case, token).1
}

/// Create a pool as `row` asks, with an app client and a `CONFIRMED` user, and answer the
/// pool id and the tokens of one admin sign-in.
async fn signed_in(router: &MultiServiceRouter, row: &Row, index: usize) -> (String, Value) {
    let mut create = json!({ "PoolName": format!("issuer-form-pool-{index}") });
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
                "ClientName": "issuer-form-client",
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

/// Every token a pool mints announces an issuer at an address this emulator answers on —
/// the endpoint the create reached, or the address the emulator is served at — with the
/// pool's id as its whole path, and the documents a verifier dereferences are served there.
#[tokio::test]
async fn an_issuer_is_published_at_an_address_this_emulator_answers_on() {
    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (router, _engine) = router(row);
        let (user_pool_id, result) = signed_in(&router, row, index).await;
        let expected = format!("https://{}/{user_pool_id}", row.expected_authority);

        for token_member in TOKEN_MEMBERS {
            let token = result[token_member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns {token_member}"));
            let announced = member(case, &claims(case, token), "iss");
            assert_eq!(
                announced, expected,
                "case {case:?}: {token_member} must announce iss {expected:?}, got {announced:?}"
            );
        }

        // What the ID TOKEN announced, read once for the assertions below, so each of them is
        // about the value the emulator produced rather than about the row's own expectation.
        let id_token = result["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns an IdToken"));
        let announced = member(case, &claims(case, id_token), "iss");

        // The form of the value the pool ANNOUNCED, stated separately so a failure says
        // which part of it is wrong.
        let (authority, path) = announced
            .strip_prefix("https://")
            .and_then(|rest| rest.split_once('/'))
            .unwrap_or_else(|| {
                panic!("case {case:?}: an issuer carries a path, got {announced:?}")
            });
        // The AUTHORITY, derived from the row's deployment rather than from the row's own
        // expectation: an emulator with an address of its own publishes THAT address — the
        // endpoint the create reached when the create addressed one, and the address the
        // emulator is served at for a creating client that resolves AWS's own hostnames —
        // with nothing in front of it, because a relying party resolves the authority an
        // issuer names and dials it. Only an emulator served on no port at all publishes the
        // documented host, whose labels are this service's endpoint prefix (under the
        // `issuer-` label for the UPDATED issuer type), the pool's Region and AWS's own zone.
        match row.served_base {
            Some(base) => {
                let served = base.trim_start_matches("https://");
                let reached = row
                    .host
                    .filter(|host| !host.ends_with(".amazonaws.com"))
                    .unwrap_or(served);
                assert_eq!(
                    authority, reached,
                    "case {case:?}: the issuer must be published at an address this emulator \
                     answers on, got {announced:?}"
                );
            }
            None => {
                let expected_prefix = match row.issuer_type {
                    Some("UPDATED") => format!("issuer-{SIGNING_NAME}"),
                    _ => SIGNING_NAME.to_string(),
                };
                assert_eq!(
                    authority,
                    format!("{expected_prefix}.{}.amazonaws.com", row.region),
                    "case {case:?}: an emulator with no address of its own must publish the \
                     documented host, got {announced:?}"
                );
            }
        }
        assert_eq!(
            path, user_pool_id,
            "case {case:?}: the issuer's whole path must be the pool's id, got {announced:?}"
        );

        // The discovery document, fetched FROM the announced issuer, states it and locates
        // the key set under it; the key set is served there too.
        let (status, document) = fetch(&router, case, &format!("{expected}{OPENID_CONFIGURATION}"))
            .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the discovery document under {expected} must be served, got \
             {status} with {document}"
        );
        assert_eq!(
            member(case, &document, "issuer"),
            expected,
            "case {case:?}: the discovery document must state the announced issuer, got \
             {document}"
        );
        let jwks_uri = member(case, &document, "jwks_uri");
        assert_eq!(
            jwks_uri,
            format!("{expected}{JWKS}"),
            "case {case:?}: the key set must sit under the issuer, got {jwks_uri:?}"
        );
        let (status, key_set) = fetch(&router, case, &jwks_uri).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the key set at {jwks_uri} must be served, got {status} with \
             {key_set}"
        );
        for token_member in TOKEN_MEMBERS {
            let token = result[token_member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns {token_member}"));
            service_common::jwt::verify(token, &key_set).unwrap_or_else(|defect| {
                panic!(
                    "case {case:?}: {token_member} must verify against the key set the announced \
                     issuer publishes, got {defect:?}"
                )
            });
        }
    }
}

/// The issuer a pool announces names THAT pool for every verifier of a user-pool token
/// (`UserPool::publishes_issuer`, the reading each one of them resolves a token's `iss`
/// through) — and so does the documented spelling of the same pool's issuer, so a verifier
/// configured with AWS's own host and handed a token carrying the address this emulator
/// publishes resolves the same pool. A host naming another Region, another service or
/// another pool names none of it.
#[tokio::test]
async fn the_announced_issuer_names_its_pool_whichever_zone_it_is_spelled_in() {
    /// A Region no row's pool lives in, to pin that the Region label is read.
    const FOREIGN_REGION: &str = "me-south-1";

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let (router, engine) = router(row);
        let (user_pool_id, result) = signed_in(&router, row, 100 + index).await;
        let token = result["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns an IdToken"));
        let announced = member(case, &claims(case, token), "iss");

        // The documented spelling of the same pool's issuer, which is what a verifier
        // configured from AWS's own documentation holds.
        let documented = format!(
            "https://{}.{}.amazonaws.com/{user_pool_id}",
            match row.issuer_type {
                Some("UPDATED") => format!("issuer-{SIGNING_NAME}"),
                _ => SIGNING_NAME.to_string(),
            },
            row.region
        );
        let named: &[(&str, String, bool)] = &[
            ("the issuer the pool announced", announced.clone(), true),
            (
                "the same issuer with the trailing slash a verifier may configure",
                format!("{announced}/"),
                true,
            ),
            (
                "the documented spelling of the same pool's issuer",
                documented,
                true,
            ),
            (
                "the same host form in another Region, where the pool does not live",
                format!(
                    "https://{SIGNING_NAME}.{FOREIGN_REGION}.localhost:39819/{user_pool_id}"
                ),
                false,
            ),
            (
                "another service's endpoint host",
                format!("https://s3.{}.amazonaws.com/{user_pool_id}", row.region),
                false,
            ),
            (
                "the announced issuer of another pool",
                format!("https://{}/{}_000000000", row.expected_authority, row.region),
                false,
            ),
            (
                "the announced host with the pool deeper in its path",
                format!("https://{}/tenants/{user_pool_id}", row.expected_authority),
                false,
            ),
        ];
        for (spelling, issuer, expected) in named {
            // The reading a verifier holding the pool applies, read off the STORED pool.
            let actual = {
                let store = cognito_idp::state::service_state(&*engine)
                    .expect("this service's own store");
                let guard = store.lock();
                guard
                    .pool(&user_pool_id)
                    .unwrap_or_else(|| panic!("case {case:?}: the created pool is stored"))
                    .publishes_issuer(issuer)
            };
            assert_eq!(
                actual, *expected,
                "case {case:?} / {spelling:?} ({issuer:?}): expected publishes_issuer == \
                 {expected}, got {actual}"
            );
        }
    }
}
