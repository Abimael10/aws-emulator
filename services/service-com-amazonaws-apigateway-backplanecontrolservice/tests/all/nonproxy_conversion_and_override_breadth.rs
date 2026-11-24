// SPDX-License-Identifier: CC0-1.0

//! The two transformations a STANDARD (non-proxy) `HTTP` integration states about the FORM a
//! payload travels in, and the parameters a request mapping template states about the
//! integration request — each asserted over a SET of payloads rather than at one length.
//!
//! `Integration$contentHandling` and `IntegrationResponse$contentHandling` target
//! `ContentHandlingStrategy` (`CONVERT_TO_BINARY | CONVERT_TO_TEXT`) and document
//! "`CONVERT_TO_BINARY`: Converts a request payload from a base64-encoded string to the
//! corresponding binary blob", "`CONVERT_TO_TEXT`: Converts a request payload from a binary
//! blob to a base64-encoded string" and "If this property is not defined, the request payload
//! will be passed through from the method request to the integration request without
//! modification" — with `RestApi$binaryMediaTypes` ("the list of binary media types supported
//! by the RestApi. By default, the RestApi supports only UTF-8-encoded text payloads") saying
//! which form a payload is already in. So:
//!
//! * a conversion is the ONE conversion each way over the payload's whole bytes, at every
//!   length: the rows below span the three padding residues the standard base64 alphabet and
//!   padding (RFC 4648) have, an empty payload, a multi-byte UTF-8 one and one of 320 bytes;
//! * text a call wants as bytes and that is not that encoding at all is the configuration
//!   error of an integration that cannot be performed (`500`), over a set of ways text fails
//!   to be base64: a character outside the alphabet, a length that is not a multiple of four,
//!   padding where no data precedes it and prose;
//! * a stated conversion on the integration RESPONSE is applied whatever the request's
//!   `Accept` names, while an unstated one follows that `Accept` against the api's own
//!   `binaryMediaTypes`.
//!
//! The parameter OVERRIDES a request template may state
//! (`#set($context.requestOverride.header.<name> = …)`, `.querystring.<name>`, `.path.<name>`)
//! name the same three locations `Integration$requestParameters` maps onto, and what the
//! template states REPLACES what those mappings produced — so a parameter the mappings
//! produced nothing for is carried by the override alone, which is asserted here against a
//! twin resource whose only difference is that its template states no override.
//!
//! The backend every case is integrated with is a Lambda function URL mounted on this suite's
//! own engine beside API Gateway, and its handler either REPORTS what it was handed or ANSWERS
//! that payload back under the media type the integration asked it for. So a row asserts the
//! transformation where the backend saw it, or where the client read it, and never from this
//! api's own records.

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

/// How long to wait between polls of a clock-owed transition, and how many polls to make: a
/// generous deadline, reached only by a transition that never comes.
const POLL: Duration = Duration::from_millis(25);
const POLLS: usize = 1_200;

/// The media type this suite's api declares BINARY, which is what decides the form a payload is
/// already in (`RestApi$binaryMediaTypes`).
const BINARY_MEDIA_TYPE: &str = "application/octet-stream";

/// A text media type the api declares nothing about, so a payload under it is text.
const TEXT_MEDIA_TYPE: &str = "text/plain";

/// The header and the query-string parameter a request template overrides, and the path
/// parameter the integration's `uri` carries a placeholder for.
const OVERRIDE_HEADER: &str = "x-ovr";
const OVERRIDE_PARAMETER: &str = "qovr";
const OVERRIDE_SEGMENT: &str = "seg";

/// The values the overriding templates state, which are the ones the backend has to be handed.
const OVERRIDDEN_HEADER_VALUE: &str = "hdr-from-the-template";
const OVERRIDDEN_PARAMETER_VALUE: &str = "qs-from-the-template";
const OVERRIDDEN_SEGMENT_VALUE: &str = "seg-from-the-template";

/// What the CLIENT sends for those two parameters where a case sends them at all, so an
/// override is seen to replace a value the mappings really did produce.
const CLIENT_HEADER_VALUE: &str = "hdr-from-the-client";
const CLIENT_PARAMETER_VALUE: &str = "qs-from-the-client";

/// The sentence API Gateway answers a call it could not perform the integration of.
const INTERNAL_SERVER_ERROR: &str = "Internal server error";

