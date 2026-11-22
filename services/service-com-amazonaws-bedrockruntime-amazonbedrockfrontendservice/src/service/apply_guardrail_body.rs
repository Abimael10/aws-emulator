// SPDX-License-Identifier: CC0-1.0

//! The `ApplyGuardrail` success body as Amazon Bedrock Runtime's own wire carries it:
//! the two members its answer renders that this checkout's Smithy model does not
//! declare, and the one member of a word-policy assessment its answer reports only
//! where that list placed a word.
//!
//! # The gap this closes
//!
//! The model a service crate is generated from is a SNAPSHOT of the API, and the live
//! wire has moved past it here. Beside the members the model declares
//! (`smithy_query(query="shape",shape="ApplyGuardrailResponse")` lists `usage`,
//! `action`, `actionReason`, `outputs`, `assessments` and `guardrailCoverage`), the
//! answer carries:
//!
//! * `output` — the text the guardrail answers with, the same list `outputs` carries,
//!   under the second name the wire carries it under. It is written from the value
//!   `outputs` already holds, so the two can never disagree, and it is present on
//!   every answer: a client reading it unconditionally never has to tell an absent
//!   member from the empty list an evaluation that answered nothing reports.
//! * `blockedResponse` — the text a guardrail that BLOCKED answers with, which is the
//!   revision's own blocked messaging ([`crate::state::guardrail_registry`]) and is
//!   exactly what `outputs` carries for that verdict. It is on the answer of a
//!   guardrail that blocked the content and of no other: which verdict this is, the
//!   answer itself states, in the reason it gives for the action it took
//!   ([`ACTION_REASON_BLOCKED`]).
//!
//! The other direction is the WORD-POLICY assessment: its managed word lists are
//! reported where a managed list placed a word, and the member is absent where none
//! did — unlike the sensitive-information assessment, whose entry lists the wire
//! reports empty. A member of an `@required` list cannot be dropped through the
//! generated shape either (a required member is not an `Option` field), so both halves
//! of the rule live here.
//!
//! # Where it is applied
//!
//! A member the model does not declare cannot be a field of a generated shape, so it
//! cannot be written from the handler or through the schema serializer. It is written
//! HERE instead, over the body the framework's own serializer just wrote, at the seam
//! [`crate::service`] already renders this one operation's answer through. Nothing
//! else about the response changes: the status, the headers and every other byte of
//! the body stay as the renderer wrote them.
//!
//! The rewrite ITSELF is not this module's: the body is read and written back value by
//! value through the shared plumbing every service whose wire carries an unmodelled
//! member rewrites a rendered success body with
//! ([`service_common::success_body_members`]), so every value the renderer wrote is
//! written back byte for byte — a grounding score's own digits above all, which no
//! re-rendering of a number may touch — and a response this rule owns nothing about, a
//! failure included, is left exactly as the renderer wrote it. What this module states is
//! WHICH members the wire carries and what they hold.
//!
//! What one call does is bounded by the body the renderer just wrote: one evaluation's
//! answered blocks and its one assessment, each read once, and nothing here reads a
//! size out of the request.
//!
//! REMOVE the two added members when the model is refreshed to a snapshot that
//! declares them: they are then fields of the output shape like every other, and
//! `tests/all/apply_guardrail.rs` pins the wire form either way.

use ::aws_smithy_runtime_api::http::Response as RtResponse;
use ::serde_json::value::RawValue;
use ::service_common::success_body_members::{raw_value, with_rewritten_success_body, RawObject};

use crate::state::guardrail_evaluation::{ACTION_INTERVENED, ACTION_REASON_BLOCKED};

/// The operation whose answer this rule owns, as the routing table names it.
const APPLY_GUARDRAIL: &str = "ApplyGuardrail";

/// `outputs` — the modelled member carrying the text the guardrail answers with.
const OUTPUTS_MEMBER: &str = "outputs";

/// `output` — the same list, under the other name the wire carries it under.
const OUTPUT_MEMBER: &str = "output";

/// `blockedResponse` — the text a guardrail that blocked the content answers with.
const BLOCKED_RESPONSE_MEMBER: &str = "blockedResponse";

/// `action` and `actionReason` — between them, how the answer states that the guardrail
/// BLOCKED the content rather than masking it or leaving it alone. Both verdicts
/// intervene and both answer one block of text, so the reason is what tells them apart;
/// the sentence is not spelled here but read from the one constant the evaluation states
/// it with ([`ACTION_REASON_BLOCKED`]), so a rewording cannot leave the two disagreeing.
const ACTION_MEMBER: &str = "action";
const ACTION_REASON_MEMBER: &str = "actionReason";

