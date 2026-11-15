// SPDX-License-Identifier: CC0-1.0

//! What this service requires of a wireless-gateway-task-definition create, and what a
//! delete makes of the definition afterwards, end to end over HTTP.
//!
//! Every case drives the real `restJson1` wire path — `MultiServiceRouter::dispatch`
//! with a signed `http::Request` — so what is asserted is what an SDK client sees, not a
//! handler return value.
//!
//! The rules under test, each asserted over a SET of inputs rather than one recorded
//! request:
//!
//! * a create that omits a member the service requires — whatever the model marks
//!   `@required` — is refused with 400 `ValidationException`, the error shape named in
//!   `x-amzn-errortype` and the sentence `Parameter <memberName> is required.` under the
//!   modelled `Message` member, and stores nothing: the same request carrying that
//!   member is accepted, so it is the member's absence that is refused and nothing else;
//! * a refusal consumes no `@idempotencyToken`, so the token is still free afterwards;
//! * a delete REMOVES the record, so every reader of it refuses that `Id` from the next
//!   call on, with the 404 `ResourceNotFoundException` this service words for a resource
//!   kind addressed by an `Id` the account does not hold — the kind and the requested id
//!   on the shape's own members — while every other stored definition survives;
//! * deleting a definition leaves a gateway task already created from it standing, which
//!   is what the operation documents;
//! * a deleted definition's `ClientRequestToken` is released with it, so replaying that
//!   token mints a new definition rather than replaying the deleted one.
//!
//! The wire harness is the one the sibling suites already have: `tests/all/*.rs` are
//! modules of ONE test target, so `router`, `call`, `error_code`, `is_uuid_shaped` and
//! `call_with_headers` are shared from there rather than copied.

use http::{Method, StatusCode};
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use super::destination_and_wireless_device_lifecycle::{call, error_code, is_uuid_shaped, router};
use super::multicast_group_update_and_delete::{call_with_headers, Answer};

/// The resource kind this service names a task definition by in a not-found refusal.
/// Source: `smithy_query(query="operation",
/// operation="GetWirelessGatewayTaskDefinition")` binds the resource at
/// `/wireless-gateway-task-definitions/{Id}`; the kind is the shape name the service
/// reports on `ResourceNotFoundException$ResourceType`.
const RESOURCE_TYPE: &str = "WirelessGatewayTaskDefinition";

/// The input member `CreateWirelessGatewayTaskDefinition` requires beyond the ones the
/// model marks `@required`: the firmware-update payload the definition exists to
/// describe.
/// Source: `smithy_query(query="operation",
/// operation="CreateWirelessGatewayTaskDefinition")` — member `Update`, target
/// `UpdateWirelessGatewayTaskCreate`.
const REQUIRED_MEMBER: &str = "Update";

/// The member the modelled `ValidationException` carries its text in.
/// Source: `smithy_query(query="shape", shape="ValidationException")` — one member,
/// `Message`, with no `@jsonName`.
const MODELLED_MESSAGE_MEMBER: &str = "Message";

/// An `Id` the published `WirelessGatewayTaskDefinitionId` pattern admits but no create
/// in this suite ever minted.
const UNMINTED_ID: &str = "33333333-4444-5555-6666-777777777777";

/// The sentence this service words when a request omits a member it requires: fixed text
/// apart from the member name, which is the omitted input member as the model spells it.
fn missing_parameter_message(member: &str) -> String {
    format!("Parameter {member} is required.")
}

/// The sentence this service words for a resource kind addressed by an `Id` the account
/// does not hold.
fn not_found_message(resource_type: &str, id: &str) -> String {
    format!(
        "Cannot complete the step because resource is not found, with resource type \
         {resource_type}, resource Id {id}"
    )
}

async fn create_definition(router: &MultiServiceRouter, body: Value) -> Answer {
    call_with_headers(
        router,
        Method::POST,
        "/wireless-gateway-task-definitions",
        Some(body),
    )
    .await
}

