// SPDX-License-Identifier: CC0-1.0

//! The REFERENCE between a service and a system, end to end over HTTP: what an
//! `associatedSystems` entry has to resolve to, and what a live association does to the
//! system's own delete.
//!
//! The rules under test:
//!
//! 1. An `associatedSystems` entry's `systemArn` is a REFERENCE to one of the account's
//!    systems, so `CreateService` and `UpdateService` resolve it against the store before
//!    anything is written: an ARN the store holds no system under — whatever form it takes,
//!    including the ARN of a system that was deleted — is refused `ValidationException`
//!    (HTTP 400) naming the value the request supplied, and the stored record is untouched.
//! 2. A system is deletable only while nothing references it: `DeleteSystem` is refused
//!    `ConflictException` (HTTP 409) while any stored service lists it among its
//!    `associatedSystems`, and succeeds — echoing the ARN it removed, after which every
//!    reader answers `ResourceNotFoundException` (HTTP 404) naming the system's own
//!    `systemId` — once the last association is released, however it was released.
//!
//! Each suite is a TABLE whose rows differ along what the rule generalises over (the form
//! the unresolvable reference takes; how the association was formed, how many systems it
//! spans, and how it is released), not one recorded request. Every case drives the real
//! wire path (`MultiServiceRouter::dispatch`), so the modelled constraints, the restJson1
//! (de)serializers and the HTTP status codes are exercised as a client would exercise them.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_resiliencehubv2_ngrhservicecore::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The modelled routes these suites drive.
///
/// SOURCE: `smithy_query(query="operation", operation="CreateSystem,DeleteSystem,
/// CreateService,UpdateService,DeleteService,GetService,ListServices")` — each operation's
/// `http` binding.
const CREATE_SYSTEM: &str = "/v2/create-system";
const GET_SYSTEM: &str = "/v2/get-system";
const DELETE_SYSTEM: &str = "/v2/delete-system";
const CREATE_SERVICE: &str = "/v2/create-service";
const UPDATE_SERVICE: &str = "/v2/update-service";
const DELETE_SERVICE: &str = "/v2/delete-service";
const GET_SERVICE: &str = "/v2/get-service";
const LIST_SERVICES: &str = "/v2/list-services";

/// The Region these suites sign for, and so the Region every minted ARN names.
const REGION: &str = "us-east-1";

/// The account every emulated ARN carries.
const ACCOUNT: &str = "000000000000";

/// An emulator holding only this service, over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(engine));
    router
}

/// A SigV4 `Authorization` header whose credential scope names this service's signing name
/// (`resiliencehub`) and [`REGION`]. Signature verification is off for these tests, so the
/// value only has to parse.
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

/// Create a system named `name`; answer its `systemArn` and its own `systemId`.
async fn system_named(router: &MultiServiceRouter, name: &str) -> (String, String) {
    let (status, body) = call(router, "POST", CREATE_SYSTEM, Some(json!({"name": name}))).await;
    assert_eq!(status, 200, "creating system {name}: got {status} {body:?}");
    let system = &body["system"];
    (
        system["systemArn"]
            .as_str()
            .unwrap_or_else(|| panic!("no system.systemArn in {body:?}"))
            .to_string(),
        system["systemId"]
            .as_str()
            .unwrap_or_else(|| panic!("no system.systemId in {body:?}"))
            .to_string(),
    )
}

/// The simplest body satisfying `CreateService`'s required members, optionally carrying
/// `associatedSystems`.
fn service_body(name: &str, associated: Option<Value>) -> Value {
    let mut body = json!({
        "name": name,
        "regions": [REGION],
        "permissionModel": {"invokerRoleName": "ResilienceHubRole"},
    });
    if let Some(associated) = associated {
        body["associatedSystems"] = associated;
    }
    body
}

/// Create a service and answer its `serviceArn`.
async fn service_named(
    router: &MultiServiceRouter,
    name: &str,
    associated: Option<Value>,
) -> String {
    let (status, body) = call(
        router,
        "POST",
        CREATE_SERVICE,
        Some(service_body(name, associated)),
    )
    .await;
    assert_eq!(status, 200, "creating service {name}: got {status} {body:?}");
    body["service"]["serviceArn"]
        .as_str()
        .unwrap_or_else(|| panic!("no service.serviceArn in {body:?}"))
        .to_string()
}

