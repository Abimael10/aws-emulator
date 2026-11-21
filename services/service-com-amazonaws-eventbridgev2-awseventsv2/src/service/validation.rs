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
//! The model's rules are the WHOLE of it: a member the model leaves optional is optional
//! here too, including an `@idempotencyToken` member, which a request may state or omit —
//! an SDK fills one in when the caller omits it, and a request that carries none is a
//! well-formed create that reaches its handler and mints its resource with no replay
//! record. This door adds no requirement of its own: it restates the model's refusals in
//! the modelled shape, and refuses nothing the model accepts.

use aws_smithy_runtime_api::http::{Request as WireRequest, Response as RtResponse};
use aws_smithy_schema_server::constraints::Violation;
use service_common::unstated_members::{stating_every_absence, unstated_members, worth_walking};
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

/// The whole `Message` of a refusal of a union-typed member the request did not set exactly
/// one variant of: the arity rule that member's shape documents, naming the member as the
/// model spells it.
///
/// FIXED text of that error class, with the member name the one variable part: the model is
/// where it comes from, not the request, and the message is the only thing telling the
/// caller the union arity was the problem rather than one of the values inside it.
fn one_variant_required(member: &str) -> String {
    format!("Exactly one {member} sub-field must be provided")
}

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
}

impl<Op: Operation> ModelledInputRefusal<Op> {
    /// The wrapper for one modelled operation.
    const fn new() -> Self {
        Self {
            operation: PhantomData,
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
    ///
    /// Two things that refuse a request are found while it is READ rather than while the
    /// built value is walked, so the read stops at the first and they reach no violation
    /// list ([`unstated_members`]): the SECOND and later `@required` members the request
    /// omits, which AWS states one clause each, and a union member set other than exactly
    /// once, whose shape documents "Exactly one variant must be set" and which the generated
    /// read otherwise answers by keeping the last variant or by failing with no violation at
    /// all.
    async fn refusal(
        &self,
        protocol: &dyn ServerHttpProtocol,
        req: &WireRequest,
    ) -> Option<InvalidInputException> {
        // The model's own constraints first, since they answer most refusals on their own.
        let violations = refusing_violations::<Op>(protocol, req);
        // What the request left UNSTATED about the input shapes is found by a walk of its
        // own, and only TWO violation sets leave anything for it to find
        // ([`worth_walking`]): exactly ONE absence, where the framework's own read stopped
        // — so a batch whose entries each break a different requirement would otherwise be
        // refused with one clause for all of them — and NO violation at all, where a union
        // member set other than exactly once is what is left, since the union's arity is no
        // constraint and so appears in no violation. The second of those is also the
        // request this service SERVES, which therefore pays for one pass of the walk: the
        // arity cannot be settled from the input schema instead, because a member schema
        // carries its member's name and not its target shape, so "does this input reach a
        // union" is not a question the schema can answer. A request with two violations, or
        // with one that is not an absence, is answered without the walk.
        if !worth_walking(&violations) {
            return refusal_message(violations).map(invalid_input);
        }
        let unstated = unstated_members::<Op>(protocol, req).await;
        let violations = stating_every_absence(violations, &unstated.omitted);
        if !violations.is_empty() {
            return refusal_message(violations).map(invalid_input);
        }
        if let Some(member) = unstated.unions.first() {
            return Some(invalid_input(one_variant_required(member)));
        }
        // The model accepted the request, so there is nothing for this door to state:
        // it enforces the model's rules and no requirement of its own.
        None
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
            if let Some(exception) = self.refusal(protocol, req).await {
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
    // Every operation the model declares, each served through the same front door.
    register!(
        create_event_bus::CreateEventBus,
        create_event_source::CreateEventSource,
        create_subscriber::CreateSubscriber,
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
