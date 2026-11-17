// SPDX-License-Identifier: CC0-1.0

//! WHICH refusal a `COGNITO_USER_POOLS`-gated method answers a presented token with, across
//! every way that token's SIGNATURE can fail to check out — the split between "present a
//! credential" and "the credential you presented is not trusted".
//!
//! Two services and one engine, so this is a router suite: the tokens are minted by the Amazon
//! Cognito user pools service (signing name `cognito-idp`) and the gate that reads them is API
//! Gateway's own `execute-api` data plane. Nothing about a token is written here that a pool did
//! not mint: every row presents a token `AdminInitiateAuth` answered with, or that same token
//! with one of its three segments rewritten.
//!
//! The RULE the rows assert, one level above the recorded case:
//!
//! > A `COGNITO_USER_POOLS` authorizer refuses in the class of the DECISION STAGE the presented
//! > value failed at. A value that is not a signed compact JWS at all — absent, empty, not three
//! > segments, nothing signed it — and a signed token whose `iss` names no user pool the
//! > authorizer's `providerARNs` list, authorized nobody: `401` `{"message":"Unauthorized"}`,
//! > fixed text. A value the authorizer DID read as a signed token of a pool it TRUSTS, whose
//! > signature does not check out against that pool's published key set, is that authorizer
//! > denying the call: `403`, through the same `ACCESS_DENIED` gateway response a `CUSTOM`
//! > authorizer's denial is answered through, so its sentence travels in that response's
//! > IAM-shaped `Message` member. The two stages are ORDERED — the issuer is settled before the
//! > signature — so the same unverifiable bytes earn `401` at an authorizer that does not list
//! > their pool and `403` at one that does. Neither class reaches the integration.
//!
//! The rows span what that rule generalises over: WHICH segment of the token was rewritten (the
//! signature, the claim set, the JOSE header's key id), HOW the signature fails (one character
//! flipped at its front, one at its end, one character short, replaced by a signature the same
//! pool really produced over OTHER bytes, replaced by another pool's, emptied, removed), HOW the
//! token is presented (bare and under the RFC 6750 bearer scheme) and WHICH gate it meets (an
//! authorizer listing the token's own pool, and one listing both pools) — plus the intact token
//! as the positive control at each gate.
//!
//! The recorded leg of `apigw-cognito-authorizer-invalid-token-negative` is one row of this
//! table: the pool's identity token with the first character of its signature segment flipped.
//!
//! Assembling the router costs stack, so it runs on an explicitly sized thread for the same
//! reason `boot_serve.rs` does.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above the size `boot_serve.rs` uses.
const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The region every call is signed for, and so the region of each pool's issuer and of the api's
/// own invoke host.
const REGION: &str = "us-west-2";

/// The stage the api is deployed to.
const STAGE: &str = "prod";

/// The permanent password this suite's fixture users sign in with — a value written for this test
/// and used nowhere else. It satisfies a pool's default password policy.
const GOOD_PASSWORD: &str = "Str0ng-Passw0rd!";

/// The header each authorizer below names as its identity source.
const TOKEN_HEADER: &str = "Authorization";

/// The status and sentence a request this authorizer does not admit at all earns.
const UNAUTHORIZED: u16 = 401;
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// The status and sentence a token of a TRUSTED pool that does not verify against that pool's
/// published key set earns: the authorizer denying the call, whose sentence travels in the
/// IAM-shaped `Message` member of the `ACCESS_DENIED` gateway response.
const DENIED: u16 = 403;
const DENIED_MESSAGE: &str = "User is not authorized to access this resource";

/// The sentence the `API_CONFIGURATION_ERROR` / `AUTHORIZER_CONFIGURATION_ERROR` gateway
/// responses carry — the `500` that must never answer a question about a token.
const CONFIGURATION_ERROR: &str = "Internal server error";

/// The marker the `MOCK` integration behind each gated resource answers, so a row can see
/// whether the integration RAN at all: a refusal withholds the backend's body entirely.
const INTEGRATION_MARKER: &str = "integration-ran";

