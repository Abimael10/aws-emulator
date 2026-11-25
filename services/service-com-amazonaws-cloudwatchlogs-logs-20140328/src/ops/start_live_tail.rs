// SPDX-License-Identifier: CC0-1.0

//! @op StartLiveTail
//!
//! `POST / -> 200` (`X-Amz-Target: Logs_20140328.StartLiveTail`). Starts a Live Tail
//! session over one to ten log groups and answers with the modelled
//! `StartLiveTailResponseStream` as a `vnd.amazon.eventstream` body.
//!
//! This operation is hand-authored rather than generated: its output member
//! `responseStream` targets a `@streaming` union, which the op-stub generator declines
//! (`src/ops/SKIPPED_STUBS.md`) and whose generated `ErasedOperation` shim refuses the
//! request. The framing and the routing seam are `service_common::event_stream` and
//! `src/service.rs`; this file holds the behaviour, and the shapes are in
//! `src/ops/types/start_live_tail_types.rs`.
//!
//! # What a session answers
//!
//! A Live Tail session is a SUBSCRIPTION to what the tailed log groups ingest while it is
//! open — "a stream of log events that have been recently ingested in the log groups" — so
//! the session registers itself in the store ([`crate::state::live_tail`]) and every
//! `PutLogEvents` into one of those groups offers its events to it. An event a group
//! already held when the session opened was ingested before there was a session to publish
//! it to, so it is not delivered.
//!
//! The model documents the stream's shape, and each part of it is served from that
//! subscription:
//!
//! * "A single LiveTailSessionStart object is sent at the start of the session" — one
//!   `sessionStart` event naming the session and echoing the selection it was started
//!   with.
//! * "Every second, a LiveTailSessionUpdate object is sent. Each of these objects
//!   contains an array of the actual log events [...] If no new log events were ingested
//!   in the past second, the LiveTailSessionUpdate object will contain an empty array."
//!   One `sessionUpdate` per second of the session therefore, carrying the events ingested
//!   in that second that the session's selection admits — published by the ingestion
//!   itself, never synthesised — and an empty array for a second that ingested none.
//! * "The array of log events contained in a LiveTailSessionUpdate can include as many
//!   as 500 log events. If the number of log events matching the request exceeds 500 per
//!   second, the log events are sampled down to 500 log events to be included in each
//!   LiveTailSessionUpdate object" — [`crate::state::live_tail::MAX_EVENTS_PER_UPDATE`],
//!   with `sessionMetadata.sampled` set exactly when the match exceeded it, as that member
//!   documents ("more than 500 log events matched the request for this update, and the
//!   sessionResults includes a sample of 500 of those events").
//!
//! # How long one session runs
//!
//! A session is answered as ONE response rather than as a connection whose frames the
//! client takes one at a time, so the client consumes nothing until the session is over —
//! which is the case the operation states a bound for: "If your client consumes the log
//! events slower than the server produces them, CloudWatch Logs buffers up to 10
//! LiveTailSessionUpdate events or 5000 log events, after which it starts dropping the
//! oldest events." The response therefore carries exactly that buffer — up to
//! [`crate::state::live_tail::MAX_BUFFERED_UPDATES`] updates and
//! [`crate::state::live_tail::MAX_BUFFERED_EVENTS`] log events — and the session then ends
//! by the stream closing, which is one of the two documented ends of a session ("You can
//! end a session before it times out by closing the session stream").
//!
//! The two remaining members of the union, `SessionTimeoutException` and
//! `SessionStreamingException`, are the OTHER two ends: the first is sent "when the session
//! times out, after it has been kept open for three hours" and the second "if an unknown
//! error occurs on the server side", so a session that ends by its stream closing carries
//! neither.
//!
//! The store a session touches is its own registration: it buffers what the ingestion
//! publishes to it and takes that registration away when it ends, so a tail leaves the
//! groups it tailed exactly as a later reader finds them, and the session itself is
//! observable through no operation of this service.
//!
//! ## Input
//!
//! * `logGroupIdentifiers` (StartLiveTailLogGroupIdentifiers, len 1..10) **required** —
//!   each item a log group to include, by name or ARN. The modelled `LogGroupIdentifier`
//!   pattern (`^[\w#+=/:,.@-]*$`) is what refuses the asterisk-suffixed ARN form the
//!   member's documentation rules out ("If you specify an ARN, the ARN can't end with an
//!   asterisk (*)").
//! * `logStreamNames` (InputLogStreamNames, len 1..100) — only events of these streams
//!   are included. "If you specify this field, you can't also specify the
//!   logStreamNamePrefixes field", and "You can specify this parameter only if you
//!   specify only one log group in logGroupIdentifiers".
//! * `logStreamNamePrefixes` (InputLogStreamNames, len 1..100) — only events of the
//!   streams whose names start with one of these prefixes, under the same two rules.
//! * `logEventFilterPattern` (FilterPattern, len 0..1024) — "An optional pattern to use
//!   to filter the results to include only log events that match the pattern", in the
//!   service's own filter-and-pattern syntax
//!   ([`crate::state::filter_pattern::FilterPattern`]).
//!
//! ## Output
//!
//! * `responseStream` (StartLiveTailResponseStream, `@streaming`) — the body: the frame a
//!   client reads the output shape itself out of
//!   (`service_common::event_stream::INITIAL_RESPONSE_EVENT`, carrying no member, because
//!   the stream is this output's only one), then the `sessionStart` event, then one
//!   `sessionUpdate` per second of the session.
//!
//! ## Errors
//!
//! * `ResourceNotFoundException` (HTTP 400) — a `logGroupIdentifiers` item names a log
//!   group this account does not hold.
//! * `InvalidParameterException` (HTTP 400) — a parameter is specified incorrectly: the
//!   two stream selectors together, a stream selector across several log groups, a
//!   filter pattern that does not parse, or a value outside a modelled constraint.
//! * `AccessDeniedException`, `InvalidOperationException`, `LimitExceededException`
//!   (HTTP 400) — the operation's remaining modelled errors; no condition of this store
//!   raises them.

