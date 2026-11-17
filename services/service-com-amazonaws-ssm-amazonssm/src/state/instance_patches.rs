// SPDX-License-Identifier: CC0-1.0

//! The per-patch compliance data a patching operation reported for ONE managed node, and
//! how a read of it narrows and pages what this endpoint holds.
//!
//! "Retrieves information about the patches on the specified managed node and their state
//! relative to the patch baseline being used for the node" (`DescribeInstancePatches`) — one
//! `PatchComplianceData` per patch the node reported, which is the detail behind the
//! high-level counts [`super::instance_patch_states`] holds.
//!
//! # What puts data here
//!
//! The same thing that puts a high-level state there: the patching operation Patch Manager
//! runs ON the node, which a send of the patch-baseline document Systems Manager publishes
//! starts ([`super::patch_runs`]). Each patch that run reported is held as a
//! [`PatchComplianceRow`], and the high-level counts of [`super::instance_patch_states`] are
//! counted from those rows, so the detail and the counts cannot disagree. A read reports the
//! patches that have been REPORTED for the node id it names, and reports none for an id this
//! endpoint holds no registration of too: the read is about what was reported for an id, so
//! an id nothing reported about has nothing to report rather than being refused
//! (`InvalidInstanceId` is this operation's refusal for a node that cannot be reached at all
//! — "you don't have permission to access the managed node … SSM Agent isn't running" —
//! which is not the same thing).
//!
//! # What a read may ask for
//!
//! A `Filters` entry narrows the patches by one of the keys this read publishes
//! ([`INSTANCE_PATCH_FILTER_KEYS`]); a key outside them names no property of a patch and is
//! refused in the voice this service refuses an unknown patch filter key in
//! ([`super::instance_patch_states::unknown_filter_key`]), before any node is resolved.

/// The filter key that narrows the patches by "the classification of the patch, such as
/// `SecurityUpdates`" (`PatchComplianceData$Classification`): "Supported keys for
/// `DescribeInstancePatches` include the following: Classification …"
/// (`DescribeInstancePatchesRequest$Filters`).
pub const FILTER_KEY_CLASSIFICATION: &str = "Classification";

/// The filter key that narrows them by "the operating system-specific ID of the patch"
/// (`PatchComplianceData$KBId`), from the same doc.
pub const FILTER_KEY_KB_ID: &str = "KBId";

/// The filter key that narrows them by "the severity of the patch such as `Critical`"
/// (`PatchComplianceData$Severity`), from the same doc.
pub const FILTER_KEY_SEVERITY: &str = "Severity";

/// The filter key that narrows them by "the state of the patch on the managed node, such as
/// `INSTALLED` or `FAILED`" (`PatchComplianceData$State`), from the same doc.
pub const FILTER_KEY_STATE: &str = "State";

/// Every key this read filters on, in the order
/// `DescribeInstancePatchesRequest$Filters` lists them. The `PatchOrchestratorFilter$Key`
/// member is a plain string in the model, so the vocabulary is enforced here — and this is
/// what the refusal of a key outside it names back to the caller.
pub const INSTANCE_PATCH_FILTER_KEYS: &[&str] = &[
    FILTER_KEY_CLASSIFICATION,
    FILTER_KEY_KB_ID,
    FILTER_KEY_SEVERITY,
    FILTER_KEY_STATE,
];

/// Whether `key` is a key this read filters on ([`INSTANCE_PATCH_FILTER_KEYS`]). The
/// comparison is exact: a filter key is a name, not a prose label, so neither a different
/// spelling nor a different case names a property of a patch.
pub fn is_instance_patch_filter_key(key: &str) -> bool {
    INSTANCE_PATCH_FILTER_KEYS.contains(&key)
}

/// What a `Filters` entry naming no filter key at all is refused with. Both members of a
/// `PatchOrchestratorFilter` are optional in the model, so such an entry reaches the
/// handler, and it names no property of a patch for the read to be narrowed by. Worded as
/// the sibling patch reads of this service word it
/// ([`super::patch_baselines::PATCH_BASELINE_FILTER_KEY_MISSING`]).
pub const INSTANCE_PATCH_FILTER_KEY_MISSING: &str =
    super::patch_baselines::PATCH_BASELINE_FILTER_KEY_MISSING;

