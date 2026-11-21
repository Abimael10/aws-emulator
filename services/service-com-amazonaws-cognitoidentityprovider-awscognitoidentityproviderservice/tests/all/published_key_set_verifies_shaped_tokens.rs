// SPDX-License-Identifier: CC0-1.0

//! The key set a user pool publishes, read as a RELYING PARTY reads it: an anonymous GET of
//! `<iss>/.well-known/jwks.json`, followed by an RS256 verification built from the published
//! parameters alone.
//!
//! The rule pinned here: every entry of that document is a COMPLETE public key of its family
//! — an RSA key travels as its modulus `n` and its public exponent `e`, "the unsigned
//! big-endian representation" of each (RFC 7518 §6.3.1), beside the `kty`, `alg`, `kid` and
//! `use` of RFC 7517 §4 — so a verifier that holds nothing but the fetched document can
//! rebuild the key the token's `kid` names and check the token's signature. A document whose
//! entry is missing either parameter is not a key: a verifier fails while parsing it, and no
//! token the pool minted can be accepted.
//!
//! The dimension the rows span is what the minting puts INTO those signed bytes: a pool whose
//! `PreTokenGenerationConfig` names `LambdaVersion` `V2_0` has its function add claims of
//! several JSON forms, suppress a generated claim, add and suppress scopes and replace the
//! group configuration (`crate::pre_token_generation_overrides`, the suite that owns the
//! claim-shaping table, supplies the fixture and the real `python3.12` package that answers).
//! Whatever the shaping, both signed tokens of the generation still verify against the pool's
//! published key, and the claims read back are read out of the payload the signature COVERS
//! rather than out of an undecoded token.
//!
//! Two counter-rows say the check is not vacuous: a payload rewritten to carry a claim the
//! function did not answer with no longer verifies, and another pool's key set — a pool holds
//! its own keys — verifies nothing of this one.
//!
//! Nothing here asserts a value baked into the service: the pool id, the client id, the
//! function ARN, the group, the user and every key parameter come back from the state the
//! setup calls created.

use base64::Engine as _;
use bytes::Bytes;
use ring::signature::{RsaPublicKeyComponents, RSA_PKCS1_2048_8192_SHA256};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::pre_token_generation_overrides::{
    asking_for, assert_claims, grouped_user, ok, pool_with_client, router, trigger_function,
    Expect, GOOD_PASSWORD, GRANTED_SCOPE, STORED_GROUP,
};

/// The published regional endpoint host every call below is addressed to.
const PUBLISHED_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// The members of `AuthenticationResult` that carry a SIGNED token, with the name used for
/// each in an assertion message. The refresh token beside them is an opaque credential that
/// states no claim and is not a JWS.
const SIGNED_TOKENS: [(&str, &str); 2] = [("AccessToken", "access"), ("IdToken", "identity")];

/// The `kty` of the key family RS256 is defined over (RFC 7518 §6.3).
const RSA_KEY_TYPE: &str = "RSA";

/// The `alg` a pool's tokens and its published keys name: RSASSA-PKCS1-v1_5 using SHA-256
/// (RFC 7518 §3.3).
const SIGNING_ALGORITHM: &str = "RS256";

/// The `use` a verification key carries (RFC 7517 §4.2).
const SIGNATURE_USE: &str = "sig";

/// The smallest modulus RFC 7518 §3.3 admits for that algorithm: "A key of size 2048 bits or
/// larger MUST be used with this algorithm", which is 256 octets of unsigned big-endian `n`.
const MIN_MODULUS_OCTETS: usize = 256;

/// An ANONYMOUS GET of `path`: the status, the `content-type` and the body. A relying party
/// fetching a key set holds no credential of the account that owns the pool.
async fn anonymous_get(
    router: &MultiServiceRouter,
    path: &str,
) -> (u16, Option<String>, Vec<u8>) {
    let request = http::Request::builder()
        .method("GET")
        .uri(path)
        .header("host", PUBLISHED_HOST)
        .body(Bytes::new())
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let media_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    (
        response.status().as_u16(),
        media_type,
        response.body().to_vec(),
    )
}