#[allow(unused_imports)]
use super::types::*;
use aws_smithy_runtime_api::http::{Request, Response};
use aws_smithy_schema_server::constraints::{validate, Violation, ViolationKind};
use aws_smithy_schema_server::DeserializableStruct;
// The bounds on a session's updates and its buffer are stated where the sessions live.
use crate::state::live_tail::{MAX_BUFFERED_EVENTS, MAX_BUFFERED_UPDATES, UPDATE_INTERVAL};
use service_common::event_stream::{
    error_response, event_stream_only_response, frame_empty_initial_response, frame_event,
};
use smithy_server_runtime::{BoxFuture, OperationContext, ServerHttpProtocol, Service};
use std::sync::Arc;

smithy_macros::smithy_model!("smithy-model/com-amazonaws-cloudwatchlogs-logs-20140328.json");

/// The operation's MODELED errors; `#[operation_error]` accepts only variants the model
/// declares for it, and every one of them is a client error at HTTP 400.
#[smithy_macros::operation_error("com.amazonaws.cloudwatchlogs#StartLiveTail")]
#[derive(Debug)]
pub enum StartLiveTailErrorModeled {
    AccessDeniedException(AccessDeniedException),
    InvalidOperationException(InvalidOperationException),
    InvalidParameterException(InvalidParameterException),
    LimitExceededException(LimitExceededException),
    ResourceNotFoundException(ResourceNotFoundException),
}

/// The `StartLiveTailResponseStream` member each event is framed as, which the frame
/// carries in its `:event-type` header (source: `smithy_query(query="shape",
/// shape="StartLiveTailResponseStream")`).
const SESSION_START_EVENT: &str = "sessionStart";
/// See [`SESSION_START_EVENT`].
const SESSION_UPDATE_EVENT: &str = "sessionUpdate";

