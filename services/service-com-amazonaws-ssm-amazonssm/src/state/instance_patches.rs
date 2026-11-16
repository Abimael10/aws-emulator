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
//! runs ON the node, which reports what it found from inside the node. No operation of this
//! service's model reports one, and the patch document that performs such a run is not among
//! the documents this emulator publishes ([`super::PUBLISHED_DOCUMENTS`]), so nothing a
//! caller can send starts one. A read therefore reports the patches that have been REPORTED
//! for the node id it names — none, because a report carries the COUNTS of each state and not
//! the patch rows behind them, so there is nothing to project and nothing is invented
//! (`PatchComplianceData` carries a title, a KB id, a classification, a severity, a state and
//! an install instant, all of them things that run reported from inside the node) — and it
//! reports that for an id this endpoint holds no registration of too: the read is about what
//! was reported for an id, so an id nothing reported about has nothing to report rather than
//! being refused (`InvalidInstanceId` is this operation's refusal for a node that cannot be
//! reached at all — "you don't have permission to access the managed node … SSM Agent isn't
//! running" — which is not the same thing).
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
