// SPDX-License-Identifier: CC0-1.0

//! DIRECTORY TEARDOWN: what `DeleteDirectory` takes out of service, the window a client
//! reads its own deletion off, and the one fact the account keeps about an id it HAD.
//!
//! The directory stops being one the account HOLDS inside the call. By the time the
//! operation ([`crate::ops::delete_directory`]) hands its caller the id, this service has
//! released everything the directory was given: the network interfaces its servers answered
//! on and the domain controller security group they sat behind are deleted in the service
//! that OWNS them, and the entry the Directory Service DATA plane reads is dropped from the
//! shared inventory. Every call whose subject has to be a live directory is refused for the
//! id from that instant, it counts towards no directory limit and it serves no DNS.
//!
//! What the delete does NOT do is make the directory vanish from the account's reporting. A
//! teardown takes time, and the modelled `DirectoryStage` enum declares the two stages it
//! is reported in: the record stays DESCRIBABLE in [`STAGE_DELETING`], reaches the chain's
//! terminal [`STAGE_DELETED`] one [`TRANSITION_SPAN`] later and leaves the account's records
//! one span after that — each step owed to the clock, so a caller that describes the
//! directory straight after the delete still observes the stage the teardown starts in.
//! That window is what a delete-and-wait client polls: a directory that jumped from in
//! service to absent would tell such a client the id was never its own.
//!
//! What the delete leaves behind once the window closes is a single FACT: that this account
//! HAD the id
//! ([`ComAmazonawsDirectoryserviceDirectoryservice20150416State::remember_deleted_directory`]).
//! That is what makes a later reporting call FILTERED by the id an empty page rather than a
//! refusal, while an id this account was never issued stays refused — the distinction a
//! delete-and-wait client reads the END of its deletion off.
//!
//! Everything that describes nothing without the directory goes with its record as the
//! delete runs: the snapshots taken of it, the trusts it is one party to and the shares
//! consumer accounts hold on it.

use super::*;

/// What a directory's teardown has to release in the services that OWN the resources its
/// create was given, read off the record as it left service.
///
/// Why it is answered rather than left on the record: the hops into the owner of network
/// interfaces and security groups hold no lock on this store, so the delete takes the record
/// out of service under its own short-lived guard and carries these identifiers out with it.
#[derive(Debug)]
pub struct DirectoryTeardown {
    /// The directory the delete applied to, as this service stored the id.
    pub directory_id: String,
    /// The domain controller security group the create was given, or `None` for a directory
    /// that was given none.
    pub security_group_id: Option<String>,
    /// The network interfaces the directory's servers answered on, in subnet order.
    pub network_interface_ids: Vec<String>,
}

impl ComAmazonawsDirectoryserviceDirectoryservice20150416State {
    /// Take the directory `directory_id` out of service as of `now` and answer what the
    /// caller must now release in the services that own it — or `None` when no directory is
    /// stored under that id, which is the caller's cue to refuse the request.
    ///
    /// THE one place a delete is recorded. The record leaves [`Self::directories`] with
    /// everything that describes nothing without it ([`Self::retire_directory`]) and is kept
    /// in [`Self::deleting_directories`] reporting [`STAGE_DELETING`], with the rest of the
    /// teardown chain owed to the clock; the id is remembered as one this account HAD, so a
    /// repeated delete finds nothing to delete and is refused the way a delete of an id that
    /// was never there is.
    pub fn tear_down_directory(
        &mut self,
        directory_id: &str,
        now: SystemTime,
    ) -> Option<DirectoryTeardown> {
        let directory = self.directories.get(directory_id)?;
        let teardown = DirectoryTeardown {
            directory_id: directory.directory_id.clone(),
            security_group_id: directory.provisioned_security_group_id().map(str::to_string),
            network_interface_ids: directory.provisioned_network_interface_ids(),
        };
        self.retire_directory(&teardown.directory_id, now);
        Some(teardown)
    }

    /// Take the record of the directory `directory_id` out of service as of `now`, with
    /// everything that describes nothing without it, and leave it reporting the stage its
    /// teardown starts in.
    ///
    /// The alias index is deliberately NOT touched: an alias "cannot be deleted or reused",
    /// so it stays reserved against the id it was created for and a later `CreateAlias`
    /// asking for it is refused.
    ///
    /// The id itself is remembered, and nothing else of the directory: an account that HAD
    /// this directory is what makes a reporting call filtered by this id an empty result
    /// once the teardown window has closed, while an id this account never had stays a
    /// refusal.
    fn retire_directory(&mut self, directory_id: &str, now: SystemTime) {
        let Some(mut directory) = self.directories.remove(directory_id) else {
            return;
        };
        let seq = directory.seq;
        // The teardown's first stage, with the next step of the chain owed to the clock.
        directory.stage = STAGE_DELETING.to_string();
        directory.stage_last_updated = now;
        directory.stage_due_at = Some(now + TRANSITION_SPAN);
        // The network the teardown releases in the service that OWNS it is released from
        // this record too, so nothing the record still reports names a resource EC2 no
        // longer holds.
        directory.forget_provisioned_network();
        self.deleting_directories
            .insert(directory_id.to_string(), directory);
        self.remember_deleted_directory(directory_id, seq);
        // A snapshot is a snapshot OF this directory, so it does not outlive it: real
        // Directory Service deletes a directory's snapshots with the directory ("all
        // snapshots of a directory are deleted when the directory is deleted"), and keeping
        // them would leave records naming an id no caller can resolve — and a store a
        // create/delete cycle could grow without end.
        self.snapshots
            .retain(|_, snapshot| snapshot.directory_id != directory_id);
        // A trust is the Amazon Web Services side of a relationship this directory is one
        // party to, so it does not outlive the directory either.
        self.trusts
            .retain(|_, trust| trust.directory_id != directory_id);
        // A directory share is the consumer account's handle on THIS directory, so it does
        // not outlive it either: keeping it would leave records naming an owner id no caller
        // can resolve.
        self.shared_directories
            .retain(|_, share| share.owner_directory_id != directory_id);
    }

    /// Materialise every step of a teardown the clock has already made due at `now`: a
    /// directory in [`STAGE_DELETING`] whose span has elapsed reports [`STAGE_DELETED`], and
    /// one that has reported the chain's terminal stage for a span is gone from the
    /// account's records altogether.
    ///
    /// Owed at `now` whether or not anybody called: each due instant was stored when the
    /// delete ran, so this only brings the record to the stage it is already in.
    pub(super) fn advance_directory_teardowns(&mut self, now: SystemTime) {
        for directory in self.deleting_directories.values_mut() {
            let Some(due) = directory.stage_due_at else {
                continue;
            };
            if now >= due && directory.stage == STAGE_DELETING {
                directory.stage = STAGE_DELETED.to_string();
                directory.stage_last_updated = due;
                directory.stage_due_at = Some(due + TRANSITION_SPAN);
            }
            // A record that has reached the terminal stage leaves the store below, so its
            // due instant is left in place for that pass to find.
        }
        self.deleting_directories.retain(|_, directory| {
            directory.stage != STAGE_DELETED
                || directory.stage_due_at.is_some_and(|due| now < due)
        });
    }
}
