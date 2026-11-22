// SPDX-License-Identifier: CC0-1.0

//! What an IPAM prefix list resolver TARGET synchronizes, and what a modification does to a
//! stored one.
//!
//! "An IPAM prefix list resolver target is an association between a specific customer-managed
//! prefix list and an IPAM prefix list resolver. The target enables the resolver to
//! synchronize CIDRs selected by its rules into the specified prefix list"
//! (`IpamPrefixListResolverTarget`): the association is what SYNCHRONIZES, so a target that has
//! been created has synchronized a VERSION of the list it names — "the version of the prefix
//! list that was last successfully synchronized by this target"
//! (`IpamPrefixListResolverTarget$LastSyncedVersion`) — and that version is what a client pins
//! as a desired version afterwards.
//!
//! WHICH version a target synchronizes is the target's own configuration: "indicates whether
//! this target automatically tracks the latest version of the prefix list"
//! (`$TrackLatestVersion`) selects the version the list stands at now, and "the desired version
//! of the prefix list that this target should synchronize with" (`$DesiredVersion`) is the one a
//! target that does not track the latest is pinned to. The prefix list's own `Version` is the
//! one place that number comes from, so the synced version is a version the list really has
//! rather than a counter of this module's own.
//!
//! "You can update version tracking settings and the desired version of the target prefix
//! list" (`ModifyIpamPrefixListResolverTarget`): a modification carries the members it changes
//! and leaves every other one as it stands, exactly as [`IpamPrefixListResolverModification`]
//! does for the resolver, and the target it leaves behind is in the non-terminal
//! `modify-in-progress` state the clock then owes `modify-complete` — at which point the target
//! synchronizes again, because what it synchronizes is what the modification just changed.
//!
//! Split out of the [`crate::state`] module root so each file holds one concern, and
//! re-exported from it, so a handler reaches these as `crate::state::<item>` like every other
//! primitive there.

use super::*;

/// A resolver target whose modification has been asked for and has not settled yet: the
/// `IpamPrefixListResolverTargetState` enum's `modify-in-progress` member
/// (`smithy_query(query="shape",shape="IpamPrefixListResolverTargetState")`), which is what
/// `ModifyIpamPrefixListResolverTarget` answers with.
pub const IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_MODIFY_IN_PROGRESS: &str = "modify-in-progress";

/// The state that enum reaches once the modification has settled: `sync-complete`, which the
/// clock owes a [`IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_MODIFY_IN_PROGRESS`] target one
/// [`TRANSITION_DELAY`] after the call that modified it.
///
/// The SYNC's terminal state rather than the modification's, because what the modification
/// settling DOES is synchronize: "you can update version tracking settings and the desired
/// version of the target prefix list" (`ModifyIpamPrefixListResolverTarget`) changes WHICH
/// version the target synchronizes, so the state it lands in is the one that says that
/// synchronization ran — which is what a client waits for before reading the version the
/// target published.
pub const IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_SYNC_COMPLETE: &str = "sync-complete";

/// What one `ModifyIpamPrefixListResolverTarget` asks to be changed about a stored target.
///
/// A member left `None` here is one the REQUEST did not carry, which leaves the stored value
/// exactly as it was rather than returning it to the default a create would have given it — the
/// same contract [`IpamPrefixListResolverModification`] has for a resolver.
#[derive(Debug, Clone)]
pub struct IpamPrefixListResolverTargetModification {
    /// "The ID of the IPAM prefix list resolver target to modify"
    /// (`ModifyIpamPrefixListResolverTargetRequest$IpamPrefixListResolverTargetId`).
    pub ipam_prefix_list_resolver_target_id: String,
    /// "The desired version of the prefix list to target. This allows you to pin the target to
    /// a specific version" (`$DesiredVersion`), or `None` when the request named none.
    pub desired_version: Option<i64>,
    /// "Indicates whether the resolver target should automatically track the latest version of
    /// the prefix list" (`$TrackLatestVersion`), or `None` when the request named none.
    pub track_latest_version: Option<bool>,
}

