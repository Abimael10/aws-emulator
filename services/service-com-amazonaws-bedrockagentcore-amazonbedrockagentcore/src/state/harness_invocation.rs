// SPDX-License-Identifier: CC0-1.0

//! What a HARNESS INVOCATION resolves and reports: the harness a request is addressed at,
//! the model it is configured to run its turn against, and the turn the stream carries.
//!
//! A harness, its versions and its endpoints belong to the AgentCore CONTROL plane
//! (`CreateHarness`, `CreateHarnessEndpoint`), a different service, so existence is read
//! THERE ([`resolve_harness`]) and never assumed from the request: a harness that plane
//! does not hold, or a `qualifier` naming no endpoint on one, is the modelled
//! `ResourceNotFoundException`. A harness is created with no endpoints at all and is
//! invocable as soon as it exists, so a request that names NO qualifier addresses the
//! harness itself rather than an alias on it.
//!
//! NO LANGUAGE MODEL RUNS IN THIS EMULATOR, and nothing here pretends otherwise. The text
//! a served turn carries is the notice the Bedrock Runtime plane of this same emulator
//! answers every inference surface with ([`runtime_state::EMULATED_COMPLETION_FLOOR`], the
//! one place that sentence is worded), composed by that plane's own generation stage from
//! the facts of the invocation that was really served — which model the harness is
//! configured with, how many messages the conversation carried and how many input tokens
//! they were. It is never the caller's own messages handed back as if an agent had replied,
//! and never prose synthesised to look like orchestration. Everything AROUND the text is
//! real and computed from the request that was served: the token counts come from that
//! plane's tokenizer, the stop reason from whether the caller's generation ceiling cut the
//! notice off, and the latency from how long serving the invocation actually took.
//!
//! The same reason is why a model configured with a THIRD-PARTY provider cannot be served
//! at all ([`ConfiguredModel`]): reaching that provider's API would be an outbound call
//! this emulator does not make, and no turn can be reported for a model nothing asked.
//!
//! What a caller can make this cost is bounded: the token count is taken over at most
//! [`MAX_TOKENIZED_PROMPT_BYTES`] of conversation, and the composition itself runs off the
//! async worker (see [`crate::ops::invoke_harness`]), so neither the size of one request
//! nor the CPU the tokenizer spends on it decides how long another caller waits.

use std::time::SystemTime;

use ::service_com_amazonaws_bedrockagentcorecontrol_amazonbedrockagentcorecontrol as control_plane;
use ::service_com_amazonaws_bedrockruntime_amazonbedrockfrontendservice::state as runtime_state;

use crate::ops::types::{HarnessContentBlock, HarnessMessage, HarnessModelConfiguration};

/// The `resource_type` field of `HarnessArn`,
/// `arn:aws:bedrock-agentcore:<region>:<account>:harness/<harnessId>`, which is what an
/// invocation carries as its `harnessArn` query parameter.
const HARNESS_ARN_RESOURCE: &str = "harness";

/// `HarnessConversationRole` for the side of the conversation a served turn speaks for
/// (source: `smithy_query(query="shape", shape="HarnessConversationRole")`, values
/// user | assistant).
pub const HARNESS_ROLE_ASSISTANT: &str = "assistant";

/// `HarnessStopReason` for a turn that said everything it had to say (source:
/// `smithy_query(query="shape", shape="HarnessStopReason")`).
pub const HARNESS_STOP_END_TURN: &str = "end_turn";

/// `HarnessStopReason` for a turn the caller's own generation ceiling cut off, from the
/// same enum.
pub const HARNESS_STOP_MAX_TOKENS: &str = "max_tokens";

/// Why a call addressed to a harness cannot be served, with the sentence it is refused
/// with. Which modelled error carries it is the operation's own choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HarnessUnavailable {
    /// Neither the harness nor the alias the request named resolved: the modelled
    /// `ResourceNotFoundException`, worded by [`super::runtime_endpoint_not_found`] — one
    /// sentence for every invocation of this plane, so a harness invocation and a runtime
    /// invocation cannot word the same condition differently.
    NotFound(String),
    /// The component that owns harnesses could not be reached: a modelled
    /// `InternalServerException`.
    StoreUnavailable(String),
}

/// Which provider serves the model a harness runs its turn against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ModelProvider {
    /// Amazon Bedrock, whose inference surface this emulator serves itself.
    Bedrock,
    /// An OpenAI-compatible API, reached over the public internet.
    OpenAi,
    /// The Gemini API, reached over the public internet.
    Gemini,
    /// A LiteLLM proxy, reached over the network.
    LiteLlm,
}

