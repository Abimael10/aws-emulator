// SPDX-License-Identifier: CC0-1.0

//! What the `Message` member of this service's error documents looks like on the wire.
//!
//! RULE: every error document this service answers CARRIES the error shape's `Message`
//! member — the sentence that error class words, or JSON `null` where that class words none
//! — so a client can tell "this class states no message" apart from "the message is the
//! empty string". Every exception this model declares has an optional `Message` (target
//! `ErrorMessage`, a plain string with no HTTP binding:
//! `smithy_query(query="shape",shape="ResourceNotFoundException,ValidationException,ConflictException")`
//! -> `Message: ErrorMessage`, `httpError` 404, 400 and 409), and a document that omits the
//! key reaches an SDK as the EMPTY STRING rather than as "no message", which inverts the
//! contract for a client branching on whether a refusal carries text.
//!
//! The rows differ along what the rule generalises over: the error class and its status
//! (400, 404, 409), the resource whose absence is being stated, the operation that resolved
//! it (reads and mutations alike), the form of the key inside its modelled constraints, why
//! the resource is absent (never created, deleted, held only by another region), and whether
//! the class words a sentence at all. A success body is checked too, so the rule stays
//! scoped to error documents.
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

/// The member every exception of this model declares for its sentence.
const MESSAGE_MEMBER: &str = "Message";

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

