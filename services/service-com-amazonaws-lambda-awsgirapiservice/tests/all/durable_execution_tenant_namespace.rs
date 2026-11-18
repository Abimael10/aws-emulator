// SPDX-License-Identifier: CC0-1.0

//! The TENANT a durable execution belongs to, end to end over `Invoke`.
//!
//! RULE: a durable execution is the state of ONE tenant's work. "If you invoke a durable
//! function using a name that already exists with the same payload, Lambda returns the
//! existing execution instead of creating a duplicate. If the payload differs, Lambda returns
//! a DurableExecutionAlreadyStartedException error" (`Invoke$DurableExecutionName`) — and
//! being answered with an existing execution's ARN and its stored result is an answer only the
//! tenant that started it may be given. So a start under a name another tenant already holds
//! is the modelled `DurableExecutionAlreadyStartedException` (HTTP 409), carrying nothing of
//! that other tenant's execution, while a repeat of the SAME tenant is de-duplicated onto its
//! own execution and answered with it.
//!
//! The rows differ along what the rule generalises over: which tenant invokes (the one that
//! started the execution, another tenant, no tenant at all), whether the name is one already
//! held, and whether the payload is the same one.
//!
//! The test RUNS handlers, so it takes the one invocation stream the suites that run handlers
//! share ([`crate::invoke_request_id::one_invocation_stream_at_a_time`]): this emulator serves
//! a bounded number of runs at once, and the memory bound a run is stopped on is read from the
//! whole process.

use std::io::Write as _;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use base64::Engine as _;
use serde_json::{json, Value};
use service_com_amazonaws_lambda_awsgirapiservice::state::ComAmazonawsLambdaAwsgirapiserviceState;
use service_com_amazonaws_lambda_awsgirapiservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// Where `CreateFunction` is bound.
const FUNCTIONS_URI: &str = "/2015-03-31/functions";

/// The router picks a service by the SigV4 credential scope's service field. Nothing in it is
/// verified: this harness turns request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=emulator-test-key/20260101/us-east-1/lambda/aws4_request, \
     SignedHeaders=host, Signature=not-verified";

/// A well-formed execution role ARN, satisfying the model's `RoleArn` pattern.
const ROLE: &str = "arn:aws:iam::000000000000:role/service-role/lambda-exec";

/// The function every row invokes.
const FUNCTION: &str = "durable-tenant-namespace";

/// The execution name the rows contend for, and one no row has taken.
const SHARED_NAME: &str = "shared-execution";
const OWN_NAME: &str = "own-execution";

/// The tenants the rows invoke as. Both satisfy `TenantId`'s pattern
/// (`^[a-zA-Z0-9\._:\/=+\-@ ]+$`, length 1..256).
const TENANT_ALPHA: &str = "tenant-alpha";
const TENANT_BETA: &str = "tenant-beta";

/// The error code a start under a name already held is refused with, and its status — from
/// the model (`Invoke` errors: `DurableExecutionAlreadyStartedException`, `httpError` 409).
const ALREADY_STARTED: &str = "DurableExecutionAlreadyStartedException";
const CONFLICT_STATUS: u16 = 409;

/// The handler every row runs: it answers the mark the event carries, so one tenant's stored
/// result is told from another's.
const HANDLER_SOURCE: &str = r#"
def handler(event, context):
    return {"mark": event.get("mark")}
"#;

/// What a dispatched request answered.
struct Answer {
    status: u16,
    headers: http::HeaderMap,
    body: bytes::Bytes,
}

impl Answer {
    /// The body text, which is the handler's payload for a served invocation and the error
    /// envelope for a refused one.
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The wire error code of a refusal (`__type` of the restJson1 envelope), or `""`.
    fn error_code(&self) -> String {
        let parsed: Value = serde_json::from_slice(&self.body).unwrap_or(Value::Null);
        parsed
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// One response header, or `None`.
    fn header(&self, name: &str) -> Option<String> {
        self.headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    }
}

/// One service mounted on its own engine, reached the way the production router reaches it.
fn router_on(engine: &Arc<engine::InMemoryEngine>) -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::clone(engine)));
    router
}

