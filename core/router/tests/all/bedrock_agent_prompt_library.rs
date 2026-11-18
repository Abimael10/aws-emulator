// SPDX-License-Identifier: CC0-1.0

//! The Bedrock Agent prompt library, end to end over HTTP: `CreatePrompt` mints a
//! prompt and STORES it, and `GetPrompt` reports that stored record back.
//!
//! The rule under test: **a prompt is minted by `CreatePrompt` with a `promptId` of its
//! own, an ARN built from the caller's Region and that id, and the DRAFT version as its
//! only version; every member the request carried is persisted exactly as it was sent
//! (an absent list and an empty one stay different values), and every later read of the
//! prompt — through either `PromptIdentifier` form, and through the tagging surface that
//! addresses its ARN — is a projection of that one record.** `includedData` decides only
//! whether the read carries the prompt's variants, a version the prompt does not hold is
//! a missing resource, a `defaultVariant` naming none of the prompt's variants is a bad
//! request, and a repeated `@idempotencyToken` returns the prompt the first call created
//! rather than minting a second one.
//!
//! The rows differ along what the rule generalises over: which optional members a
//! request carries, the `PromptTemplateType` its variant declares (`TEXT` and `CHAT`,
//! the two the modelled enum admits) and how large that template is, whether `variants`
//! is absent or an empty list, how many tags the prompt carries, the form of the
//! identifier a read addresses it by, and every value of `includedData` including the
//! omitted member.
//!
//! This suite lives here, beside the other suites of the assembled emulator, because the
//! owning crate still roots its integration tests at a hand-declared `tests/all.rs`,
//! which a fix may not edit; the router's `tests/all/` picks a new suite up with nothing
//! to declare. Every call goes through a router with the owning service mounted on one
//! shared engine, exactly as the production router mounts it, so each assertion
//! exercises the real wire path (route matching, model constraint validation,
//! serialization) rather than calling a handler directly.

use std::sync::Arc;

use bytes::Bytes;
use serde_json::{json, Value};
use smithy_server_runtime::MultiServiceRouter;

/// A SigV4 `Authorization` header scoped to the `bedrock` signing name: the routing key
/// this service claims on, and the source of the Region its ARNs carry.
const AUTHORIZATION: &str = "AWS4-HMAC-SHA256 \
     Credential=AKIDEXAMPLE/20260101/eu-west-1/bedrock/aws4_request, \
     SignedHeaders=host;x-amz-date, Signature=0123456789abcdef";

/// The Region of [`AUTHORIZATION`]'s credential scope.
const REGION: &str = "eu-west-1";

/// The account id every ARN this emulator mints carries.
const ACCOUNT: &str = "000000000000";

/// A KMS key ARN matching the modelled `KmsKeyArn` pattern.
const ENCRYPTION_KEY: &str = "arn:aws:kms:eu-west-1:000000000000:key/12345678-1234-1234-1234-123456789012";

/// A model identifier matching the modelled `PromptModelIdentifier` pattern.
const MODEL_ID: &str = "anthropic.claude-3-haiku-20240307-v1:0";

/// The members of a `GetPrompt` response that describe the prompt's metadata — every
/// member but `variants`, which is the one part a metadata-only read drops.
const METADATA_MEMBERS: [&str; 8] = [
    "name",
    "description",
    "customerEncryptionKeyArn",
    "defaultVariant",
    "id",
    "arn",
    "version",
    "createdAt",
];

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

/// The emulator under test: one router over one shared engine.
struct Emulator {
    router: MultiServiceRouter,
}

