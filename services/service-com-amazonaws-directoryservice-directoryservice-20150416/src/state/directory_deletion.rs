// SPDX-License-Identifier: CC0-1.0

//! DIRECTORY TEARDOWN: what `DeleteDirectory` takes away, and the one fact the account keeps
//! about an id it HAD.
//!
//! The teardown is COMPLETE when the delete answers. By the time the operation
//! ([`crate::ops::delete_directory`]) hands its caller the id, this service has released
//! everything the directory was given: the network interfaces its servers answered on and the
//! domain controller security group they sat behind are deleted in the service that OWNS them,
//! and the entry the Directory Service DATA plane reads is dropped from the shared inventory.
//! Nothing of the directory is left for a later call to finish.
//!
//! So nothing is owed to the clock here. A transition is owed to the clock only while this
//! service still has work to do after it answered — the servers a create has to bring up,
//! which is what a create's stage chain reports ([`TRANSITION_SPAN`],
//! [`DIRECTORY_BUILD_SPAN`]) — and a teardown that ran inside the call has none: a record kept
//! past it would describe a directory whose servers, addresses, group and data plane are
//! already gone.
//!
//! What the delete leaves behind is a single FACT: that this account HAD the id
//! ([`ComAmazonawsDirectoryserviceDirectoryservice20150416State::remember_deleted_directory`]).
//! That is what makes a later reporting call FILTERED by the id an empty page rather than a
//! refusal, while an id this account was never issued stays refused — the distinction a
//! delete-and-wait client reads its own deletion off.
//!
//! Everything that describes nothing without the directory goes with its record: the snapshots
//! taken of it, the trusts it is one party to and the shares consumer accounts hold on it.

use super::*;

/// What a directory's teardown has to release in the services that OWN the resources its
/// create was given, read off the record before it left the store.
///
/// Why it is answered rather than left on the record: the hops into the owner of network
/// interfaces and security groups hold no lock on this store, so the delete takes the record
/// away under its own short-lived guard and carries these identifiers out with it.
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
    /// Take the directory `directory_id` out of the store and answer what the caller must now
    /// release in the services that own it — or `None` when no directory is stored under that
    /// id, which is the caller's cue to refuse the request.
    ///
    /// THE one place a delete is recorded. The record goes whole, with everything that
    /// describes nothing without it ([`Self::forget_directory`]), and the id is remembered as
    /// one this account HAD, so a repeated delete finds nothing to delete and is refused the
    /// way a delete of an id that was never there is.
    pub fn tear_down_directory(&mut self, directory_id: &str) -> Option<DirectoryTeardown> {
        let directory = self.directories.get(directory_id)?;
        let teardown = DirectoryTeardown {
            directory_id: directory.directory_id.clone(),
            security_group_id: directory
                .provisioned_security_group_id()
                .map(str::to_string),
            network_interface_ids: directory.provisioned_network_interface_ids(),
        };
        let seq = directory.seq;
        self.forget_directory(&teardown.directory_id, seq);
        Some(teardown)
    }

    /// Take the record of the directory `directory_id`, created at rank `seq`, out of the
    /// store, with everything that describes nothing without it.
    ///
    /// The alias index is deliberately NOT touched: an alias "cannot be deleted or reused",
    /// so it stays reserved against the id it was created for and a later `CreateAlias`
    /// asking for it is refused.
    ///
    /// The id itself is remembered, and nothing else of the directory: an account that HAD
    /// this directory is what makes a later reporting call filtered by this id an empty
    /// result rather than a refusal, while an id this account never had stays a refusal.
    fn forget_directory(&mut self, directory_id: &str, seq: u64) {
        self.directories.remove(directory_id);
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
}
