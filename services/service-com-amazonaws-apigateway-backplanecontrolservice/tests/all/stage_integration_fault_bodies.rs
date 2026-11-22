// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a deployed stage answers when the INTEGRATION STAGE fails, and
//! for what an authorizer response API Gateway cannot use earns — across API Gateway and AWS
//! Lambda on one engine.
//!
//! Two rules are under test.
//!
//! 1. The default body of the answers API Gateway serializes ITSELF — the refusals of the
//!    integration stage, where the BACKEND rather than the api's own configuration failed the
//!    call — carries a SPACE after the member separator: an integration that did not answer
//!    within its own `timeoutInMillis` is answered `504` with
//!    `{"message": "Endpoint request timed out"}`, and one whose invocation Lambda answered
//!    with a failure of its own (a throttle) is answered `500` with
//!    `{"message": "Internal server error"}`, exactly as the `502` an answer the gateway
//!    cannot use is. Every OTHER refusal is rendered from the gateway response type's own
//!    default template (`{"message":$context.error.messageString}`), which is COMPACT and
//!    renders the JSON literal `null` for a class the gateway sets no message for — so the
//!    api's own `API_CONFIGURATION_ERROR`, the endpoint's `MISSING_AUTHENTICATION_TOKEN` and
//!    the authorizer refusals keep theirs. The rows below span both serializations over one
//!    deployed api, which is what tells one rendering from the other byte for byte.
//!
//! 2. An authorizer response may publish only SCALARS in its `context` — "you cannot set a
//!    JSON object or array as a valid value of any key in the context map" — so a response
//!    carrying a nested map or an array there is no usable authorizer response at all: the
//!    call is refused `500` through the authorizer-failure gateway response, whose default
//!    body is `{"message":null}`, and the method's integration is never reached. A response
//!    whose `context` carries only scalars decides the call and publishes every entry.
//!
//! Only a cross-service suite can pin either: the function, its reserved concurrency and its
//! resource policy are AWS Lambda's, while the api, the authorizer, the integration and the
//! stage are API Gateway's.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to.
const STAGE: &str = "live";

/// The function every method of the api is integrated with, and the one whose reserved
/// concurrency admits nothing — so an invocation of it is answered Lambda's own throttle.
const BACKEND: &str = "fault-backend";
const THROTTLED_BACKEND: &str = "fault-throttled";

/// The role a created function names. No IAM store is mounted here, so it is a value the
/// create carries rather than a record this suite relies on.
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-exec";

/// The handler the deployment package declares.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long this suite waits for Lambda's own clock-owed transition (a created function
/// becoming `Active`), and how often it looks.
const WAIT: Duration = Duration::from_secs(20);
const LOOK: Duration = Duration::from_millis(100);

/// The header the `REQUEST` authorizer declares as its identity source, which is also what
/// chooses the `context` map the authorizer returns.
const CONTEXT_HEADER: &str = "x-context-kind";

/// The `timeoutInMillis` the slow method's integration carries: the lowest value
/// `Integration$timeoutInMillis` admits ("custom timeout between 50 and 29,000
/// milliseconds"), so the row that observes the integration timeout does not sit on the
/// 29 s default.
const SHORT_TIMEOUT_MILLIS: u32 = 50;

/// The body the backend returns for a call it serves, which is what an invoked integration
/// hands the caller — and therefore what an authorizer refusal must NOT be answered with.
const SERVED_BODY: &str = r#"{"ok": true}"#;

/// The header API Gateway names the exception class of a refused request in, which is what a
/// client reads a restJson1 error code out of.
const ERROR_TYPE_HEADER: &str = "x-amzn-ErrorType";

/// The class the endpoint reports for every SERVER-side fault of a call it could not pass
/// on, whichever side produced it.
const SERVER_FAULT_CLASS: &str = "InternalServerErrorException";

/// The ONE handler both functions are created from: it answers an authorizer event with the
/// policy and the `context` map the request's own header names, and every other event with
/// the proxy envelope above.
const HANDLER_SOURCE: &str = r#"
import json
import time


def _authorizer(event):
    headers = event.get("headers") or {}
    kind = headers.get("X-Context-Kind") or headers.get("x-context-kind") or "scalars"
    if kind == "nested":
        context = {"tier": "gold", "nested": {"a": "b"}}
    elif kind == "array":
        context = {"tiers": ["gold", "silver"]}
    else:
        context = {"tier": "gold", "calls": 3, "ok": True}
    return {
        "principalId": "principal-" + kind,
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [
                {
                    "Action": "execute-api:Invoke",
                    "Effect": "Allow",
                    "Resource": event["methodArn"],
                }
            ],
        },
        "context": context,
    }


def lambda_handler(event, context):
    if event.get("type") in ("REQUEST", "TOKEN"):
        return _authorizer(event)
    if (event.get("path") or "").endswith("/slow"):
        time.sleep(2)
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"ok": True}),
    }