/// The handler behind the function URL.
///
/// Asked for a `respond` mode it ANSWERS the payload it was handed back, under the media type
/// that mode names — `astext` the text one, `asbytes` the one the api declares binary — which
/// is what the response-side conversion is then applied to. Asked for none it REPORTS what it
/// was handed: the body (with `isBase64Encoded` saying which form the event carries it in), the
/// query string, the path and the header an override states.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    headers = event.get("headers") or {}
    query = event.get("queryStringParameters") or {}
    mode = query.get("respond") or ""
    body = event.get("body") or ""
    encoded = bool(event.get("isBase64Encoded"))
    if mode == "astext" or mode == "asbytes":
        media = "text/plain"
        if mode == "asbytes":
            media = "application/octet-stream"
        return {
            "statusCode": 200,
            "headers": {"content-type": media},
            "body": body,
            "isBase64Encoded": encoded,
        }
    return {
        "statusCode": 200,
        "headers": {"content-type": "application/json"},
        "body": json.dumps(
            {
                "body": body,
                "encoded": encoded,
                "query": event.get("rawQueryString") or "",
                "path": event.get("rawPath") or "",
                "ovr": headers.get("x-ovr") or "",
            }
        ),
    }
"#;

/// One answered request: its status and its body verbatim.
struct Answer {
    status: u16,
    body: Bytes,
}

