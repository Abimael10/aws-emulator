// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for what a function's TENANT ISOLATION configuration requires of an
//! invocation of it.
//!
//! RULE: `TenancyConfig` "specifies the tenant isolation mode configuration for a Lambda
//! function", and the mode it carries is required of it
//! (`smithy_query(query="shape", shape="TenancyConfig")`: `TenantIsolationMode` is
//! `@required`, "Tenant isolation mode allows for invocation to be sent to a corresponding
//! execution environment dedicated to a specific tenant ID") — so a function configured that
//! way has no un-tenanted execution environment to be invoked in, and an invocation of it
//! that names no `TenantId` is refused: nothing runs, and no version is reported as having
//! executed. The requirement is CONDITIONAL on the function's stored configuration, which no
//! constraint of an input shape can express, so Lambda words it itself — the form its other
//! conditionally required members are refused in — carried by the modelled
//! `InvalidParameterValueException` (HTTP 400). An invocation that names a tenant
//! is served, and a function configured for no tenancy at all is invoked with or without one
//! exactly as before.
//!
//! The rows differ along what the rule generalises over: whether the function carries the
//! configuration, whether the call names a tenant and what form that tenant id takes (a
//! plain name, and one carrying the punctuation the member's own pattern admits), and WHICH
//! invoking operation makes the call — `Invoke` and `InvokeWithResponseStream` both bind
//! `TenantId`, so both require it.
//!
//! Every row drives a real HTTP request through the production `MultiServiceRouter` against
//! a function created through `CreateFunction`, whose stored configuration is read back
//! through `GetFunctionConfiguration` — so the gate is made against what the create
//! persisted rather than against anything the test asserted on its own.

use std::sync::Arc;

use base64::Engine as _;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::event_source_mappings::await_function_active;
use crate::function_lifecycle::{
    create_function, json_call, make_activation_due, router_on, send, Answer, ROLE,
};
use crate::invocation_log_delivery::{function_memory_size_mb, function_timeout_seconds};

/// The only `TenantIsolationMode` the model declares
/// (`smithy_query(query="shape", shape="TenantIsolationMode")`).
const PER_TENANT: &str = "PER_TENANT";

/// The runtime and handler of the package every function here is created with.
const RUNTIME: &str = "python3.12";
/// See [`RUNTIME`].
const HANDLER: &str = "lambda_function.lambda_handler";

/// A handler that answers a marker, so a served invocation is told apart from a refused one
/// by what the function itself returned.
const SOURCE: &str = "def lambda_handler(event, context):\n    \
                          return {'ran': True}\n";

/// The modelled error a refused invocation carries
/// (`smithy_query(query="shape", shape="InvalidParameterValueException")`: `httpError 400`).
const INVALID_PARAMETER: &str = "InvalidParameterValueException";

/// The header `TenantId` is bound to on both invoking operations
/// (`@httpHeader X-Amz-Tenant-Id`).
const TENANT_HEADER: &str = "x-amz-tenant-id";

/// The clause that refusal carries: fixed text naming the member the invocation left out and
/// the condition that requires it, in the form this service words every other conditionally
/// required member's refusal in ("Runtime is required if the deployment package is a .zip
/// file archive").
fn untenanted_refusal() -> String {
    "TenantId is required if the function is configured for tenant isolation".to_string()
}

/// The `message` of a wire error, under either casing the JSON envelopes use.
fn message(answer: &Answer) -> String {
    answer
        .string("message")
        .or_else(|| answer.string("Message"))
        .unwrap_or_default()
}

/// Which invoking operation a row calls, each at the path the model binds it to.
#[derive(Clone, Copy, Debug)]
enum Call {
    /// `POST /2015-03-31/functions/{FunctionName}/invocations`.
    Invoke,
    /// `POST /2021-11-15/functions/{FunctionName}/response-streaming-invocations`.
    Streaming,
}

impl Call {
    fn uri(self, function: &str) -> String {
        match self {
            Self::Invoke => format!("/2015-03-31/functions/{function}/invocations"),
            Self::Streaming => {
                format!("/2021-11-15/functions/{function}/response-streaming-invocations")
            }
        }
    }
}

