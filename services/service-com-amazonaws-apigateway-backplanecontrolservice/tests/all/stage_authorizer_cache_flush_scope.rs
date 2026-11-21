// SPDX-License-Identifier: CC0-1.0

//! What `FlushStageAuthorizersCache` does to the results a DEPLOYED stage is HOLDING for its
//! `CUSTOM` authorizers, read where a client reads it: through the `execute-api` data plane.
//!
//! THE RULE UNDER TEST: **while an authorizer's `authorizerResultTtlInSeconds` window is
//! open, every request presenting an identity the stage already holds a result for is decided
//! by that held result and invokes nothing; `FlushStageAuthorizersCache` is accepted with
//! `202` and invalidates EVERY result the addressed stage holds — each identity, each
//! authorizer — so the next request presenting any of them invokes that authorizer's function
//! again although its TTL is nowhere near elapsing, while the results another stage of the
//! same api holds are untouched and a result obtained after the flush is held exactly as the
//! flushed one was.**
//!
//! `Authorizer$authorizerResultTtlInSeconds` is "the TTL in seconds of cached authorizer
//! results. If it equals 0, authorization caching is disabled. If it is greater than 0, API
//! Gateway caches authorizer responses" and `Authorizer$identitySource` is "the identity
//! source for which authorization is requested", so what is held is one result per authorizer
//! per presented identity; `FlushStageAuthorizersCache` is `DELETE
//! /restapis/{restApiId}/stages/{stageName}/cache/authorizers` with a `202` success status and
//! addresses ONE stage of ONE api, so it is that stage's whole authorizer cache it empties and
//! no other stage's.
//!
//! The rows span what the rule generalises over: WHICH identity the request presents (three
//! different values at the identity source), WHICH authorizer decides the method (both Lambda
//! authorizer types, `TOKEN` and `REQUEST`, each its own record with its own held entries),
//! and WHICH stage of the api the request reached (the flushed one and a sibling serving the
//! same deployment).
//!
//! What makes an invocation OBSERVABLE: the authorizer publishes a value that is different on
//! every run in the `context` map of its result, and the method's integration response
//! template reads it back through `$context.authorizer` — once as text and once as a NUMBER,
//! because "you cannot set a JSON object or array as a valid value of any key in the `context`
//! map" leaves every scalar publishable. So a held result is told from a fresh invocation by
//! what the client is served, not by anything this suite knows about the store; the sibling
//! control-plane suite [`crate::flush_stage_authorizers_cache`] pins the `202`, the 404s and
//! what the flush leaves alone in the records.
//!
//! The authorizer is AWS Lambda's and the api, the stage, the authorizer record and the method
//! are API Gateway's, so both services are mounted on this suite's own engine, as
//! [`crate::stage_custom_authorizer`] mounts them — that suite pins the DECISION a returned
//! policy takes and the single-identity flush; this one pins the flush's SCOPE over the set of
//! identities, authorizers and stages the rule covers.

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for and every ARN names.
const REGION: &str = "us-east-1";

/// The stage whose authorizer cache is flushed.
const FLUSHED_STAGE: &str = "live";

/// Another stage of the same api, serving the same deployment: what a flush of the stage above
/// may not touch.
const SPARED_STAGE: &str = "beta";

/// The function both authorizers of the api name.
const FUNCTION: &str = "flush-authorizers-probe";

/// The role the created function names. No IAM store is mounted here, so it is a value the
/// create carries rather than a record this suite relies on.
const ROLE: &str = "arn:aws:iam::000000000000:role/lambda-exec";

/// The handler the deployment package declares.
const HANDLER: &str = "lambda_function.lambda_handler";

/// How long this suite waits for Lambda's own clock-owed transition (a created function
/// becoming `Active`), and how often it looks.
const WAIT: Duration = Duration::from_secs(30);
const LOOK: Duration = Duration::from_millis(100);

/// The header both authorizers declare as their identity source.
const IDENTITY_HEADER: &str = "authorization";

/// `Authorizer$authorizerResultTtlInSeconds` both authorizers run under: far from elapsing
/// while this suite runs, so every reuse asserted below is the cache's and every fresh
/// invocation is the flush's doing rather than the window closing.
const HELD_TTL: u32 = 300;

