// SPDX-License-Identifier: CC0-1.0

//! Which STAGE a pre-token-generation event names, over the set of generations one pool's
//! tokens can be minted by — including the one a remembered DEVICE completes.
//!
//! RULE under test: a pool's `LambdaConfig` is "a collection of user pool Lambda triggers.
//! Amazon Cognito invokes triggers at several possible stages of user pool operations"
//! (`smithy_query(query="doc",shape="LambdaConfigType")`), and the pre-token-generation
//! member is reached from every stage that MINTS a token, so the event's `triggerSource`
//! names the stage that reached it and nothing else: one value per stage, so a function
//! serving all of them can branch. Amazon Cognito documents one source per stage for this
//! trigger — a completed authentication ("called after user authentication flows have
//! completed"), a refresh ("called when a user tries to refresh the identity and access
//! tokens"), the temporary-password change ("called after the user is created by an admin.
//! This flow is invoked when the user has to change a temporary password") and the device
//! exchange ("called at the end of the authentication of a user device") — so the sign-in
//! a DEVICE proves itself to complete is a stage of its own, not the one a user's own
//! factor completes.
//!
//! The stages driven here are the three this pool's own operations can reach twice over:
//! a password sign-in that completes, a challenge the USER answers with the factor the
//! sign-in asked for, the `DEVICE_PASSWORD_VERIFIER` claim a remembered DEVICE answers
//! with, and a refresh of the session the first sign-in minted. The device exchange is
//! driven through BOTH response operations (`RespondToAuthChallenge` and
//! `AdminRespondToAuthChallenge`), because the stage a generation is at does not depend on
//! which pair of operations carried the response.
//!
//! [`super::trigger_source_branching`] owns the same rule over the stages that need no
//! device, and this suite runs its router and its function: Cognito and Lambda on one
//! engine, with a `python3.12` trigger whose handler reads `event["triggerSource"]` and
//! asks for claims recording it, so every value asserted below is one that function
//! computed. The device half of the exchange is computed here, from
//! [`super::device_authentication`]'s own derivation of the protocol, so a wrong
//! computation in the service cannot be the reason a row passes. Nothing here asserts a
//! value baked into the service: the pool id, the client id, the function ARN, the device
//! keys and the tokens all come back from the state the setup calls created.

use std::collections::BTreeSet;
use std::time::{Duration, SystemTime};

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::device_authentication::{claim_signature, DeviceCredentials, CLAIM_TIMESTAMP};
use super::pre_token_generation_overrides::claims;
use super::software_token_setup::{current_step, one_time_password};
use super::trigger_source_branching::{
    cognito, ok, router, token, trigger_function, GOOD_PASSWORD, SECOND_EVENT_VERSION,
    SOURCE_CLAIM, VERSION_CLAIM,
};

/// The user every sign-in below signs in as.
const USERNAME: &str = "ada";

/// The `ChallengeNameType` members this suite is answered with
/// (`smithy_query(query="shape",shape="ChallengeNameType")`).
const SOFTWARE_TOKEN_MFA: &str = "SOFTWARE_TOKEN_MFA";
const DEVICE_SRP_AUTH: &str = "DEVICE_SRP_AUTH";
const DEVICE_PASSWORD_VERIFIER: &str = "DEVICE_PASSWORD_VERIFIER";

/// The device password the registered verifier is derived from, which only the device holds.
const DEVICE_PASSWORD: &str = "device-password";

/// The documented pre-token-generation sources of the stages this suite reaches.
const BY_AUTHENTICATION: &str = "TokenGeneration_Authentication";
const BY_DEVICE: &str = "TokenGeneration_AuthenticateDevice";
const BY_REFRESH: &str = "TokenGeneration_RefreshTokens";

/// How long a one-time password keeps being read and presented before that is a failure,
/// and how long each try waits before looking again. One time step of a TOTP password
/// lasts 30 s, so a machine that crosses a boundary between reading one and presenting it
/// simply reads the next: the window is several steps wide, and only a service that never
/// accepts the authenticator's password runs it out. Nothing here tests the window.
const PRESENT_CODE_DEADLINE: Duration = Duration::from_secs(150);
const LOOK_AGAIN: Duration = Duration::from_millis(200);

/// Which pair of operations drives a sign-in: the client-side pair, authorized by the app
/// client alone, and the administrative pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Driver {
    Client,
    Admin,
}

