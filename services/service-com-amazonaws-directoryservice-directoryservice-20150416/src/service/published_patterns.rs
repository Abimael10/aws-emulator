// SPDX-License-Identifier: CC0-1.0

//! The expression a constraint clause of this service QUOTES for a member, where the
//! expression this service publishes is not the one the model declares.
//!
//! ## The rule
//!
//! A clause about a value outside its member's `@pattern` quotes the expression the SERVICE
//! publishes for that member's shape. That is the model's text for almost every shape of
//! this model and, for a few of them, the same text with an anchor the service does not
//! publish dropped: without the `$` that CLOSES it, where the constraint text behind those
//! refusals is anchored at its start only, and without EITHER anchor, where the published
//! expression is the bare grammar the member's value has to be made of. Which of the three
//! forms a shape is published with cannot be read off the text — all of them are spelled
//! `^…$` in the model — so it is reference data about the SHAPE, kept here, keyed by the
//! expression itself ([`PUBLISHED_UNANCHORED_AT_THE_CLOSE`], [`PUBLISHED_UNANCHORED`]) and
//! therefore holding for every member that targets it: in any operation, in whatever binding
//! carried the value, and inside the constraint set a clause nests at a collection of them.
//!
//! The DROPS themselves are the tree-wide reading of those anchors
//! ([`::service_common::validation::unanchored_at_the_close`],
//! [`::service_common::validation::unanchored`]), so nothing about how an anchor is
//! recognised is decided here, and the CHECK is always the model's expression as written:
//! this module changes what a refusal QUOTES and never which values are refused.
//!
//! ## Where it is applied
//!
//! Through [`super::stated_violation`], the one restating every path of this service's
//! constraint refusals goes through — the boundary refusal [`super::stated_refusal`] words,
//! the clause stated at a list member ([`super::list_element_refusals`]) and the clause for
//! an expression the boundary validator could not apply
//! ([`super::model_pattern_refusals`]) — and in the one handler that words a clause of its
//! own for a member whose expression the boundary cannot apply at all
//! (`crate::ops::create_alias`), so one rendering serves every member whatever found it.

/// The expressions this service publishes WITHOUT the `$` that closes them, as the model
/// declares them.
///
/// Each row is a shape whose recorded refusal quotes its expression that way:
///
/// * `AliasName` (`smithy_query(query="shape",shape="AliasName")`) — the recorded refusal of
///   a malformed `CreateAlias$Alias`;
/// * `OCSPUrl` (`smithy_query(query="shape",shape="OCSPUrl")`) — the recorded refusal of a
///   malformed `RegisterCertificate$ClientCertAuthSettings.OCSPUrl`.
const PUBLISHED_UNANCHORED_AT_THE_CLOSE: &[&str] = &[
    r"^(?!D-|d-)([\da-zA-Z]+)([-]*[\da-zA-Z])*$",
    r"^(https?|ftp|file|ldaps?)://[-a-zA-Z0-9+&@#/%?=~_|!:,.;()]*[-a-zA-Z0-9+&@#/%=~_|()]$",
];

/// The expressions this service publishes WITHOUT EITHER of the anchors the model wraps
/// them in: the bare grammar a value of the shape has to be made of.
///
/// Each row is a shape whose recorded refusal quotes its expression that way:
///
/// * `TopicName` (`smithy_query(query="shape",shape="TopicName")`) — the recorded refusal of
///   a `RegisterEventTopic$TopicName` carrying a character outside the class, which quotes
///   `[a-zA-Z0-9_-]+`.
///
/// Every expression named in neither table is published as the model declares it, anchors
/// and all, which is what the recorded refusals of a directory id, a certificate id, an
/// assessment id, a tag value, an address block, a directory name and the element sets of
/// this service's lists quote.
const PUBLISHED_UNANCHORED: &[&str] = &[r"^[a-zA-Z0-9_-]+$"];

/// The expression a clause QUOTES for the modelled `declared`: the model's own text, or that
/// text less the anchor or anchors this service does not publish for the shape.
#[must_use]
pub fn published_pattern(declared: &str) -> String {
    if PUBLISHED_UNANCHORED.contains(&declared) {
        return ::service_common::validation::unanchored(declared);
    }
    if PUBLISHED_UNANCHORED_AT_THE_CLOSE.contains(&declared) {
        return ::service_common::validation::unanchored_at_the_close(declared);
    }
    declared.to_string()
}

/// Whether a clause about `declared` quotes an expression OTHER than the model's own text —
/// what tells a refusal this service has a rendering of its own to state for it.
#[must_use]
pub fn restates(declared: &str) -> bool {
    PUBLISHED_UNANCHORED.contains(&declared)
        || PUBLISHED_UNANCHORED_AT_THE_CLOSE.contains(&declared)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a clause quotes the expression the service publishes for the shape — the
    /// model's text, less the closing anchor for the shapes published that way and less both
    /// anchors for the shapes published with neither — and a drop touches no anchor the
    /// expression writes inside itself.
    #[test]
    fn a_clause_quotes_the_published_expression() {
        // (case, the model's expression, what a clause quotes)
        let rows: [(&str, &str, &str); 6] = [
            (
                "an alias name, published without its closing anchor",
                r"^(?!D-|d-)([\da-zA-Z]+)([-]*[\da-zA-Z])*$",
                r"^(?!D-|d-)([\da-zA-Z]+)([-]*[\da-zA-Z])*",
            ),
            (
                "an OCSP URL, published without its closing anchor",
                r"^(https?|ftp|file|ldaps?)://[-a-zA-Z0-9+&@#/%?=~_|!:,.;()]*[-a-zA-Z0-9+&@#/%=~_|()]$",
                r"^(https?|ftp|file|ldaps?)://[-a-zA-Z0-9+&@#/%?=~_|!:,.;()]*[-a-zA-Z0-9+&@#/%=~_|()]",
            ),
            (
                "a topic name, published without either anchor",
                r"^[a-zA-Z0-9_-]+$",
                r"[a-zA-Z0-9_-]+",
            ),
            (
                "a directory id, published as the model declares it",
                r"^d-[0-9a-f]{10}$",
                r"^d-[0-9a-f]{10}$",
            ),
            (
                "a certificate id, published as the model declares it",
                r"^c-[0-9a-f]{10}$",
                r"^c-[0-9a-f]{10}$",
            ),
            (
                "an assessment id, published as the model declares it",
                r"^da-[0-9a-f]{18}$",
                r"^da-[0-9a-f]{18}$",
            ),
        ];
        for (case, declared, expected) in rows {
            let published = published_pattern(declared);
            assert_eq!(
                published, expected,
                "case {case:?}: expected {expected:?}, got {published:?}"
            );
            assert_eq!(
                restates(declared),
                published != declared,
                "case {case:?}: expected the restating flag to follow the rendering, got \
                 {published:?}"
            );
        }
    }
}
