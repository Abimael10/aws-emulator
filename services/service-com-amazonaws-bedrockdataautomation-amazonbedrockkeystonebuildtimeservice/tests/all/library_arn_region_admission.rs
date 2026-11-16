// SPDX-License-Identifier: CC0-1.0

//! Which `DataAutomationLibraryArn` this endpoint ADMITS, over every operation that
//! resolves one.
//!
//! THE RULE under test: a `libraryArn` addresses a library of the endpoint the request
//! reached, so an ARN the modelled `DataAutomationLibraryArn` `@pattern` admits but whose
//! Region field is not the Region the request was signed for addresses nothing this endpoint
//! can serve — it is refused with the modelled `ValidationException` (HTTP 400) and the
//! service's fixed sentence for it, DECIDED ON THE INPUT: the refusal is the same whether
//! the store holds a library under that ARN or not, and it never arrives as the 404 a client
//! reads as "create it, or treat it as deleted".
//!
//! The suites are TABLE-driven along what the rule generalises over: the OPERATION handed
//! the ARN (the library's own read, update and delete, and the three ingestion-job
//! operations bound under it), the ARN's Region (a short one, a long one, another
//! partition's), the Region the request is signed for, and whether the library the ARN names
//! exists at all.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::state;
use service_com_amazonaws_bedrockdataautomation_amazonbedrockkeystonebuildtimeservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

use crate::support::{encode_label, error_code, string_at};

/// The Region every case signs for unless it says otherwise.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header for this service's signing name (`bedrock`, per
/// `OPERATIONS.md`); the handlers read the Region out of its credential scope.
fn authorization(region: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTKEY/20240101/{region}/bedrock/aws4_request, \
         SignedHeaders=host, Signature=0123456789abcdef"
    )
}

/// The emulator under test: one shared engine, one mounted service, request authentication
/// off so the suite signs nothing for real.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// Dispatch one request signed for `region`, and return its status and decoded body.
async fn call(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    uri: &str,
    body: &Value,
) -> (u16, Value) {
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "bedrock-data-automation.amazonaws.com")
        .header("content-type", "application/json")
        .header("authorization", authorization(region))
        .body(Bytes::from(
            serde_json::to_vec(body).expect("serializable body"),
        ))
        .expect("a valid test request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let decoded = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, decoded)
}

/// A well-formed `DataAutomationLibraryArn` in `region`, naming a library no create minted:
/// the resource id is 12 characters of `[a-zA-Z0-9-]`, so the modelled pattern admits it and
/// the request reaches the handler.
fn library_arn(region: &str) -> String {
    format!("arn:aws:bedrock:{region}:000000000000:data-automation-library/abcdefabcdef")
}

/// A well-formed `DataAutomationLibraryIngestionJobArn` in `region`, for the same reason.
fn job_arn(region: &str) -> String {
    format!(
        "arn:aws:bedrock:{region}:000000000000:data-automation-library-ingestion-job/abcdefabcdef"
    )
}

/// A submission body: the members `InvokeDataAutomationLibraryIngestionJob` requires.
fn submission() -> Value {
    json!({
        "entityType": "VOCABULARY",
        "operationType": "UPSERT",
        "inputConfiguration": {
            "inlinePayload": {
                "upsertEntitiesInfo": [{
                    "vocabulary": {
                        "entityId": "admission-entity",
                        "language": "EN",
                        "phrases": [{ "text": "mockingbird" }],
                    },
                }],
            },
        },
        "outputConfiguration": { "s3Uri": "s3://bda-ingestion/output/admission" },
    })
}

/// One operation that resolves a `libraryArn`, as a call built from that ARN: its name, the
/// method and the URI the model binds it to, and the body it carries.
struct Addressed {
    operation: &'static str,
    method: &'static str,
    uri: String,
    body: Value,
}

/// Every operation that resolves a `libraryArn`, addressed at `arn`.
///
/// Source: `smithy_query(query="digest")` — the operations whose `@http` URI carries
/// `{libraryArn}` (`/data-automation-libraries/{libraryArn}/` and the ingestion-job
/// collection under it). The two entity-reading operations of that URI space are still
/// scaffolded, so they are not listed; they take the same admission check through the same
/// module when they are implemented.
fn addressed_operations(arn: &str) -> Vec<Addressed> {
    let library = format!("/data-automation-libraries/{}/", encode_label(arn));
    let jobs = format!(
        "/data-automation-libraries/{}/library-ingestion-jobs/",
        encode_label(arn)
    );
    vec![
        Addressed {
            operation: "GetDataAutomationLibrary",
            method: "POST",
            uri: library.clone(),
            body: json!({}),
        },
        Addressed {
            operation: "UpdateDataAutomationLibrary",
            method: "PUT",
            uri: library.clone(),
            body: json!({ "libraryDescription": "admission" }),
        },
        Addressed {
            operation: "DeleteDataAutomationLibrary",
            method: "DELETE",
            uri: library,
            body: json!({}),
        },
        Addressed {
            operation: "ListDataAutomationLibraryIngestionJobs",
            method: "POST",
            uri: jobs.clone(),
            body: json!({}),
        },
        Addressed {
            operation: "InvokeDataAutomationLibraryIngestionJob",
            method: "PUT",
            uri: jobs.clone(),
            body: submission(),
        },
        Addressed {
            operation: "GetDataAutomationLibraryIngestionJob",
            method: "POST",
            uri: format!("{jobs}{}", encode_label(&job_arn(REGION))),
            body: json!({}),
        },
    ]
}

