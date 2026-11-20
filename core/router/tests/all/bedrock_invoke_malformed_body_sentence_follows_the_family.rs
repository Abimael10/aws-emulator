// SPDX-License-Identifier: CC0-1.0

//! What decides the sentence an unreadable `InvokeModel` body is refused in.
//!
//! The rule under test: Amazon Bedrock hands `InvokeModel$body` to the invoked model's
//! PROVIDER in that provider's own format ("The prompt and inference parameters in the format
//! specified in the contentType in the header. You must provide the body in JSON format",
//! `smithy_query(query="operation",operation="InvokeModel")` — `body` is the `@httpPayload`
//! blob and `modelId` the required `@httpLabel`), so a body that is no request object of that
//! provider at all is refused `400 ValidationException` in the one fixed sentence that
//! provider's request validator states the class in, and the model FAMILY the `modelId`
//! resolves to is the ONLY thing that decides which sentence that is: a provider that
//! publishes its own native envelope answers "Malformed input request, please reformat your
//! input and try again.", a provider Amazon Bedrock serves on the OpenAI-compatible
//! chat-completions envelope answers "The JSON object in the request body is malformed. Fix
//! the syntax errors and retry your request."
//!
//! Nothing else about the call may change that sentence, which is what these tables run over:
//!
//! * the SURFACE that took the request — `InvokeModel` and `InvokeModelWithResponseStream`
//!   take the same request (`smithy_query(query="operation")` on both: the two inputs differ
//!   only in the header `accept` binds), so they refuse it identically;
//! * the SPELLING of `modelId` — the bare base-model id, a cross-region inference-profile id,
//!   a `foundation-model` ARN and an `inference-profile` ARN, every spelling the member's
//!   pattern allows for a base model, including a model offered only through an inference
//!   profile, where the spelling decides the THROUGHPUT and still not the sentence;
//! * the media types the request DECLARED (an absent `contentType`, one carrying a charset
//!   parameter, and each `accept` the operation documents);
//! * what came BEFORE the call on the same model, and repeating the call — a refusal is not
//!   a served inference, so neither a preceding `CountTokens`, a preceding served invocation
//!   nor a second identical attempt moves it.
//!
//! `services/.../tests/all/invoke_model_malformed_body_refusal.rs` runs the same rule over the
//! FORMS an unreadable body takes (each way JSON syntax breaks, each JSON type that is not an
//! object, the size of the body); this suite holds the dimensions around the body.
//!
//! This suite lives here, beside the other suites of the assembled emulator, because the
//! owning crate still roots its integration tests at a hand-declared `tests/all.rs`, which a
//! fix may not edit; the router's `tests/all/` picks a new suite up with nothing to declare.
//! Every call goes through a router with the owning service mounted on one shared engine,
//! exactly as the production router mounts it, so each assertion exercises the real wire path.

use std::sync::Arc;

use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

/// A host-style endpoint for the service, so a request routes however the SigV4 scope below
/// is read.
const ENDPOINT: &str = "https://bedrock-runtime.us-east-1.amazonaws.com";

/// A SigV4 `Authorization` header scoped to this service's signing name (`bedrock`) and to
/// `us-east-1`. Request authentication is off here, so the signature is not checked.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEMULATOR000000000/20260101/us-east-1/bedrock/aws4_request, \
     SignedHeaders=host, Signature=0000000000000000000000000000000000000000000000000000000000000000";

/// The sentence a provider that publishes its OWN native request envelope refuses this class
/// in, pinned literally rather than read from the constant the service carries.
const NATIVE_ENVELOPE_SENTENCE: &str = "Malformed input request, please reformat your input and try again.";

/// The sentence a provider Amazon Bedrock serves on the OpenAI-compatible chat-completions
/// envelope refuses this class in.
const OPENAI_ENVELOPE_SENTENCE: &str =
    "The JSON object in the request body is malformed. Fix the syntax errors and retry your \
     request.";

/// The two invoke surfaces, by the last path segment each binds:
/// `/model/{modelId}/invoke` and `/model/{modelId}/invoke-with-response-stream`.
const SURFACES: [&str; 2] = ["invoke", "invoke-with-response-stream"];

