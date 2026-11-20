// SPDX-License-Identifier: CC0-1.0

//! The ORDER a Direct Connect refusal lists an enumerated type's admitted values in.
//!
//! RULE under test: **a value outside a modelled enumerated type is refused in AWS's
//! validation envelope, whose requirement clause states the admitted set in the order
//! THIS SERVICE states that set in — a property of the enumerated TYPE, not of the
//! member, the operation or the request that carried the value — while the envelope, the
//! echoed value, the member path and the requirement's wording stay the framework's.**
//! `ResiliencyModel` is declared `maximum-resiliency, high-resiliency, basic-resiliency`
//! (`smithy_query(query="shape",shape="ResiliencyModel")`) and this service states it
//! ascending, so a clause that lists the model's declaration order is a defect.
//!
//! The rows differ along what the rule generalises over: how the refused value is
//! spelled (a name no value carries, an admitted value miscased, an admitted value
//! padded, the empty string, a value carrying the set's own text), how many constraints
//! one request breaks (one, and two — which moves the envelope's count and adds a
//! separator without moving the bracketed set), and which enumerated type carried the
//! value (a second type, whose order this service pins none of, must still be stated
//! with every value the model declares).
//!
//! Why it lives beside the router's suites rather than in the service crate: the rule
//! spans three crates — the vendored framework that renders a boundary refusal and its
//! requirement clauses, `service-common`'s one statement of the order a service lists a
//! value set in, and the service whose seam chooses that order — and this is the test
//! target that may observe all three at once.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request below is signed for.
const REGION: &str = "eu-west-1";

/// The `X-Amz-Target` prefix of this service's `awsJson1_1` operations.
const TARGET_PREFIX: &str = "OvertureService";

/// The error code AWS answers a constraint refusal with, and the code the framework
/// picks for one on a JSON protocol.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The order this service states `ResiliencyModel`'s value set in, which is NOT the
/// order the model declares its members in.
const RESILIENCY_MODEL_SET: &str = "[basic-resiliency, high-resiliency, maximum-resiliency]";

/// A `resiliencyGroupName` that satisfies the modelled constraints
/// (`smithy_query(query="operation",operation="CreateResiliencyGroup")`: `@length`
/// 1..255, `@pattern` `^[a-zA-Z0-9_\-]+$`), so a refusal can only be about the
/// enumerated member under test.
const GROUP_NAME: &str = "resiliency-group-under-test";

/// One emulator carrying Direct Connect over one shared engine.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_directconnect_overtureservice::EmulatedService::new(engine),
    );
    router
}

/// One answer: its HTTP status, its awsJson error code and its message.
struct Reply {
    status: u16,
    body: Value,
}

