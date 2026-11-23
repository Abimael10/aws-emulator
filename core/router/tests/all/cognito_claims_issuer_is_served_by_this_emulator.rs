// SPDX-License-Identifier: CC0-1.0

//! The ISSUER an API Gateway `COGNITO_USER_POOLS` authorizer publishes to its integration,
//! on an emulator that is SERVED on a port — driven through the whole assembled router.
//!
//! Two services and one engine, so this is a router suite: the tokens are minted by the
//! Amazon Cognito user pools service (signing name `cognito-idp`) and the gate that reads
//! them is API Gateway's own `execute-api` data plane.
//!
//! The RULE the rows assert, one level above the recorded case (a Lambda proxy integration
//! reading `event.requestContext.authorizer.claims.iss` and finding a real-AWS host nothing
//! in the emulated environment answers on):
//!
//! > A pool's token issuer is a URL its relying parties DEREFERENCE — the `iss` claim the
//! > authorizer publishes to the integration is the discovery root a verifier reads
//! > `<iss>/.well-known/openid-configuration` and `<iss>/.well-known/jwks.json` from — so on
//! > an emulator that is served on a port its authority is the address that emulator is
//! > served at, its whole path is the minting pool's id, and both documents are served
//! > there. One authorizer over several pools therefore tells them apart by that issuer.
//!
//! The rows span what the rule generalises over: WHICH endpoint the pool was created through
//! (the emulator's served address, which is the only authority a client on this machine
//! resolves, and this service's published regional host, which is a client that resolves
//! AWS's own hostnames), WHICH endpoint the token was minted on (the other one in each
//! case, so the issuer is the pool's stored one rather than the sign-in's host), and WHICH
//! token of the sign-in is presented (the identity token and the access token).
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::{EndpointUrl, MultiServiceRouter};

// The base64url coding of a JOSE segment is this test target's own
// (`crate::cognito_group_role_mapping`), imported rather than written a second time.
use crate::cognito_group_role_mapping::decode_base64url;

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every call is signed for, so the region of the pools and of the api.
const REGION: &str = "us-west-2";

/// The base URL this emulator is SERVED at in this suite — the data port a client of the
/// emulator is configured with (`MultiServiceRouter::set_url_base`, which is what an
/// emulator served on a port reports as its endpoint).
const SERVED_BASE: &str = "https://127.0.0.1:9297";

/// The `host[:port]` of [`SERVED_BASE`].
const SERVED_AUTHORITY: &str = "127.0.0.1:9297";

/// This service's published endpoint host in [`REGION`] — the address a client that resolves
/// AWS's own hostnames reaches a pool at.
fn published_host() -> String {
    format!("cognito-idp.{REGION}.amazonaws.com")
}

/// A permanent password satisfying a pool's default policy, written for this suite.
const GOOD_PASSWORD: &str = "Str0ng-Passw0rd!";

/// The user each pool signs in.
const USERNAME: &str = "claims-user";

/// The stage the api is deployed to.
const STAGE: &str = "prod";

/// The header the authorizer's identity source names.
const TOKEN_HEADER: &str = "Authorization";

/// What the `MOCK` integration behind the gated method answers: the issuer the authorizer
/// published, which is how a row observes the claim reaching the integration as minted.
const ISSUER_TEMPLATE: &str = "{\"iss\":\"$context.authorizer.claims.iss\"}";

/// The paths under an issuer a relying party dereferences: its OpenID Connect discovery
/// document and the key set that document locates (RFC 8414 §3, and the Amazon Cognito
/// Developer Guide's "Verifying a JSON web token").
const OPENID_CONFIGURATION: &str = "/.well-known/openid-configuration";
const JWKS: &str = "/.well-known/jwks.json";

/// One request dispatched through the router, and what came back.
struct Answer {
    status: u16,
    body: Value,
    text: String,
}

/// Dispatch one request with the host, path, headers and body given.
async fn dispatch(
    router: &MultiServiceRouter,
    host: &str,
    method: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(body))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let text = String::from_utf8_lossy(response.body()).to_string();
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Answer { status, body, text }
}

