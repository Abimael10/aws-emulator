// SPDX-License-Identifier: CC0-1.0

//! `DeletePrompt`, end to end over HTTP, read back through `GetPrompt`, `ListPrompts` and
//! `ListTagsForResource`: a delete the service accepts is COMMITTED to the store, so what
//! it removed is gone for every later caller.
//!
//! A request naming no version removes the prompt — the working draft it is, the numbered
//! versions cut from it and the tags they carried — and one naming a version removes that
//! version alone. Everything the delete removed then answers the modelled
//! `ResourceNotFoundException`, and everything it did not stands exactly as it was.
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

/// The `ResourceNotFoundException` sentence this service words when a resource an
/// operation was asked to resolve is not there: fixed text whose only variable parts are
/// the operation the caller invoked and the type of the resource that could not be
/// resolved, with nothing from the request interpolated into it.
fn not_found_message(operation: &str, resource_type: &str) -> String {
    format!(
        "Couldn't perform {operation} operation. Failed to retrieve resource type \
         {resource_type} because it doesn't exist. Check the resource ID and retry \
         your request."
    )
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

    /// The `version` member of every reported `promptSummaries` entry, in the order the
    /// listing reported them.
    fn listed_versions(&self, case: &str) -> Vec<String> {
        self.member(&["promptSummaries"])
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case}: expected promptSummaries, got {:?}", self.body)
            })
            .iter()
            .map(|summary| {
                summary["version"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case}: a summary carries a version"))
                    .to_string()
            })
            .collect()
    }

    /// The `id` member of every reported `promptSummaries` entry.
    fn listed_ids(&self, case: &str) -> Vec<String> {
        self.member(&["promptSummaries"])
            .as_array()
            .unwrap_or_else(|| {
                panic!("case {case}: expected promptSummaries, got {:?}", self.body)
            })
            .iter()
            .map(|summary| {
                summary["id"]
                    .as_str()
                    .unwrap_or_else(|| panic!("case {case}: a summary carries an id"))
                    .to_string()
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

    /// `CreatePrompt`, which mints the prompt a delete removes.
    async fn create_prompt(&self, case: &str, request: Value) -> Answer {
        let created = self.call("POST", "/prompts/", Some(request)).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreatePrompt -> 201, got {created:?}"
        );
        created
    }

    /// `CreatePromptVersion`, which cuts a numbered version off the working draft.
    async fn create_prompt_version(&self, case: &str, identifier: &str, request: Value) -> Answer {
        let cut = self
            .call(
                "POST",
                &format!("/prompts/{}/versions", label(identifier)),
                Some(request),
            )
            .await;
        assert_eq!(
            cut.status, 201,
            "case {case}: expected CreatePromptVersion -> 201, got {cut:?}"
        );
        cut
    }

    /// `GetPrompt` on any `PromptIdentifier` form, optionally naming a version.
    async fn get_prompt(&self, identifier: &str, prompt_version: Option<&str>) -> Answer {
        let query = prompt_version
            .map(|version| format!("?promptVersion={version}"))
            .unwrap_or_default();
        self.call("GET", &format!("/prompts/{}/{query}", label(identifier)), None)
            .await
    }

    /// `DeletePrompt` on any `PromptIdentifier` form, optionally naming the version to
    /// remove rather than the prompt.
    async fn delete_prompt(&self, identifier: &str, prompt_version: Option<&str>) -> Answer {
        let query = prompt_version
            .map(|version| format!("?promptVersion={version}"))
            .unwrap_or_default();
        self.call(
            "DELETE",
            &format!("/prompts/{}/{query}", label(identifier)),
            None,
        )
        .await
    }

    /// `ListPrompts`: the account's prompts, or one prompt's versions.
    async fn list_prompts(&self, identifier: Option<&str>) -> Answer {
        let query = identifier
            .map(|identifier| format!("?promptIdentifier={}", label(identifier)))
            .unwrap_or_default();
        self.call("GET", &format!("/prompts/{query}"), None).await
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

/// The prompt a delete is applied to: fully populated and tagged, so the removal of
/// everything it held is observable.
fn populated_prompt(name: &str) -> Value {
    json!({
        "name": name,
        "description": "the prompt being deleted",
        "defaultVariant": "summary",
        "variants": [text_variant("summary", "Summarise {{input}}")],
        "tags": { "estate": "prompts" },
    })
}

/// A delete naming no version removes the PROMPT: its working draft, every numbered
/// version cut from it, and the tags both carried. Every later read of any of them
/// answers the modelled `ResourceNotFoundException`, and a repeat of the delete does too.
///
/// Rows differ in the `PromptIdentifier` form the delete names the prompt with and in the
/// COUNT of versions the prompt held when it was deleted.
#[tokio::test]
async fn delete_prompt_removes_the_prompt_with_every_version_and_tag_it_held() {
    for cuts in [0_u32, 1, 3] {
        for form in ["a bare promptId", "the prompt's ARN"] {
            let case = format!("{form}, {cuts} version(s) cut");
            let emulator = Emulator::new();
            let created = emulator
                .create_prompt(&case, populated_prompt("deleted-prompt"))
                .await;
            let prompt_id = created.string(&["id"]);
            let prompt_arn = created.string(&["arn"]);

            let mut version_arns = Vec::new();
            for expected in 1..=cuts {
                let cut = emulator
                    .create_prompt_version(&case, &prompt_id, json!({ "tags": { "cut": "yes" } }))
                    .await;
                assert_eq!(
                    cut.string(&["version"]),
                    expected.to_string(),
                    "case {case}: versions are numbered incrementally from 1, got {cut:?}"
                );
                version_arns.push(cut.string(&["arn"]));
            }

            let identifier = match form {
                "a bare promptId" => prompt_id.clone(),
                _ => prompt_arn.clone(),
            };
            let deleted = emulator.delete_prompt(&identifier, None).await;
            assert_eq!(
                deleted.status, 200,
                "case {case}: expected DeletePrompt -> 200, got {deleted:?}"
            );
            assert_eq!(
                deleted.string(&["id"]),
                prompt_id,
                "case {case}: the delete reports the prompt it removed, got {deleted:?}"
            );
            assert_eq!(
                deleted.member(&["version"]),
                &Value::Null,
                "case {case}: a delete of the prompt reports no numbered version, \
                 got {deleted:?}"
            );

            // The read-back confirmation a client makes: the prompt is gone, through
            // either form that addressed it, and so is every version it held.
            for (read_form, read_identifier) in [
                ("a bare promptId", prompt_id.clone()),
                ("the prompt's ARN", prompt_arn.clone()),
            ] {
                let read = emulator.get_prompt(&read_identifier, None).await;
                assert_eq!(
                    read.status, 404,
                    "case {case} (read by {read_form}): expected GetPrompt -> 404, \
                     got {read:?}"
                );
                assert_eq!(
                    read.error_type.as_deref(),
                    Some("ResourceNotFoundException"),
                    "case {case} (read by {read_form}): expected \
                     ResourceNotFoundException, got {read:?}"
                );
                assert_eq!(
                    read.string(&["message"]),
                    not_found_message("GetPrompt", "Prompt"),
                    "case {case} (read by {read_form}): the refusal states the service's \
                     not-found sentence, got {read:?}"
                );
                // The refusal envelope carries nothing of the prompt that was removed.
                for member in ["arn", "id", "name", "version", "variants", "createdAt"] {
                    assert_eq!(
                        read.member(&[member]),
                        &Value::Null,
                        "case {case} (read by {read_form}): the refusal reports no \
                         {member}, got {read:?}"
                    );
                }
            }
            for version in 1..=cuts {
                let read = emulator
                    .get_prompt(&prompt_id, Some(&version.to_string()))
                    .await;
                assert_eq!(
                    read.status, 404,
                    "case {case}: version {version} went with the prompt, got {read:?}"
                );
            }

            // The listing does not report it either, and asking for its versions is a
            // missing prompt rather than an empty page.
            let listed = emulator.list_prompts(None).await;
            assert!(
                !listed.listed_ids(&case).contains(&prompt_id),
                "case {case}: a deleted prompt is in no listing, got {listed:?}"
            );
            let listed_versions = emulator.list_prompts(Some(&prompt_id)).await;
            assert_eq!(
                listed_versions.status, 404,
                "case {case}: the deleted prompt has no versions to enumerate, \
                 got {listed_versions:?}"
            );

            // The tags the prompt and its versions carried went with them.
            for (tagged, arn) in ::std::iter::once(("the prompt", prompt_arn.clone()))
                .chain(version_arns.iter().map(|arn| ("a version", arn.clone())))
            {
                let tags = emulator.list_tags(&arn).await;
                assert_eq!(
                    tags.status, 404,
                    "case {case}: {tagged} holds no tags once it is removed, got {tags:?}"
                );
            }

            // A repeat of the delete has nothing left to remove.
            let repeated = emulator.delete_prompt(&identifier, None).await;
            assert_eq!(
                repeated.status, 404,
                "case {case}: expected a repeated DeletePrompt -> 404, got {repeated:?}"
            );
            assert_eq!(
                repeated.string(&["message"]),
                not_found_message("DeletePrompt", "Prompt"),
                "case {case}: the repeated delete states the not-found sentence, \
                 got {repeated:?}"
            );
        }
    }
}

/// A delete naming a VERSION removes that version alone: the prompt's working draft and
/// the versions cut either side of it stand exactly as they were, the prompt's numbering
/// does not go back, and only the removed version answers `ResourceNotFoundException`.
///
/// Rows differ in HOW the version is named — the `promptVersion` query, or the ARN
/// qualified with the version — and in WHICH of the versions held is removed.
#[tokio::test]
async fn delete_prompt_with_a_version_removes_that_version_alone() {
    for removed in [1_u32, 2, 3] {
        for form in ["the promptVersion query", "a version-qualified ARN"] {
            let case = format!("{form}, version {removed} of 3 removed");
            let emulator = Emulator::new();
            let created = emulator
                .create_prompt(&case, populated_prompt("versioned-prompt"))
                .await;
            let prompt_id = created.string(&["id"]);
            let prompt_arn = created.string(&["arn"]);

            let mut cuts = Vec::new();
            for _ in 1..=3 {
                cuts.push(
                    emulator
                        .create_prompt_version(&case, &prompt_id, json!({}))
                        .await,
                );
            }

            let deleted = match form {
                "the promptVersion query" => {
                    emulator
                        .delete_prompt(&prompt_id, Some(&removed.to_string()))
                        .await
                }
                _ => {
                    emulator
                        .delete_prompt(&format!("{prompt_arn}:{removed}"), None)
                        .await
                }
            };
            assert_eq!(
                deleted.status, 200,
                "case {case}: expected DeletePrompt -> 200, got {deleted:?}"
            );
            assert_eq!(
                deleted.string(&["id"]),
                prompt_id,
                "case {case}: the delete names the prompt the version was cut from, \
                 got {deleted:?}"
            );
            assert_eq!(
                deleted.string(&["version"]),
                removed.to_string(),
                "case {case}: the delete reports the version it removed, got {deleted:?}"
            );

            // The removed version names no resource, and the refusal says so of the
            // VERSION rather than of the prompt, which is still there.
            let read = emulator
                .get_prompt(&prompt_id, Some(&removed.to_string()))
                .await;
            assert_eq!(
                read.status, 404,
                "case {case}: expected GetPrompt of the removed version -> 404, \
                 got {read:?}"
            );
            assert_eq!(
                read.error_type.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case}: expected ResourceNotFoundException, got {read:?}"
            );
            assert_eq!(
                read.string(&["message"]),
                not_found_message("GetPrompt", "PromptVersion"),
                "case {case}: the refusal names the version as the missing resource, \
                 got {read:?}"
            );

            // Everything the delete did not address stands as it was: the draft, and
            // every other version, byte for byte as it was cut.
            let draft = emulator.get_prompt(&prompt_id, None).await;
            assert_eq!(
                draft.body, created.body,
                "case {case}: deleting a version leaves the working draft as it stood, \
                 got {draft:?}"
            );
            for kept in [1_u32, 2, 3].into_iter().filter(|held| *held != removed) {
                let read = emulator.get_prompt(&prompt_id, Some(&kept.to_string())).await;
                assert_eq!(
                    read.status, 200,
                    "case {case}: version {kept} was not addressed, got {read:?}"
                );
                assert_eq!(
                    read.body,
                    cuts[(kept - 1) as usize].body,
                    "case {case}: version {kept} stands as it was cut, got {read:?}"
                );
            }

            // The version listing reports the draft and the versions still held, in the
            // order they were cut, and not the one that was removed.
            let listed = emulator.list_prompts(Some(&prompt_id)).await;
            assert_eq!(
                listed.status, 200,
                "case {case}: expected ListPrompts -> 200, got {listed:?}"
            );
            let expected: Vec<String> = ::std::iter::once("DRAFT".to_string())
                .chain(
                    [1_u32, 2, 3]
                        .into_iter()
                        .filter(|held| *held != removed)
                        .map(|held| held.to_string()),
                )
                .collect();
            assert_eq!(
                listed.listed_versions(&case),
                expected,
                "case {case}: the listing reports the versions still held, got {listed:?}"
            );

            // Numbering never goes back: the next cut takes the number after the highest
            // ever reached, not the one the removed version held.
            let next = emulator
                .create_prompt_version(&case, &prompt_id, json!({}))
                .await;
            assert_eq!(
                next.string(&["version"]),
                "4",
                "case {case}: the next cut takes the next number, got {next:?}"
            );

            // A repeat of the delete has no version left to remove.
            let repeated = emulator
                .delete_prompt(&prompt_id, Some(&removed.to_string()))
                .await;
            assert_eq!(
                repeated.status, 404,
                "case {case}: expected a repeated DeletePrompt -> 404, got {repeated:?}"
            );
            assert_eq!(
                repeated.string(&["message"]),
                not_found_message("DeletePrompt", "PromptVersion"),
                "case {case}: the repeat names the version as the missing resource, \
                 got {repeated:?}"
            );
        }
    }
}

/// A delete the service cannot serve is refused, and a refused delete removes nothing: a
/// prompt the store does not hold, a version the prompt was never cut at, and an
/// identifier of neither modelled form.
#[tokio::test]
async fn delete_prompt_refuses_a_request_it_cannot_serve_and_removes_nothing() {
    let case = "refused deletes";
    let emulator = Emulator::new();
    let created = emulator
        .create_prompt(case, populated_prompt("undeleted-prompt"))
        .await;
    let prompt_id = created.string(&["id"]);
    let prompt_arn = created.string(&["arn"]);
    let first = emulator
        .create_prompt_version(case, &prompt_id, json!({}))
        .await;
    let second = emulator
        .create_prompt_version(case, &prompt_id, json!({}))
        .await;

    // Rows: both modelled `PromptIdentifier` forms, over a well-formed id the store does
    // not hold.
    let unknown = "0123456789";
    for (form, identifier) in [
        ("bare promptId", unknown.to_string()),
        ("prompt ARN", prompt_arn.replace(prompt_id.as_str(), unknown)),
    ] {
        let refused = emulator.delete_prompt(&identifier, None).await;
        assert_eq!(
            refused.status, 404,
            "case {case} ({form}): expected DeletePrompt -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case} ({form}): expected ResourceNotFoundException, got {refused:?}"
        );
        assert_eq!(
            refused.string(&["message"]),
            not_found_message("DeletePrompt", "Prompt"),
            "case {case} ({form}): the refusal states the not-found sentence, \
             got {refused:?}"
        );
    }

    // Rows: versions this prompt was never cut at, named either way. The widest the
    // modelled `NumericalVersion` (`^[0-9]{1,5}$`) carries is included.
    for (form, identifier, version) in [
        ("the next number", prompt_id.clone(), Some("3".to_string())),
        ("a far higher number", prompt_id.clone(), Some("99999".to_string())),
        ("a version-qualified ARN", format!("{prompt_arn}:3"), None),
    ] {
        let refused = emulator
            .delete_prompt(&identifier, version.as_deref())
            .await;
        assert_eq!(
            refused.status, 404,
            "case {case} ({form}): expected DeletePrompt -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.string(&["message"]),
            not_found_message("DeletePrompt", "PromptVersion"),
            "case {case} ({form}): the refusal names the version as the missing \
             resource, got {refused:?}"
        );
    }

    // Rows differ in HOW the identifier misses the modelled `PromptIdentifier` forms: a
    // prompt name, ids of the wrong length or alphabet, and ARNs wrong in the service,
    // the resource type and the partition.
    for (form, identifier) in [
        ("a prompt name, not an identifier", "undeleted-prompt".to_string()),
        ("nine characters", "012345678".to_string()),
        ("eleven characters", "0123456789a".to_string()),
        ("a character outside the id alphabet", "0123_56789".to_string()),
        ("an ARN naming another service", prompt_arn.replace(":bedrock:", ":s3:")),
        ("an ARN naming another resource type", prompt_arn.replace(":prompt/", ":flow/")),
        ("an ARN in another partition", prompt_arn.replace("arn:aws:", "arn:aws-cn:")),
    ] {
        let refused = emulator.delete_prompt(&identifier, None).await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({form}, {identifier}): expected DeletePrompt -> 400, \
             got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case} ({form}, {identifier}): expected ValidationException, \
             got {refused:?}"
        );
    }

    // Every refusal above removed nothing: the draft and both versions still read back
    // exactly as they were, with the tags they carried.
    let read_draft = emulator.get_prompt(&prompt_id, None).await;
    assert_eq!(
        read_draft.body, created.body,
        "case {case}: a refused delete leaves the draft as it stood, got {read_draft:?}"
    );
    for (version, cut) in [("1", &first), ("2", &second)] {
        let read = emulator.get_prompt(&prompt_id, Some(version)).await;
        assert_eq!(
            read.body, cut.body,
            "case {case}: a refused delete leaves version {version} as it was cut, \
             got {read:?}"
        );
    }
    assert_eq!(
        emulator.list_tags(&prompt_arn).await.tags(case),
        BTreeMap::from([("estate".to_string(), "prompts".to_string())]),
        "case {case}: a refused delete leaves the prompt's tags where they stood"
    );
}