/// One model per `InvokeModel` request family, with the sentence its provider's validator
/// refuses an unreadable body in. Model ids from "Amazon Bedrock base model IDs (on-demand
/// throughput)" in the Amazon Bedrock User Guide; the families are the request envelopes
/// "Inference request parameters and response fields for foundation models" documents.
const FAMILIES: [(&str, &str, &str); 10] = [
    ("anthropic messages", "anthropic.claude-3-5-haiku-20241022-v1:0", NATIVE_ENVELOPE_SENTENCE),
    ("titan text", "amazon.titan-text-lite-v1", NATIVE_ENVELOPE_SENTENCE),
    ("nova", "amazon.nova-pro-v1:0", NATIVE_ENVELOPE_SENTENCE),
    ("llama", "meta.llama3-70b-instruct-v1:0", NATIVE_ENVELOPE_SENTENCE),
    ("mistral", "mistral.mixtral-8x7b-instruct-v0:1", NATIVE_ENVELOPE_SENTENCE),
    ("cohere command", "cohere.command-r-plus-v1:0", NATIVE_ENVELOPE_SENTENCE),
    ("openai-compatible: openai", "openai.gpt-oss-120b-1:0", OPENAI_ENVELOPE_SENTENCE),
    ("openai-compatible: deepseek", "deepseek.v3-v1:0", OPENAI_ENVELOPE_SENTENCE),
    ("openai-compatible: qwen", "qwen.qwen3-coder-30b-a3b-v1:0", OPENAI_ENVELOPE_SENTENCE),
    ("openai-compatible: moonshotai", "moonshotai.kimi-k3", OPENAI_ENVELOPE_SENTENCE),
];

/// A body that is no request object of any family: bytes whose JSON does not parse.
const UNREADABLE_BODY: &[u8] = b"{\"messages\": [{\"role\": \"user\", ";

/// A body the Anthropic Claude Messages API accepts, which binds the three members that API
/// documents as required.
const CLAUDE_BODY: &str = r#"{"anthropic_version":"bedrock-2023-05-31","max_tokens":32,"messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}"#;

/// A fresh emulator: one router with Bedrock Runtime mounted over one shared engine, the way
/// the production router mounts it.
fn emulator() -> MultiServiceRouter {
    let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
    let mut router = MultiServiceRouter::new();
    router.set_request_authentication(false);
    router.register_service(
        service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::EmulatedService::new(
            engine,
        ),
    );
    router
}

/// One HTTP exchange.
struct Call {
    status: u16,
    error_code: String,
    body: bytes::Bytes,
}

