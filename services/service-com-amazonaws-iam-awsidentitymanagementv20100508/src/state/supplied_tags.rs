// SPDX-License-Identifier: CC0-1.0

//! The validation every IAM operation applies to the TAG LIST a request supplies, in one
//! place.
//!
//! One concern, held once: "if any one of the tags is invalid … then the entire request
//! fails and the resource is not created" (`TagPolicy`), so the whole list is judged before
//! anything is read or written, and every operation that takes a `Tags` list — the `Tag*`
//! calls and the `Create*`/`Upload*` calls that attach tags as they mint a resource — judges
//! it the same way and refuses with the same sentence. The rules the list has to satisfy
//! beyond the member constraints the model states (`tagKeyType`, `tagValueType`, the list's
//! own 0..50 bound) are:
//!
//! * no key begins with the prefix AWS reserves ([`super::RESERVED_TAG_KEY_PREFIX`]);
//! * no key is named twice, because such a request does not say which of the two values the
//!   tag is to carry ([`has_duplicate_tag_key`]).
//!
//! What a resource ends up holding once the list is admissible is [`super::merge_tags`]'s
//! rule, and the per-resource quota that merge is taken against is
//! [`super::MAX_TAGS_PER_ENTITY`].

#[allow(unused_imports)]
use super::*;

/// What IAM answers a request whose tag list names one key more than once with.
///
/// Entirely fixed text, as the recorded refusal carries it: the sentence states the class of
/// malformed list, follows it with the note AWS adds about how it reads a tag key, and
/// quotes neither the repeated key nor either of the values back — so a client matching it
/// reads the same sentence whichever operation it supplied the list to. The comparison this
/// service makes keys by is stated on [`has_duplicate_tag_key`].
pub const DUPLICATE_TAG_KEY_MESSAGE: &str =
    "Duplicate tag keys found. Please note that Tag keys are case insensitive.";

/// What IAM answers a tag call that would carry a resource past the per-resource tag quota
/// with — the `LimitExceededException` of every `Tag*` operation.
///
/// Entirely fixed text: the sentence names neither the resource, nor its type, nor the
/// quota's figure, because what it reports is that the limit on the resource's tag set has
/// been reached. Held here rather than built at each call site so one sentence answers the
/// class whichever resource's tags the request was adding to, the same way the reserved-key
/// sentence ([`super::RESERVED_TAG_KEY_MESSAGE`]) does.
pub const TAG_LIMIT_EXCEEDED_MESSAGE: &str = "The number of tags has reached the maximum limit.";

/// Whether `keys` names one key more than once.
///
/// A tag is a key and ONE value ("a tag consists of a key name and an associated value",
/// `TagRole`), so a list naming one key twice carries two values for one tag and does not
/// say which of them the tag is to take: the request is ambiguous rather than merely
/// redundant, which is why it is refused outright instead of settled by an order of
/// evaluation. Keys compare exactly, as "tag keys and values are case sensitive" ("Tagging
/// IAM resources", IAM User Guide) makes two spellings of one word two keys — the same
/// comparison [`super::merge_tags`] overwrites a held tag by.
///
/// A predicate rather than a lookup, for the reason [`super::has_reserved_tag_key`] is one:
/// the refusal is the one fixed sentence [`DUPLICATE_TAG_KEY_MESSAGE`], which carries nothing
/// of the request. That sentence closes with AWS's own note about reading a tag key, which
/// is reproduced because it is the text AWS answers with and not because it states the
/// comparison made here: what two spellings of one word are is settled by the stored tags,
/// where "tag keys and values are case sensitive" makes them two tags ([`super::merge_tags`]
/// overwrites by the same comparison, and every tag suite of this crate pins that), so the
/// list is read by it too.
pub fn has_duplicate_tag_key<'a, I: IntoIterator<Item = &'a str>>(keys: I) -> bool {
    let mut seen: ::std::collections::BTreeSet<&str> = ::std::collections::BTreeSet::new();
    keys.into_iter().any(|key| !seen.insert(key))
}

/// The sentence IAM refuses a supplied tag list with, or `None` when the list is one the
/// service takes.
///
/// The ONE entry point every operation taking a `Tags` list validates it through, so the
/// refusals of that class are stated once and cannot drift apart between operations. Every
/// refusal in it is an "invalid or out-of-range value … supplied for an input parameter",
/// which is `InvalidInputException` (HTTP 400) in each of those operations' modelled error
/// sets, and each is decided before any state is read or written.
pub fn supplied_tag_refusal<'a, I: IntoIterator<Item = &'a str>>(keys: I) -> Option<&'static str> {
    let keys: Vec<&str> = keys.into_iter().collect();
    if has_reserved_tag_key(keys.iter().copied()) {
        return Some(RESERVED_TAG_KEY_MESSAGE);
    }
    if has_duplicate_tag_key(keys.iter().copied()) {
        return Some(DUPLICATE_TAG_KEY_MESSAGE);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list naming distinct keys is taken, whatever the spelling or the case.
    #[test]
    fn distinct_keys_are_admissible() {
        for keys in [
            vec![],
            vec!["env"],
            vec!["env", "team", "owner"],
            // "Tag keys and values are case sensitive", so these are three keys.
            vec!["Env", "env", "ENV"],
            vec!["Cost Center_1.2:3/4=5+6-7@8", "Ünité"],
        ] {
            assert!(
                supplied_tag_refusal(keys.iter().copied()).is_none(),
                "case {keys:?}: expected an admissible list, got {:?}",
                supplied_tag_refusal(keys.iter().copied())
            );
        }
    }

    /// A list naming one key twice is refused, wherever the repetition sits in it.
    #[test]
    fn a_repeated_key_is_refused() {
        for keys in [
            vec!["env", "env"],
            vec!["env", "team", "env"],
            vec!["team", "env", "env"],
            vec!["env", "env", "env"],
        ] {
            assert_eq!(
                supplied_tag_refusal(keys.iter().copied()),
                Some(DUPLICATE_TAG_KEY_MESSAGE),
                "case {keys:?}: expected the duplicate-key refusal"
            );
        }
    }

    /// A reserved key is refused with its own sentence, which the duplicate rule does not
    /// displace.
    #[test]
    fn a_reserved_key_keeps_its_own_sentence() {
        for keys in [vec!["aws:cost"], vec!["env", "AWS:cost", "env"]] {
            assert_eq!(
                supplied_tag_refusal(keys.iter().copied()),
                Some(RESERVED_TAG_KEY_MESSAGE),
                "case {keys:?}: expected the reserved-prefix refusal"
            );
        }
    }
}
