// SPDX-License-Identifier: CC0-1.0

//! A deployed method whose `HTTP_PROXY` integration names a Lambda function URL that SERVES
//! IN THE RESPONSE-STREAMING MODE.
//!
//! RULE: `HTTP_PROXY` is "for integrating the API method request with an HTTP endpoint …
//! with the client request passed through as-is", so the method is answered with the
//! endpoint's OWN response — and a function URL whose `InvokeMode` is `RESPONSE_STREAM`
//! answers the bytes the handler wrote, shaped by the metadata it announced ahead of them
//! (`awslambda.HttpResponseStream.from`). So the metadata's status and headers and the
//! streamed body are what the caller of the stage is served, a stream that was never begun
//! is the `502` every unusable backend response earns, and the bytes a streamed answer
//! carries are not bounded by what one buffered invocation may carry.
//!
//! The rows differ along what the rule generalises over: whether the handler announced the
//! response, how many writes it made, how MANY BYTES it wrote (one small answer and one past
//! the payload a buffered invocation carries), and a run that wrote nothing at all.

use std::io::Write as _;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The function whose URL the integration names.
const FUNCTION: &str = "apigw-url-streamer";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// How long to wait between polls of a clock-owed transition, and how many polls to make.
const POLL: Duration = Duration::from_millis(25);
const POLLS: usize = 800;

/// How many 64 KiB blocks the `/big` row's handler writes: enough that the streamed answer is
/// past the 6 MB payload one buffered invocation carries, which is what a streamed answer is
/// not bounded by. Interpolated into the handler's own source ([`handler_source`]), so one
/// number governs what is written and what the row asserts.
const BIG_BLOCKS: usize = 112;

/// How large each of those blocks is.
const BLOCK_BYTES: usize = 64 * 1024;

/// The handler every row runs: it chooses what to write from the event's own `rawPath`, so
/// one function serves every row and each answer is the RUN's own bytes.
fn handler_source() -> String {
    format!(
        r#"
exports.handler = awslambda.streamifyResponse(async (event, responseStream, context) => {{
    const path = event.rawPath;
    if (path === '/shaped') {{
        const shaped = awslambda.HttpResponseStream.from(responseStream, {{
            statusCode: 201,
            headers: {{ 'content-type': 'text/plain', 'x-written-by': 'the handler' }}
        }});
        shaped.write('first;');
        shaped.write('second;');
        shaped.write('done');
        return;
    }}
    if (path === '/big') {{
        const block = 'x'.repeat({BLOCK_BYTES});
        for (let written = 0; written < {BIG_BLOCKS}; written += 1) {{
            responseStream.write(block);
        }}
        return;
    }}
    if (path === '/never-written') {{
        throw new Error('nothing was written');
    }}
    responseStream.write('plain');
}});
"#
    )
}

/// One answered request: its status, its headers and its body verbatim.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: Bytes,
}

impl Answer {
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }
}

/// API Gateway and Lambda over ONE shared engine, as `core/router/src/lib.rs` mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(
            std::sync::Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `service`, which is what routes the
/// request. The signature itself is never checked.
fn authorization(service: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{service}/\
         aws4_request, SignedHeaders=host, Signature=0000"
    )
}

/// Dispatch one request as it arrives on the wire.
async fn dispatch(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: Bytes,
) -> Answer {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host);
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let response = router
        .dispatch(builder.body(body).expect("a well-formed request"))
        .await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// One API Gateway control-plane call that has to answer `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answer = dispatch(
        router,
        method,
        &format!("apigateway.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("apigateway")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answer.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// One Lambda `restJson1` call that has to answer `expected`.
async fn lambda(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answer = dispatch(
        router,
        method,
        &format!("lambda.{REGION}.amazonaws.com"),
        path,
        &[
            ("content-type", "application/json"),
            ("authorization", &authorization("lambda")),
        ],
        Bytes::from(payload),
    )
    .await;
    assert_eq!(
        answer.status, expected,
        "setup {case:?}: expected HTTP {expected} from Lambda, got {} with {:?}",
        answer.status,
        answer.text()
    );
    answer.json()
}

/// The member `name` of a JSON object, as text.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// A .zip deployment package holding `source` as `index.js`.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("index.js", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create the streaming function and wait until Lambda reports it `Active`.
async fn given_streaming_function(router: &MultiServiceRouter) {
    lambda(
        router,
        "CreateFunction",
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "nodejs20.x",
            "Handler": "index.handler",
            "Timeout": 30,
            "MemorySize": 512,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(&handler_source())),
            },
        })),
        201,
    )
    .await;
    for _ in 0..POLLS {
        let reported = lambda(
            router,
            "GetFunctionConfiguration",
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
            200,
        )
        .await;
        if reported.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup: Lambda never reported {FUNCTION} Active");
}

