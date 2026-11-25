// SPDX-License-Identifier: CC0-1.0

//! The consequence of the rule `instrumentation_capture_defaults` states, for the client that
//! round-trips a configuration: re-applying the `CaptureConfiguration` the service REPORTED
//! stores exactly the structure the first create supplied, so a template-vs-read-back diff is
//! empty and a re-apply pins no member the caller never chose.
//!
//! The configuration read back is a FIXED POINT of the create: feeding a read's own
//! `CaptureConfiguration` straight back into `CreateInstrumentationConfiguration` yields a
//! record that states the same members with the same values. That holds only because no
//! optional member of `CodeCaptureConfiguration` or `CaptureLimitsConfig` is modelled with a
//! `@default` (`smithy_query(query="shape",shape="CodeCaptureConfiguration,CaptureLimitsConfig")`),
//! so an omission stays an omission however many times the value goes round.
//!
//! The rows differ along what the rule generalises over: which members the first create supplies
//! (none beyond the required structure, one limit, one boolean, a list, all of them) and the
//! instrumentation type the record is created under.
//!
//! The expectation itself is the one `instrumentation_capture_defaults` owns, so both suites
//! state it once.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use crate::instrumentation_capture_defaults::assert_as_supplied;

/// The SigV4 credential scope routes the request to this service. Signature verification is
/// off (see `set_request_authentication`), so the signature bytes are never read.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-2/application-signals/aws4_request, \
     SignedHeaders=host, Signature=0000";

/// The region in [`AUTHORIZATION`].
const REGION: &str = "eu-west-2";

/// The only `DynamicInstrumentationSignalType` the model declares
/// (`smithy_query(query="shape",shape="DynamicInstrumentationSignalType")` -> `["SNAPSHOT"]`).
const SNAPSHOT: &str = "SNAPSHOT";

/// The two `InstrumentationType` values the model declares
/// (`smithy_query(query="shape",shape="InstrumentationType")`).
const BREAKPOINT: &str = "BREAKPOINT";
const PROBE: &str = "PROBE";

/// The service and environment every row is created in.
const SERVICE: &str = "fulfilment";
const ENVIRONMENT: &str = "eks:cluster-prod/orders";

/// A router with just this service mounted on a fresh engine.
fn router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_applicationsignals_applicationsignals::EmulatedService::new(engine),
    );
    router
}

/// Send one request and return its status and parsed JSON body (`Value::Null` when empty).
async fn post(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
    let payload = serde_json::to_vec(body).expect("serialize the request body");
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://application-signals.{REGION}.amazonaws.com{uri}"
        ))
        .header("content-type", "application/json")
        .header("authorization", AUTHORIZATION)
        .body(bytes::Bytes::from(payload))
        .expect("build the request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    let parsed = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// A create at a location of its own, carrying `capture_configuration` as declared.
fn create_body(instrumentation_type: &str, line: i64, capture_configuration: &Value) -> Value {
    json!({
        "InstrumentationType": instrumentation_type,
        "Service": SERVICE,
        "Environment": ENVIRONMENT,
        "SignalType": SNAPSHOT,
        "Location": { "CodeLocation": {
            "Language": "Java",
            "ClassName": "OrderService",
            "MethodName": "submit",
            "FilePath": "src/main/java/OrderService.java",
            "LineNumber": line
        }},
        "CaptureConfiguration": capture_configuration.clone()
    })
}

/// A get naming one configuration by the `LocationHash` its create answered.
fn get_body(instrumentation_type: &str, hash: &str) -> Value {
    json!({
        "Service": SERVICE,
        "Environment": ENVIRONMENT,
        "SignalType": SNAPSHOT,
        "InstrumentationType": instrumentation_type,
        "LocationIdentifier": { "LocationHash": hash }
    })
}

/// One row: the `CodeCapture` the FIRST create declares, and the type it is created under.
struct Case {
    label: &'static str,
    capture: Value,
    instrumentation_type: &'static str,
}

/// The rows: the counts and kinds of supplied member a template may declare.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "one limit declared, every sibling omitted",
            capture: json!({ "CaptureLimits": { "MaxHits": 10 } }),
            instrumentation_type: PROBE,
        },
        Case {
            label: "nothing beyond the required and empty CaptureLimits",
            capture: json!({ "CaptureLimits": {} }),
            instrumentation_type: BREAKPOINT,
        },
        Case {
            label: "one boolean declared, every limit omitted",
            capture: json!({ "CaptureStackTrace": false, "CaptureLimits": {} }),
            instrumentation_type: PROBE,
        },
        Case {
            label: "one list and two limits declared",
            capture: json!({
                "CaptureArguments": ["orderId"],
                "CaptureLimits": { "MaxStringLength": 200, "MaxStackFrames": 4 }
            }),
            instrumentation_type: BREAKPOINT,
        },
        Case {
            label: "every member of the structure declared",
            capture: json!({
                "CaptureArguments": ["orderId", "customerId"],
                "CaptureReturn": true,
                "CaptureStackTrace": true,
                "CaptureLocals": ["total"],
                "CaptureLimits": {
                    "MaxHits": 3,
                    "MaxStringLength": 32,
                    "MaxCollectionWidth": 2,
                    "MaxCollectionDepth": 1,
                    "MaxStackFrames": 1,
                    "MaxStackTraceSize": 64,
                    "MaxObjectDepth": 2,
                    "MaxFieldsPerObject": 5
                }
            }),
            instrumentation_type: PROBE,
        },
    ]
}

