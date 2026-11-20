// SPDX-License-Identifier: CC0-1.0

//! How this service words a request the emulator's SigV4 FRONT DOOR refused, driven end to
//! end over the awsQuery wire with request authentication ON — the configuration the emulator
//! serves traffic in.
//!
//! RULE: the front door answers each way of failing authentication with the class AWS names
//! for THAT failure, and the three are kept apart:
//!
//! * a request that presents NO credential — neither SigV4 presentation, so there was never a
//!   caller to look up — is `MissingAuthenticationToken`, the one class that says "you sent no
//!   credentials" and sends a client to credential DISCOVERY;
//! * a credential the emulator will not resolve — an access key id it never issued, and a
//!   SECURITY TOKEN that is not the one issued for the credential presenting it — is
//!   `InvalidClientTokenId`, the class an SDK's credential-refresh path keys on, whatever the
//!   signature over the request says. AWS reports the two from different stages of its front
//!   door, so each carries that stage's own fixed sentence: the one for a token that was
//!   presented and resolved to nothing, and the one for a key id that resolves to no usable
//!   credential — which is what a temporary credential presented with NO token is;
//! * only a credential that DOES resolve, whose signature does not verify under its secret, is
//!   `SignatureDoesNotMatch`, which points the operator at the secret half of the pair.
//!
//! Every one of them is HTTP 403 in this service's `awsQuery` error envelope, and the
//! sentences are the fixed text AWS's SigV4 layer produces, read from the module that owns
//! them (`service_common::authentication_refusal`,
//! `service_common::sigv4_presentation`) because the wording belongs to the whole tree and
//! only the CODES are this service's own.
//!
//! The rows differ along what the rule generalises over: WHAT the request presented (nothing,
//! another `Authorization` scheme, a key id that was never issued, a live session, a live
//! session whose token was altered, a live session whose token belongs to another session, a
//! live session with no token at all), HOW the request is addressed (this service's endpoint
//! host, and the emulator's own loopback address, which names no service), and whether the
//! signature verifies. A genuinely issued session opens and closes the table, so each refusal
//! is shown against a credential that authenticates immediately before and after it and the
//! endpoint is not simply broken.

use std::sync::Arc;

use emulator_server::request_authentication::SigV4Authenticator;
use service_com_amazonaws_iam_awsidentitymanagementv20100508 as iam;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;
use service_common::authentication_refusal::{
    INVALID_SECURITY_TOKEN_MESSAGE, SIGNATURE_MISMATCH_MESSAGE, UNRESOLVED_CREDENTIAL_MESSAGE,
};
use service_common::sigv4_presentation::MISSING_CREDENTIAL_MESSAGE;
use smithy_server_runtime::MultiServiceRouter;

use super::sts_sessions::{element, emulator_account, member, STS_API_VERSION, STS_SIGNING_NAME};

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The three codes this service answers the three authentication failures with.
const MISSING_CREDENTIAL_CODE: &str = "MissingAuthenticationToken";
const UNRESOLVED_CREDENTIAL_CODE: &str = "InvalidClientTokenId";
const SIGNATURE_MISMATCH_CODE: &str = "SignatureDoesNotMatch";

/// The authorization denial, which no row below may be answered: it is what a RESOLVED
/// principal gets for an action its policies do not permit, and none of these requests ever
/// named a principal whose policies could be read.
const AUTHORIZATION_DENIAL: &str = "AccessDenied";

/// An access key id of the issued form that this emulator never minted.
const NEVER_ISSUED_KEY_ID: &str = "ASIAIOSFODNN7EXAMPLE";

/// This service's endpoint host, which names the service a request is for, and the
/// emulator's own address, which names no service at all — the two ways a client reaches
/// this endpoint.
const SERVICE_HOST: &str = "sts.amazonaws.com";
const LOOPBACK_HOST: &str = "127.0.0.1:9297";

/// The call every row makes: the one operation whose model lists no error of its own, so
/// everything these rows observe is the front door's own answer.
const CALL: &str = "GetCallerIdentity";

/// The credential one request presents: an access key id, the secret it is signed with, and
/// the session token that travels with a temporary credential.
#[derive(Clone, Debug)]
struct Presented {
    access_key_id: String,
    secret_access_key: String,
    session_token: Option<String>,
}

/// One character of `value` replaced, which is a value of exactly the issued FORM that names
/// nothing this emulator holds.
fn one_character_off(value: &str) -> String {
    let mut characters: Vec<char> = value.chars().collect();
    let middle = characters.len() / 2;
    let held = characters[middle];
    characters[middle] = if held == 'A' { 'B' } else { 'A' };
    characters.into_iter().collect()
}

