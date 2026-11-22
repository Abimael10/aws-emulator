// SPDX-License-Identifier: CC0-1.0

//! One account, four services: a request signed with an access key id the account NO LONGER
//! HOLDS is refused as an UNKNOWN CREDENTIAL by every service it is presented to, each in its
//! own error vocabulary.
//!
//! RULE: when the access key id a request presents resolves to no credential the emulator
//! holds, authentication failed — no principal was resolved, so no policy was read — and the
//! service answers its own unknown-credential error with AWS's fixed sentence for that
//! condition, never the authorization denial a KNOWN principal gets for an action its
//! policies do not permit. A client must tell the two apart: the unknown-credential class is
//! what makes an SDK's credential provider re-resolve its chain, where an authorization
//! denial tells it the credential stands and only the grant is missing, so a caller whose key
//! was deleted would otherwise retry forever against a refusal it reads as a policy problem.
//!
//! The codes are each service's own, because the names differ by protocol family and by
//! service; the sentences are the fixed text AWS's SigV4 layer produces for the condition.
//! Only a cross-service suite can pin it: the key is minted and deleted by IAM's own
//! operations, and what the other three services answer a request presenting it is decided in
//! front of their handlers, so no crate-local suite can drive one side into the other's
//! answer.
//!
//! The rows differ along what the rule generalises over: the service presented the credential,
//! its protocol family and wire form (an `awsQuery` form body, a `restXml` GET, an `awsJson1_1`
//! target), and the operation called. Beside the table, two boundaries: the same key is shown
//! to AUTHENTICATE immediately before it is deleted, so what the rows observe is the deletion
//! and not a broken endpoint; and a key id the account NEVER held is refused exactly the same
//! way, because what the rule turns on is that the key resolves to nothing rather than how it
//! came to.

use std::sync::Arc;
use std::time::SystemTime;

use aws_credential_types::Credentials;
use aws_sigv4::http_request::{sign, SignableBody, SignableRequest, SigningSettings};
use aws_sigv4::sign::v4::SigningParams;
use smithy_server_runtime::MultiServiceRouter;

use crate::query_wire::{form_encode, opt_element};

/// The Region every call below is signed for.
const REGION: &str = "us-east-1";

/// The sentence AWS's SigV4 layer produces for a credential that resolves to nothing: fixed
/// text with no variable part, so a caller presenting an unknown key learns nothing about the
/// account it was presented to.
const SECURITY_TOKEN_INVALID: &str = "The security token included in the request is invalid.";

/// The sentence S3 publishes for the same condition in its own error list ("InvalidAccessKeyId
/// — The AWS access key ID that you provided does not exist in our records"), likewise fixed
/// text: the rejected key id is not echoed back.
const S3_UNKNOWN_KEY: &str = "The AWS Access Key Id you provided does not exist in our records.";

/// The authorization denial none of these refusals may carry: it is the answer a principal the
/// service DID resolve gets for an action its policies do not permit, and no request below ever
/// named a principal whose policies could be read.
const AUTHORIZATION_DENIAL: &str = "AccessDenied";

/// An access key id of the issued form that this account never held.
const NEVER_HELD_KEY_ID: &str = "AKIAIOSFODNN7EXAMPLE";

/// A secret of the issued length, for the key id above: nothing resolves it, so what it signs
/// is never recomputed.
const NEVER_HELD_SECRET: &str = "wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY";

/// The credential one request presents: the access key id its credential scope names, and the
/// secret it is signed with.
///
/// Deliberately not `Debug`: the secret half of a minted pair must not be formattable into a
/// test's output by an assertion that happens to name the whole credential.
struct Presented {
    access_key_id: String,
    secret_access_key: String,
}

