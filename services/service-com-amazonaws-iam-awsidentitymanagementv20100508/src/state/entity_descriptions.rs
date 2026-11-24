// SPDX-License-Identifier: CC0-1.0

//! The one spelling a record keeps for an entity's DESCRIPTION.
//!
//! A description is free text an account writes about one of its own entities, and every
//! member carrying one is published with a length whose floor is zero (`policyDescriptionType`
//! and `roleDescriptionType`, `smithy_query(query="shape",
//! shape="policyDescriptionType,roleDescriptionType")`). A floor of zero makes the EMPTY
//! STRING a legal value of the member rather than a way of supplying none: a description a
//! request spells as the empty string is a description the record HOLDS, and every reader of
//! the entity reports the member as that empty text. Only a request carrying no member at all
//! leaves the entity undescribed, and only then does a read omit the member.
//!
//! That matters on the wire rather than only in the store: in the awsQuery/XML protocol an
//! absent member and one serialized as an empty element decode differently, so a client that
//! reads a description back after CLEARING it — a write of the empty string is what clears one
//! — sees the empty text it wrote, where a missing member is an unwrap at the SDK boundary
//! instead. It is the reading this crate takes of a SET member an entity holds nothing in
//! ([`super::reported_entities`]): what the record holds is reported, and an empty value is
//! something held rather than nothing at all.
//!
//! Every writer of a described entity goes through this one reading, so the create's own
//! response and every later read of the record agree about what the entity is described by.

/// The description a record holds for text a request supplied: the text itself, the empty
/// string included, and NONE only where the request carried no description member at all.
///
/// Text is held exactly as the request wrote it — spaces, line breaks, and no characters at
/// all — because the member's published length admits every one of them and nothing published
/// says IAM rewrites the text it was given.
#[must_use]
pub fn held_description(supplied: Option<String>) -> Option<String> {
    supplied
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: a description is held exactly as the request supplied it, the empty string
    /// included, and none at all only where the request carried no member.
    #[test]
    fn a_description_is_held_exactly_as_the_request_supplied_it() {
        // The rows differ along what the rule generalises over: how the request spells what
        // it supplied — no member, the empty string, one character, a sentence, and text made
        // only of blank characters.
        let rows: [(&str, Option<&str>, Option<&str>); 6] = [
            ("no description member at all", None, None),
            ("the empty description", Some(""), Some("")),
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
