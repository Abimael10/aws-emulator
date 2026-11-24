// SPDX-License-Identifier: CC0-1.0

//! The `defaultVariant` membership pre-condition of the prompt writes, end to end over
//! HTTP: a write whose `defaultVariant` names none of the `variants` the request carries
//! is refused `ValidationException` (HTTP 400) with the service's FIXED cross-member
//! sentence — one sentence that interpolates nothing from the request, so it is byte-for-
//! byte the same whichever value, member or variant names the request carried — and the
//! refusal writes nothing; a write whose `defaultVariant` names one of the variants it
//! carries is served and reads back with that default variant.
//!
//! Every assertion goes through the router, so it exercises the real wire path (route
//! matching, model constraint validation, serialization) rather than calling a handler
//! directly.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService;
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the routing key
/// this service claims on, and the source of the region its ARNs carry.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// The whole message a prompt write answers when its `defaultVariant` names none of the
/// variants the request carries. Shared with the other prompt suites so one spelling of
/// the sentence is asserted everywhere.
pub const DEFAULT_VARIANT_NOT_IN_VARIANTS: &str =
    "Default variant must be present in the variants list. \
     Retry your request with the correct default variant.";

/// One value as a percent-encoded URI component, as a client that sends a non-greedy
/// `@httpLabel` encodes it.
fn label(value: &str) -> String {
    value.replace('%', "%25").replace(':', "%3A").replace('/', "%2F")
}

/// One HTTP answer from the emulator, parsed.
#[derive(Debug)]
struct Answer {
    status: u16,
    /// The restJson1 error shape name, from `x-amzn-errortype` or the body's `__type`;
    /// `None` on a success.
    error_type: Option<String>,
    body: Value,
}

impl Answer {
    fn member(&self, path: &[&str]) -> &Value {
        let mut at = &self.body;
        for key in path {
            at = &at[*key];
        }
        at
    }

    fn string(&self, path: &[&str]) -> String {
        self.member(path)
            .as_str()
            .unwrap_or_else(|| panic!("{path:?} is not a string in {:?}", self.body))
            .to_string()
    }
}

/// The emulator under test: one router over one shared engine, exactly as the production
/// router mounts this service.
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

    async fn call(&self, method: &str, uri: &str, body: Option<Value>) -> Answer {
        let bytes = body
            .map(|value| Bytes::from(serde_json::to_vec(&value).expect("serialize request body")))
            .unwrap_or_default();
        let request = http::Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "bedrock-agent.eu-west-1.amazonaws.com")
            .header("authorization", AUTHORIZATION)
            .header("content-type", "application/json")
            .body(bytes)
            .expect("build the request");

        let response = self.router.dispatch(request).await;
        let status = response.status().as_u16();
        let header_error = response
            .headers()
            .get("x-amzn-errortype")
            .and_then(|value| value.to_str().ok())
            .map(ToString::to_string);
        let raw = response.into_body();
        let body: Value = if raw.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&raw).unwrap_or_else(|error| {
                panic!(
                    "{method} {uri} -> {status} body is not JSON ({error}): {}",
                    String::from_utf8_lossy(&raw)
                )
            })
        };
        let error_type = header_error.or_else(|| {
            body.get("__type")
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
        Answer {
            status,
            error_type,
            body,
        }
    }

    async fn create_prompt(&self, request: Value) -> Answer {
        self.call("POST", "/prompts/", Some(request)).await
    }

    async fn update_prompt(&self, identifier: &str, request: Value) -> Answer {
        self.call(
            "PUT",
            &format!("/prompts/{}/", label(identifier)),
            Some(request),
        )
        .await
    }

    async fn get_prompt(&self, identifier: &str) -> Answer {
        self.call("GET", &format!("/prompts/{}/", label(identifier)), None)
            .await
    }

    /// Every prompt name the account holds, from `ListPrompts`' `promptSummaries`.
    async fn listed_prompt_names(&self, case: &str) -> Vec<String> {
        let listed = self.call("GET", "/prompts/", None).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListPrompts -> 200, got {listed:?}"
        );
        listed
            .member(&["promptSummaries"])
            .as_array()
            .unwrap_or_else(|| {
                panic!(
                    "case {case}: expected a promptSummaries list, got {:?}",
                    listed.body
                )
            })
            .iter()
            .map(|summary| {
                summary["name"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case}: a summary carries a name"))
                    .to_string()
            })
            .collect()
    }
}

