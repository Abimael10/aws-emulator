// SPDX-License-Identifier: CC0-1.0

//! Signing a user in with the Secure Remote Password protocol, driven exactly as
//! an SDK drives it (`POST /` with `X-Amz-Target:
//! AWSCognitoIdentityProviderService.InitiateAuth` / `.AdminInitiateAuth` /
//! `.RespondToAuthChallenge` / `.AdminRespondToAuthChallenge`).
//!
//! The rule under test: **a sign-in that names the secure-remote-password flow
//! sends no password — "`USER_SRP_AUTH` — Username-password authentication with
//! the Secure Remote Password (SRP) protocol" (`InitiateAuth.AuthFlow` in this
//! service's model) — so it is answered with the `PASSWORD_VERIFIER` challenge
//! of one SRP exchange (`SALT`, `SRP_B` and the `SECRET_BLOCK` the claim signs,
//! beside the `USER_ID_FOR_SRP` the challenge names the user in) and a
//! `Session`; the challenge response carries the claim that exchange checks —
//! "`PASSWORD_VERIFIER`: Respond with `PASSWORD_CLAIM_SIGNATURE`,
//! `PASSWORD_CLAIM_SECRET_BLOCK`, and `TIMESTAMP`" (`ChallengeResponses` in this
//! service's model) — and a claim computed from the password this service STORED
//! for the user carries that sign-in exactly as far as one that sent the
//! password directly, while a claim computed from any other password is refused
//! `NotAuthorizedException` with the sentence a wrong password carries.**
//!
//! The rows differ along the password the claim was computed from (the stored
//! one, another password of the same shape, the stored one cased differently and
//! the stored one with trailing space), along which pair of operations drives
//! the exchange (the client-side pair and the administrative pair), and along
//! what the user still owes once its password is proved (nothing, a new
//! password, or a registered software token).
//!
//! The client half of the exchange is computed here — the arithmetic the client
//! library of this flow does, over the group and key schedule
//! [`super::device_authentication`] already drives the device leg with — so
//! nothing but the service decides whether a claim proves the password.
//! Everything else is real: the pools, app clients, users, passwords,
//! authenticators and challenge sessions all come from this service's own
//! operations.

use std::time::SystemTime;

use base64::Engine as _;
use hmac::{Hmac, Mac};
use num_bigint::BigUint;
use serde_json::{json, Map, Value};
use sha2::Sha256;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::state as cognito_idp_state;

use super::device_authentication::{
    DERIVED_KEY_LABEL, SESSION_KEY_BYTES, generator, hash, multiplier, padded, prime,
    pseudo_random,
};
use super::software_token_setup::{
    GOOD_PASSWORD, Emulator, app_client, current_step, dispatch, emulator, error_code,
    error_message, ok, ok_self_service, one_time_password, pool,
};

/// The user every fixture signs in.
const USERNAME: &str = "ada";

/// The `AuthFlowType` member under test.
const USER_SRP_AUTH: &str = "USER_SRP_AUTH";

/// The `ChallengeNameType` members the sign-in can answer with here.
const PASSWORD_VERIFIER: &str = "PASSWORD_VERIFIER";
const NEW_PASSWORD_REQUIRED: &str = "NEW_PASSWORD_REQUIRED";
const SOFTWARE_TOKEN_MFA: &str = "SOFTWARE_TOKEN_MFA";

/// The `ChallengeParameters` entries the `PASSWORD_VERIFIER` challenge serves.
const SALT: &str = "SALT";
const SRP_B: &str = "SRP_B";
const SECRET_BLOCK: &str = "SECRET_BLOCK";
const USER_ID_FOR_SRP: &str = "USER_ID_FOR_SRP";

/// The instant the claims below are signed at, which the response carries in
/// `TIMESTAMP` and the service folds into the signed message exactly as it
/// arrives. Fixed text: nothing in the exchange reads it as a time.
const CLAIM_TIMESTAMP: &str = "Mon Jan 1 00:00:00 UTC 2024";

/// Every refusal of this service is answered 400 and names its class in the
/// body, so a row asserting a refusal asserts that status with the code.
const REFUSED: u16 = 400;

/// The class and the entirely fixed sentence a claim that proves no password
/// earns — the same answer a sign-in that sent the wrong password directly gets.
const NOT_AUTHORIZED: &str = "NotAuthorizedException";
const INCORRECT_CREDENTIALS_MESSAGE: &str = "Incorrect username or password.";

