// SPDX-License-Identifier: CC0-1.0

//! A FORGED access token over the wire: what a pool publishes for a verifier of its
//! tokens, and how the self-service operations refuse a token that does not check out
//! against that document.
//!
//! The rule under test: a pool signs each access token it issues with its own key and
//! publishes the verifying half at `/<UserPoolId>/.well-known/jwks.json` as an RSA public
//! key carrying the two parameters RFC 7518 §6.3.1 requires of one — the modulus `n` and
//! the exponent `e` — so a genuine token's signature verifies against the published
//! document. A value presented as an `AccessToken` that is a compact JWS naming a pool of
//! this service as its issuer, and whose signature does NOT check out against that pool's
//! published key set, is refused `NotAuthorizedException` with "Could not verify signature
//! for Access Token"; a value that is no signed token of this service at all — an
//! UNSECURED one (RFC 7519 §6.1 leaves no signature to check), a truncated one, one naming
//! an issuer this service holds no pool for, a value that is not a JWS — is refused
//! `NotAuthorizedException` with "Invalid Access Token". Neither refusal writes anything.
//!
//! The pool, app client, user and tokens are real: they come from `CreateUserPool`,
//! `CreateUserPoolClient`, `AdminCreateUser`, `AdminSetUserPassword` and `InitiateAuth`,
//! and the key set is read the way a verifier reads it — an anonymous GET of the pool's
//! published document. The table's rows differ along HOW the token was forged and along
//! which self-service operation it is presented to, a read and a write among them.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

/// A request's SigV4 credential scope is what routes an administrative call to this
/// service. The signature is never checked here: the suite turns request authentication
/// off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// This service's regional endpoint host, which a signed call is addressed to.
const ENDPOINT_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// The emulator's own endpoint: an address naming no service, which is how a signed-in
/// user's own client calls the self-service operations.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// The status this service answers a client error with.
const REFUSAL_STATUS: u16 = 400;

/// The error every refusal of a token below reports.
const NOT_AUTHORIZED: &str = "NotAuthorizedException";

/// The message a token this service has no record of and cannot verify a signature for
/// gets: the refusal of a value that is no signed token of this service.
const INVALID_ACCESS_TOKEN: &str = "Invalid Access Token";

/// The message a token whose signature does not check out against the published key set
/// gets.
const SIGNATURE_UNVERIFIED: &str = "Could not verify signature for Access Token";

/// The `alg` of every token a pool signs and of every key it publishes (RFC 7518 §3.3).
const RS256: &str = "RS256";

/// The `kty` of the key family `RS256` is defined over (RFC 7518 §6.3).
const RSA_KEY_TYPE: &str = "RSA";

/// The `use` a published verification key carries (RFC 7517 §4.2).
const SIGNATURE_USE: &str = "sig";

/// The smallest modulus RFC 7518 §3.3 allows for `RS256` ("A key of size 2048 bits or
/// larger MUST be used with these algorithms"), in octets.
const MINIMUM_MODULUS_BYTES: usize = 2048 / 8;

/// The public exponent an RSA key pair is generated with, big-endian: 65537.
const PUBLIC_EXPONENT: [u8; 3] = [0x01, 0x00, 0x01];

/// One emulator with just this service mounted.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(engine));
    router.set_request_authentication(false);
    router
}

/// Dispatch `operation` with `body` and return its status and JSON response body.
async fn dispatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    signed: bool,
) -> (u16, Value) {
    let host = if signed { ENDPOINT_HOST } else { EMULATOR_HOST };
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        );
    if signed {
        request = request.header("authorization", AUTHORIZATION);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(body.to_string()))
                .expect("the request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, payload)
}

/// Call the signed `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = dispatch(router, operation, body, true).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The wire error code of an error body: the `__type` without the shape namespace.
fn error_code(payload: &Value) -> &str {
    let raw = payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>");
    raw.split('#').next_back().unwrap_or(raw)
}

/// The `message` of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// A pool, an app client that allows password sign-in, and a confirmed user of it.
struct Fixture {
    user_pool_id: String,
    client_id: String,
    username: String,
}

