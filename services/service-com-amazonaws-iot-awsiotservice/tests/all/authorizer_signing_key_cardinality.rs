// SPDX-License-Identifier: CC0-1.0

//! How MANY token signing public keys a write of a custom authorizer may state.
//!
//! The rule under test: a write of an authorizer that states `tokenSigningPublicKeys`
//! must state a map holding at least one and at most two keys — a map holding none
//! registers no key a presented token signature could ever be verified against, and a
//! map holding more keys than the member exists for (publishing a new signing key
//! beside the old one while a fleet rotates) is more than the service accepts. A write
//! outside that range is refused with the modelled `InvalidRequestException` (HTTP 400,
//! `smithy_query(query="shape",shape="InvalidRequestException")`) and records nothing —
//! a map holding too few keys with the sentence stating what the member must hold, a
//! map holding too many with the sentence naming the ceiling it passed; a write that
//! states no map at all leaves the authorizer's registered keys as they are.
//!
//! `PublicKeyMap` is a map of `KeyName` to `KeyValue` with no modelled entry count
//! (`smithy_query(query="shape",shape="PublicKeyMap")`), so the cardinality is the
//! service's and is asserted here over a SET of key counts — none, one, two, three and
//! well past three — on both writers of an authorizer (`CreateAuthorizer` and
//! `UpdateAuthorizer`), rather than on one recorded request.
//!
//! Every case drives the real wire path: a restJson1 request signed for the `iot`
//! signing name, dispatched through `MultiServiceRouter`, so what is asserted is what
//! an AWS SDK client sees.

use std::sync::Arc;

use serde_json::{json, Map, Value};
use service_com_amazonaws_iot_awsiotservice::state::{
    MAX_TOKEN_SIGNING_PUBLIC_KEYS, MIN_TOKEN_SIGNING_PUBLIC_KEYS,
};
use service_com_amazonaws_iot_awsiotservice::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// The region every call here is signed for.
const REGION: &str = "us-east-1";

/// A SigV4 `Authorization` header whose credential scope names the `iot` signing name:
/// the scope is how the router picks the claiming service. The signature itself is never
/// checked — the harness turns request authentication off, exactly as an
/// unauthenticated emulator runs.
fn signed_for_iot() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIDTESTIOT/20260101/{REGION}/iot/aws4_request, \
         SignedHeaders=host;x-amz-date, Signature=0000"
    )
}

/// One response, reduced to what a client branches on.
#[derive(Debug)]
struct Reply {
    status: u16,
    error_code: Option<String>,
    body: Value,
}

impl Reply {
    /// The modelled error shape the refusal names, taken from `x-amzn-errortype`
    /// (which this protocol renders as `<Shape>:<documentation uri>`), or the empty
    /// string when the response carries no error type.
    fn code(&self) -> &str {
        self.error_code
            .as_deref()
            .unwrap_or_default()
            .split(':')
            .next()
            .unwrap_or_default()
    }

    /// The error message the refusal carries, or the empty string when there is none.
    fn message(&self) -> String {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// How many keys the reported `AuthorizerDescription` carries, or `None` when it
    /// reports no `tokenSigningPublicKeys` member at all.
    fn registered_keys(&self) -> Option<usize> {
        self.body
            .get("authorizerDescription")?
            .get("tokenSigningPublicKeys")?
            .as_object()
            .map(Map::len)
    }
}

/// The emulator under test: ONE shared engine with the IoT service mounted on it.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(engine));
        Self { router }
    }

    async fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Reply {
        let payload = body.map_or_else(Vec::new, |value| {
            serde_json::to_vec(value).expect("a request body serializes")
        });
        let request = http::Request::builder()
            .method(method)
            .uri(format!("https://iot.{REGION}.amazonaws.com{path}"))
            .header("authorization", signed_for_iot())
            .header("content-type", "application/json")
            .body(bytes::Bytes::from(payload))
            .expect("a well-formed request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let error_code = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(str::to_string);
        let bytes = response.into_body();
        let body = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        Reply {
            status,
            error_code,
            body,
        }
    }

    async fn create(&self, name: &str, body: &Value) -> Reply {
        self.call("POST", &format!("/authorizer/{name}"), Some(body))
            .await
    }

    async fn update(&self, name: &str, body: &Value) -> Reply {
        self.call("PUT", &format!("/authorizer/{name}"), Some(body))
            .await
    }

    async fn describe(&self, name: &str) -> Reply {
        self.call("GET", &format!("/authorizer/{name}"), None).await
    }
}

