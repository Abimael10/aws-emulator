// SPDX-License-Identifier: CC0-1.0

//! `UpdatePrompt`, end to end over HTTP, read back through `GetPrompt`: an update
//! REPLACES the mutable members of the prompt's WORKING DRAFT, leaves the prompt's
//! identity and `createdAt` where they stood, reports the draft version, and never moves
//! a numbered version cut from that draft — whichever form the `PromptIdentifier` took,
//! a version-qualified ARN included.
//!
//! Every assertion here goes through the router, so it exercises the real wire path
//! (route matching, model constraint validation, serialization) rather than calling a
//! handler directly.

use std::collections::BTreeMap;
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

    /// The reported `tags` map, as a sorted map so a comparison is order-free.
    fn tags(&self, case: &str) -> BTreeMap<String, String> {
        self.member(&["tags"])
            .as_object()
            .unwrap_or_else(|| panic!("case {case}: expected a tags map, got {:?}", self.body))
            .iter()
            .map(|(key, value)| {
                (
                    key.clone(),
                    value
                        .as_str()
                        .unwrap_or_else(|| panic!("case {case}: a tag value is a string"))
                        .to_string(),
                )
            })
            .collect()
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

    /// `CreatePrompt`, which mints the prompt and the working draft an update moves.
    async fn create_prompt(&self, case: &str, request: Value) -> Answer {
        let created = self.call("POST", "/prompts/", Some(request)).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreatePrompt -> 201, got {created:?}"
        );
        created
    }

    /// `UpdatePrompt` on any `PromptIdentifier` form.
    async fn update_prompt(&self, identifier: &str, request: Value) -> Answer {
        self.call(
            "PUT",
            &format!("/prompts/{}/", label(identifier)),
            Some(request),
        )
        .await
    }

    async fn create_prompt_version(&self, identifier: &str, request: Value) -> Answer {
        self.call(
            "POST",
            &format!("/prompts/{}/versions", label(identifier)),
            Some(request),
        )
        .await
    }

    /// `GetPrompt` on any `PromptIdentifier` form, optionally naming a version.
    async fn get_prompt(&self, identifier: &str, prompt_version: Option<&str>) -> Answer {
        let query = prompt_version
            .map(|version| format!("?promptVersion={version}"))
            .unwrap_or_default();
        self.call("GET", &format!("/prompts/{}/{query}", label(identifier)), None)
            .await
    }

    async fn list_tags(&self, resource_arn: &str) -> Answer {
        self.call("GET", &format!("/tags/{}", label(resource_arn)), None)
            .await
    }
}

/// One variant of a prompt: the modelled `PromptVariant` requires `name`,
/// `templateType` (the modelled `PromptTemplateType`) and `templateConfiguration` (the
/// modelled `PromptTemplateConfiguration` union, whose `text` member carries a
/// `TextPromptTemplateConfiguration`).
fn text_variant(name: &str, text: &str) -> Value {
    json!({
        "name": name,
        "templateType": "TEXT",
        "templateConfiguration": { "text": { "text": text } },
    })
}

/// A value within the modelled `KmsKeyArn` pattern
/// (`^arn:aws(|-cn|-us-gov):kms:[a-zA-Z0-9-]*:[0-9]{12}:key/[a-zA-Z0-9-]{36}$`).
const KMS_KEY_ARN: &str =
    "arn:aws:kms:eu-west-1:123456789012:key/12345678-1234-1234-1234-123456789012";

/// The prompt an update is applied to: fully populated, so a member the update leaves out
/// is observably cleared rather than merely absent from the start.
fn populated_prompt() -> Value {
    json!({
        "name": "draft-update-prompt",
        "description": "the draft as it was first written",
        "defaultVariant": "summary",
        "variants": [text_variant("summary", "Summarise {{input}}")],
        "tags": { "estate": "prompts" },
    })
}

