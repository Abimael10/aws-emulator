// SPDX-License-Identifier: CC0-1.0

//! `CreatePromptVersion`, end to end over HTTP, read back through `GetPrompt`: a cut
//! takes a STATIC SNAPSHOT of the prompt's working draft, numbers it incrementally from
//! 1, addresses it by the prompt's ARN qualified with that number, and never moves it
//! again however the draft changes.
//!
//! Every assertion here goes through the router, so it exercises the real wire path
//! (route matching, model constraint validation, serialization) rather than calling a
//! handler directly.
//!
//! The one thing these tests reach into the store for is MOVING THE DRAFT: the operation
//! that edits a prompt's draft over the wire is not implemented in this emulator yet, so
//! the draft is moved in the store and the version is then read back over HTTP to show
//! the snapshot standing still.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use bytes::Bytes;
use serde_json::{json, Value};
use service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::state::service_state;
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
    engine: Arc<engine::InMemoryEngine>,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(EmulatedService::new(Arc::clone(&engine)));
        Self { router, engine }
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

    /// `CreatePrompt`, which mints the prompt and the working draft a version is cut
    /// from.
    async fn create_prompt(&self, case: &str, request: Value) -> Answer {
        let created = self.call("POST", "/prompts/", Some(request)).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreatePrompt -> 201, got {created:?}"
        );
        created
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

    async fn tag(&self, resource_arn: &str, tags: &BTreeMap<String, String>) -> Answer {
        self.call(
            "POST",
            &format!("/tags/{}", label(resource_arn)),
            Some(json!({ "tags": tags })),
        )
        .await
    }

    /// Move the stored working draft: the name, the description and the variants it
    /// carries, and the instant it was last updated. The operation that does this over
    /// the wire is not implemented yet, so the draft is moved here and every assertion
    /// about what a version reports still goes over HTTP.
    fn move_draft(&self, case: &str, prompt_id: &str, name: &str, description: &str) {
        let state = service_state(&*self.engine).expect("the bedrock-agent store");
        let mut state = state.lock();
        let draft = state
            .prompts
            .get_mut(prompt_id)
            .unwrap_or_else(|| panic!("case {case}: expected the stored prompt"));
        draft.name = name.to_string();
        draft.description = Some(description.to_string());
        draft.variants = None;
        draft.default_variant = None;
        draft.updated_at = SystemTime::now() + Duration::from_secs(1);
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

/// Tag keys and values within the modelled `TagKey`/`TagValue` patterns.
fn tag_map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(key, value)| ((*key).to_string(), (*value).to_string()))
        .collect()
}

/// The draft shapes a cut is taken from. The rows differ in what the snapshot has to
/// carry: a described prompt with a default variant, one with neither a description nor
/// variants, and one carrying a variant but naming no default.
fn draft_rows() -> Vec<(&'static str, Value)> {
    vec![
        (
            "a described prompt with a default variant",
            json!({
                "name": "version-minting-prompt",
                "description": "the prompt being versioned",
                "defaultVariant": "summary",
                "variants": [text_variant("summary", "Summarise {{input}}")],
            }),
        ),
        (
            "a prompt carrying neither a description nor variants",
            json!({ "name": "bare-version-prompt" }),
        ),
        (
            "a prompt carrying a variant but naming no default",
            json!({
                "name": "undefaulted-version-prompt",
                "variants": [text_variant("translate", "Translate {{input}}")],
            }),
        ),
    ]
}

