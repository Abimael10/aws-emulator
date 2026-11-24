// SPDX-License-Identifier: CC0-1.0

//! Answering an outstanding challenge after its `Session` window closed, driven
//! through the router exactly as an SDK drives it (`POST /` with
//! `X-Amz-Target: AWSCognitoIdentityProviderService.RespondToAuthChallenge`).
//!
//! The rule under test: **a challenge response whose `Session` is past the window
//! the app client's `AuthSessionValidity` gave it is refused ON THE SESSION —
//! `NotAuthorizedException`, HTTP 400, "Invalid session for the user, session is
//! expired." — before and independently of whatever the response carries, for every
//! challenge a session can be outstanding on, and with no `AuthenticationResult`.**
//! `AuthSessionValidity` is "the duration, in minutes, of that session token"
//! (`smithy_query(query="doc",shape="UserPoolClientType",member="AuthSessionValidity")`),
//! so what lapsed is the session rather than anything the caller presents, and the
//! refusal leaves the user signable-in: a challenge started afresh and answered
//! while its window is open is served.
//!
//! The rows differ along WHICH challenge the session is outstanding on — the email
//! one-time password a first factor and a second factor alike, a text-message code,
//! a software token (which delivers no code at all), the choice between two
//! activated factors, and the new password a temporary one owes — and, inside each
//! row, along what the response carries: the right answer, a value the challenge
//! would refuse on its own merits, and no answer entry at all.
//!
//! The window is minutes long, so a session's stored expiry instant is rewound
//! rather than waited out: nothing here is timed against the wall clock.

use std::time::{Duration, SystemTime};

use serde_json::{json, Map, Value};

use cognito_idp::state;
use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use crate::software_token_setup::{
    app_client, current_step, dispatch, emulator, error_code, error_message, ok, ok_self_service,
    one_time_password, pool, Emulator, GOOD_PASSWORD,
};

/// The user every row signs in as, and the address and number its message factors
/// are delivered to.
const USERNAME: &str = "ada";
const EMAIL_ADDRESS: &str = "ada@example.com";
const PHONE_NUMBER: &str = "+15555550123";

/// The user the `NEW_PASSWORD_REQUIRED` row signs in as: one an administrator
/// created with a temporary password, which is what leaves it owing the change.
const NEWCOMER: &str = "newcomer";

/// The temporary password that user is created with, and the permanent one its
/// challenge response chooses — both satisfying a default pool's policy.
const TEMPORARY_PASSWORD: &str = "Temp0rary!2024";
const CHOSEN_PASSWORD: &str = "Ch0senPassw0rd!";

/// A password a default pool's policy refuses (too short, no symbol), which is what
/// the `NEW_PASSWORD_REQUIRED` row presents as the answer the challenge would refuse
/// on its own merits.
const WEAK_PASSWORD: &str = "short";

/// A value that names no MFA factor, which is what the `SELECT_MFA_TYPE` row
/// presents as the answer that challenge would refuse on its own merits.
const NOT_A_FACTOR: &str = "NOT_A_FACTOR";

/// The status `NotAuthorizedException` carries on this operation
/// (`smithy_query(query="operation",operation="RespondToAuthChallenge")`).
const REFUSED: u16 = 400;

/// The sentence a `Session` past its validity window earns.
const SESSION_EXPIRED: &str = "Invalid session for the user, session is expired.";

/// The `AuthSessionValidity` the app client of every row is created with: 3 minutes,
/// the bottom of the modelled 3..15 range.
const SHORT_VALIDITY_MINUTES: i32 = 3;

/// A second app client's `AuthSessionValidity`: 15 minutes, the top of that range.
const LONG_VALIDITY_MINUTES: i32 = 15;

/// Which challenge one row's session is outstanding on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Challenge {
    /// The email one-time password chosen as the FIRST factor of a `USER_AUTH`
    /// sign-in.
    EmailOtpFirstFactor,
    /// The same factor asked for as the user's activated MFA factor.
    EmailOtpSecondFactor,
    /// The text-message code of a sign-in whose MFA factor is the phone number.
    SmsMfa,
    /// The software token's one-time password, which nothing delivers.
    SoftwareTokenMfa,
    /// The choice between two activated factors, answered by naming one.
    SelectMfaType,
    /// The new password a temporary one owes.
    NewPasswordRequired,
}

