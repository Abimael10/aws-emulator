// SPDX-License-Identifier: CC0-1.0

//! What this service SAYS when it refuses a request, for the refusals of four
//! rules whose wording a client matches on, each asserted over a table rather
//! than on one request.
//!
//! * RULE A — "only one factor can be set as preferred"
//!   (`AdminSetUserMFAPreference` and `SetUserMFAPreference` in this service's
//!   model), so a request marking more than one MFA factor `PreferredMfa` is
//!   refused for that CONFLICT IN THE REQUEST, whichever factors carry it and
//!   whatever the user holds to receive them with: the conflict is reported ahead
//!   of every per-factor check, and nothing of the request is stored. Both
//!   operations read the same three settings structures, so both refuse it alike.
//! * RULE B — a challenge `Session` answers its challenge ONCE: the response that
//!   answers it spends it, and presenting it again is refused
//!   `NotAuthorizedException` with the single-use clause, which is what tells a
//!   replayed session apart from a value that names no session of the request at
//!   all. Asserted for both response operations and for both challenges this
//!   emulator starts with a session. The OUTSTANDING session is the state the
//!   server holds for its challenge, so a session that is still outstanding
//!   answers it whatever the stored user's status has become since.
//! * RULE C — a `DesiredDeliveryMediums` entry is a request PARAMETER, so a medium
//!   no address the request carries or the pool holds can reach is refused at
//!   validation naming the ATTRIBUTE that was not provided and the medium that was
//!   asked for, and no user is created. A code requested FOR an attribute is
//!   refused for the registered destination the user does not have instead, that
//!   request naming no medium at all.
//! * RULE D — `StartUserImportJob` begins the import of a job "that you previously
//!   created" (`StartUserImportJob.JobId`), so a job in any other state is refused
//!   `PreconditionNotMetException` naming the job the REQUEST asked for, and the
//!   stored job does not move. Asserted over how the job left the created state —
//!   started, stopped, expired by the clock — and over how far the run its own
//!   start made has got: importing, completed, or stopped by an error. The
//!   sentence is the same one for every one of them, since the status the job is
//!   in is no part of it.
//!
//! The pools, app clients, users, tokens and import jobs are real: they come from
//! `CreateUserPool`, `CreateUserPoolClient`, `AdminCreateUser`,
//! `AdminSetUserPassword`, `InitiateAuth`, `AdminInitiateAuth`,
//! `CreateUserImportJob` and the job's own upload target. One fact is planted in
//! this service's state rather than written through the operation that owns it: a
//! user's registered TOTP authenticator (which `AssociateSoftwareToken` and
//! `VerifySoftwareToken` write), so no row computes one-time passwords, and the
//! instant a job was created, so the row about an EXPIRED job needs no wait.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use serde_json::{json, Map, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_cognitoidentityprovider_awscognitoidentityproviderservice as cognito_idp;

use cognito_idp::state;

/// A request's SigV4 credential scope is what routes an administrative call to
/// this service. The signature is never checked here: the suite turns request
/// authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/us-east-1/cognito-idp/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0000";

/// This service's regional endpoint host, which a signed call is addressed to.
const ENDPOINT_HOST: &str = "cognito-idp.us-east-1.amazonaws.com";

/// The emulator's own address, which an unsigned self-service call is addressed
/// to.
const EMULATOR_HOST: &str = "127.0.0.1:9297";

/// A password satisfying a default pool's password policy.
const GOOD_PASSWORD: &str = "Passw0rd!2024";

/// Another one, for a challenge response to settle on.
const CHOSEN_PASSWORD: &str = "Ch0senPassw0rd!";

/// The temporary password an administrator-created user signs in with once, which
/// is what leaves it owing the new-password challenge.
const TEMPORARY_PASSWORD: &str = "Temp0rary!2026";

/// The three `UserMFASettingList` values: "the possible values in this list are
/// `SMS_MFA`, `EMAIL_OTP`, and `SOFTWARE_TOKEN_MFA`" (this service's model). The
/// rows below assert the two a served request leaves stored.
const SMS_MFA: &str = "SMS_MFA";
const EMAIL_OTP_MFA: &str = "EMAIL_OTP";

/// The challenge a user that must change its password answers, and the one the
/// choice-based flow delivers a one-time password for (`ChallengeNameType` in
/// this service's model).
const NEW_PASSWORD_REQUIRED: &str = "NEW_PASSWORD_REQUIRED";
const EMAIL_OTP: &str = "EMAIL_OTP";

/// The status this awsJson1_1 endpoint answers every refusal of the caller with.
const REFUSED: u16 = 400;

/// How long a poll for a status the clock owes keeps looking before it gives up.
/// Generous: the spans it waits out are under a second, and the loop stops as
/// soon as the status holds.
const POLL_DEADLINE: Duration = Duration::from_secs(60);

/// How long one look at the stored job waits before the next one.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// A CloudWatch Logs role no IAM record resolves, which is no evidence of a
/// missing trust: the import rows are not about the role.
const IMPORT_ROLE: &str = "arn:aws:iam::000000000000:role/cognito-import";

/// A CSV file a job can import: the pool's own header row and one record.
const CSV_RECORD_NAME: &str = "imported";

/// The emulator under test and the engine its state is reached through.
struct Emulator {
    engine: Arc<engine::InMemoryEngine>,
    router: MultiServiceRouter,
}

/// One emulator with just this service mounted.
fn emulator() -> Emulator {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.register_service(cognito_idp::EmulatedService::new(Arc::clone(&engine)));
    router.set_request_authentication(false);
    Emulator { engine, router }
}

/// Dispatch `operation` with `body` and return its status and JSON response body.
/// A `signed` call carries a credential scope, as an SDK sends every
/// administrative operation; an unsigned one is addressed to the emulator's own
/// address, which is how a client calls the self-service operations.
async fn dispatch(
    router: &MultiServiceRouter,
    operation: &str,
    body: &Value,
    signed: bool,
) -> (u16, Value) {
    let host = if signed { ENDPOINT_HOST } else { EMULATOR_HOST };
    let mut request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", host)
        .header("content-type", "application/x-amz-json-1.1")
        .header(
            "x-amz-target",
            format!("AWSCognitoIdentityProviderService.{operation}"),
        );
    if signed {
        request = request.header("authorization", AUTHORIZATION);
    }
    let response = router
        .dispatch(
            request
                .body(Bytes::from(body.to_string()))
                .expect("the request is well formed"),
        )
        .await;
    let status = response.status().as_u16();
    let payload = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or_else(|error| {
            panic!(
                "{operation} answered a body that is not JSON ({error}): {:?}",
                String::from_utf8_lossy(response.body())
            )
        })
    };
    (status, payload)
}