impl Emulator {
    fn new() -> Self {
        let engine: Arc<engine::InMemoryEngine> = Arc::new(engine::InMemoryEngine::new());
        let mut router = MultiServiceRouter::new();
        router.set_request_authentication(false);
        router.register_service(
            service_com_amazonaws_bedrockagent_amazonbedrockagentbuildtimelambda::EmulatedService::new(
                engine,
            ),
        );
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

    /// `GetPrompt` on any `PromptIdentifier` form, with or without `promptVersion` and
    /// `includedData`.
    async fn get_prompt(
        &self,
        identifier: &str,
        prompt_version: Option<&str>,
        included_data: Option<&str>,
    ) -> Answer {
        let query: Vec<String> = prompt_version
            .map(|version| format!("promptVersion={version}"))
            .into_iter()
            .chain(included_data.map(|value| format!("includedData={value}")))
            .collect();
        let query = if query.is_empty() {
            String::new()
        } else {
            format!("?{}", query.join("&"))
        };
        self.call(
            "GET",
            &format!("/prompts/{}/{query}", label(identifier)),
            None,
        )
        .await
    }

    /// `ListTagsForResource` on a resource ARN.
    async fn list_tags(&self, resource_arn: &str) -> Answer {
        self.call("GET", &format!("/tags/{}", label(resource_arn)), None)
            .await
    }
}

/// One value as a percent-encoded URI component, as a client that sends a non-greedy
/// `@httpLabel` encodes it.
fn label(value: &str) -> String {
    value.replace('%', "%25").replace(':', "%3A").replace('/', "%2F")
}

/// A `TEXT` variant, the smallest template the modelled
/// `TextPromptTemplateConfiguration` admits (`text` is its one required member).
fn text_variant(name: &str) -> Value {
    json!({
        "name": name,
        "templateType": "TEXT",
        "templateConfiguration": { "text": { "text": "Summarise the following: {{input}}" } },
    })
}

/// A larger `TEXT` variant: the same template plus the input variables it declares, a
/// model to run it on and the inference settings for that run.
fn configured_text_variant(name: &str) -> Value {
    json!({
        "name": name,
        "templateType": "TEXT",
        "templateConfiguration": {
            "text": {
                "text": "Answer {{question}} for {{customer}} in {{language}}.",
                "inputVariables": [
                    { "name": "question" },
                    { "name": "customer" },
                    { "name": "language" },
                ],
            },
        },
        "modelId": MODEL_ID,
        "inferenceConfiguration": {
            "text": {
                // Values exactly representable as the modelled `Temperature`/`TopP`
                // floats, so the round trip compares the request's own numbers rather
                // than their nearest float.
                "temperature": 0.5,
                "topP": 0.75,
                "maxTokens": 512,
                "stopSequences": ["\n\nHuman:"],
            },
        },
        "metadata": [{ "key": "team", "value": "platform" }],
        "additionalModelRequestFields": { "anthropic_version": "bedrock-2023-05-31" },
    })
}

/// A `CHAT` variant: the other `PromptTemplateType` the model admits, whose template is
/// a conversation rather than one text block.
fn chat_variant(name: &str) -> Value {
    json!({
        "name": name,
        "templateType": "CHAT",
        "templateConfiguration": {
            "chat": {
                "messages": [
                    { "role": "user", "content": [{ "text": "What changed in {{release}}?" }] },
                    { "role": "assistant", "content": [{ "text": "Here is the summary:" }] },
                ],
                "system": [{ "text": "You are a release-notes assistant." }],
                "inputVariables": [{ "name": "release" }],
            },
        },
        "modelId": MODEL_ID,
    })
}

/// Assert `prompt_id` is shaped as the modelled `PromptId` demands
/// (`^[0-9a-zA-Z]{10}$`).
fn assert_well_formed_id(case: &str, prompt_id: &str) {
    assert_eq!(
        prompt_id.len(),
        10,
        "case {case}: expected a 10-character promptId, got {prompt_id:?}"
    );
    assert!(
        prompt_id.chars().all(|c| c.is_ascii_alphanumeric()),
        "case {case}: expected promptId to match ^[0-9a-zA-Z]{{10}}$, got {prompt_id:?}"
    );
}

/// `CreatePrompt` persists every member the request supplied, and `GetPrompt` reports
/// that record back — over a table of requests that differ in which optional members
/// they carry, in the template type and size of the variant they declare, and in whether
/// `variants` is absent or an empty list.
#[tokio::test]
async fn created_prompts_round_trip_through_get_prompt() {
    let emulator = Emulator::new();

    let cases: Vec<(&str, Value)> = vec![
        (
            "minimal: only the required name",
            json!({ "name": "minimal-prompt" }),
        ),
        (
            "a description and a TEXT variant it defaults to",
            json!({
                "name": "summarise-prompt",
                "description": "summarises whatever it is given",
                "defaultVariant": "summary",
                "variants": [text_variant("summary")],
            }),
        ),
        (
            "a larger TEXT variant: input variables, a model and inference settings",
            json!({
                "name": "answer_prompt",
                "defaultVariant": "answer",
                "variants": [configured_text_variant("answer")],
            }),
        ),
        (
            "a CHAT variant, the other modelled template type",
            json!({
                "name": "ReleaseNotes1",
                "description": "a conversation rather than one text block",
                "defaultVariant": "chat",
                "variants": [chat_variant("chat")],
                "tags": { "team": "platform", "estate": "prompts" },
            }),
        ),
        (
            "every member of this slice supplied",
            json!({
                "name": "FullPrompt1",
                "description": "a prompt carrying every member of this slice",
                "customerEncryptionKeyArn": ENCRYPTION_KEY,
                "defaultVariant": "full",
                "variants": [configured_text_variant("full")],
                "tags": { "estate": "prompts" },
            }),
        ),
        (
            "an EMPTY variants list, which is a value and not an absence",
            json!({ "name": "empty-variants-prompt", "variants": [] }),
        ),
    ];

    for (case, request) in cases {
        let created = emulator.create_prompt(request.clone()).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreatePrompt -> 201, got {created:?}"
        );

        let prompt_id = created.string(&["id"]);
        assert_well_formed_id(case, &prompt_id);
        assert_eq!(
            created.string(&["arn"]),
            format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:prompt/{prompt_id}"),
            "case {case}: the prompt ARN must be built from the request's Region, the \
             account and the minted id"
        );
        assert_eq!(
            created.string(&["version"]),
            "DRAFT",
            "case {case}: a newly created prompt has only the DRAFT version"
        );
        // `createdAt`/`updatedAt` target `DateTimestamp`, which carries
        // `@timestampFormat("date-time")`
        // (`smithy_query(query="shape", shape="CreatePromptResponse")`), so each one is
        // populated AND carried as an RFC-3339 string rather than an epoch number.
        for member in ["createdAt", "updatedAt"] {
            let at = created.member(&[member]);
            let text = at.as_str().unwrap_or_else(|| {
                panic!("case {case}: expected {member} to be a date-time string, got {at:?}")
            });
            assert!(
                text.contains('T') && text.ends_with('Z'),
                "case {case}: expected {member} in the modelled date-time wire form, got \
                 {text:?}"
            );
        }

        // The round trip: the stored record, read back through GetPrompt.
        let fetched = emulator.get_prompt(&prompt_id, None, None).await;
        assert_eq!(
            fetched.status, 200,
            "case {case}: expected GetPrompt -> 200, got {fetched:?}"
        );
        for member in METADATA_MEMBERS
            .iter()
            .chain(["updatedAt", "variants"].iter())
        {
            assert_eq!(
                fetched.member(&[member]),
                created.member(&[member]),
                "case {case}: GetPrompt must report {member} exactly as CreatePrompt \
                 persisted it, got {:?}",
                fetched.member(&[member])
            );
        }

        // Every member the request supplied survives the round trip verbatim — the
        // empty `variants` list among them, which stays an empty list rather than
        // becoming an absence.
        for member in [
            "name",
            "description",
            "customerEncryptionKeyArn",
            "defaultVariant",
            "variants",
        ] {
            if let Some(supplied) = request.get(member) {
                assert_eq!(
                    fetched.member(&[member]),
                    supplied,
                    "case {case}: expected {member} to round-trip as {supplied:?}, got {:?}",
                    fetched.member(&[member])
                );
            } else {
                assert!(
                    fetched.member(&[member]).is_null(),
                    "case {case}: expected {member} to stay absent, got {:?}",
                    fetched.member(&[member])
                );
            }
        }

        // The tags the create stored are the ones the tagging surface reports for the
        // prompt's own ARN: one record, read through either reader.
        let expected_tags = request.get("tags").cloned().unwrap_or_else(|| json!({}));
        let listed = emulator.list_tags(&created.string(&["arn"])).await;
        assert_eq!(
            listed.status, 200,
            "case {case}: expected ListTagsForResource on the prompt ARN -> 200, got \
             {listed:?}"
        );
        assert_eq!(
            listed.member(&["tags"]),
            &expected_tags,
            "case {case}: expected the tags the create stored, got {:?}",
            listed.member(&["tags"])
        );
    }
}