/// How one service's call is put on the wire — the three protocol families these four
/// endpoints speak.
#[derive(Clone, Copy, Debug)]
enum Call {
    /// `awsQuery`: `POST /` with the action and version in a form body, answered in an XML
    /// error document.
    Query {
        action: &'static str,
        version: &'static str,
    },
    /// `restXml`: the operation IS the method and path, answered in an XML error document.
    Rest {
        method: &'static str,
        path: &'static str,
    },
    /// `awsJson1_1`: the operation named in `X-Amz-Target`, answered in a JSON error document.
    Json { target: &'static str },
}

/// One service as a row names it: where the call is addressed and signed, how it travels, and
/// the code and sentence that service answers an unknown credential with.
#[derive(Clone, Copy, Debug)]
struct Endpoint {
    case: &'static str,
    signing_name: &'static str,
    host: &'static str,
    call: Call,
    unknown_credential: (&'static str, &'static str),
}

/// The four endpoints the one credential is presented to, each with its own vocabulary for a
/// credential that resolves to nothing.
const ENDPOINTS: [Endpoint; 4] = [
    Endpoint {
        case: "sts GetCallerIdentity",
        signing_name: "sts",
        host: "sts.amazonaws.com",
        call: Call::Query {
            action: "GetCallerIdentity",
            version: "2011-06-15",
        },
        unknown_credential: ("InvalidClientTokenId", SECURITY_TOKEN_INVALID),
    },
    Endpoint {
        case: "s3 ListBuckets",
        signing_name: "s3",
        host: "s3.us-east-1.amazonaws.com",
        call: Call::Rest {
            method: "GET",
            path: "/",
        },
        unknown_credential: ("InvalidAccessKeyId", S3_UNKNOWN_KEY),
    },
    Endpoint {
        case: "kms ListKeys",
        signing_name: "kms",
        host: "kms.us-east-1.amazonaws.com",
        call: Call::Json {
            target: "TrentService.ListKeys",
        },
        unknown_credential: ("UnrecognizedClientException", SECURITY_TOKEN_INVALID),
    },
    Endpoint {
        case: "secretsmanager ListSecrets",
        signing_name: "secretsmanager",
        host: "secretsmanager.us-east-1.amazonaws.com",
        call: Call::Json {
            target: "secretsmanager.ListSecrets",
        },
        unknown_credential: ("UnrecognizedClientException", SECURITY_TOKEN_INVALID),
    },
];

/// The emulator with the five services this rule spans mounted on ONE engine and request
/// authentication ON — the configuration the emulator serves traffic in, and the only one in
/// which a credential is resolved at all — together with the key pair this process publishes,
/// which the fixture calls are signed with.
fn authenticating_emulator() -> (MultiServiceRouter, Presented) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(true);
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_sts_awssecuritytokenservicev20110615::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(service_com_amazonaws_s3_amazons3::EmulatedService::new(
        Arc::clone(&engine),
    ));
    router.register_service(
        service_com_amazonaws_kms_trentservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_secretsmanager_secretsmanager::EmulatedService::new(engine),
    );
    let published = ::emulator_server::SigV4Authenticator::minted_credentials()
        .expect("turning request authentication on publishes this process's keys");
    let account = Presented {
        access_key_id: published.access_key_id.clone(),
        secret_access_key: published.secret_access_key.clone(),
    };
    (router, account)
}

/// Sign `request` for `signing_name` in [`REGION`] with `credential`, in the `Authorization`
/// header a signing client writes — the library's own signer, so what the emulator verifies is
/// a real SigV4 signature rather than one written here.
fn signed(
    mut request: http::Request<Vec<u8>>,
    signing_name: &str,
    credential: &Presented,
) -> http::Request<Vec<u8>> {
    let identity = Credentials::new(
        &credential.access_key_id,
        &credential.secret_access_key,
        None,
        None,
        "test",
    )
    .into();
    let params = SigningParams::builder()
        .identity(&identity)
        .region(REGION)
        .name(signing_name)
        .time(SystemTime::now())
        .settings(SigningSettings::default())
        .build()
        .expect("the signing parameters are complete")
        .into();
    let (method, uri) = (request.method().to_string(), request.uri().to_string());
    let headers: Vec<(String, String)> = request
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().expect("a text header").to_string(),
            )
        })
        .collect();
    let signable = SignableRequest::new(
        &method,
        &uri,
        headers
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str())),
        SignableBody::Bytes(request.body()),
    )
    .expect("the request is signable");
    let (instructions, _) = sign(signable, &params)
        .expect("the request signs")
        .into_parts();
    instructions.apply_to_request_http1x(&mut request);
    request
}

