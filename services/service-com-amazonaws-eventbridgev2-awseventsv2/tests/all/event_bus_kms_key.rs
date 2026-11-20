// SPDX-License-Identifier: CC0-1.0

//! The KMS key an event bus is encrypted under, resolved through KMS.
//!
//! RULE: `EncryptionConfiguration$KmsKeyIdentifier` is a reference to a resource ANOTHER
//! service holds, and all the model constrains is the identifier's form
//! (`smithy_query(query="shape", shape="KmsKeyIdentifier")`: `length 0..2048`,
//! `pattern ^[a-zA-Z0-9_\-/:]*$`), so a bus whose configuration names a key is encrypted
//! only if KMS holds that key. `AccessDeniedException` is what this model declares for it —
//! "This error is also returned when the operation cannot use the AWS KMS key for the event
//! bus" — answered `403` with the reason code and the sentence of that reason. A bus created
//! or updated regardless would leave the caller believing its events are encrypted under a
//! key that does not exist, and nothing would ever say otherwise.
//!
//! Two services have to agree for this to hold — this one asks and KMS answers — so KMS is
//! mounted beside it on one engine here, exactly as the router mounts them: an emulator
//! assembled without a KMS takes the reference as given, which is the other half of the rule
//! (nothing is refused on an answer KMS did not give).
//!
//! The rows span what the rule generalises over: which key the reference names (two KMS
//! really holds, by ARN and by bare id, and three it does not, differing in the id), both
//! operations that take the configuration, and the requests that name no key at all.

use std::sync::Arc;

use aws_smithy_schema::codec::Codec;
use aws_smithy_schema_server::CborCodec;
use engine::InMemoryEngine;
use serde_json::{json, Value};
use smithy_server_runtime::{DeserializableStruct, MultiServiceRouter, Operation};

use service_com_amazonaws_eventbridgev2_awseventsv2 as eventsv2;

use eventsv2::ops::create_event_bus::CreateEventBus;
use eventsv2::ops::describe_event_bus::DescribeEventBus;
use eventsv2::ops::update_event_bus::UpdateEventBus;
use eventsv2::ops::types::{
    CreateEventBusInput, DescribeEventBusInput, EncryptionConfiguration, UpdateEventBusInput,
};

use crate::modelled_input_refusal::{encoded, refusal, rendered, REGION};

/// The account the emulator mints ARNs under, which a key ARN of this emulator names.
const ACCOUNT: &str = "000000000000";

/// The `X-Amz-Target` prefix of KMS's awsJson1_1 wire.
const KMS_TARGET_PREFIX: &str = "TrentService";

/// The error shape and the whole `Message` a bus whose key KMS does not hold is refused
/// with.
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";
const KMS_KEY_NOT_FOUND: &str = "KMS_KEY_NOT_FOUND: The event bus's KMS key does not exist.";

/// This service and KMS, over ONE engine, each reached on its own wire.
struct Pair {
    router: MultiServiceRouter,
}