/// The class a request whose parameters are refused earns.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// A temporary password an administrator creates a user with, which leaves that
/// user owing the `NEW_PASSWORD_REQUIRED` challenge.
const TEMPORARY_PASSWORD: &str = "Temp0rary!2024";

/// The permanent password such a user chooses when it answers that challenge.
const CHOSEN_PASSWORD: &str = "Ch0senPassw0rd!";

/// A pool with one app client that enables the SRP flow, and one user holding
/// the stored password the claims are checked against.
struct Fixture {
    user_pool_id: String,
    client_id: String,
    /// The pool NAME the key schedule hashes: the part of the pool id after the
    /// Region that prefixes it.
    pool_name: String,
}

/// Which pair of operations a row drives: the client-side pair, authorized by
/// the app client alone, and the administrative pair, authorized by IAM
/// credentials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Driver {
    Client,
    Admin,
}

impl Driver {
    fn initiate(self) -> &'static str {
        match self {
            Self::Client => "InitiateAuth",
            Self::Admin => "AdminInitiateAuth",
        }
    }

    fn respond(self) -> &'static str {
        match self {
            Self::Client => "RespondToAuthChallenge",
            Self::Admin => "AdminRespondToAuthChallenge",
        }
    }

    /// Whether this pair is addressed with a credential scope, as an SDK sends
    /// every administrative operation.
    fn signed(self) -> bool {
        self == Self::Admin
    }

    /// The request body of an SRP sign-in through this pair.
    fn sign_in_body(self, fixture: &Fixture, parameters: Value) -> Value {
        let mut body = Map::new();
        if self == Self::Admin {
            body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
        }
        body.insert("ClientId".to_string(), json!(fixture.client_id));
        body.insert("AuthFlow".to_string(), json!(USER_SRP_AUTH));
        body.insert("AuthParameters".to_string(), parameters);
        Value::Object(body)
    }

    /// The request body of a challenge response through this pair.
    fn respond_body(
        self,
        fixture: &Fixture,
        challenge_name: &str,
        session: &str,
        responses: Value,
    ) -> Value {
        let mut body = Map::new();
        if self == Self::Admin {
            body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
        }
        body.insert("ClientId".to_string(), json!(fixture.client_id));
        body.insert("ChallengeName".to_string(), json!(challenge_name));
        body.insert("Session".to_string(), json!(session));
        body.insert("ChallengeResponses".to_string(), responses);
        Value::Object(body)
    }
}

/// A pool named `name` with `pool_extra` merged into its creation, one app
/// client that enables the SRP and password flows, and the user `USERNAME`
/// holding `password` as its permanent password.
async fn fixture(emulator: &Emulator, name: &str, pool_extra: Value, password: &str) -> Fixture {
    let user_pool_id = pool(emulator, name, pool_extra).await;
    let client_id = app_client(
        emulator,
        &user_pool_id,
        "srp-client",
        json!({
            "ExplicitAuthFlows": [
                "ALLOW_USER_SRP_AUTH",
                "ALLOW_USER_PASSWORD_AUTH",
                "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                "ALLOW_REFRESH_TOKEN_AUTH",
            ],
        }),
    )
    .await;
    ok(
        emulator,
        "AdminCreateUser",
        &json!({ "UserPoolId": user_pool_id, "Username": USERNAME }),
    )
    .await;
    ok(
        emulator,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "Password": password,
            "Permanent": true,
        }),
    )
    .await;
    let pool_name = pool_name_of(&user_pool_id);
    Fixture {
        user_pool_id,
        client_id,
        pool_name,
    }
}

/// The pool name the key schedule hashes: a `UserPoolId` is the Region, an
/// underscore and the pool's own name, and the client library hashes that name.
fn pool_name_of(user_pool_id: &str) -> String {
    user_pool_id
        .split_once('_')
        .map_or(user_pool_id, |(_region, name)| name)
        .to_string()
}

/// The client's half of one exchange: the ephemeral pair the sign-in presents
/// `SRP_A` from, derived from `label` so each row runs its own.
struct ClientExchange {
    private: BigUint,
    public: BigUint,
}

impl ClientExchange {
    fn new(label: &str) -> Self {
        let private = BigUint::from_bytes_be(&pseudo_random(label, 32)) % prime();
        Self {
            public: generator().modpow(&private, prime()),
            private,
        }
    }

    /// The public value as `SRP_A` carries it.
    fn public_hex(&self) -> String {
        hex::encode(padded(&self.public))
    }