/// The body that integration answers: the marker above plus the `sub` of the token the authorizer
/// accepted, which is how an admitted row observes that the gate let the request through.
const CLAIMS_TEMPLATE: &str =
    "{\"ran\":\"integration-ran\",\"sub\":\"$context.authorizer.claims.sub\"}";

/// A key id no pool publishes, used as the `kid` a rewritten JOSE header names.
const UNPUBLISHED_KEY_ID: &str = "no-such-key";

/// One request dispatched through the router, and what came back.
struct Answer {
    status: u16,
    body: Value,
    /// The body as it arrived, for a response that is not JSON at all.
    text: String,
}

impl Answer {
    /// One string member of a data-plane body, or `"<no message>"`.
    fn member(&self, name: &str) -> String {
        self.body
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or("<no message>")
            .to_string()
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, self.text)
    }
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
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let text = String::from_utf8_lossy(&bytes).to_string();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Answer { status, body, text }
}

/// The credential scope a signed SDK request carries. The signature is never checked — this
/// router runs with authentication off.
fn credential_scope(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 \
         Credential=AKIDEXAMPLE/20260101/{REGION}/{signing_name}/aws4_request, \
         SignedHeaders=host, Signature=not-verified"
    )
}

/// One awsJson1_1 call to the Amazon Cognito user pools service, which must succeed.
async fn cognito(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let target = format!("AWSCognitoIdentityProviderService.{operation}");
    let authorization = credential_scope("cognito-idp");
    let answer = dispatch(
        router,
        &format!("cognito-idp.{REGION}.amazonaws.com"),
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
        answer.status,
        200,
        "setup: cognito-idp {operation} must answer 200, got {}",
        answer.summary()
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
        answer.status,
        expected_status,
        "{what} must answer HTTP {expected_status}, got {}",
        answer.summary()
    );
    answer.body
}

/// A required string member of a response body.
fn as_str(what: &str, body: &Value, member: &str) -> String {
    body.get(member)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| panic!("{what} must carry a non-empty {member:?}, got {body}"))
}

/// One base64url segment, decoded: "base64url without padding" (RFC 7515 §2), which is RFC 4648
/// §5 — the two alphabet substitutions and the restored padding turn it back into the standard
/// alphabet this workspace already decodes.
fn decode_segment(segment: &str) -> Vec<u8> {
    let mut standard = segment.replace('-', "+").replace('_', "/");
    while !standard.len().is_multiple_of(4) {
        standard.push('=');
    }
    aws_smithy_types::base64::decode(&standard)
        .unwrap_or_else(|e| panic!("a minted token's segment must be base64url ({e})"))
}

/// `value` as one base64url segment of a compact JWS.
fn encode_segment(value: &Value) -> String {
    aws_smithy_types::base64::encode(value.to_string().as_bytes())
        .replace('+', "-")
        .replace('/', "_")
        .trim_end_matches('=')
        .to_string()
}

/// The three segments of a minted compact JWS, as they travel.
fn segments_of(token: &str) -> (String, String, String) {
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(
        segments.len(),
        3,
        "a minted token must be a compact JWS of three segments, got {} segment(s)",
        segments.len()
    );
    (
        segments[0].to_string(),
        segments[1].to_string(),
        segments[2].to_string(),
    )
}

/// The JOSE header and the claim set of a compact JWS: its first two segments, decoded.
fn parts_of(token: &str) -> (Value, Value) {
    let (header, claims, _signature) = segments_of(token);
    let read = |segment: &str| -> Value {
        serde_json::from_slice(&decode_segment(segment))
            .unwrap_or_else(|e| panic!("a minted token's segment must be JSON ({e})"))
    };
    (read(&header), read(&claims))
}

/// One claim of a minted token, as a string.
fn claim(token: &str, name: &str) -> String {
    let (_header, claims) = parts_of(token);
    claims
        .get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("a minted token must carry a string {name:?} claim, got {claims}"))
        .to_string()
}

/// The minted token with `signature` in place of its own signature segment — everything the pool
/// signed left exactly as it signed it.
fn with_signature(token: &str, signature: &str) -> String {
    let (header, claims, _own) = segments_of(token);
    format!("{header}.{claims}.{signature}")
}

