// SPDX-License-Identifier: CC0-1.0

//! What one `ModifyVerifiedAccessGroup` request changes about a stored Verified Access group:
//! the record of that request its idempotency token is kept under, the ONE definition of what
//! applying it leaves behind, the one writer that applies it, and the one reading of a stored
//! group every report of one answers with.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

/// One `ModifyVerifiedAccessGroup` request, as the token it was given with recorded it: the
/// group it modified and the members it set.
///
/// "A unique, case-sensitive token that you provide to ensure idempotency of your modification
/// request" (`ModifyVerifiedAccessGroupRequest$ClientToken`) — so a repeat of one request
/// applies nothing further, while the SAME token carrying a DIFFERENT request is the mismatch
/// EC2 refuses ([`idempotent_parameter_mismatch`]) rather than a second modification. Telling
/// those two apart needs the REQUEST, which is why a recorded modification carries the members
/// it set and not just the group it named.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedAccessGroupModification {
    /// The group the request named.
    pub verified_access_group_id: String,
    /// "The ID of the Verified Access instance"
    /// (`ModifyVerifiedAccessGroupRequest$VerifiedAccessInstanceId`), or `None` when the
    /// request carried none and so left the group on the instance it was already on.
    pub verified_access_instance_id: Option<String>,
    /// "A description for the Verified Access group"
    /// (`ModifyVerifiedAccessGroupRequest$Description`), or `None` when the request carried
    /// none and so left the stored description as it was.
    pub description: Option<String>,
}

impl VerifiedAccessGroupModification {
    /// `record` as this modification leaves it: every member the request carried replaces the
    /// stored value, and every member it did not carry keeps it.
    ///
    /// The ONE definition of what a modification does to a group, so the pre-check of the
    /// store's byte budget and
    /// [`ComAmazonawsEc2Amazonec2State::modify_verified_access_group`]'s write cannot disagree
    /// about the record the call leaves behind. `LastUpdatedTime` is NOT set here: the instant
    /// a modification was applied is the store's to stamp, not a figure a pre-check may
    /// invent. The policy document beside the view is not a member of this request either
    /// (`ModifyVerifiedAccessGroupPolicy` is what carries one), so it is kept as it was.
    pub fn applied_to(&self, record: &VerifiedAccessGroupRecord) -> VerifiedAccessGroupRecord {
        let mut applied = record.clone();
        if let Some(instance_id) = self.verified_access_instance_id.clone() {
            applied.view.verified_access_instance_id = Some(instance_id);
        }
        if let Some(description) = self.description.clone() {
            applied.view.description = Some(description);
        }
        applied
    }
}

/// The caller-chosen bytes one recorded Verified Access group modification holds for the
/// process's lifetime: the members the request set and the token the map is keyed by, over a
/// fixed floor for the entry itself. The group and instance ids are the service's own, so they
/// are bounded already and not counted here.
///
/// The FLOOR is what lets [`MAX_STORED_BYTES`] bound how many modifications can be recorded, on
/// the terms [`verified_access_instance_modification_payload_bytes`] states for that resource:
/// a client token is the caller's and may be a single byte, nothing reclaims a recorded
/// modification, and a group may be modified any number of times, so this map has no
/// record-count ceiling the way a create's store has.
pub fn verified_access_group_modification_payload_bytes(
    modification: &VerifiedAccessGroupModification,
    client_token: &str,
) -> usize {
    ::std::mem::size_of::<VerifiedAccessGroupModification>()
        .saturating_add(modification.description.as_ref().map_or(0, String::len))
        .saturating_add(client_token.len())
}

/// One stored Verified Access group as a REPORT of it reads: the record itself, with "A
/// description for the Amazon Web Services Verified Access group"
/// (`VerifiedAccessGroup$Description`) no caller ever set read as empty text rather than as an
/// absent member.
///
/// The ONE reading of a stored group, so the operations that report one — the delete's answer
/// and the modify's — cannot report the same stored record two ways. A create is NOT a report:
/// it echoes the request it was given.
pub fn verified_access_group_reported(
    view: &crate::ops::types::VerifiedAccessGroup,
) -> crate::ops::types::VerifiedAccessGroup {
    let mut reported = view.clone();
    if reported.description.is_none() {
        reported.description = Some(String::new());
    }
    reported
}