    /// The key the claim is signed under: `x = H(salt | H(<PoolName><Username>:
    /// <password>))`, `u = H(PAD(A) | PAD(B))`, `S = (B - k·g^x)^(a + u·x) (mod
    /// N)` and the first [`SESSION_KEY_BYTES`] of the HKDF-SHA256 expansion of
    /// that secret, salted with `u` and labelled [`DERIVED_KEY_LABEL`].
    fn session_key(
        &self,
        pool_name: &str,
        username: &str,
        password: &str,
        server_public_hex: &str,
        salt_hex: &str,
    ) -> [u8; SESSION_KEY_BYTES] {
        let prime = prime();
        let server_public = BigUint::parse_bytes(server_public_hex.as_bytes(), 16)
            .expect("SRP_B is served as hexadecimal");
        let salt = hex::decode(salt_hex).expect("SALT is served as hexadecimal");
        let secret_exponent = BigUint::from_bytes_be(&hash(&[
            &salt,
            &hash(&[format!("{pool_name}{username}:{password}").as_bytes()]),
        ]));
        let scrambler = BigUint::from_bytes_be(&hash(&[
            &padded(&self.public),
            &padded(&server_public),
        ]));
        let offset = multiplier() * generator().modpow(&secret_exponent, prime) % prime;
        let base = (server_public + prime - offset) % prime;
        let shared = base.modpow(&(&self.private + scrambler.clone() * &secret_exponent), prime);
        let mut session_key = [0_u8; SESSION_KEY_BYTES];
        hkdf::Hkdf::<Sha256>::new(Some(&padded(&scrambler)), &padded(&shared))
            .expand(DERIVED_KEY_LABEL, &mut session_key)
            .expect("the expansion serves a key of this size");
        session_key
    }
}

/// The claim: the HMAC of the exchange's session key over the pool name, the
/// username the challenge served, the secret block it served and the instant the
/// claim was signed at.
fn claim_signature(
    session_key: &[u8],
    pool_name: &str,
    username: &str,
    secret_block: &str,
    timestamp: &str,
) -> String {
    let standard = base64::engine::general_purpose::STANDARD;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(session_key).expect("HMAC takes a key of any length");
    mac.update(pool_name.as_bytes());
    mac.update(username.as_bytes());
    mac.update(
        &standard
            .decode(secret_block)
            .expect("the secret block is base64"),
    );
    mac.update(timestamp.as_bytes());
    standard.encode(mac.finalize().into_bytes())
}

/// The `PASSWORD_VERIFIER` challenge a sign-in through `driver` was answered
/// with: its session and the three values the claim is computed from.
struct Served {
    session: String,
    user_id_for_srp: String,
    salt: String,
    server_public: String,
    secret_block: String,
}

/// Start an SRP sign-in through `driver` and read the exchange it served.
async fn serve(
    emulator: &Emulator,
    driver: Driver,
    fixture: &Fixture,
    exchange: &ClientExchange,
    case: &str,
) -> Served {
    let (status, payload) = dispatch(
        emulator,
        driver.initiate(),
        &driver.sign_in_body(
            fixture,
            json!({ "USERNAME": USERNAME, "SRP_A": exchange.public_hex() }),
        ),
        driver.signed(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {} must serve the SRP challenge, got {status} with {payload}",
        driver.initiate()
    );
    assert_eq!(
        payload["ChallengeName"].as_str(),
        Some(PASSWORD_VERIFIER),
        "case {case:?}: the SRP flow owes {PASSWORD_VERIFIER}, got {payload}"
    );
    assert!(
        payload["AuthenticationResult"].is_null(),
        "case {case:?}: no password has been proved yet, so no token is issued, got {payload}"
    );
    let parameter = |entry: &str| {
        payload["ChallengeParameters"][entry]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: the challenge must serve {entry}, got {payload}")
            })
            .to_string()
    };
    Served {
        session: payload["Session"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the challenge carries a session, got {payload}"))
            .to_string(),
        user_id_for_srp: parameter(USER_ID_FOR_SRP),
        salt: parameter(SALT),
        server_public: parameter(SRP_B),
        secret_block: parameter(SECRET_BLOCK),
    }
}

/// Answer the `PASSWORD_VERIFIER` challenge `served` with the claim `password`
/// produces, through `driver`.
async fn claim(
    emulator: &Emulator,
    driver: Driver,
    fixture: &Fixture,
    exchange: &ClientExchange,
    served: &Served,
    password: &str,
) -> (u16, Value) {
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
    dispatch(
        emulator,
        driver.respond(),
        &driver.respond_body(
            fixture,
            PASSWORD_VERIFIER,
            &served.session,
            json!({
                "USERNAME": served.user_id_for_srp,
                "PASSWORD_CLAIM_SECRET_BLOCK": served.secret_block,
                "PASSWORD_CLAIM_SIGNATURE": signature,
                "TIMESTAMP": CLAIM_TIMESTAMP,
            }),
        ),
        driver.signed(),
    )
    .await
}