/// A cut takes a snapshot of the draft, numbers it incrementally from 1, and the prompt
/// reads that snapshot back at that version — through the `promptVersion` query and
/// through the version-qualified ARN alike — while the draft keeps reporting `DRAFT`.
///
/// Rows differ in the draft's shape and in the COUNT of versions cut from it.
#[tokio::test]
async fn create_prompt_version_cuts_incrementally_numbered_snapshots_the_prompt_reads_back() {
    for (draft_case, request) in draft_rows() {
        for cuts in [0_u32, 1, 2, 3, 5] {
            let case = format!("{draft_case}, {cuts} version(s) cut");
            let emulator = Emulator::new();
            let draft = emulator.create_prompt(&case, request.clone()).await;
            let prompt_id = draft.string(&["id"]);
            let prompt_arn = draft.string(&["arn"]);

            for expected in 1..=cuts {
                let cut = emulator
                    .create_prompt_version(&prompt_id, json!({}))
                    .await;
                assert_eq!(
                    cut.status, 201,
                    "case {case}: expected CreatePromptVersion -> 201, got {cut:?}"
                );
                assert_eq!(
                    cut.string(&["version"]),
                    expected.to_string(),
                    "case {case}: versions are numbered incrementally from 1, got {cut:?}"
                );
                assert_eq!(
                    cut.string(&["id"]),
                    prompt_id,
                    "case {case}: the version names the prompt it was cut from, got {cut:?}"
                );
                assert_eq!(
                    cut.string(&["arn"]),
                    format!("{prompt_arn}:{expected}"),
                    "case {case}: the version's ARN qualifies the prompt's with its \
                     number, got {cut:?}"
                );
                assert_eq!(
                    cut.string(&["name"]),
                    draft.string(&["name"]),
                    "case {case}: the version snapshots the draft's name, got {cut:?}"
                );
                assert_eq!(
                    cut.member(&["description"]),
                    draft.member(&["description"]),
                    "case {case}: a cut that gives no description leaves the prompt's \
                     own, got {cut:?}"
                );
                assert_eq!(
                    cut.member(&["defaultVariant"]),
                    draft.member(&["defaultVariant"]),
                    "case {case}: the version snapshots the draft's default variant, \
                     got {cut:?}"
                );
                assert_eq!(
                    cut.member(&["variants"]),
                    draft.member(&["variants"]),
                    "case {case}: the version snapshots the draft's variants, got {cut:?}"
                );
                for member in ["createdAt", "updatedAt"] {
                    let stamp = cut.string(&[member]);
                    assert!(
                        stamp.contains('T') && stamp.ends_with('Z'),
                        "case {case}: expected {member} in the modelled date-time wire \
                         form, got {stamp:?}"
                    );
                }

                // The round trip: the version is read back off the stored snapshot,
                // through either form that addresses it.
                for (form, identifier, version) in [
                    ("the promptVersion query", prompt_id.clone(), Some(expected.to_string())),
                    (
                        "a version-qualified ARN",
                        format!("{prompt_arn}:{expected}"),
                        None,
                    ),
                ] {
                    let read = emulator
                        .get_prompt(&identifier, version.as_deref())
                        .await;
                    assert_eq!(
                        read.status, 200,
                        "case {case} ({form}): expected GetPrompt -> 200, got {read:?}"
                    );
                    assert_eq!(
                        read.body, cut.body,
                        "case {case} ({form}): the version reads back as it was cut, \
                         got {read:?}"
                    );
                }
            }

            // The draft is a version of its own and keeps reporting itself.
            let read_draft = emulator.get_prompt(&prompt_id, None).await;
            assert_eq!(
                read_draft.body, draft.body,
                "case {case}: cutting a version leaves the working draft as it stood, \
                 got {read_draft:?}"
            );
            assert_eq!(
                read_draft.string(&["version"]),
                "DRAFT",
                "case {case}: the prompt's own read reports its working draft, \
                 got {read_draft:?}"
            );

            // One past the last cut was never cut, so it names no resource.
            let missing = emulator
                .get_prompt(&prompt_id, Some(&(cuts + 1).to_string()))
                .await;
            assert_eq!(
                missing.status, 404,
                "case {case}: a version that was never cut names no resource, \
                 got {missing:?}"
            );
            assert_eq!(
                missing.error_type.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case}: expected the modelled ResourceNotFoundException, \
                 got {missing:?}"
            );
        }
    }
}

/// A numbered version is STATIC: once cut, nothing about it moves when the draft it was
/// cut from does, and the next cut snapshots the draft as it then stands.
#[tokio::test]
async fn a_cut_version_stands_still_while_the_draft_it_was_cut_from_moves() {
    let case = "the draft moves while its versions stand";
    let emulator = Emulator::new();
    let draft = emulator
        .create_prompt(
            case,
            json!({
                "name": "snapshot-prompt",
                "description": "the draft as it was first written",
                "defaultVariant": "summary",
                "variants": [text_variant("summary", "Summarise {{input}}")],
            }),
        )
        .await;
    let prompt_id = draft.string(&["id"]);

    let first = emulator
        .create_prompt_version(&prompt_id, json!({ "description": "the first cut" }))
        .await;
    assert_eq!(
        first.status, 201,
        "case {case}: expected CreatePromptVersion -> 201, got {first:?}"
    );
    assert_eq!(
        first.string(&["description"]),
        "the first cut",
        "case {case}: the version carries the description the request gave, got {first:?}"
    );
    assert_eq!(
        first.member(&["variants"]),
        draft.member(&["variants"]),
        "case {case}: the version snapshots the draft's variants, got {first:?}"
    );

    // The draft moves: a new name, a new description, and no variants at all.
    emulator.move_draft(case, &prompt_id, "renamed-snapshot-prompt", "the draft moved on");

    let read_first = emulator.get_prompt(&prompt_id, Some("1")).await;
    assert_eq!(
        read_first.body, first.body,
        "case {case}: a change to the draft moves the draft alone, got {read_first:?}"
    );

    let read_draft = emulator.get_prompt(&prompt_id, None).await;
    assert_eq!(
        read_draft.string(&["name"]),
        "renamed-snapshot-prompt",
        "case {case}: the draft reports the change, got {read_draft:?}"
    );
    assert_eq!(
        read_draft.member(&["variants"]),
        &Value::Null,
        "case {case}: the moved draft carries no variants, got {read_draft:?}"
    );

    // The next cut is taken from the draft as it now stands, under the next number.
    let second = emulator.create_prompt_version(&prompt_id, json!({})).await;
    assert_eq!(
        second.string(&["version"]),
        "2",
        "case {case}: the next cut takes the next number, got {second:?}"
    );
    assert_eq!(
        second.string(&["name"]),
        "renamed-snapshot-prompt",
        "case {case}: the next cut snapshots the moved draft, got {second:?}"
    );
    assert_eq!(
        second.member(&["variants"]),
        &Value::Null,
        "case {case}: the next cut snapshots the moved draft's variants, got {second:?}"
    );
    assert_eq!(
        emulator.get_prompt(&prompt_id, Some("1")).await.body,
        first.body,
        "case {case}: cutting another version leaves the first where it was"
    );
}