async fn get_definition(router: &MultiServiceRouter, id: &str) -> Answer {
    call_with_headers(
        router,
        Method::GET,
        &format!("/wireless-gateway-task-definitions/{id}"),
        None,
    )
    .await
}

async fn delete_definition(router: &MultiServiceRouter, id: &str) -> Answer {
    call_with_headers(
        router,
        Method::DELETE,
        &format!("/wireless-gateway-task-definitions/{id}"),
        None,
    )
    .await
}

/// Seed one definition from `body` and return the `Id` the create minted.
async fn seed_definition(router: &MultiServiceRouter, body: Value) -> String {
    let answered = create_definition(router, body).await;
    assert_eq!(
        answered.status,
        StatusCode::CREATED,
        "seeding a definition: expected 201, got {} {}",
        answered.status,
        answered.body
    );
    answered.body["Id"]
        .as_str()
        .expect("the create mints an Id")
        .to_string()
}

/// Every `Update` payload shape the suite seeds a definition with, so no rule is
/// asserted on one request shape alone: the whole member set, each half on its own, and
/// the member present but empty.
fn update_payloads() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a full update",
            json!({
                "UpdateDataSource": "s3://gateway-firmware/station-2.0.6.bin",
                "UpdateDataRole": "arn:aws:iam::000000000000:role/iotwireless-update",
                "LoRaWAN": {
                    "SigKeyCrc": 4_294_967_295_i64,
                    "CurrentVersion": { "Station": "2.0.5", "Model": "SX1301" },
                    "UpdateVersion": { "Station": "2.0.6", "Model": "SX1301" },
                },
            }),
        ),
        (
            "the S3 half only",
            json!({ "UpdateDataSource": "s3://gateway-firmware/station-2.0.7.bin" }),
        ),
        (
            "the LoRaWAN half only",
            json!({ "LoRaWAN": { "CurrentVersion": { "PackageVersion": "1.0.0" } } }),
        ),
        ("the member present but empty", json!({})),
    ]
}

/// The bodies a create is refused for, each differing in what it carries BESIDES the
/// member it leaves out — so what is refused is the absence of that member and not the
/// shape of the rest of the request.
fn bodies_without_the_required_member() -> Vec<(&'static str, Value)> {
    vec![
        (
            "only the modelled-required member",
            json!({ "AutoCreateTasks": false }),
        ),
        (
            "AutoCreateTasks true and a name",
            json!({ "AutoCreateTasks": true, "Name": "firmware-rollout" }),
        ),
        (
            "a name and tags",
            json!({
                "AutoCreateTasks": false,
                "Name": "tagged-definition",
                "Tags": [{ "Key": "stage", "Value": "test" }],
            }),
        ),
        (
            "an idempotency token",
            json!({
                "AutoCreateTasks": true,
                "Name": "tokened-definition",
                "ClientRequestToken": "token-for-the-refusal-case",
            }),
        ),
        (
            "a name at its published ceiling",
            json!({ "AutoCreateTasks": false, "Name": "x".repeat(2048) }),
        ),
    ]
}

