// SPDX-License-Identifier: CC0-1.0

//! The EventBridge envelope a write of a REFERENCE is published as.
//!
//! # The rule this module implements
//!
//! A write CodeCommit accepts that moves a reference is published onto the account's
//! default event bus ([`super::event_bus`]) with no opt-in and no trigger configuration,
//! under the one `detail-type` a repository's own changes are carried by
//! ([`REPOSITORY_STATE_CHANGE_DETAIL_TYPE`]), naming the repository's ARN as the single
//! entry of `resources`. `detail` says WHICH class of write it was
//! ([`EVENT_REFERENCE_CREATED`], [`EVENT_REFERENCE_UPDATED`],
//! [`EVENT_REFERENCE_DELETED`]) — the same three classes a repository trigger names
//! (`smithy_query(query="shape",shape="RepositoryTriggerEventEnum")`) — what kind of
//! reference moved ([`REFERENCE_TYPE_BRANCH`]), the reference both as the BARE name a
//! caller passed and as the fully qualified ref a git client sees, the commit the
//! reference now points at, the commit it pointed at before for a write that moved or
//! removed an existing one, the repository the write was on, and the identity whose call
//! made it. So a build triggered by a push, a mirroring consumer or an audit pipeline
//! observes an accepted write without polling CodeCommit again — and a call this service
//! REFUSES moves no reference, so it publishes nothing, which is what lets a consumer
//! tell a refused write apart from an accepted one.
//!
//! Every value is read off the STORED repository, so what is published is what the next
//! `GetBranch`, `GetCommit` or `GetRepository` of the same repository reports.
//!
//! # Where it is applied
//!
//! At the ONE place a reference moves ([`Repository::set_branch_tip`]) and the one place
//! a reference is removed ([`Repository::remove_branch_tip`]), handed back as part of the
//! [`super::reference_changes::ReferenceChange`] a write answers with, so every face of
//! the service that writes a reference — `PutFile`, `CreateCommit`, `CreateBranch`,
//! `DeleteFile`, `DeleteBranch` and the branch and pull-request merges — publishes the
//! event that write owes, and no new write path can move a reference without publishing
//! it. A write that asks a reference to stay exactly where it already is moves nothing
//! and publishes nothing.
//!
//! The git transport's push ([`crate::service::git_http`]) is the one write path that
//! publishes nothing, for the reason [`super::reference_changes`] gives: the framework
//! hook it is served through is synchronous and is handed no callback to publish over.

use super::*;

use reference_changes::ReferenceEvent;

/// The `detail-type` every change to a repository's own state — its references — is
/// published under: one type for the whole class, with the class of write itself named by
/// `detail.event`.
pub const REPOSITORY_STATE_CHANGE_DETAIL_TYPE: &str = "CodeCommit Repository State Change";

/// `detail.event` values: the name each class of reference write is published under.
///
/// One per variant of [`ReferenceEvent`], which is itself one per reference event
/// `RepositoryTriggerEventEnum` declares, so the class of write a trigger may name and
/// the class a consumer of the bus reads cannot drift apart.
pub const EVENT_REFERENCE_CREATED: &str = "referenceCreated";
pub const EVENT_REFERENCE_UPDATED: &str = "referenceUpdated";
pub const EVENT_REFERENCE_DELETED: &str = "referenceDeleted";

/// `detail.referenceType` of a reference under `refs/heads/`: a branch.
///
/// The only kind of reference the control plane writes — `CreateBranch`, `PutFile`,
/// `DeleteFile`, `CreateCommit`, `DeleteBranch` and the merges all name a `branchName` —
/// so it is the only one published here. A tag is written by the git transport alone,
/// which publishes nothing at all.
pub const REFERENCE_TYPE_BRANCH: &str = "branch";

impl ReferenceEvent {
    /// The `detail.event` a write of this class is published as.
    #[must_use]
    pub fn published_event(self) -> &'static str {
        match self {
            Self::Created => EVENT_REFERENCE_CREATED,
            Self::Updated => EVENT_REFERENCE_UPDATED,
            Self::Deleted => EVENT_REFERENCE_DELETED,
        }
    }
}

impl Repository {
    /// The envelope one write of the reference `branch` publishes.
    ///
    /// `commit_id` is where the reference points after the write — the git object name a
    /// removed reference's update carries ([`git_object::NULL_OBJECT_ID`]) for a removal,
    /// which is what says the reference points at nothing now — and
    /// `previous_commit_id` is where it pointed before, which a write that created the
    /// reference has none of and so leaves out rather than reporting empty. `actor` is
    /// the identity whose call wrote the reference, for a transport that carries one.
    #[must_use = "a write of a reference is published onto the default event bus"]
    pub fn reference_state_change(
        &self,
        branch: &str,
        commit_id: &str,
        previous_commit_id: Option<&str>,
        event: ReferenceEvent,
        actor: Option<&str>,
        at: SystemTime,
    ) -> event_bus::PublishedEvent {
        let mut detail = ::serde_json::Map::new();
        let mut put = |member: &str, value: &str| {
            detail.insert(
                member.to_string(),
                ::serde_json::Value::String(value.to_string()),
            );
        };
        // The git transport authenticates with git credentials rather than a caller's
        // own, so a write it makes names no actor and the member is left out rather than
        // published empty.
        if let Some(actor) = actor {
            put("callerUserArn", actor);
        }
        put("commitId", commit_id);
        put("event", event.published_event());
        if let Some(previous_commit_id) = previous_commit_id {
            put("oldCommitId", previous_commit_id);
        }
        // Both forms of the reference: the fully qualified ref a git client sees, through
        // the one place this crate qualifies a branch name, and the BARE name every
        // control-plane operation takes and reports.
        put("referenceFullName", &qualified_reference(branch));
        put("referenceName", branch);
        put("referenceType", REFERENCE_TYPE_BRANCH);
        put("repositoryId", &self.repository_id);
        put("repositoryName", &self.name);
        event_bus::PublishedEvent {
            repository_arn: self.arn.clone(),
            region: self.region.clone(),
            at: to_date_time(at).to_millis().unwrap_or_default(),
            detail_type: REPOSITORY_STATE_CHANGE_DETAIL_TYPE,
            detail: ::serde_json::Value::Object(detail),
        }
    }
}
