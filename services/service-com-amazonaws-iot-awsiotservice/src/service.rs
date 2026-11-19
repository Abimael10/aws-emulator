// SPDX-License-Identifier: CC0-1.0

//! The `com-amazonaws-iot-awsiotservice` service: how it claims requests and how it words the
//! failures the framework hands back.
//!
//! EDIT THIS FILE. It is emitted once with working defaults and is never
//! regenerated, so a choice made here survives adding another service or
//! re-running the generator.
//!
//! `EmulatedService` is what the router mounts (`EmulatedService::new(engine)`).
//! It wraps the generated [`ServiceDefinition`], which carries the model-derived half
//! of this service — its identity, its wire protocol, and its operation routing
//! table — built from the Smithy model at compile time. You do not edit the
//! entry; you edit the behavioral seams below.
//!
//! Five opt-in seams are yours to change; every one defaults to the framework's
//! own behaviour, so an unedited service still routes and answers correctly:
//!
//! - `can_handle` — which requests this service claims. The default matches the
//!   request's resolved route key against this service's signing name. Edit it
//!   for anything the model does not express: a signing name shared by several
//!   services, or routing that keys on the access-key id.
//! - `override_operation` — serve a request the model's routing key cannot name
//!   correctly. The default returns `None`, so every request resolves through the
//!   generated routing table. Return `Some(op)` to route one yourself.
//! - `override_refusal` — how this service words a request the framework refused
//!   before any handler ran. The default returns `None`, keeping the framework's
//!   wording. See the inline note on that method for how to reword.
//! - `override_routing_error` — how this service words a request that reached it
//!   but named no modeled operation. The default returns `None`, so the entry
//!   renders the protocol-correct default (query -> 400 InvalidAction, awsJson ->
//!   404, …). Return `Some(resp)` to answer in your own envelope if desired.
//! - `override_framework_error` — how this service words a transport-level
//!   failure the orchestrator detects up front (e.g. a malformed `aws-chunked`
//!   body). The default returns `None`, keeping the entry's protocol-correct
//!   rendering. Return `Some(resp)` to reword it if desired.
//!
//! The model-derived half of this service — its identity, wire protocol, routing
//! table, and the default error renderers — lives in the generated `ServiceDefinition`
//! and is reached, not edited, through the one `definition` method below. You
//! do not write it; editing the entry would let this file drift from the model.
//!
//! ## The error shapes this service names with its API doc namespace
//!
//! AWS IoT names `InvalidRequestException` in `x-amzn-errortype` QUALIFIED with the
//! service's API doc namespace ([`IOT_DOC_NAMESPACE`]) — the shape name, a `:`, and the
//! namespace — which is the form a restJson1 client resolves an error shape from, and it
//! names `SqlParseException` the same way. Every OTHER error shape of this service is
//! named bare, which is what the framework's serializer already writes.
//!
//! It is the SHAPE that is named that way, not the layer that answered it, so the name is
//! the same wherever one of those two shapes comes from: the service's own validation
//! layer refusing a request before any handler ran ([`validation`]), or a handler refusing
//! one on a precondition only it can see. Every request of this service is therefore served
//! behind [`ShapeNamespacedErrorType`], which qualifies those two names on the way out and
//! changes nothing else about the response — a success, a not-found, a conflict and a
//! server fault answer exactly what they answered without it.
//!
//! ## The authorization decision in front of every operation
//!
//! AWS answers the IAM decision at the layer in FRONT of a service, before the request's own
//! shape is looked at, so every request this service serves passes through
//! [`AuthorizationGate`] first: a caller whose identity-based policies do not allow
//! `iot:<OperationName>` is refused `AccessDeniedException` (HTTP 403) and the call performs
//! nothing ([`authorization`]). The same layer answers the one other refusal that is an
//! authorization answer rather than a statement about a member — a command-execution
//! `targetArn` that can name no device of this account ([`command_execution_target`]).
//! `override_operation` is the only seam that can run ahead of the
//! framework's deserialize -> validate -> handle walk, which is why the gate is picked there;
//! every request it does not refuse is served by the modelled operation the request's own
//! routing key names.

use smithy_server_runtime::{
    http, BoxFuture, ErasedOperation, OperationContext, OperationRoutingTable, Refusal, Refused,
    Service,
};
use std::sync::Arc;