/// Dispatch one request, built and signed as `endpoint` and `credential` say.
async fn presented_to(
    router: &MultiServiceRouter,
    endpoint: Endpoint,
    credential: &Presented,
) -> (u16, String) {
    let Endpoint {
        signing_name,
        host,
        call,
        ..
    } = endpoint;
    let (method, path, body) = match call {
        Call::Query { action, version } => (
            "POST",
            "/".to_string(),
            format!("Action={action}&Version={version}"),
        ),
        Call::Rest { method, path } => (method, path.to_string(), String::new()),
        Call::Json { .. } => ("POST", "/".to_string(), "{}".to_string()),
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    builder = match call {
        Call::Query { .. } => builder.header("content-type", "application/x-www-form-urlencoded"),
        Call::Rest { .. } => builder,
        Call::Json { target } => builder
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", target),
    };
    let built = builder
        .body(body.into_bytes())
        .expect("a well-formed request");
    let (parts, payload) = signed(built, signing_name, credential).into_parts();
    let response = router
        .dispatch(http::Request::from_parts(
            parts,
            bytes::Bytes::from(payload),
        ))
        .await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// The error class and sentence `body` carries, read in the envelope `call`'s protocol family
/// answers in, or `None` for a body that is no error document at all.
///
/// An awsJson class may be qualified by the namespace the shape is declared in, which is not
/// part of what a client branches on — an SDK reads the class alone — so the qualification is
/// dropped here the way an SDK drops it.
fn refusal(call: Call, body: &str) -> Option<(String, String)> {
    match call {
        Call::Json { .. } => {
            let document: serde_json::Value = serde_json::from_str(body).ok()?;
            let class = document.get("__type")?.as_str()?;
            let class = class.split('#').next_back()?;
            let message = document
                .get("message")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default();
            Some((class.to_string(), message.to_string()))
        }
        Call::Query { .. } | Call::Rest { .. } => {
            Some((opt_element(body, "Code")?, opt_element(body, "Message")?))
        }
    }
}

/// One IAM action, signed with the account's own published credential.
async fn iam(
    router: &MultiServiceRouter,
    account: &Presented,
    action: &str,
    parameters: &[(&str, String)],
) -> (u16, String) {
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (name, value) in parameters {
        body.push('&');
        body.push_str(name);
        body.push('=');
        body.push_str(&form_encode(value));
    }
    let built = http::Request::builder()
        .method("POST")
        .uri("https://iam.amazonaws.com/")
        .header("host", "iam.amazonaws.com")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(body.into_bytes())
        .expect("a well-formed awsQuery request");
    let (parts, payload) = signed(built, "iam", account).into_parts();
    let response = router
        .dispatch(http::Request::from_parts(
            parts,
            bytes::Bytes::from(payload),
        ))
        .await;
    (
        response.status().as_u16(),
        String::from_utf8_lossy(response.body()).into_owned(),
    )
}

/// Create one IAM user, grant it every action — what the rows are about is the CREDENTIAL, so
/// nothing below may turn on a missing permission — and mint it an access key.
async fn given_a_user_with_a_key(
    router: &MultiServiceRouter,
    account: &Presented,
    user_name: &str,
) -> Presented {
    let (status, body) = iam(
        router,
        account,
        "CreateUser",
        &[("UserName", user_name.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case 'the fixture user': expected HTTP 200 from CreateUser, got {status} {body}"
    );
    let (status, body) = iam(
        router,
        account,
        "PutUserPolicy",
        &[
            ("UserName", user_name.to_string()),
            ("PolicyName", "allow-every-action".to_string()),
            (
                "PolicyDocument",
                r#"{"Version":"2012-10-17","Statement":[{"Effect":"Allow","Action":"*","Resource":"*"}]}"#
                    .to_string(),
            ),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case 'the fixture grant': expected HTTP 200 from PutUserPolicy, got {status} {body}"
    );
    let (status, body) = iam(
        router,
        account,
        "CreateAccessKey",
        &[("UserName", user_name.to_string())],
    )
    .await;
    assert_eq!(
        status, 200,
        "case 'the minted key': expected HTTP 200 from CreateAccessKey, got status {status}"
    );
    // The answer carries CREDENTIAL MATERIAL, so no failure here quotes the body: a missing
    // member is reported by name.
    let minted = |name: &str| {
        opt_element(&body, name).unwrap_or_else(|| {
            panic!("case 'the minted key': the answer carries no <{name}> element")
        })
    };
    Presented {
        access_key_id: minted("AccessKeyId"),
        secret_access_key: minted("SecretAccessKey"),
    }
}

/// Every endpoint's answer to a request signed with `credential` must be its own
/// unknown-credential refusal.
async fn each_endpoint_refuses_the_credential(
    router: &MultiServiceRouter,
    credential: &Presented,
    presenting: &str,
) {
    for endpoint in ENDPOINTS {
        let case = format!("{presenting}, {}", endpoint.case);
        let (status, body) = presented_to(router, endpoint, credential).await;
        assert_eq!(
            status, 403,
            "case {case:?}: a credential that resolves to nothing is refused HTTP 403, got \
             {status} with body {body:?}"
        );
        let answered = refusal(endpoint.call, &body);
        let answered = answered.unwrap_or_else(|| {
            panic!("case {case:?}: the refusal carries no error class and sentence: {body:?}")
        });
        let (expected_code, expected_message) = endpoint.unknown_credential;
        assert_eq!(
            (answered.0.as_str(), answered.1.as_str()),
            (expected_code, expected_message),
            "case {case:?}: expected {expected_code:?} with AWS's own sentence, got {answered:?}"
        );
        assert_ne!(
            answered.0.as_str(),
            AUTHORIZATION_DENIAL,
            "case {case:?}: an authentication failure must not be answered as an authorization \
             denial, got {answered:?}"
        );
        assert!(
            !body.contains(&credential.access_key_id),
            "case {case:?}: the refusal must not echo the rejected key id, got {body:?}"
        );
    }
}

/// RULE: once `iam:DeleteAccessKey` has withdrawn a key, every service refuses a request
/// signed with it as an unknown credential in its own vocabulary — and the same key
/// authenticates at each of them immediately before the delete, so what the rows observe is
/// the withdrawal.
#[tokio::test]
async fn a_deleted_access_key_is_an_unknown_credential_at_every_service() {
    let (router, account) = authenticating_emulator();
    let user_name = "withdrawn-key-user";
    let key = given_a_user_with_a_key(&router, &account, user_name).await;

    // The boundary: the minted key RESOLVES, so each endpoint serves it. Whatever each
    // operation then answers, it is never the unknown-credential refusal below.
    for endpoint in ENDPOINTS {
        let case = format!("the key as it was minted, {}", endpoint.case);
        let (status, body) = presented_to(&router, endpoint, &key).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the minted key must authenticate and be served, got {status} with \
             body {body:?}"
        );
    }

    let (status, body) = iam(
        &router,
        &account,
        "DeleteAccessKey",
        &[
            ("UserName", user_name.to_string()),
            ("AccessKeyId", key.access_key_id.clone()),
        ],
    )
    .await;
    assert_eq!(
        status, 200,
        "case 'the delete': expected HTTP 200 from DeleteAccessKey, got {status} {body}"
    );

    each_endpoint_refuses_the_credential(&router, &key, "a key the account deleted").await;
}

/// RULE (the same one, from the other direction): what the refusal turns on is that the key id
/// resolves to NO credential, not how it came to — so a key id the account never held is
/// refused exactly as a deleted one is.
#[tokio::test]
async fn an_access_key_id_the_account_never_held_is_an_unknown_credential_too() {
    let (router, _account) = authenticating_emulator();
    let never_held = Presented {
        access_key_id: NEVER_HELD_KEY_ID.to_string(),
        secret_access_key: NEVER_HELD_SECRET.to_string(),
    };
    each_endpoint_refuses_the_credential(&router, &never_held, "a key id never held").await;
}
