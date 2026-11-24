// SPDX-License-Identifier: CC0-1.0

//! A presented `DEVICE_KEY` has to name a device the pool holds, over the wire
//! (`POST /` with `X-Amz-Target: AWSCognitoIdentityProviderService.InitiateAuth` /
//! `.AdminInitiateAuth` / `.RespondToAuthChallenge` /
//! `.AdminRespondToAuthChallenge`).
//!
//! The rule under test: **on a user pool that has activated device remembering,
//! a sign-in or a challenge response that presents a `DEVICE_KEY` naming no
//! device that pool has registered to the signing-in user is refused
//! `ResourceNotFoundException` with the sentence every device lookup of this API
//! answers, and nothing is written for it — no device is registered and no token
//! is issued.** A presented key names the device the caller signs in from:
//! `AuthParameters` is where a caller "add[s] `DEVICE_KEY` if you want to bypass
//! multi-factor authentication with a remembered device"
//! (`smithy_query(query="doc",shape="InitiateAuthRequest",member="AuthParameters")`)
//! and `ChallengeResponses` is where it "include[s] a `DEVICE_KEY` for device
//! authentication"; both sign-in operations declare
//! `ResourceNotFoundException`
//! (`smithy_query(query="operation",operation="InitiateAuth,AdminInitiateAuth")`).
//! Device remembering is activated by the PRESENCE of a `DeviceConfiguration`
//! ("When you provide a value for any property of `DeviceConfiguration`, you
//! activate the device remembering for the user pool"), so a pool that carries
//! none registers no devices and a key presented there names none to look up.
//!
//! The rows differ along WHAT the presented key names — the device this user's
//! own sign-in registered, that device once it is confirmed, a device another
//! user of the same pool holds, a well-formed key nothing ever minted, the key
//! of a device a forget has removed, and no key at all — along which pair of
//! operations presents it, and along whether the pool activated device
//! remembering at all.
//!
//! Everything is real: the pools, app clients, users, devices and challenge
//! sessions all come from this service's own operations.

use serde_json::{json, Map, Value};

use cognito_idp::state;
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use super::software_token_setup::{
    app_client, dispatch, emulator, error_code, error_message, ok, ok_self_service, pool, Emulator,
    GOOD_PASSWORD,
};

/// The user every row signs in, and the other user of the same pool whose own
/// device key one row presents.
const USERNAME: &str = "ada";
const OTHER_USERNAME: &str = "grace";

/// A well-formed `DeviceKey` (`DeviceKeyType`, pattern `^[\w-]+_[0-9a-f-]+$`)
/// that no sign-in of this service ever minted.
const UNMINTED_DEVICE_KEY: &str = "us-east-1_00000000-0000-0000-0000-000000000000";

/// Every refusal of this service is answered 400 and names its class in the body
/// ([`super::error_wire`] states and asserts that).
const REFUSED: u16 = 400;

/// The class and the entirely fixed sentence a key that names no device earns:
/// nothing of the request is interpolated, so a key another user holds reads
/// exactly like one that was never minted.
const RESOURCE_NOT_FOUND: &str = "ResourceNotFoundException";
const DEVICE_NOT_FOUND_MESSAGE: &str = "Device does not exist.";

/// The device credentials a confirm registers: "a Secure Remote Password (SRP)
/// value that your application generates when you register a user's device"
/// (`smithy_query(query="doc",shape="ConfirmDeviceRequest",member="DeviceSecretVerifierConfig")`).
fn device_credentials() -> Value {
    json!({
        "PasswordVerifier": "ZGV2aWNlLXBhc3N3b3JkLXZlcmlmaWVy",
        "Salt": "ZGV2aWNlLXNhbHQ",
    })
}

/// Which pair of operations a row drives.
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

    fn signed(self) -> bool {
        self == Self::Admin
    }

    fn auth_flow(self) -> &'static str {
        match self {
            Self::Client => "USER_PASSWORD_AUTH",
            Self::Admin => "ADMIN_USER_PASSWORD_AUTH",
        }
    }
}

/// A created pool and one of its app clients.
struct Fixture {
    user_pool_id: String,
    client_id: String,
}

