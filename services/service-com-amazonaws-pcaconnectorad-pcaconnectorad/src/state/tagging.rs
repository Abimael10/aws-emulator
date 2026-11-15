// SPDX-License-Identifier: CC0-1.0

//! The tag map of a resource addressed by ARN, which is what the three tagging endpoints
//! read and write.
//!
//! `/tags/{ResourceArn}` addresses a resource by the ARN the request carries rather than by
//! a typed label, so ONE lookup has to resolve that ARN against every resource of this
//! service that holds tags — a connector, a template, a directory registration — and hand
//! back the very map the other tagging endpoints use. Keeping it here means a removal and
//! a read cannot land on two different maps, and the record's own change timestamp travels
//! with the map, because changing a resource's tags changes the resource.
//!
//! A service principal name holds no tags: it has no ARN of its own (the modelled
//! operations address one by the registration-and-connector pair it was created under), so
//! no `ResourceArn` can name one.

use std::collections::HashMap;
use std::time::SystemTime;

use super::ComAmazonawsPcaconnectoradPcaconnectoradState;

/// Whether `value` is an ARN at all — the only thing the resource-tagging layer in front of
/// this service parses a `ResourceArn` for before it resolves anything.
///
/// The parse is the tree's ONE reading of an ARN (`::service_common::arn::parse`, which
/// requires the six colon-separated fields, the literal `arn` first, and a non-empty
/// partition, vendor and resource, with the resource keeping any further colons), so this
/// service cannot disagree with another about what an ARN is.
///
/// Nothing here is about THIS service: a value that parses names some resource somewhere,
/// so a call naming it is a lookup that may find nothing, and a value that does not parse
/// is a request the layer refuses before any service sees it.
#[must_use]
pub fn parses_as_arn(value: &str) -> bool {
    ::service_common::arn::parse(value).is_some()
}

/// The tag map of one resource, taken mutably, with the record's own change timestamp
/// beside it.
pub struct Taggable<'store> {
    /// The map the tagging endpoints read and write.
    tags: &'store mut HashMap<String, String>,
    /// The `UpdatedAt` the resource reports, bumped only by a call that changed the map.
    updated_at: &'store mut SystemTime,
}

impl Taggable<'_> {
    /// Remove every tag whose key `keys` names and answer how many were actually removed.
    ///
    /// AWS tagging semantics: a tag is identified by its key alone, so each named key takes
    /// its value with it and every other tag the resource carries stays. A key the resource
    /// does not carry removes nothing — which is what makes the modelled idempotence hold —
    /// and a call that dropped nothing leaves the record's `UpdatedAt` where it was.
    pub fn remove_tags<'key>(
        &mut self,
        keys: impl IntoIterator<Item = &'key str>,
        now: SystemTime,
    ) -> usize {
        let removed = keys
            .into_iter()
            .filter(|key| self.tags.remove(*key).is_some())
            .count();
        if removed > 0 {
            *self.updated_at = now;
        }
        removed
    }
}

impl ComAmazonawsPcaconnectoradPcaconnectoradState {
    /// The tags `resource_arn` carries, or `None` when this service holds no resource under
    /// that ARN.
    ///
    /// The ARN is the key of every map it is looked up in, so one value names at most one
    /// resource whichever kind it is.
    pub fn tags_of(&self, resource_arn: &str) -> Option<&HashMap<String, String>> {
        if let Some(connector) = self.connectors.get(resource_arn) {
            return Some(&connector.tags);
        }
        if let Some(template) = self.templates.get(resource_arn) {
            return Some(&template.tags);
        }
        self.directory_registrations
            .get(resource_arn)
            .map(|registration| &registration.tags)
    }

    /// The tag map of `resource_arn` taken mutably, or `None` when this service holds no
    /// resource under that ARN — which is what answers a tagging call naming a resource
    /// that does not exist.
    ///
    /// The SAME map [`Self::tags_of`] reads, so a removal and the read that follows it
    /// cannot disagree.
    pub fn taggable_mut(&mut self, resource_arn: &str) -> Option<Taggable<'_>> {
        if let Some(connector) = self.connectors.get_mut(resource_arn) {
            return Some(Taggable {
                tags: &mut connector.tags,
                updated_at: &mut connector.updated_at,
            });
        }
        if let Some(template) = self.templates.get_mut(resource_arn) {
            return Some(Taggable {
                tags: &mut template.tags,
                updated_at: &mut template.updated_at,
            });
        }
        self.directory_registrations
            .get_mut(resource_arn)
            .map(|registration| Taggable {
                tags: &mut registration.tags,
                updated_at: &mut registration.updated_at,
            })
    }
}