/// One row of the claim table: the password the claim is computed from and
/// whether it is the one this service stored for the user.
struct ClaimRow {
    case: &'static str,
    password: &'static str,
    proves_the_password: bool,
}

/// The rows differ along the password the claim was computed from: the stored
/// one, another password of the same shape, the stored one cased differently,
/// and the stored one with a trailing space.
const CLAIM_ROWS: &[ClaimRow] = &[
    ClaimRow {
        case: "the claim of the stored password",
        password: GOOD_PASSWORD,
        proves_the_password: true,
    },
    ClaimRow {
        case: "the claim of another password of the same shape",
        password: "NotTheP4ssword!",
        proves_the_password: false,
    },
    ClaimRow {
        case: "the claim of the stored password in another case",
        password: "passw0rd!2024",
        proves_the_password: false,
    },
    ClaimRow {
        case: "the claim of the stored password with a trailing space",
        password: "Passw0rd!2024 ",
        proves_the_password: false,
    },
];

/// Only the claim the STORED password produces completes the sign-in; every
/// other claim is refused with the sentence a wrong password carries, through
/// either pair of operations.
#[tokio::test]
async fn only_the_claim_of_the_stored_password_signs_the_user_in() {
    for driver in [Driver::Client, Driver::Admin] {
        for (index, row) in CLAIM_ROWS.iter().enumerate() {
            let emulator = emulator();
            let case = format!("{} through {}", row.case, driver.respond());
            let fixture = fixture(
                &emulator,
                &format!("srp-claim-pool-{}-{index}", driver.initiate()),
                json!({}),
                GOOD_PASSWORD,
            )
            .await;
            let exchange = ClientExchange::new(&case);
            let served = serve(&emulator, driver, &fixture, &exchange, &case).await;
            assert_eq!(
                served.user_id_for_srp, USERNAME,
                "case {case:?}: the challenge names the user it is outstanding for, got {:?}",
                served.user_id_for_srp
            );

            let (status, payload) =
                claim(&emulator, driver, &fixture, &exchange, &served, row.password).await;
            if row.proves_the_password {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the claim of the stored password must complete the sign-in, \
                     got {status} with {payload}"
                );
                assert!(
                    payload["AuthenticationResult"]["AccessToken"].is_string(),
                    "case {case:?}: the completed sign-in issues tokens, got {payload}"
                );
                assert!(
                    payload["AuthenticationResult"]["RefreshToken"].is_string(),
                    "case {case:?}: the completed sign-in issues a refresh token, got {payload}"
                );
            } else {
                assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (REFUSED, NOT_AUTHORIZED, INCORRECT_CREDENTIALS_MESSAGE),
                    "case {case:?}: a claim that proves no password is refused, got {status} \
                     with {payload}"
                );
                assert!(
                    payload.get("AuthenticationResult").is_none(),
                    "case {case:?}: a refused claim issues no tokens, got {payload}"
                );
            }
        }
    }
}

