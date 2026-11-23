// SPDX-License-Identifier: CC0-1.0

//! End-to-end tests for the Amazon Q in Connect AI-prompt slice (`CreateAIPrompt`,
//! `UpdateAIPrompt`, `DeleteAIPrompt` + `GetAIPrompt`), driven over HTTP through the same
//! router the emulator serves with.
//!
//! The rules under test, each asserted over a TABLE of inputs rather than the one recorded
//! request:
//!
//! * a create of a resource nested in an assistant PERSISTS every member the request
//!   carried under an id the service mints, and answers out of the stored record, so a
//!   later read — by id, by ARN, or by either carrying the working copy's version
//!   qualifier — reports exactly the same values, while a label naming no such resource of
//!   the assistant the route named is refused as not found;
//! * a MUTATION of that resource is what every later read of it reports: an update writes
//!   the working copy — the member it requires and every optional member it carries
//!   replace the stored values, a member it carries none of keeps the value it had, and
//!   the members the service owns are untouched — and a delete removes the record, so
//!   every later read, delete or update of it is that operation's own not-found and the
//!   assistant's ceiling counts only the prompts still live.
//!
//! The rows differ along every axis the rule generalises over:
//!
//! * every value of the modelled `AIPromptType` catalog, and every value of
//!   `AIPromptAPIFormat` (the two recommended ones and the two the model documents as
//!   being deprecated);
//! * both values the modelled `VisibilityStatus` declares;
//! * the published length bounds of `name`, `description`, `modelId` and the template's
//!   own text, and whether the optional members are sent at all;
//! * the inference configuration: absent, partial, and every parameter at each published
//!   bound of its range;
//! * the tag count (none, one, several);
//! * for a mutation: which optional members the update carries (none, one, all), in which
//!   direction the visibility status moves, how many updates are layered on one record,
//!   and which of the label forms a read resolves the update or the delete is addressed
//!   by.
//!
//! Every assertion goes over the wire — only the HTTP status, the `x-amzn-errortype`
//! header and the JSON body are inspected — so a response that is not computed from
//! persisted state cannot pass. The harness (the router, the reply reader and the
//! assistant create) is the one `resource_tags.rs` and `ai_guardrail.rs` already drive
//! this service with.

use serde_json::{json, Value};

use service_com_amazonaws_qconnect_wisdomservice::state::MAX_AI_PROMPTS_PER_ASSISTANT;

use super::ai_guardrail::{
    assert_not_found, assistant_id, QUALIFIER_LATEST, STATUS_ACTIVE, VISIBILITY_PUBLISHED,
    VISIBILITY_SAVED,
};
use super::resource_tags::{
    label, tag_map, wisdom_arn, Emulator, Reply, ABSENT_UUID, OTHER_ACCOUNT, OTHER_REGION, REGION,
};

/// The fixed sentence this endpoint answers a label naming no AI prompt of the assistant
/// the route named with, written out here rather than imported from the implementation so
/// the assertion pins the answer a client sees.
const MESSAGE_AI_PROMPT_NOT_FOUND: &str = "AI Prompt does not exist.";

/// The sentence a create whose `clientToken` replays a DIFFERENT request is refused with.
const MESSAGE_IDEMPOTENCY_CONFLICT: &str = "Idempotency conflict";

/// The `Origin` a prompt a caller created reports
/// (`smithy_query(query="shape",shape="Origin")` declares `SYSTEM` beside it).
const ORIGIN_CUSTOMER: &str = "CUSTOMER";

/// Every value the modelled `AIPromptType` enumeration declares
/// (`smithy_query(query="shape",shape="AIPromptType")`), written out here rather than
/// imported from the implementation so the table spans the whole catalog — including the
/// types no recording of this surface used.
pub(crate) const PROMPT_TYPES: [&str; 12] = [
    "ANSWER_GENERATION",
    "INTENT_LABELING_GENERATION",
    "QUERY_REFORMULATION",
    "SELF_SERVICE_PRE_PROCESSING",
    "SELF_SERVICE_ANSWER_GENERATION",
    "EMAIL_RESPONSE",
    "EMAIL_OVERVIEW",
    "EMAIL_GENERATIVE_ANSWER",
    "EMAIL_QUERY_REFORMULATION",
    "ORCHESTRATION",
    "NOTE_TAKING",
    "CASE_SUMMARIZATION",
];

/// Every value the modelled `AIPromptAPIFormat` enumeration declares
/// (`smithy_query(query="shape",shape="AIPromptAPIFormat")`): the two the model recommends
/// and the two it documents as being deprecated, which the service still accepts.
pub(crate) const API_FORMATS: [&str; 4] = [
    "MESSAGES",
    "TEXT_COMPLETIONS",
    "ANTHROPIC_CLAUDE_MESSAGES",
    "ANTHROPIC_CLAUDE_TEXT_COMPLETIONS",
];

/// The only value the modelled `AIPromptTemplateType` enumeration declares
/// (`smithy_query(query="shape",shape="AIPromptTemplateType")`).
const TEMPLATE_TYPE_TEXT: &str = "TEXT";

/// Model identifiers from the catalog `AIPromptData$modelId` documents
/// (`smithy_query(query="doc",shape="AIPromptData",member="modelId")` lists seventeen),
/// including entries no recording of this surface used.
pub(crate) const MODEL_IDS: [&str; 4] = [
    "anthropic.claude-3-haiku--v1:0",
    "us.amazon.nova-pro-v1:0",
    "eu.anthropic.claude-3-7-sonnet-20250219-v1:0",
    "apac.anthropic.claude-3-5-sonnet--v2:0",
];

/// The published bounds of the members this suite varies in length
/// (`smithy_query(query="operation",operation="CreateAIPrompt")`: `name` is `len 1..255`,
/// `description` `len 1..255` and `modelId` `len 1..2048`;
/// `smithy_query(query="shape",shape="TextFullAIPromptEditTemplateConfiguration")`: the
/// template's `text` is `len 1..1000000`).
const NAME_MIN: usize = 1;
const NAME_MAX: usize = 255;
const DESCRIPTION_MIN: usize = 1;
const DESCRIPTION_MAX: usize = 255;
const MODEL_ID_MIN: usize = 1;
const MODEL_ID_MAX: usize = 2048;
const TEMPLATE_TEXT_MIN: usize = 1;
const TEMPLATE_TEXT_MAX: usize = 1_000_000;

/// The published bounds of every `AIPromptInferenceConfiguration` parameter's range
/// (`smithy_query(query="shape",shape="AIPromptInferenceConfiguration")`: `temperature`
/// and `topP` are `range 0..1`, `topK` `range 0..200` and `maxTokensToSample`
/// `range 1..4096`).
const PROBABILITY_MIN: f64 = 0.0;
const PROBABILITY_MAX: f64 = 1.0;
const TOP_K_MIN: i64 = 0;
const TOP_K_MAX: i64 = 200;
const MAX_TOKENS_MIN: i64 = 1;
const MAX_TOKENS_MAX: i64 = 4096;

/// `CreateAIPrompt` on one assistant, as the model's URI binds it
/// (`POST /assistants/{assistantId}/aiprompts`).
pub(crate) async fn create_prompt(emulator: &Emulator, assistant: &str, body: Value) -> Reply {
    emulator
        .send(
            "POST",
            &format!("/assistants/{}/aiprompts", label(assistant)),
            Some(body),
        )
        .await
}

/// `GetAIPrompt` (`GET /assistants/{assistantId}/aiprompts/{aiPromptId}`).
pub(crate) async fn get_prompt(emulator: &Emulator, assistant: &str, prompt: &str) -> Reply {
    emulator
        .send(
            "GET",
            &format!(
                "/assistants/{}/aiprompts/{}",
                label(assistant),
                label(prompt)
            ),
            None,
        )
        .await
}

