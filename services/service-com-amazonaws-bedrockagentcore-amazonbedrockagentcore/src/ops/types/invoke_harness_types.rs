// SPDX-License-Identifier: CC0-1.0

//! Type definitions reached only by the `invoke_harness` operation.
//!
//! Hand-authored rather than generated: `InvokeHarness`'s output member `stream` targets a
//! `@streaming` union, so the op-stub generator emitted neither the operation nor its
//! shapes (`src/ops/SKIPPED_STUBS.md`). Two groups, as a hand-authored event-stream
//! operation always needs:
//!
//! * the request shape, declared so the framework's own HTTP-binding deserializer fills it
//!   from the `harnessArn` query parameter, the session and tracing headers and the JSON
//!   body, and `constraints::validate` checks it against the model's `@pattern` /
//!   `@length` traits — the request half is validated by the model, not by hand; and
//! * the event structures `InvokeHarnessStreamOutput` carries, with the shapes beneath
//!   them.
//!
//! Member sets, member order, requiredness and targets come from
//! `smithy_query(query="closure", operation="InvokeHarness",
//! model_path="services/service-com-amazonaws-bedrockagentcore-amazonbedrockagentcore/\
//! smithy-model/com-amazonaws-bedrockagentcore-amazonbedrockagentcore.json")`, and the
//! derive re-checks each one against that model at compile time.
//!
//! Two request members are declared `unsupported`, which means they are not read at all
//! rather than read and dropped: `tools` and `skills` are what a harness invocation would
//! offer a language model and fetch instructions from, and no language model runs in this
//! emulator ([`crate::ops::invoke_harness`]), so nothing here could act on either. They
//! reach `HarnessGatewayOutboundAuth`, a union whose members target `smithy.api#Unit` —
//! which the vendored codegen cannot type at all — so declaring them would also mean
//! hand-writing a codec for configuration nothing reads.
//!
//! Each structure is declared exactly once across this service's `types/` modules, and
//! `types/mod.rs` re-exports them all.
#![allow(unused_imports)]

use super::*;

smithy_macros::smithy_model!("smithy-model/com-amazonaws-bedrockagentcore-amazonbedrockagentcore.json");

/// `InvokeHarnessRequest`. `harnessArn` and `qualifier` are query parameters,
/// `runtimeSessionId` and the tracing members are headers, and the rest is the JSON body.
#[smithy_macros::smithy(
    shape = "com.amazonaws.bedrockagentcore#InvokeHarnessRequest",
    unsupported = ["tools", "skills"]
)]
#[derive(Debug, Clone, Default)]
pub struct InvokeHarnessInput {
    pub harness_arn: String,
    pub qualifier: Option<String>,
    pub runtime_session_id: String,
    pub runtime_user_id: Option<String>,
    pub trace_parent: Option<String>,
    pub trace_state: Option<String>,
    pub trace_id: Option<String>,
    pub baggage: Option<String>,
    pub messages: Vec<HarnessMessage>,
    pub model: Option<HarnessModelConfiguration>,
    pub system_prompt: Option<Vec<HarnessSystemContentBlock>>,
    pub allowed_tools: Option<Vec<String>>,
    pub max_iterations: Option<i32>,
    pub max_tokens: Option<i32>,
    pub timeout_seconds: Option<i32>,
    pub actor_id: Option<String>,
}

/// `HarnessMessage`: one turn of the conversation the invocation carries.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessMessage")]
#[derive(Debug, Clone)]
pub struct HarnessMessage {
    pub role: String,
    pub content: Vec<HarnessContentBlock>,
}

/// `HarnessContentBlock`: one block of a message — text, a tool request, a tool result, or
/// the model's reasoning.
#[smithy_macros::smithy(union_for = "com.amazonaws.bedrockagentcore#HarnessContentBlock")]
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum HarnessContentBlock {
    Text(String),
    toolUse(HarnessToolUseBlock),
    toolResult(HarnessToolResultBlock),
    reasoningContent(HarnessReasoningContentBlock),
}

/// `HarnessToolUseBlock`: a tool request a previous turn of the conversation carried.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessToolUseBlock")]
#[derive(Debug, Clone, Default)]
pub struct HarnessToolUseBlock {
    pub name: String,
    pub tool_use_id: String,
    pub input: ::aws_smithy_types::Document,
    pub r#type: Option<String>,
    pub server_name: Option<String>,
}

/// `HarnessToolResultBlock`: what a tool invoked for a previous turn produced.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessToolResultBlock")]
#[derive(Debug, Clone)]
pub struct HarnessToolResultBlock {
    pub tool_use_id: String,
    pub content: Vec<HarnessToolResultContentBlock>,
    pub status: Option<String>,
    pub r#type: Option<String>,
}

/// `HarnessToolResultContentBlock`: one block of a tool result, as text or as a document.
#[smithy_macros::smithy(
    union_for = "com.amazonaws.bedrockagentcore#HarnessToolResultContentBlock"
)]
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum HarnessToolResultContentBlock {
    Text(String),
    Json(::aws_smithy_types::Document),
}

/// `HarnessReasoningContentBlock`: the model's own reasoning, in text or redacted.
#[smithy_macros::smithy(
    union_for = "com.amazonaws.bedrockagentcore#HarnessReasoningContentBlock"
)]
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum HarnessReasoningContentBlock {
    reasoningText(HarnessReasoningTextBlock),
    redactedContent(Vec<u8>),
}

