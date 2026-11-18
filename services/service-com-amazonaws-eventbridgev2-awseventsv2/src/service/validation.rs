// SPDX-License-Identifier: CC0-1.0

//! How this service states a request it refuses before a handler runs.
//!
//! One rule lives here, and it is INPUT validation: it is decided from the request
//! alone, ahead of any state this service holds.
//!
//! # The modelled shape a refusal is stated in
//!
//! The framework refuses a request that breaks the model's own rules — a member the
//! input shape requires that the request left out, or a value outside a declared
//! `@length`/`@pattern` — before the handler is called, and renders it in a SYNTHETIC
//! error shape: the wire error code `ValidationException`, which this service's model
//! does not declare at all, with its text in a member named `message`. This service's
//! own answer for such a request is `InvalidInputException` ("A request parameter is
//! missing or not valid.", `@error client`, `@httpError 400`), which the model lists as
//! a common error of every operation and whose single member is `Message`. A client
//! matching the discriminator against the operation's modelled errors therefore reads
//! neither the error nor its text.
//!
//! [`ModelledInputRefusal`] closes that gap: `Service::override_operation` routes every
//! modelled operation through it, it asks the workspace's one shared pre-handler walk
//! (`service_common::validation::refusing_violations`, which drives the protocol's own
//! deserializer and `aws_smithy_schema_server::constraints::validate`) what the request
//! breaks, and serializes those violations as the modelled `InvalidInputException`. The
//! rules enforced are exactly the model's; the words are AWS's own constraint envelope
//! (`service_common::validation_message::aws_validation_refusal`). A request the model
//! accepts is delegated, untouched, to the generated operation in the routing table, so
//! every handler, its state and its 501 are exactly as before.
//!
//! The model's rules are almost the whole of it: a member the model leaves optional is
//! optional here too. The one rule this service adds is its own, and is about the request
//! alone, which is why it lives here rather than in the handlers it would otherwise be
//! repeated in — a CREATE of this service requires the `@idempotencyToken` `ClientToken`
//! the model leaves optional, and is refused with [`CLIENT_TOKEN_REQUIRED`] when the
//! request states none.

use aws_smithy_runtime_api::http::{Request as WireRequest, Response as RtResponse};
use aws_smithy_schema_server::constraints::Violation;
use aws_smithy_schema_server::DeserializableStruct;
use service_common::validation::refusing_violations;
use service_common::validation_message::aws_validation_refusal;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, Refusal, ServerHttpProtocol, Service,
};
use std::collections::HashMap;
use std::marker::PhantomData;

use crate::ops::types::{InvalidInputException, SCHEMA_INVALID_INPUT_EXCEPTION};

/// The wire error code (`__type`) of this service's input-validation refusal: the
/// shape its model lists as a common error of every operation for "A request
/// parameter is missing or not valid.", and the same name a handler-returned
/// `InvalidInputException` carries.
pub const INVALID_INPUT_EXCEPTION: &str = "InvalidInputException";

/// The status such a refusal is served on: `InvalidInputException`'s own
/// `@httpError(400)`.
pub const INVALID_INPUT_STATUS: u16 = 400;

/// The whole `Message` a create of this service is refused with when the request states no
/// idempotency token: the service's own requirement, named once here and read through
/// [`crate::state::CLIENT_TOKEN_REQUIRED_MESSAGE`], which is where this service's fixed
/// refusal texts live.
pub const CLIENT_TOKEN_REQUIRED: &str = crate::state::CLIENT_TOKEN_REQUIRED_MESSAGE;

/// This service's `InvalidInputException` carrying `message`. The shape models
/// `Message` alone.
fn invalid_input(message: String) -> InvalidInputException {
    InvalidInputException {
        message: Some(message),
    }
}

/// How AWS words a refusal the model's own constraints produced: the constraint
/// envelope (`N validation error(s) detected: <clause>`) over the violations, with the
/// `@pattern` requirement quoted as AWS quotes it. `None` only when there is no
/// violation to state.
fn refusal_message(violations: Vec<Violation>) -> Option<String> {
    aws_validation_refusal(&Refusal::Invalid(violations)).map(|refused| refused.message)
}

/// How one operation's input states the idempotency token a create of this service requires,
/// read off the input the protocol's own deserializer built from the request.
///
/// A function per operation rather than one over every input shape, because the member lives
/// on each create's own input type: the wrapper is generic over the operation, so the reader
/// is the one thing about that type this check cannot name itself.
type ClientTokenReader<Op> = fn(&<Op as Operation>::Input) -> Option<&str>;

