// SPDX-License-Identifier: CC0-1.0

//! The one half of a constraint refusal the framework's walk cannot state — the
//! COLLECTION a clause about a collection member is about.
//!
//! # The rule
//!
//! A value outside a constraint the model declares is refused before any handler runs,
//! over one clause per violation, each naming the value that was rejected, the member
//! path it sat at and the requirement it failed. The value is the only place the
//! request's own input is reported back, so a client comparing it against what it sent
//! has to see what it sent — a LIST included, which AWS states as the elements it holds
//! (`[<element>, …]`, each structure element as `<ShapeName>(<member>=<value>, …)`).
//!
//! A `@length` on a list is checked ON THE MEMBER, before the validating walk descends
//! into its elements, so the only rendering the framework has for the rejected value
//! there is a SUMMARY of its size (`<n> items`) — text no caller sent. The shared
//! restoring rule
//! ([`::service_common::validation_message::quoting_an_empty_submitted_collection`],
//! which [`super::validation`] already applies) can state the one collection that
//! summary determines completely, the EMPTY one; every other size needs the request's
//! own elements.
//!
//! # Why a wrapper
//!
//! [`Service::override_refusal`] is handed the violations ALONE and never the request
//! they were found in, so the elements cannot be recovered there. This is the layer that
//! has the request: a per-operation wrapper, which can re-read it.
//!
//! # Why not the tree's shared wrapper
//!
//! `service_common::submitted_collections` carries a wrapper of the same shape, and a
//! service whose refusal it can RENDER uses that one. It cannot render this service's: it
//! words the refusal itself, from the framework's clause over each violation, where this
//! service states a float member's value in its modelled type, an enum value set in its
//! own order, a `@pattern` requirement unanchored, a `@sensitive` member's clause without
//! its value and the clauses of ONE member with the pattern first
//! ([`super::validation`]) — so a refusal breaking a collection's `@length` AND one of
//! those at once would lose the second half. The wrapper here therefore makes no wording
//! decision at all: it hands the restated violations to that one seam.
//!
//! Nothing is re-implemented and no check is repeated in other words: the violations and
//! the renderings come from the tree's shared collecting walk
//! ([`::service_common::validation::refusing_violations_with_declared_echoes_over`],
//! which runs the protocol's own deserializer and the framework's own
//! `constraints::validate` and owns every bound on what one clause may quote), the
//! restoring rule is
//! [`::service_common::validation_message::quoting_the_submitted_collection`], the
//! wording is [`super::validation::refused`] (so the clauses, the envelope, the member
//! paths, the requirements and the withholding are stated in one place), and the status
//! and the wire envelope come from the framework's refusal channel.
//!
//! A refusal no clause of which is restated is left to that channel untouched: the
//! request is handed to the wrapped operation, which owns deserialize -> validate ->
//! handle -> serialize and every modelled error, and the refusal it raises is worded
//! exactly as it was before. The cost is DELIBERATE: an accepted request's body is read
//! twice, here and in the operation's own sequence, because the framework hands no
//! already-built input to an operation — which is why only the operations whose MODEL
//! declares a bounded collection member are served through it
//! (`super::management_events`: this model bounds `routingConfiguration` and no other
//! collection, so no other operation's request could produce such a clause).

use ::aws_smithy_runtime_api::http::{Request as RtRequest, Response as RtResponse};
use ::service_common::validation_message::quoting_the_submitted_collection;
use ::smithy_server_runtime::{
    BoxFuture, ErasedOperation, Operation, OperationContext, Refusal, Service, Violation,
};

/// The member POSITIONS the echo-collecting walk may visit for one request of this
/// service, stated from this model rather than left to the shared default.
///
/// A collection is quoted in a clause only when every one of its elements was walked, and
/// the one bounded collection of STRUCTURES this model declares is
/// `RoutingConfigurationList` (`@length` max 2 over the two-member
/// `RoutingConfigurationListItem`:
/// `smithy_query(query="shape", shape="RoutingConfigurationList")`), so the refusal of a
/// three-element list needs about a dozen positions. The ceiling is well past that so a
/// caller that sends many more entries than the bound admits is still quoted; past it the
/// framework's own count summary stands, which is the fail-safe direction.
const MAX_ECHO_POSITIONS: usize = 1024;

/// One modelled operation, served so that a refusal of a `@length` on one of its
/// COLLECTION members states the collection the caller submitted.
pub(super) struct QuotingSubmittedCollections<Op> {
    operation: Op,
}

impl<Op> QuotingSubmittedCollections<Op> {
    /// Wrap one operation, already constructed from the shared engine.
    pub(super) fn new(operation: Op) -> Self {
        Self { operation }
    }
}

impl<Op> ErasedOperation for QuotingSubmittedCollections<Op>
where
    Op: Operation + ErasedOperation,
{
    fn run<'r>(
        &'r self,
        svc: &'r dyn Service,
        req: &'r RtRequest,
        ctx: &'r OperationContext<'r>,
    ) -> BoxFuture<'r, RtResponse> {
        Box::pin(async move {
            // The protocol is read, used and dropped inside this one statement:
            // `&dyn ErasedServiceDefinition` is not `Sync`, so holding it across the
            // await below would make this future non-`Send`. It is the protocol this
            // service SERVES with (`override_protocol`), so the re-read sees the same
            // body the operation's own sequence does.
            let restated = match svc.override_protocol() {
                Some(protocol) => quoting_collections::<Op>(protocol, req),
                None => quoting_collections::<Op>(svc.definition().protocol(), req),
            };
            if let Some(violations) = restated {
                if let Some(stated) = super::validation::refused(&Refusal::Invalid(
                    violations.clone(),
                )) {
                    return svc.definition().refusal_response(
                        ::aws_smithy_schema_server::ProtocolError::Invalid(violations),
                        Some(stated),
                        req,
                    );
                }
            }
            self.operation.run(svc, req, ctx).await
        })
    }

    /// The wrapped operation's modelled auth schemes: wrapping an operation must not
    /// change which schemes sign it.
    fn auth_schemes(&self) -> Option<&'static [&'static str]> {
        self.operation.auth_schemes()
    }
}

/// The violations `req` earns from `Op`'s own modelled constraints, with the COLLECTION
/// a clause about a collection member is about put back — or `None` when no clause of
/// the refusal states one, which is every refusal this layer changes nothing about and
/// every request that breaks no constraint at all.
fn quoting_collections<Op: Operation>(
    protocol: &dyn ::smithy_server_runtime::ServerHttpProtocol,
    req: &RtRequest,
) -> Option<Vec<Violation>> {
    let found = ::service_common::validation::refusing_violations_with_declared_echoes_over::<Op>(
        protocol,
        req,
        MAX_ECHO_POSITIONS,
    );
    let mut restated_a_clause = false;
    let mut violations: Vec<Violation> = Vec::with_capacity(found.len());
    for (violation, echo) in found {
        let quoted = quoting_the_submitted_collection(violation.clone(), echo);
        if quoted.value != violation.value {
            restated_a_clause = true;
        }
        violations.push(quoted);
    }
    restated_a_clause.then_some(violations)
}