/// The refusal for a request that names both log-stream selectors: "If you specify this
/// field, you can't also specify the logStreamNamePrefixes field"
/// (`StartLiveTailRequest$logStreamNames`). The service states it as the PREFIX selector
/// being disabled by the names the request also gave
/// ([`crate::state::parameter_pairs::disabled_by`]) — two sentences, naming the two members
/// in the capitalised spellings those live-tail rules name them in and, in the second
/// sentence, the names it was given in the rule's own prose.
static STREAM_SELECTORS_EXCLUSIVE: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    crate::state::parameter_pairs::disabled_by(
        LOG_STREAM_NAMES_PARAMETER,
        LOG_STREAM_NAME_PREFIXES_PARAMETER,
        LOG_STREAM_NAMES_SUBJECT,
    )
});

/// The refusal for a request that selects streams while tailing more than one log group:
/// "You can specify this parameter only if you specify only one log group in
/// logGroupIdentifiers" (`StartLiveTailRequest$logStreamNames` and
/// `$logStreamNamePrefixes`). The service states this one as the selector being DISABLED
/// past one log group ([`crate::state::parameter_pairs::disabled_past_one`]), naming the
/// selector the request spelled and the identifier member it spelled more than once — both
/// in the capitalised spellings that sentence names members in.
fn stream_selector_needs_one_log_group(selector: &str) -> StartLiveTailErrorModeled {
    invalid(crate::state::parameter_pairs::disabled_past_one(
        selector,
        LOG_GROUP_IDENTIFIER_PARAMETER,
    ))
}

/// `logStreamNames` as the refusal above names it.
const LOG_STREAM_NAMES_PARAMETER: &str = "LogStreamNames";
/// `logStreamNamePrefixes` as the refusal above names it — the service states this rule
/// about the stream-prefix selector under the name `LogStreamPrefixes` (recorded), which is
/// the member's name in the words of the rule rather than its wire spelling.
const LOG_STREAM_NAME_PREFIXES_PARAMETER: &str = "LogStreamPrefixes";
/// `logStreamNames` as the SECOND sentence of the exclusive-pair refusal names it: that
/// sentence states the condition in prose ("is disabled if log stream names is provided"),
/// so the member is named in the rule's own words there rather than in either spelling
/// above.
const LOG_STREAM_NAMES_SUBJECT: &str = "log stream names";
/// One item of `logGroupIdentifiers` as the refusal above names it: the rule is about how
/// MANY of them the request named, so the sentence names the member in the singular.
const LOG_GROUP_IDENTIFIER_PARAMETER: &str = "LogGroupIdentifier";

/// The refusal CloudWatch Logs answers a Live Tail session that names a log group it does
/// not hold. Fixed text for the class: it states the resource the session could not be
/// started over and carries nothing of the request.
const LOG_GROUP_MISSING_MESSAGE: &str = "Provided LogGroup doesn't exist.";

/// The `StartLiveTail` operation.
///
/// Owns the shared engine (the constructor seam). The generated routing table holds one
/// of these and `service.rs` holds another wrapped in an `EventStreamRoute`; both reach
/// the same state, because the state lives in the shared engine, not on the operation.
pub struct StartLiveTail<Eng> {
    engine: Arc<Eng>,
}

impl<Eng> StartLiveTail<Eng> {
    /// Construct the operation from the shared engine. INFALLIBLE — the engine is a
    /// construction-time dependency, so engine access is guaranteed by the time any
    /// request runs.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }
}

