// SPDX-License-Identifier: CC0-1.0

//! The `Session` an outstanding challenge is published with, over the wire,
//! driven exactly as an SDK drives it (`POST /` with `X-Amz-Target:
//! AWSCognitoIdentityProviderService.<Op>`).
//!
//! The rule under test: **a `Session` a 2xx publishes is the opaque,
//! server-minted token Amazon Cognito vends for an outstanding exchange — a
//! base64url blob of several hundred characters, inside the modelled
//! `SessionType` length 20..4096 and with no rendering of the state it stands
//! for in it — and it IS the session the rest of the flow carries: the value
//! the response published is the one the next call of the exchange resolves,
//! nothing else is current for that challenge, and two outstanding challenges
//! never share a token.**
//!
//! `SessionType` (`smithy_query(query="shape", shape="SessionType")`) is a
//! `@sensitive` string of length 20..4096 with no `@pattern`, so the model
//! leaves the encoding to the service: the member's own description — "the
//! session identifier that maintains the state of authentication requests and
//! challenge responses" — says the caller only ever hands the value back
//! verbatim, which is what makes a short rendering of the server's own state
//! (an id, a digest) the wrong class of value and the long opaque token the
//! right one.
//!
//! The rows span the calls that publish such a session at all: the
//! administrative and the client-side sign-in that raise
//! `NEW_PASSWORD_REQUIRED`, the sign-in that raises `MFA_SETUP`, and the
//! software-token association and verification that carry that setup forward.
//! Every pool, app client, user, code and session is real: they come from
//! `CreateUserPool`, `SetUserPoolMfaConfig`, `CreateUserPoolClient`,
//! `AdminCreateUser`, `AdminSetUserPassword`, `AdminInitiateAuth`,
//! `InitiateAuth`, `AssociateSoftwareToken` and `VerifySoftwareToken`, and each
//! step presents the session the step before it published.
//!
//! Each case is named in every assertion, which reports the value it saw.

use std::time::SystemTime;

use serde_json::{json, Value};

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::state;

use super::software_token_setup::{
    app_client, current_step, dispatch, emulator, error_code, ok, ok_self_service,
    one_time_password, pool, Emulator, GOOD_PASSWORD,
};

/// The modelled bounds of a `Session` (`SessionType`: length 20..4096).
const SESSION_LENGTH: (usize, usize) = (20, 4096);

/// How short a published session may be before it is no longer the token class
/// Amazon Cognito vends: its challenge session is several hundred characters of
/// opaque material, an order of magnitude longer than any rendering of the
/// handful of facts the server holds for the challenge, and a client, proxy or
/// store calibrated against it sizes itself from that.
const MIN_TOKEN_LENGTH: usize = 200;

/// The challenge a user created by an administrator with a temporary password
/// owes.
const NEW_PASSWORD_REQUIRED: &str = "NEW_PASSWORD_REQUIRED";

/// The challenge a user of a pool that requires MFA with no factor registered
/// owes.
const MFA_SETUP: &str = "MFA_SETUP";

/// The user every flow signs in as.
const USERNAME: &str = "ada";

/// The password an administrator creates the user with, which satisfies a
/// default pool's password policy.
const TEMPORARY_PASSWORD: &str = "Temp0rary!2024";

/// The password a `NEW_PASSWORD_REQUIRED` response chooses.
const CHOSEN_PASSWORD: &str = "Ch0senPassw0rd!";

/// The status this endpoint answers a refused caller with.
const REFUSED: u16 = 400;

/// The error a challenge response whose `Session` resolves no outstanding
/// challenge of the request earns.
const SESSION_MISMATCH: &str = "CodeMismatchException";

/// One pool, one app client and the user signing in through them.
struct Fixture {
    user_pool_id: String,
    client_id: String,
}

/// Whether a flow is driven through the administrative pair of operations or the
/// client-side one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Driver {
    Admin,
    Client,
}

