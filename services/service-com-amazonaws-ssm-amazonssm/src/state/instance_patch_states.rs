// SPDX-License-Identifier: CC0-1.0

//! The high-level patch compliance state of a managed node, and how a patch-group read
//! narrows, orders and pages the reports this endpoint holds.
//!
//! "Defines the high-level patch compliance state for a managed node, providing
//! information about the number of installed, missing, not applicable, and failed patches
//! along with metadata about the operation when this information was gathered for the
//! managed node" (`InstancePatchState`). One patching operation on a node supersedes what
//! the previous one reported for it, so the store holds at most one report per
//! `(region, node id)` ([`super::ComAmazonawsSsmAmazonssmState::instance_patch_states`]),
//! which is also what makes a node's report resolvable by its id alone.
//!
//! # What puts a report here
//!
//! Patch compliance is gathered ON the node, by the patching operation Patch Manager runs
//! there: a send of the patch-baseline document Systems Manager publishes is what starts
//! one ([`super::patch_runs`]), and the report it leaves is this record. The store holds
//! the reports that have been made and invents none: a patch group whose nodes have never
//! been patched is reported as holding no state, which is what this endpoint knows, rather
//! than compliance nobody collected.
//!
//! Every count a report carries is counted from the patch rows that run reported
//! ([`super::instance_patches::PatchComplianceRow`]), so the counts and the per-patch
//! detail are one report read two ways rather than two figures that could disagree.
//!
//! # What a read reports
//!
//! `DescribeInstancePatchStatesForPatchGroup` "retrieves the high-level patch state for
//! the managed nodes in the specified patch group" — the reports of ONE patch group, in
//! ascending node-id order (the store's own order), narrowed by every filter the request
//! carries ([`INSTANCE_PATCH_STATE_FILTER_KEYS`]) and served one page at a time
//! ([`instance_patch_state_page_size`]).
//!
//! `DescribeInstancePatchStates` "retrieves the high-level patch state of one or more
//! managed nodes" — the reports of the nodes the request NAMES
//! ([`ComAmazonawsSsmAmazonssmState::instance_patch_states_of`]), in the same ascending
//! node-id order and paged the same way, up to the page size that read serves
//! ([`named_instance_patch_state_page_size`]). Both project the one store, so whichever
//! read reaches a node's report reports the same report.

use std::time::SystemTime;

use super::ComAmazonawsSsmAmazonssmState;
use crate::ops::types::{InstancePatchState, INSTANCE_PATCH_STATE_OPERATOR_TYPE_VALUES};

/// The filter key that compares "the number of patches from the patch baseline that are
/// installed on the managed node" (`InstancePatchState$InstalledCount`), from the keys
/// `InstancePatchStateFilter$Key` lists.
pub const FILTER_KEY_INSTALLED_COUNT: &str = "InstalledCount";

/// The filter key that compares "the number of patches not specified in the patch baseline
/// that are installed on the managed node" (same doc).
pub const FILTER_KEY_INSTALLED_OTHER_COUNT: &str = "InstalledOtherCount";

/// The filter key that compares "the number of patches installed by Patch Manager since
/// the last time the managed node was rebooted" (same doc).
pub const FILTER_KEY_INSTALLED_PENDING_REBOOT_COUNT: &str = "InstalledPendingRebootCount";

/// The filter key that compares "the number of patches installed on a managed node that
/// are specified in a `RejectedPatches` list" (same doc).
pub const FILTER_KEY_INSTALLED_REJECTED_COUNT: &str = "InstalledRejectedCount";

/// The filter key that compares "the number of patches from the patch baseline that are
/// applicable for the managed node but aren't currently installed" (same doc).
pub const FILTER_KEY_MISSING_COUNT: &str = "MissingCount";

/// The filter key that compares "the number of patches from the patch baseline that were
/// attempted to be installed during the last patching operation, but failed to install"
/// (same doc).
pub const FILTER_KEY_FAILED_COUNT: &str = "FailedCount";

/// The filter key that compares "the number of patches beyond the supported limit of
/// `NotApplicableCount` that aren't reported by name to Inventory" (same doc).
pub const FILTER_KEY_UNREPORTED_NOT_APPLICABLE_COUNT: &str = "UnreportedNotApplicableCount";

