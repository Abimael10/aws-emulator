// SPDX-License-Identifier: CC0-1.0

//! The history a domain keeps for each of its profiles: one record per action taken on a
//! profile, and the projection a listing reports them through.
//!
//! `ListProfileHistoryRecords` "returns a list of history records for a specific profile,
//! for a specific domain", and the actions a record can describe are the `ActionType`
//! enum's: `CREATED`, `UPDATED`, `ADDED_PROFILE_KEY`, `DELETED_PROFILE_KEY`, `INGESTED`,
//! `DELETED_BY_CUSTOMER`, `EXPIRED`, `MERGED` and `DELETED_BY_MERGE`
//! (`smithy_query(query="shape", shape="ActionType")`). So the history is not a resource a
//! caller creates: it is written BY the operations that change a profile or the objects
//! ingested onto it, which is what makes a listing report what actually happened to that
//! profile rather than a value invented when it is read.
//!
//! Every record is held with the profile it describes, in the order the actions happened,
//! so one listing walks them in one stable order — which is what makes its `NextToken`
//! cursor resumable — and deleting a profile takes its history with it ("deletes the
//! standard customer profile and all data pertaining to the profile", `DeleteProfile`).

use std::time::SystemTime;

use crate::ops::types::ProfileHistoryRecord;

use super::{to_date_time, Domain};

/// The `ObjectTypeName` of a record describing an action on the PROFILE itself rather
/// than on one ingested object.
///
/// `ProfileHistoryRecord$ObjectTypeName` is required, and the standard profile model this
/// service names a profile's own data by is `_profile`: an object type's
/// `ObjectTypeField$Target` is documented as "the location of the data in the standard
/// ProfileObject model. For example: _profile.Address.PostalCode", so the data a profile
/// carries sits under that one name (see [`super::PROFILE_TARGET_PREFIX`], which is this
/// name followed by the member separator). A record about an ingested object carries that
/// object's OWN type name instead.
pub const STANDARD_PROFILE_OBJECT_TYPE: &str = "_profile";

/// `ActionType` of the record a profile's creation writes.
pub const ACTION_CREATED: &str = "CREATED";

/// `ActionType` of the record a write to a profile's own members writes.
pub const ACTION_UPDATED: &str = "UPDATED";

/// `ActionType` of the record associating a key value with a profile writes.
pub const ACTION_ADDED_PROFILE_KEY: &str = "ADDED_PROFILE_KEY";

/// `ActionType` of the record disassociating a key value from a profile writes.
pub const ACTION_DELETED_PROFILE_KEY: &str = "DELETED_PROFILE_KEY";

/// `ActionType` of the record ingesting a profile object writes.
pub const ACTION_INGESTED: &str = "INGESTED";

/// `ActionType` of the record a caller's own deletion of a profile object writes.
pub const ACTION_DELETED_BY_CUSTOMER: &str = "DELETED_BY_CUSTOMER";

/// `ActionType` of the record an object dropped because its retention ran out writes.
pub const ACTION_EXPIRED: &str = "EXPIRED";

/// `ActionType` of the record a merge writes for the profile that absorbed the others.
pub const ACTION_MERGED: &str = "MERGED";

/// `ActionType` of the record a merge writes for a profile the merge ended.
pub const ACTION_DELETED_BY_MERGE: &str = "DELETED_BY_MERGE";

/// Emulator resource guard, NOT an AWS quota (the service publishes no limit on a
/// profile's history): the most history records this emulator keeps for one profile.
///
/// Records are written by the service as actions happen, so nothing a caller sends bounds
/// them: past this many the OLDEST record of that profile is dropped as the newest is
/// appended, which is what keeps a long-lived profile from growing the store without
/// bound. A refusal would be wrong here — the action itself is valid, and the history is
/// the service's own bookkeeping, not something the request asked to store.
pub const MAX_PROFILE_HISTORY_RECORDS: usize = 1_000;