/// Create that fixture, named after `label` so two of them do not collide.
async fn fixture(router: &MultiServiceRouter, label: &str) -> Fixture {
    let created = ok(router, "CreateUserPool", &json!({ "PoolName": label })).await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string();
    let client = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": format!("{label}-client"),
            "ExplicitAuthFlows": ["ALLOW_USER_PASSWORD_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"],
        }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string();
    let username = format!("{label}-user");
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": [{ "Name": "email", "Value": "holder@example.test" }],
        }),
    )
    .await;
    ok(
        router,
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
        username,
    }
}

/// The `AuthenticationResult` of a password sign-in of `fixture`'s user.
async fn sign_in(router: &MultiServiceRouter, fixture: &Fixture) -> Value {
    let (status, payload) = dispatch(
        router,
        "InitiateAuth",
        &json!({
            "ClientId": fixture.client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": fixture.username, "PASSWORD": GOOD_PASSWORD },
        }),
        false,
    )
    .await;
    assert_eq!(
        status, 200,
        "the sign-in must succeed, got {status} with {payload}"
    );
    payload["AuthenticationResult"].clone()
}

/// The key set the pool publishes, fetched the way a verifier does: an ANONYMOUS GET of
/// `/<UserPoolId>/.well-known/jwks.json`.
async fn served_jwks(router: &MultiServiceRouter, user_pool_id: &str) -> Value {
    let request = http::Request::builder()
        .method("GET")
        .uri(format!("/{user_pool_id}/.well-known/jwks.json"))
        .header("host", ENDPOINT_HOST)
        .body(Bytes::new())
        .expect("the request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    assert_eq!(
        status,
        200,
        "the pool {user_pool_id} must serve its key set, got {status} with {:?}",
        String::from_utf8_lossy(response.body())
    );
    serde_json::from_slice(response.body()).unwrap_or_else(|error| {
        panic!(
            "the key set must be JSON ({error}): {:?}",
            String::from_utf8_lossy(response.body())
        )
    })
}

/// The three segments of a compact JWS, as the token carries them.
fn segments(token: &str) -> (String, String, String) {
    let parts: Vec<&str> = token.split('.').collect();
    assert_eq!(
        parts.len(),
        3,
        "a compact JWS carries three segments, got {token:?}"
    );
    (
        parts[0].to_string(),
        parts[1].to_string(),
        parts[2].to_string(),
    )
}

/// One base64url segment read as the JSON object it encodes.
fn decoded(segment: &str, what: &str) -> Value {
    let bytes = service_common::jwt::decode_base64url(segment)
        .unwrap_or_else(|| panic!("the {what} segment must be base64url, got {segment:?}"));
    serde_json::from_slice(&bytes)
        .unwrap_or_else(|error| panic!("the {what} segment must be JSON ({error})"))
}

/// A JSON value as the base64url segment a compact JWS carries it in.
fn encoded(value: &Value) -> String {
    service_common::jwt::encode_base64url(value.to_string().as_bytes())
}

/// The `kid` the header of `token` names.
fn token_kid(token: &str) -> String {
    let (header, _, _) = segments(token);
    decoded(&header, "header")["kid"]
        .as_str()
        .unwrap_or_else(|| panic!("the token header names a kid, got {token:?}"))
        .to_string()
}

/// How one row forges the token it presents.
#[derive(Clone, Copy, Debug)]
enum Forgery {
    /// The genuine token the sign-in issued, which is not forged at all.
    Genuine,
    /// A group claim injected into the payload, carried under the signature the pool
    /// computed over the ORIGINAL payload.
    InjectedClaim,
    /// The last byte of the signature flipped, over the original header and payload.
    FlippedSignature,
    /// The signature of the OTHER token of the same sign-in, over this token's bytes.
    SignatureOfAnotherToken,
    /// The same claims, signed with a freshly generated key of the same algorithm — one
    /// the pool does not publish — and naming that key in its header.
    SignedByAnotherKey,
    /// The header rewritten to the unsecured form and the signature dropped (RFC 7519
    /// §6.1), which is the token "no issuer signed".
    Unsecured,
    /// The signature segment removed, leaving two segments.
    Truncated,
    /// The payload's issuer rewritten to a pool this service holds none of.
    ForeignIssuer,
    /// A value satisfying `TokenModelType`'s pattern that is no compact JWS at all.
    NotAToken,
}