/// The filter key that compares "the number of patches from the patch baseline that aren't
/// applicable for the managed node" (same doc).
pub const FILTER_KEY_NOT_APPLICABLE_COUNT: &str = "NotApplicableCount";

/// Every key a patch-group read filters on, in the order the
/// `InstancePatchStateFilter$Key` documentation lists them ("supported values include the
/// following"). The member is a plain string in the model (`InstancePatchStateFilterKey`,
/// len 1..200), so the vocabulary is decided here.
pub const INSTANCE_PATCH_STATE_FILTER_KEYS: &[&str] = &[
    FILTER_KEY_INSTALLED_COUNT,
    FILTER_KEY_INSTALLED_OTHER_COUNT,
    FILTER_KEY_INSTALLED_PENDING_REBOOT_COUNT,
    FILTER_KEY_INSTALLED_REJECTED_COUNT,
    FILTER_KEY_MISSING_COUNT,
    FILTER_KEY_FAILED_COUNT,
    FILTER_KEY_UNREPORTED_NOT_APPLICABLE_COUNT,
    FILTER_KEY_NOT_APPLICABLE_COUNT,
];

/// Whether `key` is a key a patch-group read filters on
/// ([`INSTANCE_PATCH_STATE_FILTER_KEYS`]). The comparison is exact: a filter key is a
/// name, not a prose label.
pub fn is_instance_patch_state_filter_key(key: &str) -> bool {
    INSTANCE_PATCH_STATE_FILTER_KEYS.contains(&key)
}

/// The number a filter compares a count against, or `None` for a value that is no such
/// number: "the value for the filter. Must be an integer greater than or equal to 0"
/// (`InstancePatchStateFilter$Values`).
///
/// Read as `i64` rather than as the `i32` a count is held in: a caller may name a figure
/// larger than a count can hold, and a comparison against one is answered rather than
/// refused.
pub fn instance_patch_state_filter_value(value: &str) -> Option<i64> {
    value.parse::<i64>().ok().filter(|value| *value >= 0)
}

/// How this service states a filter key naming no patch-compliance count: the key it was
/// handed, and nothing else — "Unknown Filter Key: <key>" (recorded behaviour, corpus
/// `simparity-behavioral-aws-v1`, unit
/// `ssm.describe-instance-patch-states-invalid-filter-rejected`). The key is typed as a
/// plain string in the model (`InstancePatchStateFilterKey`) while the service admits one
/// of a fixed set, so the refusal is this service's own rather than a broken modelled
/// constraint.
pub fn unknown_filter_key(key: &str) -> String {
    format!("Unknown Filter Key: {key}")
}

/// How this service states a comparison outside the modelled
/// `InstancePatchStateOperatorType`, in the same voice as [`unknown_filter_key`]. That
/// member IS a modelled enum, so the boundary walk refuses one outside it before a handler
/// runs and this is a backstop.
pub fn unknown_filter_type(comparison: &str) -> String {
    format!("Unknown Filter Type: {comparison}")
}

/// How this service states a filter value that is no number to compare a count against: the
/// value it was handed and the rule that member's own documentation gives ("the value for
/// the filter. Must be an integer greater than or equal to 0"), in the same voice as
/// [`unknown_filter_key`].
pub fn invalid_filter_value(value: &str) -> String {
    format!("Invalid Filter Value: {value}. The value for the filter must be an integer greater than or equal to 0")
}

/// Why one `Filters` entry of a request cannot be served, or `None` for a filter this read
/// can compare.
///
/// The three things a filter can get wrong: its KEY names no patch-compliance count
/// ([`unknown_filter_key`]), its comparison is outside the modelled operator set
/// ([`unknown_filter_type`]), or one of its VALUES is not the integer its key's count is
/// compared against ([`invalid_filter_value`]). Each is a refusal of the REQUEST — the
/// filter names nothing this read could narrow by — rather than a report that could not be
/// found.
pub fn instance_patch_state_filter_rejection(
    key: &str,
    comparison: &str,
    values: &[String],
) -> Option<String> {
    if !is_instance_patch_state_filter_key(key) {
        return Some(unknown_filter_key(key));
    }
    if !INSTANCE_PATCH_STATE_OPERATOR_TYPE_VALUES.contains(&comparison) {
        return Some(unknown_filter_type(comparison));
    }
    values
        .iter()
        .find(|value| instance_patch_state_filter_value(value).is_none())
        .map(|value| invalid_filter_value(value))
}