impl Driver {
    /// The operation that STARTS the sign-in.
    fn initiate(self) -> &'static str {
        match self {
            Self::Client => "InitiateAuth",
            Self::Admin => "AdminInitiateAuth",
        }
    }

    /// The operation that ANSWERS an outstanding challenge.
    fn respond(self) -> &'static str {
        match self {
            Self::Client => "RespondToAuthChallenge",
            Self::Admin => "AdminRespondToAuthChallenge",
        }
    }

    /// The `AuthFlowType` value this pair's password sign-in asks for.
    fn auth_flow(self) -> &'static str {
        match self {
            Self::Client => "USER_PASSWORD_AUTH",
            Self::Admin => "ADMIN_USER_PASSWORD_AUTH",
        }
    }

    /// The password sign-in body of this pair, presenting `device_key` where one is given.
    fn sign_in_body(
        self,
        user_pool_id: &str,
        client_id: &str,
        device_key: Option<&str>,
    ) -> Value {
        let mut parameters = json!({ "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD });
        if let Some(device_key) = device_key {
            parameters["DEVICE_KEY"] = json!(device_key);
        }
        let mut body = json!({
            "ClientId": client_id,
            "AuthFlow": self.auth_flow(),
            "AuthParameters": parameters,
        });
        if self == Self::Admin {
            body["UserPoolId"] = json!(user_pool_id);
        }
        body
    }

    /// The challenge-response body of this pair.
    fn respond_body(
        self,
        user_pool_id: &str,
        client_id: &str,
        challenge_name: &str,
        session: &str,
        mut responses: Value,
    ) -> Value {
        responses["USERNAME"] = json!(USERNAME);
        let mut body = json!({
            "ClientId": client_id,
            "ChallengeName": challenge_name,
            "Session": session,
            "ChallengeResponses": responses,
        });
        if self == Self::Admin {
            body["UserPoolId"] = json!(user_pool_id);
        }
        body
    }
}

#[tokio::test]
async fn the_pre_token_generation_source_names_the_generation_that_minted_the_tokens() {
    let router = router();
    let function = trigger_function(&router, "device-token-generation-source").await;

    // A pool that asks a remembered device to authenticate itself, whose
    // pre-token-generation trigger is the function above at the second trigger version.
    let created = ok(
        &router,
        "CreateUserPool",
        &json!({
            "PoolName": "device-generation-sources",
            // The tier a pool that configures the second trigger version runs on.
            "UserPoolTier": "ESSENTIALS",
            "DeviceConfiguration": {
                "ChallengeRequiredOnNewDevice": true,
                "DeviceOnlyRememberedOnUserPrompt": true,
            },
            "LambdaConfig": {
                "PreTokenGenerationConfig": { "LambdaVersion": "V2_0", "LambdaArn": function },
            },
        }),
    )
    .await;
    let user_pool_id = created["UserPool"]["Id"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPool must mint an id, got {created}"))
        .to_string();
    ok(
        &router,
        "SetUserPoolMfaConfig",
        &json!({
            "UserPoolId": user_pool_id,
            "MfaConfiguration": "OPTIONAL",
            "SoftwareTokenMfaConfiguration": { "Enabled": true },
        }),
    )
    .await;
    let client = ok(
        &router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": "device-generation-app",
            "ExplicitAuthFlows": [
                "ALLOW_USER_PASSWORD_AUTH",
                "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                "ALLOW_REFRESH_TOKEN_AUTH",
            ],
        }),
    )
    .await;
    let client_id = client["UserPoolClient"]["ClientId"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateUserPoolClient must mint a client id, got {client}"))
        .to_string();
    ok(
        &router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "MessageAction": "SUPPRESS",
        }),
    )
    .await;
    ok(
        &router,
        "AdminSetUserPassword",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;

    // Every source the generations below were handed, in the order they were minted.
    let mut observed: Vec<(&str, String)> = Vec::new();

    // Stage 1: a password sign-in that COMPLETES — no factor is activated yet, which is
    // also what lets the user enroll one with the access token it issues.
    let case = "a password sign-in completing through InitiateAuth";
    let signed_in = ok(
        &router,
        Driver::Client.initiate(),
        &Driver::Client.sign_in_body(&user_pool_id, &client_id, None),
    )
    .await;
    observed.push((case, assert_source(case, &signed_in, BY_AUTHENTICATION)));
    let first_access = token(case, &signed_in, "AccessToken", "access").to_string();
    let refresh_token = token(case, &signed_in, "RefreshToken", "refresh").to_string();

    // The factor the next sign-in is challenged with, enrolled by the user itself.
    let associated = ok(
        &router,
        "AssociateSoftwareToken",
        &json!({ "AccessToken": first_access }),
    )
    .await;
    let secret_code = associated["SecretCode"]
        .as_str()
        .unwrap_or_else(|| panic!("the association must report a key, got {associated}"))
        .to_string();
    present_one_time_password(&router, "VerifySoftwareToken", &secret_code, |code| {
        json!({ "AccessToken": first_access, "UserCode": code })
    })
    .await;
    ok(
        &router,
        "AdminSetUserMFAPreference",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "SoftwareTokenMfaSettings": { "Enabled": true, "PreferredMfa": true },
        }),
    )
    .await;

    // Stage 2: the challenge the USER answers with that factor, which completes the
    // sign-in and registers the device the stage below then confirms.
    let case = "a challenge answered with the user's own factor";
    let challenged = ok(
        &router,
        Driver::Client.initiate(),
        &Driver::Client.sign_in_body(&user_pool_id, &client_id, None),
    )
    .await;
    let session = challenge_session(case, SOFTWARE_TOKEN_MFA, &challenged);
    let answered = present_one_time_password(
        &router,
        Driver::Client.respond(),
        &secret_code,
        |code| {
            Driver::Client.respond_body(
                &user_pool_id,
                &client_id,
                SOFTWARE_TOKEN_MFA,
                &session,
                json!({ "SOFTWARE_TOKEN_MFA_CODE": code }),
            )
        },
    )
    .await;
    observed.push((case, assert_source(case, &answered, BY_AUTHENTICATION)));
    let metadata = &answered["AuthenticationResult"]["NewDeviceMetadata"];
    let device_key = metadata["DeviceKey"]
        .as_str()
        .unwrap_or_else(|| panic!("the sign-in must report the device it came from: {answered}"))
        .to_string();
    let device_group_key = metadata["DeviceGroupKey"]
        .as_str()
        .unwrap_or_else(|| panic!("the sign-in must report the device group: {answered}"))
        .to_string();
    let device_access = token(case, &answered, "AccessToken", "access").to_string();

    // The device registers the credentials its exchange runs over, and the user remembers
    // it — which is what lets it stand in for the factor above.
    let credentials = DeviceCredentials::new(
        device_group_key,
        device_key.clone(),
        DEVICE_PASSWORD.to_string(),
        "device-token-generation-source",
    );
    ok(
        &router,
        "ConfirmDevice",
        &json!({
            "AccessToken": device_access,
            "DeviceKey": device_key,
            "DeviceName": "ada's phone",
            "DeviceSecretVerifierConfig": credentials.verifier_config(),
        }),
    )
    .await;
    ok(
        &router,
        "UpdateDeviceStatus",
        &json!({
            "AccessToken": device_access,
            "DeviceKey": device_key,
            "DeviceRememberedStatus": "remembered",
        }),
    )
    .await;

    // Stage 3: the DEVICE's own exchange, driven through each pair of operations in turn.
    for driver in [Driver::Client, Driver::Admin] {
        let case = match driver {
            Driver::Client => "the device exchange ending in RespondToAuthChallenge",
            Driver::Admin => "the device exchange ending in AdminRespondToAuthChallenge",
        };
        let minted = device_exchange(
            &router,
            driver,
            &user_pool_id,
            &client_id,
            &credentials,
            case,
        )
        .await;
        observed.push((case, assert_source(case, &minted, BY_DEVICE)));
    }

    // Stage 4: a refresh of the session the first sign-in minted.
    let case = "a refresh of the first session through InitiateAuth";
    let refreshed = ok(
        &router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "REFRESH_TOKEN_AUTH",
            "AuthParameters": { "REFRESH_TOKEN": refresh_token },
        }),
    )
    .await;
    observed.push((case, assert_source(case, &refreshed, BY_REFRESH)));

    // One documented source per stage: a device's own proof is told apart from the user's
    // own authentication and from a refresh of it, which is the whole point of the member.
    let distinct: BTreeSet<&str> = observed.iter().map(|(_, source)| source.as_str()).collect();
    assert_eq!(
        distinct,
        BTreeSet::from([BY_AUTHENTICATION, BY_DEVICE, BY_REFRESH]),
        "expected one documented source per stage across these generations, got {observed:?}"
    );
}