impl ModelProvider {
    /// How a refusal names this provider.
    pub fn label(self) -> &'static str {
        match self {
            Self::Bedrock => "Amazon Bedrock",
            Self::OpenAi => "OpenAI",
            Self::Gemini => "Gemini",
            Self::LiteLlm => "LiteLLM",
        }
    }

    /// Whether a turn against this provider can be served at all: only the provider whose
    /// inference surface this emulator serves itself can be, because reaching any other
    /// one's API would be an outbound call this emulator does not make.
    pub fn is_served_in_process(self) -> bool {
        self == Self::Bedrock
    }
}

/// The model a harness invocation runs its turn against, read off either the request or
/// the stored harness and normalised to the three things a turn needs.
#[derive(Debug, Clone)]
pub struct ConfiguredModel {
    pub provider: ModelProvider,
    /// The model identifier the configuration names, which the turn reports as the model
    /// that generated no text.
    pub model_id: String,
    /// The generation ceiling the model configuration carries, when it carries one.
    pub max_tokens: Option<i32>,
}

impl ConfiguredModel {
    /// The model configuration an INVOCATION carried, normalised.
    pub fn of_request(configured: &HarnessModelConfiguration) -> Self {
        match configured {
            HarnessModelConfiguration::bedrockModelConfig(config) => Self {
                provider: ModelProvider::Bedrock,
                model_id: config.model_id.clone(),
                max_tokens: config.max_tokens,
            },
            HarnessModelConfiguration::openAiModelConfig(config) => Self {
                provider: ModelProvider::OpenAi,
                model_id: config.model_id.clone(),
                max_tokens: config.max_tokens,
            },
            HarnessModelConfiguration::geminiModelConfig(config) => Self {
                provider: ModelProvider::Gemini,
                model_id: config.model_id.clone(),
                max_tokens: config.max_tokens,
            },
            HarnessModelConfiguration::liteLlmModelConfig(config) => Self {
                provider: ModelProvider::LiteLlm,
                model_id: config.model_id.clone(),
                max_tokens: config.max_tokens,
            },
        }
    }

    /// The model configuration the STORED HARNESS carries, normalised. The control plane
    /// declares the same four providers for its own copy of the shape, so the two readings
    /// agree member for member.
    fn of_harness(configured: &control_plane::ops::types::HarnessModelConfiguration) -> Self {
        use control_plane::ops::types::HarnessModelConfiguration as Configured;
        match configured {
            Configured::bedrockModelConfig(config) => Self {
                provider: ModelProvider::Bedrock,
                model_id: config.model_id.clone(),
                max_tokens: config.max_tokens,
            },
            Configured::openAiModelConfig(config) => Self {
                provider: ModelProvider::OpenAi,
                model_id: config.model_id.clone(),
                max_tokens: config.max_tokens,
            },
            Configured::geminiModelConfig(config) => Self {
                provider: ModelProvider::Gemini,
                model_id: config.model_id.clone(),
                max_tokens: config.max_tokens,
            },
            Configured::liteLlmModelConfig(config) => Self {
                provider: ModelProvider::LiteLlm,
                model_id: config.model_id.clone(),
                max_tokens: config.max_tokens,
            },
        }
    }
}

/// The harness an invocation reached, as the control plane holds it.
#[derive(Debug, Clone)]
pub struct InvokedHarness {
    /// The harness the request's ARN named.
    pub harness_id: String,
    /// The endpoint the `qualifier` named, or `None` for an invocation addressed at the
    /// harness itself — which is every invocation that names no qualifier, since a harness
    /// is created with no endpoints and is invocable as soon as it exists.
    pub endpoint_name: Option<String>,
    /// The AgentCore Runtime environment this harness runs in: the runtime the service
    /// manages for it, which is what a session of this invocation runs in.
    pub runtime_arn: String,
    /// The `agentRuntimeId` of that environment.
    pub runtime_id: String,
    /// The model the HARNESS is configured with, when it is configured with one. An
    /// invocation that carries its own configuration uses that instead.
    pub model: Option<ConfiguredModel>,
    /// The system prompt the harness is configured with, as text blocks.
    pub system_prompt: Vec<String>,
    /// The generation ceiling the harness is configured with, when it carries one.
    pub max_tokens: Option<i32>,
}