/// The minted token's own signature segment.
fn signature_of(token: &str) -> String {
    segments_of(token).2
}

/// The minted token re-encoded with `header` as its JOSE header and its own signature kept.
fn with_header(token: &str, header: &Value) -> String {
    let (_own, claims, signature) = segments_of(token);
    format!("{}.{claims}.{signature}", encode_segment(header))
}

/// The minted token re-encoded with `claims` as its claim set and its own signature kept — so the
/// signature no longer covers the bytes it travels with.
fn with_claims(token: &str, claims: &Value) -> String {
    let (header, _own, signature) = segments_of(token);
    format!("{header}.{}.{signature}", encode_segment(claims))
}

/// `segment` with its first character replaced by a DIFFERENT base64url character, which is the
/// one-character tamper the recorded case applies.
fn first_character_flipped(segment: &str) -> String {
    flipped(segment, 0)
}

/// `segment` with its last character replaced by a different base64url character.
fn last_character_flipped(segment: &str) -> String {
    flipped(segment, segment.chars().count().saturating_sub(1))
}

/// `segment` with the character at `position` replaced by a different base64url character.
fn flipped(segment: &str, position: usize) -> String {
    segment
        .chars()
        .enumerate()
        .map(|(index, character)| {
            if index == position {
                if character == 'A' {
                    'B'
                } else {
                    'A'
                }
            } else {
                character
            }
        })
        .collect()
}

/// `segment` one character short, which is a signature of fewer bytes than the algorithm's.
fn one_character_short(segment: &str) -> String {
    let mut shortened: Vec<char> = segment.chars().collect();
    shortened.pop();
    shortened.into_iter().collect()
}

/// What one pool's sign-in produced: the pool's ARN, its app client id and the two tokens of the
/// sign-in.
struct Pool {
    arn: String,
    client_id: String,
    id_token: String,
    access_token: String,
}

/// Mint one pool, one app client and one confirmed user, and sign that user in.
async fn pool_with_signed_in_user(
    router: &MultiServiceRouter,
    pool_name: &str,
    username: &str,
) -> Pool {
    let pool = cognito(router, "CreateUserPool", &json!({ "PoolName": pool_name })).await;
    let pool_id = as_str("CreateUserPool", &pool["UserPool"], "Id");
    let arn = as_str("CreateUserPool", &pool["UserPool"], "Arn");

    let client = cognito(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": pool_id,
            "ClientName": format!("{pool_name}-client"),
            // The flow the sign-in below uses, plus the refresh flow AWS requires beside any
            // other `ALLOW_` flow.
            "ExplicitAuthFlows": ["ALLOW_ADMIN_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"]
        }),
    )
    .await;
    let client_id = as_str("CreateUserPoolClient", &client["UserPoolClient"], "ClientId");

    cognito(
        router,
        "AdminCreateUser",
        &json!({ "UserPoolId": pool_id, "Username": username }),
    )
    .await;
    cognito(
        router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": pool_id,
            "Username": username,
            "Password": GOOD_PASSWORD,
            "Permanent": true
        }),
    )
    .await;
    let signed_in = cognito(
        router,
        "AdminInitiateAuth",
        &json!({
            "UserPoolId": pool_id,
            "ClientId": client_id,
            "AuthFlow": "ADMIN_USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD }
        }),
    )
    .await;
    let result = &signed_in["AuthenticationResult"];
    Pool {
        arn,
        client_id,
        id_token: as_str("AdminInitiateAuth", result, "IdToken"),
        access_token: as_str("AdminInitiateAuth", result, "AccessToken"),
    }
}

