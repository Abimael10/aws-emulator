// SPDX-License-Identifier: CC0-1.0

//! A deployed method whose integration is a STANDARD (non-proxy) `HTTP` one: the three
//! transformations such an integration states about the request it makes and the response it
//! answers, and the `415` a content type nothing maps earns.
//!
//! `IntegrationType` gives `HTTP` as "for integrating an API method request with an HTTP
//! backend", which — unlike `HTTP_PROXY` — TRANSFORMS both halves of the call from the api's
//! own records. Three rules are read here, each one the member that states it:
//!
//! * `Integration$contentHandling` / `IntegrationResponse$contentHandling` —
//!   "`CONVERT_TO_BINARY`: Converts a request payload from a base64-encoded string to the
//!   corresponding binary blob", "`CONVERT_TO_TEXT`: Converts a request payload from a binary
//!   blob to a base64-encoded string", and "If this property is not defined, the request
//!   payload will be passed through from the method request to the integration request
//!   without modification". WHICH form a payload is in, and which form the call WANTS, is
//!   `RestApi$binaryMediaTypes`' to say — "the list of binary media types supported by the
//!   RestApi. By default, the RestApi supports only UTF-8-encoded text payloads" — read off
//!   the method request's `Content-Type` for an integration request and off the request's
//!   `Accept` for a method response, with a request that names no `Accept` accepting whatever
//!   the integration answered. So a text payload the call wants as bytes and that is no base64
//!   is the configuration error of an integration that cannot be performed (`500`);
//! * the OVERRIDE variables a mapping template may state
//!   (`$context.requestOverride.header.<name>`, `.querystring.<name>`,
//!   `$context.responseOverride.status`, `$context.responseOverride.header.<name>`): what
//!   they state replaces what `requestParameters`/`responseParameters` mapped, and a
//!   parameter a template overrides twice keeps the value stated FIRST;
//! * `Integration$passthroughBehavior` — `WHEN_NO_TEMPLATES` with a template defined
//!   "rejects the method request of an unmapped content-type with an HTTP 415 Unsupported
//!   Media Type response".
//!
//! The backend every case is integrated with is a Lambda function URL — "a dedicated HTTP(S)
//! endpoint that you can use to invoke your function" — mounted on this suite's own engine
//! beside API Gateway, and its handler REPORTS the bytes, the headers and the query string it
//! was handed. So a row asserts the transformation where the backend saw it, not where this
//! api claims it sent it. The last case is the other half of that pair: a function URL whose
//! `AuthType` is `AWS_IAM` refuses an unsigned caller itself, and what an `HTTP_PROXY`
//! integration pointed at it forwards is that refusal's own bytes.

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

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The module and attribute the deployment package carries its handler as.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long one invocation is given, as the function's own `Timeout`.
const FUNCTION_TIMEOUT: u64 = 30;

/// How long to wait between polls of a clock-owed transition, and how many polls to make:
/// a generous deadline, reached only by a transition that never comes.
const POLL: Duration = Duration::from_millis(25);
const POLLS: usize = 1_200;

/// The media type this suite's api declares BINARY, which is what decides the form a payload
/// is already in (`RestApi$binaryMediaTypes`).
const BINARY_MEDIA_TYPE: &str = "application/octet-stream";

/// A text media type the api declares nothing about, so a payload under it is text.
const TEXT_MEDIA_TYPE: &str = "text/plain";

/// The bytes every content-handling case is about, and their base64 text. Chosen to be text
/// in both forms, so the BACKEND can report either of them back and a row asserts which of
/// the two it was handed.
const RAW_PAYLOAD: &str = "hello world";
/// See [`RAW_PAYLOAD`]: `base64("hello world")`, over the alphabet and padding RFC 4648
/// states.
const BASE64_PAYLOAD: &str = "aGVsbG8gd29ybGQ=";

/// A text payload that is not base64 at all, so no conversion to binary decodes it.
const NOT_BASE64: &str = "not base64 at all";

