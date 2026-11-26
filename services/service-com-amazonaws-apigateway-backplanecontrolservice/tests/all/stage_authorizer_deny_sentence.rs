// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE the `execute-api` data plane refuses a custom-authorized call with, end to
//! end: what a client reads in the `403` body when the policy a Lambda authorizer returned
//! does not admit the invoked method.
//!
//! THE RULE UNDER TEST: **a call an authorizer policy EXPLICITLY DENIES is refused with IAM's
//! whole published sentence for that outcome — "User is not authorized to access this resource
//! with an explicit deny in an identity-based policy" — whose every word is fixed text, so the
//! clause naming the document that decided it is what tells this refusal from the one a policy
//! that merely fails to allow the invoke earns.** An authorizer "returns an IAM policy", so a
//! `Deny` in it is an explicit deny in an identity-based policy however that statement is
//! written.
//!
//! The rows differ along what the rule generalises over: HOW the denying statement is written —
//! the invoked method's own ARN, every resource, the api's own wildcard, a `Deny` beside an
//! `Allow` of the same method, and a `Deny` whose action is the service wildcard — beside the
//! two outcomes that are NOT an explicit deny, which must keep their own answers: an `Allow`
//! naming the invoked method (the integration runs) and an `Allow` naming another method (the
//! refusal that names no deny).
//!
//! Why it matters: a client or an operator that reads the body to tell "a Deny matched" from
//! "no Allow matched" cannot distinguish the two refusals when the sentence is truncated, so a
//! returned policy is mis-diagnosed — the two outcomes need opposite fixes.
//!
//! Only a cross-service suite can pin this: the authorizer's function is AWS Lambda's, and the
//! api, the authorizer record, the method and the stage are API Gateway's.

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

/// The path the custom-authorized method sits on.
const PATH: &str = "/secure";

/// The function the authorizer names.
const FUNCTION: &str = "deny-sentence-authorizer";

/// The role the created function names. No IAM store is mounted here, so it is a value the
/// create carries rather than a record this suite relies on.
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-exec";

/// The handler the deployment package declares.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long this suite waits for Lambda's own clock-owed transition (a created function
/// becoming `Active`), and how often it looks.
const WAIT: Duration = Duration::from_secs(20);
const LOOK: Duration = Duration::from_millis(100);

/// The header the authorizer declares as its identity source.
const IDENTITY_HEADER: &str = "authorization";

/// The body the MOCK integration behind the method answers an admitted call with.
const ADMITTED_BODY: &str = r#"{"pong":true}"#;

/// The sentence IAM publishes for a call an identity-based policy explicitly DENIES, which is
/// what the gateway refuses such an invoke with. Every word of it is fixed text.
const EXPLICIT_DENY: &str = "User is not authorized to access this resource with an explicit \
                             deny in an identity-based policy";

/// The sentence a call NO statement of the returned policy allows is refused with — the
/// outcome the clause above is what distinguishes from an explicit deny. IAM's published
/// no-matching-allow wording, which names the action the evaluation asked about; the class it
/// belongs to is spanned in `tests/all/stage_authorizer_no_matching_allow_sentence.rs`.
const NOT_AUTHORIZED: &str = "User is not authorized to access this resource because no \
                              identity-based policy allows the execute-api:Invoke action";

/// The status API Gateway refuses an authorized-but-not-admitted call at.
const REFUSED_STATUS: u16 = 403;

/// The authorizer's function: it chooses the policy it returns from the token it was handed,
/// so one function covers every policy shape the rows span. The resources it names are built
/// from the event's own `methodArn`, which is the ARN of the method the request addressed.
const AUTHORIZER_SOURCE: &str = r#"
def lambda_handler(event, context):
    token = event.get("authorizationToken") or ""
    arn = event["methodArn"]
    other = arn.rsplit("/", 1)[0] + "/elsewhere"
    api_wildcard = arn.rsplit("/", 2)[0] + "/*/*"
    invoke = "execute-api:Invoke"
    statements = {
        "allow-the-method": [{"Action": invoke, "Effect": "Allow", "Resource": arn}],
        "allow-another-method": [{"Action": invoke, "Effect": "Allow", "Resource": other}],
        "deny-the-method": [{"Action": invoke, "Effect": "Deny", "Resource": arn}],
        "deny-every-resource": [{"Action": invoke, "Effect": "Deny", "Resource": "*"}],
        "deny-the-api": [{"Action": invoke, "Effect": "Deny", "Resource": api_wildcard}],
        "deny-over-allow": [
            {"Action": invoke, "Effect": "Allow", "Resource": arn},
            {"Action": invoke, "Effect": "Deny", "Resource": arn},
        ],
        "deny-every-action": [
            {"Action": "execute-api:*", "Effect": "Deny", "Resource": arn}
        ],
    }[token]
    return {
        "principalId": "principal-" + token,
        "policyDocument": {"Version": "2012-10-17", "Statement": statements},
    }