/// The updates applied to that prompt. Rows differ in WHICH mutable members the request
/// carries — none of the optional ones, each one in turn, and all of them together — so
/// the replace semantics are read over the whole set rather than one shape.
fn update_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "the required name alone",
            json!({ "name": "renamed-draft-prompt" }),
        ),
        (
            "a name and a description",
            json!({ "name": "described-draft", "description": "the draft moved on" }),
        ),
        (
            "a name and a variant naming no default",
            json!({
                "name": "undefaulted-draft",
                "variants": [text_variant("translate", "Translate {{input}}")],
            }),
        ),
        (
            "a name, a default variant and the variant it names",
            json!({
                "name": "defaulted-draft",
                "defaultVariant": "rewrite",
                "variants": [text_variant("rewrite", "Rewrite {{input}}")],
            }),
        ),
        (
            "a name and a customer encryption key",
            json!({ "name": "encrypted-draft", "customerEncryptionKeyArn": KMS_KEY_ARN }),
        ),
        (
            "every mutable member at once",
            json!({
                "name": "fully-replaced-draft",
                "description": "every member replaced",
                "customerEncryptionKeyArn": KMS_KEY_ARN,
                "defaultVariant": "expand",
                "variants": [text_variant("expand", "Expand {{input}}")],
            }),
        ),
    ]
}

/// The mutable members an `UpdatePrompt` request may carry, which are exactly the members
/// the update replaces on the draft.
const REPLACED_MEMBERS: [&str; 5] = [
    "name",
    "description",
    "customerEncryptionKeyArn",
    "defaultVariant",
    "variants",
];

/// An update replaces the draft's mutable members with the request's — a member the
/// request leaves out is left out of the prompt too — keeps the prompt's identity and
/// `createdAt`, reports the DRAFT version, and the prompt reads that draft straight back.
#[tokio::test]
async fn update_prompt_replaces_the_drafts_mutable_members_and_the_prompt_reads_them_back() {
    for (case, update) in update_rows() {
        let emulator = Emulator::new();
        let created = emulator.create_prompt(case, populated_prompt()).await;
        let prompt_id = created.string(&["id"]);
        let prompt_arn = created.string(&["arn"]);

        let updated = emulator.update_prompt(&prompt_id, update.clone()).await;
        assert_eq!(
            updated.status, 200,
            "case {case}: expected UpdatePrompt -> 200, got {updated:?}"
        );

        // Every member the request may carry reads back as the request left it: the ones
        // it sent, and absent for the ones it did not.
        for member in REPLACED_MEMBERS {
            assert_eq!(
                updated.member(&[member]),
                update.get(member).unwrap_or(&Value::Null),
                "case {case}: {member} reads back as the update left it, got {updated:?}"
            );
        }

        // The identity of the prompt is not the request's to set, and neither is the
        // instant it came into being.
        assert_eq!(
            updated.string(&["id"]),
            prompt_id,
            "case {case}: an update leaves the prompt's id where it stood, got {updated:?}"
        );
        assert_eq!(
            updated.string(&["arn"]),
            prompt_arn,
            "case {case}: an update leaves the prompt's ARN where it stood, got {updated:?}"
        );
        assert_eq!(
            updated.member(&["createdAt"]),
            created.member(&["createdAt"]),
            "case {case}: an update leaves createdAt where it stood, got {updated:?}"
        );
        assert_eq!(
            updated.string(&["version"]),
            "DRAFT",
            "case {case}: the version an update moves is the working draft, got {updated:?}"
        );
        let stamp = updated.string(&["updatedAt"]);
        assert!(
            stamp.contains('T') && stamp.ends_with('Z'),
            "case {case}: expected updatedAt in the modelled date-time wire form, \
             got {stamp:?}"
        );

        // The round trip: the draft is read back off the stored record, through either
        // form that addresses the prompt.
        for (form, identifier) in [
            ("a bare promptId", prompt_id.clone()),
            ("the prompt's ARN", prompt_arn.clone()),
        ] {
            let read = emulator.get_prompt(&identifier, None).await;
            assert_eq!(
                read.status, 200,
                "case {case} ({form}): expected GetPrompt -> 200, got {read:?}"
            );
            assert_eq!(
                read.body, updated.body,
                "case {case} ({form}): the draft reads back as the update left it, \
                 got {read:?}"
            );
        }

        // The tags a prompt holds are the tagging surface's, not this request's.
        assert_eq!(
            emulator.list_tags(&prompt_arn).await.tags(case),
            BTreeMap::from([("estate".to_string(), "prompts".to_string())]),
            "case {case}: an update leaves the prompt's tags where they stood"
        );
    }
}

