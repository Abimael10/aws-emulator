// SPDX-License-Identifier: CC0-1.0

//! What `DeleteNotificationConfiguration` does to the store.
//!
//! RULE: a delete REMOVES the notification configuration the caller's region holds for the
//! `EventType` the URI names — answering the modelled 200 with no output members
//! (`smithy_query(query="operation",operation="DeleteNotificationConfiguration")`: `DELETE
//! /notification-configurations/{EventType}`, output `Unit`, success 200, `idempotent`
//! true) — so every later read of that event type resolves nothing and the event type can
//! be configured again. Being `@idempotent`, the delete promises only the post-condition
//! "this region holds no configuration for this event type": a call for an event type that
//! already satisfies it has nothing left to do and answers the same 200 with an empty body,
//! never a refusal. A delete is regional, as every resource of this service is: it removes
//! the record of the region the request is signed for and leaves another region's alone.
//!
//! The rows span the model's whole `EventType` value set and both ends of the round trip:
//! create -> delete -> read, delete again, re-create, and a configuration of another region
//! that the delete must not touch — plus every way an event type can already hold nothing
//! (never configured, configured and deleted, deleted repeatedly, configured only in
//! another region).
//!
//! Every case drives the real restJson1 request path (`MultiServiceRouter::dispatch`), so
//! the statuses and bodies asserted here are the ones a client sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::ops::types::{
    DELIVERY_DESTINATION_TYPE_VALUES, EVENT_TYPE_VALUES,
};
use service_com_amazonaws_iotmanagedintegrations_iotmanagedintegrations::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region the test's requests are signed for unless a case names another.
const REGION: &str = "us-east-1";

/// One HTTP answer, reduced to what the assertions read.
struct Reply {
    status: u16,
    body: Value,
}

/// A router holding one `iotmanagedintegrations` service over a fresh shared engine: the
/// same assembly `core/router` performs, with one service mounted.
fn emulator() -> MultiServiceRouter {
    let mut router = MultiServiceRouter::new();
    // The signature itself is not under test here, so requests are unauthenticated.
    router.set_request_authentication(false);
    router.register_service(EmulatedService::new(Arc::new(engine::InMemoryEngine::new())));
    router
}

/// Send one request, signed for `region`, and reduce the answer. The `Authorization`
/// credential scope is what routes the request to this service and names its region.
async fn call_in(
    router: &MultiServiceRouter,
    region: &str,
    method: &str,
    path: &str,
    body: Option<Value>,
) -> Reply {
    let payload = body.map_or_else(bytes::Bytes::new, |value| {
        bytes::Bytes::from(serde_json::to_vec(&value).expect("serialize the request body"))
    });
    let request = http::Request::builder()
        .method(method)
        .uri(format!(
            "https://iotmanagedintegrations.{region}.amazonaws.com{path}"
        ))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDTESTTESTTESTTEST/20260101/{region}/\
                 iotmanagedintegrations/aws4_request, SignedHeaders=host, Signature=0"
            ),
        )
        .header("content-type", "application/json")
        .body(payload)
        .expect("a well-formed request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let body = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    Reply { status, body }
}

/// [`call_in`] in [`REGION`].
async fn call(router: &MultiServiceRouter, method: &str, path: &str, body: Option<Value>) -> Reply {
    call_in(router, REGION, method, path, body).await
}

/// The create body of a destination called `name` delivering into `region`: a destination
/// delivers to a target of the SAME region as the call, AS an IAM role of this account
/// (`engine::EMULATOR_ACCOUNT_ID`), which is what its `RoleArn` has to name.
fn destination_request(region: &str, name: &str) -> Value {
    json!({
        "Name": name,
        "DeliveryDestinationArn":
            format!("arn:aws:kinesis:{region}:123456789012:stream/notification-delete"),
        "DeliveryDestinationType": DELIVERY_DESTINATION_TYPE_VALUES[0],
        "RoleArn":
            format!("arn:aws:iam::{}:role/managed-integrations", engine::EMULATOR_ACCOUNT_ID),
    })
}

/// Create the destination `name` in `region`, asserting the modelled 201.
async fn seed_destination(router: &MultiServiceRouter, case: &str, region: &str, name: &str) {
    let created = call_in(
        router,
        region,
        "POST",
        "/destinations",
        Some(destination_request(region, name)),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from CreateDestination, got {} with body {:?}",
        created.status, created.body
    );
}

