// SPDX-License-Identifier: CC0-1.0

//! Device authentication over the wire, driven exactly as an SDK drives it
//! (`POST /` with `X-Amz-Target: AWSCognitoIdentityProviderService.InitiateAuth`
//! / `.AdminInitiateAuth` / `.RespondToAuthChallenge` /
//! `.AdminRespondToAuthChallenge`).
//!
//! The rule under test: **on a user pool that asks a remembered device to
//! authenticate itself, a sign-in presenting that device's `DEVICE_KEY` does NOT
//! complete on the key — it is answered with `DEVICE_SRP_AUTH`, that challenge
//! is answered with `DEVICE_PASSWORD_VERIFIER` carrying `SRP_B`, `SALT` and
//! `SECRET_BLOCK`, and only a `PASSWORD_CLAIM_SIGNATURE` that verifies against
//! the per-device credentials `ConfirmDevice` registered mints tokens.** The
//! pool setting that asks for it is `ChallengeRequiredOnNewDevice`: "When true, a
//! remembered device can sign in with device authentication instead of SMS and
//! time-based one-time password (TOTP) factors for multi-factor authentication
//! (MFA)"
//! (`smithy_query(query="doc",shape="DeviceConfigurationType",member="ChallengeRequiredOnNewDevice")`),
//! and the credentials it runs over are the ones `ConfirmDevice` registers: "A
//! Secure Remote Password (SRP) value that your application generates when you
//! register a user's device"
//! (`smithy_query(query="doc",shape="ConfirmDeviceRequest",member="DeviceSecretVerifierConfig")`).
//!
//! Each table row names the case it asserts and every assertion reports the
//! actual value. The rows differ along the device password the verifier is
//! derived from, along which pair of operations drives the exchange (the
//! client-side pair and the administrative pair), along what the presented
//! `DEVICE_KEY` names, along what the pool's device configuration asks for, and
//! along which part of the claim a device gets wrong.
//!
//! The DEVICE half of the exchange is computed here, from the protocol itself —
//! the verifier from a locally chosen device password, the public value from an
//! ephemeral of this suite's own, the session key by the same expansion the
//! protocol defines and the claim as the HMAC over the values the challenge
//! served — so a wrong computation in the service cannot be the reason a row
//! passes. Its salts and ephemerals are derived from each row's own label, so
//! every row runs a different exchange and the suite still runs the same way
//! twice. Everything else is real: the pools, app clients, users, software-token
//! enrollments, challenge sessions and devices all come from this service's own
//! operations.

use base64::Engine as _;
use hmac::{Hmac, Mac};
use num_bigint::BigUint;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use std::time::{Duration, SystemTime};

use super::pre_token_generation_overrides::claims;
use super::software_token_setup::{
    app_client, current_step, dispatch, emulator, error_code, error_message, ok, ok_self_service,
    one_time_password, pool, Emulator, GOOD_PASSWORD,
};

/// The user every fixture signs in as.
const USERNAME: &str = "ada";

/// The `ChallengeNameType` members of this exchange, and the one a user with an
/// activated software token owes when no device stands in for it
/// (`smithy_query(query="shape",shape="ChallengeNameType")`).
const DEVICE_SRP_AUTH: &str = "DEVICE_SRP_AUTH";
const DEVICE_PASSWORD_VERIFIER: &str = "DEVICE_PASSWORD_VERIFIER";
const SOFTWARE_TOKEN_MFA: &str = "SOFTWARE_TOKEN_MFA";

/// The refusal a claim that does not verify earns: `NotAuthorizedException` in
/// the body, carrying the sentence a credential that does not check out earns
/// anywhere in this API, which is entirely fixed text. The STATUS is this
/// endpoint's own rule for a refusal of the caller rather than the shape's
/// `@httpError`, which [`super::error_wire`] states and asserts: every refusal is
/// answered 400 and names its class in the body and in the `x-amzn-errortype`
/// header.
const REFUSED: u16 = 400;
const NOT_AUTHORIZED: &str = "NotAuthorizedException";
const INCORRECT_CREDENTIALS_MESSAGE: &str = "Incorrect username or password.";

/// The refusal for a response answering for a device the pool holds no
/// credentials of.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const DEVICE_NOT_FOUND_MESSAGE: &str = "Device does not exist.";

/// The refusal for a response that left out an entry the challenge requires.
const INVALID_PARAMETER: &str = "InvalidParameterException";

/// The instant a device stamps its claim with, in the form a client of this
/// exchange writes one. Part of the signed message, so a response that signs one
/// instant and sends another proves nothing.
const CLAIM_TIMESTAMP: &str = "Mon Jan 1 00:00:00 UTC 2024";

/// The label this exchange's key schedule expands its shared secret under.
const DERIVED_KEY_LABEL: &[u8] = b"Caldera Derived Key";

/// The size of the key the claim is signed under.
const SESSION_KEY_BYTES: usize = 16;