"#;

/// One row: the token the request presents, and what the data plane must answer for the policy
/// the function returns for it.
struct Row {
    case: &'static str,
    token: &'static str,
    status: u16,
    /// The `Message` the refusal states, or `None` for a row the integration answers.
    refused: Option<&'static str>,
}

const ROWS: &[Row] = &[
    Row {
        case: "a Deny naming the invoked method's own ARN",
        token: "deny-the-method",
        status: REFUSED_STATUS,
        refused: Some(EXPLICIT_DENY),
    },
    Row {
        case: "a Deny over every resource",
        token: "deny-every-resource",
        status: REFUSED_STATUS,
        refused: Some(EXPLICIT_DENY),
    },
    Row {
        case: "a Deny over the api's own wildcard",
        token: "deny-the-api",
        status: REFUSED_STATUS,
        refused: Some(EXPLICIT_DENY),
    },
    Row {
        case: "a Deny beside an Allow of the same method",
        token: "deny-over-allow",
        status: REFUSED_STATUS,
        refused: Some(EXPLICIT_DENY),
    },
    Row {
        case: "a Deny whose action is the service wildcard",
        token: "deny-every-action",
        status: REFUSED_STATUS,
        refused: Some(EXPLICIT_DENY),
    },
    Row {
        case: "an Allow naming another method, which denies nothing",
        token: "allow-another-method",
        status: REFUSED_STATUS,
        refused: Some(NOT_AUTHORIZED),
    },
    Row {
        case: "an Allow naming the invoked method",
        token: "allow-the-method",
        status: 200,
        refused: None,
    },
];

/// One answered request: its status and its body verbatim.
struct Answered {
    status: u16,
    body: Bytes,
}