/// Drive the device-authentication exchange of `credentials` from a sign-in that presents
/// its key to the tokens its claim mints, and answer the minting response.
async fn device_exchange(
    router: &MultiServiceRouter,
    driver: Driver,
    user_pool_id: &str,
    client_id: &str,
    credentials: &DeviceCredentials,
    case: &str,
) -> Value {
    let started = ok(
        router,
        driver.initiate(),
        &driver.sign_in_body(user_pool_id, client_id, Some(&credentials.device_key)),
    )
    .await;
    let session = challenge_session(case, DEVICE_SRP_AUTH, &started);

    // The device's own half of the exchange, with an ephemeral pair of this exchange.
    let exchange = credentials.exchange(case);
    let served = ok(
        router,
        driver.respond(),
        &driver.respond_body(
            user_pool_id,
            client_id,
            DEVICE_SRP_AUTH,
            &session,
            json!({
                "DEVICE_KEY": credentials.device_key,
                "SRP_A": exchange.public_hex(),
            }),
        ),
    )
    .await;
    let next = challenge_session(case, DEVICE_PASSWORD_VERIFIER, &served);
    let parameters = served["ChallengeParameters"].clone();
    let server_public = parameter(case, &parameters, "SRP_B");
    let salt = parameter(case, &parameters, "SALT");
    let secret_block = parameter(case, &parameters, "SECRET_BLOCK");

    let session_key = exchange.session_key(credentials, DEVICE_PASSWORD, &server_public, &salt);
    let signature = claim_signature(&session_key, credentials, &secret_block, CLAIM_TIMESTAMP);
    ok(
        router,
        driver.respond(),
        &driver.respond_body(
            user_pool_id,
            client_id,
            DEVICE_PASSWORD_VERIFIER,
            &next,
            json!({
                "DEVICE_KEY": credentials.device_key,
                "TIMESTAMP": CLAIM_TIMESTAMP,
                "PASSWORD_CLAIM_SECRET_BLOCK": secret_block,
                "PASSWORD_CLAIM_SIGNATURE": signature,
            }),
        ),
    )
    .await
}

