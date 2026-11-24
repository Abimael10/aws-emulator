// SPDX-License-Identifier: CC0-1.0

//! Whether the ARNs a request addresses a resource by name the Region this
//! endpoint serves — the check every operation of this service is served
//! behind, in front of the handler.
//!
//! An ARN is an absolute identity: it carries the partition, the Region and the
//! account its resource lives in. This endpoint serves ONE Region, the one the
//! request's SigV4 credential scope names, so an ARN naming another Region
//! addresses no resource reachable through it — a caller-addressing mistake,
//! which S3 Tables refuses as client input (`BadRequestException`, HTTP 400,
//! [`crate::state::REGION_MISMATCH_MESSAGE`]) rather than as a resource that is
//! not there: whether a resource of that name exists in the Region the ARN names
//! is not something this endpoint answers, and a 404 would send a caller
//! creating a resource it already has somewhere else.
//!
//! The decision belongs here, in front of the handler, for the same reason the
//! authorization decision does ([`super::authorization`]): it is the SERVICE's
//! and not one operation's — every operation that takes an ARN takes it with the
//! same meaning — and a request it refuses must have no effect, so it is taken
//! before a handler reads or writes anything. It is taken at the TYPED seam, so
//! the modelled constraints of the request (the `@pattern` an ARN member
//! declares) have already been checked by the framework's own walk: a value that
//! is no ARN at all is refused for that, by the sentence this service states
//! that class with, and never reaches this check.
//!
//! WHICH members carry an ARN, and the reading of them off the request's own
//! built input, come from [`super::addressing`], which both decisions in front
//! of a handler share.

use smithy_server_runtime::OperationContext;

use super::addressing::Addressing;

/// The sentence a request is refused with when an ARN it addresses a resource by
/// names a Region other than the one the request was made against, and `None`
/// when every ARN it carries names this Region (or names no Region at all).
///
/// `addressed` is what the request's own input addresses. Nothing is read from
/// the store and nothing is written: the decision is between the request's own
/// Region and the Regions its ARNs name.
#[must_use]
pub fn refusal(ctx: &OperationContext<'_>, addressed: &Addressing) -> Option<String> {
    let region = crate::state::request_region(ctx.parts());
    addressed
        .arns()
        .any(|arn| crate::state::arn_names_another_region(arn, &region))
        .then(|| crate::state::REGION_MISMATCH_MESSAGE.to_string())
}