/// Each sign-in serves an exchange of its OWN: two sign-ins of one user carry
/// different public values and different secret blocks, so a claim cannot be
/// computed from what an earlier sign-in served.
#[tokio::test]
async fn every_sign_in_serves_an_exchange_of_its_own() {
    let emulator = emulator();
    let case = "two sign-ins of one user";
    let fixture = fixture(&emulator, "srp-exchange-pool", json!({}), GOOD_PASSWORD).await;

    let first_exchange = ClientExchange::new("first");
    let first = serve(&emulator, Driver::Client, &fixture, &first_exchange, case).await;
    let second_exchange = ClientExchange::new("second");
    let second = serve(&emulator, Driver::Client, &fixture, &second_exchange, case).await;

    assert_ne!(
        first.server_public, second.server_public,
        "case {case:?}: each exchange serves its own public value, got {:?} twice",
        first.server_public
    );
    assert_ne!(
        first.secret_block, second.secret_block,
        "case {case:?}: each exchange serves its own secret block, got {:?} twice",
        first.secret_block
    );
    assert_ne!(
        first.session, second.session,
        "case {case:?}: each sign-in carries its own session, got {:?} twice",
        first.session
    );

    // The claim of one exchange answers THAT exchange: presenting it on the
    // other sign-in's session proves nothing, because the secret block it signed
    // is not the one that session served.
    let session_key = first_exchange.session_key(
        &fixture.pool_name,
        &first.user_id_for_srp,
        GOOD_PASSWORD,
        &first.server_public,
        &first.salt,
    );
    let signature = claim_signature(
        &session_key,
        &fixture.pool_name,
        &first.user_id_for_srp,
        &first.secret_block,
        CLAIM_TIMESTAMP,
    );
    let (status, payload) = dispatch(
        &emulator,
        Driver::Client.respond(),
        &Driver::Client.respond_body(
            &fixture,
            PASSWORD_VERIFIER,
            &second.session,
            json!({
                "USERNAME": first.user_id_for_srp,
                "PASSWORD_CLAIM_SECRET_BLOCK": first.secret_block,
                "PASSWORD_CLAIM_SIGNATURE": signature,
                "TIMESTAMP": CLAIM_TIMESTAMP,
            }),
        ),
        false,
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (REFUSED, NOT_AUTHORIZED, INCORRECT_CREDENTIALS_MESSAGE),
        "case {case:?}: a claim made against another exchange proves nothing, got {status} with \
         {payload}"
    );

    // That refusal left the session standing, so the claim of its own exchange
    // still completes it.
    let (status, payload) = claim(
        &emulator,
        Driver::Client,
        &fixture,
        &second_exchange,
        &second,
        GOOD_PASSWORD,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the session must still be answerable with its own claim, got {status} \
         with {payload}"
    );
}

/// A claim answers its challenge ONCE: the session it answered is spent, so
/// replaying the same claim completes no second sign-in.
#[tokio::test]
async fn a_claim_answers_its_challenge_once() {
    let emulator = emulator();
    let case = "the same claim presented twice";
    let fixture = fixture(&emulator, "srp-replay-pool", json!({}), GOOD_PASSWORD).await;
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
        "case {case:?}: the first claim completes the sign-in, got {status} with {payload}"
    );

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
        (status, error_code(&payload)),
        (REFUSED, NOT_AUTHORIZED),
        "case {case:?}: the answered session cannot be answered again, got {status} with {payload}"
    );
    assert!(
        payload.get("AuthenticationResult").is_none(),
        "case {case:?}: a refused replay issues no tokens, got {payload}"
    );
}

/// A proved password carries the sign-in to whatever the user still owes: a user
/// Amazon Cognito is waiting on a new password from gets `NEW_PASSWORD_REQUIRED`
/// and a user with a registered software token gets `SOFTWARE_TOKEN_MFA` —
/// exactly the challenges a sign-in that sent the password directly reaches —
/// and answering that challenge completes the sign-in.
#[tokio::test]
async fn a_proved_password_reaches_the_challenge_the_user_still_owes() {
    let emulator = emulator();
    let case = "a user that must choose a new password";
    let user_pool_id = pool(&emulator, "srp-new-password-pool", json!({})).await;
    let client_id = app_client(
        &emulator,
        &user_pool_id,
        "srp-client",
        json!({ "ExplicitAuthFlows": ["ALLOW_USER_SRP_AUTH", "ALLOW_REFRESH_TOKEN_AUTH"] }),
    )
    .await;
    ok(
        &emulator,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "TemporaryPassword": TEMPORARY_PASSWORD,
        }),
    )
    .await;
    let fixture = Fixture {
        pool_name: pool_name_of(&user_pool_id),
        user_pool_id,
        client_id,
    };

    let exchange = ClientExchange::new(case);
    let served = serve(&emulator, Driver::Client, &fixture, &exchange, case).await;
    let (status, payload) = claim(
        &emulator,
        Driver::Client,
        &fixture,
        &exchange,
        &served,
        TEMPORARY_PASSWORD,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the claim of the temporary password must be served, got {status} with \
         {payload}"
    );
    assert_eq!(
        payload["ChallengeName"].as_str(),
        Some(NEW_PASSWORD_REQUIRED),
        "case {case:?}: a user that owes a new password gets that challenge, got {payload}"
    );
    assert!(
        payload["AuthenticationResult"].is_null(),
        "case {case:?}: no token is issued while a challenge stands, got {payload}"
    );
    let session = payload["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the next challenge carries a session, got {payload}"))
        .to_string();

    let (status, payload) = dispatch(
        &emulator,
        Driver::Client.respond(),
        &Driver::Client.respond_body(
            &fixture,
            NEW_PASSWORD_REQUIRED,
            &session,
            json!({ "USERNAME": USERNAME, "NEW_PASSWORD": CHOSEN_PASSWORD }),
        ),
        false,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: answering the new-password challenge completes the sign-in, got {status} \
         with {payload}"
    );
    assert!(
        payload["AuthenticationResult"]["AccessToken"].is_string(),
        "case {case:?}: the completed sign-in issues tokens, got {payload}"
    );

    // The password the challenge stored is the one the next SRP exchange checks.
    let exchange = ClientExchange::new("after the new password");
    let served = serve(&emulator, Driver::Client, &fixture, &exchange, case).await;
    let (status, payload) = claim(
        &emulator,
        Driver::Client,
        &fixture,
        &exchange,
        &served,
        CHOSEN_PASSWORD,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the chosen password is what the next exchange checks, got {status} with \
         {payload}"
    );
    assert!(
        payload["AuthenticationResult"]["AccessToken"].is_string(),
        "case {case:?}: the sign-in with the chosen password issues tokens, got {payload}"
    );
}

