// SPDX-License-Identifier: CC0-1.0

//! An id the Amazon Bedrock control plane publishes a model under is an id the Bedrock
//! Runtime plane resolves.
//!
//! The rule under test: **a published model is offered under its base model id and under the
//! version-and-context-qualified id Amazon Bedrock publishes it with beside it — the model id
//! followed by the `:`-separated version and context-length qualifiers a `CustomModelArn`
//! embeds (`([:][a-z0-9-]{1,63}){0,2}`,
//! `smithy_query(query="operation",operation="CreateCustomModelDeployment")`) — and BOTH
//! planes resolve the model under either id.** The qualified id is the one a Provisioned
//! Throughput over the model names, so it is published for a DEPRECATED model
//! (`FoundationModelLifecycleStatus` `LEGACY`,
//! `smithy_query(query="shape",shape="FoundationModelLifecycle")`) as well as for an
//! available one; what a deprecated model takes none of is a new CUSTOMIZATION, which its
//! own crate's suite pins.
//!
//! Two crates have to agree about that: the control plane decides which ids are catalogue
//! ROWS (`ListFoundationModels`, `GetFoundationModel`) and the Bedrock Runtime plane decides
//! which ids an invocation RESOLVES, so a model the catalogue publishes must not be an
//! identifier the runtime cannot interpret — the control plane's own suite
//! (`services/service-com-amazonaws-bedrock-amazonbedrockcontrolplaneservice/tests/all/foundation_models.rs`,
//! rule 7) pins the catalogue half, and a crate-local suite can see only one of the two
//! planes.
//!
//! A qualified id the catalogue publishes NO row of its own under is still read by the
//! control plane as the model its unqualified prefix names, and that half is asserted for it
//! too; the inference plane carries the ids it serves, which is what the sibling suite
//! `bedrock_catalogue_is_invocable.rs` holds it to, so nothing is claimed here about an id no
//! catalogue row publishes.
//!
//! The rows span what the rule generalises over rather than one id: both lifecycle statuses,
//! the two qualifier depths a base model id is published with (one, `…-v1:0`, and two,
//! `…-v1:2:8k`), three providers, a model each plane serves an inference for beside ones it
//! recognises and serves none for, and an id the catalogue publishes no row of its own under.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The signing name the whole Bedrock family shares; the route decides which member serves
/// a request.
const BEDROCK: &str = "bedrock";

/// The data plane's own message for a `modelId` it cannot interpret as naming a model it
/// offers — the one answer an id the control plane still resolves must never get.
const INVALID_MODEL_IDENTIFIER: &str = "The provided model identifier is invalid.";

/// The emulator with the two Bedrock planes on ONE engine, as `core/router/src/lib.rs`
/// mounts them.
fn build_router() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_bedrock_amazonbedrockcontrolplaneservice::EmulatedService::new(
            Arc::clone(&engine),
        ),
    );
    router.register_service(
        service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// A SigV4 `Authorization` header whose credential scope names the Bedrock signing name,
/// which is what routes the request. The signature is never checked.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/{BEDROCK}/\
         aws4_request, SignedHeaders=host, Signature=0000000000000000000000000000000000000\
         000000000000000000000000"
    )
}

/// One call to either plane, answered with the status and the document.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    uri: &str,
    host: &str,
    body: Option<Value>,
) -> (u16, Value) {
    let payload = body
        .map(|value| Bytes::from(value.to_string()))
        .unwrap_or_default();
    let request = http::Request::builder()
        .method(method)
        .uri(uri)
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization())
        .body(payload)
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

/// An identifier as an SDK writes it into a path label: `:` is percent-encoded so the whole
/// identifier stays ONE path segment.
fn label(identifier: &str) -> String {
    identifier.replace(':', "%3A")
}

/// One `:`-qualified identifier a client may address a published model by: the catalogue row
/// it names, and whether the catalogue publishes a row of its OWN under it.
struct Variant {
    identifier: &'static str,
    base: &'static str,
    own_row: bool,
}

