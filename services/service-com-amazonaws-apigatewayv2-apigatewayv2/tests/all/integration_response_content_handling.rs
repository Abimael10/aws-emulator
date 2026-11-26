// SPDX-License-Identifier: CC0-1.0

//! What a `ContentHandlingStrategy` a request carries is READ as, over the restJson1 wire.
//!
//! The rule under test: **a content-handling strategy the enum declares is stored and
//! reported back, and a value it does not declare names NO payload conversion — so the
//! member is read as carrying no value at all: the write is served (the create 201, the
//! patch 200), a stored strategy is left exactly as it was, a new resource gets none, and
//! every other member of the same request is applied.** The member is optional with a
//! documented meaning for carrying nothing — "If this property is not defined, the response
//! payload will be passed through … without modification"
//! (`smithy_query(query="doc", shape="UpdateIntegrationResponseRequest",
//! member="ContentHandlingStrategy")`) — so a value naming none of the strategies states
//! something this API has a reading for rather than something it cannot read, which is what
//! the recorded service serves such a write with HTTP 200 for.
//!
//! The rule belongs to the ENUM, so every member of this model that targets it is held to
//! it: the integration response's (`CreateIntegrationResponse`,
//! `UpdateIntegrationResponse`) and the integration's own (`CreateIntegration`,
//! `UpdateIntegration`).
//!
//! The tables span what the rule generalises over: every value the model's own catalog
//! declares (so a value added to it is covered here without editing this suite), and the
//! ways a value misses that catalog — a declared value in the wrong case, a near miss, a
//! word of another vocabulary, a value of another enum of this model, surrounding
//! whitespace, the empty string, and the long token the recorded write carried, which is ONE
//! row. The last two tests pin the EDGES of the rule: a value that is no string at all is
//! still the codec's business, and every other enum member of this model is still judged by
//! its own value set.

use serde_json::{json, Value};

use service_com_amazonaws_apigatewayv2_apigatewayv2::ops::types::CONTENT_HANDLING_STRATEGY_VALUES;

use crate::api_lifecycle::{as_str, call, carries_nothing, emulator, error_code};
use crate::integration_response_key_grammar::{integration, mock_integration, websocket_api};

/// The member every case writes.
const MEMBER: &str = "contentHandlingStrategy";

/// The key an integration response is created under here: the catch-all, which is a key on
/// any integration.
const RESPONSE_KEY: &str = "$default";

/// The values that name no strategy, each with the way it misses the declared catalog.
///
/// `CONTENT_HANDLING_STRATEGY_VALUES` is `CONVERT_TO_BINARY | CONVERT_TO_TEXT`
/// (`smithy_query(query="shape", shape="ContentHandlingStrategy")`), so every row below is
/// outside it.
fn values_naming_no_strategy() -> Vec<(&'static str, &'static str)> {
    vec![
        ("a declared value in the wrong case", "convert_to_binary"),
        ("a declared value in mixed case", "Convert_To_Text"),
        ("a near miss", "CONVERT_TO_BINARIES"),
        ("a prefix of a declared value", "CONVERT_TO"),
        ("a word of another vocabulary", "CONVERT_TO_IMAGE"),
        // `ProtocolType`'s value, which is an enum of this model — but not this one.
        ("a value of another enum of this model", "WEBSOCKET"),
        ("a declared value with surrounding space", " CONVERT_TO_BINARY "),
        ("the empty value", ""),
        // The value the recorded write carried: ONE row of the class.
        ("an arbitrary token", "NOT_A_REAL_CONTENT_HANDLING"),
    ]
}