/// A credential scope naming `signing_name`, which is what routes a call to that service.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsJson1_1 call to the user pools service, on the endpoint `host` addresses, which
/// must succeed.
async fn cognito(router: &MultiServiceRouter, host: &str, operation: &str, body: &Value) -> Value {
    let target = format!("AWSCognitoIdentityProviderService.{operation}");
    let authorization = credential_scope("cognito-idp");
    let answer = dispatch(
        router,
        host,
        "POST",
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("x-amz-target", &target),
            ("authorization", &authorization),
        ],
        body.to_string().into_bytes(),
    )
    .await;
    assert_eq!(
        answer.status, 200,
        "setup: cognito-idp {operation} on {host} must answer 200, got {} {}",
        answer.status, answer.text
    );
    answer.body
}

/// One API Gateway REST control-plane call, which must answer `expected_status`.
async fn control_plane(
    router: &MultiServiceRouter,
    what: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected_status: u16,
) -> Value {
    let authorization = credential_scope("apigateway");
    let answer = dispatch(
        router,
        &format!("apigateway.{REGION}.amazonaws.com"),
        method,
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization),
        ],
        body.map(|body| body.to_string().into_bytes())
            .unwrap_or_default(),
    )
    .await;
    assert_eq!(
        answer.status, expected_status,
        "setup: {what} must answer HTTP {expected_status}, got {} {}",
        answer.status, answer.text
    );
    answer.body
}

/// An ANONYMOUS GET of `url`, addressed at the authority the URL itself names — which is what
/// a relying party holding a token does with the locators it reads.
async fn fetch(router: &MultiServiceRouter, case: &str, url: &str) -> Answer {
    let rest = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("case {case:?}: {url:?} must be an https URL"));
    let (authority, path) = match rest.find('/') {
        Some(at) => (&rest[..at], &rest[at..]),
        None => (rest, "/"),
    };
    dispatch(router, authority, "GET", path, &[], Vec::new()).await
}

/// A required non-empty string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what} must carry a non-empty {member:?}, got {body}"))
}

/// The JSON of a compact JWS segment, decoded with this test target's own base64url coding
/// of a JOSE segment ([`decode_base64url`]).
fn segment_json(what: &str, segment: &str) -> Value {
    serde_json::from_slice(&decode_base64url(segment))
        .unwrap_or_else(|error| panic!("{what}: a JWS segment must carry JSON ({error})"))
}

/// The JOSE header and the claim set of the compact JWS `token`.
fn header_and_claims(what: &str, token: &str) -> (Value, Value) {
    let mut segments = token.split('.');
    let header = segments
        .next()
        .unwrap_or_else(|| panic!("{what}: a token is a compact JWS, got {token:?}"));
    let claims = segments
        .next()
        .unwrap_or_else(|| panic!("{what}: a token carries a claims segment, got {token:?}"));
    (segment_json(what, header), segment_json(what, claims))
}

/// One pool of the fixture: where it was created, where its tokens are minted, and the
/// handles the rows need.
struct Pool {
    case: &'static str,
    id: String,
    arn: String,
    client_id: String,
    /// The endpoint the sign-in is sent to, which is NOT the one the pool was created
    /// through: the issuer is the pool's stored one either way.
    sign_in_host: String,
}

/// Create a pool through `create_host`, with an app client and a `CONFIRMED` user.
async fn pool(
    router: &MultiServiceRouter,
    case: &'static str,
    name: &str,
    create_host: &str,
    sign_in_host: &str,
) -> Pool {
    let created = cognito(
        router,
        create_host,
        "CreateUserPool",
        &json!({ "PoolName": name }),
    )
    .await;
    let id = as_str("CreateUserPool", &created["UserPool"], "Id");
    let arn = as_str("CreateUserPool", &created["UserPool"], "Arn");
    let client = cognito(
        router,
        create_host,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": &id,
            "ClientName": format!("{name}-client"),
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH"],
        }),
    )
    .await;
    let client_id = as_str("CreateUserPoolClient", &client["UserPoolClient"], "ClientId");
    cognito(
        router,
        create_host,
        "AdminCreateUser",
        &json!({ "UserPoolId": &id, "Username": USERNAME, "MessageAction": "SUPPRESS" }),
    )
    .await;
    cognito(
        router,
        create_host,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": &id,
            "Username": USERNAME,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    Pool {
        case,
        id,
        arn,
        client_id,
        sign_in_host: sign_in_host.to_string(),
    }
}

