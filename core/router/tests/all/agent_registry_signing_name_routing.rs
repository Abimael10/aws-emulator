// SPDX-License-Identifier: CC0-1.0

//! Cross-service routing for the `agent-registry` SigV4 signing name.
//!
//! The Agent Registry CONTROL plane (`AgentRegistryControl`, endpointPrefix
//! `agent-registry-control`) and the Agent Registry DATA plane (`AgentRegistry`,
//! endpointPrefix `agent-registry`) SHARE one SigV4 signing name, `agent-registry`, and
//! one `arnNamespace`, while serving two different endpoints. The router resolves a
//! request's routing key from the SigV4 credential scope first, so both services see the
//! key `agent-registry`.
//!
//! The rule these suites pin: a request is served by the ENDPOINT its routing key names,
//! and within that endpoint by the plane whose own model binds a ROUTE (an HTTP method AND
//! a URI) for it. `agent-registry` is the data plane's `endpointPrefix` as well as the
//! shared signing name, so that key names the DATA plane's endpoint: the data plane claims
//! every request carrying it, the control plane claims the subset its own model binds a
//! route for, and where both claim the router keeps the one that names an operation — the
//! two models bind no identical method + URI pattern, so that is exactly one of them. A
//! request the data plane's endpoint is addressed with that NEITHER model binds a route for
//! is the data plane's own `400 ValidationException`, in its own `restJson1` envelope,
//! rather than the router's cross-protocol `404 UnknownService` (a code no model of either
//! service declares, framed as `application/x-amz-json-1.1`).
//!
//! Rows therefore span the dimensions that rule generalises over: which endpoint owns the
//! route, the method (`/registries` is bound by one model for POST and by neither for
//! GET), the depth at which the two models' URIs diverge, the greedy `{resourceArn+}`
//! label, a route neither model binds, and whether the target is named by a credential
//! scope or — on an unsigned request — by the endpoint host.
//!
//! One asymmetry the rows pin deliberately: an unbound path SIGNED for `agent-registry`
//! reaches the data plane's endpoint and is answered by its modelled refusal, while the
//! same unbound path addressed UNSIGNED to the control host resolves the key
//! `agent-registry-control`, which names an endpoint that binds no route for it and whose
//! model the data plane does not serve — so nobody claims it and the router answers its own
//! `404 UnknownService`. That follows from the key naming the endpoint, which is what the
//! shared signing name leaves as the only discriminator; it is not a defect.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::{MultiServiceRouter, Service};

/// A SigV4 `Authorization` header whose credential scope names the signing name both
/// services share. The signature is never checked: the routers here run with request
/// authentication off.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTAGENTREGISTRY/20260101/us-west-2/agent-registry/aws4_request, \
     SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000";

/// Which services claim one request, as a row states it. CLAIMING is not serving: a
/// request BOTH claim is served by the one whose model names an operation for it, which
/// [`no_agent_registry_request_is_answered_by_the_routers_refusals`] pins end to end.
const CLAIMED_BY_CONTROL: &str = "the control plane alone";
const CLAIMED_BY_DATA: &str = "the data plane alone";
const CLAIMED_BY_BOTH: &str = "both (the router serves the one that names an operation)";
const CLAIMED_BY_NEITHER: &str = "neither (the router answers 404 UnknownService)";

/// The host that names the control plane's endpoint, the one that names the data plane's,
/// and a bare emulator endpoint that names neither.
const CONTROL_HOST: &str = "agent-registry-control.us-west-2.amazonaws.com";
const DATA_HOST: &str = "agent-registry.us-west-2.amazonaws.com";
const BARE_HOST: &str = "localhost:9297";

/// A registry id of `RegistryId`'s shape (`^[a-zA-Z0-9]{12,16}$`) and a record id of
/// `RegistryRecordId`'s (`^[a-zA-Z0-9]{12}$`), for the path labels the rows address.
const REGISTRY_ID: &str = "aaaaaaaaaaaa";
const RECORD_ID: &str = "bbbbbbbbbbbb";

/// The modelled client error, and its fixed message, that the data plane's endpoint
/// refuses a request naming none of its routes with.
const VALIDATION_EXCEPTION: &str = "ValidationException";
const REQUEST_INVALID: &str = "Request invalid";

type Control =
    service_com_amazonaws_agentregistrycontrol_agentregistrycontrol::EmulatedService<
        engine::InMemoryEngine,
    >;
type Data = service_com_amazonaws_agentregistry_agentregistry::EmulatedService<
    engine::InMemoryEngine,
>;

/// The two services that share the `agent-registry` signing name, over one shared engine,
/// exactly as `router/src/lib.rs` mounts them.
fn both_services() -> (Control, Data) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    (Control::new(Arc::clone(&engine)), Data::new(engine))
}