/// The device configuration that asks a remembered device to authenticate
/// itself, and leaves the remembering to the user's own prompt.
fn asks_for_device_authentication() -> Value {
    json!({ "ChallengeRequiredOnNewDevice": true, "DeviceOnlyRememberedOnUserPrompt": true })
}

/// Which pair of operations drives a sign-in: the client-side pair, authorized by
/// the app client alone, and the administrative pair, authorized by IAM
/// credentials.
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

    /// Whether this pair is addressed with a credential scope, as the SDK sends
    /// every administrative operation.
    fn signed(self) -> bool {
        self == Self::Admin
    }

    /// The `AuthFlowType` value this pair's password sign-in asks for.
    fn auth_flow(self) -> &'static str {
        match self {
            Self::Client => "USER_PASSWORD_AUTH",
            Self::Admin => "ADMIN_USER_PASSWORD_AUTH",
        }
    }

    /// The request body of a password sign-in through this pair, with
    /// `parameters` merged into its `AuthParameters`.
    fn sign_in_body(self, fixture: &Fixture, parameters: Value) -> Value {
        let mut auth_parameters = Map::new();
        auth_parameters.insert("USERNAME".to_string(), json!(USERNAME));
        auth_parameters.insert("PASSWORD".to_string(), json!(GOOD_PASSWORD));
        merge(&mut auth_parameters, &parameters);
        let mut body = Map::new();
        if self == Self::Admin {
            body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
        }
        body.insert("ClientId".to_string(), json!(fixture.client_id));
        body.insert("AuthFlow".to_string(), json!(self.auth_flow()));
        body.insert(
            "AuthParameters".to_string(),
            Value::Object(auth_parameters),
        );
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
        let mut challenge_responses = Map::new();
        challenge_responses.insert("USERNAME".to_string(), json!(USERNAME));
        merge(&mut challenge_responses, &responses);
        let mut body = Map::new();
        if self == Self::Admin {
            body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
        }
        body.insert("ClientId".to_string(), json!(fixture.client_id));
        body.insert("ChallengeName".to_string(), json!(challenge_name));
        body.insert("Session".to_string(), json!(session));
        body.insert(
            "ChallengeResponses".to_string(),
            Value::Object(challenge_responses),
        );
        Value::Object(body)
    }
}

/// Merge the members of `extra` into `target`.
fn merge(target: &mut Map<String, Value>, extra: &Value) {
    if let Some(members) = extra.as_object() {
        for (name, value) in members {
            target.insert(name.clone(), value.clone());
        }
    }
}

/// A pool with one app client, one user holding an activated software token, and
/// one device of that user, as the fixture's operations left them.
struct Fixture {
    user_pool_id: String,
    client_id: String,
    /// The device's own credentials, as this suite generated them and
    /// `ConfirmDevice` registered them.
    credentials: DeviceCredentials,
}

/// What the device's confirmation REGISTERS as its credentials: the ones this
/// suite derived from the fixture's device password, or a value that is no
/// verifier of the exchange's group at all — zero modulo its prime, which no
/// `g^x` ever is — which is what a device that cannot give the proof holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Registered {
    Derived,
    Unusable,
}