impl Forgery {
    /// The value this row presents as its `AccessToken`, forged out of `access_token`
    /// (with `id_token` as the other token of the same sign-in).
    fn token(self, access_token: &str, id_token: &str) -> String {
        let (header, payload, signature) = segments(access_token);
        match self {
            Self::Genuine => access_token.to_string(),
            Self::InjectedClaim => {
                let mut claims = decoded(&payload, "payload");
                claims["cognito:groups"] = json!(["administrators"]);
                format!("{header}.{}.{signature}", encoded(&claims))
            }
            Self::FlippedSignature => {
                let mut bytes = service_common::jwt::decode_base64url(&signature)
                    .expect("the signature segment is base64url");
                let last = bytes.len() - 1;
                bytes[last] ^= 0x01;
                format!(
                    "{header}.{payload}.{}",
                    service_common::jwt::encode_base64url(&bytes)
                )
            }
            Self::SignatureOfAnotherToken => {
                let (_, _, other) = segments(id_token);
                format!("{header}.{payload}.{other}")
            }
            Self::SignedByAnotherKey => {
                let key = service_common::jws_signing::SigningKey::generate(RS256)
                    .expect("a freshly generated RSA key");
                key.signed_compact_jws(&decoded(&payload, "payload"))
                    .expect("the claims are signed with it")
            }
            Self::Unsecured => {
                let mut jose = decoded(&header, "header");
                jose["alg"] = json!("none");
                format!("{}.{payload}.", encoded(&jose))
            }
            Self::Truncated => format!("{header}.{payload}"),
            Self::ForeignIssuer => {
                let mut claims = decoded(&payload, "payload");
                claims["iss"] = json!(format!("https://{ENDPOINT_HOST}/us-east-1_nosuchpool"));
                format!("{header}.{}.{signature}", encoded(&claims))
            }
            Self::NotAToken => "not-an-access-token".to_string(),
        }
    }
}

/// One row of the forgery table: how the token was forged, and the message the refusal
/// owes it. A row expecting no message is the genuine token, which is served.
struct Row {
    case: &'static str,
    forgery: Forgery,
    expected_message: Option<&'static str>,
}

/// The table. The rows span what the rule generalises over: a token whose signature was
/// computed over OTHER bytes than the ones presented (a rewritten payload, a flipped
/// signature, another token's signature), one signed by a key the pool does not publish,
/// and the values that are no signed token of this service at all (the unsecured form, a
/// truncated one, one naming an issuer no pool of this service publishes, and a value that
/// is not a JWS) — beside the genuine token, which every row must leave working.
const ROWS: &[Row] = &[
    Row {
        case: "the genuine access token the sign-in issued",
        forgery: Forgery::Genuine,
        expected_message: None,
    },
    Row {
        case: "a group claim injected under the original signature",
        forgery: Forgery::InjectedClaim,
        expected_message: Some(SIGNATURE_UNVERIFIED),
    },
    Row {
        case: "the last byte of the signature flipped",
        forgery: Forgery::FlippedSignature,
        expected_message: Some(SIGNATURE_UNVERIFIED),
    },
    Row {
        case: "the signature of the other token of the same sign-in",
        forgery: Forgery::SignatureOfAnotherToken,
        expected_message: Some(SIGNATURE_UNVERIFIED),
    },
    Row {
        case: "the same claims signed by a key the pool does not publish",
        forgery: Forgery::SignedByAnotherKey,
        expected_message: Some(SIGNATURE_UNVERIFIED),
    },
    Row {
        case: "the unsecured form: alg none and no signature",
        forgery: Forgery::Unsecured,
        expected_message: Some(INVALID_ACCESS_TOKEN),
    },
    Row {
        case: "the signature segment removed",
        forgery: Forgery::Truncated,
        expected_message: Some(INVALID_ACCESS_TOKEN),
    },
    Row {
        case: "an issuer this service holds no pool for",
        forgery: Forgery::ForeignIssuer,
        expected_message: Some(INVALID_ACCESS_TOKEN),
    },
    Row {
        case: "a value that is no compact JWS",
        forgery: Forgery::NotAToken,
        expected_message: Some(INVALID_ACCESS_TOKEN),
    },
];