/// Call the signed `operation`, requiring success, and return its response body.
async fn ok(router: &MultiServiceRouter, operation: &str, body: &Value) -> Value {
    let (status, payload) = dispatch(router, operation, body, true).await;
    assert_eq!(
        status, 200,
        "{operation} must succeed, got {status} with {payload}"
    );
    payload
}

/// The `__type` of an error body, which is the wire error code.
fn error_code(payload: &Value) -> &str {
    payload
        .get("__type")
        .and_then(Value::as_str)
        .unwrap_or("<no __type>")
}

/// The `message` of an error body.
fn error_message(payload: &Value) -> &str {
    payload
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or("<no message>")
}

/// Create a pool named `name`, with `members` merged into the create, and return
/// its minted `UserPoolId`.
async fn pool(router: &MultiServiceRouter, name: &str, members: &Value) -> String {
    let mut body = Map::new();
    body.insert("PoolName".to_string(), json!(name));
    for (member, value) in members.as_object().expect("an object of members") {
        body.insert(member.clone(), value.clone());
    }
    let created = ok(router, "CreateUserPool", &Value::Object(body)).await;
    created["UserPool"]["Id"]
        .as_str()
        .expect("a minted pool id")
        .to_string()
}

/// Create an app client of `user_pool_id` admitting every flow this suite drives,
/// and return its minted id.
async fn client(router: &MultiServiceRouter, user_pool_id: &str, name: &str) -> String {
    let created = ok(
        router,
        "CreateUserPoolClient",
        &json!({
            "UserPoolId": user_pool_id,
            "ClientName": name,
            "ExplicitAuthFlows": [
                "ALLOW_USER_PASSWORD_AUTH",
                "ALLOW_ADMIN_USER_PASSWORD_AUTH",
                "ALLOW_USER_AUTH",
                "ALLOW_REFRESH_TOKEN_AUTH",
            ],
        }),
    )
    .await;
    created["UserPoolClient"]["ClientId"]
        .as_str()
        .expect("a minted client id")
        .to_string()
}