impl Pair {
    fn new() -> Self {
        let engine: Arc<InMemoryEngine> = Arc::new(InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        // Nothing here signs: the `Authorization` header carries only the credential
        // scope, which is where a service reads the Region.
        router.set_request_authentication(false);
        router.register_service(eventsv2::EmulatedService::new(Arc::clone(&engine)));
        router.register_service(
            service_com_amazonaws_kms_trentservice::EmulatedService::new(engine),
        );
        Self { router }
    }

    /// One call on this service's `rpcv2Cbor` wire.
    async fn call<O: Operation>(&self, input: &O::Input) -> (u16, bytes::Bytes) {
        let request = http::Request::builder()
            .method("POST")
            .uri(format!("/service/AWSEventsV2/operation/{}", O::NAME))
            .header("host", format!("eventsv2.{REGION}.amazonaws.com"))
            .header("content-type", "application/cbor")
            .header("smithy-protocol", "rpc-v2-cbor")
            .header("authorization", authorization("events"))
            .body(bytes::Bytes::from(encoded::<O>(input)))
            .expect("the harness request is well formed");
        let response = self.router.dispatch(request).await;
        (response.status().as_u16(), response.into_body())
    }

    /// One call of `O`, requiring a 200 and decoding the output.
    async fn ok<O: Operation>(&self, case: &str, input: &O::Input) -> O::Output
    where
        O::Output: DeserializableStruct,
    {
        let (status, body) = self.call::<O>(input).await;
        assert_eq!(
            status,
            200,
            "case {case:?}: {} must answer 200, got {status} with {:?}",
            O::NAME,
            rendered(&body)
        );
        let codec = CborCodec::default();
        let mut deserializer = codec.create_deserializer(&body);
        O::Output::deserialize(&mut deserializer).unwrap_or_else(|error| {
            panic!(
                "case {case:?}: {}'s body must decode as its modelled output, got {error:?}",
                O::NAME
            )
        })
    }

    /// One KMS call on its awsJson1_1 wire.
    async fn kms(&self, operation: &str, request: &Value) -> (u16, Value) {
        let built = http::Request::builder()
            .method("POST")
            .uri("/")
            .header("host", format!("kms.{REGION}.amazonaws.com"))
            .header("content-type", "application/x-amz-json-1.1")
            .header("x-amz-target", format!("{KMS_TARGET_PREFIX}.{operation}"))
            .header("authorization", authorization("kms"))
            .body(bytes::Bytes::from(
                serde_json::to_vec(request).expect("the request body serializes"),
            ))
            .expect("the harness request is well formed");
        let response = self.router.dispatch(built).await;
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

    /// A symmetric encryption key, created through KMS's own `CreateKey`, as the ARN and the
    /// bare key id a caller may name it by.
    async fn key(&self, description: &str) -> (String, String) {
        let (status, created) = self
            .kms("CreateKey", &json!({ "Description": description }))
            .await;
        assert_eq!(
            status, 200,
            "case {description:?}: CreateKey must succeed, got {status} {created}"
        );
        let metadata = &created["KeyMetadata"];
        let arn = metadata["Arn"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {description:?}: CreateKey reports an Arn, got {created}")
            })
            .to_string();
        let key_id = metadata["KeyId"]
            .as_str()
            .unwrap_or_else(|| {
                panic!("case {description:?}: CreateKey reports a KeyId, got {created}")
            })
            .to_string();
        (arn, key_id)
    }
}

/// A SigV4 `Authorization` header whose credential scope names `signing_name`.
fn authorization(signing_name: &str) -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTONLY/20260101/{REGION}/{signing_name}/\
         aws4_request, SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// A key ARN of this Region and account naming the key id `key_id`.
fn key_arn(key_id: &str) -> String {
    format!("arn:aws:kms:{REGION}:{ACCOUNT}:key/{key_id}")
}

/// A `CreateEventBusInput` naming `name`, encrypted under `key` when it names one.
fn create_bus(name: &str, key: Option<&str>) -> CreateEventBusInput {
    CreateEventBusInput {
        name: name.to_string(),
        client_token: Some(format!("token-for-{name}")),
        encryption_configuration: key.map(|key| EncryptionConfiguration {
            kms_key_identifier: Some(key.to_string()),
        }),
        ..Default::default()
    }
}

/// Every key identifier KMS does not resolve, each well formed, so a refusal is about the
/// key not existing rather than about the identifier's form.
fn keys_kms_does_not_hold() -> Vec<(&'static str, String)> {
    vec![
        (
            "a key ARN KMS never issued",
            key_arn("00000000-0000-0000-0000-000000000000"),
        ),
        (
            "a second key ARN KMS never issued",
            key_arn("11111111-2222-3333-4444-555555555555"),
        ),
        (
            "a bare key id KMS never issued",
            "9f8e7d6c-5b4a-3928-1706-fedcba987654".to_string(),
        ),
    ]
}

#[tokio::test]
async fn a_bus_is_refused_a_kms_key_that_kms_does_not_hold() {
    let pair = Pair::new();

    for (index, (case, key)) in keys_kms_does_not_hold().iter().enumerate() {
        let name = format!("absent-key-bus-{index}");
        let (status, body) = pair
            .call::<CreateEventBus<InMemoryEngine>>(&create_bus(&name, Some(key)))
            .await;
        assert_eq!(
            status, 403,
            "case {case:?}: CreateEventBus must answer 403, got {status} with {:?}",
            rendered(&body)
        );
        let (code, message) = refusal(case, &body);
        assert_eq!(
            code, ACCESS_DENIED_EXCEPTION,
            "case {case:?}: expected {ACCESS_DENIED_EXCEPTION}, got {code:?} with {message:?}"
        );
        assert_eq!(
            message, KMS_KEY_NOT_FOUND,
            "case {case:?}: expected {KMS_KEY_NOT_FOUND:?}, got {message:?}"
        );
    }

    // The same rule on the UPDATE: a bus encrypted under a key KMS holds cannot be moved
    // onto one it does not.
    let (held, _) = pair.key("the bus key").await;
    let arn = pair
        .ok::<CreateEventBus<InMemoryEngine>>("the bus under test", &create_bus("movable-bus", Some(&held)))
        .await
        .event_bus_arn
        .expect("CreateEventBus must report an EventBusArn");

    for (case, key) in keys_kms_does_not_hold() {
        let (status, body) = pair
            .call::<UpdateEventBus<InMemoryEngine>>(&UpdateEventBusInput {
                event_bus_arn: arn.clone(),
                encryption_configuration: Some(EncryptionConfiguration {
                    kms_key_identifier: Some(key.clone()),
                }),
                ..Default::default()
            })
            .await;
        assert_eq!(
            status, 403,
            "case {case:?}: UpdateEventBus must answer 403, got {status} with {:?}",
            rendered(&body)
        );
        let (code, message) = refusal(case, &body);
        assert_eq!(
            code, ACCESS_DENIED_EXCEPTION,
            "case {case:?}: expected {ACCESS_DENIED_EXCEPTION}, got {code:?} with {message:?}"
        );
        assert_eq!(
            message, KMS_KEY_NOT_FOUND,
            "case {case:?}: expected {KMS_KEY_NOT_FOUND:?}, got {message:?}"
        );
    }

    // Round trip: the refused updates wrote nothing, so the bus still reports the key it
    // was created under.
    let read = pair
        .ok::<DescribeEventBus<InMemoryEngine>>(
            "the bus after the refused updates",
            &DescribeEventBusInput {
                event_bus_arn: arn.clone(),
            },
        )
        .await;
    assert_eq!(
        read.encryption_configuration
            .and_then(|configuration| configuration.kms_key_identifier),
        Some(held),
        "a refused update must leave the stored key alone"
    );
}

#[tokio::test]
async fn a_bus_takes_a_kms_key_that_kms_holds() {
    let pair = Pair::new();
    let (first, first_id) = pair.key("the first bus key").await;
    let (second, _) = pair.key("the second bus key").await;

    // Rows: a key KMS holds named by ARN, the same key by its bare id (the other
    // identifier form the member admits), a second key — so acceptance is not about the
    // first create being special — and the create that names no key at all, which has no
    // reference to resolve.
    let accepted: Vec<(&str, Option<String>)> = vec![
        ("a key ARN KMS holds", Some(first.clone())),
        ("the same key by its bare id", Some(first_id.clone())),
        ("a second key KMS holds", Some(second.clone())),
        ("no encryption configuration at all", None),
    ];

    for (index, (case, key)) in accepted.iter().enumerate() {
        let name = format!("held-key-bus-{index}");
        let created = pair
            .ok::<CreateEventBus<InMemoryEngine>>(case, &create_bus(&name, key.as_deref()))
            .await;
        let arn = created
            .event_bus_arn
            .unwrap_or_else(|| panic!("case {case:?}: CreateEventBus must report an EventBusArn"));

        // Round trip: the bus was really created, and reports the key it was created
        // under.
        let read = pair
            .ok::<DescribeEventBus<InMemoryEngine>>(
                case,
                &DescribeEventBusInput {
                    event_bus_arn: arn.clone(),
                },
            )
            .await;
        assert_eq!(
            read.encryption_configuration
                .and_then(|configuration| configuration.kms_key_identifier),
            key.clone(),
            "case {case:?}: the stored key reference must be reported back"
        );
    }
}
