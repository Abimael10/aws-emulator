// SPDX-License-Identifier: CC0-1.0

//! WHEN a command leaves the account, and what reports it while it is on its way out.
//!
//! A command is not removed the instant a delete asks for it.
//! `DeleteCommandResponse$statusCode` states the rule: "If the command hasn't been
//! deprecated, or has been deprecated for a duration that is shorter than the maximum time
//! out duration of 12 hours, when calling the DeleteCommand request, the deletion will be
//! scheduled and a 202 status code will be returned. While the command is being deleted, it
//! will be in a pendingDeletion state. Once the time out duration has been reached, the
//! command will be permanently removed from your account. If the command has been deprecated
//! for a duration that is longer than the maximum time out duration of 12 hours, when calling
//! the DeleteCommand request, the command will be deleted immediately and a 204 status code
//! will be returned"
//! (`smithy_query(query="doc",shape="DeleteCommandResponse",member="statusCode")`).
//!
//! Read as state, that is ONE instant per command: the removal falls due
//! [`COMMAND_DELETION_TIMEOUT`] after the command was deprecated, and after the delete
//! request itself for a command that was never deprecated. A delete that arrives at or past
//! that instant removes the command there and then (204); one that arrives before it records
//! the instant and puts the command in `pendingDeletion` (202), where every reader reports it
//! until the clock reaches it. A command on its way out is also a command no new execution
//! may be created from, so a delete that schedules a removal DEPRECATES a command that was
//! not deprecated yet — which is the instant its own timeout is then counted from.
//!
//! The removal the clock has already made due is materialised by the next call that addresses
//! a command ([`purge_due_command_deletions`]): it was owed at its due instant whether or not
//! anybody called, so a reader may not report a command the account no longer holds, and the
//! id must be free for a later create. It is materialised in BOTH stores — this service's own
//! record and the shared index the IoT Jobs data plane resolves a `commandArn` against — so
//! the two planes cannot disagree about which commands exist.

use ::std::time::{Duration, SystemTime};

/// How long the deletion of a command that has not been deprecated for long enough is held
/// open: "the maximum time out duration of 12 hours"
/// (`smithy_query(query="doc",shape="DeleteCommandResponse",member="statusCode")`), the
/// duration after which "the command will be permanently removed from your account".
pub const COMMAND_DELETION_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);

/// What a delete of one command DID, which is what its `statusCode` reports.
///
/// The two outcomes `DeleteCommandResponse$statusCode` documents, so the handler reports the
/// code the state decided rather than restating the rule.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CommandDeletion {
    /// The command stays, in `pendingDeletion`, until its removal falls due: "the deletion
    /// will be scheduled and a 202 status code will be returned".
    Scheduled,
    /// The command is gone from the account: "the command will be deleted immediately and a
    /// 204 status code will be returned". Also what a delete of a command the account does
    /// not hold leaves behind — nothing — the model declaring no not-found error for this
    /// operation.
    #[default]
    Removed,
}

impl CommandDeletion {
    /// The `statusCode` this outcome is reported with, which is also the response's HTTP
    /// status (`DeleteCommandResponse$statusCode` is `@httpResponseCode`).
    #[must_use]
    pub fn status_code(self) -> i32 {
        match self {
            Self::Scheduled => 202,
            Self::Removed => 204,
        }
    }
}

/// How this service refuses an `UpdateCommand` against a command whose deletion is already
/// scheduled.
///
/// A command in `pendingDeletion` is on its way out of the account, so a request to change
/// it "conflicts with the current state of the resource" — the `ConflictException` (HTTP
/// 409) `UpdateCommand` models. AWS words the refusal after the call that failed and the
/// state that refused it, naming the command by the `commandId` the request carried;
/// everything outside that id is fixed text.
pub fn pending_deletion_message(command_id: &str) -> String {
    format!("Failed to update command {command_id} as it's pending deletion.")
}

impl super::RegisteredCommand {
    /// Record that the command is (or is no longer) deprecated, keeping the instant the
    /// deprecation was made.
    ///
    /// That instant is what the deletion timeout is counted from, so a repeat of a
    /// deprecation the command already carries changes nothing: deprecating twice may not
    /// push the removal of a command that has been deprecated for hours back out again.
    pub fn set_deprecated(&mut self, deprecated: bool, at: SystemTime) {
        if self.deprecated == deprecated {
            return;
        }
        self.deprecated = deprecated;
        self.deprecated_at = deprecated.then_some(at);
    }

    /// When the removal of this command falls due if a delete arrives at `now`: the deletion
    /// timeout after the deprecation, or after the delete itself for a command that carries
    /// no deprecation.
    fn deletion_due_from(&self, now: SystemTime) -> SystemTime {
        self.deprecated_at
            .unwrap_or(now)
            .checked_add(COMMAND_DELETION_TIMEOUT)
            .unwrap_or(now)
    }

