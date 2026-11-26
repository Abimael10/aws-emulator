// SPDX-License-Identifier: CC0-1.0

//! The per-listener port-range count LIMIT: how many ranges one listener's `PortRanges`
//! may carry, and the error class this service refuses a longer list with.
//!
//! # The rule
//!
//! A standard listener's `PortRanges` is modelled `@length(min: 1, max: 10)`, and both
//! operations that configure one — `CreateListener` and `UpdateListener`
//! (`smithy_query(query="operation",operation="CreateListener,UpdateListener",model_path="services/service-com-amazonaws-globalaccelerator-globalaccelerator-v20180706/smithy-model/com-amazonaws-globalaccelerator-globalaccelerator-v20180706.json")`)
//! — declare `LimitExceededException` (`errorKind` client, `@httpError` 403, "Processing
//! your request would cause you to exceed an Global Accelerator limit") beside
//! `InvalidArgumentException` (400). The service enforces that MAXIMUM as one of its
//! limits rather than as a malformed argument: a request naming more ranges than
//! [`crate::state::MAX_PORT_RANGES_PER_LISTENER`] is answered `LimitExceededException` at
//! the status the model gives that error, carrying the sentence this service states the
//! limit in — scoped to the accelerator the listener would belong to on the create leg and
//! to the listener itself on the update leg
//! ([`crate::state::port_range_limit_for_accelerator_message`],
//! [`crate::state::port_range_limit_for_listener_message`]) — and nothing is written, so
//! the listeners the store holds, and the ranges each of them holds, are exactly what they
//! were.
//!
//! What is deliberately NOT a limit here:
//!
//! * the list's `@length` MINIMUM. A `PortRanges` that names no range at all is a
//!   malformed argument, and it keeps the modelled constraint refusal every other bound of
//!   this service answers (HTTP 400 `InvalidArgumentException` over the constraint report).
//! * a request that identifies no resource to state the limit against — the `@required`
//!   member the sentence names left out, or sent empty ([`scoping_arn`]). There is no limit
//!   to tell such a caller about, so it keeps the refusal of the member it omitted.
//! * every other member of the same request. This layer reads the ONE count the limit is
//!   about and leaves everything else to the model's own walk.
//! * the CUSTOM ROUTING family's `PortRanges`, which carries the same bound on
//!   `CreateCustomRoutingListener` / `UpdateCustomRoutingListener`. Nothing records this
//!   service answering that family's count with the limit error, and this crate's suite
//!   pins it as the constraint refusal (`tests/all/custom_routing_listeners.rs`), so those
//!   two operations keep it until a recording says otherwise.
//!
//! # Where in a request's answer this limit falls
//!
//! The count is judged where the model's own walk judges it: on the request ALONE, before
//! anything resolves the resource it names. So a request that is over the limit AND names a
//! resource the store does not hold is answered the limit rather than the not-found a
//! handler would raise — the count is a fact about the request, and the member bound it
//! replaces was answered at that same stage for every other list of this service. Nothing
//! records the service's precedence between the two, so the ordering is this emulator's and
//! is kept at the one stage rather than split across two.
//!
//! # Why this is in front of the operation rather than inside it
//!
//! The walk that refuses a value outside a modelled bound is model-driven and runs inside
//! the framework's own deserialize -> validate -> handle -> serialize sequence, BEFORE any
//! handler is reached, so no handler can answer a list that walk rejects.
//! [`smithy_server_runtime::Service::override_refusal`] does see such a refusal, but it
//! chooses the error CODE and the WORDS only — the status stays the one the protocol gives
//! a constraint failure — and it is handed the violations alone, with no request, so it can
//! name neither the 403 the model gives this error nor the ARN the sentence states the
//! limit against. A layer in FRONT of that sequence can do both, and this service already
//! serves every request through one ([`super::error_message_member`]), so the limit is
//! answered there.

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use smithy_server_runtime::{
    DeserializableStruct, ErasedServiceDefinition, OperationError, ServerHttpProtocol,
};

use crate::ops::types::{CreateListenerInput, LimitExceededException, UpdateListenerInput};

