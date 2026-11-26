// SPDX-License-Identifier: CC0-1.0

//! The one place the token sequence a completed `sample` operation serves is decided.
//!
//! `ExecuteSamplingOperation`'s `sample` member generates text from the model a training
//! session holds, and `GetSamplingOperationResult` reports it as
//! `SampleOperationResult$sample`. NO LANGUAGE MODEL RUNS IN THIS EMULATOR, and nothing
//! here pretends otherwise: the token ids a sampled sequence carries are the real
//! byte-pair encoding of [`NO_LANGUAGE_MODEL_NOTICE`] — a sentence that says in so many
//! words that no model ran — extended with the one fact about the request that was served
//! (how many prompt tokens it prefilled). They are never ids synthesised to look like
//! inference: not the prompt's own ids answered back, not ids drawn from a digest or a
//! counter, not a continuation invented for a vocabulary this emulator does not hold. A
//! client that decodes what it was served therefore always reads that no inference
//! happened — the same thing the synchronous `SampleTrainingSession` says in its own body
//! ([`super::sample_response_body`]).
//!
//! Everything AROUND that text is real and computed from the request that was served and
//! from stored session state: there are exactly as many sequences as `numSamples` asked
//! for, `tokenUsage.prefill` is the prompt's own token count and `tokenUsage.decode` the
//! number of tokens actually served, the sequence is cut to `samplingParams.maxTokens` and
//! reports `length` exactly when that cut is what ended it, and `samplingParams.stop`
//! really does halt it (`stop`).
//!
//! `SampledSequence$logprobs`, `SampleResponse$promptLogprobs` and
//! `SampleResponse$topkPromptLogprobs` are left unpopulated. A log-probability is a number
//! only a model's forward pass has, so this emulator has none to report, and all three are
//! optional members whose absence is what that means; `computeLogprobs`, whose whole
//! result is that number, therefore keeps the modelled failure status
//! ([`super::no_model_runtime`]).

use ::std::sync::OnceLock;

use crate::ops::types::{
    ModelInput, SampleConfig, SampleResponse, SampleTokenUsage, SampledSequence,
};

/// What every sampled sequence this emulator serves says.
///
/// A generation cap is a CEILING on the response rather than a target, so this is the part
/// that is always said and [`notice_text`] adds what the request itself contributed.
pub const NO_LANGUAGE_MODEL_NOTICE: &str =
    "This SageMaker training-session runtime emulator runs no language model, so no text \
     was generated for this sampling operation.";

/// `StopReason`'s members, from the model's own enum: "A value of `stop` indicates a stop
/// sequence was encountered. A value of `length` indicates the maximum token limit was
/// reached."
pub const STOP_REASON_STOP: &str = "stop";
/// See [`STOP_REASON_STOP`].
pub const STOP_REASON_LENGTH: &str = "length";

/// The most sequences one `sample` operation can be asked for: `SampleConfig$numSamples`
/// is `@range(min: 1, max: 1000)`, so this is the modelled maximum and it is what bounds
/// the response a submit can ask this emulator to hold.
pub const MAX_SAMPLES: i32 = 1_000;

/// The tokenizer every token id this service serves is encoded with, built once: loading a
/// vocabulary is the expensive part and no request may pay it.
///
/// A sampling operation speaks TOKEN IDS, not text — `EncodedTextChunk$tokens` carries
/// "the token IDs representing the encoded text" — so serving one at all needs a real
/// byte-pair encoder over a published vocabulary. No AWS base model publishes its own, so
/// the vocabulary is not the session's base model's: it is the `cl100k_base` one the
/// tokenizer crate itself ships. It is deterministic, it is the single vocabulary every
/// sequence this service serves is encoded with, and nothing is read from the host or the
/// network to build it — what is loaded is a constant of the build.
///
/// Answers `None` rather than unwinding if the vocabulary cannot be built, so a request
/// that reaches an unbuildable tokenizer is reported as a failed operation rather than
/// panicking inside a handler.
fn tokenizer() -> Option<&'static ::tiktoken_rs::CoreBPE> {
    static TOKENIZER: OnceLock<Option<::tiktoken_rs::CoreBPE>> = OnceLock::new();
    TOKENIZER
        .get_or_init(|| match ::tiktoken_rs::cl100k_base() {
            Ok(tokenizer) => Some(tokenizer),
            Err(error) => {
                ::tracing::error!(%error, "the sampling vocabulary could not be built");
                None
            }
        })
        .as_ref()
}

/// The token ids `text` encodes to, in order, as the modelled `IntegerList` carries them.
///
/// Encoded as ORDINARY text: nothing here is a control marker, so a sentence that spells
/// one of the vocabulary's special markers out is encoded as the characters it wrote. The
/// text is this module's own notice rather than anything a caller wrote, so no
/// caller-controlled string reaches the encoder.
fn encode(text: &str) -> Option<Vec<i32>> {
    Some(
        tokenizer()?
            .encode_ordinary(text)
            .into_iter()
            .map(|token| i32::try_from(token).unwrap_or(i32::MAX))
            .collect(),
    )
}