/// A router serving STS beside IAM on one shared engine with request authentication ON, and
/// the account's own published credential: the one credential a refusal has RESOLVED, which
/// the sessions below are minted with.
fn authenticating_emulator() -> (MultiServiceRouter, Presented) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(true);
    router.register_service(iam::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(sts::EmulatedService::new(Arc::clone(&engine)));
    let published = SigV4Authenticator::minted_credentials()
        .expect("turning request authentication on publishes this process's keys");
    let account = Presented {
        access_key_id: published.access_key_id.clone(),
        secret_access_key: published.secret_access_key.clone(),
        session_token: None,
    };
    (router, account)
}

/// How a request presents itself: signed with a credential, or carrying no SigV4
/// presentation at all.
#[derive(Clone, Debug)]
enum Presentation {
    /// SigV4-signed with this credential, the way a client signs: the session token travels
    /// beside the signature and is signed over with everything else.
    Signed(Presented),
    /// No SigV4 presentation: no `Authorization` header at all, or one naming another
    /// authentication scheme.
    Unsigned(Option<&'static str>),
}

/// Dispatch one awsQuery `GetCallerIdentity` to `host`, presented as `presentation` says.
async fn caller_identity(
    router: &MultiServiceRouter,
    host: &str,
    presentation: &Presentation,
) -> (u16, String) {
    let body = format!("Action={CALL}&Version={STS_API_VERSION}");
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}/"))
        .header("host", host)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Presentation::Signed(credential) = presentation {
        if let Some(token) = &credential.session_token {
            builder = builder.header("x-amz-security-token", token.as_str());
        }
    }
    if let Presentation::Unsigned(Some(scheme)) = presentation {
        builder = builder.header("authorization", *scheme);
    }
    let mut request = builder
        .body(body.into_bytes())
        .expect("a well-formed awsQuery request");
    if let Presentation::Signed(credential) = presentation {
        service_common::in_process_sigv4::sign_in_process_request_as(
            &mut request,
            STS_SIGNING_NAME,
            REGION,
            &credential.access_key_id,
            &credential.secret_access_key,
        )
        .await
        .expect("the request signs");
    }
    let (parts, payload) = request.into_parts();
    let response = router
        .dispatch(http::Request::from_parts(parts, bytes::Bytes::from(payload)))
        .await;
    let status = response.status().as_u16();
    (status, String::from_utf8_lossy(response.body()).to_string())
}

/// Mint one session with `GetSessionToken`, signed with the account's own credential, and
/// answer the three values it hands back.
async fn given_session(router: &MultiServiceRouter, account: &Presented, case: &str) -> Presented {
    let body = format!("Action=GetSessionToken&Version={STS_API_VERSION}&DurationSeconds=900");
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{SERVICE_HOST}/"))
        .header("host", SERVICE_HOST)
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.into_bytes())
        .expect("a well-formed awsQuery request");
    service_common::in_process_sigv4::sign_in_process_request_as(
        &mut request,
        STS_SIGNING_NAME,
        REGION,
        &account.access_key_id,
        &account.secret_access_key,
    )
    .await
    .expect("the request signs");
    let (parts, payload) = request.into_parts();
    let response = router
        .dispatch(http::Request::from_parts(parts, bytes::Bytes::from(payload)))
        .await;
    let status = response.status().as_u16();
    let body = String::from_utf8_lossy(response.body()).to_string();
    assert_eq!(
        status, 200,
        "case {case:?}: GetSessionToken must be served, got {status}: {body}"
    );
    Presented {
        access_key_id: member(case, &body, "AccessKeyId"),
        secret_access_key: member(case, &body, "SecretAccessKey"),
        session_token: Some(member(case, &body, "SessionToken")),
    }
}

/// One row: how the request presents itself, where it is addressed, and the answer AWS gives
/// it — `None` for the rows that must be SERVED.
struct Row {
    case: &'static str,
    host: &'static str,
    presentation: Presentation,
    refused: Option<(&'static str, &'static str)>,
}

