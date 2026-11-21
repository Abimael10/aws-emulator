// SPDX-License-Identifier: CC0-1.0

//! What a `CUSTOM`-authorized method of a deployed stage answers, what its backend then runs,
//! and what the stage's own access log records about it — the three halves of one request,
//! asserted together.
//!
//! THE RULE UNDER TEST, one level above the recorded case:
//!
//! > A method whose `authorizationType` is `CUSTOM` is decided by the POLICY the Lambda
//! > function of its `Authorizer` returned for the value the request carried at the
//! > authorizer's `identitySource`, evaluated against the ARN of the method the request
//! > addressed. A policy that allows `execute-api:Invoke` on that method hands the request to
//! > the method's integration and the client is answered with what the integration produced; a
//! > policy that explicitly DENIES it, and one that allows it nowhere, are refused `403` with
//! > API Gateway's own sentence for each; a request carrying nothing at the identity source,
//! > and one whose function refuses the call with the `Unauthorized` error API Gateway
//! > documents, are refused `401`. For EVERY refused request the method's integration is never
//! > performed — nothing of the backend runs — and whichever way the request was answered, the
//! > stage writes ONE access-log record for it into the destination its `accessLogSettings`
//! > name, carrying that answer's status, the request id the response reported and the
//! > STAGE-QUALIFIED path the client addressed.
//!
//! The rows differ along what the rule generalises over: WHAT the authorizer's function
//! returned for the presented token (an `Allow` naming the invoked method, an explicit `Deny`,
//! an `Allow` naming another method of the same api), and each way a call earns no decision at
//! all (no identity source at all, so the function is not invoked; a function that fails with
//! the documented `Unauthorized` error). Each row asserts the status, the sentence, whether
//! the BACKEND ran, and the access-log record the stage left behind.
//!
//! What makes "the backend never ran" observable: the backend function PRINTS a marker the
//! request itself carries, so a run leaves that marker in the function's own CloudWatch Logs
//! group and a request that never reached it leaves nothing. The absence is read only once the
//! stage's access-log record for that same request has arrived — the gateway has by then
//! finished answering it — so the assertion waits on the CONDITION rather than on a span.
//!
//! Only a cross-service suite can pin this: the api, the authorizer record, the stage and its
//! logging configuration are API Gateway's, both functions and the grants that admit their
//! invocation are AWS Lambda's, and the groups the records land in are CloudWatch Logs' — all
//! three on ONE engine, as `core/router/src/lib.rs` mounts them.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "us-east-1";

/// The stage the api is deployed to, and the resource path of its one protected method.
const STAGE: &str = "test";
const RESOURCE_PATH: &str = "secure";

/// The two functions: the one the authorizer names, and the one behind the method.
const AUTHORIZER_FUNCTION: &str = "token-authorizer";
const BACKEND_FUNCTION: &str = "secured-backend";

/// The role a created function names. No IAM store is mounted here, so it is a value the
/// create carries rather than a record this suite relies on.
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-exec";

/// The handler each deployment package declares.
const HANDLER: &str = "lambda_function.lambda_handler";

/// The header the authorizer declares as its identity source.
const IDENTITY_HEADER: &str = "authorization";

/// The access-log destination the stage is pointed at, and the format it renders records by.
const ACCESS_GROUP: &str = "/apigw/access/authorizer";
const ACCESS_FORMAT: &str = concat!(
    r#"{"requestId":"$context.requestId","path":"$context.path","#,
    r#""status":"$context.status","principal":"$context.authorizer.principalId"}"#
);

/// The account-level role API Gateway writes the account's logs as
/// (`Account$cloudwatchRoleArn`). No IAM store is mounted here, so it is a value the patch
/// carries rather than a record this suite relies on.
const CLOUDWATCH_ROLE: &str = "arn:aws:iam::000000000000:role/apigw-cloudwatch";

/// How long this suite waits for another service's own work to become observable (a created
/// function becoming `Active`, a delivered log record), and how often it looks. Generous: the
/// wait is on the CONDITION, and nothing here is graded on how fast the host runs.
const WAIT: Duration = Duration::from_secs(30);
const LOOK: Duration = Duration::from_millis(50);

/// The line the backend prints, which is what a run of it leaves in its own log group.
const BACKEND_MARKER: &str = "BACKEND-INVOKED";

/// The sentence a call the returned policy explicitly DENIES is refused with.
const EXPLICIT_DENY: &str = "User is not authorized to access this resource with an explicit \
                             deny in an identity-based policy";

/// The sentence a call no statement of the returned policy allows is refused with.
const NOT_AUTHORIZED: &str = "User is not authorized to access this resource";

/// The body an uncustomized `UNAUTHORIZED` gateway response carries.
const UNAUTHORIZED_MESSAGE: &str = "Unauthorized";

/// The header API Gateway reports the id of the request it processed in, which the
/// access-log record's `$context.requestId` has to equal.
const REQUEST_ID_HEADER: &str = "x-amzn-requestid";