/// The `@idempotencyToken` makes a retried cut report the version it already cut; a
/// version-qualified identifier cuts from the working draft all the same; and a prompt
/// the store does not hold, or an identifier of neither modelled form, is refused rather
/// than cut.
#[tokio::test]
async fn create_prompt_version_replays_a_retried_cut_and_refuses_an_unknown_prompt() {
    let case = "retried and refused cuts";
    let emulator = Emulator::new();
    let draft = emulator
        .create_prompt(
            case,
            json!({
                "name": "idempotent-version-prompt",
                "defaultVariant": "summary",
                "variants": [text_variant("summary", "Summarise {{input}}")],
            }),
        )
        .await;
    let prompt_id = draft.string(&["id"]);
    let prompt_arn = draft.string(&["arn"]);
    // The modelled `ClientToken` is `length 33..256, pattern
    // ^[a-zA-Z0-9](-*[a-zA-Z0-9]){0,256}$`.
    let token = "idempotency-token-for-prompt-version-1";
    let other_token = "idempotency-token-for-prompt-version-2";

    let first = emulator
        .create_prompt_version(
            &prompt_id,
            json!({ "clientToken": token, "description": "the first cut version" }),
        )
        .await;
    assert_eq!(
        first.status, 201,
        "case {case}: expected CreatePromptVersion -> 201, got {first:?}"
    );
    let retried = emulator
        .create_prompt_version(
            &prompt_id,
            json!({ "clientToken": token, "description": "the first cut version" }),
        )
        .await;
    assert_eq!(
        retried.body, first.body,
        "case {case}: a retry of the same token reports the version it already cut, \
         got {retried:?}"
    );

    let second = emulator
        .create_prompt_version(&prompt_id, json!({ "clientToken": other_token }))
        .await;
    assert_eq!(
        second.string(&["version"]),
        "2",
        "case {case}: a different token cuts the next version, got {second:?}"
    );

    // A version is always cut from the WORKING DRAFT, so an identifier qualified with a
    // version cuts from the draft of the prompt it names and takes the next number.
    let from_qualified = emulator
        .create_prompt_version(&format!("{prompt_arn}:1"), json!({}))
        .await;
    assert_eq!(
        from_qualified.string(&["version"]),
        "3",
        "case {case}: a version-qualified identifier cuts the prompt's next version, \
         got {from_qualified:?}"
    );

    // Rows: both modelled `PromptIdentifier` forms, over a well-formed id the store does
    // not hold.
    let unknown = "0123456789";
    for (form, identifier) in [
        ("bare promptId", unknown.to_string()),
        ("prompt ARN", prompt_arn.replace(prompt_id.as_str(), unknown)),
    ] {
        let refused = emulator.create_prompt_version(&identifier, json!({})).await;
        assert_eq!(
            refused.status, 404,
            "case {case} ({form}): expected CreatePromptVersion -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case} ({form}): expected ResourceNotFoundException, got {refused:?}"
        );
        let message = refused.string(&["message"]);
        assert!(
            message.contains("CreatePromptVersion") && message.contains("Prompt"),
            "case {case} ({form}): the refusal names the operation and the resource type, \
             got {message:?}"
        );
    }

    // Rows differ in HOW the identifier misses the modelled `PromptIdentifier` forms: a
    // prompt name, ids of the wrong length or alphabet, and ARNs wrong in the service,
    // the resource type and the partition.
    for (form, identifier) in [
        ("a prompt name, not an identifier", "idempotent-version-prompt".to_string()),
        ("nine characters", "012345678".to_string()),
        ("eleven characters", "0123456789a".to_string()),
        ("a character outside the id alphabet", "0123_56789".to_string()),
        ("an ARN naming another service", prompt_arn.replace(":bedrock:", ":s3:")),
        ("an ARN naming another resource type", prompt_arn.replace(":prompt/", ":flow/")),
        ("an ARN in another partition", prompt_arn.replace("arn:aws:", "arn:aws-cn:")),
    ] {
        let refused = emulator.create_prompt_version(&identifier, json!({})).await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({form}, {identifier}): expected CreatePromptVersion -> 400, \
             got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case} ({form}, {identifier}): expected ValidationException, \
             got {refused:?}"
        );
    }

    // Every refusal above cut nothing: the prompt still holds exactly the three versions
    // the accepted cuts made, and a fourth names no resource.
    for version in ["1", "2", "3"] {
        let read = emulator.get_prompt(&prompt_id, Some(version)).await;
        assert_eq!(
            read.status, 200,
            "case {case}: expected version {version} to still be held, got {read:?}"
        );
        assert_eq!(
            read.string(&["version"]),
            version,
            "case {case}: version {version} reports itself, got {read:?}"
        );
    }
    let missing = emulator.get_prompt(&prompt_id, Some("4")).await;
    assert_eq!(
        missing.status, 404,
        "case {case}: a refused cut consumed no version number, got {missing:?}"
    );
}