/// One variant of a prompt: the modelled `PromptVariant` requires `name`,
/// `templateType` (the modelled `PromptTemplateType`) and `templateConfiguration` (the
/// modelled `PromptTemplateConfiguration` union, whose `text` member carries a
/// `TextPromptTemplateConfiguration`).
fn text_variant(name: &str) -> Value {
    json!({
        "name": name,
        "templateType": "TEXT",
        "templateConfiguration": { "text": { "text": format!("hello {{{{{name}}}}}") } },
    })
}

/// Rows: HOW a `defaultVariant` fails to name one of the variants the request carries.
/// They differ in the shape of the `variants` member (absent, empty, one variant under
/// another name) and in HOW the name misses (a different word, a case difference, a
/// separator difference, a prefix of the variant's name) — all within the modelled
/// `PromptVariantName` pattern `^([0-9a-zA-Z][_-]?){1,100}$` and the modelled
/// `PromptVariantList` `@length` max of 1
/// (`smithy_query(query="operation",operation="CreatePrompt")`).
fn unmatched_rows() -> Vec<(&'static str, Value, Option<Value>)> {
    vec![
        (
            "no variants member at all",
            json!("summary"),
            None,
        ),
        (
            "an empty variants list",
            json!("summary"),
            Some(json!([])),
        ),
        (
            "a variant under another name",
            json!("a_name_not_in_variants"),
            Some(json!([text_variant("actual_variant")])),
        ),
        (
            "a name differing only in case",
            json!("Summary"),
            Some(json!([text_variant("summary")])),
        ),
        (
            "a name differing only in its separator",
            json!("summary-v2"),
            Some(json!([text_variant("summary_v2")])),
        ),
        (
            "a name that is a prefix of the variant's",
            json!("summary"),
            Some(json!([text_variant("summaryLong")])),
        ),
        (
            "a name the variant's is a prefix of",
            json!("summaryLong"),
            Some(json!([text_variant("summary")])),
        ),
    ]
}

/// The request body of a prompt write carrying `default_variant` and `variants`.
fn write_request(name: &str, default_variant: &Value, variants: Option<&Value>) -> Value {
    let mut request = json!({ "name": name, "defaultVariant": default_variant });
    if let Some(variants) = variants {
        request["variants"] = variants.clone();
    }
    request
}

/// `CreatePrompt` refuses a `defaultVariant` naming none of the request's variants with
/// the fixed cross-member sentence, and the refused prompt is not created.
#[tokio::test]
async fn create_prompt_refuses_an_unmatched_default_variant_with_the_cross_member_sentence() {
    let emulator = Emulator::new();

    for (case, default_variant, variants) in unmatched_rows() {
        let name = "refused-default-variant-prompt";
        let refused = emulator
            .create_prompt(write_request(name, &default_variant, variants.as_ref()))
            .await;

        assert_eq!(
            refused.status, 400,
            "case {case}: expected CreatePrompt -> 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case}: expected ValidationException, got {refused:?}"
        );
        let message = refused.string(&["message"]);
        assert_eq!(
            message, DEFAULT_VARIANT_NOT_IN_VARIANTS,
            "case {case}: expected the fixed cross-member sentence, got {message:?}"
        );

        // The sentence is fixed: nothing from the request reaches it — not the offending
        // value, not the member name, not the names of the variants supplied.
        let value = default_variant.as_str().unwrap_or_default();
        for (slot, fragment) in [
            ("the offending defaultVariant value", value.to_string()),
            ("the input member name", "defaultVariant".to_string()),
            ("the prompt's name", name.to_string()),
        ] {
            assert!(
                !message.contains(&fragment),
                "case {case}: the refusal interpolates no {slot} ({fragment:?}), \
                 got {message:?}"
            );
        }

        // Nothing was written: the refused prompt is in no listing, so a repeat of the
        // row starts from the same store the previous one did.
        let listed = emulator.listed_prompt_names(case).await;
        assert!(
            listed.is_empty(),
            "case {case}: a refused create leaves no prompt behind, got {listed:?}"
        );
    }
}