/// A create that omits the member the service requires is refused in the modelled
/// envelope and stores nothing, while the SAME body carrying that member is accepted and
/// reads back.
#[tokio::test]
async fn a_create_that_omits_a_required_member_is_refused() {
    let expected_message = missing_parameter_message(REQUIRED_MEMBER);

    for (case, body) in bodies_without_the_required_member() {
        let router = router();
        let refused = create_definition(&router, body.clone()).await;
        assert_eq!(
            refused.status,
            StatusCode::BAD_REQUEST,
            "case {case:?}: expected 400 for a body with no {REQUIRED_MEMBER}, got {} {}",
            refused.status,
            refused.body
        );
        assert_eq!(
            error_code(&refused.body),
            "ValidationException",
            "case {case:?}: expected ValidationException, got {}",
            refused.body
        );
        assert_eq!(
            refused.error_type, "ValidationException",
            "case {case:?}: expected the error shape in x-amzn-errortype, got {:?}",
            refused.error_type
        );
        assert_eq!(
            refused.body[MODELLED_MESSAGE_MEMBER].as_str(),
            Some(expected_message.as_str()),
            "case {case:?}: expected {expected_message:?} under \
             {MODELLED_MESSAGE_MEMBER:?}, got {}",
            refused.body
        );
        assert!(
            refused.body.get("Id").is_none() && refused.body.get("Arn").is_none(),
            "case {case:?}: a refused create must mint nothing, got {}",
            refused.body
        );

        // The same request with the member present is accepted and reads back, so the
        // refusal turns on that member's absence alone.
        let mut accepted_body = body.clone();
        accepted_body
            .as_object_mut()
            .expect("an object body")
            .insert(REQUIRED_MEMBER.to_string(), update_payloads()[0].1.clone());
        let accepted = create_definition(&router, accepted_body).await;
        assert_eq!(
            accepted.status,
            StatusCode::CREATED,
            "case {case:?}: expected 201 once {REQUIRED_MEMBER} is carried, got {} {}",
            accepted.status,
            accepted.body
        );
        let id = accepted.body["Id"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: the create must mint an Id"))
            .to_string();
        assert!(
            is_uuid_shaped(&id),
            "case {case:?}: expected a uuid-shaped Id, got {id:?}"
        );
        let fetched = get_definition(&router, &id).await;
        assert_eq!(
            fetched.status,
            StatusCode::OK,
            "case {case:?}: expected the accepted definition to read back, got {} {}",
            fetched.status,
            fetched.body
        );
    }
}

/// A refusal consumes no `@idempotencyToken`: the token the refused create carried still
/// mints a definition on the next call, and replaying it then returns that one.
#[tokio::test]
async fn a_refused_create_leaves_its_idempotency_token_free() {
    let router = router();
    let token = "token-left-free-by-the-refusal";

    let refused = create_definition(
        &router,
        json!({ "AutoCreateTasks": false, "ClientRequestToken": token }),
    )
    .await;
    assert_eq!(
        refused.status,
        StatusCode::BAD_REQUEST,
        "expected 400 for a body with no {REQUIRED_MEMBER}, got {} {}",
        refused.status,
        refused.body
    );

    let body = json!({
        "AutoCreateTasks": false,
        "Name": "after-the-refusal",
        "ClientRequestToken": token,
        "Update": { "UpdateDataSource": "s3://gateway-firmware/station-2.0.6.bin" },
    });
    let accepted = create_definition(&router, body.clone()).await;
    assert_eq!(
        accepted.status,
        StatusCode::CREATED,
        "expected the same token to mint after the refusal, got {} {}",
        accepted.status,
        accepted.body
    );
    let replay = create_definition(&router, body).await;
    assert_eq!(
        replay.body["Id"], accepted.body["Id"],
        "expected the replay to return the minted Id {:?}, got {:?}",
        accepted.body["Id"], replay.body["Id"]
    );
}

/// A delete removes the definition: it answers 204 with no body, every reader refuses
/// that `Id` afterwards, a second delete of the same `Id` is refused too, and every other
/// stored definition survives.
#[tokio::test]
async fn a_deleted_definition_is_gone_from_every_reader() {
    let router = router();

    // One definition per `Update` shape, all on one store, so a delete is asserted to
    // touch exactly its own record.
    let mut seeded: Vec<(&'static str, String)> = Vec::new();
    for (case, update) in update_payloads() {
        let id = seed_definition(
            &router,
            json!({
                "AutoCreateTasks": true,
                "Name": format!("definition-for-{case}"),
                "Update": update,
            }),
        )
        .await;
        seeded.push((case, id));
    }

    for index in 0..seeded.len() {
        let (case, id) = seeded[index].clone();

        let deleted = delete_definition(&router, &id).await;
        assert_eq!(
            deleted.status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204 from the delete, got {} {}",
            deleted.status,
            deleted.body
        );
        assert!(
            deleted.body.as_object().is_some_and(|body| body.is_empty()),
            "case {case:?}: expected a body with no members from the delete, got {}",
            deleted.body
        );

        // The reader refuses it from the next call on.
        assert_not_found(case, "the read", &id, &get_definition(&router, &id).await);
        // And so does a second delete of the same id.
        assert_not_found(
            case,
            "the second delete",
            &id,
            &delete_definition(&router, &id).await,
        );

        // Every definition not yet deleted is untouched.
        for (surviving_case, surviving_id) in &seeded[index + 1..] {
            let fetched = get_definition(&router, surviving_id).await;
            assert_eq!(
                fetched.status,
                StatusCode::OK,
                "case {case:?}: expected the definition for {surviving_case:?} to \
                 survive, got {} {}",
                fetched.status,
                fetched.body
            );
            assert_eq!(
                fetched.body["Name"],
                json!(format!("definition-for-{surviving_case}")),
                "case {case:?}: expected the definition for {surviving_case:?} to keep \
                 its own Name, got {}",
                fetched.body
            );
        }
    }
}

/// An `Id` the store never held is refused by the delete in the same envelope as by the
/// read, and an `Id` outside the published pattern never reaches the store.
#[tokio::test]
async fn deleting_a_definition_nothing_holds_is_not_found() {
    let router = router();
    let held = seed_definition(
        &router,
        json!({ "AutoCreateTasks": false, "Name": "held", "Update": {} }),
    )
    .await;

    // (case, the Id addressed, the status the delete must answer)
    let rows: Vec<(&str, String, StatusCode)> = vec![
        (
            "a well-formed id nothing holds",
            UNMINTED_ID.to_string(),
            StatusCode::NOT_FOUND,
        ),
        (
            "an uppercase-hex id nothing holds",
            UNMINTED_ID.to_uppercase(),
            StatusCode::NOT_FOUND,
        ),
        (
            "not a uuid",
            "not-a-task-definition".to_string(),
            StatusCode::BAD_REQUEST,
        ),
        (
            "one group too short",
            "11111111-2222-3333-4444-55555555555".to_string(),
            StatusCode::BAD_REQUEST,
        ),
    ];

    for (case, id, expected) in rows {
        let answered = delete_definition(&router, &id).await;
        assert_eq!(
            answered.status, expected,
            "case {case:?}: expected {expected} for Id {id:?}, got {} {}",
            answered.status, answered.body
        );
        match expected {
            StatusCode::NOT_FOUND => assert_not_found(case, "the delete", &id, &answered),
            StatusCode::BAD_REQUEST => assert_eq!(
                error_code(&answered.body),
                "ValidationException",
                "case {case:?}: expected ValidationException, got {}",
                answered.body
            ),
            _ => {}
        }
    }

    // None of the refusals touched the definition the store does hold.
    let fetched = get_definition(&router, &held).await;
    assert_eq!(
        fetched.status,
        StatusCode::OK,
        "expected the held definition to survive every refusal, got {} {}",
        fetched.status,
        fetched.body
    );
}

/// Deleting a definition leaves a gateway task already created from it standing: the
/// operation documents that the delete does not affect tasks that are currently in
/// progress.
#[tokio::test]
async fn deleting_a_definition_leaves_a_task_created_from_it_standing() {
    let router = router();

    for (case, update) in update_payloads() {
        let (status, gateway) = call(
            &router,
            Method::POST,
            "/wireless-gateways",
            Some(json!({ "Name": format!("gateway-for-{case}"), "LoRaWAN": {} })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: expected 201 from the gateway create, got {status} {gateway}"
        );
        let gateway_id = gateway["Id"].as_str().expect("a gateway Id").to_string();

        let definition_id = seed_definition(
            &router,
            json!({ "AutoCreateTasks": false, "Update": update }),
        )
        .await;
        let (status, task) = call(
            &router,
            Method::POST,
            &format!("/wireless-gateways/{gateway_id}/tasks"),
            Some(json!({ "WirelessGatewayTaskDefinitionId": definition_id })),
        )
        .await;
        assert_eq!(
            status,
            StatusCode::CREATED,
            "case {case:?}: expected 201 from the task create, got {status} {task}"
        );

        let deleted = delete_definition(&router, &definition_id).await;
        assert_eq!(
            deleted.status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204 from the delete, got {} {}",
            deleted.status,
            deleted.body
        );

        let (status, held) = call(
            &router,
            Method::GET,
            &format!("/wireless-gateways/{gateway_id}/tasks"),
            None,
        )
        .await;
        assert_eq!(
            status,
            StatusCode::OK,
            "case {case:?}: expected the task to survive the delete, got {status} {held}"
        );
        assert_eq!(
            held["WirelessGatewayTaskDefinitionId"],
            json!(definition_id),
            "case {case:?}: expected the task to still name its definition, got {held}"
        );
    }
}

/// A deleted definition's `ClientRequestToken` is released with it: replaying that token
/// mints a NEW definition rather than replaying the deleted one.
#[tokio::test]
async fn a_deleted_definition_releases_its_idempotency_token() {
    let router = router();

    for (case, update) in update_payloads() {
        let body = json!({
            "AutoCreateTasks": false,
            "Name": format!("token-{case}"),
            "ClientRequestToken": format!("token-{}", case.replace(' ', "-")),
            "Update": update,
        });
        let first = seed_definition(&router, body.clone()).await;

        let deleted = delete_definition(&router, &first).await;
        assert_eq!(
            deleted.status,
            StatusCode::NO_CONTENT,
            "case {case:?}: expected 204 from the delete, got {} {}",
            deleted.status,
            deleted.body
        );

        let replayed = create_definition(&router, body).await;
        assert_eq!(
            replayed.status,
            StatusCode::CREATED,
            "case {case:?}: expected the replayed token to mint again, got {} {}",
            replayed.status,
            replayed.body
        );
        let second = replayed.body["Id"].as_str().expect("a minted Id");
        assert_ne!(
            second, first,
            "case {case:?}: expected a fresh Id for the replay after the delete, got the \
             deleted {first:?}"
        );
        assert_eq!(
            get_definition(&router, second).await.status,
            StatusCode::OK,
            "case {case:?}: expected the re-minted definition to read back"
        );
        assert_not_found(
            case,
            "the read of the deleted definition",
            &first,
            &get_definition(&router, &first).await,
        );
    }
}

/// The refusal every operation that addresses a task definition gives for an `Id` the
/// account does not hold: 404 `ResourceNotFoundException`, the error shape named in
/// `x-amzn-errortype`, the kind and the requested id on the shape's own members, and the
/// sentence this service words for that error class.
fn assert_not_found(case: &str, operation: &str, id: &str, answered: &Answer) {
    assert_eq!(
        answered.status,
        StatusCode::NOT_FOUND,
        "case {case:?}: expected 404 from {operation}, got {} {}",
        answered.status,
        answered.body
    );
    assert_eq!(
        error_code(&answered.body),
        "ResourceNotFoundException",
        "case {case:?}: expected ResourceNotFoundException from {operation}, got {}",
        answered.body
    );
    assert_eq!(
        answered.error_type, "ResourceNotFoundException",
        "case {case:?}: expected {operation} to name the error shape in x-amzn-errortype, \
         got {:?}",
        answered.error_type
    );
    assert_eq!(
        answered.body["ResourceType"].as_str(),
        Some(RESOURCE_TYPE),
        "case {case:?}: expected ResourceType {RESOURCE_TYPE:?} from {operation}, got {}",
        answered.body
    );
    assert_eq!(
        answered.body["ResourceId"].as_str(),
        Some(id),
        "case {case:?}: expected ResourceId {id:?} from {operation}, got {}",
        answered.body
    );
    let expected = not_found_message(RESOURCE_TYPE, id);
    assert_eq!(
        answered.body[MODELLED_MESSAGE_MEMBER].as_str(),
        Some(expected.as_str()),
        "case {case:?}: expected {expected:?} from {operation}, got {}",
        answered.body
    );
}