impl Driver {
    /// The sign-in operation of this pair.
    fn initiate(self) -> &'static str {
        match self {
            Self::Admin => "AdminInitiateAuth",
            Self::Client => "InitiateAuth",
        }
    }

    /// The challenge-response operation of this pair.
    fn respond(self) -> &'static str {
        match self {
            Self::Admin => "AdminRespondToAuthChallenge",
            Self::Client => "RespondToAuthChallenge",
        }
    }

    /// Whether this pair is addressed with a credential scope, as an SDK sends
    /// every administrative operation.
    fn signed(self) -> bool {
        self == Self::Admin
    }

    /// The `AuthFlowType` this pair's password sign-in asks for.
    fn auth_flow(self) -> &'static str {
        match self {
            Self::Admin => "ADMIN_USER_PASSWORD_AUTH",
            Self::Client => "USER_PASSWORD_AUTH",
        }
    }

    /// The sign-in request body for `fixture` with `password`.
    fn sign_in_body(self, fixture: &Fixture, password: &str) -> Value {
        let mut body = json!({
            "ClientId": fixture.client_id,
            "AuthFlow": self.auth_flow(),
            "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": password },
        });
        if self == Self::Admin {
            body["UserPoolId"] = json!(fixture.user_pool_id);
        }
        body
    }

    /// The challenge-response request body for `fixture`, presenting `session`.
    fn respond_body(
        self,
        fixture: &Fixture,
        challenge_name: &str,
        session: &str,
        responses: Value,
    ) -> Value {
        let mut challenge_responses = json!({ "USERNAME": USERNAME });
        if let Some(members) = responses.as_object() {
            for (name, value) in members {
                challenge_responses[name] = value.clone();
            }
        }
        let mut body = json!({
            "ClientId": fixture.client_id,
            "ChallengeName": challenge_name,
            "Session": session,
            "ChallengeResponses": challenge_responses,
        });
        if self == Self::Admin {
            body["UserPoolId"] = json!(fixture.user_pool_id);
        }
        body
    }
}

/// Call `operation` through `driver`, requiring success, and return the body.
async fn served(emulator: &Emulator, driver: Driver, operation: &str, body: &Value) -> Value {
    let (status, payload) = dispatch(emulator, operation, body, driver.signed()).await;
    assert_eq!(
        status, 200,
        "{operation} must be served, got {status} with {payload}"
    );
    payload
}

/// Assert that `session` belongs to the opaque session-token class, revealing
/// none of `held` — the facts the server keeps for the challenge the session
/// stands for.
fn assert_opaque_session(case: &str, session: &str, held: &[&str]) {
    let (low, high) = SESSION_LENGTH;
    let length = session.chars().count();
    assert!(
        (low..=high).contains(&length),
        "case {case:?}: expected a session inside the modelled length {SESSION_LENGTH:?}, got \
         {length} characters in {session:?}"
    );
    assert!(
        length >= MIN_TOKEN_LENGTH,
        "case {case:?}: expected a token of at least {MIN_TOKEN_LENGTH} characters, got {length} \
         in {session:?}"
    );
    assert!(
        session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "case {case:?}: expected a base64url token, got {session:?}"
    );
    assert!(
        data_encoding::BASE64URL_NOPAD
            .decode(session.trim_end_matches('=').as_bytes())
            .is_ok(),
        "case {case:?}: expected a token a reader can decode as a base64url blob, got {session:?}"
    );
    // A rendering of the server's own state is a digest or an identifier, which
    // is hexadecimal or decimal throughout; an opaque blob is not.
    assert!(
        !session.chars().all(|c| c.is_ascii_hexdigit()),
        "case {case:?}: expected an opaque token rather than a hexadecimal rendering of the \
         challenge state, got {session:?}"
    );
    for fact in held {
        assert!(
            !session.contains(fact),
            "case {case:?}: an opaque token states nothing of the challenge it stands for, got \
             {fact:?} inside {session:?}"
        );
    }
}

/// A pool and an app client named `name`, holding `USERNAME` as a user an
/// administrator created with a temporary password, so its sign-in owes the
/// `NEW_PASSWORD_REQUIRED` challenge.
async fn owing_a_password_change(emulator: &Emulator, name: &str) -> Fixture {
    let user_pool_id = pool(emulator, name, json!({})).await;
    let client_id = app_client(emulator, &user_pool_id, &format!("{name}-client"), json!({})).await;
    ok(
        emulator,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "TemporaryPassword": TEMPORARY_PASSWORD,
            "MessageAction": "SUPPRESS",
        }),
    )
    .await;
    Fixture {
        user_pool_id,
        client_id,
    }
}