/// RULE: every operation handed a `libraryArn` of another Region refuses it 400
/// `ValidationException` with the service's fixed sentence — whatever that Region is.
#[tokio::test]
async fn a_library_arn_of_another_region_is_refused_on_the_input() {
    let router = emulator();

    // The rows span the Region field the modelled pattern admits (`[a-zA-Z0-9-]*`): a short
    // label, a long one, another partition's, and one of a Region that is not an AWS Region
    // at all. None of them is the Region the request is signed for.
    let regions = ["eu-west-1", "ap-southeast-2", "us-gov-west-1", "us-east-10"];
    for region in regions {
        let arn = library_arn(region);
        for call_spec in addressed_operations(&arn) {
            let case = format!("{} in {region}", call_spec.operation);
            let (status, payload) = call(
                &router,
                REGION,
                call_spec.method,
                &call_spec.uri,
                &call_spec.body,
            )
            .await;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400, got {status} with {payload:?}"
            );
            assert_eq!(
                error_code(&payload),
                "ValidationException",
                "case {case:?}: expected a ValidationException, got {payload:?}"
            );
            assert_eq!(
                payload.get("message").and_then(Value::as_str),
                Some(state::library_arn::WRONG_REGION_MESSAGE),
                "case {case:?}: expected {:?}, got {payload:?}",
                state::library_arn::WRONG_REGION_MESSAGE
            );
        }
    }
}

/// RULE: the refusal is about the INPUT, not about the store — an ARN of another Region is
/// refused the same way whether or not this store holds a library under it, and the library
/// it names is left exactly as it was.
#[tokio::test]
async fn the_refusal_precedes_the_lookup() {
    let router = emulator();

    // A library CREATED at another endpoint: the create is signed for `eu-west-1`, so the
    // ARN it mints carries that Region.
    let (status, created) = call(
        &router,
        "eu-west-1",
        "PUT",
        "/data-automation-libraries/",
        &json!({ "libraryName": "admission_library", "libraryDescription": "held" }),
    )
    .await;
    assert_eq!(
        status, 201,
        "the create must succeed, got {status} with {created:?}"
    );
    let arn = string_at(&created, "libraryArn", "create");

    // Read from the OTHER endpoint: the library exists, and the answer is still the
    // input refusal rather than a 200 or a 404.
    let (status, payload) = call(
        &router,
        REGION,
        "POST",
        &format!("/data-automation-libraries/{}/", encode_label(&arn)),
        &json!({}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a stored library read from another endpoint\": expected HTTP 400, got \
         {status} with {payload:?}"
    );
    assert_eq!(
        payload.get("message").and_then(Value::as_str),
        Some(state::library_arn::WRONG_REGION_MESSAGE),
        "case \"a stored library read from another endpoint\": expected {:?}, got {payload:?}",
        state::library_arn::WRONG_REGION_MESSAGE
    );

    // A delete refused on the input performs nothing: the library still reads back from its
    // own endpoint, unchanged and ACTIVE.
    let (status, payload) = call(
        &router,
        REGION,
        "DELETE",
        &format!("/data-automation-libraries/{}/", encode_label(&arn)),
        &json!({}),
    )
    .await;
    assert_eq!(
        status, 400,
        "case \"a delete from another endpoint\": expected HTTP 400, got {status} with \
         {payload:?}"
    );
    let (status, read) = call(
        &router,
        "eu-west-1",
        "POST",
        &format!("/data-automation-libraries/{}/", encode_label(&arn)),
        &json!({}),
    )
    .await;
    assert_eq!(
        status, 200,
        "case \"the library's own endpoint\": expected HTTP 200, got {status} with {read:?}"
    );
    let library = read
        .get("library")
        .unwrap_or_else(|| panic!("case \"the library's own endpoint\": no library in {read:?}"));
    assert_eq!(
        string_at(library, "status", "the library's own endpoint"),
        state::LIBRARY_ACTIVE,
        "case \"the library's own endpoint\": the refused calls must have moved nothing, got \
         {library:?}"
    );
    assert_eq!(
        library.get("libraryDescription").and_then(Value::as_str),
        Some("held"),
        "case \"the library's own endpoint\": the refused update must have written nothing, \
         got {library:?}"
    );
}

/// RULE: conformance, not a fixed Region — an ARN whose Region IS the one the request is
/// signed for is admitted and resolved, so what it draws is the lookup's own answer (the
/// modelled 404 for a library no create minted) at every Region.
#[tokio::test]
async fn an_arn_of_the_endpoints_own_region_is_admitted() {
    let router = emulator();

    for region in ["us-east-1", "eu-west-1", "ap-southeast-2", "us-gov-west-1"] {
        let arn = library_arn(region);
        let (status, payload) = call(
            &router,
            region,
            "POST",
            &format!("/data-automation-libraries/{}/", encode_label(&arn)),
            &json!({}),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {region:?}: an admitted ARN naming no library is the modelled 404, got \
             {status} with {payload:?}"
        );
        assert_eq!(
            error_code(&payload),
            "ResourceNotFoundException",
            "case {region:?}: expected a ResourceNotFoundException, got {payload:?}"
        );
        assert_eq!(
            payload.get("message").and_then(Value::as_str),
            Some(state::LIBRARY_ARN_NOT_FOUND_MESSAGE),
            "case {region:?}: expected {:?}, got {payload:?}",
            state::LIBRARY_ARN_NOT_FOUND_MESSAGE
        );
    }
}