/// Either `PromptIdentifier` form — the bare `promptId` or the prompt's ARN — selects the
/// same stored prompt, and `includedData` decides only whether the variants come with it.
#[tokio::test]
async fn get_prompt_resolves_every_identifier_form_and_honours_included_data() {
    let emulator = Emulator::new();

    let created = emulator
        .create_prompt(json!({
            "name": "identifier-forms-prompt",
            "description": "read back through both identifier forms",
            "defaultVariant": "summary",
            "variants": [text_variant("summary")],
        }))
        .await;
    assert_eq!(
        created.status, 201,
        "case setup: expected CreatePrompt -> 201, got {created:?}"
    );
    let prompt_id = created.string(&["id"]);
    let prompt_arn = created.string(&["arn"]);

    // Rows: both modelled identifier forms x every `includedData` value, the omitted
    // member included, x the two ways of naming the working draft (the member omitted,
    // and DRAFT named explicitly).
    for (identifier_case, identifier) in [("bare promptId", &prompt_id), ("prompt ARN", &prompt_arn)]
    {
        for (data_case, included_data, expect_variants) in [
            ("includedData omitted", None, true),
            ("includedData ALL_DATA", Some("ALL_DATA"), true),
            ("includedData METADATA_ONLY", Some("METADATA_ONLY"), false),
        ] {
            for (version_case, prompt_version) in
                [("promptVersion omitted", None), ("promptVersion DRAFT", Some("DRAFT"))]
            {
                let case = format!("{identifier_case}, {data_case}, {version_case}");
                let fetched = emulator
                    .get_prompt(identifier, prompt_version, included_data)
                    .await;
                assert_eq!(
                    fetched.status, 200,
                    "case {case}: expected GetPrompt -> 200, got {fetched:?}"
                );
                assert_eq!(
                    fetched.string(&["id"]),
                    prompt_id,
                    "case {case}: both identifier forms must name the same prompt"
                );

                // The metadata is the same whatever was asked for.
                for member in METADATA_MEMBERS {
                    assert_eq!(
                        fetched.member(&[member]),
                        created.member(&[member]),
                        "case {case}: expected {member} to be reported as stored, got {:?}",
                        fetched.member(&[member])
                    );
                }

                if expect_variants {
                    assert_eq!(
                        fetched.member(&["variants"]),
                        created.member(&["variants"]),
                        "case {case}: the full response carries the stored variants"
                    );
                } else {
                    assert!(
                        fetched.member(&["variants"]).is_null(),
                        "case {case}: a metadata-only read drops the variants, got {:?}",
                        fetched.member(&["variants"])
                    );
                }
            }
        }
    }

    // A read changes nothing a later reader sees: the metadata-only reads above did not
    // drop the stored variants.
    let after = emulator.get_prompt(&prompt_id, None, None).await;
    assert_eq!(
        after.member(&["variants"]),
        created.member(&["variants"]),
        "case read-after-metadata-only: a read must not change what a later caller sees"
    );
}

