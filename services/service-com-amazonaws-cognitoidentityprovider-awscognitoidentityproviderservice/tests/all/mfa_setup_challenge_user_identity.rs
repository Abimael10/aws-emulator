// SPDX-License-Identifier: CC0-1.0

//! Which user the `MFA_SETUP` challenge of a password sign-in NAMES, and the form the
//! refresh token of the sign-in that completes after the enrolment reaches the caller in.
//!
//! The rules under test:
//!
//! 1. A password sign-in that proved the user's first factor against a pool that REQUIRES
//!    a second factor the user has activated none of is answered with
//!    `ChallengeName = MFA_SETUP`, a `Session` inside the modelled `SessionType` bounds
//!    (length 20..4096), and `ChallengeParameters` carrying `MFAS_CAN_SETUP` — the methods
//!    the pool offers to set up, as a JSON array of `UserMFASettingList` values — beside
//!    `USER_ID_FOR_SRP`; and it mints no `AuthenticationResult`, so nothing signs the user
//!    in until that challenge is answered.
//! 2. `USER_ID_FOR_SRP` names the user the pool RESOLVED, not the name the request
//!    presented: Amazon Cognito "returns the username attribute in USER_ID_FOR_SRP instead
//!    of any email address, preferred username, or phone number alias that you might have
//!    specified" in the request (`ChallengeParameters` in
//!    `smithy_query(query="operation",operation="AdminInitiateAuth")`).
//! 3. The refresh token a sign-in vends once that factor is set up is a JOSE value in
//!    COMPACT serialization — `.`-separated base64url segments whose first decodes to a
//!    JOSE header — which is the form the `.` of the modelled `TokenModelType` pattern
//!    `^[A-Za-z0-9-_=.]+$` admits
//!    (`smithy_query(query="shape",shape="TokenModelType,AuthenticationResultType")`), and
//!    not one undelimited run of characters.
//!
//! The rows span what rule 2 generalises over: every NAME a pool resolves one of its users
//! by — the stored username, that username in another capitalization on a pool that
//! compares usernames without case, an `email` alias, that alias in another
//! capitalization, and a `preferred_username` alias — each driven through BOTH halves of
//! the sign-in surface (`InitiateAuth` and `AdminInitiateAuth`), which are separate
//! response serializers.
//!
//! The harness — the emulator, the one call, the pool, the app client and the arithmetic of
//! a one-time password — is the one `crate::software_token_setup` owns, so this suite drives
//! the same endpoint as its siblings rather than a second copy of it.
//!
//! Each row names the case it asserts and every assertion reports the value it saw.

use std::time::SystemTime;

use serde_json::{json, Map, Value};

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice::state;

use crate::software_token_setup::{
    app_client, current_step, dispatch, emulator, ok, ok_self_service, one_time_password, pool,
    Emulator, GOOD_PASSWORD,
};

/// The `ChallengeNameType` value a user that must REGISTER a factor before it can sign in
/// is answered with.
const MFA_SETUP: &str = "MFA_SETUP";

/// The `ChallengeNameType` value the sign-in of a user holding a registered authenticator
/// presents that factor through.
const SOFTWARE_TOKEN_MFA_CHALLENGE: &str = "SOFTWARE_TOKEN_MFA";

/// The `UserMFASettingList` value TOTP MFA is reported under: "the possible values in this
/// list are `SMS_MFA`, `EMAIL_OTP`, and `SOFTWARE_TOKEN_MFA`" (this service's model).
const SOFTWARE_TOKEN_MFA: &str = "SOFTWARE_TOKEN_MFA";

/// The `ChallengeParameters` entry naming the user a challenge is outstanding for.
const USER_ID_FOR_SRP: &str = "USER_ID_FOR_SRP";

/// The `ChallengeParameters` entry of an `MFA_SETUP` challenge listing the methods the user
/// may set up.
const MFAS_CAN_SETUP: &str = "MFAS_CAN_SETUP";

/// The modelled bounds of a `Session` (`SessionType`: length 20..4096).
const SESSION_LENGTH: (usize, usize) = (20, 4096);

/// How many `.`-separated segments a JOSE value in compact serialization carries at least:
/// the header, and at least one segment after it.
const MINIMUM_COMPACT_SEGMENTS: usize = 2;

/// The alias attributes the pools of this suite configure, so a sign-in can name its user
/// by one of their values.
const ALIAS_ATTRIBUTES: [&str; 2] = ["email", "preferred_username"];

/// A created pool that requires a second factor, with one app client of it.
struct Fixture {
    user_pool_id: String,
    client_id: String,
}