#[test]
fn a_cognito_gate_publishes_an_issuer_this_served_emulator_answers_on() {
    std::thread::Builder::new()
        .name("cognito-claims-issuer-served".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                // Signature verification belongs to the emulator, not to this check.
                router.set_request_authentication(false);
                // This emulator IS served on a port, which is the deployment every client
                // of it meets — and the address it has of its own to publish.
                router.set_url_base(
                    EndpointUrl::parse(SERVED_BASE).expect("the suite's served base must parse"),
                );

                // 1. Two pools: one created through the emulator's served address, one
                //    through this service's published regional host. Each signs in on the
                //    OTHER endpoint.
                let published = published_host();
                let pools = [
                    pool(
                        &router,
                        "a pool created through the emulator's served address",
                        "claims-issuer-local",
                        SERVED_AUTHORITY,
                        &published,
                    )
                    .await,
                    pool(
                        &router,
                        "a pool created through this service's published regional host",
                        "claims-issuer-regional",
                        &published,
                        SERVED_AUTHORITY,
                    )
                    .await,
                ];

                // 2. The api: ONE COGNITO_USER_POOLS-gated MOCK method over BOTH pools,
                //    answering the issuer the authorizer published, deployed to a stage.
                let api = control_plane(
                    &router,
                    "CreateRestApi",
                    "POST",
                    "/restapis",
                    Some(json!({ "name": "claims-issuer-api" })),
                    201,
                )
                .await;
                let api_id = as_str("CreateRestApi", &api, "id");
                let root = as_str("CreateRestApi", &api, "rootResourceId");
                let authorizer = control_plane(
                    &router,
                    "CreateAuthorizer",
                    "POST",
                    &format!("/restapis/{api_id}/authorizers"),
                    Some(json!({
                        "name": "by-pools",
                        "type": "COGNITO_USER_POOLS",
                        "providerARNs": pools.iter().map(|pool| pool.arn.clone())
                            .collect::<Vec<String>>(),
                        "identitySource": format!("method.request.header.{TOKEN_HEADER}"),
                    })),
                    201,
                )
                .await;
                let authorizer_id = as_str("CreateAuthorizer", &authorizer, "id");
                let resource = control_plane(
                    &router,
                    "CreateResource",
                    "POST",
                    &format!("/restapis/{api_id}/resources/{root}"),
                    Some(json!({ "pathPart": "claims" })),
                    201,
                )
                .await;
                let resource_id = as_str("CreateResource", &resource, "id");
                control_plane(
                    &router,
                    "PutMethod",
                    "PUT",
                    &format!("/restapis/{api_id}/resources/{resource_id}/methods/GET"),
                    Some(json!({
                        "authorizationType": "COGNITO_USER_POOLS",
                        "authorizerId": authorizer_id,
                    })),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "PutIntegration",
                    "PUT",
                    &format!("/restapis/{api_id}/resources/{resource_id}/methods/GET/integration"),
                    Some(json!({ "type": "MOCK" })),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "PutMethodResponse",
                    "PUT",
                    &format!(
                        "/restapis/{api_id}/resources/{resource_id}/methods/GET/responses/200"
                    ),
                    Some(json!({})),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "PutIntegrationResponse",
                    "PUT",
                    &format!(
                        "/restapis/{api_id}/resources/{resource_id}/methods/GET/integration/responses/200"
                    ),
                    Some(json!({
                        "responseTemplates": { "application/json": ISSUER_TEMPLATE },
                    })),
                    201,
                )
                .await;
                control_plane(
                    &router,
                    "CreateDeployment",
                    "POST",
                    &format!("/restapis/{api_id}/deployments"),
                    Some(json!({ "stageName": STAGE })),
                    201,
                )
                .await;

                // 3. The rows: each pool, and each signed token of one sign-in. What each
                //    pool ANNOUNCED is collected from the claim the gate published, so the
                //    distinguishability check below reads what the emulator produced.
                let invoke_host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
                let mut announced_issuers: Vec<String> = Vec::new();
                for pool in &pools {
                    let case = pool.case;
                    let signed_in = cognito(
                        &router,
                        &pool.sign_in_host,
                        "AdminInitiateAuth",
                        &json!({
                            "UserPoolId": &pool.id,
                            "ClientId": &pool.client_id,
                            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
                            "AuthParameters": {
                                "USERNAME": USERNAME,
                                "PASSWORD": GOOD_PASSWORD,
                            },
                        }),
                    )
                    .await;
                    let result = &signed_in["AuthenticationResult"];
                    // A served emulator has an address of its own to publish, so that is
                    // the authority of the issuer, with the pool's id as the whole path.
                    let expected_issuer = format!("https://{SERVED_AUTHORITY}/{}", pool.id);

                    for member in ["IdToken", "AccessToken"] {
                        let token = result[member]
                            .as_str()
                            .unwrap_or_else(|| panic!("case {case:?}: a sign-in returns {member}"));
                        let what = format!("case {case:?} / {member}");
                        let (header, claims) = header_and_claims(&what, token);
                        let issuer = claims
                            .get("iss")
                            .and_then(Value::as_str)
                            .unwrap_or_else(|| {
                                panic!("{what}: a token carries a string iss claim, got {claims}")
                            })
                            .to_string();
                        assert_eq!(
                            issuer, expected_issuer,
                            "{what}: expected iss {expected_issuer:?}, got {issuer:?}"
                        );

                        // The gate admits the token and publishes that claim to the
                        // integration verbatim.
                        let answer = dispatch(
                            &router,
                            &invoke_host,
                            "GET",
                            &format!("/{STAGE}/claims"),
                            &[(TOKEN_HEADER, token)],
                            Vec::new(),
                        )
                        .await;
                        assert_eq!(
                            answer.status, 200,
                            "{what}: the authorizer must admit the pool's own token, got {} {}",
                            answer.status, answer.text
                        );
                        let published_claim = answer
                            .body
                            .get("iss")
                            .and_then(Value::as_str)
                            .unwrap_or("<no iss>");
                        assert_eq!(
                            published_claim, issuer,
                            "{what}: the integration must receive the iss claim as minted \
                             ({issuer:?}), got {published_claim:?} in {}",
                            answer.text
                        );
                        announced_issuers.push(published_claim.to_string());

                        // And that claim is a locator this emulator answers: the discovery
                        // document fetched from it names it and locates the key set under
                        // it, and that key set holds the key the token's `kid` names.
                        let discovery_url = format!("{published_claim}{OPENID_CONFIGURATION}");
                        let discovered = fetch(&router, &what, &discovery_url).await;
                        assert_eq!(
                            discovered.status, 200,
                            "{what}: {discovery_url} must be served by this emulator, got {} {}",
                            discovered.status, discovered.text
                        );
                        let stated = as_str(&what, &discovered.body, "issuer");
                        assert_eq!(
                            stated, issuer,
                            "{what}: the discovery document must state issuer {issuer:?}, got \
                             {stated:?}"
                        );
                        let jwks_uri = as_str(&what, &discovered.body, "jwks_uri");
                        assert_eq!(
                            jwks_uri,
                            format!("{issuer}{JWKS}"),
                            "{what}: the discovery document must locate the key set under the \
                             issuer, got {jwks_uri:?}"
                        );
                        let key_set = fetch(&router, &what, &jwks_uri).await;
                        assert_eq!(
                            key_set.status, 200,
                            "{what}: {jwks_uri} must be served by this emulator, got {} {}",
                            key_set.status, key_set.text
                        );
                        let key_id = as_str(&what, &header, "kid");
                        let published: Vec<String> = key_set.body["keys"]
                            .as_array()
                            .unwrap_or_else(|| {
                                panic!("{what}: a key set holds a keys array, got {}", key_set.text)
                            })
                            .iter()
                            .map(|key| as_str(&what, key, "kid"))
                            .collect();
                        assert!(
                            published.contains(&key_id),
                            "{what}: the key set at {jwks_uri} must hold the key {key_id:?} the \
                             token names, got {published:?}"
                        );
                    }
                }

                // One authorizer over two pools tells them apart by the issuer it
                // publishes: the claims the gate published for the two pools' tokens are
                // distinct, each naming its own pool.
                let distinct: std::collections::BTreeSet<&String> =
                    announced_issuers.iter().collect();
                assert_eq!(
                    (announced_issuers.len(), distinct.len()),
                    (pools.len() * 2, pools.len()),
                    "each pool's tokens must publish one issuer of its own, got \
                     {announced_issuers:?}"
                );
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
