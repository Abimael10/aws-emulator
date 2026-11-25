// SPDX-License-Identifier: CC0-1.0

//! One time step's software-token password is good for ONE use, over the wire
//! (`POST /` with `X-Amz-Target: AWSCognitoIdentityProviderService.InitiateAuth` /
//! `.AdminInitiateAuth` / `.RespondToAuthChallenge` /
//! `.AdminRespondToAuthChallenge` / `.VerifySoftwareToken`).
//!
//! The rule under test: **a one-time password of a user's software token is
//! spent by the SIGN-IN it completes, so presenting a code from a time step a
//! challenge response has already signed this user in with is refused
//! `ExpiredCodeException` with the fixed sentence that code carries, and the
//! refusal does NOT spend the challenge session: the same session completes as
//! soon as an unspent code is presented. Registering the authenticator spends
//! no code: `VerifySoftwareToken` proves the authenticator holds the key rather
//! than authenticating a sign-in, so the code that registered it still
//! completes a sign-in.** `ExpiredCodeException`
//! "is thrown if a code has expired" and is declared by both challenge-response
//! operations
//! (`smithy_query(query="operation",operation="AdminRespondToAuthChallenge")`)
//! and by neither `VerifySoftwareToken`
//! (`smithy_query(query="operation",operation="VerifySoftwareToken")`), and a
//! step's password authenticating once is RFC 6238 §5.2's rule for a TOTP
//! verifier ("the verifier MUST NOT accept the second attempt of the OTP
//! generated for the same time step"), the same rule the ±1-step
//! resynchronisation window comes from.
//!
//! The rows differ along which pair of operations presents the replay (the
//! client-side pair and the administrative pair) and along the time step the
//! presented code belongs to (the step a sign-in spent, a step older than it,
//! and the step after it, which is unspent).
//!
//! The one-time passwords are computed here from the key the association
//! answered with — the arithmetic an authenticator app does — so nothing but the
//! service decides whether a code is accepted. Everything else is real: the
//! pool, its app client, the user, the enrolment and the challenge sessions all
//! come from this service's own operations.

use std::time::SystemTime;

use serde_json::{json, Map, Value};

use super::software_token_setup::{
    app_client, current_step, dispatch, emulator, error_code, error_message, ok, ok_self_service,
    one_time_password, pool, Emulator, GOOD_PASSWORD,
};

/// The user every row enrols and signs in.
const USERNAME: &str = "ada";

/// The challenge a user with an activated software token is answered with
/// (`ChallengeNameType`).
const SOFTWARE_TOKEN_MFA: &str = "SOFTWARE_TOKEN_MFA";

/// The `ChallengeResponses` entry the software token's code travels in:
/// `{"SOFTWARE_TOKEN_MFA_CODE": [authenticator_code]}`
/// (`smithy_query(query="doc",shape="AdminRespondToAuthChallengeRequest",member="ChallengeResponses")`).
const CODE_ENTRY: &str = "SOFTWARE_TOKEN_MFA_CODE";

/// Every refusal of this service is answered 400 and names its class in the body
/// ([`super::error_wire`] states and asserts that), so a row asserting a refusal
/// asserts that status with the code and the sentence.
const REFUSED: u16 = 400;

/// The error class a code that is used up earns, and the entirely fixed sentence
/// it carries: nothing of the request or of the stored user is interpolated.
const EXPIRED_CODE: &str = "ExpiredCodeException";
const CODE_ALREADY_USED_MESSAGE: &str = "Your software token has already been used once.";

/// The error class a code the authenticator never emitted earns, which is a
/// DIFFERENT answer from a code that is used up.
const CODE_MISMATCH: &str = "CodeMismatchException";

/// A pool with TOTP MFA offered, one app client, and one user that has enrolled
/// a software token and prefers it.
struct Enrolled {
    user_pool_id: String,
    client_id: String,
    /// The key the association answered with, which the codes are computed from.
    secret_code: String,
    /// The time step the enrolment's own code belongs to, which that call leaves
    /// unspent.
    enrolment_step: u64,
}