impl Answer {
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

/// API Gateway and Lambda over ONE shared engine, as `core/router/src/lib.rs` mounts them: the
/// integration request the stage makes is dispatched back into this same router, so the
/// function URL it reaches is the one Lambda published here.
fn emulator() -> MultiServiceRouter {
    let engine = std::sync::Arc::new(engine::InMemoryEngine::new());
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
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(
            std::sync::Arc::clone(&engine),
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

/// Create `name` from a package running [`HANDLER_SOURCE`], wait until Lambda reports it
/// `Active` — a transition its own service owes, so this looks for the condition rather than
/// sleeping a span on its behalf — and publish a public function URL for it, answered as the
/// endpoint an integration is pointed at.
async fn given_backend(router: &MultiServiceRouter, name: &str) -> String {
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
    let mut active = false;
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
            active = true;
            break;
        }
        tokio::time::sleep(POLL).await;
    }
    assert!(active, "setup: Lambda never reported {name} Active");
    let created = lambda(
        router,
        &format!("CreateFunctionUrlConfig({name})"),
        "POST",
        &format!("/2021-10-31/functions/{name}/url"),
        Some(json!({ "AuthType": "NONE" })),
        201,
    )
    .await;
    lambda(
        router,
        &format!("AddPermission({name})"),
        "POST",
        &format!("/2015-03-31/functions/{name}/policy"),
        Some(json!({
            "StatementId": "public-url",
            "Action": "lambda:InvokeFunctionUrl",
            "Principal": "*",
            "FunctionUrlAuthType": "NONE",
        })),
        201,
    )
    .await;
    member(
        &format!("CreateFunctionUrlConfig({name})"),
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

/// One resource of `api` carrying an `ANY` method declared by `method`, whose integration is
/// `integration`, with the integration response `integration_response` for the `200` the
/// backend answers.
async fn given_route(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
    path_part: &str,
    method: Value,
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
        Some(method),
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

/// A method that declares nothing of the caller's request.
fn open_method() -> Value {
    json!({ "authorizationType": "NONE" })
}

/// The integration body of a STANDARD `HTTP` integration against `uri`.
///
/// `content_handling` is the conversion it states about the integration request's payload, and
/// `request_parameters` what it maps onto that request. `integrationHttpMethod` travels under
/// its `jsonName`, `httpMethod`.
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

/// One request to the deployed stage, carrying `body` under `content_type` and declaring what
/// the client ACCEPTS — which is what names the form the method response's payload is wanted in
/// where the integration response states no conversion of its own.
async fn invoke(
    router: &MultiServiceRouter,
    api: &str,
    path: &str,
    headers: &[(&str, &str)],
    body: &str,
) -> Answer {
    dispatch(
        router,
        "POST",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{path}"),
        headers,
        Bytes::from(body.to_string()),
    )
    .await
}

/// One payload every conversion row below is driven over: the TEXT it is, and the base64 text
/// that stands for its bytes.
struct Payload {
    case: &'static str,
    raw: String,
    encoded: String,
}

/// The payloads the conversions are asserted over. They differ in LENGTH — which is what
/// decides the padding the standard base64 alphabet and padding (RFC 4648, the encoding the
/// `ContentHandlingStrategy` conversions move between) carries, so one payload of each residue
/// is here — and in the bytes they are made of, so neither conversion is asserted at one length
/// or over one alphabet alone.
fn payloads() -> Vec<Payload> {
    let long: String = (0..320)
        .map(|index| char::from(b'a' + u8::try_from(index % 26).expect("a letter")))
        .collect();
    vec![
        ("an empty payload", String::new()),
        ("one byte, whose encoding carries two padding characters", "a".to_string()),
        ("two bytes, whose encoding carries one", "ab".to_string()),
        ("three bytes, whose encoding carries none", "abc".to_string()),
        ("multi-byte UTF-8 text", "h\u{e9}llo w\u{f6}rld \u{2713}".to_string()),
        ("320 bytes, the scale of a small upload", long),
    ]
    .into_iter()
    .map(|(case, raw)| Payload {
        case,
        encoded: base64::engine::general_purpose::STANDARD.encode(raw.as_bytes()),
        raw,
    })
    .collect()
}

/// Text that is NOT base64 at all, each row one way the encoding fails: a character outside the
/// standard alphabet, a length that is not a multiple of four, padding with no data before it,
/// and prose.
const NOT_BASE64: &[(&str, &str)] = &[
    ("a character outside the alphabet", "AAEC*AA="),
    ("a length that is not a multiple of four", "AAECA"),
    ("padding with nothing before it", "=="),
    ("prose", "not base64 at all"),
];

#[tokio::test]
async fn content_handling_converts_the_integration_requests_payload_at_every_length() {
    let router = emulator();
    let base = given_backend(&router, "breadth-request-backend").await;
    let (api, root) = given_api(&router, "breadth-request-conversion").await;

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
            open_method(),
            http_integration(&format!("{base}/echo"), stated, json!({}), None),
            json!({}),
        )
        .await;
    }
    given_deployment(&router, &api).await;

    for payload in payloads() {
        // (what the row asserts, the route, the media type the client declares, the body it
        // sends, the bytes the backend has to be handed)
        let rows: [(String, &str, &str, &str, &str); 4] = [
            (
                format!("CONVERT_TO_BINARY decodes {}", payload.case),
                "tobinary",
                TEXT_MEDIA_TYPE,
                &payload.encoded,
                &payload.raw,
            ),
            (
                format!("CONVERT_TO_TEXT encodes {}", payload.case),
                "totext",
                BINARY_MEDIA_TYPE,
                &payload.raw,
                &payload.encoded,
            ),
            (
                format!("an unstated conversion forwards {} as text", payload.case),
                "unstated",
                TEXT_MEDIA_TYPE,
                &payload.encoded,
                &payload.encoded,
            ),
            (
                format!("an unstated conversion forwards {} as bytes", payload.case),
                "unstated",
                BINARY_MEDIA_TYPE,
                &payload.raw,
                &payload.raw,
            ),
        ];
        for (case, route, content_type, sent, handed) in rows {
            let answered = invoke(
                &router,
                &api,
                &format!("/{route}"),
                &[("content-type", content_type)],
                sent,
            )
            .await;
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: expected HTTP 200, got {} with {:?}",
                answered.status,
                answered.text()
            );
            assert_eq!(
                answered.backend_payload(),
                Some(handed.as_bytes().to_vec()),
                "case {case:?}: expected the backend to be handed {handed:?}, got {:?}",
                answered.text()
            );
        }
    }

    // Text the integration wants as bytes and that is not that encoding at all: the
    // integration cannot be performed over it, so the call is answered as the configuration
    // error it is rather than with a payload built from part of the text.
    for (case, sent) in NOT_BASE64 {
        let answered = invoke(
            &router,
            &api,
            "/tobinary",
            &[("content-type", TEXT_MEDIA_TYPE)],
            sent,
        )
        .await;
        assert_eq!(
            answered.status,
            500,
            "case {case:?}: expected HTTP 500 for {sent:?}, got {} with {:?}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.message().as_deref(),
            Some(INTERNAL_SERVER_ERROR),
            "case {case:?}: expected the {INTERNAL_SERVER_ERROR:?} gateway response, got {:?}",
            answered.text()
        );
    }
}

#[tokio::test]
async fn content_handling_converts_the_method_responses_payload_at_every_length() {
    let router = emulator();
    let base = given_backend(&router, "breadth-response-backend").await;
    let (api, root) = given_api(&router, "breadth-response-conversion").await;

    // One route per (the media type the backend answers under, the conversion the integration
    // response states) pair: the mode travels as the one query-string parameter the integration
    // maps onto its request, since a standard integration carries nothing of the caller's that
    // it does not map. The integration itself states no conversion, so what the backend answers
    // back is the very payload the client sent.
    for (route, mode, stated) in [
        ("astextbin", "astext", Some("CONVERT_TO_BINARY")),
        ("astextpass", "astext", None),
        ("asbytestext", "asbytes", Some("CONVERT_TO_TEXT")),
        ("asbytespass", "asbytes", None),
    ] {
        let mut response = json!({});
        if let Some(stated) = stated {
            response["contentHandling"] = json!(stated);
        }
        given_route(
            &router,
            &api,
            &root,
            route,
            open_method(),
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

    for payload in payloads() {
        // (what the row asserts, the route, what the client ACCEPTS, the body it sends, the
        // bytes it has to read back)
        let rows: [(String, &str, Option<&str>, &str, &str); 5] = [
            (
                format!(
                    "a stated CONVERT_TO_BINARY decodes {} whatever is accepted",
                    payload.case
                ),
                "astextbin",
                None,
                &payload.encoded,
                &payload.raw,
            ),
            (
                format!("a stated CONVERT_TO_TEXT encodes {}", payload.case),
                "asbytestext",
                Some(BINARY_MEDIA_TYPE),
                &payload.raw,
                &payload.encoded,
            ),
            (
                format!(
                    "nothing stated, with an Accept the api declares binary, decodes {}",
                    payload.case
                ),
                "astextpass",
                Some(BINARY_MEDIA_TYPE),
                &payload.encoded,
                &payload.raw,
            ),
            (
                format!("nothing stated and nothing accepted forwards {}", payload.case),
                "astextpass",
                None,
                &payload.encoded,
                &payload.encoded,
            ),
            (
                format!(
                    "nothing stated, with an Accept the api declares nothing about, encodes {}",
                    payload.case
                ),
                "asbytespass",
                Some(TEXT_MEDIA_TYPE),
                &payload.raw,
                &payload.encoded,
            ),
        ];
        for (case, route, accept, sent, served) in rows {
            let mut headers: Vec<(&str, &str)> = vec![("content-type", TEXT_MEDIA_TYPE)];
            if let Some(accept) = accept {
                headers.push(("accept", accept));
            }
            let answered = invoke(&router, &api, &format!("/{route}"), &headers, sent).await;
            assert_eq!(
                answered.status,
                200,
                "case {case:?}: expected HTTP 200, got {} with {:?}",
                answered.status,
                answered.text()
            );
            assert_eq!(
                answered.body.as_ref(),
                served.as_bytes(),
                "case {case:?}: expected the client to read {served:?}, got {:?}",
                answered.text()
            );
        }
    }

    // The same refusal on this half of the call: text the method response is to carry as bytes
    // and that is not base64 is the integration this plane cannot perform.
    for (case, sent) in NOT_BASE64 {
        let answered = invoke(
            &router,
            &api,
            "/astextbin",
            &[("content-type", TEXT_MEDIA_TYPE)],
            sent,
        )
        .await;
        assert_eq!(
            answered.status,
            500,
            "case {case:?}: expected HTTP 500 for {sent:?}, got {} with {:?}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.message().as_deref(),
            Some(INTERNAL_SERVER_ERROR),
            "case {case:?}: expected the {INTERNAL_SERVER_ERROR:?} gateway response, got {:?}",
            answered.text()
        );
    }
}

#[tokio::test]
async fn a_request_templates_override_replaces_and_creates_the_parameters_mapped() {
    let router = emulator();
    let base = given_backend(&router, "breadth-override-backend").await;
    let (api, root) = given_api(&router, "breadth-overrides").await;

    // Two resources whose method and integration are configured IDENTICALLY — the method
    // declares the header and the query-string parameter the caller may send, and the
    // integration maps both onto the integration request — and whose templates differ only in
    // that one of them states the two overrides.
    let method = json!({
        "authorizationType": "NONE",
        "requestParameters": {
            format!("method.request.header.{OVERRIDE_HEADER}"): false,
            format!("method.request.querystring.{OVERRIDE_PARAMETER}"): false,
        },
    });
    let request_parameters = json!({
        format!("integration.request.header.{OVERRIDE_HEADER}"):
            format!("method.request.header.{OVERRIDE_HEADER}"),
        format!("integration.request.querystring.{OVERRIDE_PARAMETER}"):
            format!("method.request.querystring.{OVERRIDE_PARAMETER}"),
    });
    for (route, template) in [
        ("mapped", r#"{"templated":true}"#.to_string()),
        (
            "override",
            format!(
                "#set($context.requestOverride.header.{OVERRIDE_HEADER} = \
                 \"{OVERRIDDEN_HEADER_VALUE}\")\n\
                 #set($context.requestOverride.querystring.{OVERRIDE_PARAMETER} = \
                 \"{OVERRIDDEN_PARAMETER_VALUE}\")\n\
                 {{\"templated\":true}}"
            ),
        ),
    ] {
        given_route(
            &router,
            &api,
            &root,
            route,
            method.clone(),
            http_integration(
                &format!("{base}/echo"),
                None,
                request_parameters.clone(),
                Some(json!({ "application/json": template })),
            ),
            json!({}),
        )
        .await;
    }
    // The third location an override names: the `uri`'s own `{name}` placeholder, which the
    // integration's `requestParameters` map nothing onto — so the address is the override's to
    // decide.
    given_route(
        &router,
        &api,
        &root,
        "addressed",
        open_method(),
        http_integration(
            &format!("{base}/echo/{{{OVERRIDE_SEGMENT}}}"),
            None,
            json!({}),
            Some(json!({
                "application/json": format!(
                    "#set($context.requestOverride.path.{OVERRIDE_SEGMENT} = \
                     \"{OVERRIDDEN_SEGMENT_VALUE}\")\n{{\"templated\":true}}"
                ),
            })),
        ),
        json!({}),
    )
    .await;
    given_deployment(&router, &api).await;

    // (what the row asserts, the route, whether the client sends the two parameters, the header
    // the backend has to be handed, the query string it has to be called with)
    let rows: [(&str, &str, bool, &str, String); 4] = [
        (
            "the mapped twin forwards what the client sent",
            "mapped",
            true,
            CLIENT_HEADER_VALUE,
            format!("{OVERRIDE_PARAMETER}={CLIENT_PARAMETER_VALUE}"),
        ),
        (
            "the mapped twin carries nothing where the client sent nothing",
            "mapped",
            false,
            "",
            String::new(),
        ),
        (
            "an override REPLACES what the mappings produced from the client's request",
            "override",
            true,
            OVERRIDDEN_HEADER_VALUE,
            format!("{OVERRIDE_PARAMETER}={OVERRIDDEN_PARAMETER_VALUE}"),
        ),
        (
            "an override CREATES the parameters the mappings produced nothing for",
            "override",
            false,
            OVERRIDDEN_HEADER_VALUE,
            format!("{OVERRIDE_PARAMETER}={OVERRIDDEN_PARAMETER_VALUE}"),
        ),
    ];
    for (case, route, sends, header, query) in rows {
        let mut headers: Vec<(&str, &str)> = vec![("content-type", "application/json")];
        let mut path = format!("/{route}");
        if sends {
            headers.push((OVERRIDE_HEADER, CLIENT_HEADER_VALUE));
            path = format!("/{route}?{OVERRIDE_PARAMETER}={CLIENT_PARAMETER_VALUE}");
        }
        let answered = invoke(&router, &api, &path, &headers, r#"{"sent":true}"#).await;
        assert_eq!(
            answered.status,
            200,
            "case {case:?}: expected HTTP 200, got {} with {:?}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.reported("ovr"),
            header,
            "case {case:?}: expected the backend to be handed the header {header:?}, got {:?}",
            answered.text()
        );
        assert_eq!(
            answered.reported("query"),
            query,
            "case {case:?}: expected the backend to be called with {query:?}, got {:?}",
            answered.text()
        );
        // The template still renders the body, whatever it states about the parameters.
        assert_eq!(
            answered.backend_payload(),
            Some(br#"{"templated":true}"#.to_vec()),
            "case {case:?}: expected the rendered template as the body, got {:?}",
            answered.text()
        );
    }

    let case = "an override of the uri's own path placeholder addresses the backend";
    let answered = invoke(
        &router,
        &api,
        "/addressed",
        &[("content-type", "application/json")],
        r#"{"sent":true}"#,
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "case {case:?}: expected HTTP 200, got {} with {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.reported("path"),
        format!("/echo/{OVERRIDDEN_SEGMENT_VALUE}"),
        "case {case:?}: expected the overriding path segment to address the backend, got {:?}",
        answered.text()
    );
}