/// Create the function `name`, carrying `tenancy_config` when it is configured for tenant
/// isolation, and wait until it reports `Active`.
///
/// The budgets are the generous ones this target's invoking suites share
/// (`invocation_log_delivery`), so a handler's run is never cut off by how fast the host
/// happens to be.
async fn given_function(
    router: &MultiServiceRouter,
    engine: &Arc<engine::InMemoryEngine>,
    case: &str,
    name: &str,
    tenancy_config: Option<&str>,
) {
    let mut body = json!({
        "FunctionName": name,
        "Role": ROLE,
        "Runtime": RUNTIME,
        "Handler": HANDLER,
        "Timeout": function_timeout_seconds(),
        "MemorySize": function_memory_size_mb(),
        "Code": {
            "ZipFile": base64::engine::general_purpose::STANDARD
                .encode(crate::synchronous_invoke_run::package(SOURCE)),
        },
    });
    if let Some(mode) = tenancy_config {
        body["TenancyConfig"] = json!({ "TenantIsolationMode": mode });
    }
    let created = create_function(router, &body).await;
    assert_eq!(
        created.status,
        201,
        "case {case:?}: expected 201 from CreateFunction, got {} {}",
        created.status,
        String::from_utf8_lossy(&created.body)
    );
    make_activation_due(engine);
    await_function_active(router, case, name).await;

    // The gate below is made against what the create PERSISTED, so the configuration is
    // read back here rather than assumed.
    let read = json_call(
        router,
        "GET",
        &format!("/2015-03-31/functions/{name}/configuration"),
        &Value::Null,
    )
    .await;
    let reported = read.json()["TenancyConfig"]["TenantIsolationMode"]
        .as_str()
        .map(str::to_string);
    assert_eq!(
        reported.as_deref(),
        tenancy_config,
        "case {case:?}: expected the stored configuration to report {tenancy_config:?}, got {}",
        String::from_utf8_lossy(&read.body)
    );
}

/// One row: which operation invokes, the tenant the call names, and whether it is refused.
struct Row {
    case: &'static str,
    call: Call,
    /// `true` for the function configured for per-tenant isolation.
    isolated: bool,
    tenant: Option<&'static str>,
    refused: bool,
}

fn rows() -> Vec<Row> {
    vec![
        Row {
            case: "an isolated function invoked with no tenant at all",
            call: Call::Invoke,
            isolated: true,
            tenant: None,
            refused: true,
        },
        Row {
            case: "the same function invoked for a tenant",
            call: Call::Invoke,
            isolated: true,
            tenant: Some("tenant-a"),
            refused: false,
        },
        Row {
            // The member's own pattern admits these characters
            // (`smithy_query(query="shape", shape="TenantId")`), so a tenant id of this form
            // names a tenant as much as a plain name does.
            case: "the same function invoked for a tenant id carrying punctuation",
            call: Call::Invoke,
            isolated: true,
            tenant: Some("acct:1234/unit=b_2@x"),
            refused: false,
        },
        Row {
            case: "an isolated function invoked for no tenant on the streaming path",
            call: Call::Streaming,
            isolated: true,
            tenant: None,
            refused: true,
        },
        Row {
            case: "a function configured for no tenancy, invoked with no tenant",
            call: Call::Invoke,
            isolated: false,
            tenant: None,
            refused: false,
        },
        Row {
            case: "a function configured for no tenancy, invoked with a tenant anyway",
            call: Call::Invoke,
            isolated: false,
            tenant: Some("tenant-a"),
            refused: false,
        },
    ]
}

#[tokio::test]
async fn an_invocation_of_a_tenant_isolated_function_has_to_name_its_tenant() {
    let engine = Arc::new(engine::InMemoryEngine::new());
    let router = router_on(&engine);
    let isolated = "tenant-isolated-fn";
    let plain = "tenant-free-fn";
    given_function(&router, &engine, "the isolated function", isolated, Some(PER_TENANT)).await;
    given_function(&router, &engine, "the plain function", plain, None).await;

    for row in rows() {
        let case = row.case;
        let function = if row.isolated { isolated } else { plain };
        let headers: Vec<(&str, String)> = row
            .tenant
            .map(|tenant| vec![(TENANT_HEADER, tenant.to_string())])
            .unwrap_or_default();
        let answer = send(
            &router,
            "POST",
            &row.call.uri(function),
            "application/json",
            b"{}".to_vec(),
            &headers,
        )
        .await;

        if row.refused {
            assert_eq!(
                answer.status,
                400,
                "case {case:?}: expected HTTP 400, got {} {}",
                answer.status,
                String::from_utf8_lossy(&answer.body)
            );
            assert_eq!(
                answer.error_code(),
                INVALID_PARAMETER,
                "case {case:?}: expected {INVALID_PARAMETER}, got {}",
                answer.error_code()
            );
            let expected = untenanted_refusal();
            assert_eq!(
                message(&answer),
                expected,
                "case {case:?}: expected {expected:?}, got {:?}",
                message(&answer)
            );
            // Nothing executed: a refused invocation reports no version as having run.
            assert_eq!(
                answer.header("x-amz-executed-version"),
                None,
                "case {case:?}: a refused invocation must report no executed version, got {:?}",
                answer.header("x-amz-executed-version")
            );
            continue;
        }

        assert_eq!(
            answer.status,
            200,
            "case {case:?}: expected HTTP 200, got {} {}",
            answer.status,
            String::from_utf8_lossy(&answer.body)
        );
        let body = String::from_utf8_lossy(&answer.body).to_string();
        assert!(
            body.contains("\"ran\""),
            "case {case:?}: expected the handler's own answer, got {body:?}"
        );
    }
}