/// A router with both of them mounted, so a request that two services claim really is
/// resolved by the router's own rule.
fn router_with_both_services() -> MultiServiceRouter {
    let (control, data) = both_services();
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(control);
    router.register_service(data);
    router
}

/// The request parts the router hands to `Service::can_handle`. `signed` chooses whether
/// the request carries a credential scope at all: an unsigned one is routed by its host.
fn parts(method: &str, host: &str, uri: &str, signed: bool) -> http::request::Parts {
    let mut builder = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/json");
    if signed {
        builder = builder.header("authorization", AUTHORIZATION);
    }
    builder
        .body(Bytes::new())
        .expect("the test request is well formed")
        .into_parts()
        .0
}

/// Which of the two services claim a request, as one of the four `CLAIMED_BY_*` outcomes.
fn claimants(control: &Control, data: &Data, parts: &http::request::Parts) -> &'static str {
    match (control.can_handle(parts), data.can_handle(parts)) {
        (true, false) => CLAIMED_BY_CONTROL,
        (false, true) => CLAIMED_BY_DATA,
        (true, true) => CLAIMED_BY_BOTH,
        (false, false) => CLAIMED_BY_NEITHER,
    }
}

/// One answer from the router: the HTTP status, the restJson1 error code (the empty
/// string on a response that carries none, read from both the body's `__type` and the
/// `x-amzn-errortype` header the protocol frames an error with), the content type and the
/// message the body carries.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Value,
) -> (u16, String, String, String) {
    let payload = if body.is_null() {
        String::new()
    } else {
        body.to_string()
    };
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", BARE_HOST)
        .header("content-type", "application/json")
        .header("authorization", AUTHORIZATION)
        .body(Bytes::from(payload))
        .expect("the test request is well formed");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let content_type = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let header_code = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let parsed: Value = if response.body().is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(response.body()).unwrap_or(Value::Null)
    };
    let code = if header_code.is_empty() {
        parsed
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        header_code
    };
    let code = code.rsplit(['#', '/']).next().unwrap_or_default().to_string();
    let message = parsed
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    (status, code, content_type, message)
}

/// The ARN of a registry, for the greedy `{resourceArn+}` label the tagging routes bind.
fn registry_arn() -> String {
    format!("arn:aws:agent-registry:us-west-2:000000000000:registry/{REGISTRY_ID}")
}

