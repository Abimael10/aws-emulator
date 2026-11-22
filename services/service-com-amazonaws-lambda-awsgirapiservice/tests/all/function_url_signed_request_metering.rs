// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for an `AWS_IAM` function URL reached by a client that really SIGNS its
//! request and really dials the address the URL was published on, with CloudWatch mounted
//! beside Lambda on one engine so the series the invocation publishes is read back where a
//! client reads it.
//!
//! The RULE asserted below: a request carrying a SigV4 signature the emulator verifies for
//! this service's signing name in the endpoint's Region is an invocation of the function the
//! URL is attached to — served the response the handler's result describes — and Lambda meters
//! it on that function's own `AWS/Lambda` series exactly as it meters an invocation that
//! arrived through the `Invoke` API, so one function accumulates ONE series whichever surface
//! started the run.
//!
//! What the sibling suites pin and this one does not repeat:
//! `function_url_published_address_signature.rs` asserts what a signed caller is SERVED on the
//! published address, and `function_url_invocation_series.rs` asserts the series an invocation
//! through an `AuthType=NONE` URL publishes, on a router that verifies no signature. What only
//! this suite reaches is the two together: a router with request authentication ON, an
//! `AWS_IAM` URL, a real signature over a real connection, and the series that invocation then
//! shares with the `Invoke` API's.
//!
//! The rows differ along what the rule generalises over: the path the client appends to the
//! endpoint it was handed, whether that path carries a query, the method and body the request
//! carries, how many requests the row signs and sends, and what the run came to (a handler that
//! describes a response, and one that raises — which this plane answers as a bad gateway and
//! meters as an error). A closing test drives an UNSIGNED request at the same address, which
//! the front door refuses and nothing meters, and then a signed one: so a router that verified
//! no signature could not pass both.
//!
//! Every namespace, metric, dimension and unit name here is a literal taken from the AWS Lambda
//! metric reference rather than read from this crate's constants, so a series published under a
//! name no AWS reader would ask for fails here instead of agreeing with itself. Nothing waits on
//! the wall clock: the measurements are published by the run each request is answered off, so a
//! row reads them once its own requests have been answered, and no assertion is about how long
//! anything took.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

use crate::function_lifecycle::{make_activation_due, ROLE};
use crate::function_url_data_plane::FRONT_DOOR_FORBIDDEN_MESSAGE;
use crate::function_url_invocation_series::{function_dimensions, resource_dimensions, total};
use crate::synchronous_invoke_run::package;

/// The Region every call below is signed for, and the one this service's endpoint names.
const REGION: &str = "us-east-1";

/// The control-plane endpoint of this service, which every control call addresses.
const SERVICE_HOST: &str = "lambda.us-east-1.amazonaws.com";

/// The endpoint CloudWatch's own calls address.
const CLOUDWATCH_HOST: &str = "monitoring.us-east-1.amazonaws.com";

/// The signing name the emulator verifies a request to THIS service under — a function URL is
/// a Lambda data-plane endpoint, so a caller of one signs for it
/// (`smithy_query(query="service_info")`: `sigv4Name` `lambda`).
const SIGNING_NAME: &str = "lambda";

/// The signing name CloudWatch is reached under
/// (`smithy_query(query="service_info")` of its model: `sigv4Name` `monitoring`).
const CLOUDWATCH_SIGNING_NAME: &str = "monitoring";

/// The `X-Amz-Target` prefix of the CloudWatch service, whose `awsJson1_0` protocol the reads
/// below speak.
const CLOUDWATCH_TARGET_PREFIX: &str = "GraniteServiceVersion20100801";

/// Where `CreateFunction` and `Invoke` are bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The account id every emulator ARN carries (`engine::EMULATOR_ACCOUNT_ID`), which is the
/// principal the function URL's invocation is granted to.
const ACCOUNT: &str = "000000000000";

/// The runtime every function here is created with, and the handler its package exposes.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// The `Timeout` every function here is given, in seconds. Generous on purpose, because
/// nothing here is about it: a handler stopped at this deadline is metered as a run that
/// ERRORED, so a tight value would turn a loaded host into a failure the handler never raised.
/// Well inside the range the model allows the member
/// (`smithy_query(query="shape", shape="Timeout")`: 1 to 5400).
const GENEROUS_TIMEOUT_SECS: u32 = 300;