/// A version the prompt does not hold names no resource: until something cuts a numbered
/// version from the working draft, the only version a prompt has is its DRAFT — whether
/// the version is asked for in `promptVersion` or qualified onto the identifier itself.
#[tokio::test]
async fn get_prompt_refuses_a_version_the_prompt_does_not_hold() {
    let emulator = Emulator::new();

    let created = emulator
        .create_prompt(json!({
            "name": "version-reads-prompt",
            "defaultVariant": "summary",
            "variants": [text_variant("summary")],
        }))
        .await;
    assert_eq!(
        created.status, 201,
        "case setup: expected CreatePrompt -> 201, got {created:?}"
    );
    let prompt_id = created.string(&["id"]);
    let prompt_arn = created.string(&["arn"]);

    // Rows: the version asked for in the query member (the lowest, a middle one and the
    // widest the modelled `Version` pattern admits) and the version qualified onto the
    // ARN, which the modelled `PromptIdentifier` admits as a `prompt/<id>:<version>`
    // tail.
    let mut rows: Vec<(String, String, Option<String>)> = Vec::new();
    for version in ["1", "2", "99999"] {
        rows.push((
            format!("promptVersion {version}"),
            prompt_id.clone(),
            Some(version.to_string()),
        ));
        rows.push((
            format!("an ARN qualified with version {version}"),
            format!("{prompt_arn}:{version}"),
            None,
        ));
    }

    for (case, identifier, prompt_version) in &rows {
        let refused = emulator
            .get_prompt(identifier, prompt_version.as_deref(), None)
            .await;
        assert_eq!(
            refused.status, 404,
            "case {case}: expected GetPrompt -> 404, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ResourceNotFoundException"),
            "case {case}: expected the modelled ResourceNotFoundException, got {refused:?}"
        );
    }

    // The working draft is still readable after every refusal.
    let fetched = emulator.get_prompt(&prompt_id, None, None).await;
    assert_eq!(
        fetched.status, 200,
        "case the draft after the refusals: expected GetPrompt -> 200, got {fetched:?}"
    );
}