/// Which pair of operations a row drives: the client-side pair, authorized by the
/// app client alone, and the administrative pair, authorized by IAM credentials.
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

    /// The operation that ANSWERS the outstanding challenge.
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

    /// The `AuthFlowType` this pair's password sign-in asks for.
    fn auth_flow(self) -> &'static str {
        match self {
            Self::Client => "USER_PASSWORD_AUTH",
            Self::Admin => "ADMIN_USER_PASSWORD_AUTH",
        }
    }

    /// The request body of a password sign-in through this pair.
    fn sign_in_body(self, fixture: &Enrolled) -> Value {
        let mut body = Map::new();
        if self == Self::Admin {
            body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
        }
        body.insert("ClientId".to_string(), json!(fixture.client_id));
        body.insert("AuthFlow".to_string(), json!(self.auth_flow()));
        body.insert(
            "AuthParameters".to_string(),
            json!({ "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD }),
        );
        Value::Object(body)
    }

    /// The request body of a challenge response through this pair, presenting
    /// `code` for the outstanding software-token challenge.
    fn respond_body(self, fixture: &Enrolled, session: &str, code: &str) -> Value {
        let mut body = Map::new();
        if self == Self::Admin {
            body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
        }
        body.insert("ClientId".to_string(), json!(fixture.client_id));
        body.insert("ChallengeName".to_string(), json!(SOFTWARE_TOKEN_MFA));
        body.insert("Session".to_string(), json!(session));
        body.insert(
            "ChallengeResponses".to_string(),
            json!({ "USERNAME": USERNAME, CODE_ENTRY: code }),
        );
        Value::Object(body)
    }
}

/// Build [`Enrolled`] in a pool named `name`: the user enrols a software token
/// with the code of the time step the enrolment runs in — which that call does
/// NOT spend — and makes the factor its preferred one.
async fn enrolled(emulator: &Emulator, name: &str) -> Enrolled {
    let user_pool_id = pool(emulator, name, json!({})).await;
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
    let client_id = app_client(emulator, &user_pool_id, "single-use-client", json!({})).await;
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
            "Password": GOOD_PASSWORD,
            "Permanent": true,
        }),
    )
    .await;

    // The first sign-in completes: no factor is activated yet, which is what
    // lets the user enrol one with the access token it issues.
    let fixture = Enrolled {
        user_pool_id,
        client_id,
        secret_code: String::new(),
        enrolment_step: 0,
    };
    let signed_in = ok_self_service(
        emulator,
        "InitiateAuth",
        &Driver::Client.sign_in_body(&fixture),
    )
    .await;
    let access_token = signed_in["AuthenticationResult"]["AccessToken"]
        .as_str()
        .unwrap_or_else(|| panic!("a sign-in with no factor activated issues a token: {signed_in}"))
        .to_string();
    let associated = ok_self_service(
        emulator,
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
        emulator,
        "VerifySoftwareToken",
        &json!({
            "AccessToken": access_token,
            "UserCode": one_time_password(&secret_code, enrolment_step),
        }),
    )
    .await;
    ok(
        emulator,
        "AdminSetUserMFAPreference",
        &json!({
            "UserPoolId": fixture.user_pool_id,
            "Username": USERNAME,
            "SoftwareTokenMfaSettings": { "Enabled": true, "PreferredMfa": true },
        }),
    )
    .await;
    Enrolled {
        secret_code,
        enrolment_step,
        ..fixture
    }
}

/// Start a sign-in through `driver` and return the session of the software-token
/// challenge it is answered with.
async fn challenge(emulator: &Emulator, driver: Driver, fixture: &Enrolled, case: &str) -> String {
    let (status, payload) = dispatch(
        emulator,
        driver.initiate(),
        &driver.sign_in_body(fixture),
        driver.signed(),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: {} must be served, got {status} with {payload}",
        driver.initiate()
    );
    assert_eq!(
        payload["ChallengeName"].as_str(),
        Some(SOFTWARE_TOKEN_MFA),
        "case {case:?}: the activated factor must be challenged, got {payload}"
    );
    payload["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: an outstanding challenge carries a session, got {payload}"))
        .to_string()
}

/// Answer the challenge `session` through `driver` with `code`.
async fn respond(
    emulator: &Emulator,
    driver: Driver,
    fixture: &Enrolled,
    session: &str,
    code: &str,
) -> (u16, Value) {
    dispatch(
        emulator,
        driver.respond(),
        &driver.respond_body(fixture, session, code),
        driver.signed(),
    )
    .await
}

/// One row of the replay table: which pair of operations drives the sign-in that
/// spends a code and the replay of that code.
struct ReplayRow {
    case: &'static str,
    driver: Driver,
}