/// The authorizer's function: it decides from the token it was handed and publishes the
/// principal the record names.
const AUTHORIZER_SOURCE: &str = r#"
def lambda_handler(event, context):
    token = event.get("authorizationToken") or ""
    if token == "fail-me":
        raise Exception("Unauthorized")
    resource = event["methodArn"]
    effect = "Allow"
    if token.startswith("deny"):
        effect = "Deny"
    if token.startswith("other"):
        resource = resource.rsplit("/", 1)[0] + "/elsewhere"
    return {
        "principalId": "principal-" + token,
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [
                {"Action": "execute-api:Invoke", "Effect": effect, "Resource": resource}
            ],
        },
    }
"#;

/// The function behind the method: it PRINTS the marker the request carried and answers the
/// proxy envelope naming it, so a run of it is observable in two places.
const BACKEND_SOURCE: &str = r#"
import json


def lambda_handler(event, context):
    marker = (event.get("queryStringParameters") or {}).get("marker") or "none"
    print("BACKEND-INVOKED " + marker)
    return {
        "statusCode": 200,
        "headers": {"Content-Type": "application/json"},
        "body": json.dumps({"ran": marker}),
    }
"#;

/// One answered request: its status, its body verbatim and the request id it was reported
/// under.
struct Answered {
    status: u16,
    body: Bytes,
    request_id: Option<String>,
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

    /// The text member `name` of a JSON body, empty when it carries none.
    fn member(&self, name: &str) -> String {
        self.json()
            .get(name)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The sentence a refused answer carries. A gateway response words it under `message` or
    /// under `Message` depending on which response it is — the `ACCESS_DENIED` body AWS
    /// writes carries the capitalised member — so both spellings are read.
    fn message(&self) -> String {
        let under_lowercase = self.member("message");
        if under_lowercase.is_empty() {
            return self.member("Message");
        }
        under_lowercase
    }
}

/// The three services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts
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
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_cloudwatchlogs_logs_20140328::EmulatedService::new(engine),
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
        body,
        request_id: parts
            .headers
            .get(REQUEST_ID_HEADER)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string),
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
        answered.status,
        expected,
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

/// One CloudWatch Logs call over its `awsJson1_1` wire, answering its status and parsed body.
async fn logs(router: &MultiServiceRouter, operation: &str, body: Value) -> (u16, Value) {
    let answered = dispatch(
        router,
        "POST",
        &format!("logs.{REGION}.amazonaws.com"),
        "/",
        &[
            ("content-type", "application/x-amz-json-1.1"),
            ("authorization", &authorization("logs")),
            ("x-amz-target", &format!("Logs_20140328.{operation}")),
        ],
        Bytes::from(body.to_string()),
    )
    .await;
    (answered.status, answered.json())
}

/// One request to the api's own invoke endpoint, carrying `headers` and no credential — as a
/// client of a custom-authorized method sends none.
async fn invoke(
    router: &MultiServiceRouter,
    api: &str,
    marker: &str,
    headers: &[(&str, &str)],
) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}/{RESOURCE_PATH}?marker={marker}"),
        headers,
        Bytes::new(),
    )
    .await
}

/// The ARN of the log group `name`, as a stage's `destinationArn` names one.
fn group_arn(name: &str) -> String {
    format!(
        "arn:aws:logs:{REGION}:{account}:log-group:{name}",
        account = engine::emulator_account_id()
    )
}

/// The messages `FilterLogEvents` reports for `group`, or `None` when the account holds no
/// such group at all — which is what "nothing ran and nothing was created" looks like to a
/// client.
async fn delivered(router: &MultiServiceRouter, group: &str) -> Option<Vec<String>> {
    let (status, body) = logs(router, "FilterLogEvents", json!({ "logGroupName": group })).await;
    if status != 200 {
        return None;
    }
    Some(
        body.get("events")
            .and_then(Value::as_array)
            .map(|events| {
                events
                    .iter()
                    .map(|event| {
                        event
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string()
                    })
                    .collect()
            })
            .unwrap_or_default(),
    )
}

/// Look in `group` until one message satisfying `wanted` is held, and answer it; `None` once
/// the deadline passes with none. The wait is on the CONDITION — a delivery another service
/// makes — so it polls until it holds rather than sleeping a span.
async fn awaited_message(
    router: &MultiServiceRouter,
    group: &str,
    wanted: impl Fn(&str) -> bool,
) -> Option<String> {
    let deadline = Instant::now() + WAIT;
    loop {
        if let Some(messages) = delivered(router, group).await {
            if let Some(found) = messages.into_iter().find(|message| wanted(message)) {
                return Some(found);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        tokio::time::sleep(LOOK).await;
    }
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

/// Create one function from `source` and answer its ARN, once Lambda reports it `Active`.
async fn function(router: &MultiServiceRouter, name: &str, source: &str) -> String {
    let (status, created) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": name,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package(source)),
            },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction({name}): expected HTTP 201, got {status} with {created}"
    );
    let arn = member("CreateFunction", &created, "FunctionArn");
    let deadline = Instant::now() + WAIT;
    loop {
        let (_, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{name}/configuration"),
            None,
        )
        .await;
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return arn;
        }
        assert!(
            Instant::now() < deadline,
            "setup CreateFunction({name}): the function never became Active, last read \
             {configuration}"
        );
        tokio::time::sleep(LOOK).await;
    }
}

/// Grant `apigateway.amazonaws.com` the invocation of `name`, scoped to `source_arn`.
async fn grant(router: &MultiServiceRouter, name: &str, statement: &str, source_arn: &str) {
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{name}/policy"),
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
        "setup AddPermission({name}): expected HTTP 201, got {status} with {granted}"
    );
}

