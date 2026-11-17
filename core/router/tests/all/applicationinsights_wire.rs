// SPDX-License-Identifier: CC0-1.0

//! The wire every Application Insights suite in this test crate drives, written
//! ONCE: the signed request builder, the answer it parses, and the two calls both
//! suites make — one Application Insights operation, and the Resource Groups
//! `CreateGroup` that gives an application a group to be created over.
//!
//! A suite that writes its own drifts from the others: a credential scope that
//! names a different service, a body read under a different casing, or a group
//! created in a different form changes what the suite is actually asserting. The
//! service pair is the same in every one of them, so the plumbing lives here and
//! each suite states only its own rows.

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// Keep this at or above `boot_serve.rs`'s stack: assembling the whole router is
/// what every suite below does first.
pub(crate) const BOOT_STACK_SIZE: usize = 64 * 1024 * 1024;

/// The `X-Amz-Target` prefix this service's awsJson1_1 operations are named by.
const TARGET_PREFIX: &str = "EC2WindowsBarleyService";

/// What one call answered: the HTTP status and the parsed JSON body
/// (`Value::Null` for a body that is empty or not JSON).
#[derive(Debug)]
pub(crate) struct Answer {
    pub(crate) status: u16,
    pub(crate) body: Value,
}

impl Answer {
    /// The awsJson error code, without its namespace.
    pub(crate) fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or("")
            .rsplit(['#', ':'])
            .next()
            .unwrap_or("")
            .to_string()
    }

    /// The error message, whichever casing the envelope carries it under.
    pub(crate) fn error_message(&self) -> String {
        self.body
            .get("Message")
            .or_else(|| self.body.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string()
    }
}

/// Dispatch one request addressed to `signing_name` in `region`, the way a signed
/// SDK call is: the credential scope is what names the service and the region.
pub(crate) fn send(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    signing_name: &str,
    region: &str,
    path: &str,
    target: Option<&str>,
    body: Value,
) -> Answer {
    let host = format!("{signing_name}.{region}.amazonaws.com");
    let content_type = if target.is_some() {
        "application/x-amz-json-1.1"
    } else {
        "application/json"
    };
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", content_type)
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTEST/20260101/{region}/{signing_name}/\
                 aws4_request, SignedHeaders=host, Signature=not-verified"
            ),
        );
    if let Some(target) = target {
        request = request.header("x-amz-target", format!("{TARGET_PREFIX}.{target}"));
    }
    let request = request
        .body(bytes::Bytes::from(
            serde_json::to_vec(&body).expect("serialize body"),
        ))
        .expect("a well-formed request");
    let response = runtime.block_on(router.dispatch(request));
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Answer { status, body }
}

/// One Application Insights operation, over its awsJson1_1 wire.
pub(crate) fn application_insights(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    operation: &str,
    body: Value,
) -> Answer {
    send(
        router,
        runtime,
        "applicationinsights",
        region,
        "/",
        Some(operation),
        body,
    )
}

/// One Resource Groups operation, over its REST wire.
pub(crate) fn resource_groups(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    path: &str,
    body: Value,
) -> Answer {
    send(router, runtime, "resource-groups", region, path, None, body)
}

/// `POST /groups` — Resource Groups' own `CreateGroup`, in `region`, with the
/// service configuration the caller needs (none, for a group nothing is grouped
/// into by hand).
pub(crate) fn create_group(
    router: &MultiServiceRouter,
    runtime: &tokio::runtime::Runtime,
    region: &str,
    name: &str,
    configuration: Option<Value>,
) {
    let mut body = json!({ "Name": name });
    if let Some(configuration) = configuration {
        body.as_object_mut()
            .expect("the request is a JSON object")
            .insert("Configuration".to_string(), configuration);
    }
    let answer = resource_groups(router, runtime, region, "/groups", body);
    assert_eq!(
        answer.status, 200,
        "setup: CreateGroup({name:?}) in {region} must succeed, got {} / {:?}",
        answer.status, answer.body
    );
}
