// SPDX-License-Identifier: CC0-1.0

//! The ENVELOPE of the secure-remote-password continuation, driven exactly as an
//! SDK drives it (`POST /` with `X-Amz-Target:
//! AWSCognitoIdentityProviderService.InitiateAuth` / `.AdminInitiateAuth` /
//! `.RespondToAuthChallenge` / `.AdminRespondToAuthChallenge`).
//!
//! The rule under test: **the `PASSWORD_VERIFIER` challenge of the SRP sign-in
//! carries its continuation in its own `ChallengeParameters` rather than in a
//! session token — `Session` is an optional `SessionType` member of both sign-in
//! outputs and of both challenge-response inputs
//! (`smithy_query(query="operation",operation="InitiateAuth,RespondToAuthChallenge")`)
//! — so the sign-in answers with NO `Session`, the response to it presents none,
//! and the exchange a claim is checked against is the one whose `SECRET_BLOCK`
//! that response echoes. Those parameters are spelled as a client library of the
//! handshake reads them: `SRP_B` as the hexadecimal of the server's public value
//! at the group prime's own width, and `SECRET_BLOCK` as the long base64 block
//! the claim is signed over. A response that leaves out an entry the challenge
//! requires — "`PASSWORD_VERIFIER`: Respond with `PASSWORD_CLAIM_SIGNATURE`,
//! `PASSWORD_CLAIM_SECRET_BLOCK`, and `TIMESTAMP`" (`ChallengeResponses` in this
//! service's model) — is refused `InvalidParameterException` naming THAT entry,
//! and one whose claim proves no password is refused `NotAuthorizedException`
//! with the fixed sentence a wrong credential earns anywhere in this API.**
//!
//! The rows span what the rule generalises over: the pair of operations that
//! drives the exchange (the client-side pair and the administrative pair), the
//! user the challenge is served for, the app client's configured challenge
//! window, which required entry a response leaves out, and the way a claim can
//! fail to prove the password (another password, a block nothing served, another
//! exchange's block, a response naming another user, a continuation answered
//! through another app client).
//!
//! The client half of the exchange is computed in [`super::user_srp_sign_in`],
//! which this suite drives the handshake with; everything else is real — the
//! pools, app clients, users and passwords all come from this service's own
//! operations.

use base64::Engine as _;
use serde_json::{json, Map, Value};

use super::device_authentication::prime;
use super::software_token_setup::{
    app_client, dispatch, emulator, error_code, error_message, ok, pool, Emulator, GOOD_PASSWORD,
};
use super::user_srp_sign_in::{
    claim, claim_signature, fixture, pool_name_of, serve, ClientExchange, Driver, Fixture, Served,
    CLAIM_TIMESTAMP, INCORRECT_CREDENTIALS_MESSAGE, INVALID_PARAMETER, NOT_AUTHORIZED,
    PASSWORD_VERIFIER, REFUSED, SALT, SECRET_BLOCK, SRP_B, USERNAME, USER_ID_FOR_SRP,
    USER_SRP_AUTH,
};

/// The entries the `PASSWORD_VERIFIER` challenge requires back
/// (`ChallengeResponses` in this service's model).
const PASSWORD_CLAIM_SIGNATURE: &str = "PASSWORD_CLAIM_SIGNATURE";
const PASSWORD_CLAIM_SECRET_BLOCK: &str = "PASSWORD_CLAIM_SECRET_BLOCK";
const TIMESTAMP: &str = "TIMESTAMP";

/// The entry every challenge response names the user it answers for in, and the
/// challenge parameter the challenge names that user in.
const USERNAME_ENTRY: &str = "USERNAME";

/// The shortest base64 a `SECRET_BLOCK` can be and still be the block Amazon
/// Cognito serves: its continuation is an opaque blob of the exchange, which is
/// why a client never parses it and only echoes it back.
const SHORTEST_SECRET_BLOCK: usize = 1_000;