/// A YAML prompt template, as the member's own documentation describes it ("The YAML text
/// for the AI Prompt template").
pub(crate) fn template(text: &str) -> Value {
    json!({ "textFullAIPromptEditTemplateConfiguration": { "text": text } })
}

/// `UpdateAIPrompt` (`POST /assistants/{assistantId}/aiprompts/{aiPromptId}`).
pub(crate) async fn update_prompt(
    emulator: &Emulator,
    assistant: &str,
    prompt: &str,
    body: Value,
) -> Reply {
    emulator
        .send(
            "POST",
            &format!(
                "/assistants/{}/aiprompts/{}",
                label(assistant),
                label(prompt)
            ),
            Some(body),
        )
        .await
}

/// `DeleteAIPrompt` (`DELETE /assistants/{assistantId}/aiprompts/{aiPromptId}`).
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

/// The required members of every create in this suite, so a row states only what it
/// varies (`smithy_query(query="operation",operation="CreateAIPrompt")` marks `name`,
/// `type`, `templateConfiguration`, `visibilityStatus`, `templateType`, `modelId` and
/// `apiFormat` required).
pub(crate) fn required(
    name: &str,
    prompt_type: &str,
    api_format: &str,
    visibility: &str,
) -> Value {
    json!({
        "name": name,
        "type": prompt_type,
        "apiFormat": api_format,
        "visibilityStatus": visibility,
        "templateType": TEMPLATE_TYPE_TEXT,
        "modelId": MODEL_IDS[0],
        "templateConfiguration": template("system: You are a helpful assistant.\nmessages: []\n"),
    })
}

/// One row of the create table: what it varies, and the request body it sends.
#[derive(Debug, Clone)]
struct Case {
    label: String,
    body: Value,
}

/// The create bodies the round-trip rule is asserted over. Each row differs from the
/// others along one axis of the rule — the prompt type, the API format, the visibility
/// status, a published length bound, an optional member, the inference configuration, the
/// tag count.
fn cases() -> Vec<Case> {
    let mut cases: Vec<Case> = Vec::new();
    let mut with = |label: String, mut body: Value, members: Vec<(&str, Value)>| {
        for (member, value) in members {
            body[member] = value;
        }
        cases.push(Case { label, body });
    };

    // One row per value of the `AIPromptType` catalog, each paired with a different API
    // format, model id and visibility status, so the table spans all four catalogs.
    for (index, prompt_type) in PROMPT_TYPES.iter().enumerate() {
        let visibility = if index % 2 == 0 {
            VISIBILITY_SAVED
        } else {
            VISIBILITY_PUBLISHED
        };
        with(
            format!("type {prompt_type}, only the required members"),
            required(
                &format!("prompt of type {prompt_type}"),
                prompt_type,
                API_FORMATS[index % API_FORMATS.len()],
                visibility,
            ),
            vec![("modelId", json!(MODEL_IDS[index % MODEL_IDS.len()]))],
        );
    }

    // One row per value of the `AIPromptAPIFormat` catalog, against one type, so a format
    // is covered on its own axis as well as paired above.
    for api_format in API_FORMATS {
        with(
            format!("apiFormat {api_format}"),
            required(
                &format!("prompt in format {api_format}"),
                PROMPT_TYPES[0],
                api_format,
                VISIBILITY_PUBLISHED,
            ),
            Vec::new(),
        );
    }

    // The published length bounds of the members a create carries by value.
    for (what, length) in [
        ("name at its published minimum length", NAME_MIN),
        ("name at its published maximum length", NAME_MAX),
    ] {
        with(
            format!("{what} ({length})"),
            required(
                &"n".repeat(length),
                PROMPT_TYPES[1],
                API_FORMATS[0],
                VISIBILITY_SAVED,
            ),
            Vec::new(),
        );
    }
    for (what, length) in [
        ("description at its published minimum length", DESCRIPTION_MIN),
        ("description at its published maximum length", DESCRIPTION_MAX),
    ] {
        with(
            format!("{what} ({length})"),
            required(
                &format!("described in {length}"),
                PROMPT_TYPES[2],
                API_FORMATS[1],
                VISIBILITY_SAVED,
            ),
            vec![("description", json!("d".repeat(length)))],
        );
    }
    for (what, length) in [
        ("modelId at its published minimum length", MODEL_ID_MIN),
        ("modelId at its published maximum length", MODEL_ID_MAX),
    ] {
        with(
            format!("{what} ({length})"),
            required(
                &format!("modelled in {length}"),
                PROMPT_TYPES[3],
                API_FORMATS[2],
                VISIBILITY_PUBLISHED,
            ),
            vec![("modelId", json!("m".repeat(length)))],
        );
    }

    // The inference configuration: every parameter at each published bound of its range,
    // and a configuration that carries only one of them.
    with(
        "an inference configuration with every parameter at its published minimum"
            .to_string(),
        required(
            "inference at minimum",
            PROMPT_TYPES[4],
            API_FORMATS[0],
            VISIBILITY_SAVED,
        ),
        vec![(
            "inferenceConfiguration",
            json!({
                "temperature": PROBABILITY_MIN,
                "topP": PROBABILITY_MIN,
                "topK": TOP_K_MIN,
                "maxTokensToSample": MAX_TOKENS_MIN,
            }),
        )],
    );
    with(
        "an inference configuration with every parameter at its published maximum"
            .to_string(),
        required(
            "inference at maximum",
            PROMPT_TYPES[5],
            API_FORMATS[1],
            VISIBILITY_PUBLISHED,
        ),
        vec![(
            "inferenceConfiguration",
            json!({
                "temperature": PROBABILITY_MAX,
                "topP": PROBABILITY_MAX,
                "topK": TOP_K_MAX,
                "maxTokensToSample": MAX_TOKENS_MAX,
            }),
        )],
    );
    with(
        "an inference configuration carrying one parameter only".to_string(),
        required(
            "inference partial",
            PROMPT_TYPES[6],
            API_FORMATS[2],
            VISIBILITY_SAVED,
        ),
        vec![("inferenceConfiguration", json!({ "temperature": 0.25 }))],
    );

    // The tag count, and one row carrying every optional member at once.
    for count in [1_usize, 3] {
        with(
            format!("{count} tag(s)"),
            required(
                &format!("tagged with {count}"),
                PROMPT_TYPES[7],
                API_FORMATS[3],
                VISIBILITY_SAVED,
            ),
            vec![("tags", json!(tag_map(count)))],
        );
    }
    with(
        "every optional member at once".to_string(),
        required(
            "everything at once",
            PROMPT_TYPES[8],
            API_FORMATS[0],
            VISIBILITY_PUBLISHED,
        ),
        vec![
            ("description", json!("Answers questions, politely.")),
            ("tags", json!(tag_map(2))),
            (
                "inferenceConfiguration",
                json!({ "topK": 50, "maxTokensToSample": 512 }),
            ),
            (
                "templateConfiguration",
                template(
                    "system: |\n  You answer questions.\nmessages:\n  - role: user\n    \
                     content: '{{question}}'\n",
                ),
            ),
        ],
    );

    cases
}

/// Every member the request carried is reported back unchanged, whichever answer is being
/// read — the create's own or a later read's.
fn assert_reports_request(case: &str, what: &str, body: &Value, reply: &Reply) {
    let members = body
        .as_object()
        .unwrap_or_else(|| panic!("case {case:?}: the request body must be an object"));
    for (member, sent) in members {
        if member == "clientToken" {
            continue;
        }
        let got = reply.field(&format!("/aiPrompt/{member}"));
        assert_eq!(
            got,
            Some(sent),
            "case {case:?}: {what} must report the {member:?} the request carried, expected \
             {sent}, got {got:?}"
        );
    }
}

