// SPDX-License-Identifier: CC0-1.0

//! @op InvokeHarness
//!
//! `POST /harnesses/invoke -> 200`. Invokes the managed agent loop of one harness and
//! answers with the turn it served as a `vnd.amazon.eventstream` body: the `messageStart`,
//! `contentBlockDelta`, `contentBlockStop`, `messageStop` and `metadata` events of
//! `InvokeHarnessStreamOutput`.
//!
//! This operation is hand-authored rather than generated: its output member `stream`
//! targets a `@streaming` union, which the op-stub generator declines
//! (`src/ops/SKIPPED_STUBS.md`) and whose generated `ErasedOperation` shim refuses the
//! request. The framing and the routing seam live in `service_common::event_stream` and
//! `src/service.rs`; this file holds the behaviour, the shapes are in
//! `src/ops/types/invoke_harness_types.rs`, and what the turn is composed from is
//! [`crate::state::harness_turn`].
//!
//! # What one invocation does
//!
//! 1. IAM decides, before anything the request named is resolved: the identity-based
//!    policies in force on the calling principal say whether it may invoke the harness the
//!    request addresses ([`crate::state::authorize`]), so a refusal discloses nothing about
//!    what exists.
//! 2. The HARNESS is resolved against the AgentCore CONTROL plane, which owns harnesses
//!    and their endpoints ([`crate::state::resolve_harness`]); a `harnessArn` naming none,
//!    and a `qualifier` naming no endpoint on one, are the modelled 404.
//! 3. The MODEL is the one the invocation configured, else the one the stored harness is
//!    configured with. An invocation with neither cannot run a turn at all, and a model
//!    served by a third-party provider cannot be reached from this emulator — each is
//!    refused in its own modelled class rather than answered with a turn nothing composed.
//! 4. The SESSION the invocation runs in is recorded against the AgentCore Runtime
//!    environment the harness runs on ([`crate::state::harness_session`]), which is what a
//!    later `StopRuntimeSession` addressed at that environment's runtime ARN stops.
//! 5. The TURN is composed and framed: one assistant message carrying one text block, then
//!    the stop reason and the metadata.
//!
//! # No language model runs in this emulator
//!
//! The text the stream carries is the notice the Bedrock Runtime plane of this same
//! emulator answers every inference surface with, composed by that plane's own generation
//! stage from the facts of the invocation really served (see
//! [`crate::state::harness_turn`]). It is never the caller's own messages handed back as if
//! an agent had replied, and never prose synthesised to look like orchestration. Everything
//! around it is real: the token counts are that plane's tokenizer over the text that went
//! in and the text that came out, the stop reason is whether the configured generation
//! ceiling cut the notice off, and the latency is how long serving this invocation took.
//!
//! No tool is therefore ever requested, which is why the stream carries no
//! `contentBlockStart`, no tool-use delta and no `hookEvent`: this emulator reports nothing
//! for work nothing performed.
//!
//! ## Input
//!
//! * `harnessArn` (HarnessArn, `@httpQuery("harnessArn")`) **required** — the harness to
//!   invoke.
//! * `qualifier` (HarnessEndpointName, `@httpQuery("qualifier")`) — an endpoint of that
//!   harness; an invocation that names none addresses the harness itself.
//! * `runtimeSessionId` (SessionId, len 33..100,
//!   `@httpHeader("X-Amzn-Bedrock-AgentCore-Runtime-Session-Id")`) **required** — the
//!   session this invocation runs in.
//! * `messages` (HarnessMessages) **required** — the conversation.
//! * `model` (HarnessModelConfiguration) — the model to run the turn against; the stored
//!   harness's own is used when the invocation names none.
//! * `systemPrompt`, `allowedTools`, `maxIterations`, `maxTokens`, `timeoutSeconds`,
//!   `actorId`, `runtimeUserId` and the tracing headers — the rest of the invocation.
//!
//! ## Output
//!
//! * `stream` (InvokeHarnessStreamOutput, `@httpPayload`, `@streaming`) — the body.
//!
//! ## Errors
//!
//! * `ResourceNotFoundException` (HTTP 404) — the `harnessArn` names no harness of this
//!   account, or the `qualifier` names no endpoint on it.
//! * `ValidationException` (HTTP 400) — the request could not be read, or neither it nor
//!   the harness it addresses configures a model to run the turn against.
//! * `RuntimeClientError` (HTTP 424) — the configured model is served by a provider this
//!   emulator cannot reach.
//! * `AccessDeniedException` (403), `InternalServerException` (500),
//!   `ServiceQuotaExceededException` (402), `ThrottlingException` (429).