/// The self-service operations each row is presented to: a read of the signed-in user, a
/// write of its attributes, a read of its devices and the sign-out of its session. Each
/// one identifies its user by the `AccessToken` alone.
const OPERATIONS: [&str; 4] = [
    "GetUser",
    "UpdateUserAttributes",
    "ListDevices",
    "GlobalSignOut",
];

/// The request `operation` takes for `token`: the access token, plus whatever else the
/// operation models as required.
fn request(operation: &str, token: &str) -> Value {
    match operation {
        "UpdateUserAttributes" => json!({
            "AccessToken": token,
            "UserAttributes": [{ "Name": "nickname", "Value": "forged" }],
        }),
        _ => json!({ "AccessToken": token }),
    }
}

#[tokio::test]
async fn a_pool_publishes_the_rsa_key_its_access_token_verifies_against() {
    let router = router();
    let fixture = fixture(&router, "published-key-pool").await;
    let result = sign_in(&router, &fixture).await;
    let jwks = served_jwks(&router, &fixture.user_pool_id).await;
    let keys = jwks["keys"]
        .as_array()
        .unwrap_or_else(|| panic!("a key set carries a keys list, got {jwks}"))
        .clone();

    // Both signed tokens of one sign-in: each names its own key of the one published set,
    // so the document has to describe the key of whichever token a verifier holds.
    for member in ["AccessToken", "IdToken"] {
        let token = result[member]
            .as_str()
            .unwrap_or_else(|| panic!("case {member}: the sign-in issued one, got {result}"))
            .to_string();
        let kid = token_kid(&token);
        let key = keys
            .iter()
            .find(|key| key["kid"].as_str() == Some(kid.as_str()))
            .unwrap_or_else(|| {
                panic!("case {member}: the key set must publish the key {kid:?}, got {jwks}")
            });
        for (name, expected) in [
            ("kty", RSA_KEY_TYPE),
            ("use", SIGNATURE_USE),
            ("alg", RS256),
            ("kid", kid.as_str()),
        ] {
            let held = key[name].as_str();
            assert_eq!(
                held,
                Some(expected),
                "case {member}: the published key's {name} must be {expected:?}, got {held:?}"
            );
        }
        // RFC 7518 §6.3.1: an RSA public key publishes its modulus `n` and its exponent
        // `e`, which are the two values an RS256 verifier builds the key from.
        let modulus = integer_member(member, key, "n");
        assert!(
            modulus.len() >= MINIMUM_MODULUS_BYTES,
            "case {member}: the published modulus must be at least {MINIMUM_MODULUS_BYTES} \
             octets, got {}",
            modulus.len()
        );
        let exponent = integer_member(member, key, "e");
        assert_eq!(
            exponent,
            PUBLIC_EXPONENT.to_vec(),
            "case {member}: the published exponent must be 65537, got {exponent:?}"
        );
        // And the signature the pool computed verifies against exactly that document.
        let verified = service_common::jwt::verify(&token, &jwks).unwrap_or_else(|defect| {
            panic!("case {member}: the genuine token must verify against {jwks}: {defect}")
        });
        let issuer = verified.claim_str("iss").unwrap_or_default();
        assert!(
            issuer.ends_with(&format!("/{}", fixture.user_pool_id)),
            "case {member}: the verified token names its own pool as issuer, got {issuer:?}"
        );
    }
}

