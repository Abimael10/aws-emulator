// SPDX-License-Identifier: CC0-1.0

//! PUBLISHING a guardrail to the inference plane: the projection of a stored
//! guardrail onto the policies that plane APPLIES.
//!
//! WHY. Applying a guardrail is a data-plane call — `ApplyGuardrail` names a
//! guardrail revision and evaluates submitted content against it, and a Converse-API
//! request names one in its own `guardrailConfig` — so the policies have to be
//! readable where inference runs. The two planes keep ONE copy of them, in the
//! inference plane's own store, exactly as the account-enforced guardrail's
//! enforcement is kept there ([`enforce_guardrail_on_inference`]): this service holds
//! the guardrail RESOURCE (its name, tags, key, timestamps and status), and what it
//! writes across is the evaluable part of each revision.
//!
//! WHEN. On every request that changes what a revision of a guardrail holds — the
//! create, the update and the version cut — and the withdrawal on the delete of the
//! guardrail or of one of its versions. A guardrail that is gone applies to nothing.

use super::*;

/// Publish `guardrail` — every revision of it — as the inference plane applies it,
/// replacing whatever stood under its id.
///
/// Answers whether the inference plane's store was reached: an emulator assembled
/// without the Bedrock Runtime data plane applies no guardrail, and this control
/// plane's own record of the guardrail is unaffected either way.
///
/// Call it with NO store of this service held, so two services' stores are never
/// locked at once.
pub fn publish_guardrail_to_inference<Eng: Send + Sync + 'static>(
    engine: &Eng,
    guardrail: &Guardrail,
) -> bool {
    runtime_registry::publish(
        engine,
        runtime_registry::PublishedGuardrail {
            id: guardrail.id.clone(),
            arn: guardrail.arn.clone(),
            revisions: guardrail
                .revisions()
                .map(|revision| (revision.version.clone(), applied_policies(revision)))
                .collect(),
        },
    )
}

/// Withdraw the guardrail `guardrail_id` names from the inference plane, which the
/// delete of the whole guardrail does.
pub fn withdraw_guardrail_from_inference<Eng: Send + Sync + 'static>(
    engine: &Eng,
    guardrail_id: &str,
) -> bool {
    runtime_registry::withdraw(engine, guardrail_id)
}

