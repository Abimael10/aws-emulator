// SPDX-License-Identifier: CC0-1.0

//! The AgentCore payments plane's instruments resolve their payment MANAGER against the
//! control plane that owns it.
//!
//! RULE: a data-plane payments call names a `paymentManagerArn` the AgentCore CONTROL plane
//! minted, and resolves it against that plane BEFORE creating or reporting anything: a
//! manager that plane holds is served from the data plane's own records, and one nothing
//! created is refused with `AccessDeniedException` (HTTP 403) and the plane's fixed sentence,
//! leaving the store untouched. The two planes are separate services over one shared engine
//! and neither API creates the other's resource, which is why this suite lives here: only an
//! assembled router carrying both can drive the rule end to end. The per-input tables for
//! the two operations themselves live in the owning crate's own suite
//! (`services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/tests/all/payment_sessions.rs`).
//!
//! The rows span what the rule generalises over: which operation of the instrument surface
//! makes the call (the create and the listing), and whether the manager it names was minted
//! by the control plane or invented.

use std::sync::Arc;

use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

use service_com_amazonaws_bedrockagentcore_amazonbedrockagentcore as agentcore;
use service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as agentcorecontrol;

// The error members of a refusal: the sibling cross-plane suite owns them, so every suite
// here reads a refusal the one way.
use crate::bedrock_agentcore_data_plane_parent_resources::{error_code, message};

/// The region every request here is signed for.
const REGION: &str = "us-east-1";

/// A manager of the modelled `PaymentManagerArn` form (length 66..2048, source: smithy_query
/// query="shape" shape="PaymentManagerArn") that no create ever minted, in another partition
/// and region so the case does not depend on where a minted one lives.
const GHOST_MANAGER_ARN: &str =
    "arn:aws-us-gov:bedrock-agentcore:us-gov-west-1:000000000000:payment-manager/ghost-9z8y7x6w5v";

/// A connector id of the modelled `PaymentConnectorId` form (length 12..211, pattern
/// `^([0-9a-z][-]?){1,100}-[0-9a-z]{10}$`). A payment connector is no component's resource
/// here, so the id is the caller's own value.
const CONNECTOR: &str = "connector-0123456789";

/// The only `PaymentInstrumentType` the model publishes (source: smithy_query query="shape"
/// shape="PaymentInstrumentType").
const INSTRUMENT_TYPE: &str = "EMBEDDED_CRYPTO_WALLET";

/// The role a payment manager runs under, in the form the control plane's `RoleArn`
/// publishes.
const ROLE_ARN: &str = "arn:aws:iam::123456789012:role/AgentCorePayments";

/// The sentence this plane refuses a manager the account does not hold with: fixed text for
/// the error class, carrying nothing from the request.
const MANAGER_NOT_FOUND: &str = "Payment manager not found";

/// AgentCore Control and the AgentCore data plane over ONE shared engine, exactly as
/// `router/src/lib.rs` mounts them, with request authentication off so a test signs nothing.
fn emulator() -> (MultiServiceRouter, Arc<engine::InMemoryEngine>) {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(agentcorecontrol::EmulatedService::new(Arc::clone(&engine)));
    router.register_service(agentcore::EmulatedService::new(Arc::clone(&engine)));
    (router, engine)
}

/// One restJson1 call of either plane; the ROUTE is what picks the plane that answers.
async fn call(router: &MultiServiceRouter, path: &str, body: Value) -> (u16, Value) {
    let request = http::Request::builder()
        .method("POST")
        .uri(format!(
            "https://bedrock-agentcore.{REGION}.amazonaws.com{path}"
        ))
        .header("host", format!("bedrock-agentcore.{REGION}.amazonaws.com"))
        .header(
            "authorization",
            format!(
                "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20260101/{REGION}/bedrock-agentcore/\
                 aws4_request, SignedHeaders=host, Signature=abc"
            ),
        )
        .header("content-type", "application/json")
        .body(bytes::Bytes::from(body.to_string()))
        .expect("a well-formed request");
    let response = router.dispatch(request).await;
    let status = response.status().as_u16();
    let bytes = response.into_body();
    let parsed = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, parsed)
}

