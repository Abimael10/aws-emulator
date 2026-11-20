// SPDX-License-Identifier: CC0-1.0

//! The one spelling a record keeps for an entity's DESCRIPTION.
//!
//! A description is free text an account writes about one of its own entities, and every
//! member carrying one is published with a length whose floor is zero (`policyDescriptionType`
//! and `roleDescriptionType`, `smithy_query(query="shape",
//! shape="policyDescriptionType,roleDescriptionType")`), so a request may legally supply no
//! text at all. Text that is not there is not a description: a request that supplies the empty
//! string describes the entity exactly as much as one that supplies no member, so both are
//! HELD as none — the single spelling for the absence of a description — and no reader of the
//! entity then reports the member at all.
//!
//! That matters on the wire rather than only in the store: in the awsQuery/XML protocol an
//! absent member and one serialized as an empty element decode differently, so a client that
//! reads presence as "this entity has a description", or round-trips a read of one into a
//! create, must see the member only where there is text to report. It is the same rule this
//! crate already applies to a tag SET a write leaves empty, which is held as no set at all
//! ([`super::reported_entities`]) — with the opposite report, because a tag set has an empty
//! report and a description does not.
//!
//! Every writer of a described entity goes through this one reading, so the create's own
//! response and every later read of the record agree about whether the entity is described.

/// The description a record holds for text a request supplied: the text itself, and NONE
/// where the request supplied no description or supplied one carrying no text.
///
/// Only the EMPTY string is the absence of a description — text is held exactly as the request
/// wrote it, spaces and line breaks included, because a description of blank characters is
/// still text the account chose to write and nothing published says IAM trims it.
#[must_use]
pub fn held_description(supplied: Option<String>) -> Option<String> {
    supplied.filter(|description| !description.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a description is held exactly where the request supplied text, and the empty
    /// string is held as no description at all.
    #[test]
    fn only_a_description_carrying_text_is_held() {
        // The rows differ along what the rule generalises over: how the request spells what
        // it supplied — no member, the empty string, one character, a sentence, and text made
        // only of blank characters.
        let rows: [(&str, Option<&str>, Option<&str>); 6] = [
            ("no description member at all", None, None),
            ("the empty description", Some(""), None),
            ("a single character", Some("x"), Some("x")),
            (
                "a sentence",
                Some("Grants access to production tables."),
                Some("Grants access to production tables."),
            ),
            ("a single space", Some(" "), Some(" ")),
            ("a line break and a tab", Some("\n\t"), Some("\n\t")),
        ];

        for (case, supplied, expected) in rows {
            let held = held_description(supplied.map(str::to_string));
            assert_eq!(
                held.as_deref(),
                expected,
                "case {case:?}: expected {expected:?} to be held, got {held:?}"
            );
        }
    }
}