/// Resolve the harness an invocation addresses, read off the component that OWNS
/// harnesses.
///
/// `Ok` only for a harness this account still holds, and — when the request named a
/// `qualifier` — for an endpoint of that name on it. Everything else is the one answer
/// [`super::runtime_endpoint_not_found`] words: AWS tells a caller neither which half was
/// missing nor whether the harness belongs to somebody else.
pub fn resolve_harness<Eng: Send + Sync + 'static>(
    engine: &Eng,
    harness_arn: &str,
    qualifier: Option<&str>,
) -> Result<InvokedHarness, HarnessUnavailable> {
    let endpoint_name = qualifier.filter(|named| !named.is_empty());
    let missing = || {
        HarnessUnavailable::NotFound(super::runtime_endpoint_not_found(
            endpoint_name.unwrap_or(super::DEFAULT_RUNTIME_QUALIFIER),
            harness_arn,
        ))
    };
    let Some(harness_id) =
        ::service_common::arn::resource_id_of_type(harness_arn, HARNESS_ARN_RESOURCE)
            .filter(|id| !id.is_empty())
    else {
        return Err(missing());
    };
    let Some(control) = control_plane::state::service_state(engine) else {
        return Err(HarnessUnavailable::StoreUnavailable(
            super::RUNTIME_STORE_UNAVAILABLE.to_string(),
        ));
    };
    let store = control.lock();
    let Some(harness) = store.harnesses.get(harness_id) else {
        return Err(missing());
    };
    // A well-formed ARN of another account's harness is not this account's: the stored
    // harness's own ARN is the one that addresses it.
    if harness.harness_arn != harness_arn {
        return Err(missing());
    }
    if let Some(named) = endpoint_name {
        if !harness.endpoints.contains_key(named) {
            return Err(missing());
        }
    }
    Ok(InvokedHarness {
        harness_id: harness_id.to_string(),
        endpoint_name: endpoint_name.map(ToString::to_string),
        runtime_arn: harness.environment.agent_runtime_arn.clone(),
        runtime_id: harness.environment.agent_runtime_id.clone(),
        model: harness.model.as_ref().map(ConfiguredModel::of_harness),
        system_prompt: harness
            .system_prompt
            .iter()
            .map(|block| match block {
                control_plane::ops::types::HarnessSystemContentBlock::Text(text) => text.clone(),
            })
            .collect(),
        max_tokens: harness.max_tokens,
    })
}

/// The most bytes of the conversation one invocation's token count is taken over.
///
/// An EMULATOR bound, not an AWS one: `HarnessMessages` and the text inside a content block
/// carry no modelled maximum, and counting tokens is a byte-pair encode over text the
/// CALLER chose the size of, so without a ceiling one request could decide how much CPU
/// this process spends. A prompt past it is counted over its first
/// [`MAX_TOKENIZED_PROMPT_BYTES`] bytes.
pub const MAX_TOKENIZED_PROMPT_BYTES: usize = 256 * 1024;

/// The turn a served invocation reports: the text, the tokens it accounted for and why it
/// ended.
#[derive(Debug, Clone)]
pub struct HarnessTurn {
    /// The assistant text, as the Bedrock Runtime plane's generation stage composed it.
    pub text: String,
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub total_tokens: i32,
    /// `HarnessStopReason` for how the turn ended.
    pub stop_reason: String,
}

/// Compose the turn an invocation of `model_id` over a conversation of `messages` messages
/// whose text is `prompt` serves, bounded by the generation ceiling `max_output_tokens` the
/// configuration carried.
///
/// Every number here is counted rather than chosen: the input tokens are what the Bedrock
/// Runtime plane's tokenizer makes of the text the conversation and the system prompt
/// carried ([`harness_prompt_text`]), the output tokens are what it makes of the text
/// served, and the stop reason is `max_tokens` exactly when that plane reports the ceiling
/// cut the notice off.
///
/// CPU-bound (the tokenizer is a byte-pair encode), so a caller of this runs it OFF the
/// async worker and with no state guard held.
pub fn harness_turn(
    model_id: &str,
    messages: u32,
    prompt: &str,
    max_output_tokens: Option<i32>,
) -> HarnessTurn {
    let input_tokens = runtime_state::count_text_tokens(prompt);
    // A configuration that names no ceiling leaves the generation uncapped: AWS publishes
    // no default `maxTokens` for a harness, and the model's own default is the model's, so
    // nothing here invents one.
    let ceiling = max_output_tokens
        .and_then(|tokens| u32::try_from(tokens).ok())
        .unwrap_or(u32::MAX);
    let composed = runtime_state::emulated_completion(
        model_id,
        messages,
        input_tokens,
        ceiling,
        // The harness model configuration declares no stop sequences, so a turn can only
        // end on its own text or on the ceiling.
        &[],
    );
    let output_tokens = runtime_state::count_text_tokens(&composed.text);
    HarnessTurn {
        text: composed.text,
        input_tokens: runtime_state::token_count_i32(input_tokens),
        output_tokens: runtime_state::token_count_i32(output_tokens),
        total_tokens: runtime_state::token_count_i32(input_tokens.saturating_add(output_tokens)),
        stop_reason: if composed.truncated {
            HARNESS_STOP_MAX_TOKENS.to_string()
        } else {
            HARNESS_STOP_END_TURN.to_string()
        },
    }
}

