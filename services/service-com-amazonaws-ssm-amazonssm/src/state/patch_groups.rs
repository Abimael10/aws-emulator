// SPDX-License-Identifier: CC0-1.0

//! Which patch baseline a PATCH GROUP is patched against: the registration
//! `RegisterPatchBaselineForPatchGroup` writes, the one
//! `GetPatchBaselineForPatchGroup` reports, and the one
//! `DeregisterPatchBaselineForPatchGroup` takes away.
//!
//! A patch group is not a resource of this service: it is a VALUE a managed node carries as
//! a tag ([`super::patch_runs::PATCH_GROUP_TAG_KEYS`]), and what a register persists is the
//! pairing between that value and a baseline — "Registers a patch baseline for a patch
//! group" (`RegisterPatchBaselineForPatchGroup`). So the store holds the pairing and nothing
//! else about a group: a group no register has named holds no entry, and one whose baseline
//! was deleted holds none again.
//!
//! The pairing is per OPERATING SYSTEM. `GetPatchBaselineForPatchGroup` takes an
//! `OperatingSystem` beside the group and reports one back
//! (`smithy_query(query="operation", operation="GetPatchBaselineForPatchGroup")`), so one
//! group can be patched against a baseline of each operating system — and registering a
//! SECOND baseline of an operating system a group already has one for is what
//! `AlreadyExistsException` is documented for: "Error returned if an attempt is made to
//! register a patch group with a patch baseline that is already registered with a different
//! patch baseline".
//!
//! What a read reports for a group no register has named is the DEFAULT baseline of the
//! operating system asked about ([`ComAmazonawsSsmAmazonssmState::default_patch_baseline`]):
//! the read "retrieves the patch baseline that should be used for the specified patch group"
//! and models no not-found error at all, and what should be used for a group nothing has
//! registered a baseline for is the default of its operating system — which is also the
//! baseline Patch Manager patches such a node against
//! ([`ComAmazonawsSsmAmazonssmState::patch_baseline_in_force`]).

use super::patch_baselines::ResolvedBaseline;
use super::ComAmazonawsSsmAmazonssmState;

/// How many patch-group registrations this store holds across every Region.
///
/// Emulator resource guard, NOT a published AWS quota: the Region a registration is recorded
/// under comes from the request's own credential scope (`super::request_region`) and the
/// patch group is a free-form value the request carries, so without a ceiling a caller could
/// grow the store without bound. A register that REPLACES an entry already held is always
/// accepted, so the guard can never wedge a group an account has already registered.
pub const MAX_PATCH_GROUP_REGISTRATIONS: usize = 1_000;

/// Why a register cannot be served, as the operation's own modelled refusals.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistrationRefusal {
    /// The group already has a DIFFERENT baseline of that operating system, which is what
    /// `AlreadyExistsException` is documented for. Carries the baseline it is registered
    /// with, so the refusal can name it.
    AlreadyRegistered(String),
    /// This store can hold no further registration ([`MAX_PATCH_GROUP_REGISTRATIONS`]),
    /// which is what `ResourceLimitExceededException` reports.
    StoreFull,
}

/// How this service states that a patch group is already registered with another baseline.
///
/// `AlreadyExistsException`'s own documentation is "Error returned if an attempt is made to
/// register a patch group with a patch baseline that is already registered with a different
/// patch baseline", and the refusal NAMES both values the caller needs to act on: the group
/// it asked about and the baseline that group is registered with.
pub fn already_registered_message(patch_group: &str, baseline_id: &str) -> String {
    format!(
        "Patch group {patch_group} is already registered with patch baseline {baseline_id}"
    )
}

/// How this service states that it can hold no further patch-group registration.
///
/// `ResourceLimitExceededException`'s own documentation is "Error returned when the caller
/// has exceeded the default resource quotas", worded over the ceiling this store holds
/// ([`MAX_PATCH_GROUP_REGISTRATIONS`]) — an emulator guard rather than a published quota,
/// which is why the sentence names the count rather than quoting a published one.
pub fn registrations_store_full_message() -> String {
    format!(
        "This endpoint holds the {MAX_PATCH_GROUP_REGISTRATIONS} patch group registrations \
         it can hold"
    )
}

/// How this service states that a baseline cannot be deleted while a patch group is
/// registered with it.
///
/// `ResourceInUseException`'s own documentation is "Error returned if an attempt is made to
/// delete a patch baseline that is registered for a patch group", and the refusal names the
/// group that holds it so the caller knows which registration to take away first.
pub fn baseline_in_use_message(patch_group: &str) -> String {
    format!("Patch baseline is registered with patch group {patch_group}")
}

impl ComAmazonawsSsmAmazonssmState {
    /// The baseline `patch_group` is REGISTERED with for `operating_system` at `region`'s
    /// endpoint, or `None` when nothing has registered one there.
    ///
    /// A recorded registration is resolved against the baseline store rather than reported as
    /// stored, so a registration that no longer names a baseline this endpoint holds reports
    /// nothing instead of a baseline nothing holds.
    pub fn patch_group_registration(
        &self,
        region: &str,
        patch_group: &str,
        operating_system: &str,
    ) -> Option<ResolvedBaseline> {
        let key = (
            region.to_string(),
            patch_group.to_string(),
            operating_system.to_string(),
        );
        self.patch_group_baselines
            .get(&key)
            .and_then(|reference| self.resolve_patch_baseline(region, reference))
            .filter(|resolved| resolved.operating_system == operating_system)
    }