async fn get_service(router: &MultiServiceRouter, service_arn: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("{GET_SERVICE}?serviceArn={service_arn}"),
        None,
    )
    .await
}

async fn get_system(router: &MultiServiceRouter, system_arn: &str) -> (u16, Value) {
    call(
        router,
        "GET",
        &format!("{GET_SYSTEM}?systemArn={system_arn}"),
        None,
    )
    .await
}

async fn update_service(router: &MultiServiceRouter, body: Value) -> (u16, Value) {
    call(router, "POST", UPDATE_SERVICE, Some(body)).await
}

async fn delete_system(router: &MultiServiceRouter, system_arn: &str) -> (u16, Value) {
    call(
        router,
        "POST",
        DELETE_SYSTEM,
        Some(json!({"systemArn": system_arn})),
    )
    .await
}

/// The `serviceArn`s `ListServices` reports for the services in `system_arn`.
async fn services_in_system(router: &MultiServiceRouter, system_arn: &str) -> Vec<String> {
    let (status, body) = call(
        router,
        "GET",
        &format!("{LIST_SERVICES}?systemArn={system_arn}"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "listing the services in {system_arn}: got {status} {body:?}"
    );
    body["serviceSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("no serviceSummaries in {body:?}"))
        .iter()
        .map(|summary| {
            summary["serviceArn"]
                .as_str()
                .unwrap_or_else(|| panic!("no serviceArn in {summary:?}"))
                .to_string()
        })
        .collect()
}

/// RULE 1: an `associatedSystems` entry's `systemArn` names one of the account's systems,
/// so a create or an update carrying one the store holds no system under is refused
/// `ValidationException` (HTTP 400) naming the value as the request sent it, and nothing is
/// written.
///
/// The rows differ along the FORM the unresolvable reference takes: a resource segment
/// carrying no resource kind at all, a kind other than a system, an id nothing minted, a
/// live system's id with characters appended, and — the reference that resolved until a
/// moment ago — the ARN of a system this store deleted.
#[tokio::test]
async fn an_association_to_a_system_the_store_does_not_hold_is_refused() {
    let router = emulator();
    let (live_arn, _) = system_named(&router, "Live-System").await;
    let (doomed_arn, _) = system_named(&router, "Doomed-System").await;
    let (status, removed) = delete_system(&router, &doomed_arn).await;
    assert_eq!(status, 200, "deleting the doomed system got {removed:?}");

    // The service under test already carries one resolvable association, so a refused call
    // can be shown to have left the stored record exactly as it was.
    let holder = service_named(
        &router,
        "Reference-Holder",
        Some(json!([{"systemArn": live_arn}])),
    )
    .await;
    let (_, before) = get_service(&router, &holder).await;

    let dangling: Vec<(&str, String)> = vec![
        (
            "a resource segment naming no resource kind",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:zzzzzz"),
        ),
        (
            "an ARN of another resource kind",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:service/not-a-system"),
        ),
        (
            "a system id nothing minted",
            format!("arn:aws:resiliencehub:{REGION}:{ACCOUNT}:system/no-such-system"),
        ),
        (
            "a live system's id with characters appended",
            format!("{live_arn}zz"),
        ),
        ("the ARN of a deleted system", doomed_arn.clone()),
    ];

    for (case, system_arn) in dangling {
        let associated = json!([{"systemArn": system_arn}]);
        let expected = format!("System not found: {system_arn}");
        for (operation, answer) in [
            (
                "CreateService",
                call(
                    &router,
                    "POST",
                    CREATE_SERVICE,
                    Some(service_body("Refused-Create", Some(associated.clone()))),
                )
                .await,
            ),
            (
                "UpdateService",
                update_service(
                    &router,
                    json!({"serviceArn": holder, "associatedSystems": associated}),
                )
                .await,
            ),
        ] {
            let (status, body) = answer;
            assert_eq!(
                status, 400,
                "case {case:?}: expected HTTP 400 from {operation}, got {status} {body:?}"
            );
            assert_eq!(
                error_code(&body),
                "ValidationException",
                "case {case:?}: expected ValidationException from {operation}, got {body:?}"
            );
            assert_eq!(
                body["message"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected the refusal {expected:?} from {operation}, got {:?}",
                body["message"]
            );
        }

        // The refused update wrote nothing: the holder still reports the association it was
        // created with, and no refused create is listed in the live system.
        let (status, after) = get_service(&router, &holder).await;
        assert_eq!(
            (status, &after),
            (200, &before),
            "case {case:?}: a refused call must leave the stored service untouched, got \
             {after:?}"
        );
        assert_eq!(
            services_in_system(&router, &live_arn).await,
            vec![holder.clone()],
            "case {case:?}: a refused create must store no service"
        );
    }

    // The positive control the rule is bounded by: the same member naming the live system
    // resolves, is stored, and reads back.
    let accepted = json!([{"systemArn": live_arn}]);
    let (status, updated) = update_service(
        &router,
        json!({"serviceArn": holder, "associatedSystems": accepted.clone()}),
    )
    .await;
    assert_eq!(status, 200, "the resolvable association got {updated:?}");
    assert_eq!(
        updated["service"]["associatedSystems"], accepted,
        "a resolvable association must be stored as sent, got {updated:?}"
    );
    let (_, read) = get_service(&router, &holder).await;
    assert_eq!(
        read["service"]["associatedSystems"], accepted,
        "a later read must report the stored association, got {read:?}"
    );
}

/// How a row releases the association that blocks the system's delete.
#[derive(Debug, Clone, Copy)]
enum Release {
    /// Delete the service that holds it.
    DeleteTheService,
    /// Update the service onto no associations at all.
    AssociateNothing,
    /// Update the service onto another system, which must then block ITS delete instead.
    AssociateAnotherSystem,
}

/// RULE 2: a system a stored service lists among its `associatedSystems` is in use, so
/// `DeleteSystem` is refused `ConflictException` (HTTP 409) and the system stays readable;
/// once the last association is released the same call succeeds, echoes the ARN it removed,
/// and every later reader answers `ResourceNotFoundException` (HTTP 404) naming the
/// system's own `systemId`.
///
/// The rows differ along how the association was formed (by the create, by a later update),
/// how many systems one service holds (one, two), and how the last association is released
/// (the service is deleted, the member is cleared, the service moves to another system).
#[tokio::test]
async fn a_system_is_deletable_only_once_no_service_references_it() {
    let rows: Vec<(&str, bool, usize, Release)> = vec![
        (
            "associated by the create, released by deleting the service",
            false,
            1,
            Release::DeleteTheService,
        ),
        (
            "associated by a later update, released by clearing the member",
            true,
            1,
            Release::AssociateNothing,
        ),
        (
            "associated by the create, released by moving to another system",
            false,
            1,
            Release::AssociateAnotherSystem,
        ),
        (
            "two systems held at once, released by deleting the service",
            false,
            2,
            Release::DeleteTheService,
        ),
        (
            "two systems held by a later update, released by clearing the member",
            true,
            2,
            Release::AssociateNothing,
        ),
    ];

    for (case, by_update, held, release) in rows {
        let router = emulator();
        let mut systems = Vec::new();
        for index in 0..held {
            systems.push(system_named(&router, &format!("Held-System-{index}")).await);
        }
        let (spare_arn, spare_id) = system_named(&router, "Spare-System").await;
        let associated: Vec<Value> = systems
            .iter()
            .map(|(arn, _)| json!({"systemArn": arn}))
            .collect();

        let service = if by_update {
            let service = service_named(&router, "Holder-Service", None).await;
            let (status, body) = update_service(
                &router,
                json!({"serviceArn": service, "associatedSystems": associated}),
            )
            .await;
            assert_eq!(
                status, 200,
                "case {case:?}: associating by update got {status} {body:?}"
            );
            service
        } else {
            service_named(&router, "Holder-Service", Some(json!(associated))).await
        };

        // Every held system reports the service as one of its own, and refuses its delete
        // while it does.
        for (system_arn, system_id) in &systems {
            assert_eq!(
                services_in_system(&router, system_arn).await,
                vec![service.clone()],
                "case {case:?}: the held system must list the service"
            );
            let (status, refused) = delete_system(&router, system_arn).await;
            assert_eq!(
                status, 409,
                "case {case:?}: expected HTTP 409 while the service holds {system_arn}, got \
                 {status} {refused:?}"
            );
            assert_eq!(
                error_code(&refused),
                "ConflictException",
                "case {case:?}: expected ConflictException, got {refused:?}"
            );
            let expected = format!("Cannot delete system with associated services: {system_id}");
            assert_eq!(
                refused["message"].as_str(),
                Some(expected.as_str()),
                "case {case:?}: expected the refusal {expected:?}, got {:?}",
                refused["message"]
            );
            // The refused delete removed nothing.
            let (status, body) = get_system(&router, system_arn).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the refused delete must leave {system_arn} readable, got \
                 {status} {body:?}"
            );
        }
        // A system no service references is deletable all along, whatever the held ones do.
        let (status, removed) = delete_system(&router, &spare_arn).await;
        assert_eq!(
            status, 200,
            "case {case:?}: an unreferenced system must be deletable, got {status} {removed:?}"
        );
        assert_eq!(
            removed["systemArn"].as_str(),
            Some(spare_arn.as_str()),
            "case {case:?}: the delete must echo the ARN it removed, got {removed:?}"
        );
        let (status, gone) = get_system(&router, &spare_arn).await;
        let expected = format!("System not found: {spare_id}");
        assert_eq!(
            (status, gone["message"].as_str()),
            (404, Some(expected.as_str())),
            "case {case:?}: the deleted spare system must be gone by its own id, got {status} \
             {gone:?}"
        );

        match release {
            Release::DeleteTheService => {
                let (status, body) = call(
                    &router,
                    "POST",
                    DELETE_SERVICE,
                    Some(json!({"serviceArn": service})),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: deleting the service got {status} {body:?}"
                );
            }
            Release::AssociateNothing => {
                let (status, body) = update_service(
                    &router,
                    json!({"serviceArn": service, "associatedSystems": []}),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: clearing the member got {status} {body:?}"
                );
            }
            Release::AssociateAnotherSystem => {
                let (moved_arn, moved_id) = system_named(&router, "Moved-Onto-System").await;
                let (status, body) = update_service(
                    &router,
                    json!({
                        "serviceArn": service,
                        "associatedSystems": [{"systemArn": moved_arn}],
                    }),
                )
                .await;
                assert_eq!(
                    status, 200,
                    "case {case:?}: moving the service got {status} {body:?}"
                );
                // The reference moved with the service: the system it moved ONTO now blocks
                // its own delete.
                let (status, refused) = delete_system(&router, &moved_arn).await;
                assert_eq!(
                    status, 409,
                    "case {case:?}: the system moved onto must block its delete, got {status} \
                     {refused:?}"
                );
                let expected = format!("Cannot delete system with associated services: {moved_id}");
                assert_eq!(
                    refused["message"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the refusal {expected:?}, got {:?}",
                    refused["message"]
                );
            }
        }

        // Nothing references the held systems any more: each one lists no service, deletes,
        // echoes its ARN, and is gone from every reader by its own id.
        for (system_arn, system_id) in &systems {
            assert_eq!(
                services_in_system(&router, system_arn).await.len(),
                0,
                "case {case:?}: the released system must list no service"
            );
            let (status, deleted) = delete_system(&router, system_arn).await;
            assert_eq!(
                status, 200,
                "case {case:?}: the delete must succeed once nothing references {system_arn}, \
                 got {status} {deleted:?}"
            );
            assert_eq!(
                deleted["systemArn"].as_str(),
                Some(system_arn.as_str()),
                "case {case:?}: the delete must echo the ARN it removed, got {deleted:?}"
            );
            let expected = format!("System not found: {system_id}");
            for (reader, answer) in [
                ("GetSystem", get_system(&router, system_arn).await),
                ("DeleteSystem", delete_system(&router, system_arn).await),
            ] {
                let (status, body) = answer;
                assert_eq!(
                    status, 404,
                    "case {case:?}: expected HTTP 404 from {reader} after the delete, got \
                     {status} {body:?}"
                );
                assert_eq!(
                    error_code(&body),
                    "ResourceNotFoundException",
                    "case {case:?}: expected ResourceNotFoundException from {reader}, got \
                     {body:?}"
                );
                assert_eq!(
                    body["message"].as_str(),
                    Some(expected.as_str()),
                    "case {case:?}: expected the refusal {expected:?} from {reader}, got {:?}",
                    body["message"]
                );
            }
        }
    }
}