impl ComAmazonawsEc2Amazonec2State {
    /// The modification the `ModifyVerifiedAccessGroup` `ClientToken` `token` named, or `None`
    /// when no modification has been made under it.
    pub fn verified_access_group_modification_for_token(
        &self,
        token: &str,
    ) -> Option<&VerifiedAccessGroupModification> {
        self.verified_access_group_modification_client_tokens.get(token)
    }

    /// Apply `modification` to the group it names as of `at`, record it under `client_token`
    /// when the request carried one, and answer the group as it NOW stands — `None` when this
    /// store holds no group under that id.
    ///
    /// The one way a stored group is modified, so what the modify answers and what a later read
    /// of the same group reports cannot differ: the view it hands back is the stored record
    /// itself, read through [`verified_access_group_reported`] like every other report of a
    /// group. A member the modification leaves `None` is one the request did not carry, so the
    /// stored value stays as it was; a member it carries replaces that value
    /// ([`VerifiedAccessGroupModification::applied_to`]). Every endpoint of the group is brought
    /// back in step with it ([`Self::resync_verified_access_group_endpoints`]), because the
    /// instance an endpoint reports is a copy of the fact this record owns.
    ///
    /// The caller-chosen bytes are re-charged across the write — the record this replaces is
    /// refunded and the record it becomes is charged — so a caller cannot grow the store past
    /// [`MAX_STORED_BYTES`] by modifying one group repeatedly; a handler asks
    /// [`Self::would_exceed_stored_bytes_after_refund`] about that net change first.
    pub fn modify_verified_access_group(
        &mut self,
        modification: &VerifiedAccessGroupModification,
        client_token: Option<&str>,
        at: SystemTime,
    ) -> Option<crate::ops::types::VerifiedAccessGroup> {
        let held = self
            .verified_access_groups
            .get(&modification.verified_access_group_id)?;
        let refund = verified_access_group_payload_bytes(held, None);
        let mut record = modification.applied_to(held);
        // "The last updated time" (`VerifiedAccessGroup$LastUpdatedTime`): the instant this
        // modification was applied to the group.
        record.view.last_updated_time = Some(iso8601_utc(at));
        let charge = verified_access_group_payload_bytes(&record, None);
        let view = record.view.clone();
        self.verified_access_groups
            .insert(modification.verified_access_group_id.clone(), record);
        self.release_stored_bytes(refund);
        self.add_stored_bytes(charge);
        if let Some(token) = client_token {
            self.add_stored_bytes(verified_access_group_modification_payload_bytes(
                modification,
                token,
            ));
            self.verified_access_group_modification_client_tokens
                .insert(token.to_string(), modification.clone());
        }
        self.resync_verified_access_group_endpoints(&modification.verified_access_group_id, at);
        Some(verified_access_group_reported(&view))
    }

    /// Bring every endpoint of the Verified Access group `group_id` back in step with that
    /// group as of `at`: the instance the endpoint reports is re-read off the group.
    ///
    /// "The ID of the Amazon Web Services Verified Access instance"
    /// (`VerifiedAccessEndpoint$VerifiedAccessInstanceId`) is a copy of a fact the GROUP owns —
    /// an endpoint belongs to the instance its group belongs to, which is why a create reads it
    /// off the group rather than taking it from the request — so a writer that moves a group to
    /// another instance lands here rather than leaving the endpoint reporting the instance the
    /// group has left.
    ///
    /// An endpoint this leaves unchanged is NOT rewritten and its `LastUpdatedTime` does not
    /// move: nothing about that endpoint changed. The instance id is not a caller-chosen string
    /// ([`verified_access_endpoint_payload_bytes`] does not charge it), so no byte moves across
    /// this write either.
    pub fn resync_verified_access_group_endpoints(&mut self, group_id: &str, at: SystemTime) {
        let Some(instance_id) = self
            .verified_access_groups
            .get(group_id)
            .and_then(|record| record.view.verified_access_instance_id.clone())
        else {
            return;
        };
        for record in self.verified_access_endpoints.values_mut() {
            if record.view.verified_access_group_id.as_deref() != Some(group_id)
                || record.view.verified_access_instance_id.as_deref() == Some(instance_id.as_str())
            {
                continue;
            }
            record.view.verified_access_instance_id = Some(instance_id.clone());
            record.view.last_updated_time = Some(iso8601_utc(at));
        }
    }
}
