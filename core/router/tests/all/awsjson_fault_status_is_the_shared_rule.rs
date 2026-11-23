// SPDX-License-Identifier: CC0-1.0

//! The HTTP status an `awsJson` service answers a modelled error at, now that the three
//! services that had each spelled the classification answer it from ONE place.
//!
//! RULE under test: **on the `awsJson` protocols a modelled error is answered at its
//! FAULT, not at the exact code the shape's `@httpError` trait names — every client fault
//! is HTTP 400, with the body's `__type` as the only thing that tells one from another,
//! and a server fault keeps its 5xx.** `@httpError` is an HTTP-BINDING trait and the
//! awsJson protocols bind nothing of an operation to the transport, which is why the
//! status carries the fault rather than the shape.
//!
//! The classification now lives once, in `service_common::awsjson_fault_status`, and the
//! three crates whose recorded wire shows that front end read it there instead of
//! restating it:
//!
//! * Amazon EVS (`awsJson1_0`), whose recording answers `ResourceNotFoundException` —
//!   `@httpError(404)` in its published model — with HTTP 400;
//! * AWS HealthLake (`awsJson1_0`), whose recording answers its own
//!   `ResourceNotFoundException`, also `@httpError(404)`, with HTTP 400;
//! * Service Quotas (`awsJson1_1`), whose recording answers a lookup that names no quota,
//!   service or increase request with HTTP 400 carrying `NoSuchResourceException`, a shape
//!   its model annotates `@httpError(404)`.
//!
//! Three halves are asserted, and each spans a SET of inputs rather than one recorded
//! call:
//!
//! * over the WIRE, one operation of each of the three services, refused with a shape
//!   whose own `@httpError` is 404 and with one whose own `@httpError` is 400, so the 400
//!   on the wire is the rule's answer and not a coincidence of one shape;
//! * over each service's WHOLE declared error catalog, read out of that crate's shipped
//!   Smithy model at test time rather than written here, so no status is hand-copied and a
//!   shape a model gains later is covered;
//! * over every status a shape could declare, each of the three services answers the status
//!   the rule gives it — stated here rather than read off one of them — so all three answer the
//!   same status, which is the invariant the consolidation exists for and the one a fourth copy
//!   of the classification would break.
//!
//! Why it lives beside the router's suites rather than in one service crate: the invariant
//! spans the three service crates and the `service-common` module they now read, and this
//! is the test target that may observe all of them at once.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below signs for.
const REGION: &str = "us-east-1";

/// The access key id the requests carry: one no session of this emulator was minted for,
/// so each request speaks for the account's own published credential.
const ACCESS_KEY_ID: &str = "AKIDTESTONLY";

/// One `awsJson` service that reads the shared fault-status rule.
struct Caller {
    /// Named in every assertion this suite makes about the service.
    service: &'static str,
    /// The SigV4 signing name the request's credential scope names — what routes the
    /// request to this service.
    signing_name: &'static str,
    /// The `x-amz-target` prefix the service publishes, from its crate's `OPERATIONS.md`.
    target_prefix: &'static str,
    /// The media type its awsJson protocol version names.
    content_type: &'static str,
    /// The Smithy namespace the service's own error shapes live in.
    namespace: &'static str,
    /// The service's shipped Smithy model — the SOURCE of its declared `@httpError`
    /// values, so the catalog below is read rather than written.
    model_path: &'static str,
    /// What that crate answers for a modelled status: the shared rule, re-exported as
    /// `crate::scaffolded::wire_http_status`.
    wire_http_status: fn(u16) -> u16,
}