/// The lower bound the modelled page size of a patch-state read carries
/// (`PatchComplianceMaxResults`, range 10..100).
pub const MIN_INSTANCE_PATCH_STATE_PAGE_SIZE: i32 = 10;

/// The upper bound of the same member: "the maximum number of patches to return (per
/// page)".
pub const MAX_INSTANCE_PATCH_STATE_PAGE_SIZE: i32 = 100;

/// How many reports one page carries at most, from the modelled result list
/// (`InstancePatchStatesList`, len 1..5): a response holding more than that is outside
/// what the operation's own result shape admits. Both this and `MaxResults` bound a page
/// from above, so the smaller of the two decides it — and since the smallest `MaxResults`
/// the model admits (10) is already above this, this is what a page is bounded by, with
/// the rest of the group reached through the next token.
pub const MAX_INSTANCE_PATCH_STATES_PER_PAGE: usize = 5;

/// The page size a patch-state read serves, or the refusal message for a `MaxResults`
/// outside the modelled range — which the boundary walk already refuses, so that half is a
/// backstop rather than the first line of defence (see [`super::page_size`]).
pub fn instance_patch_state_page_size(max_results: Option<i32>) -> Result<usize, String> {
    let requested = super::page_size(
        max_results,
        MIN_INSTANCE_PATCH_STATE_PAGE_SIZE,
        MAX_INSTANCE_PATCH_STATE_PAGE_SIZE,
    )?;
    Ok(requested.min(MAX_INSTANCE_PATCH_STATES_PER_PAGE))
}

/// The page size the read that NAMES the nodes it reports on serves — the same modelled
/// `MaxResults` band, without the result-list cap [`instance_patch_state_page_size`] applies.
///
/// `DescribeInstancePatchStates` answers an `InstancePatchStateList`, which carries no length
/// of its own, while the patch-group read answers an `InstancePatchStatesList` of 1..5: the
/// result shape is what bounds a page beyond `MaxResults`, so a read whose result shape bounds
/// nothing is bounded by `MaxResults` alone. A request naming none is served up to the modelled
/// maximum, as a caller asking for no bound is everywhere in this service.
pub fn named_instance_patch_state_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_INSTANCE_PATCH_STATE_PAGE_SIZE,
        MAX_INSTANCE_PATCH_STATE_PAGE_SIZE,
    )
}

/// The patching operation that gathered a report: "SCAN assesses the patch compliance
/// state" (`InstancePatchState$Operation`), one of the modelled `PatchOperationType`
/// values (`crate::ops::types::PATCH_OPERATION_TYPE_VALUES`).
pub const OPERATION_SCAN: &str = "Scan";

/// The compliance level a baseline assigns a patch whose absence makes a node critically
/// non-compliant: "the number of patches per node that are specified as `Critical` for
/// compliance reporting in the patch baseline aren't installed"
/// (`InstancePatchState$CriticalNonCompliantCount`), from the modelled
/// `PatchComplianceLevel` vocabulary (`crate::ops::types::PATCH_COMPLIANCE_LEVEL_VALUES`).
pub const COMPLIANCE_LEVEL_CRITICAL: &str = "CRITICAL";

/// The patch classifications that say a patch is "specified as `Security` in a patch
/// advisory" (`InstancePatchState$SecurityNonCompliantCount`): the classification an
/// RPM-based, SUSE or macOS advisory publishes a security update under, and the one Windows
/// publishes it under (`super::patch_properties`' published classifications).
pub const SECURITY_CLASSIFICATIONS: &[&str] = &["Security", "SecurityUpdates"];