/// A read answers with the `AIPromptData` the create stored, member for member — with the
/// one member a read renders differently: the ARN it reports names the version it answered
/// for, which for the prompt's working copy is the floating alias.
fn assert_reads_the_create(case: &str, what: &str, created: &Value, reply: &Reply) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: {what} must be served, got HTTP {} with body {}",
        reply.status, reply.body
    );
    let mut expected = created.clone();
    let stored_arn = expected
        .get("aiPromptArn")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    expected["aiPromptArn"] = json!(format!("{stored_arn}:{QUALIFIER_LATEST}"));
    let read = reply.field("/aiPrompt");
    assert_eq!(
        read,
        Some(&expected),
        "case {case:?}: {what} must answer with the record the create stored, expected \
         {expected}, got {read:?}"
    );
}

/// Whether `value` has the form the modelled `Uuid` shape declares
/// (`smithy_query(query="shape",shape="Uuid")`:
/// `^[a-f0-9]{8}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{4}-[a-f0-9]{12}$`).
fn is_uuid_shaped(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && groups
            .iter()
            .zip([8, 4, 4, 4, 12])
            .all(|(group, expected)| {
                group.len() == expected
                    && group.bytes().all(|byte| byte.is_ascii_hexdigit())
                    && group.bytes().all(|byte| !byte.is_ascii_uppercase())
            })
}

/// The ARN AWS documents for an AI prompt of this service, written out here rather than
/// taken from the handler's helper so the test pins the format the modelled `Arn` pattern
/// describes: the prompt's own id LAST, behind its parent assistant's.
fn expected_prompt_arn(assistant_id: &str, prompt_id: &str) -> String {
    wisdom_arn(
        "aws",
        REGION,
        engine::EMULATOR_ACCOUNT_ID,
        &format!("ai-prompt/{assistant_id}/{prompt_id}"),
    )
}

/// The ARN a read of one VERSION of a prompt reports: the prompt's own ARN with the
/// qualifier of the version read appended.
fn expected_read_arn(assistant_id: &str, prompt_id: &str, qualifier: &str) -> String {
    format!(
        "{}:{qualifier}",
        expected_prompt_arn(assistant_id, prompt_id)
    )
}

#[tokio::test]
async fn a_created_prompt_is_reported_out_of_the_stored_record_by_every_later_read() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt round trip").await;

    // Everything each row's create answered, so the reads below can be compared against
    // it rather than against the request alone.
    let mut created: Vec<(Case, String, Value)> = Vec::new();
    for case in cases() {
        let reply = create_prompt(&emulator, &assistant, case.body.clone()).await;
        assert_eq!(
            reply.status, 200,
            "case {:?}: the create must be served, got HTTP {} with body {}",
            case.label, reply.status, reply.body
        );
        assert_reports_request(&case.label, "the create", &case.body, &reply);

        let prompt_id = reply.string("/aiPrompt/aiPromptId");
        assert!(
            is_uuid_shaped(&prompt_id),
            "case {:?}: the minted aiPromptId must have the modelled Uuid form, got \
             {prompt_id:?}",
            case.label
        );
        let prompt_arn = reply.string("/aiPrompt/aiPromptArn");
        assert_eq!(
            prompt_arn,
            expected_prompt_arn(&assistant, &prompt_id),
            "case {:?}: the prompt must report the ARN of this account's prompt in {REGION}, \
             got {prompt_arn:?}",
            case.label
        );
        assert_eq!(
            reply.string("/aiPrompt/assistantId"),
            assistant,
            "case {:?}: the prompt must report the assistant the route named, body {}",
            case.label,
            reply.body
        );
        assert_eq!(
            reply.string("/aiPrompt/assistantArn"),
            wisdom_arn(
                "aws",
                REGION,
                engine::EMULATOR_ACCOUNT_ID,
                &format!("assistant/{assistant}"),
            ),
            "case {:?}: the prompt must report its parent assistant's own ARN, body {}",
            case.label,
            reply.body
        );
        assert_eq!(
            reply.string("/aiPrompt/status"),
            STATUS_ACTIVE,
            "case {:?}: a created prompt is in service, body {}",
            case.label,
            reply.body
        );
        assert_eq!(
            reply.string("/aiPrompt/origin"),
            ORIGIN_CUSTOMER,
            "case {:?}: a prompt a caller created is of CUSTOMER origin, body {}",
            case.label,
            reply.body
        );
        let modified = reply.field("/aiPrompt/modifiedTime").cloned();
        assert!(
            modified.as_ref().is_some_and(Value::is_number),
            "case {:?}: the prompt must report when it was last modified, got {modified:?}",
            case.label
        );
        // The `tags` member travels for every resource this service reports, as an empty
        // object when nothing tagged it.
        let tags = reply.field("/aiPrompt/tags").cloned();
        assert!(
            tags.as_ref().is_some_and(Value::is_object),
            "case {:?}: the prompt must report a tags map, got {tags:?}",
            case.label
        );
        // A prompt created without inference parameters reports none, rather than a
        // structure the caller never sent.
        if case.body.get("inferenceConfiguration").is_none() {
            assert!(
                reply.field("/aiPrompt/inferenceConfiguration").is_none(),
                "case {:?}: a prompt created with no inferenceConfiguration reports none, \
                 body {}",
                case.label,
                reply.body
            );
        }

        let data = reply
            .field("/aiPrompt")
            .cloned()
            .unwrap_or_else(|| panic!("case {:?}: the create answered no aiPrompt", case.label));
        created.push((case, prompt_id, data));
    }

    // Every id the creates minted is distinct, so one row's record is never another's.
    for (index, (case, prompt_id, _)) in created.iter().enumerate() {
        for (other, other_id, _) in created.iter().skip(index + 1) {
            assert_ne!(
                prompt_id, other_id,
                "case {:?}: two creates must be two records, but {:?} got the same id \
                 {prompt_id:?}",
                case.label, other.label
            );
        }
    }

    // A read by id, by ARN, and by either carrying the working copy's version qualifier,
    // answer with the record the create stored.
    for (case, prompt_id, data) in &created {
        for addressed_by in [
            prompt_id.clone(),
            expected_prompt_arn(&assistant, prompt_id),
            format!("{prompt_id}:{QUALIFIER_LATEST}"),
            expected_read_arn(&assistant, prompt_id, QUALIFIER_LATEST),
        ] {
            let reply = get_prompt(&emulator, &assistant, &addressed_by).await;
            assert_eq!(
                reply.status, 200,
                "case {:?}: a read addressed by {addressed_by:?} must be served, got HTTP {} \
                 with body {}",
                case.label, reply.status, reply.body
            );
            assert_reads_the_create(&case.label, &addressed_by, data, &reply);
            assert_reports_request(&case.label, "a later read", &case.body, &reply);
            // The model: the version number is reported only when the request's
            // `aiPromptId` carried a version qualifier naming a published version, which
            // none of these did.
            assert!(
                reply.field("/versionNumber").is_none(),
                "case {:?}: a read of the prompt itself reports no versionNumber, body {}",
                case.label,
                reply.body
            );
        }
    }

    // A second read of an unchanged prompt answers alike: reading one changes nothing.
    for (case, prompt_id, data) in &created {
        let reply = get_prompt(&emulator, &assistant, prompt_id).await;
        assert_reads_the_create(&case.label, "a repeated read", data, &reply);
    }

    // The assistant the route names is also read through its ARN, which names the same
    // parent: the records are reachable either way.
    let assistant_arn = wisdom_arn(
        "aws",
        REGION,
        engine::EMULATOR_ACCOUNT_ID,
        &format!("assistant/{assistant}"),
    );
    for (case, prompt_id, data) in &created {
        let reply = get_prompt(&emulator, &assistant_arn, prompt_id).await;
        assert_reads_the_create(&case.label, "a read through the parent's ARN", data, &reply);
    }
}