/// A .zip deployment package holding [`HANDLER_SOURCE`] as `index.py`.
fn package() -> Vec<u8> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, '_, ()> = zip::write::FileOptions::default();
        writer
            .start_file("index.py", options)
            .expect("the entry starts");
        writer
            .write_all(HANDLER_SOURCE.as_bytes())
            .expect("the entry's bytes");
        writer.finish().expect("a finished archive");
    }
    buffer.into_inner()
}

/// Dispatch one request, with the headers `headers` names.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: &Value,
    headers: &[(&str, &str)],
) -> Answer {
    let payload = if body.is_null() {
        Vec::new()
    } else {
        serde_json::to_vec(body).expect("the test request body must serialize")
    };
    let mut builder = http::Request::builder()
        .method(method)
        .uri(format!("http://lambda.us-east-1.amazonaws.com{uri}"))
        .header("host", "lambda.us-east-1.amazonaws.com")
        .header("authorization", AUTHORIZATION)
        .header("content-type", "application/json");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = builder
        .body(bytes::Bytes::from(payload))
        .expect("the test request must build");
    let response = router.dispatch(request).await;
    let (parts, body) = response.into_parts();
    Answer {
        status: parts.status.as_u16(),
        headers: parts.headers,
        body,
    }
}

/// `CreateFunction`, which must succeed, then the activation the clock owes made due — so
/// nothing here waits for a span the service already owes the function.
async fn create_function(engine: &Arc<engine::InMemoryEngine>, router: &MultiServiceRouter) {
    let answer = call(
        router,
        "POST",
        FUNCTIONS_URI,
        &json!({
            "FunctionName": FUNCTION,
            "Role": ROLE,
            "Runtime": "python3.12",
            "Handler": "index.handler",
            "Timeout": 30,
            "Code": {
                "ZipFile": base64::engine::general_purpose::STANDARD.encode(package()),
            },
        }),
        &[],
    )
    .await;
    assert_eq!(
        answer.status,
        201,
        "CreateFunction must succeed, got {} {}",
        answer.status,
        answer.text()
    );
    let state = engine.state::<ComAmazonawsLambdaAwsgirapiserviceState>();
    let mut store = state.lock();
    let past = SystemTime::now() - Duration::from_secs(1);
    for function in store.functions.values_mut() {
        function.active_due = past;
    }
}

/// One `Invoke` naming a durable execution, as `tenant` when it names one.
async fn invoke(
    router: &MultiServiceRouter,
    name: &str,
    tenant: Option<&str>,
    mark: &str,
) -> Answer {
    let mut headers: Vec<(&str, &str)> = vec![("x-amz-durable-execution-name", name)];
    if let Some(tenant) = tenant {
        headers.push(("x-amz-tenant-id", tenant));
    }
    call(
        router,
        "POST",
        &format!("{FUNCTIONS_URI}/{FUNCTION}/invocations"),
        &json!({ "mark": mark }),
        &headers,
    )
    .await
}

