// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Amazon Q in Connect AI-prompt VERSION plane
//! (`CreateAIPromptVersion`, `ListAIPromptVersions` and the version-addressed read of
//! `GetAIPrompt`).
//!
//! Every assertion goes over the wire, through the same [`Emulator`] harness the rest of
//! this crate's suites drive the production router with: only the HTTP status, the
//! `x-amzn-errortype` header and the JSON body are inspected, so a response that is not
//! computed from persisted state cannot pass.
//!
//! The rule under test, asserted over a TABLE of inputs rather than one recorded request:
//! publishing a version of a versioned resource mints an IMMUTABLE, numbered snapshot of
//! that resource's current working copy — the first version of a resource is 1, each later
//! publish is the previous number plus 1 and no two publishes are handed one number even
//! when they overlap, the snapshot is addressable as `<id>:<versionNumber>` and reports
//! exactly what was published under it however often the working copy changes afterwards,
//! and the listing reports the versions still held in ascending order.
//!
//! The rows differ along every axis the rule generalises over:
//!
//! * which `AIPromptType` the prompt is, which `AIPromptAPIFormat` it speaks and which
//!   model it names;
//! * its `visibilityStatus`, and whether it carries a description at all;
//! * which member a later update changes, and how many updates are layered on one record;
//! * how many versions the prompt has published, whether they were published one after
//!   another or concurrently, and how the listing is paged and filtered;
//! * whether the publish states a `modifiedTime` precondition, and whether that
//!   precondition still holds.
//!
//! Beside the round trip it pins the refusals: an assistant this account does not hold, a
//! prompt that assistant does not hold, a prompt of ANOTHER assistant, a label that names a
//! published version where the working copy is what publishes, a version number nothing
//! published, a `nextToken` this service did not mint, and the ceiling on how many versions
//! one prompt may hold.

use serde_json::{json, Value};

use service_com_amazonaws_qconnect_wisdomservice::state::MAX_AI_PROMPT_VERSIONS_PER_PROMPT;

use super::ai_guardrail::{assistant_id, QUALIFIER_LATEST, VISIBILITY_PUBLISHED, VISIBILITY_SAVED};
use super::ai_prompt::{
    create_prompt, get_prompt, required, template, update_prompt, API_FORMATS, MODEL_IDS,
    PROMPT_TYPES,
};
use super::resource_tags::{label, Emulator, Reply, ABSENT_UUID};

/// The modelled error shapes this surface refuses with, as the wire reports them.
const CODE_NOT_FOUND: &str = "ResourceNotFoundException";
const CODE_VALIDATION: &str = "ValidationException";
const CODE_CONFLICT: &str = "ConflictException";
const CODE_QUOTA: &str = "ServiceQuotaExceededException";

/// The sentences this endpoint refuses this surface with. Written out here rather than
/// imported from the implementation, so the assertions pin the answer a client sees.
const MESSAGE_ASSISTANT_NOT_FOUND: &str = "Assistant does not exist.";
const MESSAGE_AI_PROMPT_NOT_FOUND: &str = "AI Prompt does not exist.";
const MESSAGE_MODIFIED_SINCE: &str =
    "The AI Prompt has been modified after the specified modifiedTime, so no version was \
     created. Read the AI Prompt again and retry with the modifiedTime that read reports.";
const MESSAGE_IDEMPOTENCY_CONFLICT: &str = "Idempotency conflict";
const MESSAGE_BAD_TOKEN: &str = "nextToken: the specified token is not valid.";

/// The low bound of the modelled `Version` shape
/// (`smithy_query(query="operation",operation="CreateAIPromptVersion")`: `versionNumber` is
/// `range min 1`), and a number no prompt of these tests ever publishes.
const VERSION_MIN: i64 = 1;
const VERSION_NEVER_PUBLISHED: i64 = 9;

/// Both values the modelled `Origin` declares
/// (`smithy_query(query="shape",shape="Origin")`), which `ListAIPromptVersions` filters by:
/// a prompt a caller created is `CUSTOMER`.
const ORIGIN_CUSTOMER: &str = "CUSTOMER";
const ORIGIN_SYSTEM: &str = "SYSTEM";