/// The `MemorySize` every function here is given, in MB.
const MEMORY_SIZE_MB: u32 = 512;

/// The function every case creates, attaches a URL to and invokes.
const FUNCTION: &str = "url-signed-metered";

/// The namespace Lambda's own metrics live in (the AWS Lambda metric reference).
const NAMESPACE: &str = "AWS/Lambda";

/// The names that reference gives the series under test, and the units it reports them in.
const INVOCATIONS_SERIES: &str = "Invocations";
/// See [`INVOCATIONS_SERIES`].
const DURATION_SERIES: &str = "Duration";
/// See [`INVOCATIONS_SERIES`].
const ERRORS_SERIES: &str = "Errors";
/// See [`INVOCATIONS_SERIES`].
const MILLISECONDS_UNIT: &str = "Milliseconds";
/// See [`INVOCATIONS_SERIES`].
const COUNT_UNIT: &str = "Count";

/// The `Period`, in seconds, every query reads at: the one-minute granularity Lambda's own
/// series are stored at.
const PERIOD_SECS: i64 = 60;

/// How far either side of now a query's window reaches, in seconds. Wide on purpose: no
/// assertion here is about where in the window a measurement landed.
const WINDOW_SECS: f64 = 600.0;

/// A generous ceiling on one exchange on the published port, so a loaded host still finishes:
/// no row measures how long anything took, only what was answered.
const EXCHANGE_TIMEOUT: Duration = Duration::from_secs(60);

/// The header the handler below sets on every answer it describes, so a row served by the
/// FUNCTION is distinguishable from one answered by the endpoint itself.
const BACKEND_HEADER: &str = "x-signed-url-backend";
/// See [`BACKEND_HEADER`].
const BACKEND_MARKER: &str = "served";

/// The handler every case runs. It branches on the path the request carried, which is `rawPath`
/// in the payload-format-2.0 event a function URL hands it, so one package serves every row and
/// what the series measures is that row's own run.
const SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    path = event.get("rawPath") or "/"
    if path.endswith("/raise"):
        raise RuntimeError("this invocation failed on purpose")
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json", "x-signed-url-backend": "served"},
        "body": json.dumps(
            {
                "path": path,
                "rawQueryString": event.get("rawQueryString", ""),
            }
        ),
    }
"#;

/// Lambda and CloudWatch on one engine, on a router that VERIFIES every request signature —
/// which is what an `AWS_IAM` function URL's whole contract rests on, and the shape
/// `core/router/src/lib.rs` mounts both services in.
fn authenticating_emulator(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(true);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatch_graniteserviceversion20100801::EmulatedService::new(
            Arc::clone(engine),
        ),
    );
    router
}

/// The keys this emulator process publishes, which a caller signs with and the emulator
/// verifies against.
fn published_keys() -> (String, String) {
    let keys = ::emulator_server::request_authentication::SigV4Authenticator::for_this_process();
    (keys.access_key_id.clone(), keys.secret_access_key.clone())
}

/// One request, SigV4-signed under the credential scope of `signing_name` by the workspace's
/// own signer — the same recipe the emulator's verifier re-signs with, so what this produces is
/// what that verifier computes over the same request.
async fn signed_request(
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

/// What a dispatched call answered.
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

/// Dispatch one signed request through the router, as a client's own connection would.
async fn dispatch(router: &MultiServiceRouter, request: http::Request<Vec<u8>>) -> Answer {
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

/// One Lambda CONTROL-plane call, signed as a client signs one.
async fn control(router: &MultiServiceRouter, method: &str, uri: &str, body: &Value) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let request = signed_request(
        SIGNING_NAME,
        method,
        &format!("https://{SERVICE_HOST}{uri}"),
        payload,
        &[("content-type", "application/json")],
    )
    .await;
    dispatch(router, request).await
}

/// One CloudWatch call, signed under CloudWatch's own signing name.
async fn cloudwatch(router: &MultiServiceRouter, operation: &str, body: &Value) -> Answer {
    let target = format!("{CLOUDWATCH_TARGET_PREFIX}.{operation}");
    let request = signed_request(
        CLOUDWATCH_SIGNING_NAME,
        "POST",
        &format!("https://{CLOUDWATCH_HOST}/"),
        serde_json::to_vec(body).expect("the test request body must serialize"),
        &[
            ("content-type", "application/x-amz-json-1.0"),
            ("x-amz-target", target.as_str()),
        ],
    )
    .await;
    dispatch(router, request).await
}

/// Now, in the epoch seconds a stored datapoint is stamped with.
fn now_secs() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs_f64()
}

