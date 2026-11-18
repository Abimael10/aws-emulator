// SPDX-License-Identifier: CC0-1.0

//! The tag-key NAMESPACE a caller may not write into, and how a request that names one is
//! refused.
//!
//! A tag key opening with `aws:` belongs to AWS rather than to the account: such a key is
//! applied by a service, never by a caller, so a request that asks to create or change one is
//! refused and the resource's tag set is left exactly as it was. The rule is a property of
//! the KEY, not of the operation, so it holds on every way into a tag set this service
//! publishes — the two writes of a stored tag set (`AddTagsToStream`, `TagResource`) and the
//! creates that carry a `Tags` map of their own (`CreateStream`, `CreateChannel`,
//! `RegisterStreamConsumer`) alike — and it is taken BEFORE anything is stored, so a refused
//! request writes nothing.
//!
//! The refusal is the modelled `InvalidArgumentException` ("A specified parameter exceeds its
//! restrictions, is not supported, or can't be used"), which every one of those operations
//! declares; the service's `TagKey` carries no pattern excluding the prefix
//! (`smithy_query(query="shape",shape="TagKey")` is `@length 1..128` only), so this is the
//! service's own tag rule rather than a request-shape constraint.

/// The prefix of a tag key AWS reserves for itself. A key that OPENS with it is a system tag,
/// whichever case the rest of the key is in.
pub const RESERVED_TAG_KEY_PREFIX: &str = "aws:";

/// How a request naming a reserved key is refused. FIXED text: the sentence names the
/// namespace at fault and nothing of the request, so the offending keys are not quoted back —
/// one request may carry several of them and the refusal is one sentence either way.
pub const RESERVED_TAG_KEYS_MESSAGE: &str =
    "Some tag keys in the request started with 'aws:'. System tags cannot be modified.";

/// Whether `key` names a tag AWS reserves for itself.
pub fn is_reserved_tag_key(key: &str) -> bool {
    key.starts_with(RESERVED_TAG_KEY_PREFIX)
}

/// Whether any key of `keys` names a reserved tag — what a tag-writing operation asks before
/// it stores anything.
pub fn names_reserved_tag_key<'keys>(keys: impl IntoIterator<Item = &'keys str>) -> bool {
    keys.into_iter().any(is_reserved_tag_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reserved_namespace_is_the_prefix_of_the_key() {
        // The rows differ along WHERE the prefix sits in the key, its case, and what follows
        // it: only a key that OPENS with the reserved prefix is a system tag.
        let cases = [
            ("the bare prefix", "aws:", true),
            ("a reserved key", "aws:cloudformation:stack-name", true),
            ("a reserved key with one more segment", "aws:x", true),
            ("the prefix in the middle", "team:aws:owner", false),
            ("the prefix at the end", "owner-aws:", false),
            ("the prefix without its colon", "awsome", false),
            ("another case of the prefix", "AWS:reserved", false),
            ("an ordinary key", "Environment", false),
            ("an empty key", "", false),
        ];
        for (case, key, expected) in cases {
            let actual = is_reserved_tag_key(key);
            assert_eq!(
                actual, expected,
                "case {case:?}: expected is_reserved_tag_key({key:?}) == {expected}, got {actual}"
            );
            let in_a_set = names_reserved_tag_key(["Environment", key, "Owner"]);
            assert_eq!(
                in_a_set, expected,
                "case {case:?}: expected a set carrying {key:?} beside ordinary keys to be \
                 {expected}, got {in_a_set}"
            );
        }
    }

    #[test]
    fn a_set_of_ordinary_keys_names_no_reserved_tag() {
        let keys = ["Environment", "Owner", "cost-center"];
        assert!(
            !names_reserved_tag_key(keys),
            "case \"ordinary keys\": expected {keys:?} to name no reserved tag"
        );
        assert!(
            !names_reserved_tag_key(::std::iter::empty()),
            "case \"no keys\": expected an empty set to name no reserved tag"
        );
    }
}
