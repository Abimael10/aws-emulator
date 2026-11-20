// SPDX-License-Identifier: CC0-1.0

//! What this service states about the constraint clauses its front door answers with, and
//! which operations pay for stating them.
//!
//! The framework checks a request against the constraints the model declares and renders
//! the refusal itself. One rendering in it is not a value any caller sent: a `@length` on a
//! collection is checked ON THE MEMBER, before the validating walk descends into the
//! elements, so the only thing the walk has to put in the clause's value half is a summary
//! of the collection's size (`<n> items`). AWS quotes the COLLECTION — the elements in
//! order, comma-separated, in brackets — which only the REQUEST can answer, and
//! `smithy_server_runtime::Service::override_refusal` is handed the violations without it.
//!
//! So the collecting pass runs per operation, where the request still is, through the
//! tree's one implementation of it ([`service_common::submitted_collections`]): that module
//! owns the wrapper, the collecting walk, the restored value half, the clause, the envelope
//! and the bounds on what one clause may quote. All this service states is the error code
//! its own refusal channel picks for the class ([`VALIDATION_EXCEPTION`]) and WHICH
//! operations are served through the pass: only the ones whose input declares a bounded
//! collection member, because no other can produce the clause and the pass costs a second
//! read of the body.

use ::service_common::submitted_collections::{StatedClause, StatedClauseTable};
use ::smithy_server_runtime::OperationRoutingTable;

/// The error code a constraint refusal of this `restJson1` service carries, unchanged from
/// the one the framework's refusal channel already picks for it: a restated message has to
/// travel with the code it would have travelled with, because a client branches on the
/// code.
const VALIDATION_EXCEPTION: &str = "ValidationException";

/// The operations of this service whose input declares a bounded collection member, and so
/// the only ones a clause quoting a collection can arise for: `CreateSubscription`,
/// `AssociateResourcesToSubscription` and `DisassociateResourcesFromSubscription` each
/// carry `resourceArns` (`ResourceArns`, `@length(min: 1, max: 10)`;
/// `smithy_query(query="operation", operation="CreateSubscription,…")`). Every other
/// operation takes scalars only — an ARN, a tier, an `If-Match`, a `nextToken` — and is
/// served without the pass.
pub(super) fn routing_table<Eng: Send + Sync + 'static>(
    engine: &::std::sync::Arc<Eng>,
) -> OperationRoutingTable {
    let mut stated = StatedClauseTable::new(StatedClause::under(VALIDATION_EXCEPTION));
    stated
        .quoting(crate::ops::create_subscription::CreateSubscription::new(
            ::std::sync::Arc::clone(engine),
        ))
        .quoting(
            crate::ops::associate_resources_to_subscription::AssociateResourcesToSubscription::new(
                ::std::sync::Arc::clone(engine),
            ),
        )
        .quoting(
            crate::ops::disassociate_resources_from_subscription::DisassociateResourcesFromSubscription::new(
                ::std::sync::Arc::clone(engine),
            ),
        );
    stated.finish()
}