/// `GetPrompt` answers `ResourceNotFoundException` (404) for a well-formed identifier no
/// prompt holds, in either identifier form.
#[tokio::test]
async fn get_prompt_refuses_an_identifier_no_prompt_holds() {
    let emulator = Emulator::new();

    let created = emulator
        .create_prompt(json!({ "name": "the-one-prompt" }))
        .await;
    assert_eq!(
        created.status, 201,
        "case setup: expected CreatePrompt -> 201, got {created:?}"
    );
    let stored_id = created.string(&["id"]);

    // Rows: both identifier forms, with ids the store does not hold (one all digits, one
    // mixed case, one differing from the stored id in its last character only).
    let mut unknown_ids = vec!["0123456789".to_string(), "aZ09aZ09aZ".to_string()];
    let mut nearly = stored_id.clone();
    nearly.pop();
    nearly.push(if stored_id.ends_with('X') { 'Y' } else { 'X' });
    if nearly != stored_id {
        unknown_ids.push(nearly);
    }

    for unknown in unknown_ids {
        for (form, identifier) in [
            ("bare promptId", unknown.clone()),
            (
                "prompt ARN",
                format!("arn:aws:bedrock:{REGION}:{ACCOUNT}:prompt/{unknown}"),
            ),
        ] {
            let case = format!("{form} {identifier}");
            let refused = emulator.get_prompt(&identifier, None, None).await;
            assert_eq!(
                refused.status, 404,
                "case {case}: expected GetPrompt -> 404, got {refused:?}"
            );
            assert_eq!(
                refused.error_type.as_deref(),
                Some("ResourceNotFoundException"),
                "case {case}: expected ResourceNotFoundException, got {refused:?}"
            );
        }
    }

    // The prompt that does exist is still readable.
    let fetched = emulator.get_prompt(&stored_id, None, None).await;
    assert_eq!(
        fetched.status, 200,
        "case stored prompt: expected GetPrompt -> 200, got {fetched:?}"
    );
}