/// The header a request template overrides, and the query-string parameter it overrides.
const FLOW_HEADER: &str = "x-flow";
const TAG_PARAMETER: &str = "tag";

/// The sentences API Gateway answers the two refusals these cases earn with.
const INTERNAL_SERVER_ERROR: &str = "Internal server error";
const UNSUPPORTED_MEDIA_TYPE: &str = "Unsupported Media Type";

/// The body a function URL whose `AuthType` is `AWS_IAM` answers a caller it did not
/// authenticate: the front door's own refusal, naming nothing of the request.
const FORBIDDEN_BODY: &str = r#"{"Message":"Forbidden"}"#;

/// The handler behind the function URL.
///
/// Asked for a `respond` mode it answers a FIXED payload under a fixed media type, which is
/// what the response-side cases convert; asked for none it REPORTS what it was handed — the
/// body (as the event carries it, with `isBase64Encoded` saying which form that is), the
/// header a request override states, the query string and the media type the integration
/// request declared.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    query = event.get("queryStringParameters") or {}
    headers = event.get("headers") or {}
    mode = query.get("respond") or ""
    if mode == "base64":
        return {
            "statusCode": 200,
            "headers": {"content-type": "text/plain"},
            "body": "aGVsbG8gd29ybGQ=",
        }
    if mode == "bytes":
        return {
            "statusCode": 200,
            "headers": {"content-type": "application/octet-stream"},
            "body": "hello world",
        }
    if mode == "unreadable":
        return {
            "statusCode": 200,
            "headers": {"content-type": "text/plain"},
            "body": "not base64 at all",
        }
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "body": event.get("body") or "",
                "encoded": bool(event.get("isBase64Encoded")),
                "query": event.get("rawQueryString") or "",
                "flow": headers.get("x-flow") or "",
                "sent": headers.get("content-type") or "",
            }
        ),
    }
