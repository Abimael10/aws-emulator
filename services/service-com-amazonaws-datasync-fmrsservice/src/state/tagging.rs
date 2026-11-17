// SPDX-License-Identifier: CC0-1.0

//! The tags this service's resources carry, and how a tagging call reaches them.
//!
//! A tag is a key-value pair held BY a resource, so there is no tag store of its own:
//! every taggable resource this service holds already carries the list a create stored
//! ([`super::agents::Agent::tags`], [`super::Location::tags`], [`super::Task::tags`],
//! [`super::TaskExecution::tags`]), and `TagResource`, `UntagResource` and
//! `ListTagsForResource` all write and read that
//! one list. This module owns the three things those operations have to agree on: which
//! resource an ARN addresses, how a key already held is replaced rather than repeated,
//! and how an ARN that resolves to no resource of this account is refused.
//!
//! The kinds of resource an ARN may address are the alternatives of the model's
//! `TaggableResourceArn` pattern: an agent, a transfer task, a task execution nested
//! under its task, a transfer location, a storage system and a discovery job nested
//! under its system. This service stores agents, locations, tasks and task executions, so
//! an ARN of any other kind resolves to nothing — the service holds a resource only
//! because a create stored it.

use crate::ops::types::{InvalidRequestException, TagListEntry};

use super::{
    ComAmazonawsDatasyncFmrsserviceState, LOCATION_NOT_FOUND_CODE, SYNC_SET_NOT_FOUND_CODE,
};

/// A RESOURCE GUARD on one resource's tag list, and the published ceiling the modelled
/// shapes agree with: `InputTagList` admits at most 50 entries in one `TagResource`
/// call, and `OutputTagList` at most 55, so a resource whose list grew past this would
/// eventually hold more tags than `ListTagsForResource` can report. Each call may bring
/// 50 keys of the caller's choosing and nothing expires a tag, so the list needs a
/// ceiling of its own: a call that would take one past it is refused with the modelled
/// client error and nothing is written.
pub const MAX_TAGS_PER_RESOURCE: usize = 50;

/// The kind of resource an ARN of this service addresses, read off the ARN's resource
/// part — the alternatives of the modelled `TaggableResourceArn` pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaggableKind {
    Agent,
    Location,
    Task,
    TaskExecution,
    StorageSystem,
    DiscoveryJob,
}

impl TaggableKind {
    /// The kind `arn` addresses.
    ///
    /// The resource part is the sixth colon-delimited field of an ARN
    /// (`arn:<partition>:datasync:<region>:<account>:<resource>`); a nested resource —
    /// an execution under its task, a discovery job under its system — names its parent
    /// first, so the nesting segment is what tells the two apart.
    pub fn of(arn: &str) -> Self {
        let resource = arn.splitn(6, ':').nth(5).unwrap_or(arn);
        if resource.starts_with("location/") {
            Self::Location
        } else if resource.starts_with("task/") {
            if resource.contains("/execution/") {
                Self::TaskExecution
            } else {
                Self::Task
            }
        } else if resource.starts_with("system/") {
            if resource.contains("/job/") {
                Self::DiscoveryJob
            } else {
                Self::StorageSystem
            }
        } else {
            Self::Agent
        }
    }

    /// How a refusal names this kind of resource.
    fn described(self) -> &'static str {
        match self {
            Self::Agent => "Agent",
            Self::Location => "Location",
            Self::Task => "Task",
            Self::TaskExecution => "Task execution",
            Self::StorageSystem => "Storage system",
            Self::DiscoveryJob => "Discovery job",
        }
    }

    /// The class discriminator a refusal naming a resource of this kind the account does
    /// not hold carries, where this service has one for the kind.
    fn not_found_code(self) -> Option<&'static str> {
        match self {
            Self::Location => Some(LOCATION_NOT_FOUND_CODE),
            Self::Task => Some(SYNC_SET_NOT_FOUND_CODE),
            _ => None,
        }
    }
}

/// Why a tagging call against this service's state was refused.
///
/// One refusal type for all three tagging operations, so a read, an apply and a removal
/// word and classify the same failure the same way.
#[derive(Debug, Clone)]
pub struct TagRefusal {
    pub message: String,
    /// The value BOTH `errorCode` and `datasyncErrorCode` carry, when the refusal class
    /// has a published discriminator.
    pub error_code: Option<String>,
}

