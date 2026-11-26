// SPDX-License-Identifier: CC0-1.0

//! The validation every IAM operation applies to the TAG LIST a request supplies, in one
//! place.
//!
//! One concern, held once: "if any one of the tags is invalid … then the entire request
//! fails and the resource is not created" (`TagPolicy`), so the whole list is judged before
//! anything is read or written, and every operation that takes a `Tags` list — the `Tag*`
//! calls and the `Create*`/`Upload*` calls that attach tags as they mint a resource — judges
//! it the same way. The rules the list has to satisfy
//! beyond the member constraints the model states (`tagKeyType`, `tagValueType`, the list's
//! own 0..50 bound) are:
//!
//! * no key begins with the prefix AWS reserves ([`super::RESERVED_TAG_KEY_PREFIX`]);
//! * no key is named twice, because such a request does not say which of the two values the
//!   tag is to carry ([`has_duplicate_tag_key`]).
//!
//! WHICH list is refused is therefore one rule for every operation. The SENTENCE the
//! repeated-key refusal is worded with is not: it belongs to the operation that took the
//! list, so each caller names the kind of tag list it supplied ([`TagListTarget`]) and the
//! sentence follows from that.
//!
//! What a resource ends up holding once the list is admissible is [`super::merge_tags`]'s
//! rule, and the per-resource quota that merge is taken against is
//! [`super::MAX_TAGS_PER_ENTITY`].

#[allow(unused_imports)]
use super::*;

/// What IAM answers a request supplying one of a RESOURCE's tag lists that names one key
/// more than once with.
///
/// Entirely fixed text, as the recorded refusal carries it: the one sentence states the class
/// of malformed list and quotes neither the repeated key, nor either of the values, nor the
/// resource addressed, back. The comparison this service makes keys by is stated on
/// [`has_duplicate_tag_key`]; which operations word the refusal this way is
/// [`TagListTarget`].
pub const DUPLICATE_TAG_KEY_MESSAGE: &str = "Duplicate tag keys found.";

/// What IAM answers the same malformed list with when it was supplied for a PRINCIPAL's tags
/// — a user's or a role's.
///
/// Entirely fixed text too, and the class sentence is the same one: what IAM adds for these
/// lists is its note about how it reads a tag key, and nothing of the request appears in
/// either half. The two wordings are two requirements, one per kind of tag list
/// ([`TagListTarget`]).
pub const PRINCIPAL_DUPLICATE_TAG_KEY_MESSAGE: &str =
    "Duplicate tag keys found. Please note that Tag keys are case insensitive.";

/// WHOSE tags a supplied list is for, which is what the repeated-key refusal's wording
/// follows from.
///
/// IAM has tagged its principals — users and roles — since it introduced tagging, and tags
/// every other kind of resource (policies, instance profiles, MFA devices, identity
/// providers, server certificates) through the surface it added later; the refusal a
/// repeated key answers with is worded by the operation, and the two surfaces word it
/// differently. So an operation taking a `Tags` list says which kind of list it took and
/// never the sentence itself, leaving the wording of each class stated once here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagListTarget {
    /// A list supplied for a user's or a role's tags, whether by a `Tag*` call or by the
    /// `Create*` call that mints the principal.
    Principal,
    /// A list supplied for any other IAM resource's tags.
    Resource,
}

impl TagListTarget {
    /// The sentence IAM refuses a list of this kind that names one key twice with.
    pub fn duplicate_tag_key_message(self) -> &'static str {
        match self {
            Self::Principal => PRINCIPAL_DUPLICATE_TAG_KEY_MESSAGE,
            Self::Resource => DUPLICATE_TAG_KEY_MESSAGE,
        }
    }
}

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
/// the refusal is fixed text ([`TagListTarget::duplicate_tag_key_message`]), which carries
/// nothing of the request. The sentence a principal's list is refused with closes with AWS's
/// own note about reading a tag key, which is reproduced because it is the text AWS answers
/// with and not because it states the comparison made here: what two spellings of one word
/// are is settled by the stored tags, where "tag keys and values are case sensitive" makes
/// them two tags ([`super::merge_tags`] overwrites by the same comparison, and every tag
/// suite of this crate pins that), so the list is read by it too.
pub fn has_duplicate_tag_key<'a, I: IntoIterator<Item = &'a str>>(keys: I) -> bool {
    let mut seen: ::std::collections::BTreeSet<&str> = ::std::collections::BTreeSet::new();
    keys.into_iter().any(|key| !seen.insert(key))
}