// How this service words a request the framework refused before any handler ran lives in
// `service/validation.rs`, beside the seam that answers it; WHICH error shape that refusal
// names for the planes whose operations model another one, in `service/modelled_refusal.rs`.
mod modelled_refusal;
mod validation;

// WHERE a command-execution `targetArn` outside this account is refused — in front of the
// request's own shape, beside the IAM decision — lives in
// `service/command_execution_target.rs`.
mod command_execution_target;

// The ONE reader of a request's query string, which both decisions above read a member from,
// lives in `service/request_query.rs`.
mod request_query;

use aws_smithy_runtime_api::http::{Request as WireRequest, Response as WireResponse};

/// Whether IAM lets the principal a request speaks for perform the action it asks for, taken
/// in front of every operation of this service ([`AuthorizationGate`]).
pub mod authorization;

/// The header a restJson1 client resolves an error SHAPE from.
const ERROR_TYPE_HEADER: &str = "x-amzn-errortype";

/// The error shape AWS IoT refuses a request its validation layer will not accept with.
const INVALID_REQUEST_EXCEPTION: &str = "InvalidRequestException";

/// The error shape AWS IoT refuses a rule whose SQL statement it cannot parse with.
const SQL_PARSE_EXCEPTION: &str = "SqlParseException";

/// The namespace AWS IoT qualifies an error-shape name with in [`ERROR_TYPE_HEADER`],
/// appended after a `:`.
///
/// It is the service's published API doc namespace, a fixed string: nothing of a request
/// or of this emulator's state goes into it.
const IOT_DOC_NAMESPACE: &str = "http://iot.amazonaws.com/doc/2015-10-02/";

/// What this service serves a request behind so that the error SHAPE a refusal names is
/// named the way AWS IoT names it: a shape of `qualified` carries [`IOT_DOC_NAMESPACE`],
/// and every other shape keeps the bare name the framework's serializer writes.
///
/// `qualified` is a property of the SHAPE rather than of an operation — the namespace is
/// the one the shape is declared in — so one wrapper holding the shapes this service
/// qualifies serves every operation of it, and no operation has to be named here to be
/// answered correctly.
///
/// Nothing else is re-implemented — the request is served by the generated
/// [`ErasedOperation::run`], which owns deserialize -> validate -> handle -> serialize and
/// every modelled error — and nothing but the one header is touched, so a success, a
/// not-found and a server fault answer exactly what they answered before.
pub struct ShapeNamespacedErrorType<O> {
    operation: O,
    qualified: &'static [&'static str],
}

impl<O> ShapeNamespacedErrorType<O> {
    /// Serve `operation`, already constructed from the shared engine, behind this
    /// wrapper, qualifying the error shapes `qualified` names.
    pub fn new(operation: O, qualified: &'static [&'static str]) -> Self {
        Self {
            operation,
            qualified,
        }
    }
}

impl<O: ErasedOperation> ErasedOperation for ShapeNamespacedErrorType<O> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r WireRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, WireResponse> {
        Box::pin(async move {
            let mut response = self.operation.run(svc, req, ctx).await;
            let named = response.headers().get(ERROR_TYPE_HEADER).map(str::to_owned);
            if let Some(named) = named {
                if self.qualified.contains(&named.as_str()) {
                    response
                        .headers_mut()
                        .insert(ERROR_TYPE_HEADER, qualified_error_shape(&named));
                }
            }
            response
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not
    /// change which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.operation.auth_schemes()
    }
}

/// An error shape as AWS IoT names it in [`ERROR_TYPE_HEADER`]: the shape name, a `:`, and
/// the service's API doc namespace.
fn qualified_error_shape(shape: &str) -> String {
    format!("{shape}:{IOT_DOC_NAMESPACE}")
}

/// The `com-amazonaws-iot-awsiotservice` emulated service: the generated [`ServiceDefinition`] plus
/// the behavioral seams this file owns.
///
/// Generic over the engine type `Eng` so this file names no concrete engine; the
/// router builds one shared engine and hands it to `new`.
pub struct EmulatedService<Eng: Send + Sync + 'static> {
    entry: crate::ServiceDefinition<Eng>,
    /// THE AUTHORIZATION GATE every request of this service passes through, which serves the
    /// modelled operation the request named once IAM permits the action
    /// ([`AuthorizationGate`]) — behind the one wrapper that names this service's two
    /// namespace-qualified error shapes ([`ShapeNamespacedErrorType`]), so a refusal of ANY
    /// operation names its shape the way AWS IoT names it.
    gate: ShapeNamespacedErrorType<AuthorizationGate<Eng>>,
}