impl IpamPrefixListResolverTargetModification {
    /// The target this modification leaves behind, from the one it is applied to.
    ///
    /// The ONE definition of what a modification DOES to a target: every member the request did
    /// not carry is copied across unchanged, and every member the target's own creation derived
    /// — its id, its ARN, its owner, the resolver it belongs to, the prefix list it
    /// synchronizes and the Region that list is in — is carried over untouched, since none of
    /// them follows from this request. The state and the synced version are left as they stand
    /// here; [`ComAmazonawsEc2Amazonec2State::modify_ipam_prefix_list_resolver_target`] is what
    /// moves the state and the SYNCHRONIZATION is what records a version, so no path can store
    /// either behind the clock's back.
    pub fn applied_to(
        &self,
        target: &IpamPrefixListResolverTarget,
    ) -> IpamPrefixListResolverTarget {
        let mut applied = target.clone();
        if self.desired_version.is_some() {
            applied.desired_version = self.desired_version;
        }
        if self.track_latest_version.is_some() {
            applied.track_latest_version = self.track_latest_version;
        }
        applied
    }
}

impl ComAmazonawsEc2Amazonec2State {
    /// The version the prefix list `prefix_list_id` names stands at, or `None` when this store
    /// holds no such list.
    ///
    /// "The version of the prefix list" (`ManagedPrefixList$Version`) is the one place a
    /// synchronized version comes from: every revision of a list's entry set mints the next one
    /// ([`Self::materialize_due_transitions`] applies the revision a `ModifyManagedPrefixList`
    /// started), so a target reports a version the list really has.
    pub fn managed_prefix_list_version(&self, prefix_list_id: &str) -> Option<i64> {
        self.managed_prefix_lists
            .get(prefix_list_id)
            .and_then(|record| record.view.version)
    }

    /// Synchronize the resolver target `target_id` names with the prefix list it names, and
    /// record the version it synchronized ([`IpamPrefixListResolverTarget::last_synced_version`]).
    ///
    /// The ONE place a target's synced version is written, so the version a create's
    /// synchronization recorded and the one a modification's recorded cannot be arrived at two
    /// ways. WHICH version is synchronized is the target's own configuration: the version the
    /// list stands at for a target that tracks the latest, and the version the target is pinned
    /// to otherwise — a target that pins none tracks the list it was created against, which is
    /// the version the list stands at ("if not specified, the resolver will target the latest
    /// version", `CreateIpamPrefixListResolverTargetRequest$DesiredVersion`).
    ///
    /// A version ABOVE the one the list stands at is one no synchronization could have
    /// published, so it records nothing and leaves the version the target last synchronized
    /// where it was; so does a target whose prefix list this store no longer holds. The entry
    /// set itself is not rewritten here: the CIDRs a resolver selects come from the IPAM
    /// resources its rules match, and a selection that adds no CIDR revises nothing, so the
    /// list stays at the version this records.
    pub(super) fn synchronize_ipam_prefix_list_resolver_target(&mut self, target_id: &str) {
        let Some(target) = self.ipam_prefix_list_resolver_targets.get(target_id) else {
            return;
        };
        let tracks_latest = target.track_latest_version.unwrap_or(false);
        let pinned = target.desired_version;
        let Some(prefix_list_id) = target.prefix_list_id.clone() else {
            return;
        };
        let Some(latest) = self.managed_prefix_list_version(&prefix_list_id) else {
            return;
        };
        let synchronized = match pinned {
            Some(pinned) if !tracks_latest => pinned,
            _ => latest,
        };
        if synchronized > latest {
            return;
        }
        if let Some(target) = self.ipam_prefix_list_resolver_targets.get_mut(target_id) {
            target.last_synced_version = Some(synchronized);
        }
    }

    /// Apply the move the clock owes a CREATED resolver target, as of its due instant: the
    /// target reaches [`IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_CREATE_COMPLETE`] and
    /// SYNCHRONIZES the prefix list it was created against.
    ///
    /// A created target is an association that synchronizes ("the target enables the resolver to
    /// synchronize CIDRs selected by its rules into the specified prefix list",
    /// `IpamPrefixListResolverTarget`), so the version it synchronized is part of what its
    /// creation settling MEANS rather than something a later call has to ask for — which is why
    /// the two happen together here, from
    /// [`Self::materialize_due_transitions`], its one caller.
    pub(super) fn complete_ipam_prefix_list_resolver_target_creation(&mut self, target_id: &str) {
        let Some(target) = self.ipam_prefix_list_resolver_targets.get_mut(target_id) else {
            return;
        };
        if target.state.as_deref() != Some(IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_CREATE_IN_PROGRESS)
        {
            return;
        }
        target.state = Some(IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_CREATE_COMPLETE.to_string());
        self.synchronize_ipam_prefix_list_resolver_target(target_id);
    }