/// `GuardrailOutputContent$text` — the text one answered block carries.
const TEXT_MEMBER: &str = "text";

/// `assessments`, and the word policy inside each one.
const ASSESSMENTS_MEMBER: &str = "assessments";
const WORD_POLICY_MEMBER: &str = "wordPolicy";
const MANAGED_WORD_LISTS_MEMBER: &str = "managedWordLists";

/// The text one of this answer's own members carries, or `None` for a member it does not
/// carry or does not carry a string in.
fn text(object: &RawObject<'_>, member: &str) -> Option<String> {
    let held = object.get(member)?;
    ::serde_json::from_str::<String>(held.get()).ok()
}

/// Whether this is the answer of a guardrail that BLOCKED the content, as the two
/// modelled members the verdict states together say.
fn blocked(object: &RawObject<'_>) -> bool {
    text(object, ACTION_MEMBER).as_deref() == Some(ACTION_INTERVENED)
        && text(object, ACTION_REASON_MEMBER).as_deref() == Some(ACTION_REASON_BLOCKED)
}

/// The text one answered block carries, still the bytes it was read as.
fn text_of(block: &RawValue) -> Option<&RawValue> {
    let object: RawObject = ::serde_json::from_str(block.get()).ok()?;
    object.get(TEXT_MEMBER).copied()
}

/// The word-policy assessment `policy` with an EMPTY managed word list dropped, or
/// `None` when it reports one that placed a word (or reports none at all), which is
/// left exactly as the renderer wrote it.
fn reported_word_policy(policy: &RawValue) -> Option<Vec<u8>> {
    let mut object: RawObject = ::serde_json::from_str(policy.get()).ok()?;
    let listed: Vec<&RawValue> =
        ::serde_json::from_str(object.get(MANAGED_WORD_LISTS_MEMBER)?.get()).ok()?;
    if !listed.is_empty() {
        return None;
    }
    object.remove(MANAGED_WORD_LISTS_MEMBER);
    ::serde_json::to_vec(&object).ok()
}

/// The assessment list `assessed` with each word policy reported as
/// [`reported_word_policy`] states, or `None` when none of them reported an empty
/// managed word list.
fn reported_assessments(assessed: &RawValue) -> Option<Vec<u8>> {
    let read: Vec<&RawValue> = ::serde_json::from_str(assessed.get()).ok()?;
    let mut dropped = false;
    // Each assessment's own rewritten bytes, kept alive while the list that borrows
    // them is written.
    let mut rewritten: Vec<Vec<u8>> = Vec::with_capacity(read.len());
    for assessment in read {
        let mut object: RawObject = ::serde_json::from_str(assessment.get()).ok()?;
        let reported = object
            .get(WORD_POLICY_MEMBER)
            .copied()
            .and_then(reported_word_policy);
        if let Some(bytes) = &reported {
            object.insert(WORD_POLICY_MEMBER.to_string(), raw_value(bytes)?);
            dropped = true;
        }
        rewritten.push(::serde_json::to_vec(&object).ok()?);
    }
    if !dropped {
        return None;
    }
    let listed: Vec<&RawValue> = rewritten
        .iter()
        .map(|assessment| raw_value(assessment))
        .collect::<Option<Vec<_>>>()?;
    ::serde_json::to_vec(&listed).ok()
}

/// `body` as this service sends it for one applied guardrail, or `None` when this rule
/// owns nothing about it: a body that is not a JSON object, one that reports no
/// evaluation to answer for, or one that already carries everything this rule writes.
#[must_use]
pub(super) fn served_success_body(body: &[u8]) -> Option<Vec<u8>> {
    let mut object: RawObject = ::serde_json::from_slice(body).ok()?;
    // The text the guardrail answered with, which is what both added members report.
    let outputs = object.get(OUTPUTS_MEMBER).copied()?;
    let answered: Vec<&RawValue> = ::serde_json::from_str(outputs.get()).ok()?;
    // Rewritten before the enclosing object is written to, so the new bytes outlive
    // the member that borrows them.
    let assessed = object
        .get(ASSESSMENTS_MEMBER)
        .copied()
        .and_then(reported_assessments);

    let mut written = false;
    if !object.contains_key(OUTPUT_MEMBER) {
        object.insert(OUTPUT_MEMBER.to_string(), outputs);
        written = true;
    }
    if blocked(&object) && !object.contains_key(BLOCKED_RESPONSE_MEMBER) {
        // The revision's blocked messaging, which is the text this verdict answers
        // with: taken from the answer's own outputs rather than written a second time,
        // so the two members cannot report different text.
        if let Some(text) = answered.first().copied().and_then(text_of) {
            object.insert(BLOCKED_RESPONSE_MEMBER.to_string(), text);
            written = true;
        }
    }
    if let Some(bytes) = &assessed {
        object.insert(ASSESSMENTS_MEMBER.to_string(), raw_value(bytes)?);
        written = true;
    }
    if !written {
        return None;
    }
    ::serde_json::to_vec(&object).ok()
}