/// The key set `user_pool_id` publishes, fetched at the well-known path under its issuer.
async fn served_key_set(router: &MultiServiceRouter, user_pool_id: &str) -> Value {
    let (status, media_type, body) =
        anonymous_get(router, &format!("/{user_pool_id}/.well-known/jwks.json")).await;
    assert_eq!(
        status,
        200,
        "case {user_pool_id:?}: the pool must serve its key set, got {status} with {:?}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        media_type.as_deref(),
        Some("application/json"),
        "case {user_pool_id:?}: a key set is JSON, got {media_type:?}"
    );
    serde_json::from_slice(&body).unwrap_or_else(|error| {
        panic!(
            "case {user_pool_id:?}: the key set must be JSON ({error}): {:?}",
            String::from_utf8_lossy(&body)
        )
    })
}

/// A compact-JWS segment's bytes: base64url with the padding RFC 7515 §2 strips.
fn decode_base64url(case: &str, what: &str, segment: &str) -> Vec<u8> {
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(segment)
        .unwrap_or_else(|error| {
            panic!("case {case:?}: the {what} must be base64url, got {segment:?} ({error})")
        })
}

/// A segment's bytes as the JSON it carries.
fn segment_json(case: &str, what: &str, segment: &str) -> Value {
    let bytes = decode_base64url(case, what, segment);
    serde_json::from_slice(&bytes).unwrap_or_else(|error| {
        panic!("case {case:?}: the {what} must carry JSON ({error}): {segment:?}")
    })
}

/// A segment holding `value`, the way a JWS carries one.
fn encode_segment(value: &Value) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(value.to_string().as_bytes())
}

/// Why a token was not accepted against a key set, for an assertion message.
#[derive(Debug, PartialEq, Eq)]
enum Refused {
    /// No entry of the set carries the `kid` the header names.
    UnknownKey(String),
    /// The signature does not check out under the entry that `kid` names.
    BadSignature,
}

