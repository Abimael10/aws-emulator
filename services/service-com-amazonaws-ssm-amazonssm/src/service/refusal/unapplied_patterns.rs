// SPDX-License-Identifier: CC0-1.0

//! The SEAM onto the tree's statement of one rule: the modelled `@pattern` the framework's
//! boundary DECLARES but does not APPLY, applied.
//!
//! The rule, the walk that finds those positions and the engine that evaluates their
//! expressions are NOT here — they are `service_common::unapplied_patterns`, which is the home
//! for them, and this module calls it. What is here is the one thing that belongs to this
//! service: its refusal path carries each violation beside the ECHO a clause can state the
//! submitted value with ([`SubmittedEcho`], which the shared collecting walk pairs the
//! violations it finds with), so the clauses that walk could not see have to be stated
//! together with the ones it did WITHOUT losing that pairing.
//!
//! Why the pairing matters, and why a clause added here needs no echo of its own: the echo
//! restores the COLLECTION a clause stated at a collection member quotes, where the walk
//! stored its own `<n> items` summary of one
//! (`service_common::validation_message::quoting_the_submitted_collection`). A clause the
//! boundary could not see is about a SCALAR leaf and already carries that leaf's own value, so
//! it is stated with no collection to restore.

use ::aws_smithy_schema_server::constraints::Violation;
use ::service_common::unapplied_patterns::{stated_together, unapplied_violations};
use ::service_common::validation::{refusing_violations_with_echoes, SubmittedEcho};
use ::smithy_server_runtime::{Operation, ServerHttpProtocol};

use super::RtRequest;

/// Every violation that refuses `req` before `O`'s handler runs, each paired with how a clause
/// can state the value it is about.
///
/// Both halves are the tree's shared ones: the collecting walk
/// ([`refusing_violations_with_echoes`]) and the positions whose modelled `@pattern` that
/// walk's own engine declares and does not apply ([`unapplied_violations`]), stated together
/// ([`stated_together`]) in the order one refusal states its clauses. So a requirement the
/// model states reaches a request whichever spelling the model wrote the expression in, and
/// this crate carries no second copy of either walk.
///
/// A request that breaks nothing the boundary skipped is handed back exactly as the shared
/// walk collected it: nothing is reordered for a refusal this seam has nothing to add to.
pub(crate) fn refusing_violations<O: Operation>(
    protocol: &dyn ServerHttpProtocol,
    req: &RtRequest,
) -> Vec<(Violation, SubmittedEcho)> {
    let walked = refusing_violations_with_echoes::<O>(protocol, req);
    let unapplied = unapplied_violations::<O>(protocol, req);
    if unapplied.is_empty() {
        return walked;
    }
    let stated = stated_together(
        O::INPUT_SCHEMA,
        walked
            .iter()
            .map(|(violation, _)| violation.clone())
            .collect(),
        unapplied,
    );
    // Each stated violation leaves with the echo it arrived with, matched by the violation
    // itself and taken out of the pool so two identical clauses cannot share one echo.
    let mut pool = walked;
    stated
        .into_iter()
        .map(|violation| {
            let echo = pool
                .iter()
                .position(|(walked, _)| *walked == violation)
                .map(|at| pool.remove(at).1)
                .unwrap_or(SubmittedEcho::Unrendered);
            (violation, echo)
        })
        .collect()
}