/// A version is a resource of its own, addressed by its qualified ARN: the tags a cut
/// attaches are the version's, read off the version and not off the prompt, and a
/// tagging write to one leaves the other where it stood.
///
/// Rows differ in the tags the cut carries, including none at all.
#[tokio::test]
async fn a_version_carries_the_tags_its_cut_attached_under_its_own_arn() {
    for (case, cut_tags) in [
        ("a cut carrying no tags", tag_map(&[])),
        ("a cut carrying one tag", tag_map(&[("estate", "prompts")])),
        (
            "a cut carrying several tags",
            tag_map(&[("estate", "prompts"), ("tier", "production"), ("owner", "platform")]),
        ),
    ] {
        let emulator = Emulator::new();
        let prompt_tags = tag_map(&[("estate", "drafts")]);
        let draft = emulator
            .create_prompt(
                case,
                json!({
                    "name": "tagged-version-prompt",
                    "defaultVariant": "summary",
                    "variants": [text_variant("summary", "Summarise {{input}}")],
                    "tags": prompt_tags,
                }),
            )
            .await;
        let prompt_id = draft.string(&["id"]);
        let prompt_arn = draft.string(&["arn"]);

        let mut request = json!({});
        if !cut_tags.is_empty() {
            request["tags"] = json!(cut_tags);
        }
        let cut = emulator.create_prompt_version(&prompt_id, request).await;
        assert_eq!(
            cut.status, 201,
            "case {case}: expected CreatePromptVersion -> 201, got {cut:?}"
        );
        let version_arn = cut.string(&["arn"]);

        let listed = emulator.list_tags(&version_arn).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListTagsForResource -> 200 for {version_arn}, \
             got {listed:?}"
        );
        assert_eq!(
            listed.tags(case),
            cut_tags,
            "case {case}: the version reports the tags its cut attached, got {listed:?}"
        );
        assert_eq!(
            emulator.list_tags(&prompt_arn).await.tags(case),
            prompt_tags,
            "case {case}: the prompt keeps its own tags"
        );

        // A tagging write reaches the resource the ARN names and no other.
        let written = tag_map(&[("estate", "rewritten"), ("release", "cut")]);
        let tagged = emulator.tag(&version_arn, &written).await;
        assert_eq!(
            tagged.status, 200,
            "case {case}: expected TagResource -> 200 for {version_arn}, got {tagged:?}"
        );
        let mut expected = cut_tags.clone();
        expected.extend(written.clone());
        assert_eq!(
            emulator.list_tags(&version_arn).await.tags(case),
            expected,
            "case {case}: the write merges into the version's own tags"
        );
        assert_eq!(
            emulator.list_tags(&prompt_arn).await.tags(case),
            prompt_tags,
            "case {case}: tagging a version leaves the prompt's tags where they stood"
        );

        // And the version itself still reads back as it was cut.
        let read = emulator.get_prompt(&prompt_id, Some("1")).await;
        assert_eq!(
            read.body, cut.body,
            "case {case}: a tagging write leaves the snapshot as it stood, got {read:?}"
        );
    }
}