#[tokio::test]
async fn a_prompt_template_round_trips_at_both_published_text_lengths() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt template lengths").await;

    // Rows: the published bounds of the template text, and a multi-line YAML body between
    // them.
    for (what, text) in [
        (
            format!("template text at its published minimum length {TEMPLATE_TEXT_MIN}"),
            "y".repeat(TEMPLATE_TEXT_MIN),
        ),
        (
            "a multi-line YAML template".to_string(),
            "system: |\n  Answer in one sentence.\nmessages:\n  - role: user\n".to_string(),
        ),
        (
            format!("template text at its published maximum length {TEMPLATE_TEXT_MAX}"),
            "y".repeat(TEMPLATE_TEXT_MAX),
        ),
    ] {
        let mut body = required(&what, PROMPT_TYPES[0], API_FORMATS[0], VISIBILITY_SAVED);
        body["templateConfiguration"] = template(&text);
        let created = create_prompt(&emulator, &assistant, body.clone()).await;
        assert_eq!(
            created.status, 200,
            "case {what:?}: the create must be served, got HTTP {}",
            created.status
        );
        let prompt_id = created.string("/aiPrompt/aiPromptId");
        let read = get_prompt(&emulator, &assistant, &prompt_id).await;
        assert_eq!(
            read.status, 200,
            "case {what:?}: the read must be served, got HTTP {}",
            read.status
        );
        let got = read
            .field("/aiPrompt/templateConfiguration")
            .cloned()
            .unwrap_or(Value::Null);
        assert_eq!(
            got,
            template(&text),
            "case {what:?}: the read must report the template the create persisted, got a \
             template of {} bytes",
            got.to_string().len()
        );
    }
}

#[tokio::test]
async fn a_label_naming_no_prompt_of_this_assistant_is_not_found() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt not found").await;
    let other = assistant_id(&emulator, "prompt not found elsewhere").await;

    let held = create_prompt(
        &emulator,
        &assistant,
        required("held", PROMPT_TYPES[0], API_FORMATS[0], VISIBILITY_SAVED),
    )
    .await;
    assert_eq!(
        held.status, 200,
        "the prompt this suite reads around must be created, got HTTP {} with body {}",
        held.status, held.body
    );
    let held_id = held.string("/aiPrompt/aiPromptId");

    // Rows: the prompt label that names nothing this assistant holds — an absent uuid, a
    // prompt of ANOTHER assistant, an ARN of another account, another region or another
    // resource family, and a version qualifier naming a version nothing published.
    for prompt_label in [
        ABSENT_UUID.to_string(),
        held_id.clone(),
        expected_prompt_arn(&assistant, &held_id),
        wisdom_arn(
            "aws",
            REGION,
            OTHER_ACCOUNT,
            &format!("ai-prompt/{assistant}/{held_id}"),
        ),
        wisdom_arn(
            "aws",
            OTHER_REGION,
            engine::EMULATOR_ACCOUNT_ID,
            &format!("ai-prompt/{assistant}/{held_id}"),
        ),
        format!("{held_id}:1"),
    ] {
        // The prompt is held by `assistant`, so the rows that name it correctly are read
        // against the OTHER assistant, which holds nothing.
        let reply = get_prompt(&emulator, &other, &prompt_label).await;
        assert_not_found(
            &format!("read of {prompt_label:?} under another assistant"),
            MESSAGE_AI_PROMPT_NOT_FOUND,
            &prompt_label,
            &reply,
        );
    }

    // A numbered version qualifier names no version of a prompt that published none, even
    // under the assistant that holds it.
    let reply = get_prompt(&emulator, &assistant, &format!("{held_id}:1")).await;
    assert_not_found(
        "a numbered version qualifier",
        MESSAGE_AI_PROMPT_NOT_FOUND,
        &format!("{held_id}:1"),
        &reply,
    );

    // A route whose ASSISTANT names nothing this account holds is not found for the
    // assistant, whatever the rest of the route names.
    for assistant_label in [
        ABSENT_UUID.to_string(),
        wisdom_arn(
            "aws",
            REGION,
            OTHER_ACCOUNT,
            &format!("assistant/{assistant}"),
        ),
    ] {
        for prompt_label in [held_id.clone(), ABSENT_UUID.to_string()] {
            let reply = get_prompt(&emulator, &assistant_label, &prompt_label).await;
            assert_not_found(
                &format!("read under assistant {assistant_label:?}"),
                "Assistant does not exist.",
                &assistant_label,
                &reply,
            );
        }
        let reply = create_prompt(
            &emulator,
            &assistant_label,
            required(
                "orphan",
                PROMPT_TYPES[0],
                API_FORMATS[0],
                VISIBILITY_PUBLISHED,
            ),
        )
        .await;
        assert_not_found(
            &format!("create under assistant {assistant_label:?}"),
            "Assistant does not exist.",
            &assistant_label,
            &reply,
        );
    }

    // The prompt that WAS created is still readable: a refusal persisted nothing.
    let reply = get_prompt(&emulator, &assistant, &held_id).await;
    assert_eq!(
        reply.status, 200,
        "the held prompt must still be served, got HTTP {} with body {}",
        reply.status, reply.body
    );
}

#[tokio::test]
async fn a_replayed_client_token_answers_with_the_prompt_it_already_created() {
    let emulator = Emulator::new();
    let first = assistant_id(&emulator, "prompt idempotent first").await;
    let second = assistant_id(&emulator, "prompt idempotent second").await;

    // Rows: the token's form, spanning the published length bounds of `ClientToken`
    // (`len 1..4096`).
    for token in ["a-client-token", &"t".repeat(4096), "1"] {
        let mut body = required(
            "idempotent prompt",
            PROMPT_TYPES[0],
            API_FORMATS[0],
            VISIBILITY_SAVED,
        );
        body["clientToken"] = json!(token);

        let created = create_prompt(&emulator, &first, body.clone()).await;
        assert_eq!(
            created.status, 200,
            "case {token:?}: the create must be served, got HTTP {} with body {}",
            created.status, created.body
        );
        let prompt_id = created.string("/aiPrompt/aiPromptId");

        let replayed = create_prompt(&emulator, &first, body.clone()).await;
        assert_eq!(
            replayed.status, 200,
            "case {token:?}: the replay must be served, got HTTP {} with body {}",
            replayed.status, replayed.body
        );
        assert_eq!(
            replayed.field("/aiPrompt"),
            created.field("/aiPrompt"),
            "case {token:?}: a replayed token answers with the prompt it already created"
        );

        // The same token carrying a DIFFERENT request is the idempotency conflict.
        for (member, value) in [
            ("modelId", json!(MODEL_IDS[1])),
            ("apiFormat", json!(API_FORMATS[1])),
            (
                "templateConfiguration",
                template("system: Something else entirely.\n"),
            ),
            ("tags", json!(tag_map(1))),
        ] {
            let mut changed = body.clone();
            changed[member] = value;
            let conflict = create_prompt(&emulator, &first, changed).await;
            assert_eq!(
                conflict.status, 409,
                "case {token:?}/{member}: one token identifies one request, got HTTP {} with \
                 body {}",
                conflict.status, conflict.body
            );
            assert_eq!(
                conflict.string("/message"),
                MESSAGE_IDEMPOTENCY_CONFLICT,
                "case {token:?}/{member}: the refusal must carry this endpoint's sentence \
                 for the condition, body {}",
                conflict.body
            );
        }

        // The token is scoped to the assistant it created under, so the same token on
        // another assistant creates another prompt.
        let elsewhere = create_prompt(&emulator, &second, body).await;
        assert_eq!(
            elsewhere.status, 200,
            "case {token:?}: the same token on another parent creates another prompt, got \
             HTTP {} with body {}",
            elsewhere.status, elsewhere.body
        );
        let elsewhere_id = elsewhere.string("/aiPrompt/aiPromptId");
        assert_ne!(
            elsewhere_id, prompt_id,
            "case {token:?}: the two creates must be two records, got one id {prompt_id:?}"
        );
        // Each parent reports only its own record.
        let reply = get_prompt(&emulator, &second, &prompt_id).await;
        assert_not_found(
            &format!("case {token:?}: the first parent's prompt under the second"),
            MESSAGE_AI_PROMPT_NOT_FOUND,
            &prompt_id,
            &reply,
        );
    }
}