/// The configuration a read reports is the one a re-apply of it stores: the declared structure
/// survives create -> read -> create unchanged, so a diff of the declaration against the
/// read-back value reports no change.
#[tokio::test]
async fn a_configuration_read_back_is_re_applied_unchanged() {
    let router = router();

    for (index, case) in cases().iter().enumerate() {
        let label = case.label;
        let declared = json!({ "CodeCapture": case.capture.clone() });
        let first_line = index as i64 * 2 + 1;

        let (status, created) = post(
            &router,
            "/create-instrumentation-configuration",
            &create_body(case.instrumentation_type, first_line, &declared),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200 from the first create, got {status} with {created}"
        );
        assert_as_supplied(
            label,
            "the first create",
            &case.capture,
            &created["CaptureConfiguration"],
        );

        let hash = created["LocationHash"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        assert!(
            !hash.is_empty(),
            "case {label:?}: expected a LocationHash on the create answer, got {created}"
        );

        let (status, fetched) = post(
            &router,
            "/get-instrumentation-configuration",
            &get_body(case.instrumentation_type, &hash),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200 from the get, got {status} with {fetched}"
        );
        let read_back = fetched["Configuration"]["CaptureConfiguration"].clone();
        assert_eq!(
            read_back, declared,
            "case {label:?}: expected the read to report the declared configuration {declared}, \
             got {read_back:?}"
        );

        // The value READ BACK is sent as-is to a create at another location: what a client
        // re-applying a template it built from the read does. The record it stores states the
        // same members as the first one, so nothing accumulated on the way round.
        let (status, re_applied) = post(
            &router,
            "/create-instrumentation-configuration",
            &create_body(case.instrumentation_type, first_line + 1, &read_back),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200 from the re-apply, got {status} with {re_applied}"
        );
        assert_as_supplied(
            label,
            "the re-apply",
            &case.capture,
            &re_applied["CaptureConfiguration"],
        );

        let re_applied_hash = re_applied["LocationHash"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let (status, fetched_again) = post(
            &router,
            "/get-instrumentation-configuration",
            &get_body(case.instrumentation_type, &re_applied_hash),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {label:?}: expected HTTP 200 from the get of the re-applied record, got \
             {status} with {fetched_again}"
        );
        assert_eq!(
            fetched_again["Configuration"]["CaptureConfiguration"], read_back,
            "case {label:?}: expected the re-applied record to read back as {read_back}, got {:?}",
            fetched_again["Configuration"]["CaptureConfiguration"]
        );
    }
}