/// A JWK member read as the unsigned big-endian integer RFC 7518 §6.3.1 publishes.
fn integer_member(case: &str, key: &Value, member: &str) -> Vec<u8> {
    let encoded = key[member]
        .as_str()
        .unwrap_or_else(|| panic!("case {case}: the published key must carry {member}, got {key}"));
    let bytes = service_common::jwt::decode_base64url(encoded).unwrap_or_else(|| {
        panic!("case {case}: the key's {member} must be base64url, got {encoded:?}")
    });
    assert!(
        !bytes.is_empty() && bytes[0] != 0,
        "case {case}: the key's {member} is an unsigned big-endian integer with no leading \
         zero octet, got {bytes:?}"
    );
    bytes
}

#[tokio::test]
async fn a_token_that_does_not_verify_is_refused_as_a_signature_this_service_cannot_verify() {
    for operation in OPERATIONS {
        for row in ROWS {
            let case = format!("{operation}: {}", row.case);
            // One emulator per row, so a row that signs its user out or writes an
            // attribute cannot change what the next row sees.
            let router = router();
            let fixture = fixture(&router, "forgery-pool").await;
            let result = sign_in(&router, &fixture).await;
            let access_token = result["AccessToken"].as_str().expect("an access token");
            let id_token = result["IdToken"].as_str().expect("an id token");
            let presented = row.forgery.token(access_token, id_token);

            let (status, payload) =
                dispatch(&router, operation, &request(operation, &presented), false).await;
            match row.expected_message {
                None => assert_eq!(
                    status, 200,
                    "case {case:?}: the genuine token must be served, got {status} with {payload}"
                ),
                Some(message) => {
                    assert_eq!(
                        status, REFUSAL_STATUS,
                        "case {case:?}: expected {REFUSAL_STATUS}, got {status} with {payload}"
                    );
                    assert_eq!(
                        error_code(&payload),
                        NOT_AUTHORIZED,
                        "case {case:?}: expected {NOT_AUTHORIZED}, got {payload}"
                    );
                    assert_eq!(
                        error_message(&payload),
                        message,
                        "case {case:?}: expected the message {message:?}, got {payload}"
                    );
                }
            }
        }
    }
}

#[tokio::test]
async fn a_refused_forgery_changes_nothing_a_later_caller_sees() {
    let router = router();
    let fixture = fixture(&router, "unchanged-pool").await;
    let result = sign_in(&router, &fixture).await;
    let access_token = result["AccessToken"]
        .as_str()
        .expect("an access token")
        .to_string();
    let id_token = result["IdToken"].as_str().expect("an id token").to_string();
    let administrative = json!({
        "UserPoolId": fixture.user_pool_id,
        "Username": fixture.username,
    });
    let before = ok(&router, "AdminGetUser", &administrative).await;

    // Every forgery of the table, against the write and against the read, on one pool.
    for row in ROWS {
        if row.expected_message.is_none() {
            continue;
        }
        let presented = row.forgery.token(&access_token, &id_token);
        for operation in ["UpdateUserAttributes", "GetUser"] {
            let (status, payload) =
                dispatch(&router, operation, &request(operation, &presented), false).await;
            assert_eq!(
                status, REFUSAL_STATUS,
                "case {:?} on {operation}: expected the forgery refused, got {status} with \
                 {payload}",
                row.case
            );
        }
    }

    let after = ok(&router, "AdminGetUser", &administrative).await;
    assert_eq!(
        after, before,
        "a refused forgery writes nothing, so the stored user is unchanged, got {after}"
    );
    // And the token the forgeries were built from still authorizes its own holder.
    let (status, payload) = dispatch(
        &router,
        "GetUser",
        &request("GetUser", &access_token),
        false,
    )
    .await;
    assert_eq!(
        status, 200,
        "the genuine token must still be served after every forgery, got {status} with {payload}"
    );
    assert_eq!(
        payload["Username"].as_str(),
        Some(fixture.username.as_str()),
        "the genuine token names its own user, got {payload}"
    );
}
