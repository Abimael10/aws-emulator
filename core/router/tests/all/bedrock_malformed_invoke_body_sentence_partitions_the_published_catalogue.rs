// SPDX-License-Identifier: CC0-1.0

//! Which ENVELOPE a model is served on decides the sentence an unreadable `InvokeModel` body
//! is refused in — asserted over models taken from the catalogue the control plane publishes,
//! one provider per envelope family, rather than over model ids written into a test.
//!
//! The rule under test: Amazon Bedrock hands `InvokeModel$body` to the invoked model's
//! PROVIDER in that provider's own format ("The prompt and inference parameters in the format
//! specified in the contentType in the header. You must provide the body in JSON format",
//! `smithy_query(query="operation",operation="InvokeModel")` — `body` is the `@httpPayload`
//! blob and `modelId` the required `@httpLabel`), so a body that is no request object of that
//! provider at all is refused `400 ValidationException` in the one fixed sentence that
//! provider's request validator states the class in. The wording therefore follows the
//! provider's request envelope and NOT the operation: a provider that publishes its own native
//! envelope answers "Malformed input request, please reformat your input and try again.", and a
//! provider Amazon Bedrock serves on the OpenAI-compatible chat-completions envelope answers
//! "The JSON object in the request body is malformed. Fix the syntax errors and retry your
//! request." One sentence for the whole catalogue satisfies neither half of that.
//!
//! What makes this suite a statement about the CATALOGUE rather than about a list of ids: every
//! model it invokes is one `ListFoundationModels` just reported, so each provider below is
//! exercised on whatever models that provider currently publishes — models no issue cited, and
//! models added after this was written. What it does NOT do is require the catalogue to have a
//! particular membership: a provider the listing no longer carries, a model whose invocation
//! never reaches the body (another modality, a model past its end of life, a model offered only
//! through an inference profile) and a refusal class this suite does not know are passed over
//! rather than failed, because none of them is a statement about the refusal WORDING. Whether
//! the two planes carry the same models is a different rule, pinned by
//! `bedrock_catalogue_is_invocable`.
//!
//! The suites beside this one run the same rule over the FORMS an unreadable body takes
//! (`services/.../tests/all/invoke_model_malformed_body_refusal.rs`) and over the dimensions
//! around the body — the invoke surface, the spelling of the `modelId`, the declared media
//! types (`bedrock_invoke_malformed_body_sentence_follows_the_family.rs`); this one needs the
//! control plane's own catalogue as its input, so it belongs here, with both planes mounted on
//! one engine the way the production router mounts them.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::Value;
use smithy_server_runtime::MultiServiceRouter;

/// The Region every request here is signed for.
const REGION: &str = "us-east-1";

/// The sentence a provider that publishes its OWN native request envelope refuses this class
/// in, pinned literally rather than read from the constant the service carries.
const NATIVE_ENVELOPE_SENTENCE: &str =
    "Malformed input request, please reformat your input and try again.";

/// The sentence a provider Amazon Bedrock serves on the OpenAI-compatible chat-completions
/// envelope refuses this class in.
const OPENAI_ENVELOPE_SENTENCE: &str =
    "The JSON object in the request body is malformed. Fix the syntax errors and retry your \
     request.";

/// A provider of the published catalogue, by the `providerName` the listing reports for it,
/// with the envelope its `InvokeModel` requests are written against and the sentence that
/// envelope's validator refuses an unreadable body in.
///
/// The envelopes are the ones documented per provider in "Inference request parameters and
/// response fields for foundation models" in the Amazon Bedrock User Guide (each of the five
/// native ones) and the OpenAI-compatible chat-completions API Amazon Bedrock serves the
/// remaining providers' models on.
const PROVIDERS: &[(&str, &str)] = &[
    ("Amazon", NATIVE_ENVELOPE_SENTENCE),
    ("Anthropic", NATIVE_ENVELOPE_SENTENCE),
    ("Cohere", NATIVE_ENVELOPE_SENTENCE),
    ("Meta", NATIVE_ENVELOPE_SENTENCE),
    ("Mistral AI", NATIVE_ENVELOPE_SENTENCE),
    ("DeepSeek", OPENAI_ENVELOPE_SENTENCE),
    ("Moonshot AI", OPENAI_ENVELOPE_SENTENCE),
    ("OpenAI", OPENAI_ENVELOPE_SENTENCE),
    ("Qwen", OPENAI_ENVELOPE_SENTENCE),
];

/// How many of a provider's published models one row invokes: enough that the sentence is read
/// off more than one model of the provider, bounded so the suite's cost does not grow with the
/// catalogue.
const MODELS_PER_PROVIDER: usize = 4;

/// A body that is no request object of any provider: bytes whose JSON does not parse.
const UNREADABLE_BODY: &[u8] = b"this is not json{{{";

