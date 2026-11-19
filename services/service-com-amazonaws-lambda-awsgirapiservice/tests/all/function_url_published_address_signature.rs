// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for a client that DIALS the address a function URL is published on, over
//! a real connection, with a real SigV4 signature.
//!
//! The rule under test, one level above the case that filed the issue (an `AWS_IAM` function
//! URL fronted by an API Gateway `HTTP_PROXY` integration, whose positive control is a signed
//! `GET` straight to the published endpoint):
//!
//! > The address a resource is published on hands the request it accepted OVER to the plane
//! > that serves it — the same method, the same `Host`, the same request target and the same
//! > body. Those are what the caller's SigV4 signature is computed over, and the emulator
//! > verifies that signature on the request the address hands it, so a hop that re-addresses
//! > the request admits no correctly signed caller at all; they are also what the plane reads
//! > the addressed resource and the path within it off. So an `AWS_IAM` function URL serves a
//! > signed caller the function's own answer, at the path the request carried, and refuses an
//! > unsigned one the Lambda front door's `403 {"Message":"Forbidden"}` before the function
//! > runs.
//!
//! Every row drives a real HTTP/1.1 exchange on the published port of a router that VERIFIES
//! signatures, as the emulator does when it is started with request authentication on, against
//! a function whose deployment package RUNS. The rows differ along what the rule generalises
//! over: the path the client supplies (the endpoint's own root, a nested path, a path carrying
//! a query, a percent-encoded segment), the method and the body it carries, and whether the
//! request is signed at all.

use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ACTIVATION_DELAY;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::synchronous_invoke_run::package;

/// The control-plane endpoint of this service, which every call below addresses.
const SERVICE_HOST: &str = "lambda.us-east-1.amazonaws.com";

/// The Region every call is signed for, which is the one this service's endpoint above names.
const REGION: &str = "us-east-1";