#[tokio::test]
async fn the_tags_a_create_persisted_on_a_prompt_are_what_list_tags_reports() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt tags").await;

    // Rows: the tag count — none, one, several.
    for count in [0_usize, 1, 7] {
        let mut body = required(
            &format!("tagged with {count}"),
            PROMPT_TYPES[0],
            API_FORMATS[0],
            VISIBILITY_SAVED,
        );
        let tags = tag_map(count);
        if count > 0 {
            body["tags"] = json!(tags);
        }
        let created = create_prompt(&emulator, &assistant, body).await;
        assert_eq!(
            created.status, 200,
            "case {count}: the create must be served, got HTTP {} with body {}",
            created.status, created.body
        );
        let prompt_arn = created.string("/aiPrompt/aiPromptArn");

        let reported = emulator.list_tags(&prompt_arn).await;
        assert_eq!(
            reported.status, 200,
            "case {count}: the prompt's ARN must name a resource the tagging surface holds, \
             got HTTP {} with body {}",
            reported.status, reported.body
        );
        assert_eq!(
            reported.field("/tags"),
            created.field("/aiPrompt/tags"),
            "case {count}: the reported map must be the one the create persisted"
        );
        assert_eq!(
            reported.field("/tags").cloned().unwrap_or(Value::Null),
            json!(tags),
            "case {count}: the reported map must be the one the request carried"
        );
    }
}

#[tokio::test]
async fn an_assistant_holds_at_most_the_stores_prompt_ceiling() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt ceiling").await;

    for index in 0..MAX_AI_PROMPTS_PER_ASSISTANT {
        let reply = create_prompt(
            &emulator,
            &assistant,
            required(
                &format!("prompt {index}"),
                PROMPT_TYPES[index % PROMPT_TYPES.len()],
                API_FORMATS[index % API_FORMATS.len()],
                VISIBILITY_SAVED,
            ),
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {index}: a create inside the ceiling must be served, got HTTP {} with body \
             {}",
            reply.status, reply.body
        );
    }

    let refused = create_prompt(
        &emulator,
        &assistant,
        required(
            "one too many",
            PROMPT_TYPES[0],
            API_FORMATS[0],
            VISIBILITY_SAVED,
        ),
    )
    .await;
    assert_eq!(
        refused.status, 402,
        "a create past the ceiling is the modelled quota refusal, got HTTP {} with body {}",
        refused.status, refused.body
    );
    assert_eq!(
        refused.error_type, "ServiceQuotaExceededException",
        "the refusal must carry the modelled error code, got {:?} with body {}",
        refused.error_type, refused.body
    );
    assert!(
        refused
            .string("/message")
            .contains(&MAX_AI_PROMPTS_PER_ASSISTANT.to_string()),
        "the refusal must state the ceiling it enforced, body {}",
        refused.body
    );
}

// ---------------------------------------------------------------------------
// The mutation rule: an update writes the record every later read answers with,
// and a delete removes it from every later caller
// ---------------------------------------------------------------------------

/// The members of `AIPromptData` an update can write
/// (`smithy_query(query="operation",operation="UpdateAIPrompt")` takes `visibilityStatus`
/// required, and `description`, `modelId`, `templateConfiguration` and
/// `inferenceConfiguration` optional).
const WRITABLE_MEMBERS: [&str; 5] = [
    "visibilityStatus",
    "description",
    "modelId",
    "templateConfiguration",
    "inferenceConfiguration",
];

/// The members of `AIPromptData` no update carries, so none of them may move
/// (`smithy_query(query="shape",shape="AIPromptData")`). `modifiedTime` is in neither
/// list: it reports when the record was last written, so an update is expected to move it.
const PRESERVED_MEMBERS: [&str; 11] = [
    "assistantId",
    "assistantArn",
    "aiPromptId",
    "aiPromptArn",
    "name",
    "type",
    "templateType",
    "apiFormat",
    "origin",
    "status",
    "tags",
];

/// One row of the update table: the create it starts from, and the updates layered on the
/// record that create left.
#[derive(Debug, Clone)]
struct UpdateRow {
    label: String,
    create: Value,
    updates: Vec<Value>,
}

/// An `UpdateAIPrompt` body: `visibilityStatus` is the one member the model requires, so a
/// row states only the optional members it varies.
fn update_with(visibility: &str, members: Vec<(&str, Value)>) -> Value {
    let mut body = json!({ "visibilityStatus": visibility });
    for (member, value) in members {
        body[member] = value;
    }
    body
}

/// A `CreateAIPrompt` body of this suite's first type and format, so an update row states
/// only what it needs the record to start from.
fn created_with(name: &str, visibility: &str, members: Vec<(&str, Value)>) -> Value {
    let mut body = required(name, PROMPT_TYPES[0], API_FORMATS[0], visibility);
    for (member, value) in members {
        body[member] = value;
    }
    body
}