/// One `ChallengeParameters` entry the device challenge served.
fn parameter(case: &str, parameters: &Value, name: &str) -> String {
    parameters[name]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the challenge must serve {name}, got {parameters}")
        })
        .to_string()
}

/// Assert that both customised tokens of `payload` carry `expected_source` as the source
/// the function branched on, and the configured trigger version beside it, and answer the
/// source the ACCESS token carries.
///
/// Both tokens, because the second trigger contract customises both; the access token is
/// the one a resource server reads a claim from.
fn assert_source(case: &str, payload: &Value, expected_source: &str) -> String {
    let mut from_access = String::new();
    for (member, kind) in [("AccessToken", "access"), ("IdToken", "identity")] {
        let decoded = claims(case, kind, token(case, payload, member, kind));
        let source = decoded
            .get(SOURCE_CLAIM)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        assert_eq!(
            source, expected_source,
            "case {case:?}: expected the {kind} token to carry the source the function \
             branched on, got {decoded}"
        );
        assert_eq!(
            decoded.get(VERSION_CLAIM).and_then(Value::as_str),
            Some(SECOND_EVENT_VERSION),
            "case {case:?}: expected the {kind} token to carry the configured trigger \
             version, got {decoded}"
        );
        if kind == "access" {
            from_access = source;
        }
    }
    from_access
}

/// Assert that `payload` is an outstanding challenge for `challenge_name`, and answer its
/// session.
fn challenge_session(case: &str, challenge_name: &str, payload: &Value) -> String {
    assert_eq!(
        payload["ChallengeName"].as_str(),
        Some(challenge_name),
        "case {case:?}: expected ChallengeName={challenge_name}, got {payload}"
    );
    payload["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Session, got {payload}"))
        .to_string()
}

/// Present the one-time password the enrolled authenticator shows to `operation`, as a
/// client of that authenticator does: the password of the time step it is read in, read
/// and presented again while the call is not served, so the outcome depends on the
/// service's answer rather than on how fast this runs.
async fn present_one_time_password(
    router: &MultiServiceRouter,
    operation: &str,
    secret_code: &str,
    body: impl Fn(String) -> Value,
) -> Value {
    let deadline = SystemTime::now() + PRESENT_CODE_DEADLINE;
    loop {
        let code = one_time_password(secret_code, current_step(SystemTime::now()));
        let (status, payload) = cognito(router, operation, &body(code)).await;
        if status == 200 {
            return payload;
        }
        assert!(
            SystemTime::now() < deadline,
            "{operation} must be served with the authenticator's own one-time password, got \
             {status} with {payload}"
        );
        tokio::time::sleep(LOOK_AGAIN).await;
    }
}