/// The text an invocation's input is: the system prompt the turn was given and the text of
/// every content block of every message, which is what the token count is taken over, cut
/// at [`MAX_TOKENIZED_PROMPT_BYTES`] on a character boundary.
///
/// A block that carries no text of its own — a tool request, a tool result document,
/// redacted reasoning — contributes the text it does carry and nothing invented for the
/// rest.
pub fn harness_prompt_text(messages: &[HarnessMessage], system_prompt: &[String]) -> String {
    let mut text = system_prompt.join("\n");
    for message in messages {
        if text.len() >= MAX_TOKENIZED_PROMPT_BYTES {
            break;
        }
        for block in &message.content {
            text.push('\n');
            match block {
                HarnessContentBlock::Text(said) => text.push_str(said),
                HarnessContentBlock::reasoningContent(reasoning) => {
                    if let crate::ops::types::HarnessReasoningContentBlock::reasoningText(block) =
                        reasoning
                    {
                        text.push_str(&block.text);
                    }
                }
                HarnessContentBlock::toolUse(tool) => text.push_str(&tool.name),
                HarnessContentBlock::toolResult(result) => {
                    for block in &result.content {
                        if let crate::ops::types::HarnessToolResultContentBlock::Text(said) = block {
                            text.push_str(said);
                        }
                    }
                }
            }
        }
    }
    if text.len() > MAX_TOKENIZED_PROMPT_BYTES {
        let mut cut = MAX_TOKENIZED_PROMPT_BYTES;
        while cut > 0 && !text.is_char_boundary(cut) {
            cut -= 1;
        }
        text.truncate(cut);
    }
    text
}

/// The session an invocation of a harness establishes in the AgentCore Runtime environment
/// that harness runs in, so a later `StopRuntimeSession` addressed at that environment's
/// runtime ARN has a session to stop.
pub fn harness_session(
    harness: &InvokedHarness,
    runtime_session_id: &str,
    now: SystemTime,
) -> super::RuntimeSessionRecord {
    super::RuntimeSessionRecord::started(
        &harness.runtime_id,
        harness
            .endpoint_name
            .as_deref()
            .unwrap_or(super::DEFAULT_RUNTIME_QUALIFIER),
        runtime_session_id,
        now,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: the turn says that no model ran, names the model it was configured with, and
    /// reports token counts taken over the text that was really served.
    #[test]
    fn the_turn_says_no_model_ran_and_counts_what_it_served() {
        let messages = vec![HarnessMessage {
            role: "user".to_string(),
            content: vec![HarnessContentBlock::Text("plan the trip".to_string())],
        }];
        let prompt = harness_prompt_text(&messages, &[]);
        let turn = harness_turn(
            "anthropic.claude-3-5-sonnet-20241022-v2:0",
            1,
            &prompt,
            None,
        );
        assert!(
            turn.text.starts_with(runtime_state::EMULATED_COMPLETION_FLOOR),
            "case \"an uncapped turn\": expected the turn to open with the notice that no \
             model ran, got {:?}",
            turn.text
        );
        assert!(
            turn.text.contains("anthropic.claude-3-5-sonnet-20241022-v2:0"),
            "case \"an uncapped turn\": expected the turn to name the model, got {:?}",
            turn.text
        );
        assert!(
            !turn.text.contains("plan the trip"),
            "case \"an uncapped turn\": expected the caller's own text not to be answered \
             back, got {:?}",
            turn.text
        );
        assert_eq!(
            turn.stop_reason, HARNESS_STOP_END_TURN,
            "case \"an uncapped turn\": expected {HARNESS_STOP_END_TURN}, got {:?}",
            turn.stop_reason
        );
        assert_eq!(
            turn.total_tokens,
            turn.input_tokens + turn.output_tokens,
            "case \"an uncapped turn\": expected the total to be the sum of the counted \
             halves, got {turn:?}"
        );
    }

    /// RULE: a turn the configured generation ceiling cut off ends on `max_tokens`.
    #[test]
    fn a_turn_the_ceiling_cuts_off_ends_on_max_tokens() {
        let messages = vec![HarnessMessage {
            role: "user".to_string(),
            content: vec![HarnessContentBlock::Text("hello".to_string())],
        }];
        let prompt = harness_prompt_text(&messages, &[]);
        let turn = harness_turn("amazon.titan-text-express-v1", 1, &prompt, Some(2));
        assert_eq!(
            turn.stop_reason, HARNESS_STOP_MAX_TOKENS,
            "case \"a two-token ceiling\": expected {HARNESS_STOP_MAX_TOKENS}, got {:?}",
            turn.stop_reason
        );
        assert!(
            turn.output_tokens <= 2,
            "case \"a two-token ceiling\": expected at most the ceiling in output tokens, \
             got {}",
            turn.output_tokens
        );
    }
}