/// The updates the write rule is asserted over. Each row differs from the others along one
/// axis of the rule — which optional members the update carries, which direction the
/// visibility status moves, a published bound of a member it writes, and how many updates
/// are layered on one record.
fn update_rows() -> Vec<UpdateRow> {
    let mut rows: Vec<UpdateRow> = Vec::new();

    // The required member alone, in both directions and restating the value already held:
    // an update carrying no optional member keeps every optional value the record has.
    for (what, from, to) in [
        (
            "publishing a saved prompt",
            VISIBILITY_SAVED,
            VISIBILITY_PUBLISHED,
        ),
        (
            "saving a published prompt",
            VISIBILITY_PUBLISHED,
            VISIBILITY_SAVED,
        ),
        (
            "restating the visibility status the prompt already holds",
            VISIBILITY_SAVED,
            VISIBILITY_SAVED,
        ),
    ] {
        rows.push(UpdateRow {
            label: format!("{what}, carrying only the required member"),
            create: created_with(
                "visibility only",
                from,
                vec![
                    ("description", json!("Described at create time")),
                    ("inferenceConfiguration", json!({ "topK": TOP_K_MAX })),
                ],
            ),
            updates: vec![update_with(to, Vec::new())],
        });
    }

    // `description` at each published bound, onto a prompt created with one and onto a
    // prompt created without any.
    for (what, create) in [
        (
            "onto a described prompt",
            created_with(
                "described",
                VISIBILITY_SAVED,
                vec![("description", json!("Described at create time"))],
            ),
        ),
        (
            "onto a prompt created without a description",
            created_with("undescribed", VISIBILITY_SAVED, Vec::new()),
        ),
    ] {
        rows.push(UpdateRow {
            label: format!("a description at each published bound, {what}"),
            create,
            updates: vec![
                update_with(
                    VISIBILITY_PUBLISHED,
                    vec![("description", json!("d".repeat(DESCRIPTION_MIN)))],
                ),
                update_with(
                    VISIBILITY_PUBLISHED,
                    vec![("description", json!("d".repeat(DESCRIPTION_MAX)))],
                ),
            ],
        });
    }

    // `modelId`: one entry of the documented catalog, and each published length bound.
    rows.push(UpdateRow {
        label: "another model id, from the catalog and at each published bound".to_string(),
        create: created_with("remodelled", VISIBILITY_SAVED, Vec::new()),
        updates: vec![
            update_with(VISIBILITY_SAVED, vec![("modelId", json!(MODEL_IDS[1]))]),
            update_with(
                VISIBILITY_SAVED,
                vec![("modelId", json!("m".repeat(MODEL_ID_MIN)))],
            ),
            update_with(
                VISIBILITY_PUBLISHED,
                vec![("modelId", json!("m".repeat(MODEL_ID_MAX)))],
            ),
        ],
    });

    // The prompt template, at each published length bound of its text.
    rows.push(UpdateRow {
        label: "another prompt template, at each published text length".to_string(),
        create: created_with("retemplated", VISIBILITY_PUBLISHED, Vec::new()),
        updates: vec![
            update_with(
                VISIBILITY_PUBLISHED,
                vec![(
                    "templateConfiguration",
                    template(&"t".repeat(TEMPLATE_TEXT_MIN)),
                )],
            ),
            update_with(
                VISIBILITY_SAVED,
                vec![(
                    "templateConfiguration",
                    template(&"t".repeat(TEMPLATE_TEXT_MAX)),
                )],
            ),
        ],
    });

    // The inference parameters: added by the update, every parameter at each published
    // bound of its range, then a partial set, which replaces the whole structure.
    rows.push(UpdateRow {
        label: "inference parameters at each published bound, then a partial set".to_string(),
        create: created_with("inference", VISIBILITY_SAVED, Vec::new()),
        updates: vec![
            update_with(
                VISIBILITY_SAVED,
                vec![(
                    "inferenceConfiguration",
                    json!({
                        "temperature": PROBABILITY_MIN,
                        "topP": PROBABILITY_MIN,
                        "topK": TOP_K_MIN,
                        "maxTokensToSample": MAX_TOKENS_MIN,
                    }),
                )],
            ),
            update_with(
                VISIBILITY_PUBLISHED,
                vec![(
                    "inferenceConfiguration",
                    json!({
                        "temperature": PROBABILITY_MAX,
                        "topP": PROBABILITY_MAX,
                        "topK": TOP_K_MAX,
                        "maxTokensToSample": MAX_TOKENS_MAX,
                    }),
                )],
            ),
            update_with(
                VISIBILITY_PUBLISHED,
                vec![(
                    "inferenceConfiguration",
                    json!({ "maxTokensToSample": MAX_TOKENS_MAX }),
                )],
            ),
        ],
    });

    // Every writable member at once, and several updates layered on one record: each is
    // applied to the record the previous left.
    rows.push(UpdateRow {
        label: "every writable member at once".to_string(),
        create: created_with("everything", VISIBILITY_SAVED, Vec::new()),
        updates: vec![update_with(
            VISIBILITY_PUBLISHED,
            vec![
                ("description", json!("Every writable member at once")),
                ("modelId", json!(MODEL_IDS[2])),
                (
                    "templateConfiguration",
                    template("system: Everything at once.\n"),
                ),
                (
                    "inferenceConfiguration",
                    json!({ "temperature": PROBABILITY_MAX, "topK": TOP_K_MAX }),
                ),
            ],
        )],
    });
    rows.push(UpdateRow {
        label: "three updates layered on one record".to_string(),
        create: created_with(
            "layered",
            VISIBILITY_SAVED,
            vec![("description", json!("The description the create carried"))],
        ),
        updates: vec![
            update_with(VISIBILITY_PUBLISHED, vec![("modelId", json!(MODEL_IDS[3]))]),
            update_with(
                VISIBILITY_PUBLISHED,
                vec![("description", json!("The second update"))],
            ),
            update_with(
                VISIBILITY_SAVED,
                vec![(
                    "templateConfiguration",
                    template("system: The third update.\n"),
                )],
            ),
        ],
    });

    rows
}

/// Every label form that addresses one prompt — the forms a read resolves: by id, by ARN,
/// and by either carrying the working copy's version qualifier.
fn label_forms(assistant_id: &str, prompt_id: &str) -> Vec<(String, String)> {
    vec![
        ("the prompt id".to_string(), prompt_id.to_string()),
        (
            "the prompt ARN".to_string(),
            expected_prompt_arn(assistant_id, prompt_id),
        ),
        (
            "the id carrying the working copy's qualifier".to_string(),
            format!("{prompt_id}:{QUALIFIER_LATEST}"),
        ),
        (
            "the ARN carrying the working copy's qualifier".to_string(),
            expected_read_arn(assistant_id, prompt_id, QUALIFIER_LATEST),
        ),
    ]
}

/// The `AIPromptData` an update must answer with: every member it carried replaces the
/// stored value, every other writable member is the one the previous answer reported, and
/// the members the service owns — the ARN of the resource among them — are untouched.
pub(crate) fn assert_applies_update(
    case: &str,
    what: &str,
    stored: &Value,
    update: &Value,
    reply: &Reply,
) {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: {what} must be served, got HTTP {} with body {}",
        reply.status, reply.body
    );
    let got = reply
        .field("/aiPrompt")
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no aiPrompt: {}", reply.body));

    for member in WRITABLE_MEMBERS {
        let expected = update.get(member).or_else(|| stored.get(member));
        let actual = got.get(member);
        assert_eq!(
            actual, expected,
            "case {case:?}: {what} must report the {member:?} the update carried, or the \
             stored one when it carried none, expected {expected:?}, got {actual:?}"
        );
    }
    for member in PRESERVED_MEMBERS {
        let expected = stored.get(member);
        let actual = got.get(member);
        assert_eq!(
            actual, expected,
            "case {case:?}: {what} must leave {member:?} as it was, expected {expected:?}, \
             got {actual:?}"
        );
    }

    // The write is what `modifiedTime` reports, so it never moves backwards.
    let was = stored
        .get("modifiedTime")
        .and_then(Value::as_f64)
        .unwrap_or_default();
    let now = got.get("modifiedTime").and_then(Value::as_f64);
    assert!(
        now.is_some_and(|now| now >= was),
        "case {case:?}: {what} must report when the record was last written, expected at \
         least {was}, got {now:?}"
    );
}

/// The record the update left, read back through `GetAIPrompt` however it is addressed: a
/// read answers with that record, with the one member a read renders differently — the ARN
/// it reports names the version it answered for.
pub(crate) async fn assert_reads_the_update(
    emulator: &Emulator,
    case: &str,
    assistant: &str,
    prompt_id: &str,
    written: &Value,
) {
    let mut expected = written.clone();
    expected["aiPromptArn"] = json!(expected_read_arn(assistant, prompt_id, QUALIFIER_LATEST));
    for (what, addressed_by) in label_forms(assistant, prompt_id) {
        let reply = get_prompt(emulator, assistant, &addressed_by).await;
        assert_eq!(
            reply.status, 200,
            "case {case:?}: a read addressed by {what} must be served, got HTTP {} with body \
             {}",
            reply.status, reply.body
        );
        let read = reply.field("/aiPrompt");
        assert_eq!(
            read,
            Some(&expected),
            "case {case:?}: a read addressed by {what} must answer with the record the update \
             wrote, expected {expected}, got {read:?}"
        );
        // The model: a version number is reported only when the request's `aiPromptId`
        // named a VERSION. An update writes the working copy and publishes nothing, so a
        // read of it names no numbered version.
        assert!(
            reply.field("/versionNumber").is_none(),
            "case {case:?}: a read of the working copy reports no versionNumber, body {}",
            reply.body
        );
    }

    // An update publishes no version, so a numbered qualifier still names nothing.
    let numbered = format!("{prompt_id}:1");
    let reply = get_prompt(emulator, assistant, &numbered).await;
    assert_not_found(case, MESSAGE_AI_PROMPT_NOT_FOUND, &numbered, &reply);
}