/// The signing name the emulator verifies a request to this service under.
const SIGNING_NAME: &str = "lambda";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The runtime every function below is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`), which is the
/// principal the function URL's invocation is granted to.
const ACCOUNT: &str = "000000000000";

/// How long past the activation instant this suite waits before invoking. The transition is
/// owed by the CLOCK, so nothing here sleeps on the emulator's behalf.
const SLACK: Duration = Duration::from_millis(200);

/// A generous ceiling on one exchange on the published port, so a loaded host still finishes:
/// no row measures how long anything took, only what was answered.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(60);

/// The body Lambda's own front door answers a caller it did not AUTHENTICATE with, which is
/// what an `AWS_IAM` function URL refuses an unsigned request with before the function runs.
const FORBIDDEN_BODY: &str = r#"{"Message":"Forbidden"}"#;

/// The header the handler below sets on every answer it describes, so a row that was served
/// by the FUNCTION is distinguishable from one answered by the endpoint itself.
const BACKEND_HEADER: &str = "x-echo-backend";
/// See [`BACKEND_HEADER`].
const BACKEND_MARKER: &str = "fn-url";

/// The handler every function below runs: it echoes the request the endpoint turned into an
/// event, so the answer is the RUN's and it says which path and query within the URL the
/// request reached.
fn echo_handler() -> String {
    r#"
import json


def lambda_handler(event, context):
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json", "x-echo-backend": "fn-url"},
        "body": json.dumps(
            {
                "path": event["rawPath"],
                "rawQueryString": event.get("rawQueryString", ""),
                "method": event["requestContext"]["http"]["method"],
                "body": event.get("body") or "",
            }
        ),
    }
"#
    .to_string()
}

/// One service mounted on its own engine, on a router that VERIFIES every request signature —
/// which is what an `AWS_IAM` function URL's whole contract rests on.
fn authenticating_router(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(true);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// The keys this emulator process publishes, which a caller signs with and the emulator
/// verifies against.
fn published_keys() -> (String, String) {
    let keys = ::emulator_server::request_authentication::SigV4Authenticator::for_this_process();
    (keys.access_key_id.clone(), keys.secret_access_key.clone())
}

/// One request, SigV4-signed with the keys above by the workspace's own signer — the same
/// recipe the emulator's verifier re-signs with, so what this produces is what it computes.
async fn signed_request(
    method: &str,
    url: &str,
    body: Vec<u8>,
    headers: &[(&str, &str)],
) -> http::Request<Vec<u8>> {
    signed_request_for(SIGNING_NAME, method, url, body, headers).await
}

/// The same signature, under the credential scope of `signing_name` — which is what a request
/// signed for ANOTHER service than the one it reaches carries.
async fn signed_request_for(
    signing_name: &str,
    method: &str,
    url: &str,
    body: Vec<u8>,
    headers: &[(&str, &str)],
) -> http::Request<Vec<u8>> {
    let mut builder = http::Request::builder().method(method).uri(url);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(body).expect("the test request must build");
    let (key_id, secret) = published_keys();
    ::service_common::in_process_sigv4::sign_in_process_request_as(
        &mut request,
        signing_name,
        REGION,
        &key_id,
        &secret,
    )
    .await
    .expect("the test request must sign");
    request
}

/// What a dispatched control-plane call answered.
struct Answer {
    status: u16,
    body: bytes::Bytes,
}

impl Answer {
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    fn summary(&self) -> String {
        format!("{} {}", self.status, String::from_utf8_lossy(&self.body))
    }
}

/// One CONTROL-plane call, signed as a client signs one.
async fn control(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let request = signed_request(
        method,
        &format!("https://{SERVICE_HOST}{uri}"),
        payload,
        &[("content-type", "application/json")],
    )
    .await;
    let (parts, body) = request.into_parts();
    let response = router
        .dispatch(http::Request::from_parts(parts, bytes::Bytes::from(body)))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        body,
    }
}

/// Create a function whose package RUNS `source`, and wait out the provisioning window.
async fn given_function(router: &MultiServiceRouter, name: &str) {
    let created = control(
        router,
        "POST",
        "/2015-03-31/functions",
        &json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": 20,
            "MemorySize": 512,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(&echo_handler())),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunction {name:?} must succeed, got {}",
        created.summary()
    );
    tokio::time::sleep(ACTIVATION_DELAY + SLACK).await;
}

/// `CreateFunctionUrlConfig(AuthType="AWS_IAM")` on `function`, with the two grants that admit
/// the caller's own account to the endpoint, answering the `FunctionUrl` it published.
async fn given_iam_url(router: &MultiServiceRouter, function: &str) -> String {
    given_iam_url_with_cors(router, function, &Value::Null).await
}

/// The same create, with the `Cors` settings `cors` names — absent for `Value::Null`, which is
/// a URL configured with none.
async fn given_iam_url_with_cors(
    router: &MultiServiceRouter,
    function: &str,
    cors: &Value,
) -> String {
    let mut body = json!({ "AuthType": "AWS_IAM" });
    if !cors.is_null() {
        body["Cors"] = cors.clone();
    }
    let created = control(
        router,
        "POST",
        &format!("/2021-10-31/functions/{function}/url"),
        &body,
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "CreateFunctionUrlConfig on {function:?} must succeed, got {}",
        created.summary()
    );
    let url = created
        .json()
        .get("FunctionUrl")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!(
                "the create must answer a FunctionUrl, got {}",
                created.summary()
            )
        });
    for statement in [
        json!({
            "StatementId": "invoke-function-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": ACCOUNT,
            "FunctionUrlAuthType": "AWS_IAM",
        }),
        json!({
            "StatementId": "invoke-via-function-url",
            "Action": "lambda:InvokeFunction",
            "Principal": ACCOUNT,
        }),
    ] {
        let granted = control(
            router,
            "POST",
            &format!("/2015-03-31/functions/{function}/policy"),
            &statement,
        )
        .await;
        assert_eq!(
            granted.status,
            201,
            "AddPermission on {function:?} must succeed, got {}",
            granted.summary()
        );
    }
    url
}

/// What one exchange on the published address observed.
struct Served {
    status: u16,
    /// The whole answer as it came off the connection, status line and headers included.
    raw: String,
    /// The body alone.
    body: String,
}

impl Served {
    /// The body as JSON, or `Value::Null` for a body that is none.
    fn json(&self) -> Value {
        serde_json::from_str(&self.body).unwrap_or(Value::Null)
    }

    /// Whether the answer carries `name: value`, however the header was cased on the wire.
    fn carries_header(&self, name: &str, value: &str) -> bool {
        self.raw
            .split("\r\n")
            .take_while(|line| !line.is_empty())
            .any(|line| {
                line.split_once(':').is_some_and(|(carried, written)| {
                    carried.eq_ignore_ascii_case(name) && written.trim() == value
                })
            })
    }

    /// Whether the answer carries a header of that name at all.
    fn names_header(&self, name: &str) -> bool {
        self.raw
            .split("\r\n")
            .take_while(|line| !line.is_empty())
            .any(|line| {
                line.split_once(':')
                    .is_some_and(|(carried, _)| carried.eq_ignore_ascii_case(name))
            })
    }
}

/// Dial the published address and exchange one request on it, as a client handed that endpoint
/// does: a connection in the clear, HTTP/1.1, with the authority it dialled as `Host`.
async fn exchange(
    case: &str,
    authority: &str,
    method: &str,
    target: &str,
    body: &[u8],
    credentialled: &[(String, String)],
) -> Served {
    exchange_naming(case, authority, authority, method, target, body, credentialled).await
}

/// The same exchange, with a `Host` of the caller's choosing rather than the address it
/// dialled — which is what a request to this port that names another host is.
///
/// Only the PORT is taken from the published authority: the core binds its listeners on
/// loopback (`EMULATOR_BIND_ADDRESS`, `127.0.0.1` by default), so the dial is to this machine.
async fn exchange_naming(
    case: &str,
    authority: &str,
    named_host: &str,
    method: &str,
    target: &str,
    body: &[u8],
    credentialled: &[(String, String)],
) -> Served {
    let port = authority
        .rsplit(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or_else(|| {
            panic!("case {case:?}: the published address {authority:?} carries no port to dial")
        });
    let mut stream = tokio::time::timeout(
        EXCHANGE_TIMEOUT,
        tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)),
    )
    .await
    .unwrap_or_else(|_| panic!("case {case:?}: {authority} did not accept a connection"))
    .unwrap_or_else(|error| panic!("case {case:?}: {authority} refused a connection: {error}"));
    let mut request = format!("{method} {target} HTTP/1.1\r\nHost: {named_host}\r\n");
    for (name, value) in credentialled {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
    // The framing header, unless the request already carries the client's own — a row that
    // SIGNS `content-length` sends it among its signed headers, and one value is what a
    // request may carry.
    if !credentialled
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case("content-length"))
    {
        request.push_str(&format!("content-length: {}\r\n", body.len()));
    }
    request.push_str("connection: close\r\n\r\n");
    let mut wire = request.into_bytes();
    wire.extend_from_slice(body);
    stream
        .write_all(&wire)
        .await
        .unwrap_or_else(|error| panic!("case {case:?}: the request was not sent: {error}"));
    let mut answered = Vec::new();
    tokio::time::timeout(EXCHANGE_TIMEOUT, stream.read_to_end(&mut answered))
        .await
        .unwrap_or_else(|_| panic!("case {case:?}: the published address did not answer in time"))
        .unwrap_or_else(|error| panic!("case {case:?}: the answer was not read: {error}"));
    let raw = String::from_utf8_lossy(&answered).to_string();
    let status = raw
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or_else(|| panic!("case {case:?}: no status line in {raw:?}"));
    let body = raw
        .split_once("\r\n\r\n")
        .map(|(_headers, body)| body.to_string())
        .unwrap_or_default();
    Served { status, raw, body }
}

/// The headers one SigV4-signed request to the published address travels with: the signature
/// the workspace's signer computed over that exact request, read back off it, beside the
/// headers `carried` names — which are signed with it, as a client's own are.
async fn signature_for(
    authority: &str,
    method: &str,
    target: &str,
    body: &[u8],
    carried: &[(&str, &str)],
) -> Vec<(String, String)> {
    signature_scoped_for(SIGNING_NAME, authority, method, target, body, carried).await
}

/// The same headers, under the credential scope of `signing_name`.
async fn signature_scoped_for(
    signing_name: &str,
    authority: &str,
    method: &str,
    target: &str,
    body: &[u8],
    carried: &[(&str, &str)],
) -> Vec<(String, String)> {
    let request = signed_request_for(
        signing_name,
        method,
        &format!("https://{authority}{target}"),
        body.to_vec(),
        carried,
    )
    .await;
    request
        .headers()
        .iter()
        .filter(|(name, _)| name.as_str() != "host")
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect()
}

/// The authority a published `FunctionUrl` is reached at, and the path that URL is the base of
/// — a client appends its own path to the value it was handed.
fn published(url: &str) -> (String, String) {
    let rest = url
        .strip_prefix("https://")
        .unwrap_or_else(|| panic!("the published URL must carry a scheme, got {url:?}"));
    let (authority, base) = rest.split_once('/').unwrap_or((rest, ""));
    (
        authority.to_string(),
        format!("/{}", base.trim_end_matches('/')),
    )
}

/// One row: the path a client appends to the endpoint it was handed, the method and body it
/// sends, and the path and query the function must be told it reached.
struct Row {
    case: &'static str,
    method: &'static str,
    path: &'static str,
    query: &'static str,
    body: &'static str,
    /// The media type the body is sent under, which is what decides whether the event carries
    /// it as text or base64-encoded; empty for a request that sends none.
    media_type: &'static str,
    /// Whether the client SIGNS the length it declared, which a real signer does for a request
    /// carrying a body (botocore signs every header outside a small deny list) — so a hop that
    /// drops it hands over a request whose signature cannot be recomputed.
    signs_length: bool,
    reached: &'static str,
}

/// RULE: a request a client SIGNS and sends to the address an `AWS_IAM` function URL was
/// published on is served by the function, at the path the request carried — the signature
/// verifies because the address hands the request over as the client sent it.
#[tokio::test]
async fn a_signed_client_of_an_aws_iam_function_url_is_served_by_the_function() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = authenticating_router(&engine);
    given_function(&router, "url-signed").await;
    let url = given_iam_url(&router, "url-signed").await;
    let (authority, base) = published(&url);

    let rows: &[Row] = &[
        Row {
            case: "the endpoint's own root",
            method: "GET",
            path: "/",
            query: "",
            body: "",
            media_type: "",
            signs_length: false,
            reached: "/",
        },
        Row {
            case: "a nested path carrying a query",
            method: "GET",
            path: "/direct/ok",
            query: "via=sigv4",
            body: "",
            media_type: "",
            signs_length: false,
            reached: "/direct/ok",
        },
        Row {
            case: "a path of one segment",
            method: "GET",
            path: "/items",
            query: "",
            body: "",
            media_type: "",
            signs_length: false,
            reached: "/items",
        },
        Row {
            case: "a percent-encoded segment",
            method: "GET",
            path: "/my%20items/one",
            query: "page=2&nonce=abc",
            body: "",
            media_type: "",
            signs_length: false,
            reached: "/my%20items/one",
        },
        Row {
            case: "another method, carrying a body",
            method: "POST",
            path: "/orders",
            query: "",
            body: r#"{"item":"one"}"#,
            media_type: "application/json",
            signs_length: false,
            reached: "/orders",
        },
        Row {
            case: "a body whose declared length is signed too",
            method: "POST",
            path: "/orders/signed-length",
            query: "",
            body: r#"{"item":"two"}"#,
            media_type: "application/json",
            signs_length: true,
            reached: "/orders/signed-length",
        },
        Row {
            case: "a repeat of the positive control: the function still serves",
            method: "GET",
            path: "/direct/ok",
            query: "via=sigv4",
            body: "",
            media_type: "",
            signs_length: false,
            reached: "/direct/ok",
        },
    ];

    for row in rows {
        let case = row.case;
        let target = if row.query.is_empty() {
            format!("{base}{path}", path = row.path)
        } else {
            format!("{base}{path}?{query}", path = row.path, query = row.query)
        };
        let declared_length = row.body.len().to_string();
        let mut carried: Vec<(&str, &str)> = Vec::new();
        if !row.media_type.is_empty() {
            carried.push(("content-type", row.media_type));
        }
        if row.signs_length {
            carried.push(("content-length", declared_length.as_str()));
        }
        let credentialled = signature_for(
            &authority,
            row.method,
            &target,
            row.body.as_bytes(),
            &carried,
        )
        .await;
        let served = exchange(
            case,
            &authority,
            row.method,
            &target,
            row.body.as_bytes(),
            &credentialled,
        )
        .await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: a signed caller must be served by the function, got {}",
            served.raw
        );
        assert!(
            served.carries_header(BACKEND_HEADER, BACKEND_MARKER),
            "case {case:?}: expected the function's own {BACKEND_HEADER}: {BACKEND_MARKER}, got \
             {}",
            served.raw
        );
        let echoed = served.json();
        assert_eq!(
            echoed.get("path").and_then(Value::as_str),
            Some(row.reached),
            "case {case:?}: expected the handler to be told the path {:?}, got {}",
            row.reached,
            served.body
        );
        assert_eq!(
            echoed.get("rawQueryString").and_then(Value::as_str),
            Some(row.query),
            "case {case:?}: expected the handler to be told the query {:?}, got {}",
            row.query,
            served.body
        );
        assert_eq!(
            echoed.get("method").and_then(Value::as_str),
            Some(row.method),
            "case {case:?}: expected the handler to be told the method {:?}, got {}",
            row.method,
            served.body
        );
        assert_eq!(
            echoed.get("body").and_then(Value::as_str),
            Some(row.body),
            "case {case:?}: expected the handler to be told the body {:?}, got {}",
            row.body,
            served.body
        );
    }
}

/// RULE, the other half the same address decides: a caller that signs NOTHING is refused by
/// the endpoint itself — the Lambda front door's `403 {"Message":"Forbidden"}`, with none of
/// the function's own answer, because the function never ran.
///
/// The rows differ along what the rule generalises over: the path and method of the unsigned
/// request, and a signature that does not verify beside the absent one.
#[tokio::test]
async fn an_unsigned_client_of_an_aws_iam_function_url_is_refused_before_the_function_runs() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = authenticating_router(&engine);
    given_function(&router, "url-unsigned").await;
    let url = given_iam_url(&router, "url-unsigned").await;
    let (authority, base) = published(&url);

    // A signature that cannot verify, dated now so it is judged as a SIGNATURE rather than for
    // its date.
    let stamp = {
        let now = ::aws_smithy_types::DateTime::from(std::time::SystemTime::now());
        now.fmt(::aws_smithy_types::date_time::Format::DateTime)
            .expect("an instant renders")
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
    };
    let (key_id, _secret) = published_keys();
    let mismatched = vec![
        (
            "authorization".to_string(),
            format!(
                "AWS4-HMAC-SHA256 Credential={key_id}/{date}/{REGION}/{SIGNING_NAME}/aws4_request, \
                 SignedHeaders=host, Signature=0123456789abcdef",
                date = &stamp[..8],
            ),
        ),
        ("x-amz-date".to_string(), stamp.clone()),
    ];

    /// One row of the refusal table: the request, and how it is credentialled.
    struct Unadmitted {
        case: &'static str,
        method: &'static str,
        /// The path appended to the endpoint the client was handed.
        path: &'static str,
        credentialled: Vec<(String, String)>,
    }
    let rows: Vec<Unadmitted> = vec![
        Unadmitted {
            case: "no credential at all, on a nested path",
            method: "GET",
            path: "/items/abc?via=direct",
            credentialled: Vec::new(),
        },
        Unadmitted {
            case: "no credential at all, on the root",
            method: "GET",
            path: "/",
            credentialled: Vec::new(),
        },
        Unadmitted {
            case: "no credential at all, on another method",
            method: "POST",
            path: "/orders",
            credentialled: Vec::new(),
        },
        Unadmitted {
            case: "a signature that does not verify",
            method: "GET",
            path: "/items/abc?via=direct",
            credentialled: mismatched,
        },
    ];
    for row in &rows {
        let (case, method, path) = (row.case, row.method, row.path);
        let credentialled = &row.credentialled;
        let target = format!("{base}{path}");
        let served = exchange(case, &authority, method, &target, &[], credentialled).await;
        assert_eq!(
            served.status, 403,
            "case {case:?}: an unadmitted caller must be refused, got {}",
            served.raw
        );
        assert!(
            !served.names_header(BACKEND_HEADER),
            "case {case:?}: expected none of the function's own headers, got {}",
            served.raw
        );
        assert!(
            !served.body.contains("rawQueryString"),
            "case {case:?}: expected the request not to be echoed, got {}",
            served.body
        );
    }
    // The body the FRONT DOOR answers is the one a caller that presented no credential at all
    // reads; a signature that did not verify is told that much instead, which the suite beside
    // this one asserts over its own class.
    let unsigned = exchange(
        "no credential at all, on a nested path",
        &authority,
        "GET",
        &format!("{base}/items/abc?via=direct"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        unsigned.body, FORBIDDEN_BODY,
        "case \"no credential at all\": expected the body {FORBIDDEN_BODY} exactly, got {}",
        unsigned.body
    );
}

/// RULE, the precondition handing the request over rests on: this address serves the ONE
/// function URL it was published for, so a request whose `Host` is not that address is refused
/// by the address itself — its path is never left to decide what it reaches, least of all this
/// service's own control plane.
///
/// The rows differ along what the rule generalises over: another spelling of the same loopback
/// port, this service's own endpoint host, a host of another zone, and the path each of them
/// carries (a path within the URL, and one spelled like a control-plane call).
#[tokio::test]
async fn the_published_address_serves_only_the_host_it_was_published_at() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = authenticating_router(&engine);
    given_function(&router, "url-addressed").await;
    let url = given_iam_url(&router, "url-addressed").await;
    let (authority, base) = published(&url);
    let port = authority
        .rsplit(':')
        .next()
        .unwrap_or_default()
        .to_string();

    // (case, the host the request names, the request target)
    let rows: &[(&str, String, String)] = &[
        (
            "another spelling of the same loopback port",
            format!("localhost:{port}"),
            format!("{base}/direct/ok"),
        ),
        (
            "this service's own endpoint host",
            SERVICE_HOST.to_string(),
            "/2015-03-31/functions".to_string(),
        ),
        (
            "this service's own endpoint host, inside the URL's own base",
            SERVICE_HOST.to_string(),
            format!("{base}/direct/ok"),
        ),
        (
            "a host of another zone",
            "example.test".to_string(),
            format!("{base}/direct/ok"),
        ),
        (
            "a control-plane path on the published port",
            format!("localhost:{port}"),
            "/2015-03-31/functions".to_string(),
        ),
    ];
    for (case, named_host, target) in rows {
        let served = exchange_naming(case, &authority, named_host, "GET", target, &[], &[]).await;
        assert_eq!(
            served.status, 421,
            "case {case:?}: this address must not serve a host it was not published at, got {}",
            served.raw
        );
        assert!(
            !served.names_header(BACKEND_HEADER),
            "case {case:?}: expected none of the function's own headers, got {}",
            served.raw
        );
        assert!(
            !served.body.contains("FunctionName") && !served.body.contains("rawQueryString"),
            "case {case:?}: expected neither this service's own API nor the function's answer, \
             got {}",
            served.body
        );
    }

    // The positive control: the address it WAS published at still serves, so the rows above
    // are about the host and not about the port being shut.
    let credentialled = signature_for(
        &authority,
        "GET",
        &format!("{base}/direct/ok"),
        &[],
        &[],
    )
    .await;
    let served = exchange(
        "the address the URL was published at",
        &authority,
        "GET",
        &format!("{base}/direct/ok"),
        &[],
        &credentialled,
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case \"the address the URL was published at\": the published address must still serve, \
         got {}",
        served.raw
    );
}

/// RULE, the consequence of handing the request over at the address it was published on: every
/// path on that address is a path WITHIN the function URL, so it reaches the function the URL
/// is attached to and never a modelled operation of this service's API — and a caller whose
/// credential names another service reaches no other service's surface either, it is refused
/// as that credential.
///
/// The rows differ along what the rule generalises over: a path in this service's own
/// control-plane path space, a path spelled like another service's object, and one spelled like
/// the emulator's administrative endpoint; and, for the credential, this service's own scope
/// beside another service's.
#[tokio::test]
async fn every_path_on_the_published_address_is_a_path_within_the_function_url() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = authenticating_router(&engine);
    given_function(&router, "url-every-path").await;
    let url = given_iam_url(&router, "url-every-path").await;
    let (authority, base) = published(&url);

    // The paths a client could spell that name something else on another endpoint of this
    // emulator: this service's modelled control-plane path space (`crate::ROUTE_PATH_SEGMENTS`
    // begins `2015-03-31`), another service's object, and the administrative endpoint.
    let rows: &[(&str, &str)] = &[
        ("this service's control-plane path space", "/2015-03-31/functions"),
        (
            "a control-plane path naming a function",
            "/2015-03-31/functions/url-every-path/configuration",
        ),
        ("a path spelled like another service's object", "/my-bucket/key.txt"),
        ("a path spelled like the administrative endpoint", "/__admin/health"),
    ];
    for (case, path) in rows {
        let target = format!("{base}{path}");
        let credentialled = signature_for(&authority, "GET", &target, &[], &[]).await;
        let served = exchange(case, &authority, "GET", &target, &[], &credentialled).await;
        assert_eq!(
            served.status, 200,
            "case {case:?}: the published address must serve its own function, got {}",
            served.raw
        );
        assert!(
            served.carries_header(BACKEND_HEADER, BACKEND_MARKER),
            "case {case:?}: expected the function's own {BACKEND_HEADER}: {BACKEND_MARKER} \
             rather than an answer of this service's API, got {}",
            served.raw
        );
        assert_eq!(
            served.json().get("path").and_then(Value::as_str),
            Some(*path),
            "case {case:?}: expected the whole path to be the path within the URL, got {}",
            served.body
        );
    }

    // The same addressing, under a credential scoped to ANOTHER service: it reaches no other
    // service's surface, it is refused as the credential it presented — the sentence AWS words
    // for a scope naming a service other than the one the request reached.
    let target = format!("{base}/direct/ok");
    let scoped_elsewhere =
        signature_scoped_for("s3", &authority, "GET", &target, &[], &[]).await;
    let refused = exchange(
        "a credential scoped to another service",
        &authority,
        "GET",
        &target,
        &[],
        &scoped_elsewhere,
    )
    .await;
    assert_eq!(
        refused.status, 403,
        "case \"a credential scoped to another service\": expected the refusal of that \
         credential, got {}",
        refused.raw
    );
    assert_eq!(
        refused.body, r#"{"message":"Credential should be scoped to correct service: 'lambda'. "}"#,
        "case \"a credential scoped to another service\": expected the sentence AWS words for a \
         scope naming another service, got {}",
        refused.body
    );
    assert!(
        !refused.names_header(BACKEND_HEADER),
        "case \"a credential scoped to another service\": expected none of the function's own \
         headers, got {}",
        refused.raw
    );

    // And nothing the rows above sent acted on this service's API: the function is still the
    // one the setup created, read back through the control plane.
    let configured = control(
        &router,
        "GET",
        "/2015-03-31/functions/url-every-path/configuration",
        &Value::Null,
    )
    .await;
    assert_eq!(
        configured.status,
        200,
        "the control plane must still answer for the function, got {}",
        configured.summary()
    );
    assert_eq!(
        configured.json().get("FunctionName").and_then(Value::as_str),
        Some("url-every-path"),
        "expected the function the setup created, got {}",
        configured.summary()
    );
}

/// The reserved name this emulator's own hop carries the client's `Origin` under, so the plane
/// that answers the CORS half of a function URL request reads one value — the front door's
/// reading of the browser-set header, never a caller's own.
const RESERVED_ORIGIN_HEADER: &str = "x-amz-emulator-function-url-origin";

/// The origin the URL below is configured to admit, which is the only origin its answers may
/// carry the CORS permission for.
const ADMITTED_ORIGIN: &str = "https://admitted.example";

/// RULE, the other half of handing a request over unmodified: what the endpoint's own settings
/// answer is decided by what the CLIENT SENT, not by what it claims about itself. The `Origin`
/// a browser set is what the CORS permission is built from, so a caller writing this emulator's
/// own reserved relay name itself chooses nothing.
#[tokio::test]
async fn a_caller_cannot_choose_the_origin_the_cors_answer_is_built_for() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = authenticating_router(&engine);
    given_function(&router, "url-cors-origin").await;
    let url = given_iam_url_with_cors(
        &router,
        "url-cors-origin",
        &json!({ "AllowOrigins": [ADMITTED_ORIGIN], "AllowMethods": ["GET"] }),
    )
    .await;
    let (authority, base) = published(&url);
    let target = format!("{base}/items");

    // The spoof: the reserved name, written by the caller, naming the origin the configuration
    // admits — on a request that carries no `Origin` at all. A signer never signs a
    // browser-set header, so it travels unsigned beside a signature over the rest.
    let mut spoofed = signature_for(&authority, "GET", &target, &[], &[]).await;
    spoofed.push((
        RESERVED_ORIGIN_HEADER.to_string(),
        ADMITTED_ORIGIN.to_string(),
    ));
    let served = exchange(
        "a caller writing the reserved relay name itself",
        &authority,
        "GET",
        &target,
        &[],
        &spoofed,
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case \"a caller writing the reserved relay name itself\": the signed request must \
         still be served, got {}",
        served.raw
    );
    assert!(
        !served.names_header("access-control-allow-origin"),
        "case \"a caller writing the reserved relay name itself\": a request that carried no \
         Origin must be answered with no CORS permission, got {}",
        served.raw
    );

    // The positive control: the SAME endpoint does carry the permission for an origin a browser
    // set, so the row above is about who said it rather than about the configuration.
    let mut browser_set = signature_for(&authority, "GET", &target, &[], &[]).await;
    browser_set.push(("origin".to_string(), ADMITTED_ORIGIN.to_string()));
    let served = exchange(
        "an origin the browser set",
        &authority,
        "GET",
        &target,
        &[],
        &browser_set,
    )
    .await;
    assert_eq!(
        served.status, 200,
        "case \"an origin the browser set\": the request must be served, got {}",
        served.raw
    );
    assert!(
        served.carries_header("access-control-allow-origin", ADMITTED_ORIGIN),
        "case \"an origin the browser set\": expected the permission for the origin the \
         configuration admits, got {}",
        served.raw
    );
}