/// A day in seconds, used to state a `modifiedTime` precondition that is unambiguously
/// older than the one the record carries without waiting for any clock.
const ONE_DAY_SECONDS: f64 = 86_400.0;

/// `CreateAIPromptVersion` on one prompt, as the model's URI binds it
/// (`POST /assistants/{assistantId}/aiprompts/{aiPromptId}/versions`).
pub(crate) async fn publish(
    emulator: &Emulator,
    assistant: &str,
    prompt: &str,
    body: Value,
) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/assistants/{}/aiprompts/{}/versions",
                label(assistant),
                label(prompt)
            ),
            Some(body),
        )
        .await
}

/// `ListAIPromptVersions` (`GET /assistants/{assistantId}/aiprompts/{aiPromptId}/versions`),
/// with `query` appended verbatim so a row can page and filter.
pub(crate) async fn list_versions(
    emulator: &Emulator,
    assistant: &str,
    prompt: &str,
    query: &str,
) -> Reply {
    emulator
        .send(
            "GET",
            &format!(
                "/assistants/{}/aiprompts/{}/versions{query}",
                label(assistant),
                label(prompt)
            ),
            None,
        )
        .await
}

/// `DeleteAIPrompt` (`DELETE /assistants/{assistantId}/aiprompts/{aiPromptId}`), which is
/// what releases a prompt and everything it published.
async fn delete_prompt(emulator: &Emulator, assistant: &str, prompt: &str) -> Reply {
    emulator
        .send(
            "DELETE",
            &format!(
                "/assistants/{}/aiprompts/{}",
                label(assistant),
                label(prompt)
            ),
            None,
        )
        .await
}

/// The `AIPromptData` a read of one VERSION must answer with: the record the working copy
/// carried when that version was published, with the one member a version renders
/// differently — the ARN names the version addressed.
fn versioned(record: &Value, version_number: i64) -> Value {
    at_qualifier(record, &version_number.to_string())
}

/// `record` as an operation addressed through a label carrying `qualifier` reports it: the
/// ARN names the revision addressed, which is a version's number or the working copy's
/// floating alias.
fn at_qualifier(record: &Value, qualifier: &str) -> Value {
    let mut expected = record.clone();
    let arn = record
        .get("aiPromptArn")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    expected["aiPromptArn"] = json!(format!("{arn}:{qualifier}"));
    expected
}

/// A create body for one row: every member a caller may send, with `description` left out
/// when the row carries none.
fn create_body(
    name: &str,
    prompt_type: &str,
    api_format: &str,
    visibility: &str,
    description: Option<&str>,
) -> Value {
    let mut body = required(name, prompt_type, api_format, visibility);
    if let Some(description) = description {
        body["description"] = json!(description);
    }
    body
}

/// Create one prompt and answer with (its id, the `AIPromptData` the create stored).
async fn created_prompt(
    emulator: &Emulator,
    assistant: &str,
    case: &str,
    body: Value,
) -> (String, Value) {
    let reply = create_prompt(emulator, assistant, body).await;
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the prompt this row versions must be created, got HTTP {} with body {}",
        reply.status, reply.body
    );
    let record = reply
        .field("/aiPrompt")
        .unwrap_or_else(|| {
            panic!("case {case:?}: the create answered no aiPrompt, body {}", reply.body)
        })
        .clone();
    let id = reply.string("/aiPrompt/aiPromptId");
    assert!(
        !id.is_empty(),
        "case {case:?}: the create answered no aiPromptId, body {}",
        reply.body
    );
    (id, record)
}

/// The `versionNumber`s one listing reports, in the order it reported them.
fn listed_numbers(case: &str, reply: &Reply) -> Vec<i64> {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: the listing must be served, got HTTP {} with body {}",
        reply.status, reply.body
    );
    reply
        .field("/aiPromptVersionSummaries")
        .and_then(Value::as_array)
        .unwrap_or_else(|| {
            panic!(
                "case {case:?}: the listing must report the required aiPromptVersionSummaries, \
                 body {}",
                reply.body
            )
        })
        .iter()
        .map(|summary| {
            summary
                .get("versionNumber")
                .and_then(Value::as_i64)
                .unwrap_or_else(|| {
                    panic!("case {case:?}: a summary must carry its versionNumber, got {summary}")
                })
        })
        .collect()
}