    /// Whether the removal this command already has scheduled is due at `now`.
    #[must_use]
    pub fn deletion_due(&self, now: SystemTime) -> bool {
        self.deletion_due_at.is_some_and(|due| now >= due)
    }
}

/// What ONE `DeleteCommand` call left behind, as the state decided it.
///
/// The ARNs are handed back because a command's existence and its deprecation are read
/// OUTSIDE this crate, in the shared index the IoT Jobs data plane resolves a `commandArn`
/// against: the caller writes them there under the lock order
/// [`super::command_registry`] documents, which takes this service's store first.
#[derive(Debug, Clone, Default)]
pub struct CommandDeleted {
    /// What the delete did, which is what `statusCode` reports.
    pub outcome: CommandDeletion,
    /// The ARNs of the commands the account no longer holds after this call: the one it
    /// deleted, and any whose scheduled removal it found due.
    pub removed: Vec<String>,
    /// The ARN of the command this call left DEPRECATED by scheduling its removal, when it
    /// scheduled one: a command on its way out is one no new execution may be created from,
    /// which is a fact the shared index carries.
    pub deprecated: Option<String>,
}

impl super::ComAmazonawsIotAwsiotserviceState {
    /// Delete the command `command_id` names, answering what the delete did and which
    /// commands the shared index has to be told about.
    ///
    /// The removals the clock has already made due are carried out first, so the delete is
    /// answered against the commands the account still holds.
    pub fn delete_command(&mut self, command_id: &str, now: SystemTime) -> CommandDeleted {
        let mut deleted = CommandDeleted {
            outcome: CommandDeletion::Removed,
            removed: self.remove_due_command_deletions(now),
            deprecated: None,
        };

        let Some(command) = self.commands.get_mut(command_id) else {
            // No command, so nothing is left to remove: the same outcome as a command this
            // call removed, and the operation models no not-found error to answer instead.
            return deleted;
        };

        let due = command.deletion_due_from(now);
        if due <= now {
            // "deprecated for a duration that is longer than the maximum time out duration":
            // the command is removed by this call rather than scheduled.
            if let Some(removed) = self.commands.remove(command_id) {
                deleted.removed.push(removed.command_arn);
            }
            return deleted;
        }

        // "the deletion will be scheduled [...] While the command is being deleted, it will
        // be in a pendingDeletion state", and a command on its way out is a command no new
        // execution may be created from, so the delete deprecates one that was not
        // deprecated yet and that deprecation is what the timeout is then counted from. A
        // delete repeated before the removal falls due keeps the EARLIEST instant recorded,
        // so repeating it cannot postpone the removal.
        command.set_deprecated(true, now);
        command.pending_deletion = true;
        command.deletion_due_at = Some(
            command
                .deletion_due_at
                .map_or(due, |scheduled| scheduled.min(due)),
        );
        deleted.outcome = CommandDeletion::Scheduled;
        deleted.deprecated = Some(command.command_arn.clone());
        deleted
    }

    /// Remove every command whose scheduled deletion `now` has reached, answering their ARNs
    /// so the caller takes them out of the shared index too.
    ///
    /// This materialises a removal the CLOCK made due — "once the time out duration has been
    /// reached, the command will be permanently removed from your account" — which was owed
    /// at that instant whether or not this call happened.
    pub fn remove_due_command_deletions(&mut self, now: SystemTime) -> Vec<String> {
        let due: Vec<String> = self
            .commands
            .iter()
            .filter(|(_, command)| command.deletion_due(now))
            .map(|(command_id, _)| command_id.clone())
            .collect();
        due.iter()
            .filter_map(|command_id| self.commands.remove(command_id))
            .map(|command| command.command_arn)
            .collect()
    }
}

/// Carry out the command removals the clock has already made due, in this service's store and
/// in the shared index: the first thing an operation that addresses a command does, so what it
/// reads is the set of commands the account still holds.
///
/// Takes the two locks in the order [`super::command_registry`] documents — this service's
/// store first, the shared index second — and holds neither when it returns, so a caller may
/// call it before taking either.
pub fn purge_due_command_deletions<Eng: Send + Sync + 'static>(engine: &Eng, now: SystemTime) {
    let Some(state) = super::service_state(engine) else {
        return;
    };
    let removed = state.lock().remove_due_command_deletions(now);
    if removed.is_empty() {
        return;
    }
    if let Some(registry) = super::command_registry(engine) {
        let mut index = registry.lock();
        for command_arn in &removed {
            index.remove(command_arn);
        }
    }
}
