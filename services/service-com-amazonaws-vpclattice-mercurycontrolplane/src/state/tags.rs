// SPDX-License-Identifier: CC0-1.0

//! The TAGS of a resource: which resource an ARN names, where that resource's tags are
//! kept, and what a tag write does to them.
//!
//! Tags are not a resource of their own: they are the key/value map carried BY the
//! resource the `resourceArn` of a tag operation names ("Adds the specified tags to the
//! specified resource", `TagResource`'s own documentation), so they live on that
//! resource's record in the one store it is read from and go when it goes. Every create
//! of this service models a `tags` member and persists it there, so the tag operations
//! read and write exactly what a create stored and what every other read reports.
//!
//! `Arn`, the shape all three tag operations take, constrains only the partition, the
//! service, the region and the account
//! (`smithy_query(query="shape",shape="Arn")`) — it names no kind — so an ARN is resolved
//! against every kind of this service ([`tag_target`]).

use std::collections::HashMap;

use super::{
    check_tag_total, id_from_identifier, ComAmazonawsVpclatticeMercurycontrolplaneState,
    FieldViolation, ResourceKind,
};

/// The kinds a tag operation's `resourceArn` can name: every resource kind of this
/// service.
///
/// Each one models a `tags` member on its create (`CreateServiceNetwork`,
/// `CreateService`, both association creates, `CreateListener`, `CreateRule`,
/// `CreateTargetGroup`, `CreateResourceConfiguration`), so each one carries tags and an
/// ARN of any of them is a tag operation's target.
pub const TAGGABLE_KINDS: [ResourceKind; ResourceKind::COUNT] = [
    ResourceKind::ServiceNetwork,
    ResourceKind::Service,
    ResourceKind::ServiceAssociation,
    ResourceKind::VpcAssociation,
    ResourceKind::Listener,
    ResourceKind::Rule,
    ResourceKind::TargetGroup,
    ResourceKind::ResourceConfiguration,
];

/// How a tag operation refuses a `resourceArn` that names no resource it can tag.
///
/// The tag operations answer this refusal with the error code `NotFoundException` and HTTP
/// 404 ([`crate::scaffolded::Scaffolded::not_found`]), NOT with the
/// `ResourceNotFoundException` their model declares and the other operations of this
/// service build over the kind and the identifier
/// ([`super::resource_not_found`]) — and the sentence carries nothing from the request,
/// not even the ARN. SOURCE: recorded AWS behaviour for a tag call whose ARN names no
/// resource. It is the one refusal for every ARN the operations cannot resolve, whether
/// its resource part names a kind of this service or not.
pub const TAG_TARGET_NOT_FOUND_MESSAGE: &str = "Resource was not found";

/// The kind and id a tag operation's `resourceArn` names, or `None` when it names no
/// resource of this service.
///
/// The LAST path segment is the id and the segment before it is the kind, so the nested
/// ARNs resolve by the same reading as the top-level ones: a listener's
/// `…:service/svc-…/listener/listener-…` and a rule's
/// `…:service/svc-…/listener/listener-…/rule/rule-…` name a listener and a rule, not the
/// service they nest under. Both halves have to agree — the kind segment and the id's own
/// prefix — and the id is read by the shared reader ([`id_from_identifier`]), so an ARN of
/// any partition, region or account resolves to the resource it names.
pub fn tag_target(resource_arn: &str) -> Option<(ResourceKind, String)> {
    let (head, tail) = resource_arn.rsplit_once('/')?;
    // The kind sits in the segment before the id, which is separated from what precedes
    // it by `/` in a nested ARN and by `:` in a top-level one.
    let arn_kind = head.rsplit(['/', ':']).next()?;
    TAGGABLE_KINDS.into_iter().find_map(|kind| {
        (arn_kind == kind.arn_kind())
            .then(|| id_from_identifier(tail, kind.id_prefix()))
            .flatten()
            .map(|id| (kind, id))
    })
}