/// A pool named `name` that requires MFA with TOTP offered, an app client of it
/// and `USERNAME` with a permanent password, so its sign-in owes the
/// `MFA_SETUP` challenge.
async fn owing_a_factor(emulator: &Emulator, name: &str) -> Fixture {
    let user_pool_id = pool(emulator, name, json!({})).await;
    ok(
        emulator,
        "SetUserPoolMfaConfig",
        &json!({
            "UserPoolId": user_pool_id,
            "MfaConfiguration": "ON",
            "SoftwareTokenMfaConfiguration": { "Enabled": true },
        }),
    )
    .await;
    let client_id = app_client(emulator, &user_pool_id, &format!("{name}-client"), json!({})).await;
    ok(
        emulator,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "MessageAction": "SUPPRESS",
        }),
    )
    .await;
    ok(
        emulator,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
    Fixture {
        user_pool_id,
        client_id,
    }
}

/// Read the `Session` a challenge response published, asserting the challenge
/// it names.
fn challenge_session(case: &str, challenge_name: &str, payload: &Value) -> String {
    assert_eq!(
        payload["ChallengeName"].as_str(),
        Some(challenge_name),
        "case {case:?}: expected the {challenge_name} challenge, got {payload}"
    );
    payload["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a raised challenge publishes a session, got \
             {payload}"))
        .to_string()
}