/// The second factor a user has activated stands between its proved password and
/// its tokens, exactly as it does for a sign-in that sent the password directly.
#[tokio::test]
async fn a_proved_password_still_owes_the_activated_second_factor() {
    let emulator = emulator();
    let case = "a user with a registered software token";
    let fixture = fixture(&emulator, "srp-mfa-pool", json!({}), GOOD_PASSWORD).await;
    ok(
        &emulator,
        "SetUserPoolMfaConfig",
        &json!({
            "UserPoolId": fixture.user_pool_id,
            "MfaConfiguration": "OPTIONAL",
            "SoftwareTokenMfaConfiguration": { "Enabled": true },
        }),
    )
    .await;

    // The authenticator is enrolled through the direct password sign-in, whose
    // access token authorizes the setup.
    let signed_in = ok_self_service(
        &emulator,
        "InitiateAuth",
        &json!({
            "ClientId": fixture.client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    let access_token = signed_in["AuthenticationResult"]["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the first sign-in issues a token: {signed_in}"))
        .to_string();
    let associated = ok_self_service(
        &emulator,
        "AssociateSoftwareToken",
        &json!({ "AccessToken": access_token }),
    )
    .await;
    let secret_code = associated["SecretCode"]
        .as_str()
        .expect("the association reports a key")
        .to_string();
    let enrolment_step = current_step(SystemTime::now());
    ok_self_service(
        &emulator,
        "VerifySoftwareToken",
        &json!({
            "AccessToken": access_token,
            "UserCode": one_time_password(&secret_code, enrolment_step),
        }),
    )
    .await;
    ok(
        &emulator,
        "AdminSetUserMFAPreference",
        &json!({
            "UserPoolId": fixture.user_pool_id,
            "Username": USERNAME,
            "SoftwareTokenMfaSettings": { "Enabled": true, "PreferredMfa": true },
        }),
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
        "case {case:?}: the claim of the stored password must be served, got {status} with \
         {payload}"
    );
    assert_eq!(
        payload["ChallengeName"].as_str(),
        Some(SOFTWARE_TOKEN_MFA),
        "case {case:?}: the activated factor must be challenged, got {payload}"
    );
    assert!(
        payload["AuthenticationResult"].is_null(),
        "case {case:?}: no token is issued while the factor stands, got {payload}"
    );
    let session = payload["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the factor's challenge carries a session, got {payload}"))
        .to_string();

    let (status, payload) = dispatch(
        &emulator,
        Driver::Client.respond(),
        &Driver::Client.respond_body(
            &fixture,
            SOFTWARE_TOKEN_MFA,
            &session,
            json!({
                "USERNAME": USERNAME,
                "SOFTWARE_TOKEN_MFA_CODE": one_time_password(&secret_code, enrolment_step),
            }),
        ),
        false,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the authenticator's code completes the sign-in, got {status} with {payload}"
    );
    assert!(
        payload["AuthenticationResult"]["AccessToken"].is_string(),
        "case {case:?}: the completed sign-in issues tokens, got {payload}"
    );
}

/// What a row plants on the stored user before the sign-in starts.
type StoreUser = fn(&mut cognito_idp_state::User);

/// One row of the stored-state table: the case, what the user holds, and the
/// wire error class that state owes.
type StateRow = (&'static str, StoreUser, &'static str);

/// What the stored user's own state owes the SRP sign-in: the refusals every
/// sign-in flow of this service answers for a user that cannot sign in, served
/// on the FIRST leg — an exchange a user that cannot sign in would never
/// complete is not served at all. The rows differ along the stored state.
#[tokio::test]
async fn the_stored_user_state_refuses_the_srp_sign_in() {
    // Each row is (case, what the stored user holds, the class it owes).
    let rows: &[StateRow] = &[
        (
            "a user that never confirmed its sign-up",
            |user| user.status = cognito_idp_state::UserStatus::Unconfirmed,
            "UserNotConfirmedException",
        ),
        (
            "a user Amazon Cognito put in RESET_REQUIRED",
            |user| user.status = cognito_idp_state::UserStatus::ResetRequired,
            "PasswordResetRequiredException",
        ),
        (
            "a disabled user",
            |user| user.enabled = false,
            NOT_AUTHORIZED,
        ),
    ];

    for driver in [Driver::Client, Driver::Admin] {
        for (index, (case, store, expected)) in rows.iter().enumerate() {
            let emulator = emulator();
            let case = format!("{case} through {}", driver.initiate());
            let fixture = fixture(
                &emulator,
                &format!("srp-user-state-pool-{}-{index}", driver.initiate()),
                json!({}),
                GOOD_PASSWORD,
            )
            .await;
            let exchange = ClientExchange::new(&case);

            // The state the user holds when the sign-in starts is what refuses
            // the first leg: no exchange is served for it.
            store_user(&emulator, &fixture, *store);
            let (status, payload) = dispatch(
                &emulator,
                driver.initiate(),
                &driver.sign_in_body(
                    &fixture,
                    json!({ "USERNAME": USERNAME, "SRP_A": exchange.public_hex() }),
                ),
                driver.signed(),
            )
            .await;
            assert_eq!(
                (status, error_code(&payload)),
                (REFUSED, *expected),
                "case {case:?}: the stored user's state must refuse the first leg, got {status} \
                 with {payload}"
            );
            assert!(
                payload.get("ChallengeName").is_none(),
                "case {case:?}: a refused sign-in serves no challenge, got {payload}"
            );
        }
    }
}

/// A user whose state changes while the `PASSWORD_VERIFIER` challenge stands is
/// refused by that state when its claim arrives: proving the password is not
/// what decides whether the user may sign in.
#[tokio::test]
async fn a_user_disabled_while_the_challenge_stands_cannot_answer_it() {
    let emulator = emulator();
    let case = "a user disabled after the exchange was served";
    let fixture = fixture(&emulator, "srp-disabled-late-pool", json!({}), GOOD_PASSWORD).await;
    let exchange = ClientExchange::new(case);
    let served = serve(&emulator, Driver::Client, &fixture, &exchange, case).await;

    store_user(&emulator, &fixture, |user| user.enabled = false);
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
        (status, error_code(&payload), error_message(&payload)),
        (REFUSED, NOT_AUTHORIZED, "User is disabled."),
        "case {case:?}: the stored user's state refuses the claim, got {status} with {payload}"
    );
    assert!(
        payload.get("AuthenticationResult").is_none(),
        "case {case:?}: a refused claim issues no tokens, got {payload}"
    );

    // That refusal answered no challenge, so the exchange is still bound to the
    // session: once the stored state allows the sign-in again, the SAME claim
    // completes it.
    store_user(&emulator, &fixture, |user| user.enabled = true);
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
        "case {case:?}: the outstanding challenge must still take its own claim, got {status} \
         with {payload}"
    );
    assert!(
        payload["AuthenticationResult"]["AccessToken"].is_string(),
        "case {case:?}: the completed sign-in issues tokens, got {payload}"
    );
}