/// The whole wiring: both functions, the api with one `CUSTOM`-authorized `GET` behind an
/// `AWS_PROXY` integration, the grants, the deployment, the account's CloudWatch role and the
/// stage's access logging. Answers the api id.
async fn deployed(router: &MultiServiceRouter) -> String {
    let authorizer_arn = function(router, AUTHORIZER_FUNCTION, AUTHORIZER_SOURCE).await;
    let backend_arn = function(router, BACKEND_FUNCTION, BACKEND_SOURCE).await;

    let api = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "authorizer-deny-blocks-the-backend" })),
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

    let authorizer = apigw(
        router,
        "CreateAuthorizer",
        "POST",
        &format!("/restapis/{api}/authorizers"),
        Some(json!({
            "name": "token-authorizer",
            "type": "TOKEN",
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {authorizer_arn}/invocations"
            ),
            // The documented spelling of the header source, whose case the lookup of the
            // presented value does not depend on.
            "identitySource": "method.request.header.Authorization",
            // "If it equals 0, authorization caching is disabled": every row's own token
            // reaches the function, so one row cannot be decided by another's held result.
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer = member("CreateAuthorizer", &authorizer, "id");
    let account = engine::emulator_account_id();
    // The grant that admits the AUTHORIZER's invocation is scoped to the authorizer itself,
    // which is the `aws:SourceArn` API Gateway makes such a call under.
    grant(
        router,
        AUTHORIZER_FUNCTION,
        "apigateway-invoke-authorizer",
        &format!("arn:aws:execute-api:{REGION}:{account}:{api}/*/*"),
    )
    .await;
    // The grant that admits the INTEGRATION's invocation is scoped to the methods of the api,
    // as a client scripts it: every stage, every verb, every path of this api.
    grant(
        router,
        BACKEND_FUNCTION,
        "apigateway-invoke-backend",
        &format!("arn:aws:execute-api:{REGION}:{account}:{api}/*/*"),
    )
    .await;

    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": RESOURCE_PATH })),
        201,
    )
    .await;
    let resource = member("CreateResource", &resource, "id");
    let method = format!("/restapis/{api}/resources/{resource}/methods/GET");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &method,
        Some(json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer })),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("{method}/integration"),
        Some(json!({
            "type": "AWS_PROXY",
            "httpMethod": "POST",
            "uri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {backend_arn}/invocations"
            ),
        })),
        201,
    )
    .await;
    apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    // The account's CloudWatch role, which is what API Gateway writes the account's logs as.
    apigw(
        router,
        "UpdateAccount",
        "PATCH",
        "/account",
        Some(json!({
            "patchOperations": [
                { "op": "replace", "path": "/cloudwatchRoleArn", "value": CLOUDWATCH_ROLE },
            ],
        })),
        200,
    )
    .await;
    // The destination the stage is pointed at, created by the client as a client does.
    let (status, created) = logs(
        router,
        "CreateLogGroup",
        json!({ "logGroupName": ACCESS_GROUP }),
    )
    .await;
    assert_eq!(
        status, 200,
        "setup CreateLogGroup: expected HTTP 200, got {status} with {created}"
    );
    apigw(
        router,
        "UpdateStage accessLogSettings",
        "PATCH",
        &format!("/restapis/{api}/stages/{STAGE}"),
        Some(json!({
            "patchOperations": [
                {
                    "op": "replace",
                    "path": "/accessLogSettings/destinationArn",
                    "value": group_arn(ACCESS_GROUP),
                },
                { "op": "replace", "path": "/accessLogSettings/format", "value": ACCESS_FORMAT },
            ],
        })),
        200,
    )
    .await;
    api
}

/// One row: the token the request presents, and what the stage owes it.
struct Row {
    case: &'static str,
    /// The value at the authorizer's identity source, or `None` for a request carrying none.
    token: Option<&'static str>,
    /// The status the stage answers.
    status: u16,
    /// The sentence the `message` member of a refused answer carries, or `None` for the row
    /// the integration serves.
    message: Option<&'static str>,
    /// Whether the method's integration — the backend function — runs at all.
    backend_runs: bool,
}

/// The table the rule is asserted over.
const ROWS: &[Row] = &[
    Row {
        case: "a token whose policy allows the invoked method",
        token: Some("allow-me"),
        status: 200,
        message: None,
        backend_runs: true,
    },
    Row {
        case: "a token whose policy explicitly denies the invoked method",
        token: Some("deny-me"),
        status: 403,
        message: Some(EXPLICIT_DENY),
        backend_runs: false,
    },
    Row {
        case: "a token whose policy allows another method of the same api",
        token: Some("other-method"),
        status: 403,
        message: Some(NOT_AUTHORIZED),
        backend_runs: false,
    },
    Row {
        case: "a request carrying nothing at the identity source",
        token: None,
        status: 401,
        message: Some(UNAUTHORIZED_MESSAGE),
        backend_runs: false,
    },
    Row {
        case: "a token whose function refuses the call with the Unauthorized error",
        token: Some("fail-me"),
        status: 401,
        message: Some(UNAUTHORIZED_MESSAGE),
        backend_runs: false,
    },
];