/// A `tokenSigningPublicKeys` map of `count` keys, each a distinct `KeyName` (modelled
/// `pattern ^[a-zA-Z0-9:_-]+$`) carrying a distinct PEM block as its `KeyValue`.
fn key_map(count: usize) -> Value {
    let keys: Map<String, Value> = (0..count)
        .map(|index| {
            (
                format!("SIGNING_KEY_{index}"),
                json!(format!(
                    "-----BEGIN PUBLIC KEY-----\nKEY{index}\n-----END PUBLIC KEY-----"
                )),
            )
        })
        .collect();
    Value::Object(keys)
}

/// The sentence a write whose stated map holds too FEW keys is refused with, for the
/// authorizer the request named: it states what the member must hold.
fn too_few_refusal(name: &str) -> String {
    format!(
        "Token signing keys map for authorizer {name} cannot be null or empty. There must be at \
         least one and at most two token signing public keys in the map"
    )
}

/// The sentence a write whose stated map holds too MANY keys is refused with: a
/// different refusal from the one above, naming the ceiling the map passed.
fn too_many_refusal(name: &str) -> String {
    format!(
        "Token signing keys map for authorizer {name} cannot contain more than \
         {MAX_TOKEN_SIGNING_PUBLIC_KEYS} keys"
    )
}

/// The function ARN every create here authorizes with.
fn function_arn(name: &str) -> String {
    format!("arn:aws:lambda:{REGION}:000000000000:function:{name}")
}

/// What a write stating a map of a given size must answer: the accepted range's bounds
/// are recorded, a map holding too few keys is refused with the requirement, and a map
/// holding too many is refused with the ceiling.
enum Expected {
    /// The write is accepted and the map is what the authorizer carries.
    Recorded,
    /// The write is refused `InvalidRequestException` with this message.
    Refused(fn(&str) -> String),
}

/// The key counts the rule generalises over: the accepted range's bounds, one short of
/// its minimum (the empty map the member was stated as), one past its maximum, and well
/// past its maximum.
fn counts() -> Vec<(String, usize, Expected)> {
    let past = MAX_TOKEN_SIGNING_PUBLIC_KEYS + 1;
    let far_past = MAX_TOKEN_SIGNING_PUBLIC_KEYS + 6;
    vec![
        (
            "the map stated empty".to_string(),
            0,
            Expected::Refused(too_few_refusal),
        ),
        (
            format!("the accepted minimum of {MIN_TOKEN_SIGNING_PUBLIC_KEYS}"),
            MIN_TOKEN_SIGNING_PUBLIC_KEYS,
            Expected::Recorded,
        ),
        (
            format!("the accepted maximum of {MAX_TOKEN_SIGNING_PUBLIC_KEYS}"),
            MAX_TOKEN_SIGNING_PUBLIC_KEYS,
            Expected::Recorded,
        ),
        (
            format!("one key past the maximum ({past})"),
            past,
            Expected::Refused(too_many_refusal),
        ),
        (
            format!("well past the maximum ({far_past})"),
            far_past,
            Expected::Refused(too_many_refusal),
        ),
    ]
}

