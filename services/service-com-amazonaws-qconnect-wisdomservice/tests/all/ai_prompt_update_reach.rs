// SPDX-License-Identifier: CC0-1.0

//! What an `UpdateAIPrompt` write REACHES, driven over HTTP through the same router the
//! emulator serves with.
//!
//! The rule under test: an update applies every member it carries to the working copy of
//! the ONE AI prompt its route addresses — whatever prompt type, API format or template
//! type that prompt was created with — and to nothing else, so every other prompt of the
//! same assistant still reads back as the record its own create or update left.
//!
//! `ai_prompt.rs` asserts the write rule over the members an update carries and the label
//! forms it is addressed by, always on a prompt of one type sitting alone in its assistant.
//! This suite varies the two axes that one leaves fixed:
//!
//! * the prompt the update is applied to: one row per value of the modelled `AIPromptType`
//!   catalog, each paired with a different `AIPromptAPIFormat` and model id, so the rule is
//!   asserted on a prompt of every type rather than only the first;
//! * what else the assistant holds while the update runs: every row's prompt is live at
//!   once, so each write is observed against the records of all the others.
//!
//! Every assertion goes over the wire — only the HTTP status and the JSON body are
//! inspected — and each read-back is compared with the whole record the previous answer
//! reported, so a response that is not computed from persisted state cannot pass. The
//! harness, the per-operation helpers and the two assertions of the write rule are the ones
//! `ai_prompt.rs` already drives this service's AI-prompt surface with.

use serde_json::{json, Value};

use super::ai_guardrail::{assistant_id, VISIBILITY_PUBLISHED, VISIBILITY_SAVED};
use super::ai_prompt::{
    assert_applies_update, assert_reads_the_update, create_prompt, get_prompt, required, template,
    update_prompt, API_FORMATS, MODEL_IDS, PROMPT_TYPES,
};
use super::resource_tags::{Emulator, Reply};

/// One row of the table: the prompt a create of this type and format leaves, and the
/// update layered on it.
#[derive(Debug, Clone)]
struct Row {
    label: String,
    create: Value,
    update: Value,
}

/// The rows the rule is asserted over: one per value of the `AIPromptType` catalog, each
/// created in a different API format with a different model id, and each updated with a
/// different set of the writable members — so no two rows write the same shape of request
/// and every type is reached.
fn rows() -> Vec<Row> {
    PROMPT_TYPES
        .iter()
        .enumerate()
        .map(|(index, prompt_type)| {
            let api_format = API_FORMATS[index % API_FORMATS.len()];
            // The create's visibility status alternates, and the update moves it the other
            // way, so both directions of the required member are covered across the table.
            let (created_as, updated_to) = if index % 2 == 0 {
                (VISIBILITY_SAVED, VISIBILITY_PUBLISHED)
            } else {
                (VISIBILITY_PUBLISHED, VISIBILITY_SAVED)
            };
            let mut create = required(
                &format!("reach of an update on a {prompt_type} prompt"),
                prompt_type,
                api_format,
                created_as,
            );
            create["modelId"] = json!(MODEL_IDS[index % MODEL_IDS.len()]);

            // Which optional members this row's update carries: a different subset per row,
            // from the description alone up to every one of them.
            let mut update = json!({
                "visibilityStatus": updated_to,
                "description": format!("Updated prompt {index} of type {prompt_type}"),
            });
            if index % 2 == 0 {
                update["modelId"] = json!(MODEL_IDS[(index + 1) % MODEL_IDS.len()]);
            }
            if index % 3 == 0 {
                update["templateConfiguration"] =
                    template(&format!("system: The update of prompt {index}.\nmessages: []\n"));
            }
            if index % 4 == 0 {
                // `topK` is `range 0..200`
                // (`smithy_query(query="shape",shape="AIPromptInferenceConfiguration")`),
                // and the row count is far inside it.
                update["inferenceConfiguration"] = json!({ "topK": index as i64 });
            }

            Row {
                label: format!("type {prompt_type} in format {api_format}"),
                create,
                update,
            }
        })
        .collect()
}

/// One prompt of the table, as the wire has reported it so far: the id its create minted,
/// the record the last WRITE answered with, and the record the last READ answered with.
/// The two differ in one member — a read names the version it answered for in the ARN it
/// reports — so both are kept.
#[derive(Debug)]
struct Seen {
    label: String,
    id: String,
    written: Value,
    read: Value,
}

/// The `aiPrompt` of a reply that must have been served.
fn served(case: &str, what: &str, reply: &Reply) -> Value {
    assert_eq!(
        reply.status, 200,
        "case {case:?}: {what} must be served, got HTTP {} with body {}",
        reply.status, reply.body
    );
    reply
        .field("/aiPrompt")
        .cloned()
        .unwrap_or_else(|| panic!("case {case:?}: {what} answered no aiPrompt: {}", reply.body))
}

#[tokio::test]
async fn an_update_writes_the_prompt_it_addresses_whatever_type_it_was_created_with() {
    let emulator = Emulator::new();
    let assistant = assistant_id(&emulator, "ai prompt update reach").await;
    let rows = rows();

    // Every row's prompt is created first, so each update below runs while the assistant
    // holds all of them.
    let mut seen: Vec<Seen> = Vec::new();
    for row in &rows {
        let created = create_prompt(&emulator, &assistant, row.create.clone()).await;
        let written = served(&row.label, "the create", &created);
        let id = created.string("/aiPrompt/aiPromptId");
        assert!(
            !id.is_empty(),
            "case {:?}: the create answered no aiPromptId, body {}",
            row.label,
            created.body
        );
        let read = served(
            &row.label,
            "the read after the create",
            &get_prompt(&emulator, &assistant, &id).await,
        );
        seen.push(Seen {
            label: row.label.clone(),
            id,
            written,
            read,
        });
    }

    for (index, row) in rows.iter().enumerate() {
        let case = row.label.clone();
        let id = seen[index].id.clone();

        // The answer: every member the update carried replaces the stored one, every other
        // writable member keeps the value the record already held, and the members the
        // service owns — this prompt's type, template type, API format, origin, status and
        // identity among them — are not this request's to change.
        let answer = update_prompt(&emulator, &assistant, &id, row.update.clone()).await;
        assert_applies_update(&case, "the update", &seen[index].written, &row.update, &answer);
        let written = served(&case, "the update", &answer);

        // And that record is what every later read of this prompt answers with, however the
        // read addresses it.
        assert_reads_the_update(&emulator, &case, &assistant, &id, &written).await;

        // Nothing else was reached: every other prompt of this assistant still reads back
        // as the record its own create or last update left, down to its modifiedTime.
        for (other_index, other) in seen.iter().enumerate() {
            if other_index == index {
                continue;
            }
            let untouched = served(
                &case,
                &format!("the read of the {:?} prompt", other.label),
                &get_prompt(&emulator, &assistant, &other.id).await,
            );
            assert_eq!(
                untouched, other.read,
                "case {case:?}: the update must not reach the {:?} prompt, expected {}, got \
                 {untouched}",
                other.label, other.read
            );
        }

        seen[index].read = served(
            &case,
            "the read after the update",
            &get_prompt(&emulator, &assistant, &id).await,
        );
        seen[index].written = written;
    }
}
