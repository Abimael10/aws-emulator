// SPDX-License-Identifier: CC0-1.0

//! @op InvokeInlineAgent
//!
//! `POST /agents/{sessionId} -> 200`. Serves one invocation of an agent the REQUEST
//! composes — "Invokes an inline Amazon Bedrock agent using the configurations you provide
//! with the request" — as a `vnd.amazon.eventstream` body: the `chunk`, `returnControl` and
//! `trace` events the modelled `InlineAgentResponseStream` declares.
//!
//! ## What is real here
//!
//! * there is NO stored agent to resolve: the agent is composed by the call, so what is
//!   resolved instead is the `foundationModel` it orchestrates with, read in the plane that
//!   owns the model catalogue (`crate::state::inline_agent`) — an identifier naming no model
//!   this emulator publishes is the modelled `ValidationException` (400) or
//!   `ResourceNotFoundException` (404), exactly as the inference plane classifies it, and
//!   never an empty stream. What the reference does NOT have to do is serve inference: no
//!   model is invoked here, so a model past its end of life still composes an inline agent,
//!   while one of a non-text modality — which generates no turn of a conversation at all —
//!   is refused;
//! * the `sessionId` label is checked against the model before anything is looked up, so a
//!   malformed one is the modelled `ValidationException` (400);
//! * the caller's permission for `bedrock:InvokeInlineAgent` on that model is decided before
//!   any store is read, so an unauthorized call is answered the same way whether or not the
//!   model exists;
//! * the CONVERSATION is this plane's own store: the caller's own `sessionId` opens one,
//!   reusing it continues the same conversation, `endSession` ends it, and one idle past its
//!   `idleSessionTTLInSeconds` is released — which is why the answer can report which turn
//!   of which conversation was served;
//! * RETURNING CONTROL is this operation's own half of a contract that needs no model: an
//!   action group whose `actionGroupExecutor` is `customControl: RETURN_CONTROL` is carried
//!   out by the CALLER — "to return the action group invocation results directly in the
//!   `InvokeInlineAgent` response, specify `RETURN_CONTROL`" — so the turn hands the
//!   functions that action group declares back as the modelled `returnControl` event, the
//!   LAST member of the turn, under an `invocationId` this plane issues and remembers on the
//!   conversation. The caller then returns the results under that id in
//!   `inlineSessionState.returnControlInvocationResults`, which re-enter the next turn's
//!   orchestration prompt; an `invocationId` no response of this conversation issued is the
//!   modelled `ValidationException` (400), and a result list outside the modelled 1..5 is
//!   refused by the model's own bound. That bound's refusal QUOTES the results the request
//!   submitted, rendered the way a value of that shape prints — the list in brackets, each
//!   member under its shape's name over every member that shape declares — because the value
//!   a constraint clause states is the caller's own input; the collection the framework's
//!   walk has no rendering for is restored by this service's own
//!   `crate::service::submitted_collections` layer, which every operation of this service
//!   words that one clause through, and pinned across them by
//!   `tests/all/collection_length_refusals.rs`.
//!
//! NO LANGUAGE MODEL RUNS IN THIS EMULATOR, and nothing here pretends otherwise. The
//! `chunk` events carry the notice the Bedrock Runtime plane of this same emulator answers
//! every inference surface with, extended with the facts of the invocation that was really
//! served (`crate::state::inline_agent::completion_notice`): the model the agent was
//! composed against, the turn and conversation, the capabilities the request configured
//! that went unused, and the results that re-entered the turn. The caller's own `inputText`
//! is never handed back as if an agent had replied to it, a chunk carries no `attribution`
//! (no knowledge base was queried, so there is no source to cite), and no parameter VALUE is
//! invented for a function handed back: a value is what a model elicits from the
//! conversation, so the `returnControl` event carries the parameter NAMES and TYPES the
//! request declared and nothing more.
//!
//! ## What it refuses rather than answers
//!
//! A leg this emulator cannot compute is REFUSED with the modelled `ValidationException`,
//! because a plausible-looking success is worse than an error: `guardrailConfiguration` and
//! `streamingConfigurations.applyGuardrailInterval` (no guardrail policy is evaluated, so
//! neither an intervention nor its absence can be reported), `promptOverrideConfiguration`
//! and `promptCreationConfigurations` (both change a prompt nothing reads), the four
//! `inlineSessionState` members that ask for work nothing here performs (its
//! `sessionAttributes` and `promptSessionAttributes` replace those placeholders, its `files`
//! are read by a code interpreter nothing runs and its `conversationHistory` replaces the
//! conversation this plane records itself — each refused by name, because every member of
//! that shape is declared and so the handler sees which one the request carried),
//! `collaborators` / `collaboratorConfigurations` / a non-`DISABLED` `agentCollaboration`
//! (no collaboration is orchestrated) and `customOrchestration` /
//! `orchestrationType: CUSTOM_ORCHESTRATION` (a custom orchestration runs a function this
//! emulator invokes nothing of).
//!
//! `actionGroups` and `knowledgeBases` are SERVED: a knowledge base is queried only once a
//! model elicits a query, so the notice states how many of each were configured, while an
//! action group executed by the CALLER is served by returning control for it, as above. An
//! action group's `apiSchema` stays unread: it names its operations inside an OpenAPI
//! document nothing here parses, so no operation of one can be elicited. `enableTrace` adds
//! a report ALONGSIDE the answer: the turn then carries the modelled `trace` events for the
//! orchestration step it really performed — the prompt it assembled
//! (`orchestrationTrace.modelInvocationInput`) and the action-group invocation it returned
//! control for (`orchestrationTrace.invocationInput`) — and no step a model would have
//! taken past them is invented. `bedrockModelConfigurations` names a latency profile, which
//! does not change what an answer says, and `streamingConfigurations.streamFinalResponse` is
//! honoured as the FRAMING it is: the same notice delivered in several `chunk` events
//! instead of one.
//!
//! This operation is hand-authored rather than generated: its output is a `@streaming`
//! union, which the op-stub generator declines (`src/ops/SKIPPED_STUBS.md`) and whose
//! generated `ErasedOperation` shim would decline the request. The framing and the routing
//! seam live in `service_common::event_stream`, the shapes in
//! `super::types::invoke_inline_agent_types`, and `crate::service` routes a request here
//! through `Service::override_operation`.
//!
//! ## Input
//!
//! * `sessionId` (SessionId, len 2..100, pattern `^[0-9a-zA-Z._:-]+$`, `@httpLabel`)
//!   **required** — the conversation the turn is served in.
//! * `foundationModel` (ModelIdentifier, len 1..2048) **required** — the model the agent
//!   orchestrates with.
//! * `instruction` (Instruction, len 40.., `@sensitive`) **required**.
//! * `customerEncryptionKeyArn` (KmsKeyArn), `idleSessionTTLInSeconds` (SessionTTL, range
//!   60..3600), `agentName` (Name, `@sensitive`), `agentCollaboration`
//!   (AgentCollaboration), `orchestrationType` (OrchestrationType), `inputText` (InputText,
//!   len ..25000000, `@sensitive`), `endSession` (Boolean), `enableTrace` (Boolean).
//! * The structured members — `actionGroups`, `knowledgeBases`, `guardrailConfiguration`,
//!   `promptOverrideConfiguration`, `collaboratorConfigurations`, `collaborators`,
//!   `inlineSessionState`, `streamingConfigurations`, `promptCreationConfigurations`,
//!   `bedrockModelConfigurations` and `customOrchestration`.
//!
//! ## Output
//!
//! * `completion` (InlineAgentResponseStream, `@httpPayload`, `@streaming`) **required** —
//!   the body.
//! * `contentType` (`@httpHeader x-amzn-bedrock-agent-content-type`) **required**,
//!   `sessionId` (`@httpHeader x-amz-bedrock-agent-session-id`) **required**.
//!
//! ## Errors
//!
//! * `AccessDeniedException` (HTTP 403), `BadGatewayException` (HTTP 502),
//!   `ConflictException` (HTTP 409), `DependencyFailedException` (HTTP 424),
//!   `InternalServerException` (HTTP 500), `ResourceNotFoundException` (HTTP 404),
//!   `ServiceQuotaExceededException` (HTTP 400), `ThrottlingException` (HTTP 429),
//!   `ValidationException` (HTTP 400) — input validation failed.