#[tokio::test]
async fn a_token_authorizer_decides_the_method_and_a_refusal_never_reaches_the_backend() {
    let mut router = emulator();
    let api = deployed(&router).await;
    let backend_group = format!("/aws/lambda/{BACKEND_FUNCTION}");

    for (index, row) in ROWS.iter().enumerate() {
        let case = row.case;
        let marker = format!("marker-{index}");
        let headers: Vec<(&str, &str)> = match row.token {
            Some(token) => vec![(IDENTITY_HEADER, token)],
            None => Vec::new(),
        };
        // The invoke is answered on a router that VERIFIES request signatures, as the
        // emulator does when it is started with request authentication on: the stage's own
        // work — the authorizer's invocation, the integration's and the log deliveries — is
        // made over the in-process transport under the emulator's own credentials, and this
        // is where that holds or does not. The suite's own calls carry no real signature, so
        // verification goes back off for the reads below.
        router.set_request_authentication(true);
        let answered = invoke(&router, &api, &marker, &headers).await;
        router.set_request_authentication(false);

        assert_eq!(
            answered.status,
            row.status,
            "case {case:?}: expected HTTP {} from the stage, got {} with {:?}",
            row.status,
            answered.status,
            answered.text()
        );
        match row.message {
            Some(sentence) => assert_eq!(
                answered.message(),
                sentence,
                "case {case:?}: expected the refusal {sentence:?}, got {:?}",
                answered.text()
            ),
            None => assert_eq!(
                answered.json(),
                json!({ "ran": marker }),
                "case {case:?}: expected the integration's own answer for {marker:?}, got {:?}",
                answered.text()
            ),
        }

        // The stage logged the request it just answered: one record, carrying this request's
        // own id, the status it was answered with and the stage-qualified path addressed.
        let request_id = answered
            .request_id
            .clone()
            .unwrap_or_else(|| panic!("case {case:?}: the answer reported no request id"));
        let awaited = awaited_message(&router, ACCESS_GROUP, |message| {
            message.contains(&request_id)
        })
        .await;
        let held = delivered(&router, ACCESS_GROUP).await;
        let record = awaited.unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected an access-log record for {request_id:?} in \
                 {ACCESS_GROUP:?}, got {held:?}"
            )
        });
        let logged: Value = serde_json::from_str(&record).unwrap_or(Value::Null);
        assert_eq!(
            logged,
            json!({
                "requestId": request_id,
                "path": format!("/{STAGE}/{RESOURCE_PATH}"),
                "status": row.status.to_string(),
                "principal": expected_principal(row),
            }),
            "case {case:?}: expected the stage's own record of this request, got {record:?}"
        );

        // And what the BACKEND ran: the marker this request carried, present only where the
        // authorizer admitted the call. The absence is read after the record above arrived,
        // so the gateway has finished answering the request by then.
        let ran = awaited_backend_line(&router, &backend_group, &marker, row.backend_runs).await;
        let backend_held = delivered(&router, &backend_group).await;
        assert_eq!(
            ran,
            row.backend_runs,
            "case {case:?}: expected the backend to have run={}, got the group {backend_group:?} \
             holding {backend_held:?}",
            row.backend_runs
        );
    }
}

/// The principal the authorizer published for a row, as the record's own format reports it: a
/// request the authorizer never decided publishes none, which an unresolved reference renders
/// as the placeholder AWS writes an access-log field it has no value for as.
fn expected_principal(row: &Row) -> String {
    published_principal(row.message, row.token.unwrap_or_default())
}

/// The principal a request that earned `message` and presented `token` published, as the
/// record's own format reports it — the ONE reading of it both tables of this suite assert by.
fn published_principal(message: Option<&str>, token: &str) -> String {
    match message {
        // A refused call published nothing, whichever refusal it earned.
        Some(_) => service_common::api_gateway_access_logs::UNRESOLVED_VALUE.to_string(),
        None => format!("principal-{token}"),
    }
}

/// Whether the backend's group holds the line this request's marker names: awaited where the
/// row expects the run, and read once where it expects none.
async fn awaited_backend_line(
    router: &MultiServiceRouter,
    group: &str,
    marker: &str,
    expected: bool,
) -> bool {
    let wanted = format!("{BACKEND_MARKER} {marker}");
    if expected {
        return awaited_message(router, group, |message| message.contains(&wanted))
            .await
            .is_some();
    }
    delivered(router, group)
        .await
        .unwrap_or_default()
        .iter()
        .any(|message| message.contains(&wanted))
}

/// The status and body an authorizer whose decision could not be OBTAINED earns: AWS sets no
/// error message for that class, so the gateway response's own default template renders the
/// `message` member as the JSON literal `null` ([`crate::stage_authorizer_failure_body`] pins
/// the whole class).
const AUTHORIZER_FAILED: (u16, &str) = (500, r#"{"message":null}"#);

/// What a `MOCK`-backed method answers an admitted call with, so the ROLE rows below observe
/// the decision and nothing else.
const MOCK_BODY: &str = r#"{"mock":"served"}"#;

/// The two services the credentials rule spans beside this one: the function is AWS Lambda's,
/// and the role the authorizer names — its trust policy and its permissions — is IAM's.
fn emulator_with_iam() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_apigateway_backplanecontrolservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_iam_awsidentitymanagementv20100508::EmulatedService::new(engine),
    );
    router
}

