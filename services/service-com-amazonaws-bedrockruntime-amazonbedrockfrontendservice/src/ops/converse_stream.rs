// SPDX-License-Identifier: CC0-1.0

//! @op ConverseStream
//!
//! `POST /model/{modelId}/converse-stream -> 200`. Serves one conversational turn
//! as a `vnd.amazon.eventstream` body: the same turn `Converse` serves — the same
//! validation, the same tokenizer, the same assistant text, the same stop reason
//! and the same token counts, all read from [`crate::state`] — framed as the
//! `ConverseStreamOutput` events instead of one JSON object.
//!
//! The frames, in the order the model's union documents them for a turn that
//! completes: `messageStart` (the assistant's role), the `contentBlockDelta`
//! events of each content block of the message, `contentBlockStop`,
//! `messageStop` (the stop reason) and `metadata` (the usage and the latency
//! actually measured). A streaming surface delivers its turn as INCREMENTS a
//! client appends, so each block arrives as several deltas carrying that block's
//! own index ([`crate::state::stream_framing`]), which reassemble to exactly what
//! `Converse` answers with for the same request. No language model runs in this
//! emulator, so where the content is CUT is all that is decided here: the text
//! itself is the one generation stage's ([`crate::state::emulated_completion_text`]),
//! and nothing is echoed back from the request.
//!
//! The supplied `inferenceConfig` bounds that text exactly as it bounds
//! `Converse`'s: it is composed to fit `maxTokens` and it halts where the first
//! configured `stopSequences` entry completes, so no delta carries text past a
//! configured stop sequence.
//!
//! A turn a mandatory `toolConfig.toolChoice` obliged to request a tool carries
//! no text: its block opens with a `contentBlockStart` naming the minted
//! `toolUseId` and the tool, its input arrives in that block's
//! `contentBlockDelta` events, and `messageStop` reports `tool_use`. That input is
//! the argument object the requested tool's own `inputSchema` describes
//! ([`crate::state::tool_input_arguments`]), serialized once and cut into
//! increments like any other content, so the fragments a streaming client
//! reassembles and the document `Converse` answers with for the same request are
//! one value.
//!
//! This operation is hand-authored rather than generated: its output is a
//! `@streaming` union, which the op-stub generator declines
//! (`src/ops/SKIPPED_STUBS.md`) and whose generated `ErasedOperation` shim refuses
//! the request. The framing and the routing seam live in `service_common::event_stream`;
//! this file holds the behaviour.
//!
//! ## Input
//!
//! * `modelId` (ConversationalModelId, len 1..2048, `@httpLabel`) **required**
//! * `messages` (Messages), `system` (SystemContentBlocks)
//! * `inferenceConfig` (InferenceConfiguration), `toolConfig` (ToolConfiguration)
//! * `guardrailConfig` (GuardrailStreamConfiguration),
//!   `additionalModelRequestFields` (Document),
//!   `additionalModelResponseFieldPaths` (len ..10)
//! * `promptVariables` (PromptVariableMap), `requestMetadata` (len 1..16)
//! * `performanceConfig` (PerformanceConfiguration), `serviceTier` (ServiceTier),
//!   `outputConfig` (OutputConfig)
//!
//! ## Output
//!
//! * `stream` (ConverseStreamOutput, `@httpPayload`, `@streaming`) — the body; the
//!   response binds no modelled header.
//!
//! ## Errors
//!
//! * `ValidationException` (HTTP 400) — the input fails the constraints Amazon
//!   Bedrock states (a `modelId` that is not a model identifier, a conversation
//!   with no turn to answer, a model the Converse API does not serve).
//! * `ResourceNotFoundException` (HTTP 404) — the model identifier names no model.
//! * `AccessDeniedException` (403), `InternalServerException` (500),
//!   `ModelErrorException` (424), `ModelNotReadyException` (429),
//!   `ModelTimeoutException` (408), `ServiceUnavailableException` (503),
//!   `ThrottlingException` (429).

#[allow(unused_imports)]
use super::types::*;
use aws_smithy_runtime_api::http::{Request, Response};
use service_common::event_stream::{
    deserialize_request, error_response, event_stream_only_response,
};
use smithy_server_runtime::{BoxFuture, OperationContext, ServerHttpProtocol, Service};
use std::sync::Arc;

smithy_macros::smithy_model!("smithy-model/com-amazonaws-bedrockruntime-amazonbedrockfrontendservice.json");

