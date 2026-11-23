// SPDX-License-Identifier: CC0-1.0

//! An `X-Amz-Target` attached to one of this service's requests, driven end to end over the
//! awsQuery wire with request authentication ON — the configuration the emulator serves
//! traffic in.
//!
//! RULE: this service's protocol takes the operation from the form-encoded body's `Action`,
//! so `X-Amz-Target` — the header the `awsJson` protocols name an operation out of band in —
//! is not a signal it routes on. A request in this protocol's wire form reaches this service
//! whatever target travels beside it, is served as the operation its BODY names, and is then
//! answered by the front door the same way as the identical request without the header: a
//! signed one is served, and one presenting no credential at all is refused
//! `MissingAuthenticationToken`. A target naming ANOTHER service is still that service's, so
//! the claim takes nobody else's traffic.
//!
//! Before this, such a request resolved to the target's prefix — a routing key no query
//! service answers to — so the emulator's front door found no service for it and answered a
//! `404 UnknownService` JSON document an awsQuery client cannot even parse, for an endpoint
//! that is emulated and an operation that is implemented. The authentication check never ran,
//! because nothing had been routed to run it for.
//!
//! The rows differ along what the rule generalises over: whether a target is present, the
//! FORM its prefix is written in (the service shape's local name as an SDK publishes one, the
//! same name cased differently, the fully qualified Coral name whose prefix carries dots of
//! its own), which operation the target names (one of this service's, and one no model
//! binds), which operation the BODY names, and what the request presents (a credential this
//! emulator issued, or none at all).

use std::sync::Arc;

use emulator_server::request_authentication::SigV4Authenticator;
use service_com_amazonaws_sts_awssecuritytokenservicev20110615 as sts;
use service_common::sigv4_presentation::MISSING_CREDENTIAL_MESSAGE;
use smithy_server_runtime::MultiServiceRouter;

use super::sts_sessions::{element, emulator_account, STS_API_VERSION, STS_SIGNING_NAME};

/// The Region every signed call below is signed for.
const REGION: &str = "us-east-1";

/// The code this service answers a request that presented no credential at all with.
const MISSING_CREDENTIAL_CODE: &str = "MissingAuthenticationToken";

/// The code the emulator's own front door answers a request it could resolve no service for
/// — the router's, in its own JSON envelope, and not a code any AWS model declares. No
/// request whose `Action` and endpoint name this service may be answered with it.
const UNKNOWN_SERVICE: &str = "UnknownService";

/// The emulator's own address, which a client configured against it reaches it on and which
/// names no service of its own.
const LOOPBACK_HOST: &str = "127.0.0.1:9297";

/// The target prefix an SDK publishes for this service: the service shape's local name
/// (`smithy_query(query="service_info")` -> the `com.amazonaws.sts#…` service shape).
const OWN_PREFIX: &str = "AWSSecurityTokenServiceV20110615";

/// What one request presents to authenticate with.
enum Presentation {
    /// SigV4-signed with the credential this emulator published at start.
    Signed,
    /// No `Authorization` header and no presigned credential: nothing to resolve.
    Unsigned,
}

