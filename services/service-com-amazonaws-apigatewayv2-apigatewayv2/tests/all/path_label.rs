// SPDX-License-Identifier: CC0-1.0

//! The ONE percent-encoder these suites address a caller-supplied value with: a
//! `ConnectionId` in a path segment, a stage name in a query value — whatever a case has to
//! put into a URI the way an SDK client does.
//!
//! Why it is shared: the values this crate's data planes are addressed by are not URI-safe —
//! a minted `ConnectionId` is standard base64, whose alphabet carries `/`, `+` and `=`, and
//! the stage that serves an api's root opens with `$` — so every suite that addresses one has
//! to encode it, and a suite that encoded it differently would be testing a request no client
//! sends. One statement of the rule, so they cannot drift.

/// The characters a value carries into a URI unencoded: the RFC 3986 unreserved set, which
/// is what an SDK client leaves alone.
pub(crate) const UNRESERVED: percent_encoding::AsciiSet = percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'.')
    .remove(b'_')
    .remove(b'~');

/// `label` as a request carries it into a URI: every character outside [`UNRESERVED`]
/// percent-encoded, by the maintained encoder rather than by a rule written here.
pub(crate) fn addressed(label: &str) -> String {
    percent_encoding::utf8_percent_encode(label, &UNRESERVED).to_string()
}