/// A body the Anthropic Claude Messages API accepts: the three members that API documents as
/// required.
const CLAUDE_BODY: &str = r#"{"anthropic_version":"bedrock-2023-05-31","max_tokens":32,"messages":[{"role":"user","content":[{"type":"text","text":"hi"}]}]}"#;

/// A body the OpenAI-compatible chat-completions envelope accepts: the chat transcript that
/// envelope binds its prompt as.
const OPENAI_BODY: &str = r#"{"messages":[{"role":"user","content":"hi"}],"max_tokens":32}"#;

/// The emulator with the two Bedrock planes on ONE engine, as `core/router/src/lib.rs` mounts
/// them, so the listing and the invocation read the same catalogue.
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

/// A SigV4 `Authorization` header whose credential scope names the Bedrock signing name, which
/// is what routes the request. Request authentication is off, so it is never checked.
fn authorization() -> String {
    format!(
        "AWS4-HMAC-SHA256 Credential=AKIAIOSFODNN7EXAMPLE/20260101/{REGION}/bedrock/\
         aws4_request, SignedHeaders=host, Signature=0000000000000000000000000000000000000\
         000000000000000000000000"
    )
}

/// One HTTP exchange with either plane.
struct Call {
    status: u16,
    error_code: String,
    body: Bytes,
}

impl Call {
    /// The document the response carries, or `Value::Null` when it carries none.
    fn json(&self) -> Value {
        if self.body.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&self.body).unwrap_or(Value::Null)
        }
    }

    /// The refusal message the response carries, or the empty string when it carries none.
    fn message(&self) -> String {
        self.json()
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    }

    /// The whole response body, for an assertion that reports what came back.
    fn rendered(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    /// The sentence this answer refuses an unreadable body in, when it IS one of the two
    /// envelope sentences — i.e. when the body-parse stage is what refused the call.
    fn envelope_sentence(&self) -> Option<String> {
        let message = self.message();
        let from_the_body_stage = self.status == 400
            && self.error_code == "ValidationException"
            && (message == NATIVE_ENVELOPE_SENTENCE || message == OPENAI_ENVELOPE_SENTENCE);
        from_the_body_stage.then_some(message)
    }
}

/// One call to either plane.
async fn call(
    router: &MultiServiceRouter,
    method: &str,
    host: &str,
    path: &str,
    body: Vec<u8>,
) -> Call {
    let request = http::Request::builder()
        .method(method)
        .uri(format!("https://{host}{path}"))
        .header("host", host)
        .header("content-type", "application/json")
        .header("authorization", authorization())
        .body(Bytes::from(body))
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

/// `InvokeModel` on the data plane, with `body` declared as `application/json`.
async fn invoke(router: &MultiServiceRouter, model_id: &str, body: &[u8]) -> Call {
    // `:` and `/` are the characters these identifiers carry that cannot sit in a path segment
    // unescaped.
    let encoded = model_id.replace(':', "%3A").replace('/', "%2F");
    call(
        router,
        "POST",
        &format!("bedrock-runtime.{REGION}.amazonaws.com"),
        &format!("/model/{encoded}/invoke"),
        body.to_vec(),
    )
    .await
}

/// Every model the control plane publishes, as `(modelId, providerName)`.
async fn published_models(router: &MultiServiceRouter) -> Vec<(String, String)> {
    let listed = call(
        router,
        "GET",
        &format!("bedrock.{REGION}.amazonaws.com"),
        "/foundation-models",
        Vec::new(),
    )
    .await;
    assert_eq!(
        listed.status,
        200,
        "expected ListFoundationModels to answer the catalogue, got {} with {}",
        listed.status,
        listed.rendered()
    );
    let document = listed.json();
    let summaries = document["modelSummaries"]
        .as_array()
        .unwrap_or_else(|| panic!("expected modelSummaries in {document}"));
    summaries
        .iter()
        .map(|summary| {
            (
                summary["modelId"].as_str().unwrap_or_default().to_string(),
                summary["providerName"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string(),
            )
        })
        .collect()
}

/// The models the listing reports for one provider, at most [`MODELS_PER_PROVIDER`] of them.
fn models_of<'a>(published: &'a [(String, String)], provider: &str) -> Vec<&'a str> {
    published
        .iter()
        .filter(|(_, published_provider)| published_provider == provider)
        .map(|(model_id, _)| model_id.as_str())
        .take(MODELS_PER_PROVIDER)
        .collect()
}