/// The operation's MODELED errors; `#[operation_error]` accepts only variants the
/// model declares for it.
#[smithy_macros::operation_error("com.amazonaws.bedrockruntime#ConverseStream")]
#[derive(Debug)]
pub enum ConverseStreamErrorModeled {
    AccessDeniedException(AccessDeniedException),
    InternalServerException(InternalServerException),
    ModelErrorException(ModelErrorException),
    ModelNotReadyException(ModelNotReadyException),
    ModelTimeoutException(ModelTimeoutException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceUnavailableException(ServiceUnavailableException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's own modelled 400 `ValidationException`.
fn validation(message: impl Into<String>) -> ConverseStreamErrorModeled {
    ConverseStreamErrorModeled::ValidationException(ValidationException {
        message: Some(message.into()),
    })
}

fn internal(message: impl Into<String>) -> ConverseStreamErrorModeled {
    ConverseStreamErrorModeled::InternalServerException(InternalServerException {
        message: Some(message.into()),
    })
}

/// The `ConverseStream` operation.
///
/// Owns the shared engine (the constructor seam). The generated routing table holds
/// one of these and `service.rs` holds another wrapped in an `EventStreamRoute`;
/// both reach the same state, because the state lives in the shared engine, not on
/// the operation.
pub struct ConverseStream<Eng> {
    engine: Arc<Eng>,
}

impl<Eng> ConverseStream<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> ConverseStream<Eng> {
    /// The whole request: validate it against the model and the rules the Converse
    /// APIs state, serve the turn, record it in the inference log, and frame the
    /// turn as the event stream.
    ///
    /// `async` for one reason: the guardrail stage is CPU-bound work over
    /// caller-supplied text, so it is awaited off the async worker exactly as
    /// `Converse` and `ApplyGuardrail` await it.
    ///
    /// `region` is the Region the request was signed for, which is where this
    /// account's invocation records are delivered from, and `ctx` is the request's
    /// own context, over which the invocation-logging stage reaches the log group a
    /// CloudWatch Logs destination names.
    async fn respond(
        &self,
        protocol: &dyn ServerHttpProtocol,
        req: &Request,
        ctx: &OperationContext<'_>,
        region: &str,
    ) -> Result<Response, ConverseStreamErrorModeled> {
        let started = std::time::Instant::now();
        let input: ConverseStreamInput =
            deserialize_request(protocol, req, &ConverseStreamInput::SCHEMA)
                .map_err(validation)?;

        let state = crate::state::service_state(&*self.engine)
            .ok_or_else(|| internal("the Bedrock Runtime emulator has no state store"))?;

        let model = {
            let guard = state.lock();
            match guard.resolve_model_for_invocation(&input.model_id) {
                Ok(entry) => entry.clone(),
                // An identifier the service cannot interpret is a 400, as is one
                // naming a model that is not offered for on-demand invocation;
                // one naming a model or resource that is not available here is a
                // 404 (see `crate::state::resolve_model_for_invocation`).
                Err(crate::state::ModelRejection::Invalid(message)) => {
                    return Err(validation(message))
                }
                Err(crate::state::ModelRejection::NotFound(message)) => {
                    return Err(ConverseStreamErrorModeled::ResourceNotFoundException(
                        ResourceNotFoundException {
                            message: Some(message),
                        },
                    ))
                }
            }
        };
        if !model.supports_converse {
            return Err(validation(format!(
                "The model {} does not support the Converse API.",
                model.model_id
            )));
        }
        // The preconditions `Converse` decides too, over the submitted
        // conversation and before anything is generated for it: it OPENS on a
        // `user` turn, and each of its turns carries content.
        let messages = crate::state::validate_conversation(input.messages.as_ref())
            .map_err(validation)?;
        // Every tool result in the conversation has to address a tool request the
        // conversation carries, by the id that request was issued under — the
        // rule `Converse` applies too.
        crate::state::validate_tool_results(messages).map_err(validation)?;

        // The CPU-bound stage of this turn, run OFF the async worker, through the
        // one function `Converse` composes its turn with: the input tokens are a
        // byte-pair encode over the prompt the CALLER chose the size of, so
        // counting them, refusing an input past the resolved model's context
        // window, minting the tool request and serving the prompt cache all
        // happen on the blocking pool, under the one budget every CPU-bound stage
        // of this service makes a caller wait for.
        let composed = crate::state::turn_composition::compose_converse_turn(
            &state,
            &model,
            input.system.as_ref(),
            input.messages.as_ref(),
            input.tool_config.as_ref(),
            input.inference_config.as_ref(),
        )
        .await;
        let turn = match composed {
            Ok(crate::state::turn_composition::Composed::Served(turn)) => turn,
            Ok(crate::state::turn_composition::Composed::Refused(message)) => {
                return Err(validation(message))
            }
            Err(failed) => {
                return Err(ConverseStreamErrorModeled::InternalServerException(
                    InternalServerException {
                        message: Some(failed.message),
                    },
                ))
            }
        };
        // The guardrail stage `Converse` runs, for the same reason: the
        // account-enforced guardrail applies to this request whether or not it
        // asked for a guardrail — and like every synchronous, CPU-bound stage over
        // caller-supplied text it runs off the async worker, under the one budget a
        // caller waits for.
        let turn = {
            let engine = Arc::clone(&self.engine);
            let model = model.clone();
            let system = input.system.clone();
            let messages = input.messages.clone();
            match crate::state::turn_composition::off_worker(move || {
                crate::state::enforced_guardrail::guard_converse_turn(
                    &*engine,
                    &model,
                    system.as_ref(),
                    messages.as_ref(),
                    turn,
                )
            })
            .await
            {
                Ok(turn) => turn,
                Err(failed) => return Err(internal(failed.message)),
            }
        };
        // The guardrail the REQUEST named, evaluated by the one stage `Converse` and
        // `ApplyGuardrail` evaluate the same policies with. A request naming a
        // guardrail revision this account does not hold, or one whose guardrail needs
        // grounding material it carries none of, names no guardrail this turn can be
        // guarded by.
        let guardrail = crate::state::guardrail_evaluation::converse_guardrail(
            &*self.engine,
            input
                .guardrail_config
                .as_ref()
                .and_then(|config| config.guardrail_identifier.as_deref()),
            input
                .guardrail_config
                .as_ref()
                .and_then(|config| config.guardrail_version.as_deref()),
            input
                .guardrail_config
                .as_ref()
                .map_or("", |config| config.trace.as_str()),
            input.system.as_ref(),
            input.messages.as_ref(),
        )
        .map_err(validation)?;
        // The same seam, for the same reason: these policies are synchronous,
        // CPU-bound work over caller-supplied text.
        let (turn, guardrail_trace) = match guardrail {
            None => (turn, None),
            Some(guardrail) => {
                match crate::state::turn_composition::off_worker(move || {
                    crate::state::guardrail_evaluation::guard_turn(guardrail, turn)
                })
                .await
                {
                    Ok(guarded) => guarded,
                    // The guardrail did not answer, so there is no turn to stream: a
                    // turn streamed unguarded would be one this request asked to be
                    // guarded.
                    Err(failed) => return Err(internal(failed.message)),
                }
            }
        };

        let logged = crate::state::InvocationLogEntry {
            model_id: model.model_id.clone(),
            kind: crate::state::InvokeKind::ConverseStream,
            input_tokens: turn.input_tokens,
            output_tokens: turn.output_tokens,
            at_millis: crate::state::now_millis(),
        };
        state.lock().log_invocation(logged.clone());
        // The invocation-logging stage every inference surface runs: while this
        // account has a logging destination installed, the call that was just
        // served delivers its own record there, carrying the request it arrived
        // with and the very turn its events deliver
        // (see `crate::state::invocation_logging`).
        crate::state::invocation_logging::deliver(
            &*self.engine,
            ctx,
            &crate::state::invocation_logging::LoggedInvocation {
                operation: crate::state::invocation_logging::OPERATION_CONVERSE_STREAM,
                model_id: model.model_id.clone(),
                region: region.to_string(),
                input_body: crate::state::invocation_logging::converse_request_body(
                    &ConverseStreamInput::SCHEMA,
                    &input,
                ),
                output_body: crate::state::invocation_logging::converse_answer_body(
                    turn.content(),
                    turn.stop_reason(),
                    &turn.usage(),
                ),
                input_tokens: turn.input_tokens,
                output_tokens: turn.output_tokens,
                at_millis: logged.at_millis,
            },
        )
        .await;
        // The duration this call reports in its metadata event, measured over
        // serving the turn.
        let latency_ms = crate::state::latency_ms(started.elapsed());
        // The token volume and the latency this call reports in its metadata
        // event are also its contribution to the `AWS/Bedrock` series
        // dimensioned on the invoked model, beside the one successful request it
        // is, published with no state guard of this service held.
        crate::state::invocation_metrics::publish_served_invocation(
            &*self.engine,
            &logged,
            latency_ms,
        );

        // The assistant message this turn is, block by block — the same message
        // `Converse` answers with, so the two surfaces cannot drift.
        let blocks = turn.content();
        let mut frames = Vec::new();
        self.frame(
            protocol,
            &mut frames,
            "messageStart",
            &MessageStartEvent {
                role: "assistant".to_string(),
            },
            &MessageStartEvent::SCHEMA,
        )?;
        for (index, block) in blocks.iter().enumerate() {
            let content_block_index = i32::try_from(index).unwrap_or(i32::MAX);
            // A tool request opens its block with the id and the name, and its
            // input arrives in the block's deltas — the only place a streaming
            // client reads it.
            let content = match block {
                ContentBlock::toolUse(tool_use) => {
                    self.frame(
                        protocol,
                        &mut frames,
                        "contentBlockStart",
                        &ContentBlockStartEvent {
                            start: ContentBlockStart::toolUse(ToolUseBlockStart {
                                tool_use_id: tool_use.tool_use_id.clone(),
                                name: tool_use.name.clone(),
                                r#type: tool_use.r#type.clone(),
                            }),
                            content_block_index,
                        },
                        &ContentBlockStartEvent::SCHEMA,
                    )?;
                    crate::state::document_to_json(&tool_use.input).to_string()
                }
                ContentBlock::Text(text) => text.clone(),
                // `ConverseTurn::content` answers with those two kinds only.
                _ => continue,
            };
            // Which kind of content this block carries is a property of the
            // block, so it is decided once rather than per increment.
            let carries_a_tool_request = matches!(block, ContentBlock::toolUse(_));
            // The block is delivered as the increments a client appends: each
            // delta carries a piece of this block's content and this block's own
            // index, and the pieces reassemble to the whole of it.
            for piece in crate::state::stream_framing::delta_pieces(&content) {
                let delta = if carries_a_tool_request {
                    ContentBlockDelta::toolUse(ToolUseBlockDelta {
                        input: piece.to_string(),
                    })
                } else {
                    ContentBlockDelta::Text(piece.to_string())
                };
                self.frame(
                    protocol,
                    &mut frames,
                    "contentBlockDelta",
                    &ContentBlockDeltaEvent {
                        delta,
                        content_block_index,
                    },
                    &ContentBlockDeltaEvent::SCHEMA,
                )?;
            }
            self.frame(
                protocol,
                &mut frames,
                "contentBlockStop",
                &ContentBlockStopEvent {
                    content_block_index,
                },
                &ContentBlockStopEvent::SCHEMA,
            )?;
        }
        self.frame(
            protocol,
            &mut frames,
            "messageStop",
            &MessageStopEvent {
                stop_reason: turn.stop_reason().to_string(),
                additional_model_response_fields: None,
            },
            &MessageStopEvent::SCHEMA,
        )?;
        self.frame(
            protocol,
            &mut frames,
            "metadata",
            &ConverseStreamMetadataEvent {
                usage: turn.usage(),
                metrics: ConverseStreamMetrics { latency_ms },
                // The guardrail's own assessments, reported only where the request
                // asked for the trace.
                trace: guardrail_trace.map(|guardrail| ConverseStreamTrace {
                    guardrail: Some(guardrail),
                    prompt_router: None,
                }),
                performance_config: input.performance_config.clone(),
                service_tier: input.service_tier.clone(),
            },
            &ConverseStreamMetadataEvent::SCHEMA,
        )?;

        // `ConverseStreamResponse` binds no header: the stream IS the response.
        Ok(event_stream_only_response(frames))
    }

    /// Append one `ConverseStreamOutput` event to the stream.
    fn frame(
        &self,
        protocol: &dyn ServerHttpProtocol,
        frames: &mut Vec<u8>,
        event_type: &str,
        event: &dyn ::aws_smithy_schema::serde::SerializableStruct,
        schema: &::aws_smithy_schema::Schema,
    ) -> Result<(), ConverseStreamErrorModeled> {
        let frame =
            frame_padded_event(protocol, event_type, event, schema).map_err(|error| {
                internal(format!(
                    "the {event_type} event could not be framed: {error}"
                ))
            })?;
        frames.extend_from_slice(&frame);
        Ok(())
    }
}

impl<Eng: Send + Sync + 'static> service_common::event_stream::EventStreamOperation
    for ConverseStream<Eng>
{
    fn serve<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r Request,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, Response> {
        Box::pin(async move {
            let protocol = svc.definition().protocol();
            let response = match self
                .respond(protocol, req, ctx, &crate::state::request_region(ctx.parts()))
                .await
            {
                Ok(response) => response,
                Err(error) => error_response(protocol, &error),
            };
            // One access record per request, so a refused stream is diagnosable.
            // No request payload and no credential reaches it.
            tracing::debug!(
                operation = "ConverseStream",
                service = ctx.identity().signing_name(),
                method = ctx.parts().method.as_str(),
                status = response.status().as_u16(),
                "served an event-stream request"
            );
            response
        })
    }
}
