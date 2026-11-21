// SPDX-License-Identifier: CC0-1.0

//! What a served turn reports as the CAUSE of its stop, and in whose vocabulary.
//!
//! Every inference surface of this service reads its stop reason from here, so
//! the four cannot report the same turn as having ended differently: `Converse`
//! and `ConverseStream` report the modelled `StopReason`
//! ([`converse_stop_reason`]), and `InvokeModel` and
//! `InvokeModelWithResponseStream` report the invoked model family's OWN
//! terminal word inside that family's response envelope
//! ([`family_stop_reason`]).
//!
//! Which word a turn carries is the MODEL's, not the envelope's: the service
//! passes on the reason the model that generated the turn reports for it, so a
//! cause two families word differently is reported differently for the same
//! request. The cause itself is [`TurnStop`] — a completion that had nothing
//! left to say, one the request's generation cap cut off, and one that ended on
//! a stop sequence the request configured.
//!
//! The families differ on that last cause in particular: a family whose own
//! response fields name the generated stop sequence reports a turn halted on one
//! as its own distinct reason, while a family that publishes no such reason
//! reports the halt as the ordinary end of a turn — the OpenAI-compatible schema
//! states that of its own `stop`, which covers a natural stopping point and a
//! supplied stop sequence alike — and the modelled member such a turn carries is
//! therefore `end_turn`, which is what the Amazon Nova models report for one.
//! Of the families this emulator renders envelopes for, the Anthropic Claude
//! Messages API is the one that names the sequence, in the `stop_sequence`
//! response field beside its `stop_reason`.
//!
//! Sources: each provider's response fields in "Inference request parameters and
//! response fields for foundation models" in the Amazon Bedrock User Guide; the
//! modelled members are `smithy_query(query="shape", shape="StopReason")`.

use super::{openai_chat, ModelFamily};

/// What ended one served turn's generation.
///
/// It is the cause, not the word: the word a surface reports for it is
/// [`converse_stop_reason`]'s or [`family_stop_reason`]'s. A turn that halted on
/// a configured stop sequence carries that sequence, which is what a family
/// whose response fields name the generated sequence reports beside its stop
/// reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnStop<'a> {
    /// The completion had nothing left to say.
    Finished,
    /// The generation cap the request allowed cut the completion off: there was
    /// more of it to say and the whole budget had been spent.
    TokenCap,
    /// One of the request's configured stop sequences completed inside the text,
    /// so generation ended ON that sequence — the one carried here.
    StopSequence(&'a str),
}

impl<'a> TurnStop<'a> {
    /// What ended a turn the generation stage reports as having run into the cap
    /// (`token_cap`) or as having halted on a configured sequence (`halted_on`);
    /// a turn that did neither finished.
    ///
    /// Generation ends the moment a configured sequence completes, so a halted
    /// turn never reached the cap however much of the budget it spent, and the
    /// halt is what it reports.
    #[must_use]
    pub fn of(token_cap: bool, halted_on: Option<&'a str>) -> Self {
        match (halted_on, token_cap) {
            (Some(sequence), _) => Self::StopSequence(sequence),
            (None, true) => Self::TokenCap,
            (None, false) => Self::Finished,
        }
    }
}

/// The modelled `StopReason` for a turn that ended for its own reasons rather
/// than because the request asked for a tool or a guardrail intervened.
pub const STOP_REASON_END_TURN: &str = "end_turn";
pub const STOP_REASON_MAX_TOKENS: &str = "max_tokens";
pub const STOP_REASON_STOP_SEQUENCE: &str = "stop_sequence";