/// One WebSocket api carrying a `MOCK` integration with one integration response on it, and
/// the URIs of the two resources that carry a content-handling strategy.
struct Fixture {
    emulator: smithy_server_runtime::MultiServiceRouter,
    integration_uri: String,
    response_uri: String,
    response_id: String,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let emulator = emulator();
        let api_id = websocket_api(&emulator, name).await;
        let integration_id = integration(&emulator, &api_id, mock_integration()).await;
        let collection =
            format!("/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses");
        let (status, created) = call(
            &emulator,
            "POST",
            &collection,
            Some(json!({ "integrationResponseKey": RESPONSE_KEY })),
        )
        .await;
        assert_eq!(
            status, 201,
            "expected 201 from CreateIntegrationResponse, got {status} {created}"
        );
        let response_id = as_str(&created, "integrationResponseId");
        Self {
            emulator,
            integration_uri: format!("/v2/apis/{api_id}/integrations/{integration_id}"),
            response_uri: format!("{collection}/{response_id}"),
            response_id,
        }
    }

    /// `PATCH` the integration response with `body`.
    async fn patch_response(&self, body: Value) -> (u16, Value) {
        call(&self.emulator, "PATCH", &self.response_uri, Some(body)).await
    }

    /// `PATCH` the integration itself with `body`.
    async fn patch_integration(&self, body: Value) -> (u16, Value) {
        call(&self.emulator, "PATCH", &self.integration_uri, Some(body)).await
    }

    /// The integration response as `GetIntegrationResponse` reports it.
    async fn read_response(&self, case: &str) -> Value {
        let (status, fetched) = call(&self.emulator, "GET", &self.response_uri, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetIntegrationResponse, got {status} {fetched}"
        );
        fetched
    }

    /// The integration as `GetIntegration` reports it.
    async fn read_integration(&self, case: &str) -> Value {
        let (status, fetched) = call(&self.emulator, "GET", &self.integration_uri, None).await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetIntegration, got {status} {fetched}"
        );
        fetched
    }
}

/// RULE: every strategy the model declares is stored by the patch and read back by
/// `GetIntegrationResponse`, whichever one it is.
#[tokio::test]
async fn every_declared_content_handling_strategy_is_stored_and_read_back() {
    let fixture = Fixture::new("content-handling-declared").await;
    // The catalog comes from the MODEL, so a value added to it is covered here.
    for strategy in CONTENT_HANDLING_STRATEGY_VALUES {
        let (status, answered) = fixture.patch_response(json!({ MEMBER: strategy })).await;
        assert_eq!(
            status, 200,
            "case {strategy:?}: expected 200 from UpdateIntegrationResponse, got {status} \
             {answered}"
        );
        assert_eq!(
            as_str(&answered, MEMBER),
            *strategy,
            "case {strategy:?}: expected the answer to report the strategy sent, got {answered}"
        );
        let fetched = fixture.read_response(strategy).await;
        assert_eq!(
            as_str(&fetched, MEMBER),
            *strategy,
            "case {strategy:?}: expected the reader to report the stored strategy, got {fetched}"
        );
    }
}

/// RULE: a value naming none of the declared strategies is read as no value, so the patch is
/// served, the stored strategy is untouched, and the response's identity and key are the ones
/// it had.
#[tokio::test]
async fn a_strategy_the_enum_does_not_declare_leaves_the_stored_one_alone() {
    let fixture = Fixture::new("content-handling-undeclared-patch").await;
    // Something to leave alone: the first declared strategy, stored through the same patch.
    let held = CONTENT_HANDLING_STRATEGY_VALUES[0];
    let (status, stored) = fixture.patch_response(json!({ MEMBER: held })).await;
    assert_eq!(
        status, 200,
        "expected 200 storing {held:?} first, got {status} {stored}"
    );

    for (case, value) in values_naming_no_strategy() {
        let (status, answered) = fixture.patch_response(json!({ MEMBER: value })).await;
        assert_eq!(
            status, 200,
            "case {case:?} ({value:?}): expected 200 from UpdateIntegrationResponse, got \
             {status} {answered}"
        );
        assert_eq!(
            as_str(&answered, MEMBER),
            held,
            "case {case:?} ({value:?}): expected the answer to report the strategy the record \
             already held, got {answered}"
        );
        assert_eq!(
            as_str(&answered, "integrationResponseKey"),
            RESPONSE_KEY,
            "case {case:?} ({value:?}): expected the stored key to be reported, got {answered}"
        );
        assert_eq!(
            as_str(&answered, "integrationResponseId"),
            fixture.response_id,
            "case {case:?} ({value:?}): expected the response's own id, got {answered}"
        );
        let fetched = fixture.read_response(case).await;
        assert_eq!(
            as_str(&fetched, MEMBER),
            held,
            "case {case:?} ({value:?}): expected the reader to report the untouched strategy, \
             got {fetched}"
        );
    }
}