/// Every challenge a `RespondToAuthChallenge` session can be outstanding on in a
/// pool with no custom-authentication trigger and no confirmed device.
const CHALLENGES: &[Challenge] = &[
    Challenge::EmailOtpFirstFactor,
    Challenge::EmailOtpSecondFactor,
    Challenge::SmsMfa,
    Challenge::SoftwareTokenMfa,
    Challenge::SelectMfaType,
    Challenge::NewPasswordRequired,
];

impl Challenge {
    /// The `ChallengeNameType` the sign-in reports this challenge under
    /// (`smithy_query(query="shape",shape="ChallengeNameType")`).
    fn name(self) -> &'static str {
        match self {
            Self::EmailOtpFirstFactor | Self::EmailOtpSecondFactor => "EMAIL_OTP",
            Self::SmsMfa => "SMS_MFA",
            Self::SoftwareTokenMfa => "SOFTWARE_TOKEN_MFA",
            Self::SelectMfaType => "SELECT_MFA_TYPE",
            Self::NewPasswordRequired => "NEW_PASSWORD_REQUIRED",
        }
    }

    /// The `ChallengeResponses` entry the answer travels in
    /// (`smithy_query(query="doc",shape="RespondToAuthChallengeRequest",member="ChallengeResponses")`).
    fn answer_entry(self) -> &'static str {
        match self {
            Self::EmailOtpFirstFactor | Self::EmailOtpSecondFactor => "EMAIL_OTP_CODE",
            Self::SmsMfa => "SMS_MFA_CODE",
            Self::SoftwareTokenMfa => "SOFTWARE_TOKEN_MFA_CODE",
            Self::SelectMfaType => "ANSWER",
            Self::NewPasswordRequired => "NEW_PASSWORD",
        }
    }

    /// The MFA preference the user needs for this challenge to be the one a
    /// password sign-in owes, if any.
    fn preference(self) -> Option<Value> {
        match self {
            Self::EmailOtpFirstFactor | Self::NewPasswordRequired => None,
            Self::EmailOtpSecondFactor => {
                Some(json!({ "EmailMfaSettings": { "Enabled": true, "PreferredMfa": true } }))
            }
            Self::SmsMfa => {
                Some(json!({ "SMSMfaSettings": { "Enabled": true, "PreferredMfa": true } }))
            }
            Self::SoftwareTokenMfa => Some(
                json!({ "SoftwareTokenMfaSettings": { "Enabled": true, "PreferredMfa": true } }),
            ),
            // Two factors activated and NEITHER preferred, so the sign-in asks
            // which to present.
            Self::SelectMfaType => Some(json!({
                "SoftwareTokenMfaSettings": { "Enabled": true, "PreferredMfa": false },
                "EmailMfaSettings": { "Enabled": true, "PreferredMfa": false },
            })),
        }
    }

    /// Whether this row needs the user to hold a REGISTERED software token.
    fn needs_software_token(self) -> bool {
        matches!(self, Self::SoftwareTokenMfa | Self::SelectMfaType)
    }

    /// The user whose sign-in owes this challenge.
    fn username(self) -> &'static str {
        match self {
            Self::NewPasswordRequired => NEWCOMER,
            _ => USERNAME,
        }
    }

    /// Whether answering it COMPLETES the sign-in: the choice between factors is
    /// answered with the chosen factor's own challenge instead.
    fn completes(self) -> bool {
        self != Self::SelectMfaType
    }
}

/// What one attempt presents as its answer.
#[derive(Clone, Copy, Debug)]
enum Answer {
    /// The answer the challenge is waiting for.
    Right,
    /// An answer of the same shape the challenge would refuse on its own merits.
    Refusable,
    /// No answer entry at all.
    Absent,
}

/// A pool with one app client and the users every row signs in as.
struct Fixture {
    user_pool_id: String,
    client_id: String,
    /// The key the association minted, which the authenticator this suite stands
    /// in for is provisioned with. Empty for a row that needs no software token.
    secret_code: String,
}