/// One stored revision as the inference plane APPLIES it: the two blocked-messaging
/// strings, and each policy as the entries that plane evaluates, carrying what each
/// entry does on each side.
///
/// A policy the revision does not configure contributes no entries, which is what
/// leaves it unevaluated and uncharged.
fn applied_policies(revision: &GuardrailRevision) -> runtime_registry::GuardrailPolicies {
    use runtime_registry::{PolicyAction, Sides};

    let word_policy = revision.word_policy.as_ref();
    let sensitive = revision.sensitive_information_policy.as_ref();
    runtime_registry::GuardrailPolicies {
        blocked_input_messaging: revision.blocked_input_messaging.clone(),
        blocked_outputs_messaging: revision.blocked_outputs_messaging.clone(),
        topics: revision
            .topic_policy
            .as_ref()
            .map(|policy| policy.topics.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|topic| runtime_registry::DeniedTopic {
                name: topic.name.clone(),
                // `GuardrailTopicType` declares one value, which is what a topic a
                // guardrail configures is; a revision that names none is still a
                // denied topic.
                kind: topic
                    .r#type
                    .clone()
                    .unwrap_or_else(|| runtime_registry::TOPIC_TYPE_DENY.to_string()),
                definition: topic.definition.clone(),
                examples: topic.examples.clone().unwrap_or_default(),
                sides: Sides::new(
                    topic.input_action.as_deref(),
                    topic.input_enabled,
                    topic.output_action.as_deref(),
                    topic.output_enabled,
                    PolicyAction::Block,
                ),
            })
            .collect(),
        content_filters: revision
            .content_policy
            .as_ref()
            .and_then(|policy| policy.filters.as_ref())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|filter| runtime_registry::FilterEntry {
                category: filter.r#type.clone(),
                input_strength: filter.input_strength.clone(),
                output_strength: filter.output_strength.clone(),
                sides: Sides::new(
                    filter.input_action.as_deref(),
                    filter.input_enabled,
                    filter.output_action.as_deref(),
                    filter.output_enabled,
                    PolicyAction::Block,
                ),
            })
            .collect(),
        words: word_policy
            .and_then(|policy| policy.words.as_ref())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|word| runtime_registry::WordEntry {
                text: word.text.clone(),
                sides: Sides::new(
                    word.input_action.as_deref(),
                    word.input_enabled,
                    word.output_action.as_deref(),
                    word.output_enabled,
                    PolicyAction::Block,
                ),
            })
            .collect(),
        managed_word_lists: word_policy
            .and_then(|policy| policy.managed_word_lists.as_ref())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|list| runtime_registry::ManagedWordEntry {
                kind: list.r#type.clone(),
                sides: Sides::new(
                    list.input_action.as_deref(),
                    list.input_enabled,
                    list.output_action.as_deref(),
                    list.output_enabled,
                    PolicyAction::Block,
                ),
            })
            .collect(),
        pii_entities: sensitive
            .and_then(|policy| policy.pii_entities.as_ref())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|entity| runtime_registry::PiiEntry {
                entity_type: entity.r#type.clone(),
                // A sensitive-information entry carries the action for the entry as a
                // whole (`GuardrailPiiEntity$action`, `@required`), which is what each
                // side takes where that side names none of its own — except that a
                // masking one configures the response side alone ([`sensitive_sides`]).
                sides: sensitive_sides(
                    entity.input_action.as_deref(),
                    entity.input_enabled,
                    entity.output_action.as_deref(),
                    entity.output_enabled,
                    &entity.action,
                ),
            })
            .collect(),
        regexes: sensitive
            .and_then(|policy| policy.regexes.as_ref())
            .map(Vec::as_slice)
            .unwrap_or_default()
            .iter()
            .map(|regex| runtime_registry::RegexEntry {
                name: regex.name.clone(),
                pattern: regex.pattern.clone(),
                sides: sensitive_sides(
                    regex.input_action.as_deref(),
                    regex.input_enabled,
                    regex.output_action.as_deref(),
                    regex.output_enabled,
                    &regex.action,
                ),
            })
            .collect(),
        grounding_filters: revision
            .contextual_grounding_policy
            .as_ref()
            .map(|policy| policy.filters.as_slice())
            .unwrap_or_default()
            .iter()
            .map(|filter| runtime_registry::GroundingEntry {
                kind: filter.r#type.clone(),
                threshold: filter.threshold,
                action: runtime_registry::PolicyAction::of(
                    filter.action.as_deref(),
                    filter.enabled,
                    PolicyAction::Block,
                ),
            })
            .collect(),
    }
}

/// The sides a sensitive-information entry — a PII entity type or a regular expression — is
/// evaluated on, and what it does on each.
///
/// The entry's own `action` is what a side takes where that side names none of its own
/// (`GuardrailPiiEntityConfig$action` / `GuardrailRegexConfig$action`, both `@required`),
/// with ONE asymmetry: masking REWRITES the text the guardrail answers with ("ANONYMIZE –
/// Mask the content and replace it with identifier tags"), which is a transform of the
/// RESPONSE, so an entry whose action is the masking one configures the response side and
/// the request side of it is evaluated only where the configuration asks for a request-side
/// evaluation of its own — a request-side action, or request-side evaluation enabled
/// (`GuardrailPiiEntityConfig$inputAction`, `$inputEnabled`). An entry no side of a given
/// evaluation holds is not evaluated there, so it is neither charged for nor reported there
/// ("When disabled, you aren't charged for the evaluation. The evaluation doesn't appear in
/// the response.").
fn sensitive_sides(
    input_action: Option<&str>,
    input_enabled: Option<bool>,
    output_action: Option<&str>,
    output_enabled: Option<bool>,
    action: &str,
) -> runtime_registry::Sides {
    let default = sensitive_action(action);
    let mut sides = runtime_registry::Sides::new(
        input_action,
        input_enabled,
        output_action,
        output_enabled,
        default,
    );
    if default.anonymizes() && input_action.is_none() && input_enabled.is_none() {
        sides.input = None;
    }
    sides
}

/// The action a sensitive-information entry's own `action` names.
///
/// `GuardrailSensitiveInformationAction` blocks the content, masks the value it
/// matched, or takes no action and reports the detection.
fn sensitive_action(action: &str) -> runtime_registry::PolicyAction {
    match action {
        runtime_registry::CONFIG_ACTION_ANONYMIZE => runtime_registry::PolicyAction::Anonymize,
        runtime_registry::CONFIG_ACTION_NONE => runtime_registry::PolicyAction::Detect,
        _ => runtime_registry::PolicyAction::Block,
    }
}