/// Apply `store` to the stored user of `fixture`, which is how a row plants
/// state no operation of this service writes.
fn store_user(emulator: &Emulator, fixture: &Fixture, store: StoreUser) {
    let state = cognito_idp_state::service_state(&*emulator.engine).expect("this service's state");
    let mut guard = state.lock();
    let pool = guard
        .pool_mut(&fixture.user_pool_id)
        .expect("the stored pool");
    let user = pool.user_mut(USERNAME).expect("the stored user");
    store(user);
}

/// What the first leg of the exchange refuses: the entries the flow requires, a
/// public value the group cannot use, a user the pool does not hold, and an app
/// client the flow is not enabled for.
#[tokio::test]
async fn the_srp_sign_in_refuses_what_it_cannot_serve() {
    let emulator = emulator();
    let fixture = fixture(&emulator, "srp-refusal-pool", json!({}), GOOD_PASSWORD).await;
    let exchange = ClientExchange::new("refusals");

    // Each row is (case, the `AuthParameters` presented, the class it owes, the
    // sentence it carries).
    let rows: Vec<(&str, Value, &str, String)> = vec![
        (
            "no SRP_A at all",
            json!({ "USERNAME": USERNAME }),
            INVALID_PARAMETER,
            "Missing required parameter SRP_A".to_string(),
        ),
        (
            "no USERNAME at all",
            json!({ "SRP_A": exchange.public_hex() }),
            INVALID_PARAMETER,
            "Missing required parameter USERNAME".to_string(),
        ),
        (
            "an SRP_A that is not hexadecimal",
            json!({ "USERNAME": USERNAME, "SRP_A": "not-a-number" }),
            INVALID_PARAMETER,
            "Invalid SRP_A value.".to_string(),
        ),
        (
            "an SRP_A that is zero modulo the group's prime",
            json!({ "USERNAME": USERNAME, "SRP_A": "00" }),
            INVALID_PARAMETER,
            "Invalid SRP_A value.".to_string(),
        ),
        (
            "a user this pool does not hold",
            json!({ "USERNAME": "nobody", "SRP_A": exchange.public_hex() }),
            "UserNotFoundException",
            "User does not exist.".to_string(),
        ),
    ];

    for (case, parameters, expected_code, expected_message) in rows {
        let (status, payload) = dispatch(
            &emulator,
            Driver::Client.initiate(),
            &Driver::Client.sign_in_body(&fixture, parameters),
            false,
        )
        .await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (REFUSED, expected_code, expected_message.as_str()),
            "case {case:?}: the first leg must refuse this request, got {status} with {payload}"
        );
    }

    // An app client whose `ExplicitAuthFlows` do not enable the flow serves no
    // exchange at all.
    let closed_client = app_client(
        &emulator,
        &fixture.user_pool_id,
        "no-srp-client",
        json!({ "ExplicitAuthFlows": ["ALLOW_REFRESH_TOKEN_AUTH"] }),
    )
    .await;
    let case = "an app client the SRP flow is not enabled for";
    let (status, payload) = dispatch(
        &emulator,
        Driver::Client.initiate(),
        &json!({
            "ClientId": closed_client,
            "AuthFlow": USER_SRP_AUTH,
            "AuthParameters": { "USERNAME": USERNAME, "SRP_A": exchange.public_hex() },
        }),
        false,
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (
            REFUSED,
            INVALID_PARAMETER,
            "USER_SRP_AUTH flow not enabled for this client"
        ),
        "case {case:?}: a client the flow is not enabled for serves no exchange, got {status} \
         with {payload}"
    );
}