/// The three services the classification was consolidated out of.
fn callers() -> Vec<Caller> {
    vec![
        Caller {
            service: "Amazon EVS",
            signing_name: "evs",
            target_prefix: "AmazonElasticVMwareService",
            content_type: "application/x-amz-json-1.0",
            namespace: "com.amazonaws.evs#",
            model_path: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../services/service-com-amazonaws-evs-amazonelasticvmwareservice",
                "/smithy-model/com-amazonaws-evs-amazonelasticvmwareservice.json"
            ),
            wire_http_status:
                service_com_amazonaws_evs_amazonelasticvmwareservice::scaffolded::wire_http_status,
        },
        Caller {
            service: "AWS HealthLake",
            signing_name: "healthlake",
            target_prefix: "HealthLake",
            content_type: "application/x-amz-json-1.0",
            namespace: "com.amazonaws.healthlake#",
            model_path: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../services/service-com-amazonaws-healthlake-healthlake",
                "/smithy-model/com-amazonaws-healthlake-healthlake.json"
            ),
            wire_http_status:
                service_com_amazonaws_healthlake_healthlake::scaffolded::wire_http_status,
        },
        Caller {
            service: "Service Quotas",
            signing_name: "servicequotas",
            target_prefix: "ServiceQuotasV20190624",
            content_type: "application/x-amz-json-1.1",
            namespace: "com.amazonaws.servicequotas#",
            model_path: concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../services/service-com-amazonaws-servicequotas-servicequotasv20190624",
                "/smithy-model/com-amazonaws-servicequotas-servicequotasv20190624.json"
            ),
            wire_http_status:
                service_com_amazonaws_servicequotas_servicequotasv20190624::scaffolded::wire_http_status,
        },
    ]
}

/// One emulator with an empty store, carrying all three services the way the router
/// mounts them.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // These requests sign nothing, so they must not be refused for it: what is under test
    // is the status of a modelled error, not authentication.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_evs_amazonelasticvmwareservice::EmulatedService::new(Arc::clone(
            &engine,
        )),
    );
    router.register_service(
        service_com_amazonaws_healthlake_healthlake::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(
        service_com_amazonaws_servicequotas_servicequotasv20190624::EmulatedService::new(engine),
    );
    router
}

/// Dispatch one awsJson call to `caller` and return its HTTP status, the error code it
/// carries (`None` for a served response) and its parsed body.
async fn call(
    router: &MultiServiceRouter,
    caller: &Caller,
    operation: &str,
    body: &Value,
) -> (u16, Option<String>, Value) {
    let signing_name = caller.signing_name;
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("{signing_name}.{REGION}.amazonaws.com"))
        .header("content-type", caller.content_type)
        .header(
            "x-amz-target",
            format!("{}.{operation}", caller.target_prefix),
        )
        // The credential scope is what routes the request to one of the three services
        // mounted above, and names the Region it is addressed to.
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential={ACCESS_KEY_ID}/20260101/{REGION}/{signing_name}/\
                 aws4_request, SignedHeaders=host;x-amz-target, Signature=notchecked"
            ),
        )
        .body(bytes::Bytes::from(
            serde_json::to_vec(body).expect("the request body serializes"),
        ))
        .expect("a well-formed test request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let header_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let raw = response.into_body();
    let parsed: Value = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    let code = parsed
        .get("__type")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or(header_type)
        // An error `__type` may be namespaced (`com.amazonaws.evs#Foo`) or a shape id URI.
        .map(|code| {
            code.rsplit(['#', '/'])
                .next()
                .unwrap_or_default()
                .to_string()
        });
    (status, code, parsed)
}

/// Every error shape a caller's own namespace declares, as
/// `(shape name, the status it declares, the fault it declares)`.
///
/// Read out of the shipped model at test time, so the catalog's source is the model
/// itself rather than a list written here that could drift from it. Smithy defaults a
/// declared error with no `@httpError` to 400 for a client fault and 500 for a server
/// one, which is what an absent trait is read as.
fn modelled_error_shapes(caller: &Caller) -> Vec<(String, u16, String)> {
    let model = std::fs::read_to_string(caller.model_path)
        .unwrap_or_else(|e| panic!("{}: its crate ships a Smithy model: {e}", caller.service));
    let model: Value = serde_json::from_str(&model)
        .unwrap_or_else(|e| panic!("{}: its Smithy model is JSON: {e}", caller.service));
    let shapes = model
        .get("shapes")
        .and_then(Value::as_object)
        .unwrap_or_else(|| panic!("{}: a Smithy model has shapes", caller.service));
    let mut found: Vec<(String, u16, String)> = shapes
        .iter()
        .filter(|(id, _)| id.starts_with(caller.namespace))
        .filter_map(|(id, shape)| {
            let traits = shape.get("traits")?;
            let fault = traits.get("smithy.api#error")?.as_str()?.to_string();
            let declared = traits
                .get("smithy.api#httpError")
                .and_then(Value::as_u64)
                .unwrap_or(if fault == "server" { 500 } else { 400 }) as u16;
            Some((
                id.rsplit('#').next().unwrap_or_default().to_string(),
                declared,
                fault,
            ))
        })
        .collect();
    found.sort();
    found
}