/// A refusal: the status this class carries, the modelled error shape and the sentence this
/// endpoint words that class with.
fn assert_refused(case: &str, reply: &Reply, status: u16, code: &str, message: &str) {
    assert_eq!(
        reply.status, status,
        "case {case:?}: expected HTTP {status}, got {} with body {}",
        reply.status, reply.body
    );
    assert!(
        reply.error_type.contains(code),
        "case {case:?}: expected the modelled {code}, got x-amzn-errortype {:?} with body {}",
        reply.error_type,
        reply.body
    );
    assert_eq!(
        reply.string("/message"),
        message,
        "case {case:?}: expected the sentence this endpoint words that refusal with, body {}",
        reply.body
    );
}

/// The publish answered 200 with `expected_number` and the snapshot `expected_record`,
/// answering with the data it reported.
fn assert_published(case: &str, reply: &Reply, expected_number: i64, expected_record: &Value) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: a publish of a prompt this account holds must be served, got HTTP {} \
         with body {}",
        reply.status, reply.body
    );
    assert_eq!(
        reply.field("/versionNumber").and_then(Value::as_i64),
        Some(expected_number),
        "case {case:?}: expected versionNumber {expected_number}, body {}",
        reply.body
    );
    assert_eq!(
        reply.field("/aiPrompt"),
        Some(&versioned(expected_record, expected_number)),
        "case {case:?}: the publish must answer with the working copy it snapshotted, under \
         the ARN of the version it published, body {}",
        reply.body
    );
}