/// One `COGNITO_USER_POOLS`-gated `MOCK` method under the api's root, answering
/// [`CLAIMS_TEMPLATE`]: the marker plus the claims the authorizer published.
async fn gated_resource(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
    path_part: &str,
    authorizer: Value,
) {
    let created = control_plane(
        router,
        &format!("CreateAuthorizer({path_part})"),
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(authorizer),
        201,
    )
    .await;
    let authorizer_id = as_str("CreateAuthorizer", &created, "id");

    let resource = control_plane(
        router,
        &format!("CreateResource({path_part})"),
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": path_part })),
        201,
    )
    .await;
    let resource_id = as_str("CreateResource", &resource, "id");

    control_plane(
        router,
        &format!("PutMethod({path_part})"),
        "PUT",
        &format!("/restapis/{api}/resources/{resource_id}/methods/GET"),
        Some(json!({
            "authorizationType": "COGNITO_USER_POOLS",
            "authorizerId": authorizer_id
        })),
        201,
    )
    .await;
    control_plane(
        router,
        &format!("PutIntegration({path_part})"),
        "PUT",
        &format!("/restapis/{api}/resources/{resource_id}/methods/GET/integration"),
        Some(json!({ "type": "MOCK" })),
        201,
    )
    .await;
    control_plane(
        router,
        &format!("PutMethodResponse({path_part})"),
        "PUT",
        &format!("/restapis/{api}/resources/{resource_id}/methods/GET/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    control_plane(
        router,
        &format!("PutIntegrationResponse({path_part})"),
        "PUT",
        &format!("/restapis/{api}/resources/{resource_id}/methods/GET/integration/responses/200"),
        Some(json!({ "responseTemplates": { "application/json": CLAIMS_TEMPLATE } })),
        201,
    )
    .await;
}

/// What the gate owes one row.
#[derive(Debug, Clone)]
enum Expected {
    /// The gate admitted the request, so the integration ran and answered this `sub`.
    Served(String),
    /// What the request presented is no signed token of a pool this authorizer lists, so it
    /// authorized nobody.
    Unauthorized,
    /// The token names a pool the authorizer TRUSTS and does not verify against that pool's
    /// published key set, which is that authorizer denying the call.
    Denied,
}

/// One request to the deployed stage: the gated resource it addresses, the value presented at the
/// authorizer's identity source, and what the gate owes it.
struct Row {
    case: String,
    path_part: &'static str,
    /// The value the [`TOKEN_HEADER`] carries, or `None` to present nothing at all.
    presented: Option<String>,
    expected: Expected,
}

#[test]
fn a_cognito_authorizer_refuses_an_unverifiable_token_as_a_denial() {
    std::thread::Builder::new()
        .name("apigateway-cognito-signature-class".to_string())
        .stack_size(BOOT_STACK_SIZE)
        .spawn(|| {
            let runtime = tokio::runtime::Builder::new_current_thread()
                // The IO and TIME drivers a service's background work needs; see the sibling
                // `apigateway_cognito_user_pools_authorizer` suite.
                .enable_all()
                .build()
                .expect("build a current-thread runtime");
            runtime.block_on(async {
                let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
                let mut router = build_router(engine);
                // Signature verification of the SDK call belongs to the emulator, not to this
                // check; the signature under test is the presented token's.
                router.set_request_authentication(false);

                // 1. The pool the gates trust, and a second pool whose tokens one gate lists and
                //    the other does not.
                let home =
                    pool_with_signed_in_user(&router, "sig-class-pool", "sig-class-user").await;
                let foreign =
                    pool_with_signed_in_user(&router, "sig-class-other", "other-user").await;
                let home_sub = claim(&home.id_token, "sub");
                let foreign_sub = claim(&foreign.id_token, "sub");
                let (home_header, home_claims) = parts_of(&home.id_token);

                // 2. The api, one gate per trusted-pool list, and the stage.
                let api = control_plane(
                    &router,
                    "CreateRestApi",
                    "POST",
                    "/restapis",
                    Some(json!({ "name": "cognito-signature-class-api" })),
                    201,
                )
                .await;
                let api_id = as_str("CreateRestApi", &api, "id");
                let root = as_str("CreateRestApi", &api, "rootResourceId");
                let header_source = format!("method.request.header.{TOKEN_HEADER}");

                gated_resource(
                    &router,
                    &api_id,
                    &root,
                    "by-home-pool",
                    json!({
                        "name": "home-pool",
                        "type": "COGNITO_USER_POOLS",
                        "providerARNs": [home.arn],
                        "identitySource": header_source
                    }),
                )
                .await;
                gated_resource(
                    &router,
                    &api_id,
                    &root,
                    "by-both-pools",
                    json!({
                        "name": "both-pools",
                        "type": "COGNITO_USER_POOLS",
                        "providerARNs": [home.arn, foreign.arn],
                        "identitySource": header_source
                    }),
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

                // 3. The rows: the ways a signature can fail to check out, and the boundary cases
                //    the other side of the split.
                let own_signature = signature_of(&home.id_token);
                let mut unknown_key = home_header.clone();
                if let Some(members) = unknown_key.as_object_mut() {
                    members.insert("kid".to_string(), json!(UNPUBLISHED_KEY_ID));
                }
                let mut other_subject = home_claims.clone();
                if let Some(members) = other_subject.as_object_mut() {
                    members.insert(
                        "sub".to_string(),
                        json!("00000000-0000-0000-0000-000000000000"),
                    );
                }
                let tampered = with_signature(
                    &home.id_token,
                    &first_character_flipped(&own_signature),
                );
                let foreign_tampered = with_signature(
                    &foreign.id_token,
                    &first_character_flipped(&signature_of(&foreign.id_token)),
                );
                let rows: Vec<Row> = vec![
                    Row {
                        case: "the pool's intact identity token".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(home.id_token.clone()),
                        expected: Expected::Served(home_sub.clone()),
                    },
                    // The recorded case: one character flipped in the signature segment, the rest
                    // of the token exactly as the pool signed it.
                    Row {
                        case: "the first character of the signature flipped".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(tampered.clone()),
                        expected: Expected::Denied,
                    },
                    Row {
                        case: "the same token under the RFC 6750 bearer scheme".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(format!("Bearer {tampered}")),
                        expected: Expected::Denied,
                    },
                    Row {
                        case: "the last character of the signature flipped".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(with_signature(
                            &home.id_token,
                            &last_character_flipped(&own_signature),
                        )),
                        expected: Expected::Denied,
                    },
                    Row {
                        case: "a signature one character short".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(with_signature(
                            &home.id_token,
                            &one_character_short(&own_signature),
                        )),
                        expected: Expected::Denied,
                    },
                    // A signature the SAME pool really produced, over the other token of the same
                    // sign-in: a real signature of a trusted pool, over bytes other than these.
                    Row {
                        case: "the pool's own signature of its other token".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(with_signature(
                            &home.id_token,
                            &signature_of(&home.access_token),
                        )),
                        expected: Expected::Denied,
                    },
                    Row {
                        case: "another pool's signature spliced on".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(with_signature(
                            &home.id_token,
                            &signature_of(&foreign.id_token),
                        )),
                        expected: Expected::Denied,
                    },
                    Row {
                        case: "the subject rewritten after signing".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(with_claims(&home.id_token, &other_subject)),
                        expected: Expected::Denied,
                    },
                    Row {
                        case: "a JOSE header naming a key the pool does not publish".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(with_header(&home.id_token, &unknown_key)),
                        expected: Expected::Denied,
                    },
                    // The other side of the split: a value nothing signed, or that is not a
                    // compact JWS at all, was never read as a credential of any pool.
                    Row {
                        case: "an empty signature segment, so nothing signed it".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(with_signature(&home.id_token, "")),
                        expected: Expected::Unauthorized,
                    },
                    Row {
                        case: "the signature segment removed entirely".to_string(),
                        path_part: "by-home-pool",
                        presented: Some({
                            let (header, claims, _signature) = segments_of(&home.id_token);
                            format!("{header}.{claims}")
                        }),
                        expected: Expected::Unauthorized,
                    },
                    Row {
                        case: "a value that is not a token at all".to_string(),
                        path_part: "by-home-pool",
                        presented: Some("not-a-token".to_string()),
                        expected: Expected::Unauthorized,
                    },
                    Row {
                        case: "nothing at the identity source".to_string(),
                        path_part: "by-home-pool",
                        presented: None,
                        expected: Expected::Unauthorized,
                    },
                    // The ORDER of the two stages: the issuer is settled first, so a token of a
                    // pool this gate does not list is refused without its signature deciding
                    // anything — intact or tampered.
                    Row {
                        case: "an unlisted pool's intact token".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(foreign.id_token.clone()),
                        expected: Expected::Unauthorized,
                    },
                    Row {
                        case: "an unlisted pool's token with its signature flipped".to_string(),
                        path_part: "by-home-pool",
                        presented: Some(foreign_tampered.clone()),
                        expected: Expected::Unauthorized,
                    },
                    // The same bytes at a gate that DOES list that pool: now the issuer is
                    // trusted, so the signature decides, and it does not check out.
                    Row {
                        case: "a listed pool's intact token".to_string(),
                        path_part: "by-both-pools",
                        presented: Some(foreign.id_token.clone()),
                        expected: Expected::Served(foreign_sub.clone()),
                    },
                    Row {
                        case: "a listed pool's token with its signature flipped".to_string(),
                        path_part: "by-both-pools",
                        presented: Some(foreign_tampered),
                        expected: Expected::Denied,
                    },
                ];

                let invoke_host = format!("{api_id}.execute-api.{REGION}.amazonaws.com");
                for row in &rows {
                    let path = format!("/{STAGE}/{}", row.path_part);
                    let mut headers: Vec<(&str, &str)> = Vec::new();
                    if let Some(presented) = &row.presented {
                        headers.push((TOKEN_HEADER, presented.as_str()));
                    }
                    let answer =
                        dispatch(&router, &invoke_host, "GET", &path, &headers, Vec::new()).await;
                    // A question about a token is never answered by the 500 of an authorizer
                    // whose decision could not be obtained.
                    assert_ne!(
                        answer.member("message"),
                        CONFIGURATION_ERROR,
                        "case {:?}: the authorizer must decide the call, not report a \
                         configuration error; got {}",
                        row.case,
                        answer.summary()
                    );
                    match &row.expected {
                        Expected::Unauthorized | Expected::Denied => {
                            // A denial states its sentence in the IAM-shaped `Message` member of
                            // the `ACCESS_DENIED` gateway response; every other refusal of this
                            // gate states its own in `message`.
                            let denied = matches!(row.expected, Expected::Denied);
                            let (status, message, sentence) = if denied {
                                (DENIED, answer.member("Message"), DENIED_MESSAGE)
                            } else {
                                (
                                    UNAUTHORIZED,
                                    answer.member("message"),
                                    UNAUTHORIZED_MESSAGE,
                                )
                            };
                            assert_eq!(
                                (answer.status, message.as_str()),
                                (status, sentence),
                                "case {:?}: expected {status} {sentence:?}, got {}",
                                row.case,
                                answer.summary()
                            );
                            // The refusal is taken BEFORE the integration, so the backend's body
                            // never appears.
                            assert!(
                                !answer.text.contains(INTEGRATION_MARKER),
                                "case {:?}: a refused request must not carry the integration's \
                                 body, got {}",
                                row.case,
                                answer.summary()
                            );
                        }
                        Expected::Served(sub) => {
                            assert_eq!(
                                answer.status,
                                200,
                                "case {:?}: expected the integration's 200, got {}",
                                row.case,
                                answer.summary()
                            );
                            assert_eq!(
                                answer.member("ran").as_str(),
                                INTEGRATION_MARKER,
                                "case {:?}: expected the integration's body, got {}",
                                row.case,
                                answer.summary()
                            );
                            assert_eq!(
                                answer.member("sub").as_str(),
                                sub.as_str(),
                                "case {:?}: expected the token's sub {sub:?} to reach the \
                                 integration, got {}",
                                row.case,
                                answer.summary()
                            );
                        }
                    }
                }

                // The app client each token names, read off the minted token rather than written
                // here: the gate's decision is about the pool's own records.
                assert_eq!(
                    home_claims.get("aud").and_then(Value::as_str),
                    Some(home.client_id.as_str()),
                    "the pool's identity token must name the client it was minted for, got {home_claims}"
                );
                assert_ne!(
                    home.client_id, foreign.client_id,
                    "the two pools must have minted their own app clients"
                );
            });
        })
        .expect("spawn the sized thread")
        .join()
        .expect("the suite thread must not panic");
}