/// The create body for an instrument under `manager_arn`, linked to one email account on one
/// network, as `PaymentInstrumentDetails` models it (source: smithy_query query="closure"
/// operation="CreatePaymentInstrument").
fn create_body(manager_arn: &str) -> Value {
    json!({
        "paymentManagerArn": manager_arn,
        "paymentConnectorId": CONNECTOR,
        "paymentInstrumentType": INSTRUMENT_TYPE,
        "paymentInstrumentDetails": {
            "embeddedCryptoWallet": {
                "network": "ETHEREUM",
                "linkedAccounts": [{"email": {"emailAddress": "wallet.owner@example.com"}}],
            }
        },
    })
}

/// How many payment instruments the data plane holds, so a denial can be shown to have
/// written nothing.
fn instrument_count(engine: &engine::InMemoryEngine) -> usize {
    let state = agentcore::state::service_state(engine).expect("the data plane's state");
    let store = state.lock();
    store.payment_instruments.len()
}

/// An instrument created under a manager the CONTROL plane minted is served and listed by the
/// DATA plane, while the same two calls against a manager nothing minted are denied and write
/// nothing.
#[tokio::test]
async fn payment_instruments_resolve_their_manager_against_the_control_plane() {
    let (router, engine) = emulator();

    // `CreatePaymentManager` on the control plane: `POST /payments/managers`, answering 202.
    let (status, manager) = call(
        &router,
        "/payments/managers",
        json!({"name": "payments", "authorizerType": "AWS_IAM", "roleArn": ROLE_ARN}),
    )
    .await;
    assert_eq!(
        status, 202,
        "the control plane must mint the payment manager, got {status} with {manager:?}"
    );
    let manager_arn = manager["paymentManagerArn"]
        .as_str()
        .unwrap_or_else(|| panic!("CreatePaymentManager reported no ARN: {manager:?}"))
        .to_string();

    // The data plane creates an instrument under the manager that plane holds.
    let (status, created) = call(
        &router,
        "/payments/createPaymentInstrument",
        create_body(&manager_arn),
    )
    .await;
    assert_eq!(
        status, 201,
        "CreatePaymentInstrument must answer 201 for a minted manager, got {status} with \
         {created:?}"
    );
    let instrument_id = created["paymentInstrument"]["paymentInstrumentId"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    assert_eq!(
        created["paymentInstrument"]["paymentManagerArn"].as_str(),
        Some(manager_arn.as_str()),
        "the instrument must belong to the manager the request named, got {:?}",
        created["paymentInstrument"]["paymentManagerArn"]
    );

    // And reports it back from its own records.
    let (status, listed) = call(
        &router,
        "/payments/listPaymentInstruments",
        json!({"paymentManagerArn": manager_arn}),
    )
    .await;
    assert_eq!(
        status, 200,
        "ListPaymentInstruments must answer 200 for a minted manager, got {status} with \
         {listed:?}"
    );
    assert_eq!(
        listed["paymentInstruments"][0]["paymentInstrumentId"].as_str(),
        Some(instrument_id.as_str()),
        "the listing must report the instrument that was created, got {listed:?}"
    );

    // A manager nothing minted is refused on BOTH operations, before anything is written.
    let held = instrument_count(&engine);
    for (operation, path, body) in [
        (
            "CreatePaymentInstrument",
            "/payments/createPaymentInstrument",
            create_body(GHOST_MANAGER_ARN),
        ),
        (
            "ListPaymentInstruments",
            "/payments/listPaymentInstruments",
            json!({"paymentManagerArn": GHOST_MANAGER_ARN}),
        ),
    ] {
        let (status, refused) = call(&router, path, body).await;
        assert_eq!(
            status, 403,
            "case {operation} against a manager nothing minted: expected HTTP 403, got {status} \
             with {refused:?}"
        );
        assert_eq!(
            error_code(&refused),
            "AccessDeniedException",
            "case {operation} against a manager nothing minted: expected an \
             AccessDeniedException, got {refused:?}"
        );
        assert_eq!(
            message(&refused),
            MANAGER_NOT_FOUND,
            "case {operation} against a manager nothing minted: expected {MANAGER_NOT_FOUND:?}, \
             got {:?}",
            message(&refused)
        );
        assert_eq!(
            instrument_count(&engine),
            held,
            "case {operation} against a manager nothing minted: the store must be untouched"
        );
    }
}