/// An identifier of neither modelled `PromptIdentifier` form is a bad request, not a
/// missing prompt: `GetPrompt` refuses it with `ValidationException` (400), naming the
/// member and the constraint the value failed.
#[tokio::test]
async fn get_prompt_refuses_an_identifier_of_neither_modelled_form() {
    let emulator = Emulator::new();

    let created = emulator
        .create_prompt(json!({ "name": "well-formed-prompt" }))
        .await;
    assert_eq!(
        created.status, 201,
        "case setup: expected CreatePrompt -> 201, got {created:?}"
    );

    // Rows differ in HOW the value misses the modelled forms: a prompt name, an id of
    // the wrong length or alphabet, and ARNs wrong in the partition, the vendor, the
    // Region, the account, the resource type and the version qualifier.
    let malformed = [
        ("a prompt name, not an identifier", "well-formed-prompt"),
        ("nine characters", "012345678"),
        ("eleven characters", "0123456789a"),
        ("a character outside the id alphabet", "0123_56789"),
        (
            "an ARN whose id is too short",
            "arn:aws:bedrock:eu-west-1:000000000000:prompt/012345678",
        ),
        (
            "an ARN naming another vendor",
            "arn:aws:s3:eu-west-1:000000000000:prompt/0123456789",
        ),
        (
            "an ARN whose account is not 12 digits",
            "arn:aws:bedrock:eu-west-1:0000:prompt/0123456789",
        ),
        (
            "an ARN whose Region is outside the pattern's grammar",
            "arn:aws:bedrock:EU-WEST-1:000000000000:prompt/0123456789",
        ),
        (
            "an ARN naming another resource type",
            "arn:aws:bedrock:eu-west-1:000000000000:flow/0123456789",
        ),
        (
            "an ARN in another partition",
            "arn:aws-cn:bedrock:eu-west-1:000000000000:prompt/0123456789",
        ),
        (
            "an ARN whose version qualifier is not digits",
            "arn:aws:bedrock:eu-west-1:000000000000:prompt/0123456789:DRAFT",
        ),
        (
            "an ARN whose version qualifier is too wide",
            "arn:aws:bedrock:eu-west-1:000000000000:prompt/0123456789:123456",
        ),
    ];

    for (case, identifier) in malformed {
        let refused = emulator.get_prompt(identifier, None, None).await;
        assert_eq!(
            refused.status, 400,
            "case {case} ({identifier}): expected GetPrompt -> 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case} ({identifier}): expected ValidationException, got {refused:?}"
        );
        let message = refused.string(&["message"]);
        assert!(
            message.contains("at 'promptIdentifier' failed to satisfy constraint"),
            "case {case} ({identifier}): expected the refusal to name the member and its \
             constraint, got {message:?}"
        );
    }
}

