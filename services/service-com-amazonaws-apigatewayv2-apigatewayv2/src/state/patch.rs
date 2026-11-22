// SPDX-License-Identifier: CC0-1.0

//! What a PATCH of a resource of this service stores for an OPTIONAL string member.
//!
//! A module of its own under [`crate::state`], because this is one rule every `Update…`
//! handler applies: each of them merges the members a request carries onto a stored
//! record, and each of them has to read a member carried EMPTY the same way, or two
//! resources of the same service would answer a caller that cleared a field differently.
//! [`crate::state`] re-exports every public item here, so a handler reaches them the way
//! it reaches the rest of this service's state helpers.

/// What a patch stores for an OPTIONAL string member the request carries.
///
/// An optional member is one the resource may simply not have, so its reader reports it
/// only while it holds a value. On a patch the EMPTY string therefore describes no value to
/// store: it asks for the stored one to be REMOVED, and the member is absent from that
/// answer and from every later read of the resource. Any other value replaces the stored one
/// as written.
///
/// The exceptions are the members for which the empty string is itself a VALUE the service
/// documents — `ApiMapping$ApiMappingKey`, where it addresses the root of the domain name —
/// and the REQUIRED members a resource always reports (an api's `Name`, a route's
/// `RouteKey`): there an empty value falls outside the member's own window and the patch is
/// refused rather than read as a clear. Neither kind of member is read through this.
pub fn cleared_by_empty(value: String) -> Option<String> {
    (!value.is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_empty_value_clears_the_member() {
        // Rows span what a patch can carry for such a member: the empty string, a value
        // that is only whitespace (text, so a value), a one-character value and a longer
        // one.
        let cases: &[(&str, &str, Option<&str>)] = &[
            ("the empty string, which clears", "", None),
            ("a single space, which is text", " ", Some(" ")),
            ("one character", "v", Some("v")),
            ("a longer value", "2026-01-01", Some("2026-01-01")),
        ];
        for (case, value, expected) in cases {
            let stored = cleared_by_empty(value.to_string());
            assert_eq!(
                stored.as_deref(),
                *expected,
                "case {case:?}: expected {expected:?}, got {stored:?}"
            );
        }
    }
}