/// Build the fixture in a pool named `name` whose `DeviceConfiguration` is
/// `device_configuration`: a user that has enrolled a software token and made it
/// its preferred factor, and the device its first challenged sign-in minted —
/// confirmed with the credentials `registered` names, and remembered when
/// `remember`.
///
/// `label` seeds the device's salt and the suite's ephemerals, so two fixtures
/// run different exchanges.
#[allow(clippy::too_many_arguments)]
async fn fixture(
    emulator: &Emulator,
    name: &str,
    device_configuration: Value,
    device_password: &str,
    remember: bool,
    registered: Registered,
    label: &str,
) -> Fixture {
    let user_pool_id = pool(
        emulator,
        name,
        json!({ "DeviceConfiguration": device_configuration }),
    )
    .await;
    ok(
        emulator,
        "SetUserPoolMfaConfig",
        &json!({
            "UserPoolId": user_pool_id,
            "MfaConfiguration": "OPTIONAL",
            "SoftwareTokenMfaConfiguration": { "Enabled": true },
        }),
    )
    .await;
    let client_id = app_client(emulator, &user_pool_id, "device-client", json!({})).await;
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

    // The first sign-in completes: no factor is activated yet, which is what
    // lets the user enroll one with the access token it issues.
    let bare = Fixture {
        user_pool_id: user_pool_id.clone(),
        client_id: client_id.clone(),
        credentials: DeviceCredentials::new(String::new(), String::new(), String::new(), label),
    };
    let signed_in = ok_self_service(
        emulator,
        "InitiateAuth",
        &Driver::Client.sign_in_body(&bare, json!({})),
    )
    .await;
    let first_token = signed_in["AuthenticationResult"]["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a sign-in with no factor activated issues tokens: {signed_in}"))
        .to_string();
    let associated = ok_self_service(
        emulator,
        "AssociateSoftwareToken",
        &json!({ "AccessToken": first_token }),
    )
    .await;
    let secret_code = associated["SecretCode"]
        .as_str()
        .expect("the association reports a key")
        .to_string();
    present_one_time_password(emulator, "VerifySoftwareToken", &secret_code, |code| {
        json!({ "AccessToken": first_token, "UserCode": code })
    })
    .await;
    ok(
        emulator,
        "AdminSetUserMFAPreference",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "SoftwareTokenMfaSettings": { "Enabled": true, "PreferredMfa": true },
        }),
    )
    .await;

    // The factor is activated, so the next sign-in is challenged — and the
    // response that presents the one-time password is what registers the device
    // this suite then confirms.
    let challenged = ok_self_service(
        emulator,
        "InitiateAuth",
        &Driver::Client.sign_in_body(&bare, json!({})),
    )
    .await;
    let session = challenged["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("the activated factor is challenged: {challenged}"))
        .to_string();
    let answered = present_one_time_password(
        emulator,
        "RespondToAuthChallenge",
        &secret_code,
        |code| {
            Driver::Client.respond_body(
                &bare,
                SOFTWARE_TOKEN_MFA,
                &session,
                json!({ "SOFTWARE_TOKEN_MFA_CODE": code }),
            )
        },
    )
    .await;
    let metadata = &answered["AuthenticationResult"]["NewDeviceMetadata"];
    let device_key = metadata["DeviceKey"]
        .as_str()
        .unwrap_or_else(|| panic!("the sign-in reports the device it came from: {answered}"))
        .to_string();
    let device_group_key = metadata["DeviceGroupKey"]
        .as_str()
        .unwrap_or_else(|| panic!("the sign-in reports the device group: {answered}"))
        .to_string();
    let access_token = answered["AuthenticationResult"]["AccessToken"]
        .as_str()
        .expect("the completed sign-in issues an access token")
        .to_string();

    // The device registers the credentials a later device sign-in proves itself
    // with, derived here from the device password this fixture chose.
    let credentials = DeviceCredentials::new(
        device_group_key,
        device_key.clone(),
        device_password.to_string(),
        label,
    );
    let confirmed = ok_self_service(
        emulator,
        "ConfirmDevice",
        &json!({
            "AccessToken": access_token,
            "DeviceKey": device_key,
            "DeviceName": "ada's phone",
            "DeviceSecretVerifierConfig": match registered {
                Registered::Derived => credentials.verifier_config(),
                // Base64 of one zero byte: a number this group cannot use as a
                // verifier, so the device holds no credentials to prove
                // anything with.
                Registered::Unusable => json!({ "PasswordVerifier": "AA==", "Salt": "AAEC" }),
            },
        }),
    )
    .await;
    assert_eq!(
        confirmed["UserConfirmationNecessary"].as_bool(),
        Some(true),
        "a pool that only remembers a device on the user's prompt asks for that prompt, got \
         {confirmed}"
    );
    if remember {
        ok_self_service(
            emulator,
            "UpdateDeviceStatus",
            &json!({
                "AccessToken": access_token,
                "DeviceKey": device_key,
                "DeviceRememberedStatus": "remembered",
            }),
        )
        .await;
    }
    Fixture {
        user_pool_id,
        client_id,
        credentials,
    }
}

/// One row of the claim table: which pair drives the exchange and which device
/// password the registered verifier is derived from, beside a DIFFERENT device
/// password whose claim must be refused.
struct ClaimCase {
    case: &'static str,
    driver: Driver,
    device_password: &'static str,
    other_password: &'static str,
}

/// The rows differ along the device password — its length, its alphabet and the
/// bytes it is spelled in — and along which pair of operations drives the
/// exchange, since the device's proof is the same proof whichever pair carries
/// it.
const CLAIM_CASES: &[ClaimCase] = &[
    ClaimCase {
        case: "a short device password through the client-side pair",
        driver: Driver::Client,
        device_password: "pw",
        other_password: "qw",
    },
    ClaimCase {
        case: "a long device password of mixed bytes through the client-side pair",
        driver: Driver::Client,
        device_password: "ΔΕΥΤΕΡΟΣ-device-password-0123456789-αβγ-!@#$%^&*()_+-=[]{}|;:,.<>?",
        other_password: "ΔΕΥΤΕΡΟΣ-device-password-0123456789-αβγ-!@#$%^&*()_+-=[]{}|;:,.<>",
    },
    ClaimCase {
        case: "a device password through the administrative pair",
        driver: Driver::Admin,
        device_password: "another-device-password",
        other_password: "a-third-device-password",
    },
];