/// Whichever form the identifier takes — a bare id, the prompt's ARN, or an ARN qualified
/// with a version — the update lands on the WORKING DRAFT: the response reports the
/// prompt's own unqualified ARN at version DRAFT, the numbered version stands exactly as
/// it was cut, and the next cut snapshots the updated draft under the next number.
#[tokio::test]
async fn update_prompt_moves_the_draft_whichever_identifier_form_names_the_prompt() {
    let emulator = Emulator::new();
    let case = "a prompt with a published version";
    let created = emulator.create_prompt(case, populated_prompt()).await;
    let prompt_id = created.string(&["id"]);
    let prompt_arn = created.string(&["arn"]);

    let cut = emulator.create_prompt_version(&prompt_id, json!({})).await;
    assert_eq!(
        cut.status, 201,
        "case {case}: expected CreatePromptVersion -> 201, got {cut:?}"
    );
    assert_eq!(
        cut.string(&["version"]),
        "1",
        "case {case}: versions are numbered incrementally from 1, got {cut:?}"
    );

    // Rows: every form the modelled `PromptIdentifier` admits for this prompt, each
    // applying a differently shaped update.
    for (form, identifier, update) in [
        (
            "a bare promptId",
            prompt_id.clone(),
            json!({ "name": "updated-by-id", "description": "moved by id" }),
        ),
        (
            "the prompt's ARN",
            prompt_arn.clone(),
            json!({ "name": "updated-by-arn" }),
        ),
        (
            "a version-qualified ARN",
            format!("{prompt_arn}:1"),
            json!({
                "name": "updated-by-qualified-arn",
                "defaultVariant": "summary",
                "variants": [text_variant("summary", "Summarise {{input}} briefly")],
            }),
        ),
    ] {
        let updated = emulator.update_prompt(&identifier, update.clone()).await;
        assert_eq!(
            updated.status, 200,
            "case {form}: expected UpdatePrompt -> 200, got {updated:?}"
        );
        assert_eq!(
            updated.string(&["arn"]),
            prompt_arn,
            "case {form}: the update reports the prompt's own unqualified ARN, \
             got {updated:?}"
        );
        assert_eq!(
            updated.string(&["version"]),
            "DRAFT",
            "case {form}: the version an update moves is the working draft, got {updated:?}"
        );
        assert_eq!(
            emulator.get_prompt(&prompt_id, None).await.body,
            updated.body,
            "case {form}: the draft reads back as the update left it"
        );

        // The published version is a static snapshot: no update moves it.
        let read_version = emulator.get_prompt(&prompt_id, Some("1")).await;
        assert_eq!(
            read_version.body, cut.body,
            "case {form}: an update moves the draft alone, got {read_version:?}"
        );
    }

    // The next cut is taken from the draft as the last update left it, under the next
    // number: nothing about the update consumed or re-used a version number.
    let second = emulator.create_prompt_version(&prompt_id, json!({})).await;
    assert_eq!(
        second.string(&["version"]),
        "2",
        "case {case}: the next cut takes the next number, got {second:?}"
    );
    assert_eq!(
        second.string(&["name"]),
        "updated-by-qualified-arn",
        "case {case}: the next cut snapshots the updated draft, got {second:?}"
    );
}

