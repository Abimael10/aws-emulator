// SPDX-License-Identifier: CC0-1.0

//! Serving an untag that names NO tag key.
//!
//! `UntagResource` binds `tagKeys` to a REPEATED query parameter
//! (`smithy_query(query="operation",operation="UntagResource")`: `@httpQuery("tagKeys")` over
//! `TagKeyList`), and a repeated parameter carries a list by appearing once per entry — so a
//! client asking to remove an EMPTY list of keys appends no `tagKeys` parameter at all. The
//! absence of the parameter is therefore how the empty list travels, and reading it back as
//! the empty list is what makes the round trip faithful; the recorded service answers such a
//! request rather than refusing it, and removes nothing.
//!
//! The framework cannot read it that way: `tagKeys` is `@required`, so the generated input
//! deserializer refuses a request that carries no value for it before any handler runs, and
//! the route lookup withholds the route of a request missing a `@required` query binding in
//! the first place ([`super::EmulatedService::addressed_operation`] is what resolves it).
//! This module is the one place that difference is handled: it fills the member the request
//! left out with the empty list the client sent and hands that input to the ONE
//! `UntagResource` handler, so the removal, the ARN lookup, the not-found refusal and the
//! answer's framing are the operation's own and cannot drift from the keyed case.

use aws_smithy_runtime_api::http::{Request as WireRequest, Response as WireResponse};
use aws_smithy_schema_server::constraints::Violation;
use aws_smithy_schema_server::ProtocolError;
use smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, OperationError, Service,
};
use std::sync::Arc;

/// The operation this module serves, as the model and the generated routing table name it.
pub(super) const OPERATION: &str = "UntagResource";

/// The member this route fills from an absent parameter, as the validation walk names it.
/// Every OTHER member's modelled constraints still decide the request, and so does every
/// other clause reported at this one: only the minimum length of the list the client sent
/// empty is not applied to it ([`keyless_minimum`]), because the recorded service serves that
/// request.
const KEYLESS_MEMBER: &str = "tagKeys";

/// The operation `UntagResource` is served through when its `tagKeys` parameter is absent.
///
/// Holds the operation itself — built from the SAME shared engine as the one in the generated
/// routing table, so both read and write the one store — and nothing else: every decision
/// about the request is that operation's.
pub(super) struct UntagWithoutKeys<Eng> {
    inner: crate::ops::untag_resource::UntagResource<Eng>,
}

impl<Eng> UntagWithoutKeys<Eng> {
    /// Build it from the shared engine, the way the generated entry builds every operation.
    pub(super) fn new(engine: Arc<Eng>) -> Self {
        Self {
            inner: crate::ops::untag_resource::UntagResource::new(engine),
        }
    }
}

/// The ARN the request addresses: the one `@httpLabel` segment of `/tags/{resourceArn}`,
/// percent-decoded as a PATH label, since a non-greedy label carrying an ARN has its `:` and
/// `/` encoded by most clients and sent raw by some — and a `+` in it is a literal character
/// of the value rather than a space, which is why this reads it through
/// [`service_common::percent_decode_path_label`] (the one reader for a label taken off the
/// request line) rather than the query decoder.
///
/// The segment is the SECOND of the path, as the modelled URI lays it out. A request is only
/// routed here once `service_common::addresses_modeled_http_route` has matched that pattern,
/// and the label of a non-greedy pattern is exactly one segment — so a path with any other
/// shape never reaches this reader, and an ARN sent raw with its own `/` is a request the
/// route does not match rather than one read in part.
fn addressed_resource(req: &WireRequest) -> String {
    let path = service_common::uri_path(req.uri());
    let label = service_common::nth_path_segment(path, 1).unwrap_or_default();
    service_common::percent_decode_path_label(label)
}

/// Whether `violation` is the one clause this route waives: the minimum length of the list it
/// filled from a parameter the request did not carry.
fn keyless_minimum(violation: &Violation) -> bool {
    violation.path == KEYLESS_MEMBER
        && matches!(
            violation.kind,
            aws_smithy_schema_server::constraints::ViolationKind::MinLength(_)
        )
}

impl<Eng: Send + Sync + 'static> ErasedOperation for UntagWithoutKeys<Eng> {
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r WireRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, WireResponse> {
        /// The operation this route serves, for the schemas and the status its own
        /// `impl Operation` declares.
        type Op<E> = crate::ops::untag_resource::UntagResource<E>;

        Box::pin(async move {
            // The protocol the request is served in, chosen exactly as the framework's own
            // sequence chooses it: the author's `override_protocol` first — which is what
            // frames this service's no-content answers — then the model's.
            let protocol = svc
                .override_protocol()
                .unwrap_or_else(|| svc.definition().wire_protocol(req).protocol());

            let input = crate::ops::types::UntagResourceInput {
                resource_arn: addressed_resource(req),
                // The empty list the client sent by appending no parameter.
                tag_keys: Vec::new(),
            };

            // Every other member is still judged by its own modelled constraints — the
            // `GenericV2ARN` pattern on `resourceArn` above all — through the same walk and
            // the same refusal wording the framework applies to a keyed request. The ONE
            // clause dropped is the minimum length of the list this route filled from an
            // absent parameter: the request carried that list empty and the service serves
            // it, so that bound is not applied to it. Any other clause reported at that
            // member is still refused, so a constraint the model adds later cannot be
            // silently swallowed here.
            let violations: Vec<Violation> =
                aws_smithy_schema_server::constraints::validate(&input, Op::<Eng>::INPUT_SCHEMA)
                    .into_iter()
                    .filter(|violation| !keyless_minimum(violation))
                    .collect();
            if !violations.is_empty() {
                let refused = ProtocolError::Invalid(violations);
                let chosen =
                    smithy_server_runtime::refusal_of(&refused).and_then(|r| svc.override_refusal(&r));
                return svc.definition().refusal_response(refused, chosen, req);
            }

            let rendered = match self.inner.handle(input, ctx).await {
                Ok(output) => protocol.serialize_output(
                    &output,
                    Op::<Eng>::OUTPUT_SCHEMA,
                    Op::<Eng>::NAME,
                    Op::<Eng>::SUCCESS_STATUS,
                ),
                Err(error) => protocol.serialize_error(
                    error.as_serializable(),
                    error.variant_schema(),
                    error.shape_name(),
                    error.http_status(),
                ),
            };
            match rendered {
                Ok(response) => response,
                // A response this service's own protocol could not render is a framework
                // failure, worded in this service's envelope rather than panicking.
                Err(error) => svc.definition().framework_error_response(error, req),
            }
        })
    }

    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        // The model's schemes for the operation being served, read off that operation's own
        // generated `impl ErasedOperation`, so a request reaching this route is
        // authenticated exactly as a keyed one is. `None` would defer to the modelled
        // operation the request's routing key names, and this route exists because the
        // protocol resolves no key for the request.
        ErasedOperation::auth_schemes(&self.inner)
    }
}