/// RULE: a request that presents no credential is refused for the MISSING credential, one
/// presenting a credential the emulator will not resolve — including a security token that
/// belongs to no issued session — for the credential, and only one whose credential resolves
/// for the SIGNATURE.
#[tokio::test]
async fn each_front_door_failure_is_answered_with_the_class_aws_names_for_it() {
    let (router, account) = authenticating_emulator();
    let session = given_session(&router, &account, "the session every row presents").await;
    let other = given_session(&router, &account, "a second, unrelated session").await;
    let token = session
        .session_token
        .clone()
        .expect("a minted session carries its token");

    let with_token = |token: Option<String>| Presented {
        access_key_id: session.access_key_id.clone(),
        secret_access_key: session.secret_access_key.clone(),
        session_token: token,
    };

    let rows = vec![
        Row {
            case: "the session as it was issued",
            host: SERVICE_HOST,
            presentation: Presentation::Signed(session.clone()),
            refused: None,
        },
        Row {
            case: "that session's token with one character replaced",
            host: SERVICE_HOST,
            presentation: Presentation::Signed(with_token(Some(one_character_off(&token)))),
            refused: Some((UNRESOLVED_CREDENTIAL_CODE, INVALID_SECURITY_TOKEN_MESSAGE)),
        },
        Row {
            case: "that session's token truncated by its last character",
            host: SERVICE_HOST,
            presentation: Presentation::Signed(with_token(Some(
                token[..token.len() - 1].to_string(),
            ))),
            refused: Some((UNRESOLVED_CREDENTIAL_CODE, INVALID_SECURITY_TOKEN_MESSAGE)),
        },
        Row {
            // A token this emulator really DID issue, for another session: a token resolves
            // for the credential presenting it or for nothing.
            case: "another live session's token",
            host: SERVICE_HOST,
            presentation: Presentation::Signed(with_token(other.session_token.clone())),
            refused: Some((UNRESOLVED_CREDENTIAL_CODE, INVALID_SECURITY_TOKEN_MESSAGE)),
        },
        Row {
            // No token presented at all: what the caller offered is a key id that resolves to
            // no usable credential, which is the OTHER stage's sentence under the same code.
            case: "the session's key and secret with no token at all",
            host: SERVICE_HOST,
            presentation: Presentation::Signed(with_token(None)),
            refused: Some((UNRESOLVED_CREDENTIAL_CODE, UNRESOLVED_CREDENTIAL_MESSAGE)),
        },
        Row {
            case: "an access key id this emulator never issued",
            host: SERVICE_HOST,
            presentation: Presentation::Signed(Presented {
                access_key_id: NEVER_ISSUED_KEY_ID.to_string(),
                secret_access_key: session.secret_access_key.clone(),
                session_token: Some(token.clone()),
            }),
            refused: Some((UNRESOLVED_CREDENTIAL_CODE, UNRESOLVED_CREDENTIAL_MESSAGE)),
        },
        Row {
            // The credential resolves and the token is its own: what fails is the signature.
            case: "the session signed under a secret that is not its own",
            host: SERVICE_HOST,
            presentation: Presentation::Signed(Presented {
                access_key_id: session.access_key_id.clone(),
                secret_access_key: one_character_off(&session.secret_access_key),
                session_token: Some(token.clone()),
            }),
            refused: Some((SIGNATURE_MISMATCH_CODE, SIGNATURE_MISMATCH_MESSAGE)),
        },
        Row {
            case: "no Authorization header at all, on this service's endpoint host",
            host: SERVICE_HOST,
            presentation: Presentation::Unsigned(None),
            refused: Some((MISSING_CREDENTIAL_CODE, MISSING_CREDENTIAL_MESSAGE)),
        },
        Row {
            // The recorded request's own address: the emulator's own, which names no service,
            // so the call is claimed by this protocol's wire form.
            case: "no Authorization header at all, on the emulator's own address",
            host: LOOPBACK_HOST,
            presentation: Presentation::Unsigned(None),
            refused: Some((MISSING_CREDENTIAL_CODE, MISSING_CREDENTIAL_MESSAGE)),
        },
        Row {
            case: "an Authorization header naming another authentication scheme",
            host: SERVICE_HOST,
            presentation: Presentation::Unsigned(Some("Basic YWxpY2U6c2VjcmV0")),
            refused: Some((MISSING_CREDENTIAL_CODE, MISSING_CREDENTIAL_MESSAGE)),
        },
        Row {
            case: "the session as it was issued, after every refusal above",
            host: SERVICE_HOST,
            presentation: Presentation::Signed(session.clone()),
            refused: None,
        },
    ];

    let account_id = emulator_account();
    for Row {
        case,
        host,
        presentation,
        refused,
    } in rows
    {
        let (status, body) = caller_identity(&router, host, &presentation).await;
        match refused {
            None => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the call must be served, got {status}: {body}"
                );
                let arn = member(case, &body, "Arn");
                assert!(
                    arn.contains(&account_id),
                    "case {case:?}: the served identity must name this account, got {arn:?}"
                );
            }
            Some((expected_code, expected_message)) => {
                assert_eq!(
                    status, 403,
                    "case {case:?}: an authentication failure is HTTP 403, got {status}: {body}"
                );
                let code = element(&body, "Code").unwrap_or_default();
                let message = element(&body, "Message").unwrap_or_default();
                assert_eq!(
                    (code.as_str(), message.as_str()),
                    (expected_code, expected_message),
                    "case {case:?}: expected {expected_code} with AWS's own wording, got \
                     {code:?}: {message:?}"
                );
                assert_ne!(
                    code.as_str(),
                    AUTHORIZATION_DENIAL,
                    "case {case:?}: an authentication failure must not be answered as an \
                     authorization denial, got {body}"
                );
                assert!(
                    !body.contains(&account_id),
                    "case {case:?}: a refusal must disclose no identity, got {body}"
                );
            }
        }
    }
}