#[tokio::test]
async fn publishing_a_version_numbers_it_from_one_and_snapshots_the_working_copy() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai prompt version publish").await;

    // The rows span the prompt's own axes: its type, the API format it speaks, its
    // visibility status and whether it carries a description at all.
    let rows: [(&str, &str, &str, &str, Option<&str>); 4] = [
        (
            "query reformulation, messages, published, described",
            PROMPT_TYPES[2],
            API_FORMATS[0],
            VISIBILITY_PUBLISHED,
            Some("original-description"),
        ),
        (
            "answer generation, messages, saved, described",
            PROMPT_TYPES[0],
            API_FORMATS[0],
            VISIBILITY_SAVED,
            Some("answer-generation-description"),
        ),
        (
            "email response, text completions, published, undescribed",
            PROMPT_TYPES[5],
            API_FORMATS[1],
            VISIBILITY_PUBLISHED,
            None,
        ),
        (
            "note taking, deprecated claude messages, saved, undescribed",
            PROMPT_TYPES[10],
            API_FORMATS[2],
            VISIBILITY_SAVED,
            None,
        ),
    ];

    for (index, (case, prompt_type, api_format, visibility, description)) in
        rows.into_iter().enumerate()
    {
        let (prompt, created) = created_prompt(
            &emulator,
            &assistant,
            case,
            create_body(
                &format!("version-row-{index}"),
                prompt_type,
                api_format,
                visibility,
                description,
            ),
        )
        .await;

        // The first version of a prompt is 1, and it reports the working copy as it stands.
        let first = publish(&emulator, &assistant, &prompt, json!({})).await;
        assert_published(case, &first, VERSION_MIN, &created);

        // The version is addressable as `<id>:<versionNumber>`, and that read reports the
        // number it answered for.
        let read = get_prompt(&emulator, &assistant, &format!("{prompt}:{VERSION_MIN}")).await;
        assert_eq!(
            read.status, 200,
            "case {case:?}: the published version must be readable, got HTTP {} with body {}",
            read.status, read.body
        );
        assert_eq!(
            read.field("/aiPrompt"),
            Some(&versioned(&created, VERSION_MIN)),
            "case {case:?}: a read of the version must report what was published under it, \
             body {}",
            read.body
        );
        assert_eq!(
            read.field("/versionNumber").and_then(Value::as_i64),
            Some(VERSION_MIN),
            "case {case:?}: a version-addressed read must report its versionNumber, body {}",
            read.body
        );

        // A write to the working copy leaves the published version as it was, and the next
        // publish is the previous number plus 1.
        let updated = update_prompt(
            &emulator,
            &assistant,
            &prompt,
            json!({
                "visibilityStatus": VISIBILITY_PUBLISHED,
                "description": "updated-description",
                "modelId": MODEL_IDS[1],
                "templateConfiguration": template("system: updated\nmessages: []\n"),
            }),
        )
        .await;
        assert_eq!(
            updated.status, 200,
            "case {case:?}: the update this row layers must be served, got HTTP {} with body {}",
            updated.status, updated.body
        );
        let working_copy = updated
            .field("/aiPrompt")
            .unwrap_or_else(|| {
                panic!("case {case:?}: the update answered no aiPrompt, body {}", updated.body)
            })
            .clone();

        let unchanged = get_prompt(&emulator, &assistant, &format!("{prompt}:{VERSION_MIN}")).await;
        assert_eq!(
            unchanged.field("/aiPrompt"),
            Some(&versioned(&created, VERSION_MIN)),
            "case {case:?}: a published version must be immutable under a later update, body {}",
            unchanged.body
        );

        let second = publish(&emulator, &assistant, &prompt, json!({})).await;
        assert_published(case, &second, VERSION_MIN + 1, &working_copy);

        // The working copy itself is read under the floating alias and reports no version
        // number: the publish changed nothing about it.
        let latest = get_prompt(&emulator, &assistant, &prompt).await;
        assert_eq!(
            latest.field("/aiPrompt"),
            Some(&at_qualifier(&working_copy, QUALIFIER_LATEST)),
            "case {case:?}: the working copy must still report the update's own record, body {}",
            latest.body
        );
        assert_eq!(
            latest.field("/versionNumber"),
            None,
            "case {case:?}: a read of the working copy answers for no version, body {}",
            latest.body
        );

        // Both versions are listed, in ascending order, and each summary reports the
        // snapshot it was taken of.
        let listed = list_versions(&emulator, &assistant, &prompt, "").await;
        assert_eq!(
            listed_numbers(case, &listed),
            vec![VERSION_MIN, VERSION_MIN + 1],
            "case {case:?}: the listing must report every published version in ascending \
             order, body {}",
            listed.body
        );
        for (position, expected) in [(0usize, &created), (1usize, &working_copy)] {
            let summary = listed
                .field(&format!("/aiPromptVersionSummaries/{position}/aiPromptSummary"))
                .unwrap_or_else(|| {
                    panic!(
                        "case {case:?}: summary {position} must carry its aiPromptSummary, \
                         body {}",
                        listed.body
                    )
                });
            let number = (position as i64) + VERSION_MIN;
            for member in [
                "name",
                "type",
                "templateType",
                "modelId",
                "apiFormat",
                "visibilityStatus",
                "description",
                "status",
                "origin",
                "modifiedTime",
                "assistantId",
                "assistantArn",
                "aiPromptId",
            ] {
                assert_eq!(
                    summary.get(member),
                    expected.get(member),
                    "case {case:?}: summary of version {number} must report the snapshot's \
                     {member}, got {summary}"
                );
            }
            let arn = expected
                .get("aiPromptArn")
                .and_then(Value::as_str)
                .unwrap_or_default();
            assert_eq!(
                summary.get("aiPromptArn").and_then(Value::as_str),
                Some(format!("{arn}:{number}").as_str()),
                "case {case:?}: summary of version {number} must name the version it \
                 summarises, got {summary}"
            );
        }
    }
}