/// One wire row: what makes the request a client fault, which operation it names, the
/// request, the error code it must be refused with, and the `@httpError` that shape's own
/// model declares.
struct Row {
    case: &'static str,
    operation: &'static str,
    request: Value,
    expected_code: &'static str,
    declared_status: u16,
}

/// The rows for one caller: a refusal whose shape declares 404, and one whose shape
/// declares 400, so the status on the wire is the rule's answer rather than one shape's
/// own number. Each `@httpError` is `smithy_query(query="shape", shape="...")` against
/// that service's shipped model.
fn rows_for(service: &str) -> Vec<Row> {
    match service {
        "Amazon EVS" => vec![
            Row {
                case: "a well-formed environment id this account holds no environment for",
                operation: "GetEnvironment",
                // Satisfies `^(env-[a-zA-Z0-9]{10})$`, so the refusal is the lookup's.
                request: json!({ "environmentId": "env-0000000000" }),
                expected_code: "ResourceNotFoundException",
                declared_status: 404,
            },
            Row {
                case: "an environment id that is not an environment id at all",
                operation: "GetEnvironment",
                request: json!({ "environmentId": "not-an-environment-id" }),
                expected_code: "ValidationException",
                declared_status: 400,
            },
        ],
        "AWS HealthLake" => vec![
            Row {
                case: "a data store id nothing was minted under",
                operation: "DescribeFHIRDatastore",
                request: json!({ "DatastoreId": "00000000000000000000000000000000" }),
                expected_code: "ResourceNotFoundException",
                declared_status: 404,
            },
            Row {
                case: "a data store id below its declared length minimum",
                operation: "DescribeFHIRDatastore",
                request: json!({ "DatastoreId": "" }),
                expected_code: "ValidationException",
                declared_status: 400,
            },
        ],
        "Service Quotas" => vec![
            Row {
                case: "a lookup naming a service code no catalog entry holds",
                operation: "GetServiceQuota",
                request: json!({
                    "ServiceCode": "not-a-real-service-xyz",
                    "QuotaCode": "L-00000000",
                }),
                expected_code: "NoSuchResourceException",
                declared_status: 404,
            },
            Row {
                case: "a quota code outside the pattern its member declares",
                operation: "GetServiceQuota",
                request: json!({ "ServiceCode": "vpc", "QuotaCode": "1-bad code" }),
                expected_code: "IllegalArgumentException",
                declared_status: 400,
            },
        ],
        other => panic!("no wire rows written for {other:?}"),
    }
}

/// A call of each caller that SUCCEEDS, so the rule is shown to re-state the status of an
/// ERROR and nothing else: a served response keeps its own status.
fn served_call(service: &str) -> (&'static str, Value) {
    match service {
        "Amazon EVS" => ("ListEnvironments", json!({})),
        "AWS HealthLake" => ("ListFHIRDatastores", json!({})),
        "Service Quotas" => ("ListServices", json!({})),
        other => panic!("no served call written for {other:?}"),
    }
}