/// Build the fixture for `challenge` in a pool named `name`.
async fn fixture(emulator: &Emulator, name: &str, challenge: Challenge) -> Fixture {
    let user_pool_id = pool(
        emulator,
        name,
        json!({
            // The email one-time password is offered as a FIRST factor, which is
            // what the `USER_AUTH` row chooses.
            "Policies": {
                "SignInPolicy": { "AllowedFirstAuthFactors": ["PASSWORD", "EMAIL_OTP"] }
            },
        }),
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
    let client_id = app_client(
        emulator,
        &user_pool_id,
        "window-client",
        json!({
            "ExplicitAuthFlows": [
                "ALLOW_USER_AUTH",
                "ALLOW_USER_PASSWORD_AUTH",
                "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                "ALLOW_REFRESH_TOKEN_AUTH",
            ],
            "AuthSessionValidity": SHORT_VALIDITY_MINUTES,
        }),
    )
    .await;
    ok(
        emulator,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "MessageAction": "SUPPRESS",
            "UserAttributes": [
                { "Name": "email", "Value": EMAIL_ADDRESS },
                { "Name": "email_verified", "Value": "true" },
                { "Name": "phone_number", "Value": PHONE_NUMBER },
                { "Name": "phone_number_verified", "Value": "true" },
            ],
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
    // The user that still owes its password change, for the row whose challenge is
    // that change.
    ok(
        emulator,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": NEWCOMER,
            "MessageAction": "SUPPRESS",
            "TemporaryPassword": TEMPORARY_PASSWORD,
            "UserAttributes": [
                { "Name": "email", "Value": "newcomer@example.com" },
                { "Name": "email_verified", "Value": "true" },
            ],
        }),
    )
    .await;

    let mut secret_code = String::new();
    if challenge.needs_software_token() {
        // The first sign-in completes, since no factor is activated yet, and its
        // access token is what registers the authenticator.
        let signed_in = ok_self_service(
            emulator,
            "InitiateAuth",
            &json!({
                "ClientId": client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
            }),
        )
        .await;
        let access_token = signed_in["AuthenticationResult"]["AccessToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("a sign-in with no factor activated issues an access token: {signed_in}")
            })
            .to_string();
        let associated = ok_self_service(
            emulator,
            "AssociateSoftwareToken",
            &json!({ "AccessToken": access_token }),
        )
        .await;
        secret_code = associated["SecretCode"]
            .as_str()
            .unwrap_or_else(|| panic!("the association reports a key: {associated}"))
            .to_string();
        // The registration presents the PREVIOUS time step's password, which the
        // resynchronisation window accepts and which leaves the password each
        // attempt below presents unspent.
        ok_self_service(
            emulator,
            "VerifySoftwareToken",
            &json!({
                "AccessToken": access_token,
                "UserCode": one_time_password(
                    &secret_code,
                    current_step(SystemTime::now()).saturating_sub(1),
                ),
            }),
        )
        .await;
    }

    let fixture = Fixture {
        user_pool_id,
        client_id,
        secret_code,
    };
    if let Some(settings) = challenge.preference() {
        let mut body = Map::new();
        body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
        body.insert("Username".to_string(), json!(USERNAME));
        if let Some(members) = settings.as_object() {
            for (name, value) in members {
                body.insert(name.clone(), value.clone());
            }
        }
        ok(emulator, "AdminSetUserMFAPreference", &Value::Object(body)).await;
    }
    fixture
}

/// Start `challenge` through `client_id` and return the session the sign-in left
/// outstanding.
async fn start(emulator: &Emulator, client_id: &str, case: &str, challenge: Challenge) -> String {
    let body = match challenge {
        Challenge::EmailOtpFirstFactor => json!({
            "ClientId": client_id,
            "AuthFlow": "USER_AUTH",
            "AuthParameters": { "USERNAME": USERNAME, "PREFERRED_CHALLENGE": "EMAIL_OTP" },
        }),
        Challenge::NewPasswordRequired => json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": NEWCOMER, "PASSWORD": TEMPORARY_PASSWORD },
        }),
        _ => json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
        }),
    };
    let started = ok_self_service(emulator, "InitiateAuth", &body).await;
    assert_eq!(
        started["ChallengeName"].as_str(),
        Some(challenge.name()),
        "case {case:?}: expected the sign-in to owe {}, got {started}",
        challenge.name()
    );
    started["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a challenge carries a session, got {started}"))
        .to_string()
}

