// SPDX-License-Identifier: CC0-1.0

//! The TAG SET a request applies to a CodeDeploy resource, read as the set it is.
//!
//! A resource's tags are keyed by their key — a tag whose key the resource
//! already carries replaces that key's value rather than adding a second entry
//! for it ([`super::set_tag`]) — so the tags a request applies are a MAP from key
//! to value, sent over the wire as a list. A list that names one key twice is
//! therefore not a tag set at all: the resource cannot carry both values, and
//! which of them would survive is nothing the request says. So such a request is
//! refused, by the error the operation models for a tag set it will not apply and
//! before anything is written, rather than applied with one of the two values
//! chosen for the caller.
//!
//! The rule is about a set being APPLIED. A list of tags or keys to REMOVE names
//! the same key's removal twice, which leaves exactly the tags one naming it once
//! would, so repetition there determines the outcome and is not refused.

use crate::ops::types::Tag;

/// The first key `tags` names more than once, or `None` when every key in the
/// list is distinct.
///
/// Scanning in request order reports the key at the repeat the caller can see
/// first, and a tag carrying no key at all names no key to repeat — such a tag is
/// refused on its own shape, by the operation that checks it
/// ([`super::tag_key_value`]).
#[must_use]
pub fn duplicate_tag_key(tags: &[Tag]) -> Option<&str> {
    let mut seen: ::std::collections::HashSet<&str> = ::std::collections::HashSet::new();
    tags.iter()
        .filter_map(|tag| tag.key.as_deref())
        .find(|key| !seen.insert(key))
}

/// The sentence a tag set naming one key twice is refused with: fixed text whose
/// variable part is the repeated key, taken from the request.
#[must_use]
pub fn duplicate_tag_key_message(key: &str) -> String {
    format!("Duplicate Tag key: {key}")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One row of the duplicate table: the case name, the `(key, value)` pairs
    /// the list carries, and the key that is repeated in it, if any.
    type DuplicateCase<'a> = (&'a str, &'a [(&'a str, &'a str)], Option<&'a str>);

    /// A tag list, as the model's `Tag` shape carries it (`Key`, `Value`).
    fn tags(pairs: &[(&str, &str)]) -> Vec<Tag> {
        pairs
            .iter()
            .map(|(key, value)| Tag {
                key: Some((*key).to_string()),
                value: Some((*value).to_string()),
            })
            .collect()
    }

    #[test]
    fn a_repeated_key_is_reported_and_distinct_keys_are_not() {
        // Rows differ along where the repeat sits, how many keys the list
        // carries, and whether the repeated entries share a value.
        let cases: &[DuplicateCase<'_>] = &[
            ("empty", &[], None),
            ("one tag", &[("Env", "prod")], None),
            ("distinct keys", &[("Env", "prod"), ("Owner", "ana")], None),
            (
                "case differs, so the keys differ",
                &[("Env", "prod"), ("env", "dev")],
                None,
            ),
            ("adjacent repeat", &[("Env", "prod"), ("Env", "dev")], Some("Env")),
            (
                "same value repeated",
                &[("Env", "prod"), ("Env", "prod")],
                Some("Env"),
            ),
            (
                "repeat after another key",
                &[("Env", "prod"), ("Owner", "ana"), ("Env", "dev")],
                Some("Env"),
            ),
            (
                "two repeats: the first one is reported",
                &[
                    ("Owner", "ana"),
                    ("Env", "prod"),
                    ("Owner", "bo"),
                    ("Env", "dev"),
                ],
                Some("Owner"),
            ),
        ];

        for (case, pairs, expected) in cases {
            let list = tags(pairs);
            let found = duplicate_tag_key(&list);
            assert_eq!(
                found, *expected,
                "case {case:?}: expected {expected:?}, got {found:?}"
            );
        }
    }

    #[test]
    fn the_sentence_names_the_repeated_key() {
        for key in ["owner", "Env", "a key with spaces"] {
            assert_eq!(
                duplicate_tag_key_message(key),
                format!("Duplicate Tag key: {key}"),
                "case {key:?}: the sentence carries the repeated key"
            );
        }
    }
}