/// The error shapes of this service that are named QUALIFIED with its API doc namespace in
/// [`ERROR_TYPE_HEADER`], whichever operation answers one and whichever layer of it did:
/// the legacy IoT exception the service refuses a request it will not accept with, and the
/// one it refuses an unparsable rule statement with.
const NAMESPACED_ERROR_SHAPES: &[&str] = &[INVALID_REQUEST_EXCEPTION, SQL_PARSE_EXCEPTION];

/// THE GATE every request this service serves passes through, as an operation the dispatch
/// path can run: whether IAM permits the ACTION the request named
/// ([`authorization::authorize`]).
///
/// The order is AWS's: authorization is decided by the IAM layer in FRONT of the service, so it
/// is answered before anything about the request's own resource is resolved, let alone
/// deserialized or constraint-checked. A request it does not refuse is handed to the modelled
/// operation the request named, read off this service's own routing table, so it is served by
/// exactly the centralized deserialize -> validate -> handle walk every other request takes:
/// the gate changes WHICH refusal comes first, not whether the modelled constraints are
/// enforced.
///
/// It names no operation of its own, because the operation is the routing key the request
/// carries: ONE gate stands in front of all of this service's operations, and the action it
/// judges is that key under this service's own IAM prefix.
///
/// The operation an authorized request is handed to is read off this service's own tables: the
/// generated routing table, or — for the planes whose operations model another error shape for
/// an invalid request — the wrapped one [`modelled_refusal`] builds, which serves the same
/// modelled operation behind the layer that states that shape. Neither is a re-route: both
/// tables are keyed by the operation's own modelled name.
pub struct AuthorizationGate<Eng> {
    /// The same shared engine the entry constructed each operation from, so the decision reads
    /// the account's own IAM and STS stores rather than a second copy of them.
    engine: Arc<Eng>,
    /// The operations served through [`modelled_refusal`], keyed by the routing key the
    /// protocol resolves: those whose model declares another error shape for an invalid
    /// request than the classic surface's. A key this table does not hold is served by the
    /// generated routing table, which is every other operation of this service.
    stated_refusals: OperationRoutingTable,
}

impl<Eng: Send + Sync + 'static> AuthorizationGate<Eng> {
    /// A gate on the shared engine.
    fn new(engine: Arc<Eng>) -> Self {
        Self {
            stated_refusals: modelled_refusal::stated_refusals(&engine),
            engine,
        }
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for AuthorizationGate<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r WireRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, WireResponse> {
        Box::pin(async move {
            // The operation the request named, read exactly as dispatch reads it: the routing
            // key this service's protocol resolves from the request line. It is the ACTION the
            // IAM decision is about, and the key the modelled operation is wired under.
            let Ok(operation) = svc.definition().protocol().routing_key(req) else {
                // `override_operation` picks this gate only for a request whose key resolves,
                // so this is unreachable; answered as the request naming no modelled
                // operation, which is what it would be.
                return svc.definition().routing_error_response(req);
            };
            // The resource the request ADDRESSES, under the ARN namespace of the identity the
            // request resolved to — which is this service's `arnNamespace` and `sigv4Name`
            // alike, so the ARN read out of the request is read as an ARN of THIS service —
            // and in the Region the request was signed for, which is the Region segment of an
            // ARN this service mints.
            let resource = authorization::judged_resource(
                authorization::action_prefix(ctx),
                &crate::state::request_region(ctx.parts()),
                operation,
                req,
            );
            // IAM's decision, taken before the operation reads or writes anything. A refused
            // request never reaches the delegate, so it mints nothing and reports nothing.
            if let Err(denial) =
                authorization::authorize(&*self.engine, ctx, operation, &resource)
            {
                return authorization::refusal_response(&denial);
            }
            // The OTHER answer this layer owes: a request whose `targetArn` names no device
            // of this account is refused on that ground before its own members are
            // constraint-checked, which is where AWS answers it
            // ([`command_execution_target`]). The operation decides the same thing for
            // itself; what is decided here is only the order.
            if let Some(refused) = command_execution_target::refusal(operation, req) {
                return refused;
            }
            match self
                .stated_refusals
                .get(operation)
                .or_else(|| svc.definition().routing_table().get(operation))
            {
                Some(modelled) => modelled.run(svc, req, ctx).await,
                // Unreachable in practice: the key came from this service's own protocol.
                // Answer the framework's own no-such-operation response rather than inventing
                // one.
                None => svc.definition().routing_error_response(req),
            }
        })
    }

    /// `None`, NOT an empty list: the request is still signed as the operation the MODEL names,
    /// so wrapping every operation in this gate changes which schemes sign none of them.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        None
    }
}