/// One AWS IAM call over its `awsQuery` wire, asserting the 200 it has to answer.
async fn iam(router: &MultiServiceRouter, action: &str, members: &[(&str, &str)]) {
    let encoded = |value: &str| {
        ::percent_encoding::utf8_percent_encode(value, ::percent_encoding::NON_ALPHANUMERIC)
            .to_string()
    };
    let mut body = format!("Action={action}&Version=2010-05-08");
    for (key, value) in members {
        body.push('&');
        body.push_str(&encoded(key));
        body.push('=');
        body.push_str(&encoded(value));
    }
    let answered = dispatch(
        router,
        "POST",
        "iam.amazonaws.com",
        "/",
        &[
            ("content-type", "application/x-www-form-urlencoded"),
            ("authorization", &authorization("iam")),
        ],
        Bytes::from(body),
    )
    .await;
    assert_eq!(
        answered.status,
        200,
        "setup {action}: expected HTTP 200 from IAM, got {} with {:?}",
        answered.status,
        answered.text()
    );
}

/// One role of this account: `trust` as its trust policy and `policy` as its one inline
/// policy. Answers the ARN IAM holds it under.
async fn role(router: &MultiServiceRouter, name: &str, trust: &str, policy: &str) -> String {
    iam(
        router,
        "CreateRole",
        &[
            ("RoleName", name),
            ("Path", "/"),
            ("AssumeRolePolicyDocument", trust),
        ],
    )
    .await;
    iam(
        router,
        "PutRolePolicy",
        &[
            ("RoleName", name),
            ("PolicyName", "invoke"),
            ("PolicyDocument", policy),
        ],
    )
    .await;
    format!(
        "arn:aws:iam::{account}:role/{name}",
        account = engine::emulator_account_id()
    )
}

/// A trust policy letting `principal` assume the role it is attached to.
fn trusting(principal: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{
            "Effect": "Allow",
            "Principal": { "Service": principal },
            "Action": "sts:AssumeRole",
        }],
    })
    .to_string()
}

/// An identity policy allowing `action` on everything.
fn allowing(action: &str) -> String {
    json!({
        "Version": "2012-10-17",
        "Statement": [{ "Effect": "Allow", "Action": action, "Resource": "*" }],
    })
    .to_string()
}

