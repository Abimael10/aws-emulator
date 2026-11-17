// SPDX-License-Identifier: CC0-1.0

//! Which session a software-token verification takes, over the wire, driven
//! exactly as an SDK drives it (`POST /` with `X-Amz-Target:
//! AWSCognitoIdentityProviderService.InitiateAuth` /
//! `.AssociateSoftwareToken` / `.VerifySoftwareToken`).
//!
//! The rule under test: **a `Session` whose model entry names the call it comes
//! from resolves only a session THAT call handed out — `VerifySoftwareToken`
//! takes "the session ID from an `AssociateSoftwareToken` request" (this
//! service's model), so a live session another call of the same sign-in handed
//! out (the sign-in's own challenge session, or the session a verification
//! answered with, which "satisfies an `MFA_SETUP` challenge" and is supplied in
//! a challenge response) is no session of this request and is refused as an
//! invalid session, before any one-time password is looked at.**
//!
//! `AssociateSoftwareToken` takes "the session ID from a successful sign-in",
//! which every session of that sign-in's exchange is, so the rows that begin a
//! setup present the challenge session the sign-in answered with.
//!
//! Every row presents a code the key of the setup actually emits where a key
//! exists, computed from RFC 6238's definition over the key the association
//! reported (the arithmetic the authenticator app does) — so a row that is
//! refused is refused for the session it carries and for nothing else. What each
//! row leaves behind is read back through the administrative surface:
//! `AdminSetUserMFAPreference` can activate `SOFTWARE_TOKEN_MFA` only for a user
//! that has registered a TOTP authenticator, and `AdminGetUser` reports the
//! activation.
//!
//! The pools, app clients, users and sessions are all real: they come from
//! `CreateUserPool`, `SetUserPoolMfaConfig`, `CreateUserPoolClient`,
//! `AdminCreateUser`, `AdminSetUserPassword` and `InitiateAuth`.
//!
//! Each table row names the case it asserts and every assertion reports the
//! actual value.

use std::time::SystemTime;

use serde_json::{json, Value};

use super::software_token_setup::{
    app_client, current_step, dispatch, emulator, error_code, error_message, ok, ok_self_service,
    one_time_password, pool, Emulator, GOOD_PASSWORD,
};

/// The user every row signs in as.
const USERNAME: &str = "ada";

/// The `ChallengeNameType` a sign-in that must set a factor up is answered
/// with.
const MFA_SETUP: &str = "MFA_SETUP";

/// The `UserMFASettingList` value TOTP MFA is reported under (this service's
/// model).
const SOFTWARE_TOKEN_MFA: &str = "SOFTWARE_TOKEN_MFA";

/// The status this endpoint answers every refusal with.
const REFUSED: u16 = 400;

/// The sentence a session that is no session of this request is refused with.
const INVALID_SESSION: &str = "Invalid session for the user.";

/// Which call handed out the session the verification presents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Presented {
    /// The session `AssociateSoftwareToken` handed out: the one this
    /// operation's `Session` names.
    Association,
    /// The sign-in's own challenge session, on a sign-in no association has been
    /// made for yet.
    SignInWithoutAssociation,
    /// The sign-in's own challenge session, on a sign-in whose setup an
    /// association DID begin — the key exists, but the sign-in is not the call
    /// that handed the setup's session out.
    SignInAfterAssociation,
    /// The session a previous `VerifySoftwareToken` handed out, which satisfies
    /// an `MFA_SETUP` challenge rather than carrying another verification.
    Verification,
}

/// One row of the presented-session table.
struct SessionCase {
    case: &'static str,
    presented: Presented,
    /// Whether the verification is served, and so whether the authenticator the
    /// row's session names is registered by it.
    served: bool,
}

/// The rows differ along WHICH call of the same sign-in handed the presented
/// session out, including one that carries a usable key and one that follows a
/// verification this service already served.
const SESSION_CASES: &[SessionCase] = &[
    SessionCase {
        case: "the session the association handed out",
        presented: Presented::Association,
        served: true,
    },
    SessionCase {
        case: "the sign-in's challenge session, before any association",
        presented: Presented::SignInWithoutAssociation,
        served: false,
    },
    SessionCase {
        case: "the sign-in's challenge session, after an association began the setup",
        presented: Presented::SignInAfterAssociation,
        served: false,
    },
    SessionCase {
        case: "the session a verification of this setup handed out",
        presented: Presented::Verification,
        served: false,
    },
];

