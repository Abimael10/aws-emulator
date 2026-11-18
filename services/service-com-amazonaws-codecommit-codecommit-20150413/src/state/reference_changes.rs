// SPDX-License-Identifier: CC0-1.0

//! What a write of a REFERENCE runs: which of the repository's stored triggers are in
//! scope for it, and the one handover a write makes once this service's store guard is
//! gone.
//!
//! # The rule this module implements
//!
//! `RepositoryTrigger$events` are "the repository events that cause the trigger to run
//! actions in another service, such as sending a notification through Amazon SNS"
//! (`smithy_query(query="doc",shape="RepositoryTrigger",member="events")`), and those
//! events are the reference events `RepositoryTriggerEventEnum` declares — `all`,
//! `createReference`, `updateReference`, `deleteReference`
//! (`smithy_query(query="shape",shape="RepositoryTriggerEventEnum")`). So a write that
//! creates, moves or removes a reference runs every trigger of the repository's stored
//! configuration that is in scope for BOTH that branch and that class of event, and
//! each trigger that runs sends ONE record ([`super::trigger_notifications`]) to the
//! destination it names; a write no stored trigger is in scope for sends nothing, which
//! is what makes an unlisted branch's write observably different from a listed one's.
//!
//! Which branches a trigger is in scope for is its own `branches`: the names it lists,
//! or EVERY branch for a trigger that lists none, since "the branches to be included in
//! the trigger configuration. If you specify an empty array, the trigger applies to all
//! branches" (`smithy_query(query="doc",shape="RepositoryTrigger",member="branches")`).
//! Which events it is in scope for is its own `events`, plus
//! [`super::REPOSITORY_TRIGGER_EVENT_ALL`], the one value that stands for every
//! repository event.
//!
//! # Where it is applied
//!
//! At the ONE place a reference moves ([`Repository::set_branch_tip`]) and the one place
//! a reference is removed ([`Repository::remove_branch_tip`]), so every face of the
//! service that writes a reference — `PutFile`, `CreateCommit`, `CreateBranch`,
//! `DeleteFile`, `DeleteBranch`, the branch and pull-request merges, and the git
//! transport's push — runs the triggers that configuration says, and no new write path
//! can move a reference without running them.
//!
//! A write is handed back as a [`ReferenceChange`]: the records the triggers owe their
//! destinations, the EventBridge event the write itself publishes
//! ([`super::repository_state_events`]), and the pull-request changes the write recorded
//! ([`super::pull_request_events`]). All three are DELIVERED by the handler once the
//! store guard is gone ([`ReferenceChange::publish_detached`]), because each reaches
//! another service's handler, which takes that service's own lock. The git transport is
//! the one write path that delivers none of them: the framework hook it is served
//! through is synchronous and is handed no callback to deliver over, so a push records
//! what happened and leaves it undelivered rather than reaching for a channel this crate
//! does not have there.

use super::*;

/// Which class of `RepositoryTriggerEventEnum` event one write of a reference is.
///
/// One variant per reference event the enum declares beside
/// [`super::REPOSITORY_TRIGGER_EVENT_ALL`], so the events a trigger may name and the
/// writes that run it cannot drift apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReferenceEvent {
    /// A reference the repository did not hold before: `createReference`.
    Created,
    /// A reference that moved from one commit to another: `updateReference`.
    Updated,
    /// A reference the repository no longer holds: `deleteReference`.
    Deleted,
}

impl ReferenceEvent {
    /// The `RepositoryTriggerEventEnum` value this write is, as a trigger names it.
    #[must_use]
    pub fn event(self) -> &'static str {
        match self {
            Self::Created => REPOSITORY_TRIGGER_EVENT_CREATE_REFERENCE,
            Self::Updated => REPOSITORY_TRIGGER_EVENT_UPDATE_REFERENCE,
            Self::Deleted => REPOSITORY_TRIGGER_EVENT_DELETE_REFERENCE,
        }
    }

    /// Whether a trigger naming `events` runs for this write: it names this class of
    /// event, or the one value that stands for every repository event.
    fn runs_for(self, events: &[String]) -> bool {
        events
            .iter()
            .any(|event| event == REPOSITORY_TRIGGER_EVENT_ALL || event == self.event())
    }
}

/// Whether `trigger` is in scope for a write of `branch` as `event`.
///
/// Both halves of the trigger's own scope: the branches it lists — every branch for a
/// trigger that lists none — and the events it names.
fn runs_for(trigger: &RepositoryTrigger, branch: &str, event: ReferenceEvent) -> bool {
    let named = trigger.branches.as_deref().unwrap_or_default();
    (named.is_empty() || named.iter().any(|listed| listed == branch))
        && event.runs_for(&trigger.events)
}