/// RULE: only the content-handling member is read as no value — the rest of the same request
/// is applied, so a patch carrying an undeclared strategy beside another member still stores
/// that member.
#[tokio::test]
async fn the_rest_of_a_patch_carrying_an_undeclared_strategy_is_applied() {
    let fixture = Fixture::new("content-handling-undeclared-beside").await;
    let patched_key = r"/2\d\d/";
    let (status, answered) = fixture
        .patch_response(json!({
            MEMBER: "NOT_A_REAL_CONTENT_HANDLING",
            "integrationResponseKey": patched_key,
            "responseTemplates": { "application/json": "{\"ok\":true}" }
        }))
        .await;
    assert_eq!(
        status, 200,
        "expected 200 from UpdateIntegrationResponse, got {status} {answered}"
    );
    let fetched = fixture.read_response("beside another member").await;
    for body in [&answered, &fetched] {
        assert_eq!(
            as_str(body, "integrationResponseKey"),
            patched_key,
            "case \"beside another member\": expected the patched key to be stored, got {body}"
        );
        assert_eq!(
            body.get("responseTemplates"),
            Some(&json!({ "application/json": "{\"ok\":true}" })),
            "case \"beside another member\": expected the patched templates to be stored, got \
             {body}"
        );
        assert!(
            carries_nothing(body, MEMBER),
            "case \"beside another member\": expected no strategy to have been stored, got \
             {body}"
        );
    }
}

/// RULE: the same reading on the CREATE — a response created with a value naming no strategy
/// is created with none, which is the documented pass-through.
#[tokio::test]
async fn a_create_carrying_an_undeclared_strategy_stores_no_strategy() {
    let emulator = emulator();
    let api_id = websocket_api(&emulator, "content-handling-create").await;
    let integration_id = integration(&emulator, &api_id, mock_integration()).await;
    let collection =
        format!("/v2/apis/{api_id}/integrations/{integration_id}/integrationresponses");

    // Each row needs a key of its own: one key per integration.
    for (index, (case, value)) in values_naming_no_strategy().into_iter().enumerate() {
        let key = format!("/{}0{index}/", 2 + index % 3);
        let (status, created) = call(
            &emulator,
            "POST",
            &collection,
            Some(json!({ "integrationResponseKey": key, MEMBER: value })),
        )
        .await;
        assert_eq!(
            status, 201,
            "case {case:?} ({value:?}): expected 201 from CreateIntegrationResponse, got \
             {status} {created}"
        );
        assert!(
            carries_nothing(&created, MEMBER),
            "case {case:?} ({value:?}): expected the create to report no strategy, got {created}"
        );
        let response_id = as_str(&created, "integrationResponseId");
        let (status, fetched) = call(
            &emulator,
            "GET",
            &format!("{collection}/{response_id}"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: expected 200 from GetIntegrationResponse, got {status} {fetched}"
        );
        assert!(
            carries_nothing(&fetched, MEMBER),
            "case {case:?} ({value:?}): expected the reader to report no strategy, got {fetched}"
        );
        assert_eq!(
            as_str(&fetched, "integrationResponseKey"),
            key,
            "case {case:?}: expected the created key, got {fetched}"
        );
    }
}

/// RULE: the reading belongs to the enum, not to one operation — the integration's own
/// `ContentHandlingStrategy` is read the same way by its create and its patch.
#[tokio::test]
async fn an_integrations_own_strategy_is_read_the_same_way() {
    let fixture = Fixture::new("content-handling-integration").await;

    // A create carrying a value that names no strategy: accepted, with none stored.
    let emulator = emulator();
    let api_id = websocket_api(&emulator, "content-handling-integration-create").await;
    let (status, created) = call(
        &emulator,
        "POST",
        &format!("/v2/apis/{api_id}/integrations"),
        Some(json!({ "integrationType": "MOCK", MEMBER: "CONVERT_TO_IMAGE" })),
    )
    .await;
    assert_eq!(
        status, 201,
        "expected 201 from CreateIntegration, got {status} {created}"
    );
    assert!(
        carries_nothing(&created, MEMBER),
        "expected CreateIntegration to report no strategy, got {created}"
    );

    // And the patch: a declared value is stored, every value naming none leaves it.
    let held = CONTENT_HANDLING_STRATEGY_VALUES[1];
    let (status, answered) = fixture.patch_integration(json!({ MEMBER: held })).await;
    assert_eq!(
        status, 200,
        "expected 200 from UpdateIntegration, got {status} {answered}"
    );
    assert_eq!(
        as_str(&answered, MEMBER),
        held,
        "expected UpdateIntegration to report the strategy sent, got {answered}"
    );
    for (case, value) in values_naming_no_strategy() {
        let (status, answered) = fixture.patch_integration(json!({ MEMBER: value })).await;
        assert_eq!(
            status, 200,
            "case {case:?} ({value:?}): expected 200 from UpdateIntegration, got {status} \
             {answered}"
        );
        let fetched = fixture.read_integration(case).await;
        assert_eq!(
            as_str(&fetched, MEMBER),
            held,
            "case {case:?} ({value:?}): expected the integration to keep the strategy it held, \
             got {fetched}"
        );
    }
}

/// RULE (the edge): the reading is of a STRING the enum does not declare. A value that is no
/// string at all states no strategy and no reading of one either — the CODEC refuses it, in
/// the words the codec states for the type that is there, and nothing is stored.
#[tokio::test]
async fn a_value_that_is_no_string_is_still_refused() {
    let fixture = Fixture::new("content-handling-not-a-string").await;
    let held = CONTENT_HANDLING_STRATEGY_VALUES[0];
    let (status, stored) = fixture.patch_response(json!({ MEMBER: held })).await;
    assert_eq!(
        status, 200,
        "expected 200 storing {held:?} first, got {status} {stored}"
    );

    // (case, the value, the sentence the codec states for that JSON type). The rows span the
    // JSON types that are not a string; the sentences are the codec's own
    // (`vendor/smithy-schema-server/src/json.rs`, whose `read_string` names the token kind it
    // found there), which is what pins that such a value is still refused BY the codec rather
    // than by anything this service words itself.
    let rows: &[(&str, Value, &str)] = &[
        ("a number", json!(7), "expected string, got number"),
        ("a boolean", json!(true), "expected string, got boolean"),
        (
            "an array",
            json!(["CONVERT_TO_BINARY"]),
            "expected string, got array start",
        ),
        (
            "an object",
            json!({ "strategy": "CONVERT_TO_BINARY" }),
            "expected string, got object start",
        ),
        ("null", json!(null), "expected string, got null"),
    ];
    for (case, value, stated) in rows {
        let (status, body) = fixture.patch_response(json!({ MEMBER: value })).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400 from UpdateIntegrationResponse, got {status} {body}"
        );
        assert_eq!(
            as_str(&body, "message"),
            *stated,
            "case {case:?}: expected the codec's own sentence, got {body}"
        );
        let fetched = fixture.read_response(case).await;
        assert_eq!(
            as_str(&fetched, MEMBER),
            held,
            "case {case:?}: expected the refused patch to have changed nothing, got {fetched}"
        );
    }
}