/// The states a patch is NOT COMPLIANT with its baseline in: the baseline wants the patch on
/// the node and it is not there — "these patches might be missing, have failed installation,
/// were rejected, or were installed but awaiting a required managed node reboot"
/// (`DescribePatchGroupState$InstancesWithCriticalNonCompliantPatches`) names the first two
/// of those as states a row carries; a patch the baseline defines that is not to be
/// installed on this node is compliant, and a patch that IS installed is compliant.
pub const NON_COMPLIANT_STATES: &[&str] = &[
    super::instance_patches::STATE_MISSING,
    super::instance_patches::STATE_FAILED,
];

/// The other patching operation of the same vocabulary: "INSTALL installs missing
/// patches".
pub const OPERATION_INSTALL: &str = "Install";

/// What one patching operation reported about one managed node — the record a read
/// projects through [`InstancePatchStateRecord::to_instance_patch_state`].
///
/// Everything a read reports is held here, so the report is what was gathered rather than
/// a second copy of the request. The counts the model defaults to `0`
/// (`InstalledCount`, `InstalledOtherCount`, `MissingCount`, `FailedCount`,
/// `NotApplicableCount`) are plain numbers; the ones it defaults to `null` are held as
/// `Option`, because a patching operation that reported no figure for one of them reported
/// none, and a filter on a count nothing reported has no number to compare.
#[derive(Debug, Clone)]
pub struct InstancePatchStateRecord {
    /// "The ID of the managed node the high-level patch compliance information was
    /// collected for."
    pub instance_id: String,
    /// "The name of the patch group the managed node belongs to."
    pub patch_group: String,
    /// "The ID of the patch baseline used to patch the managed node."
    pub baseline_id: String,
    /// "The ID of the patch baseline snapshot used during the patching operation when this
    /// compliance data was collected."
    pub snapshot_id: Option<String>,
    /// "An https URL or an Amazon Simple Storage Service (Amazon S3) path-style URL to a
    /// list of patches to be installed."
    pub install_override_list: Option<String>,
    /// "Placeholder information. This field will always be empty in the current release of
    /// the service."
    pub owner_information: Option<String>,
    /// "The number of patches from the patch baseline that are installed on the managed
    /// node."
    pub installed_count: i32,
    /// "The number of patches not specified in the patch baseline that are installed on
    /// the managed node."
    pub installed_other_count: i32,
    /// "The number of patches installed by Patch Manager since the last time the managed
    /// node was rebooted."
    pub installed_pending_reboot_count: Option<i32>,
    /// "The number of patches installed on a managed node that are specified in a
    /// `RejectedPatches` list."
    pub installed_rejected_count: Option<i32>,
    /// "The number of patches from the patch baseline that are applicable for the managed
    /// node but aren't currently installed."
    pub missing_count: i32,
    /// "The number of patches from the patch baseline that were attempted to be installed
    /// during the last patching operation, but failed to install."
    pub failed_count: i32,
    /// "The number of patches beyond the supported limit of `NotApplicableCount` that
    /// aren't reported by name to Inventory."
    pub unreported_not_applicable_count: Option<i32>,
    /// "The number of patches from the patch baseline that aren't applicable for the
    /// managed node and therefore aren't installed on the node."
    pub not_applicable_count: i32,
    /// "The number of security-related patches that are available but not approved because
    /// they didn't meet the patch baseline requirements."
    pub available_security_update_count: Option<i32>,
    /// "The time the most recent patching operation was started on the managed node."
    pub operation_start_time: SystemTime,
    /// "The time the most recent patching operation completed on the managed node."
    pub operation_end_time: SystemTime,
    /// "The type of patching operation that was performed" ([`OPERATION_SCAN`],
    /// [`OPERATION_INSTALL`]).
    pub operation: String,
    /// "The time of the last attempt to patch the managed node with `NoReboot` specified as
    /// the reboot option."
    pub last_no_reboot_install_operation_time: Option<SystemTime>,
    /// "Indicates the reboot option specified in the patch baseline"
    /// (`crate::ops::types::REBOOT_OPTION_VALUES`).
    pub reboot_option: Option<String>,
    /// "The number of patches per node that are specified as `Critical` for compliance
    /// reporting in the patch baseline aren't installed."
    pub critical_non_compliant_count: Option<i32>,
    /// "The number of patches per node that are specified as `Security` in a patch advisory
    /// aren't installed."
    pub security_non_compliant_count: Option<i32>,
    /// "The number of patches per node that are specified as other than `Critical` or
    /// `Security` but aren't compliant with the patch baseline."
    pub other_non_compliant_count: Option<i32>,
    /// What this patching operation reported about each patch it found, which is the detail
    /// the counts above are counted from and what `DescribeInstancePatches` reports.
    pub patches: Vec<super::instance_patches::PatchComplianceRow>,
}