/// The rows differ along which pair of operations the sign-in that spends the
/// code, and the replay of it, are presented through.
const REPLAY_ROWS: &[ReplayRow] = &[
    ReplayRow {
        case: "the code a completed sign-in spent, replayed through the client-side pair",
        driver: Driver::Client,
    },
    ReplayRow {
        case: "the code a completed sign-in spent, replayed through the administrative pair",
        driver: Driver::Admin,
    },
];

/// A code a call of this service has already accepted is refused
/// `ExpiredCodeException` the next time it is presented, no tokens are issued for
/// it, and the challenge session it was presented to is still outstanding: the
/// next unspent code completes that very session.
#[tokio::test]
async fn a_spent_software_token_code_is_refused_and_leaves_the_session_live() {
    for (index, row) in REPLAY_ROWS.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let fixture = enrolled(&emulator, &format!("single-use-pool-{index}")).await;

        // The step whose password is used up by the time the row replays it: the
        // one a completed sign-in presented, which is the enrolment's own step
        // since the registration spent nothing.
        let spent_step = {
            let session = challenge(&emulator, row.driver, &fixture, case).await;
            let step = fixture.enrolment_step;
            let (status, payload) = respond(
                &emulator,
                row.driver,
                &fixture,
                &session,
                &one_time_password(&fixture.secret_code, step),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: an unspent code must complete the sign-in, got {status} with \
                 {payload}"
            );
            step
        };

        let session = challenge(&emulator, row.driver, &fixture, case).await;
        let (status, payload) = respond(
            &emulator,
            row.driver,
            &fixture,
            &session,
            &one_time_password(&fixture.secret_code, spent_step),
        )
        .await;
        assert_eq!(
            (status, error_code(&payload), error_message(&payload)),
            (REFUSED, EXPIRED_CODE, CODE_ALREADY_USED_MESSAGE),
            "case {case:?}: a code that is used up is refused, got {status} with {payload}"
        );
        assert!(
            payload.get("AuthenticationResult").is_none(),
            "case {case:?}: a refused response issues no tokens, got {payload}"
        );

        // The refusal did not spend the session: the step after the spent one is
        // unspent, and presenting it completes THAT session.
        let (status, completed) = respond(
            &emulator,
            row.driver,
            &fixture,
            &session,
            &one_time_password(&fixture.secret_code, spent_step + 1),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the session must still be answerable with an unspent code, got \
             {status} with {completed}"
        );
        assert!(
            completed["AuthenticationResult"]["AccessToken"].is_string(),
            "case {case:?}: the completed sign-in issues tokens, got {completed}"
        );
    }
}

/// A step OLDER than the one a sign-in spent is used up as well: a password from
/// before the last one accepted is at least as used up as that one, so it is
/// refused with the same sentence.
#[tokio::test]
async fn a_step_older_than_the_spent_one_is_used_up_too() {
    let emulator = emulator();
    let case = "a code from before the step the sign-in spent";
    let fixture = enrolled(&emulator, "older-step-pool").await;

    // The sign-in presents the step AFTER the enrolment's, so the step the
    // enrolment ran in is older than the one this sign-in spends.
    let session = challenge(&emulator, Driver::Client, &fixture, case).await;
    let (status, payload) = respond(
        &emulator,
        Driver::Client,
        &fixture,
        &session,
        &one_time_password(&fixture.secret_code, fixture.enrolment_step + 1),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: an unspent code must complete the sign-in, got {status} with {payload}"
    );

    let session = challenge(&emulator, Driver::Client, &fixture, case).await;
    let (status, payload) = respond(
        &emulator,
        Driver::Client,
        &fixture,
        &session,
        &one_time_password(&fixture.secret_code, fixture.enrolment_step),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (REFUSED, EXPIRED_CODE, CODE_ALREADY_USED_MESSAGE),
        "case {case:?}: an older step's password is used up too, got {status} with {payload}"
    );
}