/// Verify `token` the way a third party does, from the PUBLISHED parameters of `key_set`
/// alone, and answer with the claims the signature covers.
///
/// The steps are RFC 7515 §5.2's and RFC 7518 §3.3's: split the compact serialization, read
/// `alg` and `kid` out of the header, find that entry, rebuild the public key from its `n`
/// and `e` (RFC 7518 §6.3.1), and check RSASSA-PKCS1-v1_5 over SHA-256 against the two
/// encoded segments. No private material and nothing of the emulator's own state is used, so
/// a token that passes is one any relying party could have accepted.
fn verify_from_published_key(
    case: &str,
    kind: &str,
    token: &str,
    key_set: &Value,
) -> Result<Value, Refused> {
    let subject = format!("the {kind} token");
    let segments: Vec<&str> = token.split('.').collect();
    assert_eq!(
        segments.len(),
        3,
        "case {case:?}: {subject} must be a compact JWS of three segments, got {token:?}"
    );
    let header = segment_json(case, &format!("{subject}'s header"), segments[0]);
    assert_eq!(
        header["alg"], SIGNING_ALGORITHM,
        "case {case:?}: {subject}'s header must name {SIGNING_ALGORITHM}, got {header}"
    );
    let kid = header["kid"].as_str().unwrap_or_else(|| {
        panic!("case {case:?}: {subject}'s header must name a key, got {header}")
    });
    let keys = key_set["keys"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: a key set carries a keys list, got {key_set}"));
    let Some(key) = keys.iter().find(|key| key["kid"].as_str() == Some(kid)) else {
        return Err(Refused::UnknownKey(kid.to_string()));
    };
    assert_eq!(
        json!({ "kty": key["kty"], "alg": key["alg"], "use": key["use"] }),
        json!({ "kty": RSA_KEY_TYPE, "alg": SIGNING_ALGORITHM, "use": SIGNATURE_USE }),
        "case {case:?}: {subject}'s key must be published as an RSA {SIGNING_ALGORITHM} \
         verification key, got {key}"
    );
    // RFC 7518 §6.3.1: an RSA public key travels as those two parameters and is rebuilt from
    // them; an entry missing either one is not a key at all.
    let component = |member: &str| -> Vec<u8> {
        let stated = key[member].as_str().unwrap_or_else(|| {
            panic!("case {case:?}: {subject}'s published key must carry {member}, got {key}")
        });
        let octets = decode_base64url(case, &format!("published {member}"), stated);
        assert!(
            !octets.is_empty(),
            "case {case:?}: {subject}'s published {member} must carry the parameter's octets, \
             got none"
        );
        octets
    };
    let modulus = component("n");
    let exponent = component("e");
    assert!(
        modulus.len() >= MIN_MODULUS_OCTETS,
        "case {case:?}: {subject}'s published modulus must be at least {MIN_MODULUS_OCTETS} \
         octets, got {}",
        modulus.len()
    );
    let public = RsaPublicKeyComponents {
        n: modulus,
        e: exponent,
    };
    let signing_input = format!("{}.{}", segments[0], segments[1]);
    let signature = decode_base64url(case, &format!("{subject}'s signature"), segments[2]);
    public
        .verify(
            &RSA_PKCS1_2048_8192_SHA256,
            signing_input.as_bytes(),
            &signature,
        )
        .map_err(|_| Refused::BadSignature)?;
    Ok(segment_json(case, &format!("{subject}'s claims"), segments[1]))
}

/// [`verify_from_published_key`] for a token that MUST be accepted.
fn accepted_claims(case: &str, kind: &str, token: &str, key_set: &Value) -> Value {
    verify_from_published_key(case, kind, token, key_set).unwrap_or_else(|refused| {
        panic!(
            "case {case:?}: the {kind} token must verify against the key set the pool \
             publishes, got {refused:?} with {key_set}"
        )
    })
}

/// The access and identity token a generation answered with.
fn signed_tokens(case: &str, payload: &Value) -> Vec<(&'static str, String)> {
    SIGNED_TOKENS
        .iter()
        .map(|(member, kind)| {
            let token = payload["AuthenticationResult"][member]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the generation must answer with {member}, got {payload}")
                });
            (*kind, token.to_string())
        })
        .collect()
}