impl InstancePatchStateRecord {
    /// A report of `operation` on `instance_id`, which ran between `started` and `ended`
    /// against `baseline_id` while the node belonged to `patch_group`.
    ///
    /// Every count starts at the figure the model defaults it to — `0` for the five counts
    /// that carry a default and nothing at all for the ones that default to `null` — so a
    /// caller of this constructor sets only the figures its own operation gathered.
    pub fn reported(
        instance_id: &str,
        patch_group: &str,
        baseline_id: &str,
        operation: &str,
        started: SystemTime,
        ended: SystemTime,
    ) -> Self {
        Self {
            instance_id: instance_id.to_string(),
            patch_group: patch_group.to_string(),
            baseline_id: baseline_id.to_string(),
            snapshot_id: None,
            install_override_list: None,
            owner_information: None,
            installed_count: 0,
            installed_other_count: 0,
            installed_pending_reboot_count: None,
            installed_rejected_count: None,
            missing_count: 0,
            failed_count: 0,
            unreported_not_applicable_count: None,
            not_applicable_count: 0,
            available_security_update_count: None,
            operation_start_time: started,
            operation_end_time: ended,
            operation: operation.to_string(),
            last_no_reboot_install_operation_time: None,
            reboot_option: None,
            critical_non_compliant_count: None,
            security_non_compliant_count: None,
            other_non_compliant_count: None,
            patches: Vec::new(),
        }
    }

    /// The report one patching operation leaves: `rows` as it found them, with every count
    /// counted from them.
    ///
    /// No count is stated twice: each is how many of `rows` are in the state that count is
    /// about, so the figures a high-level read reports are the rows a per-patch read
    /// reports. The three counts that are about a patch's COMPLIANCE rather than its state
    /// are counted over the patches that are NOT COMPLIANT — the ones the baseline wants on
    /// the node and are not there ([`NON_COMPLIANT_STATES`]), which is what "aren't
    /// compliant with the patch baseline" (`InstancePatchState$OtherNonCompliantCount`)
    /// names; a patch the baseline defines that is not to be installed on this node is
    /// compliant and counts towards none of them. Of those, the ones the baseline assigns
    /// the critical compliance level are the critically non-compliant ones, the rest that
    /// are "specified as `Security` in a patch advisory" are the security non-compliant
    /// ones, and what is left is the other non-compliant ones.
    pub fn from_rows(
        instance_id: &str,
        patch_group: &str,
        baseline_id: &str,
        operation: &str,
        started: SystemTime,
        ended: SystemTime,
        rows: Vec<super::instance_patches::PatchComplianceRow>,
    ) -> Self {
        use super::instance_patches as patches;
        let count = |state: &str| -> i32 {
            let held = rows.iter().filter(|row| row.state == state).count();
            i32::try_from(held).unwrap_or(i32::MAX)
        };
        let counted = |held: usize| -> i32 { i32::try_from(held).unwrap_or(i32::MAX) };
        let non_compliant = rows
            .iter()
            .filter(|row| NON_COMPLIANT_STATES.contains(&row.state.as_str()));
        let critical = counted(
            non_compliant
                .clone()
                .filter(|row| row.compliance_level.as_deref() == Some(COMPLIANCE_LEVEL_CRITICAL))
                .count(),
        );
        let security = counted(
            non_compliant
                .clone()
                .filter(|row| {
                    row.compliance_level.as_deref() != Some(COMPLIANCE_LEVEL_CRITICAL)
                        && SECURITY_CLASSIFICATIONS.contains(&row.classification.as_str())
                })
                .count(),
        );
        let not_on_node = counted(non_compliant.count());
        let mut report = Self::reported(
            instance_id,
            patch_group,
            baseline_id,
            operation,
            started,
            ended,
        );
        report.installed_count = count(patches::STATE_INSTALLED);
        report.installed_other_count = count(patches::STATE_INSTALLED_OTHER);
        report.installed_pending_reboot_count =
            Some(count(patches::STATE_INSTALLED_PENDING_REBOOT));
        report.installed_rejected_count = Some(count(patches::STATE_INSTALLED_REJECTED));
        report.missing_count = count(patches::STATE_MISSING);
        report.failed_count = count(patches::STATE_FAILED);
        report.not_applicable_count = count(patches::STATE_NOT_APPLICABLE);
        // "The number of patches beyond the supported limit of `NotApplicableCount` that
        // aren't reported by name to Inventory": every patch this run found is reported by
        // name below, so none of them is unreported.
        report.unreported_not_applicable_count = Some(0);
        report.available_security_update_count =
            Some(count(patches::STATE_AVAILABLE_SECURITY_UPDATE));
        report.critical_non_compliant_count = Some(critical);
        report.security_non_compliant_count = Some(security);
        report.other_non_compliant_count = Some(
            not_on_node
                .saturating_sub(critical)
                .saturating_sub(security),
        );
        report.patches = rows;
        report
    }