"#;

/// One answered request: its status, its headers and its body verbatim.
struct Answered {
    status: u16,
    headers: Vec<(String, String)>,
    body: Bytes,
}

impl Answered {
    /// The body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The body as JSON, `Null` when it is not JSON at all.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The value the response carries under the header `name`, or `None` when it carries
    /// none.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// The two services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts
/// them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The suite exercises the services, not SigV4: the control-plane calls carry a credential
    // scope so the router can name the target service, and the invokes carry no credential at
    // all, as a client of a published invoke URL sends none.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(engine),
    );
    router
}

/// A SigV4 `Authorization` whose credential scope names `signing_name`, which is what routes
/// the request. The signature itself is never checked here.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{signing_name}/\
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
) -> Answered {
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
    Answered {
        status: parts.status.as_u16(),
        headers: parts
            .headers
            .iter()
            .map(|(name, value)| {
                (
                    name.to_string(),
                    String::from_utf8_lossy(value.as_bytes()).to_string(),
                )
            })
            .collect(),
        body,
    }
}

/// One API Gateway control-plane call that has to be accepted with `expected`.
async fn apigw(
    router: &MultiServiceRouter,
    case: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
    expected: u16,
) -> Value {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
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
        answered.status, expected,
        "setup {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
        answered.status,
        answered.text()
    );
    answered.json()
}

/// One AWS Lambda call over its `restJson1` wire, answering its status and parsed body.
async fn lambda(
    router: &MultiServiceRouter,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body.map(|value| value.to_string()).unwrap_or_default();
    let answered = dispatch(
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
    (answered.status, answered.json())
}

/// One request to the api's own invoke endpoint — the host
/// `{rest-api-id}.execute-api.{region}.amazonaws.com`, which is the endpoint
/// `RestApi$disableExecuteApiEndpoint` documents — carrying `headers` and no credential, as a
/// client of a public method sends none.
async fn invoke(
    router: &MultiServiceRouter,
    api: &str,
    path: &str,
    headers: &[(&str, &str)],
) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{path}"),
        headers,
        Bytes::new(),
    )
    .await
}

/// A .zip deployment package holding `source` as the module the handler names, written in
/// memory the way a caller uploads one.
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

/// The member `name` of a JSON object, as text.
fn member(case: &str, body: &Value, name: &str) -> String {
    body.get(name)
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("setup {case:?}: no {name:?} in {body}"))
        .to_string()
}

/// Poll `GetFunctionConfiguration` until Lambda first reports `function` `Active`.
///
/// The span a created function spends `Pending` is that service's own, so nothing here sleeps
/// a fixed span on its behalf.
async fn wait_until_active(router: &MultiServiceRouter, function: &str) {
    let looks = WAIT.as_millis() / LOOK.as_millis().max(1);
    let mut last = Value::Null;
    for _ in 0..looks {
        let (_, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{function}/configuration"),
            None,
        )
        .await;
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup CreateFunction({function}): it never became Active, last read {last}");
}

/// Create `function` from the one handler above and answer its ARN.
async fn created(router: &MultiServiceRouter, function: &str) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": function,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(HANDLER_SOURCE)),
            },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction({function}): expected HTTP 201, got {status} with {created}"
    );
    member("CreateFunction", &created, "FunctionArn")
}