impl Call {
    /// The refusal message the response carries, or the empty string when it carries none.
    fn message(&self) -> String {
        serde_json::from_slice::<Value>(&self.body)
            .ok()
            .as_ref()
            .and_then(|json| json.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The whole response body, for an assertion that needs to report what came back.
    fn rendered(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }
}

/// Percent-encode the characters a `modelId` carries that cannot sit in a path segment
/// unescaped. Only `:` and `/` occur in the identifiers this suite spells.
fn encode_path(value: &str) -> String {
    value.replace(':', "%3A").replace('/', "%2F")
}

/// `POST` one request to the service, with the headers the row declares.
async fn send(
    router: &MultiServiceRouter,
    path: &str,
    headers: &[(&str, &str)],
    body: Vec<u8>,
) -> Call {
    let mut request = http::Request::builder()
        .method("POST")
        .uri(format!("{ENDPOINT}{path}"))
        .header("authorization", AUTHORIZATION)
        .header("host", "bedrock-runtime.us-east-1.amazonaws.com");
    for (name, value) in headers {
        request = request.header(*name, *value);
    }
    let request = request
        .body(bytes::Bytes::from(body))
        .expect("a well-formed test request");
    let (parts, body) = router.dispatch(request).await.into_parts();
    let error_code = parts
        .headers
        .get("x-amzn-errortype")
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default()
        .to_string();
    Call {
        status: parts.status.as_u16(),
        error_code,
        body,
    }
}

/// Invoke `model_id` on `surface` with `body` and the declared media types.
async fn invoke(
    router: &MultiServiceRouter,
    surface: &str,
    model_id: &str,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Call {
    let path = format!("/model/{}/{surface}", encode_path(model_id));
    send(router, &path, headers, body.to_vec()).await
}

/// Assert one refusal: `400 ValidationException` carrying exactly `expected`.
fn assert_refusal(case: &str, call: &Call, expected: &str) {
    assert_eq!(
        call.status,
        400,
        "case {case:?}: expected HTTP 400, got {} / {}",
        call.status,
        call.rendered()
    );
    assert_eq!(
        call.error_code, "ValidationException",
        "case {case:?}: expected ValidationException, got {:?}",
        call.error_code
    );
    assert_eq!(
        call.message(),
        expected,
        "case {case:?}: expected message {expected:?}, got {:?}",
        call.message()
    );
}

/// RULE: the sentence follows the invoked model's family, on either invoke surface — the two
/// operations take the same request, so they refuse an unreadable body identically.
#[tokio::test]
async fn both_invoke_surfaces_refuse_an_unreadable_body_in_the_families_own_sentence() {
    let router = emulator();

    for (family, model_id, expected) in FAMILIES {
        for surface in SURFACES {
            let case = format!("{family} on {surface}");
            let call = invoke(
                &router,
                surface,
                model_id,
                &[("content-type", "application/json")],
                UNREADABLE_BODY,
            )
            .await;
            assert_refusal(&case, &call, expected);
        }
    }
}

/// RULE: which model the identifier RESOLVES to decides the sentence, so every spelling the
/// `modelId` pattern allows for one base model is refused in the same sentence — the spelling
/// decides only what throughput the invocation asks for.
#[tokio::test]
async fn the_sentence_does_not_depend_on_how_the_model_was_addressed() {
    let router = emulator();

    // (family case, the spellings of ONE model, the family's sentence). The spellings are the
    // forms `InvokeModelIdentifier` allows for a base model: the bare id, a cross-region
    // inference-profile id (geography-prefixed, "Supported Regions and models for cross-region
    // inference"), a `foundation-model` ARN and an `inference-profile` ARN.
    let rows: [(&str, [&str; 4], &str); 3] = [
        (
            "a native-envelope family",
            [
                "amazon.nova-lite-v1:0",
                "us.amazon.nova-lite-v1:0",
                "arn:aws:bedrock:us-east-1::foundation-model/amazon.nova-lite-v1:0",
                "arn:aws:bedrock:us-east-1:000000000000:inference-profile/eu.amazon.nova-lite-v1:0",
            ],
            NATIVE_ENVELOPE_SENTENCE,
        ),
        (
            "a family served on the OpenAI-compatible envelope",
            [
                "openai.gpt-oss-20b-1:0",
                "apac.openai.gpt-oss-20b-1:0",
                "arn:aws:bedrock:us-east-1::foundation-model/openai.gpt-oss-20b-1:0",
                "arn:aws:bedrock:us-east-1:000000000000:inference-profile/us.openai.gpt-oss-20b-1:0",
            ],
            OPENAI_ENVELOPE_SENTENCE,
        ),
        // A model this inventory offers only THROUGH an inference profile: the two
        // foundation-model spellings of it are refused for the throughput they ask for
        // instead, which is a different class and asserted below.
        (
            "a model offered only through an inference profile",
            [
                "us.amazon.nova-micro-v1:0",
                "us-gov.amazon.nova-micro-v1:0",
                "arn:aws:bedrock:us-east-1:000000000000:inference-profile/us.amazon.nova-micro-v1:0",
                "arn:aws:bedrock:us-east-1:000000000000:inference-profile/apac.amazon.nova-micro-v1:0",
            ],
            NATIVE_ENVELOPE_SENTENCE,
        ),
    ];

    for (family, spellings, expected) in rows {
        for model_id in spellings {
            for surface in SURFACES {
                let case = format!("{family} / {model_id} on {surface}");
                let call = invoke(
                    &router,
                    surface,
                    model_id,
                    &[("content-type", "application/json")],
                    UNREADABLE_BODY,
                )
                .await;
                assert_refusal(&case, &call, expected);
            }
        }
    }

    // The same unreadable body, on the same model, addressed as a foundation model: the
    // identifier is refused for the throughput it asks for before any body is read, which is
    // what makes the rows above a statement about the FAMILY and not about the spelling.
    for model_id in [
        "amazon.nova-micro-v1:0",
        "arn:aws:bedrock:us-east-1::foundation-model/amazon.nova-micro-v1:0",
    ] {
        let call = invoke(
            &router,
            "invoke",
            model_id,
            &[("content-type", "application/json")],
            UNREADABLE_BODY,
        )
        .await;
        assert_eq!(
            call.status,
            400,
            "case {model_id:?}: expected HTTP 400, got {} / {}",
            call.status,
            call.rendered()
        );
        assert!(
            call.message().contains("on-demand throughput"),
            "case {model_id:?}: expected the on-demand throughput refusal, got {:?}",
            call.message()
        );
    }
}

/// RULE: the body is unreadable whatever media types the request declared, so the sentence is
/// the same for an absent `contentType`, one carrying a parameter, and each `accept` the
/// operation documents.
#[tokio::test]
async fn the_sentence_does_not_depend_on_the_declared_media_types() {
    let router = emulator();

    // (case, headers) — `contentType` "You must specify application/json" and `accept` "The
    // desired MIME type of the inference body in the response. The default value is
    // application/json", both from `smithy_query(query="operation",operation="InvokeModel")`.
    // A media type the model does not read is its own refusal class and is not a row here.
    let header_rows: [(&str, &[(&str, &str)]); 5] = [
        ("no media type declared at all", &[]),
        ("contentType alone", &[("content-type", "application/json")]),
        (
            "contentType with a charset parameter",
            &[("content-type", "application/json; charset=utf-8")],
        ),
        (
            "contentType and the default accept",
            &[
                ("content-type", "application/json"),
                ("accept", "application/json"),
            ],
        ),
        (
            "contentType and a wildcard accept",
            &[("content-type", "application/json"), ("accept", "*/*")],
        ),
    ];

    for (family, model_id, expected) in [FAMILIES[0], FAMILIES[6]] {
        for (header_case, headers) in header_rows {
            let case = format!("{family} / {header_case}");
            let call = invoke(&router, "invoke", model_id, headers, UNREADABLE_BODY).await;
            assert_refusal(&case, &call, expected);
        }
    }
}

/// RULE: a refused call is not a served inference, so nothing it is preceded by and nothing it
/// leaves behind changes it — a `CountTokens` on the same model, a served invocation of that
/// model, and the same attempt made twice all answer the one sentence.
#[tokio::test]
async fn the_sentence_is_unchanged_by_what_came_before_and_by_repeating_the_call() {
    let router = emulator();
    // `CountTokens` counts with the model's own tokenizer and AWS publishes it for the
    // Anthropic Claude family, so the sequence below names a model of that family.
    let model_id = "anthropic.claude-3-haiku-20240307-v1:0";
    let json = &[("content-type", "application/json")];

    let first = invoke(&router, "invoke", model_id, json, UNREADABLE_BODY).await;
    assert_refusal("the first attempt", &first, NATIVE_ENVELOPE_SENTENCE);

    let counted = send(
        &router,
        &format!("/model/{}/count-tokens", encode_path(model_id)),
        json,
        serde_json::to_vec(&serde_json::json!({
            "input": { "converse": { "messages": [
                { "role": "user", "content": [{ "text": "how many tokens is this" }] }
            ] } }
        }))
        .expect("a serializable test body"),
    )
    .await;
    assert_eq!(
        counted.status,
        200,
        "case \"CountTokens before the refused invoke\": expected HTTP 200, got {} / {}",
        counted.status,
        counted.rendered()
    );

    let after_count = invoke(&router, "invoke", model_id, json, UNREADABLE_BODY).await;
    assert_refusal("after a CountTokens call", &after_count, NATIVE_ENVELOPE_SENTENCE);

    let served = invoke(
        &router,
        "invoke",
        model_id,
        json,
        CLAUDE_BODY.as_bytes(),
    )
    .await;
    assert_eq!(
        served.status,
        200,
        "case \"a served invocation before the refused one\": expected HTTP 200, got {} / {}",
        served.status,
        served.rendered()
    );

    let after_served = invoke(&router, "invoke", model_id, json, UNREADABLE_BODY).await;
    assert_refusal(
        "after a served invocation",
        &after_served,
        NATIVE_ENVELOPE_SENTENCE,
    );

    let repeated = invoke(&router, "invoke", model_id, json, UNREADABLE_BODY).await;
    assert_refusal(
        "the same attempt repeated",
        &repeated,
        NATIVE_ENVELOPE_SENTENCE,
    );
    assert_eq!(
        repeated.message(),
        first.message(),
        "case \"the same attempt repeated\": expected the refusal {:?} again, got {:?}",
        first.message(),
        repeated.message()
    );
}