    /// The figure this report holds for the count `key` names, or `None` when the key names
    /// a count this patching operation reported nothing for.
    ///
    /// Widened to `i64` because that is what a filter's value is read as: a filter may name
    /// a figure larger than a count can hold, and comparing both sides as `i64` answers
    /// such a comparison rather than refusing it.
    pub fn count(&self, key: &str) -> Option<i64> {
        let count = match key {
            FILTER_KEY_INSTALLED_COUNT => Some(self.installed_count),
            FILTER_KEY_INSTALLED_OTHER_COUNT => Some(self.installed_other_count),
            FILTER_KEY_INSTALLED_PENDING_REBOOT_COUNT => self.installed_pending_reboot_count,
            FILTER_KEY_INSTALLED_REJECTED_COUNT => self.installed_rejected_count,
            FILTER_KEY_MISSING_COUNT => Some(self.missing_count),
            FILTER_KEY_FAILED_COUNT => Some(self.failed_count),
            FILTER_KEY_UNREPORTED_NOT_APPLICABLE_COUNT => self.unreported_not_applicable_count,
            FILTER_KEY_NOT_APPLICABLE_COUNT => Some(self.not_applicable_count),
            _ => None,
        };
        count.map(i64::from)
    }

    /// Whether this report satisfies one filter: the comparison `comparison` names between
    /// the count `key` names and `value`.
    ///
    /// A count this report holds no figure for is not a number to compare, so NO comparison
    /// on it holds — a report that never carried the figure a filter asks about is not
    /// evidence either way about it.
    pub fn matches_filter(&self, key: &str, comparison: &str, value: i64) -> bool {
        let Some(count) = self.count(key) else {
            return false;
        };
        match comparison {
            COMPARISON_EQUAL => count == value,
            COMPARISON_NOT_EQUAL => count != value,
            COMPARISON_LESS_THAN => count < value,
            COMPARISON_GREATER_THAN => count > value,
            _ => false,
        }
    }

    /// This report, as a read reports it.
    pub fn to_instance_patch_state(&self) -> InstancePatchState {
        InstancePatchState {
            instance_id: self.instance_id.clone(),
            patch_group: self.patch_group.clone(),
            baseline_id: self.baseline_id.clone(),
            snapshot_id: self.snapshot_id.clone(),
            install_override_list: self.install_override_list.clone(),
            owner_information: self.owner_information.clone(),
            installed_count: self.installed_count,
            installed_other_count: self.installed_other_count,
            installed_pending_reboot_count: self.installed_pending_reboot_count,
            installed_rejected_count: self.installed_rejected_count,
            missing_count: self.missing_count,
            failed_count: self.failed_count,
            unreported_not_applicable_count: self.unreported_not_applicable_count,
            not_applicable_count: self.not_applicable_count,
            available_security_update_count: self.available_security_update_count,
            operation_start_time: ::aws_smithy_types::DateTime::from(self.operation_start_time),
            operation_end_time: ::aws_smithy_types::DateTime::from(self.operation_end_time),
            operation: self.operation.clone(),
            last_no_reboot_install_operation_time: self
                .last_no_reboot_install_operation_time
                .map(::aws_smithy_types::DateTime::from),
            reboot_option: self.reboot_option.clone(),
            critical_non_compliant_count: self.critical_non_compliant_count,
            security_non_compliant_count: self.security_non_compliant_count,
            other_non_compliant_count: self.other_non_compliant_count,
        }
    }
}