/// A pool named `name` that REQUIRES MFA and offers the software token as the method to
/// set one up with, aliasing [`ALIAS_ATTRIBUTES`] and comparing usernames as
/// `case_sensitive` says, with one app client allowing the password flows.
async fn fixture(emulator: &Emulator, name: &str, case_sensitive: bool) -> Fixture {
    let user_pool_id = pool(
        emulator,
        name,
        json!({
            "AliasAttributes": ALIAS_ATTRIBUTES,
            "UsernameConfiguration": { "CaseSensitive": case_sensitive },
        }),
    )
    .await;
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
    Fixture {
        user_pool_id,
        client_id,
    }
}

/// Create `username` holding `attributes` with a permanent password, so it is confirmed and
/// its password sign-in reaches the second factor.
async fn user(
    emulator: &Emulator,
    user_pool_id: &str,
    username: &str,
    attributes: &[(&str, &str)],
) {
    let list: Vec<Value> = attributes
        .iter()
        .map(|(name, value)| json!({ "Name": name, "Value": value }))
        .collect();
    ok(
        emulator,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "UserAttributes": list,
            "MessageAction": "SUPPRESS",
        }),
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

/// Which half of the sign-in surface a row drives: the self-service one a user's own client
/// calls, and the administrative one an SDK signs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Half {
    SelfService,
    Admin,
}

impl Half {
    /// The operation that STARTS the sign-in.
    fn initiate(self) -> &'static str {
        match self {
            Self::SelfService => "InitiateAuth",
            Self::Admin => "AdminInitiateAuth",
        }
    }

    /// The operation that ANSWERS an outstanding challenge.
    fn respond(self) -> &'static str {
        match self {
            Self::SelfService => "RespondToAuthChallenge",
            Self::Admin => "AdminRespondToAuthChallenge",
        }
    }

    /// Whether this half is addressed with a credential scope, as an SDK sends every
    /// administrative operation.
    fn signed(self) -> bool {
        self == Self::Admin
    }

    /// The `AuthFlowType` value this half's password sign-in asks for.
    fn auth_flow(self) -> &'static str {
        match self {
            Self::SelfService => "USER_PASSWORD_AUTH",
            Self::Admin => "ADMIN_USER_PASSWORD_AUTH",
        }
    }

    /// The request body of a password sign-in naming the user `presented`.
    fn sign_in_body(self, fixture: &Fixture, presented: &str) -> Value {
        let mut body = Map::new();
        if self == Self::Admin {
            body.insert("UserPoolId".to_string(), json!(fixture.user_pool_id));
        }
        body.insert("ClientId".to_string(), json!(fixture.client_id));
        body.insert("AuthFlow".to_string(), json!(self.auth_flow()));
        body.insert(
            "AuthParameters".to_string(),
            json!({ "USERNAME": presented, "PASSWORD": GOOD_PASSWORD }),
        );
        Value::Object(body)
    }

    /// The request body of a challenge response carrying `session` and naming the user
    /// `username`.
    fn respond_body(
        self,
        fixture: &Fixture,
        challenge_name: &str,
        session: &str,
        responses: Value,
    ) -> Value {
        let mut challenge_responses = Map::new();
        if let Some(members) = responses.as_object() {
            for (name, value) in members {
                challenge_responses.insert(name.clone(), value.clone());
            }
        }
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

/// The `Session` a challenged answer carries, required to be inside the modelled bounds.
fn session_of(case: &str, payload: &Value) -> String {
    let session = payload["Session"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: a challenge carries a session, got {payload}"));
    let (low, high) = SESSION_LENGTH;
    assert!(
        (low..=high).contains(&session.chars().count()),
        "case {case:?}: expected a session inside the modelled length {SESSION_LENGTH:?}, got {} \
         characters",
        session.chars().count()
    );
    session.to_string()
}

/// The `MFAS_CAN_SETUP` entry of an `MFA_SETUP` challenge, read as the JSON array of
/// `UserMFASettingList` values it carries as a string.
fn methods_to_set_up(case: &str, payload: &Value) -> Vec<String> {
    let listed = payload["ChallengeParameters"][MFAS_CAN_SETUP]
        .as_str()
        .unwrap_or_else(|| {
            panic!("case {case:?}: {MFAS_CAN_SETUP} is a JSON array in a string, got {payload}")
        });
    serde_json::from_str(listed).unwrap_or_else(|error| {
        panic!("case {case:?}: {MFAS_CAN_SETUP} must parse as JSON ({error}), got {listed:?}")
    })
}

/// Whether `value` satisfies the modelled `TokenModelType` pattern `^[A-Za-z0-9-_=.]+$`.
fn is_modelled_token(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '=' | '.'))
}