/// One row of the table: what the function answers with, and what the VERIFIED claims of each
/// token must then be.
struct Row {
    case: &'static str,
    /// The `response` half the pre-token-generation function answers with.
    answered: Value,
    /// What the verified access token carries.
    access_token: Vec<(&'static str, Expect)>,
    /// What the verified identity token carries.
    id_token: Vec<(&'static str, Expect)>,
}

#[tokio::test]
async fn a_pools_published_key_verifies_the_tokens_its_trigger_shaped() {
    let router = router();
    let function = trigger_function(&router, "pre-token-published-key").await;
    let (user_pool_id, client_id) = pool_with_client(
        &router,
        "published-key",
        json!({
            "PreTokenGenerationConfig": { "LambdaVersion": "V2_0", "LambdaArn": function },
        }),
    )
    .await;
    let username = "verified-signer";
    grouped_user(&router, &user_pool_id, username).await;
    let key_set = served_key_set(&router, &user_pool_id).await;

    // Rows differ along what the function's answer does to the signed claims: adds claims of
    // several JSON forms, suppresses a generated one, rewrites the scopes, replaces the group
    // configuration, or asks for nothing at all. Whatever it is, the signature over the bytes
    // that result must check out against the published key.
    let rows: Vec<Row> = vec![
        Row {
            case: "an answer adding claims of several JSON forms to the access token",
            answered: json!({
                "claimsAndScopeOverrideDetails": {
                    "accessTokenGeneration": {
                        "claimsToAddOrOverride": {
                            "tenant": "acme",
                            "seats": 42,
                            "entitlements": ["reports", "exports"],
                            "limits": { "daily": 10, "plan": "gold" },
                            "trial": false,
                        },
                    },
                },
            }),
            access_token: vec![
                ("tenant", Expect::Is(json!("acme"))),
                ("seats", Expect::Is(json!(42))),
                (
                    "entitlements",
                    Expect::Is(json!(["reports", "exports"])),
                ),
                (
                    "limits",
                    Expect::Is(json!({ "daily": 10, "plan": "gold" })),
                ),
                ("trial", Expect::Is(json!(false))),
                ("token_use", Expect::Is(json!("access"))),
            ],
            id_token: vec![
                ("tenant", Expect::Absent),
                ("token_use", Expect::Is(json!("id"))),
            ],
        },
        Row {
            case: "an answer suppressing a generated claim of the access token",
            answered: json!({
                "claimsAndScopeOverrideDetails": {
                    "accessTokenGeneration": {
                        "claimsToSuppress": ["cognito:groups"],
                    },
                },
            }),
            access_token: vec![("cognito:groups", Expect::Absent)],
            // A suppression names the token it was stated for, so the other token's
            // membership stands and must still be inside the signature.
            id_token: vec![("cognito:groups", Expect::Is(json!([STORED_GROUP])))],
        },
        Row {
            case: "an answer rewriting the access token's scopes",
            answered: json!({
                "claimsAndScopeOverrideDetails": {
                    "accessTokenGeneration": {
                        "scopesToAdd": ["store/read", "store/write"],
                        "scopesToSuppress": [GRANTED_SCOPE],
                    },
                },
            }),
            access_token: vec![("scope", Expect::Is(json!("store/read store/write")))],
            id_token: vec![("scope", Expect::Absent)],
        },
        Row {
            case: "an answer replacing the group configuration and adding an identity claim",
            answered: json!({
                "claimsAndScopeOverrideDetails": {
                    "idTokenGeneration": {
                        "claimsToAddOrOverride": { "tier": "gold" },
                    },
                    "groupOverrideDetails": { "groupsToOverride": ["override-group"] },
                },
            }),
            access_token: vec![("cognito:groups", Expect::Is(json!(["override-group"])))],
            id_token: vec![
                ("tier", Expect::Is(json!("gold"))),
                ("cognito:groups", Expect::Is(json!(["override-group"]))),
            ],
        },
        Row {
            case: "an answer that overrides nothing",
            answered: json!({}),
            access_token: vec![
                ("scope", Expect::Is(json!(GRANTED_SCOPE))),
                ("cognito:groups", Expect::Is(json!([STORED_GROUP]))),
            ],
            id_token: vec![("cognito:groups", Expect::Is(json!([STORED_GROUP])))],
        },
    ];

    for row in &rows {
        let signed_in = ok(
            &router,
            "InitiateAuth",
            &json!({
                "ClientId": client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
                "ClientMetadata": asking_for(&row.answered),
            }),
        )
        .await;
        for (kind, token) in signed_tokens(row.case, &signed_in) {
            let claims = accepted_claims(row.case, kind, &token, &key_set);
            let expected = if kind == "access" {
                &row.access_token
            } else {
                &row.id_token
            };
            assert_claims(row.case, kind, &claims, expected);
            // The document the token leads a verifier to is the one just fetched: its `iss`
            // is the issuer whose well-known path served this key set.
            let issuer = claims["iss"].as_str().unwrap_or_default();
            assert!(
                issuer.ends_with(&format!("/{user_pool_id}")),
                "case {:?}: the {kind} token's iss must name the pool whose key set verified \
                 it, got {issuer:?}",
                row.case
            );
        }
    }

    // The signature covers the claims, so a payload rewritten to carry a claim the function
    // did not answer with is no longer a token this key accepts: the verification above is a
    // real check of the published parameters and not a formality.
    let case = "a payload rewritten after signing";
    let signed_in = ok(
        &router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
            "ClientMetadata": asking_for(&json!({
                "claimsAndScopeOverrideDetails": {
                    "accessTokenGeneration": { "claimsToAddOrOverride": { "tier": "free" } },
                },
            })),
        }),
    )
    .await;
    let genuine = signed_tokens(case, &signed_in)
        .into_iter()
        .find(|(kind, _)| *kind == "access")
        .map(|(_, token)| token)
        .expect("the generation answers with an access token");
    let segments: Vec<&str> = genuine.split('.').collect();
    let mut claims = segment_json(case, "the access token's claims", segments[1]);
    claims["tier"] = json!("enterprise");
    let tampered = format!(
        "{}.{}.{}",
        segments[0],
        encode_segment(&claims),
        segments[2]
    );
    assert_eq!(
        verify_from_published_key(case, "access", &tampered, &key_set),
        Err(Refused::BadSignature),
        "case {case:?}: a rewritten payload must not verify against the published key"
    );
}

