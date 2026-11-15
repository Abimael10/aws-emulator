// SPDX-License-Identifier: CC0-1.0

//! The CANONICAL JSON document of a custom lens: the `LensJSON` `ExportLens` answers.
//!
//! `LensJSON` is "the JSON representation of a lens" and is modelled as a bounded STRING
//! (`smithy_query(query="shape", shape="ExportLensOutput")`), so the bytes are the value:
//! the member order, the separators and the properties the document carries are all part
//! of what a client checksums, diffs and feeds back into `ImportLens`. The service
//! therefore publishes ONE serialization of a lens — written here from the lens the store
//! holds ([`LensDefinition`]) rather than from the bytes whatever import carried — so two
//! exports of the same lens are byte-identical whoever imported it and however that
//! document happened to be spelled.
//!
//! What this writer emits:
//!
//! * the lens's properties in the order `name`, `schemaVersion`, `description`, `pillars`,
//!   and every nested object's in the order the format declares them;
//! * no whitespace anywhere — a member separator is `,` and a name separator `:`;
//! * the `number` every question and every choice of an exported lens carries
//!   ([`LENS_DOCUMENT_NUMBER`]).
//!
//! An optional property the lens document does not declare is left out rather than written
//! as `null`, exactly as its absence is recorded on the lens.
//!
//! The string writing is deliberately explicit rather than a `::serde_json::Value` round
//! trip: a map's member order there depends on whether anything in the workspace enables
//! `serde_json/preserve_order` (the same hazard [`document_json`] guards against), and the
//! exported document's order is part of its value.

use super::*;

/// The `number` an exported question or choice carries.
///
/// The exported document declares an integer `number` on every question object and every
/// choice object, and it is the SAME on each of them — a question's second choice carries
/// what its first does — so the member is not the object's position in the array that
/// declares it. The lens JSON format specification gives an author no way to declare it
/// either: no document this service accepts carries a `number` ([`parse_lens_json`]), so a
/// lens imported from a document sets nothing for it and every question and choice of the
/// export reports the member's zero.
const LENS_DOCUMENT_NUMBER: u32 = 0;

/// The canonical JSON document of `lens` — the string `ExportLens` answers for it.
///
/// A pure function of the stored lens, so every read of a lens's content exports the same
/// bytes, and a lens whose content was imported twice under different spellings exports
/// one document.
pub fn canonical_lens_json(lens: &LensDefinition) -> String {
    let mut out = String::new();
    let mut document = CanonicalObject::open(&mut out);
    push_text(document.member("name"), &lens.name);
    push_text(document.member("schemaVersion"), &lens.schema_version);
    if let Some(description) = &lens.description {
        push_text(document.member("description"), description);
    }
    push_array(document.member("pillars"), &lens.pillars, write_pillar);
    document.close();
    out
}

/// One pillar: its id, its name and the questions it asks, in the document's own order.
fn write_pillar(out: &mut String, pillar: &LensPillar) {
    let mut object = CanonicalObject::open(out);
    push_text(object.member("id"), &pillar.id);
    push_text(object.member("name"), &pillar.name);
    push_array(object.member("questions"), &pillar.questions, write_question);
    object.close();
}

/// One question: its id, its `number`, what it asks, the choices it offers and the rules
/// that map an answer's choices to a risk.
fn write_question(out: &mut String, question: &LensQuestion) {
    let mut object = CanonicalObject::open(out);
    push_text(object.member("id"), &question.id);
    push_number(object.member("number"), LENS_DOCUMENT_NUMBER);
    push_text(object.member("title"), &question.title);
    if let Some(description) = &question.description {
        push_text(object.member("description"), description);
    }
    if let Some(helpful_resource) = &question.helpful_resource {
        write_content(object.member("helpfulResource"), helpful_resource);
    }
    push_array(object.member("choices"), &question.choices, write_choice);
    push_array(
        object.member("riskRules"),
        &question.risk_rules,
        write_risk_rule,
    );
    object.close();
}

/// One choice: its id, its `number`, its title and the content entries the lens declares
/// for it.
fn write_choice(out: &mut String, choice: &LensChoice) {
    let mut object = CanonicalObject::open(out);
    push_text(object.member("id"), &choice.id);
    push_number(object.member("number"), LENS_DOCUMENT_NUMBER);
    push_text(object.member("title"), &choice.title);
    if let Some(helpful_resource) = &choice.helpful_resource {
        write_content(object.member("helpfulResource"), helpful_resource);
    }
    if let Some(improvement_plan) = &choice.improvement_plan {
        write_content(object.member("improvementPlan"), improvement_plan);
    }
    object.close();
}

/// One `{displayText, url}` content entry, in the order `ChoiceContent` declares the two.
fn write_content(out: &mut String, content: &LensChoiceContent) {
    let mut object = CanonicalObject::open(out);
    push_text(object.member("displayText"), &content.display_text);
    if let Some(url) = &content.url {
        push_text(object.member("url"), url);
    }
    object.close();
}

/// One risk rule, kept as the document wrote it: the condition over the question's choice
/// ids and the risk it maps to.
fn write_risk_rule(out: &mut String, rule: &LensRiskRule) {
    let mut object = CanonicalObject::open(out);
    push_text(object.member("condition"), &rule.condition);
    push_text(object.member("risk"), &rule.risk);
    object.close();
}

/// One JSON object being written, member by member, in the order the canonical document
/// declares them.
///
/// The member separator is written BEFORE each member but the first, which is what lets an
/// optional property be skipped without leaving a dangling comma behind.
struct CanonicalObject<'doc> {
    out: &'doc mut String,
    written: usize,
}

impl<'doc> CanonicalObject<'doc> {
    fn open(out: &'doc mut String) -> Self {
        out.push('{');
        Self { out, written: 0 }
    }

    /// Open the member `name` and hand back the buffer its VALUE is written into.
    fn member(&mut self, name: &str) -> &mut String {
        if self.written > 0 {
            self.out.push(',');
        }
        self.written += 1;
        push_text(self.out, name);
        self.out.push(':');
        self.out
    }

    fn close(self) {
        self.out.push('}');
    }
}

/// A JSON array of `items`, each written by `write`, in the order the lens declares them.
fn push_array<Item>(out: &mut String, items: &[Item], write: impl Fn(&mut String, &Item)) {
    out.push('[');
    for (position, item) in items.iter().enumerate() {
        if position > 0 {
            out.push(',');
        }
        write(out, item);
    }
    out.push(']');
}

/// A JSON string, escaped as JSON escapes one.
fn push_text(out: &mut String, text: &str) {
    out.push_str(&::serde_json::Value::String(text.to_string()).to_string());
}

/// A JSON integer — the `number` of a question or a choice.
fn push_number(out: &mut String, number: u32) {
    out.push_str(&number.to_string());
}
