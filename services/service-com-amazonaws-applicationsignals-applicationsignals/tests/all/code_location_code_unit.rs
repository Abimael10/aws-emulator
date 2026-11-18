// SPDX-License-Identifier: CC0-1.0

//! The requirement a code location's LANGUAGE puts on the `CodeUnit` member, over HTTP.
//!
//! RULE: `CreateInstrumentationConfiguration` refuses a code location that omits the member
//! its programming language requires. For Python that member is the `CodeUnit` — "the package,
//! module, or namespace that contains the target code"
//! (`smithy_query(query="doc",shape="CodeLocation",member="CodeUnit")`) — so a Python location
//! naming none names no module to find the function in: HTTP 400 `ValidationException`, worded
//! `CodeUnit is required for Python locations`, and nothing is stored. A location of any other
//! language is served with or without it, the member being optional for them.
//!
//! The requirement is on the point being RECORDED, so it holds of the create's `Location`
//! alone: the reads and the delete HASH the location they are given to look a record up by, and
//! a location they hold no record for is a 404 rather than a malformed instrumentation point —
//! which is what the rows below assert of every operation that takes a `LocationIdentifier`.
//!
//! The rows differ along what the rule generalises over: the programming LANGUAGE (the whole
//! modelled catalogue — `smithy_query(query="shape",shape="ProgrammingLanguage")` -> `["Java",
//! "Python", "Javascript"]`), whether the location supplies a `CodeUnit`, which of
//! `CodeLocation`'s other optional members it supplies, the published `length 1..128` bounds of
//! `CodeUnit` itself, and the instrumentation type the create names.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The SigV4 credential scope routes the request to this service. Signature verification is
/// off (see `set_request_authentication`), so the signature bytes are never read.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-2/application-signals/aws4_request, \
     SignedHeaders=host, Signature=0000";

/// The region in [`AUTHORIZATION`].
const REGION: &str = "eu-west-2";

/// The only `DynamicInstrumentationSignalType` the model declares.
const SNAPSHOT: &str = "SNAPSHOT";

/// The two `InstrumentationType` values.
const BREAKPOINT: &str = "BREAKPOINT";
const PROBE: &str = "PROBE";

/// The `ProgrammingLanguage` that requires a code unit, and the member it requires.
const PYTHON: &str = "Python";
const CODE_UNIT: &str = "CodeUnit";

/// The published bounds of `CodeUnit` (`smithy_query(query="shape",shape="CodeLocation")`:
/// `CodeUnit` has `length 1..128`).
const CODE_UNIT_MIN: usize = 1;
const CODE_UNIT_MAX: usize = 128;

/// The scope every row stores its configuration in. One row per router, so the scope never has
/// to differ.
const SERVICE: &str = "pricing";
const ENVIRONMENT: &str = "eks:cluster-prod/namespace";

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
async fn call(router: &MultiServiceRouter, uri: &str, body: &Value) -> (u16, Value) {
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

/// `POST /create-instrumentation-configuration`.
async fn create(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "/create-instrumentation-configuration", body).await
}

/// `POST /list-instrumentation-configurations`.
async fn list(router: &MultiServiceRouter, body: &Value) -> (u16, Value) {
    call(router, "/list-instrumentation-configurations", body).await
}

/// The operations that take the instrumentation point as a `LocationIdentifier`, as
/// `(label, uri)`.
const IDENTIFIER_OPERATIONS: &[(&str, &str)] = &[
    ("get", "/get-instrumentation-configuration"),
    ("get status", "/get-instrumentation-configuration-status"),
    ("delete", "/delete-instrumentation-configuration"),
];

/// A `CaptureConfiguration` with only its `@required` members.
fn minimal_capture() -> Value {
    json!({ "CodeCapture": { "CaptureLimits": {} } })
}

/// The create body storing `code_location` under `instrumentation_type`.
fn create_body(code_location: &Value, instrumentation_type: &str) -> Value {
    json!({
        "InstrumentationType": instrumentation_type,
        "Service": SERVICE,
        "Environment": ENVIRONMENT,
        "SignalType": SNAPSHOT,
        "Location": { "CodeLocation": code_location },
        "CaptureConfiguration": minimal_capture(),
    })
}