/// The modelled `StopReason` member a Converse-API turn served by `family`
/// reports for `stop`.
///
/// The Converse APIs are the SERVICE's own request and response shapes over
/// every model it serves, but the stop reason they carry is still the model's:
/// `stop_sequence` is reported for a halted turn only by a family that reports a
/// halt as its own reason, and the families that word a halt as the ordinary end
/// of a turn carry `end_turn` for one.
///
/// `family` is the invoked model's response family, which is `None` only for a
/// model that generates no text at all — one no Converse-API turn is served for
/// — and whose halt is therefore reported the way the families that publish no
/// stop-sequence reason report theirs.
#[must_use]
pub fn converse_stop_reason(family: Option<ModelFamily>, stop: TurnStop<'_>) -> &'static str {
    match stop {
        TurnStop::Finished => STOP_REASON_END_TURN,
        TurnStop::TokenCap => STOP_REASON_MAX_TOKENS,
        TurnStop::StopSequence(_) => match family.is_some_and(family_names_its_stop_sequence) {
            true => STOP_REASON_STOP_SEQUENCE,
            false => STOP_REASON_END_TURN,
        },
    }
}

/// Whether `family`'s own response fields name the stop sequence a halted turn
/// was generated, which is what makes the halt a reason of its own for it.
///
/// The Anthropic Claude Messages API reports `stop_reason` `stop_sequence` and
/// the sequence itself in `stop_sequence`; the other families this emulator
/// renders envelopes for publish no field for the sequence and no terminal
/// reason for the halt, so a halted turn is the ordinary end of a turn to them.
fn family_names_its_stop_sequence(family: ModelFamily) -> bool {
    matches!(family, ModelFamily::AnthropicMessages)
}

/// The stop reason `family` reports for `stop` — in the family's OWN vocabulary,
/// which is what its `InvokeModel` response envelope and its streaming events
/// carry.
///
/// Per family as documented in "Inference request parameters and response fields
/// for foundation models" in the Amazon Bedrock User Guide. The unary response
/// envelope and the streaming events read it from here, so the two cannot report
/// a turn as having ended differently.
#[must_use]
pub fn family_stop_reason(family: ModelFamily, stop: TurnStop<'_>) -> &'static str {
    match (family, stop) {
        // The one family whose response fields name the generated sequence
        // ([`family_names_its_stop_sequence`]) reports the halt as its own
        // reason; for every other family a halted turn ended the way a finished
        // turn does, which is the arm below.
        (ModelFamily::AnthropicMessages, TurnStop::StopSequence(_)) => "stop_sequence",
        (ModelFamily::AnthropicMessages | ModelFamily::Nova, TurnStop::TokenCap) => "max_tokens",
        (ModelFamily::AnthropicMessages | ModelFamily::Nova, _) => "end_turn",
        (ModelFamily::TitanText, TurnStop::TokenCap) => "LENGTH",
        (ModelFamily::TitanText, _) => "FINISH",
        (ModelFamily::Llama | ModelFamily::Mistral, TurnStop::TokenCap) => "length",
        (ModelFamily::Llama | ModelFamily::Mistral, _) => "stop",
        (ModelFamily::CohereCommand, TurnStop::TokenCap) => "MAX_TOKENS",
        (ModelFamily::CohereCommand, _) => "COMPLETE",
        (ModelFamily::OpenAiChat, TurnStop::TokenCap) => openai_chat::FINISH_REASON_LENGTH,
        (ModelFamily::OpenAiChat, _) => openai_chat::FINISH_REASON_STOP,
    }
}

/// The stop sequence a halted turn was generated, as the response field of a
/// family that names it carries it: the sequence for a halted turn, and nothing
/// for a turn that ended any other way.
#[must_use]
pub fn halted_sequence_field(stop: TurnStop<'_>) -> serde_json::Value {
    match stop {
        TurnStop::StopSequence(sequence) => serde_json::Value::String(sequence.to_string()),
        TurnStop::Finished | TurnStop::TokenCap => serde_json::Value::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the modelled member a halted turn carries is the serving family's —
    /// `stop_sequence` from the family that reports a halt as its own reason, and
    /// `end_turn` from every family that words it as the ordinary end of a turn —
    /// while the cap and a finished completion are reported the same way for all
    /// of them.
    #[test]
    fn a_halted_turn_reports_the_serving_family_stop_reason() {
        // (family, the modelled member a halt reports)
        let rows = [
            (ModelFamily::AnthropicMessages, STOP_REASON_STOP_SEQUENCE),
            (ModelFamily::Nova, STOP_REASON_END_TURN),
            (ModelFamily::TitanText, STOP_REASON_END_TURN),
            (ModelFamily::Llama, STOP_REASON_END_TURN),
            (ModelFamily::Mistral, STOP_REASON_END_TURN),
            (ModelFamily::CohereCommand, STOP_REASON_END_TURN),
            (ModelFamily::OpenAiChat, STOP_REASON_END_TURN),
        ];
        for (family, expected) in rows {
            for sequence in ["7", ".", "in this emulator"] {
                let served = converse_stop_reason(Some(family), TurnStop::StopSequence(sequence));
                assert_eq!(
                    served, expected,
                    "case {family:?} halted on {sequence:?}: expected {expected:?}, got \
                     {served:?}"
                );
            }
            let finished = converse_stop_reason(Some(family), TurnStop::Finished);
            assert_eq!(
                finished, STOP_REASON_END_TURN,
                "case {family:?} finished: expected {STOP_REASON_END_TURN:?}, got {finished:?}"
            );
            let capped = converse_stop_reason(Some(family), TurnStop::TokenCap);
            assert_eq!(
                capped, STOP_REASON_MAX_TOKENS,
                "case {family:?} at the cap: expected {STOP_REASON_MAX_TOKENS:?}, got {capped:?}"
            );
        }
    }

    /// RULE: the cause is read off what the generation stage reports — a halt
    /// wherever a sequence completed, whatever the cap did, and the cap only for
    /// a turn that halted on nothing.
    #[test]
    fn the_cause_is_the_halt_before_the_cap() {
        assert_eq!(TurnStop::of(false, None), TurnStop::Finished);
        assert_eq!(TurnStop::of(true, None), TurnStop::TokenCap);
        assert_eq!(TurnStop::of(false, Some("7")), TurnStop::StopSequence("7"));
        assert_eq!(TurnStop::of(true, Some("7")), TurnStop::StopSequence("7"));
    }

    /// RULE: the sequence field a family that names it carries is the sequence a
    /// halted turn ended on, and nothing at all for a turn that ended otherwise.
    #[test]
    fn the_sequence_field_names_the_sequence_a_halt_ended_on() {
        assert_eq!(
            halted_sequence_field(TurnStop::StopSequence("stop here")),
            serde_json::json!("stop here")
        );
        assert_eq!(
            halted_sequence_field(TurnStop::Finished),
            serde_json::Value::Null
        );
        assert_eq!(
            halted_sequence_field(TurnStop::TokenCap),
            serde_json::Value::Null
        );
    }
}