/// How many hexadecimal digits a public value of the exchange's group is spelled
/// in: two per byte of the group's own prime, read from the group this service's
/// SRP runs over ([`prime`]).
fn public_value_digits() -> usize {
    prime().to_bytes_be().len() * 2
}

/// One row of the envelope table: the case, the pair of operations that drives
/// it, the user the challenge is served for and what the app client holds.
struct EnvelopeRow {
    case: &'static str,
    driver: Driver,
    username: &'static str,
    password: &'static str,
    /// Merged into the app client, so a row can vary the window a challenge of
    /// this client's sign-ins stands for (`AuthSessionValidity`).
    client_extra: Value,
}

/// A pool with the user `username` holding `password` permanently and one app
/// client that enables the SRP flow with `client_extra` merged in.
async fn envelope_fixture(
    emulator: &Emulator,
    name: &str,
    username: &str,
    password: &str,
    client_extra: Value,
) -> Fixture {
    let user_pool_id = pool(emulator, name, json!({})).await;
    let mut extra = Map::new();
    extra.insert(
        "ExplicitAuthFlows".to_string(),
        json!(["ALLOW_USER_SRP_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"]),
    );
    if let Some(members) = client_extra.as_object() {
        for (member, value) in members {
            extra.insert(member.clone(), value.clone());
        }
    }
    let client_id = app_client(emulator, &user_pool_id, "srp-client", Value::Object(extra)).await;
    ok(
        emulator,
        "AdminCreateUser",
        &json!({ "UserPoolId": user_pool_id, "Username": username }),
    )
    .await;
    ok(
        emulator,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "Password": password,
            "Permanent": true,
        }),
    )
    .await;
    Fixture {
        pool_name: pool_name_of(&user_pool_id),
        user_pool_id,
        client_id,
    }
}

/// Answer the `PASSWORD_VERIFIER` challenge with `responses` and no `Session`,
/// through `driver`.
async fn respond(
    emulator: &Emulator,
    driver: Driver,
    fixture: &Fixture,
    responses: Value,
) -> (u16, Value) {
    dispatch(
        emulator,
        driver.respond(),
        &driver.respond_body(fixture, PASSWORD_VERIFIER, None, responses),
        driver.signed(),
    )
    .await
}

/// The complete response to `served`, with the claim `password` produces.
fn complete_responses(
    fixture: &Fixture,
    exchange: &ClientExchange,
    served: &Served,
    password: &str,
) -> Map<String, Value> {
    let session_key = exchange.session_key(
        &fixture.pool_name,
        &served.user_id_for_srp,
        password,
        &served.server_public,
        &served.salt,
    );
    let signature = claim_signature(
        &session_key,
        &fixture.pool_name,
        &served.user_id_for_srp,
        &served.secret_block,
        CLAIM_TIMESTAMP,
    );
    let mut responses = Map::new();
    responses.insert(USERNAME_ENTRY.to_string(), json!(served.user_id_for_srp));
    responses.insert(
        PASSWORD_CLAIM_SECRET_BLOCK.to_string(),
        json!(served.secret_block),
    );
    responses.insert(PASSWORD_CLAIM_SIGNATURE.to_string(), json!(signature));
    responses.insert(TIMESTAMP.to_string(), json!(CLAIM_TIMESTAMP));
    responses
}

