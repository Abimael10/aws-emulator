// SPDX-License-Identifier: CC0-1.0

//! The Agent Registry discovery routes on the ASSEMBLED emulator — every service mounted,
//! as `aws_emulator_router::build_router` assembles the one the binary serves.
//!
//! The sibling suite `agent_registry_signing_name_routing` pins the claim rule over the
//! two services that share the `agent-registry` signing name. This one pins the same rule
//! with EVERY other service mounted beside them: no other emulated service claims a
//! request signed for `agent-registry`, so each of the data plane's three routes is served
//! by the data plane and a request its endpoint binds no route for is that endpoint's own
//! modelled refusal — never the router's `404 UnknownService` (nobody claimed it) and never
//! its `500 AmbiguousService` (several claimants named an operation for it).
//!
//! The rows span the data plane's whole route set: the two root-level routes
//! (`POST /discoverable-records-batch`, `POST /discoverable-records-search`), the labelled
//! one whose first path segment the control plane publishes too
//! (`POST /registries/{registryId}/discoverable-records-list`), and a request that binds to
//! no route at all.

use std::sync::Arc;

use aws_emulator_router::{build_router, InMemoryEngine};
use bytes::Bytes;
use serde_json::{json, Value};

/// A SigV4 `Authorization` header whose credential scope names the signing name the two
/// Agent Registry services share. The signature is never checked: the router here runs
/// with request authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTAGENTREGISTRY/20260101/us-west-2/agent-registry/aws4_request, \
     SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000";

/// A registry id of `RegistryId`'s shape (`^[a-zA-Z0-9]{12,16}$`) and a record id of
/// `RegistryRecordId`'s (`^[a-zA-Z0-9]{12}$`): no registry is created here, so each read
/// answers the data plane's own `ResourceNotFoundException`, which is still the data plane
/// answering.
const REGISTRY_ID: &str = "aaaaaaaaaaaa";
const RECORD_ID: &str = "bbbbbbbbbbbb";

/// The router's own refusals, which no request below may be answered with.
const ROUTER_REFUSALS: [&str; 2] = ["UnknownService", "AmbiguousService"];

/// One answer from the assembled router: status, restJson1 error code (from the
/// `x-amzn-errortype` header, else the body's `__type`), content type and message.
async fn call(method: &str, path: &str, body: Value) -> (u16, String, String, String) {
    let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
    let mut router = build_router(engine);
    router.set_request_authentication(false);
    let payload = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    let request = http::Request::builder()
        .method(method)
        .uri(path)
        .header("host", "localhost:9297")
        .header("content-type", "application/json")
        .header("authorization", AUTHORIZATION)
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header = |name: &str| {
        response
            .headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string()
    };
    let content_type = header("content-type");
    let error_type = header("x-amzn-errortype");
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    let code = if error_type.is_empty() {
        parsed
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        error_type
    };
    let code = code.rsplit(['#', '/']).next().unwrap_or_default().to_string();
    let message = parsed
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (status, code, content_type, message)
}

#[tokio::test]
async fn every_discovery_route_is_served_by_agent_registry_on_the_assembled_emulator() {
    // (case, method, path, body, the modelled error the data plane answers for a registry
    // no store holds). Each row's answer must come from the data plane's own restJson1
    // envelope, which is what rules out both of the router's refusals.
    let rows: Vec<(String, &str, String, Value)> = vec![
        (
            "SearchDiscoverableRegistryRecords".to_string(),
            "POST",
            "/discoverable-records-search".to_string(),
            json!({ "searchQuery": "anything", "registryIds": [REGISTRY_ID] }),
        ),
        (
            "BatchGetDiscoverableRegistryRecord".to_string(),
            "POST",
            "/discoverable-records-batch".to_string(),
            json!({ "entries": [{ "registryId": REGISTRY_ID, "recordIds": [RECORD_ID] }] }),
        ),
        (
            "ListDiscoverableRegistryRecords".to_string(),
            "POST",
            format!("/registries/{REGISTRY_ID}/discoverable-records-list"),
            json!({}),
        ),
    ];

    for (case, method, path, body) in rows {
        let (status, code, content_type, message) = call(method, &path, body).await;
        assert!(
            !ROUTER_REFUSALS.contains(&code.as_str()),
            "case {case:?} ({method} {path}): the request must reach the Agent Registry \
             data plane, got {status} {code:?} {message:?}"
        );
        assert_eq!(
            (status, code.as_str()),
            (404, "ResourceNotFoundException"),
            "case {case:?}: with no registry in the store the data plane answers its own \
             modelled 404, got {status} {code:?} {message:?}"
        );
        assert!(
            content_type.starts_with("application/json"),
            "case {case:?}: restJson1 frames the answer as application/json, got \
             {content_type:?}"
        );
    }
}

#[tokio::test]
async fn a_request_binding_no_agent_registry_route_is_refused_by_that_endpoint() {
    // The rows differ in HOW the request names no route of either Agent Registry model: a
    // labelled path that bound to nothing, a method no route carries, and a path space
    // neither model publishes.
    let rows: [(&str, &str, &str); 3] = [
        (
            "a labelled path that bound to nothing",
            "POST",
            "/registries/discoverable-records-list",
        ),
        ("a method no route carries", "GET", "/discoverable-records-search"),
        ("a path space no model publishes", "POST", "/nothing-here"),
    ];

    for (case, method, path) in rows {
        let (status, code, content_type, message) = call(method, path, json!({})).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, "ValidationException", "Request invalid"),
            "case {case:?} ({method} {path}): expected the endpoint's own modelled 400 \
             with its fixed text, got {status} {code:?} {message:?}"
        );
        assert!(
            content_type.starts_with("application/json"),
            "case {case:?}: the refusal is framed in the resolved service's protocol, got \
             {content_type:?}"
        );
    }
}
