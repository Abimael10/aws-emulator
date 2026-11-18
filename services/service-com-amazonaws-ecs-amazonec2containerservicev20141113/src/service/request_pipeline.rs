// SPDX-License-Identifier: CC0-1.0

//! What this service does with a request BEFORE a handler runs: the IAM authorization
//! decision, the refusals this service words per operation, and then the modelled operation
//! the routing key names.
//!
//! The order is the one AWS keeps. A request is authorized at the IAM layer first
//! ([`super::authorization`] for this service's own action, then [`super::pass_role`] for a
//! role the request hands Amazon ECS) — a caller that may not perform the action, or may not
//! pass the role, is refused `AccessDeniedException` whatever else is wrong with the request,
//! and nothing it asked for is read or written — and only then does the service judge the
//! request itself. A request the gate does not decide is served by the generated operation
//! exactly as it always was.
//!
//! Between the two sits the one refusal this layer words itself: a condition whose sentence
//! belongs to the OPERATION rather than to the member at fault ([`super::refusal_wording`]),
//! which the framework's own walk cannot state because it is handed no operation to key it
//! on. A request that earns none of those reaches the operation untouched.
//!
//! It is ONE [`ErasedOperation`] rather than a decision taken in
//! [`smithy_server_runtime::Service::override_operation`], because that seam is handed no
//! request context: the authorization decision needs the credential store the context
//! carries ([`smithy_server_runtime::OperationContext::credential_store`]), which is
//! reachable only once the claimed operation RUNS. So the seam claims this layer for every
//! request the routing table serves, and this layer takes the decision and then hands the
//! request on — so the layer below keeps answering exactly what it answered before, for
//! every caller.

use std::sync::Arc;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use aws_smithy_schema_server::ProtocolError;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, ErasedServiceDefinition, OperationContext, Refused,
    ServerHttpProtocol, Service,
};

use crate::ops::types::{
    AccessDeniedException, InvalidParameterException, SCHEMA_ACCESS_DENIED_EXCEPTION,
    SCHEMA_INVALID_PARAMETER_EXCEPTION,
};

/// The error code the model pins on `AccessDeniedException`, which is also its shape name.
const ACCESS_DENIED_EXCEPTION: &str = "AccessDeniedException";

/// The status the model pins on `AccessDeniedException`: `@httpError 403`
/// (`smithy_query(query="operation",operation="CreateCluster",…)`).
const ACCESS_DENIED_STATUS: u16 = 403;

/// The error code the model pins on `InvalidParameterException`, which is also its shape
/// name — the one error this service declares for a request whose parameters it will not
/// serve, and the class every refusal worded in [`super::refusal_wording`] carries.
const INVALID_PARAMETER_EXCEPTION: &str = "InvalidParameterException";

/// The status the model pins on `InvalidParameterException`: `@httpError 400`
/// (`smithy_query(query="operation",operation="CreateService",…)`).
const INVALID_PARAMETER_STATUS: u16 = 400;

/// The pre-dispatch sequence of this service.
///
/// Holds the shared engine, because the authorization decision is made from the IAM and STS
/// stores of that engine — never a store of its own.
pub struct RequestPipeline<Eng> {
    engine: Arc<Eng>,
}

impl<Eng: Send + Sync + 'static> RequestPipeline<Eng> {
    /// Build the sequence from the shared engine — the same one every operation of this
    /// service is constructed from.
    pub fn new(engine: Arc<Eng>) -> Self {
        Self { engine }
    }

    /// The authorization refusal this request earns, or `None` when IAM permits it (and for
    /// every operation and caller the gate does not decide — see [`super::authorization`]).
    ///
    /// Two decisions, in the order AWS takes them: this service's own action on the resource
    /// the request addresses, and then `iam:PassRole` on each role the request hands Amazon
    /// ECS ([`super::pass_role`]) — a caller may only pass a role for a call it is allowed to
    /// make in the first place.
    fn authorization_refusal(
        &self,
        key: &str,
        protocol: &dyn ServerHttpProtocol,
        req: &RtRequest,
        ctx: &OperationContext<'_>,
        definition: &dyn ErasedServiceDefinition,
    ) -> Option<RtResponse> {
        // A resource ARN names a resource of one Region, so it is built in the Region the
        // request is signed for — the same key every handler reads.
        let region = crate::state::request_region(ctx);
        let denied = super::authorization::judged_resource(key, protocol, req, &region)
            .and_then(|resource| {
                super::authorization::authorize(&*self.engine, ctx, key, &resource).err()
            })
            .or_else(|| {
                super::pass_role::pass_role_refusal(&*self.engine, ctx, key, protocol, req)
            })?;
        Some(access_denied_response(definition, req, &denied))
    }
}

