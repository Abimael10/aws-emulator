// SPDX-License-Identifier: CC0-1.0

//! That the emulator AS ASSEMBLED resolves an event bus's KMS key through KMS.
//!
//! RULE: an EventBridge v2 event bus whose `EncryptionConfiguration$KmsKeyIdentifier` names
//! a key KMS does not hold is refused `403`, and one naming a key KMS holds is served — so
//! the answer depends on a second service, and holds only in an emulator where both are
//! mounted on one engine.
//!
//! The EventBridge v2 crate's own suite (`tests/all/event_bus_kms_key.rs` there) pins the
//! error shape and the whole `Message` of that refusal, over a router it builds itself. What
//! THIS suite adds is the assembly: the two services are reached the way a client reaches
//! them in the shipped router — the `eventsv2` endpoint host for one, the `kms` credential
//! scope for the other — so a request addressed at this endpoint reaches this service, and
//! the KMS it asks is the one the router mounts. Only the STATUS is read here, because the
//! wording is the other suite's subject.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here signs for. A KMS key is a regional resource, so the bus and
/// the key it names live in the same Region.
const REGION: &str = "us-west-2";

/// The long-term key every emulator publishes, which is the account's own ambient identity.
const AMBIENT_KEY: &str = "AKIAIOSFODNN7EXAMPLE";

/// The `X-Amz-Target` prefix of KMS's awsJson1_1 wire.
const KMS_TARGET_PREFIX: &str = "TrentService";

/// The two services this rule spans, over ONE engine, as `core/router/src/lib.rs` mounts
/// them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // Nothing here signs: the `Authorization` header carries only the credential scope,
    // which is where a service reads the Region and the caller's access key id.
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_eventbridgev2_awseventsv2::EmulatedService::new(Arc::clone(&engine)),
    );
    router.register_service(service_com_amazonaws_kms_trentservice::EmulatedService::new(
        engine,
    ));
    router
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential={AMBIENT_KEY}/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One KMS call on its awsJson1_1 wire, returning `(status, body)`.
async fn kms(router: &MultiServiceRouter, operation: &str, request: &Value) -> (u16, Value) {
    let built = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("kms.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{KMS_TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization("kms"))
        .body(Bytes::from(
            serde_json::to_vec(request).expect("the request body serializes"),
        ))
        .expect("the test request builds");
    let response = router.dispatch(built).await;
    let status = response.status().as_u16();
    let raw = response.into_body();
    let parsed = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).to_string()))
    };
    (status, parsed)
}

/// One `CreateEventBus` on the `eventsv2` endpoint, returning the status and the answer as
/// text for a failure message.
async fn create_event_bus(
    router: &MultiServiceRouter,
    name: &str,
    kms_key_identifier: &str,
) -> (u16, String) {
    let built = http::Request::builder()
        .method("POST")
        .uri("/service/AWSEventsV2/operation/CreateEventBus")
        // The endpoint HOST is what tells the router this is the v2 endpoint: its SigV4
        // signing name (`events`) is shared with the EventBridge endpoint.
        .header("host", format!("eventsv2.{REGION}.amazonaws.com"))
        .header("content-type", "application/cbor")
        .header("smithy-protocol", "rpc-v2-cbor")
        .header("authorization", authorization("events"))
        .body(Bytes::from(create_event_bus_body(name, kms_key_identifier)))
        .expect("the test request builds");
    let response = router.dispatch(built).await;
    let status = response.status().as_u16();
    let body = response.into_body();
    (status, String::from_utf8_lossy(&body).to_string())
}

/// The `rpcv2Cbor` body of a `CreateEventBus` naming `name` and encrypted under
/// `kms_key_identifier`.
///
/// Written rather than read: the members are definite-length CBOR text strings and maps,
/// which is how `rpcv2Cbor` carries a structure. Nothing here decodes a body — the one thing
/// this suite reads off an answer is its status.
fn create_event_bus_body(name: &str, kms_key_identifier: &str) -> Vec<u8> {
    let mut body = Vec::new();
    write_head(&mut body, 5, 3);
    write_text(&mut body, "Name");
    write_text(&mut body, name);
    write_text(&mut body, "ClientToken");
    write_text(&mut body, &format!("token-for-{name}"));
    write_text(&mut body, "EncryptionConfiguration");
    write_head(&mut body, 5, 1);
    write_text(&mut body, "KmsKeyIdentifier");
    write_text(&mut body, kms_key_identifier);
    body
}

/// A CBOR text string.
fn write_text(body: &mut Vec<u8>, text: &str) {
    write_head(body, 3, text.len() as u64);
    body.extend_from_slice(text.as_bytes());
}

/// The initial byte, and any length bytes after it, of a CBOR item of major type `major`
/// carrying the argument `value`.
///
/// Every width CBOR defines for an argument, so a value is never truncated into a head too
/// narrow to hold it.
fn write_head(body: &mut Vec<u8>, major: u8, value: u64) {
    let major = major << 5;
    match value {
        0..=23 => body.push(major | value as u8),
        24..=0xFF => body.extend_from_slice(&[major | 24, value as u8]),
        0x100..=0xFFFF => {
            body.push(major | 25);
            body.extend_from_slice(&(value as u16).to_be_bytes());
        }
        0x1_0000..=0xFFFF_FFFF => {
            body.push(major | 26);
            body.extend_from_slice(&(value as u32).to_be_bytes());
        }
        _ => {
            body.push(major | 27);
            body.extend_from_slice(&value.to_be_bytes());
        }
    }
}

#[tokio::test]
async fn the_assembled_emulator_resolves_an_event_bus_key_through_the_kms_it_mounts() {
    let router = build_router();

    // A key this emulator's KMS really holds, created through KMS's own `CreateKey`.
    let (status, created) = kms(
        &router,
        "CreateKey",
        &json!({ "Description": "the event bus key" }),
    )
    .await;
    assert_eq!(
        status, 200,
        "CreateKey must succeed in the assembled emulator, got {status} {created}"
    );
    let held = created["KeyMetadata"]["Arn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreateKey must report an Arn, got {created}"))
        .to_string();

    // (case, the key the bus names, the status it is answered with)
    let rows: Vec<(&str, String, u16)> = vec![
        ("a key the mounted KMS holds", held, 200),
        (
            "a key ARN the mounted KMS never issued",
            format!(
                "arn:aws:kms:{REGION}:{}:key/00000000-0000-0000-0000-000000000000",
                engine::EMULATOR_ACCOUNT_ID
            ),
            403,
        ),
        (
            "a bare key id the mounted KMS never issued",
            "9f8e7d6c-5b4a-3928-1706-fedcba987654".to_string(),
            403,
        ),
    ];

    for (index, (case, key, expected)) in rows.iter().enumerate() {
        let (status, answered) =
            create_event_bus(&router, &format!("assembled-bus-{index}"), key).await;
        assert_eq!(
            status, *expected,
            "case {case:?}: CreateEventBus must answer {expected}, got {status} with \
             {answered:?}"
        );
    }
}