/// RULE (the other edge): the reading is this one enum's. Every other enum member of this
/// model is judged by its own value set exactly as the model declares it, so a value outside
/// one is still refused `400 BadRequestException`.
#[tokio::test]
async fn a_value_outside_another_enum_of_this_model_is_still_refused() {
    let emulator = emulator();
    let api_id = websocket_api(&emulator, "content-handling-other-enums").await;

    // (case, method, uri, body) — one write per enum member, spanning a create and a patch.
    let rows: Vec<(&str, &str, String, Value)> = vec![
        (
            "an IntegrationType outside its enum",
            "POST",
            format!("/v2/apis/{api_id}/integrations"),
            json!({ "integrationType": "MOCKED" }),
        ),
        (
            "a PassthroughBehavior outside its enum",
            "POST",
            format!("/v2/apis/{api_id}/integrations"),
            json!({ "integrationType": "MOCK", "passthroughBehavior": "ALWAYS" }),
        ),
        (
            "an IpAddressType outside its enum, on a patch",
            "PATCH",
            format!("/v2/apis/{api_id}"),
            json!({ "ipAddressType": "IPv4" }),
        ),
    ];
    for (case, method, uri, body) in rows {
        let (status, answered) = call(&emulator, method, &uri, Some(body)).await;
        assert_eq!(
            status, 400,
            "case {case:?}: expected 400, got {status} {answered}"
        );
        assert_eq!(
            error_code(&answered),
            "BadRequestException",
            "case {case:?}: expected BadRequestException, got {answered}"
        );
    }
}
