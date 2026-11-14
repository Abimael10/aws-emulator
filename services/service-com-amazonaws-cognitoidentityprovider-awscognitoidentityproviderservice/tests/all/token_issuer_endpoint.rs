// SPDX-License-Identifier: CC0-1.0

//! The ISSUER a user-pool token announces, over the endpoints a caller reaches this
//! service on and the issuer configurations a pool can hold.
//!
//! The RULE the rows assert, one level above any single deployment:
//!
//! > A user-pool token's `iss` claim is the minting pool's OWN issuer — the address the pool
//! > publishes it on with the pool's id as the whole path, which for a pool created by a
//! > client that resolves AWS's own hostnames is
//! > `https://cognito-idp.<region>.amazonaws.com/<UserPoolId>`, and
//! > `https://issuer-cognito-idp.<region>.amazonaws.com/<UserPoolId>` for such a pool
//! > configured with the `UPDATED` issuer type — rendered from what the pool STORED when it
//! > was created, whatever address the minting request was sent to. The pool's discovery
//! > document names that same issuer, and its key set is published under it.
//!
//! Which address a pool publishes on is `pool_issuer_address`'s rule: the issuer is a URL a
//! relying party fetches, so a pool created through this emulator's own endpoint publishes
//! it there. Every pool here is created through this service's published regional endpoint,
//! which is the caller the documented regional form is for.
//!
//! The issuer is token content, which this service's model does not describe: `smithy_query`
//! on `InitiateAuth` / `AdminInitiateAuth` / `GetClientToken` models only the token members
//! as opaque strings, and `IssuerConfigurationType.Type`
//! (`smithy_query(query="shape",shape="IssuerConfigurationType")`) is the only place the
//! model states an issuer at all — "the issuer URL is hosted in the user pool's region …
//! Original issuers have the format of
//! `https://cognito-idp.[region].amazonaws.com/[userPoolId]`" and "updated issuers have the
//! format of `https://issuer-cognito-idp.[region].amazonaws.com/[userPoolId]`, where region
//! is the primary Amazon Web Services Region of your user pool".
//!
//! The rows span what the rule generalises over: WHICH address the minting request reached
//! (the pool's published regional endpoint, a loopback address with a port, a named local
//! endpoint with a port, a bracketed IPv6 literal, an address that is no host at all, a
//! request naming no host), WHICH Region the pool lives in, WHICH issuer configuration it
//! holds, WHICH minting path answered (a sign-in, an admin sign-in, a refresh, a
//! machine-to-machine grant) and WHICH token of it carries the claim (the id token and the
//! access token).

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::state;

/// The local name of this service's Smithy shape, which is the `X-Amz-Target` prefix a
/// client writes.
const TARGET_PREFIX: &str = "AWSCognitoIdentityProviderService";

/// This service's SigV4 signing name, the 4th field of a credential scope.
const SIGNING_NAME: &str = "cognito-idp";

/// The region most calls of this suite are signed for, so the region the pools they
/// create live in and the region of each pool's issuer.
const REGION: &str = "us-east-1";

/// Another region, to pin that the issuer's region label is the POOL's and not a constant.
const OTHER_REGION: &str = "eu-west-1";

/// This service's published endpoint host in [`REGION`].
const PUBLISHED_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// A password satisfying a default pool's password policy, written for this suite.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// A `ClientSecretType`-shaped secret (pattern `^[\w+]+$`, length 24..64) the
/// machine-to-machine app client below is created with.
const CLIENT_SECRET: &str = "issuerm2msecret0000000000001";

/// The scope that client is granted, defined by the resource server the fixture creates.
const RESOURCE_SERVER: &str = "https://api.example.test";
const SCOPE_NAME: &str = "read";

/// One emulator with just this service mounted, beside the engine its state lives in.
fn router_and_engine() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    // Signature verification belongs to the emulator, not to this check.
    router.set_request_authentication(false);
    (router, engine)
}

/// A credential scope naming this service in `region`, which is what routes a call here
/// whatever host it addressed — and what decides the Region a created pool lives in.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{region}/{SIGNING_NAME}/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// Call `operation` on the endpoint `host` addresses, signed for `region`, requiring
/// success, and return its response body. `host` is `None` for a request that names no
/// host at all.
async fn ok_in(
    router: &MultiServiceRouter,
    region: &str,
    host: Option<&str>,
    operation: &str,
    body: &Value,
) -> Value {
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization(region));
    if let Some(host) = host {
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
            "{operation} answered a body that is not JSON ({error}): {:?}",
            String::from_utf8_lossy(response.body())
        )
    });
    assert_eq!(
        status, 200,
        "{operation} on host {host:?} must succeed, got {status} with {payload}"
    );
    payload
}