/// One stored history record: the action, what it was about, when it happened, and who
/// performed it.
///
/// A record is a log entry — nothing changes one after the action it describes — so it
/// carries no "last updated" instant of its own, and `LastUpdatedAt` is reported unset.
#[derive(Debug, Clone)]
pub struct StoredProfileHistoryRecord {
    /// `Id`, as minted by [`new_history_record_id`].
    pub id: String,
    /// Where this record sits in the order its DOMAIN's actions happened: the cursor a
    /// listing pages through ([`StoredProfileHistoryRecord::cursor`]). Kept on the record
    /// so the order survives a merge moving another profile's history in beside it.
    pub sequence: u64,
    /// `ActionType`: one of the modelled values above.
    pub action_type: String,
    /// `ObjectTypeName`: the object type the action was about, or
    /// [`STANDARD_PROFILE_OBJECT_TYPE`] for an action on the profile itself.
    pub object_type_name: String,
    /// `ProfileObjectUniqueKey`: the identity of the profile object the action was about,
    /// so an action on the profile itself carries none.
    pub profile_object_unique_key: Option<String>,
    /// `PerformedBy`: the principal whose request performed the action. An action nobody
    /// asked for — one the clock owed, such as an object's retention running out — was
    /// performed by no principal and carries none.
    pub performed_by: Option<String>,
    /// `CreatedAt`: the instant the action happened.
    pub created_at: SystemTime,
}

impl StoredProfileHistoryRecord {
    /// This record as one entry of a `ListProfileHistoryRecords` page.
    pub fn as_record(&self) -> ProfileHistoryRecord {
        ProfileHistoryRecord {
            id: self.id.clone(),
            object_type_name: self.object_type_name.clone(),
            created_at: to_date_time(self.created_at),
            // Nothing updates a record after the action it describes, so there is no
            // later instant to report.
            last_updated_at: None,
            action_type: self.action_type.clone(),
            profile_object_unique_key: self.profile_object_unique_key.clone(),
            performed_by: self.performed_by.clone(),
        }
    }

    /// The pagination cursor of this record: its sequence, zero padded to a fixed width so
    /// comparing two cursors as text orders them as the actions happened.
    pub fn cursor(&self) -> String {
        format!("{:0width$}", self.sequence, width = CURSOR_WIDTH)
    }

    /// Whether this record passes the filters a listing carries.
    ///
    /// Each of `ActionType`, `ObjectTypeName` and `PerformedBy` "applies a filter to
    /// include profile history records only with the specified … value in the response",
    /// so a filter a request omits selects nothing out, and the filters a request does
    /// carry all have to hold.
    pub fn selected_by(
        &self,
        action_type: Option<&str>,
        object_type_name: Option<&str>,
        performed_by: Option<&str>,
    ) -> bool {
        action_type.is_none_or(|wanted| self.action_type == wanted)
            && object_type_name.is_none_or(|wanted| self.object_type_name == wanted)
            && performed_by.is_none_or(|wanted| self.performed_by.as_deref() == Some(wanted))
    }
}

/// How many digits a cursor is padded to: enough for every sequence a `u64` counter can
/// reach, so one cursor never sorts before an earlier one for being shorter.
const CURSOR_WIDTH: usize = 20;

/// A fresh history record `Id`: the 32 lowercase hexadecimal characters the modelled
/// pattern `^[a-f0-9]{32}$` requires, taken from a v4 UUID's simple (undashed) form — the
/// same shape every other identifier of this service's `uuid` shape carries.
pub fn new_history_record_id() -> String {
    ::uuid::Uuid::new_v4().simple().to_string()
}

/// One action to be recorded against a profile: what happened, what it was about, and who
/// performed it. The record's id, instant and position are the store's to assign, so they
/// are not part of this.
#[derive(Debug, Clone)]
pub struct ProfileAction {
    /// `ActionType` of the record this writes.
    pub action_type: String,
    /// `ObjectTypeName` of the record this writes.
    pub object_type_name: String,
    /// `ProfileObjectUniqueKey` of the record this writes.
    pub profile_object_unique_key: Option<String>,
    /// `PerformedBy` of the record this writes.
    pub performed_by: Option<String>,
}