    /// The baseline a node of `patch_group` is PATCHED AGAINST at `region`'s endpoint: the
    /// one registered for the group, and otherwise the default baseline of
    /// `operating_system`.
    ///
    /// This is the one reading of "the patch baseline that should be used for the specified
    /// patch group" (`GetPatchBaselineForPatchGroup`), so what that read reports and what a
    /// patching operation runs against cannot disagree. A node carrying no patch group at all
    /// belongs to no group, and the baseline in force for it is the default of its operating
    /// system — which is what an empty `patch_group` resolves to here.
    pub fn patch_baseline_in_force(
        &self,
        region: &str,
        patch_group: &str,
        operating_system: &str,
    ) -> Option<ResolvedBaseline> {
        if !patch_group.is_empty() {
            if let Some(registered) =
                self.patch_group_registration(region, patch_group, operating_system)
            {
                return Some(registered);
            }
        }
        self.default_patch_baseline(region, operating_system)
    }

    /// Record `resolved` as the baseline `patch_group` is patched against at `region`'s
    /// endpoint — what `RegisterPatchBaselineForPatchGroup` persists, so a later read of the
    /// group reports it instead of the default of its operating system.
    ///
    /// The operating system the pairing is keyed by is the BASELINE's own: a baseline applies
    /// to one operating system ([`ResolvedBaseline::operating_system`]), so registering it
    /// says which operating system of the group it patches. Registering the baseline the
    /// group already has is the state the call asks for and is accepted; registering a
    /// DIFFERENT one for that operating system is refused
    /// ([`RegistrationRefusal::AlreadyRegistered`]).
    pub fn register_patch_baseline_for_patch_group(
        &mut self,
        region: &str,
        patch_group: &str,
        resolved: &ResolvedBaseline,
    ) -> Result<(), RegistrationRefusal> {
        let key = (
            region.to_string(),
            patch_group.to_string(),
            resolved.operating_system.clone(),
        );
        if let Some(held) = self.patch_group_baselines.get(&key) {
            let held = self
                .resolve_patch_baseline(region, held)
                .map(|resolved| resolved.baseline_id);
            if let Some(held) = held.filter(|held| held != &resolved.baseline_id) {
                return Err(RegistrationRefusal::AlreadyRegistered(held));
            }
        } else if self.patch_group_baselines.len() >= MAX_PATCH_GROUP_REGISTRATIONS {
            return Err(RegistrationRefusal::StoreFull);
        }
        self.patch_group_baselines
            .insert(key, resolved.reference.clone());
        Ok(())
    }

    /// Take away the registration between `patch_group` and the baseline `baseline_id` names
    /// at `region`'s endpoint, reporting whether one was held.
    ///
    /// What `DeregisterPatchBaselineForPatchGroup` persists: the pairing is gone, so a later
    /// read of the group reports the default of its operating system again and a patching
    /// operation on a node of that group runs against that default. A group registered with
    /// ANOTHER baseline keeps that registration — the call names the baseline to deregister —
    /// and the registrations of every other Region are left exactly as they were.
    pub fn deregister_patch_baseline_for_patch_group(
        &mut self,
        region: &str,
        patch_group: &str,
        baseline_id: &str,
    ) -> bool {
        let held: Vec<(String, String, String)> = self
            .patch_group_baselines
            .iter()
            .filter(|((held_region, held_group, _), reference)| {
                held_region == region
                    && held_group == patch_group
                    && self
                        .patch_baseline_query(region, reference)
                        .is_some_and(|named| named == baseline_id)
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in &held {
            self.patch_group_baselines.remove(key);
        }
        !held.is_empty()
    }

    /// The patch groups registered with the baseline `baseline_id` names at `region`'s
    /// endpoint, in ascending group order: "Patch groups included in the patch baseline"
    /// (`GetPatchBaseline$PatchGroups`) — a group is included in a baseline by registering it
    /// there, so this is the projection of the registrations that name this baseline.
    ///
    /// Without repeats: a group registered for two operating systems of one baseline is in
    /// that baseline once.
    pub fn patch_groups_of_patch_baseline(&self, region: &str, baseline_id: &str) -> Vec<String> {
        let mut groups: Vec<String> = self
            .patch_group_baselines
            .iter()
            .filter(|((held_region, _, _), reference)| {
                held_region == region
                    && self
                        .patch_baseline_query(region, reference)
                        .is_some_and(|named| named == baseline_id)
            })
            .map(|((_, group, _), _)| group.clone())
            .collect();
        groups.sort();
        groups.dedup();
        groups
    }

    /// Remove every registration of `region` that names the baseline `baseline_id`, as
    /// deleting that baseline does: nothing holds the baseline any more, so no group is
    /// patched against it and every group it had is patched against the default of its
    /// operating system again.
    pub fn drop_patch_group_registrations_of(&mut self, region: &str, baseline_id: &str) {
        let arn = super::patch_baselines::patch_baseline_arn(region, baseline_id);
        self.patch_group_baselines
            .retain(|(held_region, _, _), reference| {
                held_region != region
                    || (reference.as_str() != baseline_id && reference.as_str() != arn)
            });
    }
}