/// The sign-in answers the SRP challenge with its continuation in the challenge
/// parameters and NO session, and the response that echoes that continuation —
/// carrying no session either — completes the sign-in.
#[tokio::test]
async fn the_srp_challenge_carries_its_continuation_in_its_parameters() {
    let rows = [
        EnvelopeRow {
            case: "the client-side pair on a client holding the default window",
            driver: Driver::Client,
            username: "ada",
            password: GOOD_PASSWORD,
            client_extra: json!({}),
        },
        EnvelopeRow {
            case: "the administrative pair on a client holding the default window",
            driver: Driver::Admin,
            username: "ada",
            password: GOOD_PASSWORD,
            client_extra: json!({}),
        },
        EnvelopeRow {
            case: "a user named by an address, on a client with a longer window",
            driver: Driver::Client,
            username: "grace.hopper@example.com",
            password: "An0therP4ssword!",
            client_extra: json!({ "AuthSessionValidity": 15 }),
        },
        EnvelopeRow {
            case: "a user named in mixed case, through the administrative pair",
            driver: Driver::Admin,
            username: "Ada.Lovelace",
            password: "Y3tAn0ther!Pass",
            client_extra: json!({ "AuthSessionValidity": 5 }),
        },
    ];

    for (index, row) in rows.iter().enumerate() {
        let case = row.case;
        let emulator = emulator();
        let fixture = envelope_fixture(
            &emulator,
            &format!("srp-envelope-pool-{index}"),
            row.username,
            row.password,
            row.client_extra.clone(),
        )
        .await;

        // `serve` itself asserts that the challenge carries no `Session`.
        let exchange = ClientExchange::new(case);
        let (status, payload) = dispatch(
            &emulator,
            row.driver.initiate(),
            &row.driver.sign_in_body(
                &fixture,
                json!({ "USERNAME": row.username, "SRP_A": exchange.public_hex() }),
            ),
            row.driver.signed(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the SRP sign-in must serve its challenge, got {status} with {payload}"
        );
        assert_eq!(
            payload["ChallengeName"].as_str(),
            Some(PASSWORD_VERIFIER),
            "case {case:?}: the SRP flow owes {PASSWORD_VERIFIER}, got {payload}"
        );
        assert!(
            payload.get("Session").is_none_or(Value::is_null),
            "case {case:?}: this challenge's continuation is its parameters, so it carries no \
             Session, got {payload}"
        );
        let parameters = &payload["ChallengeParameters"];
        let parameter = |entry: &str| {
            parameters[entry]
                .as_str()
                .unwrap_or_else(|| {
                    panic!("case {case:?}: the challenge must serve {entry}, got {payload}")
                })
                .to_string()
        };
        let server_public = parameter(SRP_B);
        assert_eq!(
            (
                server_public.len(),
                server_public
                    .chars()
                    .all(|digit| matches!(digit, '0'..='9' | 'a'..='f'))
            ),
            (public_value_digits(), true),
            "case {case:?}: SRP_B is the group-width hexadecimal of the public value, got \
             {server_public:?}"
        );
        let secret_block = parameter(SECRET_BLOCK);
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&secret_block)
            .unwrap_or_else(|error| {
                panic!("case {case:?}: SECRET_BLOCK is base64 ({error}), got {secret_block:?}")
            });
        assert!(
            secret_block.len() >= SHORTEST_SECRET_BLOCK && !decoded.is_empty(),
            "case {case:?}: SECRET_BLOCK is the long base64 continuation block, got {} characters",
            secret_block.len()
        );
        let salt = parameter(SALT);
        assert!(
            hex::decode(&salt).is_ok(),
            "case {case:?}: SALT is served as hexadecimal, got {salt:?}"
        );
        assert_eq!(
            (
                parameter(USER_ID_FOR_SRP).as_str(),
                parameter(USERNAME_ENTRY).as_str()
            ),
            (row.username, row.username),
            "case {case:?}: the challenge names the user it is outstanding for, got {parameters}"
        );

        // The continuation is answerable with no session at all: the block the
        // response echoes is what names the exchange its claim is checked
        // against.
        let served = Served {
            user_id_for_srp: parameter(USER_ID_FOR_SRP),
            salt,
            server_public,
            secret_block,
        };
        let responses = complete_responses(&fixture, &exchange, &served, row.password);
        let (status, payload) =
            respond(&emulator, row.driver, &fixture, Value::Object(responses)).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the claim of the stored password completes the sign-in, got {status} \
             with {payload}"
        );
        for member in ["AccessToken", "IdToken", "RefreshToken"] {
            assert!(
                payload["AuthenticationResult"][member]
                    .as_str()
                    .is_some_and(|token| !token.is_empty()),
                "case {case:?}: the completed sign-in returns {member}, got {payload}"
            );
        }
        assert!(
            payload.get("ChallengeName").is_none_or(Value::is_null),
            "case {case:?}: nothing is outstanding once the sign-in completed, got {payload}"
        );

        // The tokens are the user's: reading the sign-in back through the access
        // token names the user the challenge was served for.
        let access_token = payload["AuthenticationResult"]["AccessToken"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let (status, read) = dispatch(
            &emulator,
            "GetUser",
            &json!({ "AccessToken": access_token }),
            false,
        )
        .await;
        assert_eq!(
            (status, read["Username"].as_str()),
            (200, Some(row.username)),
            "case {case:?}: the completed sign-in's token names the signed-in user, got {status} \
             with {read}"
        );
    }
}