/// A code that is used up is refused DIFFERENTLY from a code the authenticator
/// never emitted: the first is `ExpiredCodeException` and the second
/// `CodeMismatchException`, so a client can tell "that code was already used"
/// from "that is not your code".
#[tokio::test]
async fn a_used_code_and_a_wrong_code_name_different_errors() {
    let emulator = emulator();
    let case = "a used code beside a code no authenticator emitted";
    let fixture = enrolled(&emulator, "single-use-classes-pool").await;

    // A completed sign-in is what spends a step's password, so one is completed
    // first and the code it presented is the used-up one below.
    let session = challenge(&emulator, Driver::Client, &fixture, case).await;
    let (status, payload) = respond(
        &emulator,
        Driver::Client,
        &fixture,
        &session,
        &one_time_password(&fixture.secret_code, fixture.enrolment_step),
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: an unspent code must complete the sign-in, got {status} with {payload}"
    );

    let session = challenge(&emulator, Driver::Client, &fixture, case).await;
    // Six digits of a code this key emits in no step of the window.
    let (status, payload) = respond(&emulator, Driver::Client, &fixture, &session, "000000").await;
    assert_eq!(
        (status, error_code(&payload)),
        (REFUSED, CODE_MISMATCH),
        "case {case:?}: a code the authenticator never emitted is a mismatch, got {status} with \
         {payload}"
    );

    let (status, payload) = respond(
        &emulator,
        Driver::Client,
        &fixture,
        &session,
        &one_time_password(&fixture.secret_code, fixture.enrolment_step),
    )
    .await;
    assert_eq!(
        (status, error_code(&payload), error_message(&payload)),
        (REFUSED, EXPIRED_CODE, CODE_ALREADY_USED_MESSAGE),
        "case {case:?}: the code the earlier sign-in spent is used up, got {status} with {payload}"
    );
}

/// Registering the authenticator spends NO code: `VerifySoftwareToken` proves
/// the authenticator holds the key rather than authenticating a sign-in, and it
/// declares no expired-code error at all
/// (`smithy_query(query="operation",operation="VerifySoftwareToken")`), so the
/// very code the enrolment verified still completes the sign-in the setup was
/// asked for — through either pair of operations.
#[tokio::test]
async fn the_code_a_registration_verified_still_signs_the_user_in() {
    // The rows differ along which pair of operations presents the code.
    for driver in [Driver::Client, Driver::Admin] {
        let emulator = emulator();
        let case = format!("the enrolment's own code presented to {}", driver.respond());
        let fixture = enrolled(
            &emulator,
            &format!("registration-spends-nothing-{}", driver.respond()),
        )
        .await;

        let session = challenge(&emulator, driver, &fixture, &case).await;
        let (status, payload) = respond(
            &emulator,
            driver,
            &fixture,
            &session,
            &one_time_password(&fixture.secret_code, fixture.enrolment_step),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the code the registration verified must still sign the user in, got \
             {status} with {payload}"
        );
        assert!(
            payload["AuthenticationResult"]["AccessToken"].is_string(),
            "case {case:?}: the completed sign-in issues tokens, got {payload}"
        );
    }
}

/// The spend belongs to the KEY, not to the user: a fresh
/// `AssociateSoftwareToken` mints a new private key, so the step the previous
/// key spent is unspent on the new one and a code of that step registers the new
/// authenticator.
#[tokio::test]
async fn a_new_association_spends_nothing_of_its_own() {
    let emulator = emulator();
    let case = "a re-association after the enrolment spent a step";
    let fixture = enrolled(&emulator, "re-association-pool").await;

    let signed_in = {
        let session = challenge(&emulator, Driver::Client, &fixture, case).await;
        let (status, payload) = respond(
            &emulator,
            Driver::Client,
            &fixture,
            &session,
            &one_time_password(&fixture.secret_code, fixture.enrolment_step + 1),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: an unspent code must complete the sign-in, got {status} with {payload}"
        );
        payload
    };
    let access_token = signed_in["AuthenticationResult"]["AccessToken"]
        .as_str()
        .expect("a completed sign-in issues an access token")
        .to_string();

    // The same time step the first key spent, presented for the NEW key.
    let associated = ok_self_service(
        &emulator,
        "AssociateSoftwareToken",
        &json!({ "AccessToken": access_token }),
    )
    .await;
    let new_key = associated["SecretCode"]
        .as_str()
        .expect("the association reports a key")
        .to_string();
    assert_ne!(
        new_key, fixture.secret_code,
        "case {case:?}: a fresh association mints its own key"
    );
    let verified = ok_self_service(
        &emulator,
        "VerifySoftwareToken",
        &json!({
            "AccessToken": access_token,
            "UserCode": one_time_password(&new_key, fixture.enrolment_step + 1),
        }),
    )
    .await;
    assert_eq!(
        verified["Status"].as_str(),
        Some("SUCCESS"),
        "case {case:?}: the new key's own code registers it, got {verified}"
    );
}