/// `CreateFunctionUrlConfig` on the function in the response-streaming mode, granted to the
/// public, answering the endpoint Lambda published for it.
async fn given_streaming_url(router: &MultiServiceRouter) -> String {
    let created = lambda(
        router,
        "CreateFunctionUrlConfig",
        "POST",
        &format!("/2021-10-31/functions/{FUNCTION}/url"),
        Some(json!({ "AuthType": "NONE", "InvokeMode": "RESPONSE_STREAM" })),
        201,
    )
    .await;
    lambda(
        router,
        "AddPermission",
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "public-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": "*",
            "FunctionUrlAuthType": "NONE",
        })),
        201,
    )
    .await;
    member("CreateFunctionUrlConfig", &created, "FunctionUrl")
}

/// RULE: the method is answered with the streaming function URL's own response — the
/// metadata the handler announced and the bytes it wrote — whatever the size of those bytes,
/// and a run that wrote nothing earns the `502` every unusable backend response does.
#[tokio::test]
async fn an_http_proxy_method_answers_with_the_streamed_function_urls_own_response() {
    let router = emulator();
    given_streaming_function(&router).await;
    let endpoint = given_streaming_url(&router).await;
    let base = endpoint.trim_end_matches('/').to_string();

    let created = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "http-proxy-streaming-function-url" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &created, "id");
    let root = member("CreateRestApi", &created, "rootResourceId");
    let resource = apigw(
        &router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": "{proxy+}" })),
        201,
    )
    .await;
    let resource = member("CreateResource", &resource, "id");
    apigw(
        &router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/ANY"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        &router,
        "PutIntegration",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/ANY/integration"),
        Some(json!({
            "type": "HTTP_PROXY",
            "httpMethod": "ANY",
            "uri": format!("{base}/{{proxy}}"),
            "requestParameters": {
                "integration.request.path.proxy": "method.request.path.proxy",
            },
        })),
        201,
    )
    .await;
    apigw(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    /// One row: the path it addresses and what the stage must answer it with.
    struct Row {
        case: &'static str,
        path: &'static str,
        status: u16,
        /// The body the caller must be served, for a row whose body is fixed text.
        body: Option<&'static str>,
        /// How many bytes it must carry, for the row whose body is too large to spell.
        bytes: Option<usize>,
        /// The media type the response must carry.
        content_type: Option<&'static str>,
    }
    let rows: &[Row] = &[
        Row {
            case: "a handler that announced the response ahead of its bytes",
            path: "/shaped",
            status: 201,
            body: Some("first;second;done"),
            bytes: None,
            content_type: Some("text/plain"),
        },
        Row {
            case: "a handler that announced nothing: its bytes, served whole",
            path: "/plain",
            status: 200,
            body: Some("plain"),
            bytes: None,
            content_type: Some("application/octet-stream"),
        },
        Row {
            case: "a stream past the payload one buffered invocation carries",
            path: "/big",
            status: 200,
            body: None,
            bytes: Some(BIG_BLOCKS * BLOCK_BYTES),
            content_type: Some("application/octet-stream"),
        },
        Row {
            case: "a run that wrote nothing at all",
            path: "/never-written",
            status: 502,
            body: None,
            bytes: None,
            content_type: None,
        },
    ];

    for row in rows {
        let answered = dispatch(
            &router,
            "GET",
            &format!("{api}.execute-api.{REGION}.amazonaws.com"),
            &format!("/{STAGE}{}", row.path),
            &[],
            Bytes::new(),
        )
        .await;
        assert_eq!(
            answered.status,
            row.status,
            "case {:?}: expected HTTP {}, got {} with {:?}",
            row.case,
            row.status,
            answered.status,
            answered.json()
        );
        if let Some(body) = row.body {
            assert_eq!(
                answered.text(),
                body,
                "case {:?}: expected the body {body:?}, got {:?}",
                row.case,
                answered.text()
            );
        }
        if let Some(bytes) = row.bytes {
            assert_eq!(
                answered.body.len(),
                bytes,
                "case {:?}: expected {bytes} bytes of streamed body, got {}",
                row.case,
                answered.body.len()
            );
        }
        if let Some(content_type) = row.content_type {
            assert_eq!(
                answered.header("content-type").as_deref(),
                Some(content_type),
                "case {:?}: expected the media type {content_type:?}, got {:?}",
                row.case,
                answered.header("content-type")
            );
        }
    }
}