#[allow(unused_imports)]
use super::types::*;
use aws_smithy_runtime_api::http::{Request, Response};
use service_common::event_stream::{
    deserialize_request, error_response, event_stream_only_response, frame_event,
};
use smithy_server_runtime::{BoxFuture, OperationContext, ServerHttpProtocol, Service};
use std::sync::Arc;
use std::time::{Instant, SystemTime};

smithy_macros::smithy_model!("smithy-model/com-amazonaws-bedrockagentcore-amazonbedrockagentcore.json");

/// The operation's MODELED errors; `#[operation_error]` accepts only variants the model
/// declares for it.
#[smithy_macros::operation_error("com.amazonaws.bedrockagentcore#InvokeHarness")]
#[derive(Debug)]
pub enum InvokeHarnessErrorModeled {
    AccessDeniedException(AccessDeniedException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    RuntimeClientError(RuntimeClientError),
    ServiceQuotaExceededException(ServiceQuotaExceededException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// The index of the one content block a served turn carries: the whole assistant message
/// is one text block, so every event about it names block zero.
const FIRST_CONTENT_BLOCK: i32 = 0;

/// The `InvokeHarnessStreamOutput` members a served turn is framed as, each the
/// `:event-type` of one frame (source: `smithy_query(query="shape",
/// shape="InvokeHarnessStreamOutput")`).
const EVENT_MESSAGE_START: &str = "messageStart";
/// See [`EVENT_MESSAGE_START`].
const EVENT_CONTENT_BLOCK_DELTA: &str = "contentBlockDelta";
/// See [`EVENT_MESSAGE_START`].
const EVENT_CONTENT_BLOCK_STOP: &str = "contentBlockStop";
/// See [`EVENT_MESSAGE_START`].
const EVENT_MESSAGE_STOP: &str = "messageStop";
/// See [`EVENT_MESSAGE_START`].
const EVENT_METADATA: &str = "metadata";

/// This operation's modelled 400, for a request that cannot be read or that configures no
/// model to run its turn against.
///
/// `reason` is `ValidationExceptionReason`'s `FieldValidationFailed` (source:
/// `smithy_query(query="shape", shape="ValidationExceptionReason")`), which is the
/// member-level failure each case here is.
fn validation(message: impl Into<String>) -> InvokeHarnessErrorModeled {
    InvokeHarnessErrorModeled::ValidationException(ValidationException {
        message: message.into(),
        reason: "FieldValidationFailed".to_string(),
        field_list: None,
    })
}

fn internal(message: impl Into<String>) -> InvokeHarnessErrorModeled {
    InvokeHarnessErrorModeled::InternalServerException(InternalServerException {
        message: Some(message.into()),
    })
}

/// The `InvokeHarness` operation.
///
/// Owns the shared engine (the constructor seam). The generated routing table holds one of
/// these and `service.rs` holds another wrapped in an `EventStreamRoute`; both reach the
/// same state, because the state lives in the shared engine, not on the operation.
pub struct InvokeHarness<Eng> {
    engine: Arc<Eng>,
}

impl<Eng> InvokeHarness<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> InvokeHarness<Eng> {
    /// The whole request: resolve the harness, compose the turn it serves, record the
    /// session it ran in, and frame the turn as this operation's event stream.
    async fn respond(
        &self,
        protocol: &dyn ServerHttpProtocol,
        req: &Request,
        ctx: &OperationContext<'_>,
    ) -> Result<Response, InvokeHarnessErrorModeled> {
        let started = Instant::now();
        let input: InvokeHarnessInput =
            deserialize_request(protocol, req, &InvokeHarnessInput::SCHEMA).map_err(validation)?;

        // IAM decides FIRST: the resource this call addresses is the harness its ARN names,
        // taken from the request alone.
        crate::state::authorize(&*self.engine, ctx, "InvokeHarness", &input.harness_arn)
            .map_err(InvokeHarnessErrorModeled::AccessDeniedException)?;

        let state = crate::state::service_state(&*self.engine)
            .ok_or_else(|| internal("the shared engine does not carry this service's state"))?;

        // The harness this invocation reached, read off the component that owns harnesses.
        // Resolved and released before this service's own store is taken, which is the lock
        // order every reader of the owner's store keeps.
        let harness = crate::state::resolve_harness(
            &*self.engine,
            &input.harness_arn,
            input.qualifier.as_deref(),
        )
        .map_err(|unavailable| match unavailable {
            crate::state::HarnessUnavailable::NotFound(message) => {
                InvokeHarnessErrorModeled::ResourceNotFoundException(ResourceNotFoundException {
                    message: Some(message),
                })
            }
            crate::state::HarnessUnavailable::StoreUnavailable(message) => internal(message),
        })?;

        // The model the turn runs against: the invocation's own configuration, else the
        // stored harness's. Neither is `@required`, and AWS publishes no default model id,
        // so an invocation with neither has no model to run a turn against rather than one
        // this emulator invented.
        let model = input
            .model
            .as_ref()
            .map(crate::state::ConfiguredModel::of_request)
            .or_else(|| harness.model.clone())
            .ok_or_else(|| {
                validation(
                    "No model is configured for this harness. Configure a model on the \
                     harness or supply one in the request.",
                )
            })?;
        // A model served by a third-party provider would have to be reached over the
        // public internet, which this emulator does not do: there is no turn to report for
        // a model nothing asked, and `RuntimeClientError` is the modelled class for a
        // runtime client that could not reach what it was configured against.
        if !model.provider.is_served_in_process() {
            return Err(InvokeHarnessErrorModeled::RuntimeClientError(
                RuntimeClientError {
                    message: Some(format!(
                        "The harness is configured to run {} on {}, which this emulator \
                         cannot reach.",
                        model.model_id,
                        model.provider.label()
                    )),
                },
            ));
        }

        // The generation ceiling in force: the invocation's own, else the model
        // configuration's, else the harness's.
        let max_output_tokens = input.max_tokens.or(model.max_tokens).or(harness.max_tokens);
        // The system prompt in force: the invocation's own when it carries one, else the
        // harness's — the request REPLACES the configured prompt rather than appending to
        // it, which is what makes an invocation able to run the harness with another one.
        let system_prompt: Vec<String> = match input.system_prompt.as_ref() {
            Some(blocks) => blocks
                .iter()
                .map(|block| match block {
                    HarnessSystemContentBlock::Text(text) => text.clone(),
                })
                .collect(),
            None => harness.system_prompt.clone(),
        };
        // Counting the tokens of a conversation the CALLER chose the size of is CPU-bound
        // work (a byte-pair encode), so it happens on a blocking thread rather than on an
        // async worker, with no state guard of this service held: one invocation's prompt
        // cannot stall another caller's request.
        let prompt = crate::state::harness_prompt_text(&input.messages, &system_prompt);
        let model_id = model.model_id.clone();
        let message_count = u32::try_from(input.messages.len()).unwrap_or(u32::MAX);
        let turn = ::tokio::task::spawn_blocking(move || {
            crate::state::harness_turn(&model_id, message_count, &prompt, max_output_tokens)
        })
        .await
        .map_err(|error| internal(format!("the turn could not be composed: {error}")))?;
        let now = SystemTime::now();
        // Invoking a harness is what this operation DOES, so the session it ran in is
        // written: the runtime environment the harness runs on holds it, which is what a
        // later stop of that session addresses.
        state
            .lock()
            .insert_runtime_session(crate::state::harness_session(
                &harness,
                &input.runtime_session_id,
                now,
            ));

        let mut frames = self.frame(
            protocol,
            &turn,
            // The latency the metadata reports is measured, not chosen: how long this
            // invocation took from the request being read to its turn being framed.
            i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX),
        )?;
        frames.shrink_to_fit();
        Ok(event_stream_only_response(frames))
    }

    /// The frames one served turn is: the message it opens, its one text block, the stop
    /// reason it ended on and the metadata of the invocation.
    fn frame(
        &self,
        protocol: &dyn ServerHttpProtocol,
        turn: &crate::state::HarnessTurn,
        latency_ms: i64,
    ) -> Result<Vec<u8>, InvokeHarnessErrorModeled> {
        let mut frames = Vec::new();
        let framed = |event_type: &str,
                      event: &dyn ::aws_smithy_schema::serde::SerializableStruct,
                      schema: &::aws_smithy_schema::Schema| {
            frame_event(protocol, event_type, false, event, schema).map_err(|error| {
                internal(format!("the {event_type} event could not be framed: {error}"))
            })
        };
        frames.extend(framed(
            EVENT_MESSAGE_START,
            &HarnessMessageStartEvent {
                // A served turn is the assistant's side of the conversation.
                role: crate::state::HARNESS_ROLE_ASSISTANT.to_string(),
            },
            &HarnessMessageStartEvent::SCHEMA,
        )?);
        frames.extend(framed(
            EVENT_CONTENT_BLOCK_DELTA,
            &HarnessContentBlockDeltaEvent {
                content_block_index: FIRST_CONTENT_BLOCK,
                delta: HarnessContentBlockDelta::Text(turn.text.clone()),
            },
            &HarnessContentBlockDeltaEvent::SCHEMA,
        )?);
        frames.extend(framed(
            EVENT_CONTENT_BLOCK_STOP,
            &HarnessContentBlockStopEvent {
                content_block_index: FIRST_CONTENT_BLOCK,
            },
            &HarnessContentBlockStopEvent::SCHEMA,
        )?);
        frames.extend(framed(
            EVENT_MESSAGE_STOP,
            &HarnessMessageStopEvent {
                stop_reason: turn.stop_reason.clone(),
            },
            &HarnessMessageStopEvent::SCHEMA,
        )?);
        frames.extend(framed(
            EVENT_METADATA,
            &HarnessMetadataEvent {
                usage: HarnessTokenUsage {
                    input_tokens: turn.input_tokens,
                    output_tokens: turn.output_tokens,
                    total_tokens: turn.total_tokens,
                    // No prompt cache is served, so nothing was read from or written to
                    // one.
                    cache_read_input_tokens: None,
                    cache_write_input_tokens: None,
                },
                metrics: HarnessStreamMetrics { latency_ms },
            },
            &HarnessMetadataEvent::SCHEMA,
        )?);
        Ok(frames)
    }
}

impl<Eng: Send + Sync + 'static> service_common::event_stream::EventStreamOperation
    for InvokeHarness<Eng>
{
    fn serve<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r Request,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, Response> {
        Box::pin(async move {
            let protocol = svc.definition().protocol();
            let response = match self.respond(protocol, req, ctx).await {
                Ok(response) => response,
                Err(error) => error_response(protocol, &error),
            };
            // One access record per request, so a refused invocation is diagnosable. No
            // request payload and no credential reaches it.
            tracing::debug!(
                operation = "InvokeHarness",
                service = ctx.identity().signing_name(),
                method = ctx.parts().method.as_str(),
                status = response.status().as_u16(),
                "served an event-stream request"
            );
            response
        })
    }
}