/// [`ok_in`] for the suite's main [`REGION`].
async fn ok(
    router: &MultiServiceRouter,
    host: Option<&str>,
    operation: &str,
    body: &Value,
) -> Value {
    ok_in(router, REGION, host, operation, body).await
}

/// The claim set of a compact JWS, decoded from its second segment.
fn claims(token: &str) -> Value {
    let segment = token
        .split('.')
        .nth(1)
        .unwrap_or_else(|| panic!("{token:?} must be a compact JWS"));
    let mut standard = segment.replace('-', "+").replace('_', "/");
    while !standard.len().is_multiple_of(4) {
        standard.push('=');
    }
    let bytes = aws_smithy_types::base64::decode(&standard)
        .unwrap_or_else(|error| panic!("the claims segment must be base64url ({error})"));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("the claims segment must be JSON ({error})"))
}

/// The JOSE header of a compact JWS, decoded from its first segment.
fn jose_header(token: &str) -> Value {
    let segment = token
        .split('.')
        .next()
        .unwrap_or_else(|| panic!("{token:?} must be a compact JWS"));
    let mut standard = segment.replace('-', "+").replace('_', "/");
    while !standard.len().is_multiple_of(4) {
        standard.push('=');
    }
    let bytes = aws_smithy_types::base64::decode(&standard)
        .unwrap_or_else(|error| panic!("the header segment must be base64url ({error})"));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("the header segment must be JSON ({error})"))
}

/// The string claim `name` of `token`.
fn claim(token: &str, name: &str) -> String {
    claims(token)[name]
        .as_str()
        .unwrap_or_else(|| panic!("a token carries a {name} claim, got {}", claims(token)))
        .to_string()
}

/// The issuer a pool of `region` with the issuer type `issuer_type` publishes for the pool
/// `pool_id`.
///
/// The two forms are the ones `IssuerConfigurationType.Type` states
/// (`smithy_query(query="shape",shape="IssuerConfigurationType")`): the original
/// `https://cognito-idp.[region].amazonaws.com/[userPoolId]`, and the updated
/// `https://issuer-cognito-idp.[region].amazonaws.com/[userPoolId]`.
fn expected_issuer(region: &str, issuer_type: &str, pool_id: &str) -> String {
    let host_prefix = if issuer_type == "UPDATED" {
        "issuer-cognito-idp"
    } else {
        "cognito-idp"
    };
    format!("https://{host_prefix}.{region}.amazonaws.com/{pool_id}")
}

/// The SHAPE a Cognito issuer has, asserted beside the exact value so a row states what
/// about the value is the requirement: the scheme is `https`, the host's first two labels
/// are this service's endpoint prefix (possibly with the updated issuer's own label in
/// front of it) and the pool's Region, at least one further non-empty label follows, the
/// whole path is `/<UserPoolId>`, and there is no query and no fragment.
fn assert_issuer_shape(case: &str, issuer: &str, region: &str, pool_id: &str) {
    assert!(
        !issuer.contains('?') && !issuer.contains('#'),
        "case {case:?}: an issuer carries no query and no fragment, got {issuer:?}"
    );
    let authority_and_path = issuer
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case:?}: an issuer is an https URL, got {issuer:?}"));
    let (authority, path) = authority_and_path
        .split_once('/')
        .unwrap_or_else(|| panic!("case {case:?}: an issuer carries a path, got {issuer:?}"));
    assert_eq!(
        path, pool_id,
        "case {case:?}: the issuer's whole path is the pool's id, got {issuer:?}"
    );
    let labels: Vec<&str> = authority.split('.').collect();
    assert!(
        labels.len() >= 3 && labels.iter().all(|label| !label.is_empty()),
        "case {case:?}: an issuer host carries the service, the region and a domain, got \
         {authority:?}"
    );
    assert!(
        labels[0].ends_with(SIGNING_NAME),
        "case {case:?}: the issuer host's first label names this service, got {authority:?}"
    );
    assert_eq!(
        labels[1], region,
        "case {case:?}: the issuer host's second label is the pool's region, got {authority:?}"
    );
}

/// One endpoint a caller reaches this service on.
struct EndpointCase {
    case: &'static str,
    /// The `Host` header the minting request carries, `None` for one naming no host.
    host: Option<&'static str>,
}