impl<Eng: Send + Sync + 'static> StartLiveTail<Eng> {
    /// The whole request: open the session, then frame its `sessionStart` and the
    /// `sessionUpdate` of each second the session runs for.
    ///
    /// The session is registered by [`StartLiveTail::open`] before the first second is
    /// waited out, so the events a `PutLogEvents` stores from then on are published to it;
    /// each second's update is what that publication left in the session's buffer.
    async fn respond(
        &self,
        protocol: &dyn ServerHttpProtocol,
        req: &Request,
    ) -> Result<Response, StartLiveTailErrorModeled> {
        let (session, session_start) = self.open(protocol, req)?;

        // The body opens with the frame a client reads the OUTPUT shape out of, before any
        // frame of the session: `StartLiveTailResponse`'s only member is the stream itself,
        // so that frame carries no member.
        let mut frames = frame_empty_initial_response(protocol).map_err(|error| {
            internal(format!("the initial response could not be framed: {error}"))
        })?;
        frames.extend(
            frame_event(
                protocol,
                SESSION_START_EVENT,
                false,
                &session_start,
                &LiveTailSessionStart::SCHEMA,
            )
            .map_err(|error| {
                internal(format!("the sessionStart event could not be framed: {error}"))
            })?,
        );

        // "Every second, a LiveTailSessionUpdate object is sent", for as many of them as
        // the session's own buffer holds for a client that has consumed none of them yet
        // (see "How long one session runs"). No state is held while the second passes: the
        // update is taken from the session's buffer, which the ingestion fills.
        let mut delivered = 0usize;
        for _ in 0..MAX_BUFFERED_UPDATES {
            ::tokio::time::sleep(UPDATE_INTERVAL).await;
            let update = session.take_update();
            delivered = delivered.saturating_add(update.events.len());
            frames.extend(
                frame_event(
                    protocol,
                    SESSION_UPDATE_EVENT,
                    false,
                    &session_update(&update),
                    &LiveTailSessionUpdate::SCHEMA,
                )
                .map_err(|error| {
                    internal(format!("the sessionUpdate event could not be framed: {error}"))
                })?,
            );
            if delivered >= MAX_BUFFERED_EVENTS {
                break;
            }
        }
        Ok(event_stream_only_response(frames))
    }

    /// Read and admit the request, register the session it starts, and answer the
    /// `sessionStart` that names it.
    ///
    /// SYNCHRONOUS on purpose: every read and write of the log store the session's start
    /// needs happens here, so the store's guard is gone before the session waits for its
    /// first update.
    fn open(
        &self,
        protocol: &dyn ServerHttpProtocol,
        req: &Request,
    ) -> Result<
        (
            crate::state::live_tail::SessionHandle,
            LiveTailSessionStart,
        ),
        StartLiveTailErrorModeled,
    > {
        let input = self.read_input(protocol, req)?;

        // "If you specify this field, you can't also specify the logStreamNamePrefixes
        // field", and either selector needs a single log group.
        if input.log_stream_names.is_some() && input.log_stream_name_prefixes.is_some() {
            return Err(invalid(STREAM_SELECTORS_EXCLUSIVE.clone()));
        }
        if input.log_group_identifiers.len() > 1 {
            if input.log_stream_names.is_some() {
                return Err(stream_selector_needs_one_log_group(
                    LOG_STREAM_NAMES_PARAMETER,
                ));
            }
            if input.log_stream_name_prefixes.is_some() {
                return Err(stream_selector_needs_one_log_group(
                    LOG_STREAM_NAME_PREFIXES_PARAMETER,
                ));
            }
        }

        // The pattern is parsed before anything is looked up: a filter pattern that does
        // not parse is a bad request, not a session that delivers nothing.
        let pattern = crate::state::filter_pattern::FilterPattern::parse(
            input.log_event_filter_pattern.as_deref().unwrap_or(""),
        )
        .map_err(invalid)?;

        let state = crate::state::service_state(&*self.engine).ok_or_else(|| {
            StartLiveTailErrorModeled::InvalidOperationException(InvalidOperationException {
                message: Some("the log store is unavailable".to_string()),
            })
        })?;
        let mut store = state.lock();

        // A session is started over the log groups it names, so every identifier has to
        // resolve before any of them is tailed: a session that could never deliver one
        // group's events is refused rather than silently narrowed to the others. What the
        // session keeps is the group's NAME — which is what an ingestion names its group by
        // — mapped to the identifier the request spelled, because that is the spelling each
        // delivered event reports.
        let mut groups: std::collections::BTreeMap<String, String> =
            std::collections::BTreeMap::new();
        for identifier in &input.log_group_identifiers {
            let name = crate::state::log_group_name_from_identifier(identifier);
            let Some(group) = store.log_groups.get(name) else {
                return Err(StartLiveTailErrorModeled::ResourceNotFoundException(
                    ResourceNotFoundException {
                        message: Some(LOG_GROUP_MISSING_MESSAGE.to_string()),
                    },
                ));
            };
            // A log group's CLASS decides which features it supports, and Live Tail is
            // outside the subset the classes other than Standard support ("Log classes",
            // `CreateLogGroup$logGroupClass`): a session cannot tail such a group, so the
            // identifier naming one is the parameter that is wrong and the request is refused
            // with this service's one sentence for a feature the class does not support
            // rather than opening a session that could never deliver its events.
            if group.log_group_class != crate::state::LIVE_TAIL_LOG_GROUP_CLASS {
                return Err(invalid(
                    crate::state::LOG_CLASS_UNSUPPORTED_OPERATION_MESSAGE.to_string(),
                ));
            }
            groups
                .entry(name.to_string())
                .or_insert_with(|| identifier.clone());
        }

        let session_start = LiveTailSessionStart {
            // Both ids are minted per session: nothing in the request carries either,
            // and a session is named only in the response that starts it.
            request_id: Some(::service_common::query_error_envelope::new_request_id()),
            session_id: Some(::service_common::query_error_envelope::new_request_id()),
            log_group_identifiers: Some(input.log_group_identifiers.clone()),
            log_stream_names: input.log_stream_names.clone(),
            log_stream_name_prefixes: input.log_stream_name_prefixes.clone(),
            log_event_filter_pattern: input.log_event_filter_pattern.clone(),
        };
        // The session subscribes from HERE: it is registered under the guard the groups
        // were resolved through, so every event ingested after this point is offered to it
        // and every event ingested before it was never published to it at all.
        let session = crate::state::live_tail::SessionHandle::open(
            &state,
            &mut store,
            session_start.session_id.as_deref().unwrap_or_default(),
            crate::state::live_tail::SessionRecord::new(
                groups,
                input.log_stream_names.clone(),
                input.log_stream_name_prefixes.clone(),
                pattern,
            ),
        );
        drop(store);
        Ok((session, session_start))
    }

    /// Read the request into the modelled input shape and check it against the model.
    ///
    /// Driven through the service's own protocol, so the body codec is the protocol's;
    /// the refusal is worded by [`crate::service::constraint_envelope`] rather than by
    /// the framework's default envelope, so a bad request to this hand-authored
    /// operation reads exactly as the framework's boundary words the same violation for
    /// this service's generated operations.
    fn read_input(
        &self,
        protocol: &dyn ServerHttpProtocol,
        req: &Request,
    ) -> Result<StartLiveTailInput, StartLiveTailErrorModeled> {
        let mut deserializer = protocol
            .deserialize_input(req, &StartLiveTailInput::SCHEMA)
            .map_err(|error| invalid(format!("the request could not be read: {error}")))?;
        let input = StartLiveTailInput::deserialize(&mut *deserializer)
            .map_err(|error| invalid(serde_refusal(error)));
        drop(deserializer);
        let input = input?;
        let violations = validate(&input, &StartLiveTailInput::SCHEMA);
        if violations.is_empty() {
            return Ok(input);
        }
        // The collections the request carried, read out of the input this operation holds —
        // the one place a clause about a collection member can state what was submitted
        // there ([`crate::service::constraint_envelope_quoting`]).
        let submitted =
            service_common::validation::submitted_lists(&input, &StartLiveTailInput::SCHEMA);
        match refusal_message_quoting(&violations, &submitted) {
            Some(message) => Err(invalid(message)),
            None => Ok(input),
        }
    }
}