#[allow(unused_imports)]
use super::types::*;

use aws_smithy_runtime_api::http::{Request, Response};
use service_common::event_stream::{
    error_response, event_stream_response, frame_event, read_request, EventStreamOperation,
    RequestRefusal,
};
use smithy_server_runtime::{BoxFuture, OperationContext, ServerHttpProtocol, Service};

use crate::state::inline_agent as inline;

smithy_macros::smithy_model!("smithy-model/com-amazonaws-bedrockagentruntime-amazonbedrockagentruntimeservice.json");

/// The operation's MODELED errors; `#[operation_error]` accepts only variants the model
/// declares for it. No `Scaffolded` wrapper: this operation is implemented, so the 501
/// channel has nothing to carry.
#[smithy_macros::operation_error("com.amazonaws.bedrockagentruntime#InvokeInlineAgent")]
#[derive(Debug)]
pub enum InvokeInlineAgentError {
    AccessDeniedException(AccessDeniedException),
    BadGatewayException(BadGatewayException),
    ConflictException(ConflictException),
    DependencyFailedException(DependencyFailedException),
    InternalServerException(InternalServerException),
    ResourceNotFoundException(ResourceNotFoundException),
    ServiceQuotaExceededException(ServiceQuotaExceededException),
    ThrottlingException(ThrottlingException),
    ValidationException(ValidationException),
}