/// Why one `Filters` entry of a request cannot be served, or `None` for a filter this read
/// can narrow by.
///
/// The key has to name a property of a patch, and the filter has to carry a value to search
/// for: "the value for the filter key" is what narrows anything, so an entry naming a key and
/// no value narrows nothing. Both are refusals of the REQUEST, taken before a node is
/// resolved.
pub fn instance_patch_filter_rejection(key: &str, values: &[String]) -> Option<String> {
    if key.is_empty() {
        return Some(INSTANCE_PATCH_FILTER_KEY_MISSING.to_string());
    }
    if !is_instance_patch_filter_key(key) {
        return Some(super::instance_patch_states::unknown_filter_key(key));
    }
    values
        .is_empty()
        .then(|| super::patch_baselines::patch_baseline_filter_value_missing(key))
}

/// The lower bound the modelled page size of this read carries
/// (`PatchComplianceMaxResults`, range 10..100).
pub const MIN_INSTANCE_PATCH_PAGE_SIZE: i32 = 10;

/// The upper bound of the same member, and the page a request naming no `MaxResults` is
/// served up to: "the maximum number of patches to return (per page)" bounds a page the
/// caller asked to be bounded, so a caller asking for no bound is served the whole page the
/// operation allows.
pub const MAX_INSTANCE_PATCH_PAGE_SIZE: i32 = 100;

/// The page size this read serves, or the refusal message for a `MaxResults` outside the
/// modelled range — which the boundary walk already refuses, so this is a backstop rather
/// than the first line of defence (see [`super::page_size`]).
pub fn instance_patch_page_size(max_results: Option<i32>) -> Result<usize, String> {
    super::page_size(
        max_results,
        MIN_INSTANCE_PATCH_PAGE_SIZE,
        MAX_INSTANCE_PATCH_PAGE_SIZE,
    )
}

/// "The patch successfully installed on the managed node" — one of the modelled
/// `PatchComplianceDataState` values (`crate::ops::types::PATCH_COMPLIANCE_DATA_STATE_VALUES`).
pub const STATE_INSTALLED: &str = "INSTALLED";

/// The patch is installed on the node but is not one the baseline defines (same vocabulary).
pub const STATE_INSTALLED_OTHER: &str = "INSTALLED_OTHER";

/// The patch is installed and the node has not been rebooted since, which is what completes
/// the installation (same vocabulary).
pub const STATE_INSTALLED_PENDING_REBOOT: &str = "INSTALLED_PENDING_REBOOT";

/// The patch is installed and the baseline now rejects it: "patches with a status of
/// `INSTALLED_REJECTED` were typically installed before they were added to a
/// `RejectedPatches` list" (`DescribePatchGroupState`).
pub const STATE_INSTALLED_REJECTED: &str = "INSTALLED_REJECTED";

/// The baseline approves the patch and the node does not hold it (same vocabulary).
pub const STATE_MISSING: &str = "MISSING";

/// The patch is one the baseline defines that is not to be installed on this node (same
/// vocabulary).
pub const STATE_NOT_APPLICABLE: &str = "NOT_APPLICABLE";

/// "The patch wasn't installed on the managed node" because the installation failed (same
/// vocabulary). Nothing here fails an installation, so no row carries it; the count of the
/// rows that do is what the high-level report's `FailedCount` is.
pub const STATE_FAILED: &str = "FAILED";

/// The patch is a security update that is available and not approved by the baseline (same
/// vocabulary), which is what `AvailableSecurityUpdateCount` counts.
pub const STATE_AVAILABLE_SECURITY_UPDATE: &str = "AVAILABLE_SECURITY_UPDATE";

/// The states that say the patch IS on the node, which is what makes a later run report it
/// installed rather than missing ([`PatchComplianceRow::is_installed`]).
pub const INSTALLED_STATES: &[&str] = &[
    STATE_INSTALLED,
    STATE_INSTALLED_OTHER,
    STATE_INSTALLED_PENDING_REBOOT,
    STATE_INSTALLED_REJECTED,
];