/// A `defaultVariant` that names none of the prompt's variants is a bad request: the
/// member is documented to match the name field of one of them.
#[tokio::test]
async fn create_prompt_refuses_a_default_variant_that_names_no_variant() {
    let emulator = Emulator::new();

    // Rows: a default variant with no variants at all, with an empty variants list, and
    // with a variant whose name differs — in case, and entirely.
    let refused_cases: Vec<(&str, Value)> = vec![
        (
            "a default variant and no variants member",
            json!({ "name": "no-variants-prompt", "defaultVariant": "summary" }),
        ),
        (
            "a default variant and an empty variants list",
            json!({
                "name": "empty-variants-default",
                "defaultVariant": "summary",
                "variants": [],
            }),
        ),
        (
            "a default variant naming another variant",
            json!({
                "name": "other-variant-default",
                "defaultVariant": "summary",
                "variants": [text_variant("translation")],
            }),
        ),
        (
            "a default variant differing from the variant's name in case only",
            json!({
                "name": "case-variant-default",
                "defaultVariant": "Summary",
                "variants": [text_variant("summary")],
            }),
        ),
    ];

    for (case, request) in refused_cases {
        let refused = emulator.create_prompt(request).await;
        assert_eq!(
            refused.status, 400,
            "case {case}: expected CreatePrompt -> 400, got {refused:?}"
        );
        assert_eq!(
            refused.error_type.as_deref(),
            Some("ValidationException"),
            "case {case}: expected the modelled ValidationException, got {refused:?}"
        );
        let message = refused.string(&["message"]);
        assert!(
            message.contains("at 'defaultVariant' failed to satisfy constraint"),
            "case {case}: expected the refusal to name the member and its constraint, got \
             {message:?}"
        );
    }

    // And the accepted side of the same rule: a default variant that does name one of
    // the prompt's variants is served, as is a prompt that names no default at all.
    for (case, request) in [
        (
            "a default variant naming the prompt's own variant",
            json!({
                "name": "matching-default-prompt",
                "defaultVariant": "summary",
                "variants": [text_variant("summary")],
            }),
        ),
        (
            "variants with no default variant named",
            json!({
                "name": "no-default-prompt",
                "variants": [text_variant("summary")],
            }),
        ),
    ] {
        let created = emulator.create_prompt(request).await;
        assert_eq!(
            created.status, 201,
            "case {case}: expected CreatePrompt -> 201, got {created:?}"
        );
    }
}

/// A repeated `@idempotencyToken` returns the prompt the first call created, and two
/// prompts that share a name are still two distinct prompts — the model constrains
/// `PromptName`'s shape, not its uniqueness.
#[tokio::test]
async fn create_prompt_honours_its_idempotency_token_and_mints_one_prompt_per_call() {
    let emulator = Emulator::new();

    // Two tokens, each replayed: a replay returns the prompt the first call created and
    // mints nothing new.
    for token in [
        "idempotency-token-for-the-first-prompt",
        "IdempotencyTokenForTheSecondPrompt0",
    ] {
        let request = json!({
            "name": "idempotent-prompt",
            "defaultVariant": "summary",
            "variants": [text_variant("summary")],
            "clientToken": token,
        });
        let once = emulator.create_prompt(request.clone()).await;
        let again = emulator.create_prompt(request).await;
        assert_eq!(
            once.status, 201,
            "case token {token}: expected 201 on the first call, got {once:?}"
        );
        assert_eq!(
            again.status, 201,
            "case token {token}: expected 201 on the replay, got {again:?}"
        );
        assert_eq!(
            again.string(&["id"]),
            once.string(&["id"]),
            "case token {token}: a replayed idempotency token must return the same prompt"
        );
        assert_eq!(
            again.body, once.body,
            "case token {token}: a replay reports the stored prompt, unchanged"
        );
    }

    // Same name, no token: distinct prompts, each readable through GetPrompt.
    let first = emulator
        .create_prompt(json!({ "name": "shared-name-prompt" }))
        .await;
    let second = emulator
        .create_prompt(json!({ "name": "shared-name-prompt" }))
        .await;
    assert_eq!(
        second.status, 201,
        "case duplicate name: the model constrains PromptName's pattern, not its \
         uniqueness, got {second:?}"
    );
    assert_ne!(
        first.string(&["id"]),
        second.string(&["id"]),
        "case duplicate name: each create mints its own promptId"
    );
    for (case, answer) in [("first", &first), ("second", &second)] {
        let id = answer.string(&["id"]);
        let fetched = emulator.get_prompt(&id, None, None).await;
        assert_eq!(
            fetched.string(&["arn"]),
            answer.string(&["arn"]),
            "case duplicate name {case}: GetPrompt reports the prompt that create minted"
        );
    }
}