/// This operation's modelled 400 `ValidationException`, naming the member that was wrong
/// and what it requires — the caller has to fix the request from this text alone.
fn validation(message: impl Into<String>) -> InvokeInlineAgentError {
    InvokeInlineAgentError::ValidationException(ValidationException {
        message: Some(message.into()),
    })
}

/// This operation's modelled 404 `ResourceNotFoundException`.
fn not_found(message: String) -> InvokeInlineAgentError {
    InvokeInlineAgentError::ResourceNotFoundException(ResourceNotFoundException {
        message: Some(message),
    })
}

/// This operation's modelled 500 `InternalServerException`: a store this emulator should
/// carry is not on the engine, which is never reported as a client fault.
fn internal(message: impl Into<String>) -> InvokeInlineAgentError {
    InvokeInlineAgentError::InternalServerException(InternalServerException {
        message: Some(message.into()),
        reason: None,
    })
}

/// This operation's modelled 400 `ServiceQuotaExceededException`, answered when a resource
/// guard of this emulator's store is reached.
fn quota(message: String) -> InvokeInlineAgentError {
    InvokeInlineAgentError::ServiceQuotaExceededException(ServiceQuotaExceededException {
        message: Some(message),
    })
}

/// The `InlineAgentResponseStream` member one part of the agent's answer travels under,
/// which the frame carries as its `:event-type`
/// (`smithy_query(query="shape",shape="InlineAgentResponseStream")`).
const CHUNK_EVENT: &str = "chunk";

/// The member of that same stream an invocation handed back to the caller travels under.
const RETURN_CONTROL_EVENT: &str = "returnControl";

/// The member of that same stream one part of the orchestration trace travels under.
const TRACE_EVENT: &str = "trace";

/// The operation name, which names the IAM action this call is authorized against
/// (`bedrock:InvokeInlineAgent`). Declared here because this operation has no generated
/// `impl Operation` to carry a `NAME`.
const OPERATION_NAME: &str = "InvokeInlineAgent";

/// The member path the model reference travels in, so a refusal about it names the member
/// the caller actually carried.
const FOUNDATION_MODEL_MEMBER: &str = "foundationModel";

/// The `AgentCollaboration` value that asks for NO collaboration, which is the only one
/// this service serves (`smithy_query(query="shape",shape="AgentCollaboration")` ->
/// DISABLED, SUPERVISOR, SUPERVISOR_ROUTER).
const COLLABORATION_DISABLED: &str = "DISABLED";

/// The `OrchestrationType` that asks for an orchestration driven by the caller's own
/// function (`smithy_query(query="shape",shape="OrchestrationType")` -> DEFAULT,
/// CUSTOM_ORCHESTRATION).
const CUSTOM_ORCHESTRATION: &str = "CUSTOM_ORCHESTRATION";

/// The `InvokeInlineAgent` operation.
///
/// Owns the shared engine (the constructor seam). Held by `crate::service` behind an
/// `EventStreamRoute`, because the generated routing table cannot serve a
/// `@streaming`-output operation.
pub struct InvokeInlineAgent<Eng> {
    engine: ::std::sync::Arc<Eng>,
}

