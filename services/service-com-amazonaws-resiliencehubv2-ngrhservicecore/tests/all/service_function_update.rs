// SPDX-License-Identifier: CC0-1.0

//! `UpdateServiceFunction`, end to end over HTTP, against the functions a service holds.
//!
//! The rule under test: an update of a child resource addressed by its parent's ARN and its
//! own id replaces exactly those editable members the request carries, leaves every member
//! it omits at the value the store already held, stamps the record as updated without
//! re-minting its identity or its creation instant, and is refused — writing nothing — when
//! the pair it addresses names nothing stored (`ResourceNotFoundException`) or when the
//! `name` it asks for is already held by another child of the same parent
//! (`ConflictException`). For a service function the editable members are `name`,
//! `description` and `criticality`; `serviceFunctionId`, `source`, `resourceCount` and
//! `createdAt` are not members of the request, so no update can move them.
//!
//! Every suite drives the real wire path (`MultiServiceRouter::dispatch`), so the modelled
//! constraints, the restJson1 (de)serializers and the HTTP status codes are exercised
//! exactly as a client would exercise them.
//!
//! The suites are TABLES: each row is one input of the class the rule covers — which
//! editable members the request carries, each member's value at its modelled bounds, a
//! value replaced versus a value first supplied, each modelled `criticality`, and which of
//! the addressing members names nothing stored — not one recorded request.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::state::{
    SERVICE_FUNCTION_RESOURCE_TYPE, SERVICE_FUNCTION_SOURCE_USER, SERVICE_RESOURCE_TYPE,
};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
const CREATE_SERVICE: &str = "/v2/create-service";
const CREATE_SERVICE_FUNCTION: &str = "/v2/create-service-function";
const UPDATE_SERVICE_FUNCTION: &str = "/v2/update-function";
const LIST_SERVICE_FUNCTIONS: &str = "/v2/list-functions";

/// The Region these suites sign for.
const REGION: &str = "us-east-1";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// `EntityLabel`'s modelled bounds, which `UpdateServiceFunction$name` targets.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdateServiceFunction")` — `name`
/// targets `EntityLabel`, `length {min: 2, max: 60}`, pattern
/// `^[A-Za-z0-9][A-Za-z0-9 _\-]{1,59}$`.
const ENTITY_LABEL_MIN_LENGTH: usize = 2;
const ENTITY_LABEL_MAX_LENGTH: usize = 60;

/// `EntityDescription`'s modelled ceiling, which `UpdateServiceFunction$description`
/// targets.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdateServiceFunction")` —
/// `description` targets `EntityDescription`, `length {max: 500}`.
const ENTITY_DESCRIPTION_MAX_LENGTH: usize = 500;

/// The values `ServiceFunctionCriticality` accepts on the wire.
///
/// SOURCE: `smithy_query(query="shape", shape="ServiceFunctionCriticality")` — an enum of
/// exactly `PRIMARY` and `SUPPLEMENTAL`.
const CRITICALITY_VALUES: [&str; 2] = ["PRIMARY", "SUPPLEMENTAL"];

/// The members of `ServiceFunction` an update can replace.
///
/// SOURCE: `smithy_query(query="operation", operation="UpdateServiceFunction")` — the
/// request members other than the `serviceArn` / `serviceFunctionId` pair that addresses
/// the function.
const EDITABLE_MEMBERS: [&str; 3] = ["name", "description", "criticality"];

/// The members a function keeps across every update: its identity, where it came from, how
/// many resources it holds and when it was created — none of them a request member.
const PRESERVED_MEMBERS: [&str; 4] = [
    "serviceFunctionId",
    "source",
    "resourceCount",
    "createdAt",
];

/// A SigV4 `Authorization` header whose credential scope names this service's signing name
/// (`resiliencehub`) and the Region ARNs are minted in. Signature verification is off for
/// these tests, so the value only has to parse.
const SIGNED: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDTESTONLY00000000/20260101/us-east-1/resiliencehub/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// An emulator holding only this service, over one shared engine.
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

/// Create a service named `name` and answer its ARN.
async fn service_named(router: &MultiServiceRouter, name: &str) -> String {
    let (status, body) = call(
        router,
        "POST",
        CREATE_SERVICE,
        Some(json!({
            "name": name,
            "regions": [REGION],
            "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
        })),
    )
    .await;
    assert_eq!(status, 200, "creating service {name:?} got {status} {body:?}");
    body["service"]["serviceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no service.serviceArn in {body:?}"))
        .to_string()
}