#[tokio::test]
async fn a_durable_execution_name_is_scoped_to_the_tenant_that_started_it() {
    let _stream = crate::invoke_request_id::one_invocation_stream_at_a_time().await;
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    create_function(&engine, &router).await;

    // The tenant that starts the execution is answered its own ARN and its own result.
    let started = invoke(&router, SHARED_NAME, Some(TENANT_ALPHA), TENANT_ALPHA).await;
    assert_eq!(
        started.status,
        200,
        "case \"the tenant that starts it\": expected HTTP 200, got {} {}",
        started.status,
        started.text()
    );
    let alpha_arn = started
        .header("x-amz-durable-execution-arn")
        .unwrap_or_else(|| {
            panic!(
                "case \"the tenant that starts it\": expected a DurableExecutionArn, got {}",
                started.text()
            )
        });
    let alpha_result = started.text();
    assert!(
        alpha_result.contains(TENANT_ALPHA),
        "case \"the tenant that starts it\": expected the handler's own answer, got {alpha_result}"
    );

    // ANOTHER tenant naming the same execution with the same payload is refused the name, and
    // is told nothing of the execution the first tenant holds.
    let refused = invoke(&router, SHARED_NAME, Some(TENANT_BETA), TENANT_ALPHA).await;
    assert_eq!(
        (refused.status, refused.error_code().as_str()),
        (CONFLICT_STATUS, ALREADY_STARTED),
        "case \"another tenant, the same name\": expected {CONFLICT_STATUS} {ALREADY_STARTED}, \
         got {} {}",
        refused.status,
        refused.text()
    );
    assert_eq!(
        refused.header("x-amz-durable-execution-arn"),
        None,
        "case \"another tenant, the same name\": expected no ARN of the other tenant's \
         execution, got {}",
        refused.text()
    );
    assert!(
        !refused.text().contains(&alpha_arn),
        "case \"another tenant, the same name\": expected nothing of {alpha_arn} in the \
         refusal, got {}",
        refused.text()
    );

    // And so is an invocation that names NO tenant: the untenanted namespace is its own.
    let untenanted = invoke(&router, SHARED_NAME, None, TENANT_ALPHA).await;
    assert_eq!(
        (untenanted.status, untenanted.error_code().as_str()),
        (CONFLICT_STATUS, ALREADY_STARTED),
        "case \"no tenant, the same name\": expected {CONFLICT_STATUS} {ALREADY_STARTED}, got \
         {} {}",
        untenanted.status,
        untenanted.text()
    );

    // The tenant that started it is still de-duplicated onto its OWN execution: the same ARN
    // and the same answer, with no second run.
    let repeated = invoke(&router, SHARED_NAME, Some(TENANT_ALPHA), TENANT_ALPHA).await;
    assert_eq!(
        repeated.status,
        200,
        "case \"the same tenant again\": expected HTTP 200, got {} {}",
        repeated.status,
        repeated.text()
    );
    assert_eq!(
        repeated.header("x-amz-durable-execution-arn"),
        Some(alpha_arn.clone()),
        "case \"the same tenant again\": expected the execution it already started, got {}",
        repeated.text()
    );
    assert_eq!(
        repeated.text(),
        alpha_result,
        "case \"the same tenant again\": expected the answer that execution already came to"
    );

    // A tenant refused one name is not shut out of the function: a name nobody holds starts
    // its own execution, under an ARN of its own.
    let own = invoke(&router, OWN_NAME, Some(TENANT_BETA), TENANT_BETA).await;
    assert_eq!(
        own.status,
        200,
        "case \"another tenant, a name nobody holds\": expected HTTP 200, got {} {}",
        own.status,
        own.text()
    );
    let beta_arn = own.header("x-amz-durable-execution-arn").unwrap_or_else(|| {
        panic!(
            "case \"another tenant, a name nobody holds\": expected a DurableExecutionArn, got {}",
            own.text()
        )
    });
    assert_ne!(
        beta_arn, alpha_arn,
        "case \"another tenant, a name nobody holds\": expected an execution of its own"
    );
    assert!(
        own.text().contains(TENANT_BETA),
        "case \"another tenant, a name nobody holds\": expected its own answer, got {}",
        own.text()
    );

    // The payload rule is unchanged for the tenant that owns the name: a repeat whose payload
    // differs is refused rather than de-duplicated.
    let differing = invoke(&router, SHARED_NAME, Some(TENANT_ALPHA), "another-payload").await;
    assert_eq!(
        (differing.status, differing.error_code().as_str()),
        (CONFLICT_STATUS, ALREADY_STARTED),
        "case \"the same tenant, a differing payload\": expected {CONFLICT_STATUS} \
         {ALREADY_STARTED}, got {} {}",
        differing.status,
        differing.text()
    );
}
