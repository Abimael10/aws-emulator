// SPDX-License-Identifier: CC0-1.0

//! How `CreateServiceFunctionResources` WORDS its refusal of a resource the service never
//! discovered.
//!
//! The rule under test: that refusal is a 400 `ValidationException` whose `message` is the
//! service's fixed sentence `Resource not found: <identifier>. Resource must be discovered
//! before it can be added to a function.` around ONE variable part — the identifier taken
//! verbatim from the request's `resources`, whatever form it takes — so a client keying on
//! the `Resource not found: ` lead-in tells "discovery has not reached this resource yet"
//! from every other validation refusal.
//!
//! The suite is a TABLE: a row is one input of the class the rule covers — the FORM the
//! identifier takes (an ARN of any service, a bare name, a url, a path-shaped id), its SIZE
//! (one character up to a long identifier), and HOW MANY identifiers one call names, since
//! the refusal names the first one the service has not discovered in request order. The
//! identifier is a plain `String` on the wire (`smithy_query(query="shape",
//! shape="ResourceList")` — `list` of `String`, len 1..100, no pattern), so every row's
//! value reaches the handler unaltered.
//!
//! Every row drives the real wire path (`MultiServiceRouter::dispatch`), so the refusal is
//! rendered by the same seam a client reaches.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes this suite drives.
const CREATE_SERVICE: &str = "/v2/create-service";
const CREATE_SERVICE_FUNCTION: &str = "/v2/create-service-function";
const CREATE_SERVICE_FUNCTION_RESOURCES: &str = "/v2/create-service-function-resources";
const LIST_SERVICE_FUNCTIONS: &str = "/v2/list-functions";
const LIST_RESOURCES: &str = "/v2/list-resources";

/// A SigV4 `Authorization` header whose credential scope names this service's signing name
/// (`resiliencehub`). Signature verification is off for these tests, so the value only has
/// to parse.
const SIGNED: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTONLY00000000/20260101/us-east-1/resiliencehub/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// The fixed text the refusal carries before the identifier.
const LEAD_IN: &str = "Resource not found: ";

/// The fixed text the refusal carries after it.
const TRAILER: &str = ". Resource must be discovered before it can be added to a function.";

fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("a JSON body serializes"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("authorization", SIGNED)
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The modelled error code of a refusal, from the restJson1 `__type` discriminator.
fn error_code(body: &Value) -> String {
    body["__type"]
        .as_str()
        .unwrap_or("<no __type>")
        .rsplit(['#', ':'])
        .next()
        .unwrap_or("<no __type>")
        .to_string()
}

/// Create a service with no input source — so it has discovered nothing — and answer its
/// ARN together with the id of one function of it.
async fn service_with_a_function(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let (status, body) = call(
        router,
        "POST",
        CREATE_SERVICE,
        Some(json!({
            "name": name,
            "regions": ["us-east-1"],
            "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "creating service {name}: expected HTTP 200, got {status} {body:?}"
    );
    let service_arn = body["service"]["serviceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no service.serviceArn in {body:?}"))
        .to_string();

    let (status, body) = call(
        router,
        "POST",
        CREATE_SERVICE_FUNCTION,
        Some(json!({
            "serviceArn": service_arn,
            "name": "Ledger",
            "criticality": "PRIMARY",
        })),
    )
    .await;
    assert_eq!(
        status, 200,
        "creating the function: expected HTTP 200, got {status} {body:?}"
    );
    let function_id = body["serviceFunction"]["serviceFunctionId"]
        .as_str()
        .unwrap_or_else(|| panic!("no serviceFunction.serviceFunctionId in {body:?}"))
        .to_string();
    (service_arn, function_id)
}

/// The resources a function holds, as the service's own listing counts them.
async fn resource_count(
    router: &MultiServiceRouter,
    case: &str,
    service_arn: &str,
    function_id: &str,
) -> i64 {
    let (status, body) = call(
        router,
        "GET",
        &format!("{LIST_SERVICE_FUNCTIONS}?serviceArn={service_arn}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "case {case:?}: expected HTTP 200 from ListServiceFunctions, got {status} {body:?}"
    );
    body["serviceFunctions"]
        .as_array()
        .unwrap_or_else(|| panic!("case {case:?}: no serviceFunctions array in {body:?}"))
        .iter()
        .find(|function| function["serviceFunctionId"] == json!(function_id))
        .and_then(|function| function["resourceCount"].as_i64())
        .unwrap_or_else(|| panic!("case {case:?}: no resourceCount for {function_id} in {body:?}"))
}

/// One row: the identifiers a call names, and the one the refusal must name.
fn rows() -> Vec<(&'static str, Vec<String>, String)> {
    let long_identifier = format!("arn:aws:s3:::{}", "b".repeat(200));
    vec![
        (
            "an S3 ARN, the form the behavioral corpus records",
            vec!["arn:aws:s3:::rhv2-wording-bucket".to_string()],
            "arn:aws:s3:::rhv2-wording-bucket".to_string(),
        ),
        (
            "an ARN of another service, with a Region and an account",
            vec!["arn:aws:dynamodb:eu-west-2:000000000000:table/Orders".to_string()],
            "arn:aws:dynamodb:eu-west-2:000000000000:table/Orders".to_string(),
        ),
        (
            "an ARN of a non-commercial partition",
            vec!["arn:aws-us-gov:lambda:us-gov-west-1:000000000000:function:settle".to_string()],
            "arn:aws-us-gov:lambda:us-gov-west-1:000000000000:function:settle".to_string(),
        ),
        (
            "a bare resource name",
            vec!["rhv2-wording-table".to_string()],
            "rhv2-wording-table".to_string(),
        ),
        (
            "a single-character identifier",
            vec!["q".to_string()],
            "q".to_string(),
        ),
        (
            "a long identifier",
            vec![long_identifier.clone()],
            long_identifier,
        ),
        (
            "a resource url",
            vec!["https://sqs.us-east-1.amazonaws.com/000000000000/rhv2-wording".to_string()],
            "https://sqs.us-east-1.amazonaws.com/000000000000/rhv2-wording".to_string(),
        ),
        (
            "an identifier carrying the characters a sentence is read by",
            vec!["cluster/rhv2: wording. check".to_string()],
            "cluster/rhv2: wording. check".to_string(),
        ),
        (
            "two identifiers: the refusal names the first in request order",
            vec![
                "rhv2-wording-first".to_string(),
                "rhv2-wording-second".to_string(),
            ],
            "rhv2-wording-first".to_string(),
        ),
        (
            "the modelled maximum of identifiers, none of them discovered",
            (0..100)
                .map(|index| format!("rhv2-wording-{index:03}"))
                .collect(),
            "rhv2-wording-000".to_string(),
        ),
    ]
}

/// RULE: a resource the service never discovered is refused with the fixed sentence around
/// the request's own identifier, and nothing is associated.
#[tokio::test]
async fn an_undiscovered_resource_is_refused_in_aws_wording() {
    let router = emulator();
    let (service_arn, function_id) = service_with_a_function(&router, "UndiscoveredWording").await;

    for (case, resources, refused) in rows() {
        let (status, body) = call(
            &router,
            "POST",
            CREATE_SERVICE_FUNCTION_RESOURCES,
            Some(json!({
                "serviceArn": service_arn,
                "serviceFunctionId": function_id,
                "resources": resources,
            })),
        )
        .await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected HTTP 400 for an undiscovered resource, got {status} {body:?}"
        );
        let code = error_code(&body);
        assert_eq!(
            code, "ValidationException",
            "case {case:?}: expected ValidationException, got {code:?} in {body:?}"
        );

        let expected = format!("{LEAD_IN}{refused}{TRAILER}");
        let actual = body["message"].as_str().unwrap_or("<no message>");
        assert_eq!(
            actual, expected,
            "case {case:?}: expected {expected:?}, got {actual:?}"
        );
        // The sentence is fixed text around ONE variable part, so removing the identifier
        // leaves exactly the same text in every row.
        let fixed = actual.replace(&refused, "");
        assert_eq!(
            fixed,
            format!("{LEAD_IN}{TRAILER}"),
            "case {case:?}: the identifier must be the only variable part, got {actual:?}"
        );
        assert_eq!(
            body["reason"].as_str(),
            Some("INVALID_FIELD_VALUE"),
            "case {case:?}: the refusal reports the value as invalid, got {body:?}"
        );

        // A refused association stores nothing: the function still holds no resources.
        let held = resource_count(&router, case, &service_arn, &function_id).await;
        assert_eq!(
            held, 0,
            "case {case:?}: a refused association must store nothing, got {held} resources"
        );

        // And the reader over the function's resources answers an empty list, for either
        // value of `billable`: nothing was associated, so there is nothing to report.
        for billable in ["true", "false"] {
            let (status, body) = call(
                &router,
                "GET",
                &format!(
                    "{LIST_RESOURCES}?serviceArn={service_arn}\
                     &serviceFunctionId={function_id}&billable={billable}"
                ),
                None,
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: expected HTTP 200 from ListResources, got {status} {body:?}"
            );
            assert_eq!(
                body["serviceFunctionId"].as_str(),
                Some(function_id.as_str()),
                "case {case:?}: the listing names the function asked for, got {body:?}"
            );
            assert_eq!(
                body["serviceResources"],
                json!([]),
                "case {case:?}: billable={billable}: expected an empty list, got {body:?}"
            );
        }
    }
}

/// RULE: the undiscovered-resource refusal is told apart from the OTHER
/// `ValidationException` this operation words — the one for an association that would take a
/// function past the resources it can hold — so a client keying on the lead-in never reads
/// one as the other.
#[tokio::test]
async fn another_validation_refusal_is_not_worded_as_an_undiscovered_resource() {
    let router = emulator();
    let (service_arn, function_id) =
        service_with_a_function(&router, "UndiscoveredWordingOther").await;

    // One identifier past the modelled `ResourceList` ceiling of 100
    // (`smithy_query(query="shape", shape="ResourceList")` — `length {min: 1, max: 100}`) is
    // refused by the framework's own schema check, before the handler's discovery runs.
    let resources: Vec<String> = (0..101)
        .map(|index| format!("rhv2-wording-over-{index:03}"))
        .collect();
    let (status, body) = call(
        &router,
        "POST",
        CREATE_SERVICE_FUNCTION_RESOURCES,
        Some(json!({
            "serviceArn": service_arn,
            "serviceFunctionId": function_id,
            "resources": resources,
        })),
    )
    .await;
    assert_eq!(
        status, 400,
        "expected HTTP 400 past the ResourceList ceiling, got {status} {body:?}"
    );
    let actual = body["message"].as_str().unwrap_or("<no message>");
    assert!(
        !actual.starts_with(LEAD_IN),
        "a refusal that is not about discovery must not carry the undiscovered-resource \
         lead-in, got {actual:?}"
    );
}