/// Create a function of `service_arn` from `extras` (its `name`, `criticality` and whatever
/// optional members the case supplies) and answer the `ServiceFunction` the create
/// projected.
async fn function_in(router: &MultiServiceRouter, service_arn: &str, extras: &Value) -> Value {
    let mut body = extras.clone();
    body["serviceArn"] = json!(service_arn);
    let (status, answered) = call(router, "POST", CREATE_SERVICE_FUNCTION, Some(body)).await;
    assert_eq!(
        status, 200,
        "creating a function from {extras:?} got {status} {answered:?}"
    );
    answered["serviceFunction"].clone()
}

async fn update_function(
    router: &MultiServiceRouter,
    service_arn: &str,
    service_function_id: &str,
    extras: &Value,
) -> (u16, Value) {
    let mut body = extras.clone();
    body["serviceArn"] = json!(service_arn);
    body["serviceFunctionId"] = json!(service_function_id);
    call(router, "POST", UPDATE_SERVICE_FUNCTION, Some(body)).await
}

/// Every function `service_arn` holds, as its own listing reports them.
async fn listed_functions(router: &MultiServiceRouter, case: &str, service_arn: &str) -> Vec<Value> {
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
        .clone()
}

/// The ONE function `service_arn` lists under `service_function_id` — the read every update
/// below is proven through.
async fn listed_function(
    router: &MultiServiceRouter,
    case: &str,
    service_arn: &str,
    service_function_id: &str,
) -> Value {
    listed_functions(router, case, service_arn)
        .await
        .into_iter()
        .find(|function| function["serviceFunctionId"] == json!(service_function_id))
        .unwrap_or_else(|| {
            panic!("case {case:?}: {service_function_id} is not listed under {service_arn}")
        })
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

/// The `serviceFunctionId` a projected function carries.
fn function_id_of(case: &str, function: &Value) -> String {
    function["serviceFunctionId"]
        .as_str()
        .unwrap_or_else(|| panic!("case {case:?}: no serviceFunctionId in {function:?}"))
        .to_string()
}

/// A timestamp member as the epoch-seconds number restJson1 carries it as.
fn instant_of(case: &str, function: &Value, member: &str) -> f64 {
    function[member]
        .as_f64()
        .unwrap_or_else(|| panic!("case {case:?}: no numeric {member} in {function:?}"))
}

/// RULE: an update replaces exactly the editable members the request carries and leaves
/// every other member of the stored function — the ones it omits, its id, its source, its
/// resource count and its creation instant — where they were, and the next read answers
/// what the update stored.
///
/// The rows span the class: each editable member alone, each at its modelled bound, a value
/// first supplied versus one replaced, an empty description, every modelled criticality,
/// every editable member at once, and an update that carries none of them.
#[tokio::test]
async fn an_update_replaces_only_the_members_the_request_carries() {
    let router = emulator();
    let service_arn = service_named(&router, "Payments").await;
    let shortest_name = "A".repeat(ENTITY_LABEL_MIN_LENGTH);
    let longest_name = "N".repeat(ENTITY_LABEL_MAX_LENGTH);
    let longest_description = "d".repeat(ENTITY_DESCRIPTION_MAX_LENGTH);
    let [primary, supplemental] = CRITICALITY_VALUES;

    let rows: Vec<(&str, Value, Value, Value)> = vec![
        (
            "a new name on a function carrying nothing else",
            json!({"name": "Checkout", "criticality": primary}),
            json!({"name": "Checkout v2"}),
            json!({"name": "Checkout v2", "criticality": primary}),
        ),
        (
            "a name at EntityLabel's floor",
            json!({"name": "Floor function", "criticality": primary}),
            json!({"name": shortest_name}),
            json!({"name": shortest_name, "criticality": primary}),
        ),
        (
            "a name at EntityLabel's ceiling",
            json!({"name": "Ceiling function", "criticality": supplemental}),
            json!({"name": longest_name}),
            json!({"name": longest_name, "criticality": supplemental}),
        ),
        (
            "a description on a function that had none",
            json!({"name": "Described later", "criticality": primary}),
            json!({"description": "now described"}),
            json!({"name": "Described later", "description": "now described",
                   "criticality": primary}),
        ),
        (
            "a description at EntityDescription's ceiling over a shorter one",
            json!({"name": "Described", "description": "short", "criticality": primary}),
            json!({"description": longest_description}),
            json!({"name": "Described", "description": longest_description,
                   "criticality": primary}),
        ),
        (
            "an empty description over a filled one",
            json!({"name": "Emptied", "description": "filled in", "criticality": supplemental}),
            json!({"description": ""}),
            json!({"name": "Emptied", "description": "", "criticality": supplemental}),
        ),
        (
            "criticality moved off the created value",
            json!({"name": "Downgraded", "description": "kept", "criticality": primary}),
            json!({"criticality": supplemental}),
            json!({"name": "Downgraded", "description": "kept", "criticality": supplemental}),
        ),
        (
            "criticality moved the other way",
            json!({"name": "Upgraded", "criticality": supplemental}),
            json!({"criticality": primary}),
            json!({"name": "Upgraded", "criticality": primary}),
        ),
        (
            "criticality set to the value it already holds",
            json!({"name": "Unmoved criticality", "criticality": primary}),
            json!({"criticality": primary}),
            json!({"name": "Unmoved criticality", "criticality": primary}),
        ),
        (
            "every editable member at once",
            json!({"name": "All edited", "description": "before", "criticality": primary}),
            json!({"name": "All edited v2", "description": "after", "criticality": supplemental}),
            json!({"name": "All edited v2", "description": "after",
                   "criticality": supplemental}),
        ),
        (
            "an update carrying no editable member at all",
            json!({"name": "Untouched", "description": "kept", "criticality": supplemental}),
            json!({}),
            json!({"name": "Untouched", "description": "kept", "criticality": supplemental}),
        ),
    ];

    for (case, create_extras, update_extras, expected) in rows {
        let created = function_in(&router, &service_arn, &create_extras).await;
        let function_id = function_id_of(case, &created);

        let (status, answered) =
            update_function(&router, &service_arn, &function_id, &update_extras).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected HTTP 200, got {status} {answered:?}"
        );
        let updated = answered["serviceFunction"].clone();

        for member in EDITABLE_MEMBERS {
            // A member the row expects no value for is one neither the create nor the update
            // ever supplied, so the projection must WITHHOLD it rather than report a value of
            // its own: that is asserted as absence, not as one null equalling another.
            match expected.get(member) {
                Some(value) => assert_eq!(
                    &updated[member], value,
                    "case {case:?}: expected {member} to be {value:?}, got {:?}",
                    updated[member]
                ),
                None => assert!(
                    updated.get(member).is_none(),
                    "case {case:?}: expected no {member} to be reported, got {:?}",
                    updated[member]
                ),
            }
        }
        for member in PRESERVED_MEMBERS {
            assert_eq!(
                updated[member], created[member],
                "case {case:?}: expected {member} to stay {:?}, got {:?}",
                created[member], updated[member]
            );
        }
        assert_eq!(
            updated["serviceArn"].as_str(),
            Some(service_arn.as_str()),
            "case {case:?}: expected the parent service's ARN, got {:?}",
            updated["serviceArn"]
        );
        assert_eq!(
            updated["source"].as_str(),
            Some(SERVICE_FUNCTION_SOURCE_USER),
            "case {case:?}: a caller-created function stays caller-sourced, got {:?}",
            updated["source"]
        );
        let created_at = instant_of(case, &updated, "createdAt");
        let updated_at = instant_of(case, &updated, "updatedAt");
        assert!(
            updated_at >= created_at,
            "case {case:?}: expected updatedAt at or after createdAt {created_at}, got \
             {updated_at}"
        );

        // The read answers the same projection the update did: the store holds the edit.
        let from_list = listed_function(&router, case, &service_arn, &function_id).await;
        assert_eq!(
            from_list, updated,
            "case {case:?}: ListServiceFunctions must answer what the update stored, got \
             {from_list:?}"
        );
    }
}