/// `CreateAuthorizer` accepts a stated key map only inside the accepted cardinality,
/// and a create it refuses records nothing — so the name is still free afterwards.
#[tokio::test]
async fn create_authorizer_accepts_only_one_or_two_signing_keys() {
    let emulator = Emulator::new();

    for (case, count, expected) in counts() {
        let name = format!("create_keys_{count}");
        let created = emulator
            .create(
                &name,
                &json!({
                    "authorizerFunctionArn": function_arn(&name),
                    "tokenKeyName": "X-Amz-Custom-Token",
                    "tokenSigningPublicKeys": key_map(count),
                }),
            )
            .await;

        let refusal = match expected {
            Expected::Recorded => {
                assert_eq!(
                    created.status, 200,
                    "case {case:?}: expected HTTP 200 from CreateAuthorizer, got {created:?}"
                );
                let read = emulator.describe(&name).await;
                assert_eq!(
                    (read.status, read.registered_keys()),
                    (200, Some(count)),
                    "case {case:?}: expected the created authorizer to be readable with {count} \
                     registered keys, got {read:?}"
                );
                continue;
            }
            Expected::Refused(message) => message(&name),
        };

        assert_eq!(
            (created.status, created.code(), created.message().as_str()),
            (400, "InvalidRequestException", refusal.as_str()),
            "case {case:?}: expected the key-cardinality refusal, got {created:?}"
        );
        // Nothing was recorded for a create AWS refuses, so the name is still free.
        let read = emulator.describe(&name).await;
        assert_eq!(
            read.status, 404,
            "case {case:?}: expected the refused create to record nothing, got {read:?}"
        );
    }
}

/// `UpdateAuthorizer` enforces the same cardinality over the map it replaces, and an
/// update it refuses leaves the record exactly as it was.
#[tokio::test]
async fn update_authorizer_accepts_only_one_or_two_signing_keys() {
    let emulator = Emulator::new();

    for (case, count, expected) in counts() {
        let name = format!("update_keys_{count}");
        let created = emulator
            .create(
                &name,
                &json!({
                    "authorizerFunctionArn": function_arn(&name),
                    "tokenSigningPublicKeys": key_map(MIN_TOKEN_SIGNING_PUBLIC_KEYS),
                }),
            )
            .await;
        assert_eq!(
            created.status, 200,
            "case {case:?}: the authorizer to update must be created, got {created:?}"
        );

        let updated = emulator
            .update(&name, &json!({ "tokenSigningPublicKeys": key_map(count) }))
            .await;

        let refusal = match expected {
            Expected::Recorded => {
                assert_eq!(
                    updated.status, 200,
                    "case {case:?}: expected HTTP 200 from UpdateAuthorizer, got {updated:?}"
                );
                let read = emulator.describe(&name).await;
                assert_eq!(
                    read.registered_keys(),
                    Some(count),
                    "case {case:?}: expected the update to replace the map with {count} keys, got \
                     {read:?}"
                );
                continue;
            }
            Expected::Refused(message) => message(&name),
        };

        assert_eq!(
            (updated.status, updated.code(), updated.message().as_str()),
            (400, "InvalidRequestException", refusal.as_str()),
            "case {case:?}: expected the key-cardinality refusal, got {updated:?}"
        );
        let read = emulator.describe(&name).await;
        assert_eq!(
            read.registered_keys(),
            Some(MIN_TOKEN_SIGNING_PUBLIC_KEYS),
            "case {case:?}: expected the refused update to change nothing, got {read:?}"
        );
    }
}