/// What one write of a reference leaves to be delivered once the store guard is gone.
///
/// The three things such a write is observable by outside this service: the records the
/// repository's triggers owe their destinations, the event the reference's own move is
/// published as onto the account's default event bus, and the pull-request changes the
/// write recorded. They are handed over TOGETHER, so no write path can deliver one and
/// forget another.
#[derive(Debug, Default)]
#[must_use = "a write of a reference runs the repository's triggers and publishes the \
              event and the pull-request changes it recorded"]
pub struct ReferenceChange {
    /// The event this write of the reference is published as, or `None` for a write that
    /// moved no reference at all.
    pub repository_state_change: Option<event_bus::PublishedEvent>,
    /// The pull-request changes this write recorded, oldest first.
    pub pull_request_changes: Vec<pull_request_events::PullRequestStateChange>,
    /// The records the triggers in scope for this write owe their destinations, in the
    /// order the stored configuration holds those triggers.
    pub trigger_notifications: Vec<trigger_notifications::TriggerNotification>,
}

impl ReferenceChange {
    /// Deliver everything this write owes, each on a task of its own.
    ///
    /// Call it with NO store guard held, for the reason the module docs give. The
    /// reference's own event is published BEFORE the changes it caused on the pull
    /// requests reading from it, which is the order they happened in.
    pub fn publish_detached(self, callback: &::smithy_server_runtime::EmbeddedEmulator) {
        trigger_notifications::deliver_all_detached(self.trigger_notifications, callback);
        let mut events: Vec<event_bus::PublishedEvent> =
            self.repository_state_change.into_iter().collect();
        events.extend(
            self.pull_request_changes
                .into_iter()
                .map(pull_request_events::PullRequestStateChange::into_event),
        );
        event_bus::publish_all_detached(events, callback);
    }
}

impl Repository {
    /// The records this repository's STORED trigger configuration owes for one write of
    /// `branch` as `event`, leaving `branch` at `commit_id`.
    ///
    /// Read off the stored configuration — its triggers and the `configurationId` the
    /// write that stored them minted — so a record names the configuration a later
    /// `GetRepositoryTriggers` reports, and a repository no caller has configured owes
    /// nothing. `actor` is the identity whose call wrote the reference, for a transport
    /// that carries one.
    #[must_use]
    pub fn reference_change_notifications(
        &self,
        branch: &str,
        commit_id: &str,
        event: ReferenceEvent,
        actor: Option<&str>,
        at: SystemTime,
    ) -> Vec<trigger_notifications::TriggerNotification> {
        let Some(configuration) = self.trigger_configuration.as_ref() else {
            return Vec::new();
        };
        // "refs/heads/<branch>": a record names the FULLY QUALIFIED reference, which is
        // the form a consumer matches a reference on, through the one place this crate
        // qualifies a branch name.
        let reference = qualified_reference(branch);
        configuration
            .triggers
            .iter()
            .filter(|trigger| runs_for(trigger, branch, event))
            .map(|trigger| {
                trigger_notifications::notification(
                    self,
                    trigger,
                    &trigger_notifications::RecordedChange {
                        configuration_id: &configuration.configuration_id,
                        event_name: trigger_notifications::TRIGGER_EVENT_NAME,
                        reference: &reference,
                        commit_id,
                        caller_arn: actor,
                    },
                    at,
                )
            })
            .collect()
    }

    /// Remove the reference `branch`, answering the branch it held and what the removal
    /// leaves to be delivered.
    ///
    /// The ONE place a reference is removed, as [`Repository::set_branch_tip`] is the
    /// one place a reference moves: a removal is the `deleteReference` event a trigger
    /// may name, so every face of the service that drops a reference runs the triggers
    /// in scope for it. A name this repository does not hold removes nothing and owes
    /// nothing. The commits the reference led to are NOT deleted, only the reference to
    /// them, so a commit it reached stays readable by id.
    ///
    /// The record names the reference at the git object name a deleted reference's
    /// update carries ([`git_object::NULL_OBJECT_ID`]), which is what says the reference
    /// points at nothing now; the commit it held before the removal is what the
    /// operation's own answer reports, and what the published event names as the commit
    /// the reference pointed at before.
    #[must_use = "removing a reference runs the repository's triggers"]
    pub fn remove_branch_tip(
        &mut self,
        branch: &str,
        at: SystemTime,
        actor: Option<&str>,
    ) -> Option<(Branch, ReferenceChange)> {
        let removed = self.branches.remove(branch)?;
        let trigger_notifications = self.reference_change_notifications(
            branch,
            git_object::NULL_OBJECT_ID,
            ReferenceEvent::Deleted,
            actor,
            at,
        );
        let repository_state_change = Some(self.reference_state_change(
            branch,
            git_object::NULL_OBJECT_ID,
            Some(&removed.commit_id),
            ReferenceEvent::Deleted,
            actor,
            at,
        ));
        Some((
            removed,
            ReferenceChange {
                repository_state_change,
                pull_request_changes: Vec::new(),
                trigger_notifications,
            },
        ))
    }
}