impl<Eng: Send + Sync + 'static> std::fmt::Debug for RequestPipeline<Eng> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RequestPipeline").finish_non_exhaustive()
    }
}

impl<Eng: Send + Sync + 'static> ErasedOperation for RequestPipeline<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // The service's model-derived half is read inside this block and not held past
            // it: `&dyn ErasedServiceDefinition` is not `Sync`, and the future returned here
            // is `Send`. Only the operation this layer hands the request to — which is
            // `Send + Sync` — crosses the await below.
            let served = {
                let definition = svc.definition();
                let protocol = definition.protocol();
                match protocol.routing_key(req).ok() {
                    // A request naming no operation of this service is answered the way one
                    // that names none is, rather than judged against an action nothing
                    // serves.
                    None => Ok(definition.routing_error_response(req)),
                    Some(key) => {
                        match self.authorization_refusal(key, protocol, req, ctx, definition) {
                            Some(response) => Ok(response),
                            // A condition THIS operation words its own sentence for, stated
                            // here because the framework's walk is handed no operation to
                            // key it on (see [`super::refusal_wording`]). Everything else
                            // reaches the operation and is refused exactly as before.
                            None => match super::refusal_wording::operation_worded_refusal(
                                key, protocol, req,
                            ) {
                                Some(message) => {
                                    Ok(invalid_parameter_response(definition, req, message))
                                }
                                None => match definition.routing_table().get(key) {
                                    Some(operation) => Err(operation),
                                    None => Ok(definition.routing_error_response(req)),
                                },
                            },
                        }
                    }
                }
            };
            match served {
                Ok(response) => response,
                Err(operation) => operation.run(svc, req, ctx).await,
            }
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // `None` is "defer to the modelled operation the routing key names", so this layer
        // cannot weaken the schemes the model declares for it.
        None
    }
}

/// A refusal worded by [`super::refusal_wording`] as a response in this service's own
/// protocol envelope, serialized through the same protocol call the framework's error tail
/// makes — so a sentence chosen here and one a handler returns are indistinguishable on the
/// wire.
///
/// Where the modelled envelope cannot be rendered at all, the same code and message are
/// answered through the service's framework-refusal envelope instead, so the refusal still
/// reaches the caller rather than falling through to a handler.
fn invalid_parameter_response(
    definition: &dyn ErasedServiceDefinition,
    req: &RtRequest,
    message: String,
) -> RtResponse {
    let refused = InvalidParameterException {
        message: Some(message),
    };
    definition
        .protocol()
        .serialize_error(
            &refused,
            &SCHEMA_INVALID_PARAMETER_EXCEPTION,
            INVALID_PARAMETER_EXCEPTION,
            INVALID_PARAMETER_STATUS,
        )
        .ok()
        .unwrap_or_else(|| {
            definition.refusal_response(
                ProtocolError::Invalid(Vec::new()),
                Some(Refused {
                    code: INVALID_PARAMETER_EXCEPTION.to_string(),
                    message: refused.message.clone().unwrap_or_default(),
                }),
                req,
            )
        })
}

/// The authorization refusal as a response in this service's own protocol envelope,
/// serialized through the same protocol call the framework's error tail makes, so a refusal
/// worded here and one a handler returns are indistinguishable on the wire.
///
/// FAIL CLOSED: where the modelled envelope cannot be rendered at all, the same code and
/// message are answered through the service's framework-refusal envelope instead, so a
/// denied request can never fall through to a handler.
fn access_denied_response(
    definition: &dyn ErasedServiceDefinition,
    req: &RtRequest,
    denied: &AccessDeniedException,
) -> RtResponse {
    let message = denied.message.clone().unwrap_or_default();
    definition
        .protocol()
        .serialize_error(
            denied,
            &SCHEMA_ACCESS_DENIED_EXCEPTION,
            ACCESS_DENIED_EXCEPTION,
            ACCESS_DENIED_STATUS,
        )
        .ok()
        .unwrap_or_else(|| {
            definition.refusal_response(
                ProtocolError::Invalid(Vec::new()),
                Some(Refused {
                    code: ACCESS_DENIED_EXCEPTION.to_string(),
                    message,
                }),
                req,
            )
        })
}
