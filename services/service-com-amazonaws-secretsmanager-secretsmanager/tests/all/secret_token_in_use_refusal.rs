// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for WHICH thing a version-creating write cannot have when it
//! is refused `ResourceExistsException`, driven over the multi-service router.
//!
//! The rule under test: `ClientRequestToken` is the id of the version a write
//! creates and is an `@idempotencyToken` (`CreateSecretRequest$ClientRequestToken`,
//! and the same member on `PutSecretValueRequest` and `UpdateSecretRequest`, this
//! service's Smithy model), so when a write names a token that ALREADY identifies a
//! version of the secret it is addressed to and carries a different value, the
//! refusal is about the TOKEN — the token from the request and the ARN of the secret
//! whose version holds it — whichever of the version-creating operations made the
//! call. The plain name-collision sentence belongs to a create whose `Name` is taken
//! by a secret none of whose versions the token names, and the same token with the
//! same value is the same request and is not refused at all.
//!
//! Every test is a TABLE over the class the rule generalises: the operation that
//! made the write, the member the stored and the colliding value travel in, and the
//! length of the token at both published bounds of `ClientRequestTokenType`
//! (`@length` 32..64) and inside them.

use serde_json::{json, Map, Value};

use crate::secret_client_request_token::{
    call, emulator, fresh_token, ok, refusal, staging_map, text, token_of_length, value_member,
    TOKEN_MAX_LEN, TOKEN_MIN_LEN,
};

/// The operations that create a secret version from a value the request carries,
/// and so stake it under the `ClientRequestToken` the request names.
const VERSION_CREATING_WRITES: &[&str] = &["CreateSecret", "PutSecretValue", "UpdateSecret"];

/// The members a stored and a colliding value travel in: "You must include
/// SecretBinary or SecretString, but not both."
/// (`PutSecretValueRequest$SecretString`, this service's Smithy model.) The rows
/// pair them both ways, so a collision between two values of the same member and
/// one across members are both covered.
const VALUE_MEMBER_PAIRS: &[(&str, &str)] = &[
    ("SecretString", "SecretString"),
    ("SecretString", "SecretBinary"),
    ("SecretBinary", "SecretBinary"),
    ("SecretBinary", "SecretString"),
];

/// Token lengths spanning `ClientRequestTokenType`: both published bounds and one
/// inside them.
const TOKEN_LENGTHS: &[usize] = &[TOKEN_MIN_LEN, 36, TOKEN_MAX_LEN];

/// The sentence a write that reuses a token already naming a version is refused
/// with, built from the request's own token and the secret's ARN.
fn token_in_use(token: &str, arn: &str) -> String {
    format!(
        "You can't use ClientRequestToken {token} because that value is already in use for a \
         version of secret {arn}."
    )
}

/// The sentence a create whose `Name` is taken is refused with.
fn name_already_exists(name: &str) -> String {
    format!("The operation failed because the secret {name} already exists.")
}

/// The body that writes `value` under `member` to the secret `name` with the token
/// `token`, as `operation` addresses it.
fn write_body(operation: &str, name: &str, member: &str, value: &str, token: &str) -> Value {
    let mut body = Map::new();
    if operation == "CreateSecret" {
        body.insert("Name".to_string(), json!(name));
    } else {
        body.insert("SecretId".to_string(), json!(name));
    }
    body.insert(member.to_string(), value_member(member, value));
    body.insert("ClientRequestToken".to_string(), json!(token));
    Value::Object(body)
}

