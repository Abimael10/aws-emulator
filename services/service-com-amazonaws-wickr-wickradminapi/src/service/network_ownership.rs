// SPDX-License-Identifier: CC0-1.0

//! The ownership check this API takes in front of reading one network.
//!
//! The Wickr admin API resolves ownership of a `networkId` path label before it
//! validates the label's shape: a label it cannot own is DENIED rather than reported
//! back as a malformed value. The framework's own request walk is the other way round
//! — it validates the model's constraint traits and refuses before any handler runs —
//! so a label outside `NetworkId` (`@length` 8, `@pattern ^[0-9]{8}$`) never reaches
//! the handler, and the refusal the caller sees is the boundary's client-error
//! envelope rather than this API's denial.
//!
//! [`OwnershipFirst`] closes that gap for the one operation whose refusal path is
//! ownership-first: it collects the violations that would refuse the request before any
//! handler ran through the shared
//! [`service_common::validation::refusing_violations`] walk — the framework's own
//! deserializer plus `constraints::validate`, which is also what the centralized sequence
//! runs — and answers a request whose label breaks a declared constraint with the modelled
//! `ForbiddenError` and its fixed sentence. The rules checked are therefore exactly the
//! model's; only the ANSWER is this API's. A request whose label the model accepts is
//! delegated untouched to the operation this value fronts (the service's response-envelope
//! wrapper, and through it the generated operation in the routing table), which stays the
//! only thing that serves it — and which decides the rest of the ownership question (a
//! network of this account that is gone, an id it cannot place) from the store, where that
//! answer lives. The denial carries the same response envelope as every other response of
//! this service ([`crate::service::response_envelope`]).
//!
//! The cost is that a request the model accepts is read twice: once by the check below and
//! once by the operation it delegates to. That is a second deserialize of a single path
//! label and no second store access, and it buys the whole decision being made by the
//! framework's own walk rather than by a check written here.

use std::marker::PhantomData;

use aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, Service,
};

/// The wire error code (`__type`, and the `x-amzn-errortype` header restJson1 resolves
/// an error shape from) of the modelled `ForbiddenError` — the same name a
/// handler-returned one carries.
const FORBIDDEN_ERROR: &str = "ForbiddenError";

/// The status that shape declares (`@httpError(403)`).
const FORBIDDEN_ERROR_STATUS: u16 = 403;

/// One modelled operation, fronted so that an ownership denial precedes the model's own
/// constraint check on its path label.
///
/// Holds no operation instance and no engine: it carries the operation TYPE, which is all
/// the check needs (the input shape to read the request into and the input `Schema` to
/// validate against), and delegates a request that passes to `Inner` — the one wrapper the
/// service serves every other request through, which runs the operation the generated
/// routing table built from the shared engine. There is therefore no second instance of any
/// operation and no second store.
pub struct OwnershipFirst<Op, Inner> {
    operation: PhantomData<fn() -> Op>,
    inner: Inner,
}

impl<Op, Inner> OwnershipFirst<Op, Inner> {
    /// The one value a service holds for `Op`, fronting `inner`.
    pub const fn new(inner: Inner) -> Self {
        Self {
            operation: PhantomData,
            inner,
        }
    }
}

impl<Op, Inner: Default> Default for OwnershipFirst<Op, Inner> {
    fn default() -> Self {
        Self::new(Inner::default())
    }
}

impl<Op: Operation, Inner: ErasedOperation> ErasedOperation for OwnershipFirst<Op, Inner> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            let started = std::time::Instant::now();
            let protocol = svc.definition().wire_protocol(req).protocol();
            if label_outside_the_modelled_shape::<Op>(protocol, req) {
                let denial = crate::ops::types::ForbiddenError {
                    message: Some(crate::state::ACCESS_DENIED_MESSAGE.to_string()),
                };
                // Serialized through the service's own protocol, exactly as a
                // handler-returned `ForbiddenError` is: the same body member, the same
                // `__type`, the same `x-amzn-errortype` header, and the status the
                // model declares for the shape.
                match protocol.serialize_error(
                    &denial,
                    &crate::ops::types::SCHEMA_FORBIDDEN_ERROR,
                    FORBIDDEN_ERROR,
                    FORBIDDEN_ERROR_STATUS,
                ) {
                    Ok(response) => {
                        record_denial(ctx, Op::NAME, started.elapsed());
                        // Answered in the same envelope as every other response of this
                        // service, by the one function that puts it there.
                        return crate::service::response_envelope::enveloped(response);
                    }
                    // A modelled error that will not serialize leaves the request to
                    // the framework, which refuses it in the generic envelope — the
                    // very divergence this check removes, so it is logged rather than
                    // passed over silently.
                    Err(error) => tracing::warn!(
                        operation = Op::NAME,
                        %error,
                        "the modelled ForbiddenError would not serialize; this denial \
                         falls back to the framework's own refusal"
                    ),
                }
            }
            // Nothing to deny up front: the request is served by the operation this value
            // fronts, which runs the whole framework sequence (deserialize, validate,
            // handle, serialize) over the same bytes and answers it in this service's
            // envelope.
            self.inner.run(svc, req, ctx).await
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // Not an operation of its own: the request is signed (or not) as the MODEL
        // says for the operation it names, so authentication is decided by the
        // modelled operation this check stands in front of.
        None
    }
}

/// Whether `req` carries a value outside a constraint `Op`'s input shape declares — the
/// one refusal this API answers as a denial.
///
/// The walk is the shared one every service that words its own pre-handler refusal uses
/// ([`service_common::validation::refusing_violations`]), which is the framework's
/// deserializer followed by the model's constraint traits, in the framework's order. A
/// request the codec cannot read at all yields no violation there, so it is left to the
/// framework, which states it in its own envelope.
fn label_outside_the_modelled_shape<Op: Operation>(
    protocol: &dyn smithy_server_runtime::ServerHttpProtocol,
    req: &RtRequest,
) -> bool {
    !service_common::validation::refusing_violations::<Op>(protocol, req).is_empty()
}

/// Record a denial answered above as the operation row the framework sequence would
/// have recorded, so a denied request is not a hole in the telemetry series: the same
/// classification (from the status and error shape this denial carries), the same
/// folded handle (this service's signing name and the operation's own name), and the
/// same already sanitized request metadata. Everything is gated on a client
/// collecting, exactly as the framework gates it.
fn record_denial(
    ctx: &OperationContext<'_>,
    operation: &'static str,
    latency: std::time::Duration,
) {
    if !emulator_telemetry::recording_active() {
        return;
    }
    let handle =
        emulator_telemetry::OperationHandle::folded(ctx.identity().signing_name_static(), operation);
    emulator_telemetry::record_generated_operation_with_handle(
        &handle,
        latency,
        Some(emulator_telemetry::OperationFailure::classify(
            FORBIDDEN_ERROR_STATUS,
            FORBIDDEN_ERROR,
        )),
        smithy_server_runtime::RequestTelemetry::from_headers(&ctx.parts().headers).into_metadata(),
    );
}