/// One `sessionUpdate` event: the log events the session buffered for the second that just
/// ended, each reporting the stream it was ingested into and the identifier its group was
/// named by, and the metadata saying whether a matching event was left out.
fn session_update(update: &crate::state::live_tail::Update) -> LiveTailSessionUpdate {
    LiveTailSessionUpdate {
        session_metadata: Some(LiveTailSessionMetadata {
            sampled: update.sampled,
        }),
        session_results: Some(
            update
                .events
                .iter()
                .map(|delivered| LiveTailSessionLogEvent {
                    log_stream_name: Some(delivered.log_stream_name.clone()),
                    log_group_identifier: Some(delivered.log_group_identifier.clone()),
                    message: Some(delivered.message.clone()),
                    timestamp: Some(delivered.timestamp),
                    ingestion_time: Some(delivered.ingestion_time),
                })
                .collect(),
        ),
    }
}

/// This service's wording for the violations a request failed, falling back to the
/// framework's envelope for a class the service words no clause for. `None` when the
/// request broke nothing.
fn refusal_message(violations: &[Violation]) -> Option<String> {
    refusal_message_quoting(violations, &crate::service::SubmittedCollections::new())
}

/// The same wording, with the COLLECTIONS the request submitted in hand, so a clause about
/// a collection member states the collection rather than its position alone
/// ([`crate::service::constraint_envelope_quoting`]).
fn refusal_message_quoting(
    violations: &[Violation],
    submitted: &crate::service::SubmittedCollections,
) -> Option<String> {
    if violations.is_empty() {
        return None;
    }
    crate::service::constraint_envelope_quoting(violations, submitted)
        .or_else(|| aws_smithy_schema_server::constraints::envelope(violations))
}