/// The `Datapoints` `GetMetricStatistics` answers for the series `metric` and `dimensions`
/// identify, selected by the unit the AWS Lambda metric reference gives that series.
async fn datapoints(
    router: &MultiServiceRouter,
    case: &str,
    metric: &str,
    unit: &str,
    dimensions: &Value,
) -> Vec<Value> {
    let now = now_secs();
    let read = cloudwatch(
        router,
        "GetMetricStatistics",
        &json!({
            "Namespace": NAMESPACE,
            "MetricName": metric,
            "Dimensions": dimensions,
            "StartTime": now - WINDOW_SECS,
            "EndTime": now + WINDOW_SECS,
            "Period": PERIOD_SECS,
            "Statistics": ["Sum", "SampleCount", "Minimum", "Maximum"],
            "Unit": unit,
        }),
    )
    .await;
    assert_eq!(
        read.status,
        200,
        "case {case:?}: GetMetricStatistics({metric}, {dimensions}) answered {}",
        read.summary()
    );
    read.json()["Datapoints"]
        .as_array()
        .cloned()
        .unwrap_or_default()
}

/// Create the function every case invokes, bring it to `Active`, attach an `AWS_IAM` function
/// URL to it, grant the caller's own account the endpoint's invocation, and answer the
/// `FunctionUrl` the create published.
async fn given_signed_url(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
) -> String {
    let created = control(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": RUNTIME,
            "Handler": HANDLER,
            "Timeout": GENEROUS_TIMEOUT_SECS,
            "MemorySize": MEMORY_SIZE_MB,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(SOURCE)),
            },
        }),
    )
    .await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: CreateFunction must succeed, got {}",
        created.summary()
    );
    // The activation the CLOCK owes this function, brought forward instead of waited out.
    make_activation_due(engine);

    let published = control(
        router,
        "POST",
        &format!("/2021-10-31/functions/{FUNCTION}/url"),
        &json!({ "AuthType": "AWS_IAM" }),
    )
    .await;
    assert_eq!(
        published.status,
        201,
        "case {case:?}: CreateFunctionUrlConfig must succeed, got {}",
        published.summary()
    );
    let url = published
        .json()
        .get("FunctionUrl")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the create must answer a FunctionUrl, got {}",
                published.summary()
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
            &format!("{FUNCTIONS_URI}/{FUNCTION}/policy"),
            &statement,
        )
        .await;
        assert_eq!(
            granted.status,
            201,
            "case {case:?}: AddPermission must succeed, got {}",
            granted.summary()
        );
    }
    url
}

/// The authority a published `FunctionUrl` is reached at, and the path that URL is the base of
/// — a client appends its own path to the value it was handed.
fn published(case: &str, url: &str) -> (String, String) {
    let rest = url.strip_prefix("https://").unwrap_or_else(|| {
        panic!("case {case:?}: the published URL must carry a scheme, got {url:?}")
    });
    let (authority, base) = rest.split_once('/').unwrap_or((rest, ""));
    (
        authority.to_string(),
        format!("/{}", base.trim_end_matches('/')),
    )
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
}