/// Create the notification configuration for `event_type` in `region`, asserting the
/// modelled 201.
async fn create_configuration(
    router: &MultiServiceRouter,
    case: &str,
    region: &str,
    event_type: &str,
    destination: &str,
) {
    let created = call_in(
        router,
        region,
        "POST",
        "/notification-configurations",
        Some(json!({ "EventType": event_type, "DestinationName": destination })),
    )
    .await;
    assert_eq!(
        created.status, 201,
        "case {case:?}: expected HTTP 201 from CreateNotificationConfiguration, got {} with \
         body {:?}",
        created.status, created.body
    );
}

/// RULE: over the model's whole `EventType` value set, a configuration that was created
/// resolves, its delete answers the modelled 200 with no output members, and the read that
/// follows resolves nothing — and the event type can then be configured again.
#[tokio::test]
async fn a_delete_removes_the_configuration_and_the_event_type_can_be_configured_again() {
    let router = emulator();

    let destination = "notification.delete.round.trip";
    seed_destination(&router, "seed", REGION, destination).await;

    // The rows are the whole `EventType` value set, model-generated rather than written out
    // here (`crate::ops::types::EVENT_TYPE_VALUES`), so a value added to the model is
    // covered as soon as it is regenerated.
    for event_type in EVENT_TYPE_VALUES {
        let path = format!("/notification-configurations/{event_type}");
        create_configuration(&router, event_type, REGION, event_type, destination).await;

        let read = call(&router, "GET", &path, None).await;
        assert_eq!(
            read.status, 200,
            "case {event_type:?}: expected the created configuration to resolve, got {} with \
             body {:?}",
            read.status, read.body
        );

        let deleted = call(&router, "DELETE", &path, None).await;
        assert_eq!(
            deleted.status, 200,
            "case {event_type:?}: expected HTTP 200 from the delete, got {} with body {:?}",
            deleted.status, deleted.body
        );
        assert!(
            is_empty_body(&deleted.body),
            "case {event_type:?}: expected the delete to report no output members, got {:?}",
            deleted.body
        );

        let gone = call(&router, "GET", &path, None).await;
        assert_eq!(
            gone.status, 404,
            "case {event_type:?}: expected the deleted configuration to resolve to nothing, \
             got {} with body {:?}",
            gone.status, gone.body
        );

        // A second delete of the same event type has nothing left to remove, and the
        // post-condition it promises already holds, so it answers the same success.
        let again = call(&router, "DELETE", &path, None).await;
        assert_eq!(
            again.status, 200,
            "case {event_type:?}: expected a second delete to succeed as a no-op, got {} with \
             body {:?}",
            again.status, again.body
        );
        assert!(
            is_empty_body(&again.body),
            "case {event_type:?}: expected the second delete to report no output members, got \
             {:?}",
            again.body
        );

        // The event type is free again, which is what "the record was removed" means to a
        // caller: a create for it is accepted rather than refused as a conflict.
        create_configuration(
            &router,
            &format!("{event_type}/re-created"),
            REGION,
            event_type,
            destination,
        )
        .await;
        let recreated = call(&router, "GET", &path, None).await;
        assert_eq!(
            recreated.status, 200,
            "case {event_type:?}: expected the re-created configuration to resolve, got {} \
             with body {:?}",
            recreated.status, recreated.body
        );
    }
}

/// True when the answer carried no output members: an empty body, or a body that is an
/// empty JSON object. The modelled output is `Unit`, so both are "no members".
fn is_empty_body(body: &Value) -> bool {
    body.is_null() || body.as_object().is_some_and(serde_json::Map::is_empty)
}