#[tokio::test]
async fn a_published_key_set_verifies_no_other_pools_token() {
    let router = router();
    // Two pools, each holding its own keys: the key set of one must accept nothing the other
    // signed, which is what makes the `kid` in a header meaningful.
    let mut pools = Vec::new();
    for (index, name) in ["key-owner-one", "key-owner-two"].into_iter().enumerate() {
        let (user_pool_id, client_id) = pool_with_client(&router, name, json!({})).await;
        let username = format!("signer{index}");
        grouped_user(&router, &user_pool_id, &username).await;
        let signed_in = ok(
            &router,
            "InitiateAuth",
            &json!({
                "ClientId": client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
            }),
        )
        .await;
        let key_set = served_key_set(&router, &user_pool_id).await;
        pools.push((name, user_pool_id, key_set, signed_tokens(name, &signed_in)));
    }

    let mut published_kids: Vec<String> = Vec::new();
    for (name, user_pool_id, key_set, tokens) in &pools {
        let keys = key_set["keys"]
            .as_array()
            .unwrap_or_else(|| panic!("case {name:?}: a key set carries a keys list, got {key_set}"));
        assert_eq!(
            keys.len(),
            SIGNED_TOKENS.len(),
            "case {name:?}: the pool publishes the key of each token type it signs, got {key_set}"
        );
        for key in keys {
            // Only the public half is published: a private RSA parameter in the document
            // would be the issuer's key leaving the service.
            for private in ["d", "p", "q", "dp", "dq", "qi"] {
                assert!(
                    key.get(private).is_none(),
                    "case {name:?}: a published key carries no {private}, got {key}"
                );
            }
            let kid = key["kid"].as_str().unwrap_or_else(|| {
                panic!("case {name:?}: a published key is named by a kid, got {key}")
            });
            published_kids.push(kid.to_string());
        }

        // Its own tokens verify, and each names a DIFFERENT key of the pool's set.
        let mut named: Vec<String> = Vec::new();
        for (kind, token) in tokens {
            let claims = accepted_claims(name, kind, token, key_set);
            assert!(
                claims["iss"]
                    .as_str()
                    .is_some_and(|issuer| issuer.ends_with(&format!("/{user_pool_id}"))),
                "case {name:?}: the {kind} token's iss must name its own pool, got {claims}"
            );
            let header = segment_json(name, "header", token.split('.').next().unwrap_or_default());
            named.push(header["kid"].as_str().unwrap_or_default().to_string());
        }
        assert_ne!(
            named[0], named[1],
            "case {name:?}: the two token types name different keys of the set, got {named:?}"
        );

        // And the OTHER pool's document accepts none of them: it publishes no such key.
        for (other_name, _, other_set, _) in &pools {
            if other_name == name {
                continue;
            }
            for (kind, token) in tokens {
                let outcome = verify_from_published_key(name, kind, token, other_set);
                assert!(
                    matches!(outcome, Err(Refused::UnknownKey(_))),
                    "case {name:?}: the {kind} token must not be verifiable against \
                     {other_name:?}'s key set, got {outcome:?}"
                );
            }
        }
    }

    let mut unique = published_kids.clone();
    unique.sort();
    unique.dedup();
    assert_eq!(
        unique.len(),
        published_kids.len(),
        "each pool's keys are its own, so no two published entries share a kid, got \
         {published_kids:?}"
    );
}