/// The identifiers this rule covers: the version-and-context-qualified ids the catalogue
/// publishes a row of its own under — for a deprecated model and for available ones, with one
/// qualifier and with two, from three providers — beside a qualified id no row is published
/// under, which the control plane still reads as the model its prefix names.
const VARIANTS: &[Variant] = &[
    Variant {
        identifier: "cohere.command-text-v14:7:4k",
        base: "cohere.command-text-v14:7:4k",
        own_row: true,
    },
    Variant {
        identifier: "cohere.command-light-text-v14:7:4k",
        base: "cohere.command-light-text-v14:7:4k",
        own_row: true,
    },
    Variant {
        identifier: "cohere.embed-english-v3:0:512",
        base: "cohere.embed-english-v3:0:512",
        own_row: true,
    },
    Variant {
        identifier: "amazon.titan-embed-image-v1:0",
        base: "amazon.titan-embed-image-v1:0",
        own_row: true,
    },
    Variant {
        identifier: "amazon.titan-embed-text-v1:2:8k",
        base: "amazon.titan-embed-text-v1:2:8k",
        own_row: true,
    },
    Variant {
        identifier: "amazon.nova-lite-v1:0:300k",
        base: "amazon.nova-lite-v1:0",
        own_row: false,
    },
];

/// Every `modelId` the control plane publishes a row for.
async fn published_ids(router: &MultiServiceRouter) -> Vec<String> {
    let (status, listed) = call(
        router,
        "GET",
        "/foundation-models",
        &format!("bedrock.{REGION}.amazonaws.com"),
        None,
    )
    .await;
    assert_eq!(
        status, 200,
        "expected ListFoundationModels to answer the catalogue, got {status} with {listed}"
    );
    listed["modelSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("expected modelSummaries in {listed}"))
        .iter()
        .map(|summary| summary["modelId"].as_str().unwrap_or_default().to_string())
        .collect()
}

#[tokio::test]
async fn a_qualified_id_resolves_in_both_planes_whether_or_not_it_is_a_catalogue_row() {
    let router = build_router();
    let published = published_ids(&router).await;
    assert!(
        published.len() > 50,
        "expected the published catalogue to carry the models of every provider, got {} \
         entries",
        published.len()
    );

    for row in VARIANTS {
        let case = row.identifier;

        // The catalogue publishes a row of its own under the qualified ids it offers the
        // model under; either way the row the id names is published.
        let carried = published.iter().any(|id| id == case);
        assert_eq!(
            carried, row.own_row,
            "case {case:?}: the catalogue publishes a row of its own under this id: {}, got \
             {carried}",
            row.own_row
        );
        assert!(
            published.iter().any(|id| id == row.base),
            "case {case:?}: the model's base id {:?} must be published, got {} rows without \
             it",
            row.base,
            published.len()
        );

        // The control plane reads the qualified id as the row it names.
        let (status, body) = call(
            &router,
            "GET",
            &format!("/foundation-models/{}", label(case)),
            &format!("bedrock.{REGION}.amazonaws.com"),
            None,
        )
        .await;
        assert_eq!(
            status, 200,
            "case {case:?}: a qualified id names a published model, so a read of it must \
             answer 200, got {status} with {body}"
        );
        assert_eq!(
            body["modelDetails"]["modelId"].as_str(),
            Some(row.base),
            "case {case:?}: the read must report the row the id resolves to, got {body}"
        );

        // And the inference plane resolves every id the catalogue publishes a row under: a
        // model one plane offers must not be an identifier the other cannot interpret.
        if !row.own_row {
            continue;
        }
        let (status, answered) = call(
            &router,
            "POST",
            &format!("/model/{}/invoke", label(case)),
            &format!("bedrock-runtime.{REGION}.amazonaws.com"),
            Some(json!({ "prompt": "hi" })),
        )
        .await;
        let message = answered["message"].as_str().unwrap_or_default();
        assert_ne!(
            message, INVALID_MODEL_IDENTIFIER,
            "case {case:?}: the id names a published model, so the inference plane must not \
             report it as an identifier it cannot interpret (got {status} with {answered})"
        );
        assert!(
            status == 200 || (400..500).contains(&status),
            "case {case:?}: expected the answer or a modelled client refusal, got {status} \
             with {answered}"
        );
    }
}
