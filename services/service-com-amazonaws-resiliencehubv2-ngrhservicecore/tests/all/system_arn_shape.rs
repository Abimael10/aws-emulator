// SPDX-License-Identifier: CC0-1.0

//! What this service answers for a `systemArn` that satisfies the modelled `Arn` pattern
//! but names no system of this service.
//!
//! The rule under test: an ARN member is resolved in two steps, and WHAT it names is settled
//! before WHETHER it is stored. `Arn`'s modelled pattern is generic — it fixes the six ARN
//! fields and admits any resource segment
//! (`smithy_query(query="operation", operation="CreateUserJourney")`: `pattern
//! ^arn:(aws|aws-cn|aws-iso|aws-iso-[a-z]{1}|aws-us-gov):[A-Za-z0-9][A-Za-z0-9_/.-]{0,62}:…`)
//! — so a value it admits may name another AWS service's resource, another kind of this
//! service's resource, or no resource kind at all. Such a value is a fault in the REQUEST:
//! every operation that resolves a `systemArn` refuses it with the modelled
//! `ValidationException` (HTTP 400) worded `ARN validation error`, before any lookup, so a
//! caller can tell "the ARN you sent is wrong" from "the system is gone". An ARN that IS a
//! system ARN of this service and names no stored system is the other answer — the modelled
//! `ResourceNotFoundException` (HTTP 404) — because where the system would live (partition,
//! Region, account) says where to look rather than which kind of resource is meant.
//!
//! The suites are TABLES crossed over two axes: the FORM the ARN takes (another kind of this
//! service's resource, a segment carrying no kind at all, a kind the segment merely starts
//! with or carries deeper in, a differently cased kind, an empty identifier, another AWS
//! service's ARN, and — on the other side of the rule — another Region, account or
//! partition), and the OPERATION that resolves it (every operation of this crate that takes
//! a `systemArn`, reading and mutating, in the body and in the query string).
//!
//! Every case drives the real wire path (`MultiServiceRouter::dispatch`), so the refusal is
//! rendered by the same seam a client reaches.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    names_resource_kind, not_found_message, refusal_resource_id, ARN_VALIDATION_MESSAGE,
    SYSTEM_RESOURCE, SYSTEM_RESOURCE_TYPE,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
const CREATE_SYSTEM: &str = "/v2/create-system";
const GET_SYSTEM: &str = "/v2/get-system";
const UPDATE_SYSTEM: &str = "/v2/update-system";
const DELETE_SYSTEM: &str = "/v2/delete-system";
const CREATE_USER_JOURNEY: &str = "/v2/create-user-journey";
const GET_USER_JOURNEY: &str = "/v2/get-user-journey";
const UPDATE_USER_JOURNEY: &str = "/v2/update-user-journey";
const DELETE_USER_JOURNEY: &str = "/v2/delete-user-journey";
const LIST_USER_JOURNEYS: &str = "/v2/list-user-journeys";

/// The Region these suites sign for.
const REGION: &str = "us-east-1";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// An identifier the journey-addressed operations send. The ARN verdict is reached before
/// anything is looked up, so which journey it names cannot change the answer.
const PROBE_JOURNEY_ID: &str = "probe-journey";

fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header whose credential scope names this service's signing name
/// (`resiliencehub`) and Region. Signature verification is off for these tests, so the value
/// only has to parse.
fn signed() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY00000000/20260101/{REGION}\
         /resiliencehub/aws4_request, SignedHeaders=host;x-amz-date, \
         Signature=0123456789abcdef"
    )
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
        .header("authorization", signed())
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

/// Create a system named `name` and answer its ARN.
async fn system_named(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(router, "POST", CREATE_SYSTEM, Some(json!({"name": name}))).await;
    assert_eq!(status, 200, "creating system {name:?} got {status} {body:?}");
    body["system"]["systemArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no system.systemArn in {body:?}"))
        .to_string()
}