#[tokio::test]
async fn publishes_that_overlap_are_handed_distinct_version_numbers() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai prompt version race").await;
    let (prompt, _) = created_prompt(
        &emulator,
        &assistant,
        "race",
        create_body(
            "race-prompt",
            PROMPT_TYPES[2],
            API_FORMATS[0],
            VISIBILITY_PUBLISHED,
            Some("race-description"),
        ),
    )
    .await;

    // Five publishes of ONE prompt, driven TOGETHER so their futures interleave — what five
    // SDK clients publishing at once do.
    let racers = tokio::join!(
        publish(&emulator, &assistant, &prompt, json!({})),
        publish(&emulator, &assistant, &prompt, json!({})),
        publish(&emulator, &assistant, &prompt, json!({})),
        publish(&emulator, &assistant, &prompt, json!({})),
        publish(&emulator, &assistant, &prompt, json!({})),
    );
    let replies = [racers.0, racers.1, racers.2, racers.3, racers.4];
    let mut numbers: Vec<i64> = Vec::new();
    for (index, reply) in replies.iter().enumerate() {
        assert_eq!(
            reply.status, 200,
            "case {:?}: every racer must be served, racer {index} got HTTP {} with body {}",
            "five overlapping publishes", reply.status, reply.body
        );
        let number = reply
            .field("/versionNumber")
            .and_then(Value::as_i64)
            .unwrap_or_else(|| {
                panic!(
                    "case {:?}: racer {index} answered no versionNumber, body {}",
                    "five overlapping publishes", reply.body
                )
            });
        assert!(
            number >= VERSION_MIN,
            "case {:?}: racer {index} must be handed a version number of at least \
             {VERSION_MIN}, got {number}",
            "five overlapping publishes"
        );
        numbers.push(number);
    }
    let mut sorted = numbers.clone();
    sorted.sort_unstable();
    sorted.dedup();
    assert_eq!(
        sorted.len(),
        numbers.len(),
        "case {:?}: no two racers may be handed one version number, got {numbers:?}",
        "five overlapping publishes"
    );
    assert_eq!(
        sorted,
        (VERSION_MIN..VERSION_MIN + numbers.len() as i64).collect::<Vec<i64>>(),
        "case {:?}: the numbers handed out must be the next ones in sequence, got {numbers:?}",
        "five overlapping publishes"
    );

    // Every number a racer was answered with names a version the listing reports.
    let listed = list_versions(&emulator, &assistant, &prompt, "").await;
    assert_eq!(
        listed_numbers("five overlapping publishes", &listed),
        sorted,
        "case {:?}: the listing must report every version a racer was answered with, body {}",
        "five overlapping publishes",
        listed.body
    );
}

#[tokio::test]
async fn the_listing_pages_and_filters_the_versions_a_prompt_holds() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai prompt version listing").await;
    let (prompt, _) = created_prompt(
        &emulator,
        &assistant,
        "listing",
        create_body(
            "listing-prompt",
            PROMPT_TYPES[4],
            API_FORMATS[0],
            VISIBILITY_SAVED,
            Some("listing-description"),
        ),
    )
    .await;

    // A prompt that has published nothing lists nothing — not a refusal.
    let empty = list_versions(&emulator, &assistant, &prompt, "").await;
    assert_eq!(
        listed_numbers("no version published yet", &empty),
        Vec::<i64>::new(),
        "case {:?}: a prompt that published nothing must list nothing, body {}",
        "no version published yet",
        empty.body
    );

    let published = 3;
    for expected_number in VERSION_MIN..VERSION_MIN + published {
        let reply = publish(&emulator, &assistant, &prompt, json!({})).await;
        assert_eq!(
            reply.field("/versionNumber").and_then(Value::as_i64),
            Some(expected_number),
            "case {:?}: expected versionNumber {expected_number}, body {}",
            "listing",
            reply.body
        );
    }

    // One version per page: every page carries exactly one summary, the pages walk the
    // versions in ascending order, and the last page carries no token.
    let mut walked: Vec<i64> = Vec::new();
    let mut query = "?maxResults=1".to_string();
    for _ in 0..published + 1 {
        let page = list_versions(&emulator, &assistant, &prompt, &query).await;
        let numbers = listed_numbers("one version per page", &page);
        assert!(
            numbers.len() <= 1,
            "case {:?}: a page of one must carry at most one summary, got {numbers:?}",
            "one version per page"
        );
        walked.extend(numbers);
        match page.field("/nextToken").and_then(Value::as_str) {
            Some(token) => query = format!("?maxResults=1&nextToken={}", label(token)),
            None => break,
        }
    }
    assert_eq!(
        walked,
        (VERSION_MIN..VERSION_MIN + published).collect::<Vec<i64>>(),
        "case {:?}: paging must walk every version once, in ascending order, got {walked:?}",
        "one version per page"
    );

    // `origin` selects by the origin of the prompt the snapshot was taken of: these are a
    // caller's, so the CUSTOMER filter reports them all and the SYSTEM filter none.
    let customer = list_versions(
        &emulator,
        &assistant,
        &prompt,
        &format!("?origin={ORIGIN_CUSTOMER}"),
    )
    .await;
    assert_eq!(
        listed_numbers("origin CUSTOMER", &customer),
        (VERSION_MIN..VERSION_MIN + published).collect::<Vec<i64>>(),
        "case {:?}: versions of a prompt a caller created must pass the CUSTOMER filter, \
         body {}",
        "origin CUSTOMER",
        customer.body
    );
    let system = list_versions(
        &emulator,
        &assistant,
        &prompt,
        &format!("?origin={ORIGIN_SYSTEM}"),
    )
    .await;
    assert_eq!(
        listed_numbers("origin SYSTEM", &system),
        Vec::<i64>::new(),
        "case {:?}: no version of a prompt a caller created may pass the SYSTEM filter, \
         body {}",
        "origin SYSTEM",
        system.body
    );

    // A token this service did not mint names no position.
    let bad_token = list_versions(&emulator, &assistant, &prompt, "?nextToken=not-a-cursor").await;
    assert_refused(
        "a nextToken this service did not mint",
        &bad_token,
        400,
        CODE_VALIDATION,
        MESSAGE_BAD_TOKEN,
    );
}