/// `HarnessReasoningTextBlock`: reasoning text and the signature that verifies it.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessReasoningTextBlock")]
#[derive(Debug, Clone, Default)]
pub struct HarnessReasoningTextBlock {
    pub text: String,
    pub signature: Option<String>,
}

/// `HarnessModelConfiguration`: which model provider the invocation runs its turn against.
#[smithy_macros::smithy(union_for = "com.amazonaws.bedrockagentcore#HarnessModelConfiguration")]
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum HarnessModelConfiguration {
    bedrockModelConfig(HarnessBedrockModelConfig),
    openAiModelConfig(HarnessOpenAiModelConfig),
    geminiModelConfig(HarnessGeminiModelConfig),
    liteLlmModelConfig(HarnessLiteLlmModelConfig),
}

/// `HarnessBedrockModelConfig`: an Amazon Bedrock model and the inference bounds for it.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessBedrockModelConfig")]
#[derive(Debug, Clone, Default)]
pub struct HarnessBedrockModelConfig {
    pub model_id: String,
    pub max_tokens: Option<i32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub api_format: Option<String>,
    pub additional_params: Option<::aws_smithy_types::Document>,
}

/// `HarnessOpenAiModelConfig`: a model served by an OpenAI-compatible API.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessOpenAiModelConfig")]
#[derive(Debug, Clone, Default)]
pub struct HarnessOpenAiModelConfig {
    pub model_id: String,
    pub api_key_arn: String,
    pub api_base: Option<String>,
    pub max_tokens: Option<i32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub api_format: Option<String>,
    pub additional_params: Option<::aws_smithy_types::Document>,
}

/// `HarnessGeminiModelConfig`: a model served by the Gemini API.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessGeminiModelConfig")]
#[derive(Debug, Clone, Default)]
pub struct HarnessGeminiModelConfig {
    pub model_id: String,
    pub api_key_arn: String,
    pub max_tokens: Option<i32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub top_k: Option<i32>,
    pub additional_params: Option<::aws_smithy_types::Document>,
}

/// `HarnessLiteLlmModelConfig`: a model served through a LiteLLM proxy.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessLiteLlmModelConfig")]
#[derive(Debug, Clone, Default)]
pub struct HarnessLiteLlmModelConfig {
    pub model_id: String,
    pub api_key_arn: Option<String>,
    pub api_base: Option<String>,
    pub max_tokens: Option<i32>,
    pub temperature: Option<f32>,
    pub top_p: Option<f32>,
    pub additional_params: Option<::aws_smithy_types::Document>,
}

/// `HarnessSystemContentBlock`: one block of the system prompt the turn is given.
#[smithy_macros::smithy(union_for = "com.amazonaws.bedrockagentcore#HarnessSystemContentBlock")]
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum HarnessSystemContentBlock {
    Text(String),
}

/// The `messageStart` member of `InvokeHarnessStreamOutput`: the role of the message the
/// stream is about to carry.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessMessageStartEvent")]
#[derive(Debug, Clone, Default)]
pub struct HarnessMessageStartEvent {
    pub role: String,
}

/// The `contentBlockDelta` member: one increment of the content block at
/// `contentBlockIndex`.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessContentBlockDeltaEvent")]
#[derive(Debug, Clone)]
pub struct HarnessContentBlockDeltaEvent {
    pub content_block_index: i32,
    pub delta: HarnessContentBlockDelta,
}

/// `HarnessContentBlockDelta`: what one increment carries.
///
/// Only the `text` member is declared. The other four are increments of a tool request, of
/// a tool result and of model reasoning, and a turn this emulator serves has none of those
/// to report (no language model runs here, so no tool is ever requested) — so they are not
/// modelled rather than declared and never written.
#[smithy_macros::smithy(
    union_for = "com.amazonaws.bedrockagentcore#HarnessContentBlockDelta",
    unsupported = ["toolUse", "toolResult", "reasoningContent", "toolResultMetadata"]
)]
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone)]
pub enum HarnessContentBlockDelta {
    Text(String),
}

/// The `contentBlockStop` member: the content block at `contentBlockIndex` is complete.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessContentBlockStopEvent")]
#[derive(Debug, Clone, Default)]
pub struct HarnessContentBlockStopEvent {
    pub content_block_index: i32,
}

/// The `messageStop` member: the message is complete, and why it ended.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessMessageStopEvent")]
#[derive(Debug, Clone, Default)]
pub struct HarnessMessageStopEvent {
    pub stop_reason: String,
}

/// The `metadata` member: the tokens the invocation used and how long it took.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessMetadataEvent")]
#[derive(Debug, Clone)]
pub struct HarnessMetadataEvent {
    pub usage: HarnessTokenUsage,
    pub metrics: HarnessStreamMetrics,
}

/// `HarnessTokenUsage`: the token counts of the invocation.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessTokenUsage")]
#[derive(Debug, Clone, Default)]
pub struct HarnessTokenUsage {
    pub input_tokens: i32,
    pub output_tokens: i32,
    pub total_tokens: i32,
    pub cache_read_input_tokens: Option<i32>,
    pub cache_write_input_tokens: Option<i32>,
}

/// `HarnessStreamMetrics`: the end-to-end latency of the invocation.
#[smithy_macros::smithy(shape = "com.amazonaws.bedrockagentcore#HarnessStreamMetrics")]
#[derive(Debug, Clone, Default)]
pub struct HarnessStreamMetrics {
    pub latency_ms: i64,
}