"#;

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

    /// The `message` member of a gateway response's body.
    fn message(&self) -> Option<String> {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    /// The BYTES the backend reported it was handed, decoded from the form the event carried
    /// them in, or `None` for an answer that is not the handler's own report.
    fn backend_payload(&self) -> Option<Vec<u8>> {
        let reported = self.json();
        let body = reported.get("body")?.as_str()?.to_string();
        if reported.get("encoded")?.as_bool()? {
            base64::engine::general_purpose::STANDARD.decode(body).ok()
        } else {
            Some(body.into_bytes())
        }
    }

    /// One member of the handler's own report, as text.
    fn reported(&self, name: &str) -> String {
        self.json()
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// API Gateway and Lambda over ONE shared engine, as `core/router/src/lib.rs` mounts them:
/// the integration request the stage makes is dispatched back into this same router, so the
/// function URL it reaches is the one Lambda published here.
fn emulator() -> MultiServiceRouter {
    emulator_on(&std::sync::Arc::new(engine::InMemoryEngine::new()))
}

/// The same emulator over an engine the caller holds, so a second router can be built over
/// the records the first one created — which is how a case drives the SAME resources through
/// an emulator that verifies request signatures.
fn emulator_on(engine: &std::sync::Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature is never checked here, as in every other suite of this crate; the
    // credential scope is what routes a request.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(
            std::sync::Arc::clone(engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(
            std::sync::Arc::clone(engine),
        ),
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

/// A .zip deployment package holding `source` as `lambda_function.py`.
fn package(source: &str) -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("lambda_function.py", options)
            .expect("the entry starts");
        writer
            .write_all(source.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Create `name` from a package running [`HANDLER_SOURCE`] and wait until Lambda reports it
/// `Active` — a transition its own service owes, so this looks for the condition rather than
/// sleeping a span on its behalf.
async fn given_function(router: &MultiServiceRouter, name: &str) {
    lambda(
        router,
        &format!("CreateFunction({name})"),
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Timeout": FUNCTION_TIMEOUT,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        })),
        201,
    )
    .await;
    for _ in 0..POLLS {
        let reported = lambda(
            router,
            &format!("GetFunctionConfiguration({name})"),
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
            200,
        )
        .await;
        if reported.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        tokio::time::sleep(POLL).await;
    }
    panic!("setup: Lambda never reported {name} Active");
}

/// `CreateFunctionUrlConfig` on `function` under `auth_type`, answering the endpoint Lambda
/// minted for it, with the public granted the invocation where the URL admits an anonymous
/// caller at all.
async fn given_function_url(
    router: &MultiServiceRouter,
    function: &str,
    auth_type: &str,
) -> String {
    let created = lambda(
        router,
        &format!("CreateFunctionUrlConfig({function})"),
        "POST",
        &format!("/2021-10-31/functions/{function}/url"),
        Some(json!({ "AuthType": auth_type })),
        201,
    )
    .await;
    if auth_type == "NONE" {
        lambda(
            router,
            &format!("AddPermission({function})"),
            "POST",
            &format!("/2015-03-31/functions/{function}/policy"),
            Some(json!({
                "StatementId": "public-url",
                "Action": "lambda:InvokeFunctionUrl",
                "Principal": "*",
                "FunctionUrlAuthType": "NONE",
            })),
            201,
        )
        .await;
    }
    member(
        &format!("CreateFunctionUrlConfig({function})"),
        &created,
        "FunctionUrl",
    )
    .trim_end_matches('/')
    .to_string()
}

/// An api declaring [`BINARY_MEDIA_TYPE`] binary, as its id and its root resource id.
async fn given_api(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": name, "binaryMediaTypes": [BINARY_MEDIA_TYPE] })),
        201,
    )
    .await;
    (
        member("CreateRestApi", &created, "id"),
        member("CreateRestApi", &created, "rootResourceId"),
    )
}

/// One resource of `api` carrying an `ANY` method whose integration is `integration`, with
/// the integration response `integration_response` for the `200` the backend answers.
async fn given_route(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
    path_part: &str,
    integration: Value,
    integration_response: Value,
) {
    let created = apigw(
        router,
        &format!("CreateResource({path_part})"),
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": path_part })),
        201,
    )
    .await;
    let resource = member(&format!("CreateResource({path_part})"), &created, "id");
    apigw(
        router,
        &format!("PutMethod({path_part})"),
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/ANY"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        &format!("PutIntegration({path_part})"),
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/ANY/integration"),
        Some(integration),
        201,
    )
    .await;
    apigw(
        router,
        &format!("PutMethodResponse({path_part})"),
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/ANY/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw(
        router,
        &format!("PutIntegrationResponse({path_part})"),
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/ANY/integration/responses/200"),
        Some(integration_response),
        201,
    )
    .await;
}

/// The integration body of a STANDARD `HTTP` integration against `uri`.
///
/// `content_handling` is the conversion it states about the integration request's payload,
/// and `request_parameters` what it maps onto that request. `integrationHttpMethod` travels
/// under its `jsonName`, `httpMethod`.
fn http_integration(
    uri: &str,
    content_handling: Option<&str>,
    request_parameters: Value,
    request_templates: Option<Value>,
) -> Value {
    let mut integration = json!({
        "type": "HTTP",
        "httpMethod": "POST",
        "uri": uri,
        "requestParameters": request_parameters,
    });
    if let Some(stated) = content_handling {
        integration["contentHandling"] = json!(stated);
    }
    if let Some(templates) = request_templates {
        integration["requestTemplates"] = templates;
        // `WHEN_NO_TEMPLATES`: with a template defined, a content type none of them keys
        // "will be rejected with an HTTP 415 Unsupported Media Type response".
        integration["passthroughBehavior"] = json!("WHEN_NO_TEMPLATES");
    }
    integration
}

/// Deploy `api` to [`STAGE`].
async fn given_deployment(router: &MultiServiceRouter, api: &str) {
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;
}

/// One request to the deployed stage, carrying `body` under `content_type`.
async fn invoke(
    router: &MultiServiceRouter,
    api: &str,
    path: &str,
    content_type: Option<&str>,
    body: &str,
) -> Answer {
    invoke_accepting(router, api, path, content_type, None, body).await
}

/// The same request, declaring the media type the client ACCEPTS — which is what names the
/// form the method response's payload is wanted in.
async fn invoke_accepting(
    router: &MultiServiceRouter,
    api: &str,
    path: &str,
    content_type: Option<&str>,
    accept: Option<&str>,
    body: &str,
) -> Answer {
    let mut headers: Vec<(&str, &str)> = Vec::new();
    if let Some(content_type) = content_type {
        headers.push(("content-type", content_type));
    }
    if let Some(accept) = accept {
        headers.push(("accept", accept));
    }
    dispatch(
        router,
        "POST",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{path}"),
        &headers,
        Bytes::from(body.to_string()),
    )
    .await
}

/// One row of the REQUEST-side conversion: the conversion the integration states, the media
/// type the client declares and the body it sends, and the bytes the BACKEND has to be
/// handed — or the refusal the call earns instead.
struct RequestRow {
    case: &'static str,
    /// The path part of the resource whose integration states the conversion.
    route: &'static str,
    content_type: &'static str,
    sent: &'static str,
    /// The bytes the backend must report, or `None` for a row answered without any backend
    /// being called.
    handed: Option<&'static str>,
    status: u16,
}

#[tokio::test]
async fn content_handling_converts_the_integration_requests_payload() {
    let router = emulator();
    given_function(&router, "nonproxy-request-backend").await;
    let base = given_function_url(&router, "nonproxy-request-backend", "NONE").await;
    let (api, root) = given_api(&router, "nonproxy-request-conversion").await;

    // One route per conversion the integration may state; the row varies the media type the
    // CLIENT declares, which is what says the form its payload is already in.
    for (route, stated) in [
        ("tobinary", Some("CONVERT_TO_BINARY")),
        ("totext", Some("CONVERT_TO_TEXT")),
        ("unstated", None),
    ] {
        given_route(
            &router,
            &api,
            &root,
            route,
            http_integration(&format!("{base}/echo"), stated, json!({}), None),
            json!({}),
        )
        .await;
    }
    given_deployment(&router, &api).await;

    let rows = [
        RequestRow {
            case: "CONVERT_TO_BINARY decodes a text payload before the backend sees it",
            route: "tobinary",
            content_type: TEXT_MEDIA_TYPE,
            sent: BASE64_PAYLOAD,
            handed: Some(RAW_PAYLOAD),
            status: 200,
        },
        RequestRow {
            case: "CONVERT_TO_BINARY is a no-op on a payload the api declares binary",
            route: "tobinary",
            content_type: BINARY_MEDIA_TYPE,
            sent: BASE64_PAYLOAD,
            handed: Some(BASE64_PAYLOAD),
            status: 200,
        },
        RequestRow {
            case: "CONVERT_TO_BINARY over text that is no base64 is answered 500",
            route: "tobinary",
            content_type: TEXT_MEDIA_TYPE,
            sent: NOT_BASE64,
            handed: None,
            status: 500,
        },
        RequestRow {
            case: "CONVERT_TO_TEXT encodes a payload the api declares binary",
            route: "totext",
            content_type: BINARY_MEDIA_TYPE,
            sent: RAW_PAYLOAD,
            handed: Some(BASE64_PAYLOAD),
            status: 200,
        },
        RequestRow {
            case: "CONVERT_TO_TEXT is a no-op on a text payload",
            route: "totext",
            content_type: TEXT_MEDIA_TYPE,
            sent: RAW_PAYLOAD,
            handed: Some(RAW_PAYLOAD),
            status: 200,
        },
        RequestRow {
            case: "an unstated conversion forwards a text payload unmodified",
            route: "unstated",
            content_type: TEXT_MEDIA_TYPE,
            sent: BASE64_PAYLOAD,
            handed: Some(BASE64_PAYLOAD),
            status: 200,
        },
        RequestRow {
            case: "an unstated conversion forwards a binary payload unmodified",
            route: "unstated",
            content_type: BINARY_MEDIA_TYPE,
            sent: RAW_PAYLOAD,
            handed: Some(RAW_PAYLOAD),
            status: 200,
        },
    ];

    for row in &rows {
        let answered = invoke(
            &router,
            &api,
            &format!("/{}", row.route),
            Some(row.content_type),
            row.sent,
        )
        .await;
        assert_eq!(
            answered.status,
            row.status,
            "case {:?}: expected HTTP {}, got {} with {:?}",
            row.case,
            row.status,
            answered.status,
            answered.text()
        );
        let Some(handed) = row.handed else {
            assert_eq!(
                answered.message().as_deref(),
                Some(INTERNAL_SERVER_ERROR),
                "case {:?}: expected the {INTERNAL_SERVER_ERROR:?} gateway response, got {:?}",
                row.case,
                answered.text()
            );
            continue;
        };
        assert_eq!(
            answered.backend_payload(),
            Some(handed.as_bytes().to_vec()),
            "case {:?}: expected the backend to be handed {handed:?}, got {:?}",
            row.case,
            answered.text()
        );
        // The media type the integration request declares is the method request's own, which
        // is what the conversion was decided against.
        assert_eq!(
            answered.reported("sent"),
            row.content_type,
            "case {:?}: expected the backend to be handed the {:?} media type, got {:?}",
            row.case,
            row.content_type,
            answered.text()
        );
    }
}

/// One row of the RESPONSE-side conversion: the mode the backend answers in, the conversion
/// the integration response states, the media type the client ACCEPTS, and the bytes the
/// client has to read.
struct ResponseRow {
    case: &'static str,
    route: &'static str,
    /// The media type the request's `Accept` names, which is what says the form the method
    /// response's payload is wanted in; `None` for a request that accepts anything.
    accept: Option<&'static str>,
    /// The bytes the client must read, or `None` for a row answered a refusal instead.
    served: Option<&'static str>,
    status: u16,
}

#[tokio::test]
async fn content_handling_converts_the_method_responses_payload() {
    let router = emulator();
    given_function(&router, "nonproxy-response-backend").await;
    let base = given_function_url(&router, "nonproxy-response-backend", "NONE").await;
    let (api, root) = given_api(&router, "nonproxy-response-conversion").await;

    // One route per (backend mode, conversion) pair: the mode travels as the one query-string
    // parameter the integration maps onto its request, since a standard integration carries
    // nothing of the caller's that it does not map. The last two carry a response TEMPLATE, so
    // the payload served is the text it renders under the media type the mapping gives it
    // rather than the one the backend answered.
    for (route, mode, stated, templated) in [
        ("frombase64", "base64", Some("CONVERT_TO_BINARY"), false),
        ("frombase64pass", "base64", None, false),
        ("frombase64totext", "base64", Some("CONVERT_TO_TEXT"), false),
        ("frombytes", "bytes", Some("CONVERT_TO_TEXT"), false),
        ("frombytespass", "bytes", None, false),
        ("unreadable", "unreadable", Some("CONVERT_TO_BINARY"), false),
        ("unreadablepass", "unreadable", None, false),
        ("templated", "bytes", Some("CONVERT_TO_BINARY"), true),
        ("templatedpass", "bytes", None, true),
    ] {
        let mut response = json!({});
        if let Some(stated) = stated {
            response["contentHandling"] = json!(stated);
        }
        if templated {
            response["responseTemplates"] = json!({ "application/json": BASE64_PAYLOAD });
        }
        given_route(
            &router,
            &api,
            &root,
            route,
            http_integration(
                &format!("{base}/echo"),
                None,
                json!({ "integration.request.querystring.respond": format!("'{mode}'") }),
                None,
            ),
            response,
        )
        .await;
    }
    given_deployment(&router, &api).await;

    let rows = [
        ResponseRow {
            case: "CONVERT_TO_BINARY decodes the text the backend answered",
            route: "frombase64",
            accept: None,
            served: Some(RAW_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "nothing stated and nothing accepted: the backend's text is served as it is",
            route: "frombase64pass",
            accept: None,
            served: Some(BASE64_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "nothing stated, with an Accept the api declares binary: the text is decoded",
            route: "frombase64pass",
            accept: Some(BINARY_MEDIA_TYPE),
            served: Some(RAW_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "nothing stated, with an Accept the api declares nothing about",
            route: "frombase64pass",
            accept: Some(TEXT_MEDIA_TYPE),
            served: Some(BASE64_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "CONVERT_TO_TEXT is a no-op on the text the backend answered",
            route: "frombase64totext",
            accept: Some(BINARY_MEDIA_TYPE),
            served: Some(BASE64_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "CONVERT_TO_TEXT encodes a payload the api declares binary",
            route: "frombytes",
            accept: None,
            served: Some(BASE64_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "nothing stated, with an Accept the api declares binary: the bytes are served",
            route: "frombytespass",
            accept: Some(BINARY_MEDIA_TYPE),
            served: Some(RAW_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "nothing stated and no Accept at all: a binary payload travels as it is",
            route: "frombytespass",
            accept: None,
            served: Some(RAW_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "nothing stated, with an Accept the api declares nothing about: a binary \
                   payload is served as the base64 text of its bytes",
            route: "frombytespass",
            accept: Some(TEXT_MEDIA_TYPE),
            served: Some(BASE64_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "CONVERT_TO_BINARY over text that is no base64 is answered 500",
            route: "unreadable",
            accept: None,
            served: None,
            status: 500,
        },
        ResponseRow {
            case: "an Accept the api declares binary over text that is no base64: 500",
            route: "unreadablepass",
            accept: Some(BINARY_MEDIA_TYPE),
            served: None,
            status: 500,
        },
        ResponseRow {
            case: "the same text under an Accept wanting text is served as it is",
            route: "unreadablepass",
            accept: Some(TEXT_MEDIA_TYPE),
            served: Some(NOT_BASE64),
            status: 200,
        },
        ResponseRow {
            case: "CONVERT_TO_BINARY over a TEMPLATED body decodes the text the template \
                   rendered, whatever media type the backend answered",
            route: "templated",
            accept: None,
            served: Some(RAW_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "nothing stated over a TEMPLATED body, with an Accept the api declares \
                   binary: the text the template rendered is decoded",
            route: "templatedpass",
            accept: Some(BINARY_MEDIA_TYPE),
            served: Some(RAW_PAYLOAD),
            status: 200,
        },
        ResponseRow {
            case: "nothing stated over a TEMPLATED body and no Accept at all",
            route: "templatedpass",
            accept: None,
            served: Some(BASE64_PAYLOAD),
            status: 200,
        },
    ];

    for row in &rows {
        let answered = invoke_accepting(
            &router,
            &api,
            &format!("/{}", row.route),
            None,
            row.accept,
            "",
        )
        .await;
        assert_eq!(
            answered.status,
            row.status,
            "case {:?}: expected HTTP {}, got {} with {:?}",
            row.case,
            row.status,
            answered.status,
            answered.text()
        );
        match row.served {
            Some(served) => assert_eq!(
                answered.body.as_ref(),
                served.as_bytes(),
                "case {:?}: expected the client to read {served:?}, got {:?}",
                row.case,
                answered.text()
            ),
            None => assert_eq!(
                answered.message().as_deref(),
                Some(INTERNAL_SERVER_ERROR),
                "case {:?}: expected the {INTERNAL_SERVER_ERROR:?} gateway response, got {:?}",
                row.case,
                answered.text()
            ),
        }
    }
}

#[tokio::test]
async fn a_mapping_templates_overrides_reach_the_backend_and_the_client() {
    let router = emulator();
    given_function(&router, "nonproxy-override-backend").await;
    let base = given_function_url(&router, "nonproxy-override-backend", "NONE").await;
    let (api, root) = given_api(&router, "nonproxy-overrides").await;

    // A request template that states both an overridden header and an overridden
    // query-string parameter, over an integration that MAPS the same header to another
    // value: an override is final, so the backend is handed the overriding one.
    given_route(
        &router,
        &api,
        &root,
        "overridden",
        http_integration(
            &format!("{base}/echo"),
            None,
            json!({ format!("integration.request.header.{FLOW_HEADER}"): "'mapped'" }),
            Some(json!({
                "application/json": format!(
                    "#set($context.requestOverride.header.{FLOW_HEADER} = 'overridden')\
                     #set($context.requestOverride.querystring.{TAG_PARAMETER} = \
                     $input.params('{TAG_PARAMETER}'))$input.body"
                ),
            })),
        ),
        json!({}),
    )
    .await;
    // The same parameter stated TWICE: the value stated first is the one the backend sees.
    given_route(
        &router,
        &api,
        &root,
        "repeated",
        http_integration(
            &format!("{base}/echo"),
            None,
            json!({}),
            Some(json!({
                "application/json": format!(
                    "#set($context.requestOverride.header.{FLOW_HEADER} = 'first')\
                     #set($context.requestOverride.header.{FLOW_HEADER} = 'second')$input.body"
                ),
            })),
        ),
        json!({}),
    )
    .await;
    // A RESPONSE template that states the status and a header of the method response: the
    // backend answers `200` and the client reads the overridden status.
    given_route(
        &router,
        &api,
        &root,
        "turned",
        http_integration(&format!("{base}/echo"), None, json!({}), None),
        json!({
            "responseTemplates": {
                "application/json": "#set($context.responseOverride.status = 400)\
                     #set($context.responseOverride.header.x-overridden = 'yes')\
                     {\"turned\":true}",
            },
        }),
    )
    .await;
    // A content type no request template keys, under `WHEN_NO_TEMPLATES` with a template
    // defined: the method request is rejected rather than passed through.
    given_route(
        &router,
        &api,
        &root,
        "unmapped",
        http_integration(
            &format!("{base}/echo"),
            None,
            json!({}),
            Some(json!({ "application/json": "$input.body" })),
        ),
        json!({}),
    )
    .await;
    given_deployment(&router, &api).await;

    // The override of a header the integration also maps, and of a query-string parameter
    // nothing maps at all.
    let answered = invoke(
        &router,
        &api,
        &format!("/overridden?{TAG_PARAMETER}=from-the-caller"),
        Some("application/json"),
        r#"{"sent":true}"#,
    )
    .await;
    let case = "a request template that overrides a header and a query-string parameter";
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200, got {} with {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.reported("flow"),
        "overridden",
        "case {case:?}: expected the backend to be handed the overriding header, got {:?}",
        answered.text()
    );
    assert_eq!(
        answered.reported("query"),
        format!("{TAG_PARAMETER}=from-the-caller"),
        "case {case:?}: expected the overridden query-string parameter to reach the backend, \
         got {:?}",
        answered.text()
    );
    assert_eq!(
        answered.backend_payload(),
        Some(br#"{"sent":true}"#.to_vec()),
        "case {case:?}: expected the rendered template as the body, got {:?}",
        answered.text()
    );

    let answered = invoke(
        &router,
        &api,
        "/repeated",
        Some("application/json"),
        r#"{"sent":true}"#,
    )
    .await;
    let case = "a parameter the request template overrides twice";
    assert_eq!(
        answered.status, 200,
        "case {case:?}: expected HTTP 200, got {} with {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.reported("flow"),
        "first",
        "case {case:?}: expected the value stated first, got {:?}",
        answered.text()
    );

    let answered = invoke(
        &router,
        &api,
        "/turned",
        Some("application/json"),
        r#"{"sent":true}"#,
    )
    .await;
    let case = "a response template that overrides the status the backend answered";
    assert_eq!(
        answered.status, 400,
        "case {case:?}: expected the overriding HTTP 400, got {} with {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.header("x-overridden").as_deref(),
        Some("yes"),
        "case {case:?}: expected the overridden header, got {:?}",
        answered.headers
    );
    assert_eq!(
        answered.text(),
        r#"{"turned":true}"#,
        "case {case:?}: expected the rendered template as the body, got {:?}",
        answered.text()
    );

    let answered = invoke(
        &router,
        &api,
        "/unmapped",
        Some("text/csv"),
        "one,two\n1,2\n",
    )
    .await;
    let case = "a content type no request template keys, with passthrough WHEN_NO_TEMPLATES";
    assert_eq!(
        answered.status, 415,
        "case {case:?}: expected HTTP 415, got {} with {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.message().as_deref(),
        Some(UNSUPPORTED_MEDIA_TYPE),
        "case {case:?}: expected the {UNSUPPORTED_MEDIA_TYPE:?} gateway response, got {:?}",
        answered.text()
    );
}

#[tokio::test]
async fn an_iam_function_urls_refusal_of_an_unsigned_caller_is_what_a_proxy_forwards() {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(&engine);
    given_function(&router, "nonproxy-iam-url").await;
    let iam_url = given_function_url(&router, "nonproxy-iam-url", "AWS_IAM").await;
    let (api, root) = given_api(&router, "iam-function-url-proxy").await;
    given_route(
        &router,
        &api,
        &root,
        "forwarded",
        json!({
            "type": "HTTP_PROXY",
            "httpMethod": "POST",
            "uri": format!("{iam_url}/echo"),
        }),
        json!({}),
    )
    .await;
    given_deployment(&router, &api).await;
    // From here on the SAME emulator VERIFIES request signatures, as it does when it is
    // started: an `AWS_IAM` function URL is refused by the front door, and only an emulator
    // that reads credentials can take that refusal. The router above is kept only to build
    // the resources; nothing below uses it.
    let mut authenticating = emulator_on(&engine);
    authenticating.set_request_authentication(true);

    // A request to the function URL ITSELF, carrying no signature: the front door refuses it
    // before the plane runs, and the function does not run.
    let (host, path) = iam_url
        .trim_start_matches("https://")
        .split_once('/')
        .expect("the published function URL carries a path");
    let direct = dispatch(
        &authenticating,
        "POST",
        host,
        &format!("/{path}"),
        &[("content-type", "application/json")],
        Bytes::from_static(b"{}"),
    )
    .await;
    let case = "an unsigned request to an AWS_IAM function URL";
    assert_eq!(
        direct.status, 403,
        "case {case:?}: expected HTTP 403, got {} with {:?}",
        direct.status,
        direct.text()
    );
    assert_eq!(
        direct.text(),
        FORBIDDEN_BODY,
        "case {case:?}: expected the body {FORBIDDEN_BODY}, got {:?}",
        direct.text()
    );

    // The same refusal, forwarded by the proxy integration: its bytes are the endpoint's own.
    let forwarded = invoke(
        &authenticating,
        &api,
        "/forwarded",
        Some("application/json"),
        "{}",
    )
    .await;
    let case = "the same refusal forwarded by an HTTP_PROXY integration";
    assert_eq!(
        forwarded.status, 403,
        "case {case:?}: expected HTTP 403, got {} with {:?}",
        forwarded.status,
        forwarded.text()
    );
    assert_eq!(
        forwarded.text(),
        FORBIDDEN_BODY,
        "case {case:?}: expected the body {FORBIDDEN_BODY}, got {:?}",
        forwarded.text()
    );
}
