// SPDX-License-Identifier: CC0-1.0

//! What a deployed `HTTP_PROXY` method answers for the response the ENDPOINT it names gave
//! the request it passed on — whatever that response is — and the one case API Gateway
//! answers its own `500` for instead.
//!
//! RULE: `IntegrationType` gives `HTTP_PROXY` as "for integrating the API method request with
//! an HTTP endpoint, including a third party API, with the client request passed through
//! as-is", and such an integration maps nothing in either direction, so the endpoint's own
//! response IS the method response: its status, its headers and its body, reported to the
//! client as they arrived, for every status an endpoint can answer. API Gateway's own
//! `{"message": "Internal server error"}` is the answer to an integration that could not be
//! PERFORMED — an endpoint nothing serves — and not to an endpoint that answered a non-2xx.
//! So a refusal taken AT the proxied endpoint reaches the client as that refusal, and a
//! client can tell it from a gateway fault.
//!
//! The rows differ along what the rule generalises over: the STATUS the endpoint answers,
//! spanning each class a status can belong to (`2xx`, `3xx`, `4xx` — including the `403` an
//! endpoint refuses a caller with — and `5xx`, where the endpoint's own `500` and the
//! gateway's carry different bodies), and along WHO decided the status: the function behind
//! the URL, the function URL's own admission of an unsigned caller, and an address nothing in
//! this emulator claims.
//!
//! The backend is a Lambda function URL — "a dedicated HTTP(S) endpoint that you can use to
//! invoke your function" — mounted on this suite's own engine beside API Gateway, so the
//! endpoint the integration reaches is the one Lambda published here and the status a row
//! asserts is one another service produced.

use std::io::Write as _;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "us-east-1";

/// The stage each api is deployed to.
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

/// The header the handler answers with, which is what shows the endpoint's own headers reach
/// the client beside its status.
const BACKEND_HEADER: &str = "x-backend";
const BACKEND_VALUE: &str = "seen";

/// The sentence API Gateway answers an integration it could not perform with, through this
/// api's `API_CONFIGURATION_ERROR` gateway response.
const GATEWAY_SENTENCE: &str = "Internal server error";

/// An address no service of this emulator claims: nothing answers for it, so there is no
/// endpoint response to pass through at all.
const UNSERVED_ENDPOINT: &str = "https://nothing.example.test/echo";

/// The body a function URL whose `AuthType` is `AWS_IAM` answers a caller the emulator did
/// not authenticate, and the class that refusal names itself by: the endpoint's own refusal,
/// taken before the function could run.
const FORBIDDEN_BODY: &str = r#"{"Message":"Forbidden"}"#;
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";
const FORBIDDEN_ERROR_TYPE: &str = "AccessDeniedException";

/// The handler behind the function URL: it answers the status the query string names and a
/// body naming that status, so a row reads the endpoint's own status off the response and the
/// endpoint's own body out of it.
const HANDLER_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    query = event.get("queryStringParameters") or {}
    status = int(query.get("status", "200"))
    return {
        "statusCode": status,
        "headers": {"content-type": "application/json", "x-backend": "seen"},
        "body": json.dumps({"answered": status}),
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

    /// The `message` member of a gateway response's body, which is where API Gateway's own
    /// sentence travels.
    fn message(&self) -> Option<String> {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
    }
}

/// API Gateway and Lambda over ONE shared engine, as `core/router/src/lib.rs` mounts them:
/// the integration request the stage passes on is dispatched back into this same router, so
/// the function URL it reaches is the one Lambda published here.
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
/// published for it with its trailing `/` trimmed, so a path appends to it.
///
/// A URL whose `AuthType` is `NONE` leaves IAM out of it, and the function's own
/// resource-based policy is then what admits the anonymous caller an unsigned integration
/// request is — so the grant is added for exactly those URLs.
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

/// An api of `name`, as its id and its root resource id.
async fn given_api(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let created = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": name })),
        201,
    )
    .await;
    (
        member("CreateRestApi", &created, "id"),
        member("CreateRestApi", &created, "rootResourceId"),
    )
}