impl<Eng: Send + Sync + 'static> EmulatedService<Eng> {
    /// Build this service from the shared engine. The engine is threaded into the
    /// generated [`ServiceDefinition`], which constructs each operation from it; the
    /// operation's behaviour lives in that operation's own `impl Operation`.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self {
            entry: crate::ServiceDefinition::new(Arc::clone(&engine)),
            // The SAME shared engine the entry's own operations were constructed from, so
            // the gate reads the one store — and the operation it hands an authorized
            // request to is the entry's own, off this service's routing table.
            gate: ShapeNamespacedErrorType::new(
                AuthorizationGate::new(engine),
                NAMESPACED_ERROR_SHAPES,
            ),
        }
    }
}

impl<Eng: Send + Sync + 'static> Service for EmulatedService<Eng> {
    fn definition(&self) -> &dyn smithy_server_runtime::ErasedServiceDefinition {
        // The model-derived half of this service — identity, protocol, routing
        // table, and the default error renderers — lives in the generated entry.
        // The router reads it through this one accessor. Do not edit the entry.
        &self.entry
    }

    fn can_handle(&self, parts: &http::request::Parts) -> bool {
        // Claim a request whose resolved route key is this service's signing
        // name. Edit this for anything the model does not express — a signing
        // name shared by several services, or access-key-id-based routing.
        match smithy_server_runtime::resolve_route_key(parts) {
            Some(key) => key == self.entry.identity().signing_name(),
            None => false,
        }
    }

    fn override_operation(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<&dyn ErasedOperation> {
        // THE AUTHORIZATION GATE, and the only seam it can run in: a request is authorized
        // before its shape is validated, because the framework's centralized walk is
        // deserialize -> validate -> handle and a handler therefore runs only AFTER
        // modelled-constraint enforcement, while AWS answers the IAM decision in front of all
        // of it ([`AuthorizationGate`]). NOT a re-route: the gate serves the very operation
        // the model's own routing key names, so a key the model does not name resolves to
        // `None` here exactly as it does in the generated table.
        //
        // The gate is served behind `ShapeNamespacedErrorType` (see the module docs), which
        // names this service's two namespace-qualified error shapes the way it names them —
        // for every operation, since the namespace belongs to the SHAPE. Every other shape
        // passes through untouched, the authorization layer's own `AccessDeniedException`
        // included.
        if self.entry.protocol().routing_key(req).is_err() {
            return None;
        }
        Some(&self.gate as &dyn ErasedOperation)
    }

    fn override_refusal(&self, refusal: &Refusal) -> Option<Refused> {
        // The centralized walk calls this on a framework-level refusal (an absent
        // required member, or a value outside a declared bound) to let this service word
        // it the way AWS IoT words one: the envelope and the clauses of
        // `service::validation`, whose own module docs state what it chooses and what it
        // leaves to the framework. `None` from there keeps the framework's wording.
        validation::refused(refusal)
    }

    fn override_routing_error(
        &self,
        req: &aws_smithy_runtime_api::http::Request,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this when a request reached this service but named no
        // modeled operation — neither `override_operation` nor the generated
        // routing table resolved it. Returning `None` renders the protocol-correct
        // default for this service's envelope (query -> 400 InvalidAction text/xml,
        // awsJson -> 404, rpcv2Cbor -> 404, rest -> 501). Return `Some(resp)` to
        // word that routing failure in your own envelope instead.
        let _ = req;
        None
    }

    fn override_framework_error(
        &self,
        error: &aws_smithy_schema_server::ProtocolError,
    ) -> Option<aws_smithy_runtime_api::http::Response> {
        // The router calls this on a transport-level failure it detects up front —
        // before (or instead of) naming an operation — such as a malformed
        // `aws-chunked` body. Returning `None` keeps the entry's protocol-correct
        // rendering. Return `Some(resp)` to reword that failure in your own
        // envelope. The error is borrowed, so the default renderer can still
        // consume it on `None`.
        let _ = error;
        None
    }
}
