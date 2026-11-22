// SPDX-License-Identifier: CC0-1.0

//! The lens JSON document an import carries, read against the lens JSON format
//! specification, and how a document that is not a lens is REFUSED.
//!
//! "Lenses are defined in JSON. For more information, see JSON format specification in the
//! Well-Architected Tool User Guide" (`ImportLens` doc): the document is content this
//! service validates ITSELF, not a request member the front door reads. So the two are
//! refused in different voices, and this is the one that is not the front door's:
//!
//! * a member of the REQUEST that the shape it is read as does not admit is reported with
//!   the schema finding the layer that reads a request states about it — the bracketed
//!   report naming the JSON property, the type it found or the bound it measured
//!   ([`service_common::validation_message`]);
//! * a LENS DOCUMENT the format does not admit is reported SEMANTICALLY: the rule of the
//!   format the document broke, named in prose behind this service's `[Validation]` tag and
//!   carrying nothing of whatever reader found it. The recorded refusal of a document that
//!   declares no pillar is `[Validation] Empty pillars list` — the defect, not a validator's
//!   internal record of it.
//!
//! [`LensJsonDefect`] is therefore a CLASS of defect plus the document property the broken
//! rule holds on, and [`LensJsonDefect::phrase`] is the one place those phrases are worded,
//! so every rule this module enforces is reported the same way and the phrase for a rule is
//! read off the property the format spells it on rather than off whatever this module used to
//! read the document with.
//!
//! What the format requires, and this module enforces: a `schemaVersion` of
//! [`LENS_SCHEMA_VERSION`], a `name` and optional `description` within the modelled
//! `LensName` / `LensDescription` bounds, and at least one pillar, each with at least one
//! question, each question with at least one choice and a `riskRules` list whose last rule is
//! the [`LENS_JSON_DEFAULT_CONDITION`] one. Ids are unique within their scope, because a
//! rule, an answer and a report all name a question or a choice by its id.

use super::*;

/// Why a lens document was refused.
///
/// A value that is not a lens document AT ALL carries no defect to name — none of the
/// format's rules about a lens was reached — so it is reported as the member it arrived in
/// ([`LensJsonRejection::Unreadable`]). Everything else is one rule of the format the
/// document broke. [`lens_json_exception`] turns either into the modelled
/// `ValidationException`.
#[derive(Debug, Clone)]
pub enum LensJsonRejection {
    /// The value is not a lens document: not JSON at all, or JSON that is not an object.
    Unreadable,
    /// The rule of the lens JSON format specification the document broke.
    Defect(LensJsonDefect),
}