impl Reply {
    /// The bare shape name of the awsJson error code, or `""` on success.
    fn error_code(&self) -> String {
        self.body
            .get("__type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .rsplit('#')
            .next()
            .unwrap_or_default()
            .to_string()
    }

    /// The error envelope's message, or `""` when there is none.
    fn message(&self) -> &str {
        self.body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
    }
}

/// Call one Direct Connect operation over its `awsJson1_1` wire.
async fn dx(router: &MultiServiceRouter, operation: &str, body: Value) -> Reply {
    let authorization = format!(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/directconnect/aws4_request, \
         SignedHeaders=host;x-amz-target, Signature=0000"
    );
    let request = http::Request::builder()
        .method("POST")
        .uri("/")
        .header("host", format!("directconnect.{REGION}.amazonaws.com"))
        .header("content-type", "application/x-amz-json-1.1")
        .header("x-amz-target", format!("{TARGET_PREFIX}.{operation}"))
        .header("authorization", authorization)
        .body(Bytes::from(
            serde_json::to_vec(&body).expect("serialize the request body"),
        ))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let body = serde_json::from_slice(response.body()).unwrap_or(Value::Null);
    Reply { status, body }
}

/// The one-error validation envelope over an enum-constraint clause: the value as
/// submitted, the member path, and the admitted set as the service states it.
fn one_enum_error(value: &str, member_path: &str, value_set: &str) -> String {
    format!(
        "1 validation error detected: Value '{value}' at '{member_path}' failed to satisfy \
         constraint: Member must satisfy enum value set: {value_set}"
    )
}

/// The bracketed value set a clause states, or `None` for a message stating none.
fn stated_set(message: &str) -> Option<String> {
    let (_, tail) = message.split_once("enum value set: [")?;
    let (set, _) = tail.split_once(']')?;
    Some(set.to_string())
}

/// The values a bracketed set lists, in the order it lists them.
fn stated_values(message: &str) -> Vec<String> {
    stated_set(message)
        .unwrap_or_default()
        .split(", ")
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn a_refused_resiliency_model_lists_the_set_in_the_order_the_service_states_it() {
    let router = emulator();

    // (case, the value submitted for intendedResiliencyModel). Every spelling of a
    // value outside the type a caller can send states ONE set in ONE order: the
    // order belongs to the type, so nothing about the request moves it.
    let rows = [
        ("a name no value carries", "superfast-resiliency"),
        ("an admitted value miscased", "Basic-Resiliency"),
        ("an admitted value padded", " high-resiliency"),
        ("the empty string", ""),
        ("a value carrying the set's own text", "basic-resiliency, high-resiliency"),
    ];

    for (case, submitted) in rows {
        let refused = dx(
            &router,
            "CreateResiliencyGroup",
            json!({
                "resiliencyGroupName": GROUP_NAME,
                "intendedResiliencyModel": submitted,
            }),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case:?}: expected 400 {VALIDATION_EXCEPTION}, got {} with {}",
            refused.status,
            refused.body
        );
        let expected = one_enum_error(submitted, "intendedResiliencyModel", RESILIENCY_MODEL_SET);
        assert_eq!(
            refused.message(),
            expected,
            "case {case:?}: expected {expected:?}, got {:?}",
            refused.message()
        );
        // And the order inside the brackets is the service's: ascending, which the
        // model's declaration order is not.
        let listed = stated_values(refused.message());
        let mut ascending = listed.clone();
        ascending.sort_unstable();
        assert_eq!(
            listed, ascending,
            "case {case:?}: the set must be stated ascending, got {listed:?}"
        );
    }
}

#[tokio::test]
async fn the_stated_set_does_not_move_when_a_request_breaks_a_second_constraint() {
    let router = emulator();

    // A second broken constraint moves the envelope's count and joins two clauses,
    // and leaves the bracketed set exactly where the one-error envelope has it: a
    // `resiliencyGroupName` outside its modelled `@pattern`, and a `clientToken`
    // below its modelled `@length` minimum, each beside the enumerated value.
    let rows: [(&str, Value); 2] = [
        (
            "a name outside its pattern, beside the enumerated value",
            json!({
                "resiliencyGroupName": "not a legal name",
                "intendedResiliencyModel": "superfast-resiliency",
            }),
        ),
        (
            "a client token below its length minimum, beside the enumerated value",
            json!({
                "resiliencyGroupName": GROUP_NAME,
                "intendedResiliencyModel": "superfast-resiliency",
                "clientToken": "",
            }),
        ),
    ];

    for (case, body) in rows {
        let refused = dx(&router, "CreateResiliencyGroup", body).await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case:?}: expected 400 {VALIDATION_EXCEPTION}, got {} with {}",
            refused.status,
            refused.body
        );
        let clause = format!(
            "Value 'superfast-resiliency' at 'intendedResiliencyModel' failed to satisfy \
             constraint: Member must satisfy enum value set: {RESILIENCY_MODEL_SET}"
        );
        assert!(
            refused.message().contains(&clause),
            "case {case:?}: expected a clause {clause:?}, got {:?}",
            refused.message()
        );
        assert!(
            refused.message().starts_with("2 validation errors detected: "),
            "case {case:?}: two broken constraints are two clauses in one envelope, got {:?}",
            refused.message()
        );
    }
}

#[tokio::test]
async fn an_enum_this_service_pins_no_order_of_is_still_stated_with_every_declared_value() {
    let router = emulator();

    // The seam this service edits chooses an ORDER for the sets it states one for and
    // touches nothing else: a value outside another enumerated type is refused in the
    // same envelope, at the same member path, listing exactly the values the model
    // declares for it — compared as a SET, because this service states no order of its
    // own for that type.
    let declared: Vec<String> = service_com_amazonaws_directconnect_overtureservice::ops::types::
        REQUEST_BILLING_MODE_VALUES
        .iter()
        .map(|value| (*value).to_string())
        .collect();

    for (case, submitted) in [
        ("a name no billing mode carries", "FreeTier"),
        ("an admitted billing mode miscased", "payasyougo"),
    ] {
        let refused = dx(
            &router,
            "UpdateConnectionsBillingMode",
            json!({"connectionIds": ["dxcon-ffabc123"], "billingMode": submitted}),
        )
        .await;
        assert_eq!(
            (refused.status, refused.error_code().as_str()),
            (400, VALIDATION_EXCEPTION),
            "case {case:?}: expected 400 {VALIDATION_EXCEPTION}, got {} with {}",
            refused.status,
            refused.body
        );
        let frame = format!(
            "1 validation error detected: Value '{submitted}' at 'billingMode' failed to satisfy \
             constraint: Member must satisfy enum value set: "
        );
        assert!(
            refused.message().starts_with(&frame),
            "case {case:?}: expected the envelope {frame:?}, got {:?}",
            refused.message()
        );
        let mut listed = stated_values(refused.message());
        listed.sort_unstable();
        let mut expected = declared.clone();
        expected.sort_unstable();
        assert_eq!(
            listed, expected,
            "case {case:?}: expected every value the model declares, got {listed:?}"
        );
    }
}