    /// Apply one `ModifyIpamPrefixListResolverTarget` to the target it names and answer the
    /// record it leaves behind — or `None` when this store holds no target of that id and
    /// nothing was written.
    ///
    /// The ONE writer of a target modification, so what `ModifyIpamPrefixListResolverTarget`
    /// answers is the stored record a later `DescribeIpamPrefixListResolverTargets` of the same
    /// target reports. What the modification CHANGES is
    /// [`IpamPrefixListResolverTargetModification::applied_to`]; what this adds is what the
    /// store owns:
    ///
    /// * the target is left in the NON-TERMINAL
    ///   [`IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_MODIFY_IN_PROGRESS`] state with the move to
    ///   [`IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_SYNC_COMPLETE`] recorded in
    ///   [`Self::ipam_prefix_list_resolver_target_modify_complete_at`] one [`TRANSITION_DELAY`]
    ///   out and applied by [`Self::materialize_due_transitions`], so a caller acting straight
    ///   after the call still sees the transient state and nothing is slept for;
    /// * any move the clock still owed this target's CREATION is superseded, together with the
    ///   synchronization that move carries: a target this call has moved is not going to land in
    ///   the state its creation was moving it to. Its creation's synchronization is not lost —
    ///   the modification's own, at `modify-complete`, is the one that counts, and it
    ///   synchronizes what this call just changed.
    ///
    /// Nothing caller-chosen is retained by a modification — both members are numbers the model
    /// fixes the width of — so the store's byte budget is not asked about one.
    pub fn modify_ipam_prefix_list_resolver_target(
        &mut self,
        modification: &IpamPrefixListResolverTargetModification,
    ) -> Option<IpamPrefixListResolverTarget> {
        let target_id = modification.ipam_prefix_list_resolver_target_id.clone();
        let current = self.ipam_prefix_list_resolver_targets.get(&target_id)?;
        let mut applied = modification.applied_to(current);
        applied.state =
            Some(IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_MODIFY_IN_PROGRESS.to_string());
        self.ipam_prefix_list_resolver_targets
            .insert(target_id.clone(), applied.clone());
        self.ipam_prefix_list_resolver_target_create_complete_at
            .remove(&target_id);
        self.ipam_prefix_list_resolver_target_modify_complete_at
            .insert(target_id, Instant::now() + TRANSITION_DELAY);
        Some(applied)
    }

    /// Apply the move the clock owes a modified resolver target, as of `now`.
    ///
    /// A target a `ModifyIpamPrefixListResolverTarget` left
    /// [`IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_MODIFY_IN_PROGRESS`] had SYNCHRONIZED the
    /// version the modification left it configured for at the instant that call recorded for it,
    /// whether or not anybody called again — which is the whole point of pinning one, and why
    /// the state it lands in is [`IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_SYNC_COMPLETE`]. So the
    /// move is applied from [`Self::materialize_due_transitions`], its one caller, before
    /// anything reads the store.
    pub(super) fn materialize_due_ipam_prefix_list_resolver_target_modifications(
        &mut self,
        now: Instant,
    ) {
        let due: Vec<String> = self
            .ipam_prefix_list_resolver_target_modify_complete_at
            .iter()
            .filter(|(_, due_at)| now >= **due_at)
            .map(|(target_id, _)| target_id.clone())
            .collect();
        for target_id in due {
            self.ipam_prefix_list_resolver_target_modify_complete_at
                .remove(&target_id);
            let Some(target) = self.ipam_prefix_list_resolver_targets.get_mut(&target_id) else {
                continue;
            };
            if target.state.as_deref()
                != Some(IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_MODIFY_IN_PROGRESS)
            {
                continue;
            }
            target.state =
                Some(IPAM_PREFIX_LIST_RESOLVER_TARGET_STATE_SYNC_COMPLETE.to_string());
            self.synchronize_ipam_prefix_list_resolver_target(&target_id);
        }
    }
}