/// A response that leaves out one of the entries the challenge requires is
/// refused for THAT entry, through either pair of operations and with no
/// `Session` of its own to be asked for instead.
#[tokio::test]
async fn an_incomplete_response_names_the_entry_the_challenge_requires() {
    for driver in [Driver::Client, Driver::Admin] {
        for (index, left_out) in [
            PASSWORD_CLAIM_SIGNATURE,
            PASSWORD_CLAIM_SECRET_BLOCK,
            TIMESTAMP,
        ]
        .into_iter()
        .enumerate()
        {
            let case = format!("a response carrying no {left_out} through {}", driver.respond());
            let emulator = emulator();
            let fixture = fixture(
                &emulator,
                &format!("srp-incomplete-pool-{}-{index}", driver.initiate()),
                json!({}),
                GOOD_PASSWORD,
            )
            .await;
            let exchange = ClientExchange::new(&case);
            let served = serve(&emulator, driver, &fixture, &exchange, &case).await;

            let mut responses = complete_responses(&fixture, &exchange, &served, GOOD_PASSWORD);
            responses.remove(left_out);
            let (status, payload) =
                respond(&emulator, driver, &fixture, Value::Object(responses)).await;
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (
                    REFUSED,
                    INVALID_PARAMETER,
                    format!("Missing required parameter {left_out}").as_str()
                ),
                "case {case:?}: the absent entry must be named, got {status} with {payload}"
            );
        }
    }
}

/// What a claim can fail to prove the password by: each one is the SAME refusal,
/// the fixed sentence a wrong credential earns, so a failed sign-in is never
/// reported as a malformed request.
#[tokio::test]
async fn a_claim_that_proves_no_password_is_refused_as_a_failed_sign_in() {
    /// How a row spoils the complete response.
    type Spoil = fn(&mut Map<String, Value>);

    // Each row is (case, what it spoils in the complete response).
    let rows: &[(&str, Spoil)] = &[
        ("a signature computed from another password", |responses| {
            responses.insert(
                PASSWORD_CLAIM_SIGNATURE.to_string(),
                json!("c2lnbmF0dXJlLW9mLWFub3RoZXItcGFzc3dvcmQ="),
            );
        }),
        ("a signature that is not base64 at all", |responses| {
            responses.insert(PASSWORD_CLAIM_SIGNATURE.to_string(), json!("not-a-claim"));
        }),
        ("a secret block this service never served", |responses| {
            responses.insert(
                PASSWORD_CLAIM_SECRET_BLOCK.to_string(),
                json!("c2VjcmV0LWJsb2NrLW5vdGhpbmctc2VydmVk"),
            );
        }),
        ("a claim signed at another instant", |responses| {
            responses.insert(TIMESTAMP.to_string(), json!("Tue Jan 2 00:00:00 UTC 2024"));
        }),
        ("a response naming another user", |responses| {
            responses.insert(USERNAME_ENTRY.to_string(), json!("someone-else"));
        }),
    ];

    for (index, (case, spoil)) in rows.iter().enumerate() {
        let emulator = emulator();
        let fixture = fixture(
            &emulator,
            &format!("srp-refused-claim-pool-{index}"),
            json!({}),
            GOOD_PASSWORD,
        )
        .await;
        let exchange = ClientExchange::new(case);
        let served = serve(&emulator, Driver::Client, &fixture, &exchange, case).await;

        let mut responses = complete_responses(&fixture, &exchange, &served, GOOD_PASSWORD);
        spoil(&mut responses);
        let (status, payload) =
            respond(&emulator, Driver::Client, &fixture, Value::Object(responses)).await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (REFUSED, NOT_AUTHORIZED, INCORRECT_CREDENTIALS_MESSAGE),
            "case {case:?}: a claim that proves no password is refused as a failed sign-in, got \
             {status} with {payload}"
        );
        assert!(
            payload.get("AuthenticationResult").is_none(),
            "case {case:?}: a refused claim issues no tokens, got {payload}"
        );

        // The refusal answered nothing, so the challenge still takes its own
        // claim: the continuation is spent by the response that ANSWERS it.
        let (status, payload) = claim(
            &emulator,
            Driver::Client,
            &fixture,
            &exchange,
            &served,
            GOOD_PASSWORD,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the outstanding challenge still takes its own claim, got {status} \
             with {payload}"
        );
    }
}