/// The answer the outstanding `challenge` is waiting for: the one-time password the
/// authenticator shows, the code the challenge DELIVERED (read out of this
/// service's own state, since no response carries it), the name of a factor to
/// present, or the password the user chooses.
fn right_answer(
    emulator: &Emulator,
    fixture: &Fixture,
    challenge: Challenge,
    session: &str,
) -> String {
    match challenge {
        Challenge::SoftwareTokenMfa => {
            one_time_password(&fixture.secret_code, current_step(SystemTime::now()))
        }
        Challenge::SelectMfaType => "SOFTWARE_TOKEN_MFA".to_string(),
        Challenge::NewPasswordRequired => CHOSEN_PASSWORD.to_string(),
        _ => {
            let service_state =
                state::service_state(&*emulator.engine).expect("this service's state");
            let guard = service_state.lock();
            guard
                .auth_sessions
                .get(session)
                .and_then(|stored| stored.code.as_ref())
                .map(|code| code.code.clone())
                .unwrap_or_else(|| {
                    panic!("a {} challenge delivers a code", challenge.name())
                })
        }
    }
}

/// An answer of the same shape the challenge would refuse on its own merits: a code
/// with every digit shifted, a value naming no factor, or a password the policy
/// rejects.
fn refusable_answer(challenge: Challenge, right: &str) -> String {
    match challenge {
        Challenge::SelectMfaType => NOT_A_FACTOR.to_string(),
        Challenge::NewPasswordRequired => WEAK_PASSWORD.to_string(),
        _ => right
            .chars()
            .map(|digit| {
                char::from_digit((digit.to_digit(10).unwrap_or(0) + 1) % 10, 10).unwrap_or('0')
            })
            .collect(),
    }
}

/// The `RespondToAuthChallenge` body one attempt sends.
fn respond_body(
    fixture: &Fixture,
    challenge: Challenge,
    session: &str,
    answer: Option<&str>,
) -> Value {
    let mut responses = Map::new();
    responses.insert("USERNAME".to_string(), json!(challenge.username()));
    if let Some(answer) = answer {
        responses.insert(challenge.answer_entry().to_string(), json!(answer));
    }
    json!({
        "ClientId": fixture.client_id,
        "ChallengeName": challenge.name(),
        "Session": session,
        "ChallengeResponses": Value::Object(responses),
    })
}

/// Rewind the stored expiry instant of `session` so its window closed a minute ago:
/// `AuthSessionValidity` counts in minutes, and waiting one out would tie the suite
/// to the wall clock.
fn close_window(emulator: &Emulator, session: &str) {
    let service_state = state::service_state(&*emulator.engine).expect("this service's state");
    let mut guard = service_state.lock();
    guard
        .auth_sessions
        .get_mut(session)
        .expect("the challenge session is stored")
        .expires_at = SystemTime::now() - Duration::from_secs(60);
}

/// The stored expiry instant of `session`.
fn stored_expiry(emulator: &Emulator, session: &str) -> SystemTime {
    let service_state = state::service_state(&*emulator.engine).expect("this service's state");
    let guard = service_state.lock();
    guard
        .auth_sessions
        .get(session)
        .expect("the challenge session is stored")
        .expires_at
}