/// A pool that requires MFA with TOTP offered, one app client and one user whose
/// sign-in is waiting on the `MFA_SETUP` challenge.
struct Challenged {
    user_pool_id: String,
    /// The session the sign-in answered the challenge with.
    session: String,
}

/// Sign the row's user in up to the `MFA_SETUP` challenge, in a pool named
/// `name`.
async fn challenged(emulator: &Emulator, name: &str) -> Challenged {
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
    let client_id = app_client(emulator, &user_pool_id, "setup-client", json!({})).await;
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
    let challenge = ok_self_service(
        emulator,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_PASSWORD_AUTH",
            "AuthParameters": { "USERNAME": USERNAME, "PASSWORD": GOOD_PASSWORD },
        }),
    )
    .await;
    assert_eq!(
        challenge["ChallengeName"].as_str(),
        Some(MFA_SETUP),
        "a user of a pool that requires MFA with no factor set up owes the {MFA_SETUP} \
         challenge, got {challenge}"
    );
    let session = challenge["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("the {MFA_SETUP} challenge carries a session: {challenge}"))
        .to_string();
    Challenged {
        user_pool_id,
        session,
    }
}

/// Begin the setup on `session`, requiring success, and return the key and the
/// session the association handed out.
async fn associate(emulator: &Emulator, case: &str, session: &str) -> (String, String) {
    let payload = ok_self_service(
        emulator,
        "AssociateSoftwareToken",
        &json!({ "Session": session }),
    )
    .await;
    let secret_code = payload["SecretCode"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: the association reports a key, got {payload}"))
        .to_string();
    let association_session = payload["Session"]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: a call the session flow authorized answers a session: {payload}")
        })
        .to_string();
    (secret_code, association_session)
}

/// Present `user_code` for the setup `session` names, and return the status and
/// body of the answer.
async fn verify(emulator: &Emulator, session: &str, user_code: &str) -> (u16, Value) {
    dispatch(
        emulator,
        "VerifySoftwareToken",
        &json!({ "Session": session, "UserCode": user_code }),
        false,
    )
    .await
}

