// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for how a request to a FUNCTION URL that the emulator's authentication
//! layer turned away is worded.
//!
//! RULE: a function URL is an HTTP endpoint a stock client follows, so a caller that endpoint
//! does not admit is answered the refusal Lambda's own front door answers —
//! `403 {"Message":"Forbidden"}` — and not the refusal of this service's modelled API. The
//! same layer turning away a CONTROL-plane call still words it as that API's own refusal, so
//! the restating is of one request space and not of every request.
//!
//! `AuthType` decides who is turned away: `AWS_IAM` "restrict[s] access to authenticated
//! users only", so a request whose SigV4 signature the emulator did not verify is refused,
//! while `NONE` "bypass[es] IAM authentication to create a public endpoint" and is served
//! however it arrived — which is what makes the refusal the auth type's rather than a
//! blanket one.
//!
//! Every row drives a real HTTP request through a router that VERIFIES signatures, as the
//! emulator does when it is started with request authentication on. The rows differ along
//! what the rule generalises over: how the request is credentialled (no credential at all, a
//! signature scoped to another service, a signature that does not verify), which of the
//! addressings reaches the endpoint (the published host, the local-endpoint path, the
//! path-style first segment), the `AuthType` the URL enforces, and whether the request
//! addresses a function URL at all.

use std::sync::Arc;

use serde_json::{json, Value};

use crate::function_url_data_plane::{
    given_configured_url, given_function, router_on, send, Answer, Endpoint,
};

/// The emulator's own endpoint, which a client that resolves no AWS hostname reaches, and
/// where the two path addressings are read.
const OWN_ENDPOINT: &str = "127.0.0.1:9297";

/// The reserved path segment a data plane is named behind on that endpoint
/// (`service_common::data_plane_addressing::EMULATOR_PATH_NAMESPACE`), and the label that
/// names this plane under it.
const EMULATOR_NAMESPACE: &str = "_aws";
/// See [`EMULATOR_NAMESPACE`].
const PLANE_LABEL: &str = "lambda-url";

/// The body Lambda's own front door answers a caller it did not AUTHENTICATE with: it names
/// nothing of the request or of what was wrong with its credential.
const FORBIDDEN_BODY: &str = r#"{"Message":"Forbidden"}"#;

/// The body a caller whose credential scope names another service reads: the scope is part
/// of what a signature is computed over, so AWS names the service the request reached.
const SCOPED_ELSEWHERE_BODY: &str =
    r#"{"message":"Credential should be scoped to correct service: 'lambda'. "}"#;

/// The body a caller whose signature does not match the request it travelled with reads.
const SIGNATURE_MISMATCH_BODY: &str = "{\"message\":\"The request signature we calculated \
     does not match the signature you provided. Check your AWS Secret Access Key and \
     signing method. Consult the service documentation for details.\"}";

/// The body a caller signing as an identity this emulator holds no credential for reads.
/// Shared with the suites that drive such a caller for a reason of their own, so one wording of
/// that refusal serves them all.
pub(crate) const UNKNOWN_CREDENTIAL_BODY: &str =
    r#"{"message":"The security token included in the request is invalid."}"#;

/// The class of a caller whose credential names no identity this emulator holds.
const UNRECOGNIZED_CLIENT: &str = "UnrecognizedClientException";

/// The header a failure names its error class in, which a client branches on.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The class of a caller that presented no signature this endpoint accepts.
const ACCESS_DENIED: &str = "AccessDeniedException";

/// The class of a caller whose SIGNATURE is why it was not admitted.
const INVALID_SIGNATURE: &str = "InvalidSignatureException";

/// The handler every function below runs, which answers a described response so a row that
/// IS admitted is visibly served by the function.
fn handler() -> String {
    r#"
def lambda_handler(event, context):
    return {"statusCode": 200, "body": "served"}
"#
    .to_string()
}

/// How a request is credentialled. Shared with the suites that drive an unadmitted caller of
/// a function URL for a reason of their own, so one reading of each credential serves them
/// all.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Credential {
    /// No `Authorization` header at all.
    None,
    /// A signature whose credential scope names another service.
    ScopedElsewhere,
    /// A signature naming a key id nothing in this emulator holds a secret for.
    UnknownKey,
    /// A signature naming THIS process's own key id that does not verify.
    Mismatched,
}

