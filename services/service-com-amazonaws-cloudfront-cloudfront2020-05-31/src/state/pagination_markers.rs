// SPDX-License-Identifier: CC0-1.0

//! The ONE sentence this service refuses a listing's `Marker` with when the value is no
//! resume point that listing hands out.
//!
//! A `Marker` is the identifier the previous page ended at, so what a refusal of one states
//! is the CONSTRAINT on the parameter — which kind of identifier a marker of that listing has
//! to be. That constraint is a property of the PARAMETER rather than of the value submitted,
//! so the sentence is fixed text: nothing read off the request (the marker itself), and
//! nothing read off the service's own state (an identifier of a resource the listing is
//! scoped to), belongs in it. A caller matching the refusal matches one sentence per listing,
//! and an operator reading it is told the rule to fix rather than shown the value back.
//!
//! One renderer builds every such sentence ([`invalid_marker_message`]) from the kind of
//! identifier the listing's marker names, so the listings that reach it cannot disagree about
//! how this service states the same constraint. The listings whose refusal already stated it
//! keep their own constant beside the rest of their modelled error text
//! ([`crate::state::INVALID_PUBLIC_KEY_MARKER_MESSAGE`] and its neighbours): the SENTENCE is
//! the same text in the same template, written out once per listing because each is the wire
//! text recorded for that listing and is pinned verbatim by that listing's own suite — a
//! constant is what a suite can import and compare, and a renderer call would hide a drift in
//! the template behind a second expectation that drifted with it.

/// The template every invalid-`Marker` sentence of this service is one filling of: the
/// parameter, the constraint, and the kind of identifier it has to be.
///
/// Read the pieces off [`invalid_marker_message`] rather than splicing this yourself.
const MARKER_CONSTRAINT: &str = "Marker must be a valid";

/// The sentence a listing of DISTRIBUTIONS refuses an unusable `Marker` with — the id the
/// previous page of distributions ended at.
///
/// Stated for every listing whose marker is a distribution id, which is `ListDistributions`
/// and each listing that selects distributions by something they reference: the parameter and
/// its constraint are the same in all of them.
pub const DISTRIBUTION_MARKER_KIND: &str = "Distribution Id";

/// The sentence a listing of INVALIDATIONS refuses an unusable `Marker` with — the id of the
/// invalidation the previous page ended at, on either invalidation surface.
pub const INVALIDATION_MARKER_KIND: &str = "Invalidation Id";

/// The kind a DISTRIBUTION TENANT listing's marker names.
pub const DISTRIBUTION_TENANT_MARKER_KIND: &str = "CF distribution tenant Id";

/// The kind a CONNECTION GROUP listing's marker names.
pub const CONNECTION_GROUP_MARKER_KIND: &str = "CF connection group Id";

/// The kind a CONNECTION FUNCTION listing's marker names.
pub const CONNECTION_FUNCTION_MARKER_KIND: &str = "CF connection function Id";

/// The kind an ORIGIN ACCESS CONTROL listing's marker names.
pub const ORIGIN_ACCESS_CONTROL_MARKER_KIND: &str = "CF origin access control Id";

/// The kind an ORIGIN REQUEST POLICY listing's marker names, worded as the sibling policy
/// listings word theirs ([`crate::state::INVALID_CACHE_POLICY_MARKER_MESSAGE`],
/// [`crate::state::INVALID_RESPONSE_HEADERS_POLICY_MARKER_MESSAGE`]).
pub const ORIGIN_REQUEST_POLICY_MARKER_KIND: &str = "CF origin request policy Id";

/// The kind a CONTINUOUS DEPLOYMENT POLICY listing's marker names.
pub const CONTINUOUS_DEPLOYMENT_POLICY_MARKER_KIND: &str = "CF continuous deployment policy Id";

/// The kind a TRUST STORE listing's marker names, worded as the sibling listings of the
/// namespace-prefixed resource kinds word theirs ([`CONNECTION_GROUP_MARKER_KIND`]).
pub const TRUST_STORE_MARKER_KIND: &str = "CF trust store Id";

/// The kind a VPC ORIGIN listing's marker names, worded as the sibling listings of the
/// namespace-prefixed resource kinds word theirs ([`TRUST_STORE_MARKER_KIND`]), over the name
/// this service's own VPC origin sentences spell the kind with
/// ([`crate::state::VPC_ORIGIN_NOT_FOUND_MESSAGE`]).
pub const VPC_ORIGIN_MARKER_KIND: &str = "CF vpc origin Id";

/// The sentence this service refuses a `Marker` that is no resume point with, for a listing
/// whose marker names `kind`.
///
/// `kind` is the kind of identifier the parameter has to be — a property of the listing, not of
/// the request — so every value this takes is one of the constants above and never a value
/// read off a request or out of the store.
pub fn invalid_marker_message(kind: &str) -> String {
    format!("{MARKER_CONSTRAINT} {kind}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::INVALID_KEY_VALUE_STORE_MARKER_MESSAGE;

    /// The sentence is the constraint on the parameter and the kind of identifier it names,
    /// and it carries nothing else — one row per kind of marker this service pages by.
    #[test]
    fn an_invalid_marker_sentence_states_the_constraint_and_nothing_from_a_request() {
        // (case, the kind the listing's marker names, the sentence it is refused with)
        let rows: [(&str, &str, &str); 9] = [
            (
                "a distribution listing",
                DISTRIBUTION_MARKER_KIND,
                "Marker must be a valid Distribution Id",
            ),
            (
                "an invalidation listing",
                INVALIDATION_MARKER_KIND,
                "Marker must be a valid Invalidation Id",
            ),
            (
                "a distribution tenant listing",
                DISTRIBUTION_TENANT_MARKER_KIND,
                "Marker must be a valid CF distribution tenant Id",
            ),
            (
                "a connection group listing",
                CONNECTION_GROUP_MARKER_KIND,
                "Marker must be a valid CF connection group Id",
            ),
            (
                "a connection function listing",
                CONNECTION_FUNCTION_MARKER_KIND,
                "Marker must be a valid CF connection function Id",
            ),
            (
                "an origin access control listing",
                ORIGIN_ACCESS_CONTROL_MARKER_KIND,
                "Marker must be a valid CF origin access control Id",
            ),
            (
                "an origin request policy listing",
                ORIGIN_REQUEST_POLICY_MARKER_KIND,
                "Marker must be a valid CF origin request policy Id",
            ),
            (
                "a continuous deployment policy listing",
                CONTINUOUS_DEPLOYMENT_POLICY_MARKER_KIND,
                "Marker must be a valid CF continuous deployment policy Id",
            ),
            (
                "a listing whose marker names a name rather than an id",
                "CF key value store Name",
                INVALID_KEY_VALUE_STORE_MARKER_MESSAGE,
            ),
        ];
        for (case, kind, expected) in rows {
            let actual = invalid_marker_message(kind);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected the sentence {expected:?}, got {actual:?}"
            );
        }
    }
}
