// SPDX-License-Identifier: CC0-1.0

//! Whether a request may act on the space it addresses — the decision an operation takes
//! BEFORE it looks anything up and before it applies any rule the model does not state.
//!
//! A space is this service's unit of access: every space carries its own access control
//! (the access profile and the access grants provisioned with it, see
//! [`super::service_managed`]), so what a caller may do inside a space is decided by the
//! space records the account holds. A `spaceId` the account holds NO space record under
//! therefore authorizes nobody, whether it never named a space or named one that has
//! since been deleted, and a request addressing it is refused with the modelled
//! `AccessDeniedException` (HTTP 403).
//!
//! Two properties make that a decision rather than a lookup:
//!
//! * it is taken FIRST — ahead of the addressed resource's lookup, ahead of an
//!   `@idempotencyToken` replay, ahead of any state this operation would change and ahead
//!   of the service-level validation of a request body whose rules the model cannot
//!   express — so a caller the space does not authorize is told to fix its access rather
//!   than its payload, and nothing it asked for happens;
//! * it discloses NOTHING about the request beyond the refusal: the message is wholly
//!   fixed text ([`AUTHORIZATION_FAILED`]), naming neither the space, nor the resource
//!   inside it, nor the caller, so a refused request cannot be used to learn whether
//!   either resource exists.
//!
//! The decision belongs to the operations that address a space or one of the resources
//! its access control governs — the space itself, its access profiles and its dashboards.
//! It is NOT applied to the operations that address a resource of another kind: a listing
//! inside a space reports the collection it holds, an alert is addressed by its own
//! identifier, and a view and an access grant carry no `spaceId` at all.

use crate::ops::types::AccessDeniedException;

/// The message a request the addressed space does not authorize is refused with.
///
/// Wholly fixed text, with no variable part: the refusal is about the caller's access
/// rather than about anything the request named, so neither the space id, nor the
/// resource addressed inside it, nor the principal reaches it.
pub const AUTHORIZATION_FAILED: &str = "Authorization failed";

/// The modelled refusal itself, as every operation taking this decision returns it.
///
/// One constructor for all of them: the condition is the same one, so the error a caller
/// reads cannot be worded two ways depending on which operation refused.
#[must_use]
pub fn authorization_failed() -> AccessDeniedException {
    AccessDeniedException {
        message: AUTHORIZATION_FAILED.to_string(),
    }
}

impl super::ComAmazonawsCloudwatchomniCloudwatchomnifrontendState {
    /// The space `space_id` names, or the refusal a request the account's records do not
    /// authorize earns.
    ///
    /// Reaching the space and taking the decision are the SAME step: a `spaceId` the
    /// account holds no space record under authorizes nobody, so an operation addressing
    /// the space itself is answered from this one call rather than deciding twice.
    ///
    /// Reads the one collection of spaces every other operation reads, so a space the
    /// account has just created authorizes the calls that follow it and a space it has
    /// deleted authorizes none — there is no second record of who may reach what.
    pub fn authorized_space(
        &self,
        space_id: &str,
    ) -> Result<&super::StoredSpace, AccessDeniedException> {
        self.spaces.get(space_id).ok_or_else(authorization_failed)
    }