/// A write that states NO `tokenSigningPublicKeys` member is a different case from one
/// that states an empty map: the create records an authorizer with no registered keys,
/// and the update leaves whatever the record carries.
#[tokio::test]
async fn a_write_that_states_no_key_map_leaves_the_registered_keys_alone() {
    let emulator = Emulator::new();

    let unsigned = "no_key_map";
    let created = emulator
        .create(
            unsigned,
            &json!({
                "authorizerFunctionArn": function_arn(unsigned),
                "signingDisabled": true,
            }),
        )
        .await;
    assert_eq!(
        created.status, 200,
        "case \"a create stating no key map\": expected HTTP 200, got {created:?}"
    );
    let read = emulator.describe(unsigned).await;
    assert_eq!(
        (read.status, read.registered_keys()),
        (200, None),
        "case \"a create stating no key map\": expected an authorizer with no registered keys, \
         got {read:?}"
    );

    let signed = "keeps_key_map";
    emulator
        .create(
            signed,
            &json!({
                "authorizerFunctionArn": function_arn(signed),
                "tokenSigningPublicKeys": key_map(MAX_TOKEN_SIGNING_PUBLIC_KEYS),
            }),
        )
        .await;
    let updated = emulator
        .update(signed, &json!({ "status": "ACTIVE" }))
        .await;
    assert_eq!(
        updated.status, 200,
        "case \"an update stating no key map\": expected HTTP 200, got {updated:?}"
    );
    let read = emulator.describe(signed).await;
    assert_eq!(
        read.registered_keys(),
        Some(MAX_TOKEN_SIGNING_PUBLIC_KEYS),
        "case \"an update stating no key map\": expected the registered keys to be untouched, \
         got {read:?}"
    );
}

/// The accepted cardinality is ONE or TWO keys and each refusal is its own sentence,
/// written out here rather than interpolated from the service's constants: the rows
/// below carry literal key counts and literal messages, so a cardinality that drifted
/// would fail this test instead of being carried along by it.
#[tokio::test]
async fn one_and_two_keys_are_recorded_and_three_is_refused() {
    let emulator = Emulator::new();

    // (case, how many keys the create states, expected status, expected message)
    let rows: [(&str, usize, u16, &str); 4] = [
        ("no key at all", 0, 400,
         "Token signing keys map for authorizer written0 cannot be null or empty. There must be \
          at least one and at most two token signing public keys in the map"),
        ("one key", 1, 200, ""),
        ("two keys", 2, 200, ""),
        ("three keys", 3, 400,
         "Token signing keys map for authorizer written3 cannot contain more than 2 keys"),
    ];

    for (case, count, expected_status, expected_message) in rows {
        let name = format!("written{count}");
        let created = emulator
            .create(
                &name,
                &json!({
                    "authorizerFunctionArn": function_arn(&name),
                    "tokenSigningPublicKeys": key_map(count),
                }),
            )
            .await;
        assert_eq!(
            (created.status, created.message().as_str()),
            (expected_status, expected_message),
            "case {case:?}: expected HTTP {expected_status} and {expected_message:?}, got \
             {created:?}"
        );
        let read = emulator.describe(&name).await;
        let expected_keys = (expected_status == 200).then_some(count);
        assert_eq!(
            read.registered_keys(),
            expected_keys,
            "case {case:?}: expected the stored authorizer to carry {expected_keys:?} keys, got \
             {read:?}"
        );
    }
}

/// Each refusal's variable part is the authorizer the request named, taken from the URI
/// label — not a fixed name and not the key count.
#[tokio::test]
async fn the_refusal_names_the_authorizer_the_request_addressed() {
    let emulator = Emulator::new();

    // The names span the modelled `authorizerName` form: `length 1..128, pattern
    // ^[\w=,@-]+$` (`smithy_query(query="operation",operation="CreateAuthorizer")`), and
    // each is written with both the empty map and a map past the ceiling, so both
    // sentences are checked against the addressed name.
    for name in ["a", "Auth_01=aZ,x@y-z", &"n".repeat(128)] {
        for (count, expected) in [
            (0usize, too_few_refusal(name)),
            (MAX_TOKEN_SIGNING_PUBLIC_KEYS + 1, too_many_refusal(name)),
        ] {
            let case = format!("{name:?} writing {count} keys");
            let refused = emulator
                .create(
                    name,
                    &json!({
                        "authorizerFunctionArn": function_arn("shared"),
                        "tokenSigningPublicKeys": key_map(count),
                    }),
                )
                .await;
            assert_eq!(
                (refused.status, refused.message().as_str()),
                (400, expected.as_str()),
                "case {case:?}: expected the refusal to name the addressed authorizer, got \
                 {refused:?}"
            );
        }
    }
}