/// RULE: a delete whose post-condition already holds succeeds as a no-op (200, no output
/// members) rather than being refused, whatever made the event type unconfigured.
///
/// The rows differ along the reason the region holds nothing for the event type — never
/// configured, configured then deleted, deleted several times over, held only by another
/// region — and the never-configured row is driven over the model's whole `EventType` value
/// set (`crate::ops::types::EVENT_TYPE_VALUES`).
#[tokio::test]
async fn a_delete_of_an_unconfigured_event_type_succeeds_as_a_no_op() {
    let router = emulator();

    let destination = "notification.delete.idempotent";
    seed_destination(&router, "seed", REGION, destination).await;

    // Never configured: every value the model admits, so no row depends on a particular
    // event type, and three consecutive deletes of each, so repetition changes nothing.
    for event_type in EVENT_TYPE_VALUES {
        let path = format!("/notification-configurations/{event_type}");
        for attempt in 1..=3 {
            let case = format!("never configured/{event_type}/attempt {attempt}");
            let reply = call(&router, "DELETE", &path, None).await;
            assert_eq!(
                reply.status, 200,
                "case {case:?}: expected HTTP 200 from the delete, got {} with body {:?}",
                reply.status, reply.body
            );
            assert!(
                is_empty_body(&reply.body),
                "case {case:?}: expected no output members and no error payload, got {:?}",
                reply.body
            );
        }
        // The no-op stored nothing: the event type still resolves to nothing afterwards.
        let read = call(&router, "GET", &path, None).await;
        assert_eq!(
            read.status, 404,
            "case {event_type:?}: expected the event type to still resolve to nothing after a \
             no-op delete, got {} with body {:?}",
            read.status, read.body
        );
    }

    // Configured and then deleted, then deleted twice more: the first delete removes the
    // record and each later one is the same success, so the second of two identical deletes
    // reads exactly like the first.
    let configured = EVENT_TYPE_VALUES[0];
    let path = format!("/notification-configurations/{configured}");
    create_configuration(&router, "deleted", REGION, configured, destination).await;
    for attempt in 1..=3 {
        let case = format!("configured then deleted/attempt {attempt}");
        let reply = call(&router, "DELETE", &path, None).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected HTTP 200 from the delete, got {} with body {:?}",
            reply.status, reply.body
        );
        assert!(
            is_empty_body(&reply.body),
            "case {case:?}: expected no output members and no error payload, got {:?}",
            reply.body
        );
    }

    // Configured only in another region: a notification configuration is regional, so this
    // region holds nothing for that event type, its delete is the same no-op, and the owning
    // region's record survives it.
    let elsewhere = EVENT_TYPE_VALUES[1];
    let other = "eu-central-1";
    seed_destination(&router, "other region", other, destination).await;
    create_configuration(&router, "other region", other, elsewhere, destination).await;
    let reply = call(
        &router,
        "DELETE",
        &format!("/notification-configurations/{elsewhere}"),
        None,
    )
    .await;
    assert_eq!(
        reply.status, 200,
        "case \"other region only\": expected HTTP 200 from the delete, got {} with body {:?}",
        reply.status, reply.body
    );
    assert!(
        is_empty_body(&reply.body),
        "case \"other region only\": expected no output members and no error payload, got {:?}",
        reply.body
    );
    let owned = call_in(
        &router,
        other,
        "GET",
        &format!("/notification-configurations/{elsewhere}"),
        None,
    )
    .await;
    assert_eq!(
        owned.status, 200,
        "case \"other region only\": expected the owning region to still resolve it, got {} \
         with body {:?}",
        owned.status, owned.body
    );
}

/// RULE: a delete is regional — it removes the record of the region the request is signed
/// for, and a configuration another region holds for the same event type is untouched.
#[tokio::test]
async fn a_delete_removes_only_the_calling_regions_configuration() {
    let router = emulator();

    let destination = "notification.delete.regional";
    let other = "eu-central-1";
    seed_destination(&router, "home", REGION, destination).await;
    seed_destination(&router, "other", other, destination).await;

    // Two event types, each configured in both regions: deleting one region's record must
    // leave the other region's record for the same event type, and the home region's record
    // for the other event type, in place.
    let deleted = EVENT_TYPE_VALUES[0];
    let kept = EVENT_TYPE_VALUES[1];
    for (region, label) in [(REGION, "home"), (other, "other")] {
        for event_type in [deleted, kept] {
            create_configuration(
                &router,
                &format!("{label}/{event_type}"),
                region,
                event_type,
                destination,
            )
            .await;
        }
    }

    let removed = call(
        &router,
        "DELETE",
        &format!("/notification-configurations/{deleted}"),
        None,
    )
    .await;
    assert_eq!(
        removed.status, 200,
        "case \"home delete\": expected HTTP 200, got {} with body {:?}",
        removed.status, removed.body
    );

    for (case, region, event_type, expected) in [
        ("home/deleted", REGION, deleted, 404),
        ("home/kept", REGION, kept, 200),
        ("other/deleted", other, deleted, 200),
        ("other/kept", other, kept, 200),
    ] {
        let reply = call_in(
            &router,
            region,
            "GET",
            &format!("/notification-configurations/{event_type}"),
            None,
        )
        .await;
        assert_eq!(
            reply.status, expected,
            "case {case:?}: expected HTTP {expected}, got {} with body {:?}",
            reply.status, reply.body
        );
    }
}