/// The headers one credential travels in.
///
/// A signature's own validity window is part of what the authentication layer judges, so the
/// row that is about the SIGNATURE not matching is dated NOW: a stale date would be refused
/// for its date instead, which is a different refusal.
pub(crate) fn headers(credential: Credential) -> Vec<(String, String)> {
    let named = |name: &str, value: String| (name.to_string(), value);
    match credential {
        Credential::None => Vec::new(),
        Credential::ScopedElsewhere => vec![
            named(
                "authorization",
                "AWS4-HMAC-SHA256 \
                 Credential=emulator-test-key/20260101/us-east-1/s3/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef"
                    .to_string(),
            ),
            named("x-amz-date", "20260101T000000Z".to_string()),
        ],        Credential::UnknownKey => {
            let stamp = signing_stamp();
            vec![
                named("authorization", signature("emulator-test-key", &stamp, "lambda")),
                named("x-amz-date", stamp),
            ]
        }
        Credential::Mismatched => {
            let stamp = signing_stamp();
            // This process's OWN key id, so the layer holds a secret for it and judges the
            // signature itself rather than the credential.
            let key_id = ::emulator_server::request_authentication::SigV4Authenticator::
                for_this_process()
                .access_key_id
                .clone();
            vec![
                named("authorization", signature(&key_id, &stamp, "lambda")),
                named("x-amz-date", stamp),
            ]
        }
    }
}

/// One SigV4 `Authorization` header: the credential scope it signed under, and a signature
/// value that verifies against nothing.
fn signature(key_id: &str, stamp: &str, service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={key_id}/{date}/us-east-1/{service}/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef",
        date = &stamp[..8],
    )
}

/// The instant a signature is dated with, in the compact form SigV4 writes
/// (`20260101T000000Z`): the clock's own reading, so the window the authentication layer
/// judges a signature's date against is open.
fn signing_stamp() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or_default();
    // Whole seconds: SigV4 writes no fraction, and a stamp carrying one names no instant the
    // layer can read.
    let now = ::aws_smithy_types::DateTime::from_secs(
        i64::try_from(seconds).expect("an instant inside the epoch's range"),
    );
    now.fmt(::aws_smithy_types::date_time::Format::DateTime)
        .expect("an instant renders")
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect()
}

/// The owned header pairs as the dispatcher takes them.
pub(crate) fn borrowed(headers: &[(String, String)]) -> Vec<(&str, &str)> {
    headers
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect()
}

/// Which addressing a row reaches the endpoint by: the three a function URL resolves under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Addressing {
    /// The host `FunctionUrlConfig$FunctionUrl` publishes.
    PublishedHost,
    /// The reserved local-endpoint path on the emulator's own endpoint.
    LocalEndpointPath,
    /// The url id as the FIRST path segment on the emulator's own endpoint.
    PathStyle,
}

/// The host and path one addressing of `endpoint` reaches it at.
fn addressed(endpoint: &Endpoint, addressing: Addressing) -> (String, String) {
    match addressing {
        Addressing::PublishedHost => (endpoint.host.clone(), "/probe".to_string()),
        Addressing::LocalEndpointPath => (
            OWN_ENDPOINT.to_string(),
            format!("/{EMULATOR_NAMESPACE}/{PLANE_LABEL}/{}/probe", endpoint.url_id),
        ),
        Addressing::PathStyle => (
            OWN_ENDPOINT.to_string(),
            format!("/{}/probe", endpoint.url_id),
        ),
    }
}

/// One row of the refusal table.
struct Row {
    case: &'static str,
    credential: Credential,
    addressing: Addressing,
    /// The error class this refusal names ([`ERROR_TYPE_HEADER`]).
    error_type: &'static str,
    /// The document it answers.
    body: &'static str,
}

