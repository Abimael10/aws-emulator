// SPDX-License-Identifier: CC0-1.0

//! What a network create answers for a `networkName` the service does not accept, as a
//! client reads it off the wire.
//!
//! The rules under test:
//!
//! * `CreateNetworkRequest$networkName` is a `GenericString` whose `@pattern ^[\S\s]*$`
//!   matches every string
//!   (`smithy_query(query="operation",operation="CreateNetwork")`), so no modelled
//!   constraint can refuse a name and the service owns the whole name rule: a submitted
//!   name that is not a name a network may carry is refused with the modelled
//!   `ValidationError` (`@error client`, `@httpError 422`) before anything is stored.
//! * That refusal NAMES its error shape in both places a restJson1 client resolves one
//!   from — the `x-amzn-errortype` header and the body's `__type` — and carries exactly
//!   one `ErrorDetail` in `reasons`: `field` is the refused input member's name and
//!   `reason` is the one FIXED sentence this service states for a name it will not
//!   accept, transcribed below from the recorded wire. `ValidationError$message` stays
//!   unset, so the body carries no envelope sentence beside the per-field detail.
//! * A refused create changes nothing: no network is listed, and the identifier space is
//!   untouched, so the next name the rule accepts is minted the id a store that refused
//!   nothing would have minted.
//!
//! Every case goes through the real multi-service router — route resolution, restJson1
//! deserialization, modelled-constraint validation, the handler and response
//! serialization — so what it asserts is the wire behaviour a client sees.

use std::sync::Arc;

use serde_json::{json, Value};
use service_com_amazonaws_wickr_wickradminapi::state::{
    network_name_is_valid, NETWORK_NAME_INVALID_REASON, NETWORK_NAME_MAX_CHARS,
    NETWORK_NAME_MIN_CHARS,
};
use smithy_server_runtime::MultiServiceRouter;

/// The status the model declares for `ValidationError` (`@httpError 422`).
const VALIDATION_ERROR_STATUS: u16 = 422;

/// The error shape a restJson1 client resolves from `x-amzn-errortype` and from the
/// body's `__type`.
const VALIDATION_ERROR_SHAPE: &str = "ValidationError";

/// The member such a refusal names in `ErrorDetail$field`: the input member the request
/// carried the refused value in.
const EXPECTED_FIELD: &str = "networkName";

/// The sentence this service states for a name it will not accept, transcribed from the
/// recorded wire and stated here independently of the emulator's own constant, which the
/// last assertion below pins to it.
const EXPECTED_REASON: &str = "Network Name is not a valid name";

/// A fresh emulator holding only this service, over its own engine, so one test's
/// networks are invisible to another's.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    // The signature is not the subject here; the name rule and the refusal are.
    router.set_request_authentication(false);
    router.register_service(service_com_amazonaws_wickr_wickradminapi::EmulatedService::new(
        engine,
    ));
    router
}

/// What one call answered: the status, the `x-amzn-errortype` header (empty when the
/// answer carried none) and the parsed body.
struct Answer {
    status: u16,
    error_type: String,
    body: Value,
}

/// One request/response round trip.
async fn call(router: &MultiServiceRouter, method: &str, uri: &str, body: Option<Value>) -> Answer {
    let payload = body
        .map(|value| serde_json::to_vec(&value).expect("serialize the request body"))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "admin.wickr.us-east-1.amazonaws.com")
        // The SigV4 credential scope is how the router picks the service.
        .header(
            "authorization",
            "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260102/us-east-1/wickr/aws4_request, \
             SignedHeaders=host, Signature=00",
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(payload))
        .expect("build the request");

    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let error_type = response
        .headers()
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    let raw = response.into_body();
    let body = if raw.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&raw)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&raw).into_owned()))
    };
    Answer {
        status,
        error_type,
        body,
    }
}

/// Submit one create of `name`.
async fn create_network(router: &MultiServiceRouter, name: &str) -> Answer {
    call(
        router,
        "POST",
        "/networks",
        Some(json!({ "networkName": name, "accessLevel": "STANDARD" })),
    )
    .await
}

