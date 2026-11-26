// SPDX-License-Identifier: CC0-1.0

//! The SENTENCE the `execute-api` data plane refuses a custom-authorized call with when the
//! policy the authorizer returned simply does NOT COVER the invoked method, end to end.
//!
//! THE RULE UNDER TEST: **the refusal of a call an authorizer's returned policy does not admit
//! states IAM's whole published sentence for the outcome the evaluation of that policy reached —
//! the opening "User is not authorized to access this resource" closed by `because no
//! identity-based policy allows the <action> action` where no statement allowed the invoke, and
//! by `with an explicit deny in an identity-based policy` where a `Deny` statement matched — so
//! the closing clause is what tells the two apart.** An `Authorizer` of type `TOKEN` or
//! `REQUEST` "returns an IAM policy" and the data plane asks it about `execute-api:Invoke` on
//! the ARN of the method the request addressed, so a document that names other methods, other
//! stages, other apis or another action allows this invoke nowhere, and that is IAM's default
//! deny rather than an explicit one.
//!
//! The rows differ along what the rule generalises over: WHY the document fails to allow the
//! invoke — the resource it names (another method of the same stage, another stage of the same
//! api, another api, a path prefix that only looks like this one), the action it names, a
//! document stating nothing at all, and a `Deny` aimed at another method only — beside the two
//! outcomes that must keep their own answers: a `Deny` naming the invoked method (the clause
//! that names the deny) and an `Allow` naming it (the integration runs).
//!
//! Why it matters: a client or an operator reads the body to tell "a `Deny` matched" from "no
//! `Allow` matched", and the two outcomes need opposite fixes to the returned policy — a
//! sentence that stops at the opening clause diagnoses neither.
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
const PATH: &str = "/reports";

/// The function the authorizer names.
const FUNCTION: &str = "no-matching-allow-authorizer";

/// The role the created function names. No IAM store is mounted here, so it is a value the
/// create carries rather than a record this suite relies on.
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-exec";

/// The handler the deployment package declares.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long this suite waits for Lambda's own clock-owed transition (a created function
/// becoming `Active`), and how often it looks. Generous: the wait is on the CONDITION, and
/// nothing here is graded on how fast the host runs.
const WAIT: Duration = Duration::from_secs(30);
const LOOK: Duration = Duration::from_millis(50);

/// The header the authorizer declares as its identity source.
const IDENTITY_HEADER: &str = "authorization";

/// The body the MOCK integration behind the method answers an admitted call with.
const ADMITTED_BODY: &str = r#"{"report":"ready"}"#;

/// The sentence a call NO statement of the returned policy allows is refused with: IAM's
/// published no-matching-allow wording, closed by the action the evaluation asked about —
/// `execute-api:Invoke`, the action `execute-api` publishes for calling a deployed method and
/// the one an authorizer's returned policy is written over.
const NO_MATCHING_ALLOW: &str = "User is not authorized to access this resource because no \
                                 identity-based policy allows the execute-api:Invoke action";

/// The sentence a call an explicit `Deny` in the returned policy refuses is worded with: the
/// same opening, closed by the clause naming the document the deny is in.
const EXPLICIT_DENY: &str = "User is not authorized to access this resource with an explicit \
                             deny in an identity-based policy";

/// The status API Gateway refuses an authorized-but-not-admitted call at.
const REFUSED_STATUS: u16 = 403;