/// Every operation of this crate that resolves a `systemArn`, each called with `system_arn`
/// and otherwise valid members, answered as `(operation, status, body)`.
async fn every_operation_resolving(
    router: &MultiServiceRouter,
    system_arn: &str,
) -> Vec<(&'static str, u16, Value)> {
    let mut answers = Vec::new();
    for (operation, method, uri, body) in [
        (
            "GetSystem",
            "GET",
            format!("{GET_SYSTEM}?systemArn={system_arn}"),
            None,
        ),
        (
            "UpdateSystem",
            "POST",
            UPDATE_SYSTEM.to_string(),
            Some(json!({"systemArn": system_arn, "description": "probe"})),
        ),
        (
            "DeleteSystem",
            "POST",
            DELETE_SYSTEM.to_string(),
            Some(json!({"systemArn": system_arn})),
        ),
        (
            "CreateUserJourney",
            "POST",
            CREATE_USER_JOURNEY.to_string(),
            Some(json!({"systemArn": system_arn, "name": "Probe Journey"})),
        ),
        (
            "GetUserJourney",
            "GET",
            format!("{GET_USER_JOURNEY}?systemArn={system_arn}&userJourneyId={PROBE_JOURNEY_ID}"),
            None,
        ),
        (
            "UpdateUserJourney",
            "POST",
            UPDATE_USER_JOURNEY.to_string(),
            Some(json!({
                "systemArn": system_arn,
                "userJourneyId": PROBE_JOURNEY_ID,
                "description": "probe",
            })),
        ),
        (
            "DeleteUserJourney",
            "POST",
            DELETE_USER_JOURNEY.to_string(),
            Some(json!({"systemArn": system_arn, "userJourneyId": PROBE_JOURNEY_ID})),
        ),
        (
            "ListUserJourneys",
            "GET",
            format!("{LIST_USER_JOURNEYS}?systemArn={system_arn}&maxResults=100"),
            None,
        ),
    ] {
        let (status, answered) = call(router, method, &uri, body).await;
        answers.push((operation, status, answered));
    }
    answers
}

/// The ARNs that satisfy the modelled `Arn` pattern yet name no system of this service.
///
/// Each row names the FORM that makes it so. Every value is at least 31 characters and keeps
/// the six ARN fields the pattern requires, so each reaches a handler rather than being
/// refused at the boundary.
fn not_system_arns() -> Vec<(&'static str, String)> {
    vec![
        (
            "another kind of this service's resource",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:service/11111111-1111-4111-8111-111111111111"),
        ),
        (
            "a third kind of this service's resource",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:policy/22222222-2222-4222-8222-222222222222"),
        ),
        (
            "a resource segment naming no kind at all",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:zzzzzzzzzz"),
        ),
        (
            "a resource segment carrying an id with no kind in front of it",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:fake:ffffff"),
        ),
        (
            "a kind the segment only starts with",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:systems/33333333-3333-4333-8333-333333333333"),
        ),
        (
            "the kind carried deeper in a path-shaped segment",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:resource/{SYSTEM_RESOURCE}/44444444-4444-4444-8444-444444444444"),
        ),
        (
            "the kind in another case",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:System/55555555-5555-4555-8555-555555555555"),
        ),
        (
            "the kind with no identifier after it",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SYSTEM_RESOURCE}/"),
        ),
        (
            "another AWS service's resource",
            format!("arn:aws:dynamodb:{REGION}:{ACCOUNT}:table/Orders"),
        ),
        (
            "another AWS service whose name starts with this one's",
            format!("arn:aws:resiliencehubv2:{REGION}:{ACCOUNT}:{SYSTEM_RESOURCE}/66666666-6666-4666-8666-666666666666"),
        ),
    ]
}

/// The ARNs that DO name a system of this service and simply resolve to nothing: the
/// partition, Region and account say where the system would live.
fn system_arns_naming_nothing_stored() -> Vec<(&'static str, String)> {
    vec![
        (
            "an identifier nothing minted",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{SYSTEM_RESOURCE}/never-minted"),
        ),
        (
            "a system in another Region",
            format!("arn:aws:resiliencehub:eu-central-1:{ACCOUNT}:{SYSTEM_RESOURCE}/77777777-7777-4777-8777-777777777777"),
        ),
        (
            "a system in another account",
            format!("arn:aws:resiliencehub:{REGION}:210987654321:{SYSTEM_RESOURCE}/88888888-8888-4888-8888-888888888888"),
        ),
        (
            "a system in another partition",
            format!("arn:aws-us-gov:resiliencehub:us-gov-west-1:{ACCOUNT}:{SYSTEM_RESOURCE}/99999999-9999-4999-8999-999999999999"),
        ),
    ]
}