/// Grant `apigateway.amazonaws.com` `lambda:InvokeFunction` on `function`, narrowed to
/// `source_arn` — the `aws:SourceArn` such a grant is scoped by.
async fn granted(router: &MultiServiceRouter, function: &str, statement: &str, source_arn: &str) {
    let (status, answered) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{function}/policy"),
        Some(json!({
            "StatementId": statement,
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": source_arn,
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup AddPermission({statement}): expected HTTP 201, got {status} with {answered}"
    );
}

/// One method of the deployed api: the path it sits on, which function its `AWS_PROXY`
/// integration invokes, whether that invocation is granted, whether the integration carries
/// the short timeout, and whether the method is decided by the `REQUEST` authorizer.
struct Method {
    part: &'static str,
    function: &'static str,
    granted: bool,
    short_timeout: bool,
    authorized: bool,
}

/// The methods the api carries, each on its own path so one deployment serves every row.
const METHODS: &[Method] = &[
    // Served by the backend, which answers the proxy envelope: the control row.
    Method {
        part: "ok",
        function: BACKEND,
        granted: true,
        short_timeout: false,
        authorized: false,
    },
    // The same function, behind the shortest wait the member admits, on the path whose
    // handler sleeps past it: the integration does not answer in time.
    Method {
        part: "slow",
        function: BACKEND,
        granted: true,
        short_timeout: true,
        authorized: false,
    },
    // The function whose reserved concurrency admits nothing: Lambda answers the invocation
    // with its own throttle, so the function never runs.
    Method {
        part: "throttled",
        function: THROTTLED_BACKEND,
        granted: true,
        short_timeout: false,
        authorized: false,
    },
    // A method whose function grants this api no invocation of it at all: the api's own
    // configuration cannot serve the call.
    Method {
        part: "ungranted",
        function: BACKEND,
        granted: false,
        short_timeout: false,
        authorized: false,
    },
    // The method the `REQUEST` authorizer decides, in front of the same backend.
    Method {
        part: "guarded",
        function: BACKEND,
        granted: true,
        short_timeout: false,
        authorized: true,
    },
];

/// Stand both functions and the api up, and deploy the api to [`STAGE`]; answers the api id.
async fn deployed(router: &MultiServiceRouter) -> String {
    let backend_arn = created(router, BACKEND).await;
    let throttled_arn = created(router, THROTTLED_BACKEND).await;
    // "The number of simultaneous executions to reserve for the function"
    // (`PutFunctionConcurrency`): a reservation of zero admits no invocation at all, so every
    // call Lambda is asked to make of this function is answered its own throttle.
    let (status, reserved) = lambda(
        router,
        "PUT",
        &format!("/2017-10-31/functions/{THROTTLED_BACKEND}/concurrency"),
        Some(json!({ "ReservedConcurrentExecutions": 0 })),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup PutFunctionConcurrency: expected HTTP 200, got {status} with {reserved}"
    );

    let api = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "integration-fault-bodies" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api, "id");
    let resources = apigw(
        router,
        "GetResources",
        "GET",
        &format!("/restapis/{api}/resources"),
        None,
        200,
    )
    .await;
    let root = resources
        .get("item")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .map(|item| member("GetResources", item, "id"))
        .expect("the api's root resource");
    let account = engine::emulator_account_id();

    // The ONE authorizer the guarded method is decided by, with caching disabled — "if it
    // equals 0, authorization caching is disabled" — so each row below invokes the function
    // and is decided by what it returned for that row's own header.
    let authorizer = apigw(
        router,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": "context-authorizer",
            "type": "REQUEST",
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {backend_arn}/invocations"
            ),
            "identitySource": format!("method.request.header.{CONTEXT_HEADER}"),
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer = member("CreateAuthorizer", &authorizer, "id");
    granted(
        router,
        BACKEND,
        "apigateway-invoke-authorizer",
        &format!("arn:aws:execute-api:{REGION}:{account}:{api}/authorizers/{authorizer}"),
    )
    .await;

    for method in METHODS {
        let resource = apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": method.part })),
            201,
        )
        .await;
        let resource = member("CreateResource", &resource, "id");
        let declaration = if method.authorized {
            json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer })
        } else {
            json!({ "authorizationType": "NONE" })
        };
        apigw(
            router,
            "PutMethod",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET"),
            Some(declaration),
            201,
        )
        .await;
        let function_arn = if method.function == BACKEND {
            &backend_arn
        } else {
            &throttled_arn
        };
        let mut integration = json!({
            "type": "AWS_PROXY",
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {function_arn}/invocations"
            ),
        });
        if method.short_timeout {
            integration["timeoutInMillis"] = json!(SHORT_TIMEOUT_MILLIS);
        }
        apigw(
            router,
            "PutIntegration",
            "PUT",
            &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
            Some(integration),
            201,
        )
        .await;
        if method.granted {
            granted(
                router,
                method.function,
                &format!("apigateway-invoke-{}", method.part),
                &format!(
                    "arn:aws:execute-api:{REGION}:{account}:{api}/*/*/{}",
                    method.part
                ),
            )
            .await;
        }
    }

    let deployment = apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "description": "the snapshot the stage serves" })),
        201,
    )
    .await;
    let deployment = member("CreateDeployment", &deployment, "id");
    apigw(
        router,
        "CreateStage",
        "POST",
        &format!("/restapis/{api}/stages"),
        Some(json!({ "stageName": STAGE, "deploymentId": deployment })),
        201,
    )
    .await;

    wait_until_active(router, BACKEND).await;
    wait_until_active(router, THROTTLED_BACKEND).await;
    api
}