impl<Eng> InvokeInlineAgent<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs. Called by the generated `EmulatedService::new(engine)` constructor.
    pub fn new(engine: ::std::sync::Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> InvokeInlineAgent<Eng> {
    /// The whole request once it has been read: authorize it, resolve the model the agent
    /// orchestrates with, refuse what cannot be computed, record the turn and frame the
    /// answer.
    fn respond(
        &self,
        protocol: &dyn ServerHttpProtocol,
        input: &InvokeInlineAgentInput,
        ctx: &OperationContext<'_>,
    ) -> Result<Response, InvokeInlineAgentError> {
        // Authorization is decided against the model the agent orchestrates with — the one
        // resource this call names, and the resource an identity-based policy for
        // `bedrock:InvokeInlineAgent` can scope to — and before any store is read, so an
        // unauthorized call is answered the same way whether or not that model exists.
        let resource = crate::state::target_model::model_resource_arn(ctx, &input.foundation_model);
        if let Err(denied) = crate::state::authorize(&*self.engine, ctx, OPERATION_NAME, &resource)
        {
            return Err(InvokeInlineAgentError::AccessDeniedException(denied));
        }

        let model_id = inline::resolve_model(
            &*self.engine,
            FOUNDATION_MODEL_MEMBER,
            &input.foundation_model,
        )
        .map_err(|refusal| match refusal {
            inline::OrchestrationRefusal::StoreUnavailable => {
                internal("the model catalogue is unavailable")
            }
            inline::OrchestrationRefusal::InvalidModel(message) => validation(message),
            inline::OrchestrationRefusal::ModelNotFound(message) => not_found(message),
        })?;

        // Only once the model has RESOLVED is a leg this service cannot compute refused: a
        // reference naming no model is refused whatever else the request carries, so
        // resolution comes first and this is what a caller whose model exists is told.
        unservable_leg(input)?;

        let served = self.record_turn(input, &model_id)?;
        let mut frames = Vec::new();
        // A trace reports ALONGSIDE the answer, so it comes first: what the turn was
        // assembled from, and the invocation it is about to hand back.
        if served.trace_requested {
            frames.extend_from_slice(&trace_frames(protocol, &served)?);
        }
        match served.returned_control.as_deref() {
            // The agent is WAITING on the caller: the invocation it handed back is the last
            // member of the turn, and there is no answer to deliver beside it.
            Some(invocation_id) => {
                frames.extend_from_slice(&return_control_frame(protocol, &served, invocation_id)?);
            }
            None => {
                let notice = inline::completion_notice(&served);
                // `streamFinalResponse` is about FRAMING: with it the one answer is
                // delivered in several chunks, whose concatenation is the same text a single
                // chunk carries.
                let parts = if streams_final_response(input) {
                    inline::streamed_chunks(&notice)
                } else {
                    vec![notice]
                };
                for part in parts {
                    frames.extend_from_slice(&chunk_frame(protocol, part)?);
                }
            }
        }

        event_stream_response(
            protocol,
            OPERATION_NAME,
            &InvokeInlineAgentOutput {
                content_type: crate::state::agent_invocation::RESPONSE_CONTENT_TYPE.to_string(),
                session_id: served.session_id.clone(),
            },
            &InvokeInlineAgentOutput::SCHEMA,
            frames,
        )
        .map_err(|error| internal(format!("the response could not be serialized: {error}")))
    }

    /// Record the served turn in the conversation the request's `sessionId` addresses,
    /// opening one when this store holds none under that id, and answer the facts of the
    /// invocation the response reports.
    ///
    /// The expiry the clock already owes is materialised FIRST, so an id whose conversation
    /// has idled out begins a new one — which is what the member documents — and the lookup,
    /// the write and the ending all happen under ONE hold of the store, so the turn the
    /// answer reports is the turn this store really holds.
    fn record_turn(
        &self,
        input: &InvokeInlineAgentInput,
        model_id: &str,
    ) -> Result<inline::ServedInvocation, InvokeInlineAgentError> {
        let store = crate::state::service_state(&*self.engine)
            .ok_or_else(|| internal("the inline agent session store is unavailable"))?;
        let at = crate::state::now();
        let mut guard = store.lock();
        inline::expire_idle_conversations(&mut guard.inline_agent_sessions, at);
        let sessions = &mut guard.inline_agent_sessions;
        // The CALLER's half of the return-control contract, decided before anything is
        // written: results are taken only under an `invocationId` this conversation really
        // returned control under, so a request naming one no response of it issued records
        // no turn.
        let returned = returned_invocation(input, sessions.get(&input.session_id))?;
        // A request whose id this store holds none of OPENS a conversation, so what is
        // bounded there is the store; a request that continues one is bounded by the turns
        // that conversation already holds.
        if !sessions.contains_key(&input.session_id)
            && sessions.len() >= inline::MAX_INLINE_AGENT_SESSIONS
        {
            return Err(quota(
                crate::state::knowledge_base_session::session_quota_message(
                    inline::MAX_INLINE_AGENT_SESSIONS,
                    "inline agent sessions",
                ),
            ));
        }
        let session = sessions
            .entry(input.session_id.clone())
            .or_insert_with(|| {
                inline::InlineAgentSession::opened(
                    input.session_id.clone(),
                    input
                        .idle_session_ttl_in_seconds
                        .unwrap_or(inline::DEFAULT_IDLE_SESSION_TTL_SECONDS),
                    input.customer_encryption_key_arn.clone(),
                    at,
                )
            });
        if session.turns.len() >= inline::MAX_TURNS_PER_INLINE_AGENT_SESSION {
            return Err(quota(
                crate::state::knowledge_base_session::session_quota_message(
                    inline::MAX_TURNS_PER_INLINE_AGENT_SESSION,
                    "turns in one inline agent session",
                ),
            ));
        }
        // An inline agent is composed afresh by every request, so the conversation is
        // maintained under the configuration of its latest turn: a turn that names an idle
        // lifetime or a key is the one the conversation is kept under from then on.
        if let Some(ttl) = input.idle_session_ttl_in_seconds {
            session.idle_ttl_seconds = ttl;
        }
        if input.customer_encryption_key_arn.is_some() {
            session.encryption_key_arn = input.customer_encryption_key_arn.clone();
        }
        session.record(inline::InlineAgentTurn {
            input_characters: input
                .input_text
                .as_deref()
                .unwrap_or_default()
                .chars()
                .count(),
            model_id: model_id.to_string(),
            at,
        });
        let turn = session.turns.len();
        // The actions the caller itself carries out, and whether this turn hands them back:
        // a turn that already carries the results of an earlier invocation answers with
        // them instead of handing control back again.
        let offered = inline::offered_functions(input.action_groups.as_deref().unwrap_or_default());
        let returned_control = if returned.is_none() && !offered.is_empty() {
            let invocation_id = crate::state::new_uuid();
            // Remembered on the conversation, which is what lets a later turn of it return
            // the results of this invocation.
            session.issue_invocation(invocation_id.clone());
            Some(invocation_id)
        } else {
            None
        };
        let session_ended = input.end_session == Some(true);
        if session_ended {
            // Ending the session releases the conversation, so a later request under the
            // same id begins a new one.
            sessions.remove(&input.session_id);
        }
        Ok(inline::ServedInvocation {
            session_id: input.session_id.clone(),
            model_id: model_id.to_string(),
            agent_name: input.agent_name.clone(),
            turn,
            action_groups: input.action_groups.as_ref().map_or(0, Vec::len),
            knowledge_bases: input.knowledge_bases.as_ref().map_or(0, Vec::len),
            session_ended,
            trace_requested: input.enable_trace == Some(true),
            instruction: input.instruction.clone(),
            input_text: input.input_text.clone(),
            at,
            offered,
            returned_control,
            returned,
        })
    }
}

/// The results the request returns for an invocation of this conversation, read once the
/// store is held so the id can be checked against the conversation that issued it.
///
/// THE RULE is the model's own, stated on the member: `inlineSessionState.invocationId`
/// "must match the `invocationId` returned in the `InvokeInlineAgent` response for the
/// action whose results are provided in the `returnControlInvocationResults` field"
/// (`smithy_query(query="shape",shape="InlineSessionState")`), so results carrying an id no
/// response of this conversation issued — or no id at all — are the modelled
/// `ValidationException` (400). The list's own 1..5 bound is the model's and is checked at
/// the boundary, before this runs.
///
/// An `invocationId` that matches but answers nothing leaves the turn an ordinary one:
/// there is no result to re-enter its prompt. So does a session state that carries neither
/// of the two — it answers no invocation and names none, so the turn is served from the
/// request's own `inputText`.
fn returned_invocation(
    input: &InvokeInlineAgentInput,
    session: Option<&inline::InlineAgentSession>,
) -> Result<Option<inline::ReturnedInvocation>, InvokeInlineAgentError> {
    let Some(state) = input.inline_session_state.as_ref() else {
        return Ok(None);
    };
    let results = state
        .return_control_invocation_results
        .as_deref()
        .unwrap_or_default();
    let Some(invocation_id) = state.invocation_id.as_deref() else {
        // A session state that answers NOTHING names no invocation, and needs none: it is
        // the request's own inputText that the turn is served from.
        if results.is_empty() {
            return Ok(None);
        }
        return Err(validation(inline::unissued_invocation_message(None)));
    };
    if !session.is_some_and(|session| session.has_issued(invocation_id)) {
        return Err(validation(inline::unissued_invocation_message(Some(
            invocation_id,
        ))));
    }
    if results.is_empty() {
        return Ok(None);
    }
    Ok(Some(inline::ReturnedInvocation {
        invocation_id: invocation_id.to_string(),
        results: inline::returned_results(results),
    }))
}

/// One `chunk` event carrying `part` of the answer.
fn chunk_frame(
    protocol: &dyn ServerHttpProtocol,
    part: String,
) -> Result<Vec<u8>, InvokeInlineAgentError> {
    frame_event(
        protocol,
        CHUNK_EVENT,
        false,
        &InlineAgentPayloadPart {
            bytes: Some(part.into_bytes()),
            // No knowledge base was queried on this path, so there is no retrieved source
            // for the chunk to be attributed to.
            attribution: None,
        },
        &InlineAgentPayloadPart::SCHEMA,
    )
    .map_err(|error| {
        internal(format!(
            "the {CHUNK_EVENT} event could not be framed: {error}"
        ))
    })
}

/// The `returnControl` event the turn hands the caller: the id the results come back under,
/// and one `functionInvocationInput` per function the agent handed back.
///
/// Each input is the DECLARATION the request carried — the action group that declared the
/// function, the function, and the names and types of its parameters. No parameter value is
/// filled in: a value is what a model elicits from the conversation, and none runs here.
fn return_control_frame(
    protocol: &dyn ServerHttpProtocol,
    served: &inline::ServedInvocation,
    invocation_id: &str,
) -> Result<Vec<u8>, InvokeInlineAgentError> {
    let invocation_inputs = served
        .offered
        .iter()
        .map(|function| {
            InvocationInputMember::functionInvocationInput(FunctionInvocationInput {
                action_group: function.action_group.clone(),
                function: Some(function.function.clone()),
                parameters: Some(
                    function
                        .parameters
                        .iter()
                        .map(|parameter| FunctionParameter {
                            name: Some(parameter.name.clone()),
                            r#type: Some(parameter.parameter_type.clone()),
                            value: None,
                        })
                        .collect(),
                ),
                ..FunctionInvocationInput::default()
            })
        })
        .collect();
    frame_event(
        protocol,
        RETURN_CONTROL_EVENT,
        false,
        &InlineAgentReturnControlPayload {
            invocation_inputs: Some(invocation_inputs),
            invocation_id: Some(invocation_id.to_string()),
        },
        &InlineAgentReturnControlPayload::SCHEMA,
    )
    .map_err(|error| {
        internal(format!(
            "the {RETURN_CONTROL_EVENT} event could not be framed: {error}"
        ))
    })
}

/// The `trace` events of the turn: the orchestration step it really performed.
///
/// The first reports the PROMPT the turn was assembled from
/// (`orchestrationTrace.modelInvocationInput`), which is what lets a caller see its own
/// returned result re-enter the next turn; one more reports each action-group invocation the
/// turn returned control for (`orchestrationTrace.invocationInput`, `executionType`
/// `RETURN_CONTROL`). No step past them is invented: what would follow is a model's output,
/// and no model runs here.
fn trace_frames(
    protocol: &dyn ServerHttpProtocol,
    served: &inline::ServedInvocation,
) -> Result<Vec<u8>, InvokeInlineAgentError> {
    let mut step = 1;
    let mut frames = trace_frame(
        protocol,
        served,
        OrchestrationTrace::modelInvocationInput(ModelInvocationInput {
            trace_id: Some(inline::trace_id(served.turn, step)),
            text: Some(inline::orchestration_prompt(served)),
            r#type: Some(inline::ORCHESTRATION_PROMPT_TYPE.to_string()),
            prompt_creation_mode: Some(inline::DEFAULT_PROMPT_CREATION_MODE.to_string()),
            foundation_model: Some(served.model_id.clone()),
            ..ModelInvocationInput::default()
        }),
    )?;
    if let Some(invocation_id) = served.returned_control.as_deref() {
        for function in &served.offered {
            step += 1;
            let invocation = ActionGroupInvocationInput {
                action_group_name: Some(function.action_group.clone()),
                function: Some(function.function.clone()),
                parameters: Some(
                    function
                        .parameters
                        .iter()
                        .map(|parameter| Parameter {
                            name: Some(parameter.name.clone()),
                            r#type: Some(parameter.parameter_type.clone()),
                            value: None,
                        })
                        .collect(),
                ),
                execution_type: Some(inline::RETURN_CONTROL.to_string()),
                invocation_id: Some(invocation_id.to_string()),
                ..ActionGroupInvocationInput::default()
            };
            frames.extend_from_slice(&trace_frame(
                protocol,
                served,
                OrchestrationTrace::invocationInput(InvocationInput {
                    trace_id: Some(inline::trace_id(served.turn, step)),
                    invocation_type: Some(inline::ACTION_GROUP_INVOCATION_TYPE.to_string()),
                    action_group_invocation_input: Some(invocation),
                    ..InvocationInput::default()
                }),
            )?);
        }
    }
    Ok(frames)
}

/// One `trace` event carrying `trace` as the orchestration part of the turn it was performed
/// in.
fn trace_frame(
    protocol: &dyn ServerHttpProtocol,
    served: &inline::ServedInvocation,
    trace: OrchestrationTrace,
) -> Result<Vec<u8>, InvokeInlineAgentError> {
    frame_event(
        protocol,
        TRACE_EVENT,
        false,
        &InlineAgentTracePart {
            session_id: Some(served.session_id.clone()),
            trace: Some(Trace::orchestrationTrace(trace)),
            event_time: Some(served.at),
        },
        &InlineAgentTracePart::SCHEMA,
    )
    .map_err(|error| {
        internal(format!(
            "the {TRACE_EVENT} event could not be framed: {error}"
        ))
    })
}

/// Whether the request asked for the final response to be STREAMED, which is the one thing
/// `streamingConfigurations` decides here: "Specifies whether to enable streaming for the
/// final response. This is set to `false` by default"
/// (`smithy_query(query="shape",shape="StreamingConfigurations")`).
fn streams_final_response(input: &InvokeInlineAgentInput) -> bool {
    input
        .streaming_configurations
        .as_ref()
        .is_some_and(|streaming| streaming.stream_final_response)
}

/// The first member of an `inlineSessionState` this service cannot compute, as the sentence
/// the modelled `ValidationException` states.
///
/// `invocationId` and `returnControlInvocationResults` are the served pair: they answer an
/// action-group invocation this service returned control for. Each of the other four asks
/// for work nothing here performs, so it is REFUSED rather than dropped — the handler sees
/// every member of the shape, which is why it can refuse the one the request carried instead
/// of the whole member.
fn unservable_session_state_member(state: &InlineSessionState) -> Option<&'static str> {
    if state.session_attributes.is_some() || state.prompt_session_attributes.is_some() {
        return Some(
            "inlineSessionState.sessionAttributes and \
             inlineSessionState.promptSessionAttributes: each carries attributes that \
             persist across the session, which an action group's function reads and which \
             replace the placeholders of a prompt nothing reads here, so neither can change \
             what this invocation answers; omit both",
        );
    }
    if state.files.is_some() {
        return Some(
            "inlineSessionState.files: they are the source files a code interpreter reads, \
             and this emulator runs no code interpreter, so nothing would read them; omit \
             files",
        );
    }
    if state.conversation_history.is_some() {
        return Some(
            "inlineSessionState.conversationHistory: its messages replace the conversation \
             this service records from the turns it really served, so a supplied history \
             cannot change what this invocation answers; reuse the same sessionId to \
             continue a conversation, and omit conversationHistory",
        );
    }
    None
}