/// Assert that `token` is a JOSE value in compact serialization: non-empty base64url
/// segments joined by `.`, whose leading one decodes to a JOSE header.
fn assert_compact_jose_token(case: &str, token: &str) {
    assert!(
        is_modelled_token(token),
        "case {case:?}: a vended token satisfies TokenModelType, got {token:?}"
    );
    let segments: Vec<&str> = token.split('.').collect();
    assert!(
        segments.len() >= MINIMUM_COMPACT_SEGMENTS,
        "case {case:?}: expected at least {MINIMUM_COMPACT_SEGMENTS} `.`-separated segments, \
         got {} in {token:?}",
        segments.len()
    );
    for (at, segment) in segments.iter().enumerate() {
        assert!(
            !segment.is_empty()
                && segment
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_')),
            "case {case:?}: segment {at} is non-empty base64url, got {segment:?}"
        );
    }
    let header: Value = state::base64_url_decode(segments[0])
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the leading segment decodes to a JOSE header, got {token:?}")
        });
    assert!(
        header
            .get("alg")
            .and_then(Value::as_str)
            .is_some_and(|alg| !alg.is_empty()),
        "case {case:?}: a JOSE header names the algorithm its value is protected under, \
         got {header}"
    );
}

/// One row of the naming table: how the pool compares usernames, what the user holds, and
/// which of its names the sign-in presents.
struct NameCase {
    case: &'static str,
    /// `UsernameConfiguration.CaseSensitive` of the row's pool.
    case_sensitive: bool,
    /// The username the user is STORED under, which is what every challenge must name.
    stored: &'static str,
    /// The attributes the user holds, so the alias a row presents is one it owns.
    attributes: &'static [(&'static str, &'static str)],
    /// The name the sign-in request presents.
    presented: &'static str,
}

/// A verified email address, so the `email` alias is one a sign-in resolves. Its stored
/// spelling is mixed-case, which is what lets a row present it in another capitalization.
const EMAIL_HOLDER: &[(&str, &str)] = &[
    ("email", "Ada@Example.com"),
    ("email_verified", "true"),
];

/// A `preferred_username`, an alias attribute that carries no verified flag of its own.
const PREFERRED_USERNAME_HOLDER: &[(&str, &str)] = &[("preferred_username", "ada-the-alias")];

/// The rows differ along WHICH name the request presents (the stored username, the same
/// username in another capitalization, an `email` alias, that alias in another
/// capitalization, a `preferred_username` alias) and along whether the pool compares
/// usernames and its case-insensitive aliases without case.
const NAME_CASES: &[NameCase] = &[
    NameCase {
        case: "the stored username on a pool that compares usernames with case",
        case_sensitive: true,
        stored: "ada",
        attributes: &[],
        presented: "ada",
    },
    NameCase {
        case: "the stored username in another capitalization, compared without case",
        case_sensitive: false,
        stored: "ada",
        attributes: &[],
        presented: "ADA",
    },
    NameCase {
        case: "the user's email alias, as it is stored",
        case_sensitive: true,
        stored: "ada",
        attributes: EMAIL_HOLDER,
        presented: "Ada@Example.com",
    },
    NameCase {
        case: "the user's email alias in another capitalization, compared without case",
        case_sensitive: false,
        stored: "ada",
        attributes: EMAIL_HOLDER,
        presented: "ADA@EXAMPLE.COM",
    },
    NameCase {
        case: "the user's preferred_username alias",
        case_sensitive: true,
        stored: "ada",
        attributes: PREFERRED_USERNAME_HOLDER,
        presented: "ada-the-alias",
    },
];

/// RULES 1 and 2: a pool that requires a second factor answers the sign-in of a user
/// holding none with the `MFA_SETUP` challenge — a session, the methods to set up, and the
/// RESOLVED username — and mints no token for it, however the request named that user and
/// whichever half of the surface it came through.
#[tokio::test]
async fn the_setup_challenge_reports_the_resolved_user_and_no_token() {
    for (index, row) in NAME_CASES.iter().enumerate() {
        let emulator = emulator();
        let fixture = fixture(&emulator, &format!("mfa-setup-names-{index}"), row.case_sensitive)
            .await;
        user(&emulator, &fixture.user_pool_id, row.stored, row.attributes).await;

        for half in [Half::SelfService, Half::Admin] {
            let case = format!("{}, through {}", row.case, half.initiate());
            let (status, payload) = dispatch(
                &emulator,
                half.initiate(),
                &half.sign_in_body(&fixture, row.presented),
                half.signed(),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: the sign-in is served, got {status} with {payload}"
            );
            assert_eq!(
                payload["ChallengeName"],
                json!(MFA_SETUP),
                "case {case:?}: expected the {MFA_SETUP} challenge, got {payload}"
            );
            assert_eq!(
                payload["AuthenticationResult"],
                Value::Null,
                "case {case:?}: a challenged sign-in mints no token, got {payload}"
            );
            let _session = session_of(&case, &payload);
            assert_eq!(
                payload["ChallengeParameters"][USER_ID_FOR_SRP],
                json!(row.stored),
                "case {case:?}: expected the challenge to name the stored username \
                 {:?}, got {payload}",
                row.stored
            );
            assert_eq!(
                methods_to_set_up(&case, &payload),
                vec![SOFTWARE_TOKEN_MFA.to_string()],
                "case {case:?}: expected the pool's offered method, got {payload}"
            );
        }
    }
}

