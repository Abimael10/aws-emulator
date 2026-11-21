// SPDX-License-Identifier: CC0-1.0

//! The generation stage every inference surface of this service answers from:
//! the one place the text a served turn carries is decided.
//!
//! NO LANGUAGE MODEL RUNS IN THIS EMULATOR, and nothing here pretends otherwise.
//! The completion is the notice in [`EMULATED_COMPLETION_FLOOR`], which says in
//! so many words that no model ran, extended with the further clauses the
//! caller's generation budget still admits. It is never text synthesised to look
//! like inference: not the request's own words answered back, not a value read
//! out of the prompt, not output derived from a digest or a word list. A client
//! reading a completion can therefore always tell that no inference happened.
//!
//! `Converse`, `ConverseStream`, `InvokeModel` and `InvokeModelWithResponseStream`
//! all answer from [`emulated_completion`], so the four serve identical text for
//! identical input and the streaming pair frame exactly what their unary siblings
//! return. Everything AROUND the text — the turn semantics, the token accounting,
//! the stop reason, the latency and the invocation log — is real and computed
//! from the request that was actually served.

use super::{count_text_tokens, halt_at_stop_sequence, truncate_to_tokens};

/// The shortest completion this emulator answers an inference call with.
///
/// A generation cap is a CEILING on the response, not a target, so a completion
/// is composed to FIT the budget the caller allowed: this sentence is the part
/// that is always said, and [`emulated_completion`] appends the further clauses
/// that still fit.
pub const EMULATED_COMPLETION_FLOOR: &str = "No language model runs in this emulator.";

/// One synthesized completion: the text served, and whether the generation cap
/// cut it off before it was finished.
#[derive(Debug, Clone)]
pub struct EmulatedCompletion {
    /// The assistant text. Never the request's own text echoed back as if
    /// generated.
    pub text: String,
    /// `true` when the generation cap, rather than the completion itself, is what
    /// ended generation: there was more of the completion left to say and the
    /// whole budget the caller allowed had been used saying what came before it.
    pub truncated: bool,
    /// The configured stop sequence that completed inside this text, when one of
    /// the request's did: generation ended ON that sequence rather than because
    /// the completion had nothing left to say or because the cap cut it off. It
    /// is what a turn reports as the CAUSE of its stop
    /// ([`super::stop_reason::TurnStop`]), and the sequence a model family whose
    /// response fields name the generated one reports there.
    pub halted_on: Option<String>,
}

/// The completion a synchronous inference call answers with, composed to fit
/// `max_output_tokens` and halted at the first stop sequence it carries.
///
/// No language model runs in this emulator, so the text says so, and carries
/// only facts about the request that was actually served: how many messages it
/// held and how many tokens of input they were. It is never the request's own
/// text echoed back as if generated.
///
/// The caller's cap is the maximum number of tokens to ALLOW in the generated
/// response, so the clauses are appended in order only while the next one still
/// fits, and a completion that has nothing left to say has finished
/// (`truncated` is `false`) however much room is left over.
///
/// Generation ran into the ceiling — which is what a length stop reason reports —
/// exactly when BOTH hold: there was more of the completion left to say, and the
/// tokens served use up the whole budget the caller allowed. A completion that
/// stops with budget to spare stopped because it was finished, not because the
/// cap cut it off, which is why a modest ceiling on a short turn is not reported
/// as truncation; a budget too small even for [`EMULATED_COMPLETION_FLOOR`] cuts
/// that sentence itself off and is always reported as truncation.
///
/// `stop_sequences` are the sequences the request configured, applied by
/// [`halt_at_stop_sequence`]: generation ends where the first of them completes,
/// so a halted completion is NOT one the ceiling cut off however little of the
/// budget it used — it is reported in
/// [`EmulatedCompletion::halted_on`], which is the CAUSE a served turn names as
/// its stop reason — in the vocabulary of the model family serving it
/// ([`super::stop_reason`]).
pub fn emulated_completion(
    model_id: &str,
    messages: u32,
    input_tokens: u32,
    max_output_tokens: u32,
    stop_sequences: &[String],
) -> EmulatedCompletion {
    emulated_completion_with(
        model_id,
        messages,
        input_tokens,
        max_output_tokens,
        stop_sequences,
        &[],
    )
}

/// The same completion with further clauses the CALLER computed appended, each admitted
/// only while the generation budget still has room for it.
///
/// The clauses a caller adds are facts about the request that was served which the turn
/// has to state for the exchange to mean anything — the result a `toolResult` handed back
/// ([`super::tool_use_turn::tool_result_clauses`]), which is the whole point of a tool-use
/// round trip. They are appended AFTER the notice that no model ran, so a completion can
/// never be read as inference, and they are subject to the same budget rule as the clauses
/// above: a clause that does not fit the ceiling the caller allowed is left out rather
/// than squeezed in.
pub fn emulated_completion_with(
    model_id: &str,
    messages: u32,
    input_tokens: u32,
    max_output_tokens: u32,
    stop_sequences: &[String],
    closing: &[String],
) -> EmulatedCompletion {
    let halted = |text: String, truncated: bool| match halt_at_stop_sequence(&text, stop_sequences)
    {
        Some(halt) => EmulatedCompletion {
            text: halt.text,
            truncated: false,
            halted_on: Some(halt.sequence),
        },
        None => EmulatedCompletion {
            text,
            truncated,
            halted_on: None,
        },
    };
    if count_text_tokens(EMULATED_COMPLETION_FLOOR) > max_output_tokens {
        return halted(
            truncate_to_tokens(EMULATED_COMPLETION_FLOOR, max_output_tokens),
            true,
        );
    }
    let mut text = EMULATED_COMPLETION_FLOOR.to_string();
    // Whether anything of the completion was left unsaid: the clause that did not
    // fit the budget, and everything after it.
    let mut left_unsaid = false;
    for clause in [
        format!(" {model_id} generated no text for this request."),
        format!(" The request carried {messages} message(s) and {input_tokens} input token(s)."),
    ]
    .iter()
    .chain(closing.iter())
    {
        let extended = format!("{text}{clause}");
        if count_text_tokens(&extended) > max_output_tokens {
            left_unsaid = true;
            break;
        }
        text = extended;
    }
    // The ceiling ended generation only when it left no room for what came next
    // AND the whole budget was spent on what came before: a completion that
    // stopped with budget to spare stopped because it had nothing left to say.
    let reached_ceiling = left_unsaid && count_text_tokens(&text) >= max_output_tokens;
    halted(text, reached_ceiling)
}