/// The first leg of the request this service cannot compute, as the sentence the modelled
/// `ValidationException` states. See the module docs for why each one is refused.
fn unservable_leg(input: &InvokeInlineAgentInput) -> Result<(), InvokeInlineAgentError> {
    if input.guardrail_configuration.is_some() {
        return Err(validation(
            "guardrailConfiguration: it names the guardrail the invocation is governed by, \
             and this service evaluates no guardrail policy, so neither an intervention nor \
             its absence can be reported; omit guardrailConfiguration to take the answer \
             ungoverned",
        ));
    }
    if input
        .streaming_configurations
        .as_ref()
        .is_some_and(|streaming| streaming.apply_guardrail_interval.is_some())
    {
        return Err(validation(
            "streamingConfigurations.applyGuardrailInterval: it is how often a guardrail is \
             applied while the response is generated, and this service evaluates no \
             guardrail policy, so neither an intervention nor its absence can be reported; \
             omit it to take the response ungoverned",
        ));
    }
    if input.prompt_override_configuration.is_some() {
        return Err(validation(
            "promptOverrideConfiguration: it replaces the prompt templates of the agent \
             sequence, and no prompt is read in this emulator, so an override cannot change \
             what this invocation answers; omit it",
        ));
    }
    if input.prompt_creation_configurations.is_some() {
        return Err(validation(
            "promptCreationConfigurations: it selects which of the session's earlier turns \
             populate the agent prompt, and no prompt is read in this emulator, so the \
             selection cannot change what this invocation answers; omit it",
        ));
    }
    if let Some(state) = input.inline_session_state.as_ref() {
        if let Some(refusal) = unservable_session_state_member(state) {
            return Err(validation(refusal));
        }
    }
    if input.collaborators.is_some() || input.collaborator_configurations.is_some() {
        return Err(validation(
            "collaborators and collaboratorConfigurations: each names another agent this \
             invocation would hand work to, and nothing here orchestrates a collaboration, \
             so no collaborator is invoked and its answer cannot be part of this one; omit \
             both to invoke the inline agent alone",
        ));
    }
    if input
        .agent_collaboration
        .as_deref()
        .is_some_and(|collaboration| collaboration != COLLABORATION_DISABLED)
    {
        return Err(validation(format!(
            "agentCollaboration: it is how a supervisor agent coordinates the answers of its \
             collaborators, and nothing here orchestrates a collaboration; \
             {COLLABORATION_DISABLED} is the only value this service serves"
        )));
    }
    if input.custom_orchestration.is_some()
        || input
            .orchestration_type
            .as_deref()
            .is_some_and(|orchestration| orchestration == CUSTOM_ORCHESTRATION)
    {
        return Err(validation(format!(
            "customOrchestration and orchestrationType: {CUSTOM_ORCHESTRATION} drives the \
             agent from the function customOrchestration names, and this emulator invokes \
             no such function, so the orchestration it defines never runs; omit both to \
             invoke the inline agent with the default orchestration"
        )));
    }
    Ok(())
}