/// The endpoints the rows below mint on. Every one of them mints the SAME issuer, because
/// the issuer is the pool's own and not the address its caller used.
const ENDPOINTS: &[EndpointCase] = &[
    EndpointCase {
        case: "the pool's own published regional endpoint",
        host: Some(PUBLISHED_HOST),
    },
    EndpointCase {
        case: "a loopback address with a port, which is how this emulator is reached",
        host: Some("127.0.0.1:9297"),
    },
    EndpointCase {
        case: "a named local endpoint with a port",
        host: Some("localhost:4566"),
    },
    EndpointCase {
        case: "a bracketed IPv6 loopback endpoint",
        host: Some("[::1]:9297"),
    },
    EndpointCase {
        case: "a value no host is shaped like, which addresses no endpoint",
        host: Some("not a host"),
    },
    EndpointCase {
        case: "a request naming no host at all",
        host: None,
    },
];

/// A pool with one sign-in app client and one confirmed user, created on the PUBLISHED
/// endpoint.
struct Fixture {
    user_pool_id: String,
    client_id: String,
    username: String,
    /// The machine-to-machine app client of the same pool, and its granted scope.
    m2m_client_id: String,
}

/// Create the pool, its app clients and its user, all on the published endpoint.
async fn fixture(router: &MultiServiceRouter, pool_name: &str, username: &str) -> Fixture {
    let host = Some(PUBLISHED_HOST);
    let created = ok(router, host, "CreateUserPool", &json!({ "PoolName": pool_name })).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();

    let client = ok(
        router,
        host,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": format!("{pool_name}-client"),
            // The flows the rows sign in and refresh with.
            "ExplicitAuthFlows": [
                "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                "ALLOW_USER_PASSWORD_AUTH",
                "ALLOW_REFRESH_TOKEN_AUTH",
            ],
        }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();

    ok(
        router,
        host,
        "CreateResourceServer",
        &json!({
            "UserPoolId": user_pool_id,
            "Identifier": RESOURCE_SERVER,
            "Name": "suite resource server",
            "Scopes": [{ "ScopeName": SCOPE_NAME, "ScopeDescription": "read access" }],
        }),
    )
    .await;
    let m2m = ok(
        router,
        host,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": format!("{pool_name}-m2m"),
            "ClientSecret": CLIENT_SECRET,
            "ExplicitAuthFlows": ["ALLOW_CLIENT_TOKEN_AUTH"],
            "AllowedOAuthFlows": ["client_credentials"],
            "AllowedOAuthFlowsUserPoolClient": true,
            "AllowedOAuthScopes": [format!("{RESOURCE_SERVER}/{SCOPE_NAME}")],
        }),
    )
    .await;
    let m2m_client_id = m2m["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();

    ok(
        router,
        host,
        "AdminCreateUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;
    ok(
        router,
        host,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;

    Fixture {
        user_pool_id,
        client_id,
        username: username.to_string(),
        m2m_client_id,
    }
}

/// Every token minted for a request announces, as its issuer, the minting pool's own
/// regional issuer — on each minting path, for each signed token of it, and whatever
/// address the request was sent to.
#[tokio::test]
async fn a_minted_token_issues_from_the_pools_own_regional_issuer() {
    let (router, _engine) = router_and_engine();
    let pool = fixture(&router, "issuer-endpoint-pool", "wren").await;
    // A pool created with no `IssuerConfiguration` issues the ORIGINAL way.
    let expected = expected_issuer(REGION, "ORIGINAL", &pool.user_pool_id);

    for row in ENDPOINTS {
        let case = row.case;

        // A sign-in: the id token and the access token of one unsigned USER_PASSWORD_AUTH
        // call, which is the flow a client with no credentials uses.
        let signed_in = ok(
            &router,
            row.host,
            "InitiateAuth",
            &json!({
                "ClientId": pool.client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": pool.username, "PASSWORD": GOOD_PASSWORD },
            }),
        )
        .await;
        let result = &signed_in["AuthenticationResult"];
        for member in ["IdToken", "AccessToken"] {
            let token = result[member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns {member}"));
            let actual = claim(token, "iss");
            assert_eq!(
                actual, expected,
                "case {case:?} / InitiateAuth {member}: expected iss {expected:?}, \
                 got {actual:?}"
            );
            assert_issuer_shape(case, &actual, REGION, &pool.user_pool_id);
        }

        // A refresh on the same endpoint mints a fresh pair, which issues from there too.
        let refresh_token = result["RefreshToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns a RefreshToken"))
            .to_string();
        let refreshed = ok(
            &router,
            row.host,
            "InitiateAuth",
            &json!({
                "ClientId": pool.client_id,
                "AuthFlow": "REFRESH_TOKEN_AUTH",
                "AuthParameters": { "REFRESH_TOKEN": refresh_token },
            }),
        )
        .await;
        for member in ["IdToken", "AccessToken"] {
            let token = refreshed["AuthenticationResult"][member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: a refresh returns {member}"));
            let actual = claim(token, "iss");
            assert_eq!(
                actual, expected,
                "case {case:?} / refreshed {member}: expected iss {expected:?}, got {actual:?}"
            );
        }

        // An admin sign-in, the signed path, on the same endpoint.
        let admin = ok(
            &router,
            row.host,
            "AdminInitiateAuth",
            &json!({
                "UserPoolId": pool.user_pool_id,
                "ClientId": pool.client_id,
                "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": pool.username, "PASSWORD": GOOD_PASSWORD },
            }),
        )
        .await;
        for member in ["IdToken", "AccessToken"] {
            let token = admin["AuthenticationResult"][member]
                .as_str()
                .unwrap_or_else(|| panic!("case {case:?}: an admin sign-in returns {member}"));
            let actual = claim(token, "iss");
            assert_eq!(
                actual, expected,
                "case {case:?} / AdminInitiateAuth {member}: expected iss {expected:?}, \
                 got {actual:?}"
            );
        }

        // The machine-to-machine grant, whose access token is issued to the app client and
        // carries the same issuer.
        let granted = ok(
            &router,
            row.host,
            "GetClientToken",
            &json!({ "ClientId": pool.m2m_client_id, "Secret": CLIENT_SECRET }),
        )
        .await;
        let token = granted["ClientAuthenticationResult"]["AccessToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a grant returns an AccessToken"));
        let actual = claim(token, "iss");
        assert_eq!(
            actual, expected,
            "case {case:?} / GetClientToken AccessToken: expected iss {expected:?}, \
             got {actual:?}"
        );
    }
}

/// The issuer a pool announces is composed of the pool's OWN Region and issuer
/// configuration: the rows differ along the Region the pool was created in and the
/// `IssuerConfiguration.Type` it holds, including a pool that is reconfigured after it has
/// already minted tokens.
#[tokio::test]
async fn the_issuer_is_the_pools_region_and_configured_issuer_type() {
    let (router, _engine) = router_and_engine();

    // The issuer types are the modelled set
    // (`smithy_query(query="shape",shape="IssuerConfigurationType")`: ORIGINAL and
    // UPDATED), beside a pool that configures none and so issues the default way.
    let rows: &[(&str, &str, Option<&str>, &str)] = &[
        (
            "a pool configuring no issuer type",
            REGION,
            None,
            "ORIGINAL",
        ),
        (
            "a pool configured with the original issuer",
            REGION,
            Some("ORIGINAL"),
            "ORIGINAL",
        ),
        (
            "a pool configured with the updated issuer",
            REGION,
            Some("UPDATED"),
            "UPDATED",
        ),
        (
            "a pool of another region configuring no issuer type",
            OTHER_REGION,
            None,
            "ORIGINAL",
        ),
        (
            "a pool of another region with the updated issuer",
            OTHER_REGION,
            Some("UPDATED"),
            "UPDATED",
        ),
    ];

    for (index, (case, region, configured, effective)) in rows.iter().enumerate() {
        let mut create = json!({ "PoolName": format!("issuer-type-pool-{index}") });
        if let Some(issuer_type) = configured {
            create["IssuerConfiguration"] = json!({ "Type": issuer_type });
        }
        // Every call of a row is signed for the row's region, so the pool lives there.
        let created = ok_in(&router, region, Some(PUBLISHED_HOST), "CreateUserPool", &create).await;
        let user_pool_id = created["UserPool"]["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a minted pool id, got {created}"))
            .to_string();
        let client_id = ok_in(
            &router,
            region,
            Some(PUBLISHED_HOST),
            "CreateUserPoolClient",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientName": "issuer-type-client",
                "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH"],
            }),
        )
        .await["UserPoolClient"]["ClientId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a minted client id"))
            .to_string();
        ok_in(
            &router,
            region,
            Some(PUBLISHED_HOST),
            "AdminCreateUser",
            &json!({ "UserPoolId": user_pool_id, "Username": "finch" }),
        )
        .await;
        ok_in(
            &router,
            region,
            Some(PUBLISHED_HOST),
            "AdminSetUserPassword",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": "finch",
                "Password": GOOD_PASSWORD,
                "Permanent": true,
            }),
        )
        .await;

        // The minting request is sent to the EMULATOR's own address, so a row that passes
        // cannot be reading the issuer off the host it addressed.
        let signed_in = ok_in(
            &router,
            region,
            Some("127.0.0.1:9297"),
            "AdminInitiateAuth",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientId": client_id,
                "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": "finch", "PASSWORD": GOOD_PASSWORD },
            }),
        )
        .await;
        let token = signed_in["AuthenticationResult"]["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns an IdToken"));
        let actual = claim(token, "iss");
        let expected = expected_issuer(region, effective, &user_pool_id);
        assert_eq!(
            actual, expected,
            "case {case:?}: expected iss {expected:?}, got {actual:?}"
        );
        assert_issuer_shape(case, &actual, region, &user_pool_id);

        // The pool's own id names its Region, so the issuer's region label and the pool it
        // names agree.
        assert!(
            user_pool_id.starts_with(&format!("{region}_")),
            "case {case:?}: a pool id names its region, got {user_pool_id:?}"
        );

        // Reconfiguring the pool moves the issuer its NEXT tokens announce, which is what
        // reading the claim off stored state means.
        let reconfigured = if *effective == "UPDATED" {
            "ORIGINAL"
        } else {
            "UPDATED"
        };
        ok_in(
            &router,
            region,
            Some(PUBLISHED_HOST),
            "UpdateUserPool",
            &json!({
                "UserPoolId": user_pool_id,
                "IssuerConfiguration": { "Type": reconfigured },
            }),
        )
        .await;
        let after = ok_in(
            &router,
            region,
            Some("127.0.0.1:9297"),
            "AdminInitiateAuth",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientId": client_id,
                "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": "finch", "PASSWORD": GOOD_PASSWORD },
            }),
        )
        .await;
        let token = after["AuthenticationResult"]["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns an IdToken"));
        let actual = claim(token, "iss");
        let expected = expected_issuer(region, reconfigured, &user_pool_id);
        assert_eq!(
            actual, expected,
            "case {case:?} after reconfiguration to {reconfigured}: expected iss \
             {expected:?}, got {actual:?}"
        );
    }
}

