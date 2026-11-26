// SPDX-License-Identifier: CC0-1.0

//! What a configuration recorder FOUND when recording resumed, and why none of it is
//! recorded.
//!
//! Config does not record the configuration changes that happened while the configuration
//! recorder was stopped: a recorder observes nothing while it is off, so a resource that
//! came into existence, changed or went away in that window has no configuration item
//! recording it — the recorder simply resumes recording the changes it observes from then
//! on, and the next state such a resource changes into is what its next item records.
//!
//! An emulated recorder observes the resources it records by READING them from the services
//! that own them (`super::recording`), so the state it reads the instant recording resumes
//! is the state the stopped window left behind — exactly what must not be recorded. That
//! state is therefore noted HERE, as the state recording resumed at, and the write path
//! records no configuration item for a resource it still finds in it
//! ([`super::ComAmazonawsConfigserviceStarlingdoveserviceState::record_discovered_resource`]).
//! A resource that changes afterwards leaves that state, so its change is recorded as the
//! item it owes — the first item of a resource Config never recorded before.

use std::collections::BTreeSet;

use super::{ComAmazonawsConfigserviceStarlingdoveserviceState, DiscoveredResourceEntry};

/// The state one resource was in when recording RESUMED, which is the state the stopped
/// window left it in.
///
/// The map these are held in
/// ([`ComAmazonawsConfigserviceStarlingdoveserviceState::state_at_resumption`]) is BOUNDED
/// by the inventory it is read beside: an entry is noted only for a resource ONE observation
/// found (or one the inventory already holds an item for), the whole map is forgotten at the
/// next resumption, and each entry is dropped as soon as its resource leaves the state it
/// was noted in.
#[derive(Debug, Clone)]
pub enum StateAtResumption {
    /// The state the recorder found the resource in, as an observation of it.
    Found(Box<DiscoveredResourceEntry>),
    /// The resource was GONE: the inventory holds a configuration item for it, and it was
    /// no longer there when recording resumed, so its deletion happened while the recorder
    /// was stopped.
    Gone,
}

impl ComAmazonawsConfigserviceStarlingdoveserviceState {
    /// Forget what the previous resumption found: taken in the critical section that turns
    /// recording ON, so what is noted afterwards is what THIS resumption found and nothing
    /// a previous one did.
    pub fn forget_state_at_resumption(&mut self) {
        self.state_at_resumption.clear();
    }

    /// Note what a resumption's observation of `resource_type` found: the state the stopped
    /// window left each of that type's resources in, for every one whose state the
    /// inventory does not already record.
    ///
    /// A resource the inventory records in the very state the observation found is NOT
    /// noted: nothing changed for it while the recorder was stopped, so there is nothing
    /// for the write path to hold back, and its next change is recorded as an update of the
    /// item it holds. A resource the inventory holds an item for that the observation did
    /// not see went away while the recorder was stopped ([`StateAtResumption::Gone`]).
    ///
    /// Nothing is noted for a type the account does not RECORD: such a type is not observed
    /// at all, so no state of it was left behind to hold back.
    pub fn note_state_at_resumption(
        &mut self,
        resource_type: &str,
        observed: Vec<DiscoveredResourceEntry>,
    ) {
        if !self.records_resource_type(resource_type) {
            return;
        }
        let mut seen: BTreeSet<(String, String)> = BTreeSet::new();
        for entry in observed {
            let key = (entry.resource_type.clone(), entry.resource_id.clone());
            seen.insert(key.clone());
            let already_recorded = self.discovered_resources.get(&key).is_some_and(|held| {
                held.last_recorded_at.is_some() && !held.records_another_state(&entry)
            });
            if !already_recorded {
                self.state_at_resumption
                    .insert(key, StateAtResumption::Found(Box::new(entry)));
            }
        }
        let gone: Vec<(String, String)> = self
            .discovered_resources
            .iter()
            .filter(|(key, held)| {
                key.0 == resource_type
                    && !seen.contains(*key)
                    && held.last_recorded_at.is_some()
                    && held.deleted_at.is_none()
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in gone {
            self.state_at_resumption
                .insert(key, StateAtResumption::Gone);
        }
    }

    /// Whether `candidate` records the state recording RESUMED at for `key` — the state the
    /// stopped window left the resource in, which is not Config's to record.
    pub(super) fn is_the_state_resumed_at(
        &self,
        key: &(String, String),
        candidate: &DiscoveredResourceEntry,
    ) -> bool {
        match self.state_at_resumption.get(key) {
            None => false,
            // The resource was gone when recording resumed, so the observation that finds
            // it gone records the deletion the stopped window made.
            Some(StateAtResumption::Gone) => candidate.deleted_at.is_some(),
            Some(StateAtResumption::Found(found)) => {
                candidate.deleted_at.is_none() && !found.records_another_state(candidate)
            }
        }
    }

    /// Note that `key` has LEFT the state recording resumed at: what is recorded for it from
    /// here on is a change the recorder observed, so the state the stopped window left is no
    /// longer the one to hold back.
    pub(super) fn forget_resource_state_at_resumption(&mut self, key: &(String, String)) {
        self.state_at_resumption.remove(key);
    }
}