/// A member the request did not carry, worded as the service words an absent
/// `@required` member — the same clause the framework's own boundary renders for one.
fn serde_refusal(error: ::aws_smithy_schema::serde::SerdeError) -> String {
    match error {
        ::aws_smithy_schema::serde::SerdeError::MissingMember { member_name } => {
            let violation = Violation {
                path: aws_smithy_schema_server::constraints::path::member(&member_name),
                kind: ViolationKind::MissingRequiredMember,
                value: None,
                sensitive: false,
            };
            refusal_message(&[violation]).unwrap_or_else(|| member_name.clone())
        }
        other => other.to_string(),
    }
}

/// "A parameter is specified incorrectly" — this operation's refusal for a request it
/// will not accept.
fn invalid(message: String) -> StartLiveTailErrorModeled {
    StartLiveTailErrorModeled::InvalidParameterException(InvalidParameterException {
        message: Some(message),
    })
}

/// A failure of this emulator rather than of the request. `InvalidOperationException` is
/// the only modelled error of this operation that is not about a parameter the caller
/// chose, and every modelled error here is a client error at 400: the operation declares
/// no server fault to answer with.
fn internal(message: String) -> StartLiveTailErrorModeled {
    StartLiveTailErrorModeled::InvalidOperationException(InvalidOperationException {
        message: Some(message),
    })
}

impl<Eng: Send + Sync + 'static> service_common::event_stream::EventStreamOperation
    for StartLiveTail<Eng>
{
    fn serve<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r Request,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, Response> {
        Box::pin(async move {
            let protocol = svc.definition().protocol();
            let response = match self.respond(protocol, req).await {
                Ok(response) => response,
                Err(error) => error_response(protocol, &error),
            };
            // One access record per request, so a refused session is diagnosable. No
            // request payload and no credential reaches it.
            tracing::debug!(
                operation = "StartLiveTail",
                service = ctx.identity().signing_name(),
                method = ctx.parts().method.as_str(),
                status = response.status().as_u16(),
                "served an event-stream request"
            );
            response
        })
    }
}
