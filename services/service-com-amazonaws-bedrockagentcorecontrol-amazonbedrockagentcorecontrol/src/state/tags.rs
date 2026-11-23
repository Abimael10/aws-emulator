// SPDX-License-Identifier: CC0-1.0

//! The TAGS a resource of this control plane carries: where the tag plane's three
//! operations read and write them, and the resolution of the ARN a request addresses
//! a resource by.
//!
//! A tag belongs to the resource it is on, so it is not stored in a plane of its own:
//! every taggable family already keeps the `tags` map its own create persisted on its
//! record, and the tag plane reads and writes exactly that map. So a tag written
//! through `TagResource` is the same tag a read of the resource reports, and a tag set
//! disappears with the resource that carried it — "when a resource is deleted, the tags
//! associated with that resource are also deleted"
//! (`smithy_query(query="doc",shape="TagResource")`) — without anything having to sweep
//! a second store.
//!
//! ## What the ARN a request addresses resolves to
//!
//! The resource TYPE the ARN carries decides which of four answers a tag call gets
//! ([`TagTarget`]), on the same rule the resource-policy plane of this service is
//! resolved by ([`ResourcePolicyTarget`]), because the conditions a client branches on
//! have different remedies:
//!
//! * a type whose resources carry tags ([`TAGGABLE_ARN_RESOURCES`]), naming a resource
//!   this service holds — the tags are written, reported or removed;
//! * such a type naming no resource this service holds — HTTP 404 with the code this
//!   plane is served with ([`tag_target_not_found`]): address a resource that exists;
//! * another resource type OF THIS SERVICE ([`AGENTCORE_RESOURCE_TYPES`]) — the
//!   modelled `ValidationException`: that resource takes no tags through this plane, so
//!   the request is what has to change;
//! * an ARN naming no resource type of this service at all — the modelled
//!   `AccessDeniedException`: nothing of this account is addressed, so the call is
//!   denied rather than reported on.
//!
//! A resource whose deletion has already been ACCEPTED resolves to nothing either: the
//! removal is owed by the clock, so a tag written on it would be gone at an instant no
//! caller chose, and the tags it carried are already on their way out with it.

use super::*;
use crate::ops::types::ServiceQuotaExceededException;

/// Every ARN resource type whose resources carry a tag plane.
///
/// The operation's own documentation states the families: the feature "is currently
/// available only for AgentCore Runtime, Browser, Browser Profile, Code Interpreter
/// tool, and Gateway" (`smithy_query(query="doc",shape="TagResource")`), which is
/// exactly the set of this service's records that hold a tag map a create persisted.
/// Each family is addressed under the ARN type its own `*Arn` pattern publishes, and
/// the two sandbox families carry two spellings each — the customer-created one and
/// the one this service provides
/// (`smithy_query(query="shape",shape="BrowserArn,CodeInterpreterArn")`:
/// `browser(-custom)?` and `code-interpreter(-custom)?`).
///
/// The bare spellings are listed as taggable and resolve nothing: the sandbox a
/// service-owned ARN names is published under the service's own account segment, and
/// `TaggableResourcesArn` admits only `[0-9]{12}` there
/// (`smithy_query(query="shape",shape="TaggableResourcesArn")`), so no request this
/// plane can carry names one.
pub const TAGGABLE_ARN_RESOURCES: &[&str] = &[
    POLICY_TARGET_RUNTIME,
    POLICY_TARGET_GATEWAY,
    BROWSER_ARN_RESOURCE,
    SYSTEM_BROWSER_ARN_RESOURCE,
    BROWSER_PROFILE_ARN_RESOURCE,
    CODE_INTERPRETER_ARN_RESOURCE,
    SYSTEM_CODE_INTERPRETER_ARN_RESOURCE,
];