/// Create a pool with `device_configuration` merged in (when given) and one app
/// client that allows the password flows.
async fn fixture(
    emulator: &Emulator,
    name: &str,
    device_configuration: Option<Value>,
) -> Fixture {
    let extra = match device_configuration {
        Some(configuration) => json!({ "DeviceConfiguration": configuration }),
        None => json!({}),
    };
    let user_pool_id = pool(emulator, name, extra).await;
    let client_id = app_client(emulator, &user_pool_id, "device-key-client", json!({})).await;
    Fixture {
        user_pool_id,
        client_id,
    }
}

/// Create `username` in `user_pool_id` with a permanent password, so it is
/// confirmed and can sign in.
async fn confirmed_user(emulator: &Emulator, user_pool_id: &str, username: &str) {
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
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;
}

/// Start a password sign-in of `username` through `driver`, presenting
/// `device_key` when one is given, and return the status and body.
async fn sign_in(
    emulator: &Emulator,
    driver: Driver,
    fixture: &Fixture,
    username: &str,
    device_key: Option<&str>,
) -> (u16, Value) {
    let mut parameters = Map::new();
    parameters.insert("USERNAME".to_string(), json!(username));
    parameters.insert("PASSWORD".to_string(), json!(GOOD_PASSWORD));
    if let Some(device_key) = device_key {
        parameters.insert("DEVICE_KEY".to_string(), json!(device_key));
    }
    let mut body = Map::new();
    if driver == Driver::Admin {
        body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
    }
    body.insert("ClientId".to_string(), json!(fixture.client_id));
    body.insert("AuthFlow".to_string(), json!(driver.auth_flow()));
    body.insert("AuthParameters".to_string(), Value::Object(parameters));
    dispatch(
        emulator,
        driver.initiate(),
        &Value::Object(body),
        driver.signed(),
    )
    .await
}

/// Sign `username` in with no device key, requiring success, and return the
/// access token and the key of the device the pool registered for it.
async fn registered_device(
    emulator: &Emulator,
    fixture: &Fixture,
    username: &str,
) -> (String, String) {
    let (status, payload) = sign_in(emulator, Driver::Client, fixture, username, None).await;
    assert_eq!(
        status, 200,
        "signing {username} in must be served, got {status} with {payload}"
    );
    let result = &payload["AuthenticationResult"];
    (
        result["AccessToken"]
            .as_str()
            .unwrap_or_else(|| panic!("a completed sign-in issues an access token: {payload}"))
            .to_string(),
        result["NewDeviceMetadata"]["DeviceKey"]
            .as_str()
            .unwrap_or_else(|| panic!("a pool that remembers devices registers one: {payload}"))
            .to_string(),
    )
}

/// What one row presents as its `DEVICE_KEY`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented {
    /// No key at all.
    Nothing,
    /// The key the user's own sign-in registered, which nothing confirmed.
    OwnRegistered,
    /// That same device, once `ConfirmDevice` has confirmed it.
    OwnConfirmed,
    /// The key of a device the same pool registered to ANOTHER user.
    AnotherUsers,
    /// A well-formed key no sign-in ever minted.
    NeverMinted,
    /// The key of the user's own device, after a forget removed it.
    Forgotten,
}

/// One row of the presented-key table.
struct Row {
    case: &'static str,
    presented: Presented,
    /// Whether the sign-in presenting it must be refused as a device that does
    /// not exist.
    refused: bool,
}

/// The rows span what a presented key can name: a device the pool holds for this
/// user (registered or confirmed) and every key that names none of them.
const ROWS: &[Row] = &[
    Row {
        case: "no DEVICE_KEY at all",
        presented: Presented::Nothing,
        refused: false,
    },
    Row {
        case: "the key of this user's own registered device",
        presented: Presented::OwnRegistered,
        refused: false,
    },
    Row {
        case: "the key of this user's own confirmed device",
        presented: Presented::OwnConfirmed,
        refused: false,
    },
    Row {
        case: "the key of a device another user of the pool holds",
        presented: Presented::AnotherUsers,
        refused: true,
    },
    Row {
        case: "a well-formed key no sign-in ever minted",
        presented: Presented::NeverMinted,
        refused: true,
    },
    Row {
        case: "the key of a device a forget removed",
        presented: Presented::Forgotten,
        refused: true,
    },
];