/// The body of an operation that identifies the point by `identifier`.
fn identifier_body(identifier: Value, instrumentation_type: &str) -> Value {
    json!({
        "InstrumentationType": instrumentation_type,
        "Service": SERVICE,
        "Environment": ENVIRONMENT,
        "SignalType": SNAPSHOT,
        "LocationIdentifier": identifier,
    })
}

/// The error code a refusal reports, with any namespace prefix the envelope carries dropped.
fn error_code(answer: &Value) -> Option<String> {
    answer["__type"]
        .as_str()
        .map(|kind| kind.rsplit('#').next().unwrap_or(kind).to_string())
}

/// The sentence this service words for a location that omits the member its language requires.
fn required_refusal(member: &str, language: &str) -> String {
    format!("{member} is required for {language} locations")
}

/// One row of the location table: the `CodeLocation` a create carries and whether this service
/// records the point it describes.
struct Case {
    label: &'static str,
    code_location: Value,
    /// Whether the location names the member its language requires.
    served: bool,
}

/// The locations the rule is asserted over.
fn cases() -> Vec<Case> {
    vec![
        Case {
            label: "Python, no CodeUnit",
            code_location: json!({
                "Language": PYTHON,
                "FilePath": "app/pricing/quote.py",
            }),
            served: false,
        },
        Case {
            label: "Python, no CodeUnit, every other optional member supplied",
            code_location: json!({
                "Language": PYTHON,
                "ClassName": "Quote",
                "MethodName": "total",
                "FilePath": "app/pricing/quote.py",
                "LineNumber": 42,
            }),
            served: false,
        },
        Case {
            label: "Python, a CodeUnit",
            code_location: json!({
                "Language": PYTHON,
                "CodeUnit": "app.pricing.quote",
                "MethodName": "total",
                "FilePath": "app/pricing/quote.py",
            }),
            served: true,
        },
        Case {
            label: "Python, the shortest CodeUnit the model admits",
            code_location: json!({
                "Language": PYTHON,
                "CodeUnit": "u".repeat(CODE_UNIT_MIN),
                "FilePath": "app/pricing/quote.py",
            }),
            served: true,
        },
        Case {
            label: "Python, the longest CodeUnit the model admits",
            code_location: json!({
                "Language": PYTHON,
                "CodeUnit": "u".repeat(CODE_UNIT_MAX),
                "FilePath": "app/pricing/quote.py",
            }),
            served: true,
        },
        Case {
            label: "Java, no CodeUnit: the member is optional for it",
            code_location: json!({
                "Language": "Java",
                "ClassName": "QuoteService",
                "FilePath": "src/main/java/com/example/QuoteService.java",
            }),
            served: true,
        },
        Case {
            label: "Java, a CodeUnit",
            code_location: json!({
                "Language": "Java",
                "CodeUnit": "com.example.pricing",
                "ClassName": "QuoteService",
                "MethodName": "total",
                "FilePath": "src/main/java/com/example/QuoteService.java",
                "LineNumber": 7,
            }),
            served: true,
        },
        Case {
            label: "Javascript, no CodeUnit: the member is optional for it",
            code_location: json!({
                "Language": "Javascript",
                "FilePath": "web/src/quote.js",
            }),
            served: true,
        },
        Case {
            label: "Javascript, a CodeUnit",
            code_location: json!({
                "Language": "Javascript",
                "CodeUnit": "quote",
                "MethodName": "total",
                "FilePath": "web/src/quote.js",
            }),
            served: true,
        },
    ]
}