/// The comparison that holds when a count equals a filter's value, from the modelled
/// `InstancePatchStateOperatorType` vocabulary
/// (`crate::ops::types::INSTANCE_PATCH_STATE_OPERATOR_TYPE_VALUES`).
pub const COMPARISON_EQUAL: &str = "Equal";

/// The comparison that holds when a count differs from a filter's value (same vocabulary).
pub const COMPARISON_NOT_EQUAL: &str = "NotEqual";

/// The comparison that holds when a count is below a filter's value (same vocabulary).
pub const COMPARISON_LESS_THAN: &str = "LessThan";

/// The comparison that holds when a count is above a filter's value (same vocabulary).
pub const COMPARISON_GREATER_THAN: &str = "GreaterThan";

impl ComAmazonawsSsmAmazonssmState {
    /// The patch compliance report `region` holds for the node `instance_id`, or `None` when
    /// no patching operation has reported for that node — which is what a read of one node's
    /// patches resolves before it reports anything.
    pub fn instance_patch_state(
        &self,
        region: &str,
        instance_id: &str,
    ) -> Option<&InstancePatchStateRecord> {
        self.instance_patch_states
            .get(&(region.to_string(), instance_id.to_string()))
    }

    /// Store `report` as the patch compliance state of its node in `region`, superseding
    /// whatever the previous patching operation on that node reported.
    ///
    /// Answers `false` for a report naming no node: a report is held under the id of the
    /// node it was gathered for, so there is no key to store one without it.
    pub fn insert_instance_patch_state(
        &mut self,
        region: &str,
        report: InstancePatchStateRecord,
    ) -> bool {
        if report.instance_id.is_empty() {
            return false;
        }
        self.instance_patch_states
            .insert((region.to_string(), report.instance_id.clone()), report);
        true
    }

    /// The patch compliance reports `region` holds for the nodes `instance_ids` NAMES, in
    /// ascending node-id order, each node reported at most once however many times it was
    /// named.
    ///
    /// The store holds at most one report per node, so a node named twice has one report to
    /// report; the ascending order is the store's own, which is what lets a page resume from
    /// the node id it stopped at rather than from an offset. A named node no patching
    /// operation has reported for contributes nothing: the read is about what was gathered
    /// for a node, and nothing was.
    pub fn instance_patch_states_of<'a>(
        &'a self,
        region: &str,
        instance_ids: &[String],
    ) -> Vec<&'a InstancePatchStateRecord> {
        let mut named: Vec<&str> = instance_ids.iter().map(String::as_str).collect();
        named.sort_unstable();
        named.dedup();
        named
            .into_iter()
            .filter_map(|id| self.instance_patch_state(region, id))
            .collect()
    }

    /// The patch compliance reports `region` holds for the nodes of `patch_group`, in
    /// ascending node-id order (the store's own order, which is what lets a page resume
    /// from the node id it stopped at rather than from an offset).
    ///
    /// A patch group is named by the value of the node's own patch-group tag, so the
    /// comparison is exact: two groups whose names differ in case are two groups.
    pub fn instance_patch_states_in_group<'a>(
        &'a self,
        region: &'a str,
        patch_group: &'a str,
    ) -> impl Iterator<Item = (&'a str, &'a InstancePatchStateRecord)> + 'a {
        self.instance_patch_states
            .iter()
            .filter(move |((held, _), report)| held == region && report.patch_group == patch_group)
            .map(|((_, id), report)| (id.as_str(), report))
    }
}