#[test]
fn each_agent_registry_request_is_claimed_by_the_endpoint_its_key_names() {
    let (control, data) = both_services();

    // Every route below comes from the two models' `smithy.api#http` bindings, as each
    // crate's `OPERATIONS.md` lists them (`smithy_query(query="digest", …)` on each
    // model). The rows are (case, method, host, uri, who claims it).
    let tags_path = format!("/tags/{}", registry_arn());
    let rows: Vec<(String, &str, &str, String, &str)> = vec![
        // The control plane's own routes, on hosts that name it, name its sibling, and
        // name nothing at all: on a SIGNED request the credential scope resolves the key,
        // so the host cannot change the answer. The key names the data plane's endpoint,
        // so the data plane claims these too and the route decides which one serves them.
        ("control CreateRegistry".to_string(), "POST", CONTROL_HOST, "/registries".to_string(), CLAIMED_BY_BOTH),
        ("control CreateRegistry on the sibling's host".to_string(), "POST", DATA_HOST, "/registries".to_string(), CLAIMED_BY_BOTH),
        ("control CreateRegistry on a bare emulator endpoint".to_string(), "POST", BARE_HOST, "/registries".to_string(), CLAIMED_BY_BOTH),
        ("control GetRegistry".to_string(), "GET", BARE_HOST, format!("/registries/{REGISTRY_ID}"), CLAIMED_BY_BOTH),
        ("control UpdateRegistry".to_string(), "PATCH", BARE_HOST, format!("/registries/{REGISTRY_ID}"), CLAIMED_BY_BOTH),
        ("control DeleteRegistry".to_string(), "DELETE", BARE_HOST, format!("/registries/{REGISTRY_ID}"), CLAIMED_BY_BOTH),
        ("control ListRegistries".to_string(), "POST", BARE_HOST, "/registries-list".to_string(), CLAIMED_BY_BOTH),
        ("control CreateRegistryRecord".to_string(), "POST", BARE_HOST, format!("/registries/{REGISTRY_ID}/records"), CLAIMED_BY_BOTH),
        ("control ListRegistryRecords".to_string(), "POST", BARE_HOST, format!("/registries/{REGISTRY_ID}/records-list"), CLAIMED_BY_BOTH),
        ("control GetRegistryRecord".to_string(), "GET", BARE_HOST, format!("/registries/{REGISTRY_ID}/records/{RECORD_ID}"), CLAIMED_BY_BOTH),
        ("control UpdateRegistryRecord".to_string(), "PATCH", BARE_HOST, format!("/registries/{REGISTRY_ID}/records/{RECORD_ID}"), CLAIMED_BY_BOTH),
        ("control UpdateRegistryRecordStatus".to_string(), "PATCH", BARE_HOST, format!("/registries/{REGISTRY_ID}/records/{RECORD_ID}/status"), CLAIMED_BY_BOTH),
        ("control SubmitRegistryRecordForApproval".to_string(), "POST", BARE_HOST, format!("/registries/{REGISTRY_ID}/records/{RECORD_ID}/submit-for-approval"), CLAIMED_BY_BOTH),
        // The greedy `{resourceArn+}` label: one path space, three methods, all the
        // control plane's.
        ("control TagResource".to_string(), "POST", BARE_HOST, tags_path.clone(), CLAIMED_BY_BOTH),
        ("control ListTagsForResource".to_string(), "GET", BARE_HOST, tags_path.clone(), CLAIMED_BY_BOTH),
        ("control UntagResource".to_string(), "DELETE", BARE_HOST, format!("{tags_path}?tagKeys=team"), CLAIMED_BY_BOTH),
        // The data plane's own routes, including the one whose FIRST path segment the
        // control plane publishes too. The control plane binds none of them, so it claims
        // none of them.
        ("data BatchGetDiscoverableRegistryRecord".to_string(), "POST", BARE_HOST, "/discoverable-records-batch".to_string(), CLAIMED_BY_DATA),
        ("data SearchDiscoverableRegistryRecords".to_string(), "POST", BARE_HOST, "/discoverable-records-search".to_string(), CLAIMED_BY_DATA),
        ("data ListDiscoverableRegistryRecords".to_string(), "POST", BARE_HOST, format!("/registries/{REGISTRY_ID}/discoverable-records-list"), CLAIMED_BY_DATA),
        ("data ListDiscoverableRegistryRecords on the sibling's host".to_string(), "POST", CONTROL_HOST, format!("/registries/{REGISTRY_ID}/discoverable-records-list"), CLAIMED_BY_DATA),
        // Routes NEITHER model binds: a method no model binds on a path one of them owns,
        // a path depth neither reaches, and a path space neither publishes. The key names
        // the data plane's endpoint, so its endpoint is what refuses them — and nothing is
        // claimed by two services that both name an operation for it.
        ("no model binds GET /registries".to_string(), "GET", BARE_HOST, "/registries".to_string(), CLAIMED_BY_DATA),
        ("no model binds POST on a registry".to_string(), "POST", BARE_HOST, format!("/registries/{REGISTRY_ID}"), CLAIMED_BY_DATA),
        ("no model binds GET on the search route".to_string(), "GET", BARE_HOST, "/discoverable-records-search".to_string(), CLAIMED_BY_DATA),
        ("no model publishes this path space".to_string(), "POST", BARE_HOST, "/nothing-here".to_string(), CLAIMED_BY_DATA),
    ];

    for (case, method, host, uri, expected) in rows {
        let claimed = claimants(&control, &data, &parts(method, host, &uri, true));
        assert_eq!(
            claimed, expected,
            "case {case:?} ({method} {uri} on {host}): expected it to be claimed by \
             {expected}, got {claimed:?}"
        );
    }
}

#[test]
fn an_unsigned_request_is_claimed_by_the_endpoint_its_host_names() {
    let (control, data) = both_services();

    // With no credential scope the routing key comes from the `Host` label, which is each
    // service's `endpointPrefix` — the two differ (`agent-registry-control` names only the
    // control plane; `agent-registry` names the data plane's endpoint, which the control
    // plane's traffic signs for, so on that key the route decides). Rows span both hosts
    // against both planes' routes.
    let rows: Vec<(String, &str, &str, String, &str)> = vec![
        ("control route on the control host".to_string(), "POST", CONTROL_HOST, "/registries".to_string(), CLAIMED_BY_CONTROL),
        ("control route on the shared host".to_string(), "POST", DATA_HOST, "/registries".to_string(), CLAIMED_BY_BOTH),
        ("data route on the shared host".to_string(), "POST", DATA_HOST, "/discoverable-records-search".to_string(), CLAIMED_BY_DATA),
        ("data route on the control host".to_string(), "POST", CONTROL_HOST, "/discoverable-records-search".to_string(), CLAIMED_BY_NEITHER),
        ("shared first segment on the control host".to_string(), "POST", CONTROL_HOST, format!("/registries/{REGISTRY_ID}/discoverable-records-list"), CLAIMED_BY_NEITHER),
        ("shared first segment on the shared host".to_string(), "POST", DATA_HOST, format!("/registries/{REGISTRY_ID}/discoverable-records-list"), CLAIMED_BY_DATA),
        ("a path neither model binds, on the control host".to_string(), "POST", CONTROL_HOST, "/nothing-here".to_string(), CLAIMED_BY_NEITHER),
        ("a path neither model binds, on the shared host".to_string(), "POST", DATA_HOST, "/nothing-here".to_string(), CLAIMED_BY_DATA),
    ];

    for (case, method, host, uri, expected) in rows {
        let claimed = claimants(&control, &data, &parts(method, host, &uri, false));
        assert_eq!(
            claimed, expected,
            "case {case:?} ({method} {uri} on {host}, unsigned): expected it to be claimed \
             by {expected}, got {claimed:?}"
        );
    }
}