/// A create whose code location omits the member its language requires is refused with the
/// modelled `ValidationException` naming that member and that language; every other location in
/// the catalogue is recorded.
#[tokio::test]
async fn a_create_applies_the_requirement_its_locations_language_states() {
    for case in cases() {
        let label = case.label;
        let router = router();

        let (status, answer) = create(&router, &create_body(&case.code_location, BREAKPOINT)).await;
        if case.served {
            assert_eq!(
                status, 200,
                "case {label:?}: expected the create to answer 200, got {status} with {answer}"
            );
            assert_eq!(
                answer["Location"]["CodeLocation"], case.code_location,
                "case {label:?}: expected the stored location to be the one created, got {:?}",
                answer["Location"]["CodeLocation"]
            );
        } else {
            assert_eq!(
                status, 400,
                "case {label:?}: expected the create to answer 400, got {status} with {answer}"
            );
            assert_eq!(
                error_code(&answer),
                Some("ValidationException".to_string()),
                "case {label:?}: expected a ValidationException, got {answer}"
            );
            assert_eq!(
                answer["message"].as_str(),
                Some(required_refusal(CODE_UNIT, PYTHON).as_str()),
                "case {label:?}: expected the refusal to name the member and the language, got \
                 {answer}"
            );
        }
    }
}

/// The requirement is on the point being RECORDED: the reads and the delete take the same
/// location as a `LocationIdentifier` and are answered about the record it resolves to — 404
/// for a location this service holds nothing under — rather than refused for a member the
/// create would have required.
#[tokio::test]
async fn the_reads_and_the_delete_resolve_a_location_rather_than_requiring_the_member() {
    let router = router();
    let stored = json!({
        "Language": PYTHON,
        "CodeUnit": "app.pricing.quote",
        "MethodName": "total",
        "FilePath": "app/pricing/quote.py",
    });
    let (status, created) = create(&router, &create_body(&stored, PROBE)).await;
    assert_eq!(
        status, 200,
        "expected the create naming the code unit to answer 200, got {status} with {created}"
    );

    // The same location WITHOUT the code unit is another location, so it resolves to no record
    // at all — and each operation says so rather than refusing the member.
    let without = json!({
        "Language": PYTHON,
        "MethodName": "total",
        "FilePath": "app/pricing/quote.py",
    });
    for (operation, uri) in IDENTIFIER_OPERATIONS {
        let (status, answer) = call(
            &router,
            uri,
            &identifier_body(json!({ "CodeLocation": without }), PROBE),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {operation:?}: expected an identity holding no configuration to be reported \
             not found, got {status} with {answer}"
        );
        assert_eq!(
            error_code(&answer),
            Some("ResourceNotFoundException".to_string()),
            "case {operation:?}: expected a ResourceNotFoundException, got {answer}"
        );
    }

    // And the record the create stored is untouched by those refusals.
    let (status, listed) = list(
        &router,
        &json!({
            "InstrumentationType": PROBE,
            "Service": SERVICE,
            "Environment": ENVIRONMENT,
        }),
    )
    .await;
    assert_eq!(
        (
            status,
            listed["LatestConfigurations"].as_array().map(Vec::len)
        ),
        (200, Some(1)),
        "expected the stored configuration to survive every refused lookup, got {status} with \
         {listed}"
    );
}

/// A refused create stores NOTHING: the scope it named reports no configurations, and the same
/// point naming its module is created at that identity afterwards.
#[tokio::test]
async fn a_refused_location_is_not_persisted() {
    let router = router();
    let without = json!({
        "Language": PYTHON,
        "MethodName": "total",
        "FilePath": "app/pricing/quote.py",
    });
    let (status, refusal) = create(&router, &create_body(&without, PROBE)).await;
    assert_eq!(
        status, 400,
        "expected the create of a Python location with no CodeUnit to answer 400, got {status} \
         with {refusal}"
    );

    let scope = json!({
        "InstrumentationType": PROBE,
        "Service": SERVICE,
        "Environment": ENVIRONMENT,
    });
    let (status, listed) = list(&router, &scope).await;
    assert_eq!(
        (
            status,
            listed["LatestConfigurations"].as_array().map(Vec::len)
        ),
        (200, Some(0)),
        "expected the refused create to have stored nothing, got {status} with {listed}"
    );

    let mut with = without.clone();
    with["CodeUnit"] = json!("app.pricing.quote");
    let (status, created) = create(&router, &create_body(&with, PROBE)).await;
    assert_eq!(
        status, 200,
        "expected the create naming the code unit to answer 200, got {status} with {created}"
    );
    assert_eq!(
        created["Location"]["CodeLocation"], with,
        "expected the stored location to be the one created, got {:?}",
        created["Location"]["CodeLocation"]
    );
}