#[tokio::test]
async fn a_modified_time_precondition_refuses_a_publish_of_a_working_copy_that_moved_on() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai prompt version precondition").await;
    let (prompt, created) = created_prompt(
        &emulator,
        &assistant,
        "precondition",
        create_body(
            "precondition-prompt",
            PROMPT_TYPES[2],
            API_FORMATS[0],
            VISIBILITY_SAVED,
            Some("precondition-description"),
        ),
    )
    .await;
    let created_modified_time = created
        .get("modifiedTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("the create must report a modifiedTime, got {created}"));

    // The precondition the caller read off the create still holds, so the publish is served.
    let first = publish(
        &emulator,
        &assistant,
        &prompt,
        json!({ "modifiedTime": created_modified_time }),
    )
    .await;
    assert_published("a precondition that holds", &first, VERSION_MIN, &created);

    // A precondition naming an instant BEFORE the prompt was last written does not hold.
    let stale = publish(
        &emulator,
        &assistant,
        &prompt,
        json!({ "modifiedTime": created_modified_time - ONE_DAY_SECONDS }),
    )
    .await;
    assert_refused(
        "a precondition a day older than the record",
        &stale,
        409,
        CODE_CONFLICT,
        MESSAGE_MODIFIED_SINCE,
    );

    // An update moves the working copy on, so the create's own precondition no longer holds
    // either; the one the UPDATE reported does.
    let updated = update_prompt(
        &emulator,
        &assistant,
        &prompt,
        json!({
            "visibilityStatus": VISIBILITY_PUBLISHED,
            "description": "precondition-updated",
        }),
    )
    .await;
    assert_eq!(
        updated.status, 200,
        "case {:?}: the update must be served, got HTTP {} with body {}",
        "precondition", updated.status, updated.body
    );
    let working_copy = updated
        .field("/aiPrompt")
        .unwrap_or_else(|| panic!("the update must answer an aiPrompt, body {}", updated.body))
        .clone();
    let updated_modified_time = working_copy
        .get("modifiedTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("the update must report a modifiedTime, got {working_copy}"));
    let after_update = publish(
        &emulator,
        &assistant,
        &prompt,
        json!({ "modifiedTime": updated_modified_time }),
    )
    .await;
    assert_published(
        "a precondition stating the instant the update reported",
        &after_update,
        VERSION_MIN + 1,
        &working_copy,
    );

    // Only what the preconditions allowed was published.
    let listed = list_versions(&emulator, &assistant, &prompt, "").await;
    assert_eq!(
        listed_numbers("precondition", &listed),
        vec![VERSION_MIN, VERSION_MIN + 1],
        "case {:?}: a refused publish must number nothing, body {}",
        "precondition",
        listed.body
    );
}

#[tokio::test]
async fn a_replayed_client_token_answers_with_the_version_it_already_published() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai prompt version idempotency").await;
    let (prompt, created) = created_prompt(
        &emulator,
        &assistant,
        "idempotency",
        create_body(
            "idempotency-prompt",
            PROMPT_TYPES[2],
            API_FORMATS[0],
            VISIBILITY_PUBLISHED,
            Some("idempotency-description"),
        ),
    )
    .await;
    let modified_time = created
        .get("modifiedTime")
        .and_then(Value::as_f64)
        .unwrap_or_else(|| panic!("the create must report a modifiedTime, got {created}"));
    let token = "ai-prompt-version-token";

    let first = publish(
        &emulator,
        &assistant,
        &prompt,
        json!({ "clientToken": token }),
    )
    .await;
    assert_published("a fresh token", &first, VERSION_MIN, &created);

    // The same token carrying the same request numbers nothing a second time.
    let replay = publish(
        &emulator,
        &assistant,
        &prompt,
        json!({ "clientToken": token }),
    )
    .await;
    assert_published("the same token replayed", &replay, VERSION_MIN, &created);

    // The same token carrying a DIFFERENT request is the modelled idempotency conflict.
    let conflicting = publish(
        &emulator,
        &assistant,
        &prompt,
        json!({ "clientToken": token, "modifiedTime": modified_time }),
    )
    .await;
    assert_refused(
        "the same token with another request",
        &conflicting,
        409,
        CODE_CONFLICT,
        MESSAGE_IDEMPOTENCY_CONFLICT,
    );

    // A token of its own publishes the next version.
    let other = publish(
        &emulator,
        &assistant,
        &prompt,
        json!({ "clientToken": "ai-prompt-version-other-token" }),
    )
    .await;
    assert_published("a token of its own", &other, VERSION_MIN + 1, &created);
    let listed = list_versions(&emulator, &assistant, &prompt, "").await;
    assert_eq!(
        listed_numbers("idempotency", &listed),
        vec![VERSION_MIN, VERSION_MIN + 1],
        "case {:?}: a replay must publish no second version, body {}",
        "idempotency",
        listed.body
    );
}