impl ProfileAction {
    /// An action a caller's request performed on the PROFILE itself: its object type is
    /// the standard profile and it names no object.
    pub fn on_profile(action_type: &str) -> Self {
        Self {
            action_type: action_type.to_string(),
            object_type_name: STANDARD_PROFILE_OBJECT_TYPE.to_string(),
            profile_object_unique_key: None,
            performed_by: Some(caller_principal()),
        }
    }

    /// An action a caller's request performed on ONE ingested object of a profile: the
    /// record names that object's type and its unique key.
    pub fn on_object(action_type: &str, object_type_name: &str, unique_key: &str) -> Self {
        Self {
            action_type: action_type.to_string(),
            object_type_name: object_type_name.to_string(),
            profile_object_unique_key: Some(unique_key.to_string()),
            performed_by: Some(caller_principal()),
        }
    }

    /// The same action with no principal behind it: the clock, not a request, performed
    /// it, so the record reports nobody.
    pub fn performed_by_nobody(mut self) -> Self {
        self.performed_by = None;
        self
    }
}

/// The ARN of the principal a request to this emulator speaks for — what `PerformedBy`,
/// "the Amazon Resource Name (ARN) of the person or service principal who performed the
/// action", carries.
///
/// This emulator publishes one long-term credential, which belongs to one IAM user of the
/// account; that user is stated once for the whole emulator, so the identity recorded here
/// is the same one STS reports as the caller and IAM holds as a user, and is never
/// invented per service.
fn caller_principal() -> String {
    ::service_common::caller_principal::ambient_user_arn()
}

impl Domain {
    /// Record `action` against `profile_id` as of `now`.
    ///
    /// Called by the operations that change a profile or its objects, so a listing reports
    /// actions that really happened. An object the ingest could tie to no profile records
    /// nothing: there is no profile whose history it would belong to (see
    /// [`super::ProfileObjectId`]).
    pub fn record_profile_action(
        &mut self,
        profile_id: &str,
        action: ProfileAction,
        now: SystemTime,
    ) {
        if profile_id.is_empty() {
            return;
        }
        let sequence = self.history_sequence;
        self.history_sequence = self.history_sequence.saturating_add(1);
        let records = self.profile_history.entry(profile_id.to_string()).or_default();
        trim_to_cap(records, MAX_PROFILE_HISTORY_RECORDS.saturating_sub(1));
        records.push(StoredProfileHistoryRecord {
            id: new_history_record_id(),
            sequence,
            action_type: action.action_type,
            object_type_name: action.object_type_name,
            profile_object_unique_key: action.profile_object_unique_key,
            performed_by: action.performed_by,
            created_at: now,
        });
    }

    /// The history of one profile, oldest action first — empty for a profile nothing has
    /// happened to.
    pub fn profile_history_records(&self, profile_id: &str) -> &[StoredProfileHistoryRecord] {
        self.profile_history
            .get(profile_id)
            .map_or(&[][..], Vec::as_slice)
    }

    /// Drop one profile's history: the records are part of the data pertaining to that
    /// profile, so they go when it does.
    pub fn remove_profile_history(&mut self, profile_id: &str) {
        self.profile_history.remove(profile_id);
    }

    /// Move one profile's history onto another, keeping both profiles' actions in the
    /// order they happened: the history of a profile merged into another belongs to the
    /// profile that absorbed it, as its keys and its objects do.
    pub fn move_profile_history(&mut self, from: &str, to: &str) {
        if from == to {
            return;
        }
        let Some(moved) = self.profile_history.remove(from) else {
            return;
        };
        let records = self.profile_history.entry(to.to_string()).or_default();
        records.extend(moved);
        records.sort_by_key(|record| record.sequence);
        trim_to_cap(records, MAX_PROFILE_HISTORY_RECORDS);
    }
}

/// Drop the oldest records until at most `cap` are left.
fn trim_to_cap(records: &mut Vec<StoredProfileHistoryRecord>, cap: usize) {
    if records.len() > cap {
        records.drain(..records.len() - cap);
    }
}