/// One role row: the authorizer's `authorizerCredentials`, and what the stage then owes a
/// request carrying each token.
struct Credentialed {
    case: &'static str,
    /// Which role to create for it, as `(name, trust principal, allowed action)`, or `None`
    /// for the authorizer that names no credentials at all.
    role: Option<(&'static str, &'static str, &'static str)>,
    /// The path within the stage this authorizer's method sits on.
    path: &'static str,
}

/// The authorizers the credentials test stands up, each on its own method.
const CREDENTIALED: &[Credentialed] = &[
    Credentialed {
        case: "a role this service can assume whose policy allows the invocation",
        role: Some((
            "apigw-authorizer-invoker",
            "apigateway.amazonaws.com",
            "lambda:InvokeFunction",
        )),
        path: "under-role",
    },
    Credentialed {
        case: "a role this service can assume whose policy allows no invocation",
        role: Some((
            "apigw-authorizer-powerless",
            "apigateway.amazonaws.com",
            "logs:PutLogEvents",
        )),
        path: "powerless-role",
    },
    Credentialed {
        case: "a role this service cannot assume at all",
        role: Some((
            "apigw-authorizer-untrusted",
            "lambda.amazonaws.com",
            "lambda:InvokeFunction",
        )),
        path: "untrusted-role",
    },
    Credentialed {
        case: "no credentials at all, on a function granting this service nothing",
        role: None,
        path: "no-role",
    },
];

/// RULE: where an `Authorizer` names an IAM ROLE in its `authorizerCredentials`, API Gateway
/// invokes its function UNDER that role — so what admits the invocation is the role's own
/// trust and permissions, and the function's resource-based policy is not consulted at all.
/// "To specify an IAM role, specify the role's ARN. To use resource-based permissions on the
/// Lambda function, specify null": an authorizer naming none is admitted only by the grant on
/// the function, which is the control row here.
#[tokio::test]
async fn an_authorizer_naming_a_role_is_invoked_under_it_rather_than_by_the_function_s_policy() {
    let router = emulator_with_iam();
    // The function carries NO resource policy at all: nothing grants this service an
    // invocation of it, so every row below is decided by the role its authorizer names.
    let function_arn = function(&router, AUTHORIZER_FUNCTION, AUTHORIZER_SOURCE).await;

    let api = apigw(
        &router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "authorizer-credentials" })),
        201,
    )
    .await;
    let api = member("CreateRestApi", &api, "id");
    let resources = apigw(
        &router,
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

    for credentialed in CREDENTIALED {
        let credentials = match credentialed.role {
            Some((name, principal, action)) => {
                Some(role(&router, name, &trusting(principal), &allowing(action)).await)
            }
            None => None,
        };
        let mut body = json!({
            "name": format!("{}-authorizer", credentialed.path),
            "type": "TOKEN",
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {function_arn}/invocations"
            ),
            "identitySource": "method.request.header.Authorization",
            "authorizerResultTtlInSeconds": 0,
        });
        if let Some(credentials) = &credentials {
            body["authorizerCredentials"] = json!(credentials);
        }
        let authorizer = apigw(
            &router,
            "CreateAuthorizer",
            "POST",
            &format!("/restapis/{api}/authorizers"),
            Some(body),
            201,
        )
        .await;
        // What the create stored is what a later read reports, which is what the data plane
        // then invokes the function under.
        let authorizer = member("CreateAuthorizer", &authorizer, "id");
        let read = apigw(
            &router,
            "GetAuthorizer",
            "GET",
            &format!("/restapis/{api}/authorizers/{authorizer}"),
            None,
            200,
        )
        .await;
        assert_eq!(
            read.get("authorizerCredentials").and_then(Value::as_str),
            credentials.as_deref(),
            "case {:?}: expected GetAuthorizer to report the stored credentials, got {read}",
            credentialed.case
        );

        let resource = apigw(
            &router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": credentialed.path })),
            201,
        )
        .await;
        let resource = member("CreateResource", &resource, "id");
        let method = format!("/restapis/{api}/resources/{resource}/methods/GET");
        apigw(
            &router,
            "PutMethod",
            "PUT",
            &method,
            Some(json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer })),
            201,
        )
        .await;
        // A `MOCK` integration, so what an ADMITTED call is answered with is the api's own
        // records: the rows vary only in what admits the authorizer's invocation.
        apigw(
            &router,
            "PutIntegration",
            "PUT",
            &format!("{method}/integration"),
            Some(json!({
                "type": "MOCK",
                "requestTemplates": { "application/json": r#"{"statusCode": 200}"# },
            })),
            201,
        )
        .await;
        apigw(
            &router,
            "PutMethodResponse",
            "PUT",
            &format!("{method}/responses/200"),
            Some(json!({})),
            201,
        )
        .await;
        apigw(
            &router,
            "PutIntegrationResponse",
            "PUT",
            &format!("{method}/integration/responses/200"),
            Some(json!({
                "responseTemplates": { "application/json": MOCK_BODY },
            })),
            201,
        )
        .await;
    }
    apigw(
        &router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "stageName": STAGE })),
        201,
    )
    .await;

    for credentialed in CREDENTIALED {
        let case = credentialed.case;
        let admitted = matches!(
            credentialed.role,
            Some((_, "apigateway.amazonaws.com", "lambda:InvokeFunction"))
        );
        // Each token the ONE function decides differently, so an admitted invocation is told
        // from one that never happened by the DECISION the client is answered with.
        let rows: &[(&str, &str, u16, &str)] = &[
            ("allow-me", "an allowed token", 200, MOCK_BODY),
            ("deny-me", "a denied token", 403, EXPLICIT_DENY),
        ];
        for (token, which, status, body) in rows {
            let answered = dispatch(
                &router,
                "GET",
                &format!("{api}.execute-api.{REGION}.amazonaws.com"),
                &format!("/{STAGE}/{path}", path = credentialed.path),
                &[(IDENTITY_HEADER, token)],
                Bytes::new(),
            )
            .await;
            let (expected_status, expected_body) = if admitted {
                (*status, (*body).to_string())
            } else {
                (AUTHORIZER_FAILED.0, AUTHORIZER_FAILED.1.to_string())
            };
            assert_eq!(
                answered.status,
                expected_status,
                "case {case:?} / {which}: expected HTTP {expected_status}, got {} with {:?}",
                answered.status,
                answered.text()
            );
            let reported = if answered.status == 200 || !admitted {
                // A served call is read by its body, and a call its authorizer left undecided
                // carries no sentence to read at all — its whole body is the answer owed.
                answered.text()
            } else {
                answered.message()
            };
            assert_eq!(
                reported, expected_body,
                "case {case:?} / {which}: expected {expected_body:?}, got {:?}",
                answered.text()
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// How a client that resolves the api's own invoke host NOWHERE addresses the same stage.
//
// THE RULE, one level above the addressing the suite above uses: a client on the machine
// this emulator runs on resolves `{api-id}.execute-api.{region}.amazonaws.com` nowhere, so
// it reaches the emulator's own endpoint and carries in the PATH what it cannot carry in the
// host — the reserved `/_aws` namespace segment, then the ENDPOINT it was handed, written
// either as that endpoint's `execute-api` label followed by the api id or as the endpoint
// HOST itself (the two ways a rewriter can carry an endpoint across), then the invoke path
// whole. Such an address is read on whatever host it arrives at, and the stage answers it
// with the same decision, the same integration result and the same access-log record its own
// endpoint answers with — so a `CUSTOM`-authorized method is as decidable for that client as
// for one that reaches AWS. An address naming an endpoint whose api id this emulator holds no
// api for is served by no api of it.
//
// The rows differ along what the rule generalises over: the SPELLING of the endpoint behind
// the marker (the label and the api id; the endpoint host itself, in its plain, `-fips` and
// China-partition forms), the HOST the request arrives at (the emulator's own loopback
// endpoint with a port, that endpoint written by name, and the api's own `execute-api` host),
// and what the request presents at the authorizer's identity source, which is what decides
// whether the method's integration runs at all.
// ---------------------------------------------------------------------------------------

/// An address of the emulator's own endpoint, which names no service at all: what a client
/// that cannot resolve the api's own host sends to.
const LOCAL_ENDPOINT: &str = "127.0.0.1:9297";

/// That same endpoint written by name, the other way a client on this machine addresses it.
const LOCAL_NAME: &str = "localhost";

/// An api id this emulator holds no api for, so no endpoint of it is served by any api.
const UNHELD_API_ID: &str = "zzz9999999";

/// How a row names the endpoint it addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Addressing {
    /// `/_aws/execute-api/{api-id}/…` — the endpoint LABEL, then the api id.
    EndpointLabel,
    /// `/_aws/{api-id}.execute-api.{region}.amazonaws.com/…` — the endpoint HOST itself.
    EndpointHost,
    /// That host in the `-fips` variant spelling of the same endpoint.
    FipsEndpointHost,
    /// That host in the China partition's domain, which carries one label more.
    ChinaEndpointHost,
    /// The api's own endpoint host with no marker in the path at all: the addressing
    /// `RestApi$disableExecuteApiEndpoint` documents, which is the control here.
    OwnEndpointHost,
}

impl Addressing {
    /// The path a request of this addressing carries for `api`, asking for `marker`.
    fn path(self, api: &str, marker: &str) -> String {
        let invoke = format!("/{STAGE}/{RESOURCE_PATH}?marker={marker}");
        match self {
            Self::EndpointLabel => format!("/_aws/execute-api/{api}{invoke}"),
            Self::EndpointHost => {
                format!("/_aws/{api}.execute-api.{REGION}.amazonaws.com{invoke}")
            }
            Self::FipsEndpointHost => {
                format!("/_aws/{api}.execute-api-fips.{REGION}.amazonaws.com{invoke}")
            }
            Self::ChinaEndpointHost => {
                format!("/_aws/{api}.execute-api.cn-north-1.amazonaws.com.cn{invoke}")
            }
            Self::OwnEndpointHost => invoke,
        }
    }
}

/// One row: how the request names the endpoint, and which host it arrives at.
struct AddressRow {
    case: &'static str,
    addressing: Addressing,
    /// The host the request carries, or `None` for the api's own endpoint host.
    host: Option<&'static str>,
}

/// The table the addressing rule is asserted over.
const ADDRESS_ROWS: &[AddressRow] = &[
    AddressRow {
        case: "the endpoint label and the api id, on the emulator's own loopback endpoint",
        addressing: Addressing::EndpointLabel,
        host: Some(LOCAL_ENDPOINT),
    },
    AddressRow {
        case: "the endpoint host carried across, on the emulator's own loopback endpoint",
        addressing: Addressing::EndpointHost,
        host: Some(LOCAL_ENDPOINT),
    },
    AddressRow {
        case: "the endpoint host carried across, on that endpoint written by name",
        addressing: Addressing::EndpointHost,
        host: Some(LOCAL_NAME),
    },
    AddressRow {
        case: "the endpoint host in its -fips variant spelling",
        addressing: Addressing::FipsEndpointHost,
        host: Some(LOCAL_ENDPOINT),
    },
    AddressRow {
        case: "the endpoint host in the China partition's domain",
        addressing: Addressing::ChinaEndpointHost,
        host: Some(LOCAL_ENDPOINT),
    },
    AddressRow {
        case: "the marker read on the api's OWN endpoint host, as it is on any host",
        addressing: Addressing::EndpointLabel,
        host: None,
    },
    AddressRow {
        case: "the api's own endpoint host with no marker at all",
        addressing: Addressing::OwnEndpointHost,
        host: None,
    },
];

/// What the authorizer's policy says about a presented token, and what the stage therefore
/// owes the request: the two outcomes every addressing above is asserted over.
struct Decision {
    case: &'static str,
    token: &'static str,
    status: u16,
    /// The sentence a refused answer carries, or `None` for the row the integration serves.
    message: Option<&'static str>,
    /// Whether the method's integration — the backend function — runs at all.
    backend_runs: bool,
}

const DECISIONS: &[Decision] = &[
    Decision {
        case: "a token whose policy explicitly denies the invoked method",
        token: "deny-me",
        status: 403,
        message: Some(EXPLICIT_DENY),
        backend_runs: false,
    },
    Decision {
        case: "a token whose policy allows the invoked method",
        token: "allow-me",
        status: 200,
        message: None,
        backend_runs: true,
    },
];

/// One request to the stage, addressed as `row` says and carrying `token` at the authorizer's
/// identity source — and no credential, as a client of a `CUSTOM`-authorized method sends
/// none.
async fn invoke_addressed(
    router: &MultiServiceRouter,
    api: &str,
    row: &AddressRow,
    token: &str,
    marker: &str,
) -> Answered {
    let host = row
        .host
        .map(str::to_string)
        .unwrap_or_else(|| format!("{api}.execute-api.{REGION}.amazonaws.com"));
    dispatch(
        router,
        "GET",
        &host,
        &row.addressing.path(api, marker),
        &[(IDENTITY_HEADER, token)],
        Bytes::new(),
    )
    .await
}

#[tokio::test]
async fn a_rewritten_endpoint_reaches_the_same_stage_and_earns_the_same_decision() {
    let router = emulator();
    let api = deployed(&router).await;
    let backend_group = format!("/aws/lambda/{BACKEND_FUNCTION}");

    for (index, row) in ADDRESS_ROWS.iter().enumerate() {
        for (choice, decision) in DECISIONS.iter().enumerate() {
            let case = format!("{} / {}", row.case, decision.case);
            let marker = format!("addressed-{index}-{choice}");
            let answered = invoke_addressed(&router, &api, row, decision.token, &marker).await;

            assert_eq!(
                answered.status,
                decision.status,
                "case {case:?}: expected HTTP {} from the stage, got {} with {:?}",
                decision.status,
                answered.status,
                answered.text()
            );
            match decision.message {
                Some(sentence) => assert_eq!(
                    answered.message(),
                    sentence,
                    "case {case:?}: expected the refusal {sentence:?}, got {:?}",
                    answered.text()
                ),
                None => assert_eq!(
                    answered.json(),
                    json!({ "ran": marker }),
                    "case {case:?}: expected the integration's own answer for {marker:?}, got {:?}",
                    answered.text()
                ),
            }

            // The stage logged the request it just answered, under the STAGE-QUALIFIED path
            // the client addressed — the api's own path space, whatever the client had to
            // carry in front of it to reach this emulator.
            let request_id = answered
                .request_id
                .clone()
                .unwrap_or_else(|| panic!("case {case:?}: the answer reported no request id"));
            let awaited = awaited_message(&router, ACCESS_GROUP, |message| {
                message.contains(&request_id)
            })
            .await;
            let held = delivered(&router, ACCESS_GROUP).await;
            let record = awaited.unwrap_or_else(|| {
                panic!(
                    "case {case:?}: expected an access-log record for {request_id:?} in \
                     {ACCESS_GROUP:?}, got {held:?}"
                )
            });
            let logged: Value = serde_json::from_str(&record).unwrap_or(Value::Null);
            assert_eq!(
                logged,
                json!({
                    "requestId": request_id,
                    "path": format!("/{STAGE}/{RESOURCE_PATH}"),
                    "status": decision.status.to_string(),
                    "principal": published_principal(decision.message, decision.token),
                }),
                "case {case:?}: expected the stage's own record of this request, got {record:?}"
            );
            // ONE record per answered request, which is what the stage owes: a second
            // delivery of the same request fails here.
            let for_this_request = delivered(&router, ACCESS_GROUP)
                .await
                .unwrap_or_default()
                .iter()
                .filter(|message| message.contains(&request_id))
                .count();
            assert_eq!(
                for_this_request, 1,
                "case {case:?}: expected exactly one access-log record for {request_id:?}, got \
                 {for_this_request}"
            );

            // And what the BACKEND ran: the marker this request carried, present only where
            // the authorizer admitted the call. The absence is read once the record above
            // arrived, so the gateway has finished answering the request by then.
            let ran =
                awaited_backend_line(&router, &backend_group, &marker, decision.backend_runs).await;
            let backend_held = delivered(&router, &backend_group).await;
            assert_eq!(
                ran, decision.backend_runs,
                "case {case:?}: expected the backend to have run={}, got the group \
                 {backend_group:?} holding {backend_held:?}",
                decision.backend_runs
            );

            // The AUTHORIZER's own function ran for every row, whichever way it decided, so
            // its own log group holds that run — the evidence that tells a refusal the
            // authorizer MADE from a request that never reached it.
            let authorizer_group = format!("/aws/lambda/{AUTHORIZER_FUNCTION}");
            let framed = awaited_message(&router, &authorizer_group, |message| {
                message.starts_with("START RequestId: ")
            })
            .await;
            assert!(
                framed.is_some(),
                "case {case:?}: expected the authorizer's own run in {authorizer_group:?}, got \
                 {:?}",
                delivered(&router, &authorizer_group).await
            );
        }
    }
}

#[tokio::test]
async fn a_rewritten_endpoint_naming_an_api_this_emulator_holds_none_for_is_served_by_no_api() {
    let router = emulator();
    let api = deployed(&router).await;
    let backend_group = format!("/aws/lambda/{BACKEND_FUNCTION}");

    // Both spellings of the marker, each naming an endpoint of an api id this emulator holds
    // no api for: the api deployed above answers neither, however reachable its own stage is.
    let cases: &[(&str, String)] = &[
        (
            "the endpoint label and an api id no api of this emulator has",
            format!("/_aws/execute-api/{UNHELD_API_ID}/{STAGE}/{RESOURCE_PATH}?marker=unheld"),
        ),
        (
            "the endpoint host of an api id no api of this emulator has",
            format!(
                "/_aws/{UNHELD_API_ID}.execute-api.{REGION}.amazonaws.com/{STAGE}/\
                 {RESOURCE_PATH}?marker=unheld"
            ),
        ),
    ];
    for (case, path) in cases {
        let answered = dispatch(
            &router,
            "GET",
            LOCAL_ENDPOINT,
            path,
            &[(IDENTITY_HEADER, "allow-me")],
            Bytes::new(),
        )
        .await;
        assert_ne!(
            answered.status, 200,
            "case {case:?}: expected the stage of {api} not to serve {path:?}, got HTTP {} \
             with {:?}",
            answered.status,
            answered.text()
        );
        let ran = awaited_backend_line(&router, &backend_group, "unheld", false).await;
        assert!(
            !ran,
            "case {case:?}: expected the backend never to run for {path:?}, got the group \
             {backend_group:?} holding {:?}",
            delivered(&router, &backend_group).await
        );
    }
}