/// RULE: an update whose addressing pair names nothing stored is refused
/// `ResourceNotFoundException` and writes nothing — a missing parent is reported as the
/// missing service, and an id the addressed service does not hold as the missing function,
/// another service's function included.
#[tokio::test]
async fn an_update_naming_nothing_stored_is_refused_and_writes_nothing() {
    let router = emulator();
    let service_arn = service_named(&router, "Addressed").await;
    let other_service_arn = service_named(&router, "Other").await;
    let stored = function_in(
        &router,
        &service_arn,
        &json!({"name": "Stored", "description": "as created", "criticality": "PRIMARY"}),
    )
    .await;
    let function_id = function_id_of("setup", &stored);

    // A well-formed `Arn` (the modelled `length 31..` and pattern are enforced at the
    // boundary) whose resource segment names a service nothing created.
    let missing_service_resource = "service/11111111-1111-4111-8111-111111111111";
    let missing_service_arn =
        format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:{missing_service_resource}");

    // Each row carries the whole refusal the model declares: the message, the identifier of
    // the resource that was not found — the ARN's resource segment for a service, which is
    // what a service reports as its identifier, and the id itself for a function, which is
    // addressed by one — and the type of that resource.
    let rows: [(&str, &str, &str, String, &str, &str); 3] = [
        (
            "a serviceArn naming no stored service",
            missing_service_arn.as_str(),
            function_id.as_str(),
            format!("{SERVICE_RESOURCE_TYPE} not found: {missing_service_resource}"),
            missing_service_resource,
            SERVICE_RESOURCE_TYPE,
        ),
        (
            "the function of another service",
            other_service_arn.as_str(),
            function_id.as_str(),
            format!("Service function not found: {function_id}"),
            function_id.as_str(),
            SERVICE_FUNCTION_RESOURCE_TYPE,
        ),
        (
            "a serviceFunctionId nothing minted",
            service_arn.as_str(),
            "no-such-function",
            "Service function not found: no-such-function".to_string(),
            "no-such-function",
            SERVICE_FUNCTION_RESOURCE_TYPE,
        ),
    ];

    for (case, addressed_service, addressed_id, expected_message, expected_id, expected_type) in
        rows
    {
        let (status, body) = update_function(
            &router,
            addressed_service,
            addressed_id,
            &json!({"name": "Renamed by a refused call", "description": "refused",
                    "criticality": "SUPPLEMENTAL"}),
        )
        .await;
        assert_eq!(
            status, 404,
            "case {case:?}: expected HTTP 404, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ResourceNotFoundException",
            "case {case:?}: expected ResourceNotFoundException, got {body:?}"
        );
        assert_eq!(
            body["message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the refusal {expected_message:?}, got {:?}",
            body["message"]
        );
        assert_eq!(
            body["resourceId"].as_str(),
            Some(expected_id),
            "case {case:?}: expected resourceId {expected_id:?}, got {:?}",
            body["resourceId"]
        );
        assert_eq!(
            body["resourceType"].as_str(),
            Some(expected_type),
            "case {case:?}: expected resourceType {expected_type:?}, got {:?}",
            body["resourceType"]
        );
    }

    // None of those refusals touched the stored function.
    let from_list = listed_function(&router, "after the refusals", &service_arn, &function_id).await;
    assert_eq!(
        from_list, stored,
        "a refused update must leave the function as it was, got {from_list:?}"
    );
    let bystanders = listed_functions(&router, "the other service", &other_service_arn).await;
    assert!(
        bystanders.is_empty(),
        "a refused update must store nothing under the service it addressed, got {bystanders:?}"
    );
}

/// RULE: a child's name identifies it within its parent, so an update asking for a name
/// another child of the same parent already holds is refused `ConflictException` and writes
/// nothing, while the name a function already holds, and a name held only by another
/// service's function, are no conflict with anything.
#[tokio::test]
async fn a_rename_onto_a_taken_name_is_refused_and_a_rename_onto_its_own_is_not() {
    let router = emulator();
    let service_arn = service_named(&router, "Renames").await;
    let other_service_arn = service_named(&router, "Elsewhere").await;

    // The rows differ along the SHAPE of the taken name within `EntityLabel`: a plain one,
    // one carrying the spaces, underscore and hyphen the pattern admits, one at the floor
    // and one at the ceiling.
    let taken_names = [
        "Checkout".to_string(),
        "Order intake_v2-beta".to_string(),
        "A".repeat(ENTITY_LABEL_MIN_LENGTH),
        "Z".repeat(ENTITY_LABEL_MAX_LENGTH),
    ];

    for (index, taken) in taken_names.iter().enumerate() {
        let case = format!("the taken name {taken:?}");
        let sibling = function_in(
            &router,
            &service_arn,
            &json!({"name": taken, "criticality": "PRIMARY"}),
        )
        .await;
        let mover = function_in(
            &router,
            &service_arn,
            &json!({"name": format!("Mover {index}"), "description": "as created",
                    "criticality": "SUPPLEMENTAL"}),
        )
        .await;
        let mover_id = function_id_of(&case, &mover);

        // Another service holding that same name is not a conflict here, so the refusal
        // below is about this service's sibling and nothing else.
        let bystander = function_in(
            &router,
            &other_service_arn,
            &json!({"name": taken, "criticality": "PRIMARY"}),
        )
        .await;
        let bystander_id = function_id_of(&case, &bystander);

        let (status, body) =
            update_function(&router, &service_arn, &mover_id, &json!({"name": taken})).await;
        assert_eq!(
            status, 409,
            "case {case:?}: expected HTTP 409, got {status} {body:?}"
        );
        assert_eq!(
            error_code(&body),
            "ConflictException",
            "case {case:?}: expected ConflictException, got {body:?}"
        );
        let expected_message =
            format!("{SERVICE_FUNCTION_RESOURCE_TYPE} with name already exists: {taken}");
        assert_eq!(
            body["message"].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected the refusal {expected_message:?}, got {:?}",
            body["message"]
        );

        // The refused rename wrote nothing: neither the function it addressed nor the
        // sibling whose name it asked for moved.
        let from_list = listed_function(&router, &case, &service_arn, &mover_id).await;
        assert_eq!(
            from_list, mover,
            "case {case:?}: a refused rename must leave the function as it was, got \
             {from_list:?}"
        );
        let sibling_id = function_id_of(&case, &sibling);
        let sibling_now = listed_function(&router, &case, &service_arn, &sibling_id).await;
        assert_eq!(
            sibling_now, sibling,
            "case {case:?}: a refused rename must leave the sibling as it was, got \
             {sibling_now:?}"
        );

        // Renaming a function to the name it already holds is not a conflict with itself,
        // and it may still carry an edit of another member.
        let (status, answered) = update_function(
            &router,
            &service_arn,
            &sibling_id,
            &json!({"name": taken, "description": "renamed onto itself"}),
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: renaming a function to its own name is served, got {status} \
             {answered:?}"
        );
        assert_eq!(
            answered["serviceFunction"]["name"].as_str(),
            Some(taken.as_str()),
            "case {case:?}: expected the name to stay {taken:?}, got {:?}",
            answered["serviceFunction"]["name"]
        );
        assert_eq!(
            answered["serviceFunction"]["description"].as_str(),
            Some("renamed onto itself"),
            "case {case:?}: the other member of the same request is still applied, got {:?}",
            answered["serviceFunction"]["description"]
        );

        // The other service's function, which holds that same name, was reached by none of
        // this: a name is taken only within one service.
        let elsewhere = listed_function(&router, &case, &other_service_arn, &bystander_id).await;
        assert_eq!(
            elsewhere, bystander,
            "case {case:?}: another service's function named {taken:?} must be untouched, got \
             {elsewhere:?}"
        );

        // Once the sibling vacates the name, the mover takes it: the index of taken names
        // follows the store rather than being consulted once at creation.
        let vacated = format!("Vacated {index}");
        let (status, answered) =
            update_function(&router, &service_arn, &sibling_id, &json!({"name": vacated})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: renaming the sibling off {taken:?} is served, got {status} \
             {answered:?}"
        );
        let (status, answered) =
            update_function(&router, &service_arn, &mover_id, &json!({"name": taken})).await;
        assert_eq!(
            status, 200,
            "case {case:?}: the freed name {taken:?} is available, got {status} {answered:?}"
        );
        let moved = listed_function(&router, &case, &service_arn, &mover_id).await;
        assert_eq!(
            moved["name"].as_str(),
            Some(taken.as_str()),
            "case {case:?}: expected the mover to hold {taken:?}, got {:?}",
            moved["name"]
        );
        assert_eq!(
            listed_function(&router, &case, &service_arn, &sibling_id)
                .await["name"]
                .as_str(),
            Some(vacated.as_str()),
            "case {case:?}: expected the sibling to hold {vacated:?} after it moved off \
             {taken:?}"
        );
    }
}