/// A continuation belongs to the sign-in of ITS OWN app client: the same claim
/// presented through another app client of the same pool answers nothing.
#[tokio::test]
async fn a_continuation_answers_only_the_app_client_that_served_it() {
    let case = "a claim presented through another app client of the pool";
    let emulator = emulator();
    let fixture = fixture(&emulator, "srp-other-client-pool", json!({}), GOOD_PASSWORD).await;
    let exchange = ClientExchange::new(case);
    let served = serve(&emulator, Driver::Client, &fixture, &exchange, case).await;

    let other_client = app_client(
        &emulator,
        &fixture.user_pool_id,
        "other-srp-client",
        json!({ "ExplicitAuthFlows": ["ALLOW_USER_SRP_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"] }),
    )
    .await;
    let other = Fixture {
        user_pool_id: fixture.user_pool_id.clone(),
        client_id: other_client,
        pool_name: fixture.pool_name.clone(),
    };
    let responses = complete_responses(&fixture, &exchange, &served, GOOD_PASSWORD);
    let (status, payload) = respond(&emulator, Driver::Client, &other, Value::Object(responses)).await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (REFUSED, NOT_AUTHORIZED, INCORRECT_CREDENTIALS_MESSAGE),
        "case {case:?}: a continuation of another app client's sign-in answers nothing, got \
         {status} with {payload}"
    );

    // Through the app client that served it, the same claim completes the
    // sign-in.
    let (status, payload) = claim(
        &emulator,
        Driver::Client,
        &fixture,
        &exchange,
        &served,
        GOOD_PASSWORD,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the app client that served the challenge takes its claim, got {status} \
         with {payload}"
    );
}