/// One modelled operation, wrapped so a request refused before its handler runs is
/// stated in this service's own `InvalidInputException` rather than in the framework's
/// synthetic envelope.
///
/// Holds no operation and no engine: it carries the operation TYPE, which is all the
/// check needs (the input shape to read the request into and the input `Schema` to
/// validate it against). A request that passes is delegated to the one operation instance
/// the generated routing table built from the shared engine, so there is no second
/// instance of any operation and no second store.
struct ModelledInputRefusal<Op: Operation> {
    /// The operation this wrapper stands in front of, carried as a type alone.
    operation: PhantomData<fn() -> Op>,
    /// How to read this operation's `ClientToken`, for a create that requires one, and
    /// `None` for every operation that states no such requirement.
    client_token: Option<ClientTokenReader<Op>>,
}

impl<Op: Operation> ModelledInputRefusal<Op> {
    /// The wrapper for one modelled operation.
    const fn new() -> Self {
        Self {
            operation: PhantomData,
            client_token: None,
        }
    }

    /// The wrapper for a CREATE of this service, which requires the idempotency token
    /// `client_token` reads off its input ([`CLIENT_TOKEN_REQUIRED`]).
    const fn requiring_client_token(client_token: ClientTokenReader<Op>) -> Self {
        Self {
            operation: PhantomData,
            client_token: Some(client_token),
        }
    }

    /// This service's refusal of the request `Op` cannot be served from, or `None` when
    /// it has nothing to say — in which case the request goes on to the generated
    /// operation and the framework answers as it always did.
    ///
    /// The violations are the workspace's ONE collecting walk over a request no handler
    /// has seen yet ([`refusing_violations`]): it reads the request into the operation's
    /// input shape, folds a member the shape requires but the request omits into the same
    /// violation the framework's own walk reports for it, runs the model's constraint
    /// traits over what was read, and restores each clause's value echo to the bytes the
    /// caller sent. So this front door can find no violation set the framework's own walk
    /// would not, and a long value is quoted as submitted rather than as the elision a
    /// renderer would otherwise carry — and it refuses nothing the model accepts.
    fn refusal(
        &self,
        protocol: &dyn ServerHttpProtocol,
        req: &WireRequest,
    ) -> Option<InvalidInputException> {
        let violations = refusing_violations::<Op>(protocol, req);
        if violations.is_empty() {
            // The model accepted the request; this service's own requirement on a
            // create's idempotency token is what is left to state.
            return self.missing_client_token(protocol, req);
        }
        refusal_message(violations).map(invalid_input)
    }

    /// This service's refusal of a CREATE that states no idempotency token, or `None` when
    /// the operation requires none, the request carries one, or the request cannot be read
    /// into the operation's input at all — in which case the framework's own walk over the
    /// same bytes is what answers it.
    ///
    /// The token is read off the input the PROTOCOL's deserializer builds, so a member the
    /// request really carried is seen however the protocol writes it, and the check is made
    /// on exactly the value the handler behind this door would have been handed.
    fn missing_client_token(
        &self,
        protocol: &dyn ServerHttpProtocol,
        req: &WireRequest,
    ) -> Option<InvalidInputException> {
        let read_token = self.client_token?;
        let mut deserializer = protocol.deserialize_input(req, Op::INPUT_SCHEMA).ok()?;
        let input = <Op::Input as DeserializableStruct>::deserialize(&mut *deserializer).ok()?;
        drop(deserializer);
        crate::state::client_token_missing(read_token(&input))
            .then(|| invalid_input(CLIENT_TOKEN_REQUIRED.to_string()))
    }
}