/// This service's answer to `req` when it names more port ranges than one listener may
/// hold, or `None` when it does not — which is every request this layer leaves to the
/// framework's own sequence.
///
/// The operation is named by the key this service's own protocol resolves, and the count is
/// read off the request through that protocol and the operation's own input schema, so
/// neither the wire form of the request nor the member's own name is restated here. A
/// request no codec can read names no count to judge and is left to that sequence, which
/// answers it in this service's envelope.
pub fn refusal(definition: &dyn ErasedServiceDefinition, req: &RtRequest) -> Option<RtResponse> {
    let protocol = definition.protocol();
    match protocol.routing_key(req).ok()? {
        // The create leg states the limit against the accelerator the listener would
        // belong to: the listener it is about does not exist yet.
        "CreateListener" => {
            let input: CreateListenerInput =
                read(protocol, req, crate::ops::types::CREATE_LISTENER_INPUT_SCHEMA)?;
            let scope = scoping_arn(&input.accelerator_arn)?;
            let message = over_the_limit(input.port_ranges.len())
                .then(|| crate::state::port_range_limit_for_accelerator_message(scope))?;
            let error: crate::ops::create_listener::CreateListenerError =
                crate::ops::create_listener::CreateListenerErrorModeled::LimitExceededException(
                    limit_exceeded(message),
                )
                .into();
            Some(rendered(protocol, &error))
        }
        // The update leg states it against the listener the request identified. A request
        // carrying no `PortRanges` is not a change to them, so it names no count at all.
        "UpdateListener" => {
            let input: UpdateListenerInput =
                read(protocol, req, crate::ops::types::UPDATE_LISTENER_INPUT_SCHEMA)?;
            let scope = scoping_arn(&input.listener_arn)?;
            let message = over_the_limit(input.port_ranges.unwrap_or_default().len())
                .then(|| crate::state::port_range_limit_for_listener_message(scope))?;
            let error: crate::ops::update_listener::UpdateListenerError =
                crate::ops::update_listener::UpdateListenerErrorModeled::LimitExceededException(
                    limit_exceeded(message),
                )
                .into();
            Some(rendered(protocol, &error))
        }
        // No other operation configures a standard listener's port ranges.
        _ => None,
    }
}

/// The ARN the limit is stated against, or `None` when the request carried none.
///
/// The sentence names the resource the limit belongs to, so a request that identifies no
/// such resource — the member the model requires left out, or sent empty — has no limit to
/// be told about: it is left to the framework's own walk, which refuses it for the member it
/// omitted rather than for a count.
fn scoping_arn(arn: &str) -> Option<&str> {
    (!arn.is_empty()).then_some(arn)
}

/// Whether `ranges` is more port ranges than one listener may hold.
fn over_the_limit(ranges: usize) -> bool {
    ranges > crate::state::MAX_PORT_RANGES_PER_LISTENER
}

/// `req` read into one operation's input shape through this service's own protocol, or
/// `None` when it does not READ as that input at all.
///
/// Deliberately NOT validated: the count this layer judges is one the model's own walk
/// would refuse the request for, so a request that satisfies that walk is not one there is
/// anything to answer here.
fn read<T: DeserializableStruct>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
    schema: &'static ::aws_smithy_schema::Schema,
) -> Option<T> {
    let mut deserializer = protocol.deserialize_input(req, schema).ok()?;
    let read = T::deserialize(&mut *deserializer);
    drop(deserializer);
    read.ok()
}

/// The modelled `LimitExceededException` carrying `message` — the one shape both legs
/// raise, so the sentence and the shape are paired in one place.
fn limit_exceeded(message: String) -> LimitExceededException {
    LimitExceededException {
        message: Some(message),
    }
}

/// `error` rendered in this service's own envelope at the status the model gives it.
///
/// Rendered through the shared renderer every service answers a modelled error from, so a
/// limit answered here and the same error raised by a handler are one response: the error
/// identity, the body member and the status are the model's.
fn rendered(protocol: &dyn ServerHttpProtocol, error: &dyn OperationError) -> RtResponse {
    service_common::event_stream::error_response(protocol, error)
}