/// RULE: a `systemArn` that names no system of this service is refused `ValidationException`
/// (HTTP 400) worded `ARN validation error` by every operation that resolves one, and the
/// refused call changes nothing.
#[tokio::test]
async fn an_arn_that_names_no_system_of_this_service_is_an_arn_validation_error() {
    let router = emulator();
    // A live system with a journey in it, so every refusal can be shown to have left the
    // store as it was.
    let live = system_named(&router, "Arn-Shape-Live").await;
    let (status, body) = call(
        &router,
        "POST",
        CREATE_USER_JOURNEY,
        Some(json!({"systemArn": live, "name": "Live Journey"})),
    )
    .await;
    assert_eq!(status, 200, "creating the live journey got {status} {body:?}");
    let live_journey = body["userJourney"]["userJourneyId"]
        .as_str()
        .expect("a userJourneyId")
        .to_string();

    for (case, arn) in not_system_arns() {
        // The helper under the handlers agrees with the wire answer for this row.
        assert!(
            !names_resource_kind(&arn, SYSTEM_RESOURCE),
            "case {case:?}: {arn:?} must not be read as a system ARN of this service"
        );
        for (operation, status, body) in every_operation_resolving(&router, &arn).await {
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 from {operation} for {arn:?}, got {status} \
                 {body:?}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, "ValidationException",
                "case {case:?}: expected ValidationException from {operation}, got {code:?} in \
                 {body:?}"
            );
            let actual = body["message"].as_str().unwrap_or("<no message>");
            assert_eq!(
                actual, ARN_VALIDATION_MESSAGE,
                "case {case:?}: expected {ARN_VALIDATION_MESSAGE:?} from {operation}, got \
                 {actual:?}"
            );
            // Fixed text: the refusal interpolates nothing from the request, so no part of
            // the value the caller sent comes back in it.
            assert!(
                !actual.contains(&arn),
                "case {case:?}: {operation} must not interpolate the ARN, got {actual:?}"
            );
        }
    }

    // The live system and its journey are exactly as they were.
    let (status, body) = call(&router, "GET", &format!("{GET_SYSTEM}?systemArn={live}"), None).await;
    assert_eq!(status, 200, "the live system must still read back, got {body:?}");
    assert_eq!(
        body["system"]["systemArn"].as_str(),
        Some(live.as_str()),
        "the live system must read back under its own ARN, got {body:?}"
    );
    let (status, body) = call(
        &router,
        "GET",
        &format!("{GET_USER_JOURNEY}?systemArn={live}&userJourneyId={live_journey}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "the live journey must still read back, got {status} {body:?}"
    );
}

/// RULE: an ARN that IS a system ARN of this service but names no stored system is the
/// other answer — `ResourceNotFoundException` (HTTP 404) naming the system by its own
/// identifier — so the two refusals stay distinguishable.
#[tokio::test]
async fn a_system_arn_naming_nothing_stored_is_still_not_found() {
    let router = emulator();
    // One stored system, so the refusals are about the ARN rather than an empty store.
    system_named(&router, "Arn-Shape-Other-Live").await;

    for (case, arn) in system_arns_naming_nothing_stored() {
        assert!(
            names_resource_kind(&arn, SYSTEM_RESOURCE),
            "case {case:?}: {arn:?} must be read as a system ARN of this service"
        );
        for (operation, status, body) in every_operation_resolving(&router, &arn).await {
            assert_eq!(
                status, 404,
                "case {case:?}: expected HTTP 404 from {operation} for {arn:?}, got {status} \
                 {body:?}"
            );
            let code = error_code(&body);
            assert_eq!(
                code, "ResourceNotFoundException",
                "case {case:?}: expected ResourceNotFoundException from {operation}, got \
                 {code:?} in {body:?}"
            );
            let identifier = refusal_resource_id(SYSTEM_RESOURCE_TYPE, &arn);
            let expected = not_found_message(SYSTEM_RESOURCE_TYPE, identifier);
            let actual = body["message"].as_str().unwrap_or("<no message>");
            assert_eq!(
                actual, expected,
                "case {case:?}: expected {expected:?} from {operation}, got {actual:?}"
            );
            assert_eq!(
                body["resourceId"].as_str(),
                Some(identifier),
                "case {case:?}: expected resourceId {identifier:?} from {operation}, got \
                 {body:?}"
            );
            assert_eq!(
                body["resourceType"].as_str(),
                Some(SYSTEM_RESOURCE_TYPE),
                "case {case:?}: expected resourceType {SYSTEM_RESOURCE_TYPE:?} from \
                 {operation}, got {body:?}"
            );
        }
    }
}