/// Through both pairs of operations, a sign-in presenting a key that names a
/// device the pool holds for that user is served, and one presenting a key that
/// names no such device is refused as a device that does not exist — and the
/// refusal registers nothing, so the devices the user holds are exactly the ones
/// it held before.
#[tokio::test]
async fn a_sign_in_presenting_an_unknown_device_key_is_refused() {
    for driver in [Driver::Client, Driver::Admin] {
        for (index, row) in ROWS.iter().enumerate() {
            let emulator = emulator();
            let case = format!("{} through {}", row.case, driver.initiate());
            let fixture = fixture(
                &emulator,
                &format!("device-key-pool-{index}-{}", driver.initiate()),
                Some(json!({})),
            )
            .await;
            confirmed_user(&emulator, &fixture.user_pool_id, USERNAME).await;
            confirmed_user(&emulator, &fixture.user_pool_id, OTHER_USERNAME).await;

            let (access_token, own) = registered_device(&emulator, &fixture, USERNAME).await;
            let presented = match row.presented {
                Presented::Nothing => None,
                Presented::OwnRegistered => Some(own.clone()),
                Presented::OwnConfirmed => {
                    ok_self_service(
                        &emulator,
                        "ConfirmDevice",
                        &json!({
                            "AccessToken": access_token,
                            "DeviceKey": own,
                            "DeviceSecretVerifierConfig": device_credentials(),
                        }),
                    )
                    .await;
                    Some(own.clone())
                }
                Presented::AnotherUsers => {
                    let (_, theirs) =
                        registered_device(&emulator, &fixture, OTHER_USERNAME).await;
                    assert_ne!(
                        theirs, own,
                        "case {case:?}: two users must hold different device keys"
                    );
                    Some(theirs)
                }
                Presented::NeverMinted => Some(UNMINTED_DEVICE_KEY.to_string()),
                Presented::Forgotten => {
                    ok(
                        &emulator,
                        "AdminForgetDevice",
                        &json!({
                            "UserPoolId": fixture.user_pool_id,
                            "Username": USERNAME,
                            "DeviceKey": own,
                        }),
                    )
                    .await;
                    Some(own.clone())
                }
            };

            let before = registered_device_keys(&emulator, &fixture.user_pool_id, USERNAME);
            let (status, payload) =
                sign_in(&emulator, driver, &fixture, USERNAME, presented.as_deref()).await;
            if row.refused {
                assert_eq!(
                    (status, error_code(&payload), error_message(&payload)),
                    (REFUSED, RESOURCE_NOT_FOUND, DEVICE_NOT_FOUND_MESSAGE),
                    "case {case:?}: a key naming no device of this user is refused, got \
                     {status} with {payload}"
                );
                assert!(
                    payload.get("AuthenticationResult").is_none()
                        && payload.get("ChallengeName").is_none(),
                    "case {case:?}: a refused sign-in answers neither tokens nor a challenge, \
                     got {payload}"
                );
                let after = registered_device_keys(&emulator, &fixture.user_pool_id, USERNAME);
                assert_eq!(
                    after, before,
                    "case {case:?}: a refused sign-in registers no device, got {after:?}"
                );
                continue;
            }
            assert_eq!(
                status, 200,
                "case {case:?}: the sign-in must be served, got {status} with {payload}"
            );
            assert!(
                payload["AuthenticationResult"]["AccessToken"].is_string(),
                "case {case:?}: the sign-in must complete, got {payload}"
            );
            // A served sign-in that presented a key the pool holds registers no
            // SECOND device for it: the key named a device it already had.
            let after = registered_device_keys(&emulator, &fixture.user_pool_id, USERNAME);
            let expected = match row.presented {
                // A sign-in that presents no key at all comes from a device the
                // pool does not recognise, so it registers one.
                Presented::Nothing => before.len() + 1,
                _ => before.len(),
            };
            assert_eq!(
                after.len(),
                expected,
                "case {case:?}: expected {expected} stored device(s), got {after:?}"
            );
            assert!(
                !access_token.is_empty(),
                "case {case:?}: the setup's sign-in issued a token"
            );
        }
    }
}