#[tokio::test]
async fn no_agent_registry_request_is_answered_by_the_routers_refusals() {
    let router = router_with_both_services();

    // The two routing failures no row may provoke: `AmbiguousService` (500, several
    // claimants that each name an operation — rendered in an awsJson envelope whatever the
    // claimants' own protocol is) and `UnknownService` (404, none claimed). Every row must
    // be answered by one service's own `restJson1` envelope instead.
    let rows: Vec<(String, &str, String, Value)> = vec![
        ("CreateRegistry".to_string(), "POST", "/registries".to_string(), json!({ "name": "routing-probe" })),
        ("ListRegistries".to_string(), "POST", "/registries-list".to_string(), json!({})),
        ("GetRegistry".to_string(), "GET", format!("/registries/{REGISTRY_ID}"), Value::Null),
        ("ListTagsForResource".to_string(), "GET", format!("/tags/{}", registry_arn()), Value::Null),
        ("SearchDiscoverableRegistryRecords".to_string(), "POST", "/discoverable-records-search".to_string(), json!({ "searchQuery": "anything", "registryIds": [REGISTRY_ID] })),
        ("BatchGetDiscoverableRegistryRecord".to_string(), "POST", "/discoverable-records-batch".to_string(), json!({ "entries": [{ "registryId": REGISTRY_ID, "recordIds": [RECORD_ID] }] })),
        ("ListDiscoverableRegistryRecords".to_string(), "POST", format!("/registries/{REGISTRY_ID}/discoverable-records-list"), json!({})),
    ];

    for (case, method, uri, body) in rows {
        let (status, code, content_type, _) = call(&router, method, &uri, body).await;
        assert_ne!(
            code, "AmbiguousService",
            "case {case:?}: a request several claimants name an operation for is refused \
             500 AmbiguousService; exactly one must name it, got {status} {code:?}"
        );
        assert_ne!(
            code, "UnknownService",
            "case {case:?}: a route a model binds must be claimed, got {status} {code:?}"
        );
        assert_ne!(
            status, 500,
            "case {case:?}: no modelled route answers a 5xx, got {status} {code:?}"
        );
        assert!(
            content_type.starts_with("application/json"),
            "case {case:?}: restJson1 frames every answer as application/json, got \
             {content_type:?} with {status} {code:?}"
        );
    }

    // And a request addressed to this endpoint that NEITHER model binds a route for is the
    // endpoint's own modelled refusal, in its own envelope — not the router's
    // cross-protocol `UnknownService`. The rows differ in HOW the request names no route:
    // a path space neither model publishes, a method no route carries, and a labelled path
    // that bound to nothing.
    let unbound: [(&str, &str, &str); 4] = [
        ("a path no model publishes", "POST", "/nothing-here"),
        ("a method no route carries", "GET", "/discoverable-records-search"),
        ("a method no route carries on a bound path", "GET", "/registries"),
        (
            "a labelled path that bound to nothing",
            "POST",
            "/registries/discoverable-records-list",
        ),
    ];
    for (case, method, uri) in unbound {
        let (status, code, content_type, message) = call(&router, method, uri, json!({})).await;
        assert_eq!(
            (status, code.as_str(), message.as_str()),
            (400, VALIDATION_EXCEPTION, REQUEST_INVALID),
            "case {case:?} ({method} {uri}): expected the endpoint's own 400 \
             {VALIDATION_EXCEPTION} with its fixed text, got {status} {code:?} {message:?}"
        );
        assert!(
            content_type.starts_with("application/json"),
            "case {case:?}: the refusal is framed in the resolved service's protocol, got \
             {content_type:?}"
        );
    }
}