/// How many tags one resource holds at most: `TagsMap`'s own ceiling, which the
/// request member carrying a whole tag set is bounded by
/// (`smithy_query(query="shape",shape="TagsMap")` — `len 0..50`).
///
/// It bounds the SET A RESOURCE CARRIES, not one request: a caller tagging in several
/// calls meets the modelled quota rather than growing a resource's tag set past what
/// `ListTagsForResource` can report.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// What the ARN a tag request addressed resolves to — the one classification every
/// tag operation branches on, so none of them answers an ARN differently from the
/// others (see the module docs).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TagTarget {
    /// The resource the ARN names, which this service holds and whose type carries
    /// tags: the key its tag set is reached under (`<arn resource type>/<id>`).
    Resolved(String),
    /// A taggable resource type, naming no resource this service holds in the Region
    /// the request was signed for.
    Absent,
    /// A resource type of this service whose resources this plane does not tag.
    OutOfScope,
    /// An ARN naming no resource type of this service at all.
    Unrecognized,
}

/// The not-found answer the tag plane serves for a `resourceArn` it resolves no
/// resource of this account for: HTTP 404 with the error code `NotFoundException` and
/// the sentence "Resource was not found".
///
/// The code is the one this service's tag plane is SERVED with, which is not a shape
/// its model declares for these operations (they list `ResourceNotFoundException`), so
/// it is answered through the crate's own error channel rather than renamed to a
/// modelled shape: the code is what a client branches on. The sentence has no variable
/// part — the ARN is the caller's own value and the remedy, address a resource that
/// exists, does not depend on it — so one answer covers every ARN this plane resolves
/// nothing for, whichever of the three operations asked. It is NOT the sentence the
/// resource-policy plane of this service answers for an unresolvable ARN
/// ([`resource_arn_not_found`]): the two planes are served by different components and
/// word it differently.
pub fn tag_target_not_found<E>() -> crate::scaffolded::Scaffolded<E> {
    crate::scaffolded::Scaffolded::not_found(TAGGED_RESOURCE_NOT_FOUND)
}

/// How the tag plane words an ARN it resolves no resource for (see
/// [`tag_target_not_found`]).
pub const TAGGED_RESOURCE_NOT_FOUND: &str = "Resource was not found";

/// The modelled validation error for a `resourceArn` that names a resource of this
/// service whose type this plane does not tag.
///
/// It is the member's own refusal ([`field_validation_failed`], the shape this service
/// reports one refused member in), because the remedy is to change the request rather
/// than to create or address anything: no resource of that type takes a tag here,
/// whichever one the ARN named. Shared by every tag operation, so none of them accepts
/// a type another refuses.
pub fn tagging_unsupported_type() -> ValidationException {
    field_validation_failed(
        RESOURCE_ARN_MEMBER,
        "tagging is not supported for this resource type",
    )
}

/// The modelled quota error for a tag write that would leave a resource carrying more
/// tags than [`MAX_TAGS_PER_RESOURCE`].
///
/// `merged` is how many tags the resource would hold once the request's keys were
/// merged into the set it already carries, so the sentence says what the request would
/// have done rather than only what the limit is.
pub fn tag_quota_exceeded(merged: usize) -> ServiceQuotaExceededException {
    ServiceQuotaExceededException {
        message: Some(format!(
            "this request would leave the resource holding {merged} tags, more than the \
             maximum of {MAX_TAGS_PER_RESOURCE}"
        )),
    }
}