#[tokio::test]
async fn a_label_naming_no_prompt_of_this_assistant_publishes_and_lists_nothing() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai prompt version refusals").await;
    let other_assistant = assistant_id(&emulator, "ai prompt version refusals other").await;
    let (prompt, _) = created_prompt(
        &emulator,
        &assistant,
        "refusals",
        create_body(
            "refusals-prompt",
            PROMPT_TYPES[2],
            API_FORMATS[0],
            VISIBILITY_PUBLISHED,
            Some("refusals-description"),
        ),
    )
    .await;
    let published = publish(&emulator, &assistant, &prompt, json!({})).await;
    assert_eq!(
        published.status, 200,
        "case {:?}: the version this suite reads back must be published, got HTTP {} with \
         body {}",
        "refusals", published.status, published.body
    );

    // An assistant this account does not hold, for the publish and for the listing.
    assert_refused(
        "a publish naming an absent assistant",
        &publish(&emulator, ABSENT_UUID, &prompt, json!({})).await,
        404,
        CODE_NOT_FOUND,
        MESSAGE_ASSISTANT_NOT_FOUND,
    );
    assert_refused(
        "a listing naming an absent assistant",
        &list_versions(&emulator, ABSENT_UUID, &prompt, "").await,
        404,
        CODE_NOT_FOUND,
        MESSAGE_ASSISTANT_NOT_FOUND,
    );

    // A prompt that assistant does not hold, and a prompt of ANOTHER assistant.
    assert_refused(
        "a publish naming an absent prompt",
        &publish(&emulator, &assistant, ABSENT_UUID, json!({})).await,
        404,
        CODE_NOT_FOUND,
        MESSAGE_AI_PROMPT_NOT_FOUND,
    );
    assert_refused(
        "a publish of a prompt of another assistant",
        &publish(&emulator, &other_assistant, &prompt, json!({})).await,
        404,
        CODE_NOT_FOUND,
        MESSAGE_AI_PROMPT_NOT_FOUND,
    );
    assert_refused(
        "a listing of a prompt of another assistant",
        &list_versions(&emulator, &other_assistant, &prompt, "").await,
        404,
        CODE_NOT_FOUND,
        MESSAGE_AI_PROMPT_NOT_FOUND,
    );

    // The working copy is what publishes and what publishes versions: a label naming a
    // published VERSION names a snapshot, which is neither.
    assert_refused(
        "a publish addressed to a published version",
        &publish(
            &emulator,
            &assistant,
            &format!("{prompt}:{VERSION_MIN}"),
            json!({}),
        )
        .await,
        404,
        CODE_NOT_FOUND,
        MESSAGE_AI_PROMPT_NOT_FOUND,
    );
    assert_refused(
        "a listing addressed to a published version",
        &list_versions(
            &emulator,
            &assistant,
            &format!("{prompt}:{VERSION_MIN}"),
            "",
        )
        .await,
        404,
        CODE_NOT_FOUND,
        MESSAGE_AI_PROMPT_NOT_FOUND,
    );

    // A version number nothing published, and a qualifier that names no version at all.
    for qualifier in [
        VERSION_NEVER_PUBLISHED.to_string(),
        "NOT_A_VERSION".to_string(),
    ] {
        assert_refused(
            &format!("a read of the qualifier {qualifier:?}"),
            &get_prompt(&emulator, &assistant, &format!("{prompt}:{qualifier}")).await,
            404,
            CODE_NOT_FOUND,
            MESSAGE_AI_PROMPT_NOT_FOUND,
        );
    }

    // A prompt that is gone takes its versions with it.
    let deleted = delete_prompt(&emulator, &assistant, &prompt).await;
    assert_eq!(
        deleted.status, 204,
        "case {:?}: deleting the prompt must answer HTTP 204, got {} with body {}",
        "refusals", deleted.status, deleted.body
    );
    assert_refused(
        "a listing of a prompt that is gone",
        &list_versions(&emulator, &assistant, &prompt, "").await,
        404,
        CODE_NOT_FOUND,
        MESSAGE_AI_PROMPT_NOT_FOUND,
    );
    assert_refused(
        "a read of a version of a prompt that is gone",
        &get_prompt(&emulator, &assistant, &format!("{prompt}:{VERSION_MIN}")).await,
        404,
        CODE_NOT_FOUND,
        MESSAGE_AI_PROMPT_NOT_FOUND,
    );
}