/// Create `username` in `user_pool_id` with `attributes`, and give it the
/// permanent password every sign-in below presents.
async fn confirmed_user(
    router: &MultiServiceRouter,
    user_pool_id: &str,
    username: &str,
    attributes: &[(&str, &str)],
) {
    ok(
        router,
        "AdminCreateUser",
        &json!({
            "UserPoolId": user_pool_id,
            "Username": username,
            "MessageAction": "SUPPRESS",
            "UserAttributes": Value::Array(
                attributes
                    .iter()
                    .map(|(name, value)| json!({ "Name": name, "Value": value }))
                    .collect(),
            ),
        }),
    )
    .await;
    ok(
        router,
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

/// Plant the TOTP authenticator registration `AssociateSoftwareToken` and
/// `VerifySoftwareToken` write, so the rows that need a user who HAS registered
/// one are driven without this suite computing one-time passwords.
fn register_software_token(instance: &Emulator, user_pool_id: &str, username: &str) {
    let service_state = state::service_state(&*instance.engine).expect("this service's state");
    let mut guard = service_state.lock();
    let pool = guard.pool_mut(user_pool_id).expect("the created pool");
    let user = pool.user_mut(username).expect("the created user");
    user.software_token_registered = true;
}

/// The string members of `payload[member]`, empty when the member is absent.
fn string_list(payload: &Value, member: &str) -> Vec<String> {
    payload[member]
        .as_array()
        .map(|values| {
            values
                .iter()
                .map(|value| value.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// RULE A — two factors marked preferred
// ---------------------------------------------------------------------------

/// The sentence a request expressing two preferences is refused with: entirely
/// fixed text, naming neither factor, since only one preference can be held
/// whichever two were marked.
const TWO_PREFERRED: &str = "Two MFA types marked as preferred, only one is allowed.";

/// Which operation a preference row is submitted through: the administrative
/// write, addressed by `UserPoolId` + `Username`, or the user's own, addressed by
/// its `AccessToken`.
#[derive(Clone, Copy, Debug)]
enum Caller {
    Administrator,
    SignedInUser,
}

impl Caller {
    /// The operation this caller submits an MFA preference through.
    fn operation(self) -> &'static str {
        match self {
            Self::Administrator => "AdminSetUserMFAPreference",
            Self::SignedInUser => "SetUserMFAPreference",
        }
    }
}

/// One row of the preference table.
#[derive(Clone, Copy, Debug)]
struct PreferenceCase {
    /// What this row asserts.
    case: &'static str,
    /// The attributes the user is created with.
    attributes: &'static [(&'static str, &'static str)],
    /// Whether that user has registered a TOTP authenticator.
    software_token_registered: bool,
    /// `SMSMfaSettings`, `SoftwareTokenMfaSettings` and `EmailMfaSettings` as
    /// `(Enabled, PreferredMfa)`, each only when the row submits it.
    sms: Option<(bool, bool)>,
    software_token: Option<(bool, bool)>,
    email: Option<(bool, bool)>,
    /// The `UserMFASettingList` and `PreferredMfaSetting` a served row leaves
    /// stored, or `None` for a row refused for the conflict.
    served: Option<(&'static [&'static str], Option<&'static str>)>,
}

/// A verified phone number, a verified email address, and both.
const VERIFIED_PHONE: &[(&str, &str)] = &[
    ("phone_number", "+15550100001"),
    ("phone_number_verified", "true"),
];
const VERIFIED_EMAIL: &[(&str, &str)] = &[("email", "ada@example.com"), ("email_verified", "true")];
const VERIFIED_BOTH: &[(&str, &str)] = &[
    ("email", "ada@example.com"),
    ("email_verified", "true"),
    ("phone_number", "+15550100002"),
    ("phone_number_verified", "true"),
];

/// The rows differ along WHICH two factors are marked preferred, how MANY are,
/// whether the request also activates them, and what the user holds to receive
/// each with — including a factor the pool has no delivery configuration for,
/// whose own refusal the conflict is reported ahead of.
const PREFERENCE_CASES: &[PreferenceCase] = &[
    PreferenceCase {
        case: "SMS and software token both preferred, for a user with no registered \
               authenticator",
        attributes: VERIFIED_PHONE,
        software_token_registered: false,
        sms: Some((true, true)),
        software_token: Some((true, true)),
        email: None,
        served: None,
    },
    PreferenceCase {
        case: "SMS and email message MFA both preferred, both receivable",
        attributes: VERIFIED_BOTH,
        software_token_registered: false,
        sms: Some((true, true)),
        software_token: None,
        email: Some((true, true)),
        served: None,
    },
    PreferenceCase {
        case: "email message and software token MFA both preferred, both receivable",
        attributes: VERIFIED_EMAIL,
        software_token_registered: true,
        sms: None,
        software_token: Some((true, true)),
        email: Some((true, true)),
        served: None,
    },
    PreferenceCase {
        case: "all three factors preferred at once",
        attributes: VERIFIED_BOTH,
        software_token_registered: true,
        sms: Some((true, true)),
        software_token: Some((true, true)),
        email: Some((true, true)),
        served: None,
    },
    PreferenceCase {
        case: "two factors preferred while the request activates neither",
        attributes: VERIFIED_BOTH,
        software_token_registered: false,
        sms: Some((false, true)),
        software_token: None,
        email: Some((false, true)),
        served: None,
    },
    PreferenceCase {
        case: "one factor preferred beside another activated without a preference",
        attributes: VERIFIED_BOTH,
        software_token_registered: false,
        sms: Some((true, false)),
        software_token: None,
        email: Some((true, true)),
        served: Some((&[SMS_MFA, EMAIL_OTP_MFA], Some(EMAIL_OTP_MFA))),
    },
    PreferenceCase {
        case: "a single factor preferred, which is the one preference a user can hold",
        attributes: VERIFIED_PHONE,
        software_token_registered: false,
        sms: Some((true, true)),
        software_token: None,
        email: None,
        served: Some((&[SMS_MFA], Some(SMS_MFA))),
    },
];

/// The body one preference row submits through `caller`: only the structures the
/// row names are present, as an SDK sends only the settings the caller passed.
fn preference_body(
    caller: Caller,
    user_pool_id: &str,
    username: &str,
    access_token: &str,
    row: &PreferenceCase,
) -> Value {
    let mut body = Map::new();
    match caller {
        Caller::Administrator => {
            body.insert("UserPoolId".to_string(), json!(user_pool_id));
            body.insert("Username".to_string(), json!(username));
        }
        Caller::SignedInUser => {
            body.insert("AccessToken".to_string(), json!(access_token));
        }
    }
    for (member, settings) in [
        ("SMSMfaSettings", row.sms),
        ("SoftwareTokenMfaSettings", row.software_token),
        ("EmailMfaSettings", row.email),
    ] {
        if let Some((enabled, preferred)) = settings {
            body.insert(
                member.to_string(),
                json!({ "Enabled": enabled, "PreferredMfa": preferred }),
            );
        }
    }
    Value::Object(body)
}

/// RULE A: a request marking two factors preferred is refused for the conflict in
/// the request itself, ahead of every per-factor check, and stores nothing.
#[tokio::test]
async fn two_factors_marked_preferred_are_refused_for_the_conflict_in_the_request() {
    for (index, row) in PREFERENCE_CASES.iter().enumerate() {
        for caller in [Caller::Administrator, Caller::SignedInUser] {
            let case = format!("{} through {}", row.case, caller.operation());
            let instance = emulator();
            let user_pool_id = pool(
                &instance.router,
                &format!("preference-conflict-{index}"),
                &json!({}),
            )
            .await;
            let client_id = client(&instance.router, &user_pool_id, "preference-client").await;
            let username = "ada";
            confirmed_user(&instance.router, &user_pool_id, username, row.attributes).await;
            if row.software_token_registered {
                register_software_token(&instance, &user_pool_id, username);
            }
            let (status, signed_in) = dispatch(
                &instance.router,
                "InitiateAuth",
                &json!({
                    "ClientId": client_id,
                    "AuthFlow": "USER_PASSWORD_AUTH",
                    "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
                }),
                false,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: signing the user in must succeed, got {status} with {signed_in}"
            );
            let access_token = signed_in["AuthenticationResult"]["AccessToken"]
                .as_str()
                .expect("a completed sign-in carries an access token")
                .to_string();

            let body = preference_body(caller, &user_pool_id, username, &access_token, row);
            let (status, payload) = dispatch(
                &instance.router,
                caller.operation(),
                &body,
                matches!(caller, Caller::Administrator),
            )
            .await;
            let read = ok(
                &instance.router,
                "AdminGetUser",
                &json!({ "UserPoolId": user_pool_id, "Username": username }),
            )
            .await;
            let settings = string_list(&read, "UserMFASettingList");
            let preferred = read["PreferredMfaSetting"].as_str();

            match row.served {
                None => {
                    assert_eq!(
                        (status, error_code(&payload)),
                        (REFUSED, "InvalidParameterException"),
                        "case {case:?}: expected a {REFUSED} InvalidParameterException, \
                         got {status} with {payload}"
                    );
                    let message = error_message(&payload);
                    assert_eq!(
                        message, TWO_PREFERRED,
                        "case {case:?}: expected {TWO_PREFERRED:?}, got {message:?}"
                    );
                    // The whole request is refused, so not even the half that
                    // would have been served is stored.
                    assert!(
                        settings.is_empty() && preferred.is_none(),
                        "case {case:?}: a refused request stores no preference, \
                         got {settings:?} preferring {preferred:?}"
                    );
                }
                Some((expected_settings, expected_preferred)) => {
                    assert_eq!(
                        status, 200,
                        "case {case:?}: expected HTTP 200, got {status} with {payload}"
                    );
                    let mut sorted = settings.clone();
                    sorted.sort();
                    let mut wanted: Vec<String> = expected_settings
                        .iter()
                        .map(|setting| (*setting).to_string())
                        .collect();
                    wanted.sort();
                    assert_eq!(
                        sorted, wanted,
                        "case {case:?}: expected the stored settings {wanted:?}, got {sorted:?}"
                    );
                    assert_eq!(
                        preferred, expected_preferred,
                        "case {case:?}: expected the stored preference \
                         {expected_preferred:?}, got {preferred:?}"
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RULE B — a challenge session answers its challenge once
// ---------------------------------------------------------------------------

/// The sentence a session an earlier response already answered is refused with,
/// whose single-use clause is what distinguishes it from a value that names no
/// session of this request.
const SESSION_ALREADY_USED: &str = "Invalid session for the user, session can only be used once.";

/// The sentence a value naming no session of this request is refused with: a challenge
/// response authenticates with the pair of its session and its challenge parameters, so a
/// session half that resolves no outstanding challenge is a code/session mismatch.
const CHALLENGE_SESSION_MISMATCH: &str = "Invalid code/session provided";

/// A `SessionType` value (len 20..4096) this emulator never handed out.
const UNISSUED_SESSION: &str = "nosuchsessionvaluehere0123456789";

/// RULE B: answering the new-password challenge completes the sign-in and SPENDS
/// its session — the rows differ along the operation that carried the response.
#[tokio::test]
async fn a_replayed_new_password_session_reports_the_single_use_clause() {
    // (case, the operation answering the challenge, the one starting it)
    let rows: &[(&str, &str, &str)] = &[
        (
            "the self-service pair",
            "RespondToAuthChallenge",
            "InitiateAuth",
        ),
        (
            "the administrative pair",
            "AdminRespondToAuthChallenge",
            "AdminInitiateAuth",
        ),
    ];

    for (index, (case, responder, starter)) in rows.iter().enumerate() {
        let instance = emulator();
        let user_pool_id = pool(
            &instance.router,
            &format!("challenge-replay-{index}"),
            &json!({}),
        )
        .await;
        let client_id = client(&instance.router, &user_pool_id, "challenge-client").await;
        let username = "newcomer";
        ok(
            &instance.router,
            "AdminCreateUser",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "TemporaryPassword": TEMPORARY_PASSWORD,
                "MessageAction": "SUPPRESS",
            }),
        )
        .await;

        // The administrative flow names the pool and signs; the self-service one
        // is addressed by the app client alone.
        let administrative = starter.starts_with("Admin");
        let mut start = Map::new();
        if administrative {
            start.insert("UserPoolId".to_string(), json!(user_pool_id));
            start.insert("AuthFlow".to_string(), json!("ADMIN_USER_PASSWORD_AUTH"));
        } else {
            start.insert("AuthFlow".to_string(), json!("USER_PASSWORD_AUTH"));
        }
        start.insert("ClientId".to_string(), json!(client_id));
        start.insert(
            "AuthParameters".to_string(),
            json!({ "USERNAME": username, "PASSWORD": TEMPORARY_PASSWORD }),
        );
        let (status, started) = dispatch(
            &instance.router,
            starter,
            &Value::Object(start),
            administrative,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: {starter} must raise the challenge, got {status} with {started}"
        );
        assert_eq!(
            started["ChallengeName"], NEW_PASSWORD_REQUIRED,
            "case {case:?}: an administrator-created user owes a password change, got {started}"
        );
        let session = started["Session"]
            .as_str()
            .expect("the challenge carries a session")
            .to_string();

        // The response body the challenge is answered with, submitted twice: the
        // second one is the REPLAY.
        let mut response = Map::new();
        if administrative {
            response.insert("UserPoolId".to_string(), json!(user_pool_id));
        }
        response.insert("ClientId".to_string(), json!(client_id));
        response.insert("ChallengeName".to_string(), json!(NEW_PASSWORD_REQUIRED));
        response.insert("Session".to_string(), json!(session));
        response.insert(
            "ChallengeResponses".to_string(),
            json!({ "USERNAME": username, "NEW_PASSWORD": CHOSEN_PASSWORD }),
        );
        let response = Value::Object(response);
        let (status, answered) =
            dispatch(&instance.router, responder, &response, administrative).await;
        assert_eq!(
            status, 200,
            "case {case:?}: {responder} must complete the sign-in, got {status} with {answered}"
        );
        assert!(
            answered["AuthenticationResult"]["AccessToken"].is_string(),
            "case {case:?}: a completed sign-in carries its tokens, got {answered}"
        );

        // ROUND TRIP: the password the challenge chose is the stored one, and the
        // user owes no further change.
        let read = ok(
            &instance.router,
            "AdminGetUser",
            &json!({ "UserPoolId": user_pool_id, "Username": username }),
        )
        .await;
        assert_eq!(
            read["UserStatus"], "CONFIRMED",
            "case {case:?}: an answered challenge confirms the user, got {read}"
        );
        let (status, resigned) = dispatch(
            &instance.router,
            "InitiateAuth",
            &json!({
                "ClientId": client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": CHOSEN_PASSWORD },
            }),
            false,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the chosen password must sign the user in, \
             got {status} with {resigned}"
        );

        // The REPLAY: the same session, answered a second time.
        let (status, replayed) =
            dispatch(&instance.router, responder, &response, administrative).await;
        assert_eq!(
            (status, error_code(&replayed)),
            (REFUSED, "NotAuthorizedException"),
            "case {case:?}: expected a {REFUSED} NotAuthorizedException, \
             got {status} with {replayed}"
        );
        let message = error_message(&replayed);
        assert_eq!(
            message, SESSION_ALREADY_USED,
            "case {case:?}: expected {SESSION_ALREADY_USED:?}, got {message:?}"
        );

        // A value naming NO session of this request is the other fact, and says
        // so: the single-use clause above is what a session this request DID
        // hold reports, while a value that resolves no outstanding challenge is
        // the response's pair failing.
        let mut unissued = response.clone();
        unissued["Session"] = json!(UNISSUED_SESSION);
        let (status, unknown) =
            dispatch(&instance.router, responder, &unissued, administrative).await;
        assert_eq!(
            (status, error_code(&unknown)),
            (REFUSED, "CodeMismatchException"),
            "case {case:?}: expected a {REFUSED} CodeMismatchException, \
             got {status} with {unknown}"
        );
        let message = error_message(&unknown);
        assert_eq!(
            message, CHALLENGE_SESSION_MISMATCH,
            "case {case:?}: expected {CHALLENGE_SESSION_MISMATCH:?}, got {message:?}"
        );
    }
}

/// RULE B: the OUTSTANDING session is the state the server holds for its
/// challenge, so a second session raised for the same user answers that challenge
/// after the first one already did — the rows differ along the operation that
/// carries the second response.
#[tokio::test]
async fn an_outstanding_session_answers_its_challenge_after_another_already_did() {
    // (case, the operation answering the second session)
    let rows: &[(&str, &str)] = &[
        ("answered client-side", "RespondToAuthChallenge"),
        ("answered administratively", "AdminRespondToAuthChallenge"),
    ];
    // The second session chooses its own password, so the row proves which
    // response the stored password came from.
    const SECOND_PASSWORD: &str = "Sec0ndPassw0rd!";

    for (index, (case, responder)) in rows.iter().enumerate() {
        let instance = emulator();
        let user_pool_id = pool(
            &instance.router,
            &format!("two-session-{index}"),
            &json!({}),
        )
        .await;
        let client_id = client(&instance.router, &user_pool_id, "two-session-client").await;
        let username = "newcomer";
        ok(
            &instance.router,
            "AdminCreateUser",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "TemporaryPassword": TEMPORARY_PASSWORD,
                "MessageAction": "SUPPRESS",
            }),
        )
        .await;

        // TWO sign-ins, each handed its own session for the same challenge.
        let mut sessions = Vec::new();
        for attempt in 0..2 {
            let (status, started) = dispatch(
                &instance.router,
                "InitiateAuth",
                &json!({
                    "ClientId": client_id,
                    "AuthFlow": "USER_PASSWORD_AUTH",
                    "AuthParameters": { "USERNAME": username, "PASSWORD": TEMPORARY_PASSWORD },
                }),
                false,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: sign-in {attempt} must raise the challenge, \
                 got {status} with {started}"
            );
            sessions.push(
                started["Session"]
                    .as_str()
                    .expect("the challenge carries a session")
                    .to_string(),
            );
        }

        // The first session is answered, which confirms the user.
        let answered = ok(
            &instance.router,
            "AdminRespondToAuthChallenge",
            &json!({
                "UserPoolId": user_pool_id,
                "ClientId": client_id,
                "ChallengeName": NEW_PASSWORD_REQUIRED,
                "Session": sessions[0],
                "ChallengeResponses": {
                    "USERNAME": username,
                    "NEW_PASSWORD": CHOSEN_PASSWORD,
                },
            }),
        )
        .await;
        assert!(
            answered["AuthenticationResult"]["AccessToken"].is_string(),
            "case {case:?}: the first response must complete its sign-in, got {answered}"
        );

        // The second session is still outstanding, so it answers its challenge —
        // and the password IT chose is the one stored from then on.
        let administrative = responder.starts_with("Admin");
        let mut second = Map::new();
        if administrative {
            second.insert("UserPoolId".to_string(), json!(user_pool_id));
        }
        second.insert("ClientId".to_string(), json!(client_id));
        second.insert("ChallengeName".to_string(), json!(NEW_PASSWORD_REQUIRED));
        second.insert("Session".to_string(), json!(sessions[1]));
        second.insert(
            "ChallengeResponses".to_string(),
            json!({ "USERNAME": username, "NEW_PASSWORD": SECOND_PASSWORD }),
        );
        let second = Value::Object(second);
        let (status, completed) =
            dispatch(&instance.router, responder, &second, administrative).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an outstanding session answers its challenge, \
             got {status} with {completed}"
        );
        assert!(
            completed["AuthenticationResult"]["AccessToken"].is_string(),
            "case {case:?}: that response completes its own sign-in, got {completed}"
        );

        // ROUND TRIP: the password the second response chose signs the user in.
        let (status, resigned) = dispatch(
            &instance.router,
            "InitiateAuth",
            &json!({
                "ClientId": client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": SECOND_PASSWORD },
            }),
            false,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: the second chosen password must sign the user in, \
             got {status} with {resigned}"
        );

        // Each session answered ONCE: replaying either is refused as the replay
        // it is.
        for (which, session) in sessions.iter().enumerate() {
            let mut replay = second.clone();
            replay["Session"] = json!(session);
            let (status, payload) =
                dispatch(&instance.router, responder, &replay, administrative).await;
            assert_eq!(
                (status, error_code(&payload), error_message(&payload)),
                (REFUSED, "NotAuthorizedException", SESSION_ALREADY_USED),
                "case {case:?}: replaying session {which} is refused as a replay, \
                 got {status} with {payload}"
            );
        }
    }
}

/// RULE B for the other challenge this emulator starts with a session: a
/// one-time-password session is spent by the response that answers it, and a
/// replay reports the same single-use clause.
#[tokio::test]
async fn a_replayed_one_time_password_session_reports_the_single_use_clause() {
    let instance = emulator();
    // The pool admits the one-time password as a first authentication factor,
    // which is what lets the choice-based flow raise that challenge.
    let user_pool_id = pool(
        &instance.router,
        "otp-replay-pool",
        &json!({
            "Policies": { "SignInPolicy": { "AllowedFirstAuthFactors": ["PASSWORD", EMAIL_OTP] } },
            "AutoVerifiedAttributes": ["email"],
        }),
    )
    .await;
    let client_id = client(&instance.router, &user_pool_id, "otp-client").await;
    let username = "otp";
    confirmed_user(
        &instance.router,
        &user_pool_id,
        username,
        &[("email", "otp@example.com"), ("email_verified", "true")],
    )
    .await;

    let (status, started) = dispatch(
        &instance.router,
        "InitiateAuth",
        &json!({
            "ClientId": client_id,
            "AuthFlow": "USER_AUTH",
            "AuthParameters": { "USERNAME": username, "PREFERRED_CHALLENGE": EMAIL_OTP },
        }),
        false,
    )
    .await;
    assert_eq!(
        status, 200,
        "the choice-based flow must raise the challenge, got {status} with {started}"
    );
    let session = started["Session"]
        .as_str()
        .expect("the challenge carries a session")
        .to_string();
    // The delivered code is read out of this service's own state, because no
    // response carries it.
    let code = {
        let service_state = state::service_state(&*instance.engine).expect("this service's state");
        let guard = service_state.lock();
        guard
            .auth_sessions
            .get(&session)
            .and_then(|stored| stored.code.as_ref())
            .map(|code| code.code.clone())
            .expect("the challenge delivered a code")
    };
    let response = json!({
        "ClientId": client_id,
        "ChallengeName": EMAIL_OTP,
        "Session": session,
        "ChallengeResponses": { "USERNAME": username, "EMAIL_OTP_CODE": code },
    });
    let (status, answered) =
        dispatch(&instance.router, "RespondToAuthChallenge", &response, false).await;
    assert_eq!(
        status, 200,
        "the delivered code must complete the sign-in, got {status} with {answered}"
    );
    let (status, replayed) =
        dispatch(&instance.router, "RespondToAuthChallenge", &response, false).await;
    assert_eq!(
        (status, error_code(&replayed)),
        (REFUSED, "NotAuthorizedException"),
        "expected a {REFUSED} NotAuthorizedException, got {status} with {replayed}"
    );
    let message = error_message(&replayed);
    assert_eq!(
        message, SESSION_ALREADY_USED,
        "expected {SESSION_ALREADY_USED:?}, got {message:?}"
    );
}

// ---------------------------------------------------------------------------
// RULE C — a desired delivery medium with no address
// ---------------------------------------------------------------------------

/// The sentence a create asking for a medium nothing addresses is refused with:
/// the attribute that was not provided and the medium that was asked for, both
/// named by the model, with no trailing period.
fn unaddressed_medium(medium: &str, attribute: &str) -> String {
    format!("No {attribute} provided but desired delivery medium was {medium}")
}

/// One row of the delivery-medium table.
#[derive(Clone, Copy, Debug)]
struct MediumCase {
    /// What this row asserts.
    case: &'static str,
    /// The `DesiredDeliveryMediums` the create asks for.
    mediums: &'static [&'static str],
    /// The attributes the create carries.
    attributes: &'static [(&'static str, &'static str)],
    /// The medium and the attribute the refusal names, or `None` where the
    /// create is served.
    refusal: Option<(&'static str, &'static str)>,
}

/// The rows differ along which medium is asked for, which address the user
/// carries and how many mediums the request names.
const MEDIUM_CASES: &[MediumCase] = &[
    MediumCase {
        case: "SMS for a user carrying only an email address",
        mediums: &["SMS"],
        attributes: &[("email", "ada@example.com")],
        refusal: Some(("SMS", "phone_number")),
    },
    MediumCase {
        case: "EMAIL for a user carrying only a phone number",
        mediums: &["EMAIL"],
        attributes: &[("phone_number", "+15550100003")],
        refusal: Some(("EMAIL", "email")),
    },
    MediumCase {
        case: "both mediums for a user carrying neither address, SMS first",
        mediums: &["SMS", "EMAIL"],
        attributes: &[("name", "Nameless")],
        refusal: Some(("SMS", "phone_number")),
    },
    MediumCase {
        case: "both mediums for a user carrying neither address, EMAIL first",
        mediums: &["EMAIL", "SMS"],
        attributes: &[("name", "Nameless")],
        refusal: Some(("EMAIL", "email")),
    },
    MediumCase {
        case: "SMS for a user whose phone number is submitted empty",
        mediums: &["SMS"],
        attributes: &[("email", "ada@example.com"), ("phone_number", "")],
        refusal: Some(("SMS", "phone_number")),
    },
    MediumCase {
        case: "EMAIL for a user carrying the address it needs",
        mediums: &["EMAIL"],
        attributes: &[("email", "ada@example.com")],
        refusal: None,
    },
    MediumCase {
        case: "both mediums for a user carrying both addresses",
        mediums: &["SMS", "EMAIL"],
        attributes: &[
            ("email", "ada@example.com"),
            ("phone_number", "+15550100004"),
        ],
        refusal: None,
    },
];

/// RULE C: every row of [`MEDIUM_CASES`], driven through `AdminCreateUser`.
#[tokio::test]
async fn a_desired_delivery_medium_with_no_address_names_the_attribute_not_provided() {
    let instance = emulator();
    let user_pool_id = pool(&instance.router, "delivery-medium-pool", &json!({})).await;
    for (index, row) in MEDIUM_CASES.iter().enumerate() {
        let MediumCase {
            case,
            mediums,
            attributes,
            refusal,
        } = row;
        let username = format!("created{index}");
        let (status, payload) = dispatch(
            &instance.router,
            "AdminCreateUser",
            &json!({
                "UserPoolId": user_pool_id,
                "Username": username,
                "MessageAction": "SUPPRESS",
                "DesiredDeliveryMediums": mediums,
                "UserAttributes": Value::Array(
                    attributes
                        .iter()
                        .map(|(name, value)| json!({ "Name": name, "Value": value }))
                        .collect(),
                ),
            }),
            true,
        )
        .await;
        match refusal {
            Some((medium, attribute)) => {
                assert_eq!(
                    (status, error_code(&payload)),
                    (REFUSED, "InvalidParameterException"),
                    "case {case:?}: expected a {REFUSED} InvalidParameterException, \
                     got {status} with {payload}"
                );
                let expected = unaddressed_medium(medium, attribute);
                let message = error_message(&payload);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
                // The refusal came at request validation, so no user was written.
                let (read_status, read_back) = dispatch(
                    &instance.router,
                    "AdminGetUser",
                    &json!({ "UserPoolId": user_pool_id, "Username": username }),
                    true,
                )
                .await;
                assert_eq!(
                    (read_status, error_code(&read_back)),
                    (REFUSED, "UserNotFoundException"),
                    "case {case:?}: a refused create writes no user, \
                     got {read_status} with {read_back}"
                );
            }
            None => assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200, got {status} with {payload}"
            ),
        }
    }
}

/// The sentence a code requested for an attribute the user holds no value for is
/// refused with: the KIND of destination the user has not registered — the number
/// for the attribute a text message is sent to, the address itself for the one an
/// email is mailed to (the two attributes a `DeliveryMediumType` addresses) — and
/// no medium, this request naming none.
fn no_registered_destination(destination: &str) -> String {
    format!("User does not have a valid registered {destination}")
}

/// One row of the verification-code table.
#[derive(Clone, Copy, Debug)]
struct CodeCase {
    /// What this row asserts.
    case: &'static str,
    /// The attribute a code is asked for.
    attribute: &'static str,
    /// The attributes the signed-in user holds.
    attributes: &'static [(&'static str, &'static str)],
    /// The destination the refusal names, or `None` where the code is sent.
    destination: Option<&'static str>,
}

/// The rows differ along which attribute a code is asked for and which address
/// the signed-in user holds.
const CODE_CASES: &[CodeCase] = &[
    CodeCase {
        case: "a code for phone_number, for a user holding only an email address",
        attribute: "phone_number",
        attributes: &[("email", "ada@example.com")],
        destination: Some("phone number"),
    },
    CodeCase {
        case: "a code for email, for a user holding only a phone number",
        attribute: "email",
        attributes: &[("phone_number", "+15550100005")],
        destination: Some("email"),
    },
    CodeCase {
        case: "a code for phone_number, for a user holding neither",
        attribute: "phone_number",
        attributes: &[("name", "Nameless")],
        destination: Some("phone number"),
    },
    CodeCase {
        case: "a code for email, for a user holding the address it is sent to",
        attribute: "email",
        attributes: &[("email", "ada@example.com")],
        destination: None,
    },
    CodeCase {
        case: "a code for phone_number, for a user holding the number it is sent to",
        attribute: "phone_number",
        attributes: &[("phone_number", "+15550100006")],
        destination: None,
    },
];

/// RULE C for the other half of the addressing rule: a verification code asked
/// FOR an attribute, over every row of [`CODE_CASES`].
#[tokio::test]
async fn a_code_for_an_attribute_the_user_holds_no_value_for_names_the_destination() {
    for (index, row) in CODE_CASES.iter().enumerate() {
        let CodeCase {
            case,
            attribute,
            attributes,
            destination: refusal,
        } = row;
        let instance = emulator();
        let user_pool_id = pool(
            &instance.router,
            &format!("code-destination-{index}"),
            &json!({}),
        )
        .await;
        let client_id = client(&instance.router, &user_pool_id, "code-client").await;
        let username = "ada";
        confirmed_user(&instance.router, &user_pool_id, username, attributes).await;
        let (status, signed_in) = dispatch(
            &instance.router,
            "InitiateAuth",
            &json!({
                "ClientId": client_id,
                "AuthFlow": "USER_PASSWORD_AUTH",
                "AuthParameters": { "USERNAME": username, "PASSWORD": GOOD_PASSWORD },
            }),
            false,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: signing the user in must succeed, got {status} with {signed_in}"
        );
        let access_token = signed_in["AuthenticationResult"]["AccessToken"]
            .as_str()
            .expect("a completed sign-in carries an access token");

        let (status, payload) = dispatch(
            &instance.router,
            "GetUserAttributeVerificationCode",
            &json!({ "AccessToken": access_token, "AttributeName": attribute }),
            false,
        )
        .await;
        match refusal {
            Some(destination) => {
                assert_eq!(
                    (status, error_code(&payload)),
                    (REFUSED, "InvalidParameterException"),
                    "case {case:?}: expected a {REFUSED} InvalidParameterException, \
                     got {status} with {payload}"
                );
                let expected = no_registered_destination(destination);
                let message = error_message(&payload);
                assert_eq!(
                    message, expected,
                    "case {case:?}: expected {expected:?}, got {message:?}"
                );
            }
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: expected HTTP 200, got {status} with {payload}"
                );
                assert_eq!(
                    payload["CodeDeliveryDetails"]["AttributeName"],
                    json!(attribute),
                    "case {case:?}: the code reports the attribute it was sent for, \
                     got {payload}"
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------
// RULE D — starting a job that is not in the created state
// ---------------------------------------------------------------------------

/// How a stored job comes to be in a state a start cannot begin.
#[derive(Clone, Copy, Debug)]
enum NotCreated {
    /// It was started already, so its import is under way.
    Started,
    /// It was started and then stopped.
    Stopped,
    /// It was never started and the clock has expired it.
    Expired,
}

/// The sentence a start of a job that is not waiting to be started is refused
/// with: the `JobId` the REQUEST carried LEADS it, and the rest is fixed.
fn not_in_created_state(job_id: &str) -> String {
    format!("{job_id} is not in the created state.")
}

/// Create a job of `user_pool_id`, upload a file the pool's own import template
/// can read, and return its `JobId`.
async fn import_job(instance: &Emulator, user_pool_id: &str, job_name: &str) -> String {
    let header = ok(
        &instance.router,
        "GetCSVHeader",
        &json!({ "UserPoolId": user_pool_id }),
    )
    .await;
    let columns: Vec<String> = string_list(&header, "CSVHeader");
    let row: Vec<String> = columns
        .iter()
        .map(|column| {
            if column == state::CSV_USERNAME_COLUMN {
                CSV_RECORD_NAME.to_string()
            } else {
                String::new()
            }
        })
        .collect();
    let file = format!("{}\n{}\n", columns.join(","), row.join(","));
    job_with_file(instance, user_pool_id, job_name, file).await
}

/// Create a job of `user_pool_id`, upload `file` to the target the created job
/// names — whatever that file carries — and return its `JobId`.
async fn job_with_file(
    instance: &Emulator,
    user_pool_id: &str,
    job_name: &str,
    file: String,
) -> String {
    let created = ok(
        &instance.router,
        "CreateUserImportJob",
        &json!({
            "UserPoolId": user_pool_id,
            "JobName": job_name,
            "CloudWatchLogsRoleArn": IMPORT_ROLE,
        }),
    )
    .await;
    let job = &created["UserImportJob"];
    let url = job["PreSignedUrl"]
        .as_str()
        .unwrap_or_else(|| panic!("a created job carries its locator, got {job}"));
    let path = url
        .split_once(ENDPOINT_HOST)
        .map(|(_, path)| path.to_string())
        .unwrap_or_else(|| panic!("{url} must be a locator on {ENDPOINT_HOST}"));
    let request = http::Request::builder()
        .method("PUT")
        .uri(path)
        .header("host", ENDPOINT_HOST)
        .header("content-type", "text/csv")
        .body(Bytes::from(file))
        .expect("the request is well formed");
    let response = instance.router.dispatch(request).await;
    assert_eq!(
        response.status().as_u16(),
        200,
        "the upload target must accept the CSV, got {}",
        response.status()
    );
    job["JobId"]
        .as_str()
        .expect("a minted job id")
        .to_string()
}

/// The `Status` the stored job reports.
async fn job_status(instance: &Emulator, user_pool_id: &str, job_id: &str) -> String {
    let read = ok(
        &instance.router,
        "DescribeUserImportJob",
        &json!({ "UserPoolId": user_pool_id, "JobId": job_id }),
    )
    .await;
    read["UserImportJob"]["Status"]
        .as_str()
        .expect("a stored job reports its status")
        .to_string()
}

/// Read the stored job until its status is no longer `waiting_on`, and answer the
/// status it reported then. The CONDITION is what this waits for: the clock owes
/// the move, so the loop looks again until it holds or [`POLL_DEADLINE`] passes.
async fn poll_past_status(
    instance: &Emulator,
    case: &str,
    user_pool_id: &str,
    job_id: &str,
    waiting_on: &str,
) -> String {
    let deadline = Instant::now() + POLL_DEADLINE;
    loop {
        let status = job_status(instance, user_pool_id, job_id).await;
        if status != waiting_on {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "case {case:?}: expected the job to move past {waiting_on}, still {status}"
        );
        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// RULE D: the rows differ along HOW the job came to be out of the created state —
/// started, stopped, or expired by the clock.
#[tokio::test]
async fn starting_a_job_that_is_not_in_the_created_state_names_the_job_the_request_asked_for() {
    for (index, way) in [NotCreated::Started, NotCreated::Stopped, NotCreated::Expired]
        .iter()
        .enumerate()
    {
        let case = format!("{way:?}");
        let instance = emulator();
        let user_pool_id = pool(
            &instance.router,
            &format!("start-precondition-{index}"),
            &json!({}),
        )
        .await;
        let job_id = import_job(&instance, &user_pool_id, "start-precondition-job").await;
        let body = json!({ "UserPoolId": user_pool_id, "JobId": job_id });
        match way {
            NotCreated::Started => {
                ok(&instance.router, "StartUserImportJob", &body).await;
            }
            NotCreated::Stopped => {
                ok(&instance.router, "StartUserImportJob", &body).await;
                ok(&instance.router, "StopUserImportJob", &body).await;
                // The stop is not complete when the call returns, so the row
                // waits for the status the clock owes rather than for a span.
                poll_past_status(
                    &instance,
                    &case,
                    &user_pool_id,
                    &job_id,
                    state::JOB_STATUS_STOPPING,
                )
                .await;
            }
            NotCreated::Expired => {
                // The job's own creation instant is moved back past the expiry
                // the model documents, so the clock has already expired it.
                let service_state =
                    state::service_state(&*instance.engine).expect("this service's state");
                let mut guard = service_state.lock();
                let pool = guard.pool_mut(&user_pool_id).expect("the created pool");
                let job = pool
                    .user_import_jobs
                    .get_mut(&job_id)
                    .expect("the created job");
                job.creation_date -= state::JOB_EXPIRY_SPAN + Duration::from_secs(1);
            }
        }

        // The status the store and the clock put the job in, read before the
        // refused start and again after it.
        let before = job_status(&instance, &user_pool_id, &job_id).await;
        assert_ne!(
            before,
            state::JOB_STATUS_CREATED,
            "case {case:?}: this row needs a job that is not in the created state"
        );
        let (status, payload) = dispatch(&instance.router, "StartUserImportJob", &body, true).await;
        assert_eq!(
            (status, error_code(&payload)),
            (REFUSED, "PreconditionNotMetException"),
            "case {case:?}: expected a {REFUSED} PreconditionNotMetException, \
             got {status} with {payload}"
        );
        let expected = not_in_created_state(&job_id);
        let message = error_message(&payload);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // The sentence reports the job the request named and nothing else: not
        // the pool, and not the status the job is in.
        assert!(
            !message.contains(user_pool_id.as_str()) && !message.contains(before.as_str()),
            "case {case:?}: the sentence must quote neither the pool nor the status, \
             got {message:?}"
        );

        // A refused start moves nothing: the job is in the state it was, except
        // for the moves the clock itself owes.
        let after = job_status(&instance, &user_pool_id, &job_id).await;
        assert_ne!(
            after,
            state::JOB_STATUS_CREATED,
            "case {case:?}: a refused start must not put the job back in the created state, \
             got {after}"
        );
    }
}

/// How far a planted instant is moved past the moment a read compares it with, so
/// the move it governs is unambiguously due.
const PLANTED_STEP: Duration = Duration::from_secs(1);

/// How far the NEXT move of a planted job is pushed out, so it stays undue for as
/// long as any machine takes to run one row.
const PLANTED_HORIZON: Duration = Duration::from_secs(60 * 60);

/// Where a job's OWN run has got to when a second start arrives: the three
/// statuses the clock drives a started job through, none of them the created
/// state.
#[derive(Clone, Copy, Debug)]
enum StartedJob {
    /// The clock has made the import due, so "users are being imported"
    /// (`UserImportJobType.Status`).
    InProgress,
    /// The run read the file and the clock has made its completion due.
    Succeeded,
    /// The file's header is not one this pool's import can read, so the run
    /// placed no row and "the job has stopped due to an error".
    Failed,
}

impl StartedJob {
    /// The status the stored job reports once this row's instants are planted.
    fn status(self) -> &'static str {
        match self {
            Self::InProgress => state::JOB_STATUS_IN_PROGRESS,
            Self::Succeeded => state::JOB_STATUS_SUCCEEDED,
            Self::Failed => state::JOB_STATUS_FAILED,
        }
    }
}

/// RULE D, over the statuses a job's own RUN reaches: a job that was started has
/// left the created state for good, so a second start is refused with the same
/// sentence whether the import is still running or has reached either terminal
/// status. The rows differ along the status the stored job is in when that second
/// start arrives, and along the file the job was handed, which is what decides the
/// terminal status its run earns.
#[tokio::test]
async fn starting_a_job_whose_run_has_begun_names_the_job_whatever_the_run_reached() {
    for (index, way) in [
        StartedJob::InProgress,
        StartedJob::Succeeded,
        StartedJob::Failed,
    ]
    .into_iter()
    .enumerate()
    {
        let case = format!("{way:?}");
        let instance = emulator();
        let user_pool_id = pool(
            &instance.router,
            &format!("start-after-start-{index}"),
            &json!({}),
        )
        .await;
        // The file the job is handed is what earns the run its terminal status: a
        // header row of this pool's own import template completes the run, and one
        // the template does not name stops it with an error.
        let job_id = match way {
            StartedJob::Failed => {
                job_with_file(
                    &instance,
                    &user_pool_id,
                    "start-after-start-job",
                    "not,a,header,of,this,pool\nrecord\n".to_string(),
                )
                .await
            }
            StartedJob::InProgress | StartedJob::Succeeded => {
                import_job(&instance, &user_pool_id, "start-after-start-job").await
            }
        };
        let body = json!({ "UserPoolId": user_pool_id, "JobId": job_id });
        ok(&instance.router, "StartUserImportJob", &body).await;

        // The moves the clock owes a started job are PLANTED rather than waited
        // out: the stored instants are moved so the move this row is about is
        // already due and the next one is not, which is what puts the job in this
        // row's status without the test depending on how fast the host runs.
        {
            let service_state =
                state::service_state(&*instance.engine).expect("this service's state");
            let mut guard = service_state.lock();
            let pool = guard.pool_mut(&user_pool_id).expect("the created pool");
            let job = pool
                .user_import_jobs
                .get_mut(&job_id)
                .expect("the started job");
            let in_progress_at = job
                .in_progress_at
                .expect("a started job owes its import move");
            let completed_at = job.completed_at.expect("a started job owes its completion");
            match way {
                StartedJob::InProgress => {
                    let import_due = in_progress_at - (state::JOB_START_SPAN + PLANTED_STEP);
                    job.in_progress_at = Some(import_due);
                    job.completed_at = Some(completed_at + PLANTED_HORIZON);
                }
                StartedJob::Succeeded | StartedJob::Failed => {
                    let completion_due = completed_at - (state::JOB_IMPORT_SPAN + PLANTED_STEP);
                    job.completed_at = Some(completion_due);
                }
            }
        }

        let before = job_status(&instance, &user_pool_id, &job_id).await;
        let expected_status = way.status();
        assert_eq!(
            before, expected_status,
            "case {case:?}: this row needs a job in {expected_status}, got {before}"
        );
        let (status, payload) = dispatch(&instance.router, "StartUserImportJob", &body, true).await;
        assert_eq!(
            (status, error_code(&payload)),
            (REFUSED, "PreconditionNotMetException"),
            "case {case:?}: expected a {REFUSED} PreconditionNotMetException, \
             got {status} with {payload}"
        );
        let expected = not_in_created_state(&job_id);
        let message = error_message(&payload);
        assert_eq!(
            message, expected,
            "case {case:?}: expected {expected:?}, got {message:?}"
        );
        // The same sentence whatever the run reached: the status the job is in is
        // not part of it, and neither is the pool.
        assert!(
            !message.contains(user_pool_id.as_str()) && !message.contains(before.as_str()),
            "case {case:?}: the sentence must quote neither the pool nor the status, \
             got {message:?}"
        );

        // The refused start begins no second import: the job reports the status it
        // already had, and the run its first start made is the one it reports.
        let after = job_status(&instance, &user_pool_id, &job_id).await;
        assert_eq!(
            after, expected_status,
            "case {case:?}: a refused start must leave the job in {expected_status}, got {after}"
        );
    }
}