impl Answered {
    /// The body as text.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The `Message` an authorization refusal states, empty when the body carries none.
    fn message(&self) -> String {
        serde_json::from_slice::<Value>(&self.body)
            .unwrap_or(Value::Null)
            .get("Message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }
}

/// The two services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts them.
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
    serde_json::from_slice(&answered.body).unwrap_or(Value::Null)
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
    (
        answered.status,
        serde_json::from_slice(&answered.body).unwrap_or(Value::Null),
    )
}

/// One request to the api's own invoke endpoint, carrying the identity source the authorizer
/// declares and no credential, as a client of a custom-authorized method sends none.
async fn invoke_as(router: &MultiServiceRouter, api: &str, token: &str) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{STAGE}{PATH}"),
        &[(IDENTITY_HEADER, token)],
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

/// Poll `GetFunctionConfiguration` until Lambda first reports the function `Active`.
async fn wait_until_active(router: &MultiServiceRouter) {
    let looks = WAIT.as_millis() / LOOK.as_millis().max(1);
    let mut last = Value::Null;
    for _ in 0..looks {
        let (_, configuration) = lambda(
            router,
            "GET",
            &format!("/2015-03-31/functions/{FUNCTION}/configuration"),
            None,
        )
        .await;
        if configuration.get("State").and_then(Value::as_str) == Some("Active") {
            return;
        }
        last = configuration;
        tokio::time::sleep(LOOK).await;
    }
    panic!("setup CreateFunction: the function never became Active, last read {last}");
}

/// Stand the function and the api up, and deploy the api to [`STAGE`]: one `CUSTOM` method
/// behind a `TOKEN` authorizer that holds no result, so every row is decided by its own
/// invocation of the function.
async fn deployed(router: &MultiServiceRouter) -> String {
    let (status, function) = lambda(
        router,
        "POST",
        "/2015-03-31/functions",
        Some(json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": HANDLER,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD
                    .encode(package(AUTHORIZER_SOURCE)),
            },
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup CreateFunction: expected HTTP 201, got {status} with {function}"
    );
    let function_arn = member("CreateFunction", &function, "FunctionArn");

    let api = apigw(
        router,
        "CreateRestApi",
        "POST",
        "/restapis",
        Some(json!({ "name": "authorizer-deny-sentence" })),
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
            "name": "deny-sentence-authorizer",
            "type": "TOKEN",
            "authorizerUri": format!(
                "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                 {function_arn}/invocations"
            ),
            "identitySource": format!("method.request.header.{IDENTITY_HEADER}"),
            // Nothing is HELD, so each row's own invocation decides it: the rows differ in
            // the policy the function returns, not in what a cache kept.
            "authorizerResultTtlInSeconds": 0,
        })),
        201,
    )
    .await;
    let authorizer = member("CreateAuthorizer", &authorizer, "id");
    // "Grants a principal permission to use a function", scoped to the authorizer whose
    // function is invoked — without it the gateway obtains no decision at all.
    let (status, granted) = lambda(
        router,
        "POST",
        &format!("/2015-03-31/functions/{FUNCTION}/policy"),
        Some(json!({
            "StatementId": "apigateway-invoke-authorizer",
            "Action": "lambda:InvokeFunction",
            "Principal": "apigateway.amazonaws.com",
            "SourceArn": format!(
                "arn:aws:execute-api:{REGION}:{account}:{api}/authorizers/{authorizer}",
                account = engine::emulator_account_id(),
            ),
        })),
    )
    .await;
    assert_eq!(
        status, 201,
        "setup AddPermission: expected HTTP 201, got {status} with {granted}"
    );

    let resource = apigw(
        router,
        "CreateResource",
        "POST",
        &format!("/restapis/{api}/resources/{root}"),
        Some(json!({ "pathPart": PATH.trim_start_matches('/') })),
        201,
    )
    .await;
    let resource = member("CreateResource", &resource, "id");
    apigw(
        router,
        "PutMethod",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET"),
        Some(json!({ "authorizationType": "CUSTOM", "authorizerId": authorizer })),
        201,
    )
    .await;
    // A `MOCK` integration, so what an ADMITTED call is answered with is the api's own records
    // and nothing else: the decision is the only thing the rows vary.
    apigw(
        router,
        "PutIntegration",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration"),
        Some(json!({
            "type": "MOCK",
            "requestTemplates": { "application/json": r#"{"statusCode": 200}"# },
        })),
        201,
    )
    .await;
    apigw(
        router,
        "PutMethodResponse",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/responses/200"),
        Some(json!({})),
        201,
    )
    .await;
    apigw(
        router,
        "PutIntegrationResponse",
        "PUT",
        &format!("/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"),
        Some(json!({ "responseTemplates": { "application/json": ADMITTED_BODY } })),
        201,
    )
    .await;

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

    wait_until_active(router).await;
    api
}

/// RULE: every shape of explicit `Deny` in the returned policy earns the SAME `403` sentence —
/// IAM's whole published one, naming the identity-based policy the deny is in — while an
/// `Allow` of the invoked method runs the integration and an `Allow` of another method earns
/// the refusal that names no deny.
#[tokio::test]
async fn an_explicit_deny_is_refused_with_iams_whole_published_sentence() {
    let router = emulator();
    let api = deployed(&router).await;

    for row in ROWS {
        let case = row.case;
        let answered = invoke_as(&router, &api, row.token).await;
        assert_eq!(
            answered.status,
            row.status,
            "case {case:?}: expected HTTP {}, got {} with {:?}",
            row.status,
            answered.status,
            answered.text()
        );
        match row.refused {
            Some(expected) => assert_eq!(
                answered.message(),
                expected,
                "case {case:?}: expected the refusal {expected:?}, got {:?}",
                answered.text()
            ),
            None => assert_eq!(
                answered.text(),
                ADMITTED_BODY,
                "case {case:?}: expected the integration's own body {ADMITTED_BODY:?}, got {:?}",
                answered.text()
            ),
        }
    }
}