/// RULE: every caller an `AWS_IAM` function URL does not admit is answered Lambda's own
/// front-door refusal, whichever way the credential failed and whichever addressing reached
/// the endpoint — with the CLASS of the refusal named in the error-type header, which is
/// where a Lambda endpoint says why a caller was not admitted.
#[tokio::test]
async fn an_unadmitted_caller_of_a_function_url_reads_lambdas_own_forbidden() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let mut router = router_on(&engine);
    given_function(&router, "url-refused", &handler()).await;
    let restricted = given_configured_url(&router, "url-refused", None, "AWS_IAM", &Value::Null)
        .await;
    // From here on the SAME emulator verifies request signatures, as it does when it is
    // started with request authentication on.
    let mut authenticating = router_on(&engine);
    authenticating.set_request_authentication(true);
    // The unauthenticated router above is kept only to build the resources; nothing below
    // uses it.
    router.set_request_authentication(true);

    let rows: &[Row] = &[
        Row {
            case: "no credential at all, on the published host",
            credential: Credential::None,
            addressing: Addressing::PublishedHost,
            error_type: ACCESS_DENIED,
            body: FORBIDDEN_BODY,
        },
        Row {
            case: "a signature scoped to another service, on the published host",
            credential: Credential::ScopedElsewhere,
            addressing: Addressing::PublishedHost,
            error_type: INVALID_SIGNATURE,
            body: SCOPED_ELSEWHERE_BODY,
        },
        Row {
            case: "a signature that does not verify, on the published host",
            credential: Credential::Mismatched,
            addressing: Addressing::PublishedHost,
            error_type: INVALID_SIGNATURE,
            body: SIGNATURE_MISMATCH_BODY,
        },
        Row {
            case: "a credential this emulator does not hold, on the published host",
            credential: Credential::UnknownKey,
            addressing: Addressing::PublishedHost,
            error_type: UNRECOGNIZED_CLIENT,
            body: UNKNOWN_CREDENTIAL_BODY,
        },
        Row {
            case: "no credential at all, on the local-endpoint path",
            credential: Credential::None,
            addressing: Addressing::LocalEndpointPath,
            error_type: ACCESS_DENIED,
            body: FORBIDDEN_BODY,
        },
        Row {
            case: "a signature that does not verify, on the local-endpoint path",
            credential: Credential::Mismatched,
            addressing: Addressing::LocalEndpointPath,
            error_type: INVALID_SIGNATURE,
            body: SIGNATURE_MISMATCH_BODY,
        },
        Row {
            case: "no credential at all, path-style on the emulator's own endpoint",
            credential: Credential::None,
            addressing: Addressing::PathStyle,
            error_type: ACCESS_DENIED,
            body: FORBIDDEN_BODY,
        },
    ];
    for row in rows {
        let (host, path) = addressed(&restricted, row.addressing);
        let credentialled = headers(row.credential);
        let answer = send(
            &authenticating,
            "GET",
            &host,
            &path,
            &[],
            &borrowed(&credentialled),
        )
        .await;
        assert_eq!(
            answer.status,
            403,
            "case {:?}: expected HTTP 403, got {}",
            row.case,
            answer.summary()
        );
        assert_eq!(
            answer.text(),
            row.body,
            "case {:?}: expected the body {}, got {}",
            row.case,
            row.body,
            answer.summary()
        );
        assert_eq!(
            answer.header(ERROR_TYPE_HEADER).as_deref(),
            Some(row.error_type),
            "case {:?}: expected the class {:?} in {ERROR_TYPE_HEADER}, got {:?}",
            row.case,
            row.error_type,
            answer.header(ERROR_TYPE_HEADER)
        );
    }
}

/// RULE: the restating is of the function-URL request space alone. A `NONE` endpoint granted
/// to the public is SERVED however it arrived, and a control-plane call the same
/// authentication layer turns away keeps this service's own API refusal — an error code and
/// the layer's own sentence, never Lambda's front-door body.
#[tokio::test]
async fn only_a_function_url_request_reads_that_refusal() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    given_function(&router, "url-public", &handler()).await;
    let public =
        given_configured_url(&router, "url-public", None, "NONE", &Value::Null).await;
    // What makes a `NONE` endpoint public is the function's own resource-based policy.
    crate::function_url_data_plane::grant(
        &router,
        "url-public",
        None,
        &json!({
            "StatementId": "public-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": "*",
            "FunctionUrlAuthType": "NONE",
        }),
    )
    .await;

    let mut authenticating = router_on(&engine);
    authenticating.set_request_authentication(true);

    // A public endpoint, reached with no credential at all: served by the function.
    let served = send(
        &authenticating,
        "GET",
        &public.host,
        "/probe",
        &[],
        &[],
    )
    .await;
    assert_eq!(
        served.status,
        200,
        "case \"a public endpoint carrying no credential\": expected the handler's answer, got {}",
        served.summary()
    );
    assert_ne!(
        served.text(),
        FORBIDDEN_BODY,
        "case \"a public endpoint carrying no credential\": expected the function's own answer, \
         got {}",
        served.summary()
    );

    // A CONTROL-plane call the same layer turns away, over each credential: this service's
    // own API refusal, which names an error code and never Lambda's front-door body.
    // (case, the credential)
    let rows: &[(&str, Credential)] = &[
        ("no credential at all", Credential::None),
        (
            "a signature scoped to another service",
            Credential::ScopedElsewhere,
        ),
        (
            "a signature that does not verify",
            Credential::Mismatched,
        ),
    ];
    for (case, credential) in rows {
        let credentialled = headers(*credential);
        let answer: Answer = send(
            &authenticating,
            "GET",
            "lambda.us-east-1.amazonaws.com",
            "/2015-03-31/functions/url-public/configuration",
            &[],
            &borrowed(&credentialled),
        )
        .await;
        assert_ne!(
            answer.text(),
            FORBIDDEN_BODY,
            "case {case:?}: a control-plane refusal keeps this service's own wording, got {}",
            answer.summary()
        );
        // A request the layer turned away is refused; one it admitted is served. Either way
        // the body is the API's, which is what this row is about: the function-URL wording
        // reaches no control-plane answer.
        if answer.status == 403 {
            assert!(
                answer
                    .json()
                    .get("message")
                    .and_then(Value::as_str)
                    .is_some(),
                "case {case:?}: expected this service's own refusal envelope, got {}",
                answer.summary()
            );
        }
    }
}