    /// The refusal a request addressing the space `space_id` earns, or `None` when the
    /// account's space records authorize it.
    ///
    /// The same decision as [`authorized_space`], for an operation that goes on to
    /// address a resource HELD BY the space rather than the space itself: that resource
    /// is looked up — and may be missing — only once the space has authorized the
    /// request.
    ///
    /// [`authorized_space`]: Self::authorized_space
    #[must_use]
    pub fn space_authorization_rejection(&self, space_id: &str) -> Option<AccessDeniedException> {
        self.authorized_space(space_id).err()
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::super::{
        now, ComAmazonawsCloudwatchomniCloudwatchomnifrontendState, StoredSpace,
        SPACE_STATUS_ACTIVE,
    };
    use super::*;
    use crate::ops::types::Space;

    /// A store holding one space under `space_id`, as a create leaves it.
    fn store_holding(space_id: &str) -> ComAmazonawsCloudwatchomniCloudwatchomnifrontendState {
        let mut store = ComAmazonawsCloudwatchomniCloudwatchomnifrontendState::default();
        let seq = store.next_seq();
        let at = now();
        store.spaces.insert(
            space_id.to_string(),
            StoredSpace {
                seq,
                domain_id: "d-space-authorization".to_string(),
                space: Space {
                    space_id: space_id.to_string(),
                    name: "a-space".to_string(),
                    space_arn: format!("arn:aws:cloudwatch:us-east-1:000000000000:space/{space_id}"),
                    domain_arn: None,
                    region: "us-east-1".to_string(),
                    owner_account_id: "000000000000".to_string(),
                    data_access_role_arn: "arn:aws:iam::000000000000:role/OmniData".to_string(),
                    created_at: at,
                    updated_at: at,
                    agent_core_evaluation_role_arn: None,
                    status: SPACE_STATUS_ACTIVE.to_string(),
                    status_reason: None,
                    encryption_configuration: None,
                },
                tags: HashMap::new(),
            },
        );
        store
    }

    /// RULE: the decision follows the account's space records — a space the store holds
    /// authorizes the request, and every other well-formed `SpaceId` refuses it.
    #[test]
    fn only_a_space_the_store_holds_authorizes_a_request() {
        // Rows span the `SpaceId` class (`smithy_query(query="shape",shape="SpaceId")`:
        // ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$) — the held id,
        // an id of nil digits, one carrying every hexadecimal digit, and one differing
        // from the held id in its last character only.
        let held = "3f6b1d20-7c84-4d1a-9f0e-2b5c8a71d4e3";
        let store = store_holding(held);
        for (case, space_id, authorized) in [
            ("the space the store holds", held, true),
            (
                "an id of nil digits",
                "00000000-0000-4000-8000-000000000000",
                false,
            ),
            (
                "an id of every hexadecimal digit",
                "0123456f-4567-89ab-cdef-0123456789ab",
                false,
            ),
            (
                "an id differing in its last digit only",
                "3f6b1d20-7c84-4d1a-9f0e-2b5c8a71d4e4",
                false,
            ),
        ] {
            let rejection = store.space_authorization_rejection(space_id);
            assert_eq!(
                rejection.is_none(),
                authorized,
                "case {case:?}: expected authorized={authorized}, got {rejection:?}"
            );
            if let Some(refusal) = rejection {
                assert_eq!(
                    refusal.message, "Authorization failed",
                    "case {case:?}: expected the fixed refusal text, got {:?}",
                    refusal.message
                );
            }
            // The two forms of the decision agree, and the authorized one hands back the
            // record the store holds rather than anything built from the request.
            let reached = store.authorized_space(space_id);
            assert_eq!(
                reached.is_ok(),
                authorized,
                "case {case:?}: expected authorized={authorized}, got {reached:?}"
            );
            if let Ok(stored) = reached {
                assert_eq!(
                    stored.space.space_id, space_id,
                    "case {case:?}: expected the stored space, got {:?}",
                    stored.space.space_id
                );
            }
        }
    }

    /// RULE: the refusal names nothing of the request — not the space, not the caller.
    #[test]
    fn the_refusal_names_nothing_the_request_carried() {
        let store = ComAmazonawsCloudwatchomniCloudwatchomnifrontendState::default();
        for space_id in [
            "51f48804-9a3e-4000-8000-000000000000",
            "c5a4f0a9-ee2f-4000-8000-000000000000",
        ] {
            let refusal = store
                .space_authorization_rejection(space_id)
                .expect("an empty store authorizes no space");
            assert!(
                !refusal.message.contains(space_id),
                "case {space_id:?}: the refusal quotes the space id: {:?}",
                refusal.message
            );
            assert_eq!(
                refusal.message,
                authorization_failed().message,
                "case {space_id:?}: expected one refusal for every space, got {:?}",
                refusal.message
            );
        }
    }
}
