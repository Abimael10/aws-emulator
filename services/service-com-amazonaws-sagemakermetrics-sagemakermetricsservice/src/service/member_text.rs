// SPDX-License-Identifier: CC0-1.0

//! What TEXT a member carries says about whether the request stated a value for it — the
//! one reading of that question this service takes, wherever it judges a string.
//!
//! # The rule
//!
//! A string with no non-blank character states no more than an absent member does: the
//! request named nothing. So blank text is read as UNSET everywhere — at the request
//! boundary ([`super::envelope`]), where a resource reference is resolved
//! ([`crate::ops::batch_get_metrics`]), where a data point's series name is read
//! ([`crate::ops::batch_put_metrics`]) and where a request's addressed resources are
//! collected for the authorization decision ([`super::authorization`]) — rather than as a
//! submitted value to be judged against the member's modelled constraints.
//!
//! It is stated once, here, because a value read as unset by one layer and as submitted by
//! another is the same request answered two ways: a reference of blanks served in-band by
//! one reading and refused outright by the other.
//!
//! Blank is `str::trim`'s own reading of blank — every Unicode whitespace character, not
//! the ASCII space alone — so a reference of tabs, newlines or non-breaking spaces is read
//! exactly as a reference of spaces is.

/// Whether the text a member carries states NO value: it has no non-blank character.
///
/// A value that states nothing is read as the request not stating that member at all. Text
/// with any non-blank character in it is a submitted value, taken as sent — nothing is
/// trimmed OFF a value, since trimming would silently rename what the caller sent.
pub(crate) fn states_no_value(text: &str) -> bool {
    text.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RULE: text with no non-blank character states no value, whatever whitespace it is
    /// made of; text with one states a value, taken as sent.
    #[test]
    fn only_text_without_a_non_blank_character_states_no_value() {
        // (case, the text a member carried, whether it states no value)
        let rows = [
            ("nothing at all", "", true),
            ("one space", " ", true),
            ("several spaces", "   ", true),
            ("a tab", "\t", true),
            ("a newline", "\n", true),
            ("a carriage return and newline", "\r\n", true),
            ("mixed blanks", " \t\r\n ", true),
            // `str::trim` reads every Unicode whitespace character as blank.
            ("an ideographic space", "\u{3000}", true),
            ("a non-breaking space", "\u{00a0}", true),
            ("a word", "loss", false),
            ("a word between blanks", "  loss  ", false),
            ("one non-blank character", "x", false),
            ("a reference", "arn:aws:sagemaker:us-east-1:123456789012:t/1", false),
        ];
        for (case, text, expected) in rows {
            let actual = states_no_value(text);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected states_no_value == {expected} for {text:?}, got {actual}"
            );
        }
    }
}