#[tokio::test]
async fn an_update_writes_the_prompt_every_later_read_answers_with() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt update").await;

    for row in update_rows() {
        let created = create_prompt(&emulator, &assistant, row.create.clone()).await;
        assert_eq!(
            created.status, 200,
            "case {:?}: the create must be served, got HTTP {} with body {}",
            row.label, created.status, created.body
        );
        let prompt_id = created.string("/aiPrompt/aiPromptId");
        let mut stored = created
            .field("/aiPrompt")
            .cloned()
            .unwrap_or_else(|| panic!("case {:?}: the create answered no aiPrompt", row.label));

        for (index, update) in row.updates.iter().enumerate() {
            let what = format!("update {} of {}", index + 1, row.updates.len());
            let reply = update_prompt(&emulator, &assistant, &prompt_id, update.clone()).await;
            assert_applies_update(&row.label, &what, &stored, update, &reply);
            let written = reply
                .field("/aiPrompt")
                .cloned()
                .unwrap_or_else(|| panic!("case {:?}: {what} answered no aiPrompt", row.label));
            assert_reads_the_update(&emulator, &row.label, &assistant, &prompt_id, &written).await;
            // The next update is applied to the record this one left.
            stored = written;
        }

        // Released again, so the rows of this table do not fill the assistant's ceiling.
        assert_eq!(
            delete_prompt(&emulator, &assistant, &prompt_id).await.status,
            204,
            "case {:?}: the cleanup delete must be served",
            row.label
        );
    }
}

#[tokio::test]
async fn an_update_addressed_through_every_label_form_writes_the_working_copy() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt update labels").await;
    let created = create_prompt(
        &emulator,
        &assistant,
        created_with("addressed", VISIBILITY_SAVED, Vec::new()),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the prompt this suite writes to must be created, got HTTP {} with body {}",
        created.status, created.body
    );
    let prompt_id = created.string("/aiPrompt/aiPromptId");
    let mut stored = created
        .field("/aiPrompt")
        .cloned()
        .expect("the create answered no aiPrompt");

    for (index, (what, addressed_by)) in label_forms(&assistant, &prompt_id).iter().enumerate() {
        // The `description` pattern (`^[a-zA-Z0-9\s_.,-]+`) admits none of the label
        // forms' own punctuation, so each row writes the row number rather than the form's
        // name.
        let update = update_with(
            if index % 2 == 0 {
                VISIBILITY_PUBLISHED
            } else {
                VISIBILITY_SAVED
            },
            vec![("description", json!(format!("Written through form {index}")))],
        );
        let reply = update_prompt(&emulator, &assistant, addressed_by, update.clone()).await;
        assert_applies_update(what, "the update", &stored, &update, &reply);
        let written = reply
            .field("/aiPrompt")
            .cloned()
            .unwrap_or_else(|| panic!("case {what:?}: the update answered no aiPrompt"));
        assert_reads_the_update(&emulator, what, &assistant, &prompt_id, &written).await;
        stored = written;
    }

    // A numbered version qualifier names a published snapshot rather than the working copy
    // an update writes, and this prompt published none, so it names nothing to write.
    let numbered = format!("{prompt_id}:1");
    let reply = update_prompt(
        &emulator,
        &assistant,
        &numbered,
        update_with(VISIBILITY_SAVED, Vec::new()),
    )
    .await;
    assert_not_found(
        "an update addressed by a numbered version",
        MESSAGE_AI_PROMPT_NOT_FOUND,
        &numbered,
        &reply,
    );
}

#[tokio::test]
async fn a_mutation_naming_no_prompt_of_this_assistant_is_refused_and_writes_nothing() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt mutation refusals").await;
    let other = assistant_id(&emulator, "prompt mutation refusals elsewhere").await;
    let created = create_prompt(
        &emulator,
        &assistant,
        created_with(
            "untouched",
            VISIBILITY_SAVED,
            vec![("description", json!("The description the create carried"))],
        ),
    )
    .await;
    assert_eq!(
        created.status, 200,
        "the prompt these refusals must not touch has to be created, got HTTP {} with body {}",
        created.status, created.body
    );
    let prompt_id = created.string("/aiPrompt/aiPromptId");
    let stored = created
        .field("/aiPrompt")
        .cloned()
        .expect("the create answered no aiPrompt");

    // Rows: the prompt label that names nothing the addressed assistant holds — an absent
    // uuid and a version qualifier naming a version nothing published, under the assistant
    // that holds the prompt; and, under an assistant that holds nothing, every label form
    // of this prompt and its ARN under another account or another region, since a prompt of
    // ANOTHER assistant is not this one's however correctly it is named.
    let absent: Vec<(String, String)> = vec![
        (assistant.clone(), ABSENT_UUID.to_string()),
        (assistant.clone(), format!("{prompt_id}:1")),
        (
            other.clone(),
            wisdom_arn(
                "aws",
                REGION,
                OTHER_ACCOUNT,
                &format!("ai-prompt/{assistant}/{prompt_id}"),
            ),
        ),
        (
            other.clone(),
            wisdom_arn(
                "aws",
                OTHER_REGION,
                engine::EMULATOR_ACCOUNT_ID,
                &format!("ai-prompt/{assistant}/{prompt_id}"),
            ),
        ),
    ]
    .into_iter()
    .chain(
        label_forms(&assistant, &prompt_id)
            .into_iter()
            .map(|(_, prompt_label)| (other.clone(), prompt_label)),
    )
    .collect();

    for (assistant_label, prompt_label) in &absent {
        let reply = update_prompt(
            &emulator,
            assistant_label,
            prompt_label,
            update_with(
                VISIBILITY_PUBLISHED,
                vec![("description", json!("Never written"))],
            ),
        )
        .await;
        assert_not_found(
            &format!("update of {prompt_label:?} under {assistant_label:?}"),
            MESSAGE_AI_PROMPT_NOT_FOUND,
            prompt_label,
            &reply,
        );
        let reply = delete_prompt(&emulator, assistant_label, prompt_label).await;
        assert_not_found(
            &format!("delete of {prompt_label:?} under {assistant_label:?}"),
            MESSAGE_AI_PROMPT_NOT_FOUND,
            prompt_label,
            &reply,
        );
    }

    // A route whose ASSISTANT names nothing this account holds is not found for the
    // assistant, whatever the rest of the route names.
    for assistant_label in [
        ABSENT_UUID.to_string(),
        wisdom_arn(
            "aws",
            REGION,
            OTHER_ACCOUNT,
            &format!("assistant/{assistant}"),
        ),
    ] {
        let reply = update_prompt(
            &emulator,
            &assistant_label,
            &prompt_id,
            update_with(VISIBILITY_PUBLISHED, Vec::new()),
        )
        .await;
        assert_not_found(
            &format!("update under assistant {assistant_label:?}"),
            "Assistant does not exist.",
            &assistant_label,
            &reply,
        );
        let reply = delete_prompt(&emulator, &assistant_label, &prompt_id).await;
        assert_not_found(
            &format!("delete under assistant {assistant_label:?}"),
            "Assistant does not exist.",
            &assistant_label,
            &reply,
        );
    }

    // Nothing above wrote or removed anything: the prompt still reads back as the create
    // left it.
    let reply = get_prompt(&emulator, &assistant, &prompt_id).await;
    assert_reads_the_create("every refusal", "the read after them", &stored, &reply);
}