#[tokio::test]
async fn a_remembered_device_proves_it_holds_its_device_password() {
    for (index, row) in CLAIM_CASES.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let fixture = fixture(
            &emulator,
            &format!("device-claim-pool-{index}"),
            asks_for_device_authentication(),
            row.device_password,
            true,
            Registered::Derived,
            case,
        )
        .await;
        let device_key = fixture.credentials.device_key.clone();

        // The sign-in is NOT completed on the device key: it owes the device's
        // own exchange.
        let routed = sign_in(&emulator, row.driver, &fixture, Some(&device_key), case).await;
        let session = challenge_session(case, DEVICE_SRP_AUTH, &routed);
        assert_eq!(
            routed["ChallengeParameters"]["DEVICE_KEY"].as_str(),
            Some(device_key.as_str()),
            "case {case:?}: the challenge names the device it asks about, got {routed}"
        );

        // The claim signed under the registered device password completes the
        // sign-in, and its access token states the device that proved itself.
        let answered = device_exchange(
            &emulator,
            row.driver,
            &fixture,
            &session,
            row.device_password,
            Tamper::None,
            case,
        )
        .await;
        assert_eq!(
            answered.status, 200,
            "case {case:?}: the right claim completes the sign-in, got {} with {}",
            answered.status, answered.payload
        );
        assert!(
            answered.payload.get("ChallengeName").is_none(),
            "case {case:?}: a completed sign-in owes no further challenge, got {}",
            answered.payload
        );
        let access_token = answered.payload["AuthenticationResult"]["AccessToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!(
                    "case {case:?}: the completed exchange issues tokens, got {}",
                    answered.payload
                )
            });
        let token_claims = claims(case, "access", access_token);
        assert_eq!(
            token_claims["device_key"].as_str(),
            Some(device_key.as_str()),
            "case {case:?}: the access token states the device that authenticated, got \
             {token_claims}"
        );
        assert_eq!(
            token_claims["username"].as_str(),
            Some(USERNAME),
            "case {case:?}: the access token states the user that signed in, got {token_claims}"
        );
        assert_eq!(
            token_claims["token_use"].as_str(),
            Some("access"),
            "case {case:?}: expected an access token, got {token_claims}"
        );
        // The SALT served is the one the device registered, and each exchange
        // serves its own secret block and public value.
        assert_eq!(
            answered.served["SALT"].as_str(),
            Some(fixture.credentials.salt_hex().as_str()),
            "case {case:?}: the challenge serves the salt the device registered, got {}",
            answered.served
        );

        // The same device, the same exchange, a DIFFERENT device password: the
        // claim does not verify and no tokens come with the refusal.
        let again = sign_in(&emulator, row.driver, &fixture, Some(&device_key), case).await;
        let session = challenge_session(case, DEVICE_SRP_AUTH, &again);
        let refused = device_exchange(
            &emulator,
            row.driver,
            &fixture,
            &session,
            row.other_password,
            Tamper::None,
            case,
        )
        .await;
        assert_refused(case, "a claim from another device password", &refused);
        assert_ne!(
            refused.served["SECRET_BLOCK"].as_str(),
            answered.served["SECRET_BLOCK"].as_str(),
            "case {case:?}: each exchange serves its own secret block, got {} twice",
            answered.served
        );
        assert_ne!(
            refused.served["SRP_B"].as_str(),
            answered.served["SRP_B"].as_str(),
            "case {case:?}: each exchange serves its own public value, got {} twice",
            answered.served
        );
    }
}

/// One row of the tamper table: which part of the claim the device gets wrong,
/// every other part being the right one.
const TAMPER_CASES: &[(&str, Tamper)] = &[
    (
        "a claim signed over a secret block this exchange did not serve",
        Tamper::SecretBlock,
    ),
    (
        "a claim signed at one instant and sent with another",
        Tamper::Timestamp,
    ),
];

#[tokio::test]
async fn a_claim_that_does_not_cover_what_the_challenge_served_is_refused() {
    for (index, (case, tamper)) in TAMPER_CASES.iter().enumerate() {
        let emulator = emulator();
        let fixture = fixture(
            &emulator,
            &format!("device-tamper-pool-{index}"),
            asks_for_device_authentication(),
            "device-password",
            true,
            Registered::Derived,
            case,
        )
        .await;
        let device_key = fixture.credentials.device_key.clone();
        let routed = sign_in(&emulator, Driver::Client, &fixture, Some(&device_key), case).await;
        let session = challenge_session(case, DEVICE_SRP_AUTH, &routed);
        let refused = device_exchange(
            &emulator,
            Driver::Client,
            &fixture,
            &session,
            "device-password",
            *tamper,
            case,
        )
        .await;
        assert_refused(case, case, &refused);
    }
}

/// One row of the routing table: what the pool's `DeviceConfiguration` asks for,
/// whether the user remembered the confirmed device, what `DEVICE_KEY` the
/// sign-in presents, and the `ChallengeName` it must be answered with — `None`
/// for a sign-in that completes.
struct RoutingCase {
    case: &'static str,
    device_configuration: fn() -> Value,
    remember: bool,
    /// What the device's confirmation registered as its credentials.
    registered: Registered,
    /// What the sign-in presents: the device's own key, a key naming no device,
    /// or no key at all.
    presented: Presented,
    challenge: Option<&'static str>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented {
    OwnDevice,
    UnknownDevice,
    Nothing,
}