/// How many tokens a `ModelInput` prompt carries: the token ids of every chunk of it,
/// which is what the prefill phase of a sampling operation is given.
///
/// "The ordered list of encoded text chunks forming the input sequence", each chunk
/// carrying "the token IDs representing the encoded text" — `ModelInput$chunks`.
pub fn prompt_token_count(prompt: &ModelInput) -> i32 {
    let total: usize = prompt.chunks.iter().map(|chunk| chunk.tokens.len()).sum();
    i32::try_from(total).unwrap_or(i32::MAX)
}

/// What a sampling operation serves for one sample: the text it says, as token ids, and
/// why generation stopped.
#[derive(Debug, Clone)]
pub struct EmulatedSequence {
    /// `SampledSequence$tokens`: "The token IDs of the generated sequence."
    pub tokens: Vec<i32>,
    /// `SampledSequence$stopReason`, one of [`STOP_REASON_STOP`] / [`STOP_REASON_LENGTH`].
    pub stop_reason: &'static str,
}

/// What one sampled sequence says: the notice, plus how many prompt tokens were prefilled.
///
/// The token count is a fact about the request that was SERVED, not the request's own
/// content answered back: it is the same number `tokenUsage.prefill` reports.
fn notice_text(prefill: i32) -> String {
    format!("{NO_LANGUAGE_MODEL_NOTICE} The prompt carried {prefill} token(s).")
}

/// The byte offset the earliest of `stop` begins at, and `None` when none of them occurs
/// in `text`.
///
/// "The list of stop sequences that terminate generation when encountered in the output":
/// generation ends where the EARLIEST of them appears, and the sequence itself is not part
/// of what was generated. An empty stop sequence terminates nothing, so it is skipped
/// rather than halting every generation at its first token.
fn halt_at(text: &str, stop: &[String]) -> Option<usize> {
    stop.iter()
        .filter(|sequence| !sequence.is_empty())
        .filter_map(|sequence| text.find(sequence.as_str()))
        .min()
}

/// One sampled sequence for a prompt of `prefill` tokens, under the caller's own token
/// ceiling and stop sequences.
///
/// The order is the order a decoder works in: what is generated is halted at the first
/// stop sequence it reaches, and then cut to the token limit — so a limit the caller set
/// below the stop sequence is what ended generation and is reported as `length`, and a
/// sequence that finished inside its budget reports `stop`.
///
/// `None` when the vocabulary could not be built: there is then nothing to serve, and the
/// operation reports that rather than an empty sequence dressed as a generated one.
pub fn emulated_sequence(
    prefill: i32,
    max_tokens: i32,
    stop: &[String],
) -> Option<EmulatedSequence> {
    let text = notice_text(prefill);
    // `find` answers the start of a match, which is a char boundary of `text`, so the
    // prefix is always a valid slice.
    let generated = match halt_at(&text, stop) {
        Some(at) => &text[..at],
        None => text.as_str(),
    };
    let mut tokens = encode(generated)?;
    // A negative ceiling allows no token at all; the modelled `@range(min: 512)` keeps one
    // off this surface, and a caller of this function that is not the boundary still gets
    // the limit it asked for rather than an unbounded sequence.
    let limit = usize::try_from(max_tokens).unwrap_or(0);
    let stop_reason = if tokens.len() > limit {
        tokens.truncate(limit);
        STOP_REASON_LENGTH
    } else {
        STOP_REASON_STOP
    };
    Some(EmulatedSequence {
        tokens,
        stop_reason,
    })
}

/// The `SampleResponse` a completed `sample` operation reports, computed from the
/// configuration that was submitted.
///
/// Every sequence says the same thing because nothing was sampled: no model ran, so there
/// is no distribution for two samples of one prompt to differ along, and a response that
/// varied its sequences would be claiming one did.
pub fn sample_response(config: &SampleConfig) -> Option<SampleResponse> {
    let prefill = prompt_token_count(&config.prompt);
    let stop = config.sampling_params.stop.as_deref().unwrap_or_default();
    let sampled = emulated_sequence(prefill, config.sampling_params.max_tokens, stop)?;
    let decode = i32::try_from(sampled.tokens.len()).unwrap_or(i32::MAX);
    // "The number of independent samples to generate for the prompt": one sequence each,
    // bounded by the modelled maximum so a submit cannot ask for an unbounded response.
    let samples = usize::try_from(config.num_samples.clamp(0, MAX_SAMPLES)).unwrap_or(0);
    let sequences = (0..samples)
        .map(|_| SampledSequence {
            stop_reason: sampled.stop_reason.to_string(),
            tokens: sampled.tokens.clone(),
            logprobs: None,
            token_usage: Some(SampleTokenUsage { prefill, decode }),
        })
        .collect();
    Some(SampleResponse {
        sequences,
        prompt_logprobs: None,
        topk_prompt_logprobs: None,
    })
}

/// Why a `sample` operation that could not be encoded ends in `Failed`: the vocabulary
/// every served sequence is encoded with could not be built, so this emulator has nothing
/// to report for the operation.
pub fn no_sampling_vocabulary() -> String {
    "This SageMaker training-session runtime emulator could not build the vocabulary a \
     sampled sequence is encoded with, so the sample operation produced no result."
        .to_string()
}