/// The two calls of the handshake are ONE sign-in attempt, so the pool's threat
/// protection logs ONE event for it: the continuation the challenge was left
/// outstanding on is what the response that answers it is matched to, exactly as
/// a session token is for the challenges that carry one.
#[tokio::test]
async fn the_handshake_is_logged_as_one_sign_in_attempt() {
    let case = "an SRP handshake on a pool running threat protection";
    let emulator = emulator();
    let fixture = fixture(
        &emulator,
        "srp-auth-event-pool",
        json!({
            "UserPoolTier": "PLUS",
            "UserPoolAddOns": { "AdvancedSecurityMode": "ENFORCED" },
        }),
        GOOD_PASSWORD,
    )
    .await;

    let exchange = ClientExchange::new(case);
    let served = serve(&emulator, Driver::Client, &fixture, &exchange, case).await;
    let (status, payload) = claim(
        &emulator,
        Driver::Client,
        &fixture,
        &exchange,
        &served,
        GOOD_PASSWORD,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the handshake must complete the sign-in, got {status} with {payload}"
    );

    let history = ok(
        &emulator,
        "AdminListUserAuthEvents",
        &json!({ "UserPoolId": fixture.user_pool_id, "Username": USERNAME }),
    )
    .await;
    let events = history["AuthEvents"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: the listing carries AuthEvents, got {history}"));
    let logged: Vec<(&str, &str)> = events
        .iter()
        .map(|event| {
            (
                event["EventType"].as_str().unwrap_or_default(),
                event["EventResponse"].as_str().unwrap_or_default(),
            )
        })
        .collect();
    assert_eq!(
        logged,
        vec![("SignIn", "Pass")],
        "case {case:?}: the two calls of one handshake are one logged attempt, got {history}"
    );
}

/// One row of the refusal-wording table: the sign-in operation, the flow asked
/// for, and the sentence that API owes for that flow.
struct WordingRow {
    case: &'static str,
    operation: &'static str,
    auth_flow: &'static str,
    parameters: fn() -> Value,
    message: &'static str,
}

/// A flow the app client's stored `ExplicitAuthFlows` do not enable is refused
/// with the sentence the ASKING API words that refusal with for THAT flow.
#[tokio::test]
async fn a_flow_the_client_does_not_enable_is_refused_in_its_own_wording() {
    let rows = [
        WordingRow {
            case: "the SRP flow through the client-side sign-in",
            operation: "InitiateAuth",
            auth_flow: USER_SRP_AUTH,
            parameters: || json!({ "USERNAME": USERNAME, "SRP_A": "0f" }),
            message: "USER_SRP_AUTH is not enabled for the client.",
        },
        WordingRow {
            case: "the password flow through the client-side sign-in",
            operation: "InitiateAuth",
            auth_flow: "USER_PASSWORD_AUTH",
            parameters: || json!({ "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD }),
            message: "USER_PASSWORD_AUTH flow not enabled for this client",
        },
        WordingRow {
            case: "the refresh flow through the client-side sign-in",
            operation: "InitiateAuth",
            auth_flow: "REFRESH_TOKEN_AUTH",
            parameters: || json!({ "REFRESH_TOKEN": "not-a-token-this-emulator-issued" }),
            message: "REFRESH_TOKEN_AUTH flow not enabled for this client",
        },
        WordingRow {
            case: "the SRP flow through the administrative sign-in",
            operation: "AdminInitiateAuth",
            auth_flow: USER_SRP_AUTH,
            parameters: || json!({ "USERNAME": USERNAME, "SRP_A": "0f" }),
            message: "Auth flow not enabled for this client",
        },
    ];

    let emulator = emulator();
    let user_pool_id = pool(&emulator, "srp-flow-wording-pool", json!({})).await;
    ok(
        &emulator,
        "AdminCreateUser",
        &json!({ "UserPoolId": user_pool_id, "Username": USERNAME }),
    )
    .await;
    ok(
        &emulator,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    // An app client that enables ONE flow, which is none of the flows the rows
    // ask for.
    let client_id = app_client(
        &emulator,
        &user_pool_id,
        "no-flows-client",
        json!({ "ExplicitAuthFlows": ["ALLOW_CUSTOM_AUTH"] }),
    )
    .await;

    for row in &rows {
        let case = row.case;
        let mut body = Map::new();
        if row.operation == "AdminInitiateAuth" {
            body.insert("UserPoolId".to_string(), json!(user_pool_id));
        }
        body.insert("ClientId".to_string(), json!(client_id));
        body.insert("AuthFlow".to_string(), json!(row.auth_flow));
        body.insert("AuthParameters".to_string(), (row.parameters)());
        let (status, payload) = dispatch(
            &emulator,
            row.operation,
            &Value::Object(body),
            row.operation == "AdminInitiateAuth",
        )
        .await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (REFUSED, INVALID_PARAMETER, row.message),
            "case {case:?}: the refusal must carry the sentence this API words it with, got \
             {status} with {payload}"
        );
        assert!(
            !error_message(&payload).contains(client_id.as_str()),
            "case {case:?}: the refusal names no app client, got {}",
            error_message(&payload)
        );
    }
}
