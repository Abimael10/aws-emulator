// SPDX-License-Identifier: CC0-1.0

//! Resolving a tagging request's `ResourceArn` to the tag set of the resource it names,
//! and the content rules a submitted tag list has to satisfy before any of it is stored.
//!
//! One resolver for the whole tagging surface — `ListTagsForResource`, `TagResource`
//! and `UntagResource` — so a resource type is reachable through all three or through
//! none of them, and one judge of a submitted tag list for every operation that writes
//! one, so a key Config refuses through one of them is refused through all of them.

use super::*;

/// The tag-key namespace AWS reserves for its own tags, which a caller may not write
/// into: "Do not use the `aws:` prefix in your tag names or values because it is
/// reserved for Amazon Web Services use. You can't edit or delete tag names or values
/// with this prefix."
///
/// Matched against the START of the key and in ANY case, as the restriction is published
/// ("Do not use `aws:`, `AWS:`, or any upper or lower case combination of such as a
/// prefix for either keys or values"): a key that merely begins with these letters
/// without the separator (`awsome`), or carries the prefix further in (`my:aws:thing`),
/// names nothing reserved.
pub const RESERVED_TAG_KEY_PREFIX: &str = "aws:";

/// Whether `key` is a tag key in the namespace AWS reserves for its own tags
/// ([`RESERVED_TAG_KEY_PREFIX`]).
fn is_reserved_tag_key(key: &str) -> bool {
    key.get(..RESERVED_TAG_KEY_PREFIX.len())
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case(RESERVED_TAG_KEY_PREFIX))
}

/// How Config words `ValidationException` for a request carrying a parameter value it
/// will not accept: a FIXED sentence that echoes nothing of the request, so the offending
/// tag key is not quoted back.
pub const INVALID_PARAMETER_VALUE_MESSAGE: &str = "One or more parameter value is invalid";

/// How Config words `ValidationException` for a tag list that names one key more than
/// once. A FIXED sentence as well: it names the list, not the key that was repeated.
pub const DUPLICATE_TAGS_MESSAGE: &str = "Duplicate tags exists in tags list";

/// Why Config would not store the tag list a request submitted, or `None` when it is a
/// list Config stores.
///
/// The two rules are Config's own domain checks on the list AS A WHOLE, which is why
/// neither can be expressed as a constraint on a single member and neither is caught by
/// the boundary walk: a key in the reserved namespace ([`RESERVED_TAG_KEY_PREFIX`]) is
/// the caller writing into tags only AWS may set, and a key the same list carries twice
/// asks for two values for one tag.
///
/// The list is judged in REQUEST ORDER, entry by entry, so the refusal reported is the
/// first rule an entry of the list breaks; an entry carrying no key names no tag and is
/// passed over, exactly as [`tag_set`] drops it. A repeat is a key the list carries
/// twice VERBATIM — a tag key is case-sensitive ("Tag keys and values are case
/// sensitive"), so two keys differing in case are two tags. Judging the list before it is
/// collapsed into a [`TagSet`] is what makes a repeat visible at all — the collapse keeps
/// one value per key — and every caller runs this BEFORE it touches the store, so a
/// refused request leaves the resource's tags exactly as they were.
pub fn tag_list_refusal(tags: &[Tag]) -> Option<&'static str> {
    let mut seen: BTreeSet<&str> = BTreeSet::new();
    for tag in tags {
        let Some(key) = tag.key.as_deref() else {
            continue;
        };
        if is_reserved_tag_key(key) {
            return Some(INVALID_PARAMETER_VALUE_MESSAGE);
        }
        if !seen.insert(key) {
            return Some(DUPLICATE_TAGS_MESSAGE);
        }
    }
    None
}

impl ComAmazonawsConfigserviceStarlingdoveserviceState {
    /// The tags of the resource `arn` identifies, or `None` when this store holds no
    /// resource with that ARN.
    ///
    /// This is the ONE place an ARN is resolved to a tag set, so the tagging
    /// operations agree with each other and with the resource's own read path: the
    /// tags live ON the resource, which is also why deleting the resource deletes its
    /// tags (`TagResource`: "When a resource is deleted, the tags associated with that
    /// resource are deleted as well").
    ///
    /// EVERY taggable Config resource type this emulator persists is reached here —
    /// the types `ListTagsForResourceRequest$ResourceArn` documents (configuration
    /// recorder, Config rule, organization Config rule, conformance pack, organization
    /// conformance pack, configuration aggregator, aggregation authorization, stored
    /// query, connector), minus the ones whose write operation is still scaffolded and
    /// which therefore cannot be in the store at all. An ARN of a type nothing
    /// persists resolves to `None` exactly as the ARN of a deleted resource does, so
    /// the not-found refusal means what it says: no such resource is held here now.
    pub fn tags_of(&self, arn: &str) -> Option<&TagSet> {
        if let Some(recorder) = self.recorders.values().find(|entry| entry.arn == arn) {
            return Some(&recorder.tags);
        }
        if let Some(rule) = self.config_rules.values().find(|entry| entry.arn == arn) {
            return Some(&rule.tags);
        }
        if let Some(aggregator) = self
            .configuration_aggregators
            .values()
            .find(|entry| entry.arn == arn)
        {
            return Some(&aggregator.tags);
        }
        if let Some(authorization) = self
            .aggregation_authorizations
            .values()
            .find(|entry| entry.arn == arn)
        {
            return Some(&authorization.tags);
        }
        if let Some(stored_query) = self.stored_queries.values().find(|entry| entry.arn == arn) {
            return Some(&stored_query.tags);
        }
        if let Some(pack) = self.conformance_packs.values().find(|entry| entry.arn == arn) {
            return Some(&pack.tags);
        }
        if let Some(connector) = self.connectors.values().find(|entry| entry.arn == arn) {
            return Some(&connector.tags);
        }
        None
    }

    /// The tags of the resource `arn` identifies, for a request that changes them.
    /// See [`Self::tags_of`].
    pub fn tags_of_mut(&mut self, arn: &str) -> Option<&mut TagSet> {
        if let Some(recorder) = self.recorders.values_mut().find(|entry| entry.arn == arn) {
            return Some(&mut recorder.tags);
        }
        if let Some(rule) = self.config_rules.values_mut().find(|entry| entry.arn == arn) {
            return Some(&mut rule.tags);
        }
        if let Some(aggregator) = self
            .configuration_aggregators
            .values_mut()
            .find(|entry| entry.arn == arn)
        {
            return Some(&mut aggregator.tags);
        }
        if let Some(authorization) = self
            .aggregation_authorizations
            .values_mut()
            .find(|entry| entry.arn == arn)
        {
            return Some(&mut authorization.tags);
        }
        if let Some(stored_query) = self
            .stored_queries
            .values_mut()
            .find(|entry| entry.arn == arn)
        {
            return Some(&mut stored_query.tags);
        }
        if let Some(pack) = self
            .conformance_packs
            .values_mut()
            .find(|entry| entry.arn == arn)
        {
            return Some(&mut pack.tags);
        }
        if let Some(connector) = self.connectors.values_mut().find(|entry| entry.arn == arn) {
            return Some(&mut connector.tags);
        }
        None
    }
}