impl TagRefusal {
    /// The refusal a `ResourceArn` that names no resource this account holds earns.
    ///
    /// The tagging surface addresses ANY kind of this service's resources, by the ARN
    /// the request carried, so that is what the message names — and the class is the one
    /// the kind of resource the ARN addresses has.
    pub fn not_found(kind: TaggableKind, arn: &str) -> Self {
        Self {
            message: format!(
                "{described} {arn} is not found.",
                described = kind.described()
            ),
            error_code: kind.not_found_code().map(str::to_string),
        }
    }

    /// The refusal a call that would take one resource's tag list past its ceiling
    /// earns.
    pub fn at_capacity() -> Self {
        Self {
            message: format!(
                "This resource already holds the maximum of {MAX_TAGS_PER_RESOURCE} tags."
            ),
            error_code: None,
        }
    }

    /// This refusal as the modelled client error every tagging operation reports it
    /// through: one discriminator fills both code members.
    pub fn into_invalid_request(self) -> InvalidRequestException {
        InvalidRequestException {
            message: Some(self.message.clone()),
            error_code: self.error_code.clone(),
            datasync_error_code: self.error_code,
        }
    }
}

impl ComAmazonawsDatasyncFmrsserviceState {
    /// The tags the resource `arn` names holds, in the order they were applied — what
    /// `ListTagsForResource` pages over.
    pub fn resource_tags(&self, arn: &str) -> Result<&[TagListEntry], TagRefusal> {
        let kind = TaggableKind::of(arn);
        let held = match kind {
            TaggableKind::Agent => self.agents.get(arn).map(|agent| &agent.tags),
            TaggableKind::Location => self.locations.get(arn).map(|location| &location.tags),
            TaggableKind::Task => self.tasks.get(arn).map(|task| &task.tags),
            TaggableKind::TaskExecution => self
                .task_executions
                .get(arn)
                .map(|execution| &execution.tags),
            // No store holds a resource of these kinds, so no ARN of one resolves.
            TaggableKind::StorageSystem | TaggableKind::DiscoveryJob => None,
        };
        held.map(Vec::as_slice)
            .ok_or_else(|| TagRefusal::not_found(kind, arn))
    }

    /// Those same tags, borrowed for a tagging call to write — refused in the same words
    /// as the reading lookup, so an apply, a removal and a read agree on which ARNs
    /// resolve.
    pub fn resource_tags_mut(&mut self, arn: &str) -> Result<&mut Vec<TagListEntry>, TagRefusal> {
        let kind = TaggableKind::of(arn);
        let held = match kind {
            TaggableKind::Agent => self.agents.get_mut(arn).map(|agent| &mut agent.tags),
            TaggableKind::Location => self
                .locations
                .get_mut(arn)
                .map(|location| &mut location.tags),
            TaggableKind::Task => self.tasks.get_mut(arn).map(|task| &mut task.tags),
            TaggableKind::TaskExecution => self
                .task_executions
                .get_mut(arn)
                .map(|execution| &mut execution.tags),
            TaggableKind::StorageSystem | TaggableKind::DiscoveryJob => None,
        };
        held.ok_or_else(|| TagRefusal::not_found(kind, arn))
    }
}

/// Apply `requested` to the tags a resource already holds, as `TagResource` does.
///
/// A tag is identified by its KEY, so a key the resource already carries has its value
/// replaced where it already sits rather than appearing twice — which is also why a
/// request repeating a key applies the last value it gave for it. Only a key the
/// resource does not yet hold grows the list, so re-applying the tags a resource already
/// has cannot take it past its ceiling; a call that would is refused and NOTHING is
/// applied.
pub fn apply_tags(
    held: &mut Vec<TagListEntry>,
    requested: &[TagListEntry],
) -> Result<(), TagRefusal> {
    let mut applied = held.clone();
    for tag in requested {
        match applied.iter_mut().find(|existing| existing.key == tag.key) {
            Some(existing) => existing.value = tag.value.clone(),
            None => {
                if applied.len() >= MAX_TAGS_PER_RESOURCE {
                    return Err(TagRefusal::at_capacity());
                }
                applied.push(tag.clone());
            }
        }
    }
    *held = applied;
    Ok(())
}

/// Remove the tags whose keys `keys` names, as `UntagResource` does.
///
/// A key the resource does not carry removes nothing: the request asks for a state the
/// resource is already in, so it is not a failure.
pub fn remove_tags(held: &mut Vec<TagListEntry>, keys: &[String]) {
    held.retain(|tag| !keys.contains(&tag.key));
}