/// RULES 1, 2 and 3: the challenge is answerable — the session it carries enrols a factor,
/// the answer completes the sign-in and vends a refresh token in JOSE compact
/// serialization, and the next sign-in of that user PRESENTS the factor it set up (naming
/// the resolved username again) rather than asking it to set one up once more.
#[tokio::test]
async fn the_setup_challenge_enrols_a_factor_and_the_sign_in_vends_a_shaped_refresh_token() {
    // The rows differ along the half of the surface that answers the challenge, which is a
    // response serializer of its own, and along the name the sign-in presented.
    for (index, (half, presented)) in [
        (Half::SelfService, "Ada@Example.com"),
        (Half::Admin, "ada-the-alias"),
    ]
    .into_iter()
    .enumerate()
    {
        let emulator = emulator();
        let case = format!("enrolling as {presented:?} through {}", half.respond());
        let fixture = fixture(&emulator, &format!("mfa-setup-enrol-{index}"), true).await;
        let stored = "ada";
        let mut attributes: Vec<(&str, &str)> = EMAIL_HOLDER.to_vec();
        attributes.extend_from_slice(PREFERRED_USERNAME_HOLDER);
        user(&emulator, &fixture.user_pool_id, stored, &attributes).await;

        let (status, challenge) = dispatch(
            &emulator,
            half.initiate(),
            &half.sign_in_body(&fixture, presented),
            half.signed(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the sign-in is served, got {status} with {challenge}"
        );
        assert_eq!(
            challenge["ChallengeName"],
            json!(MFA_SETUP),
            "case {case:?}: expected the {MFA_SETUP} challenge, got {challenge}"
        );
        let challenge_session = session_of(&case, &challenge);

        // The session the challenge handed out is what carries the enrolment: the
        // association mints the key the authenticator is provisioned with, and the
        // verification hands on the session that satisfies the challenge.
        let associated = ok_self_service(
            &emulator,
            "AssociateSoftwareToken",
            &json!({ "Session": challenge_session }),
        )
        .await;
        let secret_code = associated["SecretCode"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: expected a key, got {associated}"))
            .to_string();
        let verified = ok_self_service(
            &emulator,
            "VerifySoftwareToken",
            &json!({
                "Session": associated["Session"],
                "UserCode": one_time_password(&secret_code, current_step(SystemTime::now())),
            }),
        )
        .await;
        let setup_session = session_of(&case, &verified);

        // The challenge is answered with the username and not the alias, as the modelled
        // `ChallengeResponses` requires.
        let (status, completed) = dispatch(
            &emulator,
            half.respond(),
            &half.respond_body(
                &fixture,
                MFA_SETUP,
                &setup_session,
                json!({ "USERNAME": stored }),
            ),
            half.signed(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the answer completes the sign-in, got {status} with {completed}"
        );
        let refresh_token = completed["AuthenticationResult"]["RefreshToken"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {case:?}: a completed sign-in vends a refresh token, got {completed}")
            })
            .to_string();
        assert_compact_jose_token(&case, &refresh_token);

        // ROUND TRIP: the factor is ACTIVATED for the stored user, which is what the pool's
        // own read reports.
        let read = ok(
            &emulator,
            "AdminGetUser",
            &json!({ "UserPoolId": fixture.user_pool_id, "Username": stored }),
        )
        .await;
        assert_eq!(
            read["UserMFASettingList"],
            json!([SOFTWARE_TOKEN_MFA]),
            "case {case:?}: the factor set up is activated for the user, got {read}"
        );

        // And the next sign-in of that user PRESENTS the factor instead of asking for a
        // setup, still naming the user as the pool resolved it.
        let (status, again) = dispatch(
            &emulator,
            half.initiate(),
            &half.sign_in_body(&fixture, presented),
            half.signed(),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second sign-in is served, got {status} with {again}"
        );
        assert_eq!(
            again["ChallengeName"],
            json!(SOFTWARE_TOKEN_MFA_CHALLENGE),
            "case {case:?}: expected the enrolled factor to be presented, got {again}"
        );
        assert_eq!(
            again["ChallengeParameters"][USER_ID_FOR_SRP],
            json!(stored),
            "case {case:?}: expected the challenge to name the stored username {stored:?}, \
             got {again}"
        );
    }
}