/// The rows differ along the pool setting that asks for device authentication,
/// along whether the device is remembered, along what the confirmation
/// registered for it, and along what the presented key names.
const ROUTING_CASES: &[RoutingCase] = &[
    RoutingCase {
        case: "a remembered device of a pool that asks for device authentication",
        device_configuration: asks_for_device_authentication,
        remember: true,
        registered: Registered::Derived,
        presented: Presented::OwnDevice,
        challenge: Some(DEVICE_SRP_AUTH),
    },
    RoutingCase {
        case: "a confirmed device the user has not remembered",
        device_configuration: asks_for_device_authentication,
        remember: false,
        registered: Registered::Derived,
        presented: Presented::OwnDevice,
        challenge: Some(SOFTWARE_TOKEN_MFA),
    },
    RoutingCase {
        case: "a key naming no device of this user",
        device_configuration: asks_for_device_authentication,
        remember: true,
        registered: Registered::Derived,
        presented: Presented::UnknownDevice,
        challenge: Some(SOFTWARE_TOKEN_MFA),
    },
    RoutingCase {
        case: "a sign-in that presents no device at all",
        device_configuration: asks_for_device_authentication,
        remember: true,
        registered: Registered::Derived,
        presented: Presented::Nothing,
        challenge: Some(SOFTWARE_TOKEN_MFA),
    },
    RoutingCase {
        case: "a remembered device whose confirmation registered no usable verifier",
        device_configuration: asks_for_device_authentication,
        remember: true,
        registered: Registered::Unusable,
        presented: Presented::OwnDevice,
        challenge: Some(SOFTWARE_TOKEN_MFA),
    },
    RoutingCase {
        case: "a remembered device of a pool that asks for no device challenge",
        device_configuration: || json!({ "DeviceOnlyRememberedOnUserPrompt": true }),
        remember: true,
        registered: Registered::Derived,
        presented: Presented::OwnDevice,
        challenge: None,
    },
];

#[tokio::test]
async fn the_device_challenge_stands_where_the_pool_asks_for_it() {
    for (index, row) in ROUTING_CASES.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let fixture = fixture(
            &emulator,
            &format!("device-routing-pool-{index}"),
            (row.device_configuration)(),
            "device-password",
            row.remember,
            row.registered,
            case,
        )
        .await;
        let own = fixture.credentials.device_key.clone();
        let presented = match row.presented {
            Presented::OwnDevice => Some(own),
            Presented::UnknownDevice => Some(format!("{own}-no-such-device")),
            Presented::Nothing => None,
        };
        let signed_in =
            sign_in(&emulator, Driver::Client, &fixture, presented.as_deref(), case).await;
        match row.challenge {
            Some(challenge) => {
                challenge_session(case, challenge, &signed_in);
            }
            None => {
                assert!(
                    signed_in.get("ChallengeName").is_none(),
                    "case {case:?}: expected a completed sign-in, got {signed_in}"
                );
                assert!(
                    signed_in["AuthenticationResult"]["AccessToken"].is_string(),
                    "case {case:?}: a completed sign-in issues tokens, got {signed_in}"
                );
            }
        }
    }
}