/// The keys of the devices this pool holds for `username`, read off the stored
/// state: a device a sign-in registered is stored whether or not a confirm has
/// confirmed it, and only the admin listing serves the confirmed ones, so the
/// store is where "the pool registered no device for this call" is observable.
fn registered_device_keys(
    emulator: &Emulator,
    user_pool_id: &str,
    username: &str,
) -> Vec<String> {
    let service_state = state::service_state(&*emulator.engine).expect("this service's state");
    let guard = service_state.lock();
    let pool = guard
        .pool(user_pool_id)
        .unwrap_or_else(|| panic!("the pool {user_pool_id} this suite created"));
    let user = pool
        .user(username)
        .unwrap_or_else(|| panic!("the user {username} this suite created"));
    user.devices.keys().cloned().collect()
}

/// A pool that carries NO `DeviceConfiguration` remembers no devices, so it
/// registers none and a presented key names no device to look up: the sign-in is
/// served and answers no device metadata, whatever key it carried.
#[tokio::test]
async fn a_pool_that_remembers_no_devices_judges_no_device_key() {
    let emulator = emulator();
    let fixture = fixture(&emulator, "no-device-config-pool", None).await;
    confirmed_user(&emulator, &fixture.user_pool_id, USERNAME).await;

    for (case, presented) in [
        ("no DEVICE_KEY at all", None),
        ("a key no sign-in ever minted", Some(UNMINTED_DEVICE_KEY)),
    ] {
        let (status, payload) =
            sign_in(&emulator, Driver::Client, &fixture, USERNAME, presented).await;
        assert_eq!(
            status, 200,
            "case {case:?}: a pool that remembers no devices serves the sign-in, got {status} \
             with {payload}"
        );
        assert!(
            payload["AuthenticationResult"]
                .get("NewDeviceMetadata")
                .is_none(),
            "case {case:?}: a pool that remembers no devices reports no device, got {payload}"
        );
    }
}

/// A challenge RESPONSE that presents a key naming no device of its session's
/// user is refused the same way, through both pairs of operations: the response
/// answers for a device that does not exist, so the challenge is left standing
/// and no token is issued.
#[tokio::test]
async fn a_challenge_response_presenting_an_unknown_device_key_is_refused() {
    for driver in [Driver::Client, Driver::Admin] {
        let emulator = emulator();
        let case = format!("a stale DEVICE_KEY in a response through {}", driver.initiate());
        let fixture = fixture(
            &emulator,
            &format!("response-device-pool-{}", driver.initiate()),
            Some(json!({})),
        )
        .await;
        // A user created with a TEMPORARY password owes the password challenge,
        // which is the challenge this response answers.
        ok(
            &emulator,
            "AdminCreateUser",
            &json!({
                "UserPoolId": fixture.user_pool_id,
                "Username": USERNAME,
                "TemporaryPassword": GOOD_PASSWORD,
                "MessageAction": "SUPPRESS",
            }),
        )
        .await;
        let (status, started) =
            sign_in(&emulator, driver, &fixture, USERNAME, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the sign-in must be served, got {status} with {started}"
        );
        let challenge_name = started["ChallengeName"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: a temporary password owes a challenge, got {started}"))
            .to_string();
        let session = started["Session"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the challenge carries a session, got {started}"))
            .to_string();

        let mut responses = Map::new();
        responses.insert("USERNAME".to_string(), json!(USERNAME));
        responses.insert("NEW_PASSWORD".to_string(), json!("An0ther-Passw0rd!"));
        responses.insert("DEVICE_KEY".to_string(), json!(UNMINTED_DEVICE_KEY));
        let mut body = Map::new();
        if driver == Driver::Admin {
            body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
        }
        body.insert("ClientId".to_string(), json!(fixture.client_id));
        body.insert("ChallengeName".to_string(), json!(challenge_name));
        body.insert("Session".to_string(), json!(session));
        body.insert("ChallengeResponses".to_string(), Value::Object(responses));
        let operation = match driver {
            Driver::Client => "RespondToAuthChallenge",
            Driver::Admin => "AdminRespondToAuthChallenge",
        };
        let (status, payload) =
            dispatch(&emulator, operation, &Value::Object(body), driver.signed()).await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (REFUSED, RESOURCE_NOT_FOUND, DEVICE_NOT_FOUND_MESSAGE),
            "case {case:?}: a response naming no device of this user is refused, got {status} \
             with {payload}"
        );
        assert!(
            payload.get("AuthenticationResult").is_none(),
            "case {case:?}: a refused response issues no tokens, got {payload}"
        );
    }
}