/// An update the service cannot serve is refused, and a refused update moves nothing: a
/// prompt the store does not hold, an identifier of neither modelled form, and a
/// `defaultVariant` naming none of the variants the request carries.
#[tokio::test]
async fn update_prompt_refuses_a_request_it_cannot_serve_and_leaves_the_draft_standing() {
    let emulator = Emulator::new();
    let case = "refused updates";
    let created = emulator.create_prompt(case, populated_prompt()).await;
    let prompt_id = created.string(&["id"]);
    let prompt_arn = created.string(&["arn"]);
    let served = json!({ "name": "an-update-that-is-refused" });

    // Rows: both modelled `PromptIdentifier` forms, over a well-formed id the store does
    // not hold.
    let unknown = "0123456789";
    for (form, identifier) in [
        ("bare promptId", unknown.to_string()),
        ("prompt ARN", prompt_arn.replace(prompt_id.as_str(), unknown)),
    ] {
        let refused = emulator.update_prompt(&identifier, served.clone()).await;
        assert_eq!(
            refused.status, 404,
            "case {case} ({form}): expected UpdatePrompt -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case} ({form}): expected ResourceNotFoundException, got {refused:?}"
        );
        let message = refused.string(&["message"]);
        assert!(
            message.contains("UpdatePrompt") && message.contains("Prompt"),
            "case {case} ({form}): the refusal names the operation and the resource type, \
             got {message:?}"
        );
    }

    // Rows differ in HOW the identifier misses the modelled `PromptIdentifier` forms: a
    // prompt name, ids of the wrong length or alphabet, and ARNs wrong in the service,
    // the resource type and the partition.
    for (form, identifier) in [
        ("a prompt name, not an identifier", "draft-update-prompt".to_string()),
        ("nine characters", "012345678".to_string()),
        ("eleven characters", "0123456789a".to_string()),
        ("a character outside the id alphabet", "0123_56789".to_string()),
        ("an ARN naming another service", prompt_arn.replace(":bedrock:", ":s3:")),
        ("an ARN naming another resource type", prompt_arn.replace(":prompt/", ":flow/")),
        ("an ARN in another partition", prompt_arn.replace("arn:aws:", "arn:aws-cn:")),
    ] {
        let refused = emulator.update_prompt(&identifier, served.clone()).await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({form}, {identifier}): expected UpdatePrompt -> 400, \
             got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case} ({form}, {identifier}): expected ValidationException, \
             got {refused:?}"
        );
    }

    // Rows differ in HOW `defaultVariant` fails to name one of the request's variants:
    // no variants at all, an empty list, and a variant under another name.
    for (form, update) in [
        (
            "a default variant and no variants",
            json!({ "name": "unmatched-default", "defaultVariant": "summary" }),
        ),
        (
            "a default variant and an empty variant list",
            json!({ "name": "unmatched-default", "defaultVariant": "summary", "variants": [] }),
        ),
        (
            "a default variant naming another variant",
            json!({
                "name": "unmatched-default",
                "defaultVariant": "summary",
                "variants": [text_variant("translate", "Translate {{input}}")],
            }),
        ),
    ] {
        let refused = emulator.update_prompt(&prompt_id, update).await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({form}): expected UpdatePrompt -> 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case} ({form}): expected ValidationException, got {refused:?}"
        );
        let message = refused.string(&["message"]);
        assert!(
            message.contains("defaultVariant"),
            "case {case} ({form}): the refusal names the member that failed, \
             got {message:?}"
        );
    }

    // Every refusal above wrote nothing: the draft still reads exactly as it was created.
    let read = emulator.get_prompt(&prompt_id, None).await;
    assert_eq!(
        read.body, created.body,
        "case {case}: a refused update leaves the draft as it stood, got {read:?}"
    );
}