/// The authorizer's function: it chooses the policy it returns from the token it was handed,
/// so one function covers every document shape the rows span. Each resource is built from the
/// event's own `methodArn` — the ARN of the method the request addressed — so no row carries a
/// literal id of this run.
const AUTHORIZER_SOURCE: &str = r#"
def lambda_handler(event, context):
    token = event.get("authorizationToken") or ""
    arn = event["methodArn"]
    api, stage, verb, path = arn.split(":", 5)[5].split("/", 3)
    head = arn.split(":", 5)[:5]
    def resource(api_id, stage_name, method, resource_path):
        return ":".join(head) + ":" + "/".join([api_id, stage_name, method, resource_path])
    invoke = "execute-api:Invoke"
    statements = {
        "allow-this-method": [
            {"Action": invoke, "Effect": "Allow", "Resource": arn}
        ],
        "allow-another-method": [
            {"Action": invoke, "Effect": "Allow",
             "Resource": resource(api, stage, verb, "elsewhere")}
        ],
        "allow-another-stage": [
            {"Action": invoke, "Effect": "Allow",
             "Resource": resource(api, "other-stage", verb, path)}
        ],
        "allow-another-api": [
            {"Action": invoke, "Effect": "Allow",
             "Resource": resource("zzz9999999", stage, verb, path)}
        ],
        "allow-another-verb": [
            {"Action": invoke, "Effect": "Allow",
             "Resource": resource(api, stage, "DELETE", path)}
        ],
        "allow-a-longer-path": [
            {"Action": invoke, "Effect": "Allow",
             "Resource": resource(api, stage, verb, path + "-archive")}
        ],
        "allow-another-action": [
            {"Action": "execute-api:InvalidateCache", "Effect": "Allow", "Resource": arn}
        ],
        "state-nothing": [],
        "deny-another-method": [
            {"Action": invoke, "Effect": "Deny",
             "Resource": resource(api, stage, verb, "elsewhere")}
        ],
        "deny-this-method": [
            {"Action": invoke, "Effect": "Deny", "Resource": arn}
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
        case: "an Allow naming another method of the same stage",
        token: "allow-another-method",
        status: REFUSED_STATUS,
        refused: Some(NO_MATCHING_ALLOW),
    },
    Row {
        case: "an Allow naming this method on another stage",
        token: "allow-another-stage",
        status: REFUSED_STATUS,
        refused: Some(NO_MATCHING_ALLOW),
    },
    Row {
        case: "an Allow naming this method on another api",
        token: "allow-another-api",
        status: REFUSED_STATUS,
        refused: Some(NO_MATCHING_ALLOW),
    },
    Row {
        case: "an Allow naming another HTTP method on this path",
        token: "allow-another-verb",
        status: REFUSED_STATUS,
        refused: Some(NO_MATCHING_ALLOW),
    },
    Row {
        case: "an Allow naming a path this one is only a prefix of",
        token: "allow-a-longer-path",
        status: REFUSED_STATUS,
        refused: Some(NO_MATCHING_ALLOW),
    },
    Row {
        case: "an Allow of another execute-api action on this very method",
        token: "allow-another-action",
        status: REFUSED_STATUS,
        refused: Some(NO_MATCHING_ALLOW),
    },
    Row {
        case: "a document whose statement list is empty",
        token: "state-nothing",
        status: REFUSED_STATUS,
        refused: Some(NO_MATCHING_ALLOW),
    },
    Row {
        case: "a Deny aimed at another method, which says nothing about this one",
        token: "deny-another-method",
        status: REFUSED_STATUS,
        refused: Some(NO_MATCHING_ALLOW),
    },
    Row {
        case: "a Deny naming the invoked method",
        token: "deny-this-method",
        status: REFUSED_STATUS,
        refused: Some(EXPLICIT_DENY),
    },
    Row {
        case: "an Allow naming the invoked method",
        token: "allow-this-method",
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

    /// The body as JSON, `Null` when it is not JSON at all.
    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or(Value::Null)
    }

    /// The `Message` an authorization refusal states, empty when the body carries none. The
    /// `ACCESS_DENIED` body AWS writes carries the capitalised member, and a gateway response
    /// a developer customised may word it under the lowercase one, so both are read.
    fn message(&self) -> String {
        let body = self.json();
        for name in ["Message", "message"] {
            if let Some(stated) = body.get(name).and_then(Value::as_str) {
                return stated.to_string();
            }
        }
        String::new()
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
        Some(json!({ "name": "authorizer-no-matching-allow" })),
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
            "name": "no-matching-allow-authorizer",
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

/// RULE: every document that fails to ALLOW the invoked method earns the same `403` sentence —
/// IAM's whole published one, naming the action no identity-based policy allowed — while a
/// `Deny` naming that method earns the clause that names the deny and an `Allow` naming it runs
/// the integration.
#[tokio::test]
async fn a_policy_that_allows_nothing_here_is_refused_with_iams_no_matching_allow_sentence() {
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