/// The issuer a token announces is where the pool publishes what verifies it: the pool's
/// discovery document names that same issuer, and the key set under it serves the key the
/// token's `kid` names — fetched on whatever endpoint the holder reaches this emulator at.
#[tokio::test]
async fn a_tokens_issuer_is_the_discovery_root_of_the_pools_key_set() {
    let (router, _engine) = router_and_engine();
    let pool = fixture(&router, "issuer-discovery-pool", "quinn").await;

    for row in ENDPOINTS {
        let case = row.case;
        let signed_in = ok(
            &router,
            row.host,
            "InitiateAuth",
            &json!({
                "ClientId": pool.client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": pool.username, "PASSWORD": GOOD_PASSWORD },
            }),
        )
        .await;
        let token = signed_in["AuthenticationResult"]["IdToken"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns an IdToken"))
            .to_string();
        let issuer = claim(&token, "iss");
        let kid = jose_header(&token)["kid"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a signed token names its key by kid"))
            .to_string();

        // A verifier follows the issuer's PATH to the two documents it bootstraps from,
        // on the endpoint it reaches this emulator at — here the address of the minting
        // request, so the documents are fetched wherever the holder is talking.
        let path = issuer
            .strip_prefix("https://")
            .and_then(|authority_and_path| authority_and_path.split_once('/'))
            .map(|(_authority, path)| path.to_string())
            .unwrap_or_else(|| panic!("case {case:?}: an issuer carries a path, got {issuer:?}"));
        let fetch_host = row.host.unwrap_or(PUBLISHED_HOST);
        let fetch_host = if fetch_host == "not a host" {
            PUBLISHED_HOST
        } else {
            fetch_host
        };

        // NO credential: these documents are public, exactly as AWS serves them.
        for (document, member) in [
            (".well-known/openid-configuration", Some("issuer")),
            (".well-known/jwks.json", None),
        ] {
            let target = format!("/{path}/{document}");
            let request = http::Request::builder()
                .method("GET")
                .uri(&target)
                .header("host", fetch_host)
                .body(Bytes::new())
                .expect("the request is well formed");
            let response = router.dispatch(request).await;
            let status = response.status().as_u16();
            assert_eq!(
                status,
                200,
                "case {case:?}: GET https://{fetch_host}{target} must be served, got {status} \
                 with {:?}",
                String::from_utf8_lossy(response.body())
            );
            let served: Value = serde_json::from_slice(response.body()).unwrap_or_else(|error| {
                panic!(
                    "case {case:?}: {document} must be JSON ({error}): {:?}",
                    String::from_utf8_lossy(response.body())
                )
            });
            match member {
                // The discovery document names the issuer the token names, and the key set
                // under it.
                Some(member) => {
                    assert_eq!(
                        served[member].as_str(),
                        Some(issuer.as_str()),
                        "case {case:?}: the document's {member} must be the token's iss \
                         ({issuer:?}), got {served}"
                    );
                    assert_eq!(
                        served["jwks_uri"].as_str(),
                        Some(format!("{issuer}/.well-known/jwks.json").as_str()),
                        "case {case:?}: the key set sits under the issuer, got {served}"
                    );
                }
                // The key set holds the key the token was signed with.
                None => {
                    let names_the_key = served["keys"]
                        .as_array()
                        .unwrap_or_else(|| {
                            panic!("case {case:?}: a key set has a keys list, got {served}")
                        })
                        .iter()
                        .any(|key| key["kid"].as_str() == Some(kid.as_str()));
                    assert!(
                        names_the_key,
                        "case {case:?}: the key set under the announced issuer must hold the \
                         token's key {kid:?}, got {served}"
                    );
                }
            }
        }
    }
}

/// The reading every verifier of a user-pool token shares: which issuers name one pool.
///
/// Each row is an issuer a holder could present, and whether it names the pool. The issuer
/// a pool really mints is its own regional one, which both issuer types below state; the
/// addresses of this emulator are accepted beside them, because that is how a verifier
/// configured from this emulator's own endpoint names the pool.
#[test]
fn an_issuer_names_a_pool_only_where_that_pool_is_served() {
    let pool_id = "us-east-1_abc123def";
    let rows: &[(&str, String, bool)] = &[
        (
            "the pool's published regional issuer",
            format!("https://{PUBLISHED_HOST}/{pool_id}"),
            true,
        ),
        (
            "the updated issuer of the pool's own region",
            format!("https://issuer-cognito-idp.{REGION}.amazonaws.com/{pool_id}"),
            true,
        ),
        (
            "the emulator's loopback endpoint",
            format!("https://127.0.0.1:9297/{pool_id}"),
            true,
        ),
        (
            "a named local endpoint with a port",
            format!("https://localhost:4566/{pool_id}"),
            true,
        ),
        (
            "a bracketed IPv6 loopback endpoint",
            format!("https://[::1]:9297/{pool_id}"),
            true,
        ),
        (
            "the same issuer with a trailing slash, as a verifier may configure it",
            format!("https://{PUBLISHED_HOST}/{pool_id}/"),
            true,
        ),
        (
            "an endpoint reached over plain HTTP",
            format!("http://127.0.0.1:9297/{pool_id}"),
            true,
        ),
        (
            "this service's published issuer in another region, where the pool is not",
            format!("https://cognito-idp.{OTHER_REGION}.amazonaws.com/{pool_id}"),
            false,
        ),
        (
            "the updated issuer of another region",
            format!("https://issuer-cognito-idp.{OTHER_REGION}.amazonaws.com/{pool_id}"),
            false,
        ),
        (
            "another service's endpoint",
            format!("https://s3.{REGION}.amazonaws.com/{pool_id}"),
            false,
        ),
        (
            "the published issuer of a different pool",
            format!("https://{PUBLISHED_HOST}/us-east-1_999zzz000"),
            false,
        ),
        (
            "an issuer with the pool deeper in its path",
            format!("https://{PUBLISHED_HOST}/tenants/{pool_id}"),
            false,
        ),
        (
            "an issuer with no scheme, which is a provider NAME and not a URL",
            format!("{PUBLISHED_HOST}/{pool_id}"),
            false,
        ),
        ("an empty issuer", String::new(), false),
    ];

    for (case, issuer, expected) in rows {
        let actual = state::is_pool_issuer(REGION, pool_id, issuer);
        assert_eq!(
            actual, *expected,
            "case {case:?} ({issuer:?}): expected is_pool_issuer == {expected}, got {actual}"
        );
    }
}