#[tokio::test]
async fn a_reused_token_with_another_value_is_refused_for_the_token() {
    for operation in VERSION_CREATING_WRITES {
        for (stored_member, colliding_member) in VALUE_MEMBER_PAIRS {
            for length in TOKEN_LENGTHS {
                let case = format!(
                    "{operation} replaying a {length}-character token, {stored_member} stored and \
                     {colliding_member} colliding"
                );
                let router = emulator();
                let name = format!("token-in-use/{operation}-{stored_member}-{length}");
                let token = token_of_length(*length);
                // The secret holds one version, staked under this very token.
                let created = ok(
                    &router,
                    "CreateSecret",
                    &write_body(
                        "CreateSecret",
                        &name,
                        stored_member,
                        "the-stored-value",
                        &token,
                    ),
                    &case,
                )
                .await;
                let arn = text(&created, "ARN", &case);
                assert_eq!(
                    text(&created, "VersionId", &case),
                    token,
                    "case {case:?}: expected the token to become the version id, got {created}"
                );

                // The same token with ANOTHER value: refused for the token.
                let (status, parsed) = call(
                    &router,
                    operation,
                    &write_body(
                        operation,
                        &name,
                        colliding_member,
                        "the-colliding-value",
                        &token,
                    ),
                )
                .await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected 400, got {status} with body {parsed}"
                );
                let (code, message) = refusal(&parsed);
                assert_eq!(
                    code, "ResourceExistsException",
                    "case {case:?}: expected ResourceExistsException, got {code:?} with message \
                     {message:?}"
                );
                let expected = token_in_use(&token, &arn);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );

                // The refused write stored nothing: the secret still holds only the
                // version the token already named, still current and still holding
                // the value it was staked with.
                let staged = staging_map(&router, &name, &case).await;
                assert_eq!(
                    staged,
                    Map::from_iter([(token.clone(), json!(["AWSCURRENT"]))]),
                    "case {case:?}: expected only the staked version, still current, got \
                     {staged:?}"
                );
                let read = ok(&router, "GetSecretValue", &json!({"SecretId": name}), &case).await;
                assert_eq!(
                    read.get(stored_member),
                    Some(&value_member(stored_member, "the-stored-value")),
                    "case {case:?}: expected the stored value untouched, got {read}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_reused_token_with_the_same_value_is_the_same_request() {
    // The boundary of the rule: the token is an `@idempotencyToken`, so the same
    // token carrying the same value is the same request — answered from what is
    // stored rather than refused for the token or for the name. The rows run over
    // the operation that replays it, the member the value travels in, and the
    // token's length.
    for operation in VERSION_CREATING_WRITES {
        for (member, _) in VALUE_MEMBER_PAIRS {
            for length in TOKEN_LENGTHS {
                let case =
                    format!("{operation} replaying {member} under a {length}-character token");
                let router = emulator();
                let name = format!("token-replay/{operation}-{member}-{length}");
                let token = token_of_length(*length);
                let created = ok(
                    &router,
                    "CreateSecret",
                    &write_body("CreateSecret", &name, member, "the-stored-value", &token),
                    &case,
                )
                .await;

                let replayed = ok(
                    &router,
                    operation,
                    &write_body(operation, &name, member, "the-stored-value", &token),
                    &case,
                )
                .await;
                assert_eq!(
                    text(&replayed, "VersionId", &case),
                    token,
                    "case {case:?}: expected the stored version back, got {replayed}"
                );
                assert_eq!(
                    text(&replayed, "ARN", &case),
                    text(&created, "ARN", &case),
                    "case {case:?}: expected the stored ARN back, got {replayed}"
                );

                let staged = staging_map(&router, &name, &case).await;
                assert_eq!(
                    staged,
                    Map::from_iter([(token.clone(), json!(["AWSCURRENT"]))]),
                    "case {case:?}: expected the replay to add no version, got {staged:?}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_create_whose_token_names_no_version_is_refused_for_the_name() {
    // The other half of the rule: when the token names no version of the secret
    // already holding the name, what the create cannot have is the NAME, and the
    // refusal is the name-collision sentence. The rows run over the shape of the
    // create — a fresh token with a value, a fresh token with no value at all
    // (which creates no version, so its token names none: "If you include
    // SecretString or SecretBinary then Secrets Manager creates an initial secret
    // version", `CreateSecret`, this service's Smithy model), and a create carrying
    // no value whose token is the one the stored version already holds — and over
    // the member and the token length.
    let rows: &[(&str, &str, bool, bool)] = &[
        // (label, name segment, carries a value, replays the stored version's token)
        ("a fresh token and a value", "fresh-with-value", true, false),
        ("a fresh token and no value", "fresh-valueless", false, false),
        (
            "the stored token and no value",
            "stored-token-valueless",
            false,
            true,
        ),
    ];
    for (label, segment, carries_value, replays_token) in rows {
        for (stored_member, colliding_member) in VALUE_MEMBER_PAIRS {
            for length in TOKEN_LENGTHS {
                let case = format!(
                    "a create with {label}, {stored_member} stored, a {length}-character token"
                );
                let router = emulator();
                let name = format!("name-taken/{segment}-{stored_member}-{length}");
                let stored_token = token_of_length(*length);
                let created = ok(
                    &router,
                    "CreateSecret",
                    &write_body(
                        "CreateSecret",
                        &name,
                        stored_member,
                        "the-stored-value",
                        &stored_token,
                    ),
                    &case,
                )
                .await;

                let token = if *replays_token {
                    stored_token.clone()
                } else {
                    fresh_token()
                };
                let body = if *carries_value {
                    write_body(
                        "CreateSecret",
                        &name,
                        colliding_member,
                        "another-value",
                        &token,
                    )
                } else {
                    json!({"Name": name.clone(), "ClientRequestToken": token.clone()})
                };
                let (status, parsed) = call(&router, "CreateSecret", &body).await;
                assert_eq!(
                    status, 400,
                    "case {case:?}: expected 400, got {status} with body {parsed}"
                );
                let (code, message) = refusal(&parsed);
                assert_eq!(
                    code, "ResourceExistsException",
                    "case {case:?}: expected ResourceExistsException, got {code:?} with message \
                     {message:?}"
                );
                let expected = name_already_exists(&name);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );

                // The secret the name belongs to is untouched by the refused create.
                let staged = staging_map(&router, &name, &case).await;
                assert_eq!(
                    staged,
                    Map::from_iter([(stored_token.clone(), json!(["AWSCURRENT"]))]),
                    "case {case:?}: expected only the stored version, got {staged:?}"
                );
                let read = ok(&router, "GetSecretValue", &json!({"SecretId": name}), &case).await;
                assert_eq!(
                    text(&read, "ARN", &case),
                    text(&created, "ARN", &case),
                    "case {case:?}: expected the stored secret untouched, got {read}"
                );
            }
        }
    }
}