/// RULE: the sentence an unreadable body is refused in is the one the invoked model's provider
/// states that class in, so every model of a provider is refused in that provider's own
/// sentence and the two envelopes' sentences differ — one constant for the catalogue cannot
/// answer both.
#[tokio::test]
async fn each_providers_envelope_decides_the_sentence_an_unreadable_body_is_refused_in() {
    let router = build_router();
    let published = published_models(&router).await;
    assert!(
        !published.is_empty(),
        "expected ListFoundationModels to publish a catalogue to invoke, got none"
    );

    // The providers whose sentence was read off a model, by the envelope each is served on.
    let mut native_providers: Vec<&str> = Vec::new();
    let mut openai_providers: Vec<&str> = Vec::new();

    for (provider, expected) in PROVIDERS {
        let mut read_from_a_model = false;
        for model_id in models_of(&published, provider) {
            let refusal = invoke(&router, model_id, UNREADABLE_BODY).await;
            // A model whose invocation never reaches the body — another modality, a model past
            // its end of life, a model offered only through an inference profile — is refused
            // in its own class, which says nothing about the wording under test.
            let Some(sentence) = refusal.envelope_sentence() else {
                continue;
            };
            assert_eq!(
                sentence,
                *expected,
                "case {provider:?} / {model_id:?}: a body that is no request object of this \
                 provider's envelope is refused in that envelope's sentence, expected \
                 {expected:?}, got {sentence:?} (HTTP {} {})",
                refusal.status,
                refusal.rendered()
            );
            // The refusal is the DOCUMENT's, so it carries nothing the request named.
            assert!(
                !sentence.contains(model_id),
                "case {provider:?} / {model_id:?}: the refusal must name nothing the request \
                 carried, got {sentence:?}"
            );
            read_from_a_model = true;
        }
        if !read_from_a_model {
            // This provider publishes no model whose body this plane reads right now, so it
            // states nothing about the wording: a catalogue that carries no such model is not a
            // defect of the refusal.
            continue;
        }
        if *expected == NATIVE_ENVELOPE_SENTENCE {
            native_providers.push(provider);
        } else {
            openai_providers.push(provider);
        }
    }

    // BOTH envelopes were exercised, on more than one provider each: that is what no single
    // hard-coded sentence can satisfy, and what makes the equality above a statement about the
    // envelope rather than about one model id.
    assert!(
        native_providers.len() >= 2 && openai_providers.len() >= 2,
        "expected the refusal to be read off at least two providers of each envelope, got the \
         native-envelope providers {native_providers:?} and the OpenAI-compatible ones \
         {openai_providers:?}"
    );
}

/// RULE: the refusal is the BODY's, so a model that answers a readable request of its own
/// envelope with a turn refuses an unreadable one in its provider's sentence — the same model,
/// the same declared media type, one readable body and one that is not.
#[tokio::test]
async fn a_model_that_serves_a_readable_body_refuses_an_unreadable_one_in_its_own_sentence() {
    let router = build_router();
    let published = published_models(&router).await;

    // (the provider, a readable request of its envelope, the sentence it refuses an unreadable
    // one in) — one provider per envelope.
    let rows = [
        ("Anthropic", CLAUDE_BODY, NATIVE_ENVELOPE_SENTENCE),
        ("Moonshot AI", OPENAI_BODY, OPENAI_ENVELOPE_SENTENCE),
    ];

    let mut rows_run = 0_usize;
    for (provider, readable, expected) in rows {
        for model_id in models_of(&published, provider) {
            // The model this row runs on is one this plane SERVES a readable request of that
            // envelope for; what any other model of the provider does with the readable body
            // is not what this row is about.
            let served = invoke(&router, model_id, readable.as_bytes()).await;
            if served.status != 200 {
                continue;
            }
            let case = format!("{provider} / {model_id}");
            assert!(
                served.json().is_object(),
                "case {case:?}: expected a JSON response body, got {:?}",
                served.rendered()
            );

            // The same model, the same declared media type, a body that is no request object of
            // the envelope: refused in that envelope's own sentence.
            let refused = invoke(&router, model_id, UNREADABLE_BODY).await;
            assert_eq!(
                refused.status,
                400,
                "case {case:?}: expected HTTP 400, got {} with {}",
                refused.status,
                refused.rendered()
            );
            assert_eq!(
                refused.error_code, "ValidationException",
                "case {case:?}: expected ValidationException, got {:?}",
                refused.error_code
            );
            assert_eq!(
                refused.message(),
                expected,
                "case {case:?}: expected message {expected:?}, got {:?}",
                refused.message()
            );
            // The refusal is the document's, so it names nothing the request carried.
            for echoed in ["messages", "role", "user", model_id] {
                assert!(
                    !refused.message().contains(echoed),
                    "case {case:?}: the refusal must name nothing the request carried, but it \
                     names {echoed:?}: {:?}",
                    refused.message()
                );
            }
            rows_run += 1;
            break;
        }
    }

    // At least one envelope was exercised end to end (served, then refused). Both rows run
    // against the catalogue as it stands; a provider the listing stops carrying takes its row
    // with it, which is a statement about the catalogue and not about the refusal.
    assert!(
        rows_run >= 1,
        "expected at least one served-then-refused model across the envelopes {:?}, ran none",
        rows.map(|(provider, _, _)| provider)
    );
}