/// The authorizer every row is decided by. It allows the call whatever identity it was
/// presented — the decision is the sibling suite's subject, not this one's — and publishes a
/// value that differs on every run, as text and as a number, beside the event type it saw.
const HANDLER_SOURCE: &str = r#"
import time


def lambda_handler(event, context):
    token = event.get("authorizationToken")
    if token is None:
        headers = event.get("headers") or {}
        token = headers.get("Authorization") or headers.get("authorization") or ""
    ran = time.time_ns()
    return {
        "principalId": "principal-" + token,
        "policyDocument": {
            "Version": "2012-10-17",
            "Statement": [
                {"Action": "execute-api:Invoke", "Effect": "Allow", "Resource": "*"}
            ],
        },
        "context": {"ran": str(ran), "count": ran, "kind": event["type"]},
    }
"#;

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

    /// The text member `name` of a JSON body, empty when it carries none.
    fn member(&self, name: &str) -> String {
        self.json()
            .get(name)
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
    router.register_service(service_com_amazonaws_lambda_awsgirapiservice::EmulatedService::new(
        engine,
    ));
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

/// One API Gateway control-plane call that has to be answered with `expected`.
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
        "case {case:?}: expected HTTP {expected} from {method} {path}, got {} with {:?}",
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
/// `{rest-api-id}.execute-api.{region}.amazonaws.com` — presenting `token` at the identity
/// source both authorizers declare and no credential, as a client of a custom-authorized
/// method sends none.
async fn invoke_as(
    router: &MultiServiceRouter,
    api: &str,
    stage: &str,
    path: &str,
    token: &str,
) -> Answered {
    dispatch(
        router,
        "GET",
        &format!("{api}.execute-api.{REGION}.amazonaws.com"),
        &format!("/{stage}{path}"),
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

/// One authorizer of the api and the method it decides, by the path that method sits on.
struct Guarded {
    /// The path within each stage.
    path: &'static str,
    /// `Authorizer$type`: the two Lambda authorizer types, which differ in the event the
    /// function is handed and are two separate records, so two separate caches.
    kind: &'static str,
}

/// The guarded methods of the api, each on its own path so one deployment serves both.
const GUARDED: &[Guarded] = &[
    Guarded {
        path: "/token",
        kind: "TOKEN",
    },
    Guarded {
        path: "/request",
        kind: "REQUEST",
    },
];

/// Stand the function and the api up, and deploy the api to both stages.
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
                    .encode(package(HANDLER_SOURCE)),
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
        Some(json!({ "name": "flush-authorizers-scope" })),
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

    for guarded in GUARDED {
        let part = guarded.path.trim_start_matches('/');
        let authorizer = apigw(
            router,
            "CreateAuthorizer",
            "POST",
            &format!("/restapis/{api}/authorizers"),
            Some(json!({
                "name": format!("{part}-authorizer"),
                "type": guarded.kind,
                "authorizerUri": format!(
                    "arn:aws:apigateway:{REGION}:lambda:path/2015-03-31/functions/\
                     {function_arn}/invocations"
                ),
                "identitySource": format!("method.request.header.{IDENTITY_HEADER}"),
                "authorizerResultTtlInSeconds": HELD_TTL,
            })),
            201,
        )
        .await;
        let authorizer = member("CreateAuthorizer", &authorizer, "id");
        // "Grants a principal permission to use a function" (`AddPermission`), scoped by
        // `SourceArn` to the AUTHORIZER whose function is invoked.
        let (status, granted) = lambda(
            router,
            "POST",
            &format!("/2015-03-31/functions/{FUNCTION}/policy"),
            Some(json!({
                "StatementId": format!("apigateway-invoke-{part}"),
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
            "setup AddPermission({part}): expected HTTP 201, got {status} with {granted}"
        );
        let resource = apigw(
            router,
            "CreateResource",
            "POST",
            &format!("/restapis/{api}/resources/{root}"),
            Some(json!({ "pathPart": part })),
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
        // A `MOCK` integration and no cache cluster on either stage, so what an answered call
        // carries is the authorizer's own published values and nothing cached anywhere else.
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
        // The response template reads back what the AUTHORIZER published for this call — the
        // per-run value as text and the same value as a NUMBER, left unquoted so a context
        // entry that is not text has to reach the template for the body to be JSON at all.
        apigw(
            router,
            "PutIntegrationResponse",
            "PUT",
            &format!(
                "/restapis/{api}/resources/{resource}/methods/GET/integration/responses/200"
            ),
            Some(json!({
                "responseTemplates": {
                    "application/json": "{\"ran\":\"$context.authorizer.ran\",\
                                          \"count\":$context.authorizer.count,\
                                          \"kind\":\"$context.authorizer.kind\",\
                                          \"principal\":\"$context.authorizer.principalId\"}",
                },
            })),
            201,
        )
        .await;
    }

    let deployment = apigw(
        router,
        "CreateDeployment",
        "POST",
        &format!("/restapis/{api}/deployments"),
        Some(json!({ "description": "the snapshot both stages serve" })),
        201,
    )
    .await;
    let deployment = member("CreateDeployment", &deployment, "id");
    for stage in [FLUSHED_STAGE, SPARED_STAGE] {
        apigw(
            router,
            "CreateStage",
            "POST",
            &format!("/restapis/{api}/stages"),
            Some(json!({ "stageName": stage, "deploymentId": deployment })),
            201,
        )
        .await;
    }

    wait_until_active(router).await;
    api
}

/// One row of the table: an identity presented at the identity source, on the method one of
/// the api's two authorizers decides.
struct Row {
    /// What the row is called in every assertion it makes.
    case: &'static str,
    /// The guarded path, which names the authorizer deciding it.
    path: &'static str,
    /// `Authorizer$type` of that authorizer, which the row reads back out of what the
    /// function published.
    kind: &'static str,
    /// The value the request carries at the identity source.
    token: &'static str,
}

/// The rows: three identities across both authorizer types, so the flush is asserted over a
/// SET of held entries rather than the one a single call fills.
const ROWS: &[Row] = &[
    Row {
        case: "the first identity on the TOKEN authorizer",
        path: "/token",
        kind: "TOKEN",
        token: "let-me-in",
    },
    Row {
        case: "a second identity on the TOKEN authorizer",
        path: "/token",
        kind: "TOKEN",
        token: "another-caller",
    },
    Row {
        case: "the first identity on the REQUEST authorizer",
        path: "/request",
        kind: "REQUEST",
        token: "let-me-in",
    },
    Row {
        case: "a third identity on the REQUEST authorizer",
        path: "/request",
        kind: "REQUEST",
        token: "a-third-token",
    },
];

/// The per-run value the answered call carries: the text member, and the same value read as
/// the NUMBER the other member rendered it as.
fn published(row: &Row, where_from: &str, answered: &Answered) -> (String, u64) {
    assert_eq!(
        answered.status, 200,
        "case {:?} ({where_from}): expected HTTP 200, got {} with {:?}",
        row.case,
        answered.status,
        answered.text()
    );
    let ran = answered.member("ran");
    assert!(
        !ran.is_empty(),
        "case {:?} ({where_from}): expected the authorizer's published value, got {:?}",
        row.case,
        answered.text()
    );
    let count = answered
        .json()
        .get("count")
        .and_then(Value::as_u64)
        .unwrap_or_else(|| {
            panic!(
                "case {:?} ({where_from}): expected the numeric context entry to render as a \
                 number, got {:?}",
                row.case,
                answered.text()
            )
        });
    assert_eq!(
        answered.member("kind"),
        row.kind,
        "case {:?} ({where_from}): expected the {} event, got {:?}",
        row.case,
        row.kind,
        answered.text()
    );
    assert_eq!(
        answered.member("principal"),
        format!("principal-{}", row.token),
        "case {:?} ({where_from}): expected the identity's own principal, got {:?}",
        row.case,
        answered.text()
    );
    (ran, count)
}

/// RULE: a flush of a stage's authorizer cache invalidates every result that stage holds —
/// each identity and each authorizer — and nothing another stage holds, and a result obtained
/// after it is held exactly as the flushed one was.
#[tokio::test]
async fn a_flush_reinvokes_every_held_identity_of_the_addressed_stage_alone() {
    let router = emulator();
    let api = deployed(&router).await;
    let api = api.as_str();

    // Each row's identity, on each stage: the first call invokes the authorizer and the next
    // one inside the TTL is decided by what the stage now holds, so it carries the very value
    // that invocation published.
    let mut held = Vec::new();
    for row in ROWS {
        let mut per_stage = Vec::new();
        for stage in [FLUSHED_STAGE, SPARED_STAGE] {
            let first = invoke_as(&router, api, stage, row.path, row.token).await;
            let (ran, count) = published(row, &format!("the first call on {stage}"), &first);
            let again = invoke_as(&router, api, stage, row.path, row.token).await;
            let (ran_again, count_again) =
                published(row, &format!("the same identity again on {stage}"), &again);
            assert_eq!(
                (ran_again.as_str(), count_again),
                (ran.as_str(), count),
                "case {:?}: inside the TTL the result {stage} holds must decide the call, \
                 expected {ran:?}/{count}, got {ran_again:?}/{count_again}",
                row.case
            );
            per_stage.push((stage, ran, count));
        }
        held.push(per_stage);
    }

    // Every pair of entries the two stages now hold is its own: a stage holds its own results,
    // so no two of the values above may coincide.
    let values: Vec<&String> = held
        .iter()
        .flatten()
        .map(|(_, ran, _)| ran)
        .collect();
    for (index, value) in values.iter().enumerate() {
        for other in values.iter().skip(index + 1) {
            assert_ne!(
                value, other,
                "case \"the entries the stages hold\": every identity of every authorizer of \
                 every stage earns its own invocation, got {value:?} twice"
            );
        }
    }

    let flushed = dispatch(
        &router,
        "DELETE",
        &format!("apigateway.{REGION}.amazonaws.com"),
        &format!("/restapis/{api}/stages/{FLUSHED_STAGE}/cache/authorizers"),
        &[("authorization", &authorization("apigateway"))],
        Bytes::new(),
    )
    .await;
    assert_eq!(
        flushed.status, 202,
        "case \"FlushStageAuthorizersCache\": expected HTTP 202, got {} with {:?}",
        flushed.status,
        flushed.text()
    );

    for (row, per_stage) in ROWS.iter().zip(&held) {
        for (stage, ran, count) in per_stage {
            let after = invoke_as(&router, api, stage, row.path, row.token).await;
            let (ran_after, count_after) =
                published(row, &format!("the call after the flush on {stage}"), &after);
            if *stage == FLUSHED_STAGE {
                // The window is nowhere near elapsed, and the authorizer ran again anyway:
                // that is what the flush did.
                assert_ne!(
                    (ran_after.as_str(), count_after),
                    (ran.as_str(), *count),
                    "case {:?}: after the flush the authorizer must be invoked again on \
                     {stage}, got the held {ran:?}/{count} back",
                    row.case
                );
                // And what it returned this time is held exactly as the flushed result was:
                // the flush invalidated the entries, it did not stop the stage caching.
                let reheld = invoke_as(&router, api, stage, row.path, row.token).await;
                let (ran_reheld, count_reheld) = published(
                    row,
                    &format!("the identity again after the flush on {stage}"),
                    &reheld,
                );
                assert_eq!(
                    (ran_reheld.as_str(), count_reheld),
                    (ran_after.as_str(), count_after),
                    "case {:?}: the result obtained after the flush must be held too, \
                     expected {ran_after:?}/{count_after}, got {ran_reheld:?}/{count_reheld}",
                    row.case
                );
            } else {
                // A flush addresses one stage: this one still holds what it held.
                assert_eq!(
                    (ran_after.as_str(), count_after),
                    (ran.as_str(), *count),
                    "case {:?}: the flush of {FLUSHED_STAGE} must leave what {stage} holds \
                     alone, expected {ran:?}/{count}, got {ran_after:?}/{count_after}",
                    row.case
                );
            }
        }
    }
}