/// One resource of `api` under `root`, carrying a `GET` method whose `HTTP_PROXY` integration
/// passes the request on to `uri`.
async fn given_proxy_resource(
    router: &MultiServiceRouter,
    api: &str,
    root: &str,
    path_part: &str,
    uri: &str,
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
        &format!("/restapis/{api}/resources/{resource}/methods/GET"),
        Some(json!({ "authorizationType": "NONE" })),
        201,
    )
    .await;
    apigw(
        router,
        &format!("PutIntegration({path_part})"),
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
        Some(json!({
            "type": "HTTP_PROXY",
            // `integrationHttpMethod` travels under its `jsonName`, which is what
            // `smithy_query(query="operation",operation="PutIntegration")` reports.
            "httpMethod": "GET",
            "uri": uri,
        })),
        201,
    )
    .await;
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

/// One request to the deployed stage, as a client of the api sends it: unsigned, which is
/// what a method whose `authorizationType` is `NONE` is reached with.
async fn invoke(router: &MultiServiceRouter, api: &str, path: &str) -> Answer {
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{path}"),
        &[],
        Bytes::new(),
    )
    .await
}

/// RULE: every status the proxied endpoint answers is the status the client reads, with the
/// endpoint's own body and headers — which is what makes a non-2xx from the backend tellable
/// from API Gateway's own failure.
#[tokio::test]
async fn an_http_proxy_method_answers_with_every_status_its_endpoint_gave_it() {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(&engine);
    given_function(&router, "proxy-status-backend").await;
    let endpoint = given_function_url(&router, "proxy-status-backend", "NONE").await;
    let (api, root) = given_api(&router, "http-proxy-status-passthrough").await;
    given_proxy_resource(&router, &api, &root, "echo", &format!("{endpoint}/echo")).await;
    given_proxy_resource(&router, &api, &root, "nowhere", UNSERVED_ENDPOINT).await;
    given_deployment(&router, &api).await;

    // The rows span the status CLASSES an endpoint answers in, the two refusal statuses a
    // client would otherwise read as a gateway fault among them, and the endpoint's own
    // server errors — including the very `500` API Gateway answers an unperformable
    // integration with, which is told from it by the body.
    let statuses: [u16; 14] = [
        200, 201, 202, 301, 400, 401, 403, 404, 409, 418, 429, 500, 502, 503,
    ];
    for status in statuses {
        let case = format!("the endpoint's own HTTP {status}, passed through");
        let answered = invoke(&router, &api, &format!("/echo?status={status}")).await;
        assert_eq!(
            answered.status, status,
            "case {case:?}: expected HTTP {status}, got {} with {:?}",
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.json(),
            json!({ "answered": status }),
            "case {case:?}: expected the endpoint's own body, got {:?}",
            answered.text()
        );
        assert_eq!(
            answered.header(BACKEND_HEADER).as_deref(),
            Some(BACKEND_VALUE),
            "case {case:?}: expected the endpoint's own header, got {:?}",
            answered.headers
        );
        assert_ne!(
            answered.message().as_deref(),
            Some(GATEWAY_SENTENCE),
            "case {case:?}: expected the endpoint's response rather than the api's own \
             {GATEWAY_SENTENCE:?}, got {:?}",
            answered.text()
        );
    }

    // The one case API Gateway answers its OWN failure for: the integration could not be
    // performed at all, so there is no endpoint response to pass through.
    let case = "a uri naming an endpoint nothing in this emulator serves";
    let answered = invoke(&router, &api, "/nowhere?status=200").await;
    assert_eq!(
        answered.status, 500,
        "case {case:?}: expected HTTP 500, got {} with {:?}",
        answered.status,
        answered.text()
    );
    assert_eq!(
        answered.message().as_deref(),
        Some(GATEWAY_SENTENCE),
        "case {case:?}: expected the {GATEWAY_SENTENCE:?} gateway response, got {:?}",
        answered.text()
    );
    assert!(
        answered.header(BACKEND_HEADER).is_none(),
        "case {case:?}: expected no endpoint header, got {:?}",
        answered.headers
    );
}