/// The headers one SigV4-signed request to the published address travels with: the signature
/// the workspace's signer computed over that exact request, read back off it, beside the headers
/// `carried` names — which are signed with it, as a client's own are.
async fn signature_for(
    authority: &str,
    method: &str,
    target: &str,
    body: &[u8],
    carried: &[(&str, &str)],
) -> Vec<(String, String)> {
    let signature = signed_request(
        SIGNING_NAME,
        method,
        &format!("https://{authority}{target}"),
        body.to_vec(),
        carried,
    )
    .await;
    signature
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

/// Dial the published address and exchange one request on it, as a client handed that endpoint
/// does: a connection in the clear, HTTP/1.1, with the authority it dialled as `Host` and the
/// headers `credentialled` names — the signature a signed caller travels with, and nothing at
/// all for a caller that signs nothing.
///
/// Only the PORT is taken from the published authority: the core binds its listeners on
/// loopback (`EMULATOR_BIND_ADDRESS`, `127.0.0.1` by default), so the dial is to this machine.
async fn exchange(
    case: &str,
    authority: &str,
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

    let mut request = format!("{method} {target} HTTP/1.1\r\nHost: {authority}\r\n");
    for (name, value) in credentialled {
        request.push_str(&format!("{name}: {value}\r\n"));
    }
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

/// One request a row signs and sends to the published address.
#[derive(Clone, Copy, Debug)]
struct Request {
    method: &'static str,
    /// The path the client appends to the endpoint it was handed.
    path: &'static str,
    /// The query that path carries, empty for a request carrying none.
    query: &'static str,
    /// The body, empty for a request carrying none.
    body: &'static str,
    /// Whether the client SIGNS the length it declared, which a real signer does for a request
    /// carrying a body.
    signs_length: bool,
    /// The HTTP status the endpoint must answer: `200` for a handler that described a response,
    /// `502` for a run that produced none at all.
    status: u16,
}

/// Sign `made` and send it to `authority`, asserting the status the row expects and, for a
/// request the function answered, that the answer is the FUNCTION's.
async fn send_signed(case: &str, authority: &str, base: &str, index: usize, made: &Request) {
    let target = if made.query.is_empty() {
        format!("{base}{path}", path = made.path)
    } else {
        format!(
            "{base}{path}?{query}",
            path = made.path,
            query = made.query
        )
    };
    let declared_length = made.body.len().to_string();
    let mut carried: Vec<(&str, &str)> = Vec::new();
    if !made.body.is_empty() {
        carried.push(("content-type", "application/json"));
    }
    if made.signs_length {
        carried.push(("content-length", declared_length.as_str()));
    }
    let credentialled =
        signature_for(authority, made.method, &target, made.body.as_bytes(), &carried).await;
    let served = exchange(
        case,
        authority,
        made.method,
        &target,
        made.body.as_bytes(),
        &credentialled,
    )
    .await;
    assert_eq!(
        served.status, made.status,
        "case {case:?}: request {index} of {made:?} must be answered {}, got {}",
        made.status, served.raw
    );
    if made.status == 200 {
        assert!(
            served.carries_header(BACKEND_HEADER, BACKEND_MARKER),
            "case {case:?}: request {index} of {made:?} must carry the function's own \
             {BACKEND_HEADER}: {BACKEND_MARKER}, got {}",
            served.raw
        );
        let echoed = served.json();
        assert_eq!(
            echoed.get("path").and_then(Value::as_str),
            Some(made.path),
            "case {case:?}: request {index} must reach the path {:?}, got {}",
            made.path,
            served.body
        );
        assert_eq!(
            echoed.get("rawQueryString").and_then(Value::as_str),
            Some(made.query),
            "case {case:?}: request {index} must reach the query {:?}, got {}",
            made.query,
            served.body
        );
    }
}

/// One row of the table: the requests it signs and sends, and what the three series must then
/// report about them.
struct Row {
    case: &'static str,
    requests: &'static [Request],
    /// What `Invocations` must sum to: the requests of the row that reached the function.
    invocation_sum: f64,
    /// How many measurements `Duration` and `Errors` must each hold: one per invocation that
    /// ran.
    samples: f64,
    /// What `Errors` must sum to: the runs of the row that came to a function error.
    error_sum: f64,
}

/// The rows. Every expectation is written out rather than computed from the requests, so a
/// publisher that counted requests and one that counted runs cannot both pass.
const ROWS: &[Row] = &[
    Row {
        case: "a bare signed GET of the endpoint's own root",
        requests: &[Request {
            method: "GET",
            path: "/",
            query: "",
            body: "",
            signs_length: false,
            status: 200,
        }],
        invocation_sum: 1.0,
        samples: 1.0,
        error_sum: 0.0,
    },
    Row {
        case: "a signed GET of a nested path carrying a query",
        requests: &[Request {
            method: "GET",
            path: "/direct/ok",
            query: "via=sigv4&page=2",
            body: "",
            signs_length: false,
            status: 200,
        }],
        invocation_sum: 1.0,
        samples: 1.0,
        error_sum: 0.0,
    },
    Row {
        case: "a signed POST carrying a body whose declared length is signed too",
        requests: &[Request {
            method: "POST",
            path: "/orders",
            query: "",
            body: r#"{"item":"one"}"#,
            signs_length: true,
            status: 200,
        }],
        invocation_sum: 1.0,
        samples: 1.0,
        error_sum: 0.0,
    },
    Row {
        case: "a signed GET whose handler raises, answered as a bad gateway",
        requests: &[Request {
            method: "GET",
            path: "/raise",
            query: "",
            body: "",
            signs_length: false,
            status: 502,
        }],
        invocation_sum: 1.0,
        samples: 1.0,
        error_sum: 1.0,
    },
    Row {
        case: "three signed requests of their own methods, one of them failing",
        requests: &[
            Request {
                method: "GET",
                path: "/first",
                query: "",
                body: "",
                signs_length: false,
                status: 200,
            },
            Request {
                method: "GET",
                path: "/raise",
                query: "",
                body: "",
                signs_length: false,
                status: 502,
            },
            Request {
                method: "PUT",
                path: "/third",
                query: "",
                body: r#"{"item":"two"}"#,
                signs_length: false,
                status: 200,
            },
        ],
        invocation_sum: 3.0,
        samples: 3.0,
        error_sum: 1.0,
    },
];

/// Assert the three series of `dimensions` report what `row` expects of them.
async fn assert_series(
    router: &MultiServiceRouter,
    case: &str,
    dimensions: &Value,
    invocation_sum: f64,
    samples: f64,
    error_sum: f64,
) {
    let invocations = datapoints(router, case, INVOCATIONS_SERIES, COUNT_UNIT, dimensions).await;
    let counted = total(&invocations, "Sum");
    assert_eq!(
        counted, invocation_sum,
        "case {case:?}: {INVOCATIONS_SERIES} of {dimensions} must sum to {invocation_sum}, got \
         {counted} over {invocations:?}"
    );

    let durations = datapoints(router, case, DURATION_SERIES, MILLISECONDS_UNIT, dimensions).await;
    let measured = total(&durations, "SampleCount");
    assert_eq!(
        measured, samples,
        "case {case:?}: {DURATION_SERIES} of {dimensions} must hold {samples} measurement(s), \
         got {measured} over {durations:?}"
    );
    assert!(
        durations
            .iter()
            .all(|point| point["Unit"].as_str() == Some(MILLISECONDS_UNIT)),
        "case {case:?}: every {DURATION_SERIES} datapoint of {dimensions} must carry the unit \
         {MILLISECONDS_UNIT:?}, got {durations:?}"
    );

    // `Errors` is the binary indicator of what one invocation came to, so every run reports on
    // it — 1 for a run that failed and 0 for one that returned. The SUM is therefore the
    // failures and the SAMPLE COUNT is the runs, which is what tells a function that failed
    // nothing from one nobody invoked.
    let errors = datapoints(router, case, ERRORS_SERIES, COUNT_UNIT, dimensions).await;
    let failed = total(&errors, "Sum");
    assert_eq!(
        failed, error_sum,
        "case {case:?}: {ERRORS_SERIES} of {dimensions} must sum to {error_sum}, got {failed} \
         over {errors:?}"
    );
    let reported = total(&errors, "SampleCount");
    assert_eq!(
        reported, samples,
        "case {case:?}: {ERRORS_SERIES} of {dimensions} must report on each of the {samples} \
         run(s), got {reported} over {errors:?}"
    );
}

/// RULE: a correctly SigV4-signed request to the address an `AWS_IAM` function URL was
/// published on is an invocation of the function — served the response the handler described —
/// and is metered on that function's own `AWS/Lambda` series as one.
#[tokio::test]
async fn a_signed_aws_iam_function_url_request_is_served_and_metered_as_an_invocation() {
    for row in ROWS {
        // One account per row, so the only invocations in the window are this row's.
        let engine = Arc::new(engine::InMemoryEngine::new());
        let router = authenticating_emulator(&engine);
        let case = row.case;
        let url = given_signed_url(&router, &engine, case).await;
        let (authority, base) = published(case, &url);

        for (index, made) in row.requests.iter().enumerate() {
            send_signed(case, &authority, &base, index, made).await;
        }

        for dimensions in [function_dimensions(FUNCTION), resource_dimensions(FUNCTION)] {
            assert_series(
                &router,
                case,
                &dimensions,
                row.invocation_sum,
                row.samples,
                row.error_sum,
            )
            .await;
        }

        // Reading the series changes nothing a later reader sees.
        let again = datapoints(
            &router,
            case,
            INVOCATIONS_SERIES,
            COUNT_UNIT,
            &function_dimensions(FUNCTION),
        )
        .await;
        let counted_again = total(&again, "Sum");
        assert_eq!(
            counted_again, row.invocation_sum,
            "case {case:?}: a second read of {INVOCATIONS_SERIES} must report the same {}, got \
             {counted_again} over {again:?}",
            row.invocation_sum
        );
    }
}

/// RULE, on the identity of the series rather than on its values: the series is the FUNCTION's,
/// so an invocation a signed client started through the published function URL and one started
/// through the `Invoke` API accumulate on ONE series instead of each surface publishing its own.
#[tokio::test]
async fn a_signed_url_invocation_and_an_invoke_api_call_share_one_series() {
    let case = "one signed request through the URL and one through Invoke";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = authenticating_emulator(&engine);
    let url = given_signed_url(&router, &engine, case).await;
    let (authority, base) = published(case, &url);

    send_signed(
        case,
        &authority,
        &base,
        0,
        &Request {
            method: "GET",
            path: "/through-the-url",
            query: "",
            body: "",
            signs_length: false,
            status: 200,
        },
    )
    .await;

    let invoked = control(
        &router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        &json!({ "rawPath": "/through-invoke" }),
    )
    .await;
    assert_eq!(
        invoked.status,
        200,
        "case {case:?}: Invoke must answer the run, got {}",
        invoked.summary()
    );

    assert_series(&router, case, &function_dimensions(FUNCTION), 2.0, 2.0, 0.0).await;
}

/// RULE, the half of the same contract that makes the rows above a real positive control: the
/// endpoint admits a caller on its SIGNATURE, so the same address that serves a signed request
/// refuses one carrying no credential at all the Lambda front door's `403` document, with
/// nothing invoked and nothing metered — and then serves the signed request made after it and
/// meters exactly that one.
#[tokio::test]
async fn the_same_address_refuses_an_unsigned_caller_and_meters_only_the_signed_one() {
    let case = "an unsigned request and then a signed one, on one AWS_IAM endpoint";
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = authenticating_emulator(&engine);
    let url = given_signed_url(&router, &engine, case).await;
    let (authority, base) = published(case, &url);

    let refused = exchange(
        case,
        &authority,
        "GET",
        &format!("{base}/unsigned"),
        &[],
        &[],
    )
    .await;
    assert_eq!(
        refused.status, 403,
        "case {case:?}: an unsigned caller must be refused 403, got {}",
        refused.raw
    );
    assert_eq!(
        refused.json().get("Message").and_then(Value::as_str),
        Some(FRONT_DOOR_FORBIDDEN_MESSAGE),
        "case {case:?}: the refusal must be the front door's own document, got {}",
        refused.body
    );
    assert!(
        !refused.carries_header(BACKEND_HEADER, BACKEND_MARKER),
        "case {case:?}: a refused request must carry nothing of the function's answer, got {}",
        refused.raw
    );
    assert_series(&router, case, &function_dimensions(FUNCTION), 0.0, 0.0, 0.0).await;

    send_signed(
        case,
        &authority,
        &base,
        1,
        &Request {
            method: "GET",
            path: "/signed",
            query: "",
            body: "",
            signs_length: false,
            status: 200,
        },
    )
    .await;
    assert_series(&router, case, &function_dimensions(FUNCTION), 1.0, 1.0, 0.0).await;
}