/// What a response to the `PASSWORD_VERIFIER` challenge has to carry: each entry
/// the challenge requires is reported as the parameter it is.
#[tokio::test]
async fn a_password_verifier_response_names_the_entry_it_is_missing() {
    let emulator = emulator();
    let fixture = fixture(&emulator, "srp-response-entries-pool", json!({}), GOOD_PASSWORD).await;

    // The rows differ along which required entry the response leaves out.
    for entry in [
        "PASSWORD_CLAIM_SIGNATURE",
        "PASSWORD_CLAIM_SECRET_BLOCK",
        "TIMESTAMP",
    ] {
        let case = format!("a response carrying no {entry}");
        let exchange = ClientExchange::new(&case);
        let served = serve(&emulator, Driver::Client, &fixture, &exchange, &case).await;
        let session_key = exchange.session_key(
            &fixture.pool_name,
            &served.user_id_for_srp,
            GOOD_PASSWORD,
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
        responses.insert("USERNAME".to_string(), json!(USERNAME));
        responses.insert(
            "PASSWORD_CLAIM_SECRET_BLOCK".to_string(),
            json!(served.secret_block),
        );
        responses.insert("PASSWORD_CLAIM_SIGNATURE".to_string(), json!(signature));
        responses.insert("TIMESTAMP".to_string(), json!(CLAIM_TIMESTAMP));
        responses.remove(entry);

        let (status, payload) = dispatch(
            &emulator,
            Driver::Client.respond(),
            &Driver::Client.respond_body(
                &fixture,
                PASSWORD_VERIFIER,
                &served.session,
                Value::Object(responses),
            ),
            false,
        )
        .await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (
                REFUSED,
                INVALID_PARAMETER,
                format!("Missing required parameter {entry}").as_str()
            ),
            "case {case:?}: the absent entry must be named, got {status} with {payload}"
        );
    }
}