/// `UpdatePrompt` applies the same cross-member rule, with the same fixed sentence, and a
/// refused update leaves the stored draft exactly as it stood.
#[tokio::test]
async fn update_prompt_refuses_an_unmatched_default_variant_with_the_same_sentence() {
    let emulator = Emulator::new();
    let created = emulator
        .create_prompt(json!({
            "name": "standing-draft-prompt",
            "defaultVariant": "summary",
            "variants": [text_variant("summary")],
        }))
        .await;
    assert_eq!(
        created.status, 201,
        "expected CreatePrompt -> 201 for the standing draft, got {created:?}"
    );
    let prompt_id = created.string(&["id"]);

    for (case, default_variant, variants) in unmatched_rows() {
        let refused = emulator
            .update_prompt(
                &prompt_id,
                write_request("updated-draft-prompt", &default_variant, variants.as_ref()),
            )
            .await;

        assert_eq!(
            refused.status, 400,
            "case {case}: expected UpdatePrompt -> 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case}: expected ValidationException, got {refused:?}"
        );
        let message = refused.string(&["message"]);
        assert_eq!(
            message, DEFAULT_VARIANT_NOT_IN_VARIANTS,
            "case {case}: expected the fixed cross-member sentence, got {message:?}"
        );

        let read = emulator.get_prompt(&prompt_id).await;
        assert_eq!(
            read.body, created.body,
            "case {case}: a refused update leaves the draft as it stood, got {read:?}"
        );
    }
}

/// The matching side of the same rule: a `defaultVariant` that DOES name one of the
/// variants the request carries, and a request that names no default variant at all, are
/// served — and the prompt reads back with whatever the request left.
///
/// Rows differ in the name's alphabet and separators, each within the modelled
/// `PromptVariantName` pattern `^([0-9a-zA-Z][_-]?){1,100}$`.
#[tokio::test]
async fn a_default_variant_naming_a_supplied_variant_is_served_and_reads_back() {
    for (case, default_variant, variants) in [
        (
            "the only variant, by name",
            Some("actual_variant"),
            Some(json!([text_variant("actual_variant")])),
        ),
        (
            "a variant named with a hyphen",
            Some("summary-v2"),
            Some(json!([text_variant("summary-v2")])),
        ),
        (
            "a variant named in digits",
            Some("0123456789"),
            Some(json!([text_variant("0123456789")])),
        ),
        (
            "a variant named in one character",
            Some("a"),
            Some(json!([text_variant("a")])),
        ),
        (
            "no default variant, with a variant supplied",
            None,
            Some(json!([text_variant("unreferenced")])),
        ),
        ("no default variant and no variants", None, None),
    ] {
        let emulator = Emulator::new();
        let name = "served-default-variant-prompt";
        let mut request = json!({ "name": name });
        if let Some(default_variant) = default_variant {
            request["defaultVariant"] = json!(default_variant);
        }
        if let Some(variants) = variants.as_ref() {
            request["variants"] = variants.clone();
        }

        let created = emulator.create_prompt(request.clone()).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreatePrompt -> 201, got {created:?}"
        );
        assert_eq!(
            created.member(&["defaultVariant"]),
            request.get("defaultVariant").unwrap_or(&Value::Null),
            "case {case}: the create reports the default variant the request sent, \
             got {created:?}"
        );

        // The round trip: the prompt is read back off the stored record.
        let read = emulator.get_prompt(&created.string(&["id"])).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected GetPrompt -> 200, got {read:?}"
        );
        assert_eq!(
            read.member(&["defaultVariant"]),
            request.get("defaultVariant").unwrap_or(&Value::Null),
            "case {case}: the prompt reads back with the default variant it was created \
             with, got {read:?}"
        );
        assert_eq!(
            read.member(&["variants"]),
            request.get("variants").unwrap_or(&Value::Null),
            "case {case}: the prompt reads back with the variants it was created with, \
             got {read:?}"
        );

        let listed = emulator.listed_prompt_names(case).await;
        assert_eq!(
            listed,
            vec![name.to_string()],
            "case {case}: the accepted prompt is the one the account holds"
        );
    }
}