#[tokio::test]
async fn a_device_response_that_answers_for_nothing_is_refused() {
    let emulator = emulator();
    let case = "device response refusals";
    let fixture = fixture(
        &emulator,
        "device-refusal-pool",
        asks_for_device_authentication(),
        "device-password",
        true,
        Registered::Derived,
        case,
    )
    .await;
    let device_key = fixture.credentials.device_key.clone();

    // A `DEVICE_SRP_AUTH` response that carries no public value answers nothing.
    let routed = sign_in(&emulator, Driver::Client, &fixture, Some(&device_key), case).await;
    let session = challenge_session(case, DEVICE_SRP_AUTH, &routed);
    let (status, payload) = dispatch(
        &emulator,
        "RespondToAuthChallenge",
        &Driver::Client.respond_body(
            &fixture,
            DEVICE_SRP_AUTH,
            &session,
            json!({ "DEVICE_KEY": device_key }),
        ),
        false,
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (REFUSED, INVALID_PARAMETER),
        "case {case:?}: a response with no SRP_A is refused for the entry it left out, got \
         {status} with {payload}"
    );
    assert!(
        error_message(&payload).contains("SRP_A"),
        "case {case:?}: the refusal names the entry at fault, got {payload}"
    );

    // A `DEVICE_SRP_AUTH` response answering for a device this user does not
    // hold answers for nothing.
    let routed = sign_in(&emulator, Driver::Client, &fixture, Some(&device_key), case).await;
    let session = challenge_session(case, DEVICE_SRP_AUTH, &routed);
    let exchange = fixture.credentials.exchange("unknown device");
    let (status, payload) = dispatch(
        &emulator,
        "RespondToAuthChallenge",
        &Driver::Client.respond_body(
            &fixture,
            DEVICE_SRP_AUTH,
            &session,
            json!({
                "DEVICE_KEY": format!("{device_key}-no-such-device"),
                "SRP_A": exchange.public_hex(),
            }),
        ),
        false,
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (REFUSED, RESOURCE_NOT_FOUND, DEVICE_NOT_FOUND_MESSAGE),
        "case {case:?}: a response naming another device is refused for the device, got {status} \
         with {payload}"
    );

    // A `DEVICE_PASSWORD_VERIFIER` response that carries no signature answers
    // nothing.
    let routed = sign_in(&emulator, Driver::Client, &fixture, Some(&device_key), case).await;
    let session = challenge_session(case, DEVICE_SRP_AUTH, &routed);
    let served = ok_self_service(
        &emulator,
        "RespondToAuthChallenge",
        &Driver::Client.respond_body(
            &fixture,
            DEVICE_SRP_AUTH,
            &session,
            json!({ "DEVICE_KEY": device_key, "SRP_A": exchange.public_hex() }),
        ),
    )
    .await;
    let next = challenge_session(case, DEVICE_PASSWORD_VERIFIER, &served);
    let (status, payload) = dispatch(
        &emulator,
        "RespondToAuthChallenge",
        &Driver::Client.respond_body(
            &fixture,
            DEVICE_PASSWORD_VERIFIER,
            &next,
            json!({
                "DEVICE_KEY": device_key,
                "TIMESTAMP": CLAIM_TIMESTAMP,
                "PASSWORD_CLAIM_SECRET_BLOCK":
                    served["ChallengeParameters"]["SECRET_BLOCK"].as_str().unwrap_or_default(),
            }),
        ),
        false,
    )
    .await;
    assert_eq!(
        (status, error_code(&payload)),
        (REFUSED, INVALID_PARAMETER),
        "case {case:?}: a claim with no signature is refused for the entry it left out, got \
         {status} with {payload}"
    );
    assert!(
        error_message(&payload).contains("PASSWORD_CLAIM_SIGNATURE"),
        "case {case:?}: the refusal names the entry at fault, got {payload}"
    );
}