/// Half one: over the wire, each of the three services answers a modelled client error at
/// the shared fault status, whatever its shape's `@httpError` declares — and a served call
/// keeps its own 200.
#[tokio::test]
async fn each_caller_answers_a_modelled_client_error_at_the_shared_fault_status() {
    let router = emulator();

    for caller in callers() {
        let service = caller.service;
        for row in rows_for(service) {
            let Row {
                case,
                operation,
                request,
                expected_code,
                declared_status,
            } = row;
            let (status, code, body) = call(&router, &caller, operation, &request).await;
            // The status the shared rule gives that shape's declared status, read through
            // the crate under test rather than written here.
            let expected = (caller.wire_http_status)(declared_status);
            assert_eq!(
                expected, 400,
                "case {service} / {case:?}: the shared rule must put a client fault \
                 declared at {declared_status} on 400, got {expected}"
            );
            assert_eq!(
                status, expected,
                "case {service} / {case:?} on {operation} (its shape declares \
                 @httpError({declared_status})): expected HTTP {expected}, got {status}: {body}"
            );
            assert_eq!(
                code.as_deref(),
                Some(expected_code),
                "case {service} / {case:?} on {operation}: expected the error code \
                 {expected_code}, got {code:?}: {body}"
            );
        }

        let (operation, request) = served_call(service);
        let (status, code, body) = call(&router, &caller, operation, &request).await;
        assert_eq!(
            status, 200,
            "case {service} / a served call on {operation}: the rule restates the status of \
             an error only, so this must keep its 200, got {status} / {code:?}: {body}"
        );
    }
}

/// Half two: over the WHOLE catalog each caller's model declares, the status on the wire
/// is the fault — 400 for a client fault whatever 4xx it declares, its own 5xx for a
/// server fault — plus each published bound of the split and one past it.
#[test]
fn every_error_shape_a_callers_model_declares_goes_on_the_wire_at_its_fault() {
    for caller in callers() {
        let service = caller.service;
        let catalog = modelled_error_shapes(&caller);
        assert!(
            catalog.len() >= 5,
            "case {service}: its model must declare the error shapes this rule is about, \
             got {catalog:?}"
        );
        // The catalog has to reach beyond the one shape the recordings cite, or the rule
        // would be asserted on a single value.
        assert!(
            catalog
                .iter()
                .any(|(_, declared, _)| ![400_u16, 404].contains(declared)),
            "case {service}: the catalog must carry a declared status beyond 400 and 404, \
             got {catalog:?}"
        );
        assert!(
            catalog.iter().any(|(_, _, fault)| fault == "server"),
            "case {service}: the catalog must carry a server fault, got {catalog:?}"
        );

        for (shape, declared, fault) in &catalog {
            let expected = if fault == "server" { *declared } else { 400 };
            let actual = (caller.wire_http_status)(*declared);
            assert_eq!(
                actual, expected,
                "case {service} / {shape:?} (@error {fault}, @httpError {declared}): expected \
                 wire status {expected}, got {actual}"
            );
        }

        for (case, declared, expected) in [
            ("the lowest client status", 400_u16, 400_u16),
            ("the highest client status", 499, 400),
            ("the lowest server status", 500, 500),
            ("one past the lowest server status", 501, 501),
        ] {
            let actual = (caller.wire_http_status)(declared);
            assert_eq!(
                actual, expected,
                "case {service} / {case:?} (@httpError {declared}): expected wire status \
                 {expected}, got {actual}"
            );
        }
    }
}

/// Half three, the invariant the consolidation exists for: for every status a shape could
/// declare, EACH of the three services answers the status the rule gives it, and so all three
/// answer the same one. A fourth copy of the classification, or a correction made in one crate
/// and not the others, fails here.
///
/// The expected status is stated here rather than read off one of the callers, so this half
/// cannot pass by three crates agreeing on a wrong answer: below the server class the fault is
/// the client's and the status is 400, and a 5xx is the server's own and is kept.
#[test]
fn the_three_callers_answer_one_status_for_every_status_a_shape_could_declare() {
    for caller in callers() {
        let service = caller.service;
        for declared in 100_u16..500 {
            let actual = (caller.wire_http_status)(declared);
            assert_eq!(
                actual, 400,
                "case {service} / a declared status of {declared}, below the server class: \
                 expected 400, got {actual}"
            );
        }
        for declared in 500_u16..=599 {
            let actual = (caller.wire_http_status)(declared);
            assert_eq!(
                actual, declared,
                "case {service} / a declared status of {declared}, a server fault: expected it \
                 kept, got {actual}"
            );
        }
    }
}