/// `resp` as this service answers an applied guardrail: a success carrying the members
/// the live wire renders ([`served_success_body`]).
///
/// A response this rule owns nothing about is returned untouched — status, headers and
/// every byte of the body exactly as the renderer wrote them. A failure is one of those:
/// it reports no evaluation, and this service's refusal wording is
/// [`super::as_this_service_answers_a_guardrail`]'s.
#[must_use]
pub(super) fn with_served_success_body(resp: RtResponse) -> RtResponse {
    // The seam this runs at serves the one URI space that applies a guardrail, and the
    // operation the request was routed to is what decides whether this rule owns the
    // answer at all.
    if ::service_common::restated_refusal::served_operation().as_deref() != Some(APPLY_GUARDRAIL) {
        return resp;
    }
    with_rewritten_success_body(resp, served_success_body)
}

#[cfg(test)]
mod tests {
    //! The rewriting rule, driven on bodies built by hand. The wire behaviour it adds
    //! up to — which members an applied guardrail's answer carries for which verdict,
    //! over the operation's own wire — is `tests/all/apply_guardrail.rs`.

    use super::*;
    use ::serde_json::Value;

    /// The answer of one verdict: the modelled members a rendered body carries.
    fn answer(reason: &str, outputs: &str) -> String {
        format!(
            r#"{{"action":"{ACTION_INTERVENED}","actionReason":"{reason}","outputs":{outputs},
               "assessments":[{{"wordPolicy":{{"customWords":[{{"match":"zephyrine"}}],
               "managedWordLists":[]}}}}]}}"#
        )
    }

    /// RULE: the text the guardrail answered with is reported under both names the wire
    /// carries it under, and under `blockedResponse` as well on the answer of a
    /// guardrail that BLOCKED. The rows differ along the verdict and along how many
    /// blocks the guardrail answered with.
    #[test]
    fn the_answered_text_is_reported_under_every_name_the_wire_carries_it_under() {
        let rows: [(&str, &str, &str, Option<&str>); 4] = [
            (
                "blocked",
                ACTION_REASON_BLOCKED,
                r#"[{"text":"Sorry, the model cannot answer this question."}]"#,
                Some("Sorry, the model cannot answer this question."),
            ),
            (
                "masked",
                "Guardrail masked.",
                r#"[{"text":"call {order-id} back"}]"#,
                None,
            ),
            ("no action", "No action.", "[]", None),
            (
                "blocked, several blocks",
                ACTION_REASON_BLOCKED,
                r#"[{"text":"first"},{"text":"second"}]"#,
                Some("first"),
            ),
        ];

        for (case, reason, outputs, expected_blocked) in rows {
            let body = answer(reason, outputs);
            let rewritten = served_success_body(body.as_bytes())
                .unwrap_or_else(|| panic!("case {case:?}: expected a rewritten body"));
            let document: Value =
                ::serde_json::from_slice(&rewritten).expect("the rewritten body is JSON");
            assert_eq!(
                document[OUTPUT_MEMBER], document[OUTPUTS_MEMBER],
                "case {case:?}: both names must report the same text, got {document}"
            );
            let answered = document[BLOCKED_RESPONSE_MEMBER].as_str();
            assert_eq!(
                answered, expected_blocked,
                "case {case:?}: expected the blocked response {expected_blocked:?}, got \
                 {answered:?} in {document}"
            );
        }
    }

    /// RULE: a word-policy assessment reports the managed word lists it PLACED, and
    /// reports the member not at all where none placed a word. The rows differ along
    /// what the policy placed.
    #[test]
    fn a_word_policy_reports_the_managed_lists_it_placed() {
        let rows: [(&str, &str, bool); 3] = [
            (
                "nothing managed placed",
                r#"{"customWords":[{"match":"zephyrine"}],"managedWordLists":[]}"#,
                false,
            ),
            (
                "a managed word placed",
                r#"{"customWords":[],"managedWordLists":[{"match":"damn","type":"PROFANITY"}]}"#,
                true,
            ),
            (
                "both placed",
                r#"{"customWords":[{"match":"zephyrine"}],
                   "managedWordLists":[{"match":"damn","type":"PROFANITY"}]}"#,
                true,
            ),
        ];

        for (case, policy, reported) in rows {
            let body = format!(
                r#"{{"action":"NONE","actionReason":"No action.","outputs":[],
                   "assessments":[{{"wordPolicy":{policy}}}]}}"#
            );
            let rewritten = served_success_body(body.as_bytes())
                .unwrap_or_else(|| panic!("case {case:?}: expected a rewritten body"));
            let document: Value =
                ::serde_json::from_slice(&rewritten).expect("the rewritten body is JSON");
            let policy = &document[ASSESSMENTS_MEMBER][0][WORD_POLICY_MEMBER];
            assert_eq!(
                !policy[MANAGED_WORD_LISTS_MEMBER].is_null(),
                reported,
                "case {case:?}: expected the member reported={reported}, got {policy}"
            );
            assert!(
                !policy["customWords"].is_null(),
                "case {case:?}: the custom words are reported whatever they placed, got {policy}"
            );
        }
    }

    /// RULE: every value the body already carried is written back byte for byte — the
    /// digits of a score above all, which no re-rendering of a number may touch. The
    /// rows differ along the kind of value sitting beside the answered text.
    #[test]
    fn every_value_the_body_already_carried_is_written_back_unchanged() {
        for (case, body, kept) in [
            (
                "a score of many digits",
                r#"{"outputs":[],"assessments":[{"contextualGroundingPolicy":{"filters":[{"score":0.6666666666666666}]}}]}"#,
                "0.6666666666666666",
            ),
            (
                "a trailing zero a shorter rendering would drop",
                r#"{"outputs":[],"assessments":[{"invocationMetrics":{"guardrailProcessingLatency":1.2500}}]}"#,
                "1.2500",
            ),
            (
                "the text of an answered block",
                r#"{"outputs":[{"text":"a \"quoted\" answer"}],"assessments":[]}"#,
                r#"a \"quoted\" answer"#,
            ),
        ] {
            let rewritten = served_success_body(body.as_bytes())
                .unwrap_or_else(|| panic!("case {case:?}: expected a rewritten body"));
            let rendered = String::from_utf8(rewritten).expect("the rewritten body is text");
            assert!(
                rendered.contains(kept),
                "case {case:?}: expected {kept:?} written back unchanged, got {rendered}"
            );
        }
    }

    /// Nothing to rewrite: a body that reports no evaluation, one that already carries
    /// what this rule writes, and one that is not JSON at all.
    #[test]
    fn a_body_this_rule_does_not_own_is_left_alone() {
        for (case, body) in [
            ("no outputs reported", r#"{"action":"NONE"}"#),
            (
                "already served",
                r#"{"action":"NONE","outputs":[],"output":[],"assessments":[]}"#,
            ),
            ("not JSON", "<Error/>"),
        ] {
            assert!(
                served_success_body(body.as_bytes()).is_none(),
                "case {case:?}: the body must be left alone"
            );
        }
    }

    /// The answer of a guardrail that only MASKED carries no blocked messaging, even
    /// where the reason it states is the blocked one under another action: both members
    /// the verdict states have to say it blocked.
    #[test]
    fn the_blocked_messaging_is_reported_only_where_both_members_say_it_blocked() {
        for (case, action, reason) in [
            ("intervened, masked", ACTION_INTERVENED, "Guardrail masked."),
            ("no action at all", "NONE", "No action."),
            ("no action, blocked reason", "NONE", ACTION_REASON_BLOCKED),
        ] {
            let body = format!(
                r#"{{"action":"{action}","actionReason":"{reason}",
                   "outputs":[{{"text":"some text"}}],"assessments":[]}}"#
            );
            let rewritten = served_success_body(body.as_bytes())
                .unwrap_or_else(|| panic!("case {case:?}: expected a rewritten body"));
            let document: Value =
                ::serde_json::from_slice(&rewritten).expect("the rewritten body is JSON");
            assert!(
                document[BLOCKED_RESPONSE_MEMBER].is_null(),
                "case {case:?}: no blocked messaging is reported, got {document}"
            );
        }
    }
}