#[tokio::test]
async fn every_published_challenge_session_is_an_opaque_aws_shaped_token() {
    let emulator = emulator();
    // (case, the session the call published, the facts the server holds for it)
    let mut published: Vec<(String, String, Vec<String>)> = Vec::new();

    // The two sign-in pairs that raise `NEW_PASSWORD_REQUIRED`.
    for driver in [Driver::Admin, Driver::Client] {
        let case = format!("{} raising {NEW_PASSWORD_REQUIRED}", driver.initiate());
        let fixture = owing_a_password_change(
            &emulator,
            &format!("token-shape-{}", driver.initiate().to_lowercase()),
        )
        .await;
        let started = served(
            &emulator,
            driver,
            driver.initiate(),
            &driver.sign_in_body(&fixture, TEMPORARY_PASSWORD),
        )
        .await;
        let session = challenge_session(&case, NEW_PASSWORD_REQUIRED, &started);
        published.push((
            case,
            session,
            vec![
                fixture.user_pool_id.clone(),
                fixture.client_id.clone(),
                USERNAME.to_string(),
                NEW_PASSWORD_REQUIRED.to_string(),
            ],
        ));
    }

    // The software-token setup: the sign-in raises `MFA_SETUP`, the association
    // carries the setup forward and the verification hands on the session that
    // satisfies the challenge. Each step presents the session the step before it
    // published, so each one is reached only because that value resolved.
    let fixture = owing_a_factor(&emulator, "token-shape-mfa-setup").await;
    let started = served(
        &emulator,
        Driver::Client,
        "InitiateAuth",
        &Driver::Client.sign_in_body(&fixture, GOOD_PASSWORD),
    )
    .await;
    let setup_challenge = challenge_session("InitiateAuth raising MFA_SETUP", MFA_SETUP, &started);
    let associated = ok_self_service(
        &emulator,
        "AssociateSoftwareToken",
        &json!({ "Session": setup_challenge }),
    )
    .await;
    let secret_code = associated["SecretCode"]
        .as_str()
        .expect("the association reports the key it minted")
        .to_string();
    let association_session = associated["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("the association carries the setup forward: {associated}"))
        .to_string();
    let verified = ok_self_service(
        &emulator,
        "VerifySoftwareToken",
        &json!({
            "Session": association_session,
            "UserCode": one_time_password(&secret_code, current_step(SystemTime::now())),
        }),
    )
    .await;
    let verification_session = verified["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("the verification hands on a session: {verified}"))
        .to_string();
    let held = vec![
        fixture.user_pool_id.clone(),
        fixture.client_id.clone(),
        USERNAME.to_string(),
        MFA_SETUP.to_string(),
        secret_code.clone(),
    ];
    published.push((
        format!("InitiateAuth raising {MFA_SETUP}"),
        setup_challenge,
        held.clone(),
    ));
    published.push((
        "AssociateSoftwareToken carrying the setup forward".to_string(),
        association_session,
        held.clone(),
    ));
    published.push((
        format!("VerifySoftwareToken satisfying {MFA_SETUP}"),
        verification_session,
        held,
    ));

    for (case, session, held) in &published {
        let held: Vec<&str> = held.iter().map(String::as_str).collect();
        assert_opaque_session(case, session, &held);
        // Each outstanding exchange carries its own token: no two of the
        // sessions this emulator published are the same value.
        let same = published
            .iter()
            .filter(|(_, other, _)| other == session)
            .count();
        assert_eq!(
            same, 1,
            "case {case:?}: expected a token no other call published, got it {same} times"
        );
    }

    // The setup was driven END TO END on the sessions above: the session the
    // verification published completes the sign-in the setup challenge opened.
    let completed = served(
        &emulator,
        Driver::Client,
        "RespondToAuthChallenge",
        &Driver::Client.respond_body(
            &fixture,
            MFA_SETUP,
            published
                .last()
                .map(|(_, session, _)| session.as_str())
                .expect("the verification's session"),
            json!({}),
        ),
    )
    .await;
    assert!(
        completed["AuthenticationResult"]["AccessToken"].is_string(),
        "the session the verification published must complete the sign-in, got {completed}"
    );
}

#[tokio::test]
async fn the_session_a_challenge_publishes_is_the_one_the_flow_carries() {
    // The rows differ along the pair of operations that raises and answers the
    // challenge, which is what the rule generalises over.
    for driver in [Driver::Admin, Driver::Client] {
        let emulator = emulator();
        let case = format!("the challenge of {}", driver.initiate());
        let fixture = owing_a_password_change(
            &emulator,
            &format!("carried-{}", driver.initiate().to_lowercase()),
        )
        .await;
        let started = served(
            &emulator,
            driver,
            driver.initiate(),
            &driver.sign_in_body(&fixture, TEMPORARY_PASSWORD),
        )
        .await;
        let session = challenge_session(&case, NEW_PASSWORD_REQUIRED, &started);

        // The published value IS the key the outstanding challenge is held
        // under: the server has no second, current value for it.
        {
            let service_state =
                state::service_state(&*emulator.engine).expect("this service's state");
            let guard = service_state.lock();
            let stored = guard.auth_sessions.get(&session);
            assert!(
                stored.is_some_and(|stored| stored.challenge_name == NEW_PASSWORD_REQUIRED
                    && stored.username == USERNAME
                    && stored.user_pool_id == fixture.user_pool_id
                    && stored.client_id == fixture.client_id),
                "case {case:?}: the published session must be the key of the outstanding \
                 challenge, got {stored:?}"
            );
        }

        // A value that is not the published one, however close, is no session of
        // this challenge: only the token the response published resolves it.
        let mut shorter = session.clone();
        shorter.pop();
        let longer = format!("{session}A");
        for (variant, presented) in [
            ("one character short", shorter.as_str()),
            ("one character longer", longer.as_str()),
        ] {
            let (status, payload) = dispatch(
                &emulator,
                driver.respond(),
                &driver.respond_body(
                    &fixture,
                    NEW_PASSWORD_REQUIRED,
                    presented,
                    json!({ "NEW_PASSWORD": CHOSEN_PASSWORD }),
                ),
                driver.signed(),
            )
            .await;
            assert_eq!(
                (status, error_code(&payload)),
                (REFUSED, SESSION_MISMATCH),
                "case {case:?} with a session {variant}: expected the session half to resolve \
                 nothing, got {status} with {payload}"
            );
        }

        // Handing the published value back VERBATIM is what the flow carries, so
        // it answers the challenge and completes the sign-in.
        let answered = served(
            &emulator,
            driver,
            driver.respond(),
            &driver.respond_body(
                &fixture,
                NEW_PASSWORD_REQUIRED,
                &session,
                json!({ "NEW_PASSWORD": CHOSEN_PASSWORD }),
            ),
        )
        .await;
        assert!(
            answered["AuthenticationResult"]["AccessToken"].is_string(),
            "case {case:?}: the published session must answer the challenge, got {answered}"
        );
    }
}