/// The literal first segment of this operation's modelled URI, `POST /agents/{sessionId}`.
const AGENTS_SEGMENT: &str = "agents";

/// Whether `req` addresses this operation's modelled route: `POST` on the one literal
/// segment plus the single `sessionId` label.
///
/// The seam that routes it ([`crate::service::EmulatedService::override_operation`]) asks
/// this, so the path this operation recognises is stated once, beside the operation. WHICH
/// Bedrock service claims such a request at all is `service_common::bedrock`'s: the
/// build-time plane binds `POST /agents/{agentId}/` on the same two segments, and the claim
/// rule splits the two.
pub fn addresses(req: &Request) -> bool {
    if req.method() != "POST" {
        return false;
    }
    let path = service_common::uri_path(req.uri());
    let mut segments = path.trim_start_matches('/').split('/');
    let (Some(AGENTS_SEGMENT), Some(label), None) =
        (segments.next(), segments.next(), segments.next())
    else {
        return false;
    };
    !label.is_empty()
}

/// Serving this operation is hand-authored because the generated `ErasedOperation` shim of
/// a `@streaming`-output operation declines the request (it cannot drive
/// `serialize_as_event_stream<P>` through an object-safe `run`).
///
/// The request half is the model's own: the framework's deserializer and constraint walk
/// over THIS operation's input, refused in this service's own wording
/// (`Service::override_refusal`), so a malformed URI label or body member is refused exactly
/// as one from a non-streaming operation of the same service.
impl<Eng: ::std::marker::Send + ::std::marker::Sync + 'static> EventStreamOperation
    for InvokeInlineAgent<Eng>
{
    fn serve<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r Request,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, Response> {
        Box::pin(async move {
            let definition = svc.definition();
            let protocol = definition.protocol();
            let input = match read_request::<InvokeInlineAgentInput>(
                protocol,
                req,
                &InvokeInlineAgentInput::SCHEMA,
            ) {
                Ok(input) => input,
                Err(RequestRefusal::Invalid(violations)) => {
                    // Refused through this service's ONE refusal seam for a hand-authored
                    // operation, so the clause about a COLLECTION member quotes the
                    // collection the request carried — which the framework's walk has no
                    // rendering for (see `crate::service::submitted_collections`).
                    return crate::service::submitted_collections::quoted_refusal_response::<
                        InvokeInlineAgentInput,
                    >(svc, req, &InvokeInlineAgentInput::SCHEMA, violations);
                }
                // A body the protocol's codec could not parse has no violation form, so
                // the reader's own sentence is what the modelled 400 carries.
                Err(RequestRefusal::Unreadable(message)) => {
                    return error_response(protocol, &validation(message))
                }
            };
            match self.respond(protocol, &input, ctx) {
                Ok(response) => response,
                Err(error) => error_response(protocol, &error),
            }
        })
    }
}