/// RULE: the default body of an answer API Gateway serializes ITSELF — the refusals of the
/// integration stage — carries a space after the member separator, while every refusal
/// rendered from a gateway response type's own default template is compact.
#[tokio::test]
async fn the_integration_stages_own_answers_carry_the_services_serialization() {
    let router = emulator();
    let api = deployed(&router).await;

    /// One row: the path it addresses, and the status, the body VERBATIM — separator
    /// included — and the exception class the stage must answer it with.
    struct Row {
        case: &'static str,
        path: &'static str,
        headers: &'static [(&'static str, &'static str)],
        status: u16,
        body: &'static str,
        /// The value of [`ERROR_TYPE_HEADER`] the answer must carry, or `None` for a row
        /// whose class the endpoint states none of.
        error_type: Option<&'static str>,
    }
    // Rows differ along WHICH answer the call earns: the two the integration stage produces
    // itself, and the refusals rendered from a gateway response's own default template.
    let rows: &[Row] = &[
        Row {
            case: "an integration that answered: the function's own envelope",
            path: "/ok",
            headers: &[],
            status: 200,
            body: SERVED_BODY,
            error_type: None,
        },
        Row {
            case: "an integration that did not answer within its own timeoutInMillis",
            path: "/slow",
            headers: &[],
            status: 504,
            body: r#"{"message": "Endpoint request timed out"}"#,
            error_type: Some(SERVER_FAULT_CLASS),
        },
        Row {
            case: "an invocation Lambda answered with its own throttle",
            path: "/throttled",
            headers: &[],
            status: 500,
            body: r#"{"message": "Internal server error"}"#,
            error_type: Some(SERVER_FAULT_CLASS),
        },
        Row {
            case: "a function whose policy admits this method nothing",
            path: "/ungranted",
            headers: &[],
            status: 500,
            body: r#"{"message":"Internal server error"}"#,
            error_type: Some(SERVER_FAULT_CLASS),
        },
        Row {
            case: "a path the deployment holds no method for",
            path: "/nowhere",
            headers: &[],
            status: 403,
            body: r#"{"message":"Missing Authentication Token"}"#,
            error_type: Some("MissingAuthenticationTokenException"),
        },
        Row {
            case: "an authorizer response the gateway cannot use",
            path: "/guarded",
            headers: &[(CONTEXT_HEADER, "nested")],
            status: 500,
            body: r#"{"message":null}"#,
            error_type: Some("AuthorizerConfigurationException"),
        },
    ];

    for row in rows {
        let answered = invoke(&router, &api, row.path, row.headers).await;
        assert_eq!(
            answered.status,
            row.status,
            "case {:?}: expected HTTP {}, got {} with {:?}",
            row.case,
            row.status,
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.text(),
            row.body,
            "case {:?}: expected the body {:?} byte for byte, got {:?}",
            row.case,
            row.body,
            answered.text()
        );
        assert_eq!(
            answered.header(ERROR_TYPE_HEADER),
            row.error_type,
            "case {:?}: expected {ERROR_TYPE_HEADER} {:?}, got {:?}",
            row.case,
            row.error_type,
            answered.header(ERROR_TYPE_HEADER)
        );
    }
}

/// RULE: an authorizer response may publish only scalars in its `context`, so one carrying a
/// nested map or an array there is no usable authorizer response: the call is refused `500`
/// with the body API Gateway answers a call no authorizer decided with, and the method's
/// integration is never reached.
#[tokio::test]
async fn an_authorizer_context_entry_that_is_not_a_scalar_refuses_the_call() {
    let router = emulator();
    let api = deployed(&router).await;

    /// One row: the `context` map the authorizer returns for it, and whether that response is
    /// one the call can be decided from.
    struct Row {
        case: &'static str,
        kind: &'static str,
        usable: bool,
    }
    // Rows differ along the SHAPE of the values the returned `context` carries.
    let rows: &[Row] = &[
        Row {
            case: "a context of scalars: a string, a number and a boolean",
            kind: "scalars",
            usable: true,
        },
        Row {
            case: "a context entry carrying a nested map",
            kind: "nested",
            usable: false,
        },
        Row {
            case: "a context entry carrying an array",
            kind: "array",
            usable: false,
        },
    ];

    for row in rows {
        let answered = invoke(&router, &api, "/guarded", &[(CONTEXT_HEADER, row.kind)]).await;
        if row.usable {
            assert_eq!(
                answered.status,
                200,
                "case {:?}: expected the integration to serve the call with HTTP 200, got {} \
                 with {:?}",
                row.case,
                answered.status,
                answered.text()
            );
            assert_eq!(
                answered.text(),
                SERVED_BODY,
                "case {:?}: expected the integration's own body {SERVED_BODY:?}, got {:?}",
                row.case,
                answered.text()
            );
            continue;
        }
        assert_eq!(
            answered.status,
            500,
            "case {:?}: expected HTTP 500, got {} with {:?}",
            row.case,
            answered.status,
            answered.text()
        );
        assert_eq!(
            answered.text(),
            r#"{"message":null}"#,
            "case {:?}: expected the authorizer-failure body byte for byte, got {:?}",
            row.case,
            answered.text()
        );
        // The integration was never reached: what an invoked integration answers is the
        // function's own envelope, and none of it is in this response.
        assert_eq!(
            answered.json().get("ok"),
            None,
            "case {:?}: expected the integration not to have served the call, got {:?}",
            row.case,
            answered.text()
        );
    }
}