impl<Op: Operation> ErasedOperation for ModelledInputRefusal<Op> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r WireRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let protocol = svc.definition().wire_protocol(req).protocol();
            if let Some(exception) = self.refusal(protocol, req) {
                // Serialized through the service's own protocol, exactly as a
                // handler-returned `InvalidInputException` is: the same body member
                // name (`Message`), the same `__type`, and the status the model
                // declares for it.
                match protocol.serialize_error(
                    &exception,
                    &SCHEMA_INVALID_INPUT_EXCEPTION,
                    INVALID_INPUT_EXCEPTION,
                    INVALID_INPUT_STATUS,
                ) {
                    Ok(response) => return response,
                    // A modelled error that will not serialize leaves the request to
                    // the framework, which refuses it in the synthetic envelope — the
                    // very divergence this wrapper removes, so it is logged rather than
                    // passed over silently.
                    Err(error) => tracing::warn!(
                        operation = Op::NAME,
                        %error,
                        "the modelled InvalidInputException would not serialize; this \
                         refusal falls back to the framework's envelope"
                    ),
                }
            }
            // Nothing to state: the request is served by the generated operation, which
            // runs the whole framework sequence (deserialize, validate, handle,
            // serialize) over the same bytes.
            match svc.definition().routing_table().get(Op::NAME) {
                Some(operation) => operation.run(svc, req, ctx).await,
                // Unreachable in practice: this wrapper is only reachable under the key
                // the generated table registered. Answer the framework's own
                // no-such-operation response rather than inventing one.
                None => svc.definition().routing_error_response(req),
            }
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // The wrapper is not an operation of its own: the request is signed (or not) as
        // the MODEL says for the operation it names, so authentication is decided by
        // the modelled operation this wrapper stands in front of.
        None
    }
}

/// The input-validation front door of every operation this service declares, keyed by
/// the routing key the model names each operation by ([`Operation::NAME`], the same key
/// the generated routing table uses).
///
/// A constraint is a property of the INPUT SHAPE and `InvalidInputException` is a common
/// error of every operation of this service, so the check goes in front of all of them —
/// implemented or still scaffolded. A stub answers 501 for a request it could serve and
/// the modelled 400 for one no operation could, which is the order the real service
/// evaluates them in.
///
/// An operation left out of this table keeps the framework's synthetic envelope — the
/// defect this check removes — so the list is not left to review:
/// `every_modelled_operation_states_its_refusal_in_the_modelled_shape` in
/// `tests/all/modelled_input_refusal.rs` drives every operation the model declares and
/// fails when one of them is not served through the wrapper.
pub(super) fn refusal_table<Eng: Send + Sync + 'static>(
) -> HashMap<&'static str, Box<dyn ErasedOperation>> {
    let mut refusals: HashMap<&'static str, Box<dyn ErasedOperation>> = HashMap::new();
    macro_rules! register {
        ($($module:ident :: $operation:ident),+ $(,)?) => {
            $({
                refusals.insert(
                    <crate::ops::$module::$operation<Eng> as Operation>::NAME,
                    Box::new(ModelledInputRefusal::<crate::ops::$module::$operation<Eng>>::new())
                        as Box<dyn ErasedOperation>,
                );
            })+
        };
    }
    // The CREATES, which require the idempotency token the model leaves optional: each
    // states where its own input carries it.
    macro_rules! register_create {
        ($($module:ident :: $operation:ident),+ $(,)?) => {
            $({
                refusals.insert(
                    <crate::ops::$module::$operation<Eng> as Operation>::NAME,
                    Box::new(ModelledInputRefusal::<
                        crate::ops::$module::$operation<Eng>,
                    >::requiring_client_token(|input| input.client_token.as_deref()))
                        as Box<dyn ErasedOperation>,
                );
            })+
        };
    }
    register_create!(
        create_event_bus::CreateEventBus,
        create_event_source::CreateEventSource,
        create_subscriber::CreateSubscriber,
    );
    // Every other operation the model declares, each served through the same front door.
    register!(
        delete_event_bus::DeleteEventBus,
        delete_event_source::DeleteEventSource,
        delete_resource_policy::DeleteResourcePolicy,
        delete_subscriber::DeleteSubscriber,
        describe_event_bus::DescribeEventBus,
        describe_event_source::DescribeEventSource,
        describe_subscriber::DescribeSubscriber,
        get_resource_policy::GetResourcePolicy,
        list_event_buses::ListEventBuses,
        list_event_sources::ListEventSources,
        list_resource_policies::ListResourcePolicies,
        list_subscribers::ListSubscribers,
        list_tags_for_resource::ListTagsForResource,
        put_events::PutEvents,
        put_raw_events::PutRawEvents,
        put_resource_policy::PutResourcePolicy,
        revoke_resource::RevokeResource,
        tag_resource::TagResource,
        untag_resource::UntagResource,
        update_event_bus::UpdateEventBus,
        update_event_source::UpdateEventSource,
        update_subscriber::UpdateSubscriber,
    );
    refusals
}