impl ComAmazonawsVpclatticeMercurycontrolplaneState {
    /// The tags the resource `kind`/`id` carries, or `None` when this store holds no such
    /// resource.
    ///
    /// The one place a kind maps to where its tags are kept, so a tag read reports the map
    /// a create persisted and a tag write changes that same map — there is no second copy
    /// of a resource's tags. A kind no implemented create mints yet holds no resource, so
    /// nothing can name one.
    pub fn tags_of(&self, kind: ResourceKind, id: &str) -> Option<&HashMap<String, String>> {
        match kind {
            ResourceKind::ServiceNetwork => self.service_networks.get(id).map(|held| &held.tags),
            ResourceKind::Service => self.services.get(id).map(|held| &held.tags),
            ResourceKind::ServiceAssociation => {
                self.service_associations.get(id).map(|held| &held.tags)
            }
            ResourceKind::VpcAssociation => self.vpc_associations.get(id).map(|held| &held.tags),
            ResourceKind::Listener => self.listeners.get(id).map(|held| &held.tags),
            ResourceKind::Rule => self.rules.get(id).map(|held| &held.tags),
            ResourceKind::TargetGroup => self.target_groups.get(id).map(|held| &held.tags),
            // `CreateResourceConfiguration` is still a 501, so no resource configuration
            // can be stored for one to carry tags.
            ResourceKind::ResourceConfiguration => None,
        }
    }

    /// The same map, to write: `None` when this store holds no resource `kind`/`id`.
    fn tags_of_mut(
        &mut self,
        kind: ResourceKind,
        id: &str,
    ) -> Option<&mut HashMap<String, String>> {
        match kind {
            ResourceKind::ServiceNetwork => {
                self.service_networks.get_mut(id).map(|held| &mut held.tags)
            }
            ResourceKind::Service => self.services.get_mut(id).map(|held| &mut held.tags),
            ResourceKind::ServiceAssociation => self
                .service_associations
                .get_mut(id)
                .map(|held| &mut held.tags),
            ResourceKind::VpcAssociation => {
                self.vpc_associations.get_mut(id).map(|held| &mut held.tags)
            }
            ResourceKind::Listener => self.listeners.get_mut(id).map(|held| &mut held.tags),
            ResourceKind::Rule => self.rules.get_mut(id).map(|held| &mut held.tags),
            ResourceKind::TargetGroup => self.target_groups.get_mut(id).map(|held| &mut held.tags),
            ResourceKind::ResourceConfiguration => None,
        }
    }

    /// ADD `tags` to the resource `kind`/`id`: `None` when this store holds no such
    /// resource, `Some(Err(..))` when the write would take it past the modelled `TagMap`
    /// ceiling and nothing is written, `Some(Ok(()))` when the resource carries them.
    ///
    /// A tag write is a MERGE, not a replace: "Adds the specified tags to the specified
    /// resource", so a key the request sends takes its new value — tag keys are
    /// case-sensitive (`TagKey`'s own documentation), so only the exact key is replaced —
    /// and a key the request does not mention keeps the value it had. The resource's own
    /// `lastUpdatedAt` is left where it is: a tag is carried BY the resource, and nothing
    /// about the resource itself changed.
    ///
    /// The ceiling is the `@length` the model puts on `TagMap`
    /// ([`super::MAX_TAGS`]), which bounds `ListTagsForResource`'s output as well as one
    /// request's input, so a resource cannot be made to hold more tags than a read of it
    /// can report — and no sequence of tag calls can grow this store without bound.
    pub fn add_tags(
        &mut self,
        kind: ResourceKind,
        id: &str,
        tags: &HashMap<String, String>,
    ) -> Option<Result<(), FieldViolation>> {
        let held = self.tags_of_mut(kind, id)?;
        let fresh = tags.keys().filter(|key| !held.contains_key(*key)).count();
        if let Err(violation) = check_tag_total(held.len() + fresh) {
            return Some(Err(violation));
        }
        for (key, value) in tags {
            held.insert(key.clone(), value.clone());
        }
        Some(Ok(()))
    }

    /// REMOVE the tags `keys` names from the resource `kind`/`id`, answering whether this
    /// store holds such a resource.
    ///
    /// "Removes the specified tags from the specified resource" (`UntagResource`'s own
    /// documentation): a key the resource does not carry removes nothing, which is what
    /// makes the `@idempotent` operation repeatable, and a key it does carry is gone for
    /// every later read. Only the keys named are touched.
    pub fn remove_tags(&mut self, kind: ResourceKind, id: &str, keys: &[String]) -> bool {
        let Some(held) = self.tags_of_mut(kind, id) else {
            return false;
        };
        for key in keys {
            held.remove(key);
        }
        true
    }
}