/// The sentence IAM refuses a supplied tag list with, or `None` when the list is one the
/// service takes.
///
/// The ONE entry point every operation taking a `Tags` list validates it through, so WHICH
/// lists are refused is stated once and cannot drift apart between operations. Every refusal
/// in it is an "invalid or out-of-range value … supplied for an input parameter", which is
/// `InvalidInputException` (HTTP 400) in each of those operations' modelled error sets, and
/// each is decided before any state is read or written.
///
/// `target` is whose tags the caller supplied the list for: the reserved-prefix refusal is
/// one sentence for every list, while the repeated-key refusal is worded per kind of list
/// ([`TagListTarget`]).
pub fn supplied_tag_refusal<'a, I: IntoIterator<Item = &'a str>>(
    target: TagListTarget,
    keys: I,
) -> Option<&'static str> {
    let keys: Vec<&str> = keys.into_iter().collect();
    if has_reserved_tag_key(keys.iter().copied()) {
        return Some(RESERVED_TAG_KEY_MESSAGE);
    }
    if has_duplicate_tag_key(keys.iter().copied()) {
        return Some(target.duplicate_tag_key_message());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A list naming distinct keys is taken, whatever the spelling or the case, whosever
    /// tags it was supplied for.
    #[test]
    fn distinct_keys_are_admissible() {
        for target in [TagListTarget::Principal, TagListTarget::Resource] {
            for keys in [
                vec![],
                vec!["env"],
                vec!["env", "team", "owner"],
                // "Tag keys and values are case sensitive", so these are three keys.
                vec!["Env", "env", "ENV"],
                vec!["Cost Center_1.2:3/4=5+6-7@8", "Ünité"],
            ] {
                assert!(
                    supplied_tag_refusal(target, keys.iter().copied()).is_none(),
                    "case {target:?} {keys:?}: expected an admissible list, got {:?}",
                    supplied_tag_refusal(target, keys.iter().copied())
                );
            }
        }
    }

    /// A list naming one key twice is refused, wherever the repetition sits in it, with the
    /// sentence of the kind of tag list it was supplied for.
    #[test]
    fn a_repeated_key_is_refused() {
        for (target, expected) in [
            (TagListTarget::Principal, PRINCIPAL_DUPLICATE_TAG_KEY_MESSAGE),
            (TagListTarget::Resource, DUPLICATE_TAG_KEY_MESSAGE),
        ] {
            for keys in [
                vec!["env", "env"],
                vec!["env", "team", "env"],
                vec!["team", "env", "env"],
                vec!["env", "env", "env"],
            ] {
                assert_eq!(
                    supplied_tag_refusal(target, keys.iter().copied()),
                    Some(expected),
                    "case {target:?} {keys:?}: expected {expected:?}"
                );
            }
        }
    }

    /// A reserved key is refused with its own sentence, which is one sentence for every kind
    /// of tag list and which the duplicate rule does not displace.
    #[test]
    fn a_reserved_key_keeps_its_own_sentence() {
        for target in [TagListTarget::Principal, TagListTarget::Resource] {
            for keys in [vec!["aws:cost"], vec!["env", "AWS:cost", "env"]] {
                assert_eq!(
                    supplied_tag_refusal(target, keys.iter().copied()),
                    Some(RESERVED_TAG_KEY_MESSAGE),
                    "case {target:?} {keys:?}: expected the reserved-prefix refusal"
                );
            }
        }
    }
}