#[tokio::test]
async fn a_deleted_prompt_is_reported_to_no_later_caller() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt delete").await;

    // Rows: the label form the delete addresses the prompt by — the same forms a read
    // resolves — each on a prompt of its own, beside one the delete never addresses.
    for (index, (what, _)) in label_forms(&assistant, ABSENT_UUID).iter().enumerate() {
        let kept = create_prompt(
            &emulator,
            &assistant,
            created_with(&format!("kept {index}"), VISIBILITY_SAVED, Vec::new()),
        )
        .await;
        let kept_id = kept.string("/aiPrompt/aiPromptId");
        let kept_record = kept
            .field("/aiPrompt")
            .cloned()
            .unwrap_or_else(|| panic!("case {what:?}: the create answered no aiPrompt"));
        let doomed = create_prompt(
            &emulator,
            &assistant,
            created_with(&format!("deleted {index}"), VISIBILITY_PUBLISHED, Vec::new()),
        )
        .await;
        assert_eq!(
            doomed.status, 200,
            "case {what:?}: the prompt to delete must be created, got HTTP {} with body {}",
            doomed.status, doomed.body
        );
        let doomed_id = doomed.string("/aiPrompt/aiPromptId");
        let addressed_by = label_forms(&assistant, &doomed_id)[index].1.clone();

        let deleted = delete_prompt(&emulator, &assistant, &addressed_by).await;
        assert_eq!(
            deleted.status, 204,
            "case {what:?}: the delete must answer the modelled 204, got HTTP {} with body {}",
            deleted.status, deleted.body
        );
        assert!(
            deleted
                .body
                .as_object()
                .is_none_or(serde_json::Map::is_empty),
            "case {what:?}: the modelled 204 carries no document members, got {}",
            deleted.body
        );

        // Every operation that resolves the prompt stops finding it, whichever label form
        // addresses it: a read, a second delete, and an update.
        for (form, label_of_gone) in label_forms(&assistant, &doomed_id) {
            let case = format!("{what:?} deleted, then addressed by {form}");
            let reply = get_prompt(&emulator, &assistant, &label_of_gone).await;
            assert_not_found(
                &format!("read: {case}"),
                MESSAGE_AI_PROMPT_NOT_FOUND,
                &label_of_gone,
                &reply,
            );
            let reply = delete_prompt(&emulator, &assistant, &label_of_gone).await;
            assert_not_found(
                &format!("second delete: {case}"),
                MESSAGE_AI_PROMPT_NOT_FOUND,
                &label_of_gone,
                &reply,
            );
            let reply = update_prompt(
                &emulator,
                &assistant,
                &label_of_gone,
                update_with(VISIBILITY_SAVED, Vec::new()),
            )
            .await;
            assert_not_found(
                &format!("update: {case}"),
                MESSAGE_AI_PROMPT_NOT_FOUND,
                &label_of_gone,
                &reply,
            );
        }

        // The delete removed ONE record: the prompt it did not address still reads back as
        // its create left it.
        let reply = get_prompt(&emulator, &assistant, &kept_id).await;
        assert_reads_the_create(what, "the sibling the delete left alone", &kept_record, &reply);
    }
}

#[tokio::test]
async fn a_delete_releases_the_assistants_prompt_ceiling() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt ceiling release").await;

    let mut first = String::new();
    for index in 0..MAX_AI_PROMPTS_PER_ASSISTANT {
        let reply = create_prompt(
            &emulator,
            &assistant,
            created_with(&format!("prompt {index}"), VISIBILITY_SAVED, Vec::new()),
        )
        .await;
        assert_eq!(
            reply.status, 200,
            "case {index}: a create inside the ceiling must be served, got HTTP {} with body \
             {}",
            reply.status, reply.body
        );
        if index == 0 {
            first = reply.string("/aiPrompt/aiPromptId");
        }
    }
    let refused = create_prompt(
        &emulator,
        &assistant,
        created_with("one too many", VISIBILITY_SAVED, Vec::new()),
    )
    .await;
    assert_eq!(
        refused.status, 402,
        "a create past the ceiling is the modelled quota refusal, got HTTP {} with body {}",
        refused.status, refused.body
    );

    // The ceiling counts the prompts still LIVE, so releasing one makes room for exactly
    // one more.
    assert_eq!(
        delete_prompt(&emulator, &assistant, &first).await.status,
        204,
        "the delete that makes room must be served"
    );
    let served = create_prompt(
        &emulator,
        &assistant,
        created_with("after the delete", VISIBILITY_PUBLISHED, Vec::new()),
    )
    .await;
    assert_eq!(
        served.status, 200,
        "a create into the room the delete freed must be served, got HTTP {} with body {}",
        served.status, served.body
    );
    let refused = create_prompt(
        &emulator,
        &assistant,
        created_with("one too many again", VISIBILITY_SAVED, Vec::new()),
    )
    .await;
    assert_eq!(
        refused.status, 402,
        "the ceiling still holds after a delete and a create, got HTTP {} with body {}",
        refused.status, refused.body
    );
}

#[tokio::test]
async fn a_replayed_client_token_on_an_update_answers_with_the_record_its_write_left() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "prompt update idempotency").await;

    // Rows: the token's form, spanning the published length bounds of `ClientToken`
    // (`len 1..4096`).
    for token in ["an-update-token", &"t".repeat(4096), "1"] {
        let created = create_prompt(
            &emulator,
            &assistant,
            created_with("idempotent update", VISIBILITY_SAVED, Vec::new()),
        )
        .await;
        assert_eq!(
            created.status, 200,
            "case {token:?}: the create must be served, got HTTP {} with body {}",
            created.status, created.body
        );
        let prompt_id = created.string("/aiPrompt/aiPromptId");
        let stored = created
            .field("/aiPrompt")
            .cloned()
            .unwrap_or_else(|| panic!("case {token:?}: the create answered no aiPrompt"));

        let mut update = update_with(
            VISIBILITY_PUBLISHED,
            vec![("description", json!("The update this token identifies"))],
        );
        update["clientToken"] = json!(token);
        let written = update_prompt(&emulator, &assistant, &prompt_id, update.clone()).await;
        assert_applies_update(token, "the update", &stored, &update, &written);
        let record = written
            .field("/aiPrompt")
            .cloned()
            .unwrap_or_else(|| panic!("case {token:?}: the update answered no aiPrompt"));

        // The same token carrying the same request answers with the record that write
        // left, writing nothing a second time.
        let replayed = update_prompt(&emulator, &assistant, &prompt_id, update.clone()).await;
        assert_eq!(
            replayed.field("/aiPrompt"),
            Some(&record),
            "case {token:?}: a replayed token must answer with the record its write left, \
             body {}",
            replayed.body
        );

        // The same token carrying a DIFFERENT request is the idempotency conflict, and
        // writes nothing.
        for (member, value) in [
            ("visibilityStatus", json!(VISIBILITY_SAVED)),
            ("description", json!("Another request under one token")),
            ("modelId", json!(MODEL_IDS[1])),
            (
                "templateConfiguration",
                template("system: Something else entirely.\n"),
            ),
            (
                "inferenceConfiguration",
                json!({ "maxTokensToSample": MAX_TOKENS_MIN }),
            ),
        ] {
            let mut changed = update.clone();
            changed[member] = value;
            let conflict = update_prompt(&emulator, &assistant, &prompt_id, changed).await;
            assert_eq!(
                conflict.status, 409,
                "case {token:?}/{member}: one token identifies one request, got HTTP {} with \
                 body {}",
                conflict.status, conflict.body
            );
            assert_eq!(
                conflict.string("/message"),
                MESSAGE_IDEMPOTENCY_CONFLICT,
                "case {token:?}/{member}: the refusal must carry this endpoint's sentence \
                 for the condition, body {}",
                conflict.body
            );
        }
        assert_reads_the_update(&emulator, token, &assistant, &prompt_id, &record).await;

        // Released again, so the rows of this table do not fill the assistant's ceiling.
        assert_eq!(
            delete_prompt(&emulator, &assistant, &prompt_id).await.status,
            204,
            "case {token:?}: the cleanup delete must be served"
        );
    }
}