/// One rule of the lens JSON format specification a document broke, as the refusal NAMES
/// it: the class of the defect and the document property the rule holds on, which together
/// are the whole phrase a refusal states ([`LensJsonDefect::phrase`]).
///
/// A property is named as the FORMAT spells it in the document, because that is the text the
/// author has to go and fix.
#[derive(Debug, Clone)]
pub enum LensJsonDefect {
    /// A property the format requires that the document does not declare.
    Missing { property: &'static str },
    /// A list property the format requires an entry of that the document declares no entry
    /// in: one it leaves out, carries no value in, or declares as an empty list — either way
    /// the lens has none of what that list is a list of, which is the one rule broken.
    EmptyList { property: &'static str },
    /// A text property the format requires a value in that the document declares empty.
    EmptyText { property: &'static str },
    /// A property declared with a value the format does not admit there: a value of another
    /// type, outside the bounds the property is published with, outside the set of values the
    /// format declares for it, or a list whose entries break the format's own rule about
    /// their order.
    Invalid { property: &'static str },
    /// Two entries of one list declaring the same id, where the format identifies an entry
    /// of that list by it. `entity` is the format's own name for ONE entry of the list.
    DuplicateId { entity: &'static str, id: String },
}

impl LensJsonDefect {
    /// The phrase this defect is REPORTED as — the whole variable part of the refusal's
    /// message, behind this service's failure-class tag.
    ///
    /// One place, so every rule this module enforces is named in the same voice: the class of
    /// the defect, then the property of the document the rule holds on as the format spells
    /// it. Nothing of the document's values reaches a phrase except an id the format
    /// identifies an entry by, which is the only thing that tells the author WHICH entry to
    /// fix.
    pub fn phrase(&self) -> String {
        match self {
            Self::Missing { property } => format!("Missing {property}"),
            Self::EmptyList { property } => format!("Empty {property} list"),
            Self::EmptyText { property } => format!("Empty {property}"),
            Self::Invalid { property } => format!("Invalid {property}"),
            Self::DuplicateId { entity, id } => format!("Duplicate {entity} id {id}"),
        }
    }
}

/// The modelled `ValidationException` (HTTP 400) for a refused lens document.
///
/// Either way the request carried a document and the document is not a lens, so the request
/// never became what it had to be: the envelope is the one this service answers a request it
/// could not READ with ([`unstated_reason_exception`]) — `Reason` carried unclassified, and
/// no field finding. What they state differs with what the service knows: a value that is no
/// lens document at all is answered by naming the member it arrived in
/// ([`unparsable_document_exception`]), and a document that IS one states the rule of the
/// format it broke ([`LensJsonDefect::phrase`]). Built through the same constructors every
/// other refusal in this service uses, so the envelope cannot drift.
pub fn lens_json_exception(
    rejection: &LensJsonRejection,
) -> crate::ops::types::ValidationException {
    match rejection {
        LensJsonRejection::Unreadable => unparsable_document_exception(LENS_JSON_MEMBER),
        LensJsonRejection::Defect(defect) => unstated_reason_exception(defect.phrase()),
    }
}

/// `LensName`'s modelled length bounds (`smithy_query(query="shape", shape="Lens")`:
/// `Name` targets `LensName`, length 1..128), which the lens document's `name` becomes.
const LENS_NAME_LENGTH: (usize, usize) = (1, 128);

/// `LensDescription`'s modelled length bounds (length 1..1024), which the lens
/// document's `description` becomes.
const LENS_DESCRIPTION_LENGTH: (usize, usize) = (1, 1024);

/// `ChoiceContent$DisplayText`'s modelled length bounds
/// (`smithy_query(query="shape", shape="ChoiceContent")`: length 1..1024), which the
/// `displayText` of a choice's content entry becomes.
const LENS_CHOICE_CONTENT_DISPLAY_TEXT_LENGTH: (usize, usize) = (1, 1024);

/// `ChoiceContent$Url`'s modelled length bounds (length 1..1024), which the `url` of a
/// choice's content entry becomes.
const LENS_CHOICE_CONTENT_URL_LENGTH: (usize, usize) = (1, 1024);

/// The `improvementPlan` property of a choice in the lens JSON format specification: the
/// per-choice remediation the lens author writes, which `Choice$ImprovementPlan` and
/// `ChoiceImprovementPlan` report.
const LENS_JSON_IMPROVEMENT_PLAN: &str = "improvementPlan";

/// The `helpfulResource` property of a question or a choice in the lens JSON format
/// specification: the resource the lens author points a reader at, which
/// `Choice$HelpfulResource` reports for a choice.
const LENS_JSON_HELPFUL_RESOURCE: &str = "helpfulResource";

/// `QuestionDescription`'s modelled length bounds
/// (`smithy_query(query="shape", shape="QuestionDescription")`: length 1..1024), which a
/// question's `description` in the lens document becomes.
const LENS_QUESTION_DESCRIPTION_LENGTH: (usize, usize) = (1, 1024);

/// The `id` the format identifies a pillar, a question and a choice by, and the bounds it
/// is read within: an id is what a rule, an answer and a report name one by, and every
/// `PillarId` / `QuestionId` / `ChoiceId` this API reports is modelled 1..128
/// (`smithy_query(query="shape", shape="PillarId")`).
const LENS_JSON_ID: &str = "id";
const LENS_JSON_ID_LENGTH: (usize, usize) = (1, 128);

/// The format's own name for ONE entry of the `pillars` list, of a pillar's `questions`
/// list and of a question's `choices` list — how a refusal names the entry whose id is
/// declared twice.
const LENS_JSON_PILLAR: &str = "pillar";
const LENS_JSON_QUESTION: &str = "question";
const LENS_JSON_CHOICE: &str = "choice";

/// The defect for a property the document does not declare.
fn missing(property: &'static str) -> LensJsonRejection {
    LensJsonRejection::Defect(LensJsonDefect::Missing { property })
}

/// The defect for a list property the document declares no entry in.
fn empty_list(property: &'static str) -> LensJsonRejection {
    LensJsonRejection::Defect(LensJsonDefect::EmptyList { property })
}

/// The defect for a text property the document declares empty.
fn empty_text(property: &'static str) -> LensJsonRejection {
    LensJsonRejection::Defect(LensJsonDefect::EmptyText { property })
}

/// The defect for a property declared with a value the format does not admit there.
fn invalid(property: &'static str) -> LensJsonRejection {
    LensJsonRejection::Defect(LensJsonDefect::Invalid { property })
}

/// The member `property` of `object`, or the defect that reports the document does not
/// declare it. A property declared with no value is one the document does not declare.
fn lens_json_member<'doc>(
    object: &'doc ::serde_json::Value,
    property: &'static str,
) -> Result<&'doc ::serde_json::Value, LensJsonRejection> {
    match object.get(property) {
        Some(::serde_json::Value::Null) | None => Err(missing(property)),
        Some(value) => Ok(value),
    }
}

/// A required string property of a lens document, within `bounds` characters.
///
/// A property the document declares nothing in is the EMPTY defect rather than the invalid
/// one: what the author has to fix is that the property says nothing, not that its value is
/// of the wrong shape.
fn lens_json_string(
    object: &::serde_json::Value,
    property: &'static str,
    bounds: (usize, usize),
) -> Result<String, LensJsonRejection> {
    let value = lens_json_member(object, property)?;
    let text = value.as_str().ok_or_else(|| invalid(property))?;
    let (min, max) = bounds;
    // The format bounds a string in CHARACTERS, as every modelled `@length` does.
    let length = text.chars().count();
    if length == 0 {
        return Err(empty_text(property));
    }
    if length < min || length > max {
        return Err(invalid(property));
    }
    Ok(text.to_string())
}

/// One CONTENT entry of a lens document — a `{displayText, url}` object the format
/// declares on a choice or on a question — or `None` when the document declares none under
/// `property`.
///
/// The entry says nothing without its text, so `displayText` is required of an entry that
/// is there at all, and `url` is optional: both are read within the bounds the modelled
/// `ChoiceContent` carries for them, so a document cannot declare content this service
/// would then report outside the shape that reports it.
fn lens_json_choice_content(
    object: &::serde_json::Value,
    property: &'static str,
) -> Result<Option<LensChoiceContent>, LensJsonRejection> {
    let entry = match object.get(property) {
        None | Some(::serde_json::Value::Null) => return Ok(None),
        Some(entry) => entry,
    };
    if !entry.is_object() {
        return Err(invalid(property));
    }
    let display_text = lens_json_string(
        entry,
        "displayText",
        LENS_CHOICE_CONTENT_DISPLAY_TEXT_LENGTH,
    )?;
    let url = match entry.get("url") {
        None | Some(::serde_json::Value::Null) => None,
        Some(_) => Some(lens_json_string(entry, "url", LENS_CHOICE_CONTENT_URL_LENGTH)?),
    };
    Ok(Some(LensChoiceContent { display_text, url }))
}

/// A list property the format requires an entry of.
///
/// A document that leaves the property out, declares no value in it or declares it as an
/// empty list breaks ONE rule — the lens declares none of what the list is a list of — so
/// all three are the EMPTY defect; a property declared as something that is not a list at
/// all is the invalid one.
fn lens_json_array<'doc>(
    object: &'doc ::serde_json::Value,
    property: &'static str,
) -> Result<&'doc Vec<::serde_json::Value>, LensJsonRejection> {
    let value = match object.get(property) {
        None | Some(::serde_json::Value::Null) => return Err(empty_list(property)),
        Some(value) => value,
    };
    let items = value.as_array().ok_or_else(|| invalid(property))?;
    if items.is_empty() {
        return Err(empty_list(property));
    }
    Ok(items)
}

/// Record `id` as taken within the list that declares it, or refuse it as a duplicate.
///
/// `entity` is the format's own name for one entry of that list: a rule, an answer and a
/// report all name a question or a choice by its id, so two of them cannot share one, and
/// the refusal names the id that is declared twice because that is what tells the author
/// which entry to fix.
fn lens_json_unique(
    seen: &mut std::collections::BTreeSet<String>,
    id: &str,
    entity: &'static str,
) -> Result<(), LensJsonRejection> {
    if !seen.insert(id.to_string()) {
        return Err(LensJsonRejection::Defect(LensJsonDefect::DuplicateId {
            entity,
            id: id.to_string(),
        }));
    }
    Ok(())
}

/// The custom lens a JSON document defines, or the rule of the lens JSON format
/// specification it broke.
///
/// The properties are read in the order the format declares them, so a document breaking
/// several rules is refused for the first of them a reader of the document meets.
///
/// The `@length(2..500000)` on `LensJSON` ("A custom lens cannot exceed 500 KB in size")
/// is a modelled constraint, so the boundary validator refuses an oversized document
/// before any of this runs.
pub fn parse_lens_json(document: &str) -> Result<LensDefinition, LensJsonRejection> {
    // A value that is not a lens document at all — not JSON, or JSON that is no object —
    // is refused by naming the member alone: the decoder's own diagnostic is not part of
    // the API contract, so it is not built into a message, and no rule about a lens was
    // reached.
    let value: ::serde_json::Value =
        ::serde_json::from_str(document).map_err(|_| LensJsonRejection::Unreadable)?;
    if !value.is_object() {
        return Err(LensJsonRejection::Unreadable);
    }

    let schema_version = lens_json_string(&value, "schemaVersion", (1, 32))?;
    if schema_version != LENS_SCHEMA_VERSION {
        return Err(invalid("schemaVersion"));
    }
    let name = lens_json_string(&value, "name", LENS_NAME_LENGTH)?;
    let description = match value.get("description") {
        None | Some(::serde_json::Value::Null) => None,
        Some(_) => Some(lens_json_string(
            &value,
            "description",
            LENS_DESCRIPTION_LENGTH,
        )?),
    };

    let mut pillar_ids = std::collections::BTreeSet::new();
    let mut question_ids = std::collections::BTreeSet::new();
    let mut pillars = Vec::new();
    for pillar_value in lens_json_array(&value, "pillars")? {
        let pillar_id = lens_json_string(pillar_value, LENS_JSON_ID, LENS_JSON_ID_LENGTH)?;
        lens_json_unique(&mut pillar_ids, &pillar_id, LENS_JSON_PILLAR)?;
        let pillar_name = lens_json_string(pillar_value, "name", LENS_NAME_LENGTH)?;

        let mut questions = Vec::new();
        for question_value in lens_json_array(pillar_value, "questions")? {
            let question_id = lens_json_string(question_value, LENS_JSON_ID, LENS_JSON_ID_LENGTH)?;
            lens_json_unique(&mut question_ids, &question_id, LENS_JSON_QUESTION)?;
            let title = lens_json_string(question_value, "title", (1, 1024))?;
            // "The description of the question" (`QuestionDescription`) and the question's
            // own helpful resource: optional properties of the lens JSON format
            // specification, kept because the lens document is the resource — what the
            // document declares about a question is what an export of the lens reports
            // ([`canonical_lens_json`]).
            let description = match question_value.get("description") {
                None | Some(::serde_json::Value::Null) => None,
                Some(_) => Some(lens_json_string(
                    question_value,
                    "description",
                    LENS_QUESTION_DESCRIPTION_LENGTH,
                )?),
            };
            let helpful_resource =
                lens_json_choice_content(question_value, LENS_JSON_HELPFUL_RESOURCE)?;

            let mut choice_ids = std::collections::BTreeSet::new();
            let mut choices = Vec::new();
            for choice_value in lens_json_array(question_value, "choices")? {
                let choice_id = lens_json_string(choice_value, LENS_JSON_ID, LENS_JSON_ID_LENGTH)?;
                lens_json_unique(&mut choice_ids, &choice_id, LENS_JSON_CHOICE)?;
                choices.push(LensChoice {
                    id: choice_id,
                    title: lens_json_string(choice_value, "title", (1, 1024))?,
                    // The lens JSON format specification carries no per-choice prose, so
                    // an imported lens publishes none.
                    description: None,
                    // "The helpful resource details" the lens AUTHOR writes for this best
                    // practice: the document's own `helpfulResource` entry, kept for the
                    // same reason its improvement plan is.
                    helpful_resource: lens_json_choice_content(
                        choice_value,
                        LENS_JSON_HELPFUL_RESOURCE,
                    )?,
                    // "The improvement plan details" the lens AUTHOR writes for this best
                    // practice: the document's own `improvementPlan` entry, kept so the
                    // question's choice catalog and the improvement plan of a review of
                    // this lens report what the document declares.
                    improvement_plan: lens_json_choice_content(
                        choice_value,
                        LENS_JSON_IMPROVEMENT_PLAN,
                    )?,
                });
            }

            let rule_values = lens_json_array(question_value, "riskRules")?;
            let mut risk_rules = Vec::new();
            for (rule_index, rule_value) in rule_values.iter().enumerate() {
                let condition =
                    lens_json_string(rule_value, "condition", LENS_JSON_CONDITION_LENGTH)?;
                let risk = lens_json_string(rule_value, "risk", (1, 32))?;
                if !LENS_JSON_RISK_VALUES.contains(&risk.as_str()) {
                    return Err(invalid("risk"));
                }
                // The last rule is the one that applies when no other matched, so it
                // carries the default condition and no other rule may: a `riskRules` list
                // ordered any other way is one the format does not admit.
                let is_last = rule_index + 1 == rule_values.len();
                let is_default = condition == LENS_JSON_DEFAULT_CONDITION;
                if is_last != is_default {
                    return Err(invalid("riskRules"));
                }
                risk_rules.push(LensRiskRule { condition, risk });
            }

            questions.push(LensQuestion {
                id: question_id,
                title,
                description,
                helpful_resource,
                choices,
                risk_rules,
            });
        }

        pillars.push(LensPillar {
            id: pillar_id,
            name: pillar_name,
            questions,
        });
    }

    Ok(LensDefinition {
        schema_version,
        name,
        description,
        pillars,
    })
}