/// What one patching operation reported about ONE patch of one node — "information about
/// the state of a patch on a particular managed node as it relates to the patch baseline
/// used to patch the node" (`PatchComplianceData`).
///
/// Everything a read reports is held here, as the run reported it, beside the two things
/// the counts are worked out from: the patch's own id, which is what makes a later run
/// recognise a patch it already installed, and the compliance level the baseline assigned
/// it, which is what makes a patch that is not installed count against one of the
/// non-compliant counts.
#[derive(Debug, Clone)]
pub struct PatchComplianceRow {
    /// "The ID of the patch" in the catalogue Patch Manager publishes
    /// ([`super::patch_catalog::PublishedPatch::id`]) — not reported by this read, which
    /// names a patch by its operating-system id, and the key a run recognises a patch by.
    pub patch_id: String,
    /// "The title of the patch" (`PatchComplianceData$Title`).
    pub title: String,
    /// "The operating system-specific ID of the patch" (`PatchComplianceData$KBId`), which
    /// for a Windows patch is its Microsoft Knowledge Base ID and for every other operating
    /// system is the catalogue's own id for it.
    pub kb_id: String,
    /// "The classification of the patch, such as `SecurityUpdates`"
    /// (`PatchComplianceData$Classification`), or empty where the patch's operating system
    /// publishes no classification.
    pub classification: String,
    /// "The severity of the patch such as `Critical`" (`PatchComplianceData$Severity`) —
    /// the Microsoft severity for a Windows patch, the patch's own severity level
    /// otherwise, and empty where its operating system publishes neither.
    pub severity: String,
    /// "The state of the patch on the managed node, such as `INSTALLED` or `FAILED`"
    /// (`PatchComplianceData$State`): one of the `STATE_*` values above.
    pub state: String,
    /// "The date/time the patch was installed on the managed node" — the epoch for a patch
    /// that is not installed, which is what this endpoint reports for a patch no
    /// installation instant exists for.
    pub installed_time: ::std::time::SystemTime,
    /// "The compliance severity level for a patch" (`PatchStatus$ComplianceLevel`) as the
    /// baseline assigns it, and nothing for a patch its baseline assigns none. Not reported
    /// by this read; what the non-compliant counts of the high-level report are counted by.
    pub compliance_level: Option<String>,
}

impl PatchComplianceRow {
    /// The row a patching operation reports for `patch`, which it found in `state` on a
    /// node of `operating_system` at `installed_time`, with the compliance level its
    /// baseline assigns it.
    pub fn reported(
        patch: &super::patch_catalog::PublishedPatch,
        operating_system: &str,
        state: &str,
        installed_time: ::std::time::SystemTime,
        compliance_level: Option<String>,
    ) -> Self {
        Self {
            patch_id: patch.id.clone(),
            title: patch.title.clone(),
            kb_id: patch
                .kb_number
                .clone()
                .unwrap_or_else(|| patch.id.clone()),
            classification: patch.classification.clone().unwrap_or_default(),
            // "Applies to Windows patches only" is what the Microsoft severity of a patch
            // is published for, so a patch of another operating system reports its own
            // severity level under the one member this read has for a severity.
            severity: if operating_system == super::patch_catalog::EFFECTIVE_PATCH_OPERATING_SYSTEM
            {
                patch
                    .msrc_severity
                    .clone()
                    .or_else(|| patch.severity.clone())
                    .unwrap_or_default()
            } else {
                patch
                    .severity
                    .clone()
                    .or_else(|| patch.msrc_severity.clone())
                    .unwrap_or_default()
            },
            state: state.to_string(),
            installed_time,
            compliance_level,
        }
    }

    /// Whether this row says the patch is ON the node ([`INSTALLED_STATES`]).
    pub fn is_installed(&self) -> bool {
        INSTALLED_STATES.contains(&self.state.as_str())
    }

    /// The value a filter on `key` is matched against, or `None` when `key` names no
    /// property of a patch ([`INSTANCE_PATCH_FILTER_KEYS`]).
    pub fn filter_value(&self, key: &str) -> Option<&str> {
        match key {
            FILTER_KEY_CLASSIFICATION => Some(&self.classification),
            FILTER_KEY_KB_ID => Some(&self.kb_id),
            FILTER_KEY_SEVERITY => Some(&self.severity),
            FILTER_KEY_STATE => Some(&self.state),
            _ => None,
        }
    }

    /// Whether this row satisfies one filter: any of its `values` is what the row holds for
    /// the key, the way "the value for the filter key" narrows a patch read.
    pub fn matches_filter(&self, key: &str, values: &[String]) -> bool {
        self.filter_value(key)
            .is_some_and(|held| values.iter().any(|value| value == held))
    }

    /// This row, as a read reports it.
    pub fn to_patch_compliance_data(&self) -> crate::ops::types::PatchComplianceData {
        crate::ops::types::PatchComplianceData {
            title: self.title.clone(),
            kb_id: self.kb_id.clone(),
            classification: self.classification.clone(),
            severity: self.severity.clone(),
            state: self.state.clone(),
            installed_time: ::aws_smithy_types::DateTime::from(self.installed_time),
            // "The IDs of one or more Common Vulnerabilities and Exposure (CVE) issues
            // that are resolved by the patch" (`PatchComplianceData$CVEIds`): the
            // catalogue Patch Manager publishes here names none, so none is reported.
            cve_ids: None,
        }
    }
}