/// Present the one-time password the enrolled authenticator shows to
/// `operation`, as a client of that authenticator does: the password of the time
/// step it is read in, recomputed and presented again where the step the service
/// checks it in is no longer the step it was computed in.
///
/// That race is the one thing about this fixture that depends on WHEN it runs —
/// one time step lasts 30 s and a busy machine can cross a boundary between the
/// two — so the call is repeated until it is served or a generous deadline
/// passes, rather than asserted on the first try.
async fn present_one_time_password(
    emulator: &Emulator,
    operation: &str,
    secret_code: &str,
    body: impl Fn(String) -> Value,
) -> Value {
    let deadline = SystemTime::now() + PRESENT_CODE_DEADLINE;
    loop {
        let code = one_time_password(secret_code, current_step(SystemTime::now()));
        let (status, payload) = dispatch(emulator, operation, &body(code), false).await;
        if status == 200 {
            return payload;
        }
        assert!(
            SystemTime::now() < deadline,
            "{operation} must be served with the authenticator's own one-time password, got \
             {status} with {payload}"
        );
        ::tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// How long [`present_one_time_password`] keeps presenting a freshly read
/// one-time password, and how long it waits between tries: a window several time
/// steps wide, so only a service that never accepts the authenticator's password
/// runs it out.
const PRESENT_CODE_DEADLINE: Duration = Duration::from_secs(150);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Start a password sign-in through `driver`, presenting `device_key` where one
/// is given, and return the response body of a served call.
async fn sign_in(
    emulator: &Emulator,
    driver: Driver,
    fixture: &Fixture,
    device_key: Option<&str>,
    case: &str,
) -> Value {
    let parameters = match device_key {
        Some(device_key) => json!({ "DEVICE_KEY": device_key }),
        None => json!({}),
    };
    let (status, payload) = dispatch(
        emulator,
        driver.initiate(),
        &driver.sign_in_body(fixture, parameters),
        driver.signed(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {} must be served, got {status} with {payload}",
        driver.initiate()
    );
    payload
}

/// Assert that `payload` is an outstanding challenge for `challenge_name` and
/// return its session.
fn challenge_session(case: &str, challenge_name: &str, payload: &Value) -> String {
    assert_eq!(
        payload["ChallengeName"].as_str(),
        Some(challenge_name),
        "case {case:?}: expected ChallengeName={challenge_name}, got {payload}"
    );
    assert!(
        payload.get("AuthenticationResult").is_none(),
        "case {case:?}: an outstanding challenge withholds the tokens, got {payload}"
    );
    payload["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: expected a Session, got {payload}"))
        .to_string()
}

/// Assert that `answered` is the refusal a claim that proves nothing earns, with
/// no tokens beside it.
fn assert_refused(case: &str, what: &str, answered: &Answered) {
    assert_eq!(
        (
            answered.status,
            error_code(&answered.payload),
            error_message(&answered.payload),
        ),
        (REFUSED, NOT_AUTHORIZED, INCORRECT_CREDENTIALS_MESSAGE),
        "case {case:?}: {what} is refused, got {} with {}",
        answered.status,
        answered.payload
    );
    assert!(
        answered.payload.get("AuthenticationResult").is_none(),
        "case {case:?}: {what} comes with no tokens, got {}",
        answered.payload
    );
}

/// What part of its claim the device gets wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tamper {
    /// Nothing: the claim covers exactly what the challenge served.
    None,
    /// The claim is signed over a secret block of the device's own choosing.
    SecretBlock,
    /// The claim is signed at one instant and sent with another.
    Timestamp,
}

/// What driving the exchange to its end answered: the `ChallengeParameters` the
/// `DEVICE_PASSWORD_VERIFIER` challenge served, and the status and body of the
/// response to that challenge.
struct Answered {
    served: Value,
    status: u16,
    payload: Value,
}

/// Drive the exchange from the `DEVICE_SRP_AUTH` session `session` to its end,
/// signing the claim under `password` and applying `tamper` to what it covers.
async fn device_exchange(
    emulator: &Emulator,
    driver: Driver,
    fixture: &Fixture,
    session: &str,
    password: &str,
    tamper: Tamper,
    case: &str,
) -> Answered {
    let credentials = &fixture.credentials;
    let exchange = credentials.exchange(&format!("{case}/{password}/{tamper:?}"));
    let (status, served) = dispatch(
        emulator,
        driver.respond(),
        &driver.respond_body(
            fixture,
            DEVICE_SRP_AUTH,
            session,
            json!({
                "DEVICE_KEY": credentials.device_key,
                "SRP_A": exchange.public_hex(),
            }),
        ),
        driver.signed(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: the device's public value is answered with the password challenge, got \
         {status} with {served}"
    );
    let next = challenge_session(case, DEVICE_PASSWORD_VERIFIER, &served);
    let parameters = served["ChallengeParameters"].clone();
    let server_public = parameters["SRP_B"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the challenge serves SRP_B, got {parameters}"))
        .to_string();
    let salt = parameters["SALT"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the challenge serves SALT, got {parameters}"))
        .to_string();
    let secret_block = parameters["SECRET_BLOCK"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: the challenge serves SECRET_BLOCK, got {parameters}")
        })
        .to_string();

    // The key the claim is signed under, computed from the values the challenge
    // served and the device password this response presents.
    let session_key = exchange.session_key(credentials, password, &server_public, &salt);
    // What the claim COVERS, and what the response then sends: a tampered row
    // signs one value and sends another.
    let (signed_block, sent_block) = match tamper {
        Tamper::SecretBlock => (
            base64::engine::general_purpose::STANDARD.encode(b"a block of the device's own"),
            secret_block.clone(),
        ),
        _ => (secret_block.clone(), secret_block.clone()),
    };
    let (signed_timestamp, sent_timestamp) = match tamper {
        Tamper::Timestamp => (
            CLAIM_TIMESTAMP.to_string(),
            "Tue Jan 2 00:00:00 UTC 2024".to_string(),
        ),
        _ => (CLAIM_TIMESTAMP.to_string(), CLAIM_TIMESTAMP.to_string()),
    };
    let signature = claim_signature(&session_key, credentials, &signed_block, &signed_timestamp);
    let (status, payload) = dispatch(
        emulator,
        driver.respond(),
        &driver.respond_body(
            fixture,
            DEVICE_PASSWORD_VERIFIER,
            &next,
            json!({
                "DEVICE_KEY": credentials.device_key,
                "TIMESTAMP": sent_timestamp,
                "PASSWORD_CLAIM_SECRET_BLOCK": sent_block,
                "PASSWORD_CLAIM_SIGNATURE": signature,
            }),
        ),
        driver.signed(),
    )
    .await;
    Answered {
        served: parameters,
        status,
        payload,
    }
}

/// One device's own credentials: the device the keys name, the device password
/// its owner chose and the salt its verifier is derived with.
struct DeviceCredentials {
    device_group_key: String,
    device_key: String,
    password: String,
    salt: BigUint,
}

impl DeviceCredentials {
    /// The credentials of the device `device_key`, with the salt derived from
    /// `label` so each fixture registers its own.
    fn new(device_group_key: String, device_key: String, password: String, label: &str) -> Self {
        Self {
            device_group_key,
            device_key,
            password,
            salt: BigUint::from_bytes_be(&pseudo_random(&format!("{label}/salt"), 16)),
        }
    }

    /// The `DeviceSecretVerifierConfig` this device registers: the verifier and
    /// the salt it was derived with, each base64 of its bytes.
    fn verifier_config(&self) -> Value {
        let standard = base64::engine::general_purpose::STANDARD;
        json!({
            "PasswordVerifier": standard.encode(padded(&self.verifier())),
            "Salt": standard.encode(padded(&self.salt)),
        })
    }

    /// The salt as the challenge reports it.
    fn salt_hex(&self) -> String {
        hex::encode(padded(&self.salt))
    }

    /// `v = g^x (mod N)`, the verifier derived from this device's password.
    fn verifier(&self) -> BigUint {
        generator().modpow(&self.secret_exponent(&self.salt, &self.password), prime())
    }

    /// `x = H(PAD(salt) | H(<DeviceGroupKey><DeviceKey>:<device password>))`,
    /// the exponent the device password is folded into.
    fn secret_exponent(&self, salt: &BigUint, password: &str) -> BigUint {
        let material = format!("{}{}:{password}", self.device_group_key, self.device_key);
        BigUint::from_bytes_be(&hash(&[&padded(salt), &hash(&[material.as_bytes()])]))
    }

    /// One exchange of this device: an ephemeral pair derived from `label`, so
    /// each exchange of a row runs its own.
    fn exchange(&self, label: &str) -> ClientExchange {
        let private = BigUint::from_bytes_be(&pseudo_random(label, 32)) % prime();
        ClientExchange {
            public: generator().modpow(&private, prime()),
            private,
        }
    }
}

/// The device's half of one exchange.
struct ClientExchange {
    private: BigUint,
    public: BigUint,
}

impl ClientExchange {
    /// The public value as `SRP_A` carries it.
    fn public_hex(&self) -> String {
        hex::encode(padded(&self.public))
    }

    /// The key the device signs its claim under: `u = H(PAD(A) | PAD(B))`,
    /// `S = (B - k·g^x)^(a + u·x) (mod N)` and the first [`SESSION_KEY_BYTES`] of
    /// the HKDF-SHA256 expansion of that secret, salted with `u` and labelled
    /// [`DERIVED_KEY_LABEL`].
    fn session_key(
        &self,
        credentials: &DeviceCredentials,
        password: &str,
        server_public_hex: &str,
        salt_hex: &str,
    ) -> [u8; SESSION_KEY_BYTES] {
        let prime = prime();
        let server_public = BigUint::parse_bytes(server_public_hex.as_bytes(), 16)
            .expect("SRP_B is served as hexadecimal");
        let salt =
            BigUint::parse_bytes(salt_hex.as_bytes(), 16).expect("SALT is served as hexadecimal");
        let scrambler = BigUint::from_bytes_be(&hash(&[
            &padded(&self.public),
            &padded(&server_public),
        ]));
        let secret_exponent = credentials.secret_exponent(&salt, password);
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

/// The claim a device signs: the HMAC of its session key over the device group
/// key, the device key, the secret block the challenge served and the instant it
/// signed at.
fn claim_signature(
    session_key: &[u8],
    credentials: &DeviceCredentials,
    secret_block: &str,
    timestamp: &str,
) -> String {
    let standard = base64::engine::general_purpose::STANDARD;
    let mut mac =
        Hmac::<Sha256>::new_from_slice(session_key).expect("HMAC takes a key of any length");
    mac.update(credentials.device_group_key.as_bytes());
    mac.update(credentials.device_key.as_bytes());
    mac.update(
        &standard
            .decode(secret_block)
            .expect("the secret block is base64"),
    );
    mac.update(timestamp.as_bytes());
    standard.encode(mac.finalize().into_bytes())
}

/// The prime of the group this exchange runs over, and the generator Amazon
/// Cognito pairs with it.
fn prime() -> &'static BigUint {
    &srp::groups::G_3072.n
}

fn generator() -> BigUint {
    BigUint::from(2_u32)
}

/// `k = H(PAD(N) | PAD(g))`, the multiplier parameter of SRP-6a.
fn multiplier() -> BigUint {
    BigUint::from_bytes_be(&hash(&[&padded(prime()), &padded(&generator())]))
}

/// The SHA-256 digest of `parts` concatenated.
fn hash(parts: &[&[u8]]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part);
    }
    hasher.finalize().into()
}

/// `value` in the big-endian bytes this exchange hashes and reports it as: its
/// shortest representation, with a leading zero byte where the first byte's top
/// bit is set.
fn padded(value: &BigUint) -> Vec<u8> {
    let bytes = value.to_bytes_be();
    if bytes.first().is_some_and(|first| first & 0x80 != 0) {
        let mut padded = vec![0_u8];
        padded.extend_from_slice(&bytes);
        return padded;
    }
    bytes
}

/// `bytes` bytes derived from `label`: the suite's salts and ephemerals, so each
/// row runs its own exchange and two runs of the suite run the same ones. Nothing
/// a device's own secret has to be unguessable for depends on this — it stands in
/// for the randomness a device's client would draw.
fn pseudo_random(label: &str, bytes: usize) -> Vec<u8> {
    let mut drawn = Vec::with_capacity(bytes);
    let mut block = hash(&[label.as_bytes()]);
    while drawn.len() < bytes {
        drawn.extend_from_slice(&block);
        block = hash(&[&block]);
    }
    drawn.truncate(bytes);
    drawn
}