/// Whether the user of `user_pool_id` has a TOTP authenticator registered: only
/// a registered one can be activated as the preferred factor (a user "must
/// register a TOTP authenticator before they set this as their preferred MFA
/// method" — `SetUserMFAPreference.SoftwareTokenMfaSettings` in this service's
/// model), and the activation is read back through `AdminGetUser`.
async fn registered(emulator: &Emulator, case: &str, user_pool_id: &str) -> bool {
    let (status, payload) = dispatch(
        emulator,
        "AdminSetUserMFAPreference",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": USERNAME,
            "SoftwareTokenMfaSettings": { "Enabled": true, "PreferredMfa": true },
        }),
        true,
    )
    .await;
    assert!(
        status == 200 || status == REFUSED,
        "case {case:?}: activating TOTP MFA answers 200 for a registered authenticator and \
         {REFUSED} for none, got {status} with {payload}"
    );
    if status != 200 {
        return false;
    }
    let read = ok(
        emulator,
        "AdminGetUser",
        &json!({ "UserPoolId": user_pool_id, "Username": USERNAME }),
    )
    .await;
    let settings: Vec<String> = read["UserMFASettingList"]
        .as_array()
        .map(|list| {
            list.iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    assert_eq!(
        settings,
        vec![SOFTWARE_TOKEN_MFA.to_string()],
        "case {case:?}: an activated factor is reported as [{SOFTWARE_TOKEN_MFA}], got \
         {settings:?}"
    );
    true
}

#[tokio::test]
async fn a_verification_takes_only_the_session_an_association_handed_out() {
    for (index, row) in SESSION_CASES.iter().enumerate() {
        let emulator = emulator();
        let case = row.case;
        let fixture = challenged(&emulator, &format!("session-origin-pool-{index}")).await;

        // Which session the row presents, and a code the key of the setup it
        // belongs to emits — so nothing but the session's own origin can be the
        // reason the row is refused. A row that never began a setup has no key
        // to read a code off, and never gets as far as one being checked.
        let (presented, code) = match row.presented {
            Presented::Association => {
                let (secret_code, association_session) =
                    associate(&emulator, case, &fixture.session).await;
                let code = one_time_password(&secret_code, current_step(SystemTime::now()));
                (association_session, code)
            }
            Presented::SignInWithoutAssociation => {
                (fixture.session.clone(), "000000".to_string())
            }
            Presented::SignInAfterAssociation => {
                let (secret_code, _) = associate(&emulator, case, &fixture.session).await;
                let code = one_time_password(&secret_code, current_step(SystemTime::now()));
                (fixture.session.clone(), code)
            }
            Presented::Verification => {
                let (secret_code, association_session) =
                    associate(&emulator, case, &fixture.session).await;
                let code = one_time_password(&secret_code, current_step(SystemTime::now()));
                let (status, payload) = verify(&emulator, &association_session, &code).await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: the verification this row builds on must be served, got \
                     {status} with {payload}"
                );
                let setup_session = payload["Session"]
                    .as_str()
                    .unwrap_or_else(|| {
                        panic!(
                            "case {case:?}: a served verification of the session flow answers a \
                             session, got {payload}"
                        )
                    })
                    .to_string();
                let code = one_time_password(&secret_code, current_step(SystemTime::now()));
                (setup_session, code)
            }
        };

        let (status, payload) = verify(&emulator, &presented, &code).await;
        if row.served {
            assert_eq!(
                status, 200,
                "case {case:?}: expected the verification to be served, got {status} with \
                 {payload}"
            );
            assert_eq!(
                payload["Status"].as_str(),
                Some("SUCCESS"),
                "case {case:?}: expected SUCCESS, got {payload}"
            );
        } else {
            assert_eq!(
                status, REFUSED,
                "case {case:?}: expected the verification to answer {REFUSED}, got {status} with \
                 {payload}"
            );
            assert_eq!(
                error_code(&payload),
                "NotAuthorizedException",
                "case {case:?}: expected NotAuthorizedException, got {payload}"
            );
            assert_eq!(
                error_message(&payload),
                INVALID_SESSION,
                "case {case:?}: expected {INVALID_SESSION:?}, got {:?}",
                error_message(&payload)
            );
        }

        // What the row left behind: only a served verification registers the
        // authenticator, and the row that follows one leaves the registration
        // that verification already wrote.
        let expected = row.served || row.presented == Presented::Verification;
        let registered = registered(&emulator, case, &fixture.user_pool_id).await;
        assert_eq!(
            registered, expected,
            "case {case:?}: expected the authenticator registered = {expected}, got \
             {registered}"
        );
    }
}

#[tokio::test]
async fn a_refused_session_leaves_the_setup_answerable_on_the_right_one() {
    let emulator = emulator();
    let case = "a verification refused for its session, then one on the association's";
    let fixture = challenged(&emulator, "session-origin-retry-pool").await;
    let (secret_code, association_session) = associate(&emulator, case, &fixture.session).await;

    // The sign-in's own session is refused, and the refusal consumes nothing:
    // the session the association handed out still carries the setup.
    let code = one_time_password(&secret_code, current_step(SystemTime::now()));
    let (status, payload) = verify(&emulator, &fixture.session, &code).await;
    assert_eq!(
        status, REFUSED,
        "case {case:?}: expected the sign-in's session to be refused, got {status} with {payload}"
    );
    assert_eq!(
        error_message(&payload),
        INVALID_SESSION,
        "case {case:?}: expected {INVALID_SESSION:?}, got {:?}",
        error_message(&payload)
    );

    let code = one_time_password(&secret_code, current_step(SystemTime::now()));
    let (status, payload) = verify(&emulator, &association_session, &code).await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected the association's session to still carry the setup, got {status} \
         with {payload}"
    );
    assert_eq!(
        payload["Status"].as_str(),
        Some("SUCCESS"),
        "case {case:?}: expected SUCCESS, got {payload}"
    );
    let registered = registered(&emulator, case, &fixture.user_pool_id).await;
    assert!(
        registered,
        "case {case:?}: expected the authenticator registered after the served verification, got \
         {registered}"
    );
}