/// What the emulator must answer one row with.
enum Answer {
    /// Served by the operation the BODY named: the awsQuery envelope's result element.
    Served(&'static str),
    /// Refused by the front door with this code and AWS's own sentence for it.
    Refused(&'static str, &'static str),
    /// Claimed by no service here, which is the router's own refusal.
    Unclaimed,
}

/// A router serving this service alone with request authentication ON, and the credential the
/// emulator published when it was turned on — the one credential a signed row presents.
fn authenticating_emulator() -> (MultiServiceRouter, &'static SigV4Authenticator) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(true);
    router.register_service(sts::EmulatedService::new(Arc::clone(&engine)));
    let published = SigV4Authenticator::minted_credentials()
        .expect("turning request authentication on publishes this process's keys");
    (router, published)
}

/// Dispatch one awsQuery call whose body names `action`, carrying `target` when the row
/// attaches one, presented as `presentation` says.
async fn call(
    router: &MultiServiceRouter,
    target: Option<&str>,
    action: &str,
    presentation: &Presentation,
    published: &SigV4Authenticator,
) -> (u16, String) {
    let body = format!("Action={action}&Version={STS_API_VERSION}");
    let mut builder = http::Request::builder()
        .method("POST")
        .uri(format!("https://{LOOPBACK_HOST}/"))
        .header("host", LOOPBACK_HOST)
        .header("content-type", "application/x-www-form-urlencoded");
    if let Some(target) = target {
        builder = builder.header("x-amz-target", target);
    }
    let mut request = builder
        .body(body.into_bytes())
        .expect("a well-formed awsQuery request");
    if matches!(presentation, Presentation::Signed) {
        service_common::in_process_sigv4::sign_in_process_request_as(
            &mut request,
            STS_SIGNING_NAME,
            REGION,
            &published.access_key_id,
            &published.secret_access_key,
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

/// One row: the target attached to the request, the operation its body names, what it
/// presents, and the answer it must get.
struct Row {
    case: &'static str,
    target: Option<String>,
    action: &'static str,
    presentation: Presentation,
    answer: Answer,
}

/// RULE: the operation comes from the body's `Action` and the answer from the credential the
/// request presented, whatever `X-Amz-Target` travels beside them.
#[tokio::test]
async fn a_query_request_is_served_by_its_action_whatever_target_it_carries() {
    let (router, published) = authenticating_emulator();
    let account = emulator_account();
    let rows = vec![
        Row {
            case: "a signed call with no target at all",
            target: None,
            action: "GetCallerIdentity",
            presentation: Presentation::Signed,
            answer: Answer::Served("GetCallerIdentityResult"),
        },
        Row {
            case: "a signed call carrying this service's published target",
            target: Some(format!("{OWN_PREFIX}.GetCallerIdentity")),
            action: "GetCallerIdentity",
            presentation: Presentation::Signed,
            answer: Answer::Served("GetCallerIdentityResult"),
        },
        Row {
            case: "the same target cased as no model spells it",
            target: Some(format!("{}.GetCallerIdentity", OWN_PREFIX.to_lowercase())),
            action: "GetCallerIdentity",
            presentation: Presentation::Signed,
            answer: Answer::Served("GetCallerIdentityResult"),
        },
        Row {
            // A fully qualified Coral prefix: its dots are part of the name, not operation
            // separators.
            case: "a target naming this service by its fully qualified name",
            target: Some(format!("com.amazonaws.sts.{OWN_PREFIX}.GetCallerIdentity")),
            action: "GetCallerIdentity",
            presentation: Presentation::Signed,
            answer: Answer::Served("GetCallerIdentityResult"),
        },
        Row {
            // The target's operation segment is not read: the body decides, so a target
            // naming ANOTHER operation of this service does not change which one serves.
            case: "a target naming one operation while the body names another",
            target: Some(format!("{OWN_PREFIX}.GetSessionToken")),
            action: "GetCallerIdentity",
            presentation: Presentation::Signed,
            answer: Answer::Served("GetCallerIdentityResult"),
        },
        Row {
            case: "a target naming an operation no model binds",
            target: Some(format!("{OWN_PREFIX}.NotAnOperation")),
            action: "GetCallerIdentity",
            presentation: Presentation::Signed,
            answer: Answer::Served("GetCallerIdentityResult"),
        },
        Row {
            // Another action, resolved from the body under the same target: the body is read
            // for every call and not only for the one the target happens to agree with.
            case: "a target naming this service, with the body naming a minting call",
            target: Some(format!("{OWN_PREFIX}.GetCallerIdentity")),
            action: "GetSessionToken",
            presentation: Presentation::Signed,
            answer: Answer::Served("GetSessionTokenResult"),
        },
        Row {
            // The recorded shape of the defect: no credential at all, with a target
            // attached. The answer is the front door's, in this service's envelope.
            case: "an unsigned call carrying this service's published target",
            target: Some(format!("{OWN_PREFIX}.GetCallerIdentity")),
            action: "GetCallerIdentity",
            presentation: Presentation::Unsigned,
            answer: Answer::Refused(MISSING_CREDENTIAL_CODE, MISSING_CREDENTIAL_MESSAGE),
        },
        Row {
            // The same request without the header, so the refusal is the credential's and
            // not the routing's.
            case: "an unsigned call with no target at all",
            target: None,
            action: "GetCallerIdentity",
            presentation: Presentation::Unsigned,
            answer: Answer::Refused(MISSING_CREDENTIAL_CODE, MISSING_CREDENTIAL_MESSAGE),
        },
        Row {
            // A target naming another service is that service's, so this claim takes none of
            // its traffic: with only this service mounted, nobody claims it.
            case: "an unsigned call targeted at another service",
            target: Some("AWSCognitoIdentityService.GetId".to_string()),
            action: "GetCallerIdentity",
            presentation: Presentation::Unsigned,
            answer: Answer::Unclaimed,
        },
    ];

    for Row {
        case,
        target,
        action,
        presentation,
        answer,
    } in rows
    {
        let (status, body) = call(
            &router,
            target.as_deref(),
            action,
            &presentation,
            published,
        )
        .await;
        match answer {
            Answer::Served(result_element) => {
                assert_eq!(
                    status, 200,
                    "case {case:?}: the call must be served, got {status}: {body}"
                );
                assert!(
                    body.contains(&format!("<{result_element}>")),
                    "case {case:?}: expected the <{result_element}> the body's Action names, \
                     got {body}"
                );
                assert!(
                    !body.contains(UNKNOWN_SERVICE),
                    "case {case:?}: a served call must not be the router's refusal, got {body}"
                );
            }
            Answer::Refused(expected_code, expected_message) => {
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
                assert!(
                    !body.contains(&account),
                    "case {case:?}: a refusal must disclose no identity, got {body}"
                );
            }
            Answer::Unclaimed => {
                assert_eq!(
                    status, 404,
                    "case {case:?}: a request for another service must reach nobody here, \
                     got {status}: {body}"
                );
                assert!(
                    body.contains(UNKNOWN_SERVICE),
                    "case {case:?}: expected the router's own refusal, got {body}"
                );
            }
        }
    }
}