#[tokio::test]
async fn a_prompt_holds_at_most_the_stores_version_ceiling() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai prompt version ceiling").await;
    let (prompt, _) = created_prompt(
        &emulator,
        &assistant,
        "ceiling",
        create_body(
            "ceiling-prompt",
            PROMPT_TYPES[2],
            API_FORMATS[0],
            VISIBILITY_PUBLISHED,
            Some("ceiling-description"),
        ),
    )
    .await;

    for expected_number in VERSION_MIN..=MAX_AI_PROMPT_VERSIONS_PER_PROMPT as i64 {
        let published = publish(&emulator, &assistant, &prompt, json!({})).await;
        assert_eq!(
            published.field("/versionNumber").and_then(Value::as_i64),
            Some(expected_number),
            "case {:?}: expected versionNumber {expected_number}, body {}",
            "ceiling",
            published.body
        );
    }
    assert_refused(
        "a publish past the ceiling",
        &publish(&emulator, &assistant, &prompt, json!({})).await,
        402,
        CODE_QUOTA,
        &format!(
            "The maximum number of AI Prompt versions ({MAX_AI_PROMPT_VERSIONS_PER_PROMPT}) for \
             this AI Prompt has been reached. Delete an AI Prompt version before creating \
             another."
        ),
    );
}