/// `name` as an SDK puts it into the URI: every character outside the unreserved set
/// (RFC 3986 `ALPHA / DIGIT / "-" / "." / "_" / "~"`) percent-encoded, which is what a
/// `@httpLabel` carrying a space or a non-ASCII letter looks like on the wire.
fn encode_label(name: &str) -> String {
    let mut encoded = String::new();
    for byte in name.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(*byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// The create body of a destination called `name` delivering into `region`: a destination
/// delivers to a target of the SAME region as the call, AS an IAM role of this account
/// (`engine::EMULATOR_ACCOUNT_ID`), which is what its `RoleArn` has to name.
fn destination_request(region: &str, name: &str) -> Value {
    json!({
        "Name": name,
        "DeliveryDestinationArn":
            format!("arn:aws:kinesis:{region}:123456789012:stream/error-document-message"),
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
async fn seed_configuration(
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

/// Assert `reply` is the expected refusal and its document reports NO sentence: the message
/// member is on the wire, as JSON `null` — an omitted key would read as the empty string,
/// and an empty string would read as a stated-but-empty sentence.
fn assert_no_sentence(case: &str, reply: &Reply, status: u16) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected HTTP {status}, got {} with body {:?}",
        reply.status, reply.body
    );
    let object = reply
        .body
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: expected a JSON object, got {:?}", reply.body));
    let message = object.get(MESSAGE_MEMBER).unwrap_or_else(|| {
        panic!(
            "case {case:?}: expected the {MESSAGE_MEMBER} member on the body, got {:?}",
            reply.body
        )
    });
    assert_eq!(
        message,
        &Value::Null,
        "case {case:?}: expected {MESSAGE_MEMBER} to be JSON null, got {message:?} in body {:?}",
        reply.body
    );
}

/// Assert `reply` is the expected refusal and its document reports a sentence: the message
/// member is a non-empty string.
fn assert_a_sentence(case: &str, reply: &Reply, status: u16) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected HTTP {status}, got {} with body {:?}",
        reply.status, reply.body
    );
    let message = reply.body.get(MESSAGE_MEMBER).and_then(Value::as_str);
    assert!(
        message.is_some_and(|sentence| !sentence.is_empty()),
        "case {case:?}: expected {MESSAGE_MEMBER} to be a non-empty string, got {:?} in body {:?}",
        reply.body.get(MESSAGE_MEMBER),
        reply.body
    );
}

/// RULE: every operation of the notification-configuration surface that resolves an event
/// type and finds no configuration states it with NO sentence — the message member on the
/// wire as JSON null — over the model's whole `EventType` value set and whichever operation
/// resolved it.
#[tokio::test]
async fn an_unconfigured_event_type_reports_no_sentence_at_all() {
    let router = emulator();

    // A destination the update can legitimately point at, so a refusal below is about the
    // configuration being absent and nothing else.
    let destination = "error.document.message";
    seed_destination(&router, "seed", REGION, destination).await;

    // The rows are the whole `EventType` value set, model-generated rather than written out
    // here (`crate::ops::types::EVENT_TYPE_VALUES`), so a value added to the model is
    // covered as soon as it is regenerated.
    for event_type in EVENT_TYPE_VALUES {
        let path = format!("/notification-configurations/{}", encode_label(event_type));
        // The operations that have to RESOLVE a configuration before they can answer. The
        // delete is not among them: it is `@idempotent`, so an event type the region holds
        // nothing for is already in the state it promises and answers 200 with no document
        // at all — `notification_configuration_delete` covers that path.
        for (operation, method, body) in [
            ("GetNotificationConfiguration", "GET", None),
            (
                "UpdateNotificationConfiguration",
                "PUT",
                Some(json!({ "DestinationName": destination })),
            ),
        ] {
            let case = format!("{operation}/{event_type}");
            let reply = call(&router, method, &path, body).await;
            assert_no_sentence(&case, &reply, 404);
        }
    }
}

/// RULE, the same answer whatever made the configuration absent: one that was created and
/// then deleted, and one that exists only in another region, are stated with no sentence
/// too, so nothing about the body depends on the record never having existed anywhere.
#[tokio::test]
async fn a_deleted_or_out_of_region_configuration_reports_no_sentence_either() {
    let router = emulator();

    let destination = "error.document.message.causes";
    seed_destination(&router, "seed", REGION, destination).await;

    // Created, then deleted: the read and the update that follow the delete are exactly
    // where a client reads "no configuration" rather than "no message".
    let deleted = EVENT_TYPE_VALUES[0];
    seed_configuration(&router, "deleted", REGION, deleted, destination).await;
    let removed = call(
        &router,
        "DELETE",
        &format!("/notification-configurations/{deleted}"),
        None,
    )
    .await;
    assert_eq!(
        removed.status, 200,
        "case \"deleted\": expected HTTP 200 from DeleteNotificationConfiguration, got {} with \
         body {:?}",
        removed.status, removed.body
    );

    // Configured in another region only: a notification configuration is regional, so this
    // region still holds none for that event type.
    let elsewhere = EVENT_TYPE_VALUES[1];
    let other = "eu-central-1";
    seed_destination(&router, "other region", other, destination).await;
    seed_configuration(&router, "other region", other, elsewhere, destination).await;

    for (why, event_type) in [("deleted", deleted), ("other region", elsewhere)] {
        let path = format!("/notification-configurations/{event_type}");
        for (operation, method, body) in [
            ("GetNotificationConfiguration", "GET", None),
            (
                "UpdateNotificationConfiguration",
                "PUT",
                Some(json!({ "DestinationName": destination })),
            ),
        ] {
            let case = format!("{operation}/{why}");
            let reply = call(&router, method, &path, body).await;
            assert_no_sentence(&case, &reply, 404);
        }
    }

    // The region that owns the out-of-region configuration still reports it, so the rows
    // above are about the region asking and not about a store that lost the record.
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
        "case \"other region/owner\": expected the owning region to resolve it, got {} with \
         body {:?}",
        owned.status, owned.body
    );
}

/// RULE: the destination surface states an absent name with no sentence as well — the same
/// rendering reaches every operation that answers this refusal, for every form of name the
/// model admits and whatever made the destination absent, not just the
/// notification-configuration read.
#[tokio::test]
async fn an_absent_destination_reports_no_sentence_on_every_operation() {
    let router = emulator();

    seed_destination(&router, "seed", REGION, "error.document.seed").await;

    // A destination that was created and then deleted: the cause of the absence is not what
    // the body reports either.
    let deleted = "error.document.deleted";
    seed_destination(&router, "deleted", REGION, deleted).await;
    let removed = call(
        &router,
        "DELETE",
        &format!("/destinations/{}", encode_label(deleted)),
        None,
    )
    .await;
    assert_eq!(
        removed.status, 200,
        "case \"deleted\": expected HTTP 200 from DeleteDestination, got {} with body {:?}",
        removed.status, removed.body
    );

    // `Name`: `@length` 1..128, `@pattern` `^[\p{L}\p{N} ._-]+$`
    // (`smithy_query(query="operation",operation="GetDestination")`). The rows differ along
    // the name's form: both ends of the length bound, a space, the punctuation the pattern
    // admits, a non-ASCII letter, and digits only.
    let longest = "d".repeat(128);
    let names: Vec<(&str, String)> = vec![
        ("one character, the length minimum", "z".to_string()),
        ("128 characters, the length maximum", longest),
        ("a space", "two words".to_string()),
        (
            "the punctuation the pattern admits",
            "dot.and-dash_1".to_string(),
        ),
        ("a non-ASCII letter", "caffè".to_string()),
        ("digits only", "0123456789".to_string()),
        ("created and then deleted", deleted.to_string()),
    ];

    for (form, name) in &names {
        let path = format!("/destinations/{}", encode_label(name));
        for (operation, method, body) in [
            ("GetDestination", "GET", None),
            (
                "UpdateDestination",
                "PUT",
                Some(json!({ "Description": "a description" })),
            ),
            ("DeleteDestination", "DELETE", None),
        ] {
            let case = format!("{operation}/{form}");
            let reply = call(&router, method, &path, body).await;
            assert_no_sentence(&case, &reply, 404);
        }
    }
}

/// RULE, the other half: an error class that DOES word a sentence reports it as a non-empty
/// string, so the rendering adds a null only where there is nothing to say. The rows span
/// the classes and statuses this service answers: a refusal stated before any handler ran, a
/// reference that resolves to nothing, a resource whose own 404 words a reason, and a
/// conflict.
#[tokio::test]
async fn an_error_class_that_words_a_sentence_still_reports_it() {
    let router = emulator();

    let destination = "error.document.message.sentences";
    seed_destination(&router, "seed", REGION, destination).await;
    let configured = EVENT_TYPE_VALUES[0];
    seed_configuration(&router, "seed", REGION, configured, destination).await;

    let rows: Vec<(&str, &str, String, Option<Value>, u16)> = vec![
        // A value outside `EventType`'s declared set, refused before the handler runs.
        (
            "an event type outside the modelled value set",
            "GET",
            "/notification-configurations/NOT_AN_EVENT_TYPE".to_string(),
            None,
            400,
        ),
        // A reference to a destination this region holds none of: the create is refused with
        // the sentence the destination's own absence is worded with.
        (
            "a create naming a destination that does not exist",
            "POST",
            "/notification-configurations".to_string(),
            Some(json!({
                "EventType": EVENT_TYPE_VALUES[2],
                "DestinationName": "error.document.absent.destination",
            })),
            400,
        ),
        // A second configuration for an event type that already has one.
        (
            "a create for an event type already configured",
            "POST",
            "/notification-configurations".to_string(),
            Some(json!({ "EventType": configured, "DestinationName": destination })),
            409,
        ),
        // The managed thing and the cloud connector are the contrast to the two bare
        // surfaces: their own 404s word a reason, so that reason is what the member carries.
        (
            "a read of a managed thing that does not exist",
            "GET",
            "/managed-things/errordocmessage0001".to_string(),
            None,
            404,
        ),
        (
            "a mutation of a managed thing that does not exist",
            "PUT",
            "/managed-things/errordocmessage0001".to_string(),
            Some(json!({ "Name": "lamp one" })),
            404,
        ),
        (
            "a read of a cloud connector that does not exist",
            "GET",
            "/cloud-connectors/errordocmessage0001".to_string(),
            None,
            404,
        ),
    ];

    for (case, method, path, body, status) in rows {
        let reply = call(&router, method, &path, body).await;
        assert_a_sentence(case, &reply, status);
    }
}

/// The rule is about ERROR documents: a success body keeps reporting only the members its
/// output shape declares, so nothing gains a message member there.
#[tokio::test]
async fn a_success_body_gains_no_message_member() {
    let router = emulator();

    let destination = "error.document.message.success";
    seed_destination(&router, "seed", REGION, destination).await;
    let event_type = EVENT_TYPE_VALUES[0];
    seed_configuration(&router, "seed", REGION, event_type, destination).await;

    for (case, path) in [
        ("the configuration read", format!("/notification-configurations/{event_type}")),
        ("the destination read", format!("/destinations/{}", encode_label(destination))),
    ] {
        let reply = call(&router, "GET", &path, None).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: expected HTTP 200, got {} with body {:?}",
            reply.status, reply.body
        );
        let keys: Vec<&String> = reply
            .body
            .as_object()
            .map(|object| object.keys().collect())
            .unwrap_or_default();
        assert!(
            !keys.iter().any(|key| key.eq_ignore_ascii_case(MESSAGE_MEMBER)),
            "case {case:?}: expected no {MESSAGE_MEMBER} member on a success body, got {keys:?}"
        );
    }
}