impl ComAmazonawsBedrockagentcorecontrolAmazonbedrockagentcorecontrolState {
    /// What `resource_arn` resolves to for the tag plane: the key the resource's tag
    /// set is reached under, or which refusal the ARN earns (see the module docs and
    /// [`TagTarget`]).
    ///
    /// The ARN is resolved rather than parsed loosely: the resource part names the type
    /// and the id, and the whole ARN must be the one this service publishes for that
    /// resource in the Region the request was signed for ([`runtime_arn`] /
    /// [`gateway_arn`] / [`browser_arn`] / [`browser_profile_arn`] /
    /// [`code_interpreter_arn`]), so a partition, account or Region that is not this
    /// emulator's names no resource here.
    pub fn tag_target(&self, region: &str, resource_arn: &str) -> TagTarget {
        let Some((_, resource)) = resource_arn.rsplit_once(':') else {
            return TagTarget::Unrecognized;
        };
        let Some((resource_type, resource_id)) = resource.split_once('/') else {
            return TagTarget::Unrecognized;
        };
        if !TAGGABLE_ARN_RESOURCES.contains(&resource_type) {
            return if AGENTCORE_RESOURCE_TYPES.contains(&resource_type) {
                TagTarget::OutOfScope
            } else {
                TagTarget::Unrecognized
            };
        }
        // A resource the clock already owes a removal is on its way out, so the tag
        // plane resolves it no more than a resource that is gone.
        let published = match resource_type {
            POLICY_TARGET_RUNTIME => self
                .agent_runtimes
                .get(resource_id)
                .filter(|record| !record.is_deleting())
                .map(|_| runtime_arn(region, resource_id)),
            POLICY_TARGET_GATEWAY => self
                .gateways
                .get(resource_id)
                .filter(|record| !record.is_deleting())
                .map(|_| gateway_arn(region, resource_id)),
            BROWSER_ARN_RESOURCE => self
                .browsers
                .get(resource_id)
                .map(|_| browser_arn(region, resource_id)),
            BROWSER_PROFILE_ARN_RESOURCE => self
                .browser_profiles
                .get(resource_id)
                .filter(|record| !record.is_deleting())
                .map(|_| browser_profile_arn(region, resource_id)),
            CODE_INTERPRETER_ARN_RESOURCE => self
                .code_interpreters
                .get(resource_id)
                .map(|_| code_interpreter_arn(region, resource_id)),
            // The sandboxes this service provides are published under an ARN this
            // plane cannot carry (see [`TAGGABLE_ARN_RESOURCES`]).
            _ => None,
        };
        match published {
            Some(published) if published == resource_arn => TagTarget::Resolved(resource.to_string()),
            _ => TagTarget::Absent,
        }
    }

    /// The tag set the resource `target` names carries, as the record that resource
    /// lives on holds it.
    pub fn resource_tags(&self, target: &str) -> Option<&BTreeMap<String, String>> {
        let (resource_type, resource_id) = target.split_once('/')?;
        match resource_type {
            POLICY_TARGET_RUNTIME => self.agent_runtimes.get(resource_id).map(|r| &r.tags),
            POLICY_TARGET_GATEWAY => self.gateways.get(resource_id).map(|r| &r.tags),
            BROWSER_ARN_RESOURCE => self.browsers.get(resource_id).map(|r| &r.tags),
            BROWSER_PROFILE_ARN_RESOURCE => self.browser_profiles.get(resource_id).map(|r| &r.tags),
            CODE_INTERPRETER_ARN_RESOURCE => self.code_interpreters.get(resource_id).map(|r| &r.tags),
            _ => None,
        }
    }

    /// The tag set the resource `target` names carries, to write: the one map the
    /// resource's own record holds, so a tag write and every read of that resource see
    /// one tag set.
    pub fn resource_tags_mut(&mut self, target: &str) -> Option<&mut BTreeMap<String, String>> {
        let (resource_type, resource_id) = target.split_once('/')?;
        match resource_type {
            POLICY_TARGET_RUNTIME => self.agent_runtimes.get_mut(resource_id).map(|r| &mut r.tags),
            POLICY_TARGET_GATEWAY => self.gateways.get_mut(resource_id).map(|r| &mut r.tags),
            BROWSER_ARN_RESOURCE => self.browsers.get_mut(resource_id).map(|r| &mut r.tags),
            BROWSER_PROFILE_ARN_RESOURCE => {
                self.browser_profiles.get_mut(resource_id).map(|r| &mut r.tags)
            }
            CODE_INTERPRETER_ARN_RESOURCE => {
                self.code_interpreters.get_mut(resource_id).map(|r| &mut r.tags)
            }
            _ => None,
        }
    }

    /// Materialise every clock-owed removal the tag plane reads across, so a resource
    /// whose removal is already due is gone before a tag call looks for it.
    ///
    /// The removals were owed at this instant whether or not anybody called, which is
    /// why a reporting tag operation performs them too.
    pub fn expire_deleted_taggable_resources(&mut self, now: SystemTime) {
        self.expire_deleted_agent_runtimes(now);
        self.expire_deleted_gateways_and_targets(now);
        self.expire_deleted_browser_profiles(now);
    }
}