#[tokio::test]
async fn an_answer_past_the_session_window_is_refused_on_the_session() {
    for (index, challenge) in CHALLENGES.iter().copied().enumerate() {
        let emulator = emulator();
        let fixture = fixture(&emulator, &format!("window-pool-{index}"), challenge).await;

        for answer_form in [Answer::Right, Answer::Refusable, Answer::Absent] {
            let case = format!(
                "{} answered with {answer_form:?} past its window",
                challenge.name()
            );
            // A session the clock has expired is SWEPT by the call that reads it,
            // so every attempt is handed a challenge of its own.
            let session = start(&emulator, &fixture.client_id, &case, challenge).await;
            let right = right_answer(&emulator, &fixture, challenge, &session);
            let presented = match answer_form {
                Answer::Right => Some(right.clone()),
                Answer::Refusable => Some(refusable_answer(challenge, &right)),
                Answer::Absent => None,
            };
            close_window(&emulator, &session);

            let (status, payload) = dispatch(
                &emulator,
                "RespondToAuthChallenge",
                &respond_body(&fixture, challenge, &session, presented.as_deref()),
                false,
            )
            .await;
            assert_eq!(
                (status, error_code(&payload)),
                (REFUSED, "NotAuthorizedException"),
                "case {case:?}: expected a {REFUSED} NotAuthorizedException, got {status} with \
                 {payload}"
            );
            assert_eq!(
                error_message(&payload),
                SESSION_EXPIRED,
                "case {case:?}: expected the session-expired sentence, got {:?}",
                error_message(&payload)
            );
            assert!(
                payload["AuthenticationResult"].is_null(),
                "case {case:?}: a refusal carries no AuthenticationResult, got {payload}"
            );
        }

        // The clock is the only thing that refused those attempts: the same
        // challenge, started afresh and answered while its window is open, is
        // served — so nothing about the user was locked by the expiries.
        let case = format!("{} answered inside its window", challenge.name());
        let session = start(&emulator, &fixture.client_id, &case, challenge).await;
        let right = right_answer(&emulator, &fixture, challenge, &session);
        let (status, payload) = dispatch(
            &emulator,
            "RespondToAuthChallenge",
            &respond_body(&fixture, challenge, &session, Some(&right)),
            false,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an answer inside the window must be served, got {status} with \
             {payload}"
        );
        assert_eq!(
            payload["AuthenticationResult"]["AccessToken"]
                .as_str()
                .is_some_and(|token| !token.is_empty()),
            challenge.completes(),
            "case {case:?}: expected tokens={}, got {payload}",
            challenge.completes()
        );
    }
}

#[tokio::test]
async fn the_window_a_challenge_session_carries_is_the_app_clients_own() {
    // The window is the app client's `AuthSessionValidity`, so a session started
    // through the client that asks for the longer one is stored with the later
    // expiry — compared against each other rather than against the wall clock.
    let emulator = emulator();
    let fixture = fixture(
        &emulator,
        "window-length-pool",
        Challenge::EmailOtpFirstFactor,
    )
    .await;
    let patient_client = app_client(
        &emulator,
        &fixture.user_pool_id,
        "patient-client",
        json!({
            "ExplicitAuthFlows": ["ALLOW_USER_AUTH", "ALLOW_USER_PASSWORD_AUTH"],
            "AuthSessionValidity": LONG_VALIDITY_MINUTES,
        }),
    )
    .await;

    let short_case = format!("a client whose window is {SHORT_VALIDITY_MINUTES} minutes");
    let short = start(
        &emulator,
        &fixture.client_id,
        &short_case,
        Challenge::EmailOtpFirstFactor,
    )
    .await;
    let long_case = format!("a client whose window is {LONG_VALIDITY_MINUTES} minutes");
    let long = start(
        &emulator,
        &patient_client,
        &long_case,
        Challenge::EmailOtpFirstFactor,
    )
    .await;

    let short_expiry = stored_expiry(&emulator, &short);
    let long_expiry = stored_expiry(&emulator, &long);
    assert!(
        long_expiry > short_expiry,
        "case {long_case:?}: expected a later expiry than {short_case:?}, got \
         {long_expiry:?} against {short_expiry:?}"
    );

    // The longer window is still open where the shorter one has been closed, so the
    // refusal follows the SESSION's own window and not the pool or the user.
    close_window(&emulator, &short);
    for (case, session, expected_status) in [
        (&short_case, &short, REFUSED),
        (&long_case, &long, 200_u16),
    ] {
        let right = right_answer(
            &emulator,
            &fixture,
            Challenge::EmailOtpFirstFactor,
            session,
        );
        let mut body = respond_body(
            &fixture,
            Challenge::EmailOtpFirstFactor,
            session,
            Some(&right),
        );
        if session == &long {
            body["ClientId"] = json!(patient_client);
        }
        let (status, payload) = dispatch(&emulator, "RespondToAuthChallenge", &body, false).await;
        assert_eq!(
            status, expected_status,
            "case {case:?}: expected HTTP {expected_status}, got {status} with {payload}"
        );
        if expected_status == REFUSED {
            assert_eq!(
                (error_code(&payload), error_message(&payload)),
                ("NotAuthorizedException", SESSION_EXPIRED),
                "case {case:?}: expected the session-expired refusal, got {payload}"
            );
        }
    }
}