/// The `networkId` of every network the store reports, so a refusal that stored something
/// cannot hide behind the create's own answer.
async fn listed_network_ids(router: &MultiServiceRouter, case: &str) -> Vec<String> {
    let answer = call(router, "GET", "/networks", None).await;
    assert_eq!(
        answer.status, 200,
        "case {case:?}: ListNetworks expected 200, got {} with {}",
        answer.status, answer.body
    );
    answer.body["networks"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .map(|network| {
            network["networkId"]
                .as_str()
                .unwrap_or_default()
                .to_string()
        })
        .collect()
}

/// The names this service will not accept, one row per way the rule can be broken.
///
/// The rows differ along what the rule generalises over: the length of the name (each end
/// of the window the member documentation's "between 1 and 20 characters" bounds above,
/// one past each end, and well past the long end), the class of a character the name is
/// built from (punctuation, a symbol, a control character, a pictograph), where a space
/// sits, and the script the name is written in. `^[\S\s]*$` refuses none of them, so every
/// row reaches the handler and is decided by the name rule alone.
fn refused_network_names() -> Vec<(&'static str, String)> {
    vec![
        // The two lengths the recorded wire fixes, written as LITERALS so no change to
        // the constants can make the table stop covering what was recorded.
        ("three characters", "abc".to_string()),
        ("forty-two characters", "n".repeat(42)),
        ("empty", String::new()),
        (
            "one short of the shortest name",
            "a".repeat(NETWORK_NAME_MIN_CHARS - 1),
        ),
        (
            "one past the longest name",
            "n".repeat(NETWORK_NAME_MAX_CHARS + 1),
        ),
        (
            "one past the longest name in multi-byte characters",
            "ü".repeat(NETWORK_NAME_MAX_CHARS + 1),
        ),
        ("a dot", "demo.net".to_string()),
        ("a colon", "demo:net".to_string()),
        ("a plus sign", "demo+net".to_string()),
        ("a currency symbol", "demo€net".to_string()),
        ("a control character", "demo\u{7}net".to_string()),
        ("a line break", "demo\nnet".to_string()),
        ("a leading space", " demo-net".to_string()),
        ("a trailing space", "demo-net ".to_string()),
        ("spaces only", "    ".to_string()),
        ("a pictograph", "demo\u{1f6e1}net".to_string()),
        ("another script with punctuation", "ネット・net".to_string()),
    ]
}

/// RULE: a `networkName` the rule does not accept is refused with the modelled
/// `ValidationError`, named where a restJson1 client reads an error shape, detailing the
/// refused member once with the service's fixed sentence and nothing else — and the
/// refusal stores no network.
#[tokio::test]
async fn a_refused_network_name_answers_a_named_validation_error_and_stores_nothing() {
    let router = emulator();

    for (case, name) in refused_network_names() {
        assert!(
            !network_name_is_valid(&name),
            "case {case:?}: the row is only a refused name while the rule turns it down, \
             got {name:?}"
        );
        let answer = create_network(&router, &name).await;
        assert_eq!(
            answer.status, VALIDATION_ERROR_STATUS,
            "case {case:?} ({} characters): expected status {VALIDATION_ERROR_STATUS}, got {} \
             with {}",
            name.chars().count(),
            answer.status,
            answer.body
        );
        assert!(
            answer.error_type.ends_with(VALIDATION_ERROR_SHAPE),
            "case {case:?}: expected x-amzn-errortype to name {VALIDATION_ERROR_SHAPE}, got {:?}",
            answer.error_type
        );
        assert!(
            answer.body["__type"]
                .as_str()
                .is_some_and(|shape| shape.ends_with(VALIDATION_ERROR_SHAPE)),
            "case {case:?}: expected the body to name {VALIDATION_ERROR_SHAPE}, got {}",
            answer.body
        );
        // The diagnosis lives in `reasons`; this error class states no envelope sentence,
        // and a refused create reports no created network either.
        assert!(
            answer.body["message"].is_null(),
            "case {case:?}: expected no top-level message, got {}",
            answer.body
        );
        for member in ["networkId", "networkName", "encryptionKeyArn"] {
            assert!(
                answer.body[member].is_null(),
                "case {case:?}: a refusal reports no {member}, got {}",
                answer.body
            );
        }
        let reasons = answer.body["reasons"].as_array().cloned().unwrap_or_else(|| {
            panic!(
                "case {case:?}: expected a reasons list, got {}",
                answer.body
            )
        });
        assert_eq!(
            reasons.len(),
            1,
            "case {case:?}: one refused member is one detail, got {}",
            answer.body
        );
        assert_eq!(
            reasons[0]["field"],
            json!(EXPECTED_FIELD),
            "case {case:?}: expected the detail to name {EXPECTED_FIELD}, got {}",
            answer.body
        );
        assert_eq!(
            reasons[0]["reason"],
            json!(EXPECTED_REASON),
            "case {case:?}: expected the fixed reason {EXPECTED_REASON:?}, got {}",
            reasons[0]["reason"]
        );
        assert_eq!(
            listed_network_ids(&router, case).await,
            Vec::<String>::new(),
            "case {case:?}: the refusal must store no network"
        );
    }

    // The sentence is the same for every row, so it carries no part of the submitted
    // value, and it is the one the service holds as its fixed text for this refusal.
    assert_eq!(
        NETWORK_NAME_INVALID_REASON, EXPECTED_REASON,
        "the service's fixed reason must be the recorded sentence, got \
         {NETWORK_NAME_INVALID_REASON:?}"
    );
}

/// RULE: the refusal happens before anything is minted, so a refused create consumes no
/// `networkId` — a store that refused every row above mints the same first id as one that
/// was asked for nothing else, and the network it does create reads back.
#[tokio::test]
async fn a_refused_create_consumes_no_network_id() {
    // The id a store that refused nothing mints first, read from a second emulator rather
    // than written down here, so the case states the invariant and not the id space.
    let untouched = emulator();
    let reference = create_network(&untouched, "reference-net").await;
    assert!(
        (200..300).contains(&reference.status),
        "case \"a store asked for nothing else\": expected a success, got {} with {}",
        reference.status,
        reference.body
    );
    let first_id = reference.body["networkId"]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "case \"a store asked for nothing else\": no networkId in {}",
                reference.body
            )
        })
        .to_string();

    let router = emulator();
    for (case, name) in refused_network_names() {
        let answer = create_network(&router, &name).await;
        assert_eq!(
            answer.status, VALIDATION_ERROR_STATUS,
            "case {case:?}: expected status {VALIDATION_ERROR_STATUS}, got {} with {}",
            answer.status, answer.body
        );
    }

    // Every name the rule accepts is still created, whatever was refused before it: the
    // rows span the length window's ends, the separators a name may carry and a letter
    // outside ASCII.
    let accepted: Vec<(&str, String)> = vec![
        ("the shortest name", "a".repeat(NETWORK_NAME_MIN_CHARS)),
        ("the longest name", "n".repeat(NETWORK_NAME_MAX_CHARS)),
        ("letters, digits and separators", "demo net-1_0".to_string()),
        ("a letter outside ASCII", "démo-net".to_string()),
    ];
    let mut minted: Vec<String> = Vec::new();
    for (case, name) in &accepted {
        assert!(
            network_name_is_valid(name),
            "case {case:?}: the row is only an accepted name while the rule accepts it, \
             got {name:?}"
        );
        let answer = create_network(&router, name).await;
        assert!(
            (200..300).contains(&answer.status),
            "case {case:?}: expected a success, got {} with {}",
            answer.status,
            answer.body
        );
        assert_eq!(
            answer.body["networkName"],
            json!(name),
            "case {case:?}: expected the submitted name back, got {}",
            answer.body
        );
        let network_id = answer.body["networkId"]
            .as_str()
            .unwrap_or_else(|| panic!("case {case:?}: no networkId in {}", answer.body))
            .to_string();

        // Read it back, so what the create answered is what was stored.
        let fetched = call(&router, "GET", &format!("/networks/{network_id}"), None).await;
        assert_eq!(
            fetched.status, 200,
            "case {case:?}: GetNetwork expected 200, got {} with {}",
            fetched.status, fetched.body
        );
        assert_eq!(
            fetched.body["networkName"],
            json!(name),
            "case {case:?}: GetNetwork must report the stored name, got {}",
            fetched.body
        );
        minted.push(network_id);
    }

    assert_eq!(
        minted.first().map(String::as_str),
        Some(first_id.as_str()),
        "case \"the first create a store serves\": the refusals must consume no id, so the \
         first network minted after them is {first_id}, got {minted:?}"
    );
    assert_eq!(
        listed_network_ids(&router, "every name the rule accepts").await,
        minted,
        "case \"every name the rule accepts\": the store must report exactly the networks \
         it minted, got {minted:?}"
    );
}