/// One row of the discrimination below: the resource whose integration names the endpoint,
/// the status the client must read, the body it must carry, and whether that body is the
/// endpoint's own answer or the api's gateway response.
struct Decided {
    case: &'static str,
    path: &'static str,
    status: u16,
    body: &'static str,
    /// Whether the FUNCTION behind the URL ran for this call, which is what puts its own
    /// header on the answer: neither a refusal the endpoint took before running it nor an
    /// integration that reached no endpoint carries one.
    function_ran: bool,
}

/// RULE: a refusal taken AT the proxied endpoint is passed through as that refusal, and only
/// an integration that could not be performed is answered with API Gateway's own `500` — so
/// the two are tellable apart by the status and the body a client holds.
///
/// Driven on an emulator that VERIFIES request signatures, which is how it runs when it is
/// started: a function URL whose `AuthType` is `AWS_IAM` is refused to an unsigned caller
/// before the function runs, and API Gateway signs nothing it passes on.
#[tokio::test]
async fn an_endpoints_own_refusal_is_told_from_an_integration_that_could_not_be_performed() {
    let engine: std::sync::Arc<engine::InMemoryEngine> =
        std::sync::Arc::new(engine::InMemoryEngine::new());
    let router = emulator_on(&engine);
    given_function(&router, "proxy-iam-backend").await;
    given_function(&router, "proxy-open-backend").await;
    let restricted = given_function_url(&router, "proxy-iam-backend", "AWS_IAM").await;
    let public = given_function_url(&router, "proxy-open-backend", "NONE").await;
    let (api, root) = given_api(&router, "http-proxy-refusal-passthrough").await;
    given_proxy_resource(
        &router,
        &api,
        &root,
        "restricted",
        &format!("{restricted}/echo"),
    )
    .await;
    given_proxy_resource(&router, &api, &root, "open", &format!("{public}/echo")).await;
    given_proxy_resource(&router, &api, &root, "nowhere", UNSERVED_ENDPOINT).await;
    given_deployment(&router, &api).await;
    // From here on the SAME resources are driven through an emulator that reads credentials:
    // only such an emulator refuses the unsigned integration request an `AWS_IAM` function
    // URL does not admit. The router above is kept only to have built the records.
    let mut authenticating = emulator_on(&engine);
    authenticating.set_request_authentication(true);

    let rows: [Decided; 3] = [
        Decided {
            case: "an AWS_IAM function URL, which does not admit the unsigned request the \
                   integration passes on",
            path: "/restricted?status=200",
            status: 403,
            body: FORBIDDEN_BODY,
            function_ran: false,
        },
        Decided {
            case: "a function URL that admits the caller, answering through the function",
            path: "/open?status=200",
            status: 200,
            body: r#"{"answered": 200}"#,
            function_ran: true,
        },
        Decided {
            case: "an endpoint nothing in this emulator serves, so no integration was \
                   performed",
            path: "/nowhere?status=200",
            status: 500,
            body: r#"{"message":"Internal server error"}"#,
            function_ran: false,
        },
    ];
    for row in &rows {
        let answered = invoke(&authenticating, &api, row.path).await;
        assert_eq!(
            answered.status, row.status,
            "case {:?}: expected HTTP {}, got {} with {:?}",
            row.case,
            row.status,
            answered.status,
            answered.text()
        );
        let expected: Value =
            serde_json::from_str(row.body).expect("each row's body is written as JSON");
        assert_eq!(
            answered.json(),
            expected,
            "case {:?}: expected the body {:?}, got {:?}",
            row.case,
            row.body,
            answered.text()
        );
        assert_eq!(
            answered.header(BACKEND_HEADER).is_some(),
            row.function_ran,
            "case {:?}: the handler's own header is reported exactly for the row whose \
             function ran, got {:?}",
            row.case,
            answered.headers
        );
    }

    // The refusal the endpoint took names its own CLASS in the header a Lambda endpoint
    // reports a failure's class in: a passed-through response carries the endpoint's headers
    // as well as its status and body.
    let case = "the class an AWS_IAM function URL's refusal names itself by";
    let refused = invoke(&authenticating, &api, "/restricted?status=200").await;
    assert_eq!(
        refused.header(ERROR_TYPE_HEADER).as_deref(),
        Some(FORBIDDEN_ERROR_TYPE),
        "case {case:?}: expected the endpoint's own {ERROR_TYPE_HEADER} header, got {:?}",
        refused.headers
    );
}
