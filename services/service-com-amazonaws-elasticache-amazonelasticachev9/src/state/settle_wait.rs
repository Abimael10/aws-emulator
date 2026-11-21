// SPDX-License-Identifier: CC0-1.0

//! What a REPORTING operation answers with while the clock still owes a change
//! it has already scheduled: the record as it stands.
//!
//! Every transient status this service reports — a cluster `creating`, a
//! snapshot `creating` or `deleting`, a replication group `modifying`, a user
//! group `modifying` — is held for the span of the CALL that started the work
//! ([`CLUSTER_TRANSITION`]), stored as the due instant on the record and
//! materialised by whichever later call reads it. No call of this service waits
//! for such a span to pass: the call that STARTS the work answers with the
//! transient status, and a report arriving before the change falls due answers
//! that status too. That span is shorter than the gap between two requests, so
//! the FIRST read a client issues after the starting call answers with the
//! settled state: a client polling a lifecycle status on an interval sized for
//! AWS must not lose that interval to a resource this emulator has already
//! finished, or a flow that chains several waits never converges.
//!
//! What a report DOES write is the change the clock had already made due at the
//! instant it answers: each reporting operation runs the settling pass for the
//! records it reads before it reads them, so the state it reports is the state
//! the clock owed whether or not anybody had called. [`rbac_change_due`] is the
//! cheap question that pass is guarded by on the hot path.
//!
//! [`rbac_change_due`]: ComAmazonawsElasticacheAmazonelasticachev9State::rbac_change_due

use std::time::SystemTime;

use super::*;

impl ComAmazonawsElasticacheAmazonelasticachev9State {
    /// Whether the clock owes an RBAC record — a user or a user group — a change at `now`:
    /// a status transition it has reached, or a removal a delete made due.
    ///
    /// Read before settling them, so a caller that settles on a hot path (a RESP session,
    /// which is judged against the membership of the moment) walks the maps only when there
    /// is something to carry through.
    pub fn rbac_change_due(&self, now: SystemTime) -> bool {
        let user_due = self.users.values().any(|user| {
            user.pending_status
                .as_ref()
                .is_some_and(|pending| now >= pending.due)
                || user.pending_removal.is_some_and(|due| now >= due)
        });
        user_due
            || self.user_groups.values().any(|group| {
                group
                    .pending_status
                    .as_ref()
                    .is_some_and(|pending| now >= pending.due)
                    || group.pending_removal.is_some_and(|due| now >= due)
            })
    }
}
